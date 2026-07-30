// @vitest-environment jsdom
import { describe, expect, it } from "vitest";

import {
  groupedShellShortcuts,
  isEditableTarget,
  resolveShellShortcut,
  SHELL_SHORTCUTS,
  shortcutKeycaps,
  usesCommandModifier,
  type ShellShortcutAction,
} from "./ShellShortcuts";

function keyEvent(overrides: Partial<KeyboardEvent>): KeyboardEvent {
  return {
    key: "b",
    code: "KeyB",
    metaKey: true,
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    ...overrides,
  } as KeyboardEvent;
}

/** The default context: a mac-style keyboard, nothing in the way. */
function context(overrides: Partial<Parameters<typeof resolveShellShortcut>[1]> = {}) {
  return { editable: true, modalOpen: false, command: true, ...overrides };
}

describe("shell shortcut resolution", () => {
  it("classifies text fields as editable and other elements as not", () => {
    const input = document.createElement("input");
    const textarea = document.createElement("textarea");
    const editable = document.createElement("div");
    editable.setAttribute("contenteditable", "true");
    const plain = document.createElement("div");

    expect(isEditableTarget(input)).toBe(true);
    expect(isEditableTarget(textarea)).toBe(true);
    expect(isEditableTarget(editable)).toBe(true);
    expect(isEditableTarget(plain)).toBe(false);
    expect(isEditableTarget(null)).toBe(false);
  });

  it("fires chorded shortcuts even while the composer has focus", () => {
    // The whole point of mod-chorded combos: the reader can reach them without
    // leaving the field they are typing in.
    const cases: Array<[Partial<KeyboardEvent>, ShellShortcutAction]> = [
      [{ key: "b", code: "KeyB" }, "toggle-sidebar"],
      [{ key: "n", code: "KeyN" }, "new-chat"],
      [{ key: "k", code: "KeyK" }, "focus-composer"],
      [{ key: "/", code: "Slash" }, "show-shortcuts"],
      [{ key: "0", code: "Digit0" }, "zoom-reset"],
    ];
    for (const [event, id] of cases) {
      const resolved = resolveShellShortcut(keyEvent(event), context());
      expect(resolved?.id).toBe(id);
    }
  });

  it("keeps letter shortcuts under the same physical keys on any layout", () => {
    // Dvorak puts "l" where QWERTY puts N. The shortcut belongs to the key
    // position the reader's muscle memory knows, not to the character that
    // position happens to produce.
    const dvorakN = keyEvent({ key: "l", code: "KeyN" });
    expect(resolveShellShortcut(dvorakN, context())?.id).toBe("new-chat");

    // And the key that does produce "n" on Dvorak is not a new chat.
    const dvorakElsewhere = keyEvent({ key: "n", code: "KeyL" });
    expect(resolveShellShortcut(dvorakElsewhere, context())).toBeNull();
  });

  it("matches punctuation on the character, which has no fixed position", () => {
    // "/" sits on Period on AZERTY, on Bracketleft-adjacent keys elsewhere. The
    // glyph is what the shortcut is named after, so the glyph is what matches.
    const azertySlash = keyEvent({ key: "/", code: "Period", shiftKey: true });
    expect(resolveShellShortcut(azertySlash, context())?.id).toBe(
      "show-shortcuts",
    );
  });

  it("takes only the modifier the platform actually uses", () => {
    // Ctrl+B on macOS is the caret moving back a character; the sidebar has no
    // claim on it. The same chord is the real binding everywhere else.
    const ctrlB = keyEvent({ metaKey: false, ctrlKey: true });
    expect(resolveShellShortcut(ctrlB, context())).toBeNull();
    expect(resolveShellShortcut(ctrlB, context({ command: false }))?.id).toBe(
      "toggle-sidebar",
    );
    expect(
      resolveShellShortcut(keyEvent({}), context({ command: false })),
    ).toBeNull();
  });

  it("reaches zoom whether or not shift is held for the key", () => {
    // Cmd+= and Cmd+Shift+= are the same intent on a US keyboard, and the
    // reader has no way to know which one the app is listening for.
    for (const [key, shiftKey] of [
      ["=", false],
      ["+", true],
      ["-", false],
      ["_", true],
    ] as Array<[string, boolean]>) {
      const resolved = resolveShellShortcut(
        keyEvent({ key, shiftKey, code: "Equal" }),
        context(),
      );
      expect(resolved?.id).toBe(key === "=" || key === "+" ? "zoom-in" : "zoom-out");
    }
  });

  it("stays out of the way of plain typing", () => {
    const resolved = resolveShellShortcut(
      keyEvent({ key: "n", code: "KeyN", metaKey: false, ctrlKey: false }),
      context(),
    );
    expect(resolved).toBeNull();
  });

  it("suppresses every shortcut while a modal dialog is open", () => {
    const resolved = resolveShellShortcut(keyEvent({ key: "n", code: "KeyN" }), {
      editable: false,
      modalOpen: true,
      command: true,
    });
    expect(resolved).toBeNull();
  });
});

describe("shortcut help", () => {
  it("names the modifier the reader's keyboard actually has", () => {
    // The table records `mod: true` because the listener takes either modifier;
    // the help dialog has to pick the one that is true where it is being read.
    const zoomIn = SHELL_SHORTCUTS.find((def) => def.id === "zoom-in")!;
    expect(shortcutKeycaps(zoomIn, true)).toEqual(["⌘", "="]);
    expect(shortcutKeycaps(zoomIn, false)).toEqual(["Ctrl", "="]);
    expect(usesCommandModifier("Mozilla/5.0 (Macintosh; Intel Mac OS X)")).toBe(
      true,
    );
    expect(usesCommandModifier("Mozilla/5.0 (Windows NT 10.0; Win64)")).toBe(
      false,
    );
  });

  it("files every shortcut under a heading the dialog renders", () => {
    // A shortcut grouped under a heading nobody lists would vanish from the
    // help while still firing — the exact drift the dialog exists to prevent.
    const listed = groupedShellShortcuts().flatMap(({ items }) => items);
    expect(listed).toHaveLength(SHELL_SHORTCUTS.length);
    expect(new Set(listed)).toEqual(new Set(SHELL_SHORTCUTS));
  });
});
