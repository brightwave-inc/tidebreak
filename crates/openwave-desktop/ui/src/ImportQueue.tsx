import {
  CheckCircle2Icon,
  FileWarningIcon,
  Loader2Icon,
  UploadIcon,
  XIcon,
} from "lucide-react";

import { DocumentIcon } from "@/components/document-table/DocumentIcon";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import type { LibraryImportState } from "./documents";
import {
  importIsActive,
  sortedImportQueue,
  useImportQueueStore,
} from "./ImportQueueStore";
import { mediaTypeForFileName } from "./mediaTypeForFileName";

/**
 * The import run in progress, as a card over the conversation.
 *
 * Every state is carried by a glyph and a colour rather than by wording, so a
 * run that went wrong reads as wrong before any of it is read. The count in the
 * heading is what tells a reader whether the run they are looking at is the one
 * they started.
 */
export function ImportQueue() {
  const entries = useImportQueueStore((state) => state.entries);
  const dismiss = useImportQueueStore((state) => state.dismiss);
  if (entries.length === 0) return null;

  const sorted = sortedImportQueue(entries);
  const failed = sorted.filter((entry) => entry.status === "failed");
  const settled = sorted.filter((entry) => !importIsActive(entry.status)).length;
  const succeeded = settled - failed.length;
  const running = settled < sorted.length;

  // Per-file states, not bytes: the host reports each file as queued, streaming
  // or done, so this steps as files land rather than sweeping.
  const percentage = Math.round((settled / sorted.length) * 100);

  return (
    <aside
      className="fixed right-4 bottom-4 z-30 flex w-[min(24rem,calc(100vw-2rem))] flex-col gap-2 rounded-lg border bg-card p-2 shadow-lg"
      aria-labelledby="import-queue-title"
    >
      <div className="flex w-full items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-3">
          <div className="shrink-0 rounded-md bg-muted p-2.5">
            <UploadIcon className="size-4" />
          </div>
          <div className="flex min-w-0 flex-col">
            <h2 id="import-queue-title" className="truncate text-sm font-medium">
              {running
                ? `Adding ${sorted.length} ${sorted.length === 1 ? "source" : "sources"}`
                : `Added ${succeeded} ${succeeded === 1 ? "source" : "sources"}`}
            </h2>
            {failed.length > 0 && (
              <span className="text-xs text-muted-foreground">
                {failed.length} {failed.length === 1 ? "source" : "sources"} failed
              </span>
            )}
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {failed.length > 0 && (
            <FileWarningIcon className="size-4 shrink-0 text-critical" />
          )}
          {running ? (
            <Badge variant="info">
              <Loader2Icon className="size-3 animate-spin" />
              {percentage}%
            </Badge>
          ) : (
            <Button
              variant="ghost"
              size="icon-xs"
              className="text-muted-foreground hover:text-foreground"
              onClick={dismiss}
            >
              <XIcon className="size-4" />
              <span className="sr-only">Dismiss</span>
            </Button>
          )}
        </div>
      </div>

      <ul
        className="max-h-64 overflow-y-auto rounded-lg border bg-muted/10 p-2"
        aria-live="polite"
      >
        {sorted.map((entry) => {
          const note = stateNote(entry.status);
          return (
            <li
              key={entry.importId}
              className="border-b border-muted/50 py-1 last:border-b-0"
            >
              <div className="flex items-center gap-2 text-xs">
                <DocumentIcon
                  mediaType={mediaTypeForFileName(entry.displayName)}
                  className="size-4 shrink-0"
                />
                <span className="min-w-0 flex-1 truncate">{entry.displayName}</span>
                {note && (
                  <span className="shrink-0 text-muted-foreground italic">{note}</span>
                )}
                <StateGlyph status={entry.status} />
              </div>
              {entry.status === "failed" && entry.message && (
                <p className="ml-6 truncate text-xs text-critical">{entry.message}</p>
              )}
            </li>
          );
        })}
      </ul>
    </aside>
  );
}

/**
 * The glyph a row wears. Three shapes, because a reader scanning the list is
 * asking one question: is this one done, working, or broken.
 */
function StateGlyph({ status }: { status: LibraryImportState }) {
  switch (status) {
    case "queued":
      return (
        <UploadIcon
          aria-label="Waiting"
          className="size-3 shrink-0 text-muted-foreground"
        />
      );
    case "streaming":
      return <Loader2Icon aria-label="Adding" className="size-3 shrink-0 animate-spin" />;
    case "imported":
    case "already_present":
      return (
        <CheckCircle2Icon aria-label="Added" className="size-3 shrink-0 text-success" />
      );
    case "failed":
      return <XIcon aria-label="Failed" className="size-3 shrink-0 text-critical" />;
  }
}

/**
 * Words only where the glyph is not enough. A tick beside a name says "added"
 * on its own; what it cannot say is that the file was already here.
 */
function stateNote(status: LibraryImportState): string | null {
  return status === "already_present" ? "Already added" : null;
}
