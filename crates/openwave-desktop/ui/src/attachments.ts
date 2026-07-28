import { invoke } from "@tauri-apps/api/core";

import { parseLibraryImportBatch, type LibraryImportBatch } from "./documents";
import { parseAttachedImage, type PickedImage } from "./ImageAttachments";

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
export async function attachChatFiles(chatId: string): Promise<AttachedFiles | null> {
  return parseAttachedFiles(await invoke("attach_chat_files", { request: { chatId } }));
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
