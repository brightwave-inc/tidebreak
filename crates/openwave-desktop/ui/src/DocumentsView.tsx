import { useEffect, useMemo, useRef, useState } from "react";
import { FileText } from "lucide-react";
import {
  deleteLibraryDocument,
  importLibraryDocuments,
  listLibraryDocuments,
  retryLibraryDocument,
  searchLibraryDocuments,
  type ImportedDocument,
  type LibraryCatalog,
  type LibraryDocument,
  type LibraryImportBatch,
  type LibraryImportSuccess,
  type LibrarySearchResult,
} from "./documents";
import {
  PICKER_BUSY_MESSAGE,
  PICKER_HOLDERS,
  useNativePickerLatch,
} from "./NativePickerLatch";
import { useConfirm } from "./components/ConfirmDialog";

type SortColumn = "title" | "type" | "size" | "date";
type SortDirection = "ascending" | "descending";

export type DocumentsApis = {
  list: (chatId: string) => Promise<LibraryCatalog>;
  import: (chatId: string) => Promise<LibraryImportBatch | null>;
  search: (chatId: string, query: string) => Promise<LibrarySearchResult[]>;
  delete: (chatId: string, documentId: string) => Promise<void>;
  retry: (chatId: string, documentId: string) => Promise<void>;
};

const defaultApis: DocumentsApis = {
  list: listLibraryDocuments,
  import: importLibraryDocuments,
  search: searchLibraryDocuments,
  delete: deleteLibraryDocument,
  retry: retryLibraryDocument,
};

