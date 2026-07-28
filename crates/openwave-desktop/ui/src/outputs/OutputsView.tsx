import { FileOutputIcon, RotateCwIcon } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";

import { PanelSecondaryHeader } from "@/components/PanelHeader";
import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { WithTooltip } from "@/components/ui/tooltip";
import {
  exportDeliverable,
  listDeliverables,
  type DeliverableSummary,
  type DeliverablesCatalog,
  type OutputExportResult,
} from "@/deliverables";
import {
  PICKER_BUSY_MESSAGE,
  PICKER_HOLDERS,
  useNativePickerLatch,
} from "@/NativePickerLatch";
import { OutputsTable } from "./OutputsTable";

export type OutputsApis = {
  list: (chatId: string) => Promise<DeliverablesCatalog>;
  export: (chatId: string, outputId: string) => Promise<OutputExportResult>;
};

const defaultApis: OutputsApis = {
  list: listDeliverables,
  export: exportDeliverable,
};

/**
 * A conversation's outputs, as the panel addressed `outputs`.
 *
 * The list and the reader are two panels rather than one split, so an output is
 * addressable: `outputs.{outputId}` opens it directly, which is what makes a
 * link from the transcript land somewhere. Saving is offered from both — from
 * here because a reader clearing a batch out to disk should not have to open
 * each one first.
 */
export function OutputsView({
  chatId,
  onOpen = () => {},
  apis = defaultApis,
}: {
  chatId: string;
  /** Navigate to the `outputs.{outputId}` panel contract. */
  onOpen?: (outputId: string) => void;
  apis?: OutputsApis;
}) {
  const [catalog, setCatalog] = useState<DeliverablesCatalog>({
    deliverables: [],
    truncated: false,
  });
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busyOutputId, setBusyOutputId] = useState<string | null>(null);
  const [saveStatus, setSaveStatus] = useState<{
    message: string;
    error: boolean;
  } | null>(null);
  const [countSuffix, setCountSuffix] = useState("");
  const generationRef = useRef(0);

  async function refresh(showLoading = false) {
    const generation = ++generationRef.current;
    if (showLoading) setLoading(true);
    setError(null);
    try {
      const next = await apis.list(chatId);
      if (generation !== generationRef.current) return;
      setCatalog(next);
    } catch (caught) {
      if (generation !== generationRef.current) return;
      setError(friendlyOutputError(caught, "Could not load this conversation's outputs."));
    } finally {
      if (generation === generationRef.current) setLoading(false);
    }
  }

  useEffect(() => {
    setCatalog({ deliverables: [], truncated: false });
    setError(null);
    setSaveStatus(null);
    setBusyOutputId(null);
    void refresh(true);
    return () => {
      generationRef.current += 1;
    };
  }, [chatId, apis]);

  const onSave = useCallback(
    async (output: DeliverableSummary) => {
      if (busyOutputId) return;
      if (!useNativePickerLatch.getState().claim(PICKER_HOLDERS.exportOutput)) {
        setSaveStatus({ message: PICKER_BUSY_MESSAGE, error: true });
        return;
      }
      setBusyOutputId(output.outputId);
      setSaveStatus(null);
      try {
        const result = await apis.export(chatId, output.outputId);
        if (result.status === "completed") {
          setSaveStatus({ message: `${output.filename} was saved.`, error: false });
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
        setBusyOutputId(null);
      }
    },
    [apis, chatId, busyOutputId],
  );

  const hasOutputs = catalog.deliverables.length > 0;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <PanelSecondaryHeader showBorder={false} className="pr-1 pl-4">
        <div className="flex items-baseline gap-3">
          <h1 className="text-lg font-medium">Outputs</h1>
          {hasOutputs && (
            <span className="text-lg font-medium text-muted-foreground">{countSuffix}</span>
          )}
        </div>
        <span className="grow" />
        <div className="pr-2">
          <WithTooltip label="Refresh">
            <Button
              variant="ghost"
              size="icon-sm"
              disabled={loading}
              onClick={() => void refresh(true)}
            >
              <RotateCwIcon className="size-4" />
              <span className="sr-only">Refresh</span>
            </Button>
          </WithTooltip>
        </div>
      </PanelSecondaryHeader>

      <div className="flex min-h-0 flex-1 flex-col gap-2 pt-4">
        {saveStatus && (
          <div
            className={
              saveStatus.error
                ? "mx-4 shrink-0 rounded-md bg-critical-background px-3 py-2 text-sm text-critical-foreground-muted"
                : "mx-4 shrink-0 rounded-md bg-info-background px-3 py-2 text-sm text-info-foreground-muted"
            }
            role={saveStatus.error ? "alert" : "status"}
          >
            {saveStatus.message}
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
              onClick={() => void refresh(true)}
            >
              Try again
            </Button>
          </div>
        )}
        {catalog.truncated && (
          <p className="shrink-0 px-4 text-xs text-muted-foreground">
            Showing the newest 100 outputs.
          </p>
        )}

        {loading && !hasOutputs ? (
          <p className="px-4 text-sm text-muted-foreground" role="status">
            Loading outputs for this conversation…
          </p>
        ) : !hasOutputs ? (
          <Empty>
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <FileOutputIcon />
              </EmptyMedia>
              <EmptyTitle>No outputs yet</EmptyTitle>
              <EmptyDescription>
                Ask OpenWave to write a report, a plan, a CSV, a JSON file or a web
                page. What it creates stays here until you choose where to save it.
              </EmptyDescription>
            </EmptyHeader>
          </Empty>
        ) : (
          <OutputsTable
            outputs={catalog.deliverables}
            busyOutputId={busyOutputId}
            onOpen={onOpen}
            onSave={(output) => void onSave(output)}
            onCountChange={setCountSuffix}
          />
        )}
      </div>
    </div>
  );
}

export function exportFailureMessage(
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

export function friendlyOutputError(error: unknown, fallback: string): string {
  const message = String(error).replace(/^Error:\s*/, "").trim();
  return message && message.length <= 240 ? message : fallback;
}
