import { useEffect, useRef, useState } from "react";
import { Download, FileOutput, RefreshCw } from "lucide-react";
import {
  exportDeliverable,
  listDeliverables,
  readDeliverable,
  type DeliverablePreview,
  type DeliverablesCatalog,
  type OutputExportResult,
} from "./deliverables";
import { documentIcon } from "./documentIcon";
import { MessageMarkdown } from "./MessageMarkdown";
import {
  PICKER_BUSY_MESSAGE,
  PICKER_HOLDERS,
  useNativePickerLatch,
} from "./NativePickerLatch";

type DeliverableApis = {
  list: (chatId: string) => Promise<DeliverablesCatalog>;
  read: (chatId: string, outputId: string) => Promise<DeliverablePreview>;
  export: (chatId: string, outputId: string) => Promise<OutputExportResult>;
};

const defaultApis: DeliverableApis = {
  list: listDeliverables,
  read: readDeliverable,
  export: exportDeliverable,
};

export function DeliverablesView({
  chatId,
  initialOutputId,
  apis = defaultApis,
}: {
  chatId: string;
  /** Opaque output to preview on arrival; ignored when this chat does not own it. */
  initialOutputId?: string;
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
  const pendingOutputIdRef = useRef<string | null>(initialOutputId ?? null);

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
      const target = pendingOutputIdRef.current;
      pendingOutputIdRef.current = null;
      const owns = (outputId: string | null | undefined) =>
        !!outputId && next.deliverables.some((item) => item.outputId === outputId);
      setCatalog(next);
      setSelected((current) =>
        owns(target)
          ? target
          : owns(current)
            ? current
            : (next.deliverables[0]?.outputId ?? null),
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
    // The requested opaque identity belongs to the conversation that asked for it. Its
    // only other clear sits after the stale-generation bail, which a chat switch
    // always takes — so without resetting here the next conversation opens on
    // the file the previous one wanted.
    pendingOutputIdRef.current = initialOutputId ?? null;
    void refresh(true);
    return () => {
      catalogGenerationRef.current += 1;
      previewGenerationRef.current += 1;
    };
  }, [chatId, apis]);

  // Being pointed somewhere new while the surface is already open resolves
  // against the catalog in hand, and otherwise waits for the next one.
  useEffect(() => {
    if (!initialOutputId) return;
    if (catalog.deliverables.some((item) => item.outputId === initialOutputId)) {
      pendingOutputIdRef.current = null;
      setSelected(initialOutputId);
    } else {
      pendingOutputIdRef.current = initialOutputId;
    }
    // `catalog` is read here, so it belongs in the dependencies: a target set
    // before the list arrived has to re-resolve when it does, by any path and
    // not only through an explicit refresh.
  }, [initialOutputId, catalog]);

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
    if (!useNativePickerLatch.getState().claim(PICKER_HOLDERS.exportOutput)) {
      setExportStatus({ message: PICKER_BUSY_MESSAGE, error: true });
      return;
    }
    setExporting(true);
    setExportStatus(null);
    try {
      const result = await apis.export(chatId, selected);
      const filename =
        catalog.deliverables.find((item) => item.outputId === selected)?.filename ??
        "Output";
      if (result.status === "completed") {
        setExportStatus({ message: `${filename} was saved.`, error: false });
      } else if (result.status === "failed") {
        setExportStatus({
          message: exportFailureMessage(result.reason),
          error: true,
        });
      }
    } catch (caught) {
      setExportStatus({
        message: friendlyOutputError(caught, "Could not save that output."),
        error: true,
      });
    } finally {
      useNativePickerLatch.getState().release(PICKER_HOLDERS.exportOutput);
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
                {catalog.deliverables.map((item) => {
                  const Icon = documentIcon(item.mediaType);
                  return (
                  <button
                    type="button"
                    className={`deliverable-row${selected === item.outputId ? " is-selected" : ""}`}
                    aria-current={selected === item.outputId ? "true" : undefined}
                    key={item.outputId}
                    onClick={() => setSelected(item.outputId)}
                  >
                    <span className="deliverable-icon" aria-hidden="true">
                      <Icon size={16} />
                    </span>
                    <span className="deliverable-copy">
                      <strong>{item.filename}</strong>
                      <small>
                        {formatBytes(item.sizeBytes)} ·{" "}
                        {item.revisionCount === 1
                          ? "1 revision"
                          : `${item.revisionCount} revisions`}{" "}
                        · {formatDate(item.updatedAt)}
                      </small>
                    </span>
                  </button>
                  );
                })}
              </div>
            </section>

            <section className="deliverable-preview-panel" aria-label="Output preview">
              <div className="deliverable-preview-heading">
                <div>
                  <h2>{preview?.filename ?? "Preview"}</h2>
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

function exportFailureMessage(
  reason: Extract<OutputExportResult, { status: "failed" }>["reason"],
): string {
  switch (reason) {
    case "source_unavailable":
      return "That output revision is no longer available.";
    case "destination_unavailable":
      return "The selected save destination is no longer available.";
    case "ambiguous_native_failure":
      return "OpenWave could not confirm whether the output was saved. Check the selected destination before trying again.";
  }
}

function friendlyOutputError(error: unknown, fallback: string): string {
  const message = String(error).replace(/^Error:\s*/, "").trim();
  return message && message.length <= 240 ? message : fallback;
}
