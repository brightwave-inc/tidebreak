import { describe, expect, it } from "vitest";

import type { CodeTranscriptItem } from "../code/CodeSessionReducer";
import type { ChatMessage } from "../MessageList";
import {
  holdsEveryTerm,
  loadedChatMatches,
  loadedCodeMatches,
} from "./loadedFind";
import { queryTerms } from "./messageSearch";

describe("finding in what a transcript has loaded", () => {
  it("matches every word at the start of a word, folded", () => {
    expect(
      holdsEveryTerm("The Café opens at nine", queryTerms("cafe op")),
    ).toBe(true);
    expect(holdsEveryTerm("SubmitButton renders", queryTerms("button"))).toBe(
      true,
    );
    // Inside a word is not a match, and every word must be there.
    expect(holdsEveryTerm("The harbour", queryTerms("arbour"))).toBe(false);
    expect(holdsEveryTerm("The harbour", queryTerms("harbour dues"))).toBe(
      false,
    );
    expect(holdsEveryTerm("The harbour", [])).toBe(false);
  });

  it("reads a chat's questions and answers, newest first", () => {
    const messages: ChatMessage[] = [
      {
        id: "q1",
        role: "user",
        turnId: "t1",
        text: "What are the harbour dues?",
      },
      {
        id: "a1",
        role: "assistant",
        turnId: "t1",
        text: "Harbour dues stay the same.",
        sources: [],
      },
      { id: "n1", role: "system", text: "Harbour notice" },
      { id: "q2", role: "user", turnId: "t2", text: "And the berths?" },
    ];
    expect(
      loadedChatMatches("chat-1", messages, queryTerms("harbour")).map(
        (hit) => [hit.message_id, hit.source, hit.turn_id],
      ),
    ).toEqual([
      ["a1", "assistant", "t1"],
      ["q1", "user", "t1"],
    ]);
  });

  it("reads a code session's rows by the events that open them", () => {
    const rows: CodeTranscriptItem[] = [
      {
        kind: "user",
        id: "u1",
        turnId: "t1",
        text: "Fix the harbour fee rounding",
        createdAt: "2026-09-24T12:00:00Z",
      },
      {
        kind: "tool",
        id: "tool-1",
        turnId: "t1",
        callId: "c1",
        parentCallId: null,
        name: "Read",
        detail: { kind: "file_read", path: "src/harbour.rs" },
        status: "succeeded",
        preview: "",
        startedAt: null,
        durationMs: null,
        seqs: [11, 12],
      },
      {
        kind: "assistant",
        id: "a1",
        turnId: "t1",
        parentCallId: null,
        text: "Harbour fees now round down.",
        streaming: false,
        seqs: [13],
      },
      {
        // Streamed, and no event names it yet.
        kind: "assistant",
        id: "a2",
        turnId: "t1",
        parentCallId: null,
        text: "Harbour tests pass",
        streaming: true,
      },
    ];
    expect(
      loadedCodeMatches("sess-1", rows, queryTerms("harbour")).map((hit) => [
        hit.source,
        hit.event_seq,
        hit.turn_id,
      ]),
    ).toEqual([
      ["assistant", 13, "t1"],
      ["tool", 11, "t1"],
      ["user", undefined, "t1"],
    ]);
  });
});
