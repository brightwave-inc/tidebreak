import { invoke } from "@tauri-apps/api/core";

import { parseLibraryImportBatch, type LibraryImportBatch } from "./documents";
import {
  parseAttachedImage,
  parseHostPublishedImage,
  type PickedImage,
  type PublishedImage,
} from "./ImageAttachments";

/** One file the host could not attach as an image, named so it can be reported. */
export type FailedImage = {
  fileName: string;
  message: string;
};

/**
 * What one mixed selection produced.
 *
 * Images and documents are separated because the composer shows them
 * differently, and because a failure in one says nothing about the other. The
 * renderer does no routing of its own: picked paths never cross the boundary,
 * so the host has already decided what each file was.
 */
export type AttachedFiles = {
  images: PickedImage[];
  documents: LibraryImportBatch | null;
  failedImages: FailedImage[];
};

/** Attach any files through one native picker. `null` if the reader dismissed it. */
export async function attachChatFiles(
  chatId: string,
): Promise<AttachedFiles | null> {
  return parseAttachedFiles(
    await invoke("attach_chat_files", { request: { chatId } }),
  );
}

/** Claim one just-dropped native path set and attach it to the composer. */
export async function attachDroppedChatFiles(
  chatId: string,
): Promise<AttachedFiles | null> {
  return parseAttachedFiles(
    await invoke("attach_dropped_chat_files", { request: { chatId } }),
  );
}

/**
 * Publish an image the renderer is already holding, from the host.
 *
 * A pasted or dropped image has no path, so it cannot go through the picker
 * route — but it cannot go straight to the server either. Under a native host
 * the publish endpoint is mounted behind the client-executor token, which the
 * renderer does not have, and a bearer-authenticated POST from here comes back
 * `401` with an empty body. So the bytes take the same last mile as every other
 * attachment: the host posts them and hands back the identity it was given.
 */
export async function publishChatImage(
  chatId: string,
  file: Blob,
): Promise<PublishedImage> {
  const contentBase64 = encodeBase64(await file.arrayBuffer());
  return parseHostPublishedImage(
    await invoke("publish_chat_image", { request: { chatId, contentBase64 } }),
  );
}

export async function publishCodeImage(
  sessionId: string,
  file: Blob,
): Promise<PublishedImage> {
  const contentBase64 = encodeBase64(await file.arrayBuffer());
  return parseHostPublishedImage(
    await invoke("publish_code_image", {
      request: { sessionId, contentBase64 },
    }),
  );
}

/**
 * Base64 in chunks, because spreading 16 MB of bytes into one call to
 * `String.fromCharCode` overflows the argument stack.
 */
function encodeBase64(buffer: ArrayBuffer): string {
  const bytes = new Uint8Array(buffer);
  const chunkSize = 0x8000;
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += chunkSize) {
    binary += String.fromCharCode(
      ...bytes.subarray(offset, offset + chunkSize),
    );
  }
  return btoa(binary);
}

export function parseAttachedFiles(value: unknown): AttachedFiles | null {
  if (value === null) return null;
  if (!isRecord(value)) throw new Error("Invalid attachment response");
  const { images, documents, failedImages } = value;
  if (!Array.isArray(images) || !Array.isArray(failedImages)) {
    throw new Error("Invalid attachment response");
  }
  return {
    images: images.map((image) => {
      const parsed = parseAttachedImage(image);
      // Only the top-level result is nullable; a null inside the list would
      // mean the host emitted an entry for an image it did not attach.
      if (!parsed) throw new Error("Invalid attachment response");
      return parsed;
    }),
    documents: documents === null ? null : parseLibraryImportBatch(documents),
    failedImages: failedImages.map(parseFailedImage),
  };
}

function parseFailedImage(value: unknown): FailedImage {
  if (
    !isRecord(value) ||
    typeof value.fileName !== "string" ||
    typeof value.message !== "string"
  ) {
    throw new Error("Invalid attachment response");
  }
  return { fileName: value.fileName, message: value.message };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
