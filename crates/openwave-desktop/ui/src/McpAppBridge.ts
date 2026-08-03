/**
 * The host side of the MCP Apps postMessage protocol.
 *
 * The embedded view (built on `@modelcontextprotocol/ext-apps`) speaks plain
 * JSON-RPC 2.0 over `window.postMessage`: it sends `ui/initialize` first,
 * follows with a `ui/notifications/initialized` notification, and then
 * expects the tool's input and result as notifications. The JSON-RPC
 * envelope is strict on the view side, so replies carry exactly the standard
 * fields and nothing else.
 *
 * The frame is sandboxed without `allow-same-origin`, so its origin is
 * opaque; identity is established by `event.source` alone, and outbound
 * messages use the `"*"` target origin (a `"null"` target would not
 * deliver). Anything that is not JSON-RPC from our frame is ignored;
 * requests we do not implement get a standard method-not-found error so the
 * view degrades without hanging.
 */

export const MCP_APPS_PROTOCOL_VERSION = "2026-01-26";

import {
  AppInvokeRefusalError,
  type AppInvokeResult,
  type AppRestInvokeResult,
  type McpAppPayload,
} from "./api";

export type { McpAppPayload };

/**
 * Executes one tool call on the embedded view's behalf. Local-app hosts wire
 * this to `POST /apps/{id}/invoke`; both the arguments and the result are
 * opaque passthrough the bridge forwards without interpreting. A rejection
 * becomes a JSON-RPC error reply — a typed [`AppInvokeRefusalError`] keeps
 * its machine-readable kind in `error.data` (the refusal envelope is
 * host-authored, so surfacing it is not interpretation).
 */
export type AppToolInvoker = (tool: string, args: unknown) => Promise<AppInvokeResult>;

/**
 * Executes one granted REST operation on the embedded view's behalf — the
 * `operations/call` sibling of [`AppToolInvoker`], wired to the same invoke
 * route. Parameters, body, and the `{status, content_type, body_base64}`
 * result are opaque passthrough in both directions; rejections map to
 * JSON-RPC errors exactly as tool-call rejections do.
 */
export type AppOperationInvoker = (
  operationId: string,
  parameters: unknown,
  body: unknown,
) => Promise<AppRestInvokeResult>;

const MIN_FRAME_HEIGHT = 160;
const MAX_FRAME_HEIGHT = 800;

export type McpAppBridge = {
  /** Window `message` handler. Filters to the bridged frame's source. */
  handleMessage: (event: MessageEvent) => void;
  /** Provide the tool payload; delivered once the view has initialized. */
  deliverPayload: (payload: McpAppPayload) => void;
  /** Stop posting anything further (frame unmounting). */
  dispose: () => void;
};

