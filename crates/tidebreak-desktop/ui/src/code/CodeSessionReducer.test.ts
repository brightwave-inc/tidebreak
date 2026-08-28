import { describe, expect, it, vi } from "vitest";
import type { CodeEvent, SequencedCodeEventFrame } from "../api/types";
import {
  applyAcceptedTurn,
  applyCodeTurnSnapshot,
  applyStoredRewrites,
  applyTurnRewrite,
  hydrateCodeTurns,
  initialCodeSessionState,
  mainAgentTranscriptItems,
  markCodeSessionHydrated,
  reconcileCodeTurnSnapshot,
  reconcilePendingCodeTurns,
  reduceCodeSessionEvent,
  subagentTranscriptItems,
  userItemId,
  type CodeSessionDeps,
  type CodeSessionState,
} from "./CodeSessionReducer";

const NOW = "2026-08-15T12:00:00.000Z";
const LATER = "2026-08-15T12:00:02.500Z";
const LONG_TURN_START = "2026-08-15T20:41:23.000Z";
const LONG_TURN_END = "2026-08-15T20:42:28.000Z";
const FAST_REPLAY_START = "2026-08-15T20:45:00.000Z";
const FAST_REPLAY_END = "2026-08-15T20:45:00.040Z";

const NO_USAGE = {
  input_tokens: 10,
  output_tokens: 4,
  cache_read_input_tokens: 0,
  cache_creation_input_tokens: 0,
  context_tokens: 0,
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
  let current = {
    state,
    effects: [] as ReturnType<typeof reduceCodeSessionEvent>["effects"],
  };
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

  it("applies a transient frame without moving the cursor", () => {
    // Assistant deltas are live-only (record 57): no row holds them, so the
    // duplicate check does not apply and the resume cursor must stay where
    // the journal left it. Gating them on `seq` would drop every one.
    const clock = deps();
    const { state } = play(
      [{ type: "turn_started", turn_id: "t1" }],
      initialCodeSessionState(),
      clock,
    );
    const first = reduceCodeSessionEvent(
      state,
      {
        seq: state.lastSeq,
        event: { type: "assistant_delta", text: "half a " },
        transient: true,
      },
      clock,
    );
    const second = reduceCodeSessionEvent(
      first.state,
      {
        seq: state.lastSeq,
        event: { type: "assistant_delta", text: "sentence" },
        transient: true,
      },
      clock,
    );
    expect(second.state.assistantBuffer).toBe("half a sentence");
    expect(second.state.lastSeq).toBe(state.lastSeq);
  });

  it("replaces streamed text with a catch-up tail after reconnect", () => {
    const clock = deps();
    const { state } = play(
      [{ type: "turn_started", turn_id: "t1" }],
      initialCodeSessionState(),
      clock,
    );
    const streamed = reduceCodeSessionEvent(
      state,
      {
        seq: state.lastSeq,
        event: { type: "assistant_delta", text: "first second " },
        transient: true,
      },
      clock,
    );
    const caughtUp = reduceCodeSessionEvent(
      streamed.state,
      {
        seq: state.lastSeq,
        event: { type: "assistant_delta", text: "first second third" },
        transient: true,
        replacement: true,
      },
      clock,
    );
    const continued = reduceCodeSessionEvent(
      caughtUp.state,
      {
        seq: state.lastSeq,
        event: { type: "assistant_delta", text: "." },
        transient: true,
      },
      clock,
    );

    expect(continued.state.assistantBuffer).toBe("first second third.");
    expect(continued.state.lastSeq).toBe(state.lastSeq);
  });

  it("says so when the replay started partway through", () => {
    const clock = deps();
    const first = reduceCodeSessionEvent(
      initialCodeSessionState(),
      {
        seq: 900,
        event: { type: "turn_started", turn_id: "t9" },
        replayed: true,
        truncated: true,
      },
      clock,
    );
    expect(first.state.items[0]).toMatchObject({
      kind: "notice",
      level: "info",
    });
    // A reconnect that truncates again must not stack a second line.
    const again = reduceCodeSessionEvent(
      first.state,
      {
        seq: 901,
        event: { type: "turn_started", turn_id: "t10" },
        replayed: true,
        truncated: true,
      },
      clock,
    );
    expect(
      again.state.items.filter((item) => item.kind === "notice"),
    ).toHaveLength(1);
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

  it("marks an undecided request abandoned rather than denied", () => {
    const { state } = play([
      { type: "turn_started", turn_id: "t1" },
      { type: "approval_requested", approval_id: "appr-1" },
      {
        type: "approval_resolved",
        approval_id: "appr-1",
        decision: { type: "abandoned" },
      },
    ]);
    const card = state.items.find((item) => item.kind === "approval");
    expect(card).toMatchObject({
      kind: "approval",
      approvalId: "appr-1",
      state: "abandoned",
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
        {
          type: "session_started",
          harness_kind: "claude_code",
          harness_version: "1.0",
        },
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

describe("subagent attribution", () => {
  it("keeps parent and child assistant messages in separate streams", () => {
    const { state } = play([
      { type: "turn_started", turn_id: "t1" },
      { type: "assistant_delta", text: "Parent response" },
      {
        type: "assistant_message",
        text: "Child report",
        parent_call_id: "task-1",
      },
      { type: "turn_completed", usage: NO_USAGE },
    ]);

    const messages = state.items.filter((item) => item.kind === "assistant");
    expect(messages).toHaveLength(2);
    expect(messages[0]).toMatchObject({
      text: "Parent response",
      parentCallId: null,
      streaming: false,
    });
    expect(messages[1]).toMatchObject({
      text: "Child report",
      parentCallId: "task-1",
      streaming: false,
    });
    expect(state.assistantBuffer).toBe("");
  });

  it("preserves tool attribution and filters the parent and child views", () => {
    const accepted = applyAcceptedTurn(initialCodeSessionState(), {
      id: "t1",
      session_id: "s1",
      ordinal: 1,
      status: "running",
      fast_mode: false,
      user_input: "Delegate the audit",
      attachments: [],
      started_at: NOW,
    });
    const { state } = play(
      [
        {
          type: "tool_started",
          call_id: "task-1",
          name: "Task",
          detail: { kind: "other", summary: "Audit the parser" },
        },
        {
          type: "tool_started",
          call_id: "child-read",
          name: "Read",
          detail: { kind: "file_read", path: "src/parser.rs" },
          parent_call_id: "task-1",
        },
        {
          type: "tool_completed",
          call_id: "child-read",
          outcome: "succeeded",
          preview: "parser source",
          parent_call_id: "task-1",
        },
        {
          type: "assistant_message",
          text: "The parser is sound.",
          parent_call_id: "task-1",
        },
      ],
      accepted,
    );

    const childTool = state.items.find(
      (item) => item.kind === "tool" && item.callId === "child-read",
    );
    expect(childTool).toMatchObject({
      parentCallId: "task-1",
      status: "succeeded",
      preview: "parser source",
    });

    expect(
      mainAgentTranscriptItems(state.items).map((item) => item.kind),
    ).toEqual(["user", "tool"]);
    expect(
      subagentTranscriptItems(state.items, "task-1").map((item) => item.kind),
    ).toEqual(["tool", "assistant"]);
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
  fast_mode: false,
  user_input: "list the files",
  attachments: [],
  started_at: NOW,
  ended_at: LATER,
};

describe("hydrate then replay", () => {
  it("does not let a stale running snapshot reopen a completed turn", () => {
    const now = vi
      .fn<() => string>()
      .mockReturnValueOnce(NOW)
      .mockReturnValue(LATER);
    const clock: CodeSessionDeps = { nextId: deps().nextId, now };
    const started = reduceCodeSessionEvent(
      initialCodeSessionState(),
      framed(1, { type: "turn_started", turn_id: "t1" }),
      clock,
    );
    const completed = reduceCodeSessionEvent(
      started.state,
      framed(2, { type: "turn_completed", usage: NO_USAGE }),
      clock,
    );

    const refreshed = applyCodeTurnSnapshot(completed.state, {
      ...SNAPSHOT_TURN,
      status: "running",
      ended_at: undefined,
    });

    expect(refreshed).toMatchObject({
      busy: false,
      activeTurnId: null,
      turnStartedAt: null,
      lifecycle: "idle",
    });
    expect(refreshed.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      durationMs: 2_500,
    });
  });

  it("does not let an older running snapshot replace a newer active turn", () => {
    const clock: CodeSessionDeps = {
      nextId: deps().nextId,
      now: vi
        .fn<() => string>()
        .mockReturnValueOnce(NOW)
        .mockReturnValueOnce(LATER)
        .mockReturnValue("2026-08-15T12:00:03.000Z"),
    };
    const t1Started = reduceCodeSessionEvent(
      initialCodeSessionState(),
      framed(1, { type: "turn_started", turn_id: "t1" }),
      clock,
    );
    const t1Completed = reduceCodeSessionEvent(
      t1Started.state,
      framed(2, { type: "turn_completed", usage: NO_USAGE }),
      clock,
    );
    const t2Started = reduceCodeSessionEvent(
      t1Completed.state,
      framed(3, { type: "turn_started", turn_id: "t2" }),
      clock,
    );

    const refreshed = applyCodeTurnSnapshot(t2Started.state, {
      ...SNAPSHOT_TURN,
      status: "running",
      ended_at: undefined,
    });

    expect(refreshed).toMatchObject({
      busy: true,
      activeTurnId: "t2",
      journalTurnId: "t2",
      turnStartedAt: "2026-08-15T12:00:03.000Z",
      lifecycle: "running",
    });
  });

  it("keeps a long completed turn's durable duration during fast replay", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      {
        ...SNAPSHOT_TURN,
        started_at: LONG_TURN_START,
        ended_at: LONG_TURN_END,
      },
    ]);
    const now = vi
      .fn<() => string>()
      .mockReturnValueOnce(FAST_REPLAY_START)
      .mockReturnValue(FAST_REPLAY_END);
    const clock: CodeSessionDeps = { nextId: deps().nextId, now };

    const started = reduceCodeSessionEvent(
      hydrated,
      framed(1, { type: "turn_started", turn_id: "t1" }, true),
      clock,
    );
    const completed = reduceCodeSessionEvent(
      started.state,
      framed(2, { type: "turn_completed", usage: NO_USAGE }, true),
      clock,
    );

    expect(completed.state.items[0]).toMatchObject({
      kind: "user",
      createdAt: LONG_TURN_START,
    });
    expect(completed.state.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      durationMs: 65_000,
    });
    expect(now).not.toHaveBeenCalled();
  });

  it("keeps one durable boundary after duplicate replayed terminal events", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      {
        ...SNAPSHOT_TURN,
        started_at: LONG_TURN_START,
        ended_at: LONG_TURN_END,
      },
    ]);
    const now = vi
      .fn<() => string>()
      .mockReturnValueOnce(FAST_REPLAY_START)
      .mockReturnValue(FAST_REPLAY_END);
    const clock: CodeSessionDeps = { nextId: deps().nextId, now };
    const started = reduceCodeSessionEvent(
      hydrated,
      framed(1, { type: "turn_started", turn_id: "t1" }, true),
      clock,
    );
    const completed = reduceCodeSessionEvent(
      started.state,
      framed(2, { type: "turn_completed", usage: NO_USAGE }, true),
      clock,
    );
    const duplicate = reduceCodeSessionEvent(
      completed.state,
      framed(3, { type: "turn_completed", usage: NO_USAGE }, true),
      clock,
    );
    const boundaries = duplicate.state.items.filter(
      (item) => item.kind === "turn_boundary",
    );

    expect(boundaries).toHaveLength(1);
    expect(boundaries[0]).toMatchObject({
      turnId: "t1",
      durationMs: 65_000,
    });
    expect(now).not.toHaveBeenCalled();
  });

  it("keeps replay duration unknown when the snapshot has no durable end", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      { ...SNAPSHOT_TURN, ended_at: undefined },
    ]);
    const now = vi
      .fn<() => string>()
      .mockReturnValueOnce(FAST_REPLAY_START)
      .mockReturnValue(FAST_REPLAY_END);
    const clock: CodeSessionDeps = { nextId: deps().nextId, now };
    const started = reduceCodeSessionEvent(
      hydrated,
      framed(1, { type: "turn_started", turn_id: "t1" }, true),
      clock,
    );
    const completed = reduceCodeSessionEvent(
      started.state,
      framed(2, { type: "turn_completed", usage: NO_USAGE }, true),
      clock,
    );

    expect(completed.state.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      durationMs: null,
    });
    expect(now).not.toHaveBeenCalled();
  });

  it("requests and applies exact timing after a replay-only terminal", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      { ...SNAPSHOT_TURN, status: "running", ended_at: undefined },
    ]);
    const replayedStart = reduceCodeSessionEvent(
      hydrated,
      framed(1, { type: "turn_started", turn_id: "t1" }, true),
      deps(),
    );
    const completed = reduceCodeSessionEvent(
      replayedStart.state,
      framed(2, { type: "turn_completed", usage: NO_USAGE }, true),
      deps(),
    );

    expect(completed.effects).toEqual([
      { type: "turn_resolved" },
      { type: "turn_snapshot_needed", turnId: "t1" },
    ]);
    expect(completed.state.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      durationMs: null,
    });

    const reconciled = reconcileCodeTurnSnapshot(completed.state, {
      ...SNAPSHOT_TURN,
      started_at: LONG_TURN_START,
      ended_at: LONG_TURN_END,
    });
    expect(reconciled).toMatchObject({
      busy: false,
      activeTurnId: null,
      lifecycle: "idle",
    });
    expect(reconciled.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      durationMs: 65_000,
    });
  });

  it("does not reopen an attributed terminal from a stale running hydration", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      { ...SNAPSHOT_TURN, status: "running", ended_at: undefined },
    ]);
    const replayedStart = reduceCodeSessionEvent(
      hydrated,
      framed(1, { type: "turn_started", turn_id: "t1" }, true),
      deps(),
    );
    const completed = reduceCodeSessionEvent(
      replayedStart.state,
      framed(2, { type: "turn_completed", usage: NO_USAGE }, true),
      deps(),
    );

    const staleHydration = hydrateCodeTurns(completed.state, [
      { ...SNAPSHOT_TURN, status: "running", ended_at: undefined },
    ]);

    expect(staleHydration).toMatchObject({
      busy: false,
      activeTurnId: null,
      journalTurnId: null,
      lifecycle: "idle",
    });
    expect(staleHydration.pendingTerminalReconciliations.get(2)).toMatchObject({
      eventSeq: 2,
      turnId: "t1",
      status: "completed",
    });
  });

  it("keeps a newly accepted turn active while buffered replay finishes an older turn", () => {
    const accepted = applyAcceptedTurn(initialCodeSessionState(), {
      ...SNAPSHOT_TURN,
      id: "t2",
      ordinal: 2,
      status: "running",
      fast_mode: false,
      user_input: "run the tests",
      ended_at: undefined,
    });

    expect(accepted).toMatchObject({
      lastSeq: 0,
      activeTurnId: "t2",
      journalTurnId: null,
      acceptedTurnFence: { turnId: "t2", afterSeq: 0 },
      turnActivityRevision: 1,
    });

    const replayedStart = reduceCodeSessionEvent(
      accepted,
      framed(1, { type: "turn_started", turn_id: "t1" }, true),
      deps(),
    );
    const replayedTerminal = reduceCodeSessionEvent(
      replayedStart.state,
      framed(2, { type: "turn_completed", usage: NO_USAGE }, true),
      deps(),
    );

    expect(replayedStart.state).toMatchObject({
      lastSeq: 1,
      busy: true,
      activeTurnId: "t2",
      journalTurnId: "t1",
      acceptedTurnFence: { turnId: "t2", afterSeq: 0 },
      turnActivityRevision: 1,
    });
    expect(replayedTerminal.state).toMatchObject({
      lastSeq: 2,
      busy: true,
      activeTurnId: "t2",
      journalTurnId: null,
      acceptedTurnFence: { turnId: "t2", afterSeq: 0 },
      turnActivityRevision: 1,
      lifecycle: "running",
    });
    expect(
      replayedTerminal.state.items.find(
        (item) => item.kind === "turn_boundary" && item.turnId === "t1",
      ),
    ).toMatchObject({ status: "completed", usage: NO_USAGE });
  });

  it("rejects a snapshot activity update captured before an accepted turn", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      { ...SNAPSHOT_TURN, status: "running", ended_at: undefined },
    ]);
    const replayedStart = reduceCodeSessionEvent(
      hydrated,
      framed(1, { type: "turn_started", turn_id: "t1" }, true),
      deps(),
    );
    const terminal = reduceCodeSessionEvent(
      replayedStart.state,
      framed(2, { type: "turn_completed", usage: NO_USAGE }, true),
      deps(),
    );
    const observedTurnActivityRevision = terminal.state.turnActivityRevision;
    const accepted = applyAcceptedTurn(terminal.state, {
      ...SNAPSHOT_TURN,
      id: "t2",
      ordinal: 2,
      status: "running",
      fast_mode: false,
      user_input: "run the tests",
      ended_at: undefined,
    });

    expect(accepted.lastSeq).toBe(terminal.state.lastSeq);
    const reconciled = reconcilePendingCodeTurns(
      accepted,
      [SNAPSHOT_TURN],
      [
        {
          turnId: "t1",
          eventSeq: 2,
          observedSeq: 2,
          observedTurnActivityRevision,
        },
      ],
    );

    expect(reconciled).toMatchObject({
      lastSeq: 2,
      busy: true,
      activeTurnId: "t2",
      acceptedTurnFence: { turnId: "t2", afterSeq: 2 },
      lifecycle: "running",
    });
    expect(
      reconciled.items.find((item) => item.id === "boundary:t1"),
    ).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      durationMs: 2_500,
    });
  });

  it("reconciles a capped terminal against the hydrated active turn", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      {
        ...SNAPSHOT_TURN,
        id: "t2",
        ordinal: 2,
        status: "running",
        fast_mode: false,
        user_input: "run the tests",
        ended_at: undefined,
      },
    ]);
    const terminal = reduceCodeSessionEvent(
      hydrated,
      {
        seq: 2_001,
        replayed: true,
        truncated: true,
        event: { type: "turn_completed", usage: NO_USAGE },
      },
      deps(),
    );

    expect(terminal.state).toMatchObject({
      lastSeq: 2_001,
      busy: true,
      activeTurnId: "t2",
      journalTurnId: null,
      lifecycle: "running",
    });
    expect(terminal.effects).toEqual([
      { type: "turn_snapshot_needed", turnId: null },
      { type: "turn_resolved" },
    ]);

    const reconciled = reconcilePendingCodeTurns(
      terminal.state,
      [
        {
          ...SNAPSHOT_TURN,
          id: "t2",
          ordinal: 2,
          user_input: "run the tests",
          started_at: NOW,
          ended_at: LATER,
        },
      ],
      [
        {
          turnId: null,
          eventSeq: 2_001,
          observedSeq: 2_001,
          observedTurnActivityRevision: terminal.state.turnActivityRevision,
        },
      ],
    );
    expect(reconciled).toMatchObject({
      busy: false,
      activeTurnId: null,
      journalTurnId: null,
      turnStartedAt: null,
      lifecycle: "idle",
    });
    expect(reconciled.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t2",
      durationMs: 2_500,
    });
    expect(reconciled.pendingTerminalReconciliations.size).toBe(1);
  });

  it("keeps a capped failure unassigned when only its snapshot candidate completes", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      {
        ...SNAPSHOT_TURN,
        id: "t2",
        ordinal: 2,
        status: "running",
        fast_mode: false,
        user_input: "run the tests",
        ended_at: undefined,
      },
    ]);
    const terminal = reduceCodeSessionEvent(
      hydrated,
      {
        seq: 2_001,
        replayed: true,
        truncated: true,
        event: {
          type: "turn_failed",
          error: { message: "compiler crashed: missing libssl" },
        },
      },
      deps(),
    );

    expect(terminal.state.pendingTerminalReconciliations.get(2_001)).toEqual({
      eventSeq: 2_001,
      turnId: null,
      candidateTurnId: "t2",
      nextTurnId: null,
      status: "failed",
      usage: null,
      error: "compiler crashed: missing libssl",
      diffstat: null,
      previousUsage: null,
    });
    expect(
      terminal.state.items.find(
        (item) => item.kind === "turn_boundary" && item.turnId === "t2",
      ),
    ).toBeUndefined();

    const reconciled = hydrateCodeTurns(terminal.state, [
      {
        ...SNAPSHOT_TURN,
        id: "t2",
        ordinal: 2,
        status: "failed",
        fast_mode: false,
        user_input: "run the tests",
        started_at: NOW,
        ended_at: LATER,
      },
    ]);

    expect(reconciled).toMatchObject({
      busy: false,
      activeTurnId: null,
      lifecycle: "idle",
    });
    expect(reconciled.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t2",
      status: "failed",
      durationMs: 2_500,
      error: null,
    });
    expect(reconciled.pendingTerminalReconciliations.size).toBe(1);
  });

  it("does not attach a capped failure after hydration already returned the terminal turn", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      {
        ...SNAPSHOT_TURN,
        id: "t2",
        ordinal: 2,
        status: "failed",
        fast_mode: false,
        user_input: "run the tests",
      },
    ]);
    const terminal = reduceCodeSessionEvent(
      hydrated,
      {
        seq: 2_001,
        replayed: true,
        truncated: true,
        event: {
          type: "turn_failed",
          error: { message: "compiler crashed: missing libssl" },
        },
      },
      deps(),
    );

    const reconciled = reconcilePendingCodeTurns(
      terminal.state,
      [
        {
          ...SNAPSHOT_TURN,
          id: "t2",
          ordinal: 2,
          status: "failed",
          fast_mode: false,
          user_input: "run the tests",
        },
      ],
      [
        {
          turnId: null,
          eventSeq: 2_001,
          observedSeq: 2_001,
          observedTurnActivityRevision: terminal.state.turnActivityRevision,
        },
      ],
    );

    expect(reconciled.pendingTerminalReconciliations.size).toBe(1);
    expect(
      reconciled.items.find(
        (item) => item.kind === "turn_boundary" && item.turnId === "t2",
      ),
    ).toMatchObject({
      status: "failed",
      durationMs: 2_500,
      error: null,
    });
  });

  it("does not attach a capped failure when no initial turn snapshot was available", () => {
    const terminal = reduceCodeSessionEvent(
      initialCodeSessionState(),
      {
        seq: 2_001,
        replayed: true,
        truncated: true,
        event: {
          type: "turn_failed",
          error: { message: "compiler crashed: missing libssl" },
        },
      },
      deps(),
    );

    const reconciled = reconcilePendingCodeTurns(
      terminal.state,
      [
        SNAPSHOT_TURN,
        {
          ...SNAPSHOT_TURN,
          id: "t2",
          ordinal: 2,
          status: "failed",
          fast_mode: false,
          user_input: "run the tests",
        },
      ],
      [
        {
          turnId: null,
          eventSeq: 2_001,
          observedSeq: 2_001,
          observedTurnActivityRevision: terminal.state.turnActivityRevision,
        },
      ],
    );

    expect(reconciled.pendingTerminalReconciliations.size).toBe(1);
    expect(reconciled.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t2",
      status: "failed",
      error: null,
    });
  });

  it("does not give a later completed turn an earlier unattributed terminal's usage", () => {
    const replayUsage = {
      ...NO_USAGE,
      input_tokens: 91,
      output_tokens: 37,
    };
    const terminal = reduceCodeSessionEvent(
      initialCodeSessionState(),
      {
        seq: 2_001,
        replayed: true,
        truncated: true,
        event: { type: "turn_completed", usage: replayUsage },
      },
      deps(),
    );
    const reconciled = reconcilePendingCodeTurns(
      terminal.state,
      [
        {
          ...SNAPSHOT_TURN,
          id: "t2",
          ordinal: 2,
          user_input: "later completed work",
        },
      ],
      [
        {
          turnId: null,
          eventSeq: 2_001,
          observedSeq: 2_001,
          observedTurnActivityRevision: terminal.state.turnActivityRevision,
        },
      ],
    );

    expect(reconciled.pendingTerminalReconciliations.size).toBe(1);
    expect(reconciled.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t2",
      status: "completed",
      usage: null,
      error: null,
    });
    expect(reconciled.lastUsage).toBeNull();
  });

  it("keeps a capped failure pending while its stale candidate still runs", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      {
        ...SNAPSHOT_TURN,
        id: "t2",
        ordinal: 2,
        status: "running",
        fast_mode: false,
        user_input: "run the tests",
        ended_at: undefined,
      },
    ]);
    const terminal = reduceCodeSessionEvent(
      hydrated,
      {
        seq: 2_001,
        replayed: true,
        truncated: true,
        event: {
          type: "turn_failed",
          error: { message: "belongs to an older turn" },
        },
      },
      deps(),
    );

    const reconciled = hydrateCodeTurns(terminal.state, [
      {
        ...SNAPSHOT_TURN,
        status: "failed",
      },
      {
        ...SNAPSHOT_TURN,
        id: "t2",
        ordinal: 2,
        status: "running",
        fast_mode: false,
        user_input: "run the tests",
        ended_at: undefined,
      },
    ]);

    expect(reconciled).toMatchObject({
      busy: true,
      activeTurnId: "t2",
      lifecycle: "running",
      contentRevision: 1,
    });
    expect(reconciled.pendingTerminalReconciliations.get(2_001)).toMatchObject({
      turnId: null,
      candidateTurnId: "t2",
      status: "failed",
      error: "belongs to an older turn",
    });
    expect(
      reconciled.items.find(
        (item) =>
          item.kind === "turn_boundary" &&
          item.error === "belongs to an older turn",
      ),
    ).toBeUndefined();
    expect(
      reconciled.items.find(
        (item) => item.kind === "turn_boundary" && item.turnId === "t1",
      ),
    ).toMatchObject({ error: null });
    expect(
      reconciled.items.find(
        (item) => item.kind === "turn_boundary" && item.turnId === "t2",
      ),
    ).toBeUndefined();
  });

  it("does not attribute capped replay output to a stale retained turn", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      {
        ...SNAPSHOT_TURN,
        id: "t1",
        ordinal: 1,
        status: "running",
        fast_mode: false,
        user_input: "old work",
        ended_at: undefined,
      },
    ]);
    const output = reduceCodeSessionEvent(
      hydrated,
      {
        seq: 2_000,
        replayed: true,
        truncated: true,
        event: { type: "assistant_delta", text: "new turn output" },
      },
      deps(),
    );
    const terminal = reduceCodeSessionEvent(
      output.state,
      framed(
        2_001,
        { type: "turn_failed", error: { message: "new turn failed" } },
        true,
      ),
      deps(),
    );

    const reconciled = reconcilePendingCodeTurns(
      terminal.state,
      [
        {
          ...SNAPSHOT_TURN,
          id: "t1",
          ordinal: 1,
          status: "completed",
          fast_mode: false,
          user_input: "old work",
        },
        {
          ...SNAPSHOT_TURN,
          id: "t2",
          ordinal: 2,
          status: "failed",
          fast_mode: false,
          user_input: "new work",
        },
      ],
      [
        {
          turnId: null,
          eventSeq: 2_001,
          observedSeq: 2_001,
          observedTurnActivityRevision: terminal.state.turnActivityRevision,
        },
      ],
    );

    expect(
      reconciled.items.find((item) => item.kind === "assistant"),
    ).toMatchObject({ turnId: null, text: "new turn output" });
    expect(
      reconciled.items.find(
        (item) => item.kind === "turn_boundary" && item.turnId === "t1",
      ),
    ).toMatchObject({ error: null });
    expect(reconciled.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t2",
      error: null,
    });
    expect(reconciled.pendingTerminalReconciliations.size).toBe(1);
  });

  it("inserts a recovered boundary before the following turn", () => {
    const terminal = reduceCodeSessionEvent(
      initialCodeSessionState(),
      {
        seq: 100,
        replayed: true,
        truncated: true,
        event: { type: "turn_completed", usage: NO_USAGE },
      },
      deps(),
    );
    const started = reduceCodeSessionEvent(
      terminal.state,
      framed(101, { type: "turn_started", turn_id: "t2" }, true),
      deps(),
    );
    const output = reduceCodeSessionEvent(
      started.state,
      framed(102, { type: "assistant_delta", text: "second turn" }, true),
      deps(),
    );

    const reconciled = reconcilePendingCodeTurns(
      output.state,
      [
        SNAPSHOT_TURN,
        {
          ...SNAPSHOT_TURN,
          id: "t2",
          ordinal: 2,
          status: "running",
          fast_mode: false,
          user_input: "run the tests",
          ended_at: undefined,
        },
      ],
      [
        {
          turnId: null,
          eventSeq: 100,
          observedSeq: 102,
          observedTurnActivityRevision: output.state.turnActivityRevision,
        },
      ],
    );

    expect(reconciled.items.map((item) => item.id)).toEqual([
      "notice:truncated-replay",
      "user:t1",
      "boundary:t1",
      "user:t2",
      "c1",
    ]);
    expect(reconciled).toMatchObject({
      busy: true,
      activeTurnId: "t2",
      journalTurnId: "t2",
    });
  });

  it("promotes confirmed replay usage when the snapshot omits usage", () => {
    const usage = { ...NO_USAGE, input_tokens: 44, output_tokens: 12 };
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      { ...SNAPSHOT_TURN, status: "running", ended_at: undefined },
    ]);
    const started = reduceCodeSessionEvent(
      hydrated,
      framed(1, { type: "turn_started", turn_id: "t1" }, true),
      deps(),
    );
    const terminal = reduceCodeSessionEvent(
      started.state,
      framed(2, { type: "turn_completed", usage }, true),
      deps(),
    );

    expect(terminal.state.lastUsage).toBeNull();
    const reconciled = reconcilePendingCodeTurns(
      terminal.state,
      [SNAPSHOT_TURN],
      [
        {
          turnId: "t1",
          eventSeq: 2,
          observedSeq: 2,
          observedTurnActivityRevision: terminal.state.turnActivityRevision,
        },
      ],
    );

    expect(reconciled.lastUsage).toEqual(usage);
    expect(reconciled.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      usage,
    });
  });

  it("uses a stale refresh for history without settling a newer live turn", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      { ...SNAPSHOT_TURN, status: "running", ended_at: undefined },
    ]);
    const started = reduceCodeSessionEvent(
      hydrated,
      framed(1, { type: "turn_started", turn_id: "t1" }, true),
      deps(),
    );
    const terminal = reduceCodeSessionEvent(
      started.state,
      framed(2, { type: "turn_completed", usage: NO_USAGE }, true),
      deps(),
    );
    const newer = reduceCodeSessionEvent(
      terminal.state,
      framed(3, { type: "turn_started", turn_id: "t2" }),
      deps(),
    );

    const reconciled = reconcilePendingCodeTurns(
      newer.state,
      [
        {
          ...SNAPSHOT_TURN,
          started_at: LONG_TURN_START,
          ended_at: LONG_TURN_END,
        },
        {
          ...SNAPSHOT_TURN,
          id: "t2",
          ordinal: 2,
          status: "running",
          fast_mode: false,
          user_input: "run the tests",
          ended_at: undefined,
        },
      ],
      [
        {
          turnId: "t1",
          eventSeq: 2,
          observedSeq: 2,
          observedTurnActivityRevision: terminal.state.turnActivityRevision,
        },
      ],
    );

    expect(reconciled).toMatchObject({
      busy: true,
      activeTurnId: "t2",
      journalTurnId: "t2",
      lifecycle: "running",
    });
    expect(
      reconciled.items.find((item) => item.id === "boundary:t1"),
    ).toMatchObject({ durationMs: 65_000 });
  });

  it("does not let an older terminal refresh settle a newer turn", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      { ...SNAPSHOT_TURN, status: "running", ended_at: undefined },
    ]);
    const replayedStart = reduceCodeSessionEvent(
      hydrated,
      framed(1, { type: "turn_started", turn_id: "t1" }, true),
      deps(),
    );
    const completed = reduceCodeSessionEvent(
      replayedStart.state,
      framed(2, { type: "turn_completed", usage: NO_USAGE }, true),
      deps(),
    );
    const t2Started = reduceCodeSessionEvent(
      completed.state,
      framed(3, { type: "turn_started", turn_id: "t2" }),
      deps(),
    );

    const reconciled = reconcileCodeTurnSnapshot(t2Started.state, {
      ...SNAPSHOT_TURN,
      started_at: LONG_TURN_START,
      ended_at: LONG_TURN_END,
    });

    expect(reconciled).toMatchObject({
      busy: true,
      activeTurnId: "t2",
      journalTurnId: "t2",
      lifecycle: "running",
    });
    expect(
      reconciled.items.find((item) => item.id === "boundary:t1"),
    ).toMatchObject({
      kind: "turn_boundary",
      durationMs: 65_000,
    });
  });

  it("clears stale activity when an authoritative snapshot has no running turn", () => {
    const running = hydrateCodeTurns(initialCodeSessionState(), [
      { ...SNAPSHOT_TURN, status: "running", ended_at: undefined },
    ]);
    const completed = hydrateCodeTurns(running, [SNAPSHOT_TURN]);

    expect(completed).toMatchObject({
      busy: false,
      activeTurnId: null,
      journalTurnId: null,
      turnStartedAt: null,
      turnStartObservedLive: false,
      lifecycle: "idle",
    });
    expect(completed.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      durationMs: 2_500,
    });
  });

  it("lets live timing replace a replay-only unknown boundary", () => {
    const now = vi
      .fn<() => string>()
      .mockReturnValueOnce(NOW)
      .mockReturnValue(LATER);
    const clock: CodeSessionDeps = { nextId: deps().nextId, now };
    const replayedStart = reduceCodeSessionEvent(
      initialCodeSessionState(),
      framed(1, { type: "turn_started", turn_id: "t1" }, true),
      clock,
    );
    const replayedEnd = reduceCodeSessionEvent(
      replayedStart.state,
      framed(2, { type: "turn_completed", usage: NO_USAGE }, true),
      clock,
    );
    expect(replayedEnd.state.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      durationMs: null,
    });

    const liveStart = reduceCodeSessionEvent(
      replayedEnd.state,
      framed(3, { type: "turn_started", turn_id: "t1" }),
      clock,
    );
    const liveEnd = reduceCodeSessionEvent(
      liveStart.state,
      framed(4, { type: "turn_completed", usage: NO_USAGE }),
      clock,
    );

    expect(liveEnd.state.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      durationMs: 2_500,
    });
    expect(now).toHaveBeenCalledTimes(2);
  });

  it("measures a genuinely live turn from the client clock", () => {
    const now = vi
      .fn<() => string>()
      .mockReturnValueOnce(NOW)
      .mockReturnValue(LATER);
    const clock: CodeSessionDeps = { nextId: deps().nextId, now };
    const started = reduceCodeSessionEvent(
      initialCodeSessionState(),
      framed(1, { type: "turn_started", turn_id: "t1" }),
      clock,
    );
    const completed = reduceCodeSessionEvent(
      started.state,
      framed(2, { type: "turn_completed", usage: NO_USAGE }),
      clock,
    );

    expect(completed.state.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      durationMs: 2_500,
    });
    expect(now).toHaveBeenCalledTimes(2);
  });

  it("keeps live timing when completion arrives through replay", () => {
    const now = vi
      .fn<() => string>()
      .mockReturnValueOnce(NOW)
      .mockReturnValue(LATER);
    const clock: CodeSessionDeps = { nextId: deps().nextId, now };
    const started = reduceCodeSessionEvent(
      initialCodeSessionState(),
      framed(1, { type: "turn_started", turn_id: "t1" }),
      clock,
    );
    const completed = reduceCodeSessionEvent(
      started.state,
      framed(2, { type: "turn_completed", usage: NO_USAGE }, true),
      clock,
    );

    expect(completed.state.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      durationMs: 2_500,
    });
    expect(now).toHaveBeenCalledTimes(2);
  });

  it("keeps live provenance when prompt hydration replaces the start timestamp", () => {
    const now = vi
      .fn<() => string>()
      .mockReturnValueOnce("2026-08-15T12:00:00.100Z")
      .mockReturnValue(LATER);
    const clock: CodeSessionDeps = { nextId: deps().nextId, now };
    const started = reduceCodeSessionEvent(
      initialCodeSessionState(),
      framed(1, { type: "turn_started", turn_id: "t1" }),
      clock,
    );
    const hydrated = applyAcceptedTurn(started.state, {
      ...SNAPSHOT_TURN,
      status: "running",
      ended_at: undefined,
    });
    const completed = reduceCodeSessionEvent(
      hydrated,
      framed(2, { type: "turn_completed", usage: NO_USAGE }, true),
      clock,
    );

    expect(completed.state.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      durationMs: 2_500,
    });
    expect(now).toHaveBeenCalledTimes(2);
  });

  it("keeps a hydrated running turn's durable start for live completion", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      {
        ...SNAPSHOT_TURN,
        status: "running",
        ended_at: undefined,
      },
    ]);
    const now = vi.fn<() => string>().mockReturnValue(LATER);
    const clock: CodeSessionDeps = { nextId: deps().nextId, now };
    const replayedStart = reduceCodeSessionEvent(
      hydrated,
      framed(1, { type: "turn_started", turn_id: "t1" }, true),
      clock,
    );

    expect(replayedStart.state.turnStartedAt).toBe(NOW);
    expect(now).not.toHaveBeenCalled();

    const completed = reduceCodeSessionEvent(
      replayedStart.state,
      framed(2, { type: "turn_completed", usage: NO_USAGE }),
      clock,
    );
    expect(completed.state.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      durationMs: 2_500,
    });
    expect(now).toHaveBeenCalledTimes(1);
  });

  it("does not assign a truncated replay terminal to the hydrated running turn", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      {
        ...SNAPSHOT_TURN,
        started_at: LONG_TURN_START,
        ended_at: LONG_TURN_END,
      },
      {
        ...SNAPSHOT_TURN,
        id: "t2",
        ordinal: 2,
        status: "running",
        fast_mode: false,
        user_input: "run the tests",
        started_at: NOW,
        ended_at: undefined,
      },
    ]);
    const now = vi.fn<() => string>().mockReturnValue(LATER);
    const clock: CodeSessionDeps = { nextId: deps().nextId, now };
    const staleTerminal = reduceCodeSessionEvent(
      hydrated,
      {
        seq: 100,
        replayed: true,
        truncated: true,
        event: { type: "turn_completed", usage: NO_USAGE },
      },
      clock,
    );

    expect(staleTerminal.effects).toEqual([
      { type: "turn_snapshot_needed", turnId: null },
      { type: "turn_resolved" },
    ]);
    expect(staleTerminal.state.busy).toBe(true);
    expect(staleTerminal.state.activeTurnId).toBe("t2");
    expect(staleTerminal.state.lifecycle).toBe("running");
    expect(staleTerminal.state.lastUsage).toBeNull();
    expect(
      staleTerminal.state.items.find(
        (item) => item.kind === "turn_boundary" && item.turnId === "t2",
      ),
    ).toBeUndefined();
    expect(now).not.toHaveBeenCalled();

    const replayedStart = reduceCodeSessionEvent(
      staleTerminal.state,
      framed(101, { type: "turn_started", turn_id: "t2" }, true),
      clock,
    );
    const completed = reduceCodeSessionEvent(
      replayedStart.state,
      framed(102, {
        type: "turn_completed",
        usage: { ...NO_USAGE, input_tokens: 20, output_tokens: 8 },
      }),
      clock,
    );

    expect(completed.state.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t2",
      durationMs: 2_500,
      usage: { input_tokens: 20, output_tokens: 8 },
    });
    expect(now).toHaveBeenCalledTimes(1);
  });

  it("shows the user prompt after a disposed store reopens from after=0", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      SNAPSHOT_TURN,
    ]);
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
    expect(
      replayed.state.items.filter((item) => item.kind === "user"),
    ).toHaveLength(1);
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
    expect(
      replayed.state.items.find((item) => item.kind === "assistant"),
    ).toMatchObject({
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
    expect(
      state.items.filter((item) => item.kind === "assistant"),
    ).toHaveLength(1);
    expect(state.items.map((item) => item.kind)).toEqual([
      "assistant",
      "tool",
      "turn_boundary",
    ]);
  });

  it("inserts a late assistant message before the finished turn's seam", () => {
    const { state: completed } = play([
      { type: "turn_started", turn_id: "t1" },
      { type: "turn_completed", usage: NO_USAGE },
    ]);
    const late = reduceCodeSessionEvent(
      completed,
      framed(completed.lastSeq + 1, {
        type: "assistant_message",
        text: "Three issues triaged.",
      }),
      deps(),
    );

    expect(late.state.items.map((item) => item.kind)).toEqual([
      "assistant",
      "turn_boundary",
    ]);
    expect(late.state.items[0]).toMatchObject({
      kind: "assistant",
      text: "Three issues triaged.",
      turnId: "t1",
      streaming: false,
    });
  });

  it("keeps a late assistant message on the turn that just finished after the next prompt is accepted", () => {
    const { state: completed } = play([
      { type: "turn_started", turn_id: "t1" },
      { type: "turn_completed", usage: NO_USAGE },
    ]);
    const accepted = applyAcceptedTurn(completed, {
      ...SNAPSHOT_TURN,
      id: "t2",
      ordinal: 2,
      status: "running",
      user_input: "poll for them",
      ended_at: undefined,
    });
    const late = reduceCodeSessionEvent(
      accepted,
      framed(accepted.lastSeq + 1, {
        type: "assistant_message",
        text: "Three issues triaged.",
      }),
      deps(),
    );

    expect(late.state.items.map((item) => item.kind)).toEqual([
      "assistant",
      "turn_boundary",
      "user",
    ]);
    expect(late.state.items[0]).toMatchObject({
      kind: "assistant",
      text: "Three issues triaged.",
      turnId: "t1",
    });
    expect(late.state.items[2]).toMatchObject({
      kind: "user",
      turnId: "t2",
      text: "poll for them",
    });
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
    expect(
      state.items.filter((item) => item.kind === "file_activity"),
    ).toHaveLength(1);
    expect(
      state.items.find((item) => item.kind === "file_activity"),
    ).toMatchObject({
      kind: "file_activity",
      turnId: "t1",
      files: {
        "a.ts": {
          kind: "modified",
          diffstat: {
            files: 1,
            insertions: 12,
            deletions: 3,
            truncated: false,
          },
        },
        "b.ts": {
          kind: "added",
          diffstat: {
            files: 1,
            insertions: 32,
            deletions: 5,
            truncated: false,
          },
        },
      },
    });
    expect(state.contentRevision).toBe(3);
  });
});

