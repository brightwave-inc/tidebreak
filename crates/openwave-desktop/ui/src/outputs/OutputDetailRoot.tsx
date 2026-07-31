import { formatDistanceToNow } from "date-fns";
import { ArrowLeftIcon, DownloadIcon, HistoryIcon, RotateCcwIcon } from "lucide-react";
import { useEffect, useState } from "react";

import { PanelBreadcrumb } from "@/components/PanelHeader";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { WithTooltip } from "@/components/ui/tooltip";
import {
  exportDeliverable,
  listOutputRevisions,
  readDeliverable,
  readOutputRevision,
  restoreOutputRevision,
  type DeliverablePreview,
  type DeliverableSummary,
  type OutputExportResult,
  type OutputRevisionInfo,
  type OutputRevisionsCatalog,
} from "@/deliverables";
import {
  PICKER_BUSY_MESSAGE,
  PICKER_HOLDERS,
  useNativePickerLatch,
} from "@/NativePickerLatch";
import { PanelFrame } from "@/panel/PanelFrame";
import type { PanelPosition } from "@/panel/panelTypes";
import { usePanelNav } from "@/panel/usePanelNav";
import { OutputContent } from "./OutputContent";
import { outputTypeLabel } from "./outputFormat";
import { exportFailureMessage, friendlyOutputError } from "./OutputsView";

export type OutputDetailApis = {
  read: (chatId: string, outputId: string) => Promise<DeliverablePreview>;
  export: (chatId: string, outputId: string) => Promise<OutputExportResult>;
  listRevisions: (chatId: string, outputId: string) => Promise<OutputRevisionsCatalog>;
  readRevision: (
    chatId: string,
    outputId: string,
    revisionId: string,
  ) => Promise<DeliverablePreview>;
  restoreRevision: (
    chatId: string,
    outputId: string,
    revisionId: string,
  ) => Promise<DeliverableSummary>;
};

const defaultApis: OutputDetailApis = {
  read: readDeliverable,
  export: exportDeliverable,
  listRevisions: listOutputRevisions,
  readRevision: readOutputRevision,
  restoreRevision: restoreOutputRevision,
};

function producedByLabel(revision: OutputRevisionInfo): string {
  switch (revision.producedBy) {
    case "agent":
      return "Agent";
    case "backgroundAgent":
      return "Background agent";
    case "user":
      return "You";
  }
}

/**
 * One output, opened in a panel addressed as `outputs.{outputId}`.
 *
 * The address is the selection: this panel reads the output it was asked for and
 * nothing else. That is what replaced the list's old habit of steering its own
 * selection toward whatever it had been pointed at, then having to stop steering
 * once the reader chose for themselves.
 *
 * Version history is append-only: viewing an old version is a preview, and
 * restoring one appends a new version with that content — nothing is rewound.
 */
