import { useEffect, useState } from "react";

import {
  useFileDownload,
  type FileBytesSource,
} from "@/document/useFileDownload";

/**
 * Adapt Tidebreak's immutable byte source to the URL contract used by Extend's
 * viewers. Every URL belongs to one mount and is revoked as soon as that source
 * is replaced or the viewer closes.
 */
export function useLocalDocumentUrl(source: FileBytesSource) {
  const download = useFileDownload(source, { parseAs: "blob" });
  const [objectUrl, setObjectUrl] = useState<string | null>(null);

  useEffect(() => {
    if (!download.data) {
      setObjectUrl(null);
      return;
    }

    const nextUrl = URL.createObjectURL(download.data);
    setObjectUrl(nextUrl);
    return () => URL.revokeObjectURL(nextUrl);
  }, [download.data]);

  return { ...download, objectUrl };
}
