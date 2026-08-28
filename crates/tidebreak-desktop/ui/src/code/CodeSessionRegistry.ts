import type { ApiClient } from "../api/client";
import type { CodeTurnSnapshot, SequencedCodeEventFrame } from "../api/types";
import { codeClientGeneration } from "./CodeClientGeneration";
import { CodeSessionController } from "./CodeSessionController";
import {
  createCodeSessionStore,
  type CodeSessionStore,
} from "./CodeSessionStore";
import {
  applyCodeTurnSnapshot,
  applyStoredRewrites,
  applyTurnRewrite,
  hydrateCodeTurns,
  reconcilePendingCodeTurns,
  type CodeSessionDeps,
} from "./CodeSessionReducer";

/**
 * Per-session stores and the sockets that feed them.
 *
 * Chat pins one session at a time. Code mode keeps several open: two views of
 * the same session must share one store and one socket. When the last view
 * unmounts, the registry closes the socket but retains a few recent stores so
 * returning to a workspace can paint its transcript before reconnecting.
 */

export type CodeSessionOpenSocket = (
  after: number,
  onFrame: (frame: SequencedCodeEventFrame) => void,
) => WebSocket;

export type CodeSessionEntry = {
  store: ReturnType<typeof createCodeSessionStore>;
  controller: CodeSessionController | null;
  refCount: number;
  turnsHydrated: boolean;
  clientGeneration: number | null;
};

const registry = new Map<string, CodeSessionEntry>();
const retainedSessionIds = new Set<string>();

/** Bound transcript memory while keeping normal workspace switching instant. */
export const MAX_RETAINED_CODE_SESSIONS = 4;

const defaultDeps: CodeSessionDeps = {
  nextId: () => {
    nextItem += 1;
    return `code-${nextItem}`;
  },
  now: () => new Date().toISOString(),
};

let nextItem = 0;

function createController(
  store: ReturnType<typeof createCodeSessionStore>,
  openSocket: CodeSessionOpenSocket,
  deps: CodeSessionDeps,
  hydrateTurns: (() => Promise<CodeTurnSnapshot[]>) | undefined,
  hydrateBeforeConnect: boolean,
  onTurnsHydrated: () => void,
): CodeSessionController {
  const fetchedPrompts = new Set<string>();
  const fillPrompt = (turnId: string) => {
    if (!hydrateTurns || fetchedPrompts.has(turnId)) return;
    const state = store.getState();
    if (
      state.items.some((item) => item.kind === "user" && item.turnId === turnId)
    ) {
      return;
    }
    fetchedPrompts.add(turnId);
    void hydrateTurns()
      .then((turns) => {
        const turn = turns.find((candidate) => candidate.id === turnId);
        if (!turn) return;
        store
          .getState()
          .update((session) => applyCodeTurnSnapshot(session, turn));
      })
      .catch(() => {
        // The prompt lands on the next open. The turn itself still streams.
      });
  };
  let controller: CodeSessionController;
  controller = new CodeSessionController({
    openSocket,
    getAfter: () => store.getState().lastSeq,
    onEvents: (frames, initialViewSettled) => {
      // A turn the socket announces has no prompt bubble yet: submit answers
      // only when the turn ends, and a queued follow-up is never answered
      // with a turn at all. Pull the snapshot so the transcript shows what
      // the engine is working on while it works.
      const effects = store
        .getState()
        .applyEvents(frames, deps, initialViewSettled);
      let turnSnapshotNeeded = false;
      for (const effect of effects) {
        if (effect.type === "turn_began") fillPrompt(effect.turnId);
        if (effect.type === "turn_snapshot_needed") turnSnapshotNeeded = true;
      }
      if (turnSnapshotNeeded) controller.requestTurnRefresh();
    },
    onConnectionState: (connectionState) => {
      store.getState().setConnectionState(connectionState);
    },
    hydrateTurns: hydrateBeforeConnect ? hydrateTurns : undefined,
    onHydrate: (turns) => {
      store.getState().update((session) => hydrateCodeTurns(session, turns));
      onTurnsHydrated();
    },
    refreshTurns: hydrateTurns,
    onTurnRefresh: (turns, requested) => {
      store
        .getState()
        .update((session) =>
          reconcilePendingCodeTurns(session, turns, requested),
        );
    },
    getPendingTurnRefreshes: () => {
      const state = store.getState();
      return [...state.pendingTerminalReconciliations.values()].map(
        (pending) => ({
          turnId: pending.turnId,
          eventSeq: pending.eventSeq,
          observedSeq: state.lastSeq,
          observedTurnActivityRevision: state.turnActivityRevision,
        }),
      );
    },
  });
  return controller;
}

