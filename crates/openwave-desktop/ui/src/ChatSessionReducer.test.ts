import { describe, expect, it } from "vitest";
import type { AgentEvent, SequencedEvent } from "./api";
import {
  applyTerminalHydration,
  initialChatSessionState,
  reduceChatSessionEvent,
  type ChatSessionDeps,
  type ChatSessionState,
  type ChatSessionTransition,
} from "./ChatSessionReducer";
import type { ChatMessage } from "./MessageList";

const NOW = "2026-07-23T12:00:00.000Z";

function makeDeps(): ChatSessionDeps {
  let seq = 0;
  return {
    nextId: () => `t${++seq}`,
    now: () => NOW,
  };
}

/** Feed events in order, auto-assigning increasing seqs. */
function play(
  events: AgentEvent[],
  state: ChatSessionState = initialChatSessionState(),
  deps: ChatSessionDeps = makeDeps(),
): ChatSessionTransition {
  let transition: ChatSessionTransition = { state, effects: [] };
  events.forEach((event, index) => {
    transition = reduceChatSessionEvent(
      transition.state,
      { seq: state.lastSeq + index + 1, event },
      deps,
    );
  });
  return transition;
}

function framed(seq: number, event: AgentEvent): SequencedEvent {
  return { seq, event };
}

const TURN: AgentEvent = { type: "turn_started", turn_id: "turn-1" };

describe("seq cursor", () => {
  it("ignores duplicate and stale events entirely", () => {
    const deps = makeDeps();
    const { state } = play([TURN, { type: "text_delta", text: "Hello" }]);
    const replay = reduceChatSessionEvent(
      state,
      framed(state.lastSeq, { type: "text_delta", text: "AGAIN" }),
      deps,
    );
    expect(replay.state).toBe(state);
    expect(replay.effects).toEqual([]);
  });

  it("advances the cursor for decoded-but-unrendered events", () => {
    for (const type of [
      "reasoning_delta",
      "context_truncated",
      "event_omitted",
    ] as const) {
      const { state, effects } = reduceChatSessionEvent(
        initialChatSessionState(),
        framed(7, { type }),
        makeDeps(),
      );
      expect(state.lastSeq).toBe(7);
      expect(state.messages).toEqual([]);
      expect(effects).toEqual([]);
    }
  });
});

describe("turn_started", () => {
  it("begins a busy turn with a fresh assistant bubble and reset stream state", () => {
    const { state, effects } = play([TURN]);
    expect(state.busy).toBe(true);
    expect(state.activeTurnId).toBe("turn-1");
    expect(state.messages).toEqual([
      { id: "t1", role: "assistant", text: "", sources: [], createdAt: NOW },
    ]);
    expect(effects).toEqual([
      { type: "refresh_agent_runs" },
      { type: "invalidate_terminal_hydration" },
      { type: "turn_began", turnId: "turn-1", startsDifferentTurn: true },
    ]);
  });

  it("does not report a different turn when the id matches the active one", () => {
    const first = play([TURN]);
    const again = reduceChatSessionEvent(
      first.state,
      framed(first.state.lastSeq + 1, TURN),
      makeDeps(),
    );
    expect(again.effects).toContainEqual({
      type: "turn_began",
      turnId: "turn-1",
      startsDifferentTurn: false,
    });
  });
});

describe("text_delta", () => {
  it("accumulates scrubbed text into the trailing assistant bubble", () => {
    const { state } = play([
      TURN,
      { type: "text_delta", text: "Hel" },
      { type: "text_delta", text: "lo" },
    ]);
    const last = state.messages[state.messages.length - 1];
    expect(last).toMatchObject({ role: "assistant", text: "Hello" });
    expect(state.messages).toHaveLength(1);
  });

  it("starts a new assistant bubble when the transcript ends elsewhere", () => {
    const { state } = play([{ type: "text_delta", text: "orphan" }]);
    expect(state.messages).toEqual([
      {
        id: "t1",
        role: "assistant",
        text: "orphan",
        sources: [],
        createdAt: NOW,
      },
    ]);
  });

  it("withholds a partial source marker until the stream settles", () => {
    const marker = "[[ow-source:0123456789abcdef0123456789abcdef]]";
    const split = 10;
    const mid = play([
      TURN,
      { type: "text_delta", text: `Answer ${marker.slice(0, split)}` },
    ]);
    const lastMid = mid.state.messages[mid.state.messages.length - 1];
    expect(lastMid).toMatchObject({ role: "assistant", text: "Answer " });

    const done = reduceChatSessionEvent(
      mid.state,
      framed(mid.state.lastSeq + 1, {
        type: "text_delta",
        text: marker.slice(split),
      }),
      makeDeps(),
    );
    const lastDone = done.state.messages[done.state.messages.length - 1];
    expect(lastDone).toMatchObject({ role: "assistant", text: "Answer " });
  });

  it("flushes a withheld non-marker tail at a terminal boundary", () => {
    const mid = play([TURN, { type: "text_delta", text: "Answer [[ow-sour" }]);
    const done = reduceChatSessionEvent(
      mid.state,
      framed(mid.state.lastSeq + 1, { type: "turn_completed" }),
      makeDeps(),
    );
    const last = done.state.messages[done.state.messages.length - 1];
    expect(last).toMatchObject({ role: "assistant", text: "Answer [[ow-sour" });
  });
});

