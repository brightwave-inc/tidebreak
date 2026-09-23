// @vitest-environment jsdom
import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { MENU_COMMAND_EVENT, useShellMenuCommands } from "./nativeMenu";
import {
  SHELL_SHORTCUTS,
  type ShellShortcutHandlers,
  type ShellShortcutMode,
} from "./ShellShortcuts";

type HostListener = (event: { payload: unknown }) => void;
const hostListeners = vi.hoisted(() => new Map<string, HostListener>());

vi.mock("@tauri-apps/api/core", () => ({ isTauri: () => true }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (event: string, handler: HostListener) => {
    hostListeners.set(event, handler);
    return () => hostListeners.delete(event);
  },
}));

afterEach(() => {
  cleanup();
  document.body.innerHTML = "";
});

function handlersSpy(): ShellShortcutHandlers {
  const handlers = {} as ShellShortcutHandlers;
  for (const def of SHELL_SHORTCUTS) handlers[def.id] = vi.fn();
  return handlers;
}

function Harness({
  handlers,
  mode,
}: {
  handlers: ShellShortcutHandlers;
  mode: ShellShortcutMode;
}) {
  useShellMenuCommands(handlers, () => mode);
  return null;
}

async function raise(payload: unknown) {
  await waitFor(() => expect(hostListeners.has(MENU_COMMAND_EVENT)).toBe(true));
  hostListeners.get(MENU_COMMAND_EVENT)?.({ payload });
}

describe("useShellMenuCommands", () => {
  it("runs the shortcut's handler with no key event", async () => {
    const handlers = handlersSpy();
    render(<Harness handlers={handlers} mode="code" />);

    await raise("new");

    expect(handlers["code-new-workspace"]).toHaveBeenCalledWith(null);
    expect(handlers["new-chat"]).not.toHaveBeenCalled();
  });

  it("ignores a command it does not know", async () => {
    const handlers = handlersSpy();
    render(<Harness handlers={handlers} mode="chat" />);

    await raise("reload-app");
    await raise(7);

    for (const handler of Object.values(handlers)) {
      expect(handler).not.toHaveBeenCalled();
    }
  });

  it("holds a command back while a dialog is open", async () => {
    const handlers = handlersSpy();
    const dialog = document.createElement("div");
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("data-state", "open");
    document.body.append(dialog);
    render(<Harness handlers={handlers} mode="chat" />);

    await raise("settings");
    await raise("command-palette");

    expect(handlers["open-settings"]).not.toHaveBeenCalled();
    expect(handlers["open-command-palette"]).toHaveBeenCalledWith(null);
  });
});
