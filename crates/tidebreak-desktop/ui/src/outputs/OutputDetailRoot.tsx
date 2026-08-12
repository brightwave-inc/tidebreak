import { formatDistanceToNow } from "date-fns";
import {
  ArrowLeftIcon,
  DownloadIcon,
  HistoryIcon,
  PencilIcon,
  RotateCcwIcon,
} from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";

import { useConfirm } from "@/components/ConfirmDialog";
import { PanelBreadcrumb } from "@/components/PanelHeader";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Textarea } from "@/components/ui/textarea";
import { WithTooltip } from "@/components/ui/tooltip";
import {
  exportDeliverable,
  isEditableTextMediaType,
  listOutputRevisions,
  readDeliverable,
  readOutputRevision,
  restoreOutputRevision,
  saveOutputRevision,
  type DeliverablePreview,
  type DeliverableSummary,
  type OutputExportResult,
  type OutputRevisionInfo,
  type OutputRevisionsCatalog,
  type SaveOutputRevisionResult,
} from "@/deliverables";
import {
  PICKER_BUSY_MESSAGE,
  PICKER_HOLDERS,
  useNativePickerLatch,
} from "@/NativePickerLatch";
import { PanelFrame } from "@/panel/PanelFrame";
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
  save: (
    chatId: string,
    outputId: string,
    expectedRevisionId: string,
    content: string,
  ) => Promise<SaveOutputRevisionResult>;
};

