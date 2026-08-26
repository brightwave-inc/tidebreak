import { useMemo } from "react";
import { toast } from "sonner";
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
import { friendlyErrorMessage } from "../lib/utils";
import { useCodeUiStore } from "./CodeUiStore";

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

export type CodeCloneRequest = Parameters<ApiClient["startCodeClone"]>[0];

export type CodeCloneTracking = {
  request: CodeCloneRequest;
  clientGeneration: number;
  background: boolean;
  notified: boolean;
  startedOrder: number;
};

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
   * Clone jobs started by this window, including enough context to resume the
   * onboarding flow after its dialog or the whole Code route unmounts.
   */
  cloneTracking: Record<string, CodeCloneTracking>;
  /** A durable-read failure is separate from a clone failure. */
  cloneReadErrors: Record<string, string>;
  /** The ApiClient generation that owns every clone entry in this store. */
  cloneClientGeneration: number | null;
  /**
   * Warm harness installs, keyed by engine. The New Workspace dialog starts
   * them and reads their phase from here; nothing else depends on them, and a
   * reconnect drops them because the doctor report is the durable answer for
   * what is installed.
   */
  harnessInstalls: Partial<Record<HarnessKind, CodeHarnessInstallSnapshot>>;
  viewedWorkspaceId: string | null;
  /**
   * Bumped on each `delivery` notice: the pull-request store changed
   * (decision 66). Delivery surfaces re-read their queries when this moves
   * instead of running their own poll timers.
   */
  deliveryRevision: number;
};

export type CodeUpdatesAction =
  | { type: "snapshot"; sessions: CodeSessionDigest[] }
  | { type: "digest"; digest: CodeSessionDigest }
  | { type: "clone_progress"; job: CodeCloneJobSnapshot }
  | { type: "harness_install"; install: CodeHarnessInstallSnapshot }
  | { type: "delivery" }
  | { type: "view"; workspaceId: string | null }
  | { type: "reset" };

const EMPTY: CodeUpdatesState = {
  conversationsByWorkspace: {},
  childrenByWorkspace: {},
  cloneJobs: {},
  cloneTracking: {},
  cloneReadErrors: {},
  cloneClientGeneration: null,
  harnessInstalls: {},
  viewedWorkspaceId: null,
  deliveryRevision: 0,
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
          [action.job.id]: mergeCloneJob(
            state.cloneJobs[action.job.id],
            action.job,
          ),
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
    case "delivery":
      return { ...state, deliveryRevision: state.deliveryRevision + 1 };
    case "view":
      return { ...state, viewedWorkspaceId: action.workspaceId };
    case "reset":
      return { ...EMPTY, viewedWorkspaceId: state.viewedWorkspaceId };
  }
}

function mergeCloneJob(
  current: CodeCloneJobSnapshot | undefined,
  incoming: CodeCloneJobSnapshot,
): CodeCloneJobSnapshot {
  if (current?.done && !incoming.done) return current;
  return incoming;
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

/**
 * One digest per workspace, from a snapshot of the store.
 *
 * Split from the hook so callers outside React — the rail-walking shortcuts —
 * order workspaces by the same digests the rail draws with, rather than by a
 * second, subtly different arrangement.
 */
export function workspaceDigests(
  state: Pick<CodeUpdatesState, "conversationsByWorkspace">,
): Record<string, CodeSessionDigest> {
  const digests: Record<string, CodeSessionDigest> = {};
  for (const workspaceId of Object.keys(state.conversationsByWorkspace)) {
    const digest = workspaceDigest(state, workspaceId);
    if (digest) digests[workspaceId] = digest;
  }
  return digests;
}

/** One digest per workspace, for the rail and the card lists. */
export function useWorkspaceDigests(): Record<string, CodeSessionDigest> {
  const conversations = useCodeUpdatesStore(
    (state) => state.conversationsByWorkspace,
  );
  return useMemo(
    () => workspaceDigests({ conversationsByWorkspace: conversations }),
    [conversations],
  );
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
    attention.state.type === "needs_you" &&
    attention.state.source === "structured"
  );
}

