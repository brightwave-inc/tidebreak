import { useEffect, useRef, useState } from "react";
import { Download, FileOutput, RefreshCw } from "lucide-react";
import {
  exportDeliverable,
  listDeliverables,
  readDeliverable,
  type DeliverablePreview,
  type DeliverablesCatalog,
} from "./deliverables";
import { MessageMarkdown } from "./MessageMarkdown";

type DeliverableApis = {
  list: (chatId: string) => Promise<DeliverablesCatalog>;
  read: (chatId: string, filename: string) => Promise<DeliverablePreview>;
  export: (chatId: string, filename: string) => Promise<boolean>;
};

const defaultApis: DeliverableApis = {
  list: listDeliverables,
  read: readDeliverable,
  export: exportDeliverable,
};

export function DeliverablesView({
  chatId,
  initialFilename,
  apis = defaultApis,
}: {
  chatId: string;
  /** Output to preview on arrival; ignored when this chat has no such file. */
  initialFilename?: string;
  apis?: DeliverableApis;
}) {
  const [catalog, setCatalog] = useState<DeliverablesCatalog>({
    deliverables: [],
    truncated: false,
  });
  const [selected, setSelected] = useState<string | null>(null);
  const [preview, setPreview] = useState<DeliverablePreview | null>(null);
  const [loading, setLoading] = useState(true);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [exporting, setExporting] = useState(false);
  const [exportStatus, setExportStatus] = useState<{
    message: string;
    error: boolean;
  } | null>(null);
  const [previewVersion, setPreviewVersion] = useState(0);
  const catalogGenerationRef = useRef(0);
  const previewGenerationRef = useRef(0);
  const pendingFilenameRef = useRef<string | null>(initialFilename ?? null);

  async function refresh(showLoading = false) {
    const generation = ++catalogGenerationRef.current;
    if (showLoading) setLoading(true);
    setError(null);
    try {
      const next = await apis.list(chatId);
      if (generation !== catalogGenerationRef.current) return;
      // Steer the first catalog that arrives, then step aside: a later refresh
      // or a choice from the list must not snap back to where the caller
      // pointed. Resolving here rather than after the fact means an output the
      // chat does not own never gets read.
      const target = pendingFilenameRef.current;
      pendingFilenameRef.current = null;
      const owns = (filename: string | null | undefined) =>
        !!filename && next.deliverables.some((item) => item.filename === filename);
      setCatalog(next);
      setSelected((current) =>
        owns(target)
          ? target
          : owns(current)
            ? current
            : (next.deliverables[0]?.filename ?? null),
      );
      setPreviewVersion((current) => current + 1);
    } catch (caught) {
      if (generation !== catalogGenerationRef.current) return;
      setError(friendlyOutputError(caught, "Could not load this conversation's outputs."));
    } finally {
      if (generation === catalogGenerationRef.current) setLoading(false);
    }
  }

  useEffect(() => {
    setCatalog({ deliverables: [], truncated: false });
    setSelected(null);
    setPreview(null);
    setError(null);
    setPreviewError(null);
    setExportStatus(null);
    void refresh(true);
    return () => {
      catalogGenerationRef.current += 1;
      previewGenerationRef.current += 1;
    };
  }, [chatId, apis]);

  // Being pointed somewhere new while the surface is already open resolves
  // against the catalog in hand, and otherwise waits for the next one.
  useEffect(() => {
    if (!initialFilename) return;
    if (catalog.deliverables.some((item) => item.filename === initialFilename)) {
      pendingFilenameRef.current = null;
      setSelected(initialFilename);
    } else {
      pendingFilenameRef.current = initialFilename;
    }
  }, [initialFilename]);

  useEffect(() => {
    if (!selected) {
      setPreview(null);
      setPreviewError(null);
      return;
    }
    const generation = ++previewGenerationRef.current;
    setPreview(null);
    setPreviewError(null);
    setPreviewLoading(true);
    setExportStatus(null);
    void apis
      .read(chatId, selected)
      .then((next) => {
        if (generation === previewGenerationRef.current) setPreview(next);
      })
      .catch((caught) => {
        if (generation === previewGenerationRef.current) {
          setPreviewError(friendlyOutputError(caught, "Could not preview that output."));
        }
      })
      .finally(() => {
        if (generation === previewGenerationRef.current) setPreviewLoading(false);
      });
  }, [chatId, selected, previewVersion, apis]);

  async function onExport() {
    if (!selected || exporting) return;
    setExporting(true);
    setExportStatus(null);
    try {
      const saved = await apis.export(chatId, selected);
      if (saved) {
        setExportStatus({ message: `${selected} was saved.`, error: false });
      }
    } catch (caught) {
      setExportStatus({
        message: friendlyOutputError(caught, "Could not save that output."),
        error: true,
      });
    } finally {
      setExporting(false);
    }
  }

  return (
    <section className="deliverables-view" aria-labelledby="deliverables-title">
      <header className="deliverables-header">
        <div>
          <h1 id="deliverables-title">Outputs</h1>
          <p>
            Files OpenWave creates for you stay private until you choose where to
            save them.
          </p>
        </div>
        <div className="deliverables-header-actions">
          <button
            type="button"
            className="btn"
            disabled={loading}
            onClick={() => void refresh(true)}
          >
            <RefreshCw size={14} aria-hidden="true" />
            Refresh
          </button>
        </div>
      </header>

      <div className="deliverables-content">
        {error && (
          <div className="deliverables-error" role="alert">
            <span>{error}</span>
            <button type="button" className="btn" onClick={() => void refresh(true)}>
              Try again
            </button>
          </div>
        )}

        {loading && catalog.deliverables.length === 0 ? (
          <div className="deliverables-empty" role="status">
            Loading outputs for this conversation…
          </div>
        ) : catalog.deliverables.length === 0 && !error ? (
          <div className="deliverables-empty">
            <FileOutput size={22} aria-hidden="true" />
            <strong>No outputs yet</strong>
            <span>
              Ask OpenWave to create a report, plan, CSV, JSON file, or web page.
            </span>
          </div>
        ) : (
          <div className="deliverables-workspace">
            <section className="deliverables-list-panel" aria-label="Conversation outputs">
              <div className="deliverables-section-heading">
                <div>
                  <h2>Files</h2>
                  <p>
                    {catalog.deliverables.length}{" "}
                    {catalog.deliverables.length === 1 ? "output" : "outputs"}
                  </p>
                  {catalog.truncated && <p>Showing the newest 100 outputs.</p>}
                </div>
              </div>
              <div className="deliverables-list">
                {catalog.deliverables.map((item) => (
                  <button
                    type="button"
                    className={`deliverable-row${selected === item.filename ? " is-selected" : ""}`}
                    aria-current={selected === item.filename ? "true" : undefined}
                    key={item.filename}
                    onClick={() => setSelected(item.filename)}
                  >
                    <span className="deliverable-icon" aria-hidden="true">
                      <FileOutput size={16} />
                    </span>
                    <span className="deliverable-copy">
                      <strong>{item.filename}</strong>
                      <small>
                        {formatBytes(item.sizeBytes)} · {formatDate(item.updatedAt)}
                      </small>
                    </span>
                  </button>
                ))}
              </div>
            </section>

            <section className="deliverable-preview-panel" aria-label="Output preview">
              <div className="deliverable-preview-heading">
                <div>
                  <h2>{selected ?? "Preview"}</h2>
                  {preview && <p>{mediaTypeLabel(preview.mediaType)}</p>}
                </div>
                <button
                  type="button"
                  className="btn btn-primary"
                  disabled={!preview || exporting}
                  onClick={() => void onExport()}
                >
                  <Download size={14} aria-hidden="true" />
                  {exporting ? "Saving…" : "Save As…"}
                </button>
              </div>
              {previewLoading ? (
                <div className="deliverable-preview-state" role="status">
                  Loading preview…
                </div>
              ) : previewError ? (
                <div className="deliverable-preview-state is-error" role="alert">
                  {previewError}
                </div>
              ) : preview ? (
                <div className="deliverable-preview">
                  {preview.mediaType === "text/markdown" ? (
                    <MessageMarkdown>{preview.content}</MessageMarkdown>
                  ) : (
                    <pre>{preview.content}</pre>
                  )}
                  {preview.truncated && (
                    <p className="deliverable-preview-truncated">
                      Preview truncated. Saving exports the complete file.
                    </p>
                  )}
                </div>
              ) : null}
              {exportStatus && (
                <p
                  className={`deliverable-export-status${exportStatus.error ? " is-error" : ""}`}
                  role={exportStatus.error ? "alert" : "status"}
                >
                  {exportStatus.message}
                </p>
              )}
            </section>
          </div>
        )}
      </div>
    </section>
  );
}

function formatBytes(bytes: number): string {
  if (bytes < 1_000) return `${bytes} B`;
  return `${(bytes / 1_000).toFixed(bytes < 10_000 ? 1 : 0)} KB`;
}

function formatDate(value: string): string {
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  }).format(new Date(value));
}

function mediaTypeLabel(mediaType: DeliverablePreview["mediaType"]): string {
  switch (mediaType) {
    case "text/markdown":
      return "Markdown";
    case "text/csv":
      return "CSV";
    case "application/json":
      return "JSON";
    case "text/html":
      return "HTML";
    default:
      return "Plain text";
  }
}

function friendlyOutputError(error: unknown, fallback: string): string {
  const message = String(error).replace(/^Error:\s*/, "").trim();
  return message && message.length <= 240 ? message : fallback;
}
