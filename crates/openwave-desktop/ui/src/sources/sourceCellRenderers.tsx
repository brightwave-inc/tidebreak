import type { CustomCellRendererProps } from "ag-grid-react";
import { format } from "date-fns";
import { MoreHorizontalIcon } from "lucide-react";
import { useState } from "react";

import { DocumentIcon } from "@/components/document-table/DocumentIcon";
import { DocumentStatusPill } from "@/components/document/DocumentStatusPill";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { WithTooltip } from "@/components/ui/tooltip";
import type { LibraryDocument } from "@/documents";
import { documentTitle, formatSize, mediaTypeLabel } from "./sourceFormat";

/**
 * What each column of the source catalog draws.
 *
 * The grid owns the rows, so per-row actions cannot be closed over at the call
 * site the way they would be in plain JSX; they arrive through the grid's
 * `context`, which is what {@link SourceGridContext} describes.
 */
export type SourceGridContext = {
  onOpen: (documentId: string) => void;
  onDownload: (document: LibraryDocument) => void;
  onDelete: (document: LibraryDocument) => void;
  onRetry: (document: LibraryDocument) => void;
  canDownload: boolean;
  /** The source with an action in flight, whose row actions are disabled. */
  busyDocumentId: string | null;
};

type CellProps = CustomCellRendererProps<LibraryDocument>;

function gridContext(props: CellProps): SourceGridContext {
  return props.context as SourceGridContext;
}

/** Name: the glyph, the title as the way in, and the preparation state. */
export function NameCellRenderer(props: CellProps) {
  const document = props.data!;
  const context = gridContext(props);
  const title = documentTitle(document);

  return (
    <div className="flex h-full min-w-0 items-center justify-between">
      <button
        type="button"
        className="flex min-w-0 flex-1 cursor-pointer items-center gap-2 text-left text-sm underline-offset-2 hover:underline"
        onClick={() => context.onOpen(document.documentId)}
        aria-label={`Open ${title}`}
      >
        <WithTooltip label={mediaTypeLabel(document.mediaType)}>
          <span className="flex shrink-0 items-center">
            <DocumentIcon mediaType={document.mediaType} />
          </span>
        </WithTooltip>
        <span className="truncate">{title}</span>
      </button>
      <div
        className="ml-3 flex items-center gap-2"
        // The pill's popover and retry live inside a row the grid would
        // otherwise treat as a click on the row itself.
        onClick={(event) => event.stopPropagation()}
      >
        <DocumentStatusPill
          document={document}
          isRetryPending={context.busyDocumentId === document.documentId}
          onRetryClick={() => context.onRetry(document)}
        />
      </div>
    </div>
  );
}

export function TypeCellRenderer(props: CellProps) {
  return (
    <span className="text-sm text-foreground">{mediaTypeLabel(props.data!.mediaType)}</span>
  );
}

export function SizeCellRenderer(props: CellProps) {
  return (
    <span className="text-sm tabular-nums text-foreground">
      {formatSize(props.data!.sizeBytes)}
    </span>
  );
}

/** Date: short in the cell, full in the tooltip, machine-readable underneath. */
export function DateCellRenderer(props: CellProps) {
  const updatedAt = props.data!.updatedAt;
  const date = new Date(updatedAt);
  return (
    <WithTooltip label={format(date, "MMM dd, yyyy")}>
      <time dateTime={updatedAt} className="text-sm text-foreground">
        {format(date, "MMM dd")}
      </time>
    </WithTooltip>
  );
}

export function ActionsCellRenderer(props: CellProps) {
  const document = props.data!;
  const context = gridContext(props);
  const [isMenuOpen, setIsMenuOpen] = useState(false);
  const busy = context.busyDocumentId === document.documentId;

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
            <span className="sr-only">More options for {documentTitle(document)}</span>
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent side="left" align="start">
          <DropdownMenuItem onClick={() => context.onOpen(document.documentId)}>
            <span>Open</span>
          </DropdownMenuItem>
          {context.canDownload && (
            <DropdownMenuItem onClick={() => context.onDownload(document)}>
              <span>Download</span>
            </DropdownMenuItem>
          )}
          {document.failure?.retriable && (
            <DropdownMenuItem onClick={() => context.onRetry(document)}>
              <span>Retry</span>
            </DropdownMenuItem>
          )}
          <DropdownMenuSeparator />
          <DropdownMenuItem variant="destructive" onClick={() => context.onDelete(document)}>
            <span>Delete</span>
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}
