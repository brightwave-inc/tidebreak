import { create } from "zustand";

/**
 * What just happened to a conversation's active turn.
 *
 * `began` and `began_same_turn` are the same server event told apart by whether
 * it opened a *different* turn than the one already running. Guidance sent to a
 * turn that is still going is still standing, so only a different turn retires
 * it; a cancel request is retired either way.
 *
 * `submitted` is the local half: the composer has posted a message and the turn
 * it opened has not been confirmed yet.
 */
export type TurnLifecycleEvent =
  | "began"
  | "began_same_turn"
  | "resolved"
  | "submitted";

/**
 * The last thing that happened to the active turn, behind a revision counter.
 *
 * Same shape and the same reason as [RefreshSignals]: the event socket is owned
 * by the component that holds the chat session, but the turn controls live with
 * the composer that renders them. A counter rather than a flag so two events in
 * a row are two reactions, not one — and the event rides alongside it because,
 * unlike "go and poll again", what the controls should do differs per event and
 * a bare counter cannot carry that.
 */
export type TurnLifecycleStore = {
  revision: number;
  last: TurnLifecycleEvent;
  signal: (event: TurnLifecycleEvent) => void;
};

export function createTurnLifecycleStore() {
  return create<TurnLifecycleStore>()((set) => ({
    revision: 0,
    last: "resolved",
    signal: (event) =>
      set((state) => ({ revision: state.revision + 1, last: event })),
  }));
}

export const useTurnLifecycle = createTurnLifecycleStore();
