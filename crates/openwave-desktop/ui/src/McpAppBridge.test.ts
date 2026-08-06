import { describe, expect, it, vi } from "vitest";
import {
  createMcpAppBridge,
  MCP_APPS_PROTOCOL_VERSION,
  type McpAppPayload,
} from "./McpAppBridge";

const PAYLOAD: McpAppPayload = {
  arguments: { operation: "list" },
  content: '{"status":200}',
  structured_content: { status: 200 },
  is_error: false,
};

function harness(
  theme: "light" | "dark" = "dark",
  invokeOperation?: Parameters<typeof createMcpAppBridge>[0]["invokeOperation"],
) {
  const postMessage = vi.fn();
  const frame = { postMessage } as unknown as Window;
  const onHeight = vi.fn();
  let currentTheme = theme;
  const bridge = createMcpAppBridge({
    frame: () => frame,
    theme: () => currentTheme,
    onHeight,
    invokeOperation,
  });
  const fromView = (data: unknown, source: unknown = frame) =>
    bridge.handleMessage({ data, source } as MessageEvent);
  const sent = () => postMessage.mock.calls.map(([message]) => message);
  const targets = () => postMessage.mock.calls.map(([, target]) => target);
  return {
    bridge,
    fromView,
    sent,
    targets,
    onHeight,
    frame,
    setTheme: (next: "light" | "dark") => {
      currentTheme = next;
    },
  };
}

