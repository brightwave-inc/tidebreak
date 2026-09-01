import type {
  Attention,
  CodeSessionDigest,
  CodeUpdateNotice,
} from "../generated/wire";

export type UpdatesState = {
  byId: Record<string, CodeSessionDigest>;
  order: string[];
  /**
   * True only after a snapshot action has landed. An empty list is
   * trustworthy only then; a live socket with no snapshot still means
   * "loading".
   */
  snapshotReceived: boolean;
};

export const EMPTY_UPDATES: UpdatesState = {
  byId: {},
  order: [],
  snapshotReceived: false,
};

export type UpdatesAction =
  | { type: "snapshot"; sessions: CodeSessionDigest[] }
  | { type: "digest"; digest: CodeSessionDigest }
  | { type: "reset" };

export function noticeToAction(
  notice: CodeUpdateNotice,
): UpdatesAction | null {
  if (notice.type === "snapshot") {
    return { type: "snapshot", sessions: notice.sessions };
  }
  if (notice.type === "digest") {
    return {
      type: "digest",
      digest: {
        workspace: notice.workspace,
        session: notice.session,
        kind: notice.kind,
        ...(notice.harness_kind !== undefined
          ? { harness_kind: notice.harness_kind }
          : {}),
        lifecycle: notice.lifecycle,
        attention: notice.attention,
        title: notice.title,
        turn_count: notice.turn_count,
        ...(notice.activity !== undefined ? { activity: notice.activity } : {}),
        ...(notice.pr_state !== undefined ? { pr_state: notice.pr_state } : {}),
        ...(notice.pr_count !== undefined ? { pr_count: notice.pr_count } : {}),
        ...(notice.watch_state !== undefined
          ? { watch_state: notice.watch_state }
          : {}),
        ...(notice.watch_detail !== undefined
          ? { watch_detail: notice.watch_detail }
          : {}),
        ...(notice.watch_cycles !== undefined
          ? { watch_cycles: notice.watch_cycles }
          : {}),
        ...(notice.subagents !== undefined
          ? { subagents: notice.subagents }
          : {}),
        ...(notice.recap !== undefined ? { recap: notice.recap } : {}),
      },
    };
  }
  return null;
}

export function reduceUpdates(
  state: UpdatesState,
  action: UpdatesAction,
): UpdatesState {
  switch (action.type) {
    case "reset":
      return EMPTY_UPDATES;
    case "snapshot": {
      const byId: Record<string, CodeSessionDigest> = {};
      const order: string[] = [];
      for (const digest of action.sessions) {
        byId[digest.session] = digest;
        order.push(digest.session);
      }
      return {
        byId,
        order: sortSessionIds(byId, order),
        snapshotReceived: true,
      };
    }
    case "digest": {
      const digest = action.digest;
      const byId = { ...state.byId, [digest.session]: digest };
      const order = state.order.includes(digest.session)
        ? state.order
        : [digest.session, ...state.order];
      return {
        byId,
        order: sortSessionIds(byId, order),
        snapshotReceived: state.snapshotReceived,
      };
    }
  }
}

function sortSessionIds(
  byId: Record<string, CodeSessionDigest>,
  order: string[],
): string[] {
  return [...order].sort((leftId, rightId) => {
    const left = byId[leftId];
    const right = byId[rightId];
    if (!left || !right) return 0;
    const urgency = digestUrgency(left) - digestUrgency(right);
    if (urgency !== 0) return urgency;
    if (left.turn_count !== right.turn_count) {
      return right.turn_count - left.turn_count;
    }
    return order.indexOf(leftId) - order.indexOf(rightId);
  });
}

function digestUrgency(digest: CodeSessionDigest): number {
  if (digest.lifecycle === "ended") return 4;
  if (digest.attention.state.type === "needs_you") return 0;
  if (digest.lifecycle === "running") return 1;
  if (digest.attention.state.type === "done_unreviewed") return 2;
  return 3;
}

export function listedSessions(state: UpdatesState): CodeSessionDigest[] {
  return state.order
    .map((id) => state.byId[id])
    .filter((digest): digest is CodeSessionDigest => digest !== undefined);
}

/**
 * Badge when a human should look: waiting approval/question, stalled/fenced,
 * or a finished turn that has not been reviewed.
 */
export function attentionBadgeLabel(
  attention: Attention | undefined,
): string | null {
  if (!attention) return null;
  switch (attention.state.type) {
    case "needs_you":
      return attention.state.prompt || "Needs you";
    case "stalled":
      return "Stalled";
    case "fenced":
      return "Fenced";
    case "done_unreviewed":
      return "Done";
    case "working":
    case "idle":
    case "manual":
      return null;
  }
}

export function lifecycleLabel(lifecycle: CodeSessionDigest["lifecycle"]): string {
  switch (lifecycle) {
    case "created":
      return "Created";
    case "idle":
      return "Idle";
    case "running":
      return "Running";
    case "fenced":
      return "Fenced";
    case "ended":
      return "Ended";
  }
}

export function harnessLabel(kind: string | undefined): string {
  switch (kind) {
    case "claude_code":
      return "Claude Code";
    case "codex":
      return "Codex CLI";
    case "opencode":
      return "opencode";
    case "grok":
      return "Grok CLI";
    case "internal":
      return "Tidebreak";
    default:
      return kind ?? "Unknown harness";
  }
}

export function isCodeUpdateNotice(value: unknown): value is CodeUpdateNotice {
  if (!value || typeof value !== "object") return false;
  const type = (value as { type?: unknown }).type;
  return (
    type === "snapshot" ||
    type === "digest" ||
    type === "terminal_activity" ||
    type === "clone_progress" ||
    type === "harness_install" ||
    type === "delivery"
  );
}
