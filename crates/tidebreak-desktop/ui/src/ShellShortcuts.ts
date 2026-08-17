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
  | "history-back"
  | "history-forward"
  | "toggle-sidebar"
  | "new-chat"
  | "code-new-workspace"
  | "toggle-code-review"
  | "toggle-code-terminal"
  | "focus-composer"
  | "zoom-in"
  | "zoom-out"
  | "zoom-reset"
  | "reload-app"
  | "show-shortcuts";

/**
 * Which half of the app a shortcut belongs to.
 *
 * Chat and code are two modes of one app, and the same chord means the thing
 * the reader is looking at: Cmd+N starts a conversation in chat and a
 * workspace in code. Mode is derived from the route family rather than stored,
 * so the shortcut and the screen can never disagree about which one is up.
 */
export type ShellShortcutMode = "chat" | "code";

/** The heading a shortcut sits under in the help dialog. */
export type ShellShortcutGroup =
  | "Navigation"
  | "Chat"
  | "Code"
  | "View"
  | "Help";

/** Group headings in the order the help dialog lists them. */
export const SHELL_SHORTCUT_GROUPS: readonly ShellShortcutGroup[] = [
  "Navigation",
  "Chat",
  "Code",
  "View",
  "Help",
];

/**
 * How a definition recognises its key, which differs by what kind of key it is.
 *
 * Letters and digits are matched on `event.code` — the physical key position,
 * `KeyN` for the key labelled N on a US board. A layout decides which character
 * a position produces, so matching the character would move the shortcut to
 * wherever that letter happens to sit: Cmd+N is under the QWERTY N on AZERTY
 * too, and matching `event.key` would put it under the physical `?` key
 * instead.
 *
 * Punctuation goes the other way. The glyph is the whole point of `Cmd+/`, and
 * its position is nowhere near fixed across layouts, so those match the
 * character the keyboard actually produced.
 */
type ShellShortcutBinding =
  /**
   * `event.code` values that trigger it. More than one because separate
   * physical keys can carry the same digit — the number row and the numpad.
   */
  | { codes: readonly string[]; keys?: never }
  /**
   * The characters that trigger it, matched case-insensitively against
   * `event.key`. More than one because a keyboard can reach the same intent
   * through more than one glyph — zooming in is `=` unshifted and `+` shifted.
   */
  | { keys: readonly string[]; codes?: never };

export type ShellShortcutDef = ShellShortcutBinding & {
  id: ShellShortcutAction;
  /** Requires the platform command modifier — Cmd on macOS, Ctrl elsewhere. */
  mod: boolean;
  /** Requires Alt/Option; defaults to must-not. */
  alt?: boolean;
  /**
   * Whether shift must be held (defaults to must-not). `"any"` for shortcuts
   * whose key is reachable both ways.
   */
  shift?: boolean | "any";
  /** What the shortcut does, phrased for a help dialog. */
  description: string;
  /** Which heading the help dialog files it under. */
  group: ShellShortcutGroup;
  /**
   * The mode the shortcut belongs to, or every mode when absent. Two
   * definitions may share a chord only across scopes — that is the whole point
   * of the field, and the only way one chord can mean the right thing on both
   * sides of the app.
   */
  scope?: ShellShortcutMode;
  /**
   * Whether the shortcut may fire while focus is in a text field. Chorded
   * combos (mod+key) are safe there — they are not something a reader types —
   * so the composer's own focus never swallows them.
   */
  allowInEditable: boolean;
};

