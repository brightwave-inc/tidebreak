import { create } from "zustand";
import type { Chat } from "./api";

/**
 * The chat list and its mutation progress. State only — the async mutations
 * (create/delete/rename orchestration with the confirm dialog, fences, and
 * session lifecycle) stay with the caller, which reads and writes here so the
 * rails can subscribe directly instead of receiving drilled props.
 *
 * Which conversation is open is deliberately absent: that lives in the URL and
 * is read with {@link useActiveChatId}. A second copy here would be a copy that
 * can disagree, and the one it replaced did — it was written on entering a chat
 * and never cleared on the way out, so the shell went on believing a chat was
 * open while the reader stood on home.
 */
export type ChatListStore = {
  chats: Chat[];
  /**
   * Whether the list has been fetched. An empty list means something different
   * before and after the first load — "no chats yet, make one" versus "not
   * asked yet" — and the home route has to tell them apart.
   */
  chatsLoaded: boolean;
  chatsError: string | null;
  creatingChat: boolean;
  deletingChatId: string | null;
  renamingChatId: string | null;
  renameChatDraft: string;
  savingTitle: boolean;
  setChats: (chats: Chat[]) => void;
  /** Replace a chat in the list by id. */
  replaceChat: (chat: Chat) => void;
  prependChat: (chat: Chat) => void;
  setChatsError: (error: string | null) => void;
  setCreatingChat: (creating: boolean) => void;
  setDeletingChatId: (chatId: string | null) => void;
  beginRename: (chat: Chat) => void;
  setRenameDraft: (draft: string) => void;
  setSavingTitle: (saving: boolean) => void;
  endRename: () => void;
};

export function createChatListStore() {
  return create<ChatListStore>()((set) => ({
    chats: [],
    chatsLoaded: false,
    chatsError: null,
    creatingChat: false,
    deletingChatId: null,
    renamingChatId: null,
    renameChatDraft: "",
    savingTitle: false,
    setChats: (chats) => set({ chats, chatsLoaded: true }),
    replaceChat: (chat) =>
      set((state) => ({
        chats: state.chats.map((item) => (item.id === chat.id ? chat : item)),
      })),
    prependChat: (chat) => set((state) => ({ chats: [chat, ...state.chats] })),
    setChatsError: (chatsError) => set({ chatsError }),
    setCreatingChat: (creatingChat) => set({ creatingChat }),
    setDeletingChatId: (deletingChatId) => set({ deletingChatId }),
    beginRename: (chat) =>
      set({ renamingChatId: chat.id, renameChatDraft: chat.title ?? "" }),
    setRenameDraft: (renameChatDraft) => set({ renameChatDraft }),
    setSavingTitle: (savingTitle) => set({ savingTitle }),
    endRename: () =>
      set({ renamingChatId: null, renameChatDraft: "", savingTitle: false }),
  }));
}

export const useChatListStore = createChatListStore();
