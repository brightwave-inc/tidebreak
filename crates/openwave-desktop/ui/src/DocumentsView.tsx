import { useEffect, useMemo, useRef, useState } from "react";
import {
  ArrowDown,
  ArrowUp,
  FileText,
  MoreHorizontal,
  RefreshCw,
  Search,
  Trash2,
} from "lucide-react";

import {
  deleteLibraryDocument,
  importLibraryDocuments,
  listLibraryDocuments,
  retryLibraryDocument,
  searchLibraryDocuments,
  type ImportedDocument,
  type LibraryDocument,
  type LibraryImportSuccess,
  type LibrarySearchResult,
} from "./documents";
import {
  PICKER_BUSY_MESSAGE,
  PICKER_HOLDERS,
  useNativePickerLatch,
} from "./NativePickerLatch";
import { useConfirm } from "./components/ConfirmDialog";
import { PanelSecondaryHeader } from "./components/PanelHeader";
import { Button } from "./components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "./components/ui/dropdown-menu";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "./components/ui/empty";
import { Input } from "./components/ui/input";

type SortKey = "name" | "type" | "size" | "date";
type SortDirection = "asc" | "desc";
type Sort = { key: SortKey; direction: SortDirection };

const DEFAULT_SORT: Sort = { key: "date", direction: "desc" };

