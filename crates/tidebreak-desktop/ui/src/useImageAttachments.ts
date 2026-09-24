import { useRef, useState } from "react";

import type { ApiClient } from "./api";
import { publishChatImage, publishCodeImage } from "./attachments";
import { useComposerDrafts } from "./ComposerDrafts";
import { hasLocalHostAuthority } from "./host";
import {
  heldImageAttachment,
  imageAttachmentName,
  imageAttachmentRejection,
  isTextDocumentAttachmentName,
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
  /**
   * Put a cleared strip back after a refused send. Local previews are gone —
   * those object URLs were revoked with the clear — so ready chips fall back
   * to name and geometry the way host-picked images always do.
   */
  restore: (items: readonly ImageAttachment[]) => void;
};

/** Which conversation family the bytes are published into. */
export type ImagePublishScope = "chat" | "code";

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
// hook performs itself are forgotten one id at a time instead. A draft that
// moved to another key took its backing along first, so nothing is lost.
useComposerDrafts.subscribe((state, previous) => {
  for (const chatId of Object.keys(previous.attachments)) {
    if (!(chatId in state.attachments)) releaseBacking(chatId);
  }
});

function setComposerImages(
  chatId: string,
  change: (current: readonly ImageAttachment[]) => ImageAttachment[],
): void {
  const current =
    useComposerDrafts.getState().attachments[chatId]?.images ?? [];
  useComposerDrafts.getState().setImages(chatId, change(current));
}

function readComposerImages(draftKey: string): readonly ImageAttachment[] {
  return useComposerDrafts.getState().attachments[draftKey]?.images ?? [];
}

/**
 * Move one file's bytes, by whichever route this build has.
 *
 * Under a native host the server mounts the image publish endpoint behind the
 * client-executor token, so the renderer cannot post to it and the bytes go
 * over IPC for the host to publish. In a browser — `pnpm dev`, and the UI
 * tests — the same endpoint sits on the renderer's own bearer, and posting it
 * directly is what gives the chip real byte progress.
 *
 * A window attached to a remote machine takes the browser route too. The
 * host would publish into the store on this computer, where the conversation
 * does not exist, so the bytes have to go to the machine that holds it.
 */
async function publishFile(
  client: ApiClient,
  scope: ImagePublishScope,
  targetId: string,
  file: File,
  signal: AbortSignal,
  onProgress: (uploadedBytes: number) => void,
) {
  if (!hasLocalHostAuthority()) {
    return uploadImageAttachment(client, targetId, file, {
      onProgress,
      signal,
      path:
        scope === "code"
          ? (id) => `/sessions/${encodeURIComponent(id)}/attachments/images`
          : undefined,
    });
  }
  // One IPC call with no cancellation seam, so a removal mid-flight is
  // honoured on the way out instead of interrupting it. The bytes are
  // published by then, but nothing references them, so the server's orphan
  // sweep reclaims them.
  const published =
    scope === "code"
      ? await publishCodeImage(targetId, file)
      : await publishChatImage(targetId, file);
  if (signal.aborted) throw new DOMException("Upload cancelled", "AbortError");
  return published;
}

/**
 * Upload one attachment of the composer `draftKey`, moving its chip through
 * uploading to ready or failed. Resolves either way; the chip carries the
 * outcome.
 */
