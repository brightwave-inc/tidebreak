import { useEffect, useRef, useState } from "react";

import type { ApiClient } from "./api";
import {
  attachChatImage,
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
  MAX_IMAGE_ATTACHMENTS,
  type ImageAttachment,
} from "./ImageAttachments";
import {
  PICKER_BUSY_MESSAGE,
  PICKER_HOLDERS,
  useNativePickerLatch,
} from "./NativePickerLatch";

export type ImageAttachmentControls = {
  attachments: ImageAttachment[];
  /** The native picker is open, so a second one would be refused by the host. */
  attachingFromPicker: boolean;
  /** Why the last attach was refused outright, before any bytes moved. */
  error: string | null;
  attachFromPicker: () => void;
  attachFiles: (files: readonly File[]) => void;
  remove: (id: string) => void;
  retry: (id: string) => void;
  /** Forget everything, once a turn has carried it. */
  clear: () => void;
};

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
 * The `File` behind each upload is kept until the attachment leaves the list.
 * That is what makes retry a retry, rather than an invitation to go and find
 * the file again.
 */
export function useImageAttachments(
  client: ApiClient,
  chatId: string,
): ImageAttachmentControls {
  const [attachments, setAttachments] = useState<ImageAttachment[]>([]);
  const [attachingFromPicker, setAttachingFromPicker] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const attachmentsRef = useRef<ImageAttachment[]>([]);
  const filesRef = useRef(new Map<string, File>());
  const abortsRef = useRef(new Map<string, AbortController>());
  const previewsRef = useRef(new Map<string, string>());
  const mountedRef = useRef(true);

  attachmentsRef.current = attachments;

  // An object URL outlives the element that rendered it, so it has to be handed
  // back explicitly. Closing a conversation with images attached would otherwise
  // pin their bytes in memory for the life of the window, and an upload nobody
  // is waiting for would keep running.
  useEffect(() => {
    mountedRef.current = true;
    const aborts = abortsRef.current;
    const previews = previewsRef.current;
    const files = filesRef.current;
    return () => {
      mountedRef.current = false;
      for (const controller of aborts.values()) controller.abort();
      for (const url of previews.values()) URL.revokeObjectURL(url);
      aborts.clear();
      previews.clear();
      files.clear();
    };
  }, [chatId]);

  function update(
    change: (current: readonly ImageAttachment[]) => ImageAttachment[],
  ) {
    if (!mountedRef.current) return;
    setAttachments((current) => change(current));
  }

  function forget(id: string) {
    abortsRef.current.get(id)?.abort();
    abortsRef.current.delete(id);
    const preview = previewsRef.current.get(id);
    if (preview) URL.revokeObjectURL(preview);
    previewsRef.current.delete(id);
    filesRef.current.delete(id);
  }

  async function upload(id: string) {
    const file = filesRef.current.get(id);
    if (!file) return;
    const controller = new AbortController();
    abortsRef.current.set(id, controller);
    update((current) => withUploadStarted(current, id));
    try {
      const published = await uploadImageAttachment(client, chatId, file, {
        onProgress: (uploadedBytes) =>
          update((current) => withUploadProgress(current, id, uploadedBytes)),
        signal: controller.signal,
      });
      update((current) => withUploadPublished(current, id, published));
    } catch (err) {
      // A cancelled upload belongs to an attachment the reader already removed,
      // so there is no chip left to carry the message.
      if (err instanceof DOMException && err.name === "AbortError") return;
      update((current) => withUploadFailed(current, id, failureText(err)));
    } finally {
      abortsRef.current.delete(id);
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
    const now = new Date();
    const queued = files.map((file) => {
      const id = crypto.randomUUID();
      filesRef.current.set(id, file);
      const previewUrl = URL.createObjectURL(file);
      previewsRef.current.set(id, previewUrl);
      return queuedImageAttachment(id, {
        name: imageAttachmentName(file, now),
        byteLen: file.size,
        previewUrl,
      });
    });
    update((current) => [...current, ...queued]);
    for (const attachment of queued) void upload(attachment.id);
  }

  async function openPicker() {
    if (attachingFromPicker) return;
    if (attachmentsRef.current.length >= MAX_IMAGE_ATTACHMENTS) {
      setError(`A message can carry at most ${MAX_IMAGE_ATTACHMENTS} images.`);
      return;
    }
    if (!useNativePickerLatch.getState().claim(PICKER_HOLDERS.attachImage)) {
      setError(PICKER_BUSY_MESSAGE);
      return;
    }
    setAttachingFromPicker(true);
    setError(null);
    try {
      const published = await attachChatImage(chatId);
      // Dismissing the picker is a normal outcome, not a failure.
      if (!published) return;
      update((current) => [
        ...current,
        readyImageAttachment(crypto.randomUUID(), published),
      ]);
    } catch (err) {
      if (mountedRef.current) setError(failureText(err));
    } finally {
      useNativePickerLatch.getState().release(PICKER_HOLDERS.attachImage);
      if (mountedRef.current) setAttachingFromPicker(false);
    }
  }

  return {
    attachments,
    attachingFromPicker,
    error,
    attachFromPicker: () => void openPicker(),
    attachFiles,
    remove: (id) => {
      forget(id);
      update((current) => withoutAttachment(current, id));
    },
    retry: (id) => {
      update((current) => withRetryQueued(current, id));
      void upload(id);
    },
    clear: () => {
      for (const attachment of attachmentsRef.current) forget(attachment.id);
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
