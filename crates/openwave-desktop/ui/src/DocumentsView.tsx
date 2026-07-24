import { useEffect, useRef, useState } from "react";
import { FileText } from "lucide-react";
import {
  importLibraryDocument,
  listLibraryDocuments,
  searchLibraryDocuments,
  type ImportedDocument,
  type LibraryDocument,
  type LibrarySearchResult,
} from "./documents";

export function DocumentsView({
  chatId,
}: {
  chatId: string;
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

  async function onImport() {
    if (importing) return;
    setImporting(true);
    setError(null);
    setImported(null);
    try {
      const accepted = await importLibraryDocument(chatId);
      if (!mountedRef.current || !accepted) return;
      setImported(accepted);
      await refreshCatalog();
    } catch (err) {
      if (mountedRef.current) {
        setError(friendlyError(err, "Could not add that source."));
      }
    } finally {
      if (mountedRef.current) setImporting(false);
    }
  }

  async function onSearch(event: React.FormEvent) {
    event.preventDefault();
    const normalized = query.trim();
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
            {importing ? "Adding…" : "Add source…"}
          </button>
        </div>
      </header>

      <div className="documents-content">
        {imported && (
          <div className="document-notice" role="status">
            <strong>{imported.displayName}</strong> was added to this conversation.
            OpenWave is preparing it for search.
          </div>
        )}
        {error && (
          <div className="document-error" role="alert">
            <span>{error}</span>
            <button type="button" className="btn" onClick={() => void refreshCatalog(true)}>
              Try again
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
              aria-label="Search sources"
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
              <p>{documents.length} {documents.length === 1 ? "source" : "sources"}</p>
              {catalogTruncated && (
                <p>Showing the newest 1,000 sources.</p>
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
              <span>Add a file to use it in this conversation.</span>
            </div>
          ) : (
            <div className="document-list">
              {documents.map((document) => (
                <article className="document-row" key={document.documentId}>
                  <div className="document-icon" aria-hidden="true">
                    <FileText size={16} />
                  </div>
                  <div className="document-copy">
                    <strong>{documentTitle(document)}</strong>
                    <span>
                      {mediaTypeLabel(document.mediaType)} · Updated {formatDate(document.updatedAt)}
                    </span>
                  </div>
                  <DocumentStatus status={document.processingStatus} />
                </article>
              ))}
            </div>
          )}
        </section>
      </div>
    </section>
  );
}

function DocumentStatus({ status }: { status: LibraryDocument["processingStatus"] }) {
  const label =
    status === "ready"
      ? "Available"
      : status === "failed"
        ? "Needs attention"
        : "Preparing";
  return <span className={`document-status is-${status}`}>{label}</span>;
}

function documentTitle(document: LibraryDocument): string {
  return document.title?.trim() || `Source ${document.documentId.slice(0, 8)}`;
}

function mediaTypeLabel(mediaType: string): string {
  const base = mediaType.split(";")[0]?.trim().toLowerCase() ?? "";
  if (base === "application/pdf") return "PDF";
  if (base === "text/markdown") return "Markdown";
  if (base.startsWith("text/")) return "Text";
  return "File";
}

function formatDate(value: string): string {
  return new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(
    new Date(value),
  );
}

function friendlyError(error: unknown, fallback: string): string {
  const message = String(error).replace(/^Error:\s*/, "").trim();
  return message && message.length <= 240 ? message : fallback;
}
