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
  type BrowserBounds,
  type BrowserHostAction,
  type BrowserHostEvent,
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
  const [slow, setSlow] = useState(false);
  const [runtime, setRuntime] = useState<BrowserHostSnapshot | null>(null);
  const surfaceRef = useRef<HTMLDivElement | null>(null);
  const mountedRef = useRef(true);
  const nativeReady = useRef(false);
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

  const scheduleNativeReveal = useCallback(() => {
    cancelNativeReveal();
    if (
      !mountedRef.current ||
      !nativeReady.current ||
      !visibleRef.current ||
      !host.available()
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
        !host.available()
      ) {
        return;
      }
      void host
        .command(workspaceId, browserId, { type: "set_visible", visible: true })
        .catch(() => undefined);
    });
    nativeRevealFrame.current = frame;
  }, [browserId, cancelNativeReveal, host, workspaceId]);

  /**
   * Shared post-ready reconciliation for create and existing-view paths.
   *
   * Reads the live viewport-surface bounds (not a stale pre-await capture),
   * applies them, then reveals the native surface through the existing
   * cancellable one-animation-frame mechanism so an in-flight overlay
   * handoff is never overpainted.  Hides immediately when the tab is
   * unmounted or obscured.
   */
  const reconcileAfterNativeReady = useCallback(async () => {
    if (!mountedRef.current) {
      nativeReady.current = false;
      cancelNativeReveal();
      await host.command(workspaceId, browserId, {
        type: "set_visible",
        visible: false,
      });
      return;
    }
    nativeReady.current = true;
    const bounds = readBrowserBounds(viewportSurfaceRef.current);
    if (bounds && !sameBrowserBounds(lastNativeBounds.current, bounds)) {
      lastNativeBounds.current = bounds;
      recordRuntime(
        await host.command(workspaceId, browserId, {
          type: "set_bounds",
          bounds,
        }),
      );
    }
    if (!mountedRef.current || !visibleRef.current) {
      if (!mountedRef.current) nativeReady.current = false;
      cancelNativeReveal();
      recordRuntime(
        await host.command(workspaceId, browserId, {
          type: "set_visible",
          visible: false,
        }),
      );
      return;
    }
    scheduleNativeReveal();
  }, [
    browserId,
    cancelNativeReveal,
    host,
    recordRuntime,
    scheduleNativeReveal,
    workspaceId,
  ]);

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
            failBrowserSession(
              value,
              "The in-app browser is available in the Tidebreak desktop app",
            ),
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
      const bounds = readBrowserBounds(viewportSurfaceRef.current);
      if (!bounds) return;

      try {
        const snapshot = await host.command(workspaceId, browserId, {
          type: "snapshot",
        });
        if (cancelled) return;
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
          await reconcileAfterNativeReady();
        } else {
          recordRuntime(
            await host.command(workspaceId, browserId, {
              type: "create",
              url: current.url,
              bounds,
              visible: false,
            }),
          );
          lastNativeBounds.current = bounds;
          await reconcileAfterNativeReady();
          nativeHistoryAvailable.current = current.history.length <= 1;
        }
      } catch (error) {
        if (!cancelled) {
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
    browserId,
    host,
    recordRuntime,
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
        if (
          !bounds ||
          !nativeReady.current ||
          sameBrowserBounds(lastNativeBounds.current, bounds)
        ) {
          return;
        }
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
  }, [browserId, host, workspaceId]);

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
        failBrowserSession(
          current,
          "The in-app browser is available in the Tidebreak desktop app",
        ),
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
      const bounds = readBrowserBounds(viewportSurfaceRef.current);
      if (!bounds) throw new Error("The browser surface is not ready");
      recordRuntime(
        await host.command(workspaceId, browserId, {
          type: "create",
          url: target.url,
          bounds,
          visible: false,
        }),
      );
      lastNativeBounds.current = bounds;
      await reconcileAfterNativeReady();
      nativeHistoryAvailable.current = sessionRef.current.history.length <= 1;
    } catch (error) {
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
    updateSession((current) => ({
      ...current,
      loadState: "loading",
      error: null,
    }));
    await runHostCommand("reload");
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
        onOverlayOpenChange={setHistoryOpen}
        onAgentAccessOpenChange={setAgentAccessOpen}
        agentAccessOpen={agentAccessOpen}
        onToggleInspect={() => {
          const next = !session.inspectEnabled;
          updateSession((current) => ({ ...current, inspectEnabled: next }));
          void runHostAction(
            next
              ? { type: "set_inspect", enabled: true }
              : { type: "remove_inspect" },
          );
        }}
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
              onRetry={session.url ? () => void navigate() : undefined}
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
        <p className="mt-5 text-[10px] font-semibold tracking-[0.16em] text-muted-foreground uppercase">
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
          <div className="mt-5 flex flex-wrap gap-x-5 gap-y-2 text-[11px] text-muted-foreground">
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
