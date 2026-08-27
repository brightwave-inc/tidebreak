import { describe, expect, it, vi } from "vitest";

import { codeNavigationPaletteRows } from "./codePaletteRows";

describe("codeNavigationPaletteRows", () => {
  it("puts analytics before delivery and navigates to its route", () => {
    const navigate = vi.fn();
    const rows = codeNavigationPaletteRows({
      navigate,
      onNewWorkspace: vi.fn(),
      onQuickOpen: vi.fn(),
    });

    const analytics = rows.find((row) => row.id === "navigate:analytics");
    const delivery = rows.findIndex(
      (row) => row.id === "navigate:pull-requests",
    );

    expect(analytics).toBeDefined();
    expect(rows.indexOf(analytics!)).toBeLessThan(delivery);
    analytics?.onSelect();
    expect(navigate).toHaveBeenCalledWith("/code/analytics");
  });

  it("does not keep a Delivery notifications destination", () => {
    const navigate = vi.fn();
    const rows = codeNavigationPaletteRows({
      navigate,
      onNewWorkspace: vi.fn(),
      onQuickOpen: vi.fn(),
    });

    expect(rows.some((row) => row.id === "navigate:notifications")).toBe(false);
    for (const row of rows) {
      navigate.mockClear();
      row.onSelect();
    }
    expect(navigate.mock.calls.flat()).not.toContain("/code/notifications");
  });
});
