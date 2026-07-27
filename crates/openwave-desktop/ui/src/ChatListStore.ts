import { create } from "zustand";
import type { Chat } from "./api";

/**
 * The chat list and its mutation progress. State only — the async mutations
 * (create/delete/rename/select orchestration with the confirm dialog, fences,
 * and session lifecycle) stay with the caller, which reads and writes here so
 * the sidebar can subscribe directly instead of receiving drilled props.
 */
export type ChatListStore = {
  chats: Chat[];
  /**
   * Whether the list has been fetched. An empty list means something different
   * before and after the first load — "no chats yet, make one" versus "not
   * asked yet" — and the home route has to tell them apart.
   */
  chatsLoaded: boolean;
  /** The selected chat; `null` only before the first chat resolves at boot. */
  selected: Chat | null;
  chatsError: string | null;
  creatingChat: boolean;
  deletingChatId: string | null;
  renamingChatId: string | null;
  renameChatDraft: string;
  savingTitle: boolean;
  setChats: (chats: Chat[]) => void;
  setSelected: (chat: Chat | null) => void;
  /** Replace a chat everywhere it appears (list and selection) by id. */
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
    selected: null,
    chatsError: null,
    creatingChat: false,
    deletingChatId: null,
    renamingChatId: null,
    renameChatDraft: "",
    savingTitle: false,
    setChats: (chats) => set({ chats, chatsLoaded: true }),
    setSelected: (selected) => set({ selected }),
    replaceChat: (chat) =>
      set((state) => ({
        chats: state.chats.map((item) => (item.id === chat.id ? chat : item)),
        selected: state.selected?.id === chat.id ? chat : state.selected,
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
