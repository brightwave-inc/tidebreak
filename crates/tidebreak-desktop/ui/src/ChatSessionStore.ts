import { create } from "zustand";
import type { SequencedEvent } from "./api";
import {
  initialChatSessionState,
  reduceChatSessionEvent,
  type ChatSessionDeps,
  type ChatSessionEffect,
  type ChatSessionState,
} from "./ChatSessionReducer";

/**
 * The chat session's state, held outside React so socket callbacks and other
 * async work always see current truth (`getState()`) without ref mirrors.
 *
 * `applyEvents` is the only entry point for stream events: it runs the pure
 * reducer over a run of frames, publishes once, and hands the resulting
 * effects back to the caller, which owns their application (polling
 * refreshes, hydration, cancel/steer cleanup). One publish per run is what
 * lets a replayed turn render once rather than once per frame. `applyEvent`
 * is the one-frame form. `update` is for non-stream writers (hydration,
 * optimistic sends, resets of individual fields); `reset` swaps in a fresh
 * session on chat switch.
 */
export type ChatSessionStore = ChatSessionState & {
  applyEvents: (
    frames: readonly SequencedEvent[],
    deps: ChatSessionDeps,
  ) => ChatSessionEffect[];
  applyEvent: (
    framed: SequencedEvent,
    deps: ChatSessionDeps,
  ) => ChatSessionEffect[];
  update: (change: (session: ChatSessionState) => ChatSessionState) => void;
  reset: () => void;
};

export function createChatSessionStore() {
  return create<ChatSessionStore>()((set, get) => {
    const applyEvents = (
      frames: readonly SequencedEvent[],
      deps: ChatSessionDeps,
    ): ChatSessionEffect[] => {
      const before = sessionOf(get());
      let state = before;
      const effects: ChatSessionEffect[] = [];
      for (const framed of frames) {
        const transition = reduceChatSessionEvent(state, framed, deps);
        state = transition.state;
        effects.push(...transition.effects);
      }
      // A run of duplicates changes nothing, and publishing it anyway would
      // wake every subscriber for no reason.
      if (state !== before) set(state);
      return effects;
    };
    return {
      ...initialChatSessionState(),
      applyEvents,
      applyEvent: (framed, deps) => applyEvents([framed], deps),
      update: (change) => {
        set(change(sessionOf(get())));
      },
      reset: () => {
        set(initialChatSessionState());
      },
    };
  });
}

/** The plain session fields, without the store's action functions. */
function sessionOf(store: ChatSessionStore): ChatSessionState {
  const { applyEvents, applyEvent, update, reset, ...session } = store;
  void applyEvents;
  void applyEvent;
  void update;
  void reset;
  return session;
}

/** The app-wide session store; one chat session is live at a time. */
export const useChatSessionStore = createChatSessionStore();
