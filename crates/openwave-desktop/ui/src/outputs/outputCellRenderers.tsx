import type { CustomCellRendererProps } from "ag-grid-react";
import { format } from "date-fns";
import { BotIcon, DownloadIcon, MoreHorizontalIcon, Undo2Icon } from "lucide-react";
import { useState } from "react";

import { DocumentIcon } from "@/components/document-table/DocumentIcon";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { WithTooltip } from "@/components/ui/tooltip";
import type { DeliverableSummary } from "@/deliverables";
import { formatBytes } from "@/lib/formatBytes";
import { outputTypeLabel, revisionLabel } from "./outputFormat";

/**
 * What each column of the outputs catalog draws.
 *
 * Per-row actions arrive through the grid's `context` rather than being closed
 * over at the call site, because the grid owns the rows.
 */
export type OutputGridContext = {
  onOpen: (outputId: string) => void;
  onSave: (output: DeliverableSummary) => void;
  /** Revert a merged output: step back a revision, or retract the merge. */
  onRevert: (output: DeliverableSummary) => void;
  /** The output with a save or revert in flight, whose actions are disabled. */
  busyOutputId: string | null;
};

type CellProps = CustomCellRendererProps<DeliverableSummary>;

function gridContext(props: CellProps): OutputGridContext {
  return props.context as OutputGridContext;
}

export function NameCellRenderer(props: CellProps) {
  const output = props.data!;
  const context = gridContext(props);

  return (
    <div className="flex h-full min-w-0 items-center">
      <button
        type="button"
        className="flex min-w-0 flex-1 cursor-pointer items-center gap-2 text-left text-sm underline-offset-2 hover:underline"
        onClick={() => context.onOpen(output.outputId)}
        aria-label={`Open ${output.filename}`}
      >
        <WithTooltip label={outputTypeLabel(output.mediaType)}>
          <span className="flex shrink-0 items-center">
            <DocumentIcon mediaType={output.mediaType} />
          </span>
        </WithTooltip>
        <span className="truncate">{output.filename}</span>
      </button>
      {output.producingRunId !== null && (
        <WithTooltip label="Auto-merged from a background agent">
          <span className="ml-2 flex shrink-0 items-center gap-1 rounded-full bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">
            <BotIcon className="size-3" />
            Agent
          </span>
        </WithTooltip>
      )}
    </div>
  );
}

export function TypeCellRenderer(props: CellProps) {
  return (
    <span className="text-sm text-foreground">
      {outputTypeLabel(props.data!.mediaType)}
    </span>
  );
}

export function SizeCellRenderer(props: CellProps) {
  return (
    <span className="text-sm tabular-nums text-foreground">
      {formatBytes(props.data!.sizeBytes)}
    </span>
  );
}

/**
 * Revisions: the count alone, since the column header says what it counts. An
 * output rewritten by a later turn is the same output, and this is the only
 * place that says how many times that happened.
 */
export function RevisionsCellRenderer(props: CellProps) {
  const output = props.data!;
  return (
    <WithTooltip label={revisionLabel(output)}>
      <span className="text-sm tabular-nums text-foreground">
        {output.revisionCount}
      </span>
    </WithTooltip>
  );
}

export function DateCellRenderer(props: CellProps) {
  const updatedAt = props.data!.updatedAt;
  const date = new Date(updatedAt);
  return (
    <WithTooltip label={format(date, "MMM dd, yyyy HH:mm")}>
      <time dateTime={updatedAt} className="text-sm text-foreground">
        {format(date, "MMM dd")}
      </time>
    </WithTooltip>
  );
}

export function ActionsCellRenderer(props: CellProps) {
  const output = props.data!;
  const context = gridContext(props);
  const [isMenuOpen, setIsMenuOpen] = useState(false);
  const busy = context.busyOutputId === output.outputId;

  return (
    <div className="flex items-center justify-center py-2">
      <DropdownMenu open={isMenuOpen} onOpenChange={setIsMenuOpen}>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="icon-xs"
            disabled={busy}
            className="shrink-0 text-muted-foreground hover:text-foreground"
          >
            <MoreHorizontalIcon />
            <span className="sr-only">More options for {output.filename}</span>
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent side="left" align="start">
          <DropdownMenuItem onClick={() => context.onOpen(output.outputId)}>
            <span>Open</span>
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => context.onSave(output)}>
            <DownloadIcon />
            <span>Save as…</span>
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem variant="destructive" onClick={() => context.onRevert(output)}>
            <Undo2Icon />
            <span>{output.revisionCount > 1 ? "Revert version" : "Revert"}</span>
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}
