import {
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { ExternalLink, Globe2, RefreshCw } from "lucide-react";

import { Button } from "@/components/ui/button";
import { friendlyErrorMessage } from "@/lib/utils";
import { BrowserNoticeRow, BrowserToolbar } from "./BrowserToolbar";
import {
  type BrowserBounds,
  type BrowserHostEvent,
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
  const [slow, setSlow] = useState(false);
  const surfaceRef = useRef<HTMLDivElement | null>(null);
  const mountedRef = useRef(true);
  const nativeReady = useRef(false);
  const nativeHistoryAvailable = useRef(false);
  const lastNativeBounds = useRef<BrowserBounds | null>(null);
  const persistTimer = useRef<number | null>(null);
  const sessionRef = useRef(session);
  const visible = !obscured && !historyOpen && session.loadState !== "failed";
  const visibleRef = useRef(visible);

  sessionRef.current = session;
  visibleRef.current = visible;

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

  const settleCreatedNativeView = useCallback(async () => {
    nativeReady.current = true;
    await host.command(browserId, {
      type: "set_visible",
      visible: mountedRef.current && visibleRef.current,
    });
    const bounds = readBrowserBounds(surfaceRef.current);
    if (bounds && !sameBrowserBounds(lastNativeBounds.current, bounds)) {
      lastNativeBounds.current = bounds;
      await host.command(browserId, { type: "set_bounds", bounds });
    }
  }, [browserId, host]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

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
          if (!cancelled && event.sessionId === browserId) {
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
              friendlyErrorMessage(error, "Could not connect to the in-app browser"),
            ),
          );
        }
        return;
      }

      if (!current.url) return;
      const bounds = readBrowserBounds(surfaceRef.current);
      if (!bounds) return;

      try {
        const snapshot = await host.command(browserId, { type: "snapshot" });
        if (cancelled) return;
        if (snapshot.exists) {
          nativeReady.current = true;
          nativeHistoryAvailable.current = true;
          if (snapshot.url) {
            updateSession((value) =>
              finishBrowserNavigation(value, snapshot.url!),
            );
            setAddress(browserDisplayAddress(snapshot.url));
          }
          await host.command(browserId, { type: "set_bounds", bounds });
          lastNativeBounds.current = bounds;
          await host.command(browserId, {
            type: "set_visible",
            visible: visibleRef.current,
          });
        } else {
          await host.command(browserId, {
            type: "create",
            url: current.url,
            bounds,
            visible: visibleRef.current,
          });
          lastNativeBounds.current = bounds;
          await settleCreatedNativeView();
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
            message: event.message || "This address cannot open in the in-app browser",
          }),
        );
      }
    }

    void boot();
    return () => {
      cancelled = true;
      unsubscribe?.();
      if (nativeReady.current && host.available()) {
        void host
          .command(browserId, { type: "set_visible", visible: false })
          .catch(() => undefined);
      }
    };
  }, [browserId, host, settleCreatedNativeView, updateSession]);

  useEffect(() => {
    const surface = surfaceRef.current;
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
          .command(browserId, { type: "set_bounds", bounds })
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
  }, [browserId, host]);

  useEffect(() => {
    if (!nativeReady.current || !host.available()) return;
    void host
      .command(browserId, { type: "set_visible", visible })
      .catch(() => undefined);
  }, [browserId, host, visible]);

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
        await host.command(browserId, { type: "navigate", url: target.url });
        return;
      }
      const bounds = readBrowserBounds(surfaceRef.current);
      if (!bounds) throw new Error("The browser surface is not ready");
      await host.command(browserId, {
        type: "create",
        url: target.url,
        bounds,
        visible,
      });
      lastNativeBounds.current = bounds;
      await settleCreatedNativeView();
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

  async function runHostAction(
    action:
      | { type: "reload" | "stop" | "back" | "forward" }
      | { type: "navigate"; url: string },
  ) {
    if (!host.available() || !nativeReady.current) return;
    try {
      await host.command(browserId, action);
    } catch (error) {
      updateSession((current) =>
        failBrowserSession(
          current,
          friendlyErrorMessage(error, "The browser command failed"),
        ),
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
  const noticeAction = actionableNoticeUrl && notice
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
      aria-label={session.title === "Browser" ? "Browser" : `Browser: ${session.title}`}
    >
      <BrowserToolbar
        session={session}
        address={address}
        addressError={addressError}
        canGoBack={canBrowserGoBack(session)}
        canGoForward={canBrowserGoForward(session)}
        onAddressChange={(value) => {
          setAddress(value);
          if (addressError) setAddressError(null);
        }}
        onNavigate={() => void navigate()}
        onBack={() => void history(-1)}
        onForward={() => void history(1)}
        onReload={() => void reload()}
        onStop={() => void stop()}
        onSelectHistory={(index) => void selectHistory(index)}
        onOpenExternal={() => void openExternal()}
        onOverlayOpenChange={setHistoryOpen}
      />
      {notice && (
        <BrowserNoticeRow
          message={notice.message}
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
          actionLabel="Open externally"
          onAction={() => void openExternal()}
          onDismiss={() => setSlow(false)}
        />
      )}
      <div ref={surfaceRef} className="relative min-h-0 flex-1 overflow-hidden">
        {!showNative && (
          <BrowserFallback
            error={session.error}
            hasUrl={Boolean(session.url)}
            onRetry={session.url ? () => void navigate() : undefined}
            onOpenExternal={session.url ? () => void openExternal() : undefined}
          />
        )}
      </div>
    </section>
  );
}

function BrowserFallback({
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
  return (
    <div className="grid h-full min-h-72 place-items-center px-6 py-10">
      <div className="max-w-sm text-center">
        <Globe2 className="mx-auto size-7 text-muted-foreground" />
        <h2 className="mt-4 text-sm font-medium">
          {error ? "Could not open this page" : "Open a page in this workspace"}
        </h2>
        <p className="mt-1.5 text-xs leading-5 text-muted-foreground">
          {error ||
            "Use the address bar for a local development server, documentation, or a pull request."}
        </p>
        {hasUrl && (onRetry || onOpenExternal) && (
          <div className="mt-4 flex items-center justify-center gap-2">
            {onRetry && (
              <Button type="button" variant="secondary" size="sm" onClick={onRetry}>
                <RefreshCw />
                Try again
              </Button>
            )}
            {onOpenExternal && (
              <Button type="button" variant="outline" size="sm" onClick={onOpenExternal}>
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
