import type {
  Attention,
  CodeSessionLifecycle,
  CodeTurnSnapshot,
  CodeTurnStatus,
  CodeUsage,
  Diffstat,
  FileChangeKind,
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
      turnId: string;
      text: string;
      /** When the server accepted the turn, for the message footer's time. */
      createdAt: string;
      attachments?: import("../generated/wire").CodeTurnAttachment[];
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
      /** When this client saw the call start. Null on journal replay. */
      startedAt: string | null;
      /** Wall time from start to completion. Null when replayed or still running. */
      durationMs: number | null;
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
      diffstat: Diffstat | null;
    }
  | {
      kind: "approval";
      id: string;
      approvalId: string;
      state: "pending" | "approved" | "denied";
    }
  | {
      kind: "steer";
      id: string;
      turnId: string | null;
      text: string;
    }
  | {
      kind: "file_activity";
      id: string;
      turnId: string | null;
      files: Record<string, { kind: FileChangeKind; diffstat: Diffstat }>;
    };

export type CodeSessionState = {
  /** Highest event seq applied; events at or below it are duplicates. */
  lastSeq: number;
  /** Whether new stream presentation should animate rather than catch up. */
  animateStreaming: boolean;
  /**
   * Whether the durable turn snapshot has settled, either way.
   *
   * A session that has not hydrated yet is transiently empty, which reads
   * exactly like a session with nothing in it. The transcript shows a skeleton
   * until this flips, so an existing session does not flash its empty state
   * before its history lands.
   */
  hydrated: boolean;
  items: CodeTranscriptItem[];
  busy: boolean;
  activeTurnId: string | null;
  turnStartedAt: string | null;
  assistantBuffer: string;
  reasoningBuffer: string;
  harnessKind: HarnessKind | null;
  harnessVersion: string | null;
  lastUsage: CodeUsage | null;
  /** Session lifecycle as the journal last stated it. */
  lifecycle: CodeSessionLifecycle | null;
  /**
   * Bumped whenever the journal says the worktree may have moved: a turn
   * resolved, or a checkpoint was recorded. Views that read the worktree
   * through the API (git status, changed files, the diff) treat a change here
   * as "your copy is stale", so they do not need their own polling.
   */
  contentRevision: number;
  /**
   * Latest `attention_changed` from the journal. Not a transcript item — the
   * header badge reads this so a stall or a need-you does not grow the log.
   */
  attention: Attention | null;
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
    hydrated: false,
    items: [],
    busy: false,
    activeTurnId: null,
    turnStartedAt: null,
    assistantBuffer: "",
    reasoningBuffer: "",
    harnessKind: null,
    harnessVersion: null,
    lastUsage: null,
    lifecycle: null,
    contentRevision: 0,
    attention: null,
  };
}

export function userItemId(turnId: string): string {
  return `user:${turnId}`;
}

export function boundaryItemId(turnId: string): string {
  return `boundary:${turnId}`;
}

export function fileActivityItemId(turnId: string | null): string {
  return turnId ? `files:${turnId}` : "files:open";
}

/**
 * The durable snapshot and initial journal replay have settled.
 *
 * The skeleton comes down even if either source was empty or unavailable: the
 * reader still reaches a transcript they can send into, but never watches a
 * historical turn rebuild itself over several paints.
 */
export function markCodeSessionHydrated(
  state: CodeSessionState,
): CodeSessionState {
  return state.hydrated ? state : { ...state, hydrated: true };
}

/**
 * Record a turn the server accepted. Create and hydrate share this so a
 * reopen and a live send produce the same turn-keyed user item.
 */
export function applyAcceptedTurn(
  state: CodeSessionState,
  turn: CodeTurnSnapshot,
): CodeSessionState {
  const user: CodeTranscriptItem = {
    kind: "user",
    id: userItemId(turn.id),
    turnId: turn.id,
    text: turn.user_input,
    // Every accepted turn carries its own start, live or replayed, so the
    // prompt's timestamp never depends on when this client happened to see it.
    createdAt: turn.started_at,
    attachments: turn.attachments ?? [],
  };
  const hasUser = state.items.some(
    (item) => item.kind === "user" && item.turnId === turn.id,
  );
  const items = hasUser
    ? state.items.map((item) =>
        item.kind === "user" && item.turnId === turn.id ? user : item,
      )
    : insertUserBeforeTurn(state.items, user, turn.id);
  const running = turn.status === "running";
  return {
    ...state,
    items,
    busy: running || state.busy,
    activeTurnId: running ? turn.id : state.activeTurnId,
    turnStartedAt: running ? turn.started_at : state.turnStartedAt,
    lifecycle: running ? "running" : state.lifecycle,
  };
}

