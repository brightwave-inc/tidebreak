import { create } from "zustand";

type ChatAttentionStore = {
  /** Chats with a live user question or folder-access request. */
  chatIdsWithPendingPrompts: ReadonlySet<string>;
  setChatIdsWithPendingPrompts: (chatIds: Iterable<string>) => void;
  clear: () => void;
};

function sameChatIds(left: ReadonlySet<string>, right: ReadonlySet<string>): boolean {
  return left.size === right.size && [...left].every((chatId) => right.has(chatId));
}

/**
 * Shell-level attention state, deliberately separate from the selected chat's
 * prompt details. Both the recent rail and the all-chats panel subscribe here.
 */
export const useChatAttention = create<ChatAttentionStore>()((set) => ({
  chatIdsWithPendingPrompts: new Set(),
  setChatIdsWithPendingPrompts: (chatIds) => {
    const next = new Set(chatIds);
    set((state) =>
      sameChatIds(state.chatIdsWithPendingPrompts, next)
        ? state
        : { chatIdsWithPendingPrompts: next },
    );
  },
  clear: () =>
    set((state) =>
      state.chatIdsWithPendingPrompts.size === 0
        ? state
        : { chatIdsWithPendingPrompts: new Set() },
    ),
}));
