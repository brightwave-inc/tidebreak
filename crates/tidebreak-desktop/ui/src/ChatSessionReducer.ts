import type { SequencedEvent } from "./api";
import { parseToolActionPreview, parseToolResultPreview } from "./api";
import { contextTruncationNotice } from "./ContextUsage";
import type { RendererTurnUsage } from "./generated/wire";
import { AssistantSourceMarkerStreamScrubber } from "./AssistantSourceMarkerStream";
import type { ChatMessage } from "./MessageList";
import { TURN_CANCELLED_NOTICE } from "./MessageList";
import type { ToolCallStatus } from "./ToolCallCard";
import { toolApprovalPresentation } from "./ToolCallCard";
import { upsertPendingApprovalCard } from "./ApprovalHistory";
import { PRESENTATION_MEDIA_TYPES } from "./document/officePdf";

/**
 * The pure state machine for one chat session's live event stream.
 *
 * Everything the stream mutates lives here so the transition logic is testable
 * without React: the transcript, the streaming buffer and its marker scrubber,
 * the seq cursor, and the bookkeeping sets. Side requirements that belong to
 * the host component (polling refreshes, terminal-transcript hydration,
 * cancel/steer cleanup) are returned as declarative [`ChatSessionEffect`]s.
 *
 * Purity caveat: the marker scrubber is a stateful stream processor, so a
 * transition may advance the scrubber held by its *input* state. Treat every
 * state value as consumed by the transition that receives it — never reduce
 * from a stale snapshot. Reset points always install a fresh scrubber.
 */
export type ChatSessionState = {
  /** Highest event seq applied; events at or below it are duplicates. */
  lastSeq: number;
  /** Whether new stream presentation should animate rather than catch up. */
  animateStreaming: boolean;
  messages: ChatMessage[];
  busy: boolean;
  activeTurnId: string | null;
  /** Scrubbed text accumulated for the trailing assistant bubble. */
  assistantBuffer: string;
  /**
   * Reasoning accumulated for the open turn. The snapshot stores this on the
   * turn's output message, so the live stream keeps one copy and paints it
   * on the latest live assistant bubble instead of opening a new thought
   * accordion after every tool batch.
   */
  reasoningBuffer: string;
  markerScrubber: AssistantSourceMarkerStreamScrubber;
  /** Tool calls streamed in the current step; discarded on interruption. */
  provisionalToolCallIds: ReadonlySet<string>;
  /** Message ids already present via hydration; used to dedup steered echoes. */
  hydratedMessageIds: ReadonlySet<string>;
  /** One truncation notice per turn, however many truncation events arrive. */
  contextTruncationNoted: boolean;
  /**
   * Token counts from the most recent terminal turn, or null before any turn
   * in this chat has finished.
   *
   * The composer's context meter reads this. It is deliberately the last
   * turn's figures rather than a running total: each turn re-sends the
   * conversation, so the latest turn is the best available account of what the
   * window is holding, and summing turns would just count the transcript once
   * per turn.
   */
  lastTurnUsage: RendererTurnUsage | null;
  /**
   * Whether code execution is preparing its sandbox image right now.
   *
   * Pushed outside the journal, so it is not reduced from an event and never
   * survives a reload — which is correct: it describes a wait that is either
   * happening or over.
   */
  sandboxPreparing: boolean;
  /**
   * Semantic compaction is running for the open turn, on the conversation's
   * own model and route.
   * Cleared on finished or any turn-terminal event so it never sticks.
   */
  compacting: boolean;
};

