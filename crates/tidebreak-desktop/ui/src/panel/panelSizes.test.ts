import { describe, expect, it } from "vitest";

import { panelSizes } from "./PanelLayout";

describe("panelSizes", () => {
  it("gives the conversation the window when nothing is open beside it", () => {
    expect(panelSizes({ hasTabs: false })).toEqual({ chat: 100, panels: 0 });
    // Expanding is meaningless with nothing to expand.
    expect(panelSizes({ hasTabs: false, fullscreen: true })).toEqual({
      chat: 100,
      panels: 0,
    });
  });

  it("splits evenly with panels open", () => {
    expect(panelSizes({ hasTabs: true })).toEqual({ chat: 50, panels: 50 });
  });

  it("hands the window to an expanded panel", () => {
    expect(panelSizes({ hasTabs: true, fullscreen: true })).toEqual({
      chat: 0,
      panels: 100,
    });
  });
});