export const SHELL_SHORTCUTS: readonly ShellShortcutDef[] = [
  {
    id: "history-back",
    keys: ["arrowleft"],
    mod: false,
    alt: true,
    description: "Go back (outside text fields)",
    group: "Navigation",
    // Option+Arrow is word-by-word caret movement in every macOS text field,
    // and Alt+Arrow is the same habit elsewhere. Taking it from the composer
    // would break typing to provide navigation the reader can reach with the
    // mouse; unlike the mod-chorded shortcuts below, this one is a key
    // combination people genuinely press while writing.
    allowInEditable: false,
  },
  {
    id: "history-forward",
    keys: ["arrowright"],
    mod: false,
    alt: true,
    description: "Go forward (outside text fields)",
    group: "Navigation",
    allowInEditable: false,
  },
  {
    id: "new-chat",
    codes: ["KeyN"],
    mod: true,
    description: "Start a new chat",
    group: "Chat",
    scope: "chat",
    allowInEditable: true,
  },
  {
    // Cmd+N is "make me a new one of what I am looking at". In code mode the
    // unit of work is a workspace — a worktree and a branch — so making a chat
    // here would both do the wrong thing and navigate the reader out of the
    // mode they were working in.
    id: "code-new-workspace",
    codes: ["KeyN"],
    mod: true,
    description: "New workspace",
    group: "Code",
    scope: "code",
    allowInEditable: true,
  },
  {
    id: "toggle-code-review",
    codes: ["KeyI"],
    mod: true,
    description: "Show or hide the review sidebar",
    group: "Code",
    scope: "code",
    allowInEditable: true,
  },
  {
    id: "toggle-code-terminal",
    codes: ["KeyJ"],
    mod: true,
    description: "Show or hide the terminal",
    group: "Code",
    scope: "code",
    allowInEditable: true,
  },
  {
    id: "focus-composer",
    codes: ["KeyK"],
    mod: true,
    description: "Focus the message composer",
    group: "Chat",
    allowInEditable: true,
  },
  {
    id: "toggle-sidebar",
    codes: ["KeyB"],
    mod: true,
    description: "Show or hide the sidebar",
    group: "View",
    allowInEditable: true,
  },
  {
    id: "zoom-in",
    keys: ["=", "+"],
    mod: true,
    shift: "any",
    description: "Make the interface larger",
    group: "View",
    allowInEditable: true,
  },
  {
    id: "zoom-out",
    keys: ["-", "_"],
    mod: true,
    shift: "any",
    description: "Make the interface smaller",
    group: "View",
    allowInEditable: true,
  },
  {
    id: "zoom-reset",
    codes: ["Digit0", "Numpad0"],
    mod: true,
    description: "Reset the interface size",
    group: "View",
    allowInEditable: true,
  },
  {
    id: "reload-app",
    codes: ["KeyR"],
    mod: true,
    description: "Reload the app",
    group: "View",
    allowInEditable: true,
  },
  {
    id: "show-shortcuts",
    // `?` is the shifted glyph the same physical key produces, so the reader
    // reaches this whether or not they think of it as Cmd+Shift+/.
    keys: ["/", "?"],
    mod: true,
    shift: "any",
    description: "Show keyboard shortcuts",
    group: "Help",
    allowInEditable: true,
  },
];

/**
 * Whether the platform command modifier is Cmd rather than Ctrl.
 *
 * The table records `mod: true` rather than a glyph because the modifier is a
 * property of the platform, not of the shortcut. Both the listener and the help
 * dialog resolve it through here, so what the dialog draws is what fires: on
 * macOS only Cmd+B toggles the sidebar, and Ctrl+B is left to the text field it
 * means something else in. Takes the user-agent string rather than reading
 * `navigator` so it is testable.
 */
export function usesCommandModifier(userAgent: string): boolean {
  return /Mac|iPhone|iPad|iPod/.test(userAgent);
}

/**
 * The keycaps a shortcut should be drawn as, in the order they are pressed.
 *
 * Derived from the same definition the listener matches on, so the help dialog
 * cannot come to disagree with the binding. Only the first of a definition's
 * keys is shown — the alternates exist so a shifted glyph or a second physical
 * key still matches, not because they are a second shortcut worth documenting.
 *
 * A code-matched binding is drawn as the character its physical key carries on
 * a US board, which is the label on most keycaps and the only name the reader
 * has for the position.
 */
export function shortcutKeycaps(
  def: ShellShortcutDef,
  command: boolean,
): string[] {
  const caps: string[] = [];
  if (def.mod) caps.push(command ? "⌘" : "Ctrl");
  if (def.alt) caps.push(command ? "⌥" : "Alt");
  if (def.shift === true) caps.push(command ? "⇧" : "Shift");
  caps.push(shortcutKeyLabel(def));
  return caps;
}

/** The single keycap a definition's key is drawn as. */
function shortcutKeyLabel(def: ShellShortcutDef): string {
  if (def.codes) {
    const code = def.codes[0] ?? "";
    const label = code.replace(/^(Key|Digit|Numpad)/, "");
    return label.length === 1 ? label.toUpperCase() : code;
  }
  const key = def.keys[0] ?? "";
  if (key === "arrowleft") return "←";
  if (key === "arrowright") return "→";
  return key.length === 1 ? key.toUpperCase() : key;
}

