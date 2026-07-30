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
          call_id: "call-1",
          tool: "web_search",
          result_unreadable: false,
          status: "completed",
          started_at: "2026-07-16T10:00:01Z",
          finished_at: "2026-07-16T10:00:01Z",
        },
      ],
    );

    expect(entries).toEqual([
      expect.objectContaining({ id: "user-1", kind: "message" }),
      expect.objectContaining({
        id: "call-1",
        callId: "call-1",
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
        call_id: "call-2",
        tool: "spawn_sandbox_agent",
        result_unreadable: false,
        status: "completed",
        started_at: "2026-07-16T10:00:00Z",
        finished_at: "2026-07-16T10:00:00Z",
      },
      {
        call_id: "call-3",
        tool: "wait_for_agents",
        result_unreadable: false,
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

  it("hydrates answered questions with a fixed presentation kind", () => {
    const entries = hydrateTranscriptHistory([], [
      {
        call_id: "call-4",
        tool: "ask_user_questions",
        result_unreadable: false,
        status: "completed",
        started_at: "2026-07-16T10:00:00Z",
        finished_at: "2026-07-16T10:00:01Z",
      },
    ]);

    expect(entries).toEqual([
      expect.objectContaining({ name: "ask_user_questions" }),
    ]);
  });

  it("hydrates delegated file reads as their fixed presentation kind", () => {
    const entries = hydrateTranscriptHistory([], [
      {
        call_id: "call-5",
        tool: "read_delegated_file",
        result_unreadable: false,
        status: "completed",
        started_at: "2026-07-16T10:00:00Z",
        finished_at: "2026-07-16T10:00:01Z",
      },
    ]);

    expect(entries).toEqual([
      expect.objectContaining({ name: "read_delegated_file" }),
    ]);
    expect(JSON.stringify(entries)).not.toContain("finished_at");
  });

  it("hydrates source discovery, direct reads, and semantic search distinctly", () => {
    const entries = hydrateTranscriptHistory([], [
      {
        call_id: "call-6",
        tool: "list_sources",
        result_unreadable: false,
        status: "completed",
        started_at: "2026-07-16T10:00:00Z",
        finished_at: "2026-07-16T10:00:00Z",
      },
      {
        call_id: "call-7",
        tool: "read_source",
        result_unreadable: false,
        status: "completed",
        started_at: "2026-07-16T10:00:01Z",
        finished_at: "2026-07-16T10:00:01Z",
      },
      {
        call_id: "call-8",
        tool: "search",
        result_unreadable: false,
        status: "completed",
        started_at: "2026-07-16T10:00:02Z",
        finished_at: "2026-07-16T10:00:02Z",
      },
    ]);

    expect(entries).toEqual([
      expect.objectContaining({ name: "list_sources" }),
      expect.objectContaining({ name: "read_source" }),
      expect.objectContaining({ name: "search" }),
    ]);
  });

  // The payload here matches the wire exactly: the server groups citations by
  // message and nests them, so `message_id` is skipped during serialization.
  // The previous fixture invented that field, which made this suite pass while
  // production dropped every historical citation.
  it("keeps the sources the server nested under each assistant message", () => {
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
              ordinal: 1,
              document_id: "document-1",
              locator: { kind: "page", page: 2 },
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
      sources: [
        {
          id: "citation-1",
          documentId: "document-1",
          locator: { kind: "page", page: 2 },
        },
      ],
    });
    expect(entries[1]).toMatchObject({ id: "assistant-2", sources: [] });
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

  it("folds an unrecognized historical tool to the generic card", () => {
    const [entry] = hydrateTranscriptHistory([], [
      {
        call_id: "call-10",
        tool: "other",
        result_unreadable: false,
        status: "failed",
        started_at: "2026-07-16T10:00:00Z",
        finished_at: null,
      },
    ]);

    // `other` is the server's own fold and a real member of the renderer's
    // tool vocabulary, so it resolves to the generic presentation. The previous
    // sentinel name existed in neither the copy nor the icon table.
    expect(entry).toMatchObject({
      kind: "tool",
      name: "other",
      status: "failed",
    });
  });
});
