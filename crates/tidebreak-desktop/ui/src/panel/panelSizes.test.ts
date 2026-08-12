import { describe, expect, it } from "vitest";

import { panelSizes } from "./PanelLayout";

describe("panelSizes", () => {
  it("gives the conversation the window when nothing is open beside it", () => {
    expect(panelSizes({ hasTabs: false })).toEqual([100, 0]);
    // Expanding is meaningless with nothing to expand.
    expect(panelSizes({ hasTabs: false, fullscreen: true })).toEqual([100, 0]);
  });

  it("splits evenly with panels open", () => {
    expect(panelSizes({ hasTabs: true })).toEqual([50, 50]);
  });

  it("hands the window to an expanded panel", () => {
    expect(panelSizes({ hasTabs: true, fullscreen: true })).toEqual([0, 100]);
  });
});
