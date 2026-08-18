import { describe, expect, it } from "vitest";

import { EMPTY_LAYOUT } from "@/panel/panelTypes";
import {
  closeCodeChromeTab,
  focusCodeChromeTab,
  splitCodeChromeLayout,
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
  it("lifts the terminal out and drops files/diff so they stay in the inspector", () => {
    const layout = {
      tabs: [
        { type: "files" as const },
        { type: "terminal" as const },
        { type: "diff" as const },
      ],
      activeIndex: 1,
      fullscreen: false,
    };

    expect(splitCodeChromeLayout(layout)).toEqual({
      panels: EMPTY_LAYOUT,
      terminal: { type: "terminal" },
    });
  });

  it("keeps the visible side tab when the URL did not name the terminal", () => {
    expect(splitCodeChromeLayout(foldersTerminalAgents).panels.activeIndex).toBe(0);
    expect(splitCodeChromeLayout(foldersTerminalAgents).panels.tabs).toEqual([
      { type: "folders" },
      { type: "agents" },
    ]);
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
    expect(toggleTerminalLayout(EMPTY_LAYOUT).tabs).toEqual([{ type: "terminal" }]);
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
});