function retainCodeSession(sessionId: string): void {
  retainedSessionIds.delete(sessionId);
  retainedSessionIds.add(sessionId);
  while (retainedSessionIds.size > MAX_RETAINED_CODE_SESSIONS) {
    const oldest = retainedSessionIds.values().next().value;
    if (oldest === undefined) return;
    retainedSessionIds.delete(oldest);
    registry.get(oldest)?.controller?.dispose();
    registry.delete(oldest);
  }
}

export function acquireCodeSession(
  sessionId: string,
  openSocket: CodeSessionOpenSocket,
  deps: CodeSessionDeps = defaultDeps,
  hydrateTurns?: () => Promise<CodeTurnSnapshot[]>,
  clientGeneration: number | null = null,
): ReturnType<typeof createCodeSessionStore> {
  let existing = registry.get(sessionId);
  if (existing && existing.clientGeneration !== clientGeneration) {
    existing.controller?.dispose();
    retainedSessionIds.delete(sessionId);
    registry.delete(sessionId);
    existing = undefined;
  }
  if (existing) {
    if (existing.refCount === 0) {
      retainedSessionIds.delete(sessionId);
      existing.refCount = 1;
      const pendingTerminalReconciliation =
        existing.store.getState().pendingTerminalReconciliations.size > 0;
      existing.controller = createController(
        existing.store,
        openSocket,
        deps,
        hydrateTurns,
        !existing.turnsHydrated || pendingTerminalReconciliation,
        () => {
          existing.turnsHydrated = true;
        },
      );
      existing.controller.start();
    } else {
      existing.refCount += 1;
    }
    return existing.store;
  }
  const store = createCodeSessionStore();
  const entry: CodeSessionEntry = {
    store,
    controller: null,
    refCount: 1,
    turnsHydrated: hydrateTurns === undefined,
    clientGeneration,
  };
  registry.set(sessionId, entry);
  entry.controller = createController(
    store,
    openSocket,
    deps,
    hydrateTurns,
    !entry.turnsHydrated,
    () => {
      entry.turnsHydrated = true;
    },
  );
  entry.controller.start();
  return store;
}

export function acquireCodeSessionFromClient(
  sessionId: string,
  client: Pick<ApiClient, "openCodeEvents" | "listCodeSessionTurns">,
  deps?: CodeSessionDeps,
): ReturnType<typeof createCodeSessionStore> {
  return acquireCodeSession(
    sessionId,
    (after, onFrame) => client.openCodeEvents(sessionId, after, onFrame),
    deps,
    () => client.listCodeSessionTurns(sessionId),
    codeClientGeneration(client),
  );
}

export function releaseCodeSession(sessionId: string): void {
  const existing = registry.get(sessionId);
  if (!existing || existing.refCount === 0) return;
  existing.refCount -= 1;
  if (existing.refCount > 0) return;
  existing.controller?.dispose();
  existing.controller = null;
  existing.store.getState().setConnectionState("reconnecting");
  retainCodeSession(sessionId);
}

export function peekCodeSession(
  sessionId: string,
): CodeSessionEntry | undefined {
  return registry.get(sessionId);
}

/** Stamp a live recap onto a retained or open session store. */
export function applyLiveTurnRewrite(
  sessionId: string,
  turnId: string,
  state: "rewriting" | "rewritten" | "failed",
  rewrite?: string,
): void {
  const entry = registry.get(sessionId);
  if (!entry) return;
  entry.store.getState().update((session) => {
    let next = session;
    if (state === "rewritten" && rewrite) {
      next = {
        ...next,
        storedRewrites: { ...next.storedRewrites, [turnId]: rewrite },
      };
    }
    next = {
      ...next,
      items: applyTurnRewrite(next.items, turnId, {
        rewrite,
        rewriteState: state,
      }),
    };
    return state === "rewritten" ? applyStoredRewrites(next) : next;
  });
}

/** Test-only: drop every live entry without waiting for unmounts. */
export function resetCodeSessionRegistry(): void {
  for (const entry of registry.values()) {
    entry.controller?.dispose();
  }
  registry.clear();
  retainedSessionIds.clear();
  nextItem = 0;
}

export type { CodeSessionStore };
