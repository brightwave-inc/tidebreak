import { invoke } from "@tauri-apps/api/core";

import type { ApiClient } from "./api";
import { parseLibraryImportBatch, type LibraryImportBatch } from "./documents";
import {
  isSupportedImageType,
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

/**
 * What one browser selection produced.
 *
 * The images come back as the files themselves rather than as published
 * identities: they take the composer's own upload path, which shows byte
 * progress and can retry, while the native picker publishes in the host and
 * hands back finished attachments.
 */
export type HeldFiles = {
  images: File[];
  documents: LibraryImportBatch | null;
};

/**
 * Ask for files through the browser's own picker.
 *
 * Resolves with an empty list when the reader dismisses it, so nothing on the
 * way in has to tell a dismissal from a failure.
 */
export function pickHeldFiles(): Promise<File[]> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.multiple = true;
    // Off screen rather than hidden: a `display: none` input is not clickable
    // in every engine, and the picker has to open from the reader's gesture.
    input.style.position = "fixed";
    input.style.left = "-9999px";
    document.body.append(input);
    function settle(files: File[]) {
      input.remove();
      resolve(files);
    }
    input.addEventListener("change", () =>
      settle(Array.from(input.files ?? [])),
    );
    input.addEventListener("cancel", () => settle([]));
    input.click();
  });
}

/**
 * Route files the renderer already holds to the machine this window works on.
 *
 * This is the split the host performs on picked paths, performed here because
 * these bytes never reach the host: pixels for the model on one side, sources
 * to parse and cite on the other. One file that cannot be imported is reported
 * beside the ones that were, so a bad file does not cost the reader the rest
 * of the selection.
 */
export async function attachHeldChatFiles(
  client: ApiClient,
  chatId: string,
  files: readonly File[],
): Promise<HeldFiles> {
  const images = files.filter((file) => isSupportedImageType(file.type));
  const sources = files.filter((file) => !isSupportedImageType(file.type));
  if (sources.length === 0) return { images, documents: null };
  const results = await Promise.all(
    sources.map(async (file) => {
      try {
        const { document_id } = await client.ingestChatDocument(chatId, file);
        return {
          status: "imported" as const,
          document: {
            documentId: document_id,
            displayName: file.name,
            mediaType: file.type || "application/octet-stream",
            byteLen: file.size,
          },
        };
      } catch (error) {
        return {
          status: "failed" as const,
          displayName: file.name,
          message: importFailureText(error),
        };
      }
    }),
  );
  return { images, documents: { results } };
}

function importFailureText(error: unknown): string {
  const message = String(error)
    .replace(/^Error:\s*/, "")
    .trim();
  return message && message.length <= 240
    ? message
    : "That file could not be attached.";
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
