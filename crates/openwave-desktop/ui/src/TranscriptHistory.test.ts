import { describe, expect, it } from "vitest";
import { hydrateTranscriptHistory } from "./TranscriptHistory";

describe("hydrateTranscriptHistory", () => {
  it("interleaves terminal safe tool cards with durable messages", () => {
    const entries = hydrateTranscriptHistory(
      [
        {
          id: "user-1",
          role: "user",
          content: "find this",
          created_at: "2026-07-16T10:00:00Z",
        },
        {
          id: "assistant-1",
          role: "assistant",
          content: "done",
          created_at: "2026-07-16T10:00:02Z",
        },
      ],
      [
        {
          title: "Search the web",
          status: "completed",
          started_at: "2026-07-16T10:00:01Z",
          finished_at: "2026-07-16T10:00:01Z",
        },
      ],
    );

    expect(entries).toEqual([
      expect.objectContaining({ id: "user-1", kind: "message" }),
      expect.objectContaining({
        id: "tool-history:2026-07-16T10:00:01Z:0",
        kind: "tool",
        name: "web_search",
        status: "completed",
      }),
      expect.objectContaining({ id: "assistant-1", kind: "message" }),
    ]);
  });

  it("keeps the server's generic historical title on the generic card path", () => {
    const [entry] = hydrateTranscriptHistory([], [
      {
        title: "Use a tool",
        status: "failed",
        started_at: "2026-07-16T10:00:00Z",
        finished_at: null,
      },
    ]);

    expect(entry).toMatchObject({
      kind: "tool",
      name: "historical_unknown_tool",
      status: "failed",
    });
  });
});
