import { describe, expect, it } from "vitest";

import { panelSizes } from "./PanelLayout";

const NOTHING_OPEN = { isSplit: false, hasLeft: false, hasRight: false };

describe("panelSizes", () => {
  it("gives the conversation everything when nothing else is open", () => {
    expect(panelSizes(NOTHING_OPEN)).toEqual([0, 100, 0]);
  });

  it("splits evenly with one panel open", () => {
    expect(panelSizes({ isSplit: true, hasLeft: true, hasRight: false })).toEqual([50, 50, 0]);
    expect(panelSizes({ isSplit: true, hasLeft: false, hasRight: true })).toEqual([0, 50, 50]);
  });

  it("steps the conversation out when both slots are taken", () => {
    // Three columns in a desktop window is not a readable transcript.
    expect(panelSizes({ isSplit: true, hasLeft: true, hasRight: true })).toEqual([50, 0, 50]);
  });

  it("gives a fullscreen panel the whole width", () => {
    expect(
      panelSizes({ isSplit: true, hasLeft: true, hasRight: true, fullscreen: "left" }),
    ).toEqual([100, 0, 0]);
    expect(
      panelSizes({ isSplit: true, hasLeft: true, hasRight: true, fullscreen: "right" }),
    ).toEqual([0, 0, 100]);
  });

  it("always totals a full width", () => {
    for (const fullscreen of [undefined, "left", "right"] as const) {
      for (const hasLeft of [false, true]) {
        for (const hasRight of [false, true]) {
          for (const isSplit of [false, true]) {
            const sizes = panelSizes({ isSplit, fullscreen, hasLeft, hasRight });
            expect(sizes.reduce((total, size) => total + size, 0)).toBe(100);
          }
        }
      }
    }
  });
});
