import { describe, expect, it } from "vitest";
import type { CodeEvent, SequencedCodeEventFrame } from "../api/types";
import {
  applyAcceptedTurn,
  hydrateCodeTurns,
  initialCodeSessionState,
  markCodeSessionHydrated,
  reduceCodeSessionEvent,
  userItemId,
  type CodeSessionDeps,
  type CodeSessionState,
} from "./CodeSessionReducer";

const NOW = "2026-08-15T12:00:00.000Z";
const LATER = "2026-08-15T12:00:02.500Z";

const NO_USAGE = {
  input_tokens: 10,
  output_tokens: 4,
  cache_read_input_tokens: 0,
  cache_creation_input_tokens: 0,
};

function deps(): CodeSessionDeps {
  let seq = 0;
  return {
    nextId: () => `c${++seq}`,
    now: () => NOW,
  };
}

function framed(
  seq: number,
  event: CodeEvent,
  replayed = false,
): SequencedCodeEventFrame {
  return replayed ? { seq, event, replayed: true } : { seq, event };
}

function play(
  events: CodeEvent[],
  state: CodeSessionState = initialCodeSessionState(),
  clock: CodeSessionDeps = deps(),
) {
  let current = { state, effects: [] as ReturnType<typeof reduceCodeSessionEvent>["effects"] };
  events.forEach((event, index) => {
    current = reduceCodeSessionEvent(
      current.state,
      framed(state.lastSeq + index + 1, event),
      clock,
    );
  });
  return current;
}

describe("seq cursor", () => {
  it("ignores duplicate and stale events entirely", () => {
    const clock = deps();
    const { state } = play(
      [
        { type: "turn_started", turn_id: "t1" },
        { type: "assistant_delta", text: "Hi" },
      ],
      initialCodeSessionState(),
      clock,
    );
    const replay = reduceCodeSessionEvent(
      state,
      framed(state.lastSeq, { type: "assistant_delta", text: "AGAIN" }),
      clock,
    );
    expect(replay.state).toBe(state);
    expect(replay.effects).toEqual([]);
  });

  it("advances the cursor for unknown event kinds", () => {
    const { state, effects } = reduceCodeSessionEvent(
      initialCodeSessionState(),
      framed(4, { type: "future_kind" } as unknown as CodeEvent),
      deps(),
    );
    expect(state.lastSeq).toBe(4);
    expect(state.items).toEqual([]);
    expect(effects).toEqual([]);
  });

  it("suppresses animation during replay", () => {
    const { state } = reduceCodeSessionEvent(
      initialCodeSessionState(),
      framed(1, { type: "turn_started", turn_id: "t1" }, true),
      deps(),
    );
    expect(state.animateStreaming).toBe(false);
  });
});

describe("approvals", () => {
  it("parks a card on request and marks it denied on resolve", () => {
    const { state } = play([
      { type: "turn_started", turn_id: "t1" },
      { type: "approval_requested", approval_id: "appr-1" },
      {
        type: "approval_resolved",
        approval_id: "appr-1",
        decision: { type: "deny", feedback: "use fixtures" },
      },
    ]);
    const card = state.items.find((item) => item.kind === "approval");
    expect(card).toMatchObject({
      kind: "approval",
      approvalId: "appr-1",
      state: "denied",
    });
  });
});

