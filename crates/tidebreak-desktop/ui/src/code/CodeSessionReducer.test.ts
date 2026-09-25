import { describe, expect, it, vi } from "vitest";
import type { CodeEvent, SequencedCodeEventFrame } from "../api/types";
import {
  actorLabel,
  applyAcceptedTurn,
  applyCodeTurnSnapshot,
  applySessionTreeSnapshot,
  applyStoredRewrites,
  applyTurnRewrite,
  hydrateCodeTurns,
  initialCodeSessionState,
  mainAgentTranscriptItems,
  markCodeSessionHydrated,
  reconcilePendingCodeTurns,
  reduceCodeSessionEvent,
  subagentTranscriptItems,
  userItemId,
  type CodeSessionDeps,
  type CodeSessionState,
  credentialRefusalNotice,
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

  it("appends a capped-window notice after retained history on reconnect", () => {
    const existing = play([
      { type: "turn_started", turn_id: "t1" },
      { type: "assistant_delta", text: "Existing history" },
    ]).state;
    const reconnected = reduceCodeSessionEvent(
      existing,
      {
        seq: existing.lastSeq + 1,
        event: { type: "turn_started", turn_id: "t2" },
        replayed: true,
        truncated: true,
      },
      deps(),
    );

    expect(reconnected.state.items.at(-1)).toMatchObject({
      id: "notice:truncated-replay",
      kind: "notice",
    });
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

  it("paints a refused credential borrow as a warning that says its remedy once", () => {
    const { state } = play([
      { type: "turn_started", turn_id: "t1" },
      {
        type: "credential_refused",
        reason: "connection_ended",
        message:
          "this external connection has no live gateway delegation; reconnect it from Slack",
        remediation:
          "Reconnect this session from Slack; a newer connect or a revoke ended the one it used.",
      },
      { type: "turn_completed", usage: NO_USAGE },
    ]);
    const notice = state.items.find((item) => item.kind === "notice");
    expect(notice).toMatchObject({ kind: "notice", level: "warning" });
    const message = (notice as { message: string }).message;
    expect(message).toBe(
      "Push refused: this session's Slack connection ended when it was revoked or replaced by a newer one. Reconnect the session from Slack, then push again.",
    );
    expect(message.match(/reconnect/gi)).toHaveLength(1);
    expect(message).not.toMatch(/delegation/);
  });

  it("keeps the link a not-connected refusal carries, and the reason a forge gave", () => {
    expect(
      credentialRefusalNotice({
        reason: "not_connected",
        message:
          "To use GitHub as yourself here, connect your GitHub account at the Model Gateway: https://gateway.example.com/connect",
        remediation:
          "Connect your GitHub account at the gateway, then push again.",
      }),
    ).toBe(
      "Push refused. To use GitHub as yourself here, connect your GitHub account at the Model Gateway: https://gateway.example.com/connect. Then push again.",
    );
    expect(
      credentialRefusalNotice({
        reason: "forge_refused",
        message: "The installation is suspended",
        remediation:
          "Read the reason above; if it names the deployment, ask an administrator.",
      }),
    ).toBe(
      "Push refused: The installation is suspended. If this names the deployment, ask an administrator.",
    );
  });

  it("closes a refused turn as completed with its usage", () => {
    const { state } = play([
      { type: "turn_started", turn_id: "t1" },
      {
        type: "turn_refused",
        usage: NO_USAGE,
        refusal: { details: { category: "cyber" }, partial_output: false },
      },
    ]);
    expect(state.busy).toBe(false);
    expect(state.items.at(-1)).toMatchObject({
      kind: "turn_boundary",
      turnId: "t1",
      status: "completed",
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
  it("puts a replayed restore after the turn before it, not after later prompts", () => {
    // Hydration lays down every prompt and boundary before the journal
    // replays, so a restore that ran between turns 1 and 2 must land ahead
    // of turn 2's prompt rather than at the end of the transcript.
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      SNAPSHOT_TURN,
      { ...SNAPSHOT_TURN, id: "t2", ordinal: 2, user_input: "try again" },
    ]);
    const restore: CodeEvent = {
      type: "checkpoint_restored",
      restore_id: "r-1",
      target: { kind: "before_turn", turn_id: "t1" },
      diffstat: { files: 2, insertions: 1, deletions: 9, truncated: false },
      status: "completed",
    };
    let state = hydrated;
    const replay: CodeEvent[] = [
      { type: "turn_started", turn_id: "t1" },
      { type: "turn_completed", usage: NO_USAGE },
      restore,
      { type: "turn_started", turn_id: "t2" },
      { type: "turn_completed", usage: NO_USAGE },
    ];
    replay.forEach((event, index) => {
      state = reduceCodeSessionEvent(
        state,
        framed(index + 1, event, true),
        deps(),
      ).state;
    });

    expect(state.items.map((item) => item.kind)).toEqual([
      "user",
      "turn_boundary",
      "restore",
      "user",
      "turn_boundary",
    ]);
    expect(state.items[2]).toMatchObject({
      kind: "restore",
      restoreId: "r-1",
      turnOrdinal: 1,
    });

    // The same restore arriving again, on a reconnect, adds nothing.
    const again = reduceCodeSessionEvent(state, framed(9, restore), deps());
    expect(
      again.state.items.filter((item) => item.kind === "restore"),
    ).toHaveLength(1);
    expect(again.state.contentRevision).toBe(state.contentRevision);
  });

  it("records a review between turns once, naming the turn it read", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      SNAPSHOT_TURN,
      { ...SNAPSHOT_TURN, id: "t2", ordinal: 2, user_input: "try again" },
    ]);
    const review: CodeEvent = {
      type: "review_finished",
      review_id: "rev-1",
      harness: "codex",
      model: "gpt-5.5",
      turn_id: "t1",
      outcome: "completed",
      findings: 3,
    };
    let state = hydrated;
    const replay: CodeEvent[] = [
      { type: "turn_started", turn_id: "t1" },
      { type: "turn_completed", usage: NO_USAGE },
      review,
      { type: "turn_started", turn_id: "t2" },
      { type: "turn_completed", usage: NO_USAGE },
    ];
    replay.forEach((event, index) => {
      state = reduceCodeSessionEvent(
        state,
        framed(index + 1, event, true),
        deps(),
      ).state;
    });
    expect(state.items.map((item) => item.kind)).toEqual([
      "user",
      "turn_boundary",
      "review",
      "user",
      "turn_boundary",
    ]);
    expect(state.items[2]).toMatchObject({
      kind: "review",
      reviewId: "rev-1",
      harness: "codex",
      model: "gpt-5.5",
      turnOrdinal: 1,
      outcome: "completed",
      findings: 3,
    });
    const again = reduceCodeSessionEvent(state, framed(9, review), deps());
    expect(
      again.state.items.filter((item) => item.kind === "review"),
    ).toHaveLength(1);
  });

  it("updates a restore row in place when the restore ends", () => {
    const started: CodeEvent = {
      type: "checkpoint_restored",
      restore_id: "r-2",
      target: { kind: "before_turn", turn_id: "t1" },
      diffstat: { files: 1, insertions: 0, deletions: 3, truncated: false },
      status: "started",
    };
    let state = hydrateCodeTurns(initialCodeSessionState(), [SNAPSHOT_TURN]);
    state = reduceCodeSessionEvent(state, framed(1, started), deps()).state;
    state = reduceCodeSessionEvent(
      state,
      framed(2, { ...started, status: "partial", error: "disk full" }),
      deps(),
    ).state;

    const rows = state.items.filter((item) => item.kind === "restore");
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ status: "partial", error: "disk full" });
  });

  it("carries a trigger's structured event onto the user item", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      {
        ...SNAPSHOT_TURN,
        user_input: "Tidebreak trigger: checks failed on #3411.",
        actor: {
          principal: null,
          display: "Trigger: checks_failed",
          channel_kind: null,
          external_identity: null,
          trigger: {
            source: "trigger" as const,
            condition: "checks_failed" as const,
            pr_number: 3411,
          },
        },
      },
    ]);
    expect(hydrated.items[0]).toMatchObject({
      kind: "user",
      actorLabel: "Trigger: checks_failed",
      trigger: { condition: "checks_failed", pr_number: 3411 },
    });
  });

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

    const reconciled = reconcilePendingCodeTurns(completed.state, [
      {
        ...SNAPSHOT_TURN,
        started_at: LONG_TURN_START,
        ended_at: LONG_TURN_END,
      },
    ]);
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

    const reconciled = reconcilePendingCodeTurns(t2Started.state, [
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
        user_input: "run the tests",
        ended_at: undefined,
      },
    ]);

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

  it("keeps a parked turn busy: waiting is open, not terminal", () => {
    const waiting = hydrateCodeTurns(initialCodeSessionState(), [
      { ...SNAPSHOT_TURN, status: "waiting", ended_at: undefined },
    ]);
    expect(waiting).toMatchObject({
      busy: true,
      activeTurnId: SNAPSHOT_TURN.id,
      lifecycle: "running",
    });
    expect(
      waiting.items.find((item) => item.kind === "turn_boundary"),
    ).toBeUndefined();
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

  it("returns the original items when the rewrite is already current", () => {
    const items = [closing];
    expect(
      applyTurnRewrite(items, "t1", {
        rewrite: "The turn added three tools.",
        rewriteState: "rewritten",
      }),
    ).toBe(items);
  });

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

describe("actorLabel", () => {
  const PRINCIPAL = "user:019fc880-8c55-7c31-8d5c-980c6a98783a";

  it("renders a display name as-is, even when everything else is set", () => {
    expect(
      actorLabel({
        principal: PRINCIPAL,
        display: "Ada Lovelace",
        channel_kind: "slack",
        external_identity: "U123",
      }),
    ).toBe("Ada Lovelace");
  });

  it("renders a generic origin label when only the channel kind is known", () => {
    expect(
      actorLabel({
        principal: PRINCIPAL,
        display: null,
        channel_kind: "slack",
        external_identity: "U123",
      }),
    ).toBe("Slack user");
  });

  it("never renders the principal: a principal-only actor has no label", () => {
    expect(
      actorLabel({
        principal: PRINCIPAL,
        display: null,
        channel_kind: null,
        external_identity: null,
      }),
    ).toBeUndefined();
  });

  it("treats empty strings as absent", () => {
    expect(
      actorLabel({
        principal: PRINCIPAL,
        display: "",
        channel_kind: "",
        external_identity: null,
      }),
    ).toBeUndefined();
  });

  it("returns undefined for a missing actor", () => {
    expect(actorLabel(null)).toBeUndefined();
    expect(actorLabel(undefined)).toBeUndefined();
  });
});

describe("session tree", () => {
  const child = {
    id: "child-1",
    title: "Inspect the parser",
    status: "running" as const,
    attention: false,
    fenced: false,
    workspace_id: "ws-child",
    execution_location: "machine" as const,
  };

  it("keeps snapshot trees until a live session_tree replaces them", () => {
    const hydrated = applySessionTreeSnapshot(initialCodeSessionState(), {
      children: [child],
      wait: { waiting: 1, total: 2 },
    });
    expect(hydrated.children).toEqual([child]);
    expect(hydrated.wait).toEqual({ waiting: 1, total: 2 });
    const live = reduceCodeSessionEvent(
      hydrated,
      framed(1, {
        type: "session_tree",
        children: [{ ...child, status: "completed" }],
        wait: null,
      }),
      deps(),
    );
    expect(live.state.children[0]?.status).toBe("completed");
    expect(live.state.wait).toBeNull();
  });

  it("clears a wait without inferring one from running children", () => {
    const waiting = applySessionTreeSnapshot(initialCodeSessionState(), {
      children: [child],
      wait: { waiting: 1, total: 1 },
    });
    const cleared = reduceCodeSessionEvent(
      waiting,
      framed(1, {
        type: "session_tree",
        children: [child],
        wait: null,
      }),
      deps(),
    );
    expect(cleared.state.children).toHaveLength(1);
    expect(cleared.state.wait).toBeNull();
  });
});

describe("background activity", () => {
  const own = (event: CodeEvent): CodeEvent => ({
    type: "background_activity",
    event,
  });

  /** The transcript as kind, turn, and text, in order. */
  function outline(state: CodeSessionState) {
    return state.items.map((item) => {
      switch (item.kind) {
        case "user":
          return `user ${item.turnId}: ${item.text}`;
        case "assistant":
          return `${item.background ? "own" : `reply ${item.turnId}`}: ${item.text}`;
        case "tool":
          return `${item.background ? "own tool" : `tool ${item.turnId}`}: ${item.callId} ${item.status}`;
        case "notice":
          return `notice: ${item.message}`;
        case "turn_boundary":
          return `end ${item.turnId}`;
        default:
          return item.kind;
      }
    });
  }

  const SECOND_TURN = {
    ...SNAPSHOT_TURN,
    id: "t2",
    ordinal: 2,
    user_input: "Say the word done.",
  };

  it("keeps the engine's answer out of the reply of a turn that waits for it", () => {
    const clock = deps();
    const first = play(
      [
        { type: "turn_started", turn_id: "t1" },
        { type: "assistant_message", text: "Started the background job." },
        { type: "turn_completed", usage: NO_USAGE },
        own({
          type: "harness_notice",
          level: "info",
          message: "Background command completed",
        }),
      ],
      applyAcceptedTurn(initialCodeSessionState(), {
        ...SNAPSHOT_TURN,
        status: "running",
        ended_at: undefined,
      }),
      clock,
    );
    // The person sends while the engine runs its own turn.
    const waiting = applyAcceptedTurn(first.state, {
      ...SECOND_TURN,
      status: "running",
      ended_at: undefined,
    });
    const { state } = play(
      [
        { type: "turn_started", turn_id: "t2" },
        own({
          type: "assistant_message",
          text: "The background job finished.",
        }),
        { type: "assistant_delta", text: "do" },
        { type: "assistant_delta", text: "ne." },
        { type: "assistant_message", text: "done." },
        { type: "turn_completed", usage: NO_USAGE },
      ],
      waiting,
      clock,
    );

    expect(outline(state)).toEqual([
      "user t1: list the files",
      "reply t1: Started the background job.",
      "end t1",
      "notice: Background command completed",
      "own: The background job finished.",
      "user t2: Say the word done.",
      "reply t2: done.",
      "end t2",
    ]);
  });

  it("places the engine's own work the same way after a reload", () => {
    const hydrated = hydrateCodeTurns(initialCodeSessionState(), [
      SNAPSHOT_TURN,
      SECOND_TURN,
    ]);
    const events: CodeEvent[] = [
      { type: "turn_started", turn_id: "t1" },
      { type: "assistant_message", text: "Started the background job." },
      { type: "turn_completed", usage: NO_USAGE },
      own({
        type: "harness_notice",
        level: "info",
        message: "Background command completed",
      }),
      { type: "turn_started", turn_id: "t2" },
      own({ type: "assistant_message", text: "The background job finished." }),
      { type: "assistant_message", text: "done." },
      { type: "turn_completed", usage: NO_USAGE },
    ];
    let state = hydrated;
    events.forEach((event, index) => {
      state = reduceCodeSessionEvent(
        state,
        framed(index + 1, event, true),
        deps(),
      ).state;
    });

    expect(outline(state)).toEqual([
      "user t1: list the files",
      "reply t1: Started the background job.",
      "end t1",
      "notice: Background command completed",
      "own: The background job finished.",
      "user t2: Say the word done.",
      "reply t2: done.",
      "end t2",
    ]);
  });

  it("settles a call the engine ran on its own and marks the worktree changed", () => {
    const { state } = play([
      { type: "turn_started", turn_id: "t1" },
      { type: "turn_completed", usage: NO_USAGE },
      own({
        type: "tool_started",
        call_id: "toolu_own",
        name: "Bash",
        detail: { kind: "command", cmd: "cat job.log", cwd: "" },
      }),
    ]);
    expect(outline(state).at(-1)).toBe("own tool: toolu_own running");
    const settled = reduceCodeSessionEvent(
      state,
      framed(
        state.lastSeq + 1,
        own({
          type: "tool_completed",
          call_id: "toolu_own",
          outcome: "failed",
          preview: "Claude Code stopped before this call finished.",
        }),
      ),
      deps(),
    ).state;
    expect(outline(settled).at(-1)).toBe("own tool: toolu_own failed");
    expect(settled.contentRevision).toBe(state.contentRevision + 1);
    expect(settled.busy).toBe(false);
  });
});
