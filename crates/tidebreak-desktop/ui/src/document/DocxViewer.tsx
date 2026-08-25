import type { HTMLAttributes } from "react";
import { useRef } from "react";

import { DocxViewerPreview } from "@/components/extend/docx-viewer";
import { FileDownloadProgressIndicator } from "@/components/document/FileDownloadProgress";
import { useSecureViewerLinks } from "@/document/extendViewerSurface";
import { useLocalDocumentUrl } from "@/document/useLocalDocumentUrl";
import type { FileBytesSource } from "@/document/useFileDownload";
import {
  DocumentViewerShell,
  DocumentViewerState,
} from "@/document/ViewerPrimitives";
import { useTheme } from "@/theme";

interface Props extends HTMLAttributes<HTMLDivElement> {
  source: FileBytesSource;
}

/**
 * Extend's WASM importer maps OOXML into React nodes instead of inserting
 * document-authored HTML or styles. Keep the surface in explicit read-only
 * mode and route every materialized hyperlink through Tidebreak's host gate.
 */
export default function DocxViewer({ source, className, ...restProps }: Props) {
  const { resolved: resolvedTheme } = useTheme();
  const containerRef = useRef<HTMLDivElement>(null);
  const file = useLocalDocumentUrl(source);
  useSecureViewerLinks(containerRef);

  return (
    <DocumentViewerShell
      ref={containerRef}
      className={className}
      {...restProps}
    >
      {file.error ? (
        <DocumentViewerState variant="error">
          This document could not be loaded.
        </DocumentViewerState>
      ) : !file.objectUrl ? (
        file.progress ? (
          <FileDownloadProgressIndicator progress={file.progress} />
        ) : (
          <DocumentViewerState variant="loading">
            Loading document…
          </DocumentViewerState>
        )
      ) : (
        <div className="min-h-0 grow overflow-hidden rounded-md border bg-background shadow-xs">
          <DocxViewerPreview
            className="h-full min-h-0"
            defaultZoom={50}
            fileName="document.docx"
            isDark={resolvedTheme === "dark"}
            onIsDarkChange={() => undefined}
            showDownload={false}
            showToolbar
            showUpload={false}
            src={file.objectUrl}
          />
        </div>
      )}
    </DocumentViewerShell>
  );
}