describe("turn lifecycle", () => {
  it("records deltas, tools, notices, and a completed boundary with usage", () => {
    const clock: CodeSessionDeps = {
      nextId: deps().nextId,
      now: (() => {
        let tick = 0;
        return () => {
          tick += 1;
          return tick === 1 ? NOW : LATER;
        };
      })(),
    };
    const { state, effects } = play(
      [
        { type: "session_started", harness_kind: "claude_code", harness_version: "1.0" },
        { type: "turn_started", turn_id: "t1" },
        { type: "assistant_delta", text: "Hello" },
        { type: "assistant_delta", text: " world" },
        {
          type: "tool_started",
          call_id: "c1",
          name: "Bash",
          detail: { kind: "command", cmd: "ls", cwd: "/tmp" },
        },
        {
          type: "tool_completed",
          call_id: "c1",
          outcome: "succeeded",
          preview: "ok",
        },
        { type: "harness_notice", level: "warning", message: "degraded" },
        { type: "turn_completed", usage: NO_USAGE },
      ],
      initialCodeSessionState(),
      clock,
    );
    expect(state.busy).toBe(false);
    expect(state.activeTurnId).toBeNull();
    expect(state.lifecycle).toBe("idle");
    expect(state.harnessKind).toBe("claude_code");
    expect(state.lastUsage).toEqual(NO_USAGE);
    expect(effects.at(-1)).toEqual({ type: "turn_resolved" });
    const kinds = state.items.map((item) => item.kind);
    expect(kinds).toEqual(["assistant", "tool", "notice", "turn_boundary"]);
    const assistant = state.items[0];
    expect(assistant).toMatchObject({
      kind: "assistant",
      text: "Hello world",
      streaming: false,
    });
    const tool = state.items[1];
    expect(tool).toMatchObject({
      kind: "tool",
      callId: "c1",
      status: "succeeded",
      preview: "ok",
      startedAt: LATER,
      durationMs: 0,
    });
    const boundary = state.items[3];
    expect(boundary).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      status: "completed",
      durationMs: 2500,
      usage: NO_USAGE,
    });
  });

  it("closes a failed turn with the bounded error", () => {
    const { state } = play([
      { type: "turn_started", turn_id: "t1" },
      { type: "turn_failed", error: { message: "engine exited" } },
    ]);
    expect(state.busy).toBe(false);
    expect(state.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      status: "failed",
      error: "engine exited",
    });
  });
});

describe("late tool arguments", () => {
  function toolItem(state: CodeSessionState) {
    return state.items.find((item) => item.kind === "tool");
  }

  it("names the subject an empty started detail could not", () => {
    // Engines open a call before its arguments finish streaming, so the
    // started detail can be empty and the line falls back to the tool name.
    const { state } = play([
      { type: "turn_started", turn_id: "t1" },
      {
        type: "tool_started",
        call_id: "c1",
        name: "Bash",
        detail: { kind: "command", cmd: "", cwd: "" },
      },
      {
        type: "tool_completed",
        call_id: "c1",
        outcome: "succeeded",
        preview: "ok",
        detail: {
          kind: "command",
          cmd: "cargo test -p tidebreak-server",
          cwd: "/workspace",
        },
      },
    ]);
    expect(toolItem(state)).toMatchObject({
      detail: {
        kind: "command",
        cmd: "cargo test -p tidebreak-server",
        cwd: "/workspace",
      },
    });
  });

  it("keeps a populated detail rather than taking a weaker correction", () => {
    const { state } = play([
      { type: "turn_started", turn_id: "t1" },
      {
        type: "tool_started",
        call_id: "c1",
        name: "Bash",
        detail: { kind: "command", cmd: "cargo test", cwd: "/workspace" },
      },
      {
        type: "tool_completed",
        call_id: "c1",
        outcome: "succeeded",
        preview: "ok",
        detail: { kind: "other", summary: "Bash" },
      },
    ]);
    expect(toolItem(state)).toMatchObject({
      detail: { kind: "command", cmd: "cargo test", cwd: "/workspace" },
    });
  });

  it("leaves the started detail alone when no correction rides the completion", () => {
    const { state } = play([
      { type: "turn_started", turn_id: "t1" },
      {
        type: "tool_started",
        call_id: "c1",
        name: "read_file",
        detail: { kind: "file_read", path: "README.md" },
      },
      {
        type: "tool_completed",
        call_id: "c1",
        outcome: "succeeded",
        preview: "demo",
      },
    ]);
    expect(toolItem(state)).toMatchObject({
      detail: { kind: "file_read", path: "README.md" },
    });
  });
});

const SNAPSHOT_TURN = {
  id: "t1",
  session_id: "sess-1",
  ordinal: 1,
  status: "completed" as const,
  user_input: "list the files",
  attachments: [],
  started_at: NOW,
  ended_at: LATER,
};

