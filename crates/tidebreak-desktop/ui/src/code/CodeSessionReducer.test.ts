import { describe, expect, it } from "vitest";
import type { CodeEvent, SequencedCodeEventFrame } from "../api/types";
import {
  applyAcceptedTurn,
  hydrateCodeTurns,
  initialCodeSessionState,
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
      framed(4, { type: "file_changed", path: "a.ts", kind: "modified", diffstat: {
        files: 1,
        insertions: 1,
        deletions: 0,
        truncated: false,
      } }),
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

const SNAPSHOT_TURN = {
  id: "t1",
  session_id: "sess-1",
  ordinal: 1,
  status: "completed" as const,
  user_input: "list the files",
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
    });
    expect(hydrated.items[1]).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      status: "completed",
      durationMs: 2500,
    });

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
    });
    expect(
      replayed.state.items.filter((item) => item.kind === "turn_boundary"),
    ).toHaveLength(1);
    expect(replayed.state.items.find((item) => item.kind === "assistant")).toMatchObject({
      text: "README.md",
    });
    expect(replayed.state.lifecycle).toBe("idle");
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
  });
});
