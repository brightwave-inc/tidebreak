import type { HTMLAttributes } from "react";
import { useCallback, useEffect, useRef, useState } from "react";

import {
  PDFViewer as ExtendPdfViewer,
  type PDFViewerHandle,
} from "@/components/extend/pdf-viewer";
import { FileDownloadProgressIndicator } from "@/components/document/FileDownloadProgress";
import { useRegisterPdfControls } from "@/document/PdfControlsContext";
import { useSecureViewerLinks } from "@/document/extendViewerSurface";
import { useLocalDocumentUrl } from "@/document/useLocalDocumentUrl";
import { usePdfPageState } from "@/document/usePdfPageState";
import type { FileBytesSource } from "@/document/useFileDownload";
import {
  DocumentViewerShell,
  DocumentViewerState,
} from "@/document/ViewerPrimitives";

interface Props extends HTMLAttributes<HTMLDivElement> {
  source: FileBytesSource;
  /** Open on this page the first time it is requested for this document. */
  targetPage?: number;
}

/** A local, continuous, searchable PDF surface using Extend's viewer chrome. */
export function PdfViewer(props: Props) {
  return <PdfViewerSource key={props.source.cacheKey} {...props} />;
}

function PdfViewerSource({
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
      <DocumentViewerShell className={className} {...restProps}>
        <DocumentViewerState variant="error">
          This document could not be loaded.
        </DocumentViewerState>
      </DocumentViewerShell>
    );
  }

  if (!file.objectUrl) {
    return (
      <DocumentViewerShell className={className} {...restProps}>
        {file.progress ? (
          <FileDownloadProgressIndicator progress={file.progress} />
        ) : (
          <DocumentViewerState variant="loading">
            Loading document…
          </DocumentViewerState>
        )}
      </DocumentViewerShell>
    );
  }

  return (
    <DocumentViewerShell
      ref={containerRef}
      className={className}
      {...restProps}
    >
      <div className="min-h-0 grow overflow-hidden rounded-md border bg-background shadow-xs">
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
    </DocumentViewerShell>
  );
}