export type ChatSessionEffect =
  | { type: "refresh_folder_access" }
  | { type: "refresh_output_writebacks" }
  | { type: "refresh_user_questions" }
  | { type: "refresh_plan_approvals" }
  /**
   * The agent replaced its task plan. The event carries no steps — it is a
   * hint that the chat's durable plan moved on, and the panel re-reads it.
   */
  | { type: "refresh_task_plan" }
  /** A turn began; the host resets cancel state (and steer state when asked). */
  | { type: "turn_began"; turnId: string; startsDifferentTurn: boolean }
  /** A turn reached a terminal event; the host clears cancel/steer state. */
  | { type: "turn_resolved" }
  /** Replace the optimistic transcript with the authoritative one. */
  | { type: "hydrate_terminal_transcript" }
  /** A terminal boundary passed without hydration; stale loads must not land. */
  | { type: "invalidate_terminal_hydration" }
  /**
   * A turn just produced its first presentation output; start the managed
   * LibreOffice download now so the first preview click finds it ready (or at
   * least under way) instead of starting a 300 MB fetch on demand.
   */
  | { type: "warm_presentation_converter" };

export type ChatSessionTransition = {
  state: ChatSessionState;
  effects: ChatSessionEffect[];
};

/** Injected id/clock dependencies so transitions are deterministic in tests. */
export type ChatSessionDeps = {
  nextId: () => string;
  now: () => string;
};

export function initialChatSessionState(): ChatSessionState {
  return {
    lastSeq: 0,
    animateStreaming: true,
    messages: [],
    busy: false,
    activeTurnId: null,
    assistantBuffer: "",
    reasoningBuffer: "",
    markerScrubber: new AssistantSourceMarkerStreamScrubber(),
    provisionalToolCallIds: new Set(),
    hydratedMessageIds: new Set(),
    contextTruncationNoted: false,
    lastTurnUsage: null,
    sandboxPreparing: false,
    compacting: false,
  };
}

