// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { TerminalPane } from "./TerminalPane";

const written: string[] = [];
const terminals: Array<{ disposed: boolean; write: (data: string, cb?: () => void) => void }> =
  [];

vi.mock("@xterm/xterm", () => ({
  Terminal: class MockTerminal {
    cols = 80;
    rows = 24;
    disposed = false;
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
    constructor() {
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
});