export function DocumentsView({
  chatId,
  documentId,
  onOpen = () => {},
  apis = defaultApis,
}: {
  chatId: string;
  /** URL-selected source identity. The detail viewer lands in #543. */
  documentId?: string;
  /** Navigate to the existing `sources.{documentId}` panel contract. */
  onOpen?: (documentId: string) => void;
  apis?: DocumentsApis;
}) {
  const [documents, setDocuments] = useState<LibraryDocument[]>([]);
  const [catalogTruncated, setCatalogTruncated] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [importing, setImporting] = useState(false);
  const [imported, setImported] = useState<ImportedDocument | null>(null);
  const [query, setQuery] = useState("");
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [results, setResults] = useState<LibrarySearchResult[] | null>(null);
  const [filter, setFilter] = useState("");
  const [sort, setSort] = useState<{
    column: SortColumn;
    direction: SortDirection;
  }>({ column: "date", direction: "descending" });
  const [busyDocument, setBusyDocument] = useState<string | null>(null);
  const mountedRef = useRef(true);
  const scopeRef = useRef(0);
  const catalogRequestRef = useRef(0);
  const searchRequestRef = useRef(0);
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
    setResults(null);
    setSearchError(null);
    setSearching(false);
    setFilter("");
    setBusyDocument(null);
    searchRequestRef.current += 1;
    void refreshCatalog(true);
    return () => {
      mountedRef.current = false;
      scopeRef.current += 1;
      catalogRequestRef.current += 1;
      searchRequestRef.current += 1;
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

  async function onSearch(event: React.FormEvent) {
    event.preventDefault();
    const normalized = query.trim();
    if (!normalized || searching) return;
    const request = ++searchRequestRef.current;
    const scope = scopeRef.current;
    setSearching(true);
    setSearchError(null);
    try {
      const next = await apis.search(chatId, normalized);
      if (!isCurrentScope(scope) || request !== searchRequestRef.current) return;
      setResults(next);
    } catch (err) {
      if (!isCurrentScope(scope) || request !== searchRequestRef.current) return;
      setSearchError(
        friendlyError(err, "Could not search this conversation's sources."),
      );
      setResults(null);
    } finally {
      if (isCurrentScope(scope) && request === searchRequestRef.current) {
        setSearching(false);
      }
    }
  }

  async function onDelete(document: LibraryDocument) {
    const title = documentTitle(document);
    const scope = scopeRef.current;
    const accepted = await confirm({
      title: `Delete ${title}?`,
      description:
        "This removes the source from this conversation and cannot be undone.",
      confirmLabel: "Delete source",
      destructive: true,
    });
    if (!accepted || !isCurrentScope(scope)) return;
    catalogRequestRef.current += 1;
    setBusyDocument(document.documentId);
    setError(null);
    try {
      await apis.delete(chatId, document.documentId);
      if (!isCurrentScope(scope)) return;
      setDocuments((current) =>
        current.filter((candidate) => candidate.documentId !== document.documentId),
      );
      setResults((current) =>
        current?.filter((result) => result.documentId !== document.documentId) ?? null,
      );
    } catch (err) {
      if (isCurrentScope(scope)) {
        setError(friendlyError(err, "Could not delete that source."));
      }
    } finally {
      if (isCurrentScope(scope)) setBusyDocument(null);
    }
  }

  async function onRetry(document: LibraryDocument) {
    if (!document.failure?.retriable) return;
    const scope = scopeRef.current;
    catalogRequestRef.current += 1;
    setBusyDocument(document.documentId);
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
      if (isCurrentScope(scope)) setBusyDocument(null);
    }
  }

  function changeSort(column: SortColumn) {
    setSort((current) => ({
      column,
      direction:
        current.column === column
          ? current.direction === "ascending"
            ? "descending"
            : "ascending"
          : column === "date"
            ? "descending"
            : "ascending",
    }));
  }

  const visibleDocuments = useMemo(
    () => filterAndSortDocuments(documents, filter, sort.column, sort.direction),
    [documents, filter, sort],
  );
  const titles = new Map(
    documents.map((document) => [document.documentId, documentTitle(document)]),
  );

  return (
    <section className="documents-view" aria-labelledby="documents-title">
      <header className="documents-header">
        <div>
          <h1 id="documents-title">Sources</h1>
          <p>
            Add files for OpenWave to use in this conversation. Supported sources
            are prepared for search on this device.
          </p>
        </div>
        <div className="documents-header-actions">
          <button
            type="button"
            className="btn btn-primary"
            disabled={importing}
            onClick={() => void onImport()}
          >
            {importing ? "Adding…" : "Add sources…"}
          </button>
        </div>
      </header>

      <div className="documents-content">
        {imported && (
          <div className="document-notice" role="status">
            <span>
              <strong>{imported.displayName}</strong> was added to this conversation.
              OpenWave is preparing it for search.
            </span>
          </div>
        )}
        {error && (
          <div className="document-error" role="alert">
            <span>{error}</span>
            <button type="button" className="btn" onClick={() => void refreshCatalog(true)}>
              Refresh
            </button>
          </div>
        )}

        <section className="document-search" aria-labelledby="document-search-title">
          <div>
            <h2 id="document-search-title">Search this conversation</h2>
            <p>Find relevant passages in sources that are ready.</p>
          </div>
          <form onSubmit={(event) => void onSearch(event)}>
            <input
              aria-label="Search source contents"
              placeholder="What are you looking for?"
              maxLength={500}
              value={query}
              onChange={(event) => {
                searchRequestRef.current += 1;
                setSearching(false);
                setSearchError(null);
                setResults(null);
                setQuery(event.target.value);
              }}
            />
            <button
              type="submit"
              className="btn"
              disabled={searching || !query.trim()}
            >
              {searching ? "Searching…" : "Search"}
            </button>
          </form>
          {searchError && <p className="document-search-error">{searchError}</p>}
          {results !== null && (
            <div className="document-results" aria-live="polite">
              {results.length === 0 ? (
                <p className="document-empty-small">No matching passages found.</p>
              ) : (
                results.map((result, index) => (
                  <article className="document-result" key={`${result.documentId}-${index}`}>
                    <div className="document-result-source">
                      <strong>{titles.get(result.documentId) ?? "Source"}</strong>
                      {result.heading && <span>{result.heading}</span>}
                    </div>
                    <p>{result.snippet}</p>
                  </article>
                ))
              )}
            </div>
          )}
        </section>

        <section className="document-catalog" aria-labelledby="document-catalog-title">
          <div className="document-section-heading">
            <div>
              <h2 id="document-catalog-title">Conversation sources</h2>
              <p>
                {documents.length} {documents.length === 1 ? "source" : "sources"}
                {filter.trim() && ` · ${visibleDocuments.length} shown`}
              </p>
              {catalogTruncated && (
                <p>
                  Showing the newest 1,000 sources; sorting and filtering apply to
                  these sources.
                </p>
              )}
            </div>
            <button
              type="button"
              className="document-refresh"
              disabled={loading}
              onClick={() => void refreshCatalog(true)}
            >
              Refresh
            </button>
          </div>

          {loading && documents.length === 0 ? (
            <div className="document-empty" role="status">
              Loading sources for this conversation…
            </div>
          ) : documents.length === 0 && !error ? (
            <div className="document-empty">
              <strong>No sources yet</strong>
              <span>
                Drop files of any type, up to 16 MB each. PDFs, Office documents,
                Markdown, text, and supported images are prepared for search; other
                formats remain available as conversation sources.
              </span>
              <span>This panel shows the newest 1,000 sources.</span>
            </div>
          ) : (
            <>
              <div className="document-catalog-toolbar">
                <label>
                  <span className="sr-only">Filter sources</span>
                  <input
                    type="search"
                    aria-label="Filter sources"
                    placeholder="Filter by title or type"
                    value={filter}
                    onChange={(event) => setFilter(event.target.value)}
                  />
                </label>
              </div>
              {visibleDocuments.length === 0 ? (
                <div className="document-empty">
                  <strong>No matching sources</strong>
                  <span>Try another title or file type.</span>
                  <button type="button" className="btn" onClick={() => setFilter("")}>
                    Clear filter
                  </button>
                </div>
              ) : (
                <div className="document-table-wrap">
                  <table className="document-table">
                    <caption className="sr-only">
                      Sources attached to this conversation
                    </caption>
                    <thead>
                      <tr>
                        <SortHeader
                          label="Title"
                          column="title"
                          sortColumn={sort.column}
                          direction={sort.direction}
                          onSort={changeSort}
                        />
                        <SortHeader
                          label="Type"
                          column="type"
                          sortColumn={sort.column}
                          direction={sort.direction}
                          onSort={changeSort}
                        />
                        <SortHeader
                          label="Size"
                          column="size"
                          sortColumn={sort.column}
                          direction={sort.direction}
                          onSort={changeSort}
                        />
                        <SortHeader
                          label="Date"
                          column="date"
                          sortColumn={sort.column}
                          direction={sort.direction}
                          onSort={changeSort}
                        />
                        <th scope="col">Status</th>
                        <th scope="col">
                          <span className="sr-only">Actions</span>
                        </th>
                      </tr>
                    </thead>
                    <tbody>
                      {visibleDocuments.map((document) => {
                        const title = documentTitle(document);
                        const busy = busyDocument === document.documentId;
                        return (
                          <tr
                            key={document.documentId}
                            data-status={document.processingStatus}
                            aria-current={
                              document.documentId === documentId ? "true" : undefined
                            }
                          >
                            <td>
                              <button
                                type="button"
                                className="document-title-button"
                                onClick={() => onOpen(document.documentId)}
                                aria-label={`Open ${title}`}
                              >
                                <span className="document-icon" aria-hidden="true">
                                  <FileText size={16} />
                                </span>
                                <strong>{title}</strong>
                              </button>
                            </td>
                            <td>{mediaTypeLabel(document.mediaType)}</td>
                            <td>{formatSize(document.sizeBytes)}</td>
                            <td>
                              <time dateTime={document.updatedAt}>
                                {formatDate(document.updatedAt)}
                              </time>
                            </td>
                            <td className="document-status-cell">
                              <DocumentStatus
                                document={document}
                                busy={busy}
                                onRetry={() => void onRetry(document)}
                              />
                            </td>
                            <td>
                              <div className="document-actions">
                                <button
                                  type="button"
                                  className="document-action"
                                  disabled={busy}
                                  onClick={() => onOpen(document.documentId)}
                                >
                                  Open
                                </button>
                                <button
                                  type="button"
                                  className="document-action is-danger"
                                  disabled={busy}
                                  onClick={() => void onDelete(document)}
                                  aria-label={`Delete ${title}`}
                                >
                                  Delete
                                </button>
                              </div>
                            </td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>
              )}
            </>
          )}
        </section>
      </div>
      {confirmDialog}
    </section>
  );
}

function SortHeader({
  label,
  column,
  sortColumn,
  direction,
  onSort,
}: {
  label: string;
  column: SortColumn;
  sortColumn: SortColumn;
  direction: SortDirection;
  onSort: (column: SortColumn) => void;
}) {
  const active = column === sortColumn;
  return (
    <th scope="col" aria-sort={active ? direction : "none"}>
      <button type="button" onClick={() => onSort(column)}>
        {label}
        <span aria-hidden="true">{active ? (direction === "ascending" ? " ↑" : " ↓") : ""}</span>
      </button>
    </th>
  );
}

function DocumentStatus({
  document,
  busy,
  onRetry,
}: {
  document: LibraryDocument;
  busy: boolean;
  onRetry: () => void;
}) {
  if (document.processingStatus === "ready" && document.searchable) {
    return <span className="sr-only">Ready</span>;
  }
  if (document.processingStatus === "ready") {
    return (
      <span className="document-unsearchable">
        Not searchable
        <span>OpenWave stored this file, but found no searchable text.</span>
      </span>
    );
  }
  if (
    document.processingStatus === "queued" ||
    document.processingStatus === "processing"
  ) {
    return (
      <span className="document-processing" role="status">
        <span className="document-processing-pulse" aria-hidden="true" />
        Preparing
      </span>
    );
  }
  const failure = document.failure;
  return (
    <span className="document-failure">
      <span>{failure?.message ?? "OpenWave could not prepare this source."}</span>
      {failure?.retriable && (
        <button type="button" className="document-retry" disabled={busy} onClick={onRetry}>
          {busy ? "Retrying…" : "Retry"}
        </button>
      )}
    </span>
  );
}

function filterAndSortDocuments(
  documents: LibraryDocument[],
  filter: string,
  column: SortColumn,
  direction: SortDirection,
): LibraryDocument[] {
  const normalized = filter.trim().toLocaleLowerCase();
  const visible = normalized
    ? documents.filter((document) =>
        [documentTitle(document), mediaTypeLabel(document.mediaType), document.mediaType]
          .join(" ")
          .toLocaleLowerCase()
          .includes(normalized),
      )
    : [...documents];
  return visible.sort((left, right) => {
    if (column === "size") {
      if (left.sizeBytes === null) return right.sizeBytes === null ? tieBreak(left, right) : 1;
      if (right.sizeBytes === null) return -1;
    }
    const comparison =
      column === "title"
        ? documentTitle(left).localeCompare(documentTitle(right), undefined, {
            sensitivity: "base",
          })
        : column === "type"
          ? mediaTypeLabel(left.mediaType).localeCompare(
              mediaTypeLabel(right.mediaType),
              undefined,
              { sensitivity: "base" },
            )
          : column === "size"
            ? (left.sizeBytes ?? 0) - (right.sizeBytes ?? 0)
            : Date.parse(left.updatedAt) - Date.parse(right.updatedAt);
    return comparison === 0
      ? tieBreak(left, right)
      : comparison * (direction === "ascending" ? 1 : -1);
  });
}

function tieBreak(left: LibraryDocument, right: LibraryDocument): number {
  return documentTitle(left)
    .localeCompare(documentTitle(right), undefined, { sensitivity: "base" })
    || left.documentId.localeCompare(right.documentId);
}

function documentTitle(document: LibraryDocument): string {
  return document.title?.trim() || `Source ${document.documentId.slice(0, 8)}`;
}

function mediaTypeLabel(mediaType: string): string {
  const base = mediaType.split(";")[0]?.trim().toLowerCase() ?? "";
  if (base === "application/pdf") return "PDF";
  if (base === "text/markdown") return "Markdown";
  if (base.includes("wordprocessingml") || base === "application/msword") return "Word";
  if (base.includes("spreadsheetml") || base === "application/vnd.ms-excel") return "Excel";
  if (base.includes("presentationml") || base === "application/vnd.ms-powerpoint") {
    return "PowerPoint";
  }
  if (base.startsWith("image/")) return "Image";
  if (base.startsWith("text/")) return "Text";
  return "File";
}

function formatSize(value: number | null): string {
  if (value === null) return "—";
  if (value < 1_024) return `${value} B`;
  if (value < 1_048_576) return `${formatNumber(value / 1_024)} KB`;
  return `${formatNumber(value / 1_048_576)} MB`;
}

function formatNumber(value: number): string {
  return new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(value);
}

function formatDate(value: string): string {
  return new Intl.DateTimeFormat(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  }).format(new Date(value));
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
