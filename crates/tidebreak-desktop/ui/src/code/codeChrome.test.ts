import { describe, expect, it } from "vitest";

import { EMPTY_LAYOUT } from "@/panel/panelTypes";
import {
  closeCodeChromeTab,
  focusCodeChromeTab,
  splitCodeChromeLayout,
  toggleTerminalLayout,
} from "./codeChrome";

const filesTerminalDiff = {
  tabs: [
    { type: "files" as const },
    { type: "terminal" as const },
    { type: "diff" as const },
  ],
  activeIndex: 0,
  fullscreen: false,
};

describe("code chrome layout", () => {
  it("keeps files and diff in the side region and lifts the terminal out", () => {
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
      panels: {
        tabs: [{ type: "files" }, { type: "diff" }],
        activeIndex: 1,
        fullscreen: false,
      },
      terminal: { type: "terminal" },
    });
  });

  it("keeps the visible side tab when the URL did not name the terminal", () => {
    expect(splitCodeChromeLayout(filesTerminalDiff).panels.activeIndex).toBe(0);
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
    const withFiles = {
      tabs: [{ type: "files" as const }],
      activeIndex: 0,
      fullscreen: false,
    };

    const opened = toggleTerminalLayout(withFiles);
    expect(opened.tabs).toEqual([{ type: "files" }, { type: "terminal" }]);
    expect(toggleTerminalLayout(opened)).toEqual(withFiles);
    expect(toggleTerminalLayout(EMPTY_LAYOUT).tabs).toEqual([{ type: "terminal" }]);
    expect(toggleTerminalLayout(toggleTerminalLayout(EMPTY_LAYOUT))).toEqual(
      EMPTY_LAYOUT,
    );
  });

  it("leaves the visible files or diff tab selected when the drawer opens", () => {
    const layout = {
      tabs: [{ type: "files" as const }, { type: "diff" as const }],
      activeIndex: 0,
      fullscreen: false,
    };

    const opened = toggleTerminalLayout(layout);
    expect(opened).toEqual({
      tabs: [{ type: "files" }, { type: "diff" }, { type: "terminal" }],
      activeIndex: 0,
      fullscreen: false,
    });
    expect(splitCodeChromeLayout(opened).panels.activeIndex).toBe(0);
  });

  it("focuses and closes a strip tab without treating the terminal as a strip index", () => {
    expect(focusCodeChromeTab(filesTerminalDiff, 1)).toEqual({
      ...filesTerminalDiff,
      activeIndex: 2,
    });
    expect(closeCodeChromeTab(filesTerminalDiff, 1)).toEqual({
      tabs: [{ type: "files" }, { type: "terminal" }],
      activeIndex: 0,
      fullscreen: false,
    });
    // Closing the showing Diff tab must land on Files, not the drawer
    // sitting between them in the URL.
    expect(
      closeCodeChromeTab({ ...filesTerminalDiff, activeIndex: 2 }, 1),
    ).toEqual({
      tabs: [{ type: "files" }, { type: "terminal" }],
      activeIndex: 0,
      fullscreen: false,
    });
  });
});
