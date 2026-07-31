/**
 * `document/` is the heavy side of viewing a source: the media-type dispatcher
 * below, the viewers built on a document engine of their own (pdf.js, Univer)
 * with the parsing and control surfaces those need, and the download every
 * viewer shares (`useFileDownload`).
 *
 * `components/document/` next to it is the reading side: the extracted text,
 * the panel's view switcher, and the viewers that need nothing beyond the
 * file's characters — image, text/markdown, JSON, XML. A new format goes
 * wherever its viewer's weight puts it; both sides download the same way.
 */
import { lazy, Suspense } from "react";
import { Loader2Icon } from "lucide-react";

import type { SheetHighlightRange } from "@/document/UniverSpreadsheetViewer";
import type { FileBytesSource } from "@/document/useFileDownload";

// pdf.js is a large dependency and most sessions never open a PDF, so it is
// fetched from the app bundle on first use rather than at startup.
const PdfViewer = lazy(() =>
  import("@/document/PdfViewer").then((m) => ({ default: m.PdfViewer })),
);
// The spreadsheet and word-document engines are larger still, and split apart
// so that opening a workbook does not also fetch the document renderer.
const UniverSpreadsheetViewer = lazy(
  () => import("@/document/UniverSpreadsheetViewer"),
);
const UniverDocumentViewer = lazy(
  () => import("@/document/UniverDocumentViewer"),
);

const SPREADSHEET_MEDIA_TYPES = new Set([
  "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  "application/vnd.ms-excel",
]);

/**
 * Delimited text is a spreadsheet with one sheet and no styling — the same
 * viewer renders it, with the workbook style pass skipped.
 */
const DELIMITED_TEXT_MEDIA_TYPES = new Set([
  "text/csv",
  "text/tab-separated-values",
]);

const WORD_DOCUMENT_MEDIA_TYPE =
  "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

/**
 * Which viewer renders a source in its original form, chosen by media type.
 *
 * The dispatcher is the registration point for every format we learn to show:
 * a new viewer is one branch here plus one entry in {@link hasOriginalViewer}.
 * Formats with no viewer return null and the document panel falls back to the
 * extracted text, which is a better floor than refusing to open the source.
 */
export function hasOriginalViewer(mediaType: string): boolean {
  const type = normalizeMediaType(mediaType);
  return (
    type === "application/pdf" ||
    type === WORD_DOCUMENT_MEDIA_TYPE ||
    SPREADSHEET_MEDIA_TYPES.has(type) ||
    DELIMITED_TEXT_MEDIA_TYPES.has(type)
  );
}

/**
 * Whether this format's original view has pages, and so can be opened at the
 * one a citation was recorded on. The others draw the source as a single run,
 * where a page number means nothing.
 */
export function isPaginatedOriginalViewer(mediaType: string): boolean {
  return normalizeMediaType(mediaType) === "application/pdf";
}

/**
 * Whether this format's original view is a grid, and so can be opened at the
 * cell range a citation was recorded on.
 *
 * OpenDocument workbooks are indexed with cell ranges but have no viewer here,
 * so a citation into one opens the extracted text instead — where the rows it
 * quoted are what gets highlighted.
 */
export function isGridOriginalViewer(mediaType: string): boolean {
  const type = normalizeMediaType(mediaType);
  return SPREADSHEET_MEDIA_TYPES.has(type) || DELIMITED_TEXT_MEDIA_TYPES.has(type);
}

interface DocumentViewerProps {
  source: FileBytesSource;
  mediaType: string;
  /** Open on this page the first time it is requested for this document. */
  targetPage?: number;
  /**
   * Cells of a cited range to select and scroll to. Only a grid viewer has
   * anywhere to put them.
   */
  citationCellRange?: SheetHighlightRange;
  className?: string;
}

export function DocumentViewer({
  source,
  mediaType,
  targetPage,
  citationCellRange,
  className,
}: DocumentViewerProps) {
  const type = normalizeMediaType(mediaType);

  if (type === "application/pdf") {
    return (
      <ViewerBoundary>
        <PdfViewer
          source={source}
          targetPage={targetPage}
          className={className}
        />
      </ViewerBoundary>
    );
  }

  if (SPREADSHEET_MEDIA_TYPES.has(type) || DELIMITED_TEXT_MEDIA_TYPES.has(type)) {
    return (
      <ViewerBoundary>
        <UniverSpreadsheetViewer
          key={source.id}
          source={source}
          highlightRange={citationCellRange}
          isCsv={DELIMITED_TEXT_MEDIA_TYPES.has(type)}
          className={className}
        />
      </ViewerBoundary>
    );
  }

  if (type === WORD_DOCUMENT_MEDIA_TYPE) {
    return (
      <ViewerBoundary>
        <UniverDocumentViewer
          key={source.id}
          source={source}
          className={className}
        />
      </ViewerBoundary>
    );
  }

  return null;
}

function ViewerBoundary({ children }: { children: React.ReactNode }) {
  return (
    <Suspense
      fallback={
        <div className="flex grow items-center justify-center">
          <Loader2Icon className="size-6 animate-spin text-muted-foreground" />
        </div>
      }
    >
      {children}
    </Suspense>
  );
}

/** Media types arrive with parameters (`application/pdf; charset=binary`). */
function normalizeMediaType(mediaType: string): string {
  return mediaType.split(";", 1)[0]!.trim().toLowerCase();
}
