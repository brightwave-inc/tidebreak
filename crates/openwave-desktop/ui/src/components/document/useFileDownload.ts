import { useEffect, useState } from "react";

import { useApp } from "@/AppContext";

export type ParseAsType = "text" | "blob";

type ParsedData<T extends ParseAsType> = T extends "text" ? string : Blob;

interface UseFileDownloadOptions<T extends ParseAsType> {
  /**
   * How to parse the downloaded bytes.
   * - "blob": the bytes as they were stored (default)
   * - "text": decoded as UTF-8
   */
  parseAs?: T;
  /** Whether to download at all. Default: true. */
  enabled?: boolean;
}

export interface FileDownloadResult<T extends ParseAsType> {
  data: ParsedData<T> | null;
  isLoading: boolean;
  isError: boolean;
  /** The stored media type, once the response has arrived. */
  contentType: string | null;
}

/**
 * Download one source's original bytes.
 *
 * The file is addressed by document id inside its conversation, never by a
 * host path, so a viewer can draw the file the reader imported without the
 * renderer learning where on disk it came from. The download is deferred
 * behind `enabled` — opening a large source to read its extracted text should
 * not pull the whole file across first.
 */
export function useFileDownload<T extends ParseAsType = "blob">(
  chatId: string,
  documentID: string,
  options: UseFileDownloadOptions<T> = {},
): FileDownloadResult<T> {
  const { parseAs = "blob" as T, enabled = true } = options;
  const { client } = useApp();
  const [result, setResult] = useState<FileDownloadResult<T>>(idle);

  useEffect(() => {
    if (!enabled) {
      setResult(idle);
      return;
    }
    const controller = new AbortController();
    setResult({ data: null, isLoading: true, isError: false, contentType: null });
    void client
      .getChatDocumentFile(chatId, documentID, controller.signal)
      .then(async (blob) => {
        const data = (parseAs === "text" ? await blob.text() : blob) as ParsedData<T>;
        if (controller.signal.aborted) return;
        setResult({
          data,
          isLoading: false,
          isError: false,
          contentType: blob.type || null,
        });
      })
      .catch(() => {
        if (controller.signal.aborted) return;
        setResult({ data: null, isLoading: false, isError: true, contentType: null });
      });
    return () => controller.abort();
  }, [client, chatId, documentID, parseAs, enabled]);

  return result;
}

const idle = {
  data: null,
  isLoading: false,
  isError: false,
  contentType: null,
} as const;
