import { useEffect, useRef, useState } from "react";
import { AppWindow } from "lucide-react";
import { useApp } from "./AppContext";
import { createMcpAppBridge, type McpAppBridge } from "./McpAppBridge";
import { useTheme } from "./theme";

/**
 * The sandboxed surface for an MCP Apps view.
 *
 * The transcript event carries only a typed reference (server namespace and
 * `ui://` URI); this card resolves it through the dedicated view route and
 * renders the returned document inside an iframe that is never same-origin
 * with the app: `sandbox="allow-scripts"` deliberately omits
 * `allow-same-origin`, so the view runs with an opaque origin — no cookies,
 * no storage, no reach into Tidebreak's DOM or its bearer token.
 *
 * The card also runs the MCP Apps host bridge: it answers the view's
 * `ui/initialize`, and once the view reports ready it forwards the call's
 * input and result — fetched as an opaque envelope the transcript itself
 * never reads — so an interactive view lights up with real data.
 */
type ViewState =
  | { kind: "loading" }
  | { kind: "unavailable" }
  | { kind: "ready"; url: string };

const DEFAULT_FRAME_HEIGHT = 96;

export function McpAppCard({
  server,
  resourceUri,
  chatId,
  callId,
}: {
  server: string;
  resourceUri: string;
  /** When both ids are present the bridge delivers the call's result. */
  chatId?: string;
  callId?: string;
}) {
  const { client } = useApp();
  const { resolved: resolvedTheme } = useTheme();
  const [state, setState] = useState<ViewState>({ kind: "loading" });
  const [frameHeight, setFrameHeight] = useState(DEFAULT_FRAME_HEIGHT);
  const frameRef = useRef<HTMLIFrameElement | null>(null);
  const bridgeRef = useRef<McpAppBridge | null>(null);

  const themeRef = useRef(resolvedTheme);
  themeRef.current = resolvedTheme;

  // The bridge listener must exist before the frame's document runs, or the
  // view's ui/initialize request is lost and it hangs until its timeout.
  // One bridge per mount: recreating it (e.g. on a theme change) would
  // destroy the buffered handshake state mid-flight and strand the view
  // without its data, so the theme is read through a ref instead.
  useEffect(() => {
    const bridge = createMcpAppBridge({
      frame: () => frameRef.current?.contentWindow ?? null,
      theme: () => themeRef.current,
      onHeight: setFrameHeight,
    });
    bridgeRef.current = bridge;
    window.addEventListener("message", bridge.handleMessage);
    return () => {
      window.removeEventListener("message", bridge.handleMessage);
      bridge.dispose();
      bridgeRef.current = null;
    };
  }, []);

  useEffect(() => {
    if (!chatId || !callId) return;
    let cancelled = false;
    void client
      .getMcpAppPayload(chatId, callId)
      .then((payload) => {
        if (!cancelled) bridgeRef.current?.deliverPayload(payload);
      })
      .catch(() => {
        // Without a payload the view still renders; it just waits for data.
      });
    return () => {
      cancelled = true;
    };
  }, [client, chatId, callId]);

  useEffect(() => {
    let cancelled = false;
    setState({ kind: "loading" });
    // The document is served by the host with its own strict CSP — a frame
    // minted here as a blob would inherit the app's policy and refuse the
    // view's inline script. The address is a single-use capability, since an
    // iframe cannot carry the API bearer.
    client
      .createMcpViewFrame(server, resourceUri)
      .then((session) => {
        if (cancelled) return;
        setState({ kind: "ready", url: client.baseUrl + session.frame_path });
      })
      .catch(() => {
        if (!cancelled) setState({ kind: "unavailable" });
      });
    return () => {
      cancelled = true;
    };
  }, [client, server, resourceUri]);

  return (
    <section
      className="bg-background max-w-prose overflow-hidden rounded-lg border"
      aria-label={`App view from MCP server ${server}`}
    >
      {/* The one host-drawn provenance mark: embedded documents must stay
          visually attributable to their server, but the label costs one slim
          row — the sandbox itself is the iframe attribute, not this text. */}
      <div
        className="text-muted-foreground flex w-full min-w-0 items-center gap-1.5 px-2.5 py-1 text-xs font-medium"
        title={`App view from ${server} · sandboxed`}
      >
        <AppWindow className="size-3.5 shrink-0" aria-hidden="true" />
        <span className="truncate">{server}</span>
      </div>
      <div className="border-t">
        {state.kind === "loading" && (
          <p className="text-muted-foreground p-3 text-xs">Loading view…</p>
        )}
        {state.kind === "unavailable" && (
          <p className="text-muted-foreground p-3 text-xs">
            This view is unavailable. Reconnect the “{server}” MCP server in
            Settings to refresh it.
          </p>
        )}
        {state.kind === "ready" && (
          <iframe
            ref={frameRef}
            title={`MCP App view from ${server}`}
            src={state.url}
            sandbox="allow-scripts"
            referrerPolicy="no-referrer"
            className="w-full border-0"
            style={{ height: frameHeight }}
          />
        )}
      </div>
    </section>
  );
}
