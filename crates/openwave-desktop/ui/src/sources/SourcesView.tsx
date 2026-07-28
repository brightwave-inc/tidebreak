import { FileIcon, PlusIcon, RotateCwIcon } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";

import { PanelSecondaryHeader } from "@/components/PanelHeader";
import { useConfirm } from "@/components/ConfirmDialog";
import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { WithTooltip } from "@/components/ui/tooltip";
import {
  deleteLibraryDocument,
  exportLibraryDocument,
  importLibraryDocuments,
  listLibraryDocuments,
  retryLibraryDocument,
  type ImportedDocument,
  type LibraryCatalog,
  type LibraryDocument,
  type LibraryImportBatch,
  type LibraryImportSuccess,
} from "@/documents";
import { hasNativeHost } from "@/host";
import {
  PICKER_BUSY_MESSAGE,
  PICKER_HOLDERS,
  useNativePickerLatch,
} from "@/NativePickerLatch";
import { documentTitle } from "./sourceFormat";
import { SourceTable } from "./SourceTable";

export type SourcesApis = {
  list: (chatId: string) => Promise<LibraryCatalog>;
  import: (chatId: string) => Promise<LibraryImportBatch | null>;
  delete: (chatId: string, documentId: string) => Promise<void>;
  retry: (chatId: string, documentId: string) => Promise<void>;
  export: (chatId: string, documentId: string) => Promise<boolean>;
};

const defaultApis: SourcesApis = {
  list: listLibraryDocuments,
  import: importLibraryDocuments,
  delete: deleteLibraryDocument,
  retry: retryLibraryDocument,
  export: exportLibraryDocument,
};

/**
 * A conversation's sources, as the panel addressed `sources`.
 *
 * The catalog is polled while anything in it is still being prepared, which is
 * the only reason this component holds the documents rather than the grid: a
 * refresh has to survive the reader sorting, filtering and selecting rows.
 */
