import { describe, expect, it } from "vitest";

import { EMPTY_LAYOUT } from "@/panel/panelTypes";
import {
  closeAllEditorTabs,
  closeCodeChromeTab,
  closeEditorTab,
  closeEditorTabsToRight,
  closeFocusedCodeTab,
  closeOtherEditorTabs,
  codeBrowserIds,
  centerTabCount,
  selectCenterTab,
  stepCenterTab,
  focusCodeChromeTab,
  focusConversation,
  focusEditorTab,
  mergeEditorSplit,
  moveEditorTab,
  openCodeEditor,
  removedCodeBrowserIds,
  splitCodeChromeLayout,
  splitFocusedEditor,
  toggleTerminalLayout,
} from "./codeChrome";

const foldersTerminalAgents = {
  tabs: [
    { type: "folders" as const },
    { type: "terminal" as const },
    { type: "agents" as const },
  ],
  activeIndex: 0,
  fullscreen: false,
};

describe("code chrome layout", () => {
  it("lifts the terminal out of the side region into the drawer", () => {
    const layout = {
      tabs: [{ type: "terminal" as const }],
      activeIndex: 0,
      fullscreen: false,
    };

    expect(splitCodeChromeLayout(layout)).toEqual({
      panels: EMPTY_LAYOUT,
      editors: EMPTY_LAYOUT,
      splitEditors: EMPTY_LAYOUT,
      terminal: { type: "terminal" },
    });
  });

  it("keeps the visible side tab when the URL did not name the terminal", () => {
    expect(
      splitCodeChromeLayout(foldersTerminalAgents).panels.activeIndex,
    ).toBe(0);
    expect(splitCodeChromeLayout(foldersTerminalAgents).panels.tabs).toEqual([
      { type: "folders" },
      { type: "agents" },
    ]);
    expect(splitCodeChromeLayout(foldersTerminalAgents).editors.tabs).toEqual(
      [],
    );
    expect(
      splitCodeChromeLayout(foldersTerminalAgents).splitEditors.tabs,
    ).toEqual([]);
  });

  it("leaves a terminal-only layout as the conversation plus a drawer", () => {
    expect(
      splitCodeChromeLayout({
        tabs: [{ type: "terminal" }],
        activeIndex: 0,
        fullscreen: true,
      }),
    ).toEqual({
      panels: EMPTY_LAYOUT,
      editors: EMPTY_LAYOUT,
      splitEditors: EMPTY_LAYOUT,
      terminal: { type: "terminal" },
    });
  });

  it("opens and closes the terminal without dropping the other tabs", () => {
    const withFolders = {
      tabs: [{ type: "folders" as const }],
      activeIndex: 0,
      fullscreen: false,
    };

    const opened = toggleTerminalLayout(withFolders);
    expect(opened.tabs).toEqual([{ type: "folders" }, { type: "terminal" }]);
    expect(toggleTerminalLayout(opened)).toEqual(withFolders);
    expect(toggleTerminalLayout(EMPTY_LAYOUT).tabs).toEqual([
      { type: "terminal" },
    ]);
    expect(toggleTerminalLayout(toggleTerminalLayout(EMPTY_LAYOUT))).toEqual(
      EMPTY_LAYOUT,
    );
  });

  it("leaves the visible side tab selected when the drawer opens", () => {
    const layout = {
      tabs: [{ type: "folders" as const }, { type: "agents" as const }],
      activeIndex: 0,
      fullscreen: false,
    };

    const opened = toggleTerminalLayout(layout);
    expect(opened).toEqual({
      tabs: [{ type: "folders" }, { type: "agents" }, { type: "terminal" }],
      activeIndex: 0,
      fullscreen: false,
    });
    expect(splitCodeChromeLayout(opened).panels.activeIndex).toBe(0);
  });

  it("focuses and closes a strip tab without treating the terminal as a strip index", () => {
    expect(focusCodeChromeTab(foldersTerminalAgents, 1)).toEqual({
      ...foldersTerminalAgents,
      activeIndex: 2,
    });
    expect(closeCodeChromeTab(foldersTerminalAgents, 1)).toEqual({
      tabs: [{ type: "folders" }, { type: "terminal" }],
      activeIndex: 0,
      fullscreen: false,
    });
    // Closing the showing Agents tab must land on Folders, not the drawer
    // sitting between them in the URL.
    expect(
      closeCodeChromeTab({ ...foldersTerminalAgents, activeIndex: 2 }, 1),
    ).toEqual({
      tabs: [{ type: "folders" }, { type: "terminal" }],
      activeIndex: 0,
      fullscreen: false,
    });
  });

  it("keeps file tabs in the center and focuses chat without closing them", () => {
    const layout = {
      tabs: [
        { type: "file" as const, path: "src/lib.rs" },
        { type: "diff" as const, path: "src/lib.rs" },
        { type: "terminal" as const },
      ],
      activeIndex: 0,
      fullscreen: false,
    };
    const split = splitCodeChromeLayout(layout);
    expect(split.editors.tabs).toEqual([
      { type: "file", path: "src/lib.rs" },
      { type: "diff", path: "src/lib.rs" },
    ]);
    expect(split.panels.tabs).toEqual([]);
    expect(split.terminal).toEqual({ type: "terminal" });

    const chat = focusConversation(layout);
    expect(chat.conversationFocused).toBe(true);
    expect(focusEditorTab(chat, 1).conversationFocused).toBe(false);
    expect(focusEditorTab(chat, 1).activeIndex).toBe(1);
    expect(closeEditorTab(layout, 0).tabs.map((tab) => tab.type)).toEqual([
      "diff",
      "terminal",
    ]);
  });

  it("treats browser sessions as editor tabs without confusing them with the inspector", () => {
    const layout = {
      tabs: [
        { type: "folders" as const },
        { type: "browser" as const, browserId: "browser-1" },
      ],
      activeIndex: 1,
      fullscreen: false,
    };

    const chrome = splitCodeChromeLayout(layout);
    expect(chrome.panels.tabs).toEqual([{ type: "folders" }]);
    expect(chrome.editors.tabs).toEqual([
      { type: "browser", browserId: "browser-1" },
    ]);
    expect(
      openCodeEditor(layout, {
        type: "browser",
        browserId: "browser-2",
      }).tabs,
    ).toContainEqual({ type: "browser", browserId: "browser-2" });
  });

  it("reports only browser sessions truly removed by close operations", () => {
    const layout = {
      tabs: [
        { type: "browser" as const, browserId: "browser-1" },
        { type: "file" as const, path: "src/lib.rs" },
      ],
      activeIndex: 0,
      fullscreen: false,
      editorSplit: {
        tabs: [{ type: "browser" as const, browserId: "browser-2" }],
        activeIndex: 0,
      },
    };

    const moved = moveEditorTab(layout, "primary", 0, "secondary");
    expect(removedCodeBrowserIds(layout, moved)).toEqual([]);
    expect(codeBrowserIds(mergeEditorSplit(moved))).toEqual([
      "browser-2",
      "browser-1",
    ]);

    const closed = closeAllEditorTabs(layout, "secondary");
    expect(removedCodeBrowserIds(layout, closed)).toEqual(["browser-2"]);

    const primaryOnly = closeOtherEditorTabs(layout, 1, "primary");
    expect(removedCodeBrowserIds(layout, primaryOnly)).toEqual(["browser-1"]);

    const withBrowserOnRight = {
      ...layout,
      tabs: [
        { type: "file" as const, path: "src/lib.rs" },
        { type: "browser" as const, browserId: "browser-1" },
      ],
    };
    expect(
      removedCodeBrowserIds(
        withBrowserOnRight,
        closeEditorTabsToRight(withBrowserOnRight, 0, "primary"),
      ),
    ).toEqual(["browser-1"]);
    expect(removedCodeBrowserIds(layout, closeAllEditorTabs(layout))).toEqual([
      "browser-1",
      "browser-2",
    ]);
  });

  it("closes editor-tab groups without dropping side panels or the terminal", () => {
    const layout = {
      tabs: [
        { type: "folders" as const },
        { type: "file" as const, path: "src/lib.rs" },
        { type: "diff" as const, path: "src/main.rs" },
        { type: "file" as const, path: "README.md" },
        { type: "terminal" as const },
      ],
      activeIndex: 3,
      fullscreen: false,
    };

    expect(closeEditorTabsToRight(layout, 0)).toEqual({
      ...layout,
      tabs: [
        { type: "folders" },
        { type: "file", path: "src/lib.rs" },
        { type: "terminal" },
      ],
      activeIndex: 1,
    });
    expect(closeOtherEditorTabs(layout, 1)).toEqual({
      ...layout,
      tabs: [
        { type: "folders" },
        { type: "diff", path: "src/main.rs" },
        { type: "terminal" },
      ],
      activeIndex: 1,
      conversationFocused: false,
    });
    expect(closeAllEditorTabs(layout)).toEqual({
      ...layout,
      tabs: [{ type: "folders" }, { type: "terminal" }],
      activeIndex: 0,
      conversationFocused: undefined,
    });
  });

  it("scopes close actions to the editor group that owns the menu", () => {
    const layout = {
      tabs: [
        { type: "folders" as const },
        { type: "file" as const, path: "src/lib.rs" },
        { type: "diff" as const, path: "src/main.rs" },
        { type: "terminal" as const },
      ],
      activeIndex: 2,
      fullscreen: false,
      editorSplit: {
        tabs: [
          { type: "file" as const, path: "README.md" },
          { type: "diff" as const, path: "README.md" },
        ],
        activeIndex: 1,
        focused: true,
      },
    };

    expect(closeAllEditorTabs(layout, "primary")).toEqual({
      ...layout,
      tabs: [{ type: "folders" }, { type: "terminal" }],
      activeIndex: 0,
      conversationFocused: undefined,
    });
    expect(closeAllEditorTabs(layout, "secondary")).toEqual({
      ...layout,
      editorSplit: undefined,
    });

    expect(closeOtherEditorTabs(layout, 0, "primary")).toEqual({
      ...layout,
      tabs: [
        { type: "folders" },
        { type: "file", path: "src/lib.rs" },
        { type: "terminal" },
      ],
      activeIndex: 1,
      conversationFocused: false,
      editorSplit: { ...layout.editorSplit, focused: undefined },
    });
    expect(closeOtherEditorTabs(layout, 0, "secondary")).toEqual({
      ...layout,
      editorSplit: {
        tabs: [{ type: "file", path: "README.md" }],
        activeIndex: 0,
        focused: true,
      },
    });
  });

  it("moves editor tabs into a durable secondary group and back", () => {
    const layout = {
      tabs: [
        { type: "file" as const, path: "src/lib.rs" },
        { type: "diff" as const, path: "src/main.rs" },
        { type: "terminal" as const },
      ],
      activeIndex: 0,
      fullscreen: false,
    };

    const split = moveEditorTab(layout, "primary", 0, "secondary");
    expect(split.tabs).toEqual([
      { type: "diff", path: "src/main.rs" },
      { type: "terminal" },
    ]);
    expect(split.editorSplit).toEqual({
      tabs: [{ type: "file", path: "src/lib.rs" }],
      activeIndex: 0,
      focused: true,
    });
    expect(splitCodeChromeLayout(split).splitEditors.tabs).toEqual([
      { type: "file", path: "src/lib.rs" },
    ]);

    expect(moveEditorTab(split, "secondary", 0, "primary").editorSplit).toBe(
      undefined,
    );
    expect(
      mergeEditorSplit(split).tabs.filter((tab) => tab.type === "file"),
    ).toEqual([{ type: "file", path: "src/lib.rs" }]);
  });

  it("counts the main agent as the first numbered tab", () => {
    // Cmd+1 names a position on screen, and position one is the main agent —
    // the tab the strip always draws first and the URL never stores as one.
    const layout = {
      tabs: [
        { type: "file" as const, path: "src/lib.rs" },
        { type: "diff" as const, path: "src/main.rs" },
        { type: "terminal" as const },
      ],
      activeIndex: 0,
      fullscreen: false,
      conversationFocused: false,
    };

    expect(centerTabCount(layout)).toBe(3);
    expect(selectCenterTab(layout, 0).conversationFocused).toBe(true);
    const second = selectCenterTab(layout, 2);
    expect(second.conversationFocused).toBe(false);
    expect(second.tabs[second.activeIndex]).toEqual({
      type: "diff",
      path: "src/main.rs",
    });
    // The terminal is a drawer, not a tab, so it is not position four.
    expect(selectCenterTab(layout, 3)).toBe(layout);
  });

  it("numbers the split group on its own, with no main agent in it", () => {
    // The right-hand group draws editor tabs only, so its first position is a
    // file rather than the conversation the left group leads with.
    const layout = {
      tabs: [{ type: "file" as const, path: "src/lib.rs" }],
      activeIndex: 0,
      fullscreen: false,
      conversationFocused: false,
      editorSplit: {
        tabs: [
          { type: "file" as const, path: "README.md" },
          { type: "file" as const, path: "Cargo.toml" },
        ],
        activeIndex: 0,
        focused: true,
      },
    };

    expect(centerTabCount(layout)).toBe(2);
    expect(selectCenterTab(layout, 1).editorSplit?.activeIndex).toBe(1);
  });

  it("wraps when cycling past either end of the strip", () => {
    const layout = {
      tabs: [
        { type: "file" as const, path: "src/lib.rs" },
        { type: "diff" as const, path: "src/main.rs" },
      ],
      activeIndex: 0,
      fullscreen: false,
      conversationFocused: true,
    };

    // Back from the main agent lands on the last tab, not on nothing.
    expect(stepCenterTab(layout, -1).activeIndex).toBe(1);
    expect(stepCenterTab(layout, 1).activeIndex).toBe(0);
    // A strip of one is the conversation alone; cycling has nowhere to go.
    expect(stepCenterTab(EMPTY_LAYOUT, 1)).toBe(EMPTY_LAYOUT);
  });

  it("sends the focused tab whichever way it can go", () => {
    const layout = {
      tabs: [
        { type: "file" as const, path: "src/lib.rs" },
        { type: "diff" as const, path: "src/main.rs" },
      ],
      activeIndex: 1,
      fullscreen: false,
    };

    // From the left group the only direction is right.
    const split = splitFocusedEditor(layout);
    expect(split.tabs).toHaveLength(1);
    expect(split.editorSplit?.tabs).toEqual([
      { type: "diff", path: "src/main.rs" },
    ]);
    expect(split.editorSplit?.focused).toBe(true);

    // And from the right group, back again: one chord, both directions.
    expect(splitFocusedEditor(split).editorSplit?.tabs ?? []).toHaveLength(0);
    expect(splitFocusedEditor(split).tabs).toHaveLength(2);

    // The conversation is not a tab the split can hold, and an empty strip has
    // nothing to send. Both decline by returning the layout untouched, which is
    // what tells the shell to let the key through.
    const onConversation = { ...layout, conversationFocused: true };
    expect(splitFocusedEditor(onConversation)).toBe(onConversation);
    expect(splitFocusedEditor(EMPTY_LAYOUT)).toBe(EMPTY_LAYOUT);
  });

  it("closes the editor tab that owns focus for the shell shortcut", () => {
    const primary = {
      tabs: [
        { type: "file" as const, path: "src/lib.rs" },
        { type: "diff" as const, path: "src/main.rs" },
        { type: "terminal" as const },
      ],
      activeIndex: 1,
      fullscreen: false,
      conversationFocused: false,
      editorSplit: {
        tabs: [{ type: "file" as const, path: "README.md" }],
        activeIndex: 0,
      },
    };

    expect(closeFocusedCodeTab(primary)).toEqual({
      ...primary,
      tabs: [{ type: "file", path: "src/lib.rs" }, { type: "terminal" }],
      activeIndex: 0,
    });

    const secondary = {
      ...primary,
      editorSplit: { ...primary.editorSplit, focused: true },
    };
    expect(closeFocusedCodeTab(secondary)).toEqual({
      ...primary,
      editorSplit: undefined,
    });
  });

  it("does not claim Cmd/Ctrl+W when the persistent conversation owns focus", () => {
    const layout = {
      tabs: [{ type: "file" as const, path: "src/lib.rs" }],
      activeIndex: 0,
      fullscreen: false,
      conversationFocused: true,
    };

    expect(closeFocusedCodeTab(layout)).toBeNull();
    expect(closeFocusedCodeTab(EMPTY_LAYOUT)).toBeNull();
  });

  it("opens new editors in the group that last received focus", () => {
    const split = moveEditorTab(
      {
        tabs: [{ type: "file", path: "src/lib.rs" }],
        activeIndex: 0,
        fullscreen: false,
      },
      "primary",
      0,
      "secondary",
    );
    const opened = openCodeEditor(split, {
      type: "file",
      path: "src/main.rs",
    });
    expect(opened.editorSplit?.tabs).toEqual([
      { type: "file", path: "src/lib.rs" },
      { type: "file", path: "src/main.rs" },
    ]);
    expect(opened.editorSplit?.activeIndex).toBe(1);
  });
});
