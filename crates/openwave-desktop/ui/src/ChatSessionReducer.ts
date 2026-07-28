import type { SequencedEvent } from "./api";
import { parseToolActionPreview, parseToolResultPreview } from "./api";
import { AssistantSourceMarkerStreamScrubber } from "./AssistantSourceMarkerStream";
import type { ChatMessage } from "./MessageList";
import type { ToolCallStatus } from "./ToolCallCard";
import { toolApprovalPresentation } from "./ToolCallCard";
import { upsertPendingApprovalCard } from "./ApprovalHistory";

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
  messages: ChatMessage[];
  busy: boolean;
  activeTurnId: string | null;
  /** Scrubbed text accumulated for the trailing assistant bubble. */
  assistantBuffer: string;
  markerScrubber: AssistantSourceMarkerStreamScrubber;
  /** Tool calls streamed in the current step; discarded on interruption. */
  provisionalToolCallIds: ReadonlySet<string>;
  /** Message ids already present via hydration; used to dedup steered echoes. */
  hydratedMessageIds: ReadonlySet<string>;
  /** The model is emitting reasoning; cleared once visible output starts. */
  reasoningActive: boolean;
  /** One truncation notice per turn, however many truncation events arrive. */
  contextTruncationNoted: boolean;
};

export type ChatSessionEffect =
  | { type: "refresh_folder_access" }
  | { type: "refresh_output_writebacks" }
  | { type: "refresh_user_questions" }
  /** A turn began; the host resets cancel state (and steer state when asked). */
  | { type: "turn_began"; turnId: string; startsDifferentTurn: boolean }
  /** A turn reached a terminal event; the host clears cancel/steer state. */
  | { type: "turn_resolved" }
  /** Replace the optimistic transcript with the authoritative one. */
  | { type: "hydrate_terminal_transcript" }
  /** A terminal boundary passed without hydration; stale loads must not land. */
  | { type: "invalidate_terminal_hydration" };

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
    messages: [],
    busy: false,
    activeTurnId: null,
    assistantBuffer: "",
    markerScrubber: new AssistantSourceMarkerStreamScrubber(),
    provisionalToolCallIds: new Set(),
    hydratedMessageIds: new Set(),
    reasoningActive: false,
    contextTruncationNoted: false,
  };
}

export function reduceChatSessionEvent(
  state: ChatSessionState,
  framed: SequencedEvent,
  deps: ChatSessionDeps,
): ChatSessionTransition {
  if (framed.seq <= state.lastSeq) return { state, effects: [] };
  state = { ...state, lastSeq: framed.seq };
  const event = framed.event;
  const effects: ChatSessionEffect[] = [];

  switch (event.type) {
    case "turn_started": {
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
          reasoningActive: false,
          contextTruncationNoted: false,
          assistantBuffer: "",
          markerScrubber: new AssistantSourceMarkerStreamScrubber(),
          provisionalToolCallIds: new Set(),
          messages: [
            ...state.messages,
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
          reasoningActive: false,
          assistantBuffer,
          messages: withTrailingAssistantText(
            state.messages,
            assistantBuffer,
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
          reasoningActive: false,
          assistantBuffer: "",
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
          reasoningActive: false,
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

    case "approval_required": {
      const approval = toolApprovalPresentation(event.approval);
      const provisionalToolCallIds = new Set(state.provisionalToolCallIds);
      provisionalToolCallIds.delete(event.call_id);
      return {
        state: {
          ...state,
          provisionalToolCallIds,
          messages: upsertPendingApprovalCard(state.messages, {
            callId: event.call_id,
            action: event.action,
            approval: event.approval,
            // Validated here rather than trusted: the socket frame is the one
            // place a preview arrives without having gone through the HTTP
            // recovery parser.
            preview: parseToolActionPreview(event.preview),
            canApprove: approval.canApprove,
            canRemember: approval.canRemember,
          }),
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
      return {
        state: {
          ...state,
          provisionalToolCallIds,
          messages: updateToolCall(state.messages, event.call_id, (tool) => ({
            ...tool,
            status:
              tool.status === "cancelled"
                ? "cancelled"
                : event.status === "failed"
                  ? "failed"
                  : "completed",
            // Validated here rather than trusted: the socket frame is the one
            // place a projection arrives without going through the HTTP parser.
            // A call approved by a standing grant never had an approval card,
            // so completion is the first time the action itself arrives.
            preview: parseToolActionPreview(event.action) ?? tool.preview,
            result: parseToolResultPreview(event.result),
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
        { type: "hydrate_terminal_transcript" },
      );
      return {
        state: {
          ...state,
          busy: false,
          activeTurnId: null,
          reasoningActive: false,
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
        { type: "hydrate_terminal_transcript" },
      );
      return {
        state: {
          ...state,
          busy: false,
          activeTurnId: null,
          reasoningActive: false,
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
        { type: "invalidate_terminal_hydration" },
        { type: "turn_resolved" },
        { type: "refresh_user_questions" },
      );
      return {
        state: {
          ...state,
          busy: false,
          activeTurnId: null,
          reasoningActive: false,
          provisionalToolCallIds: new Set(),
          messages: [
            ...settleActiveToolCalls(state.messages, "cancelled"),
            { id: deps.nextId(), role: "system", text: "turn cancelled" },
          ],
        },
        effects,
      };
    }

    case "turn_failed": {
      state = flushMarkerTail(state, deps);
      effects.push(
        { type: "invalidate_terminal_hydration" },
        { type: "turn_resolved" },
        { type: "refresh_user_questions" },
      );
      return {
        state: {
          ...state,
          busy: false,
          activeTurnId: null,
          reasoningActive: false,
          provisionalToolCallIds: new Set(),
          messages: [
            ...settleActiveToolCalls(state.messages, "failed"),
            {
              id: deps.nextId(),
              role: "error",
              text: "The turn could not be completed.",
            },
          ],
        },
        effects,
      };
    }

    case "reasoning_delta": {
      return { state: { ...state, reasoningActive: true }, effects };
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
        text: "Earlier conversation was trimmed to fit the model's context.",
      });
      return {
        state: { ...state, contextTruncationNoted: true, messages },
        effects,
      };
    }

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
  },
): ChatSessionState {
  return {
    ...state,
    lastSeq: Math.max(state.lastSeq, hydration.lastEventSeq),
    hydratedMessageIds: hydration.messageIds,
    messages: hydration.messages,
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
    messages: withTrailingAssistantText(state.messages, assistantBuffer, deps),
  };
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
        status: approved ? "running" : "cancelled",
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
