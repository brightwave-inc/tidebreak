import { describe, expect, it } from "vitest";

import { sidebarUsesOverlay } from "./sidebarLayout";

describe("sidebarUsesOverlay", () => {
  it("collapses the rail at the 720px minimum window", () => {
    expect(sidebarUsesOverlay(720)).toBe(true);
  });

  it("keeps the rail in layout at 900px and above", () => {
    expect(sidebarUsesOverlay(900)).toBe(false);
    expect(sidebarUsesOverlay(1280)).toBe(false);
  });
});
