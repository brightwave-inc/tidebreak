import {
  type CSSProperties,
  type ReactNode,
  type RefObject,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import {
  Bot,
  Braces,
  ExternalLink,
  Globe2,
  Link2,
  RefreshCw,
  TriangleAlert,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { BrowserNoticeRow, BrowserToolbar } from "./BrowserToolbar";
import { BrowserViewportControl } from "./BrowserViewportControl";
import {
  browserUnavailableMessage,
  resetCodeBrowserProfile,
  type BrowserBounds,
  type BrowserHostAction,
  type BrowserHostEvent,
  type BrowserProfileResetPhase,
  type BrowserHostSnapshot,
  type CodeBrowserHost,
  nativeCodeBrowserHost,
} from "./browserHost";
import {
  browserDisplayAddress,
  browserTarget,
  validateBrowserUrl,
} from "./browserNavigation";
import {
  readStoredBrowserSession,
  restoreOrCreateBrowserSession,
  writeStoredBrowserSession,
} from "./browserPersistence";
import {
  type BrowserViewport,
  restoreOrDefaultViewport,
  viewportTargetWidth,
  writeStoredViewport,
} from "./browserViewport";
import {
  beginBrowserNavigation,
  canBrowserGoBack,
  canBrowserGoForward,
  failBrowserSession,
  finishBrowserNavigation,
  moveBrowserHistory,
  observeBrowserNavigation,
  setBrowserNotice,
  setBrowserTitle,
  type BrowserSession,
} from "./browserSession";

const SLOW_LOAD_MS = 15_000;
const PERSIST_DELAY_MS = 150;
let nextProfileResetId = 0;

function createProfileResetId(): number {
  nextProfileResetId =
    nextProfileResetId >= Number.MAX_SAFE_INTEGER ? 1 : nextProfileResetId + 1;
  return nextProfileResetId;
}

type BrowserAgentHostAction = Extract<
  BrowserHostAction,
  {
    type:
      | "share_with_agent"
      | "revoke_agent_access"
      | "stop_agent_control"
      | "take_human_control";
  }
>;

export type CodeBrowserTabProps = {
  workspaceId: string;
  browserId: string;
  initialUrl?: string;
  /** Hide the native view while app-owned DOM overlays cover the editor. */
  obscured?: boolean;
  host?: CodeBrowserHost;
  onTitleChange?: (title: string) => void;
};

export function CodeBrowserTab(props: CodeBrowserTabProps) {
  return (
    <CodeBrowserTabSession
      key={`${props.workspaceId}:${props.browserId}`}
      {...props}
    />
  );
}

function CodeBrowserTabSession({
  workspaceId,
  browserId,
  initialUrl,
  obscured = false,
  host = nativeCodeBrowserHost,
  onTitleChange,
}: CodeBrowserTabProps) {
  const [session, setSession] = useState(() =>
    restoreOrCreateBrowserSession({ browserId, workspaceId, initialUrl }),
  );
  const [address, setAddress] = useState(session.address);
  const [addressError, setAddressError] = useState<string | null>(null);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [agentAccessOpen, setAgentAccessOpen] = useState(false);
  const [viewportOpen, setViewportOpen] = useState(false);
  const [profileResetPhase, setProfileResetPhase] =
    useState<BrowserProfileResetPhase | null>(null);
  const [profileResetReconstructing, setProfileResetReconstructing] =
    useState(false);
  const [slow, setSlow] = useState(false);
  const [runtime, setRuntime] = useState<BrowserHostSnapshot | null>(null);
  const surfaceRef = useRef<HTMLDivElement | null>(null);
  const mountedRef = useRef(true);
  const nativeReady = useRef(false);
  const nativePresence = useRef<"unknown" | "missing" | "present">("unknown");
  const nativeSurfaceGeneration = useRef(0);
  const nativeCreateInFlight = useRef<{
    generation: number;
    operation: Promise<void>;
  } | null>(null);
  const nativeCreateRequestedUrl = useRef<string | null>(null);
  const activeProfileResetId = useRef<number | null>(null);
  const locallyInitiatedProfileResetId = useRef<number | null>(null);
  const completedProfileResetIds = useRef(new Set<number>());
  const profileResetRecovery = useRef<{
    resetId: number;
    operation: Promise<void>;
  } | null>(null);
  const nativeExpectedNavigationUrl = useRef<string | null>(null);
  const nativeDocumentEpoch = useRef(0);
  const resizeCreateAttempted = useRef(false);
  const nativeHistoryAvailable = useRef(false);
  const lastNativeBounds = useRef<BrowserBounds | null>(null);
  const nativeRevealFrame = useRef<number | null>(null);
  const persistTimer = useRef<number | null>(null);
  const sessionRef = useRef(session);
  const [viewport, setViewport] = useState(() => restoreOrDefaultViewport());
  const viewportSurfaceRef = useRef<HTMLDivElement | null>(null);
  const [renderedViewportWidth, setRenderedViewportWidth] = useState<
    number | null
  >(null);
  const visible =
    !obscured &&
    !historyOpen &&
    !agentAccessOpen &&
    !viewportOpen &&
    !profileResetReconstructing &&
    session.loadState !== "failed";
  const visibleRef = useRef(visible);

  sessionRef.current = session;
  visibleRef.current = visible;

  // Reset agentAccessOpen when agent access is revoked or becomes unavailable
  // while its compact menu is open. Mirror BrowserAgentAccessControl's render
  // guard: Radix does not call onOpenChange(false) when the control returns
  // null, so without this the native WKWebView would stay hidden permanently.
  const agentAccessAvailable = Boolean(
    runtime?.engine?.capabilities.semanticSnapshot &&
      runtime?.agentAccess?.origin,
  );
  useEffect(() => {
    if (agentAccessOpen && !agentAccessAvailable) {
      setAgentAccessOpen(false);
    }
  }, [agentAccessAvailable, agentAccessOpen]);

  const updateSession = useCallback(
    (update: (current: BrowserSession) => BrowserSession) => {
      setSession((current) => {
        const next = update(current);
        sessionRef.current = next;
        return next;
      });
    },
    [],
  );

  const recordRuntime = useCallback(
    (snapshot: BrowserHostSnapshot) => {
      if (
        snapshot.browserId === browserId &&
        snapshot.workspaceId === workspaceId
      ) {
        setRuntime(snapshot);
        if (snapshot.documentEpoch !== undefined) {
          nativeDocumentEpoch.current = Math.max(
            nativeDocumentEpoch.current,
            snapshot.documentEpoch,
          );
        }
        if (snapshot.inspectEnabled !== undefined) {
          updateSession((current) => ({
            ...current,
            inspectEnabled: snapshot.inspectEnabled!,
          }));
        }
      }
    },
    [browserId, workspaceId, updateSession],
  );

  const cancelNativeReveal = useCallback(() => {
    if (nativeRevealFrame.current === null) return;
    const frame = nativeRevealFrame.current;
    nativeRevealFrame.current = null;
    window.cancelAnimationFrame(frame);
  }, []);

  const markNativeClosedForProfileReset = useCallback(() => {
    cancelNativeReveal();
    nativeSurfaceGeneration.current =
      nativeSurfaceGeneration.current >= Number.MAX_SAFE_INTEGER
        ? 1
        : nativeSurfaceGeneration.current + 1;
    nativeReady.current = false;
    nativePresence.current = "missing";
    nativeCreateRequestedUrl.current = null;
    nativeExpectedNavigationUrl.current = null;
    nativeDocumentEpoch.current = 0;
    resizeCreateAttempted.current = false;
    nativeHistoryAvailable.current = false;
    lastNativeBounds.current = null;
    visibleRef.current = false;
    setProfileResetReconstructing(true);
  }, [cancelNativeReveal]);

  const beginProfileResetCycle = useCallback(
    (resetId: number, phase: BrowserProfileResetPhase): boolean => {
      if (completedProfileResetIds.current.has(resetId)) return false;
      if (activeProfileResetId.current !== resetId) {
        activeProfileResetId.current = resetId;
        profileResetRecovery.current = null;
        markNativeClosedForProfileReset();
      }
      setProfileResetPhase(phase);
      return true;
    },
    [markNativeClosedForProfileReset],
  );

  const finishProfileResetCycle = useCallback((resetId: number) => {
    completedProfileResetIds.current.add(resetId);
    while (completedProfileResetIds.current.size > 8) {
      const oldest = completedProfileResetIds.current.values().next().value;
      if (oldest === undefined) break;
      completedProfileResetIds.current.delete(oldest);
    }
    if (activeProfileResetId.current !== resetId) return;
    activeProfileResetId.current = null;
    if (profileResetRecovery.current?.resetId === resetId) {
      profileResetRecovery.current = null;
    }
    setProfileResetPhase(null);
    setProfileResetReconstructing(false);
  }, []);

  const scheduleNativeReveal = useCallback(
    (expectedGeneration = nativeSurfaceGeneration.current) => {
      cancelNativeReveal();
      if (
        !mountedRef.current ||
        !nativeReady.current ||
        !visibleRef.current ||
        !host.available() ||
        nativeSurfaceGeneration.current !== expectedGeneration
      ) {
        return;
      }

      const frame = window.requestAnimationFrame(() => {
        if (nativeRevealFrame.current !== frame) return;
        nativeRevealFrame.current = null;
        if (
          !mountedRef.current ||
          !nativeReady.current ||
          !visibleRef.current ||
          !host.available() ||
          nativeSurfaceGeneration.current !== expectedGeneration
        ) {
          return;
        }
        void host
          .command(workspaceId, browserId, {
            type: "set_visible",
            visible: true,
          })
          .catch(() => undefined);
      });
      nativeRevealFrame.current = frame;
    },
    [browserId, cancelNativeReveal, host, workspaceId],
  );

  /** Reconcile live bounds and visibility after a native view becomes ready. */
  const reconcileAfterNativeReady = useCallback(
    async (
      expectedGeneration = nativeSurfaceGeneration.current,
    ): Promise<boolean> => {
      if (nativeSurfaceGeneration.current !== expectedGeneration) return false;
      if (!mountedRef.current) {
        nativeReady.current = false;
        cancelNativeReveal();
        if (nativeSurfaceGeneration.current !== expectedGeneration) {
          return false;
        }
        await host.command(workspaceId, browserId, {
          type: "set_visible",
          visible: false,
        });
        return false;
      }
      nativeReady.current = true;
      const bounds = readBrowserBounds(viewportSurfaceRef.current);
      if (bounds && !sameBrowserBounds(lastNativeBounds.current, bounds)) {
        if (nativeSurfaceGeneration.current !== expectedGeneration) {
          return false;
        }
        lastNativeBounds.current = bounds;
        const snapshot = await host.command(workspaceId, browserId, {
          type: "set_bounds",
          bounds,
        });
        if (nativeSurfaceGeneration.current !== expectedGeneration) {
          return false;
        }
        recordRuntime(snapshot);
      }
      if (nativeSurfaceGeneration.current !== expectedGeneration) return false;
      if (!mountedRef.current || !visibleRef.current) {
        if (!mountedRef.current) nativeReady.current = false;
        cancelNativeReveal();
        if (nativeSurfaceGeneration.current !== expectedGeneration) {
          return false;
        }
        const snapshot = await host.command(workspaceId, browserId, {
          type: "set_visible",
          visible: false,
        });
        if (nativeSurfaceGeneration.current !== expectedGeneration) {
          return false;
        }
        recordRuntime(snapshot);
        return true;
      }
      scheduleNativeReveal(expectedGeneration);
      return true;
    },
    [
      browserId,
      cancelNativeReveal,
      host,
      recordRuntime,
      scheduleNativeReveal,
      workspaceId,
    ],
  );

  const createAndReconcileNative = useCallback(
    async (url: string): Promise<boolean> => {
      if (!host.available()) return false;
      const generation = nativeSurfaceGeneration.current;
      if (nativeReady.current) {
        await reconcileAfterNativeReady(generation);
        return (
          nativeSurfaceGeneration.current === generation && nativeReady.current
        );
      }
      nativeCreateRequestedUrl.current = url;
      const pending = nativeCreateInFlight.current;
      if (pending?.generation === generation) {
        nativeExpectedNavigationUrl.current = url;
        await pending.operation;
        return (
          nativeSurfaceGeneration.current === generation && nativeReady.current
        );
      }
      const bounds = readBrowserBounds(viewportSurfaceRef.current);
      if (!bounds) return false;

      resizeCreateAttempted.current = true;
      const operation = (async () => {
        try {
          const snapshot = await host.command(workspaceId, browserId, {
            type: "create",
            url,
            bounds,
            visible: false,
          });
          if (nativeSurfaceGeneration.current !== generation) return;
          recordRuntime(snapshot);
          nativePresence.current = "present";
          lastNativeBounds.current = bounds;
          await reconcileAfterNativeReady(generation);
          if (nativeSurfaceGeneration.current !== generation) return;
          nativeHistoryAvailable.current =
            sessionRef.current.history.length <= 1;
        } catch (error) {
          if (nativeSurfaceGeneration.current !== generation) return;
          nativeReady.current = false;
          nativePresence.current = "missing";
          if (nativeExpectedNavigationUrl.current === url) {
            nativeExpectedNavigationUrl.current = null;
          }
          throw error;
        }

        let deliveredUrl = url;
        while (
          mountedRef.current &&
          nativeReady.current &&
          nativeSurfaceGeneration.current === generation
        ) {
          const requestedUrl = nativeCreateRequestedUrl.current;
          if (!requestedUrl || requestedUrl === deliveredUrl) break;
          recordRuntime(
            await host.command(workspaceId, browserId, {
              type: "navigate",
              url: requestedUrl,
            }),
          );
          deliveredUrl = requestedUrl;
          nativeHistoryAvailable.current = false;
        }
      })();
      const inFlight = { generation, operation };
      nativeCreateInFlight.current = inFlight;
      try {
        await operation;
        return (
          nativeSurfaceGeneration.current === generation && nativeReady.current
        );
      } finally {
        if (nativeCreateInFlight.current === inFlight) {
          nativeCreateInFlight.current = null;
          nativeCreateRequestedUrl.current = null;
        }
      }
    },
    [browserId, host, reconcileAfterNativeReady, recordRuntime, workspaceId],
  );

  const reconstructAfterProfileReset = useCallback(
    (resetId: number): Promise<void> => {
      if (completedProfileResetIds.current.has(resetId)) {
        return Promise.resolve();
      }
      const pending = profileResetRecovery.current;
      if (pending?.resetId === resetId) return pending.operation;
      if (!beginProfileResetCycle(resetId, "reconstructing")) {
        return Promise.resolve();
      }

      const operation = (async () => {
        const url = sessionRef.current.url;
        if (!url) return;
        try {
          if (!host.available()) throw new Error(browserUnavailableMessage());
          const created = await createAndReconcileNative(url);
          if (!created) throw new Error("The browser surface is not ready");
        } catch (error) {
          if (mountedRef.current) {
            updateSession((current) =>
              failBrowserSession(
                current,
                friendlyErrorMessage(
                  error,
                  "Could not restore the in-app browser",
                ),
              ),
            );
          }
          throw error;
        }
      })();
      profileResetRecovery.current = { resetId, operation };
      return operation;
    },
    [beginProfileResetCycle, createAndReconcileNative, host, updateSession],
  );

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      cancelNativeReveal();
      nativeReady.current = false;
      if (host.available()) {
        void host
          .command(workspaceId, browserId, {
            type: "set_visible",
            visible: false,
          })
          .catch(() => undefined);
      }
    };
  }, [browserId, cancelNativeReveal, host, workspaceId]);

  useEffect(() => {
    onTitleChange?.(session.title);
  }, [onTitleChange, session.title]);

  useEffect(() => {
    if (persistTimer.current !== null) {
      window.clearTimeout(persistTimer.current);
    }
    persistTimer.current = window.setTimeout(() => {
      persistTimer.current = null;
      writeStoredBrowserSession(sessionRef.current);
    }, PERSIST_DELAY_MS);
    return () => {
      if (persistTimer.current !== null) {
        window.clearTimeout(persistTimer.current);
        persistTimer.current = null;
      }
    };
  }, [session]);

  useEffect(
    () => () => {
      // An explicit tab close removes storage before or after this component's
      // cleanup depending on React's unmount order. Do not resurrect a session
      // the workspace owner has already closed.
      if (readStoredBrowserSession(browserId)) {
        writeStoredBrowserSession(sessionRef.current);
      }
    },
    [browserId],
  );

  useEffect(() => {
    writeStoredViewport(viewport);
  }, [viewport]);

  useEffect(() => {
    if (session.loadState !== "loading") {
      setSlow(false);
      return;
    }
    const timer = window.setTimeout(() => setSlow(true), SLOW_LOAD_MS);
    return () => window.clearTimeout(timer);
  }, [session.loadState, session.url]);

  useEffect(() => {
    let cancelled = false;
    let unsubscribe: (() => void) | undefined;

    async function boot() {
      const current = sessionRef.current;
      if (!host.available()) {
        if (current.url) {
          updateSession((value) =>
            failBrowserSession(value, browserUnavailableMessage()),
          );
        }
        return;
      }
      try {
        const stop = await host.subscribe((event) => {
          if (
            !cancelled &&
            event.browserId === browserId &&
            event.workspaceId === workspaceId
          ) {
            handleHostEvent(event);
          }
        });
        if (cancelled) {
          stop();
          return;
        }
        unsubscribe = stop;
      } catch (error) {
        if (!cancelled) {
          updateSession((value) =>
            failBrowserSession(
              value,
              friendlyErrorMessage(
                error,
                "Could not connect to the in-app browser",
              ),
            ),
          );
        }
        return;
      }

      if (!current.url) return;

      const snapshotGeneration = nativeSurfaceGeneration.current;
      try {
        const snapshot = await host.command(workspaceId, browserId, {
          type: "snapshot",
        });
        if (
          cancelled ||
          nativeSurfaceGeneration.current !== snapshotGeneration
        ) {
          return;
        }
        if (
          snapshot.browserId !== browserId ||
          snapshot.workspaceId !== workspaceId
        ) {
          throw new Error(
            "The browser host returned a session from another workspace",
          );
        }
        recordRuntime(snapshot);
        if (snapshot.exists) {
          nativePresence.current = "present";
          nativeReady.current = true;
          nativeHistoryAvailable.current = true;
          if (snapshot.url) {
            updateSession((value) => {
              let next =
                snapshot.loadState === "loading"
                  ? observeBrowserNavigation(value, snapshot.url!, "loading")
                  : finishBrowserNavigation(value, snapshot.url!);
              if (snapshot.title) next = setBrowserTitle(next, snapshot.title);
              return next;
            });
            setAddress(browserDisplayAddress(snapshot.url));
          }
          await reconcileAfterNativeReady(snapshotGeneration);
        } else {
          nativePresence.current = "missing";
          await createAndReconcileNative(current.url);
        }
      } catch (error) {
        if (
          !cancelled &&
          nativeSurfaceGeneration.current === snapshotGeneration
        ) {
          updateSession((value) =>
            failBrowserSession(
              value,
              friendlyErrorMessage(error, "Could not open the in-app browser"),
            ),
          );
        }
      }
    }

    function handleHostEvent(event: BrowserHostEvent) {
      if (event.type === "profile_reset_closing") {
        if (event.resetId !== undefined) {
          beginProfileResetCycle(event.resetId, "closing");
        }
        return;
      }
      if (event.type === "profile_reset_deleting_data") {
        if (event.resetId !== undefined) {
          beginProfileResetCycle(event.resetId, "deleting");
        }
        return;
      }
      if (event.type === "profile_reset_reconstruct") {
        if (event.resetId === undefined) return;
        const resetId = event.resetId;
        if (completedProfileResetIds.current.has(resetId)) return;
        const recovery = reconstructAfterProfileReset(resetId);
        void recovery.then(
          () => {
            if (locallyInitiatedProfileResetId.current !== resetId) {
              finishProfileResetCycle(resetId);
            }
          },
          () => {
            if (locallyInitiatedProfileResetId.current !== resetId) {
              finishProfileResetCycle(resetId);
            }
          },
        );
        return;
      }

      if (
        event.documentEpoch !== undefined &&
        event.documentEpoch < nativeDocumentEpoch.current
      ) {
        return;
      }
      if (
        (event.type === "navigation_started" ||
          event.type === "navigation_finished") &&
        event.url
      ) {
        const expectedUrl = nativeExpectedNavigationUrl.current;
        if (expectedUrl && event.url !== expectedUrl) return;
        if (event.url === expectedUrl) {
          nativeExpectedNavigationUrl.current = null;
        }
      }
      if (event.documentEpoch !== undefined) {
        nativeDocumentEpoch.current = Math.max(
          nativeDocumentEpoch.current,
          event.documentEpoch,
        );
      }
      if (event.type === "navigation_started" && event.url) {
        updateSession((current) =>
          observeBrowserNavigation(current, event.url!, "loading"),
        );
        setAddress(browserDisplayAddress(event.url));
        setAddressError(null);
        updateSession((current) => ({ ...current, inspectEnabled: false }));
      } else if (event.type === "navigation_finished" && event.url) {
        updateSession((current) =>
          finishBrowserNavigation(current, event.url!),
        );
        setAddress(browserDisplayAddress(event.url));
      } else if (event.type === "same_document_navigation" && event.url) {
        updateSession((current) =>
          observeBrowserNavigation(current, event.url!, "ready"),
        );
        setAddress(browserDisplayAddress(event.url));
        setAddressError(null);
      } else if (event.type === "title_changed" && event.title) {
        updateSession((current) =>
          event.url && event.url !== current.url
            ? current
            : setBrowserTitle(current, event.title!),
        );
      } else if (event.type === "popup_blocked") {
        updateSession((current) =>
          setBrowserNotice(current, {
            kind: "popup",
            url: event.url,
            message: "This page tried to open a new window",
          }),
        );
      } else if (event.type === "download_blocked") {
        updateSession((current) =>
          setBrowserNotice(current, {
            kind: "download",
            url: event.url,
            message: "Downloads are not available in the in-app browser yet",
          }),
        );
      } else if (event.type === "navigation_blocked") {
        updateSession((current) =>
          setBrowserNotice(current, {
            kind: "blocked",
            url: event.url,
            message:
              event.message || "This address cannot open in the in-app browser",
          }),
        );
      }
      if (event.controller !== undefined || event.agentAccess !== undefined) {
        setRuntime((current) => ({
          ...(current || {
            exists: true,
            workspaceId,
            browserId,
          }),
          ...(event.controller !== undefined
            ? { controller: event.controller }
            : {}),
          ...(event.agentAccess !== undefined
            ? { agentAccess: event.agentAccess }
            : {}),
          ...(event.url !== undefined ? { url: event.url } : {}),
          ...(event.title !== undefined ? { title: event.title } : {}),
          ...(event.loadState !== undefined
            ? { loadState: event.loadState }
            : {}),
          ...(event.documentEpoch !== undefined
            ? { documentEpoch: event.documentEpoch }
            : {}),
        }));
      }
    }

    void boot();
    return () => {
      cancelled = true;
      unsubscribe?.();
    };
  }, [
    beginProfileResetCycle,
    browserId,
    createAndReconcileNative,
    finishProfileResetCycle,
    host,
    recordRuntime,
    reconstructAfterProfileReset,
    reconcileAfterNativeReady,
    updateSession,
    workspaceId,
  ]);

  useEffect(() => {
    const surface = viewportSurfaceRef.current;
    if (!surface || !host.available()) return;
    let frame: number | null = null;

    const sync = () => {
      if (frame !== null) window.cancelAnimationFrame(frame);
      frame = window.requestAnimationFrame(() => {
        frame = null;
        const bounds = readBrowserBounds(surface);
        if (!bounds) return;
        if (!nativeReady.current) {
          if (profileResetReconstructing) return;
          const url = sessionRef.current.url;
          if (
            nativePresence.current === "missing" &&
            !resizeCreateAttempted.current &&
            url
          ) {
            resizeCreateAttempted.current = true;
            void createAndReconcileNative(url)
              .then((created) => {
                if (!created || !mountedRef.current) return;
                updateSession((current) => ({
                  ...current,
                  loadState: "loading",
                  error: null,
                }));
              })
              .catch((error) => {
                if (!mountedRef.current) return;
                updateSession((current) =>
                  failBrowserSession(
                    current,
                    friendlyErrorMessage(
                      error,
                      "Could not open the in-app browser",
                    ),
                  ),
                );
              });
          }
          return;
        }
        if (sameBrowserBounds(lastNativeBounds.current, bounds)) return;
        lastNativeBounds.current = bounds;
        void host
          .command(workspaceId, browserId, { type: "set_bounds", bounds })
          .catch(() => {
            if (sameBrowserBounds(lastNativeBounds.current, bounds)) {
              lastNativeBounds.current = null;
            }
          });
      });
    };

    const observer = new ResizeObserver(sync);
    observer.observe(surface);
    window.addEventListener("resize", sync);
    sync();
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", sync);
      if (frame !== null) window.cancelAnimationFrame(frame);
    };
  }, [
    browserId,
    createAndReconcileNative,
    host,
    profileResetReconstructing,
    updateSession,
    workspaceId,
  ]);

  useEffect(() => {
    if (!nativeReady.current || !host.available()) return;
    if (!visible) {
      cancelNativeReveal();
      void host
        .command(workspaceId, browserId, {
          type: "set_visible",
          visible: false,
        })
        .catch(() => undefined);
      return;
    }

    scheduleNativeReveal();
    return cancelNativeReveal;
  }, [cancelNativeReveal, host, scheduleNativeReveal, visible]);

  async function navigate(input = address) {
    const target = browserTarget(input);
    if (!target.ok) {
      setAddressError(target.message);
      return;
    }
    setAddressError(null);
    setAddress(browserDisplayAddress(target.url));
    updateSession((current) => beginBrowserNavigation(current, target.url));

    if (!host.available()) {
      updateSession((current) =>
        failBrowserSession(current, browserUnavailableMessage()),
      );
      return;
    }
    try {
      if (nativeReady.current) {
        recordRuntime(
          await host.command(workspaceId, browserId, {
            type: "navigate",
            url: target.url,
          }),
        );
        return;
      }
      const created = await createAndReconcileNative(target.url);
      if (!created) throw new Error("The browser surface is not ready");
    } catch (error) {
      if (nativeExpectedNavigationUrl.current === target.url) {
        nativeExpectedNavigationUrl.current = null;
      }
      if (mountedRef.current) {
        updateSession((current) =>
          failBrowserSession(
            current,
            friendlyErrorMessage(error, "Could not open that address"),
          ),
        );
      }
    }
  }

  async function history(direction: -1 | 1) {
    const next = moveBrowserHistory(sessionRef.current, direction);
    if (next === sessionRef.current) return;
    updateSession(() => next);
    setAddress(next.address);
    if (nativeHistoryAvailable.current) {
      await runHostCommand(direction === -1 ? "back" : "forward");
    } else if (next.url) {
      await runHostAction({ type: "navigate", url: next.url });
    }
  }

  async function selectHistory(index: number) {
    const target = sessionRef.current.history[index];
    if (!target || target.url === sessionRef.current.url) return;
    const next = beginBrowserNavigation(sessionRef.current, target.url);
    updateSession(() => next);
    setAddress(browserDisplayAddress(target.url));
    nativeHistoryAvailable.current = false;
    await runHostAction({ type: "navigate", url: target.url });
  }

  async function runHostCommand(type: "reload" | "stop" | "back" | "forward") {
    await runHostAction({ type });
  }

  async function runHostAction(action: BrowserHostAction) {
    if (!host.available() || !nativeReady.current) return;
    try {
      recordRuntime(await host.command(workspaceId, browserId, action));
    } catch (error) {
      updateSession((current) =>
        failBrowserSession(
          current,
          friendlyErrorMessage(error, "The browser command failed"),
        ),
      );
    }
  }

  async function runAgentHostAction(action: BrowserAgentHostAction) {
    if (!host.available() || !nativeReady.current) return;
    try {
      recordRuntime(await host.command(workspaceId, browserId, action));
    } catch (error) {
      const fallback =
        action.type === "share_with_agent" ||
        action.type === "revoke_agent_access"
          ? "Could not update agent access"
          : "Could not change browser control";
      updateSession((current) =>
        setBrowserNotice(current, {
          kind: "blocked",
          message: friendlyErrorMessage(error, fallback),
        }),
      );
    }
  }

  async function reload() {
    if (!session.url) return;
    if (!nativeReady.current) {
      await retryNativeCreate();
      return;
    }
    updateSession((current) => ({
      ...current,
      loadState: "loading",
      error: null,
    }));
    await runHostCommand("reload");
  }

  async function retryNativeCreate() {
    const url = sessionRef.current.url;
    if (!url || !host.available()) return;
    try {
      const created = await createAndReconcileNative(url);
      if (!created) return;
      updateSession((current) => ({
        ...current,
        loadState: "loading",
        error: null,
      }));
    } catch (error) {
      if (!mountedRef.current) return;
      updateSession((current) =>
        failBrowserSession(
          current,
          friendlyErrorMessage(error, "Could not open the in-app browser"),
        ),
      );
    }
  }

  async function toggleInspect() {
    if (!host.available() || !nativeReady.current) return;
    const next = !sessionRef.current.inspectEnabled;
    try {
      recordRuntime(
        await host.command(
          workspaceId,
          browserId,
          next
            ? { type: "set_inspect", enabled: true }
            : { type: "remove_inspect" },
        ),
      );
    } catch (error) {
      updateSession((current) =>
        setBrowserNotice(current, {
          kind: "blocked",
          message: friendlyErrorMessage(
            error,
            next
              ? "Could not show inspect highlights"
              : "Could not hide inspect highlights",
          ),
        }),
      );
    }
  }

  async function stop() {
    await runHostCommand("stop");
    updateSession((current) => ({
      ...current,
      loadState: current.url ? "ready" : "idle",
    }));
  }

  async function openExternal(url = session.url) {
    if (!url) return;
    await host.openExternal(url).catch(() => undefined);
  }

  async function resetProfile() {
    const resetId = createProfileResetId();
    locallyInitiatedProfileResetId.current = resetId;
    beginProfileResetCycle(resetId, "closing");
    let resetError: unknown;
    try {
      await resetCodeBrowserProfile(workspaceId, browserId, resetId, host);
    } catch (error) {
      resetError = error;
    }

    let reconstructionError: unknown;
    try {
      await reconstructAfterProfileReset(resetId);
    } catch (error) {
      reconstructionError = error;
    } finally {
      locallyInitiatedProfileResetId.current = null;
      finishProfileResetCycle(resetId);
    }
    if (resetError) throw resetError;
    if (reconstructionError) throw reconstructionError;
  }
  const notice = session.notice;
  const noticeTarget = notice?.url ? validateBrowserUrl(notice.url) : null;
  const actionableNoticeUrl = noticeTarget?.ok ? noticeTarget.url : null;
  const noticeAction =
    actionableNoticeUrl && notice
      ? notice.kind === "popup"
        ? {
            label: "Open here",
            run: () => {
              updateSession((current) => setBrowserNotice(current, null));
              void navigate(actionableNoticeUrl);
            },
          }
        : notice.kind === "download"
          ? {
              label: "Open externally",
              run: () => {
                updateSession((current) => setBrowserNotice(current, null));
                void openExternal(actionableNoticeUrl);
              },
            }
          : null
      : null;
  const showNative = Boolean(session.url) && session.loadState !== "failed";

  return (
    <section
      className="flex min-h-0 flex-1 flex-col bg-background text-foreground"
      aria-label={
        session.title === "Browser" ? "Browser" : `Browser: ${session.title}`
      }
    >
      <BrowserToolbar
        session={session}
        address={address}
        addressError={addressError}
        canGoBack={canBrowserGoBack(session)}
        canGoForward={canBrowserGoForward(session)}
        controller={runtime?.controller}
        agentAccess={runtime?.agentAccess}
        engine={runtime?.engine}
        onAddressChange={(value) => {
          setAddress(value);
          if (addressError) setAddressError(null);
        }}
        onNavigate={() => void navigate()}
        onBack={() => void history(-1)}
        onForward={() => void history(1)}
        onReload={() => void reload()}
        onStop={() => void stop()}
        onStopAgent={() =>
          void runAgentHostAction({ type: "stop_agent_control" })
        }
        onTakeOver={() =>
          void runAgentHostAction({ type: "take_human_control" })
        }
        onShareAgent={() =>
          void runAgentHostAction({ type: "share_with_agent" })
        }
        onRevokeAgent={() =>
          void runAgentHostAction({ type: "revoke_agent_access" })
        }
        onSelectHistory={(index) => void selectHistory(index)}
        onOpenExternal={() => void openExternal()}
        profileResetPhase={profileResetPhase}
        onResetProfile={resetProfile}
        onOverlayOpenChange={setHistoryOpen}
        onAgentAccessOpenChange={setAgentAccessOpen}
        agentAccessOpen={agentAccessOpen}
        onToggleInspect={() => void toggleInspect()}
        inspectEnabled={session.inspectEnabled}
        viewportControl={
          <BrowserViewportControl
            viewport={viewport}
            renderedWidth={renderedViewportWidth}
            onViewportChange={setViewport}
            onOverlayOpenChange={setViewportOpen}
            disabled={!session.url}
          />
        }
      />
      {notice && (
        <BrowserNoticeRow
          message={notice.message}
          tone={notice.kind === "blocked" ? "critical" : "warning"}
          actionLabel={noticeAction?.label}
          onAction={noticeAction?.run}
          onDismiss={() =>
            updateSession((current) => setBrowserNotice(current, null))
          }
        />
      )}
      {!notice && slow && session.loadState === "loading" && (
        <BrowserNoticeRow
          message="This page is taking longer than expected"
          tone="info"
          actionLabel="Open externally"
          onAction={() => void openExternal()}
          onDismiss={() => setSlow(false)}
        />
      )}
      <div ref={surfaceRef} className="relative min-h-0 flex-1 overflow-hidden">
        <ViewportSurface
          viewport={viewport}
          showNative={showNative}
          surfaceRef={viewportSurfaceRef}
          onViewportBoundsChange={setRenderedViewportWidth}
        >
          {!showNative && (
            <BrowserFallback
              error={session.error}
              hasUrl={Boolean(session.url)}
              onRetry={session.url ? () => void retryNativeCreate() : undefined}
              onOpenExternal={
                session.url ? () => void openExternal() : undefined
              }
            />
          )}
        </ViewportSurface>
      </div>
    </section>
  );
}