describe("unattributed terminals", () => {
  it("invalidates worktree readers without inventing a transcript boundary", () => {
    const terminal = reduceCodeSessionEvent(
      initialCodeSessionState(),
      framed(1, { type: "turn_completed", usage: NO_USAGE }),
      deps(),
    );

    expect(terminal.effects).toEqual([{ type: "turn_resolved" }]);
    expect(terminal.state.contentRevision).toBe(1);
    expect(
      terminal.state.items.some((item) => item.kind === "turn_boundary"),
    ).toBe(false);
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
    const accepted = applyAcceptedTurn(
      initialCodeSessionState(),
      SNAPSHOT_TURN,
    );
    expect(accepted.items[0]).toMatchObject({
      kind: "user",
      createdAt: NOW,
    });

    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      SNAPSHOT_TURN,
    ]);
    expect(hydrated.items[0]).toMatchObject({
      kind: "user",
      createdAt: NOW,
    });
  });
});

describe("reasoning lifecycle", () => {
  it("settles a reasoning block when the next call or the answer starts", () => {
    // An engine thinks between every pair of calls. Left live until the turn
    // ends, every block in the turn pulses and every one claims to be the one
    // the engine is in right now.
    const { state } = play([
      { type: "turn_started", turn_id: "t1" },
      { type: "reasoning_delta", text: "Check how fencing works." },
      {
        type: "tool_started",
        call_id: "c1",
        name: "Grep",
        detail: { kind: "search", query: "Fenced" },
      },
      {
        type: "tool_completed",
        call_id: "c1",
        outcome: "succeeded",
        preview: "recovery.rs",
      },
      { type: "reasoning_delta", text: "Now the identity we store." },
      { type: "assistant_delta", text: "Auto-reap is safe because" },
    ]);

    const reasoning = state.items.filter((item) => item.kind === "reasoning");
    expect(reasoning).toHaveLength(2);
    expect(reasoning.every((item) => item.streaming === false)).toBe(true);
  });

  it("leaves the parent's reasoning live while a subagent works", () => {
    // A subagent's call says nothing about what its parent was thinking, so it
    // must not settle a block the parent is still writing.
    const { state } = play([
      { type: "turn_started", turn_id: "t1" },
      {
        type: "tool_started",
        call_id: "task-1",
        name: "Task",
        detail: { kind: "other", summary: "Audit the parser" },
      },
      { type: "reasoning_delta", text: "While that runs, consider fencing." },
      {
        type: "tool_started",
        call_id: "child-read",
        name: "Read",
        detail: { kind: "file_read", path: "src/parser.rs" },
        parent_call_id: "task-1",
      },
    ]);

    const reasoning = state.items.find((item) => item.kind === "reasoning");
    expect(reasoning).toMatchObject({ kind: "reasoning", streaming: true });
  });
});

