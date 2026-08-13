import { describe, expect, it } from "vitest";
import {
  contextTokens,
  contextTruncationNotice,
  contextUsageLevel,
  contextUsagePercent,
  formatTokenCount,
} from "./ContextUsage";

const USAGE = {
  input_tokens: 1_000,
  output_tokens: 500,
  cache_read_input_tokens: 60_000,
  cache_creation_input_tokens: 2_500,
};

describe("context occupancy", () => {
  it("counts cached prompt tokens as occupying the window", () => {
    // The four counts are disjoint — a cache hit still fills the window, and
    // a meter that read only `input_tokens` would report 1k against 200k for
    // a conversation actually holding 64k.
    expect(contextTokens(USAGE)).toBe(64_000);
    expect(contextUsagePercent(USAGE, 200_000)).toBe(32);
  });

  it("has no percent when the model's window is unknown", () => {
    expect(contextUsagePercent(USAGE, undefined)).toBeNull();
    expect(contextUsagePercent(USAGE, 0)).toBeNull();
  });

  it("clamps a turn whose totals exceed the window", () => {
    // A long multi-step turn re-sends its transcript on every model call, so
    // the summed totals legitimately run past the window. "Full" is the most
    // a reader can act on.
    expect(contextUsagePercent(USAGE, 8_000)).toBe(100);
  });
});

describe("thresholds", () => {
  it("escalates at 75 and 90, inclusive", () => {
    expect(contextUsageLevel(74)).toBe("normal");
    expect(contextUsageLevel(75)).toBe("warning");
    expect(contextUsageLevel(89)).toBe("warning");
    expect(contextUsageLevel(90)).toBe("critical");
  });
});

describe("token formatting", () => {
  it("quotes counts at the scale people use", () => {
    expect(formatTokenCount(840)).toBe("840");
    expect(formatTokenCount(200_000)).toBe("200k");
    expect(formatTokenCount(1_000_000)).toBe("1M");
    expect(formatTokenCount(1_500_000)).toBe("1.5M");
  });
});

describe("truncation notice", () => {
  it("carries the before and after sizes", () => {
    expect(contextTruncationNotice(128_000, 96_000)).toContain(
      "~128k → ~96k tokens",
    );
  });

  it("falls back to the plain sentence when the numbers say nothing", () => {
    // An older server sends zeros, and a "fitted" size at or above the
    // original would be nonsense to print.
    expect(contextTruncationNotice(0, 0)).not.toContain("~");
    expect(contextTruncationNotice(1_000, 1_000)).not.toContain("~");
  });
});