/**
 * The shortcuts under their headings, in the order the help dialog lists them.
 *
 * Grouping lives here rather than in the dialog so a shortcut cannot go missing
 * from the help by being filed under a heading nobody renders. Filtered to the
 * mode the reader is in: listing every scope would show two Cmd+N rows, only
 * one of which is true where the dialog is being read.
 */
export function groupedShellShortcuts(mode: ShellShortcutMode): Array<{
  group: ShellShortcutGroup;
  items: ShellShortcutDef[];
}> {
  const byGroup = new Map<ShellShortcutGroup, ShellShortcutDef[]>();
  for (const def of SHELL_SHORTCUTS) {
    if (!inScope(def, mode)) continue;
    const items = byGroup.get(def.group) ?? [];
    items.push(def);
    byGroup.set(def.group, items);
  }
  return SHELL_SHORTCUT_GROUPS.flatMap((group) => {
    const items = byGroup.get(group);
    return items ? [{ group, items }] : [];
  });
}

/** Whether a definition is live in a mode — an unscoped one is live in all. */
function inScope(def: ShellShortcutDef, mode: ShellShortcutMode): boolean {
  return def.scope === undefined || def.scope === mode;
}

type ShortcutKeyEvent = Pick<
  KeyboardEvent,
  "key" | "code" | "metaKey" | "ctrlKey" | "shiftKey" | "altKey"
>;

/**
 * Whether a key event is the combo a shortcut is bound to.
 *
 * `command` says which modifier this platform's shortcuts are chorded with, and
 * the other one is a mismatch rather than an alternative: Ctrl+B on macOS is
 * the reader moving the caret back a character, not asking for the sidebar.
 */
export function matchesShellShortcut(
  event: ShortcutKeyEvent,
  def: ShellShortcutDef,
  command: boolean,
): boolean {
  if (event.altKey !== (def.alt ?? false)) return false;
  const hit = def.codes
    ? def.codes.includes(event.code)
    : def.keys.includes(event.key.toLowerCase());
  if (!hit) return false;
  if (def.shift !== "any" && event.shiftKey !== (def.shift ?? false)) return false;
  if (command ? event.ctrlKey : event.metaKey) return false;
  const hasMod = command ? event.metaKey : event.ctrlKey;
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
 * dialog would be a surprise. `mode` decides which scoped definitions are live,
 * and is required rather than defaulted so every caller has to say which half
 * of the app the key was pressed in. Kept pure so the guard is testable without
 * a DOM.
 */
export function resolveShellShortcut(
  event: ShortcutKeyEvent,
  context: {
    editable: boolean;
    modalOpen: boolean;
    command: boolean;
    mode: ShellShortcutMode;
  },
): ShellShortcutDef | null {
  if (context.modalOpen) return null;
  for (const def of SHELL_SHORTCUTS) {
    if (!inScope(def, context.mode)) continue;
    if (!matchesShellShortcut(event, def, context.command)) continue;
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
 * `mode` is a getter for the same reason, and because the mode is read from the
 * route: asking for it at keydown keeps the shell out of a re-render on every
 * navigation.
 *
 * Capture, not bubble: a focused PTY (and anything else that stops a keydown
 * from bubbling) would otherwise swallow Cmd+J / Cmd+I before this ran.
 */
export function useShellShortcuts(
  handlers: ShellShortcutHandlers,
  mode: () => ShellShortcutMode,
): void {
  const handlersRef = useRef(handlers);
  handlersRef.current = handlers;
  const modeRef = useRef(mode);
  modeRef.current = mode;

  useEffect(() => {
    const command = usesCommandModifier(navigator.userAgent);
    function onKeyDown(event: KeyboardEvent) {
      if (event.defaultPrevented) return;
      const def = resolveShellShortcut(event, {
        editable: isEditableTarget(event.target),
        modalOpen: hasOpenModalDialog(document),
        command,
        mode: modeRef.current(),
      });
      if (!def) return;
      event.preventDefault();
      handlersRef.current[def.id]();
    }
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, []);
}