const defaultApis: OutputDetailApis = {
  read: readDeliverable,
  export: exportDeliverable,
  listRevisions: listOutputRevisions,
  readRevision: readOutputRevision,
  restoreRevision: restoreOutputRevision,
  save: saveOutputRevision,
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
 * Editing works the same way: Save publishes a new user-authored version rather
 * than rewriting the one on screen, and it is conditional on that version still
 * being current, so an edit can never land on top of something it never saw.
 */
export function OutputDetailRoot({
  chatId,
  outputId,
  apis = defaultApis,
}: {
  chatId: string;
  outputId: string;
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
  /** The open editor: the revision it started from, plus the working draft. */
  const [editor, setEditor] = useState<{
    baseRevisionId: string;
    draft: string;
  } | null>(null);
  const [savingEdit, setSavingEdit] = useState(false);
  /** Set when a save was refused because a newer version became current. */
  const [editConflict, setEditConflict] = useState(false);
  const { confirm, dialog: confirmDialog } = useConfirm();

  useEffect(() => {
    let cancelled = false;
    setPreview(null);
    setLoadError(null);
    setRevisions(null);
    setPreviewRevision(null);
    setRevisionPreview(null);
    setHistoryOpen(false);
    setEditor(null);
    setEditConflict(false);
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
      toast.error(PICKER_BUSY_MESSAGE);
      return;
    }
    setSaving(true);
    try {
      const result = await apis.export(chatId, outputId);
      const filename = preview?.filename ?? "Output";
      if (result.status === "completed") {
        toast.success(`${filename} was saved.`);
      } else if (result.status === "failed") {
        toast.error(exportFailureMessage(result.reason));
      }
    } catch (caught) {
      toast.error(friendlyOutputError(caught, "Could not save that output."));
    } finally {
      useNativePickerLatch.getState().release(PICKER_HOLDERS.exportOutput);
      setSaving(false);
    }
  }

  const dirty = editor !== null && editor.draft !== (preview?.content ?? "");

  /** Leave the editor, asking first when the draft holds unsaved changes. */
  async function leaveEditor(): Promise<boolean> {
    if (!editor) return true;
    if (
      dirty &&
      !(await confirm({
        title: "Discard your changes?",
        description: "Your edits to this output have not been saved.",
        confirmLabel: "Discard",
        destructive: true,
      }))
    ) {
      return false;
    }
    setEditor(null);
    setEditConflict(false);
    return true;
  }

  async function onSaveEdit() {
    if (!editor || savingEdit) return;
    setSavingEdit(true);
    try {
      const result = await apis.save(
        chatId,
        outputId,
        editor.baseRevisionId,
        editor.draft,
      );
      if (result.status === "conflict") {
        // Nothing was written. The draft stays on screen so the reader can
        // copy from it before taking the newer version.
        setEditConflict(true);
        return;
      }
      setPreview(result.preview);
      setRevisions(null);
      setEditor(null);
      setEditConflict(false);
      toast.success(
        `Saved as version ${result.preview.revisionCount} of ${result.preview.filename}.`,
      );
    } catch (caught) {
      toast.error(friendlyOutputError(caught, "Could not save your changes."));
    } finally {
      setSavingEdit(false);
    }
  }

  /** The reconcile path out of a conflict: take the version that won. */
  async function onReloadLatest() {
    if (
      !(await confirm({
        title: "Replace your text with the latest version?",
        description:
          "Your unsaved edit will be lost. Copy anything you still need first.",
        confirmLabel: "Reload latest",
        destructive: true,
      }))
    ) {
      return;
    }
    try {
      const next = await apis.read(chatId, outputId);
      setPreview(next);
      setRevisions(null);
      setEditConflict(false);
      setEditor(
        next.truncated || !isEditableTextMediaType(next.mediaType)
          ? null
          : { baseRevisionId: next.revisionId, draft: next.content },
      );
    } catch (caught) {
      toast.error(
        friendlyOutputError(caught, "Could not load the latest version."),
      );
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
      toast.error(
        friendlyOutputError(caught, "Could not load this output's versions."),
      );
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
      toast.error(friendlyOutputError(caught, "Could not preview that version."));
    }
  }

  async function onRestore() {
    if (!previewRevision || restoring) return;
    setRestoring(true);
    try {
      await apis.restoreRevision(chatId, outputId, previewRevision.revisionId);
      const restoredOrdinal = previewRevision.ordinal;
      setPreviewRevision(null);
      setRevisionPreview(null);
      setRevisions(null);
      const next = await apis.read(chatId, outputId);
      setPreview(next);
      toast.success(`Restored version ${restoredOrdinal} as the latest version.`);
    } catch (caught) {
      toast.error(friendlyOutputError(caught, "Could not restore that version."));
    } finally {
      setRestoring(false);
    }
  }

  const viewing = previewRevision ? revisionPreview : preview;
  const showHistory = (preview?.revisionCount ?? 0) > 1;
  // A truncated preview is not the whole file, so editing it would quietly drop
  // whatever the preview left out. Save As… still exports the complete file.
  const canEdit =
    preview !== null &&
    previewRevision === null &&
    !preview.truncated &&
    isEditableTextMediaType(preview.mediaType);

  return (
    <PanelFrame
      showBorder
      breadcrumb={
        <PanelBreadcrumb
          firstPart={
            <button
              type="button"
              className="cursor-pointer hover:underline"
              onClick={() => {
                void leaveEditor().then((left) => {
                  if (left) openPanel({ type: "outputs" });
                });
              }}
            >
              Outputs
            </button>
          }
          currentItem={preview?.filename}
        />
      }
      headerRightSlot={
        editor ? (
          <div className="flex items-center gap-2">
            <Button
              variant="ghost"
              size="xs"
              disabled={savingEdit}
              onClick={() => void leaveEditor()}
            >
              Cancel
            </Button>
            <Button
              size="xs"
              disabled={savingEdit || !dirty}
              onClick={() => void onSaveEdit()}
            >
              {savingEdit ? "Saving…" : "Save"}
            </Button>
          </div>
        ) : (
          <div className="flex items-center gap-1">
            {canEdit && (
              <WithTooltip label="Edit">
                <Button
                  variant="ghost"
                  size="icon-sm"
                  onClick={() =>
                    setEditor({
                      baseRevisionId: preview.revisionId,
                      draft: preview.content,
                    })
                  }
                >
                  <PencilIcon className="size-4" />
                  <span className="sr-only">Edit</span>
                </Button>
              </WithTooltip>
            )}
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
        )
      }
    >
      {confirmDialog}
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
      {editConflict && (
        <div
          className="mx-6 mt-3 flex shrink-0 flex-wrap items-center gap-2 rounded-md bg-critical-background px-3 py-2 text-sm text-critical-foreground-muted"
          role="alert"
        >
          <span className="min-w-0 flex-1">
            A newer version of this output was published while you were editing,
            so nothing was saved. Reload it to work from the latest text.
          </span>
          <Button
            variant="outline"
            size="xs"
            className="shrink-0"
            onClick={() => void onReloadLatest()}
          >
            Reload latest version
          </Button>
        </div>
      )}
      {loadError ? (
        <p className="p-6 text-sm text-critical" role="alert">
          {loadError}
        </p>
      ) : editor ? (
        <div className="flex min-h-0 flex-1 flex-col gap-2 p-6">
          <Textarea
            aria-label={`Edit ${preview?.filename ?? "output"}`}
            className="min-h-0 flex-1 resize-none font-mono text-sm"
            spellCheck={false}
            value={editor.draft}
            onChange={(event) => {
              setEditor({ ...editor, draft: event.target.value });
              setEditConflict(false);
            }}
          />
          <p className="shrink-0 text-xs text-muted-foreground">
            Saving publishes a new version. Earlier versions stay in this
            output's history.
          </p>
        </div>
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
    </PanelFrame>
  );
}
