import { useEffect, useState } from "react";

import { HttpError, type DocumentDetail } from "@/api";
import { useApp } from "@/AppContext";
import {
  DocumentDetails,
  isDocumentRenderable,
  type DocumentView,
} from "@/components/document/document-details";
import { DocumentError } from "@/components/document/error";
import { Button } from "@/components/ui/button";
import {
  isGridOriginalViewer,
  isPaginatedOriginalViewer,
} from "@/document/DocumentViewer";
import { exportLibraryDocument } from "@/documents";
import { hasNativeHost } from "@/host";
import { PanelFrame } from "@/panel/PanelFrame";
import type { PanelPosition } from "@/panel/panelTypes";
import { usePanelNav } from "@/panel/usePanelNav";
import { useCitationPlacement } from "./citationPlacement";
import {
  DocumentDetailActions,
  DocumentDetailBreadcrumb,
} from "./DocumentDetailHeader";

type Props = {
  chatId: string;
  documentID: string;
  position: PanelPosition;
  /**
   * The citation this panel was opened from, as the third part of its address.
   * It names a place inside the document: the passage to highlight, and the
   * page to open on where the source has pages.
   */
  citationId?: string;
  /** Resolves false when the reader dismissed the save dialog. */
  download?: (chatId: string, documentID: string) => Promise<boolean>;
  canDownload?: boolean;
};

/**
 * One source, opened in a panel addressed as `sources.{documentId}`, or as
 * `sources.{documentId}.{citationId}` when a citation led here.
 */
