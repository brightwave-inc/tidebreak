import type { HTMLAttributes } from "react";
import { useState } from "react";

import { useLocalDocumentUrl } from "@/document/useLocalDocumentUrl";
import type { FileBytesSource } from "@/document/useFileDownload";
import { DocumentViewerState } from "@/document/ViewerPrimitives";
import { cn } from "@/lib/utils";
import { FileDownloadProgressIndicator } from "./FileDownloadProgress";

interface Props extends HTMLAttributes<HTMLDivElement> {
  source: FileBytesSource;
}

export function ImageViewer({ source, className, ...restProps }: Props) {
  return (
    <ImageViewerSource
      key={source.cacheKey}
      source={source}
      className={className}
      {...restProps}
    />
  );
}

function ImageViewerSource({ source, className, ...restProps }: Props) {
  const [imageError, setImageError] = useState(false);
  const file = useLocalDocumentUrl(source);

  if (!file.objectUrl) {
    return (
      <div className={cn("relative overflow-auto", className)} {...restProps}>
        {file.error ? (
          <DocumentViewerState variant="error">
            This image could not be loaded.
          </DocumentViewerState>
        ) : file.progress ? (
          <FileDownloadProgressIndicator progress={file.progress} />
        ) : (
          <DocumentViewerState variant="loading">
            Loading image…
          </DocumentViewerState>
        )}
      </div>
    );
  }

  if (imageError) {
    return (
      <div className={cn("relative overflow-auto", className)} {...restProps}>
        <DocumentViewerState variant="error">
          This image could not be loaded.
        </DocumentViewerState>
      </div>
    );
  }

  return (
    <div className={cn("relative overflow-auto", className)} {...restProps}>
      <div className="flex justify-center p-4">
        <img
          src={file.objectUrl}
          alt="Document image"
          className="max-w-full shadow-lg"
          onError={() => setImageError(true)}
        />
      </div>
    </div>
  );
}
