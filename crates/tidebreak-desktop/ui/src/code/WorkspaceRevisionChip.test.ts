import { describe, expect, it } from "vitest";

import { savedAgo } from "./WorkspaceRevisionChip";

describe("savedAgo", () => {
  it("reads a checkpoint's age in the fewest characters a chip can hold", () => {
    expect(savedAgo(-5_000)).toBe("just now");
    expect(savedAgo(9_999)).toBe("just now");
    expect(savedAgo(42_000)).toBe("42s ago");
    expect(savedAgo(3 * 60_000 + 5_000)).toBe("3m ago");
    expect(savedAgo(2 * 3_600_000)).toBe("2h ago");
    expect(savedAgo(4 * 86_400_000)).toBe("4d ago");
  });
});