export function DocumentsView({
  chatId,
  onOpenDocument,
}: {
  chatId: string;
  /**
   * Show one source on its own. Arrives as a callback rather than being read
   * from the layout here so the list can be rendered without the routing that
   * decides where a document panel lands.
   */
  onOpenDocument?: (documentId: string) => void;
}) {
  const [documents, setDocuments] = useState<LibraryDocument[]>([]);
  const [catalogTruncated, setCatalogTruncated] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [importing, setImporting] = useState(false);
  const [imported, setImported] = useState<ImportedDocument | null>(null);
  const [filter, setFilter] = useState("");
  const [sort, setSort] = useState<Sort>(DEFAULT_SORT);
  const [busyDocumentId, setBusyDocumentId] = useState<string | null>(null);
  const [passageQuery, setPassageQuery] = useState("");
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [results, setResults] = useState<LibrarySearchResult[] | null>(null);
  const { confirm, dialog } = useConfirm();
  const mountedRef = useRef(true);
  const catalogRequestRef = useRef(0);
  const searchRequestRef = useRef(0);

  async function refreshCatalog(showLoading = false) {
    const request = ++catalogRequestRef.current;
    if (showLoading) setLoading(true);
    try {
      const next = await listLibraryDocuments(chatId);
      if (!mountedRef.current || request !== catalogRequestRef.current) return;
      setDocuments(next.documents);
      setCatalogTruncated(next.truncated);
      setError(null);
    } catch (err) {
      if (!mountedRef.current || request !== catalogRequestRef.current) return;
      setError(friendlyError(err, "Could not load this conversation's sources."));
    } finally {
      if (mountedRef.current && request === catalogRequestRef.current) {
        setLoading(false);
      }
    }
  }

  useEffect(() => {
    mountedRef.current = true;
    setDocuments([]);
    setCatalogTruncated(false);
    setError(null);
    setImported(null);
    setFilter("");
    setSort(DEFAULT_SORT);
    setResults(null);
    setSearchError(null);
    setSearching(false);
    searchRequestRef.current += 1;
    void refreshCatalog(true);
    return () => {
      mountedRef.current = false;
      catalogRequestRef.current += 1;
      searchRequestRef.current += 1;
    };
  }, [chatId]);

  const processing = documents.some((document) =>
    ["queued", "processing"].includes(document.processingStatus),
  );

  useEffect(() => {
    if (!processing) return;
    const interval = window.setInterval(() => void refreshCatalog(), 2_000);
    return () => window.clearInterval(interval);
  }, [processing]);

  const visible = useMemo(() => sortDocuments(documents, filter, sort), [
    documents,
    filter,
    sort,
  ]);

  async function onImport() {
    if (importing) return;
    if (!useNativePickerLatch.getState().claim(PICKER_HOLDERS.importSource)) {
      setError(PICKER_BUSY_MESSAGE);
      return;
    }
    setImporting(true);
    setError(null);
    setImported(null);
    try {
      const batch = await importLibraryDocuments(chatId);
      const accepted = batch?.results.find(isImportedDocument);
      if (!mountedRef.current || !accepted) return;
      setImported(accepted.document);
      await refreshCatalog();
    } catch (err) {
      if (mountedRef.current) {
        setError(friendlyError(err, "Could not add that source."));
      }
    } finally {
      useNativePickerLatch.getState().release(PICKER_HOLDERS.importSource);
      if (mountedRef.current) setImporting(false);
    }
  }

  async function onDelete(document: LibraryDocument) {
    if (busyDocumentId) return;
    const confirmed = await confirm({
      title: `Remove ${documentTitle(document)}?`,
      description:
        "This conversation stops using the source and its prepared text is discarded. The original file on your device is untouched.",
      confirmLabel: "Remove",
      destructive: true,
    });
    if (!confirmed || !mountedRef.current) return;
    setBusyDocumentId(document.documentId);
    setError(null);
    try {
      await deleteLibraryDocument(chatId, document.documentId);
      if (!mountedRef.current) return;
      // Drop the row before the catalog comes back: the delete is durable, and
      // waiting on a round trip leaves a source the reader just removed sitting
      // in the list looking like the action failed.
      setDocuments((current) =>
        current.filter((entry) => entry.documentId !== document.documentId),
      );
      await refreshCatalog();
    } catch (err) {
      if (mountedRef.current) {
        setError(friendlyError(err, "Could not remove that source."));
      }
    } finally {
      if (mountedRef.current) setBusyDocumentId(null);
    }
  }

  async function onRetry(document: LibraryDocument) {
    if (busyDocumentId) return;
    setBusyDocumentId(document.documentId);
    setError(null);
    try {
      await retryLibraryDocument(chatId, document.documentId);
      if (!mountedRef.current) return;
      await refreshCatalog();
    } catch (err) {
      if (mountedRef.current) {
        setError(friendlyError(err, "Could not prepare that source again."));
      }
    } finally {
      if (mountedRef.current) setBusyDocumentId(null);
    }
  }

  async function onSearch(event: React.FormEvent) {
    event.preventDefault();
    const normalized = passageQuery.trim();
    if (!normalized || searching) return;
    const request = ++searchRequestRef.current;
    setSearching(true);
    setSearchError(null);
    try {
      const next = await searchLibraryDocuments(chatId, normalized);
      if (!mountedRef.current || request !== searchRequestRef.current) return;
      setResults(next);
    } catch (err) {
      if (!mountedRef.current || request !== searchRequestRef.current) return;
      setSearchError(
        friendlyError(err, "Could not search this conversation's sources."),
      );
      setResults(null);
    } finally {
      if (mountedRef.current && request === searchRequestRef.current) {
        setSearching(false);
      }
    }
  }

  const titles = new Map(
    documents.map((document) => [document.documentId, documentTitle(document)]),
  );

  return (
    <>
      <PanelSecondaryHeader className="px-4">
        <h1 className="text-lg font-medium">Sources</h1>
        <span className="text-sm text-muted-foreground">{documents.length}</span>
        <span className="grow" />
        <Button size="sm" disabled={importing} onClick={() => void onImport()}>
          {importing ? "Adding…" : "Add sources…"}
        </Button>
      </PanelSecondaryHeader>

      <div className="flex min-h-0 flex-1 flex-col gap-3 p-4">
        {imported && (
          <p className="text-sm text-muted-foreground" role="status">
            <span className="font-medium text-foreground">
              {imported.displayName}
            </span>{" "}
            was added. OpenWave is preparing it for search.
          </p>
        )}
        {error && (
          <div
            role="alert"
            className="flex items-center gap-2 rounded-md border border-critical-background bg-critical-background px-3 py-2 text-sm text-critical-foreground"
          >
            <span className="min-w-0 flex-1">{error}</span>
            <Button size="xs" variant="outline" onClick={() => void refreshCatalog(true)}>
              Try again
            </Button>
          </div>
        )}

        <div className="relative">
          <Search className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            className="pl-8"
            placeholder="Search sources"
            aria-label="Search sources"
            value={filter}
            onChange={(event) => setFilter(event.target.value)}
          />
        </div>

        {loading && documents.length === 0 ? (
          <p className="text-sm text-muted-foreground" role="status">
            Loading sources for this conversation…
          </p>
        ) : documents.length === 0 ? (
          !error && (
            <Empty className="border">
              <EmptyHeader>
                <EmptyMedia variant="icon">
                  <FileText />
                </EmptyMedia>
                <EmptyTitle>No sources yet</EmptyTitle>
                <EmptyDescription>
                  Drop files anywhere in this window, or use Add sources. PDFs,
                  Word and Excel documents, Markdown, and plain text are prepared
                  for search; other files are stored so OpenWave can still open
                  and cite them. Folders and empty files are not accepted.
                </EmptyDescription>
              </EmptyHeader>
            </Empty>
          )
        ) : visible.length === 0 ? (
          <p className="text-sm text-muted-foreground">No source name contains that.</p>
        ) : (
          <div className="min-h-0 flex-1 overflow-y-auto">
            <table className="w-full table-fixed text-sm">
              <thead className="sticky top-0 bg-background">
                <tr className="border-b text-xs text-muted-foreground">
                  <SortableHeader
                    className="w-auto"
                    sortKey="name"
                    sort={sort}
                    onSort={setSort}
                  >
                    Name
                  </SortableHeader>
                  <SortableHeader
                    className="w-20"
                    sortKey="type"
                    sort={sort}
                    onSort={setSort}
                  >
                    Type
                  </SortableHeader>
                  <SortableHeader
                    className="w-20"
                    sortKey="size"
                    sort={sort}
                    onSort={setSort}
                  >
                    Size
                  </SortableHeader>
                  <SortableHeader
                    className="w-20"
                    sortKey="date"
                    sort={sort}
                    onSort={setSort}
                  >
                    Added
                  </SortableHeader>
                  <th className="w-8">
                    <span className="sr-only">Actions</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {visible.map((document) => (
                  <DocumentRow
                    key={document.documentId}
                    document={document}
                    busy={busyDocumentId === document.documentId}
                    disabled={busyDocumentId !== null}
                    onOpen={onOpenDocument}
                    onDelete={() => void onDelete(document)}
                    onRetry={() => void onRetry(document)}
                  />
                ))}
              </tbody>
            </table>
            {catalogTruncated && (
              <p className="pt-2 text-xs text-muted-foreground">
                Showing the newest 1,000 sources.
              </p>
            )}
          </div>
        )}

        <section
          className="shrink-0 border-t pt-3"
          aria-labelledby="document-passage-search"
        >
          <h2 id="document-passage-search" className="sr-only">
            Find a passage
          </h2>
          <form className="flex gap-2" onSubmit={(event) => void onSearch(event)}>
            <Input
              aria-label="Find a passage in these sources"
              placeholder="Find a passage…"
              maxLength={500}
              value={passageQuery}
              onChange={(event) => {
                searchRequestRef.current += 1;
                setSearching(false);
                setSearchError(null);
                setResults(null);
                setPassageQuery(event.target.value);
              }}
            />
            <Button
              type="submit"
              variant="outline"
              disabled={searching || !passageQuery.trim()}
            >
              {searching ? "Searching…" : "Find"}
            </Button>
          </form>
          {searchError && (
            <p className="pt-2 text-sm text-critical-foreground">{searchError}</p>
          )}
          {results !== null && (
            <div className="max-h-52 overflow-y-auto pt-2" aria-live="polite">
              {results.length === 0 ? (
                <p className="text-sm text-muted-foreground">
                  No matching passages found.
                </p>
              ) : (
                results.map((result, index) => (
                  <article
                    className="border-b py-2 last:border-b-0"
                    key={`${result.documentId}-${index}`}
                  >
                    <p className="text-xs text-muted-foreground">
                      {titles.get(result.documentId) ?? "Source"}
                      {result.heading && ` · ${result.heading}`}
                    </p>
                    <p className="pt-1 text-sm">{result.snippet}</p>
                  </article>
                ))
              )}
            </div>
          )}
        </section>
      </div>
      {dialog}
    </>
  );
}

