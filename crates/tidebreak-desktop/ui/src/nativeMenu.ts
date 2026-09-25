import { useEffect, useRef } from "react";
import { isTauri } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import {
  hasOpenModalDialog,
  shellShortcutFor,
  type ShellShortcutAction,
  type ShellShortcutHandlers,
  type ShellShortcutMode,
} from "./ShellShortcuts";

/**
 * Raised by the native menu for the items the renderer carries out. The
 * payload is the item's id in `menu.rs`, one of {@link MENU_COMMANDS}.
 */
export const MENU_COMMAND_EVENT = "desktop-menu-command";

/** Where Help > Documentation goes. */
export const DOCUMENTATION_URL = "https://naingthet.github.io/tidebreak/docs/";

/**
 * The shell shortcut behind each menu item that has one.
 *
 * On macOS a menu item claims its chord before the key reaches the webview, so
 * in the packaged app Cmd+N, Cmd+K, Cmd+B, and the rest arrive as these
 * commands and never as keydowns. Naming the shortcut the same chord matches
 * keeps the two paths on one handler and one set of guards. New is the one
 * item whose shortcut depends on which half of the app is up.
 */
const MENU_COMMAND_SHORTCUTS = {
  new: { chat: "new-chat", code: "code-new-workspace" },
  "command-palette": "open-command-palette",
  settings: "open-settings",
  "toggle-sidebar": "toggle-sidebar",
  "zoom-in": "zoom-in",
  "zoom-out": "zoom-out",
  "zoom-reset": "zoom-reset",
  "keyboard-shortcuts": "show-shortcuts",
} as const satisfies Record<
  string,
  ShellShortcutAction | Record<ShellShortcutMode, ShellShortcutAction>
>;

/** Menu items that run no shortcut: the shell handles each one itself. */
const MENU_COMMANDS_WITHOUT_SHORTCUTS = [
  "documentation",
  "install-cli-command",
] as const;

export type MenuCommand =
  | keyof typeof MENU_COMMAND_SHORTCUTS
  | (typeof MENU_COMMANDS_WITHOUT_SHORTCUTS)[number];

/** Every command the native menu raises. `menu.rs` names the same set. */
export const MENU_COMMANDS: readonly MenuCommand[] = [
  ...(Object.keys(MENU_COMMAND_SHORTCUTS) as MenuCommand[]),
  ...MENU_COMMANDS_WITHOUT_SHORTCUTS,
];

export function isMenuCommand(value: unknown): value is MenuCommand {
  return (
    typeof value === "string" &&
    (MENU_COMMANDS as readonly string[]).includes(value)
  );
}

/**
 * The shortcut a menu command runs, or `null` when it runs none.
 *
 * The keyboard's modal guard holds here too. A menu item is only a way to
 * press its chord, so it acts behind an open dialog exactly when the chord
 * would: the palette's own item closes the palette, and nothing else reaches
 * past a dialog the reader is still deciding in. Documentation and the
 * command install are not shortcuts, so they resolve to none.
 */
export function menuCommandShortcut(
  command: MenuCommand,
  context: { modalOpen: boolean; mode: ShellShortcutMode },
): ShellShortcutAction | null {
  if (!(command in MENU_COMMAND_SHORTCUTS)) return null;
  const target =
    MENU_COMMAND_SHORTCUTS[command as keyof typeof MENU_COMMAND_SHORTCUTS];
  const action = typeof target === "string" ? target : target[context.mode];
  const def = shellShortcutFor(action, context.mode);
  if (!def) return null;
  if (context.modalOpen && !def.allowInModal) return null;
  return action;
}

/**
 * Run `handler` whenever the native host raises `event`.
 *
 * The handler is read through a ref so the listener registers once for the
 * caller's lifetime instead of being torn down and rebound every time a
 * callback changes identity between renders. Outside the desktop app there is
 * no host to raise anything, so nothing is registered at all.
 */
export function useNativeHostEvent(
  event: string,
  handler: (payload: unknown) => void,
): void {
  const handlerRef = useRef(handler);
  handlerRef.current = handler;
  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    let unlisten: UnlistenFn | undefined;
    void listen<unknown>(event, ({ payload }) =>
      handlerRef.current(payload),
    ).then((stop) => {
      if (cancelled) stop();
      else unlisten = stop;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [event]);
}

/**
 * Run the shell shortcut a native menu item asks for.
 *
 * Mounted beside `useShellShortcuts` and under the same gate, so a menu item
 * does nothing exactly when its chord would do nothing. The handler receives
 * `null` in place of the key event, because no key was pressed.
 */
export function useShellMenuCommands(
  handlers: ShellShortcutHandlers,
  mode: () => ShellShortcutMode,
): void {
  useNativeHostEvent(MENU_COMMAND_EVENT, (payload) => {
    if (!isMenuCommand(payload)) return;
    const action = menuCommandShortcut(payload, {
      modalOpen: hasOpenModalDialog(document),
      mode: mode(),
    });
    if (action) handlers[action](null);
  });
}
