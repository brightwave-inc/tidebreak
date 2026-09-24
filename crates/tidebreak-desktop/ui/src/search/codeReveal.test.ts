import { describe, expect, it } from "vitest";

import type {
  CodeEvent,
  CodeTurnSnapshot,
  SequencedCodeEventFrame,
} from "../api/types";
import {
  initialCodeSessionState,
  itemForEvent,
  reduceCodeSessionEvent,
} from "../code/CodeSessionReducer";
import { historyFromJournal } from "./useCodeTranscriptSearch";

const NOW = "2026-09-24T12:00:00Z";

function turn(id: string, ordinal: number, input: string): CodeTurnSnapshot {
  return {
    id,
    session_id: "sess-1",
    ordinal,
    status: "completed",
    fast_mode: false,
    user_input: input,
    attachments: [],
    started_at: NOW,
    ended_at: NOW,
  } as CodeTurnSnapshot;
}

function frames(
  events: readonly [number, CodeEvent][],
  truncated = false,
): SequencedCodeEventFrame[] {
  return events.map(([seq, event], index) => ({
    seq,
    event,
    replayed: true,
    ...(truncated && index === 0 ? { truncated: true } : {}),
  }));
}

function play(events: readonly [number, CodeEvent][]) {
  let state = initialCodeSessionState();
  let id = 0;
  const deps = { nextId: () => `row-${++id}`, now: () => NOW };
  for (const frame of frames(events)) {
    state = reduceCodeSessionEvent(state, frame, deps).state;
  }
  return state.items;
}

describe("finding the row a code search hit names", () => {
  const events: [number, CodeEvent][] = [
    [10, { type: "turn_started", turn_id: "t1" }],
    [
      11,
      {
        type: "tool_started",
        call_id: "c1",
        name: "Bash",
        detail: { kind: "command", cmd: "cargo test", cwd: "/repo" },
      },
    ],
    [
      12,
      {
        type: "tool_completed",
        call_id: "c1",
        outcome: "succeeded",
        preview: "ok",
      },
    ],
    [13, { type: "assistant_message", text: "The tests pass." }],
    [14, { type: "user_steered", text: "also run clippy" }],
  ];

  it("finds a tool call by the event that started it or the one that ended it", () => {
    const items = play(events);
    const started = itemForEvent(items, { eventSeq: 11 });
    expect(started).toMatchObject({ kind: "tool", callId: "c1" });
    expect(itemForEvent(items, { eventSeq: 12 })).toBe(started);
  });

  it("finds an assistant message and a steer by their events", () => {
    const items = play(events);
    expect(itemForEvent(items, { eventSeq: 13 })).toMatchObject({
      kind: "assistant",
      text: "The tests pass.",
    });
    expect(itemForEvent(items, { eventSeq: 14 })).toMatchObject({
      kind: "steer",
      text: "also run clippy",
    });
  });

  it("finds nothing for an event the transcript never replayed", () => {
    expect(itemForEvent(play(events), { eventSeq: 3 })).toBeNull();
  });
});

describe("a window of an old session's journal", () => {
  it("draws the turns it saw start, and the event a search named", () => {
    // Events 500 through 503 of a session far longer than the replay: the
    // window shows turn 7 and its work, not every turn the session had.
    const view = historyFromJournal(
      frames(
        [
          [500, { type: "turn_started", turn_id: "t7" }],
          [
            501,
            {
              type: "tool_started",
              call_id: "c9",
              name: "Read",
              detail: { kind: "file_read", path: "src/harbour.rs" },
            },
          ],
          [
            502,
            { type: "assistant_message", text: "Harbour fees round down." },
          ],
          [
            503,
            {
              type: "turn_completed",
              usage: {
                input_tokens: 10,
                output_tokens: 4,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                context_tokens: 0,
              },
            },
          ],
        ],
        true,
      ),
      [turn("t1", 1, "start"), turn("t7", 7, "why do fees round?")],
    );
    const prompts = view.items.filter((item) => item.kind === "user");
    expect(prompts).toEqual([
      expect.objectContaining({ turnId: "t7", text: "why do fees round?" }),
    ]);
    expect(itemForEvent(view.items, { eventSeq: 502 })).toMatchObject({
      kind: "assistant",
      text: "Harbour fees round down.",
    });
    expect(itemForEvent(view.items, { eventSeq: 501 })).toMatchObject({
      kind: "tool",
      callId: "c9",
    });
    // The window says it is not the whole session.
    expect(
      view.items.some(
        (item) =>
          item.kind === "notice" && /Earlier history/.test(item.message),
      ),
    ).toBe(true);
  });
});
