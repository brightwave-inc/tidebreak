import { useEffect, useState } from "react";
import { AppWindow, ShieldCheck } from "lucide-react";
import { useApp } from "./AppContext";

/**
 * The sandboxed surface for an MCP Apps view.
 *
 * The transcript event carries only a typed reference (server namespace and
 * `ui://` URI); this card resolves it through the dedicated view route and
 * renders the returned document inside an iframe that is never same-origin
 * with the app: `sandbox="allow-scripts"` deliberately omits
 * `allow-same-origin`, so the view runs with an opaque origin — no cookies,
 * no storage, no reach into OpenWave's DOM or its bearer token.
 */
type ViewState =
  | { kind: "loading" }
  | { kind: "unavailable" }
  | { kind: "ready"; url: string };

export function McpAppCard({
  server,
  resourceUri,
}: {
  server: string;
  resourceUri: string;
}) {
  const { client } = useApp();
  const [state, setState] = useState<ViewState>({ kind: "loading" });

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
      <div className="flex w-full items-center justify-between gap-2 px-2.5 py-1.5">
        <span className="text-muted-foreground flex min-w-0 items-center gap-1.5 text-xs font-medium">
          <AppWindow className="size-3.5 shrink-0" aria-hidden="true" />
          <span className="truncate">App view · {server}</span>
        </span>
        <span className="text-muted-foreground flex items-center gap-1 text-xs">
          <ShieldCheck className="size-3.5 shrink-0" aria-hidden="true" />
          Sandboxed
        </span>
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
            title={`MCP App view from ${server}`}
            src={state.url}
            sandbox="allow-scripts"
            referrerPolicy="no-referrer"
            className="h-96 w-full border-0"
          />
        )}
      </div>
    </section>
  );
}