function SortableHeader({
  children,
  className,
  sortKey,
  sort,
  onSort,
}: {
  children: React.ReactNode;
  className?: string;
  sortKey: SortKey;
  sort: Sort;
  onSort: (sort: Sort) => void;
}) {
  const active = sort.key === sortKey;
  return (
    <th className={`px-2 py-1.5 text-left font-normal ${className ?? ""}`}>
      <button
        type="button"
        className="flex cursor-pointer items-center gap-1 hover:text-foreground"
        aria-sort={active ? (sort.direction === "asc" ? "ascending" : "descending") : "none"}
        onClick={() =>
          onSort({
            key: sortKey,
            // A second click on the active column reverses it; a new column
            // starts from the order that reads most usefully for its values.
            direction: active
              ? sort.direction === "asc"
                ? "desc"
                : "asc"
              : defaultDirection(sortKey),
          })
        }
      >
        {children}
        {active &&
          (sort.direction === "asc" ? (
            <ArrowUp className="size-3" aria-hidden="true" />
          ) : (
            <ArrowDown className="size-3" aria-hidden="true" />
          ))}
      </button>
    </th>
  );
}

function DocumentRow({
  document,
  busy,
  disabled,
  onOpen,
  onDelete,
  onRetry,
}: {
  document: LibraryDocument;
  busy: boolean;
  disabled: boolean;
  onOpen?: (documentId: string) => void;
  onDelete: () => void;
  onRetry: () => void;
}) {
  const title = documentTitle(document);
  const failed = document.processingStatus === "failed";
  const retriable = failed && document.failure?.retriable === true;

  return (
    <tr className="border-b last:border-b-0 hover:bg-muted/60">
      <td className="px-2 py-2">
        <button
          type="button"
          className="flex w-full min-w-0 cursor-pointer items-center gap-2 text-left disabled:cursor-default"
          disabled={!onOpen}
          onClick={() => onOpen?.(document.documentId)}
        >
          <FileText className="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
          <span className="min-w-0 truncate">{title}</span>
          <DocumentStatus document={document} />
        </button>
        {failed && (
          <div className="flex items-center gap-2 pt-1 pl-6">
            <span className="min-w-0 text-xs text-critical-foreground">
              {failureExplanation(document)}
            </span>
            {retriable && (
              <Button size="2xs" variant="outline" disabled={disabled} onClick={onRetry}>
                <RefreshCw aria-hidden="true" />
                {busy ? "Retrying…" : "Retry"}
              </Button>
            )}
          </div>
        )}
      </td>
      <td className="px-2 py-2 text-muted-foreground">{mediaTypeLabel(document.mediaType)}</td>
      <td className="px-2 py-2 text-muted-foreground">{formatBytes(document.sizeBytes)}</td>
      <td className="px-2 py-2 text-muted-foreground">{formatDate(document.createdAt)}</td>
      <td className="px-1 py-2">
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button size="icon-xs" variant="ghost" disabled={disabled}>
              <MoreHorizontal aria-hidden="true" />
              <span className="sr-only">{`Actions for ${title}`}</span>
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            {onOpen && (
              <DropdownMenuItem onSelect={() => onOpen(document.documentId)}>
                Open
              </DropdownMenuItem>
            )}
            {retriable && (
              <DropdownMenuItem onSelect={onRetry}>
                <RefreshCw aria-hidden="true" />
                Retry
              </DropdownMenuItem>
            )}
            {(onOpen || retriable) && <DropdownMenuSeparator />}
            <DropdownMenuItem variant="destructive" onSelect={onDelete}>
              <Trash2 aria-hidden="true" />
              Remove
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </td>
    </tr>
  );
}

