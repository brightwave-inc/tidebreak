import { useCallback } from "react";

import type { ApiClient } from "../api/client";
import type { CodeFileChange } from "../api/types";
import { Badge } from "@/components/ui/badge";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { DiffstatBadge } from "./TurnReviewCard";
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
  onOpenFile,
  contentRevision = 0,
}: {
  client: Pick<ApiClient, "listCodeWorkspaceFiles">;
  workspaceId: string;
  turnId?: string;
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
      <header className="flex shrink-0 items-center justify-between gap-2 border-b px-3 py-2">
        <h2 className="text-sm font-medium">Changed files</h2>
        <div className="flex items-center gap-2">
          {refreshing && <Spinner className="size-3.5" aria-label="Refreshing" />}
          {listing && <DiffstatBadge stat={listing.stat} />}
        </div>
      </header>
      {error && <p className="text-critical px-3 py-2 text-sm">{error}</p>}
      {listing?.truncated && (
        <p className="text-muted-foreground px-3 py-2 text-xs">
          File list was truncated. Open a turn or a single file for a smaller view.
        </p>
      )}
      {!listing && !error && (
        <div className="flex flex-col gap-2 px-3 py-3" aria-hidden="true">
          <Skeleton className="h-4 w-2/3" />
          <Skeleton className="h-4 w-1/2" />
          <Skeleton className="h-4 w-3/5" />
        </div>
      )}
      <ul className="min-h-0 flex-1 overflow-y-auto">
        {(listing?.files ?? []).map((file) => (
          <li key={`${file.kind}:${file.path}`}>
            <button
              type="button"
              className="hover:bg-muted/60 flex w-full items-center gap-2 px-3 py-2 text-left text-xs"
              onClick={() => onOpenFile(file.path)}
            >
              <KindBadge kind={file.kind} />
              <span className="min-w-0 flex-1 truncate font-mono" title={file.path}>
                {fileLabel(file)}
              </span>
              <span className="text-muted-foreground shrink-0">
                +{file.insertions} −{file.deletions}
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

function KindBadge({ kind }: { kind: CodeFileChange["kind"] }) {
  const label =
    kind === "added"
      ? "A"
      : kind === "deleted"
        ? "D"
        : kind === "renamed"
          ? "R"
          : "M";
  return (
    <Badge variant="outline" size="sm">
      {label}
    </Badge>
  );
}

function fileLabel(file: CodeFileChange): string {
  if (file.kind === "renamed" && file.previous_path) {
    return `${file.previous_path} → ${file.path}`;
  }
  return file.path;
}
