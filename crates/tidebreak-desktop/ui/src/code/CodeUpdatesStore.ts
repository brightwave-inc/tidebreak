import { create } from "zustand";

import type { ApiClient } from "../api/client";
import type {
  Attention,
  CodeCloneJobSnapshot,
  CodeHarnessInstallSnapshot,
  CodeSessionDigest,
  CodeSessionLifecycle,
  CodeUpdateNotice,
  HarnessKind,
  PullRequestDigest,
} from "../api/types";
import {
  INITIAL_RECONNECT_DELAY_MS,
  MAX_RECONNECT_DELAY_MS,
  nextReconnectDelay,
} from "../ChatSessionController";
import { requestUserAttention } from "../host";

/**
 * Install-wide digest store fed by `WS /code/updates`.
 *
 * List surfaces read lifecycle, attention, and PR state from here. The
 * socket restates a full snapshot on every connect, so a dropped notice is
 * cheap.
 */

export type CodeUpdatesState = {
  /** The interactive session's digest, one per workspace. Never a watch. */
  byWorkspace: Record<string, CodeSessionDigest>;
  /**
   * Watch digests, keyed workspace → session. Children beside the
   * conversation, never in its slot — ADR 0050's rule, kept by construction:
   * the two maps have disjoint sources.
   */
  childrenByWorkspace: Record<string, Record<string, CodeSessionDigest>>;
  cloneJobs: Record<string, CodeCloneJobSnapshot>;
  /**
   * Warm harness installs, keyed by engine. The New Workspace dialog starts
   * them and reads their phase from here; nothing else depends on them, and a
   * reconnect drops them because the doctor report is the durable answer for
   * what is installed.
   */
  harnessInstalls: Partial<Record<HarnessKind, CodeHarnessInstallSnapshot>>;
  viewedWorkspaceId: string | null;
};

export type CodeUpdatesAction =
  | { type: "snapshot"; sessions: CodeSessionDigest[] }
  | { type: "digest"; digest: CodeSessionDigest }
  | { type: "clone_progress"; job: CodeCloneJobSnapshot }
  | { type: "harness_install"; install: CodeHarnessInstallSnapshot }
  | { type: "view"; workspaceId: string | null }
  | { type: "reset" };

const EMPTY: CodeUpdatesState = {
  byWorkspace: {},
  childrenByWorkspace: {},
  cloneJobs: {},
  harnessInstalls: {},
  viewedWorkspaceId: null,
};

export function reduceCodeUpdates(
  state: CodeUpdatesState,
  action: CodeUpdatesAction,
): CodeUpdatesState {
  switch (action.type) {
    case "snapshot": {
      // The snapshot restates every live session, so both maps rebuild from
      // scratch — a reconnect self-heals a missed end notice.
      const byWorkspace: Record<string, CodeSessionDigest> = {};
      const childrenByWorkspace: Record<
        string,
        Record<string, CodeSessionDigest>
      > = {};
      for (const digest of action.sessions) {
        if (digest.kind === "watch") {
          (childrenByWorkspace[digest.workspace] ??= {})[digest.session] =
            digest;
        } else {
          byWorkspace[digest.workspace] = digest;
        }
      }
      return { ...state, byWorkspace, childrenByWorkspace };
    }
    case "digest": {
      if (action.digest.kind === "watch") {
        return {
          ...state,
          childrenByWorkspace: upsertChild(
            state.childrenByWorkspace,
            action.digest,
          ),
        };
      }
      return {
        ...state,
        byWorkspace: {
          ...state.byWorkspace,
          [action.digest.workspace]: action.digest,
        },
      };
    }
    case "clone_progress":
      return {
        ...state,
        cloneJobs: {
          ...state.cloneJobs,
          [action.job.id]: action.job,
        },
      };
    case "harness_install":
      return {
        ...state,
        harnessInstalls: {
          ...state.harnessInstalls,
          [action.install.kind]: action.install,
        },
      };
    case "view":
      return { ...state, viewedWorkspaceId: action.workspaceId };
    case "reset":
      return { ...EMPTY, viewedWorkspaceId: state.viewedWorkspaceId };
  }
}

function upsertChild(
  children: CodeUpdatesState["childrenByWorkspace"],
  digest: CodeSessionDigest,
): CodeUpdatesState["childrenByWorkspace"] {
  const forWorkspace = { ...children[digest.workspace] };
  if (digest.lifecycle === "ended") {
    // An ended watch leaves the rail; the snapshot on reconnect would drop
    // it anyway, this just does it without waiting for one.
    delete forWorkspace[digest.session];
  } else {
    forWorkspace[digest.session] = digest;
  }
  if (Object.keys(forWorkspace).length === 0) {
    const next = { ...children };
    delete next[digest.workspace];
    return next;
  }
  return { ...children, [digest.workspace]: forWorkspace };
}

/** A workspace's watch digests, in a stable order for rendering. */
export function watchChildren(
  state: Pick<CodeUpdatesState, "childrenByWorkspace">,
  workspaceId: string,
): CodeSessionDigest[] {
  const children = state.childrenByWorkspace[workspaceId];
  if (!children) return [];
  return Object.values(children).sort((left, right) =>
    left.session.localeCompare(right.session),
  );
}

