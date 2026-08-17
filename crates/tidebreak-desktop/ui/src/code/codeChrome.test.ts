import { describe, expect, it } from "vitest";

import { EMPTY_LAYOUT } from "@/panel/panelTypes";
import { splitCodeChromeLayout, toggleTerminalLayout } from "./codeChrome";

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
});