describe("applyTurnRewrite", () => {
  const closing = {
    kind: "assistant" as const,
    id: "a1",
    turnId: "t1",
    parentCallId: null,
    text: "The harness restated three tool calls.",
    streaming: false,
    rewrite: "The turn added three tools.",
    rewriteState: "rewritten" as const,
  };

  it("does not let a rewriting notice clear a stored rewrite", () => {
    const next = applyTurnRewrite([closing], "t1", {
      rewriteState: "rewriting",
    });
    expect(next[0]).toMatchObject({
      rewrite: "The turn added three tools.",
      rewriteState: "rewritten",
    });
  });

  it("does not let a failed notice without text clear a stored rewrite", () => {
    const next = applyTurnRewrite([closing], "t1", { rewriteState: "failed" });
    expect(next[0]).toMatchObject({
      rewrite: "The turn added three tools.",
      rewriteState: "rewritten",
    });
  });
});

describe("stored recaps", () => {
  it("stamps a stored recap after replay builds the closing message", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      { ...SNAPSHOT_TURN, rewrite: "The recap." },
    ]);
    expect(hydrated.storedRewrites.t1).toBe("The recap.");
    const { state } = play(
      [
        { type: "turn_started", turn_id: "t1" },
        { type: "assistant_delta", text: "The original closing message." },
        { type: "turn_completed", usage: NO_USAGE },
      ],
      hydrated,
    );
    const stamped = applyStoredRewrites(state);
    const assistant = stamped.items.find(
      (item) => item.kind === "assistant" && item.turnId === "t1",
    );
    expect(assistant).toMatchObject({
      kind: "assistant",
      text: "The original closing message.",
      rewrite: "The recap.",
      rewriteState: "rewritten",
    });
  });
});
