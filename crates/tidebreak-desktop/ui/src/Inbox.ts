import { create } from "zustand";

import { inboxConversationKey, type InboxEntry } from "./api";

/**
 * Everything parked on the reader, in one place.
 *
 * Shell state, like {@link useChatAttention}: an item waits whether or not the
 * screen that would show it is mounted, so the watcher that fills this lives in
 * the shell and every view only reads. The list is the server's read model
 * verbatim — the inbox stores nothing of its own, and an item leaves it when
 * the next poll no longer finds its journal row parked.
 */
export type InboxStore = {
  entries: InboxEntry[];
  /** Whether a first read has landed, so an empty list can be told from "not yet". */
  loaded: boolean;
  setEntries: (entries: InboxEntry[]) => void;
  clear: () => void;
};

/**
 * Whether two reads describe the same queue.
 *
 * Compares the conversation and its attention, not the parked calls: a chat
 * can change what it is waiting on without the queue changing, and a code
 * conversation has no calls here at all.
 */
function sameEntries(left: InboxEntry[], right: InboxEntry[]): boolean {
  return (
    left.length === right.length &&
    left.every((entry, index) => {
      const other = right[index];
      if (!other) return false;
      return (
        inboxConversationKey(entry.conversation) ===
          inboxConversationKey(other.conversation) &&
        entry.attention.state.type === other.attention.state.type &&
        entry.items.length === other.items.length
      );
    })
  );
}

export const useInbox = create<InboxStore>()((set) => ({
  entries: [],
  loaded: false,
  setEntries: (entries) =>
    set((state) =>
      state.loaded && sameEntries(state.entries, entries)
        ? state
        : { entries, loaded: true },
    ),
  clear: () => set({ entries: [], loaded: false }),
}));