export function createMcpAppBridge(options: {
  /** The bridged frame's window, when mounted. */
  frame: () => Window | null;
  /** Read lazily when the view initializes, so one bridge — and its
   * buffered handshake state — survives a theme change. */
  theme: () => "light" | "dark";
  onHeight?: (height: number) => void;
  /**
   * When present, the view may call `tools/call` and the bridge forwards it
   * here. Absent — the MCP App transcript card — `tools/call` refuses with
   * method-not-found exactly as before: a view only gets the methods its
   * host deliberately declared.
   */
  invokeTool?: AppToolInvoker;
  /**
   * When present, the view may call `operations/call` and the bridge
   * forwards it here — the REST sibling of `invokeTool`, under the same
   * rule: absent, the method refuses with method-not-found, so a view only
   * gets the methods its host deliberately declared.
   */
  invokeOperation?: AppOperationInvoker;
}): McpAppBridge {
  let viewInitialized = false;
  let payload: McpAppPayload | null = null;
  let delivered = false;
  let disposed = false;

  function post(message: unknown) {
    if (disposed) return;
    options.frame()?.postMessage(message, "*");
  }

  function flush() {
    if (!viewInitialized || delivered || payload === null) return;
    delivered = true;
    // Ordering contract: input exactly once, before the result.
    post({
      jsonrpc: "2.0",
      method: "ui/notifications/tool-input",
      params: { arguments: isRecord(payload.arguments) ? payload.arguments : {} },
    });
    post({
      jsonrpc: "2.0",
      method: "ui/notifications/tool-result",
      params: {
        content: [{ type: "text", text: payload.content }],
        ...(payload.structured_content === undefined ||
        payload.structured_content === null
          ? {}
          : { structuredContent: payload.structured_content }),
        isError: payload.is_error,
      },
    });
  }

  function handleMessage(event: MessageEvent) {
    const frame = options.frame();
    if (disposed || frame === null || event.source !== frame) return;
    const message: unknown = event.data;
    if (!isRecord(message) || message.jsonrpc !== "2.0") return;
    const method = typeof message.method === "string" ? message.method : null;
    const id = message.id;
    const isRequest = method !== null && id !== undefined;

    if (isRequest) {
      switch (method) {
        case "ui/initialize":
          post({
            jsonrpc: "2.0",
            id,
            result: {
              protocolVersion: MCP_APPS_PROTOCOL_VERSION,
              hostInfo: { name: "OpenWave", version: "1.0.0" },
              hostCapabilities: {},
              // All four result fields are required by the view's schema —
              // an omitted hostContext fails its connect outright.
              hostContext: {
                theme: options.theme(),
                displayMode: "inline",
                platform: "desktop",
              },
            },
          });
          break;
        case "ping":
          post({ jsonrpc: "2.0", id, result: {} });
          break;
        case "tools/call": {
          const invoke = options.invokeTool;
          if (!invoke) {
            post({
              jsonrpc: "2.0",
              id,
              error: { code: -32601, message: "Method not found" },
            });
            break;
          }
          const params = isRecord(message.params) ? message.params : {};
          const tool = typeof params.name === "string" ? params.name : null;
          if (!tool) {
            post({
              jsonrpc: "2.0",
              id,
              error: { code: -32602, message: "tools/call needs a string name" },
            });
            break;
          }
          // The reply is asynchronous; `post` already refuses after dispose,
          // so a result landing after unmount goes nowhere.
          void invoke(tool, params.arguments).then(
            (result) =>
              post({
                jsonrpc: "2.0",
                id,
                result: {
                  content: [{ type: "text", text: result.content }],
                  ...(result.structured_content === undefined ||
                  result.structured_content === null
                    ? {}
                    : { structuredContent: result.structured_content }),
                  isError: result.is_error,
                },
              }),
            (error: unknown) =>
              post({
                jsonrpc: "2.0",
                id,
                error:
                  error instanceof AppInvokeRefusalError
                    ? {
                        code: -32000,
                        message: error.message,
                        data: { kind: error.kind },
                      }
                    : { code: -32000, message: String(error) },
              }),
          );
          break;
        }
        case "operations/call": {
          const invoke = options.invokeOperation;
          if (!invoke) {
            post({
              jsonrpc: "2.0",
              id,
              error: { code: -32601, message: "Method not found" },
            });
            break;
          }
          const params = isRecord(message.params) ? message.params : {};
          const operationId =
            typeof params.operation_id === "string" ? params.operation_id : null;
          if (!operationId) {
            post({
              jsonrpc: "2.0",
              id,
              error: {
                code: -32602,
                message: "operations/call needs a string operation_id",
              },
            });
            break;
          }
          // Parameters, body, and the REST result are opaque passthrough the
          // bridge never interprets; the result object crosses back verbatim.
          // The reply is asynchronous; `post` already refuses after dispose.
          void invoke(operationId, params.parameters, params.body).then(
            (result) => post({ jsonrpc: "2.0", id, result }),
            (error: unknown) =>
              post({
                jsonrpc: "2.0",
                id,
                error:
                  error instanceof AppInvokeRefusalError
                    ? {
                        code: -32000,
                        message: error.message,
                        data: { kind: error.kind },
                      }
                    : { code: -32000, message: String(error) },
              }),
          );
          break;
        }
        default:
          // Fail closed but politely: the view sees a rejected promise, not
          // a hung request or a torn-down connection.
          post({
            jsonrpc: "2.0",
            id,
            error: { code: -32601, message: "Method not found" },
          });
      }
      return;
    }

    switch (method) {
      case "ui/notifications/initialized":
        viewInitialized = true;
        flush();
        break;
      case "ui/notifications/size-changed": {
        const height = isRecord(message.params)
          ? message.params.height
          : undefined;
        if (typeof height === "number" && Number.isFinite(height)) {
          options.onHeight?.(
            Math.min(MAX_FRAME_HEIGHT, Math.max(MIN_FRAME_HEIGHT, Math.ceil(height))),
          );
        }
        break;
      }
      default:
        // Logging, teardown requests, and future notifications are safe to
        // drop; nothing in the protocol requires an answer to them.
        break;
    }
  }

  return {
    handleMessage,
    deliverPayload(next) {
      payload = next;
      flush();
    },
    dispose() {
      disposed = true;
    },
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
