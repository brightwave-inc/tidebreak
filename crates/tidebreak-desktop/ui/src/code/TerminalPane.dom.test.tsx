// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { TerminalPane } from "./TerminalPane";

const written: string[] = [];
const terminals: Array<{
  disposed: boolean;
  options: { theme?: Record<string, string> };
  write: (data: string, cb?: () => void) => void;
}> = [];

vi.mock("@xterm/xterm", () => ({
  Terminal: class MockTerminal {
    cols = 80;
    rows = 24;
    disposed = false;
    options: { theme?: Record<string, string> };
    write(data: string, cb?: () => void) {
      written.push(data);
      cb?.();
    }
    loadAddon() {}
    open() {}
    dispose() {
      this.disposed = true;
    }
    onData() {
      return { dispose() {} };
    }
    constructor(options?: { theme?: Record<string, string> }) {
      this.options = { ...options };
      terminals.push(this);
    }
  },
}));

vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class {
    fit() {}
  },
}));

vi.mock("@xterm/xterm/css/xterm.css", () => ({}));

afterEach(() => {
  cleanup();
  written.length = 0;
  terminals.length = 0;
  vi.restoreAllMocks();
  document.documentElement.classList.remove("dark");
});

function encode(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

describe("TerminalPane", () => {
  it("disposes the renderer when the pane unmounts", async () => {
    const client = {
      listCodeTerminals: vi.fn().mockResolvedValue([]),
      createCodeTerminal: vi.fn().mockResolvedValue({
        id: "term-1",
        workspace_id: "ws-1",
        cols: 80,
        rows: 24,
        ended: false,
        created_at: "2026-08-15T12:00:00.000Z",
      }),
      readCodeTerminal: vi.fn().mockResolvedValue({
        id: "term-1",
        workspace_id: "ws-1",
        bytes: "",
        cursor: 0,
        overflow: false,
        truncated: false,
        ended: false,
      }),
      writeCodeTerminal: vi.fn().mockResolvedValue(undefined),
      resizeCodeTerminal: vi.fn().mockResolvedValue({
        id: "term-1",
        workspace_id: "ws-1",
        cols: 80,
        rows: 24,
        ended: false,
        created_at: "2026-08-15T12:00:00.000Z",
      }),
    };
    const { unmount } = render(
      <TerminalPane client={client} workspaceId="ws-1" />,
    );
    await waitFor(() => expect(client.createCodeTerminal).toHaveBeenCalled());
    expect(terminals).toHaveLength(1);
    expect(terminals[0]?.disposed).toBe(false);
    unmount();
    expect(terminals[0]?.disposed).toBe(true);
  });

  it("renders a truncation notice when the ring overflowed", async () => {
    const client = {
      listCodeTerminals: vi.fn().mockResolvedValue([
        {
          id: "term-1",
          workspace_id: "ws-1",
          cols: 80,
          rows: 24,
          ended: false,
          created_at: "2026-08-15T12:00:00.000Z",
        },
      ]),
      createCodeTerminal: vi.fn(),
      readCodeTerminal: vi.fn().mockResolvedValue({
        id: "term-1",
        workspace_id: "ws-1",
        bytes: encode("\r\n[output truncated]\r\nlater"),
        cursor: 12,
        overflow: true,
        truncated: false,
        ended: false,
      }),
      writeCodeTerminal: vi.fn(),
      resizeCodeTerminal: vi.fn(),
    };
    render(<TerminalPane client={client} workspaceId="ws-1" />);
    expect(await screen.findByTestId("terminal-truncated")).toHaveTextContent(
      "Output was truncated.",
    );
  });

  it("renders the ended-shell state", async () => {
    const client = {
      listCodeTerminals: vi.fn().mockResolvedValue([]),
      createCodeTerminal: vi.fn().mockResolvedValue({
        id: "term-1",
        workspace_id: "ws-1",
        cols: 80,
        rows: 24,
        ended: true,
        created_at: "2026-08-15T12:00:00.000Z",
      }),
      readCodeTerminal: vi.fn().mockResolvedValue({
        id: "term-1",
        workspace_id: "ws-1",
        bytes: "",
        cursor: 0,
        overflow: false,
        truncated: false,
        ended: true,
      }),
      writeCodeTerminal: vi.fn(),
      resizeCodeTerminal: vi.fn(),
    };
    render(<TerminalPane client={client} workspaceId="ws-1" />);
    expect(await screen.findByTestId("terminal-ended")).toHaveTextContent(
      "Shell ended.",
    );
  });

  it("builds the xterm theme from CSS tokens and reapplies it when the root class flips", async () => {
    const light: Record<string, string> = {
      "--background": "#fafafa",
      "--foreground": "#18181b",
      "--muted-foreground": "#71717a",
      "--success": "#16a34a",
      "--success-foreground": "#14532d",
      "--warning": "#ca8a04",
      "--warning-foreground": "#713f12",
      "--critical": "#dc2626",
      "--critical-foreground": "#7f1d1d",
      "--info": "#2563eb",
      "--info-foreground": "#1e3a8a",
    };
    const dark: Record<string, string> = {
      "--background": "#18181b",
      "--foreground": "#e4e4e7",
      "--muted-foreground": "#a1a1aa",
      "--success": "#4ade80",
      "--success-foreground": "#bbf7d0",
      "--warning": "#facc15",
      "--warning-foreground": "#fef08a",
      "--critical": "#f87171",
      "--critical-foreground": "#fecaca",
      "--info": "#60a5fa",
      "--info-foreground": "#bfdbfe",
    };
    const original = window.getComputedStyle;
    vi.spyOn(window, "getComputedStyle").mockImplementation((element) => {
      if (element === document.documentElement) {
        const tokens = document.documentElement.classList.contains("dark")
          ? dark
          : light;
        return {
          getPropertyValue: (name: string) => tokens[name] ?? "",
        } as CSSStyleDeclaration;
      }
      return original.call(window, element);
    });

    const client = {
      listCodeTerminals: vi.fn().mockResolvedValue([]),
      createCodeTerminal: vi.fn().mockResolvedValue({
        id: "term-1",
        workspace_id: "ws-1",
        cols: 80,
        rows: 24,
        ended: false,
        created_at: "2026-08-15T12:00:00.000Z",
      }),
      readCodeTerminal: vi.fn().mockResolvedValue({
        id: "term-1",
        workspace_id: "ws-1",
        bytes: "",
        cursor: 0,
        overflow: false,
        truncated: false,
        ended: false,
      }),
      writeCodeTerminal: vi.fn(),
      resizeCodeTerminal: vi.fn(),
    };

    document.documentElement.classList.remove("dark");
    render(<TerminalPane client={client} workspaceId="ws-1" />);
    await waitFor(() => expect(client.createCodeTerminal).toHaveBeenCalled());

    expect(screen.getByTestId("terminal-host")).toHaveAttribute(
      "aria-label",
      "Terminal output",
    );
    expect(terminals[0]?.options.theme).toMatchObject({
      background: "#fafafa",
      foreground: "#18181b",
      red: "#dc2626",
      green: "#16a34a",
      yellow: "#ca8a04",
      blue: "#2563eb",
    });

    document.documentElement.classList.add("dark");
    await waitFor(() =>
      expect(terminals[0]?.options.theme).toMatchObject({
        background: "#18181b",
        foreground: "#e4e4e7",
        red: "#f87171",
        green: "#4ade80",
      }),
    );
    document.documentElement.classList.remove("dark");
  });
});