export function noticeToAction(
  notice: CodeUpdateNotice,
): CodeUpdatesAction | null {
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
  if (notice.type === "delivery") {
    return { type: "delivery" };
  }
  return null;
}

type CodeUpdatesStore = CodeUpdatesState & {
  apply: (action: CodeUpdatesAction) => void;
  setViewedWorkspace: (workspaceId: string | null) => void;
  resetLive: () => void;
  reset: () => void;
};

export const useCodeUpdatesStore = create<CodeUpdatesStore>()((set, get) => ({
  ...EMPTY,
  apply: (action) => {
    let backgroundClone: CodeCloneJobSnapshot | null = null;
    set((previous) => {
      let next = reduceCodeUpdates(previous, action);
      // OS attention stays keyed to the conversation: a watch child's state
      // change would compare against the interactive digest and misfire.
      if (action.type === "digest" && action.digest.kind !== "watch") {
        maybeNotify(previous, action.digest);
      }
      if (action.type === "clone_progress") {
        const cloneReadErrors = { ...next.cloneReadErrors };
        delete cloneReadErrors[action.job.id];
        next = { ...next, cloneReadErrors };
        const tracking = next.cloneTracking[action.job.id];
        const updatedJob = next.cloneJobs[action.job.id];
        if (tracking?.background && updatedJob?.done && !tracking.notified) {
          backgroundClone = updatedJob;
          next = {
            ...next,
            cloneTracking: {
              ...next.cloneTracking,
              [action.job.id]: { ...tracking, notified: true },
            },
          };
        }
      }
      return next;
    });
    if (backgroundClone) notifyBackgroundClone(backgroundClone);
  },
  setViewedWorkspace: (workspaceId) => {
    set(reduceCodeUpdates(get(), { type: "view", workspaceId }));
  },
  resetLive: () =>
    set((state) => ({
      ...EMPTY,
      cloneJobs: state.cloneJobs,
      cloneTracking: state.cloneTracking,
      cloneReadErrors: state.cloneReadErrors,
      cloneClientGeneration: state.cloneClientGeneration,
      viewedWorkspaceId: state.viewedWorkspaceId,
    })),
  reset: () => set({ ...EMPTY }),
}));

const clientGenerations = new WeakMap<object, number>();
let nextClientGeneration = 0;
let nextCloneOrder = 0;

/** A stable identity for one ApiClient object. */
export function codeClientGeneration(client: object): number {
  const existing = clientGenerations.get(client);
  if (existing !== undefined) return existing;
  nextClientGeneration += 1;
  clientGenerations.set(client, nextClientGeneration);
  return nextClientGeneration;
}

/**
 * Move clone state to one client authority. A job ID from another machine or
 * token generation must never be read through the replacement client.
 */
export function activateCodeCloneClient(client: object): number {
  const clientGeneration = codeClientGeneration(client);
  useCodeUpdatesStore.setState((state) => {
    if (state.cloneClientGeneration === clientGeneration) return state;
    return {
      cloneJobs: {},
      cloneTracking: {},
      cloneReadErrors: {},
      cloneClientGeneration: clientGeneration,
    };
  });
  return clientGeneration;
}

/** Record a clone before the palette can unmount. */
export function trackCodeClone(
  client: object,
  job: CodeCloneJobSnapshot,
  request: CodeCloneRequest,
  background = false,
): boolean {
  const clientGeneration = codeClientGeneration(client);
  let tracked = false;
  useCodeUpdatesStore.setState((state) => {
    if (state.cloneClientGeneration !== clientGeneration) return state;
    tracked = true;
    const cloneReadErrors = { ...state.cloneReadErrors };
    delete cloneReadErrors[job.id];
    nextCloneOrder += 1;
    const trackedJob = mergeCloneJob(state.cloneJobs[job.id], job);
    return {
      cloneJobs: { ...state.cloneJobs, [job.id]: trackedJob },
      cloneTracking: {
        ...state.cloneTracking,
        [job.id]: {
          request,
          clientGeneration,
          background,
          notified: false,
          startedOrder: nextCloneOrder,
        },
      },
      cloneReadErrors,
    };
  });
  if (tracked && background && job.done) {
    setCodeCloneBackground(client, job.id, true);
  }
  return tracked;
}

