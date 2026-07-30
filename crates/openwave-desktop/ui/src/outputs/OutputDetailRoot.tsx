import { DownloadIcon, Undo2Icon } from "lucide-react";
import { useEffect, useState } from "react";

import { PanelBreadcrumb } from "@/components/PanelHeader";
import { Button } from "@/components/ui/button";
import { WithTooltip } from "@/components/ui/tooltip";
import {
  exportDeliverable,
  isTextDeliverableMediaType,
  readDeliverable,
  revertOutput,
  type DeliverablePreview,
  type OutputExportResult,
  type OutputRevertResult,
} from "@/deliverables";
import { MessageMarkdown } from "@/MessageMarkdown";
import {
  PICKER_BUSY_MESSAGE,
  PICKER_HOLDERS,
  useNativePickerLatch,
} from "@/NativePickerLatch";
import { PanelFrame } from "@/panel/PanelFrame";
import type { PanelPosition } from "@/panel/panelTypes";
import { usePanelNav } from "@/panel/usePanelNav";
import { outputTypeLabel } from "./outputFormat";
import { exportFailureMessage, friendlyOutputError } from "./OutputsView";

export type OutputDetailApis = {
  read: (chatId: string, outputId: string) => Promise<DeliverablePreview>;
  export: (chatId: string, outputId: string) => Promise<OutputExportResult>;
  revert: (chatId: string, outputId: string) => Promise<OutputRevertResult>;
};

const defaultApis: OutputDetailApis = {
  read: readDeliverable,
  export: exportDeliverable,
  revert: revertOutput,
};

/**
 * One output, opened in a panel addressed as `outputs.{outputId}`.
 *
 * The address is the selection: this panel reads the output it was asked for and
 * nothing else. That is what replaced the list's old habit of steering its own
 * selection toward whatever it had been pointed at, then having to stop steering
 * once the reader chose for themselves.
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
  const [reverting, setReverting] = useState(false);
  const [saveStatus, setSaveStatus] = useState<{
    message: string;
    error: boolean;
  } | null>(null);

  useEffect(() => {
    let cancelled = false;
    setPreview(null);
    setLoadError(null);
    setSaveStatus(null);
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

  async function onRevert() {
    if (reverting) return;
    setReverting(true);
    setSaveStatus(null);
    try {
      const result = await apis.revert(chatId, outputId);
      if (result.status === "retracted") {
        // The output no longer exists in the conversation; there is nothing left
        // to preview here, so return to the catalog where Undo is offered.
        openPanel({ type: "outputs" });
        return;
      }
      const next = await apis.read(chatId, outputId);
      setPreview(next);
      setSaveStatus({
        message: "Reverted to the previous version.",
        error: false,
      });
    } catch (caught) {
      setSaveStatus({
        message: friendlyOutputError(caught, "Could not revert that output."),
        error: true,
      });
    } finally {
      setReverting(false);
    }
  }

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
          <WithTooltip label="Revert">
            <Button
              variant="ghost"
              size="icon-sm"
              disabled={!preview || reverting}
              onClick={() => void onRevert()}
            >
              <Undo2Icon className="size-4" />
              <span className="sr-only">Revert</span>
            </Button>
          </WithTooltip>
          <WithTooltip label="Save as…">
            <Button
              variant="ghost"
              size="icon-sm"
              disabled={!preview || saving}
              onClick={() => void onSave()}
            >
              <DownloadIcon className="size-4" />
              <span className="sr-only">Save as…</span>
            </Button>
          </WithTooltip>
        </div>
      }
    >
      {loadError ? (
        <p className="p-6 text-sm text-critical" role="alert">
          {loadError}
        </p>
      ) : preview ? (
        <>
          <div className="shrink-0 px-6 pt-4 text-xs text-muted-foreground">
            {outputTypeLabel(preview.mediaType)}
          </div>
          <div className="min-h-0 flex-1 overflow-auto p-6">
            <div className="mx-auto max-w-4xl">
              {!isTextDeliverableMediaType(preview.mediaType) ? (
                <p className="text-sm text-muted-foreground" role="status">
                  No preview for this file type. Save as… exports the file.
                </p>
              ) : preview.mediaType === "text/markdown" ? (
                <MessageMarkdown>{preview.content}</MessageMarkdown>
              ) : (
                <pre className="font-mono text-xs break-words whitespace-pre-wrap">
                  {preview.content}
                </pre>
              )}
              {preview.truncated && (
                <p className="mt-6 text-xs text-muted-foreground">
                  Preview truncated. Saving writes the complete file.
                </p>
              )}
            </div>
          </div>
        </>
      ) : (
        <p className="p-6 text-sm text-muted-foreground" role="status">
          Loading this output…
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
