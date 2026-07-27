import { create } from "zustand";

/**
 * A message written on the home screen, waiting for the conversation it will
 * open in.
 *
 * Home creates the chat and navigates; the chat route is what actually posts.
 * Handing the text over in a store rather than posting from home keeps one
 * send path — the same optimistic transcript entry, turn id, and failure
 * handling — instead of a second copy that would drift.
 */
export type FirstMessageStore = {
  /** The chat the pending text belongs to; null when there is nothing waiting. */
  chatId: string | null;
  text: string;
  hold: (chatId: string, text: string) => void;
  /** Read and clear in one step, so a remount cannot send it twice. */
  take: (chatId: string) => string | null;
};

export function createFirstMessageStore() {
  return create<FirstMessageStore>()((set, get) => ({
    chatId: null,
    text: "",
    hold: (chatId, text) => set({ chatId, text }),
    take: (chatId) => {
      const { chatId: heldFor, text } = get();
      if (heldFor !== chatId || !text) return null;
      set({ chatId: null, text: "" });
      return text;
    },
  }));
}

export const useFirstMessage = createFirstMessageStore();
