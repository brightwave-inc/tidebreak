// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  SHELL_SHORTCUTS,
  useShellShortcuts,
  usesCommandModifier,
  type ShellShortcutAction,
  type ShellShortcutHandlers,
} from "./ShellShortcuts";
import { TerminalPane } from "./code/TerminalPane";

vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    cols = 80;
    rows = 24;
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

afterEach(cleanup);

function noopHandlers(
  overrides: Partial<ShellShortcutHandlers> = {},
): ShellShortcutHandlers {
  const handlers = {} as ShellShortcutHandlers;
  for (const def of SHELL_SHORTCUTS) {
    handlers[def.id] = () => {};
  }
  return { ...handlers, ...overrides };
}

function Harness({ handlers }: { handlers: ShellShortcutHandlers }) {
  useShellShortcuts(handlers, () => "code");
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
  return <TerminalPane client={client} workspaceId="ws-1" />;
}

function chord(
  action: Extract<
    ShellShortcutAction,
    "toggle-code-terminal" | "toggle-code-review"
  >,
) {
  const command = usesCommandModifier(navigator.userAgent);
  return new KeyboardEvent("keydown", {
    key: action === "toggle-code-terminal" ? "j" : "i",
    code: action === "toggle-code-terminal" ? "KeyJ" : "KeyI",
    metaKey: command,
    ctrlKey: !command,
    bubbles: true,
    cancelable: true,
  });
}

describe("shell shortcut delivery", () => {
  it("fires Cmd+J and Cmd+I while the terminal host is focused", () => {
    const fired: ShellShortcutAction[] = [];
    render(
      <Harness
        handlers={noopHandlers({
          "toggle-code-terminal": () => {
            fired.push("toggle-code-terminal");
          },
          "toggle-code-review": () => {
            fired.push("toggle-code-review");
          },
        })}
      />,
    );

    const host = screen.getByTestId("terminal-host");
    host.dispatchEvent(chord("toggle-code-terminal"));
    host.dispatchEvent(chord("toggle-code-review"));

    expect(fired).toEqual(["toggle-code-terminal", "toggle-code-review"]);
  });
});
