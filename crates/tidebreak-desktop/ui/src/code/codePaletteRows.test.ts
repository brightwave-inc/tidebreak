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
});
