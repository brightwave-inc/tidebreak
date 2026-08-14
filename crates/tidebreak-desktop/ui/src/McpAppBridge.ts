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
  type AppFolderInvokeResult,
  type AppRestInvokeResult,
  type McpAppPayload,
} from "./api";

export type { McpAppPayload };

/**
 * Executes one granted REST operation on the embedded view's behalf,
 * wired to `POST /apps/{id}/invoke`. Parameters, body, and the
 * `{status, content_type, body_base64}` result are opaque passthrough in
 * both directions; a rejection becomes a JSON-RPC error reply — a typed
 * [`AppInvokeRefusalError`] keeps its machine-readable kind in `error.data`
 * (the refusal envelope is host-authored, so surfacing it is not
 * interpretation).
 */
export type AppOperationInvoker = (
  operationId: string,
  parameters: unknown,
  body: unknown,
) => Promise<AppRestInvokeResult>;

/**
 * Executes one granted gateway operation on the embedded view's behalf — the
 * same `operations/call` method, routed by the presence of
 * `connected_app_id` in the call's params.
 *
 * That field is the gateway shell's own invoke vocabulary, and naming it the
 * same here is the whole point: a bundle written against this
 * bridge runs unmodified in the gateway's shell, and one written there runs
 * unmodified here. The two hosts differ in where a binding resolves, not in
 * what the bundle speaks.
 */
export type AppGatewayOperationInvoker = (
  gatewayApp: string,
  operationId: string,
  pathParameters: unknown,
  query: unknown,
  body: unknown,
) => Promise<AppRestInvokeResult>;

/**
 * Executes one granted folder operation on the embedded view's behalf — the
 * `fs/*` sibling of the operation invoker, wired to the same invoke route.
 * Content crosses base64-encoded in both directions; rejections map to
 * JSON-RPC errors exactly as operation-call rejections do.
 */
export type AppFolderInvoker = (
  folder: string,
  op: "list" | "read" | "write",
  path?: string,
  contentBase64?: string,
  replace?: boolean,
) => Promise<AppFolderInvokeResult>;

const MIN_FRAME_HEIGHT = 40;
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
   * When present, the view may call `operations/call` and the bridge
   * forwards it here. Absent — the MCP App transcript card —
   * `operations/call` refuses with method-not-found: a view only gets the
   * methods its host deliberately declared.
   */
  invokeOperation?: AppOperationInvoker;
  /**
   * When present, an `operations/call` naming a `connected_app_id` is routed
   * here instead — the gateway leg, under the same capability-by-presence
   * rule. Absent, such a call refuses like any other undeclared capability.
   */
  invokeGatewayOperation?: AppGatewayOperationInvoker;
  /**
   * When present, the view may call `fs/list`, `fs/read`, and `fs/write`
   * and the bridge forwards them here — the folder sibling, under the same
   * capability-by-presence rule.
   */
  invokeFolder?: AppFolderInvoker;
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
              hostInfo: { name: "Tidebreak", version: "1.0.0" },
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
        case "operations/call": {
          const params = isRecord(message.params) ? message.params : {};
          // A `connected_app_id` names a gateway connected app, so the call
          // is a relay rather than a local execution. Legacy calls without it
          // stay local; both are the same method, because a bundle should not
          // have to know which host it is running in.
          const gatewayApp =
            typeof params.connected_app_id === "string"
              ? params.connected_app_id
              : null;
          const invoke = gatewayApp
            ? options.invokeGatewayOperation
            : options.invokeOperation;
          if (!invoke) {
            post({
              jsonrpc: "2.0",
              id,
              error: { code: -32601, message: "Method not found" },
            });
            break;
          }
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
          // Every argument half and the result are opaque passthrough the
          // bridge never interprets; the result object crosses back verbatim.
          // The reply is asynchronous; `post` already refuses after dispose.
          const call = gatewayApp
            ? (invoke as AppGatewayOperationInvoker)(
                gatewayApp,
                operationId,
                params.path_parameters,
                params.query,
                params.body,
              )
            : (invoke as AppOperationInvoker)(
                operationId,
                params.parameters,
                params.body,
              );
          void call.then(
            (result) => post({ jsonrpc: "2.0", id, result }),
            (error: unknown) => post({ jsonrpc: "2.0", id, error: rpcError(error) }),
          );
          break;
        }
        case "fs/list":
        case "fs/read":
        case "fs/write": {
          const invoke = options.invokeFolder;
          if (!invoke) {
            post({
              jsonrpc: "2.0",
              id,
              error: { code: -32601, message: "Method not found" },
            });
            break;
          }
          const params = isRecord(message.params) ? message.params : {};
          const folder = typeof params.folder === "string" ? params.folder : null;
          if (!folder) {
            post({
              jsonrpc: "2.0",
              id,
              error: { code: -32602, message: `${method} needs a string folder` },
            });
            break;
          }
          const op =
            method === "fs/list" ? "list" : method === "fs/read" ? "read" : "write";
          const path = typeof params.path === "string" ? params.path : undefined;
          const contentBase64 =
            typeof params.content_base64 === "string"
              ? params.content_base64
              : undefined;
          const replace =
            typeof params.replace === "boolean" ? params.replace : undefined;
          // The result object crosses back verbatim; the server owns the
          // shape and the closed failure vocabulary. The reply is
          // asynchronous; `post` already refuses after dispose.
          void invoke(folder, op, path, contentBase64, replace).then(
            (result) => post({ jsonrpc: "2.0", id, result }),
            (error: unknown) => post({ jsonrpc: "2.0", id, error: rpcError(error) }),
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

/**
 * One rejected invoke as a JSON-RPC error. A typed
 * {@link AppInvokeRefusalError} keeps its machine-readable kind in
 * `error.data` — the refusal envelope is host-authored, so surfacing it is
 * not interpretation, and it is how the view tells "connect this app at the
 * gateway" from "this call failed".
 */
function rpcError(error: unknown): {
  code: number;
  message: string;
  data?: { kind: string };
} {
  return error instanceof AppInvokeRefusalError
    ? { code: -32000, message: error.message, data: { kind: error.kind } }
    : { code: -32000, message: String(error) };
}
