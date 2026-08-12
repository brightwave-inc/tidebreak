import { describe, expect, it } from "vitest";

import { nextNavigationReach } from "./DesktopNavigation";

describe("nextNavigationReach", () => {
  it("keeps forward live after going back, and kills it once a push replaces the entries ahead", () => {
    let reach = { index: 0, furthest: 0 };
    reach = nextNavigationReach(reach, "PUSH", 1);
    reach = nextNavigationReach(reach, "PUSH", 2);
    expect(reach).toEqual({ index: 2, furthest: 2 });

    reach = nextNavigationReach(reach, "BACK", 1);
    expect(reach.index).toBeLessThan(reach.furthest);

    // Navigating somewhere new from a back entry discards what was ahead, so
    // forward has to go dead even though the stack is no shorter than before.
    reach = nextNavigationReach(reach, "PUSH", 2);
    expect(reach).toEqual({ index: 2, furthest: 2 });
  });
});
