// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useCodeUiStore } from "./CodeUiStore";
import {
  DEFAULT_TERMINAL_DRAWER_HEIGHT,
  TerminalDrawer,
} from "./TerminalDrawer";

vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    cols = 80;
    rows = 24;
    options = {};
    write(_data: string, cb?: () => void) {
      cb?.();
    }
    loadAddon() {}
    open() {}
    dispose() {}
    onData() {
      return { dispose() {} };
    }
  },
}));

vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class {
    fit() {}
  },
}));

vi.mock("@xterm/xterm/css/xterm.css", () => ({}));

const HEIGHTS_KEY = "tidebreak.code-terminal-drawer-heights";

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

afterEach(() => {
  cleanup();
  window.localStorage.removeItem(HEIGHTS_KEY);
  useCodeUiStore.setState({ terminalDrawerHeights: {} });
});

describe("TerminalDrawer", () => {
  it("resizes from the keyboard and persists the height per workspace", async () => {
    const user = userEvent.setup();
    render(
      <TerminalDrawer
        client={client as never}
        workspaceId="ws-1"
        onClose={() => {}}
      />,
    );

    const separator = screen.getByRole("separator", {
      name: "Resize terminal",
    });
    expect(separator).toHaveAttribute(
      "aria-valuenow",
      String(DEFAULT_TERMINAL_DRAWER_HEIGHT),
    );
    expect(separator).toHaveAttribute("tabindex", "0");

    separator.focus();
    await user.keyboard("{ArrowUp}");

    const taller = DEFAULT_TERMINAL_DRAWER_HEIGHT + 32;
    expect(separator).toHaveAttribute("aria-valuenow", String(taller));
    expect(useCodeUiStore.getState().terminalDrawerHeights["ws-1"]).toBe(
      taller,
    );
    expect(window.localStorage.getItem(HEIGHTS_KEY)).toBe(
      JSON.stringify({ "ws-1": taller }),
    );

    await user.keyboard("{ArrowDown}");
    expect(separator).toHaveAttribute(
      "aria-valuenow",
      String(DEFAULT_TERMINAL_DRAWER_HEIGHT),
    );
  });

  it("restores the persisted height for the workspace", () => {
    window.localStorage.setItem(
      HEIGHTS_KEY,
      JSON.stringify({ "ws-1": 336, "ws-2": 176 }),
    );
    useCodeUiStore.setState({
      terminalDrawerHeights: { "ws-1": 336, "ws-2": 176 },
    });

    render(
      <TerminalDrawer
        client={client as never}
        workspaceId="ws-1"
        onClose={() => {}}
      />,
    );

    expect(
      screen.getByRole("separator", { name: "Resize terminal" }),
    ).toHaveAttribute("aria-valuenow", "336");
  });
});