/** Make a clone resumable without leaving its completion handoff armed. */
export function setCodeCloneBackground(
  client: object,
  jobId: string,
  background: boolean,
): void {
  const clientGeneration = codeClientGeneration(client);
  let terminal: CodeCloneJobSnapshot | null = null;
  useCodeUpdatesStore.setState((state) => {
    const tracking = state.cloneTracking[jobId];
    if (
      state.cloneClientGeneration !== clientGeneration ||
      tracking?.clientGeneration !== clientGeneration
    ) {
      return state;
    }
    const job = state.cloneJobs[jobId];
    const notify = background && job?.done && !tracking.notified;
    if (notify) terminal = job;
    return {
      cloneTracking: {
        ...state.cloneTracking,
        [jobId]: {
          ...tracking,
          background,
          notified: tracking.notified || Boolean(notify),
        },
      },
    };
  });
  if (terminal) notifyBackgroundClone(terminal);
}

export type ResumableCodeClone = {
  job: CodeCloneJobSnapshot;
  tracking: CodeCloneTracking;
  readError: string | null;
};

/** The most recently started clone that this client can resume. */
export function latestCodeClone(client: object): ResumableCodeClone | null {
  const clientGeneration = codeClientGeneration(client);
  const state = useCodeUpdatesStore.getState();
  if (state.cloneClientGeneration !== clientGeneration) return null;
  const tracking = Object.entries(state.cloneTracking)
    .filter(([, entry]) => entry.clientGeneration === clientGeneration)
    .sort(([, left], [, right]) => right.startedOrder - left.startedOrder)[0];
  if (!tracking) return null;
  const [jobId, entry] = tracking;
  const job = state.cloneJobs[jobId];
  if (!job) return null;
  return {
    job,
    tracking: entry,
    readError: state.cloneReadErrors[jobId] ?? null,
  };
}

/** Remove a clone once onboarding has handed its repository off. */
export function forgetCodeClone(client: object, jobId: string): void {
  const clientGeneration = codeClientGeneration(client);
  useCodeUpdatesStore.setState((state) => {
    const tracking = state.cloneTracking[jobId];
    if (
      state.cloneClientGeneration !== clientGeneration ||
      tracking?.clientGeneration !== clientGeneration
    ) {
      return state;
    }
    const cloneJobs = { ...state.cloneJobs };
    const cloneTracking = { ...state.cloneTracking };
    const cloneReadErrors = { ...state.cloneReadErrors };
    delete cloneJobs[jobId];
    delete cloneTracking[jobId];
    delete cloneReadErrors[jobId];
    return { cloneJobs, cloneTracking, cloneReadErrors };
  });
}

type CloneUpdatesClient = Pick<
  ApiClient,
  "getCodeCloneJob" | "openCodeUpdates"
>;

const cloneReconciliations = new Map<
  string,
  Promise<CodeCloneJobSnapshot | null>
>();

