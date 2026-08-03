/**
 * Presentations preview as converted PDFs.
 *
 * No engine here draws slides natively, so a presentation is converted to PDF
 * by a LibreOffice the user already has installed and rendered with the PDF
 * viewer. The conversion command takes the original bytes and returns PDF
 * bytes, which keeps it transport-agnostic: HTTP-fetched source documents and
 * IPC-read output revisions wrap into the same converted source.
 *
 * A machine without LibreOffice is a designed-for state, not an error path:
 * the converter's absence surfaces as {@link ConverterMissingError} and the
 * viewer shows an install hint while the original file stays exportable.
 */
import { invoke } from "@tauri-apps/api/core";

import type { FileBytesSource } from "@/document/useFileDownload";

export const PRESENTATION_MEDIA_TYPES = new Set([
  "application/vnd.openxmlformats-officedocument.presentationml.presentation",
  "application/vnd.ms-powerpoint",
  "application/vnd.oasis.opendocument.presentation",
]);

/** LibreOffice is not installed (or its remains cannot start). */
export class ConverterMissingError extends Error {
  constructor() {
    super("No presentation converter is installed");
    this.name = "ConverterMissingError";
  }
}

type PresentationPdfResponse =
  | { status: "converted"; pdfBase64: string }
  | { status: "converterMissing" };

/**
 * Convert one presentation's bytes to PDF on the host.
 *
 * Throws {@link ConverterMissingError} when there is no LibreOffice to run;
 * every other failure propagates as an ordinary error with the host's message.
 */
export async function convertPresentationToPdf(
  bytes: Uint8Array,
  mediaType: string,
): Promise<Uint8Array> {
  const response = (await invoke("convert_presentation_to_pdf", {
    request: { contentBase64: encodeBase64(bytes), mediaType },
  })) as PresentationPdfResponse;
  if (response.status === "converterMissing") {
    throw new ConverterMissingError();
  }
  return decodeBase64(response.pdfBase64);
}

/**
 * The converted-PDF bytes of a presentation source.
 *
 * Wraps the original source rather than a new identity: the cache key extends
 * the original's (which already names immutable content), so the in-session
 * byte cache holds the converted PDF and a re-open never reconverts. The host
 * keeps its own on-disk cache keyed by content hash for cross-session hits.
 */
export function presentationPdfSource(
  original: FileBytesSource,
  mediaType: string,
): FileBytesSource {
  return {
    id: original.id,
    cacheKey: `${original.cacheKey}/converted-pdf`,
    fetch: async (signal, onProgress) => {
      const source = await original.fetch(signal, onProgress);
      const pdf = await convertPresentationToPdf(source.bytes, mediaType);
      if (signal.aborted) {
        throw new DOMException("The operation was aborted.", "AbortError");
      }
      return { bytes: pdf, contentType: "application/pdf" };
    },
  };
}

function encodeBase64(bytes: Uint8Array): string {
  let binary = "";
  const CHUNK = 0x8000;
  for (let offset = 0; offset < bytes.length; offset += CHUNK) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + CHUNK));
  }
  return btoa(binary);
}

function decodeBase64(value: string): Uint8Array {
  const binary = atob(value);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
}
