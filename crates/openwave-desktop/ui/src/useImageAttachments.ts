import { useRef, useState } from "react";

import type { ApiClient } from "./api";
import { publishChatImage } from "./attachments";
import { useComposerDrafts } from "./ComposerDrafts";
import { hasNativeHost } from "./host";
import {
  imageAttachmentName,
  imageAttachmentRejection,
  queuedImageAttachment,
  readyImageAttachment,
  uploadImageAttachment,
  withRetryQueued,
  withUploadFailed,
  withUploadProgress,
  withUploadPublished,
  withUploadStarted,
  withoutAttachment,
  type ImageAttachment,
  type PickedImage,
} from "./ImageAttachments";

export type ImageAttachmentControls = {
  attachments: ImageAttachment[];
  /** Why the last attach was refused outright, before any bytes moved. */
  error: string | null;
  /** Take images the host has already published, from the composer's picker. */
  adopt: (published: readonly PickedImage[]) => void;
  attachFiles: (files: readonly File[]) => void;
  remove: (id: string) => void;
  retry: (id: string) => void;
  /** Forget everything, once a turn has carried it. */
  clear: () => void;
};

const NO_IMAGES: ImageAttachment[] = [];

/**
 * The bytes behind the chips, which no store can hold.
 *
 * The attachment list itself lives in the composer draft store, so it survives
 * the route remount that every chat switch causes. What cannot go there — the
 * `File` a retry re-reads, the upload to abort, the preview's object URL — is
 * kept here per conversation, for exactly as long as the draft it belongs to.
 */
type ImageBacking = {
  files: Map<string, File>;
  aborts: Map<string, AbortController>;
  previews: Map<string, string>;
};

const backingByChat = new Map<string, ImageBacking>();

function backingFor(chatId: string): ImageBacking {
  let backing = backingByChat.get(chatId);
  if (!backing) {
    backing = { files: new Map(), aborts: new Map(), previews: new Map() };
    backingByChat.set(chatId, backing);
  }
  return backing;
}

function forgetBacking(chatId: string, id: string): void {
  const backing = backingByChat.get(chatId);
  if (!backing) return;
  backing.aborts.get(id)?.abort();
  backing.aborts.delete(id);
  const preview = backing.previews.get(id);
  if (preview) URL.revokeObjectURL(preview);
  backing.previews.delete(id);
  backing.files.delete(id);
}

/**
 * Hand every object URL and in-flight upload for one conversation back. An
 * object URL outlives the element that rendered it, so it has to be handed
 * back explicitly — but only when the draft itself is gone. Revoking on
 * unmount would destroy the very thing the store just kept.
 */
function releaseBacking(chatId: string): void {
  const backing = backingByChat.get(chatId);
  if (!backing) return;
  for (const controller of backing.aborts.values()) controller.abort();
  for (const url of backing.previews.values()) URL.revokeObjectURL(url);
  backingByChat.delete(chatId);
}

// A draft entry that disappears entirely — the chat was deleted, its composer
// cleared from outside the route — takes its backing with it. Removals the
// hook performs itself are forgotten one id at a time instead.
useComposerDrafts.subscribe((state, previous) => {
  for (const chatId of Object.keys(previous.attachments)) {
    if (!(chatId in state.attachments)) releaseBacking(chatId);
  }
});

function setComposerImages(
  chatId: string,
  change: (current: readonly ImageAttachment[]) => ImageAttachment[],
): void {
  const current = useComposerDrafts.getState().attachments[chatId]?.images ?? [];
  useComposerDrafts.getState().setImages(chatId, change(current));
}

/**
 * The images waiting on one conversation's composer.
 *
 * Two ways in, one state machine. A file the renderer already holds — dropped
 * or pasted — is uploaded from here, with real byte progress and a preview made
 * from the local bytes. A file chosen through the native picker is read and
 * published entirely in the host, so it arrives already finished and with no
 * pixels to preview. Both land in the same list, so removal, retry, and send
 * gating cannot behave differently depending on how the image got here.
 *
 * The list is the conversation's composer draft, so switching chats and coming
 * back finds the strip as it was left — including an upload that finished
 * while the reader was looking at another chat. The `File` behind each upload
 * is kept until the attachment leaves the list. That is what makes retry a
 * retry, rather than an invitation to go and find the file again.
 */
