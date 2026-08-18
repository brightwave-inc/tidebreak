import { useCallback } from "react";

import type { ApiClient } from "../api/client";
import type { CodeFileChange } from "../api/types";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import { useLiveResource } from "./useLiveContent";

/**
 * Changed files vs the workspace base, optionally filtered to one turn.
 * Clicking a row opens the Diff panel at that file.
 *
 * The list reloads on every content revision so a panel left open while the
 * engine works does not keep describing the worktree as it was.
 */
export function FilesPanel({
  client,
  workspaceId,
  turnId,
  selected,
  onOpenFile,
  contentRevision = 0,
}: {
  client: Pick<ApiClient, "listCodeWorkspaceFiles">;
  workspaceId: string;
  turnId?: string;
  selected?: string;
  onOpenFile: (file: string) => void;
  /** Bumped by the session journal when the worktree may have moved. */
  contentRevision?: number;
}) {
  const load = useCallback(
    () => client.listCodeWorkspaceFiles(workspaceId, turnId),
    [client, workspaceId, turnId],
  );
  const {
    data: listing,
    error,
    refreshing,
  } = useLiveResource({
    key: `${workspaceId} ${turnId ?? ""}`,
    revision: contentRevision,
    load,
    errorMessage: "Could not load changed files",
  });

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
      <div className="px-3 pt-3">
        <h2 className="text-sm font-medium">Changed files</h2>
        {refreshing && (
          <Spinner className="mt-2 size-3.5" aria-label="Refreshing" />
        )}
      </div>
      {error && <p className="text-critical px-3 py-2 text-sm">{error}</p>}
      {listing?.truncated && (
        <p className="text-muted-foreground px-3 py-2 text-xs">
          File list was truncated. Open a single file for the rest.
        </p>
      )}
      {!listing && !error && (
        <div className="flex flex-col gap-2 px-3 py-3" aria-hidden="true">
          <Skeleton className="h-4 w-2/3" />
          <Skeleton className="h-4 w-1/2" />
          <Skeleton className="h-4 w-3/5" />
        </div>
      )}
      <ul className="min-h-0 flex-1 overflow-y-auto px-3 pb-4">
        {(listing?.files ?? []).map((file) => (
          <li key={`${file.kind}:${file.path}`}>
            <button
              type="button"
              className={cn(
                "flex w-full items-baseline justify-between gap-3 border-b py-2 text-left text-xs",
                selected === file.path && "bg-muted/40",
              )}
              onClick={() => onOpenFile(file.path)}
            >
              <code className="min-w-0 truncate" title={file.path}>
                {fileLabel(file)}
              </code>
              <span className="text-success shrink-0 tabular-nums">
                {statLabel(file)}
              </span>
            </button>
          </li>
        ))}
      </ul>
      {listing && listing.files.length === 0 && !error && (
        <p className="text-muted-foreground px-3 py-6 text-sm">No files changed.</p>
      )}
    </div>
  );
}

function fileLabel(file: CodeFileChange): string {
  if (file.kind === "renamed" && file.previous_path) {
    return `${file.previous_path} → ${file.path}`;
  }
  return file.path;
}

function statLabel(file: CodeFileChange): string {
  const added = file.insertions > 0 ? `+${file.insertions}` : "";
  const removed = file.deletions > 0 ? `-${file.deletions}` : "";
  return [added, removed].filter(Boolean).join(" ") || "+0";
}
