import { describe, expect, it } from "vitest";

import {
  layoutRailIndicatorTops,
  transcriptNavigationEntries,
} from "./TranscriptNavigation";

describe("transcriptNavigationEntries", () => {
  it("builds concise user and tool destinations from the rendered transcript", () => {
    const entries = transcriptNavigationEntries([
      {
        id: "user-1",
        role: "user",
        text: "  Compare the quarterly totals\nby region  ",
      },
      {
        id: "tool-1",
        role: "tool",
        callId: "call-1",
        name: "web_search",
        status: "running",
      },
      {
        id: "assistant-1",
        role: "assistant",
        text: "Working on it",
        sources: [],
      },
    ]);

    expect(entries).toEqual([
      {
        anchorId: "user-1",
        kind: "user",
        label: "Compare the quarterly totals by region",
        active: false,
      },
      {
        anchorId: "tool-1",
        kind: "tool",
        label: "Searching the web",
        toolName: "web_search",
        active: true,
      },
    ]);
  });
});

describe("layoutRailIndicatorTops", () => {
  it("stacks indicators that clamp to the same rail edge", () => {
    expect(
      [...layoutRailIndicatorTops(
        [
          { anchorId: "a", desiredTop: 0, distanceFromViewport: 80 },
          { anchorId: "b", desiredTop: 0, distanceFromViewport: 40 },
          { anchorId: "c", desiredTop: 0, distanceFromViewport: 10 },
        ],
        120,
      ).entries()],
    ).toEqual([
      ["c", 0],
      ["b", 24],
      ["a", 48],
    ]);
  });

  it("pulls a bottom cluster upward without overlap", () => {
    expect(
      [...layoutRailIndicatorTops(
        [
          { anchorId: "a", desiredTop: 96, distanceFromViewport: 0 },
          { anchorId: "b", desiredTop: 96, distanceFromViewport: 0 },
          { anchorId: "c", desiredTop: 96, distanceFromViewport: 0 },
        ],
        120,
      ).values()],
    ).toEqual([48, 72, 96]);
  });

  it("hides the farthest indicators when the rail cannot fit them", () => {
    expect(
      [...layoutRailIndicatorTops(
        [
          { anchorId: "onscreen", desiredTop: 20, distanceFromViewport: 0 },
          { anchorId: "near", desiredTop: 0, distanceFromViewport: 10 },
          { anchorId: "far", desiredTop: 0, distanceFromViewport: 300 },
        ],
        48,
      ).keys()],
    ).toEqual(["near", "onscreen"]);
  });
});