export function useImageAttachments(
  client: ApiClient,
  chatId: string,
): ImageAttachmentControls {
  const attachments = useComposerDrafts(
    (state) => state.attachments[chatId]?.images ?? NO_IMAGES,
  );
  const [error, setError] = useState<string | null>(null);
  const attachmentsRef = useRef<ImageAttachment[]>([]);

  attachmentsRef.current = attachments;

  function update(
    change: (current: readonly ImageAttachment[]) => ImageAttachment[],
  ) {
    setComposerImages(chatId, change);
  }

  /**
   * Move one file's bytes, by whichever route this build has.
   *
   * Under a native host the server mounts the image publish endpoint behind the
   * client-executor token, so the renderer cannot post to it and the bytes go
   * over IPC for the host to publish. In a browser — `pnpm dev`, and the UI
   * tests — the same endpoint sits on the renderer's own bearer, and posting it
   * directly is what gives the chip real byte progress.
   */
  async function publish(id: string, file: File, signal: AbortSignal) {
    if (!hasNativeHost()) {
      return uploadImageAttachment(client, chatId, file, {
        onProgress: (uploadedBytes) =>
          update((current) => withUploadProgress(current, id, uploadedBytes)),
        signal,
      });
    }
    // One IPC call with no cancellation seam, so a removal mid-flight is
    // honoured on the way out instead of interrupting it. The bytes are
    // published by then, but nothing references them, so the server's orphan
    // sweep reclaims them.
    const published = await publishChatImage(chatId, file);
    if (signal.aborted) throw new DOMException("Upload cancelled", "AbortError");
    return published;
  }

  async function upload(id: string) {
    const backing = backingFor(chatId);
    const file = backing.files.get(id);
    if (!file) return;
    const controller = new AbortController();
    backing.aborts.set(id, controller);
    update((current) => withUploadStarted(current, id));
    try {
      const published = await publish(id, file, controller.signal);
      update((current) => withUploadPublished(current, id, published));
    } catch (err) {
      // A cancelled upload belongs to an attachment the reader already removed,
      // so there is no chip left to carry the message.
      if (err instanceof DOMException && err.name === "AbortError") return;
      update((current) => withUploadFailed(current, id, failureText(err)));
    } finally {
      backing.aborts.delete(id);
    }
  }

  function attachFiles(files: readonly File[]) {
    if (files.length === 0) return;
    const rejection = imageAttachmentRejection(attachmentsRef.current, files);
    if (rejection) {
      setError(rejection);
      return;
    }
    setError(null);
    const backing = backingFor(chatId);
    const now = new Date();
    const queued = files.map((file) => {
      const id = crypto.randomUUID();
      backing.files.set(id, file);
      const previewUrl = URL.createObjectURL(file);
      backing.previews.set(id, previewUrl);
      return queuedImageAttachment(id, {
        name: imageAttachmentName(file, now),
        byteLen: file.size,
        previewUrl,
      });
    });
    update((current) => [...current, ...queued]);
    for (const attachment of queued) void upload(attachment.id);
  }

  function adopt(published: readonly PickedImage[]) {
    if (published.length === 0) return;
    setError(null);
    update((current) => [
      ...current,
      ...published.map((image) =>
        readyImageAttachment(crypto.randomUUID(), image),
      ),
    ]);
  }

  return {
    attachments,
    error,
    adopt,
    attachFiles,
    remove: (id) => {
      forgetBacking(chatId, id);
      update((current) => withoutAttachment(current, id));
    },
    retry: (id) => {
      update((current) => withRetryQueued(current, id));
      void upload(id);
    },
    clear: () => {
      for (const attachment of attachmentsRef.current) {
        forgetBacking(chatId, attachment.id);
      }
      update(() => []);
      setError(null);
    },
  };
}

function failureText(error: unknown): string {
  const message = String(error).replace(/^Error:\s*/, "").trim();
  return message && message.length <= 240
    ? message
    : "Could not attach that image.";
}
