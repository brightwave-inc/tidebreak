import { useEffect, useState } from "react";

import type { DocumentDetail } from "@/api";
import { useApp } from "@/AppContext";
import {
  DocumentDetails,
  isDocumentRenderable,
  type DocumentView,
} from "@/components/document/document-details";
import { DocumentError } from "@/components/document/error";
import { exportLibraryDocument } from "@/documents";
import { hasNativeHost } from "@/host";
import { PanelFrame } from "@/panel/PanelFrame";
import type { PanelPosition } from "@/panel/panelTypes";
import { usePanelNav } from "@/panel/usePanelNav";
import {
  DocumentDetailActions,
  DocumentDetailBreadcrumb,
} from "./DocumentDetailHeader";

type Props = {
  chatId: string;
  documentID: string;
  position: PanelPosition;
  /** Resolves false when the reader dismissed the save dialog. */
  download?: (chatId: string, documentID: string) => Promise<boolean>;
  canDownload?: boolean;
};

/** One source, opened in a panel addressed as `sources.{documentId}`. */
export function DocumentDetailRoot({
  chatId,
  documentID,
  position,
  download = exportLibraryDocument,
  canDownload = hasNativeHost(),
}: Props) {
  const { client } = useApp();
  const { openPanel } = usePanelNav();
  const [info, setInfo] = useState<DocumentDetail | null>(null);
  const [loadError, setLoadError] = useState(false);
  const [downloading, setDownloading] = useState(false);
  const [downloadError, setDownloadError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setInfo(null);
    setLoadError(false);
    setDownloadError(null);
    void client
      .getChatDocument(chatId, documentID)
      .then((next) => {
        if (!cancelled) setInfo(next);
      })
      .catch(() => {
        if (!cancelled) setLoadError(true);
      });
    return () => {
      cancelled = true;
    };
  }, [client, chatId, documentID]);

  const hasOriginalDocumentTab =
    info != null && info.processing_status !== "failed" && isDocumentRenderable(info.media_type);

  const [view, setView] = useState<DocumentView>("original_doc");

  // Reset the view when the document changes. A format with no viewer has only
  // the extracted text to land on.
  useEffect(() => {
    setView(hasOriginalDocumentTab ? "original_doc" : "extracted_text");
  }, [documentID, hasOriginalDocumentTab]);

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
        <DocumentError>The document is no longer available.</DocumentError>
      ) : info ? (
        <DocumentDetails
          chatId={chatId}
          info={info}
          view={view}
          hasOriginalDocumentTab={hasOriginalDocumentTab}
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

function documentTitle(info: DocumentDetail): string {
  return info.title?.trim() || `Source ${info.document_id.slice(0, 8)}`;
}

function friendlyDownloadError(error: unknown): string {
  const message = String(error).replace(/^Error:\s*/, "").trim();
  return message && message.length <= 240 ? message : "Could not save that source.";
}
