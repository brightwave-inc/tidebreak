import { describe, expect, it } from "vitest";

import { transcriptNavigationEntries } from "./TranscriptNavigation";

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
