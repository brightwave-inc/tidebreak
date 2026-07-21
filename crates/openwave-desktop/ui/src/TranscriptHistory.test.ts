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
          citations: [],
        },
        {
          id: "assistant-1",
          role: "assistant",
          content: "done",
          created_at: "2026-07-16T10:00:02Z",
          citations: [],
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

  it("hydrates background delegation and wait activity with fixed tool kinds", () => {
    const entries = hydrateTranscriptHistory([], [
      {
        title: "Delegate a task",
        status: "completed",
        started_at: "2026-07-16T10:00:00Z",
        finished_at: "2026-07-16T10:00:00Z",
      },
      {
        title: "Wait for background agents",
        status: "completed",
        started_at: "2026-07-16T10:00:01Z",
        finished_at: "2026-07-16T10:00:02Z",
      },
    ]);

    expect(entries).toEqual([
      expect.objectContaining({ name: "spawn_sandbox_agent" }),
      expect.objectContaining({ name: "wait_for_agents" }),
    ]);
    expect(JSON.stringify(entries)).not.toContain("finished_at");
  });

  it("attaches sources only to their exact owning assistant message", () => {
    const entries = hydrateTranscriptHistory(
      [
        {
          id: "assistant-1",
          role: "assistant",
          content: "first",
          created_at: "2026-07-16T10:00:00Z",
          citations: [
            {
              id: "citation-1",
              message_id: "assistant-1",
              ordinal: 1,
              excerpt: "safe excerpt",
              heading: "Safe heading",
              pages: [2],
            },
            {
              id: "citation-crossed",
              message_id: "assistant-2",
              ordinal: 2,
              excerpt: "must not cross messages",
              heading: null,
              pages: [],
            },
          ],
        },
        {
          id: "assistant-2",
          role: "assistant",
          content: "second",
          created_at: "2026-07-16T10:00:01Z",
          citations: [],
        },
      ],
      [],
    );

    expect(entries[0]).toMatchObject({
      id: "assistant-1",
      sources: [{ id: "citation-1", excerpt: "safe excerpt" }],
    });
    expect(entries[1]).toMatchObject({ id: "assistant-2", sources: [] });
    expect(JSON.stringify(entries)).not.toContain("must not cross messages");
  });

  it("treats citations omitted from a partial response as an empty source list", () => {
    const [entry] = hydrateTranscriptHistory(
      [
        {
          id: "assistant-legacy",
          role: "assistant",
          content: "still readable",
          created_at: "2026-07-16T10:00:00Z",
        },
      ],
      [],
    );

    expect(entry).toMatchObject({
      id: "assistant-legacy",
      text: "still readable",
      sources: [],
    });
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
