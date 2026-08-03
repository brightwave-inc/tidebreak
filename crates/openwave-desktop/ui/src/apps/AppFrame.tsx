import { useEffect, useRef, useState } from "react";

import { AppInvokeRefusalError } from "@/api";
import { createMcpAppBridge, type McpAppBridge } from "@/McpAppBridge";
import { useTheme } from "@/theme";
import type { AppsApis } from "./appsApis";

/**
 * The running app: one stored revision in the same sandbox the MCP App card
 * uses — `sandbox="allow-scripts"` without `allow-same-origin`, so the bundle
 * runs with an opaque origin and no reach into OpenWave's DOM, storage, or
 * bearer — plus the host bridge with both invoke legs wired in. The frame
 * posts `tools/call` or `operations/call`; the bridge forwards either to the
 * bearer-authenticated invoke route and posts the result back, both
 * directions opaque passthrough.
 *
 * A `consent_required` refusal mid-session (revoked, or a server reconfigured
 * while the app was open) is reported upward so the host can re-present the
 * consent sheet; the frame itself just sees a rejected call.
 */
type FrameState =
  | { kind: "loading" }
  | { kind: "unavailable" }
  | { kind: "ready"; url: string };

const DEFAULT_FRAME_HEIGHT = 384;

export function AppFrame({
  appId,
  name,
  apis,
  onConsentRequired,
}: {
  appId: string;
  /** Display name, for the frame's accessible title only. */
  name: string;
  apis: AppsApis;
  onConsentRequired?: () => void;
}) {
  const { resolved: resolvedTheme } = useTheme();
  const [state, setState] = useState<FrameState>({ kind: "loading" });
  const [frameHeight, setFrameHeight] = useState(DEFAULT_FRAME_HEIGHT);
  const frameRef = useRef<HTMLIFrameElement | null>(null);
  const bridgeRef = useRef<McpAppBridge | null>(null);

  const themeRef = useRef(resolvedTheme);
  themeRef.current = resolvedTheme;
  const onConsentRequiredRef = useRef(onConsentRequired);
  onConsentRequiredRef.current = onConsentRequired;

  // One bridge per mount, created before the frame's document runs — the
  // same ordering contract as McpAppCard, for the same reason.
  useEffect(() => {
    const bridge = createMcpAppBridge({
      frame: () => frameRef.current?.contentWindow ?? null,
      theme: () => themeRef.current,
      onHeight: setFrameHeight,
      invokeTool: async (tool, args) => {
        try {
          return await apis.invoke(appId, tool, args);
        } catch (error) {
          if (
            error instanceof AppInvokeRefusalError &&
            error.kind === "consent_required"
          ) {
            onConsentRequiredRef.current?.();
          }
          throw error;
        }
      },
      invokeOperation: async (operationId, parameters, body) => {
        try {
          return await apis.invokeOperation(appId, operationId, parameters, body);
        } catch (error) {
          if (
            error instanceof AppInvokeRefusalError &&
            error.kind === "consent_required"
          ) {
            onConsentRequiredRef.current?.();
          }
          throw error;
        }
      },
    });
    bridgeRef.current = bridge;
    window.addEventListener("message", bridge.handleMessage);
    return () => {
      window.removeEventListener("message", bridge.handleMessage);
      bridge.dispose();
      bridgeRef.current = null;
    };
  }, [apis, appId]);

  useEffect(() => {
    let cancelled = false;
    setState({ kind: "loading" });
    // The bundle is served by the host under its own strict CSP; the address
    // is a single-use capability because an iframe cannot carry the bearer.
    apis
      .viewSession(appId)
      .then((session) => {
        if (cancelled) return;
        setState({ kind: "ready", url: apis.baseUrl + session.frame_path });
      })
      .catch(() => {
        if (!cancelled) setState({ kind: "unavailable" });
      });
    return () => {
      cancelled = true;
    };
  }, [apis, appId]);

  return (
    <div className="bg-background mx-4 overflow-hidden rounded-lg border">
      {state.kind === "loading" && (
        <p className="text-muted-foreground p-3 text-xs">Opening app…</p>
      )}
      {state.kind === "unavailable" && (
        <p className="text-muted-foreground p-3 text-xs">
          This app could not be opened. Its stored revision may be missing —
          try again, or delete the app.
        </p>
      )}
      {state.kind === "ready" && (
        <iframe
          ref={frameRef}
          title={`App: ${name}`}
          src={state.url}
          sandbox="allow-scripts"
          referrerPolicy="no-referrer"
          className="w-full border-0"
          style={{ height: frameHeight }}
        />
      )}
    </div>
  );
}
