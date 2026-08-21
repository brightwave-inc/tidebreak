import { useMemo } from "react";
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

/** Digests for one kind of session, keyed workspace → session. */
export type DigestsByWorkspace = Record<
  string,
  Record<string, CodeSessionDigest>
>;

export type CodeUpdatesState = {
  /**
   * Conversation digests, keyed workspace → session. Never a watch.
   *
   * A workspace runs several agents (record 55), so it names a set rather
   * than one digest. List surfaces collapse the set through
   * `workspaceDigest`; the workspace page reads all of it to label its
   * conversation tabs.
   */
  conversationsByWorkspace: DigestsByWorkspace;
  /**
   * Watch digests, keyed workspace → session. Children beside the
   * conversations, never among them — ADR 0050's rule, kept by construction:
   * the two maps have disjoint sources.
   */
  childrenByWorkspace: DigestsByWorkspace;
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
  conversationsByWorkspace: {},
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
      const conversationsByWorkspace: DigestsByWorkspace = {};
      const childrenByWorkspace: DigestsByWorkspace = {};
      for (const digest of action.sessions) {
        const map =
          digest.kind === "watch"
            ? childrenByWorkspace
            : conversationsByWorkspace;
        (map[digest.workspace] ??= {})[digest.session] = digest;
      }
      return { ...state, conversationsByWorkspace, childrenByWorkspace };
    }
    case "digest": {
      if (action.digest.kind === "watch") {
        return {
          ...state,
          // An ended watch leaves the rail. The snapshot on reconnect would
          // drop it anyway; this does it without waiting for one.
          childrenByWorkspace: upsertDigest(
            state.childrenByWorkspace,
            action.digest,
            true,
          ),
        };
      }
      return {
        ...state,
        // An ended conversation stays. Its tab is already gone — the strip
        // reads the session list, not this map — and the workspace header
        // keeps a title and a PR from it until the next snapshot.
        conversationsByWorkspace: upsertDigest(
          state.conversationsByWorkspace,
          action.digest,
          false,
        ),
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

function upsertDigest(
  map: DigestsByWorkspace,
  digest: CodeSessionDigest,
  dropEnded: boolean,
): DigestsByWorkspace {
  const forWorkspace = { ...map[digest.workspace] };
  if (dropEnded && digest.lifecycle === "ended") {
    delete forWorkspace[digest.session];
  } else {
    forWorkspace[digest.session] = digest;
  }
  if (Object.keys(forWorkspace).length === 0) {
    const next = { ...map };
    delete next[digest.workspace];
    return next;
  }
  return { ...map, [digest.workspace]: forWorkspace };
}

/**
 * The one digest a list surface should show for a workspace.
 *
 * A card is a row per workspace, not per agent, so several conversations
 * collapse to the one that most wants a person: a need first, then a running
 * engine, then anything still live. Title and PR state come from the
 * workspace itself, so every sibling agrees on them and this choice only
 * decides whose attention the card reports.
 */
export function workspaceDigest(
  state: Pick<CodeUpdatesState, "conversationsByWorkspace">,
  workspaceId: string,
): CodeSessionDigest | undefined {
  const conversations = state.conversationsByWorkspace[workspaceId];
  if (!conversations) return undefined;
  let best: CodeSessionDigest | undefined;
  for (const digest of Object.values(conversations)) {
    if (!best || digestUrgency(digest) < digestUrgency(best)) best = digest;
  }
  return best;
}

function digestUrgency(digest: CodeSessionDigest): number {
  if (digest.lifecycle === "ended") return 4;
  if (digest.attention.state.type === "needs_you") return 0;
  if (digest.lifecycle === "running") return 1;
  if (digest.attention.state.type === "done_unreviewed") return 2;
  return 3;
}

/** One digest per workspace, for the rail and the card lists. */
export function useWorkspaceDigests(): Record<string, CodeSessionDigest> {
  const conversations = useCodeUpdatesStore(
    (state) => state.conversationsByWorkspace,
  );
  return useMemo(() => {
    const digests: Record<string, CodeSessionDigest> = {};
    for (const workspaceId of Object.keys(conversations)) {
      const digest = workspaceDigest(
        { conversationsByWorkspace: conversations },
        workspaceId,
      );
      if (digest) digests[workspaceId] = digest;
    }
    return digests;
  }, [conversations]);
}

/** The digest that speaks for one workspace, for a header or an inspector. */
export function useWorkspaceDigest(
  workspaceId: string,
): CodeSessionDigest | undefined {
  return useCodeUpdatesStore((state) => workspaceDigest(state, workspaceId));
}

/**
 * The live digest for one session, whichever map holds it.
 *
 * A workspace page shows one session at a time and may be showing a watch
 * child, so it asks by id rather than taking the workspace's representative.
 */
export function useSessionDigest(
  workspaceId: string,
  sessionId: string | null,
): CodeSessionDigest | undefined {
  return useCodeUpdatesStore((state) =>
    sessionId
      ? (state.conversationsByWorkspace[workspaceId]?.[sessionId] ??
        state.childrenByWorkspace[workspaceId]?.[sessionId])
      : undefined,
  );
}

const NO_DIGESTS: Record<string, CodeSessionDigest> = {};

/**
 * Every conversation digest in one workspace, keyed by session.
 *
 * The workspace page labels a tab per agent, so it needs each agent's own
 * state rather than the one the card collapses to.
 */
export function useConversationDigests(
  workspaceId: string,
): Record<string, CodeSessionDigest> {
  return useCodeUpdatesStore(
    (state) => state.conversationsByWorkspace[workspaceId] ?? NO_DIGESTS,
  );
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
        ...(notice.activity !== undefined ? { activity: notice.activity } : {}),
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
  const prior =
    previous.conversationsByWorkspace[digest.workspace]?.[digest.session]
      ?.attention;
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
