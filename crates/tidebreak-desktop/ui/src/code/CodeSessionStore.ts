import { create } from "zustand";
import type { SequencedCodeEventFrame } from "../api/types";
import type { CodeConnectionState } from "./CodeSessionController";
import {
  initialCodeSessionState,
  reduceCodeSessionEvent,
  type CodeSessionDeps,
  type CodeSessionEffect,
  type CodeSessionState,
} from "./CodeSessionReducer";

/**
 * One code session's state, held outside React so socket callbacks always see
 * current truth without ref mirrors.
 *
 * This is the chat session store factory generalized from one pinned instance
 * to N: the registry constructs one per open session, and nothing here is
 * app-global.
 *
 * `connectionState` sits beside the reducer so a reconnect does not replay as
 * a journal event, and so the first paint can stay `live` instead of flashing
 * a reconnect bar before the socket opens.
 */
export type CodeSessionStore = CodeSessionState & {
  connectionState: CodeConnectionState;
  setConnectionState: (connectionState: CodeConnectionState) => void;
  /**
   * Turn id from the last `turn_began` effect. The pane watches this to drop
   * a queued follow-up once the worker promotes it.
   */
  lastTurnBeganId: string | null;
  applyEvent: (
    framed: SequencedCodeEventFrame,
    deps: CodeSessionDeps,
  ) => CodeSessionEffect[];
  update: (change: (session: CodeSessionState) => CodeSessionState) => void;
  reset: () => void;
};

export function createCodeSessionStore() {
  return create<CodeSessionStore>()((set, get) => ({
    ...initialCodeSessionState(),
    connectionState: "live",
    lastTurnBeganId: null,
    setConnectionState: (connectionState) => set({ connectionState }),
    applyEvent: (framed, deps) => {
      const { state, effects } = reduceCodeSessionEvent(
        sessionOf(get()),
        framed,
        deps,
      );
      const began = effects.find((effect) => effect.type === "turn_began");
      set(began ? { ...state, lastTurnBeganId: began.turnId } : state);
      return effects;
    },
    update: (change) => {
      set(change(sessionOf(get())));
    },
    reset: () => {
      set({
        ...initialCodeSessionState(),
        connectionState: "live",
        lastTurnBeganId: null,
      });
    },
  }));
}

function sessionOf(store: CodeSessionStore): CodeSessionState {
  const {
    applyEvent,
    update,
    reset,
    connectionState,
    setConnectionState,
    lastTurnBeganId,
    ...session
  } = store;
  void applyEvent;
  void update;
  void reset;
  void connectionState;
  void setConnectionState;
  void lastTurnBeganId;
  return session;
}