export function SourcesView({
  chatId,
  onOpen = () => {},
  apis = defaultApis,
  canDownload = hasNativeHost(),
}: {
  chatId: string;
  /** Navigate to the existing `sources.{documentId}` panel contract. */
  onOpen?: (documentId: string) => void;
  apis?: SourcesApis;
  canDownload?: boolean;
}) {
  const [documents, setDocuments] = useState<LibraryDocument[]>([]);
  const [catalogTruncated, setCatalogTruncated] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [importing, setImporting] = useState(false);
  const [imported, setImported] = useState<ImportedDocument | null>(null);
  const [busyDocumentId, setBusyDocumentId] = useState<string | null>(null);
  const [countSuffix, setCountSuffix] = useState("");
  const mountedRef = useRef(true);
  const scopeRef = useRef(0);
  const catalogRequestRef = useRef(0);
  const { confirm, dialog: confirmDialog } = useConfirm();

  function isCurrentScope(scope: number) {
    return mountedRef.current && scope === scopeRef.current;
  }

  async function refreshCatalog(showLoading = false) {
    const request = ++catalogRequestRef.current;
    const scope = scopeRef.current;
    if (showLoading) setLoading(true);
    try {
      const next = await apis.list(chatId);
      if (!isCurrentScope(scope) || request !== catalogRequestRef.current) return;
      setDocuments(next.documents);
      setCatalogTruncated(next.truncated);
      setError(null);
    } catch (err) {
      if (!isCurrentScope(scope) || request !== catalogRequestRef.current) return;
      setError(friendlyError(err, "Could not load this conversation's sources."));
    } finally {
      if (isCurrentScope(scope) && request === catalogRequestRef.current) {
        setLoading(false);
      }
    }
  }

  useEffect(() => {
    mountedRef.current = true;
    scopeRef.current += 1;
    setDocuments([]);
    setCatalogTruncated(false);
    setError(null);
    setImported(null);
    setBusyDocumentId(null);
    void refreshCatalog(true);
    return () => {
      mountedRef.current = false;
      scopeRef.current += 1;
      catalogRequestRef.current += 1;
    };
  }, [chatId, apis]);

  const processing = documents.some((document) =>
    ["queued", "processing"].includes(document.processingStatus),
  );

  useEffect(() => {
    if (!processing) return;
    const interval = window.setInterval(() => void refreshCatalog(), 2_000);
    return () => window.clearInterval(interval);
  }, [processing, chatId, apis]);

  async function onImport() {
    if (importing) return;
    if (!useNativePickerLatch.getState().claim(PICKER_HOLDERS.importSource)) {
      setError(PICKER_BUSY_MESSAGE);
      return;
    }
    const scope = scopeRef.current;
    setImporting(true);
    setError(null);
    setImported(null);
    try {
      const batch = await apis.import(chatId);
      const accepted = batch?.results.find(isImportedDocument);
      if (!isCurrentScope(scope) || !accepted) return;
      setImported(accepted.document);
      await refreshCatalog();
    } catch (err) {
      if (isCurrentScope(scope)) {
        setError(friendlyError(err, "Could not add that source."));
      }
    } finally {
      useNativePickerLatch.getState().release(PICKER_HOLDERS.importSource);
      if (isCurrentScope(scope)) setImporting(false);
    }
  }

  const onDelete = useCallback(
    async (document: LibraryDocument) => {
      const scope = scopeRef.current;
      const accepted = await confirm({
        title: `Delete ${documentTitle(document)}?`,
        description:
          "This removes the source from this conversation and cannot be undone.",
        confirmLabel: "Delete source",
        destructive: true,
      });
      if (!accepted || !isCurrentScope(scope)) return;
      catalogRequestRef.current += 1;
      setBusyDocumentId(document.documentId);
      setError(null);
      try {
        await apis.delete(chatId, document.documentId);
        if (!isCurrentScope(scope)) return;
        setDocuments((current) =>
          current.filter((candidate) => candidate.documentId !== document.documentId),
        );
      } catch (err) {
        if (isCurrentScope(scope)) {
          setError(friendlyError(err, "Could not delete that source."));
        }
      } finally {
        if (isCurrentScope(scope)) setBusyDocumentId(null);
      }
    },
    [apis, chatId, confirm],
  );

  const onDeleteMany = useCallback(
    async (selected: LibraryDocument[]) => {
      if (selected.length === 0) return;
      const scope = scopeRef.current;
      const accepted = await confirm({
        title:
          selected.length === 1
            ? `Delete ${documentTitle(selected[0]!)}?`
            : `Delete ${selected.length} sources?`,
        description:
          "This removes them from this conversation and cannot be undone.",
        confirmLabel: selected.length === 1 ? "Delete source" : "Delete sources",
        destructive: true,
      });
      if (!accepted || !isCurrentScope(scope)) return;
      catalogRequestRef.current += 1;
      setError(null);

      // Report the sources that did come out even when some deletions fail,
      // rather than leaving rows on screen that are already gone.
      const deleted: string[] = [];
      let failure: unknown = null;
      for (const document of selected) {
        try {
          await apis.delete(chatId, document.documentId);
          deleted.push(document.documentId);
        } catch (err) {
          failure ??= err;
        }
      }
      if (!isCurrentScope(scope)) return;
      const removed = new Set(deleted);
      setDocuments((current) =>
        current.filter((candidate) => !removed.has(candidate.documentId)),
      );
      if (failure !== null) {
        const remaining = selected.length - deleted.length;
        setError(
          friendlyError(
            failure,
            remaining === 1
              ? "Could not delete one of those sources."
              : `Could not delete ${remaining} of those sources.`,
          ),
        );
      }
    },
    [apis, chatId, confirm],
  );

  const onRetry = useCallback(
    async (document: LibraryDocument) => {
      if (!document.failure?.retriable) return;
      const scope = scopeRef.current;
      catalogRequestRef.current += 1;
      setBusyDocumentId(document.documentId);
      setError(null);
      try {
        await apis.retry(chatId, document.documentId);
        if (!isCurrentScope(scope)) return;
        setDocuments((current) =>
          current.map((candidate) =>
            candidate.documentId === document.documentId
              ? {
                  ...candidate,
                  processingStatus: "queued",
                  searchable: false,
                  failure: null,
                }
              : candidate,
          ),
        );
        void refreshCatalog();
      } catch (err) {
        if (isCurrentScope(scope)) {
          setError(friendlyError(err, "Could not retry that source."));
        }
      } finally {
        if (isCurrentScope(scope)) setBusyDocumentId(null);
      }
    },
    [apis, chatId],
  );

  const onDownload = useCallback(
    async (document: LibraryDocument) => {
      const scope = scopeRef.current;
      try {
        await apis.export(chatId, document.documentId);
      } catch (err) {
        if (isCurrentScope(scope)) {
          setError(friendlyError(err, "Could not save that source."));
        }
      }
    },
    [apis, chatId],
  );

  const addSourcesButton = (
    <Button size="sm" disabled={importing} onClick={() => void onImport()}>
      <PlusIcon className="size-4" />
      {importing ? "Adding…" : "Add sources"}
    </Button>
  );

  const hasDocuments = documents.length > 0;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <PanelSecondaryHeader showBorder={false} className="pr-1 pl-4">
        <div className="flex items-baseline gap-3">
          <h1 className="text-lg font-medium">Sources</h1>
          {hasDocuments && (
            <span className="text-lg font-medium text-muted-foreground">{countSuffix}</span>
          )}
        </div>
        <span className="grow" />
        <div className="flex items-center gap-1 pr-2">
          <WithTooltip label="Refresh">
            <Button
              variant="ghost"
              size="icon-sm"
              disabled={loading}
              onClick={() => void refreshCatalog(true)}
            >
              <RotateCwIcon className="size-4" />
              <span className="sr-only">Refresh</span>
            </Button>
          </WithTooltip>
          {addSourcesButton}
        </div>
      </PanelSecondaryHeader>

      <div className="flex min-h-0 flex-1 flex-col gap-2 pt-4">
        {imported && (
          <div
            className="mx-4 shrink-0 rounded-md bg-info-background px-3 py-2 text-sm text-info-foreground-muted"
            role="status"
          >
            <strong className="font-medium">{imported.displayName}</strong> was added.
            OpenWave is preparing it for search.
          </div>
        )}
        {error && (
          <div
            className="mx-4 flex shrink-0 items-center justify-between gap-3 rounded-md bg-critical-background px-3 py-2 text-sm text-critical-foreground-muted"
            role="alert"
          >
            <span>{error}</span>
            <Button
              variant="outline"
              size="xs"
              className="shrink-0"
              onClick={() => void refreshCatalog(true)}
            >
              Refresh
            </Button>
          </div>
        )}
        {catalogTruncated && (
          <p className="shrink-0 px-4 text-xs text-muted-foreground">
            Showing the newest 1,000 sources; searching and filtering apply to these.
          </p>
        )}

        {loading && !hasDocuments ? (
          <p className="px-4 text-sm text-muted-foreground" role="status">
            Loading sources for this conversation…
          </p>
        ) : !hasDocuments ? (
          <Empty>
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <FileIcon />
              </EmptyMedia>
              <EmptyTitle>No sources yet</EmptyTitle>
              <EmptyDescription>
                Drop files here, or add folders and ZIP archives. PDFs, Office
                documents, Markdown, text, and supported images are prepared for search;
                other formats stay available to the conversation as sources.
              </EmptyDescription>
            </EmptyHeader>
            <EmptyContent>{addSourcesButton}</EmptyContent>
            <EmptyContent className="gap-1">
              <p className="text-muted-foreground">Maximum file size: 16MB</p>
            </EmptyContent>
          </Empty>
        ) : (
          <SourceTable
            documents={documents}
            busyDocumentId={busyDocumentId}
            canDownload={canDownload}
            onOpen={onOpen}
            onDownload={(document) => void onDownload(document)}
            onDelete={(document) => void onDelete(document)}
            onDeleteMany={onDeleteMany}
            onRetry={(document) => void onRetry(document)}
            onCountChange={setCountSuffix}
          />
        )}
      </div>
      {confirmDialog}
    </div>
  );
}

function isImportedDocument(result: { status: string }): result is LibraryImportSuccess {
  return result.status === "imported" || result.status === "already_present";
}

function friendlyError(error: unknown, fallback: string): string {
  const message = String(error).replace(/^Error:\s*/, "").trim();
  return message && message.length <= 240 ? message : fallback;
}