export function reduceChatSessionEvent(
  state: ChatSessionState,
  framed: SequencedEvent,
  deps: ChatSessionDeps,
): ChatSessionTransition {
  if (framed.seq <= state.lastSeq) return { state, effects: [] };
  state = {
    ...state,
    lastSeq: framed.seq,
    // Replayed journal frames rebuild the active turn after navigation or a
    // reconnect. They are current state, not new activity, so only the first
    // genuinely live frame re-enables the presentation typewriters.
    animateStreaming: framed.replayed !== true,
  };
  const event = framed.event;
  const effects: ChatSessionEffect[] = [];

  switch (event.type) {
    case "turn_started": {
      // A resumed attempt starts a new renderer candidate for the same turn.
      // Calls admitted before an approval boundary have already left this set;
      // any IDs still here were only streamed optimistically and will never
      // receive completion events from the resumed attempt. Sandbox-spawn
      // siblings are the exception: the server checkpoints them one at a time,
      // and each resume still owes completion events for the carried tail.
      const messages = discardUnadmittedToolCallsAtResume(
        state.messages,
        state.provisionalToolCallIds,
      );
      effects.push(
        { type: "invalidate_terminal_hydration" },
        {
          type: "turn_began",
          turnId: event.turn_id,
          startsDifferentTurn: state.activeTurnId !== event.turn_id,
        },
      );
      return {
        state: {
          ...state,
          busy: true,
          activeTurnId: event.turn_id,
          // A compaction whose finish event never arrived (disconnect, replay
          // gap) must not label the next turn as compacting.
          compacting: false,
          contextTruncationNoted: false,
          assistantBuffer: "",
          reasoningBuffer: "",
          markerScrubber: new AssistantSourceMarkerStreamScrubber(),
          provisionalToolCallIds: new Set(),
          messages: [
            ...messages,
            {
              id: deps.nextId(),
              role: "assistant",
              text: "",
              sources: [],
              createdAt: deps.now(),
            },
          ],
        },
        effects,
      };
    }

    case "text_delta": {
      const assistantBuffer =
        state.assistantBuffer + state.markerScrubber.push(event.text);
      return {
        state: {
          ...state,
          assistantBuffer,
          messages: paintTurnReasoning(
            withTrailingAssistantText(
              state.messages,
              assistantBuffer,
              deps,
            ),
            state.reasoningBuffer,
            deps,
          ),
        },
        effects,
      };
    }

    case "stream_interrupted": {
      // The whole optimistic candidate is invalidated at this boundary. Finish
      // clears any withheld marker-like tail before the replacement starts.
      state.markerScrubber.finish();
      const messages = discardToolCalls(
        state.messages,
        state.provisionalToolCallIds,
      );
      // A visible partial stays on screen as superseded — dimmed until the
      // replacement streams beneath it and the authoritative transcript
      // sweeps it at turn completion. Empty bubbles just drop.
      const last = messages[messages.length - 1];
      if (last?.role === "assistant") {
        if (last.text) {
          messages[messages.length - 1] = { ...last, superseded: true };
        } else {
          messages.pop();
        }
      }
      return {
        state: {
          ...state,
          assistantBuffer: "",
          reasoningBuffer: "",
          provisionalToolCallIds: new Set(),
          messages,
        },
        effects,
      };
    }

    case "tool_call_started": {
      state = flushMarkerTail(state, deps);
      if (state.provisionalToolCallIds.size === 0) {
        state = {
          ...state,
          assistantBuffer: "",
          markerScrubber: new AssistantSourceMarkerStreamScrubber(),
        };
      }
      if (event.name === "request_folder_access") {
        effects.push({ type: "refresh_folder_access" });
      }
      if (event.name === "write_output_to_connected_folder") {
        effects.push({ type: "refresh_output_writebacks" });
      }
      const provisionalToolCallIds = new Set(state.provisionalToolCallIds);
      provisionalToolCallIds.add(event.call_id);
      return {
        state: {
          ...state,
          provisionalToolCallIds,
          messages: upsertToolCall(
            state.messages,
            event.call_id,
            event.name,
            "running",
            deps,
          ),
        },
        effects,
      };
    }

    case "tool_call_args_delta": {
      // Arguments are intentionally not retained in renderer state. They can
      // contain paths, file content, credentials, or provider-specific data.
      return {
        state: {
          ...state,
          messages: updateToolCall(state.messages, event.call_id, (tool) => ({
            ...tool,
            status:
              tool.status === "waiting_approval" ? tool.status : "running",
          })),
        },
        effects,
      };
    }

    case "user_questions_asked": {
      effects.push({ type: "refresh_user_questions" });
      return { state, effects };
    }

    case "plan_proposed": {
      effects.push({ type: "refresh_plan_approvals" });
      return { state, effects };
    }

    case "task_plan_updated": {
      effects.push({ type: "refresh_task_plan" });
      return { state, effects };
    }

    case "approval_required": {
      const approval = toolApprovalPresentation(event.approval);
      const provisionalToolCallIds = new Set(state.provisionalToolCallIds);
      provisionalToolCallIds.delete(event.call_id);
      return {
        state: {
          ...state,
          provisionalToolCallIds,
          messages: upsertPendingApprovalCard(
            state.messages,
            {
              callId: event.call_id,
              action: event.action,
              approval: event.approval,
              // Validated here rather than trusted: the socket frame is the one
              // place a preview arrives without having gone through the HTTP
              // recovery parser.
              preview: parseToolActionPreview(event.preview),
              canApprove: approval.canApprove,
              canRemember: event.grant_rungs.length > 0,
              autoJudging: event.auto_judging,
              grantRungs: event.grant_rungs,
            },
            true,
          ),
        },
        effects,
      };
    }

    case "approval_decided": {
      return {
        state: {
          ...state,
          messages: updateApprovalAndToolCall(
            state.messages,
            event.call_id,
            event.approved,
          ),
        },
        effects,
      };
    }

    case "tool_call_completed": {
      const provisionalToolCallIds = new Set(state.provisionalToolCallIds);
      provisionalToolCallIds.delete(event.call_id);
      // An exec call that published durable outputs changes the Outputs
      // catalog; the sidebar count and outputs panel refresh on the same
      // signal the writeback flow uses. The preview only lists created and
      // updated files, so any row at all means the catalog moved.
      const completedResult = parseToolResultPreview(event.result);
      if (
        completedResult?.tool === "exec" &&
        (completedResult.outputs?.length ?? 0) > 0
      ) {
        effects.push({ type: "refresh_output_writebacks" });
        // A deck exists now; the preview will want a converter shortly.
        if (
          completedResult.outputs?.some(
            (output) =>
              output.mediaType !== null &&
              PRESENTATION_MEDIA_TYPES.has(output.mediaType),
          )
        ) {
          effects.push({ type: "warm_presentation_converter" });
        }
      }
      return {
        state: {
          ...state,
          provisionalToolCallIds,
          messages: updateToolCall(state.messages, event.call_id, (tool) => ({
            ...tool,
            status:
              tool.status === "cancelled" || tool.status === "denied"
                ? tool.status
                : event.status === "failed"
                  ? "failed"
                  : "completed",
            // Validated here rather than trusted: the socket frame is the one
            // place a projection arrives without going through the HTTP parser.
            // A call approved by a standing grant never had an approval card,
            // so completion is the first time the action itself arrives.
            preview: parseToolActionPreview(event.action) ?? tool.preview,
            result: completedResult,
          })),
        },
        effects,
      };
    }

    case "user_steered": {
      if (state.hydratedMessageIds.has(event.message_id)) {
        // Replay after hydration: the transcript already holds this message,
        // but hydration placed it before any replayed stream content. Journal
        // order is the true order, so move it to its replay position.
        const index = state.messages.findIndex(
          (message) => message.id === event.message_id,
        );
        if (index < 0 || index === state.messages.length - 1) {
          return { state, effects };
        }
        const messages = [...state.messages];
        const [moved] = messages.splice(index, 1);
        messages.push(moved);
        return { state: { ...state, messages }, effects };
      }
      const hydratedMessageIds = new Set(state.hydratedMessageIds);
      hydratedMessageIds.add(event.message_id);
      return {
        state: {
          ...state,
          hydratedMessageIds,
          messages: [
            ...state.messages,
            {
              id: event.message_id,
              role: "user",
              text: event.text,
              createdAt: deps.now(),
            },
          ],
        },
        effects,
      };
    }

    case "turn_completed": {
      state = flushMarkerTail(state, deps);
      effects.push(
        { type: "turn_resolved" },
        { type: "refresh_user_questions" },
        { type: "refresh_plan_approvals" },
        { type: "hydrate_terminal_transcript" },
      );
      return {
        state: {
          ...state,
          busy: false,
          compacting: false,
          activeTurnId: null,
          lastTurnUsage: event.usage,
          provisionalToolCallIds: new Set(),
        },
        effects,
      };
    }

    case "turn_refused": {
      state = flushMarkerTail(state, deps);
      effects.push(
        { type: "turn_resolved" },
        { type: "refresh_user_questions" },
        { type: "refresh_plan_approvals" },
        { type: "hydrate_terminal_transcript" },
      );
      return {
        state: {
          ...state,
          busy: false,
          compacting: false,
          activeTurnId: null,
          lastTurnUsage: event.usage,
          provisionalToolCallIds: new Set(),
          messages: [
            ...discardToolCalls(
              state.messages,
              state.provisionalToolCallIds,
            ),
            {
              id: deps.nextId(),
              role: "refusal",
              category: event.refusal.category,
              partialOutput: event.refusal.partial_output,
            },
          ],
        },
        effects,
      };
    }

    case "turn_cancelled": {
      state = flushMarkerTail(state, deps);
      effects.push(
        { type: "hydrate_terminal_transcript" },
        { type: "turn_resolved" },
        { type: "refresh_user_questions" },
        { type: "refresh_plan_approvals" },
      );
      return {
        state: {
          ...state,
          busy: false,
          compacting: false,
          activeTurnId: null,
          lastTurnUsage: event.usage,
          provisionalToolCallIds: new Set(),
          messages: [
            ...settleActiveToolCalls(state.messages, "cancelled"),
            {
              id: deps.nextId(),
              role: "system",
              text: TURN_CANCELLED_NOTICE,
            },
          ],
        },
        effects,
      };
    }

    case "turn_failed": {
      state = flushMarkerTail(state, deps);
      effects.push(
        { type: "hydrate_terminal_transcript" },
        { type: "turn_resolved" },
        { type: "refresh_user_questions" },
        { type: "refresh_plan_approvals" },
      );
      return {
        state: {
          ...state,
          busy: false,
          compacting: false,
          activeTurnId: null,
          provisionalToolCallIds: new Set(),
          messages: [
            ...settleActiveToolCalls(state.messages, "failed"),
            // The category rides into the transcript as data; the renderer
            // owns both the copy and which recovery it offers.
            {
              id: deps.nextId(),
              role: "turn_failure",
              category: event.category,
              detail: event.detail,
              model: event.model,
            },
          ],
        },
        effects,
      };
    }

    case "reasoning_delta": {
      const reasoningBuffer = state.reasoningBuffer + event.text;
      return {
        state: {
          ...state,
          reasoningBuffer,
          messages: paintTurnReasoning(state.messages, reasoningBuffer, deps),
        },
        effects,
      };
    }

    case "context_truncated": {
      if (state.contextTruncationNoted) return { state, effects };
      // Insert above the trailing assistant bubble (if streaming has begun)
      // so subsequent deltas keep extending one answer under the notice.
      const messages = [...state.messages];
      const insertAt =
        messages[messages.length - 1]?.role === "assistant"
          ? messages.length - 1
          : messages.length;
      messages.splice(insertAt, 0, {
        id: deps.nextId(),
        role: "system",
        text: contextTruncationNotice(
          event.original_tokens,
          event.fitted_tokens,
        ),
      });
      return {
        state: { ...state, contextTruncationNoted: true, messages },
        effects,
      };
    }

    case "compaction_started":
      return { state: { ...state, compacting: true }, effects };

    case "compaction_finished":
      return { state: { ...state, compacting: false }, effects };

    // Decoded but not presented; still advances the seq cursor.
    case "event_omitted":
      return { state, effects };

    // Event types this build does not know (a newer server) advance the
    // cursor and change nothing — falling off the switch would return
    // undefined and crash the caller once per frame.
    default:
      return { state, effects };
  }
}

