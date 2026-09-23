import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { MENU_COMMANDS, menuCommandShortcut } from "./nativeMenu";

describe("menu commands", () => {
  it("runs the shortcut the same chord runs on the keyboard", () => {
    const chat = { modalOpen: false, mode: "chat" as const };
    expect(menuCommandShortcut("command-palette", chat)).toBe(
      "open-command-palette",
    );
    expect(menuCommandShortcut("settings", chat)).toBe("open-settings");
    expect(menuCommandShortcut("toggle-sidebar", chat)).toBe("toggle-sidebar");
    expect(menuCommandShortcut("zoom-in", chat)).toBe("zoom-in");
    expect(menuCommandShortcut("zoom-out", chat)).toBe("zoom-out");
    expect(menuCommandShortcut("zoom-reset", chat)).toBe("zoom-reset");
    expect(menuCommandShortcut("keyboard-shortcuts", chat)).toBe(
      "show-shortcuts",
    );
    // Documentation opens a page; it is no shortcut's to run.
    expect(menuCommandShortcut("documentation", chat)).toBeNull();
  });

  it("makes New whatever Cmd+N makes where the reader is", () => {
    expect(menuCommandShortcut("new", { modalOpen: false, mode: "chat" })).toBe(
      "new-chat",
    );
    expect(menuCommandShortcut("new", { modalOpen: false, mode: "code" })).toBe(
      "code-new-workspace",
    );
  });

  it("stays behind an open dialog exactly when the chord would", () => {
    // The menu is only another way to press the chord. The palette's own
    // item still closes the palette; nothing else reaches past a dialog.
    const behindDialog = { modalOpen: true, mode: "code" as const };
    expect(menuCommandShortcut("new", behindDialog)).toBeNull();
    expect(menuCommandShortcut("settings", behindDialog)).toBeNull();
    expect(menuCommandShortcut("command-palette", behindDialog)).toBe(
      "open-command-palette",
    );
  });

  it("names the same commands the native menu raises", () => {
    // `menu.rs` emits each item's id as the command. A name that drifts on
    // either side is a menu item that silently does nothing.
    const source = readFileSync(
      new URL("../../src/menu.rs", import.meta.url),
      "utf8",
    );
    const list = source.match(
      /const RENDERER_COMMANDS: \[&str; \d+\] = \[([^\]]*)\];/,
    );
    expect(list, "RENDERER_COMMANDS in menu.rs").not.toBeNull();
    const raised = (list?.[1] ?? "")
      .split(",")
      .map((name) => name.trim())
      .filter(Boolean)
      .map((name) => {
        const value = source.match(
          new RegExp(`const ${name}: &str = "([^"]+)";`),
        );
        expect(value, name).not.toBeNull();
        return value?.[1];
      });
    expect(new Set(raised)).toEqual(new Set(MENU_COMMANDS));
  });
});