export function DocumentDetailRoot({
  chatId,
  documentID,
  position,
  citationId,
  download = exportLibraryDocument,
  canDownload = hasNativeHost(),
}: Props) {
  const { client } = useApp();
  const { openPanel } = usePanelNav();
  const [info, setInfo] = useState<DocumentDetail | null>(null);
  const [loadError, setLoadError] = useState<LoadError | null>(null);
  const [reloads, setReloads] = useState(0);
  const [downloading, setDownloading] = useState(false);
  const [downloadError, setDownloadError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setInfo(null);
    setLoadError(null);
    setDownloadError(null);
    void client
      .getChatDocument(chatId, documentID)
      .then((next) => {
        if (!cancelled) setInfo(next);
      })
      .catch((caught) => {
        if (!cancelled) setLoadError(describeLoadFailure(caught));
      });
    return () => {
      cancelled = true;
    };
  }, [client, chatId, documentID, reloads]);

  const hasOriginalDocumentTab =
    info != null &&
    info.has_original_bytes &&
    isDocumentRenderable(info.media_type);

  const [view, setView] = useState<DocumentView>("original_doc");

  const placement = useCitationPlacement(documentID, citationId);
  const paginated = info != null && isPaginatedOriginalViewer(info.media_type);
  const citationPage =
    paginated && placement?.kind === "page"
      ? placement.page
      : paginated && placement?.kind === "pages"
        ? placement.start
        : undefined;
  const citationLines =
    placement?.kind === "lines"
      ? { start: placement.start, end: placement.end }
      : undefined;

  // A cell range only opens a source the panel draws as a grid. A workbook
  // format with no viewer here — OpenDocument today — still carries the range on
  // its citation, and lands on the extracted text where the rows it quoted are
  // highlighted instead.
  const citationCellRange =
    placement?.kind === "sheet" &&
    hasOriginalDocumentTab &&
    info != null &&
    isGridOriginalViewer(info.media_type)
      ? {
          sheetName: placement.sheet,
          sheetIndex: null,
          ...splitCells(placement.cells),
        }
      : undefined;

  // Arriving from a citation, land on whichever view can show where it points:
  // the recorded page of a paginated original, the range of a grid one, or else
  // the extracted text, where the passage itself is highlighted. A citation the
  // transcript cannot resolve opens the document the same way the source list
  // does.
  //
  // The original view is only worth landing on where there is one.
  const citationView: DocumentView | null = !placement
    ? null
    : (citationPage != null || citationCellRange != null) &&
        hasOriginalDocumentTab
      ? "original_doc"
      : citationLines
        ? "extracted_text"
        : null;

  // Reset the view when the document changes. A format with no viewer has only
  // the extracted text to land on.
  //
  // Landing is per citation rather than per resolution. Two citations into one
  // source often want the same view, so the view alone cannot tell the second
  // click from the first: without the citation in the dependencies, a reader who
  // had since switched views stayed on the view they were on and the second
  // citation landed nowhere. The citation only changes when the reader clicks
  // one, so the transcript re-resolving the citation the panel is already open
  // at still leaves a deliberate view switch alone.
  useEffect(() => {
    setView(citationView ?? (hasOriginalDocumentTab ? "original_doc" : "extracted_text"));
  }, [documentID, hasOriginalDocumentTab, citationView, citationId]);

  const documentName = info ? documentTitle(info) : undefined;

  async function onDownload() {
    if (downloading) return;
    setDownloading(true);
    setDownloadError(null);
    try {
      await download(chatId, documentID);
    } catch (caught) {
      setDownloadError(friendlyDownloadError(caught));
    } finally {
      setDownloading(false);
    }
  }

  return (
    <PanelFrame
      position={position}
      showBorder
      breadcrumb={
        <DocumentDetailBreadcrumb
          documentName={documentName}
          onBackToSources={() => openPanel({ type: "sources" })}
        />
      }
      headerRightSlot={
        <DocumentDetailActions
          view={view}
          onViewChange={setView}
          showOriginalView={hasOriginalDocumentTab}
          canDownload={canDownload && info != null}
          downloading={downloading}
          onDownload={() => void onDownload()}
        />
      }
    >
      {loadError ? (
        <DocumentError>
          <div className="flex flex-col items-center gap-3">
            <span>{loadError.message}</span>
            {loadError.retriable && (
              <Button
                variant="outline"
                size="sm"
                className="font-normal"
                onClick={() => setReloads((count) => count + 1)}
              >
                Try again
              </Button>
            )}
          </div>
        </DocumentError>
      ) : info ? (
        <DocumentDetails
          chatId={chatId}
          info={info}
          view={view}
          hasOriginalDocumentTab={hasOriginalDocumentTab}
          targetLines={citationLines}
          targetPage={citationPage}
          citationCellRange={citationCellRange}
        />
      ) : (
        <p className="p-6 text-sm text-muted-foreground" role="status">
          Loading this source…
        </p>
      )}
      {downloadError && (
        <p className="shrink-0 px-6 pb-2 text-sm text-critical" role="alert">
          {downloadError}
        </p>
      )}
    </PanelFrame>
  );
}

function splitCells(cells: string | null) {
  const [startCell, endCell] = cells?.split(":", 2) ?? [null, null];
  return { startCell, endCell: endCell ?? startCell };
}

/** Why the source did not load, and whether asking again could help. */
type LoadError = { message: string; retriable: boolean };

/**
 * What to say when a source will not load.
 *
 * Only a 404 means the source is gone, and that is the only case allowed to say
 * so. Everything else — an expired token, a server that fell over, a connection
 * that dropped — is about reaching the source rather than the source itself, and
 * is worth asking again for. Collapsing the two told readers a file had been
 * deleted whenever the app could not reach its own server.
 */
function describeLoadFailure(error: unknown): LoadError {
  if (error instanceof HttpError && error.status === 404) {
    return { message: "The document is no longer available.", retriable: false };
  }
  if (error instanceof HttpError) {
    return {
      message: `The document could not be loaded (${error.status}).`,
      retriable: true,
    };
  }
  return { message: "The document could not be loaded.", retriable: true };
}

function documentTitle(info: DocumentDetail): string {
  return info.title?.trim() || `Source ${info.document_id.slice(0, 8)}`;
}

function friendlyDownloadError(error: unknown): string {
  const message = String(error).replace(/^Error:\s*/, "").trim();
  return message && message.length <= 240 ? message : "Could not save that source.";
}
