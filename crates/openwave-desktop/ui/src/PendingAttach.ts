import { create } from "zustand";

/**
 * Signals that the file picker should open as soon as a conversation mounts.
 *
 * When "Upload files" is clicked on the home page there is no chat yet, so the
 * home route creates one, navigates to it, and drops a marker here. The chat
 * route picks it up on mount and opens the native picker — the same code path
 * as clicking "Upload files" inside a conversation.
 *
 * `take` reads and clears atomically so a re-render cannot open it twice.
 */
type PendingAttachStore = {
  chatId: string | null;
  hold: (chatId: string) => void;
  take: (chatId: string) => boolean;
};

export const usePendingAttach = create<PendingAttachStore>()((set, get) => ({
  chatId: null,
  hold: (chatId) => set({ chatId }),
  take: (chatId) => {
    if (get().chatId !== chatId) return false;
    set({ chatId: null });
    return true;
  },
}));