describe("hydrate then replay", () => {
  it("shows the user prompt after a disposed store reopens from after=0", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [SNAPSHOT_TURN]);
    expect(hydrated.items[0]).toEqual({
      kind: "user",
      id: userItemId("t1"),
      turnId: "t1",
      text: "list the files",
      createdAt: NOW,
      attachments: [],
    });
    expect(hydrated.items[1]).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      status: "completed",
      durationMs: 2500,
    });
    expect(hydrated.lastUsage).toBeNull();

    const replayed = play(
      [
        { type: "turn_started", turn_id: "t1" },
        { type: "assistant_delta", text: "README.md" },
        { type: "turn_completed", usage: NO_USAGE },
      ],
      hydrated,
    );
    expect(replayed.state.items.filter((item) => item.kind === "user")).toHaveLength(
      1,
    );
    expect(replayed.state.items[0]).toMatchObject({
      kind: "user",
      turnId: "t1",
      text: "list the files",
      createdAt: NOW,
    });
    expect(
      replayed.state.items.filter((item) => item.kind === "turn_boundary"),
    ).toHaveLength(1);
    expect(replayed.state.items.map((item) => item.kind)).toEqual([
      "user",
      "assistant",
      "turn_boundary",
    ]);
    expect(replayed.state.items.find((item) => item.kind === "assistant")).toMatchObject({
      text: "README.md",
    });
    expect(replayed.state.lifecycle).toBe("idle");
  });

  it("does not reprint an assistant snapshot after tools", () => {
    const { state } = play([
      { type: "turn_started", turn_id: "t1" },
      { type: "assistant_delta", text: "On it — checking." },
      {
        type: "tool_started",
        call_id: "c1",
        name: "Grep",
        detail: { kind: "search", query: "image" },
      },
      {
        type: "assistant_message",
        text: "On it — checking.",
      },
      { type: "turn_completed", usage: NO_USAGE },
    ]);
    expect(state.items.filter((item) => item.kind === "assistant")).toHaveLength(1);
    expect(state.items.map((item) => item.kind)).toEqual([
      "assistant",
      "tool",
      "turn_boundary",
    ]);
  });

  it("keeps a later turn's seam from capturing an earlier turn's replay", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      SNAPSHOT_TURN,
      { ...SNAPSHOT_TURN, id: "t2", user_input: "?" },
    ]);
    const replayed = play(
      [
        { type: "turn_started", turn_id: "t1" },
        { type: "assistant_delta", text: "first" },
        { type: "turn_completed", usage: NO_USAGE },
        { type: "turn_started", turn_id: "t2" },
        { type: "assistant_delta", text: "second" },
        { type: "turn_completed", usage: NO_USAGE },
      ],
      hydrated,
    );
    expect(replayed.state.items.map((item) => item.kind)).toEqual([
      "user",
      "assistant",
      "turn_boundary",
      "user",
      "assistant",
      "turn_boundary",
    ]);
    expect(replayed.state.items[1]).toMatchObject({
      kind: "assistant",
      text: "first",
    });
    expect(replayed.state.items[4]).toMatchObject({
      kind: "assistant",
      text: "second",
    });
  });

  it("shows prompts from a snapshot that includes usage", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      { ...SNAPSHOT_TURN, usage: NO_USAGE },
    ]);
    expect(hydrated.items[0]).toMatchObject({
      kind: "user",
      turnId: "t1",
      text: "list the files",
    });
    expect(hydrated.items[1]).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      usage: NO_USAGE,
    });
    expect(hydrated.lastUsage).toEqual(NO_USAGE);
  });

  it("places an accepted user item above that turn's already-streamed reply", () => {
    const streamed = play([
      { type: "turn_started", turn_id: "t1" },
      { type: "assistant_delta", text: "README.md" },
      { type: "turn_completed", usage: NO_USAGE },
    ]);
    const accepted = applyAcceptedTurn(streamed.state, {
      ...SNAPSHOT_TURN,
      status: "completed",
      usage: NO_USAGE,
    });
    expect(accepted.items.map((item) => item.kind)).toEqual([
      "user",
      "assistant",
      "turn_boundary",
    ]);
    expect(accepted.items[0]).toMatchObject({
      kind: "user",
      turnId: "t1",
      text: "list the files",
      createdAt: NOW,
    });
  });

  it("converges a live accept with hydrate on the same turn id", () => {
    const accepted = applyAcceptedTurn(initialCodeSessionState(), {
      ...SNAPSHOT_TURN,
      status: "running",
      ended_at: undefined,
    });
    const again = hydrateCodeTurns(accepted, [
      { ...SNAPSHOT_TURN, status: "running", ended_at: undefined },
    ]);
    expect(again.items.filter((item) => item.kind === "user")).toHaveLength(1);
    expect(again.items[0]?.id).toBe(userItemId("t1"));
    expect(again.items[0]).toMatchObject({ createdAt: NOW });
  });
});