/** Read the server's durable clone state and ignore a stale client response. */
export function reconcileCodeClone(
  client: Pick<ApiClient, "getCodeCloneJob">,
  jobId: string,
): Promise<CodeCloneJobSnapshot | null> {
  const clientGeneration = codeClientGeneration(client);
  const key = `${clientGeneration}:${jobId}`;
  const existing = cloneReconciliations.get(key);
  if (existing) return existing;

  const state = useCodeUpdatesStore.getState();
  if (
    state.cloneClientGeneration !== clientGeneration ||
    state.cloneTracking[jobId]?.clientGeneration !== clientGeneration
  ) {
    return Promise.resolve(null);
  }

  useCodeUpdatesStore.setState((current) => {
    if (!current.cloneReadErrors[jobId]) return current;
    const cloneReadErrors = { ...current.cloneReadErrors };
    delete cloneReadErrors[jobId];
    return { cloneReadErrors };
  });

  const request = Promise.resolve()
    .then(() => client.getCodeCloneJob(jobId))
    .then((job) => {
      const current = useCodeUpdatesStore.getState();
      if (
        current.cloneClientGeneration !== clientGeneration ||
        current.cloneTracking[jobId]?.clientGeneration !== clientGeneration
      ) {
        return null;
      }
      current.apply({ type: "clone_progress", job });
      return job;
    })
    .catch((error: unknown) => {
      useCodeUpdatesStore.setState((current) => {
        if (
          current.cloneClientGeneration !== clientGeneration ||
          current.cloneTracking[jobId]?.clientGeneration !== clientGeneration
        ) {
          return current;
        }
        return {
          cloneReadErrors: {
            ...current.cloneReadErrors,
            [jobId]: friendlyErrorMessage(
              error,
              "Could not check clone progress",
            ),
          },
        };
      });
      return null;
    })
    .finally(() => {
      cloneReconciliations.delete(key);
    });
  cloneReconciliations.set(key, request);
  return request;
}

function notifyBackgroundClone(job: CodeCloneJobSnapshot): void {
  const action = {
    label: job.error ? "Retry" : "Open",
    onClick: () => useCodeUiStore.getState().setAddRepoOpen(true),
  };
  if (job.error) {
    toast.error("Repository clone failed", {
      description: job.error,
      action,
    });
    return;
  }
  toast.success("Repository cloned", {
    description: "Create a workspace when you are ready.",
    action,
  });
}

function maybeNotify(
  previous: CodeUpdatesState,
  digest: CodeSessionDigest,
): void {
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

let activeClient: CloneUpdatesClient | null = null;
let socket: WebSocket | null = null;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let reconnectDelayMs = INITIAL_RECONNECT_DELAY_MS;
let generation = 0;

export function connectCodeUpdates(client: CloneUpdatesClient): () => void {
  if (activeClient === client && socket) {
    return disconnectCodeUpdates;
  }
  disconnectCodeUpdates();
  activateCodeCloneClient(client);
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
    socket.onopen = null;
    socket.onclose = null;
    socket.onerror = null;
    socket.close();
    socket = null;
  }
  useCodeUpdatesStore.getState().resetLive();
}

function open(client: CloneUpdatesClient, born: number): void {
  if (born !== generation) return;
  const next = client.openCodeUpdates((notice) => {
    if (born !== generation) return;
    const action = noticeToAction(notice);
    if (
      action?.type === "clone_progress" &&
      useCodeUpdatesStore.getState().cloneClientGeneration !==
        codeClientGeneration(client)
    ) {
      return;
    }
    if (action) useCodeUpdatesStore.getState().apply(action);
  });
  socket = next;
  next.onopen = () => {
    if (born !== generation) return;
    reconnectDelayMs = INITIAL_RECONNECT_DELAY_MS;
    reconcileTrackedClones(client, born);
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

function scheduleReconnect(client: CloneUpdatesClient, born: number): void {
  if (born !== generation) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    open(client, born);
  }, reconnectDelayMs);
  reconnectDelayMs = nextReconnectDelay(reconnectDelayMs);
}

function reconcileTrackedClones(
  client: CloneUpdatesClient,
  born: number,
): void {
  if (born !== generation) return;
  const clientGeneration = codeClientGeneration(client);
  const state = useCodeUpdatesStore.getState();
  if (state.cloneClientGeneration !== clientGeneration) return;
  for (const [jobId, tracking] of Object.entries(state.cloneTracking)) {
    if (
      tracking.clientGeneration === clientGeneration &&
      !state.cloneJobs[jobId]?.done
    ) {
      void reconcileCodeClone(client, jobId);
    }
  }
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