describe("createMcpAppBridge", () => {
  it("answers ui/initialize with every required result field", () => {
    const { fromView, sent, targets } = harness("dark");
    fromView({ jsonrpc: "2.0", id: 0, method: "ui/initialize", params: {} });

    expect(sent()).toEqual([
      {
        jsonrpc: "2.0",
        id: 0,
        result: {
          protocolVersion: MCP_APPS_PROTOCOL_VERSION,
          hostInfo: { name: "OpenWave", version: "1.0.0" },
          hostCapabilities: {},
          hostContext: { theme: "dark", displayMode: "inline", platform: "desktop" },
        },
      },
    ]);
    // A sandboxed frame's origin is opaque; only "*" delivers.
    expect(targets()).toEqual(["*"]);
  });

  it("reads the theme at initialize time, not at construction", () => {
    // One bridge must survive a theme change without losing handshake state;
    // the reply reflects whatever the app's theme is when the view asks.
    const { fromView, sent, setTheme } = harness("light");
    setTheme("dark");
    fromView({ jsonrpc: "2.0", id: 0, method: "ui/initialize", params: {} });
    const [reply] = sent() as Array<{ result: { hostContext: { theme: string } } }>;
    expect(reply.result.hostContext.theme).toBe("dark");
  });

  it("buffers the payload until the view reports initialized, then sends input before result", () => {
    const { bridge, fromView, sent } = harness();
    bridge.deliverPayload(PAYLOAD);
    expect(sent()).toEqual([]);

    fromView({ jsonrpc: "2.0", method: "ui/notifications/initialized" });
    expect(sent().map((message) => (message as { method?: string }).method)).toEqual([
      "ui/notifications/tool-input",
      "ui/notifications/tool-result",
    ]);
    const [input, result] = sent() as Array<{ params: Record<string, unknown> }>;
    expect(input.params).toEqual({ arguments: { operation: "list" } });
    expect(result.params).toEqual({
      content: [{ type: "text", text: '{"status":200}' }],
      structuredContent: { status: 200 },
      isError: false,
    });

    // The pair is sent exactly once, whichever side repeats itself.
    fromView({ jsonrpc: "2.0", method: "ui/notifications/initialized" });
    bridge.deliverPayload(PAYLOAD);
    expect(sent()).toHaveLength(2);
  });

  it("delivers a payload that arrives after initialization", () => {
    const { bridge, fromView, sent } = harness();
    fromView({ jsonrpc: "2.0", method: "ui/notifications/initialized" });
    expect(sent()).toEqual([]);
    bridge.deliverPayload({ content: "text only", is_error: true });
    const [input, result] = sent() as Array<{ params: Record<string, unknown> }>;
    expect(input.params).toEqual({ arguments: {} });
    // No structuredContent key at all when the tool produced none.
    expect(result.params).toEqual({
      content: [{ type: "text", text: "text only" }],
      isError: true,
    });
  });

  it("rejects unknown requests with method-not-found and ignores unknown notifications", () => {
    const { fromView, sent } = harness();
    fromView({ jsonrpc: "2.0", id: 7, method: "ui/open-link", params: { url: "https://x" } });
    expect(sent()).toEqual([
      { jsonrpc: "2.0", id: 7, error: { code: -32601, message: "Method not found" } },
    ]);
    fromView({ jsonrpc: "2.0", method: "notifications/message", params: { level: "info" } });
    fromView({ jsonrpc: "2.0", method: "ui/notifications/request-teardown" });
    expect(sent()).toHaveLength(1);
  });

  it("refuses operations/call without an invoker, and undeclared methods with one", () => {
    // The transcript card wires no invoker: operations/call stays
    // method-not-found there.
    const bare = harness();
    bare.fromView({
      jsonrpc: "2.0",
      id: 1,
      method: "operations/call",
      params: { operation_id: "listIssues" },
    });
    expect(bare.sent()).toEqual([
      { jsonrpc: "2.0", id: 1, error: { code: -32601, message: "Method not found" } },
    ]);

    // Wiring the invoker declares operations/call and nothing else: any
    // other method — including the removed tools/call verb — still fails
    // closed rather than acquiring a handler by accident.
    const invokeOperation = vi.fn();
    const wired = harness("dark", invokeOperation);
    wired.fromView({ jsonrpc: "2.0", id: 2, method: "resources/read", params: {} });
    wired.fromView({
      jsonrpc: "2.0",
      id: 3,
      method: "tools/call",
      params: { name: "mcp__cmd__doit", arguments: {} },
    });
    expect(wired.sent()).toEqual([
      { jsonrpc: "2.0", id: 2, error: { code: -32601, message: "Method not found" } },
      { jsonrpc: "2.0", id: 3, error: { code: -32601, message: "Method not found" } },
    ]);
    expect(invokeOperation).not.toHaveBeenCalled();
  });

  it("answers ping and clamps size-changed heights", () => {
    const { fromView, sent, onHeight } = harness();
    fromView({ jsonrpc: "2.0", id: 1, method: "ping", params: {} });
    expect(sent()).toEqual([{ jsonrpc: "2.0", id: 1, result: {} }]);

    fromView({ jsonrpc: "2.0", method: "ui/notifications/size-changed", params: { height: 512.4 } });
    expect(onHeight).toHaveBeenLastCalledWith(513);
    fromView({ jsonrpc: "2.0", method: "ui/notifications/size-changed", params: { height: 20 } });
    expect(onHeight).toHaveBeenLastCalledWith(40);
    fromView({ jsonrpc: "2.0", method: "ui/notifications/size-changed", params: { height: 99999 } });
    expect(onHeight).toHaveBeenLastCalledWith(800);
    fromView({ jsonrpc: "2.0", method: "ui/notifications/size-changed", params: {} });
    expect(onHeight).toHaveBeenCalledTimes(3);
  });

  it("ignores messages from other sources and non-JSON-RPC traffic", () => {
    const { fromView, sent } = harness();
    fromView({ jsonrpc: "2.0", id: 0, method: "ui/initialize" }, { not: "the frame" });
    fromView("just a string");
    fromView({ some: "other postMessage traffic" });
    fromView(null);
    expect(sent()).toEqual([]);
  });

  it("posts nothing after dispose", () => {
    const { bridge, fromView, sent } = harness();
    bridge.dispose();
    fromView({ jsonrpc: "2.0", id: 0, method: "ui/initialize" });
    bridge.deliverPayload(PAYLOAD);
    expect(sent()).toEqual([]);
  });
});
