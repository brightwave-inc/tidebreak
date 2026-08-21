import { Loader2Icon } from "lucide-react";
import type { HTMLAttributes } from "react";
import { useEffect, useState } from "react";

import {
  useFileDownload,
  type FileBytesSource,
} from "@/document/useFileDownload";
import { cn } from "@/lib/utils";
import { FileDownloadProgressIndicator } from "./FileDownloadProgress";

interface Props extends HTMLAttributes<HTMLDivElement> {
  source: FileBytesSource;
}

export function ImageViewer({ source, className, ...restProps }: Props) {
  const [imageError, setImageError] = useState(false);
  const [imageUrl, setImageUrl] = useState<string | null>(null);
  const fileId = source.id;
  const fileDownload = useFileDownload(source, {
    parseAs: "blob",
  });

  useEffect(() => {
    if (!fileDownload.data) return;
    const url = URL.createObjectURL(fileDownload.data);
    setImageUrl(url);
    // An object URL outlives its blob, so a panel left open through a session
    // of clicking around would otherwise leak every image it drew.
    return () => URL.revokeObjectURL(url);
  }, [fileDownload.data]);

  // Reset error state on document change
  useEffect(() => {
    setImageError(false);
  }, [fileId]);

  if (fileDownload.isLoading) {
    return (
      <div className={cn("relative overflow-auto", className)} {...restProps}>
        {fileDownload.progress ? (
          <FileDownloadProgressIndicator progress={fileDownload.progress} />
        ) : (
          <div className="flex h-64 items-center justify-center text-muted-foreground">
            <div className="flex flex-col items-center gap-2">
              <Loader2Icon className="size-6 animate-spin" />
              <p>Loading image…</p>
            </div>
          </div>
        )}
      </div>
    );
  }

  if (fileDownload.error || imageError) {
    return (
      <div className={cn("relative overflow-auto", className)} {...restProps}>
        <div className="flex h-64 items-center justify-center text-muted-foreground">
          <p>Failed to load image</p>
        </div>
      </div>
    );
  }

  return (
    <div className={cn("relative overflow-auto", className)} {...restProps}>
      <div className="flex justify-center p-4">
        {imageUrl && (
          <img
            src={imageUrl}
            alt="Document image"
            className="max-w-full shadow-lg"
            onError={() => setImageError(true)}
          />
        )}
      </div>
    </div>
  );
}
