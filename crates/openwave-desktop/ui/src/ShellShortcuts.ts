import { useEffect, useRef } from "react";

/**
 * Shell-level keyboard shortcuts: the ones that act on the frame rather than on
 * a single conversation, so they work from every route.
 *
 * The bindings live in one declarative table rather than scattered through
 * component handlers, so a future shortcuts-help dialog can read the same list
 * the listener acts on and stay honest about what the keys actually do.
 */
export type ShellShortcutAction =
  | "toggle-sidebar"
  | "new-chat"
  | "focus-composer"
  | "zoom-in"
  | "zoom-out"
  | "zoom-reset";

export type ShellShortcutDef = {
  id: ShellShortcutAction;
  /**
   * The characters that trigger it, matched case-insensitively against
   * `event.key`. More than one because a keyboard can reach the same intent
   * through more than one glyph — zooming in is `=` unshifted and `+` shifted.
   */
  keys: readonly string[];
  /** Requires the platform command modifier — Cmd on macOS, Ctrl elsewhere. */
  mod: boolean;
  /**
   * Whether shift must be held (defaults to must-not). `"any"` for shortcuts
   * whose key is reachable both ways.
   */
  shift?: boolean | "any";
  /** What the shortcut does, phrased for a help dialog. */
  description: string;
  /**
   * Whether the shortcut may fire while focus is in a text field. Chorded
   * combos (mod+key) are safe there — they are not something a reader types —
   * so the composer's own focus never swallows them.
   */
  allowInEditable: boolean;
};

export const SHELL_SHORTCUTS: readonly ShellShortcutDef[] = [
  {
    id: "toggle-sidebar",
    keys: ["b"],
    mod: true,
    description: "Show or hide the sidebar",
    allowInEditable: true,
  },
  {
    id: "new-chat",
    keys: ["n"],
    mod: true,
    description: "Start a new chat",
    allowInEditable: true,
  },
  {
    id: "focus-composer",
    keys: ["k"],
    mod: true,
    description: "Focus the message composer",
    allowInEditable: true,
  },
  {
    id: "zoom-in",
    keys: ["=", "+"],
    mod: true,
    shift: "any",
    description: "Make the interface larger",
    allowInEditable: true,
  },
  {
    id: "zoom-out",
    keys: ["-", "_"],
    mod: true,
    shift: "any",
    description: "Make the interface smaller",
    allowInEditable: true,
  },
  {
    id: "zoom-reset",
    keys: ["0"],
    mod: true,
    description: "Reset the interface size",
    allowInEditable: true,
  },
];

type ShortcutKeyEvent = Pick<
  KeyboardEvent,
  "key" | "metaKey" | "ctrlKey" | "shiftKey" | "altKey"
>;

/** Whether a key event is the combo a shortcut is bound to. */
export function matchesShellShortcut(
  event: ShortcutKeyEvent,
  def: ShellShortcutDef,
): boolean {
  if (event.altKey) return false;
  if (!def.keys.includes(event.key.toLowerCase())) return false;
  if (def.shift !== "any" && event.shiftKey !== (def.shift ?? false)) return false;
  const hasMod = event.metaKey || event.ctrlKey;
  return def.mod ? hasMod : !hasMod;
}

/** Whether focus sits in a field the reader is typing into. */
export function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return true;
  if (target.isContentEditable) return true;
  // `isContentEditable` is layout-derived and unimplemented in some test DOMs;
  // the attribute is the direct signal the shortcut layer actually cares about.
  const editable = target.getAttribute("contenteditable");
  return editable === "" || editable === "true";
}

/**
 * The shortcut a key event should trigger, or `null` when none should.
 *
 * A modal dialog on screen suppresses every shell shortcut: the reader is
 * mid-decision, and toggling the frame or starting a new chat behind the
 * dialog would be a surprise. Kept pure so the guard is testable without a DOM.
 */
export function resolveShellShortcut(
  event: ShortcutKeyEvent,
  context: { editable: boolean; modalOpen: boolean },
): ShellShortcutDef | null {
  if (context.modalOpen) return null;
  for (const def of SHELL_SHORTCUTS) {
    if (!matchesShellShortcut(event, def)) continue;
    if (context.editable && !def.allowInEditable) return null;
    return def;
  }
  return null;
}

/** A radix dialog or alert dialog currently open on screen. */
function hasOpenModalDialog(doc: Document): boolean {
  return (
    doc.querySelector(
      '[role="dialog"][data-state="open"], [role="alertdialog"][data-state="open"]',
    ) !== null
  );
}

export type ShellShortcutHandlers = Record<ShellShortcutAction, () => void>;

/**
 * Bind the shell shortcuts to a single window listener for the app's lifetime.
 *
 * Handlers are read through a ref so the listener registers once and never has
 * to be torn down and rebound as the callbacks change identity between renders.
 */
export function useShellShortcuts(handlers: ShellShortcutHandlers): void {
  const handlersRef = useRef(handlers);
  handlersRef.current = handlers;

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.defaultPrevented) return;
      const def = resolveShellShortcut(event, {
        editable: isEditableTarget(event.target),
        modalOpen: hasOpenModalDialog(document),
      });
      if (!def) return;
      event.preventDefault();
      handlersRef.current[def.id]();
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);
}
