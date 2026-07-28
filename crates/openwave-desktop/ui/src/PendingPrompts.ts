import { create } from "zustand";

import type {
  PendingFolderAccessRequest,
  PendingOutputWritebackRequest,
  PendingUserQuestions,
} from "./api";

/**
 * What the agent is waiting on the reader for, in the conversation they have
 * open.
 *
 * This is deliberately not view state. The agent parks a turn until it is
 * answered, and being told about that must not depend on which screen happens
 * to be rendered — so the watcher that fills this store lives in the shell and
 * the views only read from it. See [useChatPromptWatcher].
 */
export type PendingPromptsStore = {
  /** The conversation these requests belong to; null before the first read. */
  chatId: string | null;
  userQuestions: PendingUserQuestions[];
  folderAccess: PendingFolderAccessRequest[];
  outputWritebacks: PendingOutputWritebackRequest[];
  /**
   * Ask the watcher to read again. Acting on a request should show its result
   * immediately rather than at the next poll.
   */
  refresh: () => void;
  setUserQuestions: (chatId: string, requests: PendingUserQuestions[]) => void;
  setFolderAccess: (chatId: string, requests: PendingFolderAccessRequest[]) => void;
  setOutputWritebacks: (
    chatId: string,
    requests: PendingOutputWritebackRequest[],
  ) => void;
  setRefresh: (refresh: () => void) => void;
  /** Drop everything held for a conversation that is no longer the open one. */
  reset: (chatId: string | null) => void;
};

export function createPendingPromptsStore() {
  return create<PendingPromptsStore>()((set, get) => ({
    chatId: null,
    userQuestions: [],
    folderAccess: [],
    outputWritebacks: [],
    refresh: () => {},
    // A read that lands after the reader has moved on describes a conversation
    // nobody is looking at, and writing it would put another chat's questions
    // on screen.
    setUserQuestions: (chatId, userQuestions) => {
      if (get().chatId !== chatId) return;
      set({ userQuestions });
    },
    setFolderAccess: (chatId, folderAccess) => {
      if (get().chatId !== chatId) return;
      set({ folderAccess });
    },
    setOutputWritebacks: (chatId, outputWritebacks) => {
      if (get().chatId !== chatId) return;
      set({ outputWritebacks });
    },
    setRefresh: (refresh) => set({ refresh }),
    reset: (chatId) =>
      set({ chatId, userQuestions: [], folderAccess: [], outputWritebacks: [] }),
  }));
}

export const usePendingPrompts = createPendingPromptsStore();
