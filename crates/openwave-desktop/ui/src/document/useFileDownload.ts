import { useEffect, useMemo, useRef, useState } from "react";

import type { ApiClient } from "@/api";

/**
 * A source document's original bytes, held for the life of the process.
 *
 * A document's content is immutable once imported — replacing a file produces a
 * new document with a new id — so a cache hit can never be stale, and reopening
 * a workbook costs nothing.
 */
const byteCache = new Map<string, Uint8Array>();

export type FileDownload = {
  /** A fresh copy each time the bytes change: the parsers take ownership. */
  data: ArrayBuffer | null;
  isLoading: boolean;
  error: Error | null;
};

export function useFileDownload(
  client: Pick<ApiClient, "getDocumentFileContent">,
  documentId: string,
): FileDownload {
  const [bytes, setBytes] = useState<Uint8Array | null>(
    () => byteCache.get(documentId) ?? null,
  );
  const [error, setError] = useState<Error | null>(null);
  const [isLoading, setIsLoading] = useState(!byteCache.has(documentId));
  const requestRef = useRef(0);

  useEffect(() => {
    const request = ++requestRef.current;
    const cached = byteCache.get(documentId);
    if (cached) {
      setBytes(cached);
      setError(null);
      setIsLoading(false);
      return;
    }

    const controller = new AbortController();
    setBytes(null);
    setError(null);
    setIsLoading(true);

    void (async () => {
      try {
        const downloaded = await client.getDocumentFileContent(
          documentId,
          controller.signal,
        );
        if (request !== requestRef.current) return;
        byteCache.set(documentId, downloaded);
        setBytes(downloaded);
      } catch (err) {
        if (controller.signal.aborted || request !== requestRef.current) return;
        setError(err instanceof Error ? err : new Error(String(err)));
      } finally {
        if (request === requestRef.current) setIsLoading(false);
      }
    })();

    return () => controller.abort();
  }, [client, documentId]);

  const data = useMemo(() => {
    if (!bytes) return null;
    return bytes.buffer.slice(
      bytes.byteOffset,
      bytes.byteOffset + bytes.byteLength,
    ) as ArrayBuffer;
  }, [bytes]);

  return { data, isLoading, error };
}
