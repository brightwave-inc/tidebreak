import { useEffect, useState } from "react";

import { toast } from "sonner";

import { HttpError, type DocumentDetail } from "@/api";
import { useApp } from "@/AppContext";
import { useChatListStore } from "@/ChatListStore";
import { friendlyErrorMessage } from "@/lib/utils";
import {
  DocumentDetails,
  isDocumentRenderable,
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
import { useCitationPlacement } from "./citationPlacement";
import {
  DocumentDetailActions,
  DocumentDetailBreadcrumb,
} from "./DocumentDetailHeader";

type Props = {
  chatId: string;
  documentID: string;
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
 * One attachment, opened in a panel addressed as `document.{documentId}`, or
 * with a citation id when a citation led here.
 */
export function DocumentDetailRoot({
  chatId,
  documentID,
  citationId,
  download = exportLibraryDocument,
  canDownload = hasNativeHost(),
}: Props) {
  const { client } = useApp();
  const [info, setInfo] = useState<DocumentDetail | null>(null);
  const [loadError, setLoadError] = useState<LoadError | null>(null);
  const [reloads, setReloads] = useState(0);
  const [downloading, setDownloading] = useState(false);
  const [downloadError, setDownloadError] = useState<string | null>(null);
  const [sharing, setSharing] = useState(false);
  const [shared, setShared] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setInfo(null);
    setLoadError(null);
    setDownloadError(null);
    setShared(false);
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

  const hasOriginalDocument =
    info != null &&
    info.has_original_bytes &&
    isDocumentRenderable(info.media_type);

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
    hasOriginalDocument &&
    info != null &&
    isGridOriginalViewer(info.media_type)
      ? {
          sheetName: placement.sheet,
          sheetIndex: null,
          ...splitCells(placement.cells),
        }
      : undefined;

  const documentName = info ? documentTitle(info) : undefined;

  // Only offered where there is somewhere to promote to. A file is shared with
  // a project by a deliberate act here rather than by arriving in one of its
  // conversations, so a project holds what someone meant it to hold.
  const projectId =
    useChatListStore((state) => state.chats).find((chat) => chat.id === chatId)
      ?.project_id ?? null;

  async function onAddToProject() {
    if (!projectId || sharing) return;
    setSharing(true);
    try {
      await client.promoteDocumentToProject(projectId, chatId, documentID);
      setShared(true);
      toast.success("Added to the project.");
    } catch (caught) {
      toast.error(
        friendlyErrorMessage(caught, "Could not add this file to the project."),
      );
    } finally {
      setSharing(false);
    }
  }

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
      showBorder
      breadcrumb={
        <DocumentDetailBreadcrumb
          documentName={documentName}
        />
      }
      headerRightSlot={
        <DocumentDetailActions
          canDownload={canDownload && info != null}
          downloading={downloading}
          onDownload={() => void onDownload()}
          canAddToProject={projectId != null && info != null}
          sharing={sharing}
          shared={shared}
          onAddToProject={() => void onAddToProject()}
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
          hasOriginalDocument={hasOriginalDocument}
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
