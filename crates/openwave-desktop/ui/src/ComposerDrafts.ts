import { create } from "zustand";

import type { ImportedDocument } from "./documents";
import type { ImageAttachment } from "./ImageAttachments";

const KEY_PREFIX = "composer_";
const ATTACHMENTS_PREFIX = "composer_attachments_";

/** The home composer's draft, written before a chat exists to hold it. */
export const HOME_DRAFT_KEY = "home";

function storageKeyFor(key: string): string {
  return `${KEY_PREFIX}${key}`;
}

function attachmentsStorageKeyFor(key: string): string {
  return `${ATTACHMENTS_PREFIX}${key}`;
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

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * What one composer's attachment strip holds but has not sent.
 *
 * `pendingChatId` is written by the home composer alone. Home attaches files
 * before a chat exists, so a chat is created silently to publish them to, and
 * its id has to survive with the attachments — a restored draft without it
 * would offer images no chat will accept. A chat's own composer leaves it
 * null: its key is the chat id already.
 */
export type ComposerAttachmentDraft = {
  images: ImageAttachment[];
  files: ImportedDocument[];
  pendingChatId: string | null;
};

export const EMPTY_ATTACHMENT_DRAFT: ComposerAttachmentDraft = {
  images: [],
  files: [],
  pendingChatId: null,
};

function isEmptyAttachmentDraft(draft: ComposerAttachmentDraft): boolean {
  return (
    draft.images.length === 0 &&
    draft.files.length === 0 &&
    draft.pendingChatId === null
  );
}

/**
 * Only what a reload can honestly restore goes into the session. A published
 * image is server identity plus geometry, so it re-sends as-is; a queued or
 * failed one is a promise to move bytes the renderer holds in a `File`, which
 * no storage can keep — restoring its chip would offer a retry with nothing
 * behind it. Object URLs die with the page, so restored chips fall back to
 * format and geometry the way host-picked images always do.
 */
function storableDraft(
  draft: ComposerAttachmentDraft,
): ComposerAttachmentDraft {
  return {
    images: draft.images
      .filter((image) => image.status === "ready" && image.attachmentId !== null)
      .map((image) => ({ ...image, previewUrl: null })),
    files: draft.files,
    pendingChatId: draft.pendingChatId,
  };
}

function parseStoredAttachmentDraft(value: string): ComposerAttachmentDraft | null {
  try {
    const parsed: unknown = JSON.parse(value);
    if (!isRecord(parsed)) return null;
    const images = (Array.isArray(parsed.images) ? parsed.images : [])
      .filter(
        (image): image is ImageAttachment =>
          isRecord(image) &&
          image.status === "ready" &&
          typeof image.attachmentId === "string",
      )
      .map((image) => ({ ...image, previewUrl: null }));
    const files = (Array.isArray(parsed.files) ? parsed.files : []).filter(
      (file): file is ImportedDocument =>
        isRecord(file) &&
        typeof file.documentId === "string" &&
        typeof file.displayName === "string",
    );
    return {
      images,
      files,
      pendingChatId:
        typeof parsed.pendingChatId === "string" ? parsed.pendingChatId : null,
    };
  } catch {
    return null;
  }
}

function readStoredAttachmentDrafts(): Record<string, ComposerAttachmentDraft> {
  const drafts: Record<string, ComposerAttachmentDraft> = {};
  try {
    const storage = window.sessionStorage;
    for (let index = 0; index < storage.length; index += 1) {
      const name = storage.key(index);
      if (!name?.startsWith(ATTACHMENTS_PREFIX)) continue;
      const value = storage.getItem(name);
      if (!value) continue;
      const draft = parseStoredAttachmentDraft(value);
      if (draft && !isEmptyAttachmentDraft(draft)) {
        drafts[name.slice(ATTACHMENTS_PREFIX.length)] = draft;
      }
    }
  } catch {
    // A composer with no memory still works; an unreadable store is not fatal.
  }
  return drafts;
}

function writeStoredAttachmentDraft(
  key: string,
  draft: ComposerAttachmentDraft,
): void {
  try {
    if (isEmptyAttachmentDraft(draft)) {
      window.sessionStorage.removeItem(attachmentsStorageKeyFor(key));
    } else {
      window.sessionStorage.setItem(
        attachmentsStorageKeyFor(key),
        JSON.stringify(storableDraft(draft)),
      );
    }
  } catch {
    // Persisting a draft is best-effort; the session still holds it.
  }
}

/**
 * What a composer is holding but has not sent, kept per composer: the typed
 * text and the attachment strip.
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
  attachments: Record<string, ComposerAttachmentDraft>;
  setDraft: (key: string, text: string) => void;
  /**
   * Forget a composer's whole draft — text, attachments, everything. For when
   * the chat the draft belonged to no longer exists to send it to.
   */
  clearDraft: (key: string) => void;
  setImages: (key: string, images: ImageAttachment[]) => void;
  setFiles: (key: string, files: ImportedDocument[]) => void;
  setPendingChatId: (key: string, chatId: string | null) => void;
};

export function createComposerDraftStore() {
  return create<ComposerDraftStore>()((set, get) => {
    function updateAttachments(
      key: string,
      change: (current: ComposerAttachmentDraft) => ComposerAttachmentDraft,
    ): void {
      const next = change(
        get().attachments[key] ?? EMPTY_ATTACHMENT_DRAFT,
      );
      writeStoredAttachmentDraft(key, next);
      set((state) => {
        const attachments = { ...state.attachments };
        if (isEmptyAttachmentDraft(next)) delete attachments[key];
        else attachments[key] = next;
        return { attachments };
      });
    }

    return {
      drafts: readStoredDrafts(),
      attachments: readStoredAttachmentDrafts(),
      setDraft: (key, text) => {
        writeStoredDraft(key, text);
        set((state) => ({ drafts: { ...state.drafts, [key]: text } }));
      },
      clearDraft: (key) => {
        writeStoredDraft(key, "");
        writeStoredAttachmentDraft(key, EMPTY_ATTACHMENT_DRAFT);
        set((state) => {
          const drafts = { ...state.drafts };
          delete drafts[key];
          const attachments = { ...state.attachments };
          delete attachments[key];
          return { drafts, attachments };
        });
      },
      setImages: (key, images) =>
        updateAttachments(key, (current) => ({ ...current, images })),
      setFiles: (key, files) =>
        updateAttachments(key, (current) => ({ ...current, files })),
      setPendingChatId: (key, chatId) =>
        updateAttachments(key, (current) => ({
          ...current,
          pendingChatId: chatId,
        })),
    };
  });
}

export const useComposerDrafts = createComposerDraftStore();

/** This composer's unsent text, restored from a previous visit if there is one. */
export function useComposerDraft(key: string): string {
  return useComposerDrafts((state) => state.drafts[key] ?? "");
}

/** This composer's unsent attachments, restored from a previous visit if there are any. */
export function useComposerAttachments(key: string): ComposerAttachmentDraft {
  return useComposerDrafts(
    (state) => state.attachments[key] ?? EMPTY_ATTACHMENT_DRAFT,
  );
}
