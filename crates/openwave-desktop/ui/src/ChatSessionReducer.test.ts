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
    const { state, effects } = reduceChatSessionEvent(
      initialChatSessionState(),
      framed(7, { type: "event_omitted" }),
      makeDeps(),
    );
    expect(state.lastSeq).toBe(7);
    expect(state.messages).toEqual([]);
    expect(effects).toEqual([]);
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

  it("refreshes durable question cards from the bounded question event", () => {
    const { state, effects } = play([
      {
        type: "user_questions_asked",
        call_id: "question-call",
        turn_id: "turn-1",
      },
    ]);
    expect(effects).toContainEqual({ type: "refresh_user_questions" });
    expect(state.messages).toEqual([]);
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
  it("keeps a visible partial as superseded and discards provisional tools", () => {
    const { state } = play([
      TURN,
      { type: "tool_call_started", call_id: "done", name: "search" },
      { type: "tool_call_completed", call_id: "done", status: "completed" },
      { type: "text_delta", text: "partial answer" },
      { type: "tool_call_started", call_id: "pending", name: "search" },
      { type: "stream_interrupted" },
    ]);
    const roles = state.messages.map((m) => m.role);
    expect(roles).toEqual(["assistant", "tool", "assistant"]);
    expect(state.messages[2]).toMatchObject({
      text: "partial answer",
      superseded: true,
    });
    expect(state.messages.find((m) => m.role === "tool")).toMatchObject({
      callId: "done",
    });
    expect(state.assistantBuffer).toBe("");
    expect(state.provisionalToolCallIds.size).toBe(0);
  });

  it("streams the replacement into a fresh bubble beneath the superseded one", () => {
    const { state } = play([
      TURN,
      { type: "text_delta", text: "first try" },
      { type: "stream_interrupted" },
      { type: "text_delta", text: "second try" },
    ]);
    const assistants = state.messages.filter((m) => m.role === "assistant");
    expect(assistants).toHaveLength(2);
    expect(assistants[0]).toMatchObject({
      text: "first try",
      superseded: true,
    });
    expect(assistants[1]).toMatchObject({ text: "second try" });
    expect(assistants[1]).not.toHaveProperty("superseded", true);
  });

  it("still drops an empty streaming bubble outright", () => {
    const { state } = play([TURN, { type: "stream_interrupted" }]);
    expect(state.messages).toEqual([]);
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
      { type: "refresh_user_questions" },
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
      { type: "refresh_user_questions" },
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

describe("referential stability for memoized rows", () => {
  it("text_delta preserves the identity of every settled message", () => {
    const base = play([
      TURN,
      { type: "tool_call_started", call_id: "c1", name: "search" },
      { type: "tool_call_completed", call_id: "c1", status: "completed" },
      { type: "text_delta", text: "first" },
    ]);
    const settled = base.state.messages.slice(0, -1);
    const next = reduceChatSessionEvent(
      base.state,
      framed(base.state.lastSeq + 1, { type: "text_delta", text: " second" }),
      makeDeps(),
    );
    // Only the streaming tail changed; every settled row is the same object,
    // which is what lets React.memo skip re-rendering them per token.
    next.state.messages.slice(0, -1).forEach((message, index) => {
      expect(message).toBe(settled[index]);
    });
    expect(next.state.messages[next.state.messages.length - 1]).not.toBe(
      base.state.messages[base.state.messages.length - 1],
    );
  });
});

describe("forward compatibility", () => {
  it("tolerates event types this build does not know", () => {
    const { state, effects } = reduceChatSessionEvent(
      initialChatSessionState(),
      {
        seq: 9,
        event: { type: "hologram_delta" } as unknown as AgentEvent,
      },
      makeDeps(),
    );
    expect(state.lastSeq).toBe(9);
    expect(state.messages).toEqual([]);
    expect(effects).toEqual([]);
  });
});

describe("reasoning presentation", () => {
  it("marks reasoning active and clears it when visible output starts", () => {
    const thinking = play([TURN, { type: "reasoning_delta" }]);
    expect(thinking.state.reasoningActive).toBe(true);

    const speaking = reduceChatSessionEvent(
      thinking.state,
      framed(thinking.state.lastSeq + 1, { type: "text_delta", text: "hi" }),
      makeDeps(),
    );
    expect(speaking.state.reasoningActive).toBe(false);
  });

  it("clears reasoning at tool starts, interruptions, and terminals", () => {
    for (const event of [
      { type: "tool_call_started", call_id: "c", name: "search" },
      { type: "stream_interrupted" },
      { type: "turn_completed" },
      { type: "turn_failed" },
    ] as AgentEvent[]) {
      const thinking = play([TURN, { type: "reasoning_delta" }]);
      const next = reduceChatSessionEvent(
        thinking.state,
        framed(thinking.state.lastSeq + 1, event),
        makeDeps(),
      );
      expect(next.state.reasoningActive).toBe(false);
    }
  });
});

describe("context truncation notice", () => {
  const NOTICE = "Earlier conversation was trimmed";

  it("inserts one notice above the streaming bubble and keeps the answer whole", () => {
    const { state } = play([
      TURN,
      { type: "context_truncated" },
      { type: "text_delta", text: "the answer" },
      { type: "context_truncated" },
      { type: "text_delta", text: " continues" },
    ]);
    const notices = state.messages.filter(
      (m) => m.role === "system" && m.text.includes(NOTICE),
    );
    expect(notices).toHaveLength(1);
    const last = state.messages[state.messages.length - 1];
    expect(last).toMatchObject({
      role: "assistant",
      text: "the answer continues",
    });
  });

  it("resets the once-per-turn dedup at the next turn", () => {
    const first = play([TURN, { type: "context_truncated" }]);
    const second = play(
      [
        { type: "turn_started", turn_id: "turn-2" },
        { type: "context_truncated" },
      ],
      first.state,
    );
    const notices = second.state.messages.filter(
      (m) => m.role === "system" && m.text.includes(NOTICE),
    );
    expect(notices).toHaveLength(2);
  });
});

describe("replaying an active turn over a hydrated transcript", () => {
  it("keeps the superseded partial and steer message in journal order", () => {
    // Re-entering a chat mid-turn: hydration placed the persisted messages,
    // then the journal replays the in-flight turn's events from the top.
    const hydrated = applyTerminalHydration(initialChatSessionState(), {
      messages: [
        { id: "u1", role: "user", text: "write about birds" },
        { id: "steer-1", role: "user", text: "make it volcanos" },
      ],
      messageIds: new Set(["u1", "steer-1"]),
      lastEventSeq: 0,
    });
    const { state } = play(
      [
        { type: "turn_started", turn_id: "t1" },
        { type: "text_delta", text: "Birds are great" },
        { type: "stream_interrupted" },
        { type: "user_steered", message_id: "steer-1", text: "make it volcanos" },
        { type: "text_delta", text: "Volcanoes erupt" },
      ],
      hydrated,
    );
    const order = state.messages.map((m) =>
      m.role === "assistant" ? `${m.superseded ? "superseded" : "live"}` : m.id,
    );
    expect(order).toEqual(["u1", "superseded", "steer-1", "live"]);
    const assistants = state.messages.filter((m) => m.role === "assistant");
    expect(assistants[0]).toMatchObject({ text: "Birds are great" });
    expect(assistants[1]).toMatchObject({ text: "Volcanoes erupt" });
  });
});