/** True when a digest transition should poke the OS attention affordance. */
export function shouldRequestOsAttention(
  previous: Attention | undefined,
  next: Attention,
  workspaceId: string,
  viewedWorkspaceId: string | null,
): boolean {
  if (viewedWorkspaceId === workspaceId) return false;
  if (!isStructuredNeed(next)) return false;
  return !previous || !isStructuredNeed(previous);
}

function isStructuredNeed(attention: Attention): boolean {
  return (
    attention.state.type === "needs_you" && attention.state.source === "structured"
  );
}

export function noticeToAction(notice: CodeUpdateNotice): CodeUpdatesAction | null {
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
        lifecycle: notice.lifecycle,
        attention: notice.attention,
        title: notice.title,
        turn_count: notice.turn_count,
        ...(notice.pr_state !== undefined ? { pr_state: notice.pr_state } : {}),
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
      },
    };
  }
  if (notice.type === "clone_progress") {
    return {
      type: "clone_progress",
      job: {
        id: notice.job,
        phase: notice.phase,
        done: notice.done,
        ...(notice.percent !== undefined ? { percent: notice.percent } : {}),
        ...(notice.error !== undefined ? { error: notice.error } : {}),
        ...(notice.repo_id !== undefined ? { repo_id: notice.repo_id } : {}),
      },
    };
  }
  if (notice.type === "harness_install") {
    return {
      type: "harness_install",
      install: {
        kind: notice.kind,
        phase: notice.phase,
        done: notice.done,
        ...(notice.version !== undefined ? { version: notice.version } : {}),
        ...(notice.error !== undefined ? { error: notice.error } : {}),
      },
    };
  }
  return null;
}

type CodeUpdatesStore = CodeUpdatesState & {
  apply: (action: CodeUpdatesAction) => void;
  setViewedWorkspace: (workspaceId: string | null) => void;
  reset: () => void;
};

export const useCodeUpdatesStore = create<CodeUpdatesStore>()((set, get) => ({
  ...EMPTY,
  apply: (action) => {
    const previous = get();
    const next = reduceCodeUpdates(previous, action);
    // OS attention stays keyed to the conversation: a watch child's state
    // change would compare against the interactive digest and misfire.
    if (action.type === "digest" && action.digest.kind !== "watch") {
      maybeNotify(previous, action.digest);
    }
    set(next);
  },
  setViewedWorkspace: (workspaceId) => {
    set(reduceCodeUpdates(get(), { type: "view", workspaceId }));
  },
  reset: () => set({ ...EMPTY }),
}));

function maybeNotify(previous: CodeUpdatesState, digest: CodeSessionDigest): void {
  const prior = previous.byWorkspace[digest.workspace]?.attention;
  if (
    shouldRequestOsAttention(
      prior,
      digest.attention,
      digest.workspace,
      previous.viewedWorkspaceId,
    )
  ) {
    void requestUserAttention().catch(() => {
      // Best-effort dock bounce. The digest itself is the durable signal.
    });
  }
}

let activeClient: Pick<ApiClient, "openCodeUpdates"> | null = null;
let socket: WebSocket | null = null;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let reconnectDelayMs = INITIAL_RECONNECT_DELAY_MS;
let generation = 0;

export function connectCodeUpdates(
  client: Pick<ApiClient, "openCodeUpdates">,
): () => void {
  if (activeClient === client && socket) {
    return disconnectCodeUpdates;
  }
  disconnectCodeUpdates();
  activeClient = client;
  generation += 1;
  const born = generation;
  open(client, born);
  return disconnectCodeUpdates;
}

export function disconnectCodeUpdates(): void {
  generation += 1;
  activeClient = null;
  if (reconnectTimer !== null) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  if (socket) {
    socket.onclose = null;
    socket.onerror = null;
    socket.close();
    socket = null;
  }
  useCodeUpdatesStore.getState().reset();
}

function open(client: Pick<ApiClient, "openCodeUpdates">, born: number): void {
  if (born !== generation) return;
  const next = client.openCodeUpdates((notice) => {
    if (born !== generation) return;
    const action = noticeToAction(notice);
    if (action) useCodeUpdatesStore.getState().apply(action);
  });
  socket = next;
  next.onopen = () => {
    if (born !== generation) return;
    reconnectDelayMs = INITIAL_RECONNECT_DELAY_MS;
  };
  next.onclose = () => {
    if (born !== generation) return;
    socket = null;
    scheduleReconnect(client, born);
  };
  next.onerror = () => {
    next.close();
  };
}

function scheduleReconnect(
  client: Pick<ApiClient, "openCodeUpdates">,
  born: number,
): void {
  if (born !== generation) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    open(client, born);
  }, reconnectDelayMs);
  reconnectDelayMs = nextReconnectDelay(reconnectDelayMs);
}

export function digestLifecycle(
  digest: CodeSessionDigest | undefined,
): CodeSessionLifecycle | undefined {
  return digest?.lifecycle;
}

export function digestPr(
  digest: CodeSessionDigest | undefined,
): PullRequestDigest | undefined {
  return digest?.pr_state;
}

export { MAX_RECONNECT_DELAY_MS };
