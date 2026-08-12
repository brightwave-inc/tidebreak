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
 * `applyEvent` is the only entry point for stream events: it runs the pure
 * reducer and hands the resulting effects back to the caller, which owns
 * their application (polling refreshes, hydration, cancel/steer cleanup).
 * `update` is for non-stream writers (hydration, optimistic sends, resets of
 * individual fields); `reset` swaps in a fresh session on chat switch.
 */
export type ChatSessionStore = ChatSessionState & {
  applyEvent: (
    framed: SequencedEvent,
    deps: ChatSessionDeps,
  ) => ChatSessionEffect[];
  update: (change: (session: ChatSessionState) => ChatSessionState) => void;
  reset: () => void;
};

export function createChatSessionStore() {
  return create<ChatSessionStore>()((set, get) => ({
    ...initialChatSessionState(),
    applyEvent: (framed, deps) => {
      const { state, effects } = reduceChatSessionEvent(
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
      set(initialChatSessionState());
    },
  }));
}

/** The plain session fields, without the store's action functions. */
function sessionOf(store: ChatSessionStore): ChatSessionState {
  const { applyEvent, update, reset, ...session } = store;
  void applyEvent;
  void update;
  void reset;
  return session;
}

/** The app-wide session store; one chat session is live at a time. */
export const useChatSessionStore = createChatSessionStore();
