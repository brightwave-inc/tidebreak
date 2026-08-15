import { create } from "zustand";
import type { SequencedCodeEventFrame } from "../api/types";
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
 */
export type CodeSessionStore = CodeSessionState & {
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
    applyEvent: (framed, deps) => {
      const { state, effects } = reduceCodeSessionEvent(
        sessionOf(get()),
        framed,
        deps,
      );
      set(state);
      return effects;
    },
    update: (change) => {
      set(change(sessionOf(get())));
    },
    reset: () => {
      set(initialCodeSessionState());
    },
  }));
}

function sessionOf(store: CodeSessionStore): CodeSessionState {
  const { applyEvent, update, reset, ...session } = store;
  void applyEvent;
  void update;
  void reset;
  return session;
}
