import { create } from "zustand";

import type { InboxItem } from "./api";

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
  items: InboxItem[];
  /** Whether a first read has landed, so an empty list can be told from "not yet". */
  loaded: boolean;
  setItems: (items: InboxItem[]) => void;
  clear: () => void;
};

function sameItems(left: InboxItem[], right: InboxItem[]): boolean {
  return (
    left.length === right.length &&
    left.every((item, index) => item.callId === right[index]?.callId)
  );
}

export const useInbox = create<InboxStore>()((set) => ({
  items: [],
  loaded: false,
  setItems: (items) =>
    set((state) =>
      state.loaded && sameItems(state.items, items)
        ? state
        : { items, loaded: true },
    ),
  clear: () => set({ items: [], loaded: false }),
}));
