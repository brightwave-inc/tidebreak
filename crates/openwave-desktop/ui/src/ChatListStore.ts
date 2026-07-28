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
  /**
   * The chat whose name arrived from the server while this window watched, so
   * the surfaces that show it can type it out once instead of blinking it in.
   *
   * A chat id rather than a boolean because the name belongs to one row, and it
   * is cleared on leaving that conversation so returning to it later shows the
   * settled name rather than replaying the animation.
   */
  derivedTitleChatId: string | null;
  setChats: (chats: Chat[]) => void;
  /** Replace a chat in the list by id. */
  replaceChat: (chat: Chat) => void;
  /** Take a title the server derived, and mark it as newly arrived. */
  applyDerivedTitle: (chatId: string, title: string) => void;
  /** Forget the arrival, so the name is just the name from here on. */
  clearDerivedTitle: () => void;
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
    derivedTitleChatId: null,
    setChats: (chats) => set({ chats, chatsLoaded: true }),
    replaceChat: (chat) =>
      set((state) => ({
        chats: state.chats.map((item) => (item.id === chat.id ? chat : item)),
      })),
    applyDerivedTitle: (chatId, title) =>
      set((state) => {
        const known = state.chats.find((item) => item.id === chatId);
        // A name this window already shows is not news. The socket restates the
        // current name on every connect — that is what covers a title stored
        // before the renderer was listening — so without this, reconnecting
        // would replay the animation for a name that has been there all along.
        if (!known || known.title === title) return {};
        return {
          chats: state.chats.map((item) =>
            item.id === chatId ? { ...item, title } : item,
          ),
          derivedTitleChatId: chatId,
        };
      }),
    clearDerivedTitle: () => set({ derivedTitleChatId: null }),
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
