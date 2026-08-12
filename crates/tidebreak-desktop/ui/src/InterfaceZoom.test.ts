import { describe, expect, it } from "vitest";

import { DEFAULT_ZOOM, ZOOM_STEPS, nextZoom } from "./InterfaceZoom";

describe("nextZoom", () => {
  it("stops at both ends rather than running off the steps", () => {
    const smallest = ZOOM_STEPS[0] as number;
    const largest = ZOOM_STEPS[ZOOM_STEPS.length - 1] as number;

    expect(nextZoom(smallest, "out")).toBe(smallest);
    expect(nextZoom(largest, "in")).toBe(largest);
  });

  it("moves one step from a level that is not on the steps", () => {
    // A level restored from an older set of steps, or one the platform rounded,
    // still has to move by a single press instead of jumping to an end.
    const above = nextZoom(1.02, "in");
    const below = nextZoom(1.02, "out");

    expect(above).toBeGreaterThan(DEFAULT_ZOOM);
    expect(below).toBeLessThan(DEFAULT_ZOOM);
    expect(ZOOM_STEPS.indexOf(above) - ZOOM_STEPS.indexOf(below)).toBe(2);
  });
});