describe("hydration flag", () => {
  it("starts unset and flips only when the snapshot settles", () => {
    const initial = initialCodeSessionState();
    expect(initial.hydrated).toBe(false);

    // Applying turns is not settlement: a snapshot that never arrives still
    // has to flip, so the flag is not a side effect of hydrateCodeTurns.
    const withTurns = hydrateCodeTurns(initial, [SNAPSHOT_TURN]);
    expect(withTurns.hydrated).toBe(false);

    const settled = markCodeSessionHydrated(withTurns);
    expect(settled.hydrated).toBe(true);
    expect(settled.items).toBe(withTurns.items);
    expect(markCodeSessionHydrated(settled)).toBe(settled);
  });
});

describe("user_steered", () => {
  it("appends a steer item on the active turn", () => {
    const { state } = play([
      { type: "turn_started", turn_id: "t1" },
      { type: "user_steered", text: "use fixtures" },
    ]);
    expect(state.items.at(-1)).toMatchObject({
      kind: "steer",
      turnId: "t1",
      text: "use fixtures",
    });
  });
});

describe("file_changed", () => {
  it("aggregates per-turn file activity and bumps contentRevision", () => {
    const { state } = play([
      { type: "turn_started", turn_id: "t1" },
      {
        type: "file_changed",
        path: "a.ts",
        kind: "modified",
        diffstat: { files: 1, insertions: 10, deletions: 2, truncated: false },
      },
      {
        type: "file_changed",
        path: "b.ts",
        kind: "added",
        diffstat: { files: 1, insertions: 32, deletions: 5, truncated: false },
      },
      {
        type: "file_changed",
        path: "a.ts",
        kind: "modified",
        diffstat: { files: 1, insertions: 12, deletions: 3, truncated: false },
      },
    ]);
    expect(state.items.filter((item) => item.kind === "file_activity")).toHaveLength(
      1,
    );
    expect(state.items.find((item) => item.kind === "file_activity")).toMatchObject({
      kind: "file_activity",
      turnId: "t1",
      files: {
        "a.ts": {
          kind: "modified",
          diffstat: { files: 1, insertions: 12, deletions: 3, truncated: false },
        },
        "b.ts": {
          kind: "added",
          diffstat: { files: 1, insertions: 32, deletions: 5, truncated: false },
        },
      },
    });
    expect(state.contentRevision).toBe(3);
  });
});

describe("attention_changed", () => {
  it("reduces into state.attention and does not add a transcript item", () => {
    const { state } = play([
      {
        type: "attention_changed",
        state: { type: "stalled", idle_secs: 90 },
        source: "heuristic",
      },
    ]);
    expect(state.attention).toEqual({
      state: { type: "stalled", idle_secs: 90 },
      source: "heuristic",
    });
    expect(state.items).toEqual([]);
  });
});

describe("user item createdAt", () => {
  it("carries the turn's started_at on accept and hydrate", () => {
    const accepted = applyAcceptedTurn(initialCodeSessionState(), SNAPSHOT_TURN);
    expect(accepted.items[0]).toMatchObject({
      kind: "user",
      createdAt: NOW,
    });

    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [SNAPSHOT_TURN]);
    expect(hydrated.items[0]).toMatchObject({
      kind: "user",
      createdAt: NOW,
    });
  });
});
