import {
  Check,
  File,
  FileOutput,
  Folder,
  Globe,
  Loader2,
  Quote,
  BookOpenText as Source,
  X,
  type LucideIcon,
} from "lucide-react";

import type {
  EntriesResultPreview,
  ResultEntry,
  ResultEntryKind,
  ResultFailure,
} from "./api";
import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";
import { ToolCardShell } from "./ToolCardShell";
import { ToolIcon } from "./ToolIcon";
import { toolCallPresentation, type ToolCallStatus } from "./ToolCallCard";

type ToolEntriesCardProps = {
  name: string;
  status: ToolCallStatus;
  /** What the call surfaced, once it has surfaced anything. */
  result: EntriesResultPreview;
};

/**
 * What one row is, drawn from the server's closed vocabulary.
 *
 * Keyed by [`ResultEntryKind`] so a kind added to that union without a glyph
 * fails to compile rather than rendering a hole in the middle of a list.
 */
const ENTRY_ICONS: Record<ResultEntryKind, LucideIcon> = {
  file: File,
  folder: Folder,
  source: Source,
  passage: Quote,
  link: Globe,
  output: FileOutput,
};

/**
 * The card for a call that found, read, or wrote a list of things.
 *
 * One card for every such tool rather than one per tool: what distinguishes a
 * source search from a directory listing on screen is the icon, the headline,
 * and the count — all of which already come from the tool's own name. The rows
 * differ only in what they are, which the server says per row.
 *
 * An empty list still gets a card, and that is the point. A search that matched
 * nothing and a search whose result was never projected are the same muted rail
 * line, and only one of them is a fact about the conversation.
 */
export function ToolEntriesCard({ name, status, result }: ToolEntriesCardProps) {
  const presentation = toolCallPresentation(name, status);
  const { entries, failures, elided } = result;
  const total = entries.length + elided;

  return (
    <ToolCardShell
      label={`${presentation.label}: ${presentation.statusLabel}`}
      icon={<ToolIcon name={name} className="size-3.5 shrink-0" />}
      title={presentation.title}
      badge={
        <EntriesBadge
          presentation={presentation}
          total={total}
          failureCount={failures.length}
        />
      }
    >
      {entries.length === 0 && failures.length === 0 ? (
        <p className="text-muted-foreground px-2.5 py-2 text-xs">
          {emptyMessage(presentation.tone)}
        </p>
      ) : null}
      {entries.length > 0 && (
        <div className="flex max-h-56 flex-col gap-0.5 overflow-auto p-1">
          {entries.map((entry, index) => (
            <EntryRow key={`${entry.kind}:${entry.label}:${index}`} entry={entry} />
          ))}
        </div>
      )}
      <FailureRows failures={failures} />
      {elided > 0 && (
        <p className="text-muted-foreground border-t px-2.5 py-1.5 text-xs">
          {elided} more not shown
        </p>
      )}
    </ToolCardShell>
  );
}

/**
 * What the call could not do, below what it did and divided from it.
 *
 * Its own section rather than rows mixed into the list: a failure is not a
 * result with a bad outcome, it is the absence of one, and a reader scanning
 * for what they got should not have to check each row for whether it counted.
 */
function FailureRows({ failures }: { failures: ResultFailure[] }) {
  if (failures.length === 0) return null;
  return (
    <div className="flex flex-col gap-0.5 border-t p-1">
      {failures.map((failure, index) => (
        <div
          key={`${failure.label ?? ""}:${index}`}
          className="flex items-start gap-2 rounded p-1 px-1.5 text-sm"
        >
          <X
            className="text-destructive mt-0.5 size-4 shrink-0"
            aria-hidden="true"
          />
          <span className="truncate">{failure.label ?? "Item"}</span>
          <span className="text-destructive ml-auto min-w-0 truncate text-right text-xs">
            {failure.error}
          </span>
        </div>
      ))}
    </div>
  );
}

function EntryRow({ entry }: { entry: ResultEntry }) {
  const Icon = ENTRY_ICONS[entry.kind];
  return (
    <div className="hover:bg-muted flex items-center gap-2 rounded p-1 px-1.5 text-sm transition-colors">
      <Icon className="text-muted-foreground size-4 shrink-0" aria-hidden="true" />
      <span className="truncate">{entry.label}</span>
      {entry.detail !== null && (
        <span className="text-muted-foreground truncate text-xs">
          {entry.detail}
        </span>
      )}
      {entry.meta !== null && (
        <span className="text-muted-foreground ml-auto shrink-0 text-xs tabular-nums">
          {entry.meta}
        </span>
      )}
    </div>
  );
}

/**
 * The count is the outcome here, the way an exit code is for a command: "Done"
 * says nothing a reader could not already assume, and "Found 0" is the single
 * most useful thing a search can report.
 */
function EntriesBadge({
  presentation,
  total,
  failureCount,
}: {
  presentation: { tone: string; badgeLabel: string };
  total: number;
  failureCount: number;
}) {
  if (presentation.tone === "running") {
    return (
      <Badge variant="outline" className="shrink-0 gap-1">
        <Loader2 className="size-3 animate-spin" aria-hidden="true" />
        Running…
      </Badge>
    );
  }
  if (presentation.tone === "completed") {
    // A partial success is not a success. The badge carries the failure count
    // so a collapsed card cannot read as "12 results" when two of them failed.
    if (failureCount > 0) {
      return (
        <Badge variant="warning" className="shrink-0 gap-1">
          {total} {total === 1 ? "result" : "results"} · {failureCount} failed
        </Badge>
      );
    }
    return (
      <Badge
        variant={total === 0 ? "outline" : "success"}
        className="shrink-0 gap-1"
      >
        {total > 0 && <Check className="size-3" aria-hidden="true" />}
        {total} {total === 1 ? "result" : "results"}
      </Badge>
    );
  }
  return (
    <Badge
      variant="outline"
      className={cn(
        "text-muted-foreground shrink-0 gap-1",
        presentation.tone === "failed" && "text-destructive",
      )}
    >
      <X className="size-3" aria-hidden="true" />
      {presentation.badgeLabel}
    </Badge>
  );
}

function emptyMessage(tone: string): string {
  if (tone === "running") return "Waiting for results…";
  if (tone === "completed") return "Nothing was found.";
  return "No results.";
}
