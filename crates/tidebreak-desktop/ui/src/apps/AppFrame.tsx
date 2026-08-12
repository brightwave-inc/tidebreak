import { useEffect, useRef, useState } from "react";

import { AppInvokeRefusalError } from "@/api";
import { createMcpAppBridge, type McpAppBridge } from "@/McpAppBridge";
import { useTheme } from "@/theme";
import type { AppsApis } from "./appsApis";

/**
 * The running app: one stored revision in the same sandbox the MCP App card
 * uses — `sandbox="allow-scripts"` without `allow-same-origin`, so the bundle
 * runs with an opaque origin and no reach into Tidebreak's DOM, storage, or
 * bearer — plus the host bridge with the invoke legs wired in. The frame
 * posts `operations/call` or `fs/*`; the bridge forwards either to the
 * bearer-authenticated invoke route and posts the result back, both
 * directions opaque passthrough.
 *
 * A `consent_required` refusal mid-session (revoked, or a server reconfigured
 * while the app was open) is reported upward so the host can re-present the
 * consent sheet; a `gateway_authorization_required` refusal is reported the
 * same way so the host can offer a connect prompt. The frame itself just sees
 * a rejected call in both cases.
 *
 * Unlike the inline MCP App card, this frame fills whatever space the panel
 * gives it — the bridge's app-reported height is ignored and tall content
 * scrolls inside the sandbox.
 */
type FrameState =
  | { kind: "loading" }
  | { kind: "unavailable" }
  | { kind: "ready"; url: string };

export function AppFrame({
  appId,
  name,
  apis,
  onConsentRequired,
  onGatewayConnectRequired,
}: {
  appId: string;
  /** Display name, for the frame's accessible title only. */
  name: string;
  apis: AppsApis;
  onConsentRequired?: () => void;
  /**
   * The gateway refused a relayed call for want of a credential only the
   * viewer can supply, and only at the gateway. Reported upward with the
   * server's message so the host can offer the connect affordance; nothing
   * here can resolve it.
   */
  onGatewayConnectRequired?: (message: string) => void;
}) {
  const { resolved: resolvedTheme } = useTheme();
  const [state, setState] = useState<FrameState>({ kind: "loading" });
  const frameRef = useRef<HTMLIFrameElement | null>(null);
  const bridgeRef = useRef<McpAppBridge | null>(null);

  const themeRef = useRef(resolvedTheme);
  themeRef.current = resolvedTheme;
  const onConsentRequiredRef = useRef(onConsentRequired);
  onConsentRequiredRef.current = onConsentRequired;
  const onGatewayConnectRequiredRef = useRef(onGatewayConnectRequired);
  onGatewayConnectRequiredRef.current = onGatewayConnectRequired;

  // One bridge per mount, created before the frame's document runs — the
  // same ordering contract as McpAppCard, for the same reason.
  useEffect(() => {
    // Every leg reports the two refusals the host acts on and rethrows: the
    // frame still sees a rejected call either way.
    const reportRefusals = (error: unknown) => {
      if (error instanceof AppInvokeRefusalError) {
        if (error.kind === "consent_required") onConsentRequiredRef.current?.();
        if (error.kind === "gateway_authorization_required")
          onGatewayConnectRequiredRef.current?.(error.message);
      }
      throw error;
    };
    const bridge = createMcpAppBridge({
      frame: () => frameRef.current?.contentWindow ?? null,
      theme: () => themeRef.current,
      invokeOperation: (operationId, parameters, body) =>
        apis
          .invokeOperation(appId, operationId, parameters, body)
          .catch(reportRefusals),
      invokeGatewayOperation: (
        gatewayApp,
        operationId,
        pathParameters,
        query,
        body,
      ) =>
        apis
          .invokeGatewayOperation(
            appId,
            gatewayApp,
            operationId,
            pathParameters,
            query,
            body,
          )
          .catch(reportRefusals),
      invokeFolder: (folder, op, path, contentBase64, replace) =>
        apis
          .invokeFolder(appId, folder, op, path, contentBase64, replace)
          .catch(reportRefusals),
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
    <div className="bg-background mx-4 flex min-h-0 flex-1 flex-col overflow-hidden rounded-lg border">
      {state.kind === "loading" && (
        <p className="text-muted-foreground p-3 text-xs">Opening app…</p>
      )}
      {state.kind === "unavailable" && (
        <p className="text-muted-foreground p-3 text-xs">
          This app could not be opened. Its stored revision may be missing — try
          again, or delete the app.
        </p>
      )}
      {state.kind === "ready" && (
        <iframe
          ref={frameRef}
          title={`App: ${name}`}
          src={state.url}
          sandbox="allow-scripts"
          referrerPolicy="no-referrer"
          className="w-full flex-1 border-0"
        />
      )}
    </div>
  );
}
