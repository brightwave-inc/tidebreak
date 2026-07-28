import { useEffect, useMemo, useRef, useState } from "react";

import type { ApiClient, FileDownloadProgress } from "@/api";

/** How a viewer wants the bytes handed to it. */
export type FileDownloadFormat = "arrayBuffer" | "text" | "blob";

type Decoded<F extends FileDownloadFormat> = F extends "text"
  ? string
  : F extends "blob"
    ? Blob
    : ArrayBuffer;

/** The bytes as they were stored, with the media type they were stored under. */
type StoredFile = { bytes: Uint8Array; contentType: string | null };

/**
 * Source documents' original bytes, held for the life of the process.
 *
 * A document's content is immutable once imported — replacing a file produces a
 * new document with a new id — so a cache hit can never be stale. It earns its
 * place because the panel unmounts the viewer whenever the reader switches
 * between the original and the extracted text: without the cache, every flip
 * back re-downloads the whole file and redraws from a spinner.
 *
 * Nothing is evicted, so a long session that opens many large sources holds
 * them all. That was already true of the workbook and PDF viewers, which cached
 * the biggest files of the seven.
 */
const byteCache = new Map<string, StoredFile>();

/** Drop the cache so one test's bytes cannot answer another's download. */
export function clearFileDownloadCache(): void {
  byteCache.clear();
}

export type FileDownload<F extends FileDownloadFormat> = {
  /**
   * The downloaded file in the requested shape, or null until it arrives. A
   * fresh copy each time the bytes change: the parsers take ownership.
   */
  data: Decoded<F> | null;
  isLoading: boolean;
  error: Error | null;
  /** The stored media type, once the response has arrived. */
  contentType: string | null;
  /**
   * How far the transfer has got, or null when there is nothing to report — a
   * small file, a response that never declared its length, or a cache hit,
   * which is instant and has no transfer to report on.
   */
  progress: FileDownloadProgress | null;
};

/**
 * Download one source's original bytes, for every viewer that draws them.
 *
 * The file is addressed by document id inside its conversation, never by a host
 * path, so a viewer can draw the file the reader imported without the renderer
 * learning where on disk it came from. The client is passed in rather than
 * pulled from context so the hook can be driven without a provider, and so a
 * viewer's dependencies are visible in its props.
 */
export function useFileDownload<F extends FileDownloadFormat>(
  client: Pick<ApiClient, "getChatDocumentFile">,
  chatId: string,
  documentId: string,
  options: { parseAs: F },
): FileDownload<F> {
  const { parseAs } = options;
  const key = `${chatId}/${documentId}`;
  const [file, setFile] = useState<StoredFile | null>(
    () => byteCache.get(key) ?? null,
  );
  const [error, setError] = useState<Error | null>(null);
  const [isLoading, setIsLoading] = useState(!byteCache.has(key));
  const [progress, setProgress] = useState<FileDownloadProgress | null>(null);
  const requestRef = useRef(0);

  useEffect(() => {
    const request = ++requestRef.current;
    const cached = byteCache.get(key);
    if (cached) {
      setFile(cached);
      setError(null);
      setIsLoading(false);
      setProgress(null);
      return;
    }

    const controller = new AbortController();
    setFile(null);
    setError(null);
    setIsLoading(true);
    setProgress(null);

    void (async () => {
      try {
        const downloaded = await client.getChatDocumentFile(
          chatId,
          documentId,
          controller.signal,
          (next) => {
            if (!controller.signal.aborted && request === requestRef.current) {
              setProgress(next);
            }
          },
        );
        if (request !== requestRef.current) return;
        byteCache.set(key, downloaded);
        setFile(downloaded);
      } catch (err) {
        if (controller.signal.aborted || request !== requestRef.current) return;
        setError(err instanceof Error ? err : new Error(String(err)));
      } finally {
        if (request === requestRef.current) {
          setIsLoading(false);
          setProgress(null);
        }
      }
    })();

    return () => controller.abort();
  }, [client, chatId, documentId, key]);

  const data = useMemo(() => {
    if (!file) return null;
    return decode(file, parseAs);
  }, [file, parseAs]);

  return { data, isLoading, error, contentType: file?.contentType ?? null, progress };
}

function decode<F extends FileDownloadFormat>(
  file: StoredFile,
  parseAs: F,
): Decoded<F> {
  const { bytes, contentType } = file;
  switch (parseAs) {
    case "text":
      return new TextDecoder().decode(bytes) as Decoded<F>;
    case "blob":
      return new Blob(
        [bytes],
        contentType ? { type: contentType } : undefined,
      ) as Decoded<F>;
    default:
      // A copy, so a parser that transfers the buffer cannot detach the cache.
      return bytes.buffer.slice(
        bytes.byteOffset,
        bytes.byteOffset + bytes.byteLength,
      ) as Decoded<F>;
  }
}