/**
 * Fold an authoritative terminal transcript into the session, replacing the
 * optimistic stream. The seq cursor only moves forward: a snapshot can trail
 * events that arrived while it loaded.
 */
export function applyTerminalHydration(
  state: ChatSessionState,
  hydration: {
    messages: ChatMessage[];
    messageIds: ReadonlySet<string>;
    lastEventSeq: number;
    lastTurnUsage: RendererTurnUsage | null;
  },
): ChatSessionState {
  return {
    ...state,
    lastSeq: Math.max(state.lastSeq, hydration.lastEventSeq),
    hydratedMessageIds: hydration.messageIds,
    messages: hydration.messages,
    reasoningBuffer: "",
    // The snapshot is authoritative when it has counts — it is rebuilt from
    // the durable turn rows. A chat with no finished turn yet leaves whatever
    // the live stream established rather than blanking the meter.
    lastTurnUsage: hydration.lastTurnUsage ?? state.lastTurnUsage,
  };
}

/** Append any withheld marker-like tail to the trailing assistant bubble. */
function flushMarkerTail(
  state: ChatSessionState,
  deps: ChatSessionDeps,
): ChatSessionState {
  const tail = state.markerScrubber.finish();
  if (!tail) return state;
  const assistantBuffer = state.assistantBuffer + tail;
  return {
    ...state,
    assistantBuffer,
    messages: paintTurnReasoning(
      withTrailingAssistantText(state.messages, assistantBuffer, deps),
      state.reasoningBuffer,
      deps,
    ),
  };
}

