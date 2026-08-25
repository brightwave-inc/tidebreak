import { create } from "zustand";
import type { ImageAttachment } from "./ImageAttachments";
import type { ImportedDocument } from "./documents";
import type { PastedTextAttachment } from "./PastedText";

export type PendingFirstMessage = {
  text: string;
  images: ImageAttachment[];
  files: ImportedDocument[];
  pastedTexts: PastedTextAttachment[];
  /** Skills picked on home, which only the chat route can actually post. */
  skills: string[];
  /** Whether voice transcription contributed to this message's text. */
  voiceInputUsed: boolean;
};

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
  pending: PendingFirstMessage | null;
  hold: (chatId: string, pending: PendingFirstMessage) => void;
  /** Read and clear in one step, so a remount cannot send it twice. */
  take: (chatId: string) => PendingFirstMessage | null;
};

export function createFirstMessageStore() {
  return create<FirstMessageStore>()((set, get) => ({
    chatId: null,
    pending: null,
    hold: (chatId, pending) => set({ chatId, pending }),
    take: (chatId) => {
      const { chatId: heldFor, pending } = get();
      if (
        heldFor !== chatId ||
        !pending ||
        (!pending.text && pending.pastedTexts.length === 0)
      ) {
        return null;
      }
      set({ chatId: null, pending: null });
      return pending;
    },
  }));
}

export const useFirstMessage = createFirstMessageStore();
