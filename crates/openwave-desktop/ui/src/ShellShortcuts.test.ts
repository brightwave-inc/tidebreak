// @vitest-environment jsdom
import { describe, expect, it } from "vitest";

import {
  isEditableTarget,
  resolveShellShortcut,
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
    ];
    for (const [key, id] of cases) {
      const resolved = resolveShellShortcut(keyEvent({ key }), {
        editable: true,
        modalOpen: false,
      });
      expect(resolved?.id).toBe(id);
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
