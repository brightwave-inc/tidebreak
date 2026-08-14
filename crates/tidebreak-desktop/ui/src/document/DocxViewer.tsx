import type { HTMLAttributes, ReactNode } from "react";
import { useRef } from "react";

import { DocxViewerPreview } from "@/components/extend/docx-viewer";
import { FileDownloadProgressIndicator } from "@/components/document/FileDownloadProgress";
import {
  LIGHT_DOCUMENT_SURFACE,
  useSecureViewerLinks,
} from "@/document/extendViewerSurface";
import { useLocalDocumentUrl } from "@/document/useLocalDocumentUrl";
import type { FileBytesSource } from "@/document/useFileDownload";
import { cn } from "@/lib/utils";

interface Props extends HTMLAttributes<HTMLDivElement> {
  source: FileBytesSource;
}

/**
 * Extend's WASM importer maps OOXML into React nodes instead of inserting
 * document-authored HTML or styles. Keep the surface in explicit read-only
 * mode and route every materialized hyperlink through Tidebreak's host gate.
 */
export default function DocxViewer({
  source,
  className,
  ...restProps
}: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  const file = useLocalDocumentUrl(source);
  useSecureViewerLinks(containerRef);

  return (
    <div
      ref={containerRef}
      className={cn(
        "relative flex min-h-0 flex-col overflow-hidden",
        className,
        LIGHT_DOCUMENT_SURFACE,
      )}
      {...restProps}
    >
      {file.error ? (
        <ViewerMessage>This document could not be loaded.</ViewerMessage>
      ) : !file.objectUrl ? (
        file.progress ? (
          <FileDownloadProgressIndicator progress={file.progress} />
        ) : (
          <ViewerMessage>Loading document…</ViewerMessage>
        )
      ) : (
        <div className="min-h-0 grow overflow-hidden rounded-md border bg-white shadow-xs">
          <DocxViewerPreview
            className="h-full min-h-0"
            defaultZoom={50}
            fileName="document.docx"
            isDark={false}
            onIsDarkChange={() => undefined}
            showDownload={false}
            showToolbar
            showUpload={false}
            src={file.objectUrl}
          />
        </div>
      )}
    </div>
  );
}

function ViewerMessage({ children }: { children: ReactNode }) {
  return (
    <div className="flex min-h-64 grow items-center justify-center text-sm text-muted-foreground">
      {children}
    </div>
  );
}