export function BrowserFallback({
  error,
  hasUrl,
  onRetry,
  onOpenExternal,
}: {
  error: string | null;
  hasUrl: boolean;
  onRetry?: () => void;
  onOpenExternal?: () => void;
}) {
  const Icon = error ? TriangleAlert : Globe2;
  return (
    <div className="relative isolate grid h-full min-h-72 place-items-center overflow-hidden bg-page-background/35 px-6 py-12">
      <div
        aria-hidden
        className="pointer-events-none absolute inset-0 -z-10 opacity-45 [background-image:linear-gradient(to_right,color-mix(in_oklch,var(--border-subtle)_42%,transparent)_1px,transparent_1px),linear-gradient(to_bottom,color-mix(in_oklch,var(--border-subtle)_42%,transparent)_1px,transparent_1px)] [background-size:32px_32px] [mask-image:linear-gradient(to_bottom,transparent,black_24%,black_74%,transparent)]"
      />
      <div className="w-full max-w-lg text-left">
        <div
          className={cn(
            "grid size-10 place-items-center rounded-xl border bg-background shadow-[0_8px_28px_color-mix(in_oklch,var(--foreground)_7%,transparent)]",
            error
              ? "border-critical-border text-critical"
              : "border-border-subtle text-foreground",
          )}
        >
          <Icon className="size-4.5" />
        </div>
        <p className="mt-5 text-2xs font-semibold tracking-[0.16em] text-muted-foreground uppercase">
          Workspace browser
        </p>
        <h2 className="mt-1.5 max-w-md text-xl font-semibold tracking-[-0.025em] text-balance">
          {error
            ? "This page did not open"
            : "Bring the live work into the workspace"}
        </h2>
        <p className="mt-2 max-w-md text-sm leading-6 text-pretty text-muted-foreground">
          {error ||
            "Open a local preview, documentation, or a pull request here. The browser stays attached to this workspace so you and its agents can work from the same page."}
        </p>
        {!error && (
          <div className="mt-5 flex flex-wrap gap-x-5 gap-y-2 text-xs text-muted-foreground">
            <span className="inline-flex items-center gap-1.5">
              <Braces className="size-3.5 text-foreground/70" />
              Local previews
            </span>
            <span className="inline-flex items-center gap-1.5">
              <Link2 className="size-3.5 text-foreground/70" />
              Docs and reviews
            </span>
            <span className="inline-flex items-center gap-1.5">
              <Bot className="size-3.5 text-foreground/70" />
              Agent inspection
            </span>
          </div>
        )}
        {hasUrl && (onRetry || onOpenExternal) && (
          <div className="mt-5 flex flex-wrap items-center gap-2">
            {onRetry && (
              <Button
                type="button"
                variant="secondary"
                size="sm"
                onClick={onRetry}
              >
                <RefreshCw />
                Try again
              </Button>
            )}
            {onOpenExternal && (
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={onOpenExternal}
              >
                <ExternalLink />
                Open externally
              </Button>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

/**
 * Centers and clips the native webview column inside the browser surface.
 *
 * For Fit, the column fills the surface. For fixed presets and custom widths,
 * the column is constrained to the target width (clamped to the surface),
 * centered horizontally, and reports its actual rendered width so the toolbar
 * can display it without a second control. The native webview is positioned
 * at the column's bounds so it visually matches the simulated viewport.
 *
 * A muted backdrop fills the gutter on either side of a fixed/custom column so
 * the simulated device frame reads as a deliberate inset, not a broken layout.
 */
function ViewportSurface({
  viewport,
  showNative,
  surfaceRef,
  onViewportBoundsChange,
  children,
}: {
  viewport: BrowserViewport;
  showNative: boolean;
  surfaceRef: RefObject<HTMLDivElement | null>;
  onViewportBoundsChange: (width: number | null) => void;
  children: ReactNode;
}) {
  useEffect(() => {
    const el = surfaceRef.current;
    if (!el) return;
    let frame: number | null = null;
    const sync = () => {
      if (frame !== null) return;
      frame = window.requestAnimationFrame(() => {
        frame = null;
        const rect = surfaceRef.current?.getBoundingClientRect();
        onViewportBoundsChange(rect && rect.width > 0 ? rect.width : null);
      });
    };
    const observer = new ResizeObserver(sync);
    observer.observe(el);
    sync();
    return () => {
      observer.disconnect();
      if (frame !== null) window.cancelAnimationFrame(frame);
    };
  }, [surfaceRef, onViewportBoundsChange]);

  const isFit = viewport.preset === "fit";
  const targetWidth = isFit ? null : viewportTargetWidth(viewport);
  const style: CSSProperties = isFit
    ? {}
    : { width: targetWidth ? `${targetWidth}px` : undefined, maxWidth: "100%" };

  return (
    <div className="absolute inset-0 flex justify-center overflow-hidden">
      {!isFit && (
        <div
          aria-hidden
          className="pointer-events-none absolute inset-0 bg-muted/30"
        />
      )}
      <div
        ref={surfaceRef}
        className={cn(
          "relative min-h-0",
          isFit ? "flex-1 w-full" : "h-full shrink-0",
        )}
        style={style}
      >
        {showNative ? null : children}
      </div>
    </div>
  );
}

function readBrowserBounds(element: HTMLElement | null): BrowserBounds | null {
  if (!element) return null;
  const bounds = element.getBoundingClientRect();
  if (bounds.width < 1 || bounds.height < 1) return null;
  return {
    x: bounds.x,
    y: bounds.y,
    width: bounds.width,
    height: bounds.height,
  };
}

function sameBrowserBounds(
  left: BrowserBounds | null,
  right: BrowserBounds,
): boolean {
  return Boolean(
    left &&
      left.x === right.x &&
      left.y === right.y &&
      left.width === right.width &&
      left.height === right.height,
  );
}
