import { describe, expect, it } from "vitest";
import {
  hydrateTurnHistory,
  initialTranscript,
  reduceTranscript,
} from "./transcript";

describe("reduceTranscript", () => {
  it("ignores duplicate durable seq", () => {
    let state = initialTranscript();
    state = reduceTranscript(state, {
      seq: 1,
      event: { type: "turn_started", turn_id: "t1" },
    });
    const again = reduceTranscript(state, {
      seq: 1,
      event: { type: "turn_started", turn_id: "t1" },
    });
    expect(again).toBe(state);
    expect(again.lastSeq).toBe(1);
  });

  it("does not advance lastSeq on transient assistant deltas", () => {
    let state = reduceTranscript(initialTranscript(), {
      seq: 4,
      event: { type: "turn_started", turn_id: "t1" },
    });
    state = reduceTranscript(state, {
      seq: 4,
      transient: true,
      event: { type: "assistant_delta", text: "Hello" },
    });
    expect(state.lastSeq).toBe(4);
    const assistant = state.items.find((item) => item.kind === "assistant");
    expect(assistant?.kind === "assistant" && assistant.text).toBe("Hello");
  });

  it("collapses tool start/complete into one card", () => {
    let state = reduceTranscript(initialTranscript(), {
      seq: 1,
      event: {
        type: "tool_started",
        call_id: "c1",
        name: "Bash",
        detail: { kind: "command", cmd: "ls", cwd: "." },
      },
    });
    state = reduceTranscript(state, {
      seq: 2,
      event: {
        type: "tool_completed",
        call_id: "c1",
        outcome: "succeeded",
        preview: "ok",
      },
    });
    const tools = state.items.filter((item) => item.kind === "tool");
    expect(tools).toHaveLength(1);
    expect(tools[0]).toMatchObject({
      name: "Bash",
      summary: "ok",
    });
  });

  it("tracks the active turn through terminal transitions", () => {
    let state = reduceTranscript(initialTranscript(), {
      seq: 1,
      event: { type: "turn_started", turn_id: "t1" },
    });
    expect(state.activeTurnId).toBe("t1");
    state = reduceTranscript(state, {
      seq: 2,
      event: { type: "turn_interrupted" },
    });
    expect(state.activeTurnId).toBeNull();
  });

  it("marks durable approval changes for an authoritative list refresh", () => {
    let state = reduceTranscript(initialTranscript(), {
      seq: 7,
      event: {
        type: "approval_requested",
        approval_id: "approval-1",
      },
    });
    expect(state.approvalRevision).toBe(7);

    state = reduceTranscript(state, {
      seq: 8,
      event: {
        type: "approval_resolved",
        approval_id: "approval-1",
        decision: { type: "deny", feedback: "Use the focused test." },
      },
    });
    expect(state.approvalRevision).toBe(8);
  });

  it("hydrates user turns once, in ordinal order, and recovers a running id", () => {
    const turns = [
      {
        id: "t2",
        session_id: "s1",
        ordinal: 2,
        status: "running" as const,
        fast_mode: false,
        user_input: "second",
        attachments: [],
        started_at: "2026-08-27T00:00:02Z",
      },
      {
        id: "t1",
        session_id: "s1",
        ordinal: 1,
        status: "completed" as const,
        fast_mode: false,
        user_input: "first",
        attachments: [],
        started_at: "2026-08-27T00:00:01Z",
      },
    ];
    let state = hydrateTurnHistory(initialTranscript(), turns);
    expect(state.activeTurnId).toBe("t2");
    expect(
      state.items.filter((item) => item.kind === "user").map((item) => item.text),
    ).toEqual(["first", "second"]);
    state = hydrateTurnHistory(state, turns);
    expect(state.items.filter((item) => item.kind === "user")).toHaveLength(2);
  });

  it("anchors a newly hydrated promoted turn before its start marker", () => {
    let state = reduceTranscript(initialTranscript(), {
      seq: 1,
      event: { type: "turn_started", turn_id: "t2" },
    });
    state = hydrateTurnHistory(state, [
      {
        id: "t2",
        session_id: "s1",
        ordinal: 2,
        status: "running",
        fast_mode: false,
        user_input: "queued follow-up",
        attachments: [],
        started_at: "2026-08-27T00:00:02Z",
      },
    ]);

    expect(state.items.map((item) => item.id)).toEqual(["user:t2", "turn:t2"]);
  });

  it("does not resurrect a stale running row after a terminal event replayed", () => {
    const staleRunning = {
      id: "t1",
      session_id: "s1",
      ordinal: 1,
      status: "running" as const,
      fast_mode: false,
      user_input: "finish this",
      attachments: [],
      started_at: "2026-08-27T00:00:00Z",
    };
    let state = reduceTranscript(initialTranscript(), {
      seq: 1,
      event: { type: "turn_started", turn_id: "t1" },
    });
    state = reduceTranscript(state, {
      seq: 2,
      event: {
        type: "turn_completed",
        usage: {
          input_tokens: 0,
          output_tokens: 0,
          cache_read_input_tokens: 0,
          cache_creation_input_tokens: 0,
          context_tokens: 0,
        },
      },
    });

    state = hydrateTurnHistory(state, [staleRunning]);
    expect(state.activeTurnId).toBeNull();
  });
});
