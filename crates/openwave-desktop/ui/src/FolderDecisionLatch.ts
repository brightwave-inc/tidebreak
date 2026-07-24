import { create } from "zustand";

/**
 * The one folder request whose decision is currently being resolved.
 *
 * Resolving a decision opens the host's native folder picker, and the host
 * serialises those — a second call while one is open is rejected outright.
 * The picker is a single shared resource, so the latch that keeps a reader
 * from starting a second decision cannot live with the conversation: leaving
 * a chat while its picker is still open would otherwise hand out a fresh
 * claim, and the next decision would fail in the host instead of being
 * offered as unavailable.
 *
 * Held app-wide and keyed by call id, so the card that owns the open decision
 * can still show its own progress while every other card reads as blocked.
 */
export type FolderDecisionLatchStore = {
  resolving: Set<string>;
  /** Take the latch for this call, or report that another decision holds it. */
  claim: (callId: string) => boolean;
  release: (callId: string) => void;
};

export function createFolderDecisionLatchStore() {
  return create<FolderDecisionLatchStore>()((set, get) => ({
    resolving: new Set(),
    claim: (callId) => {
      if (get().resolving.size > 0) return false;
      set({ resolving: new Set([callId]) });
      return true;
    },
    release: (callId) =>
      set((state) => {
        const next = new Set(state.resolving);
        next.delete(callId);
        return { resolving: next };
      }),
  }));
}

export const useFolderDecisionLatch = createFolderDecisionLatchStore();
