import { create } from "zustand";

/**
 * Server-side state the event stream can report as moved on.
 */
export type RefreshTarget = "agentRuns" | "folderAccess" | "userQuestions";

/**
 * A revision counter per pollable target.
 *
 * The event socket is owned by the component that holds the chat session, but
 * the pollers that own this state live with the surfaces that render it.
 * Bumping a revision lets the stream say "this has changed" without the root
 * holding a callback ref for every poller, and without a poller reaching back
 * into the stream. A counter rather than a flag so two signals in a row are two
 * refreshes, not one.
 */
export type RefreshSignalStore = {
  agentRuns: number;
  folderAccess: number;
  userQuestions: number;
  signal: (target: RefreshTarget) => void;
};

export function createRefreshSignalStore() {
  return create<RefreshSignalStore>()((set) => ({
    agentRuns: 0,
    folderAccess: 0,
    userQuestions: 0,
    signal: (target) =>
      set((state) => ({ [target]: state[target] + 1 }) as Partial<RefreshSignalStore>),
  }));
}

export const useRefreshSignals = createRefreshSignalStore();
