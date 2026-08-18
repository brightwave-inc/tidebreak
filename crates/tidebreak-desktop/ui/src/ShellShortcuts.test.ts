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
  type ShellShortcutMode,
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

/** The default context: a mac-style keyboard in chat, nothing in the way. */
function context(overrides: Partial<Parameters<typeof resolveShellShortcut>[1]> = {}) {
  return {
    editable: true,
    modalOpen: false,
    command: true,
    mode: "chat" as ShellShortcutMode,
    ...overrides,
  };
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
      [{ key: "r", code: "KeyR" }, "reload-app"],
    ];
    for (const [event, id] of cases) {
      const resolved = resolveShellShortcut(keyEvent(event), context());
      expect(resolved?.id).toBe(id);
    }
  });

  it("gives Cmd+N to whichever mode the reader is in", () => {
    // One chord, two modes: a conversation in chat, a workspace in code. The
    // regression this pins is Cmd+N in code mode creating a chat and taking
    // the reader out of the mode they were working in.
    const cmdN = keyEvent({ key: "n", code: "KeyN" });
    expect(resolveShellShortcut(cmdN, context({ mode: "chat" }))?.id).toBe(
      "new-chat",
    );
    expect(resolveShellShortcut(cmdN, context({ mode: "code" }))?.id).toBe(
      "code-new-workspace",
    );
    expect(
      resolveShellShortcut(
        keyEvent({ key: "i", code: "KeyI" }),
        context({ mode: "code" }),
      )?.id,
    ).toBe("toggle-code-review");
    expect(
      resolveShellShortcut(
        keyEvent({ key: "j", code: "KeyJ" }),
        context({ mode: "code" }),
      )?.id,
    ).toBe("toggle-code-terminal");
    expect(
      resolveShellShortcut(
        keyEvent({ key: "i", code: "KeyI" }),
        context({ mode: "chat" }),
      ),
    ).toBeNull();
    expect(
      resolveShellShortcut(
        keyEvent({ key: "j", code: "KeyJ" }),
        context({ mode: "chat" }),
      ),
    ).toBeNull();
    expect(
      resolveShellShortcut(
        keyEvent({ key: "w", code: "KeyW" }),
        context({ mode: "chat" }),
      ),
    ).toBeNull();
    expect(
      resolveShellShortcut(
        keyEvent({ key: "w", code: "KeyW" }),
        context({ mode: "code" }),
      )?.id,
    ).toBe("close-tab");
    // Unscoped shortcuts are the frame's and act the same on both sides.
    expect(
      resolveShellShortcut(keyEvent({}), context({ mode: "code" }))?.id,
    ).toBe("toggle-sidebar");
  });

  it("leaves Alt+Arrow to the field the reader is typing in", () => {
    // Option+Arrow jumps a word in every macOS text field. History navigation
    // takes it only when focus is somewhere the keys mean nothing else.
    const back = keyEvent({
      key: "ArrowLeft",
      code: "ArrowLeft",
      metaKey: false,
      altKey: true,
    });
    expect(resolveShellShortcut(back, context())).toBeNull();
    expect(resolveShellShortcut(back, context({ editable: false }))?.id).toBe(
      "history-back",
    );
    expect(
      resolveShellShortcut(
        keyEvent({
          key: "ArrowRight",
          code: "ArrowRight",
          metaKey: false,
          altKey: true,
        }),
        context({ editable: false, command: false }),
      )?.id,
    ).toBe("history-forward");
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
      mode: "chat",
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
    // Per mode, because the help lists what fires where it is being read.
    for (const mode of ["chat", "code"] as ShellShortcutMode[]) {
      const listed = groupedShellShortcuts(mode).flatMap(({ items }) => items);
      const live = SHELL_SHORTCUTS.filter(
        (def) => def.scope === undefined || def.scope === mode,
      );
      expect(listed).toHaveLength(live.length);
      expect(new Set(listed)).toEqual(new Set(live));
    }
  });
});

describe("shortcut table", () => {
  it("binds each chord to one action per mode", () => {
    // Scoping makes it possible for two definitions to share a chord, which is
    // the point — and also the way a shortcut silently stops firing, because
    // resolution takes the first match in table order. Sharing is allowed only
    // across modes; within one, a chord means exactly one thing.
    const owners = new Map<string, ShellShortcutAction>();
    for (const def of SHELL_SHORTCUTS) {
      const modes: ShellShortcutMode[] = def.scope
        ? [def.scope]
        : ["chat", "code"];
      // "any" shift matches held and unheld alike, so it occupies both.
      const shifts = def.shift === "any" ? [true, false] : [def.shift ?? false];
      const keys = def.codes
        ? def.codes.map((code) => `code:${code}`)
        : def.keys.map((key) => `key:${key}`);
      for (const mode of modes) {
        for (const shift of shifts) {
          for (const key of keys) {
            const chord = [mode, def.mod, def.alt ?? false, shift, key].join("|");
            const owner = owners.get(chord);
            expect(owner, `${chord} is bound twice`).toBeUndefined();
            owners.set(chord, def.id);
          }
        }
      }
    }
  });
});
