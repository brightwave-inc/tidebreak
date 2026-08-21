// @vitest-environment jsdom
import type { ReactNode } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AppContextProvider, type AppContextValue } from "./AppContext";
import type { ApiClient } from "./api";
import { McpAppCard } from "./McpAppCard";

function withApp(client: Partial<ApiClient>, node: ReactNode) {
  return (
    <AppContextProvider
      value={{ client: client as ApiClient } as AppContextValue}
    >
      {node}
    </AppContextProvider>
  );
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("McpAppCard", () => {
  it("renders the minted frame address in a sandboxed, non-same-origin frame", async () => {
    const createMcpViewFrame = vi
      .fn()
      .mockResolvedValue({ frame_path: "/mcp/view-frames/token-1" });
    render(
      withApp(
        {
          createMcpViewFrame,
          baseUrl: "http://127.0.0.1:7777",
        } as Partial<ApiClient>,
        <McpAppCard server="gateway" resourceUri="ui://gateway/app.html" />,
      ),
    );

    const frame = await screen.findByTitle("MCP App view from gateway");
    expect(createMcpViewFrame).toHaveBeenCalledWith(
      "gateway",
      "ui://gateway/app.html",
    );
    // The document is host-served with its own CSP; the renderer only ever
    // holds an opaque address, never markup.
    expect(frame).toHaveAttribute(
      "src",
      "http://127.0.0.1:7777/mcp/view-frames/token-1",
    );
    expect(frame).toHaveAttribute("sandbox", "allow-scripts");
    expect(frame.getAttribute("sandbox")).not.toContain("allow-same-origin");
    // The slim provenance row keeps embedded documents attributable.
    expect(screen.getByText("gateway")).toBeInTheDocument();
  });

  it("degrades to a reconnect hint when no frame can be minted", async () => {
    const createMcpViewFrame = vi.fn().mockRejectedValue(new Error("404"));
    render(
      withApp(
        {
          createMcpViewFrame,
          baseUrl: "http://127.0.0.1:7777",
        } as Partial<ApiClient>,
        <McpAppCard server="gateway" resourceUri="ui://gateway/app.html" />,
      ),
    );

    expect(
      await screen.findByText(/This view is unavailable/),
    ).toBeInTheDocument();
    expect(screen.queryByTitle(/MCP App view/)).not.toBeInTheDocument();
  });
});