/**
 * A source that is ready says nothing: a list where every row is decorated
 * makes the rows that need attention harder to find, not easier.
 */
function DocumentStatus({ document }: { document: LibraryDocument }) {
  const { processingStatus: status, searchable } = document;
  if (status === "queued" || status === "processing") {
    return (
      <span
        className="ml-auto flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground"
        aria-label="Preparing"
      >
        <span
          aria-hidden="true"
          className="size-1.5 animate-pulse rounded-full bg-openwave"
        />
        Preparing
      </span>
    );
  }
  // A source can finish processing without producing anything to search — a
  // scan without OCR, or a format whose parser is missing on this host. Saying
  // nothing there would promise a search that silently never matches.
  if (status === "ready" && !searchable) {
    return (
      <span
        className="ml-auto shrink-0 text-xs text-muted-foreground"
        title="Stored in this conversation, but nothing in it can be searched."
      >
        Not searchable
      </span>
    );
  }
  return null;
}

/**
 * Failure categories recorded by the processing pipeline, in the terms a reader
 * can act on. An unfamiliar category still gets a sentence rather than a code.
 */
const FAILURE_EXPLANATIONS: Record<string, string> = {
  activation_fenced: "This source changed while it was being prepared.",
  dimension_mismatch: "This source was prepared for a different search model.",
  embedding_failed: "Preparing this source for search did not finish.",
  generation_conflict: "This source changed while it was being prepared.",
  generation_fenced: "This source changed while it was being prepared.",
  index_failed: "Preparing this source for search did not finish.",
  invalid_document_stage: "Preparation stopped part-way through.",
  parse_failed: "OpenWave could not read what is inside this file.",
  parser_unavailable: "This device has no reader for this kind of file.",
  pipeline_changed: "This source was prepared by an older version of OpenWave.",
  source_blob_digest_mismatch: "The stored copy no longer matches this source.",
  source_blob_length_mismatch: "The stored copy no longer matches this source.",
  source_blob_missing: "The stored copy of this source is gone.",
  source_blob_read_failed: "The stored copy of this source could not be read.",
  unsupported_job_kind: "Preparation stopped part-way through.",
  vector_store_failed: "Preparing this source for search did not finish.",
};

