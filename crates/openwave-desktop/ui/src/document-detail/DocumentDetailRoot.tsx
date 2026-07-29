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
import { isPaginatedOriginalViewer } from "@/document/DocumentViewer";
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
    info != null && info.processing_status !== "failed" && isDocumentRenderable(info.media_type);

  const [view, setView] = useState<DocumentView>("original_doc");

  const placement = useCitationPlacement(documentID, citationId);
  const paginated = info != null && isPaginatedOriginalViewer(info.media_type);
  const citationPage =
    placement?.page != null && paginated ? placement.page : undefined;
  // Rectangles are only worth carrying where there is a page to draw them on;
  // a citation into an unpaginated source has none, and most citations carry
  // none at all.
  const citationBounds =
    citationPage != null && placement != null && placement.bounds.length > 0
      ? placement.bounds
      : undefined;

  // Arriving from a citation, land on whichever view can show where it points:
  // the recorded page of a paginated original, or else the extracted text,
  // where the passage itself is highlighted. A citation the transcript cannot
  // resolve opens the document the same way the source list does.
  const citationView: DocumentView | null = !placement
    ? null
    : citationPage != null
      ? "original_doc"
      : "extracted_text";

  // Reset the view when the document changes. A format with no viewer has only
  // the extracted text to land on.
  useEffect(() => {
    setView(citationView ?? (hasOriginalDocumentTab ? "original_doc" : "extracted_text"));
  }, [documentID, hasOriginalDocumentTab, citationView]);

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
          citationSpan={placement?.span}
          targetPage={citationPage}
          citationBounds={citationBounds}
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