describe("tool call lifecycle", () => {
  const START_SEARCH: AgentEvent = {
    type: "tool_call_started",
    call_id: "call-1",
    name: "search",
  };

  it("upserts a running tool card and marks it provisional", () => {
    const { state } = play([TURN, START_SEARCH]);
    expect(state.messages).toContainEqual({
      id: "t2",
      role: "tool",
      callId: "call-1",
      name: "search",
      status: "running",
    });
    expect(state.provisionalToolCallIds.has("call-1")).toBe(true);
  });

  it("requests a folder-access refresh for that specific tool", () => {
    const { effects } = play([
      TURN,
      { type: "tool_call_started", call_id: "c", name: "request_folder_access" },
    ]);
    expect(effects).toContainEqual({ type: "refresh_folder_access" });
  });

  it("keeps args streaming from downgrading an approval wait", () => {
    const { state } = play([
      TURN,
      START_SEARCH,
      {
        type: "approval_required",
        call_id: "call-1",
        action: "search",
        approval: "search_may_share_query_and_excerpts",
        class: "sensitive",
      },
      { type: "tool_call_args_delta", call_id: "call-1" },
    ]);
    const tool = state.messages.find((m) => m.role === "tool");
    expect(tool).toMatchObject({ status: "waiting_approval" });
  });

  it("completes, fails, and keeps cancellation sticky", () => {
    const completed = play([
      TURN,
      START_SEARCH,
      { type: "tool_call_completed", call_id: "call-1", status: "completed" },
    ]);
    expect(completed.state.messages.find((m) => m.role === "tool")).toMatchObject(
      { status: "completed" },
    );
    expect(completed.state.provisionalToolCallIds.size).toBe(0);
    expect(completed.effects).toContainEqual({ type: "refresh_agent_runs" });

    const rejectedThenCompleted = play([
      TURN,
      START_SEARCH,
      {
        type: "approval_required",
        call_id: "call-1",
        action: "search",
        approval: "search_may_share_query_and_excerpts",
        class: "sensitive",
      },
      { type: "approval_decided", call_id: "call-1", approved: false },
      { type: "tool_call_completed", call_id: "call-1", status: "completed" },
    ]);
    expect(
      rejectedThenCompleted.state.messages.find((m) => m.role === "tool"),
    ).toMatchObject({ status: "cancelled" });
  });
});

describe("approvals", () => {
  const APPROVAL: AgentEvent = {
    type: "approval_required",
    call_id: "call-1",
    action: "search",
    approval: "search_may_share_query_and_excerpts",
    class: "sensitive",
  };

  it("adds an approvable card and releases the call from provisional", () => {
    const { state } = play([
      TURN,
      { type: "tool_call_started", call_id: "call-1", name: "search" },
      APPROVAL,
    ]);
    const card = state.messages.find((m) => m.role === "approval");
    expect(card).toMatchObject({ callId: "call-1", canApprove: true });
    expect(state.provisionalToolCallIds.has("call-1")).toBe(false);
  });

  it("resolves the card and resumes or cancels the tool on decision", () => {
    const approved = play([
      TURN,
      { type: "tool_call_started", call_id: "call-1", name: "search" },
      APPROVAL,
      { type: "approval_decided", call_id: "call-1", approved: true },
    ]);
    expect(approved.state.messages.find((m) => m.role === "approval")).toMatchObject(
      { resolved: true },
    );
    expect(approved.state.messages.find((m) => m.role === "tool")).toMatchObject(
      { status: "running" },
    );

    const rejected = play([
      TURN,
      { type: "tool_call_started", call_id: "call-1", name: "search" },
      APPROVAL,
      { type: "approval_decided", call_id: "call-1", approved: false },
    ]);
    expect(rejected.state.messages.find((m) => m.role === "tool")).toMatchObject(
      { status: "cancelled" },
    );
  });
});