function failureExplanation(document: LibraryDocument): string {
  const reason = document.failure?.reason;
  return (
    (reason && FAILURE_EXPLANATIONS[reason]) ??
    "OpenWave could not prepare this source."
  );
}

function defaultDirection(key: SortKey): SortDirection {
  // Newest and largest first; names and types read better alphabetically.
  return key === "date" || key === "size" ? "desc" : "asc";
}

export function sortDocuments(
  documents: LibraryDocument[],
  filter: string,
  sort: Sort,
): LibraryDocument[] {
  const needle = filter.trim().toLowerCase();
  const matches = needle
    ? documents.filter((document) =>
        documentTitle(document).toLowerCase().includes(needle),
      )
    : [...documents];
  const order = sort.direction === "asc" ? 1 : -1;
  return matches.sort((left, right) => {
    switch (sort.key) {
      case "name":
        return order * documentTitle(left).localeCompare(documentTitle(right));
      case "type":
        return (
          order * mediaTypeLabel(left.mediaType).localeCompare(mediaTypeLabel(right.mediaType))
        );
      case "size":
        return order * ((left.sizeBytes ?? 0) - (right.sizeBytes ?? 0));
      case "date":
        return order * (Date.parse(left.createdAt) - Date.parse(right.createdAt));
    }
  });
}

function documentTitle(document: LibraryDocument): string {
  return document.title?.trim() || `Source ${document.documentId.slice(0, 8)}`;
}

function mediaTypeLabel(mediaType: string): string {
  const base = mediaType.split(";")[0]?.trim().toLowerCase() ?? "";
  if (base === "application/pdf") return "PDF";
  if (base === "text/markdown") return "Markdown";
  if (base === "text/csv" || base === "text/tab-separated-values") return "CSV";
  if (base === "application/json") return "JSON";
  if (base === "text/html") return "HTML";
  if (base.includes("wordprocessingml") || base === "application/msword") return "Word";
  if (base.includes("spreadsheetml") || base === "application/vnd.ms-excel") return "Excel";
  if (base.includes("presentationml") || base === "application/vnd.ms-powerpoint") {
    return "Slides";
  }
  if (base.startsWith("image/")) return "Image";
  if (base.startsWith("text/")) return "Text";
  return "File";
}

function formatBytes(bytes: number | null): string {
  if (bytes === null) return "—";
  if (bytes < 1_000) return `${bytes} B`;
  if (bytes < 1_000_000) return `${(bytes / 1_000).toFixed(0)} KB`;
  return `${(bytes / 1_000_000).toFixed(bytes < 10_000_000 ? 1 : 0)} MB`;
}

function formatDate(value: string): string {
  return new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(
    new Date(value),
  );
}

function isImportedDocument(
  result: { status: string },
): result is LibraryImportSuccess {
  return result.status === "imported" || result.status === "already_present";
}

function friendlyError(error: unknown, fallback: string): string {
  const message = String(error).replace(/^Error:\s*/, "").trim();
  return message && message.length <= 240 ? message : fallback;
}
