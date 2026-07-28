import { Loader2Icon } from "lucide-react";
import type { HTMLAttributes } from "react";
import { useEffect, useState } from "react";

import { cn } from "@/lib/utils";
import { FileDownloadProgressIndicator } from "./FileDownloadProgress";
import { useFileDownload } from "./useFileDownload";

interface Props extends HTMLAttributes<HTMLDivElement> {
  chatId: string;
  documentID: string;
}

export function ImageViewer({ chatId, documentID, className, ...restProps }: Props) {
  const [imageError, setImageError] = useState(false);
  const [imageUrl, setImageUrl] = useState<string | null>(null);
  const fileDownload = useFileDownload(chatId, documentID, { parseAs: "blob" });

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
  }, [documentID]);

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

  if (fileDownload.isError || imageError) {
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