/**
 * Keep the turn's reasoning on the latest live assistant bubble — the same
 * place the durable snapshot attaches `ChatTerminalTurnSnapshot.reasoning`.
 * Intermediate bubbles from earlier segments do not keep their own copy.
 */
function paintTurnReasoning(
  messages: ChatMessage[],
  reasoning: string,
  deps: ChatSessionDeps,
): ChatMessage[] {
  let lastLive = -1;
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const candidate = messages[index];
    if (candidate?.role === "assistant" && !candidate.superseded) {
      lastLive = index;
      break;
    }
  }
  if (lastLive < 0) {
    if (!reasoning) return messages;
    return [
      ...messages,
      {
        id: deps.nextId(),
        role: "assistant",
        text: "",
        sources: [],
        reasoning,
        createdAt: deps.now(),
      },
    ];
  }
  return messages.map((message, index) => {
    if (message.role !== "assistant" || message.superseded) return message;
    if (index === lastLive) {
      return message.reasoning === reasoning
        ? message
        : { ...message, reasoning: reasoning || undefined };
    }
    if (message.reasoning === undefined) return message;
    return { ...message, reasoning: undefined };
  });
}

function withTrailingAssistantText(
  messages: ChatMessage[],
  text: string,
  deps: ChatSessionDeps,
): ChatMessage[] {
  const copy = [...messages];
  const last = copy[copy.length - 1];
  if (last?.role === "assistant" && !last.superseded) {
    copy[copy.length - 1] = { ...last, text };
  } else {
    copy.push({
      id: deps.nextId(),
      role: "assistant",
      text,
      sources: [],
      createdAt: deps.now(),
    });
  }
  return copy;
}

