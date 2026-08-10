import { describe, expect, it } from "vitest";

import {
  compactionUpdateFrom,
  toCompactionForm,
} from "./CompactionPanel";

const SETTINGS = {
  threshold_fraction: 0.75,
  target_fraction: 0.25,
  min_threshold_tokens: 50_000,
  protect_recent_messages: 5,
};

describe("compaction settings form", () => {
  it("round-trips the stored fractions as whole percentages", () => {
    const form = toCompactionForm(SETTINGS);
    expect(form).toEqual({
      thresholdPercent: "75",
      targetPercent: "25",
      protectRecent: "5",
    });
    expect(compactionUpdateFrom(form)).toEqual({
      update: {
        threshold_fraction: 0.75,
        target_fraction: 0.25,
        protect_recent_messages: 5,
      },
    });
  });

  it("refuses what the server would refuse, before the request", () => {
    // Compacting down to more than the point compaction starts at is a policy
    // the server rejects outright; the reader hears it from the form instead.
    const inverted = compactionUpdateFrom({
      thresholdPercent: "25",
      targetPercent: "75",
      protectRecent: "5",
    });
    expect(inverted).toEqual({
      error: "The compaction point must be above what compaction leaves behind.",
    });
    expect(
      compactionUpdateFrom({
        thresholdPercent: "75",
        targetPercent: "25",
        protectRecent: "0",
      }),
    ).toHaveProperty("error");
    expect(
      compactionUpdateFrom({
        thresholdPercent: "0",
        targetPercent: "25",
        protectRecent: "5",
      }),
    ).toHaveProperty("error");
  });
});
