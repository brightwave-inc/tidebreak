import { create } from "zustand";

import type { ApiClient } from "../api/client";
import type {
  Attention,
  CodeCloneJobSnapshot,
  CodeSessionDigest,
  CodeSessionLifecycle,
  CodeUpdateNotice,
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
  byWorkspace: Record<string, CodeSessionDigest>;
  cloneJobs: Record<string, CodeCloneJobSnapshot>;
  viewedWorkspaceId: string | null;
};

export type CodeUpdatesAction =
  | { type: "snapshot"; sessions: CodeSessionDigest[] }
  | { type: "digest"; digest: CodeSessionDigest }
  | { type: "clone_progress"; job: CodeCloneJobSnapshot }
  | { type: "view"; workspaceId: string | null }
  | { type: "reset" };

const EMPTY: CodeUpdatesState = {
  byWorkspace: {},
  cloneJobs: {},
  viewedWorkspaceId: null,
};

export function reduceCodeUpdates(
  state: CodeUpdatesState,
  action: CodeUpdatesAction,
): CodeUpdatesState {
  switch (action.type) {
    case "snapshot": {
      const byWorkspace: Record<string, CodeSessionDigest> = {};
      for (const digest of action.sessions) {
        byWorkspace[digest.workspace] = digest;
      }
      return { ...state, byWorkspace };
    }
    case "digest":
      return {
        ...state,
        byWorkspace: {
          ...state.byWorkspace,
          [action.digest.workspace]: action.digest,
        },
      };
    case "clone_progress":
      return {
        ...state,
        cloneJobs: {
          ...state.cloneJobs,
          [action.job.id]: action.job,
        },
      };
    case "view":
      return { ...state, viewedWorkspaceId: action.workspaceId };
    case "reset":
      return { ...EMPTY, viewedWorkspaceId: state.viewedWorkspaceId };
  }
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
        lifecycle: notice.lifecycle,
        attention: notice.attention,
        title: notice.title,
        turn_count: notice.turn_count,
        ...(notice.pr_state !== undefined ? { pr_state: notice.pr_state } : {}),
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
    if (action.type === "digest") {
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
