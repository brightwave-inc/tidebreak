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
    metaKey: true,
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    ...overrides,
  } as KeyboardEvent;
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
    const cases: Array<[string, ShellShortcutAction]> = [
      ["b", "toggle-sidebar"],
      ["n", "new-chat"],
      ["k", "focus-composer"],
      ["/", "show-shortcuts"],
    ];
    for (const [key, id] of cases) {
      const resolved = resolveShellShortcut(keyEvent({ key }), {
        editable: true,
        modalOpen: false,
      });
      expect(resolved?.id).toBe(id);
    }
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
      const resolved = resolveShellShortcut(keyEvent({ key, shiftKey }), {
        editable: true,
        modalOpen: false,
      });
      expect(resolved?.id).toBe(key === "=" || key === "+" ? "zoom-in" : "zoom-out");
    }
  });

  it("stays out of the way of plain typing", () => {
    const resolved = resolveShellShortcut(
      keyEvent({ key: "n", metaKey: false, ctrlKey: false }),
      { editable: true, modalOpen: false },
    );
    expect(resolved).toBeNull();
  });

  it("suppresses every shortcut while a modal dialog is open", () => {
    const resolved = resolveShellShortcut(keyEvent({ key: "n" }), {
      editable: false,
      modalOpen: true,
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