async function uploadAttachment(
  client: ApiClient,
  draftKey: string,
  id: string,
  scope: ImagePublishScope,
  target: () => Promise<string>,
): Promise<void> {
  const backing = backingFor(draftKey);
  const file = backing.files.get(id);
  if (!file) return;
  const controller = new AbortController();
  backing.aborts.set(id, controller);
  const update = (
    change: (current: readonly ImageAttachment[]) => ImageAttachment[],
  ) => setComposerImages(draftKey, change);
  update((current) => withUploadStarted(current, id));
  try {
    const targetId = await target();
    const published = await publishFile(
      client,
      scope,
      targetId,
      file,
      controller.signal,
      (uploadedBytes) =>
        update((current) => withUploadProgress(current, id, uploadedBytes)),
    );
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

/**
 * Publish every image the composer `draftKey` holds to `targetId`.
 *
 * A held image was attached before its conversation existed. The send that
 * creates the conversation calls this, so the chips show a real upload from
 * that moment. Resolves once every held image is ready; rejects if one fails,
 * leaving the failed chip in place for a retry.
 */
export async function publishHeldImages(
  client: ApiClient,
  draftKey: string,
  targetId: string,
  scope: ImagePublishScope,
): Promise<void> {
  const held = readComposerImages(draftKey).filter(
    (item) => item.status === "held",
  );
  await Promise.all(
    held.map((item) =>
      uploadAttachment(client, draftKey, item.id, scope, async () => targetId),
    ),
  );
  const failed = readComposerImages(draftKey).find(
    (item) =>
      held.some((heldItem) => heldItem.id === item.id) &&
      item.status === "failed",
  );
  if (failed) {
    throw new Error(failed.error ?? "An image could not be attached.");
  }
}

/**
 * Add image files to a composer as held images, without a mounted composer.
 *
 * Returns why the files were refused, or `null`. The new-workspace dialog and
 * Uneff me put their images on the new session's composer this way before the
 * send publishes them.
 */
export function holdComposerImages(
  draftKey: string,
  files: readonly File[],
): string | null {
  return addFiles(draftKey, files, heldImageAttachment);
}

/**
 * Move the bytes behind one composer's chips to another composer, with the
 * draft they belong to. The start surface hands its draft to the session it
 * creates this way.
 */
export function moveImageBacking(from: string, to: string): void {
  if (from === to) return;
  const source = backingByChat.get(from);
  if (!source) return;
  backingByChat.delete(from);
  const target = backingFor(to);
  for (const [id, file] of source.files) target.files.set(id, file);
  for (const [id, controller] of source.aborts)
    target.aborts.set(id, controller);
  for (const [id, url] of source.previews) target.previews.set(id, url);
}

/**
 * Take the files behind a composer's held images, in chip order, and clear
 * its strip. The new-workspace dialog hands them to the session it creates.
 */
export function takeHeldImageFiles(draftKey: string): File[] {
  const backing = backingByChat.get(draftKey);
  const files: File[] = [];
  for (const item of readComposerImages(draftKey)) {
    const file = backing?.files.get(item.id);
    if (item.status === "held" && file) files.push(file);
  }
  for (const item of readComposerImages(draftKey)) {
    forgetBacking(draftKey, item.id);
  }
  setComposerImages(draftKey, () => []);
  return files;
}

/**
 * Let go of the bytes behind chips a sent turn carried. Their previews and
 * files are no longer needed once the server holds the images.
 */
export function forgetComposerImages(
  draftKey: string,
  ids: readonly string[],
): void {
  for (const id of ids) forgetBacking(draftKey, id);
}

/**
 * Move a composer's whole draft — text, pasted text, and images with the
 * bytes behind them — to another composer.
 *
 * The bytes move first. Removing the draft from `from` otherwise reads as a
 * cleared composer and hands its previews and uploads back.
 */
export function moveComposerDraft(from: string, to: string): void {
  if (from === to) return;
  moveImageBacking(from, to);
  useComposerDrafts.getState().moveDraft(from, to);
}

/** The bytes behind one composer's chips, held aside while a send runs. */
export type DetachedImageBacking = ImageBacking;

/**
 * Take the bytes behind a composer's chips out of it, so emptying the strip
 * for a send does not hand the previews back.
 *
 * A refused send puts them back with {@link reattachImageBacking}, and its
 * chips come back with their thumbnails. An accepted one lets them go with
 * {@link releaseDetachedBacking}.
 */
export function detachImageBacking(
  draftKey: string,
): DetachedImageBacking | null {
  const backing = backingByChat.get(draftKey) ?? null;
  backingByChat.delete(draftKey);
  return backing;
}

/** Put bytes taken by {@link detachImageBacking} back under a composer. */
export function reattachImageBacking(
  draftKey: string,
  backing: DetachedImageBacking | null,
): void {
  if (!backing) return;
  const target = backingFor(draftKey);
  for (const [id, file] of backing.files) target.files.set(id, file);
  for (const [id, controller] of backing.aborts)
    target.aborts.set(id, controller);
  for (const [id, url] of backing.previews) target.previews.set(id, url);
}

/** Hand back the previews and uploads of bytes a sent turn carried. */
export function releaseDetachedBacking(
  backing: DetachedImageBacking | null,
): void {
  if (!backing) return;
  for (const controller of backing.aborts.values()) controller.abort();
  for (const url of backing.previews.values()) URL.revokeObjectURL(url);
}

function addFiles(
  draftKey: string,
  files: readonly File[],
  make: typeof queuedImageAttachment,
): string | null {
  const images = files.filter(
    (file) => !isTextDocumentAttachmentName(file.name),
  );
  if (images.length === 0) return null;
  const rejection = imageAttachmentRejection(
    readComposerImages(draftKey),
    images,
  );
  if (rejection) return rejection;
  const backing = backingFor(draftKey);
  const now = new Date();
  const added = images.map((file) => {
    const id = crypto.randomUUID();
    backing.files.set(id, file);
    const previewUrl =
      typeof URL.createObjectURL === "function"
        ? URL.createObjectURL(file)
        : null;
    if (previewUrl) backing.previews.set(id, previewUrl);
    return make(id, {
      name: imageAttachmentName(file, now),
      byteLen: file.size,
      previewUrl,
    });
  });
  setComposerImages(draftKey, (current) => [...current, ...added]);
  return null;
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
 *
 * @param draftKey which composer's strip this is — a chat id, or the home
 *   composer's key.
 * @param publishTarget the chat the bytes belong to, when that is not the
 *   draft's own key. Home attaches before its chat exists, so the target is
 *   resolved at the moment the bytes move rather than at mount, and creating
 *   the chat is part of resolving it.
 * @param options.hold hold attached files instead of uploading them, for a
 *   composer whose conversation does not exist yet and cannot be created on
 *   attach. The send publishes them with {@link publishHeldImages}.
 */
export function useImageAttachments(
  client: ApiClient,
  draftKey: string,
  publishTarget?: () => Promise<string>,
  scope: ImagePublishScope = "chat",
  options: { hold?: boolean } = {},
): ImageAttachmentControls {
  const attachments = useComposerDrafts(
    (state) => state.attachments[draftKey]?.images ?? NO_IMAGES,
  );
  const [error, setError] = useState<string | null>(null);
  const attachmentsRef = useRef<ImageAttachment[]>([]);
  const publishTargetRef = useRef(publishTarget);

  attachmentsRef.current = attachments;
  publishTargetRef.current = publishTarget;

  function update(
    change: (current: readonly ImageAttachment[]) => ImageAttachment[],
  ) {
    setComposerImages(draftKey, change);
  }

  function upload(id: string) {
    return uploadAttachment(client, draftKey, id, scope, async () =>
      publishTargetRef.current ? publishTargetRef.current() : draftKey,
    );
  }

  function attachFiles(files: readonly File[]) {
    if (files.every((file) => isTextDocumentAttachmentName(file.name))) return;
    const before = new Set(readComposerImages(draftKey).map((item) => item.id));
    const rejection = addFiles(
      draftKey,
      files,
      options.hold ? heldImageAttachment : queuedImageAttachment,
    );
    if (rejection) {
      setError(rejection);
      return;
    }
    setError(null);
    if (options.hold) return;
    for (const attachment of readComposerImages(draftKey)) {
      if (!before.has(attachment.id) && attachment.status === "queued") {
        void upload(attachment.id);
      }
    }
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
      forgetBacking(draftKey, id);
      update((current) => withoutAttachment(current, id));
    },
    retry: (id) => {
      update((current) => withRetryQueued(current, id));
      void upload(id);
    },
    clear: () => {
      for (const attachment of attachmentsRef.current) {
        forgetBacking(draftKey, attachment.id);
      }
      update(() => []);
      setError(null);
    },
    restore: (items) => {
      update((current) => {
        const currentIds = new Set(current.map((item) => item.id));
        const restored = items
          .filter((item) => !currentIds.has(item.id))
          .map((item) => ({ ...item, previewUrl: null }));
        return [...restored, ...current];
      });
    },
  };
}

function failureText(error: unknown): string {
  const message = String(error)
    .replace(/^Error:\s*/, "")
    .trim();
  return message && message.length <= 240
    ? message
    : "Could not attach that image.";
}
