import type {
  CodeUsage,
  HarnessKind,
  HarnessNoticeLevel,
  SequencedCodeEventFrame,
  ToolDetail,
  ToolOutcome,
} from "../api/types";

/**
 * The pure state machine for one code session's live event stream.
 *
 * Chat already has this shape: a reducer owns the transcript and the seq
 * cursor, and returns declarative effects for anything a host component must
 * do. Code mode generalizes that to N sessions, but each session still
 * reduces the same way so replay, reconnect, and tests stay deterministic.
 */

export type CodeToolStatus = "running" | ToolOutcome;

export type CodeTranscriptItem =
  | {
      kind: "user";
      id: string;
      turnId: string | null;
      text: string;
    }
  | {
      kind: "assistant";
      id: string;
      turnId: string | null;
      text: string;
      streaming: boolean;
    }
  | {
      kind: "reasoning";
      id: string;
      turnId: string | null;
      text: string;
      streaming: boolean;
    }
  | {
      kind: "tool";
      id: string;
      turnId: string | null;
      callId: string;
      name: string;
      detail: ToolDetail;
      status: CodeToolStatus;
      preview: string;
    }
  | {
      kind: "notice";
      id: string;
      level: HarnessNoticeLevel;
      message: string;
    }
  | {
      kind: "turn_boundary";
      id: string;
      turnId: string | null;
      status: "completed" | "failed" | "interrupted";
      durationMs: number | null;
      usage: CodeUsage | null;
      error: string | null;
    };

export type CodeSessionState = {
  /** Highest event seq applied; events at or below it are duplicates. */
  lastSeq: number;
  /** Whether new stream presentation should animate rather than catch up. */
  animateStreaming: boolean;
  items: CodeTranscriptItem[];
  busy: boolean;
  activeTurnId: string | null;
  turnStartedAt: string | null;
  assistantBuffer: string;
  reasoningBuffer: string;
  harnessKind: HarnessKind | null;
  harnessVersion: string | null;
  lastUsage: CodeUsage | null;
};

export type CodeSessionEffect =
  | { type: "turn_began"; turnId: string }
  | { type: "turn_resolved" };

export type CodeSessionTransition = {
  state: CodeSessionState;
  effects: CodeSessionEffect[];
};

export type CodeSessionDeps = {
  nextId: () => string;
  now: () => string;
};

export function initialCodeSessionState(): CodeSessionState {
  return {
    lastSeq: 0,
    animateStreaming: true,
    items: [],
    busy: false,
    activeTurnId: null,
    turnStartedAt: null,
    assistantBuffer: "",
    reasoningBuffer: "",
    harnessKind: null,
    harnessVersion: null,
    lastUsage: null,
  };
}

export function reduceCodeSessionEvent(
  state: CodeSessionState,
  framed: SequencedCodeEventFrame,
  deps: CodeSessionDeps,
): CodeSessionTransition {
  if (framed.seq <= state.lastSeq) return { state, effects: [] };
  state = {
    ...state,
    lastSeq: framed.seq,
    animateStreaming: framed.replayed !== true,
  };
  const event = framed.event;
  const effects: CodeSessionEffect[] = [];

  switch (event.type) {
    case "session_started":
      return {
        state: {
          ...state,
          harnessKind: event.harness_kind,
          harnessVersion: event.harness_version,
        },
        effects,
      };

    case "turn_started": {
      effects.push({ type: "turn_began", turnId: event.turn_id });
      return {
        state: {
          ...state,
          busy: true,
          activeTurnId: event.turn_id,
          turnStartedAt: deps.now(),
          assistantBuffer: "",
          reasoningBuffer: "",
        },
        effects,
      };
    }

    case "assistant_delta": {
      const assistantBuffer = state.assistantBuffer + event.text;
      return {
        state: {
          ...state,
          assistantBuffer,
          items: upsertStreaming(
            state.items,
            "assistant",
            assistantBuffer,
            state.activeTurnId,
            deps.nextId,
          ),
        },
        effects,
      };
    }

    case "assistant_message": {
      return {
        state: {
          ...state,
          assistantBuffer: event.text,
          items: finalizeStreaming(
            upsertStreaming(
              state.items,
              "assistant",
              event.text,
              state.activeTurnId,
              deps.nextId,
            ),
            "assistant",
          ),
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
          items: upsertStreaming(
            state.items,
            "reasoning",
            reasoningBuffer,
            state.activeTurnId,
            deps.nextId,
          ),
        },
        effects,
      };
    }

    case "tool_started": {
      return {
        state: {
          ...state,
          items: [
            ...finalizeStreaming(state.items, "assistant"),
            {
              kind: "tool",
              id: deps.nextId(),
              turnId: state.activeTurnId,
              callId: event.call_id,
              name: event.name,
              detail: event.detail,
              status: "running",
              preview: "",
            },
          ],
        },
        effects,
      };
    }

    case "tool_completed": {
      return {
        state: {
          ...state,
          items: state.items.map((item) =>
            item.kind === "tool" && item.callId === event.call_id
              ? { ...item, status: event.outcome, preview: event.preview }
              : item,
          ),
        },
        effects,
      };
    }

    case "harness_notice": {
      return {
        state: {
          ...state,
          items: [
            ...state.items,
            {
              kind: "notice",
              id: deps.nextId(),
              level: event.level,
              message: event.message,
            },
          ],
        },
        effects,
      };
    }

    case "turn_completed":
    case "turn_failed":
    case "turn_interrupted": {
      effects.push({ type: "turn_resolved" });
      const status =
        event.type === "turn_completed"
          ? "completed"
          : event.type === "turn_failed"
            ? "failed"
            : "interrupted";
      const usage = event.type === "turn_completed" ? event.usage : null;
      const error = event.type === "turn_failed" ? event.error.message : null;
      return {
        state: {
          ...state,
          busy: false,
          lastUsage: usage ?? state.lastUsage,
          assistantBuffer: "",
          reasoningBuffer: "",
          items: [
            ...finalizeStreaming(
              finalizeStreaming(state.items, "assistant"),
              "reasoning",
            ),
            {
              kind: "turn_boundary",
              id: deps.nextId(),
              turnId: state.activeTurnId,
              status,
              durationMs: durationMs(state.turnStartedAt, deps.now()),
              usage,
              error,
            },
          ],
          activeTurnId: null,
          turnStartedAt: null,
        },
        effects,
      };
    }

    default:
      return { state, effects };
  }
}

function upsertStreaming(
  items: CodeTranscriptItem[],
  kind: "assistant" | "reasoning",
  text: string,
  turnId: string | null,
  nextId: () => string,
): CodeTranscriptItem[] {
  const last = items[items.length - 1];
  if (last && last.kind === kind && last.streaming) {
    return [...items.slice(0, -1), { ...last, text }];
  }
  return [
    ...items,
    { kind, id: nextId(), turnId, text, streaming: true },
  ];
}

function finalizeStreaming(
  items: CodeTranscriptItem[],
  kind: "assistant" | "reasoning",
): CodeTranscriptItem[] {
  return items.map((item) =>
    item.kind === kind && item.streaming ? { ...item, streaming: false } : item,
  );
}

function durationMs(startedAt: string | null, endedAt: string): number | null {
  if (!startedAt) return null;
  const start = Date.parse(startedAt);
  const end = Date.parse(endedAt);
  if (!Number.isFinite(start) || !Number.isFinite(end) || end < start) {
    return null;
  }
  return end - start;
}
