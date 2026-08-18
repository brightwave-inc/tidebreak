import type { ApiClient } from "../api/client";
import type { CodeTurnSnapshot, SequencedCodeEventFrame } from "../api/types";
import { CodeSessionController } from "./CodeSessionController";
import {
  createCodeSessionStore,
  type CodeSessionStore,
} from "./CodeSessionStore";
import {
  applyAcceptedTurn,
  hydrateCodeTurns,
  markCodeSessionHydrated,
  type CodeSessionDeps,
} from "./CodeSessionReducer";

/**
 * Per-session stores and the sockets that feed them.
 *
 * Chat pins one session at a time. Code mode keeps several open: two views of
 * the same session must share one store and one socket, and unmounting the
 * last view must close that socket. The map is ref-counted so that contract
 * is mechanical rather than something each page has to remember.
 */

export type CodeSessionOpenSocket = (
  after: number,
  onFrame: (frame: SequencedCodeEventFrame) => void,
) => WebSocket;

export type CodeSessionEntry = {
  store: ReturnType<typeof createCodeSessionStore>;
  controller: CodeSessionController;
  refCount: number;
};

const registry = new Map<string, CodeSessionEntry>();

const defaultDeps: CodeSessionDeps = {
  nextId: () => {
    nextItem += 1;
    return `code-${nextItem}`;
  },
  now: () => new Date().toISOString(),
};

let nextItem = 0;

export function acquireCodeSession(
  sessionId: string,
  openSocket: CodeSessionOpenSocket,
  deps: CodeSessionDeps = defaultDeps,
  hydrateTurns?: () => Promise<CodeTurnSnapshot[]>,
): ReturnType<typeof createCodeSessionStore> {
  const existing = registry.get(sessionId);
  if (existing) {
    existing.refCount += 1;
    return existing.store;
  }
  const store = createCodeSessionStore();
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
        store.getState().update((session) => applyAcceptedTurn(session, turn));
      })
      .catch(() => {
        // The prompt lands on the next open. The turn itself still streams.
      });
  };
  const controller = new CodeSessionController({
    openSocket,
    getAfter: () => store.getState().lastSeq,
    onEvent: (frame) => {
      // A turn the socket announces has no prompt bubble yet: submit answers
      // only when the turn ends, and a queued follow-up is never answered
      // with a turn at all. Pull the snapshot so the transcript shows what
      // the engine is working on while it works.
      for (const effect of store.getState().applyEvent(frame, deps)) {
        if (effect.type === "turn_began") fillPrompt(effect.turnId);
      }
    },
    onConnectionState: (connectionState) => {
      store.getState().setConnectionState(connectionState);
    },
    hydrateTurns,
    onHydrate: (turns) => {
      store.getState().update((session) => hydrateCodeTurns(session, turns));
    },
    onHydrateSettled: () => {
      store.getState().update(markCodeSessionHydrated);
    },
  });
  registry.set(sessionId, { store, controller, refCount: 1 });
  controller.start();
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
  );
}

export function releaseCodeSession(sessionId: string): void {
  const existing = registry.get(sessionId);
  if (!existing) return;
  existing.refCount -= 1;
  if (existing.refCount > 0) return;
  existing.controller.dispose();
  registry.delete(sessionId);
}

export function peekCodeSession(
  sessionId: string,
): CodeSessionEntry | undefined {
  return registry.get(sessionId);
}

/** Test-only: drop every live entry without waiting for unmounts. */
export function resetCodeSessionRegistry(): void {
  for (const entry of registry.values()) {
    entry.controller.dispose();
  }
  registry.clear();
  nextItem = 0;
}

export type { CodeSessionStore };
