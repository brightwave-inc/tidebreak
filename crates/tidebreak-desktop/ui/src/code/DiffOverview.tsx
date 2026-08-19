import { useCallback } from "react";

import type { ApiClient } from "../api/client";
import type { CodeFileChange, FileChangeKind } from "../api/types";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import { DiffstatBadge } from "./TurnReviewCard";
import { FOCUS_RING, HOVER_TINT } from "./interactive";
import { useLiveResource } from "./useLiveContent";

const FILE_KIND: Record<
  FileChangeKind,
  { letter: string; label: string; className: string }
> = {
  added: {
    letter: "A",
    label: "Added",
    className: "bg-success-background text-success-foreground",
  },
  modified: {
    letter: "M",
    label: "Modified",
    className: "bg-info-background text-info-foreground",
  },
  deleted: {
    letter: "D",
    label: "Deleted",
    className: "bg-critical-background text-critical-foreground",
  },
  renamed: {
    letter: "R",
    label: "Renamed",
    className: "bg-warning-background text-warning-foreground",
  },
};

/**
 * Compact source-control index. It deliberately fetches the bounded changed
 * file list rather than the unified patch: the sidebar answers what changed;
 * the center pane answers how.
 */
export function DiffOverview({
  client,
  workspaceId,
  turnId,
  turnLabel,
  selected,
  contentRevision = 0,
  onOpenFile,
}: {
  client: Pick<ApiClient, "listCodeWorkspaceFiles">;
  workspaceId: string;
  turnId?: string;
  /** Ordinal label for the scoped turn. Never a raw id. */
  turnLabel?: string;
  selected?: string;
  contentRevision?: number;
  onOpenFile: (path: string) => void;
}) {
  const load = useCallback(
    () => client.listCodeWorkspaceFiles(workspaceId, turnId),
    [client, workspaceId, turnId],
  );
  const {
    data: payload,
    error,
    refreshing,
  } = useLiveResource({
    key: `${workspaceId}:${turnId ?? "workspace"}`,
    revision: contentRevision,
    load,
    errorMessage: "Could not load changed files",
  });

  const scopeCaption = turnId ? (turnLabel ?? "This turn") : "Workspace vs base";

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
      <header className="flex shrink-0 items-center justify-between gap-2 px-3 pb-2 pt-3">
        <div className="min-w-0">
          <h2 className="text-sm font-medium">Changes</h2>
          <p className="text-muted-foreground truncate font-mono text-[11px]">
            {scopeCaption}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <span className="grid size-3.5 shrink-0 place-items-center">
            {refreshing && (
              <Spinner className="size-3.5" aria-label="Refreshing" />
            )}
          </span>
          {payload && <DiffstatBadge stat={payload.stat} />}
        </div>
      </header>
      {error && <p className="text-critical px-3 py-2 text-sm">{error}</p>}
      {payload?.truncated && (
        <p className="text-muted-foreground border-y px-3 py-2 text-xs">
          The changed-file list was truncated.
        </p>
      )}
      {!payload && !error && <ChangesSkeleton />}
      {payload && payload.files.length > 0 && (
        <ul
          className="min-h-0 flex-1 overflow-y-auto px-1 pb-4 pt-1"
          aria-label="Changed files"
        >
          {payload.files.map((file) => (
            <ChangeRow
              key={`${file.previous_path ?? ""}:${file.path}`}
              file={file}
              selected={selected === file.path}
              onOpenFile={onOpenFile}
            />
          ))}
        </ul>
      )}
      {payload && payload.files.length === 0 && !error && (
        <p className="text-muted-foreground px-3 py-6 text-sm">
          {emptyChangesText(turnId, turnLabel)}
        </p>
      )}
    </div>
  );
}

function ChangeRow({
  file,
  selected,
  onOpenFile,
}: {
  file: CodeFileChange;
  selected: boolean;
  onOpenFile: (path: string) => void;
}) {
  const kind = FILE_KIND[file.kind];
  const { directory, name } = pathParts(file.path);
  const previous = file.previous_path
    ? `Previously ${file.previous_path}`
    : directory || kind.label;

  return (
    <li>
      <button
        type="button"
        className={cn(
          "group flex w-full cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-left",
          FOCUS_RING,
          HOVER_TINT,
          selected ? "bg-muted/70" : "hover:bg-muted/45",
        )}
        aria-label={`${kind.label} ${file.path}, ${file.insertions} insertions, ${file.deletions} deletions`}
        aria-current={selected ? "page" : undefined}
        onClick={() => onOpenFile(file.path)}
      >
        <span
          className={cn(
            "grid size-5 shrink-0 place-items-center rounded font-mono text-[10px] font-semibold",
            kind.className,
          )}
          aria-hidden
        >
          {kind.letter}
        </span>
        <span className="min-w-0 flex-1">
          <span className="block truncate font-mono text-xs" title={file.path}>
            {name}
          </span>
          <span
            className="text-muted-foreground block truncate font-mono text-[10px] leading-4"
            title={file.previous_path ?? directory}
          >
            {previous}
          </span>
        </span>
        <span className="shrink-0 font-mono text-[10px] tabular-nums">
          <span className="text-success-foreground">+{file.insertions}</span>{" "}
          <span className="text-critical-foreground">−{file.deletions}</span>
        </span>
      </button>
    </li>
  );
}

function pathParts(path: string): { directory: string; name: string } {
  const slash = path.lastIndexOf("/");
  if (slash < 0) return { directory: "", name: path };
  return {
    directory: path.slice(0, slash + 1),
    name: path.slice(slash + 1),
  };
}

function emptyChangesText(turnId?: string, turnLabel?: string): string {
  if (turnId) return `${turnLabel ?? "This turn"} changed no files.`;
  return "The worktree matches its base branch.";
}

function ChangesSkeleton() {
  return (
    <div className="flex flex-col gap-2 px-3 py-3" aria-hidden="true">
      <Skeleton className="h-8 w-full" />
      <Skeleton className="h-8 w-5/6" />
      <Skeleton className="h-8 w-11/12" />
    </div>
  );
}