export function upsertToolCall(
  messages: ChatMessage[],
  callId: string,
  name: string,
  status: ToolCallStatus,
  deps: ChatSessionDeps,
): ChatMessage[] {
  const existing = messages.findIndex(
    (message) => message.role === "tool" && message.callId === callId,
  );
  if (existing >= 0) {
    return messages.map((message, index) =>
      index === existing && message.role === "tool"
        ? { ...message, status }
        : message,
    );
  }
  return [
    ...messages,
    { id: deps.nextId(), role: "tool", callId, name, status },
  ];
}

export function updateToolCall(
  messages: ChatMessage[],
  callId: string,
  update: (
    tool: Extract<ChatMessage, { role: "tool" }>,
  ) => Extract<ChatMessage, { role: "tool" }>,
): ChatMessage[] {
  return messages.map((message) =>
    message.role === "tool" && message.callId === callId
      ? update(message)
      : message,
  );
}

export function updateApprovalAndToolCall(
  messages: ChatMessage[],
  callId: string,
  approved: boolean,
): ChatMessage[] {
  return messages.map((message) => {
    if (message.role === "approval" && message.callId === callId) {
      return { ...message, resolved: true };
    }
    if (message.role === "tool" && message.callId === callId) {
      return {
        ...message,
        status: approved ? "running" : "denied",
      };
    }
    return message;
  });
}

export function settleActiveToolCalls(
  messages: ChatMessage[],
  status: Extract<ToolCallStatus, "failed" | "cancelled">,
): ChatMessage[] {
  const activeCallIds = new Set(
    messages.flatMap((message) =>
      message.role === "tool" &&
      (message.status === "running" || message.status === "waiting_approval")
        ? [message.callId]
        : [],
    ),
  );
  return messages.map((message) =>
    message.role === "tool" &&
    (message.status === "running" || message.status === "waiting_approval")
      ? { ...message, status }
      : message.role === "approval" &&
          !message.resolved &&
          activeCallIds.has(message.callId)
        ? { ...message, resolved: true }
        : message,
  );
}

export function discardToolCalls(
  messages: ChatMessage[],
  callIds: ReadonlySet<string>,
): ChatMessage[] {
  return messages.filter(
    (message) => message.role !== "tool" || !callIds.has(message.callId),
  );
}

function discardUnadmittedToolCallsAtResume(
  messages: ChatMessage[],
  callIds: ReadonlySet<string>,
): ChatMessage[] {
  const unadmitted = new Set(
    messages.flatMap((message) =>
      message.role === "tool" &&
      callIds.has(message.callId) &&
      message.name !== "spawn_sandbox_agent"
        ? [message.callId]
        : [],
    ),
  );
  return discardToolCalls(messages, unadmitted);
}
