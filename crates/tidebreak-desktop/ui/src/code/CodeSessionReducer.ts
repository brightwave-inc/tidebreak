import type {
  ApprovalDecisionKind,
  CheckpointRestoreStatus,
  TurnActor,
} from "../generated/wire";
import type {
  CheckpointRestoreTarget,
  CodeApprovalState,
  CodeEvent,
  CodeSessionLifecycle,
  CodeSessionSnapshot,
  CodeTurnSnapshot,
  CodeTurnStatus,
  CodeUsage,
  Diffstat,
  FileChangeKind,
  HarnessKind,
  HarnessNoticeLevel,
  ReviewOutcome,
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

/**
 * Terminal approval state each resolution carries. `abandoned` is not a
 * decision: the tool call resolved before anyone made one, so the card stops
 * offering buttons and says the request went undecided.
 */
const RESOLVED_APPROVAL_STATE: Record<
  ApprovalDecisionKind["type"],
  CodeApprovalState
> = {
  approve: "approved",
  deny: "denied",
  abandoned: "abandoned",
  approved_with_grant: "approved",
  answered: "approved",
  // An accepted plan; a rejected one is special-cased where this is read.
  plan_decided: "approved",
};

export type CodeTranscriptItem =
  | {
      kind: "user";
      id: string;
      turnId: string;
      text: string;
      /** When the server accepted the turn, for the message footer's time. */
      createdAt: string;
      /**
       * Who submitted the turn, when the row names someone (decision 0086).
       * Absent on a turn the session's owner sent, and on rows written before
       * turns recorded an actor.
       */
      actorLabel?: string;
      /**
       * The pull-request event a trigger or watch fired this turn on. The
       * transcript draws such a turn as a structured event card instead of a
       * person's message.
       */
      trigger?: import("../generated/wire").TriggerTurnContext;
      attachments?: import("../generated/wire").ImageRef[];
    }
  | {
      kind: "assistant";
      id: string;
      turnId: string | null;
      /** The spanning Task call when a harness subagent produced this text. */
      parentCallId: string | null;
      text: string;
      streaming: boolean;
      /** Lucid rewrite of the closing message. The journal text stays in `text`. */
      rewrite?: string;
      rewriteState?: "rewriting" | "rewritten" | "failed";
      /**
       * Written by a turn the engine started on its own, outside every turn
       * of the person's. The person's reply never streams into it.
       */
      background?: true;
      /** The journal events that wrote this row; see {@link itemForEvent}. */
      seqs?: readonly number[];
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
      /** The spanning Task call when a harness subagent issued this tool. */
      parentCallId: string | null;
      name: string;
      detail: ToolDetail;
      status: CodeToolStatus;
      preview: string;
      /** When this client saw the call start. Null on journal replay. */
      startedAt: string | null;
      /** Wall time from start to completion. Null when replayed or still running. */
      durationMs: number | null;
      /** Run by a turn the engine started on its own. */
      background?: true;
      /** The journal events that wrote this row; see {@link itemForEvent}. */
      seqs?: readonly number[];
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
      state: CodeApprovalState;
    }
  | {
      kind: "steer";
      id: string;
      turnId: string | null;
      text: string;
      /** The journal event that wrote this row; see {@link itemForEvent}. */
      seqs?: readonly number[];
    }
  | {
      kind: "file_activity";
      id: string;
      turnId: string | null;
      files: Record<string, { kind: FileChangeKind; diffstat: Diffstat }>;
    }
  | {
      /**
       * The worktree went back to an earlier state between turns. The id
       * names the state the restore replaced, which is what undoing it puts
       * back.
       */
      kind: "restore";
      id: string;
      restoreId: string;
      target: CheckpointRestoreTarget;
      /** The restored turn's ordinal, when the transcript knows it. */
      turnOrdinal: number | null;
      diffstat: Diffstat;
      /**
       * How far the restore got. The row lands as `started` before any file
       * moves and changes in place when the restore ends.
       */
      status: CheckpointRestoreStatus;
      /** Why a failed or partial restore stopped. */
      error: string | null;
    }
  | {
      /**
       * Another engine reviewed the workspace's changes, read-only, and the
       * review ended. Its findings are comments on the diff, not rows here.
       */
      kind: "review";
      id: string;
      reviewId: string;
      harness: HarnessKind;
      model: string | null;
      /** The turn it reviewed; null for the working tree. */
      turnId: string | null;
      /** That turn's ordinal, when the transcript knows it. */
      turnOrdinal: number | null;
      outcome: ReviewOutcome;
      findings: number;
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
  /** Turn established by a journal `turn_started` frame. */
  journalTurnId: string | null;
  /**
   * The turn whose end the journal applied last. What the engine does on its
   * own between turns lands after that turn and before the next one.
   */
  settledTurnId: string | null;
  /**
   * A submit accepted after this journal cursor still owns live activity until
   * the journal names that turn or a newer live frame supersedes it.
   */
  acceptedTurnFence: { turnId: string; afterSeq: number } | null;
  /** Changes when live activity changes without necessarily moving `lastSeq`. */
  turnActivityRevision: number;
  turnStartedAt: string | null;
  /** Whether this store observed the active turn start as a live frame. */
  turnStartObservedLive: boolean;
  /** Boundaries whose timing came from durable turn snapshots. */
  durableBoundaryTurnIds: ReadonlySet<string>;
  /** Durable turn order, used to place boundaries recovered after later turns render. */
  turnOrdinals: ReadonlyMap<string, number>;
  /**
   * Replayed terminals that still need an authoritative turn snapshot.
   *
   * This state lives with the retained transcript instead of one controller,
   * so a failed read survives workspace close and reopen. The event sequence
   * is the key because a capped replay may contain several terminals before
   * the first retained `turn_started` frame establishes any turn identity.
   */
  pendingTerminalReconciliations: ReadonlyMap<
    number,
    PendingTerminalReconciliation
  >;
  assistantBuffer: string;
  reasoningBuffer: string;
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
   * Recap text from durable turn snapshots, keyed by turn id. Hydration
   * runs before journal replay, so these stamp onto the closing message
   * after assistant rows exist.
   */
  storedRewrites: Record<string, string>;
  /** Direct children from the snapshot and live `session_tree` events. */
  children: NonNullable<CodeSessionSnapshot["children"]>;
  /** Authoritative wait; never inferred from running children. */
  wait: CodeSessionSnapshot["wait"];
};

export type CodeSessionEffect =
  | { type: "turn_began"; turnId: string }
  | { type: "turn_resolved" }
  | { type: "turn_snapshot_needed"; turnId: string | null };

export type CodeSessionTransition = {
  state: CodeSessionState;
  effects: CodeSessionEffect[];
};

export type CodeSessionDeps = {
  nextId: () => string;
  now: () => string;
};

export type PendingTerminalReconciliation = {
  eventSeq: number;
  /** Exact when a retained `turn_started` established the terminal's turn. */
  turnId: string | null;
  /** Snapshot activity is only an ordering hint. It is never identity proof. */
  candidateTurnId: string | null;
  /** The first retained start after an unassigned terminal, when one appears. */
  nextTurnId: string | null;
  status: "completed" | "failed" | "interrupted";
  usage: CodeUsage | null;
  error: string | null;
  diffstat: Diffstat | null;
  previousUsage: CodeUsage | null;
};

export function initialCodeSessionState(): CodeSessionState {
  return {
    lastSeq: 0,
    animateStreaming: true,
    hydrated: false,
    items: [],
    busy: false,
    activeTurnId: null,
    journalTurnId: null,
    settledTurnId: null,
    acceptedTurnFence: null,
    turnActivityRevision: 0,
    turnStartedAt: null,
    turnStartObservedLive: false,
    durableBoundaryTurnIds: new Set(),
    turnOrdinals: new Map(),
    pendingTerminalReconciliations: new Map(),
    assistantBuffer: "",
    reasoningBuffer: "",
    lastUsage: null,
    lifecycle: null,
    contentRevision: 0,
    storedRewrites: {},
    children: [],
    wait: null,
  };
}

export function applySessionTreeSnapshot(
  state: CodeSessionState,
  snapshot: Pick<CodeSessionSnapshot, "children" | "wait">,
): CodeSessionState {
  if (snapshot.children === undefined && snapshot.wait === undefined) {
    return state;
  }
  const children = snapshot.children ?? [];
  const wait = snapshot.wait === undefined ? null : snapshot.wait;
  if (sessionTreeUnchanged(state, children, wait)) return state;
  return { ...state, children, wait };
}

function sessionTreeUnchanged(
  state: CodeSessionState,
  children: CodeSessionState["children"],
  wait: CodeSessionState["wait"],
): boolean {
  const currentWait = state.wait ?? null;
  const nextWait = wait ?? null;
  if ((currentWait === null) !== (nextWait === null)) return false;
  if (
    currentWait &&
    nextWait &&
    (currentWait.waiting !== nextWait.waiting ||
      currentWait.total !== nextWait.total)
  ) {
    return false;
  }
  if (state.children.length !== children.length) return false;
  return state.children.every((child, index) => {
    const next = children[index];
    return (
      next !== undefined &&
      child.id === next.id &&
      child.status === next.status &&
      child.attention === next.attention &&
      child.fenced === next.fenced &&
      child.title === next.title &&
      child.workspace_id === next.workspace_id &&
      child.execution_location === next.execution_location
    );
  });
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

/** Parent-facing transcript with harness-owned child activity folded away. */
export function mainAgentTranscriptItems(
  items: readonly CodeTranscriptItem[],
): CodeTranscriptItem[] {
  return items.filter(
    (item) =>
      (item.kind !== "assistant" && item.kind !== "tool") ||
      item.parentCallId === null,
  );
}

/** One harness subagent's attributed assistant and tool activity. */
export function subagentTranscriptItems(
  items: readonly CodeTranscriptItem[],
  callId: string,
): CodeTranscriptItem[] {
  return items.filter(
    (item) =>
      (item.kind === "assistant" || item.kind === "tool") &&
      item.parentCallId === callId,
  );
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

function turnIsTerminal(
  status: CodeTurnStatus,
): status is "completed" | "failed" | "interrupted" {
  return (
    status === "completed" || status === "failed" || status === "interrupted"
  );
}

/** Whether a turn is still open: live, queued, or parked waiting on a decision. */
function turnIsOpen(status: CodeTurnStatus): boolean {
  return !turnIsTerminal(status);
}

/** Whether the journal has already ended `turnId` in this transcript. */
function turnHasEnded(state: CodeSessionState, turnId: string): boolean {
  return state.items.some(
    (item) => item.kind === "turn_boundary" && item.turnId === turnId,
  );
}

/**
 * Record a turn that a live submit received before its journal start.
 *
 * The server answers a send once the turn is accepted, so the answer says
 * `running`. It can still land after the socket has delivered the turn's
 * end, such as an engine that fails as it spawns. That turn keeps its prompt
 * and stays ended: reopening it would show a running turn with a stop button
 * that nothing will ever finish.
 */
export function applyAcceptedTurn(
  state: CodeSessionState,
  turn: CodeTurnSnapshot,
): CodeSessionState {
  const next = upsertTurnPrompt(state, turn);
  if (!turnIsOpen(turn.status) || turnHasEnded(state, turn.id)) return next;
  const continuesObservedTurn =
    state.activeTurnId === turn.id && state.turnStartObservedLive;
  return {
    ...next,
    busy: true,
    activeTurnId: turn.id,
    acceptedTurnFence: { turnId: turn.id, afterSeq: state.lastSeq },
    turnActivityRevision: state.turnActivityRevision + 1,
    turnStartedAt: turn.started_at,
    turnStartObservedLive: continuesObservedTurn,
    lifecycle: "running",
  };
}

/**
 * Apply one fetched turn without letting the snapshot choose live activity.
 *
 * Prompt fetches race the journal. Their durable user item and completed
 * boundary remain useful, but a running response may already be stale by the
 * time it arrives and must not reopen a turn or replace a newer active turn.
 */
export function applyCodeTurnSnapshot(
  state: CodeSessionState,
  turn: CodeTurnSnapshot,
): CodeSessionState {
  const next = upsertTurnPrompt(state, turn);
  if (!turnIsTerminal(turn.status)) return next;
  const durableBoundaryTurnIds = new Set(next.durableBoundaryTurnIds);
  durableBoundaryTurnIds.add(turn.id);
  const withBoundary = {
    ...next,
    durableBoundaryTurnIds,
    items: upsertTurnBoundary(
      next.items,
      {
        turnId: turn.id,
        status: turn.status,
        durationMs: durationMs(turn.started_at, turn.ended_at ?? null),
        usage: turn.usage ?? null,
        error: null,
        diffstat: turn.diffstat ?? null,
      },
      next.turnOrdinals,
    ),
  };
  if (!turn.rewrite) return withBoundary;
  const withStored = {
    ...withBoundary,
    storedRewrites: {
      ...withBoundary.storedRewrites,
      [turn.id]: turn.rewrite,
    },
  };
  return {
    ...withStored,
    items: applyTurnRewrite(withStored.items, turn.id, {
      rewrite: turn.rewrite,
      rewriteState: "rewritten",
    }),
  };
}

/**
 * Reconcile one requested turn after replay exposed an uncertain terminal.
 *
 * A completed snapshot may settle only that same active turn. If another turn
 * started while the request was in flight, the snapshot still fills durable
 * transcript data but leaves the newer activity untouched.
 */
function reconcileCodeTurnSnapshotWithPending(
  state: CodeSessionState,
  turn: CodeTurnSnapshot,
  pending: PendingTerminalReconciliation | undefined,
): CodeSessionState {
  if (!turnIsTerminal(turn.status)) {
    return applyCodeTurnSnapshot(state, turn);
  }

  let next = applyCodeTurnSnapshot(state, turn);
  if (pending) {
    const terminalMatches = pending.status === turn.status;
    next = {
      ...next,
      items: replaceTurnBoundary(
        next.items,
        {
          turnId: turn.id,
          status: turn.status,
          durationMs: durationMs(turn.started_at, turn.ended_at ?? null),
          usage: turn.usage ?? (terminalMatches ? pending.usage : null),
          error: terminalMatches ? pending.error : null,
          diffstat:
            turn.diffstat ?? (terminalMatches ? pending.diffstat : null),
        },
        next.turnOrdinals,
      ),
    };
    next = withoutPendingTerminalReconciliation(next, pending.eventSeq);
  }
  if (
    state.activeTurnId !== turn.id ||
    (state.journalTurnId !== null && state.journalTurnId !== turn.id)
  ) {
    return next;
  }
  const terminalMatches = pending?.status === turn.status;
  return {
    ...next,
    busy: false,
    activeTurnId: null,
    journalTurnId: null,
    settledTurnId: turn.id,
    acceptedTurnFence:
      state.acceptedTurnFence?.turnId === turn.id
        ? null
        : state.acceptedTurnFence,
    turnActivityRevision: state.turnActivityRevision + 1,
    turnStartedAt: null,
    turnStartObservedLive: false,
    assistantBuffer: "",
    reasoningBuffer: "",
    items: finalizeStreaming(
      finalizeStreaming(next.items, "assistant"),
      "reasoning",
    ),
    lastUsage:
      turn.usage ??
      (terminalMatches ? pending?.usage : null) ??
      (pending && !terminalMatches ? pending.previousUsage : next.lastUsage),
    lifecycle:
      next.lifecycle === "running" ? "idle" : (next.lifecycle ?? "idle"),
    contentRevision: next.contentRevision + 1,
  };
}

/** Reconcile every pending terminal that appears in one authoritative read. */
export function reconcilePendingCodeTurns(
  state: CodeSessionState,
  turns: readonly CodeTurnSnapshot[],
  requested?: readonly {
    turnId: string | null;
    eventSeq: number;
    observedSeq: number;
    observedTurnActivityRevision: number;
  }[],
): CodeSessionState {
  const orderedTurns = [...turns].sort(
    (left, right) => left.ordinal - right.ordinal,
  );
  let next = withTurnOrdinals(state, orderedTurns);
  const requests =
    requested ??
    [...state.pendingTerminalReconciliations.values()].map((pending) => ({
      turnId: pending.turnId,
      eventSeq: pending.eventSeq,
      observedSeq: state.lastSeq,
      observedTurnActivityRevision: state.turnActivityRevision,
    }));
  const currentPending = requests.flatMap((request) => {
    const pending = next.pendingTerminalReconciliations.get(request.eventSeq);
    if (!pending || pending.turnId !== request.turnId) return [];
    return [pending];
  });
  const usedTurnIds = new Set<string>();
  const assignments = new Map<string, PendingTerminalReconciliation>();

  for (const pending of currentPending) {
    if (pending.turnId === null) continue;
    const turn = orderedTurns.find(
      (candidate) => candidate.id === pending.turnId,
    );
    if (!turn) continue;
    usedTurnIds.add(turn.id);
    assignments.set(turn.id, pending);
  }

  for (const [pending, turn] of assignUnattributedTerminals(
    orderedTurns,
    currentPending.filter((candidate) => candidate.turnId === null),
    usedTurnIds,
  )) {
    assignments.set(turn.id, pending);
  }

  for (const turn of orderedTurns) {
    const pending = assignments.get(turn.id);
    next = pending
      ? reconcileCodeTurnSnapshotWithPending(next, turn, pending)
      : applyCodeTurnSnapshot(next, turn);
  }

  const responseMatchesCurrentJournal =
    requested === undefined ||
    (requested.length > 0 &&
      requested.every((request) => request.observedSeq === state.lastSeq));
  const responseMatchesCurrentActivity =
    requested === undefined ||
    (requested.length > 0 &&
      requested.every(
        (request) =>
          request.observedTurnActivityRevision === state.turnActivityRevision,
      ));
  if (
    orderedTurns.length > 0 &&
    responseMatchesCurrentJournal &&
    responseMatchesCurrentActivity
  ) {
    next = applyAuthoritativeTurnActivity(next, orderedTurns);
  }
  return next;
}

function withPendingTerminalReconciliation(
  state: CodeSessionState,
  pending: PendingTerminalReconciliation,
): CodeSessionState {
  const pendingTerminalReconciliations = new Map(
    state.pendingTerminalReconciliations,
  );
  if (pending.turnId !== null) {
    for (const [eventSeq, current] of pendingTerminalReconciliations) {
      if (current.turnId === pending.turnId) {
        pendingTerminalReconciliations.delete(eventSeq);
      }
    }
  }
  pendingTerminalReconciliations.set(pending.eventSeq, pending);
  return { ...state, pendingTerminalReconciliations };
}

function withoutPendingTerminalReconciliation(
  state: CodeSessionState,
  eventSeq: number,
): CodeSessionState {
  if (!state.pendingTerminalReconciliations.has(eventSeq)) return state;
  const pendingTerminalReconciliations = new Map(
    state.pendingTerminalReconciliations,
  );
  pendingTerminalReconciliations.delete(eventSeq);
  return { ...state, pendingTerminalReconciliations };
}

function latestPendingForTurn(
  state: CodeSessionState,
  turnId: string,
): PendingTerminalReconciliation | undefined {
  return [...state.pendingTerminalReconciliations.values()]
    .filter((pending) => pending.turnId === turnId)
    .sort((left, right) => right.eventSeq - left.eventSeq)[0];
}

function anchorUnattributedTerminals(
  state: CodeSessionState,
  nextTurnId: string,
): CodeSessionState {
  let changed = false;
  const pendingTerminalReconciliations = new Map(
    [...state.pendingTerminalReconciliations.entries()].map(
      ([eventSeq, pending]) => {
        if (pending.turnId !== null || pending.nextTurnId !== null) {
          return [eventSeq, pending] as const;
        }
        changed = true;
        return [eventSeq, { ...pending, nextTurnId }] as const;
      },
    ),
  );
  return changed ? { ...state, pendingTerminalReconciliations } : state;
}

function withTurnOrdinals(
  state: CodeSessionState,
  turns: readonly CodeTurnSnapshot[],
): CodeSessionState {
  if (turns.length === 0) return state;
  const turnOrdinals = new Map(state.turnOrdinals);
  let changed = false;
  for (const turn of turns) {
    if (turnOrdinals.get(turn.id) === turn.ordinal) continue;
    turnOrdinals.set(turn.id, turn.ordinal);
    changed = true;
  }
  return changed ? { ...state, turnOrdinals } : state;
}

function assignUnattributedTerminals(
  turns: readonly CodeTurnSnapshot[],
  pending: readonly PendingTerminalReconciliation[],
  usedTurnIds: ReadonlySet<string>,
): Array<[PendingTerminalReconciliation, CodeTurnSnapshot]> {
  const assignments: Array<[PendingTerminalReconciliation, CodeTurnSnapshot]> =
    [];
  const claimed = new Set(usedTurnIds);
  const groups = new Map<string | null, PendingTerminalReconciliation[]>();
  for (const item of pending) {
    const group = groups.get(item.nextTurnId) ?? [];
    group.push(item);
    groups.set(item.nextTurnId, group);
  }

  for (const [nextTurnId, group] of groups) {
    // Without either an exact `turn_started` identity or a following start as
    // an ordinal anchor, a capped terminal could belong to any earlier turn.
    // Do not bind it to whichever snapshot happens to be last.
    if (nextTurnId === null) continue;
    group.sort((left, right) => left.eventSeq - right.eventSeq);
    const anchor = turns.findIndex((turn) => turn.id === nextTurnId);
    if (anchor === -1) continue;
    if (anchor < group.length) continue;

    const proposed: Array<[PendingTerminalReconciliation, CodeTurnSnapshot]> =
      [];
    let valid = true;
    for (let offset = 0; offset < group.length; offset += 1) {
      const item = group[group.length - 1 - offset];
      const turn = turns[anchor - 1 - offset];
      if (
        !item ||
        !turn ||
        turnIsOpen(turn.status) ||
        turn.status !== item.status ||
        claimed.has(turn.id)
      ) {
        valid = false;
        break;
      }
      proposed.push([item, turn]);
    }
    if (!valid) continue;
    for (const assignment of proposed.reverse()) {
      claimed.add(assignment[1].id);
      assignments.push(assignment);
    }
  }
  return assignments;
}

function applyAuthoritativeTurnActivity(
  state: CodeSessionState,
  turns: readonly CodeTurnSnapshot[],
): CodeSessionState {
  const acceptedTurn = state.acceptedTurnFence
    ? turns.find((turn) => turn.id === state.acceptedTurnFence?.turnId)
    : undefined;
  if (
    state.acceptedTurnFence &&
    (!acceptedTurn || turnIsOpen(acceptedTurn.status))
  ) {
    return {
      ...state,
      lastUsage: latestTurnUsage(state, turns),
      busy: true,
      activeTurnId: state.acceptedTurnFence.turnId,
      turnStartedAt: acceptedTurn?.started_at ?? state.turnStartedAt,
      lifecycle: "running",
    };
  }
  const open = [...turns]
    .reverse()
    .find(
      (turn) =>
        turnIsOpen(turn.status) && !latestPendingForTurn(state, turn.id),
    );
  const lastUsage = latestTurnUsage(state, turns);
  if (open) {
    const activityChanged = state.activeTurnId !== open.id;
    return {
      ...state,
      lastUsage,
      busy: true,
      activeTurnId: open.id,
      journalTurnId:
        state.activeTurnId === open.id ? state.journalTurnId : null,
      turnStartedAt: open.started_at,
      turnStartObservedLive:
        state.activeTurnId === open.id && state.turnStartObservedLive,
      acceptedTurnFence: null,
      turnActivityRevision:
        state.turnActivityRevision + (activityChanged ? 1 : 0),
      lifecycle: "running",
    };
  }
  const activityChanged = state.activeTurnId !== null || state.busy;
  return {
    ...state,
    lastUsage,
    busy: false,
    activeTurnId: null,
    journalTurnId: null,
    acceptedTurnFence: null,
    turnActivityRevision:
      state.turnActivityRevision + (activityChanged ? 1 : 0),
    turnStartedAt: null,
    turnStartObservedLive: false,
    assistantBuffer: "",
    reasoningBuffer: "",
    items: finalizeStreaming(
      finalizeStreaming(state.items, "assistant"),
      "reasoning",
    ),
    lifecycle:
      state.lifecycle === "running" ? "idle" : (state.lifecycle ?? "idle"),
  };
}

function latestTurnUsage(
  state: CodeSessionState,
  turns: readonly CodeTurnSnapshot[],
): CodeUsage | null {
  for (const turn of [...turns].reverse()) {
    if (turnIsOpen(turn.status)) continue;
    if (turn.usage) return turn.usage;
    if (!state.durableBoundaryTurnIds.has(turn.id)) continue;
    const boundary = state.items.find(
      (item) => item.kind === "turn_boundary" && item.turnId === turn.id,
    );
    if (boundary?.kind === "turn_boundary" && boundary.usage) {
      return boundary.usage;
    }
  }
  return state.lastUsage;
}

/**
 * The name to show for an actor: the channel's display name, or a generic
 * origin label ("Slack user") when an adapter sent no name. The principal is
 * an internal key and must never reach the UI, and neither may the channel's
 * `external_identity`. A turn with no label renders as the session's owner,
 * which is what returning nothing means here.
 */
export function actorLabel(
  actor: TurnActor | null | undefined,
): string | undefined {
  if (actor?.display) return actor.display;
  const kind = actor?.channel_kind;
  if (kind) return `${kind.charAt(0).toUpperCase()}${kind.slice(1)} user`;
  return undefined;
}

function upsertTurnPrompt(
  state: CodeSessionState,
  turn: CodeTurnSnapshot,
): CodeSessionState {
  const turnOrdinals = new Map(state.turnOrdinals);
  turnOrdinals.set(turn.id, turn.ordinal);
  const user: CodeTranscriptItem = {
    kind: "user",
    id: userItemId(turn.id),
    turnId: turn.id,
    text: turn.user_input,
    // Every accepted turn carries its own start, live or replayed, so the
    // prompt's timestamp never depends on when this client happened to see it.
    createdAt: turn.started_at,
    actorLabel: actorLabel(turn.actor),
    trigger: turn.actor?.trigger ?? undefined,
    attachments: turn.attachments ?? [],
  };
  const hasUser = state.items.some(
    (item) => item.kind === "user" && item.turnId === turn.id,
  );
  const items = hasUser
    ? state.items.map((item) =>
        item.kind === "user" && item.turnId === turn.id ? user : item,
      )
    : insertUserBeforeTurn(state.items, user, turn.id, turnOrdinals);
  return { ...state, items, turnOrdinals };
}

/** Snapshot of durable turns, applied before the journal replays. */
export function hydrateCodeTurns(
  state: CodeSessionState,
  turns: readonly CodeTurnSnapshot[],
): CodeSessionState {
  return reconcilePendingCodeTurns(state, turns);
}

export function reduceCodeSessionEvent(
  state: CodeSessionState,
  framed: SequencedCodeEventFrame,
  deps: CodeSessionDeps,
): CodeSessionTransition {
  // A transient frame is live-only: no row holds it, so its `seq` is the
  // cursor it streamed behind rather than a position of its own. Applying it
  // must not move the cursor, and the duplicate check does not apply — the
  // journal will never hand it back. A replacement frame contains the whole
  // current assistant tail, so it replaces the buffer instead of appending.
  const transient = framed.transient === true;
  if (!transient && framed.seq <= state.lastSeq) return { state, effects: [] };
  const cappedReplayStart =
    framed.replayed === true && framed.truncated === true;
  state = {
    ...state,
    lastSeq: transient ? state.lastSeq : framed.seq,
    animateStreaming: framed.replayed !== true,
    journalTurnId: cappedReplayStart ? null : state.journalTurnId,
    settledTurnId: cappedReplayStart ? null : state.settledTurnId,
    assistantBuffer: cappedReplayStart ? "" : state.assistantBuffer,
    reasoningBuffer: cappedReplayStart ? "" : state.reasoningBuffer,
    items:
      framed.truncated === true
        ? withTruncationNotice(state.items)
        : state.items,
  };
  const event = framed.event;
  const effects: CodeSessionEffect[] = [];
  // Snapshot activity can be stale across a retained close. Historical rows
  // stay unassigned until replay itself establishes their turn.
  const attributedTurnId = activityTurnId(state, framed);

  switch (event.type) {
    case "session_started":
    case "model_reported":
      // Session metadata already comes from the durable session snapshot.
      return { state, effects };

    case "turn_resumed": {
      // Same turn, not a new one. Keep the transcript on this turn so the
      // journal reads as a resume rather than a restart.
      return {
        state: {
          ...state,
          busy: true,
          activeTurnId: event.turn_id,
          journalTurnId: event.turn_id,
          items: insertBeforeTurnBoundary(state.items, event.turn_id, {
            kind: "notice",
            id: deps.nextId(),
            level: "info",
            message: "The turn resumed after the worker restarted.",
          }),
        },
        effects,
      };
    }

    case "turn_started": {
      effects.push({ type: "turn_began", turnId: event.turn_id });
      const anchored = anchorUnattributedTerminals(state, event.turn_id);
      if (anchored !== state) {
        effects.push({ type: "turn_snapshot_needed", turnId: null });
        state = anchored;
      }
      const durableStartedAt = acceptedTurnStartedAt(
        state.items,
        event.turn_id,
      );
      const observedStartedAt =
        state.activeTurnId === event.turn_id ? state.turnStartedAt : null;
      const continuesObservedTurn =
        state.journalTurnId === event.turn_id && state.turnStartObservedLive;
      const preservesAcceptedTurn =
        replayFollowsAcceptedTurn(state, framed) &&
        state.acceptedTurnFence.turnId !== event.turn_id;
      const activityChanged =
        !preservesAcceptedTurn && state.activeTurnId !== event.turn_id;
      return {
        state: {
          ...state,
          busy: true,
          activeTurnId: preservesAcceptedTurn
            ? state.activeTurnId
            : event.turn_id,
          journalTurnId: event.turn_id,
          acceptedTurnFence: preservesAcceptedTurn
            ? state.acceptedTurnFence
            : null,
          turnActivityRevision:
            state.turnActivityRevision + (activityChanged ? 1 : 0),
          // Hydration carries the server's accepted timestamp. Keep it when
          // replay walks the same turn, and never invent a start for history
          // that the client did not observe live.
          turnStartedAt: preservesAcceptedTurn
            ? state.turnStartedAt
            : (durableStartedAt ??
              observedStartedAt ??
              (framed.replayed === true ? null : deps.now())),
          turnStartObservedLive: preservesAcceptedTurn
            ? state.turnStartObservedLive
            : framed.replayed !== true || continuesObservedTurn,
          assistantBuffer: "",
          reasoningBuffer: "",
          lifecycle: "running",
        },
        effects,
      };
    }

    case "assistant_delta": {
      const assistantBuffer =
        framed.replacement === true
          ? event.text
          : state.assistantBuffer + event.text;
      return {
        state: {
          ...state,
          assistantBuffer,
          items: upsertStreaming(
            // Prose starting means the thinking behind it ended. Left live, an
            // earlier block keeps pulsing and keeps saying "Thinking" while the
            // answer it produced is already being written underneath it.
            finalizeStreaming(state.items, "reasoning"),
            "assistant",
            assistantBuffer,
            attributedTurnId,
            null,
            deps.nextId,
          ),
        },
        effects,
      };
    }

    case "assistant_message": {
      const parentCallId = event.parent_call_id ?? null;
      const upserted = upsertStreaming(
        state.items,
        "assistant",
        event.text,
        attributedTurnId,
        parentCallId,
        deps.nextId,
      );
      return {
        state: {
          ...state,
          // Child messages are complete attributed records. They must not
          // replace the parent's delta buffer or a later parent message will
          // merge into the child's transcript.
          assistantBuffer:
            parentCallId === null ? event.text : state.assistantBuffer,
          items: finalizeStreaming(
            transient
              ? upserted
              : tagEvent(
                  upserted,
                  lastIndexOfKindForTurn(
                    upserted,
                    "assistant",
                    attributedTurnId,
                    parentCallId,
                  ),
                  framed.seq,
                ),
            "assistant",
            parentCallId,
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
            attributedTurnId,
            null,
            deps.nextId,
          ),
        },
        effects,
      };
    }

    case "tool_started": {
      const parentCallId = event.parent_call_id ?? null;
      const settled = finalizeStreaming(state.items, "assistant", parentCallId);
      // The reasoning that led to this call is done, the same way the prose
      // before it is. A subagent's call settles nothing of the parent's: its
      // own work is not what the parent was thinking about.
      const opened =
        parentCallId === null
          ? finalizeStreaming(settled, "reasoning")
          : settled;
      return {
        state: {
          ...state,
          assistantBuffer: parentCallId === null ? "" : state.assistantBuffer,
          reasoningBuffer: parentCallId === null ? "" : state.reasoningBuffer,
          items: insertBeforeTurnBoundary(opened, attributedTurnId, {
            kind: "tool",
            id: deps.nextId(),
            turnId: attributedTurnId,
            callId: event.call_id,
            parentCallId,
            name: event.name,
            detail: event.detail,
            status: "running",
            preview: "",
            startedAt: framed.replayed ? null : deps.now(),
            durationMs: null,
            seqs: transient ? [] : [framed.seq],
          }),
        },
        effects,
      };
    }

    case "tool_completed": {
      const parentCallId = event.parent_call_id ?? null;
      return {
        state: {
          ...state,
          items: state.items.map((item) =>
            item.kind === "tool" &&
            item.callId === event.call_id &&
            item.parentCallId === parentCallId
              ? {
                  ...item,
                  status: event.outcome,
                  preview: event.preview,
                  detail: mergeToolDetail(item.detail, event.detail),
                  durationMs: framed.replayed
                    ? null
                    : durationMs(item.startedAt, deps.now()),
                  seqs: transient
                    ? item.seqs
                    : [...(item.seqs ?? []), framed.seq],
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
            : insertBeforeTurnBoundary(state.items, attributedTurnId, {
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
        event.decision.type === "plan_decided" && !event.decision.approve
          ? "denied"
          : RESOLVED_APPROVAL_STATE[event.decision.type];
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
          items: insertBeforeTurnBoundary(state.items, attributedTurnId, {
            kind: "notice",
            id: deps.nextId(),
            level: event.level,
            message: event.message,
          }),
        },
        effects,
      };
    }

    case "credential_refused": {
      // A push the machine could not credential: one notice row naming the
      // reason and the remedy, where the engine's own error lands beside it.
      return {
        state: {
          ...state,
          items: insertBeforeTurnBoundary(state.items, attributedTurnId, {
            kind: "notice",
            id: deps.nextId(),
            level: "warning",
            message: credentialRefusalNotice(event),
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

    case "checkpoint_restored": {
      // A restore runs between turns and belongs to none of them, so it
      // lands where the engine's own between-turn activity does: after the
      // turn the journal last ended, before any prompt already placed for a
      // later turn. Its second row, how it ended, updates the first in place.
      const id = `restore:${event.restore_id}`;
      const row = {
        kind: "restore" as const,
        id,
        restoreId: event.restore_id,
        target: event.target,
        turnOrdinal:
          event.target.kind === "before_turn"
            ? (state.turnOrdinals.get(event.target.turn_id) ?? null)
            : null,
        diffstat: event.diffstat,
        status: event.status,
        error: event.error ?? null,
      };
      const existing = state.items.find((item) => item.id === id);
      if (existing) {
        if (
          existing.kind === "restore" &&
          existing.status === row.status &&
          existing.error === row.error
        ) {
          return { state, effects };
        }
        return {
          state: {
            ...state,
            items: state.items.map((item) => (item.id === id ? row : item)),
            contentRevision: state.contentRevision + 1,
          },
          effects,
        };
      }
      return {
        state: {
          ...state,
          items: insertBackgroundItem(state, framed, row),
          contentRevision: state.contentRevision + 1,
        },
        effects,
      };
    }

    case "review_finished": {
      // A review runs beside the conversation, never in a turn, so its row
      // lands where the engine's own between-turn activity does. One row
      // per review; a replayed frame changes nothing.
      const id = `review:${event.review_id}`;
      if (state.items.some((item) => item.id === id)) {
        return { state, effects };
      }
      const row = {
        kind: "review" as const,
        id,
        reviewId: event.review_id,
        harness: event.harness,
        model: event.model ?? null,
        turnId: event.turn_id ?? null,
        turnOrdinal: event.turn_id
          ? (state.turnOrdinals.get(event.turn_id) ?? null)
          : null,
        outcome: event.outcome,
        findings: event.findings,
      };
      return {
        state: {
          ...state,
          items: insertBackgroundItem(state, framed, row),
        },
        effects,
      };
    }

    case "user_steered": {
      return {
        state: {
          ...state,
          items: insertBeforeTurnBoundary(state.items, attributedTurnId, {
            kind: "steer",
            id: deps.nextId(),
            turnId: attributedTurnId,
            text: event.text,
            seqs: transient ? [] : [framed.seq],
          }),
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
            attributedTurnId,
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
    case "turn_refused":
    case "turn_failed":
    case "turn_interrupted": {
      // A refusal is a completed turn whose answer was to decline: the
      // engine's turn row says so, and the code view ends the turn on it.
      const status =
        event.type === "turn_completed" || event.type === "turn_refused"
          ? "completed"
          : event.type === "turn_failed"
            ? "failed"
            : "interrupted";
      const usage =
        event.type === "turn_completed" || event.type === "turn_refused"
          ? event.usage
          : null;
      const error = event.type === "turn_failed" ? event.error.message : null;
      const diffstat =
        event.type === "turn_completed"
          ? (event.checkpoint?.diffstat ?? null)
          : null;
      const turnId =
        state.journalTurnId ??
        (framed.replayed === true ? null : state.activeTurnId);
      // Terminal rows carry no turn id. A replay may start after the matching
      // `turn_started`, so snapshot activity alone cannot safely attribute it.
      if (turnId === null) {
        if (framed.replayed === true) {
          state = withPendingTerminalReconciliation(state, {
            eventSeq: framed.seq,
            turnId: null,
            candidateTurnId: state.activeTurnId,
            nextTurnId: null,
            status,
            usage,
            error,
            diffstat,
            previousUsage: state.lastUsage,
          });
          effects.push({
            type: "turn_snapshot_needed",
            turnId: null,
          });
          effects.push({ type: "turn_resolved" });
          return {
            state: {
              ...state,
              contentRevision: state.contentRevision + 1,
            },
            effects,
          };
        }
        // A reader can attach after the matching start. The terminal still
        // proves that the worktree may have changed, even when no transcript
        // turn can be named safely.
        effects.push({ type: "turn_resolved" });
        return {
          state: { ...state, contentRevision: state.contentRevision + 1 },
          effects,
        };
      }
      effects.push({ type: "turn_resolved" });
      const hasDurableBoundary = state.durableBoundaryTurnIds.has(turnId);
      const needsSnapshot = framed.replayed === true && !hasDurableBoundary;
      if (needsSnapshot) {
        state = withPendingTerminalReconciliation(state, {
          eventSeq: framed.seq,
          turnId,
          candidateTurnId: turnId,
          nextTurnId: null,
          status,
          usage,
          error,
          diffstat,
          previousUsage: state.lastUsage,
        });
        effects.push({ type: "turn_snapshot_needed", turnId });
      } else {
        const pending = latestPendingForTurn(state, turnId);
        if (pending) {
          state = withoutPendingTerminalReconciliation(state, pending.eventSeq);
        }
      }
      const canMeasureTurn =
        state.activeTurnId === turnId &&
        state.turnStartedAt !== null &&
        (framed.replayed !== true || state.turnStartObservedLive);
      const turnDuration =
        canMeasureTurn && !hasDurableBoundary
          ? durationMs(state.turnStartedAt, deps.now())
          : null;
      const resolvesActiveTurn =
        state.activeTurnId === turnId &&
        !(
          replayFollowsAcceptedTurn(state, framed) &&
          state.acceptedTurnFence.turnId !== turnId
        );
      const finalized = finalizeStreaming(
        finalizeStreaming(state.items, "assistant"),
        "reasoning",
      );
      return {
        state: {
          ...state,
          busy: resolvesActiveTurn ? false : state.busy,
          lastUsage: needsSnapshot
            ? state.lastUsage
            : (usage ?? state.lastUsage),
          assistantBuffer: "",
          reasoningBuffer: "",
          items: turnId
            ? upsertTurnBoundary(
                finalized,
                {
                  turnId,
                  status,
                  // A snapshot boundary owns completed timing. Replayed or
                  // duplicate terminal frames can enrich it, but cannot
                  // replace its server-derived duration with client time.
                  durationMs: turnDuration,
                  usage,
                  error,
                  diffstat,
                },
                state.turnOrdinals,
              )
            : finalized,
          activeTurnId: resolvesActiveTurn ? null : state.activeTurnId,
          journalTurnId:
            state.journalTurnId === turnId ? null : state.journalTurnId,
          settledTurnId: turnId,
          acceptedTurnFence: resolvesActiveTurn
            ? null
            : state.acceptedTurnFence,
          turnActivityRevision:
            state.turnActivityRevision + (resolvesActiveTurn ? 1 : 0),
          turnStartedAt: resolvesActiveTurn ? null : state.turnStartedAt,
          turnStartObservedLive: resolvesActiveTurn
            ? false
            : state.turnStartObservedLive,
          lifecycle: resolvesActiveTurn ? "idle" : state.lifecycle,
          // A failed or interrupted turn still leaves whatever the engine
          // wrote before it stopped, so every resolution is a content change.
          contentRevision: state.contentRevision + 1,
        },
        effects,
      };
    }

    case "stream_interrupted":
    case "tool_args_delta":
    case "task_plan_updated":
    case "context_truncated":
    case "compaction_started":
    case "compaction_finished":
      // Only the internal harness emits these, and code mode filters it out.
      return { state, effects };

    case "session_tree":
      return {
        state: {
          ...state,
          children: event.children,
          wait: event.wait,
        },
        effects,
      };

    case "background_activity":
      return {
        state: reduceBackgroundActivity(state, event.event, framed, deps),
        effects,
      };

    default:
      return { state, effects };
  }
}

/**
 * Apply what the engine did on its own, such as the turn Claude Code runs
 * when a background task ends.
 *
 * It belongs to no turn of the person's, even when it arrives while one
 * waits for the engine: it never joins that turn's reply or its streaming
 * text, and a reload places it the same way. A call it runs can change the
 * worktree, so its end counts as a content change.
 */
function reduceBackgroundActivity(
  state: CodeSessionState,
  event: CodeEvent,
  framed: SequencedCodeEventFrame,
  deps: CodeSessionDeps,
): CodeSessionState {
  switch (event.type) {
    case "assistant_message":
      return {
        ...state,
        items: insertBackgroundItem(state, framed, {
          kind: "assistant",
          id: deps.nextId(),
          turnId: null,
          parentCallId: event.parent_call_id ?? null,
          text: event.text,
          streaming: false,
          background: true,
          seqs: framed.transient === true ? [] : [framed.seq],
        }),
      };
    case "tool_started":
      return {
        ...state,
        items: insertBackgroundItem(state, framed, {
          kind: "tool",
          id: deps.nextId(),
          turnId: null,
          callId: event.call_id,
          parentCallId: event.parent_call_id ?? null,
          name: event.name,
          detail: event.detail,
          status: "running",
          preview: "",
          startedAt: framed.replayed ? null : deps.now(),
          durationMs: null,
          background: true,
          seqs: framed.transient === true ? [] : [framed.seq],
        }),
      };
    case "tool_completed": {
      const parentCallId = event.parent_call_id ?? null;
      return {
        ...state,
        items: state.items.map((item) =>
          item.kind === "tool" &&
          item.background === true &&
          item.callId === event.call_id &&
          item.parentCallId === parentCallId
            ? {
                ...item,
                status: event.outcome,
                preview: event.preview,
                detail: mergeToolDetail(item.detail, event.detail),
                durationMs: framed.replayed
                  ? null
                  : durationMs(item.startedAt, deps.now()),
                seqs:
                  framed.transient === true
                    ? item.seqs
                    : [...(item.seqs ?? []), framed.seq],
              }
            : item,
        ),
        contentRevision: state.contentRevision + 1,
      };
    }
    case "harness_notice":
      return {
        ...state,
        items: insertBackgroundItem(state, framed, {
          kind: "notice",
          id: deps.nextId(),
          level: event.level,
          message: event.message,
        }),
      };
    case "file_changed":
      return { ...state, contentRevision: state.contentRevision + 1 };
    default:
      return state;
  }
}

/**
 * Place one item of the engine's own activity where it happened: before
 * the message of the person's turn that is waiting for the engine, or after
 * the turn the journal last ended and before the next one.
 */
function insertBackgroundItem(
  state: CodeSessionState,
  framed: SequencedCodeEventFrame,
  item: CodeTranscriptItem,
): CodeTranscriptItem[] {
  const items = state.items;
  const openTurnId =
    state.journalTurnId ??
    (framed.replayed === true ? null : state.activeTurnId);
  const insertAt = (index: number) => [
    ...items.slice(0, index),
    item,
    ...items.slice(index),
  ];
  if (openTurnId !== null) {
    const message = items.findIndex(
      (candidate) =>
        candidate.kind === "user" && candidate.turnId === openTurnId,
    );
    if (message !== -1) return insertAt(message);
  }
  if (state.settledTurnId !== null) {
    const settled = items.findIndex(
      (candidate) =>
        candidate.kind === "turn_boundary" &&
        candidate.turnId === state.settledTurnId,
    );
    const next =
      settled === -1
        ? -1
        : items.findIndex(
            (candidate, index) => index > settled && candidate.kind === "user",
          );
    if (next !== -1) return insertAt(next);
  }
  return [...items, item];
}

function replayFollowsAcceptedTurn(
  state: CodeSessionState,
  framed: SequencedCodeEventFrame,
): state is CodeSessionState & {
  acceptedTurnFence: { turnId: string; afterSeq: number };
} {
  return (
    framed.replayed === true &&
    state.acceptedTurnFence !== null &&
    framed.seq > state.acceptedTurnFence.afterSeq
  );
}

/**
 * Which turn a frame's rows belong on.
 *
 * Replay follows the journal's `turn_started`. Live frames follow the active
 * turn, except in the window after a submit is accepted and before the engine
 * names that turn: leftover activity from the turn that just finished must
 * not land on the new prompt. That is what made a delayed first reply appear
 * under the next user message.
 */
function activityTurnId(
  state: CodeSessionState,
  framed: SequencedCodeEventFrame,
): string | null {
  if (framed.replayed === true) return state.journalTurnId;
  if (state.acceptedTurnFence !== null && state.journalTurnId === null) {
    return lastBoundaryTurnId(state.items) ?? state.activeTurnId;
  }
  return state.activeTurnId ?? lastBoundaryTurnId(state.items);
}

function lastBoundaryTurnId(
  items: readonly CodeTranscriptItem[],
): string | null {
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index];
    if (item.kind === "turn_boundary" && item.turnId) return item.turnId;
  }
  return null;
}

/**
 * Say that the replay started partway through.
 *
 * The server caps how much journal one connect replays and flags the first
 * frame of a capped window. Without a line saying so, a long session would
 * quietly open on its middle and read as if that were the beginning.
 */
function withTruncationNotice(
  items: CodeTranscriptItem[],
): CodeTranscriptItem[] {
  if (items.some((item) => item.id === TRUNCATED_NOTICE_ID)) return items;
  const notice: CodeTranscriptItem = {
    kind: "notice",
    id: TRUNCATED_NOTICE_ID,
    level: "info",
    message: "Earlier history in this conversation is not shown.",
  };
  return items.length === 0 ? [notice] : [...items, notice];
}

/** Fixed so a reconnect does not stack a second copy of the same line. */
const TRUNCATED_NOTICE_ID = "notice:truncated-replay";

function upsertStreaming(
  items: CodeTranscriptItem[],
  kind: "assistant" | "reasoning",
  text: string,
  turnId: string | null,
  parentCallId: string | null,
  nextId: () => string,
): CodeTranscriptItem[] {
  const last = items[items.length - 1];
  if (
    last &&
    streamingItemMatches(last, kind, turnId, parentCallId) &&
    last.streaming
  ) {
    return [...items.slice(0, -1), { ...last, text }];
  }
  const existing = lastIndexOfKindForTurn(items, kind, turnId, parentCallId);
  if (existing !== -1) {
    const prev = items[existing];
    if (prev.kind !== kind) {
      return insertBeforeTurnBoundary(
        items,
        turnId,
        streamingItem(kind, nextId(), turnId, parentCallId, text),
      );
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
      return insertBeforeTurnBoundary(
        items,
        turnId,
        streamingItem(kind, nextId(), turnId, parentCallId, suffix),
      );
    }
  }
  if (text.trim() === "") return items;
  return insertBeforeTurnBoundary(
    items,
    turnId,
    streamingItem(kind, nextId(), turnId, parentCallId, text),
  );
}

function streamingItem(
  kind: "assistant" | "reasoning",
  id: string,
  turnId: string | null,
  parentCallId: string | null,
  text: string,
): CodeTranscriptItem {
  return kind === "assistant"
    ? {
        kind,
        id,
        turnId,
        parentCallId,
        text,
        streaming: true,
      }
    : { kind, id, turnId, text, streaming: true };
}

function lastIndexOfKindForTurn(
  items: readonly CodeTranscriptItem[],
  kind: "assistant" | "reasoning",
  turnId: string | null,
  parentCallId: string | null,
): number {
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index];
    if (streamingItemMatches(item, kind, turnId, parentCallId)) {
      return index;
    }
  }
  return -1;
}

/**
 * Record on the row at `index` that journal event `seq` wrote it. A search
 * hit names a code event by its sequence number, and this is how the
 * transcript finds the row that event drew.
 */
function tagEvent(
  items: CodeTranscriptItem[],
  index: number,
  seq: number,
): CodeTranscriptItem[] {
  const item = items[index];
  if (!item || (item.kind !== "assistant" && item.kind !== "steer")) {
    return items;
  }
  if (item.seqs?.includes(seq)) return items;
  const tagged = { ...item, seqs: [...(item.seqs ?? []), seq] };
  return items.map((candidate, at) => (at === index ? tagged : candidate));
}

/**
 * The row a journal event drew, or a turn's prompt when the event is the
 * turn's own input. `null` when the transcript does not hold it: the event
 * is older than the part of the journal this transcript replayed.
 */
export function itemForEvent(
  items: readonly CodeTranscriptItem[],
  target: { eventSeq?: number; turnId?: string },
): CodeTranscriptItem | null {
  if (target.eventSeq !== undefined) {
    const seq = target.eventSeq;
    return (
      items.find(
        (item) =>
          (item.kind === "assistant" ||
            item.kind === "tool" ||
            item.kind === "steer") &&
          item.seqs?.includes(seq) === true,
      ) ?? null
    );
  }
  if (target.turnId) {
    return (
      items.find(
        (item) => item.kind === "user" && item.turnId === target.turnId,
      ) ?? null
    );
  }
  return null;
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
  parentCallId?: string | null,
): CodeTranscriptItem[] {
  return items.map((item) =>
    item.kind === kind &&
    item.streaming &&
    (parentCallId === undefined ||
      streamingItemMatches(item, kind, item.turnId, parentCallId))
      ? { ...item, streaming: false }
      : item,
  );
}

function streamingItemMatches(
  item: CodeTranscriptItem,
  kind: "assistant" | "reasoning",
  turnId: string | null,
  parentCallId: string | null,
): item is Extract<CodeTranscriptItem, { kind: "assistant" | "reasoning" }> {
  if (item.kind !== kind || item.turnId !== turnId) return false;
  // The engine's own text is never the person's reply, whatever turn a
  // frame is attributed to.
  if (item.kind === "assistant" && item.background === true) return false;
  return item.kind !== "assistant" || item.parentCallId === parentCallId;
}

function insertUserBeforeTurn(
  items: CodeTranscriptItem[],
  user: Extract<CodeTranscriptItem, { kind: "user" }>,
  turnId: string,
  turnOrdinals: ReadonlyMap<string, number>,
): CodeTranscriptItem[] {
  let index = items.findIndex(
    (item) => "turnId" in item && item.turnId === turnId,
  );
  if (index === -1) {
    const ordinal = turnOrdinals.get(turnId);
    if (ordinal !== undefined) {
      index = items.findIndex((item) => {
        if (!("turnId" in item) || item.turnId === null) return false;
        const itemOrdinal = turnOrdinals.get(item.turnId);
        return itemOrdinal !== undefined && itemOrdinal > ordinal;
      });
    }
  }
  if (index === -1) return [...items, user];
  return [...items.slice(0, index), user, ...items.slice(index)];
}

function acceptedTurnStartedAt(
  items: readonly CodeTranscriptItem[],
  turnId: string,
): string | null {
  const user = items.find(
    (item) => item.kind === "user" && item.turnId === turnId,
  );
  return user?.kind === "user" ? user.createdAt : null;
}

function upsertTurnBoundary(
  items: CodeTranscriptItem[],
  boundary: {
    turnId: string;
    status: "completed" | "failed" | "interrupted";
    durationMs: number | null;
    usage: CodeUsage | null;
    error: string | null;
    diffstat: Diffstat | null;
  },
  turnOrdinals: ReadonlyMap<string, number>,
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
      candidate.kind === "turn_boundary" &&
      candidate.turnId === boundary.turnId,
  );
  if (index === -1) {
    return insertTurnBoundary(items, item, turnOrdinals);
  }
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

function replaceTurnBoundary(
  items: CodeTranscriptItem[],
  boundary: {
    turnId: string;
    status: "completed" | "failed" | "interrupted";
    durationMs: number | null;
    usage: CodeUsage | null;
    error: string | null;
    diffstat: Diffstat | null;
  },
  turnOrdinals: ReadonlyMap<string, number>,
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
      candidate.kind === "turn_boundary" &&
      candidate.turnId === boundary.turnId,
  );
  if (index === -1) {
    return insertTurnBoundary(items, item, turnOrdinals);
  }
  return [...items.slice(0, index), item, ...items.slice(index + 1)];
}

function insertTurnBoundary(
  items: CodeTranscriptItem[],
  boundary: Extract<CodeTranscriptItem, { kind: "turn_boundary" }>,
  turnOrdinals: ReadonlyMap<string, number>,
): CodeTranscriptItem[] {
  const ordinal = boundary.turnId
    ? turnOrdinals.get(boundary.turnId)
    : undefined;
  if (ordinal === undefined) return [...items, boundary];
  const index = items.findIndex((item) => {
    if (!("turnId" in item) || item.turnId === null) return false;
    const itemOrdinal = turnOrdinals.get(item.turnId);
    return itemOrdinal !== undefined && itemOrdinal > ordinal;
  });
  if (index === -1) return [...items, boundary];
  return [...items.slice(0, index), boundary, ...items.slice(index)];
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

/**
 * Stamp a recap onto the last parent assistant row of a turn.
 *
 * The journal text stays on `text`. A later `rewriting` or `failed` notice
 * does not clear a recap the turn snapshot already stored.
 */
export function applyTurnRewrite(
  items: CodeTranscriptItem[],
  turnId: string,
  rewrite: {
    rewrite?: string;
    rewriteState: "rewriting" | "rewritten" | "failed";
  },
): CodeTranscriptItem[] {
  let last = -1;
  for (let index = 0; index < items.length; index += 1) {
    const item = items[index];
    if (
      item?.kind === "assistant" &&
      item.turnId === turnId &&
      item.parentCallId === null
    ) {
      last = index;
    }
  }
  if (last < 0) return items;
  const item = items[last];
  if (item?.kind !== "assistant") return items;
  const nextRewrite = rewrite.rewrite ?? item.rewrite;
  const nextState =
    item.rewrite &&
    rewrite.rewrite === undefined &&
    rewrite.rewriteState !== "rewritten"
      ? (item.rewriteState ?? "rewritten")
      : rewrite.rewriteState;
  if (item.rewrite === nextRewrite && item.rewriteState === nextState) {
    return items;
  }
  const next = items.slice();
  next[last] = {
    ...item,
    rewrite: nextRewrite,
    rewriteState: nextState,
  };
  return next;
}

/** Stamp stored recaps onto closing messages that exist after replay. */
export function applyStoredRewrites(state: CodeSessionState): CodeSessionState {
  let items = state.items;
  let changed = false;
  for (const [turnId, rewrite] of Object.entries(state.storedRewrites)) {
    const next = applyTurnRewrite(items, turnId, {
      rewrite,
      rewriteState: "rewritten",
    });
    if (next !== items) {
      items = next;
      changed = true;
    }
  }
  return changed ? { ...state, items } : state;
}

/**
 * What a refused push says, once and in plain words.
 *
 * The event's message is what the machine told the git helper, and its
 * remediation is a sentence written for every surface. Joined, they said the
 * same instruction twice, and the connection case in the gateway's own terms,
 * so the reason picks the words here, as the event intends.
 */
export function credentialRefusalNotice(event: {
  reason: "connection_ended" | "not_connected" | "forge_refused";
  message: string;
  remediation: string;
}): string {
  switch (event.reason) {
    case "connection_ended":
      return "Push refused: this session's Slack connection ended when it was revoked or replaced by a newer one. Reconnect the session from Slack, then push again.";
    case "not_connected":
      // The message names where to connect, with the link when there is one.
      return `Push refused. ${asSentence(event.message)} Then push again.`;
    case "forge_refused":
      return `Push refused: ${asSentence(event.message)} If this names the deployment, ask an administrator.`;
  }
}

/** A message ends in a full stop before the next sentence starts. */
function asSentence(message: string): string {
  const trimmed = message.trim();
  return /[.!?]$/.test(trimmed) ? trimmed : `${trimmed}.`;
}