/** Snapshot of durable turns, applied before the journal replays. */
export function hydrateCodeTurns(
  state: CodeSessionState,
  turns: readonly CodeTurnSnapshot[],
): CodeSessionState {
  let next = state;
  for (const turn of turns) {
    next = applyAcceptedTurn(next, turn);
    if (turn.status !== "running") {
      next = {
        ...next,
        items: upsertTurnBoundary(next.items, {
          turnId: turn.id,
          status: turn.status,
          durationMs: durationMs(turn.started_at, turn.ended_at ?? null),
          usage: turn.usage ?? null,
          error: null,
          diffstat: turn.diffstat ?? null,
        }),
      };
    }
  }
  const lastUsage =
    [...turns].reverse().find((turn) => turn.usage)?.usage ?? next.lastUsage;
  const open = [...turns].reverse().find((turn) => turn.status === "running");
  if (open) {
    return {
      ...next,
      lastUsage,
      busy: true,
      activeTurnId: open.id,
      turnStartedAt: open.started_at,
      lifecycle: "running",
    };
  }
  if (turns.length > 0) {
    return { ...next, lastUsage, lifecycle: next.lifecycle ?? "idle" };
  }
  return next;
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
          turnStartedAt: state.turnStartedAt ?? deps.now(),
          assistantBuffer: "",
          reasoningBuffer: "",
          lifecycle: "running",
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
          assistantBuffer: "",
          reasoningBuffer: "",
          items: insertBeforeTurnBoundary(
            finalizeStreaming(state.items, "assistant"),
            state.activeTurnId,
            {
              kind: "tool",
              id: deps.nextId(),
              turnId: state.activeTurnId,
              callId: event.call_id,
              name: event.name,
              detail: event.detail,
              status: "running",
              preview: "",
              startedAt: framed.replayed ? null : deps.now(),
              durationMs: null,
            },
          ),
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
              ? {
                  ...item,
                  status: event.outcome,
                  preview: event.preview,
                  detail: mergeToolDetail(item.detail, event.detail),
                  durationMs: framed.replayed
                    ? null
                    : durationMs(item.startedAt, deps.now()),
                }
              : item,
          ),
        },
        effects,
      };
    }

    case "approval_requested": {
      const approvalId = event.approval_id;
      const existing = state.items.some(
        (item) => item.kind === "approval" && item.approvalId === approvalId,
      );
      return {
        state: {
          ...state,
          items: existing
            ? state.items
            : insertBeforeTurnBoundary(state.items, state.activeTurnId, {
                kind: "approval",
                id: `approval:${approvalId}`,
                approvalId,
                state: "pending",
              }),
        },
        effects,
      };
    }

    case "approval_resolved": {
      const approvalId = event.approval_id;
      const nextState =
        event.decision.type === "approve" ? "approved" : "denied";
      return {
        state: {
          ...state,
          items: state.items.map((item) =>
            item.kind === "approval" && item.approvalId === approvalId
              ? { ...item, state: nextState }
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
          items: insertBeforeTurnBoundary(state.items, state.activeTurnId, {
            kind: "notice",
            id: deps.nextId(),
            level: event.level,
            message: event.message,
          }),
        },
        effects,
      };
    }

    case "checkpoint_recorded": {
      return {
        state: {
          ...state,
          items: applyDiffstat(state.items, event.turn_id, event.diffstat),
          contentRevision: state.contentRevision + 1,
        },
        effects,
      };
    }

    case "user_steered": {
      return {
        state: {
          ...state,
          items: insertBeforeTurnBoundary(state.items, state.activeTurnId, {
            kind: "steer",
            id: deps.nextId(),
            turnId: state.activeTurnId,
            text: event.text,
          }),
        },
        effects,
      };
    }

    case "attention_changed": {
      return {
        state: {
          ...state,
          attention: { state: event.state, source: event.source },
        },
        effects,
      };
    }

    case "file_changed": {
      return {
        state: {
          ...state,
          items: upsertFileActivity(
            state.items,
            state.activeTurnId,
            event.path,
            event.kind,
            event.diffstat,
          ),
          contentRevision: state.contentRevision + 1,
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
      const diffstat =
        event.type === "turn_completed"
          ? (event.checkpoint?.diffstat ?? null)
          : null;
      const turnId = state.activeTurnId;
      const finalized = finalizeStreaming(
        finalizeStreaming(state.items, "assistant"),
        "reasoning",
      );
      return {
        state: {
          ...state,
          busy: false,
          lastUsage: usage ?? state.lastUsage,
          assistantBuffer: "",
          reasoningBuffer: "",
          items: turnId
            ? upsertTurnBoundary(finalized, {
                turnId,
                status,
                durationMs: durationMs(state.turnStartedAt, deps.now()),
                usage,
                error,
                diffstat,
              })
            : finalized,
          activeTurnId: null,
          turnStartedAt: null,
          lifecycle: "idle",
          // A failed or interrupted turn still leaves whatever the engine
          // wrote before it stopped, so every resolution is a content change.
          contentRevision: state.contentRevision + 1,
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
  if (
    last &&
    last.kind === kind &&
    last.streaming &&
    last.turnId === turnId
  ) {
    return [...items.slice(0, -1), { ...last, text }];
  }
  const existing = lastIndexOfKindForTurn(items, kind, turnId);
  if (existing !== -1) {
    const prev = items[existing];
    if (prev.kind !== kind) {
      return insertBeforeTurnBoundary(items, turnId, {
        kind,
        id: nextId(),
        turnId,
        text,
        streaming: true,
      });
    }
    if (text === prev.text || prev.text.startsWith(text)) {
      return items;
    }
    if (text.startsWith(prev.text)) {
      const suffix = text.slice(prev.text.length);
      if (suffix.trim() === "") return items;
      const next = items[existing + 1];
      if (!next || next.kind === "turn_boundary") {
        return items.map((item, index) =>
          index === existing ? { ...prev, text, streaming: true } : item,
        );
      }
      return insertBeforeTurnBoundary(items, turnId, {
        kind,
        id: nextId(),
        turnId,
        text: suffix,
        streaming: true,
      });
    }
  }
  if (text.trim() === "") return items;
  return insertBeforeTurnBoundary(items, turnId, {
    kind,
    id: nextId(),
    turnId,
    text,
    streaming: true,
  });
}

function lastIndexOfKindForTurn(
  items: readonly CodeTranscriptItem[],
  kind: "assistant" | "reasoning",
  turnId: string | null,
): number {
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index];
    if (item.kind === kind && item.turnId === turnId) return index;
  }
  return -1;
}

function insertBeforeTurnBoundary(
  items: CodeTranscriptItem[],
  turnId: string | null,
  item: CodeTranscriptItem,
): CodeTranscriptItem[] {
  if (turnId) {
    const boundary = items.findIndex(
      (candidate) =>
        candidate.kind === "turn_boundary" && candidate.turnId === turnId,
    );
    if (boundary !== -1) {
      return [...items.slice(0, boundary), item, ...items.slice(boundary)];
    }
  }
  return [...items, item];
}

function finalizeStreaming(
  items: CodeTranscriptItem[],
  kind: "assistant" | "reasoning",
): CodeTranscriptItem[] {
  return items.map((item) =>
    item.kind === kind && item.streaming ? { ...item, streaming: false } : item,
  );
}

function insertUserBeforeTurn(
  items: CodeTranscriptItem[],
  user: Extract<CodeTranscriptItem, { kind: "user" }>,
  turnId: string,
): CodeTranscriptItem[] {
  const index = items.findIndex(
    (item) => "turnId" in item && item.turnId === turnId,
  );
  if (index === -1) return [...items, user];
  return [...items.slice(0, index), user, ...items.slice(index)];
}

function upsertTurnBoundary(
  items: CodeTranscriptItem[],
  boundary: {
    turnId: string;
    status: Exclude<CodeTurnStatus, "running">;
    durationMs: number | null;
    usage: CodeUsage | null;
    error: string | null;
    diffstat: Diffstat | null;
  },
): CodeTranscriptItem[] {
  const item: CodeTranscriptItem = {
    kind: "turn_boundary",
    id: boundaryItemId(boundary.turnId),
    turnId: boundary.turnId,
    status: boundary.status,
    durationMs: boundary.durationMs,
    usage: boundary.usage,
    error: boundary.error,
    diffstat: boundary.diffstat,
  };
  const index = items.findIndex(
    (candidate) =>
      candidate.kind === "turn_boundary" && candidate.turnId === boundary.turnId,
  );
  if (index === -1) return [...items, item];
  const existing = items[index];
  if (existing && existing.kind === "turn_boundary") {
    return [
      ...items.slice(0, index),
      {
        ...item,
        usage: boundary.usage ?? existing.usage,
        error: boundary.error ?? existing.error,
        durationMs: boundary.durationMs ?? existing.durationMs,
        diffstat: boundary.diffstat ?? existing.diffstat,
      },
      ...items.slice(index + 1),
    ];
  }
  return items;
}

function applyDiffstat(
  items: CodeTranscriptItem[],
  turnId: string,
  diffstat: Diffstat,
): CodeTranscriptItem[] {
  return items.map((item) =>
    item.kind === "turn_boundary" && item.turnId === turnId
      ? { ...item, diffstat }
      : item,
  );
}

function upsertFileActivity(
  items: CodeTranscriptItem[],
  turnId: string | null,
  path: string,
  kind: FileChangeKind,
  diffstat: Diffstat,
): CodeTranscriptItem[] {
  const id = fileActivityItemId(turnId);
  const index = items.findIndex(
    (item) => item.kind === "file_activity" && item.turnId === turnId,
  );
  if (index === -1) {
    return insertBeforeTurnBoundary(items, turnId, {
      kind: "file_activity",
      id,
      turnId,
      files: { [path]: { kind, diffstat } },
    });
  }
  const existing = items[index];
  if (!existing || existing.kind !== "file_activity") return items;
  return [
    ...items.slice(0, index),
    {
      ...existing,
      files: { ...existing.files, [path]: { kind, diffstat } },
    },
    ...items.slice(index + 1),
  ];
}

function durationMs(
  startedAt: string | null,
  endedAt: string | null,
): number | null {
  if (!startedAt || !endedAt) return null;
  const start = Date.parse(startedAt);
  const end = Date.parse(endedAt);
  if (!Number.isFinite(start) || !Number.isFinite(end) || end < start) {
    return null;
  }
  return end - start;
}

/**
 * How much a detail says about the call: the subject a tool line names.
 *
 * Zero is a detail with no subject, one is a bare tool name, and two is a
 * real command, path, or query. Mirrors `ToolDetail::specificity` in
 * `tidebreak-core`.
 */
function toolDetailSpecificity(detail: ToolDetail): number {
  const subject =
    detail.kind === "command"
      ? detail.cmd
      : detail.kind === "search"
        ? detail.query
        : detail.kind === "other"
          ? detail.summary
          : detail.path;
  if (!subject.trim()) return 0;
  return detail.kind === "other" ? 1 : 2;
}

/**
 * Fold a completed call's detail into the one its start carried.
 *
 * Engines open a tool call before its arguments finish streaming, so the
 * detail on `tool_started` can name nothing and the line falls back to the
 * tool's name. `tool_completed` carries the detail rebuilt from the complete
 * arguments, which is the more trustworthy view — it wins unless it says
 * less, so a correction never downgrades a line that already names its
 * subject.
 */
function mergeToolDetail(
  current: ToolDetail,
  correction: ToolDetail | null | undefined,
): ToolDetail {
  if (!correction) return current;
  return toolDetailSpecificity(correction) >= toolDetailSpecificity(current)
    ? correction
    : current;
}