describe("stream_interrupted", () => {
  it("discards the optimistic candidate but keeps settled work", () => {
    const { state } = play([
      TURN,
      { type: "tool_call_started", call_id: "done", name: "search" },
      { type: "tool_call_completed", call_id: "done", status: "completed" },
      { type: "text_delta", text: "partial answer" },
      { type: "tool_call_started", call_id: "pending", name: "search" },
      { type: "stream_interrupted" },
    ]);
    // The empty bubble minted at turn start survives (it renders as nothing);
    // the streamed partial answer and the pending tool card are discarded.
    const roles = state.messages.map((m) => m.role);
    expect(roles).toEqual(["assistant", "tool"]);
    expect(state.messages[0]).toMatchObject({ role: "assistant", text: "" });
    expect(state.messages[1]).toMatchObject({ callId: "done" });
    expect(state.assistantBuffer).toBe("");
    expect(state.provisionalToolCallIds.size).toBe(0);
  });
});

describe("user_steered", () => {
  it("appends the steered message once, deduping hydrated echoes", () => {
    const steer: AgentEvent = {
      type: "user_steered",
      message_id: "srv-1",
      text: "take a left",
    };
    const first = play([TURN, steer]);
    expect(first.state.messages).toContainEqual({
      id: "srv-1",
      role: "user",
      text: "take a left",
      createdAt: NOW,
    });
    const replayed = reduceChatSessionEvent(
      first.state,
      framed(first.state.lastSeq + 1, steer),
      makeDeps(),
    );
    expect(
      replayed.state.messages.filter((m) => m.id === "srv-1"),
    ).toHaveLength(1);
  });
});

describe("terminal events", () => {
  it("turn_completed resolves the turn and requests hydration", () => {
    const { state, effects } = play([TURN, { type: "turn_completed" }]);
    expect(state.busy).toBe(false);
    expect(state.activeTurnId).toBeNull();
    expect(effects).toEqual([
      { type: "turn_resolved" },
      { type: "refresh_agent_runs" },
      { type: "hydrate_terminal_transcript" },
    ]);
  });

  it("turn_cancelled settles active tools, resolves their cards, and notes it", () => {
    const { state, effects } = play([
      TURN,
      { type: "tool_call_started", call_id: "call-1", name: "search" },
      {
        type: "approval_required",
        call_id: "call-1",
        action: "search",
        approval: "search_may_share_query_and_excerpts",
        class: "sensitive",
      },
      { type: "turn_cancelled" },
    ]);
    expect(state.busy).toBe(false);
    expect(state.messages.find((m) => m.role === "tool")).toMatchObject({
      status: "cancelled",
    });
    expect(state.messages.find((m) => m.role === "approval")).toMatchObject({
      resolved: true,
    });
    expect(state.messages[state.messages.length - 1]).toMatchObject({
      role: "system",
      text: "turn cancelled",
    });
    expect(effects).toEqual([
      { type: "invalidate_terminal_hydration" },
      { type: "turn_resolved" },
      { type: "refresh_agent_runs" },
    ]);
  });

  it("turn_failed settles active tools as failed and appends the error bubble", () => {
    const { state } = play([
      TURN,
      { type: "tool_call_started", call_id: "call-1", name: "search" },
      { type: "turn_failed" },
    ]);
    expect(state.messages.find((m) => m.role === "tool")).toMatchObject({
      status: "failed",
    });
    expect(state.messages[state.messages.length - 1]).toMatchObject({
      role: "error",
      text: "The turn could not be completed.",
    });
  });

  it("terminal settling leaves already-finished tools untouched", () => {
    const { state } = play([
      TURN,
      { type: "tool_call_started", call_id: "done", name: "search" },
      { type: "tool_call_completed", call_id: "done", status: "completed" },
      { type: "turn_failed" },
    ]);
    expect(state.messages.find((m) => m.role === "tool")).toMatchObject({
      status: "completed",
    });
  });
});

describe("applyTerminalHydration", () => {
  it("replaces the transcript and only moves the seq cursor forward", () => {
    const authoritative: ChatMessage[] = [
      { id: "srv-1", role: "user", text: "hi", createdAt: NOW },
    ];
    const base = play([TURN]).state;
    const behind = applyTerminalHydration(base, {
      messages: authoritative,
      messageIds: new Set(["srv-1"]),
      lastEventSeq: 0,
    });
    expect(behind.lastSeq).toBe(base.lastSeq);
    expect(behind.messages).toEqual(authoritative);
    expect(behind.hydratedMessageIds.has("srv-1")).toBe(true);

    const ahead = applyTerminalHydration(base, {
      messages: authoritative,
      messageIds: new Set(["srv-1"]),
      lastEventSeq: 99,
    });
    expect(ahead.lastSeq).toBe(99);
  });
});
