import { describe, expect, it } from "vitest";

import { memorySummary } from "./chatMemoryPresence";

describe("memorySummary", () => {
  it("phrases every state the row can be in", () => {
    expect(memorySummary({ enabled: false, recordCount: 4 }, false)).toBe(
      "Off",
    );
    expect(memorySummary({ enabled: true, recordCount: 4 }, true)).toBe(
      "Off for this chat",
    );
    expect(memorySummary({ enabled: true, recordCount: 0 }, false)).toBe(
      "On · nothing approved yet",
    );
    expect(memorySummary({ enabled: true, recordCount: 1 }, false)).toBe(
      "1 record in context",
    );
    expect(memorySummary({ enabled: true, recordCount: 3 }, false)).toBe(
      "3 records in context",
    );
  });
});