export function OutputDetailRoot({
  chatId,
  outputId,
  position,
  apis = defaultApis,
}: {
  chatId: string;
  outputId: string;
  position: PanelPosition;
  apis?: OutputDetailApis;
}) {
  const { openPanel } = usePanelNav();
  const [preview, setPreview] = useState<DeliverablePreview | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [restoring, setRestoring] = useState(false);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [revisions, setRevisions] = useState<OutputRevisionInfo[] | null>(null);
  /** The non-current version being previewed, if any. */
  const [previewRevision, setPreviewRevision] = useState<OutputRevisionInfo | null>(null);
  const [revisionPreview, setRevisionPreview] = useState<DeliverablePreview | null>(null);
  const [saveStatus, setSaveStatus] = useState<{
    message: string;
    error: boolean;
  } | null>(null);

  useEffect(() => {
    let cancelled = false;
    setPreview(null);
    setLoadError(null);
    setSaveStatus(null);
    setRevisions(null);
    setPreviewRevision(null);
    setRevisionPreview(null);
    setHistoryOpen(false);
    void apis
      .read(chatId, outputId)
      .then((next) => {
        if (!cancelled) setPreview(next);
      })
      .catch((caught) => {
        if (!cancelled) {
          setLoadError(friendlyOutputError(caught, "Could not preview that output."));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [apis, chatId, outputId]);

  async function onSave() {
    if (saving) return;
    if (!useNativePickerLatch.getState().claim(PICKER_HOLDERS.exportOutput)) {
      setSaveStatus({ message: PICKER_BUSY_MESSAGE, error: true });
      return;
    }
    setSaving(true);
    setSaveStatus(null);
    try {
      const result = await apis.export(chatId, outputId);
      const filename = preview?.filename ?? "Output";
      if (result.status === "completed") {
        setSaveStatus({ message: `${filename} was saved.`, error: false });
      } else if (result.status === "failed") {
        setSaveStatus({ message: exportFailureMessage(result.reason), error: true });
      }
    } catch (caught) {
      setSaveStatus({
        message: friendlyOutputError(caught, "Could not save that output."),
        error: true,
      });
    } finally {
      useNativePickerLatch.getState().release(PICKER_HOLDERS.exportOutput);
      setSaving(false);
    }
  }

  async function onHistoryOpenChange(open: boolean) {
    setHistoryOpen(open);
    if (!open) return;
    try {
      const catalog = await apis.listRevisions(chatId, outputId);
      setRevisions(catalog.revisions);
    } catch (caught) {
      setHistoryOpen(false);
      setSaveStatus({
        message: friendlyOutputError(caught, "Could not load this output's versions."),
        error: true,
      });
    }
  }

  async function onViewRevision(revision: OutputRevisionInfo) {
    setHistoryOpen(false);
    if (revision.isCurrent) {
      setPreviewRevision(null);
      setRevisionPreview(null);
      return;
    }
    setPreviewRevision(revision);
    setRevisionPreview(null);
    try {
      const content = await apis.readRevision(chatId, outputId, revision.revisionId);
      setRevisionPreview(content);
    } catch (caught) {
      setPreviewRevision(null);
      setSaveStatus({
        message: friendlyOutputError(caught, "Could not preview that version."),
        error: true,
      });
    }
  }

  async function onRestore() {
    if (!previewRevision || restoring) return;
    setRestoring(true);
    setSaveStatus(null);
    try {
      await apis.restoreRevision(chatId, outputId, previewRevision.revisionId);
      const restoredOrdinal = previewRevision.ordinal;
      setPreviewRevision(null);
      setRevisionPreview(null);
      setRevisions(null);
      const next = await apis.read(chatId, outputId);
      setPreview(next);
      setSaveStatus({
        message: `Restored version ${restoredOrdinal} as the latest version.`,
        error: false,
      });
    } catch (caught) {
      setSaveStatus({
        message: friendlyOutputError(caught, "Could not restore that version."),
        error: true,
      });
    } finally {
      setRestoring(false);
    }
  }

  const viewing = previewRevision ? revisionPreview : preview;
  const showHistory = (preview?.revisionCount ?? 0) > 1;

  return (
    <PanelFrame
      position={position}
      showBorder
      breadcrumb={
        <PanelBreadcrumb
          firstPart={
            <button
              type="button"
              className="cursor-pointer hover:underline"
              onClick={() => openPanel({ type: "outputs" })}
            >
              Outputs
            </button>
          }
          currentItem={preview?.filename}
        />
      }
      headerRightSlot={
        <div className="flex items-center gap-1">
          {showHistory && (
            <Popover
              open={historyOpen}
              onOpenChange={(open) => void onHistoryOpenChange(open)}
            >
              <WithTooltip label="Version history">
                <PopoverTrigger asChild>
                  <Button variant="ghost" size="icon-sm">
                    <HistoryIcon className="size-4" />
                    <span className="sr-only">Version history</span>
                  </Button>
                </PopoverTrigger>
              </WithTooltip>
              <PopoverContent align="end" className="w-64 p-2">
                <p className="px-2 pt-1 pb-2 text-sm font-medium">Version history</p>
                {revisions === null ? (
                  <p className="px-2 pb-2 text-sm text-muted-foreground" role="status">
                    Loading versions…
                  </p>
                ) : (
                  <ul className="max-h-72 overflow-y-auto">
                    {revisions.map((revision) => (
                      <li key={revision.revisionId}>
                        <button
                          type="button"
                          className="group flex w-full cursor-pointer items-center justify-between gap-2 rounded-md px-2 py-1.5 text-left hover:bg-accent"
                          onClick={() => void onViewRevision(revision)}
                        >
                          <span className="min-w-0">
                            <span className="flex items-center gap-2 text-sm">
                              v{revision.ordinal}
                              {revision.isCurrent && (
                                <Badge variant="outline" size="sm">
                                  Current version
                                </Badge>
                              )}
                            </span>
                            <span className="block truncate text-xs text-muted-foreground">
                              {producedByLabel(revision)} ·{" "}
                              {formatDistanceToNow(new Date(revision.createdAt))} ago
                            </span>
                          </span>
                          {!revision.isCurrent && (
                            <RotateCcwIcon className="size-3.5 shrink-0 text-muted-foreground opacity-0 group-hover:opacity-100" />
                          )}
                        </button>
                      </li>
                    ))}
                  </ul>
                )}
              </PopoverContent>
            </Popover>
          )}
          <WithTooltip label="Save as…">
            <Button
              variant="ghost"
              size="icon-sm"
              disabled={!preview || saving || previewRevision !== null}
              onClick={() => void onSave()}
            >
              <DownloadIcon className="size-4" />
              <span className="sr-only">Save as…</span>
            </Button>
          </WithTooltip>
        </div>
      }
    >
      {previewRevision && (
        <div
          className="mx-6 mt-3 flex shrink-0 flex-wrap items-center gap-2 rounded-md bg-info-background px-3 py-2 text-sm text-info-foreground-muted"
          role="status"
        >
          <HistoryIcon className="size-4 shrink-0" />
          <span className="min-w-0 flex-1 truncate">
            Viewing v{previewRevision.ordinal} — {preview?.filename ?? "Output"}
          </span>
          <Button
            variant="outline"
            size="xs"
            className="shrink-0"
            disabled={restoring}
            onClick={() => void onRestore()}
          >
            <RotateCcwIcon className="size-3.5" />
            Restore this version
          </Button>
          <Button
            variant="ghost"
            size="xs"
            className="shrink-0"
            onClick={() => {
              setPreviewRevision(null);
              setRevisionPreview(null);
            }}
          >
            <ArrowLeftIcon className="size-3.5" />
            Back to latest
          </Button>
        </div>
      )}
      {loadError ? (
        <p className="p-6 text-sm text-critical" role="alert">
          {loadError}
        </p>
      ) : viewing ? (
        <>
          <div className="shrink-0 px-6 pt-4 text-xs text-muted-foreground">
            {outputTypeLabel(viewing.mediaType)}
          </div>
          <OutputContent chatId={chatId} preview={viewing} />
        </>
      ) : (
        <p className="p-6 text-sm text-muted-foreground" role="status">
          {previewRevision ? "Loading that version…" : "Loading this output…"}
        </p>
      )}
      {saveStatus && (
        <p
          className={
            saveStatus.error
              ? "shrink-0 px-6 pb-2 text-sm text-critical"
              : "shrink-0 px-6 pb-2 text-sm text-muted-foreground"
          }
          role={saveStatus.error ? "alert" : "status"}
        >
          {saveStatus.message}
        </p>
      )}
    </PanelFrame>
  );
}
