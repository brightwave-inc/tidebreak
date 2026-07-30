import { create } from "zustand";

const KEY_PREFIX = "composer_";

/** The home composer's draft, written before a chat exists to hold it. */
export const HOME_DRAFT_KEY = "home";

function storageKeyFor(key: string): string {
  return `${KEY_PREFIX}${key}`;
}

function readStoredDrafts(): Record<string, string> {
  const drafts: Record<string, string> = {};
  try {
    const storage = window.sessionStorage;
    for (let index = 0; index < storage.length; index += 1) {
      const name = storage.key(index);
      if (!name?.startsWith(KEY_PREFIX)) continue;
      const text = storage.getItem(name);
      if (text) drafts[name.slice(KEY_PREFIX.length)] = text;
    }
  } catch {
    // A composer with no memory still works; an unreadable store is not fatal.
  }
  return drafts;
}

function writeStoredDraft(key: string, text: string): void {
  try {
    if (text) window.sessionStorage.setItem(storageKeyFor(key), text);
    else window.sessionStorage.removeItem(storageKeyFor(key));
  } catch {
    // Persisting a draft is best-effort; the session still holds it.
  }
}

/**
 * What is typed but not yet sent, kept per composer.
 *
 * The chat route is remounted per conversation, so a draft held in the route
 * dies the moment you look at another chat — which is exactly when people
 * switch away to check something they were about to reference. The draft
 * belongs to the conversation, not to the mounted route, so it lives here.
 *
 * Session storage rather than local: an unsent message should survive
 * navigation and a reload, but a draft still sitting there weeks later is
 * clutter, not a courtesy. Keys are the chat id, or [HOME_DRAFT_KEY] for the
 * composer that has no chat yet.
 */
export type ComposerDraftStore = {
  drafts: Record<string, string>;
  setDraft: (key: string, text: string) => void;
  clearDraft: (key: string) => void;
};

export function createComposerDraftStore() {
  return create<ComposerDraftStore>()((set) => ({
    drafts: readStoredDrafts(),
    setDraft: (key, text) => {
      writeStoredDraft(key, text);
      set((state) => ({ drafts: { ...state.drafts, [key]: text } }));
    },
    clearDraft: (key) => {
      writeStoredDraft(key, "");
      set((state) => {
        if (!(key in state.drafts)) return state;
        const drafts = { ...state.drafts };
        delete drafts[key];
        return { drafts };
      });
    },
  }));
}

export const useComposerDrafts = createComposerDraftStore();

/** This composer's unsent text, restored from a previous visit if there is one. */
export function useComposerDraft(key: string): string {
  return useComposerDrafts((state) => state.drafts[key] ?? "");
}
