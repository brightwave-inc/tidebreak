import { useEffect, useMemo, useRef, useState } from "react";

import type { ApiClient, FileDownloadProgress } from "@/api";
import { readDeliverableFile } from "@/deliverables";

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
 * Fetch one file's bytes for a viewer. Progress is optional — Tauri-backed
 * sources load in one shot and have nothing to report.
 */
export type FileBytesFetcher = (
  signal: AbortSignal,
  onProgress?: (progress: FileDownloadProgress) => void,
) => Promise<{ bytes: Uint8Array; contentType: string | null }>;

/**
 * Where a viewer gets its bytes, and how they are addressed in the cache.
 *
 * Sources and outputs both draw with the same engines; they differ only in
 * how the bytes are fetched. `id` is the stable identity UI state (page,
 * remount keys) keys off; `cacheKey` must change whenever the bytes would.
 */
export type FileBytesSource = {
  id: string;
  cacheKey: string;
  fetch: FileBytesFetcher;
};

/**
 * How many bytes of viewed files may be held at once.
 *
 * The budget is on total bytes rather than on entries because the cost being
 * bounded is memory: a cap of "ten documents" treats a 12 MB workbook and a
 * 4 KB note as the same thing. A source or binary output cannot exceed 16 MB
 * — the import / acceptance limit both the server and the renderer enforce —
 * so 64 MB holds four of the largest files a reader can open, or dozens of
 * ordinary ones. That is generous for an access pattern of "the file I am
 * reading, and the one before it", while still being small next to what a
 * desktop renderer already occupies, and it no longer grows with the length
 * of the session.
 */
const MAX_CACHED_BYTES = 64 * 1024 * 1024;

/**
 * Viewed files' original bytes, held across viewer mounts.
 *
 * A source document's content is immutable once imported — replacing a file
 * produces a new document with a new id — so a cache hit can never be stale.
 * An output revision is likewise write-once. The cache earns its place
 * because the panel unmounts the viewer whenever the reader switches between
 * views: without it, every flip back re-downloads the whole file and redraws
 * from a spinner.
 *
 * Eviction is least-recently-*read*, not least-recently-inserted: the file a
 * reader keeps flipping back to is the one worth keeping, however long ago it
 * was downloaded.
 */
export type ByteCache = {
  get(key: string): StoredFile | undefined;
  set(key: string, file: StoredFile): void;
  clear(): void;
};

/**
 * A byte cache bounded to `budgetBytes`, evicting least-recently-read first.
 *
 * Exported so the eviction policy can be exercised against a budget small
 * enough to test; the hook uses the one below.
 */
export function createByteCache(budgetBytes: number): ByteCache {
  // A Map iterates in insertion order, so re-inserting on every read leaves the
  // first key as the least recently read one.
  const entries = new Map<string, StoredFile>();
  let heldBytes = 0;

  return {
    get(key) {
      const file = entries.get(key);
      if (!file) return undefined;
      entries.delete(key);
      entries.set(key, file);
      return file;
    },

    set(key, file) {
      const previous = entries.get(key);
      if (previous) {
        entries.delete(key);
        heldBytes -= previous.bytes.byteLength;
      }
      // A file larger than the whole budget is not admitted. Taking it would
      // mean evicting every other entry and still not fitting, so the reader
      // would lose a working set they are using to make room for something the
      // cache cannot keep anyway. Such a source re-downloads on each flip.
      if (file.bytes.byteLength > budgetBytes) return;

      entries.set(key, file);
      heldBytes += file.bytes.byteLength;
      // The new entry fits on its own, so this always stops before reaching it.
      for (const [oldest, evicted] of entries) {
        if (heldBytes <= budgetBytes) break;
        entries.delete(oldest);
        heldBytes -= evicted.bytes.byteLength;
      }
    },

    clear() {
      entries.clear();
      heldBytes = 0;
    },
  };
}

const byteCache = createByteCache(MAX_CACHED_BYTES);

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
   * small file, a response that never declared its length, a cache hit, or a
   * one-shot IPC load with no streaming progress.
   */
  progress: FileDownloadProgress | null;
};

/** Bytes of one imported source document, addressed by document id. */
export function documentFileSource(
  client: Pick<ApiClient, "getChatDocumentFile">,
  chatId: string,
  documentId: string,
): FileBytesSource {
  return {
    id: documentId,
    cacheKey: `document/${chatId}/${documentId}`,
    fetch: (signal, onProgress) =>
      client.getChatDocumentFile(chatId, documentId, signal, onProgress),
  };
}

/**
 * Bytes of one output revision in private scratch.
 *
 * `revisionId` is required so the cache key names immutable content: omitting
 * it and keying on "current" would serve a stale revision after an update.
 */
export function outputFileSource(
  chatId: string,
  outputId: string,
  revisionId: string,
): FileBytesSource {
  return {
    id: `${outputId}/${revisionId}`,
    cacheKey: `output/${chatId}/${outputId}/${revisionId}`,
    fetch: async (signal) => {
      const file = await readDeliverableFile(chatId, outputId, revisionId);
      if (signal.aborted) {
        throw new DOMException("The operation was aborted.", "AbortError");
      }
      return { bytes: file.bytes, contentType: file.mediaType };
    },
  };
}

/**
 * Download one file's original bytes, for every viewer that draws them.
 *
 * The source is passed in rather than pulled from context so the hook can be
 * driven without a provider, and so a viewer's dependencies are visible in
 * its props. Sources and outputs share the cache and the decode path.
 */
export function useFileDownload<F extends FileDownloadFormat>(
  source: FileBytesSource,
  options: { parseAs: F },
): FileDownload<F> {
  const { parseAs } = options;
  const { cacheKey, fetch } = source;
  const [file, setFile] = useState<StoredFile | null>(
    () => byteCache.get(cacheKey) ?? null,
  );
  const [error, setError] = useState<Error | null>(null);
  const [isLoading, setIsLoading] = useState(file === null);
  const [progress, setProgress] = useState<FileDownloadProgress | null>(null);
  const requestRef = useRef(0);
  // Keep the latest fetch without re-running the effect when the caller
  // rebuilds an otherwise-identical source object each render.
  const fetchRef = useRef(fetch);
  fetchRef.current = fetch;

  useEffect(() => {
    const request = ++requestRef.current;
    const cached = byteCache.get(cacheKey);
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
        const downloaded = await fetchRef.current(
          controller.signal,
          (next) => {
            if (!controller.signal.aborted && request === requestRef.current) {
              setProgress(next);
            }
          },
        );
        if (request !== requestRef.current) return;
        byteCache.set(cacheKey, downloaded);
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
  }, [cacheKey]);

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
