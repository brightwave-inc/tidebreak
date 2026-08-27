import { describe, expect, it } from "vitest";

import { RESIZABLE_HANDLE_CLASS } from "./resizable";

/**
 * `react-resizable-panels` labels a separator by its own axis, not by its
 * group's: a horizontal group is split by a separator that reports
 * `aria-orientation="vertical"`. Reading that word as "the group is vertical"
 * made the handle a full-width row inside every horizontal group, which left
 * the panels beside it zero pixels wide — the code workspace lost its journal,
 * composer, and center tabs the moment the inspector opened.
 */
describe("resizable handle", () => {
  it("sizes the bar from the separator's own axis", () => {
    expect(RESIZABLE_HANDLE_CLASS).toContain(
      "aria-[orientation=horizontal]:w-full",
    );
    expect(RESIZABLE_HANDLE_CLASS).toContain(
      "aria-[orientation=horizontal]:h-px",
    );
  });

  it("never grows a vertical separator to the full width of its group", () => {
    expect(RESIZABLE_HANDLE_CLASS).not.toContain("aria-[orientation=vertical]");
    expect(RESIZABLE_HANDLE_CLASS).toContain("w-px");
  });
});
