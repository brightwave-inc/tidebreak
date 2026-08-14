import type { HTMLAttributes, ReactNode } from "react";
import { forwardRef, useCallback, useEffect, useRef, useState } from "react";

import {
  PDFViewer as ExtendPdfViewer,
  type PDFViewerHandle,
} from "@/components/extend/pdf-viewer";
import { FileDownloadProgressIndicator } from "@/components/document/FileDownloadProgress";
import { useRegisterPdfControls } from "@/document/PdfControlsContext";
import {
  LIGHT_DOCUMENT_SURFACE,
  useSecureViewerLinks,
} from "@/document/extendViewerSurface";
import { useLocalDocumentUrl } from "@/document/useLocalDocumentUrl";
import { usePdfPageState } from "@/document/usePdfPageState";
import type { FileBytesSource } from "@/document/useFileDownload";
import { cn } from "@/lib/utils";

interface Props extends HTMLAttributes<HTMLDivElement> {
  source: FileBytesSource;
  /** Open on this page the first time it is requested for this document. */
  targetPage?: number;
}

/** A local, continuous, searchable PDF surface using Extend's viewer chrome. */
export function PdfViewer({
  source,
  targetPage,
  className,
  ...restProps
}: Props) {
  const viewerRef = useRef<PDFViewerHandle>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const [numPages, setNumPages] = useState(0);
  const file = useLocalDocumentUrl(source);
  const { currentPage, setCurrentPage } = usePdfPageState(source.id, {
    numPages,
    targetPage,
  });

  useSecureViewerLinks(containerRef);

  const goToPage = useCallback(
    (page: number) => {
      const nextPage = Math.min(Math.max(1, Math.round(page)), numPages || 1);
      setCurrentPage(nextPage);
      viewerRef.current?.scrollToPage(nextPage, {
        behavior: "smooth",
        block: "start",
      });
    },
    [numPages, setCurrentPage],
  );

  // Restore remembered position, or apply a citation, when the engine learns
  // the document's page count. Scroll callbacks thereafter update state without
  // snapping the continuous viewport back to the top of its active page.
  useEffect(() => {
    if (numPages <= 0) return;
    const requested = targetPage ?? currentPage;
    const page = Math.min(Math.max(1, Math.round(requested)), numPages);
    viewerRef.current?.scrollToPage(page, {
      behavior: "auto",
      block: "start",
    });
  }, [numPages, source.id, targetPage]);

  const registerPdfControls = useRegisterPdfControls();
  useEffect(() => {
    if (!registerPdfControls) return;
    registerPdfControls(
      numPages > 0 ? { currentPage, numPages, setPage: goToPage } : null,
    );
  }, [currentPage, goToPage, numPages, registerPdfControls]);
  useEffect(() => {
    if (!registerPdfControls) return;
    return () => registerPdfControls(null);
  }, [registerPdfControls]);

  if (file.error) {
    return (
      <ViewerShell className={className} {...restProps}>
        <ViewerMessage>This document could not be loaded.</ViewerMessage>
      </ViewerShell>
    );
  }

  if (!file.objectUrl) {
    return (
      <ViewerShell className={className} {...restProps}>
        {file.progress ? (
          <FileDownloadProgressIndicator progress={file.progress} />
        ) : (
          <ViewerMessage>Loading document…</ViewerMessage>
        )}
      </ViewerShell>
    );
  }

  return (
    <ViewerShell
      ref={containerRef}
      className={className}
      {...restProps}
    >
      <div className="min-h-0 grow overflow-hidden rounded-md border bg-white shadow-xs">
        <ExtendPdfViewer
          ref={viewerRef}
          className="h-full min-h-0"
          defaultZoom={1}
          fileName="document.pdf"
          onActivePageChange={setCurrentPage}
          onDocumentLoadSuccess={setNumPages}
          showDownload={false}
          showRotateControls
          showToolbar
          showUpload={false}
          src={file.objectUrl}
        />
      </div>
    </ViewerShell>
  );
}

const ViewerShell = forwardRef<HTMLDivElement, HTMLAttributes<HTMLDivElement>>(
  ({ className, ...props }, ref) => (
    <div
      ref={ref}
      className={cn(
        "relative flex min-h-0 flex-col overflow-hidden",
        className,
        LIGHT_DOCUMENT_SURFACE,
      )}
      {...props}
    />
  ),
);
ViewerShell.displayName = "PdfViewerShell";

function ViewerMessage({ children }: { children: ReactNode }) {
  return (
    <div className="flex min-h-64 grow items-center justify-center text-sm text-muted-foreground">
      {children}
    </div>
  );
}
