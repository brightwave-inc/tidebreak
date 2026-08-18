import { useCallback } from "react";

import type { ApiClient } from "../api/client";
import type { CodeFileChange, FileChangeKind } from "../api/types";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import { useLiveResource } from "./useLiveContent";

const FILE_KIND_LETTER: Record<FileChangeKind, string> = {
  added: "A",
  modified: "M",
  deleted: "D",
  renamed: "R",
};

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

  const files = listing?.files ?? [];
  const empty = listing !== null && files.length === 0 && !error;

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
      <div className="flex items-center justify-between gap-2 px-3 pt-3">
        <h2 className="text-sm font-medium">Changed files</h2>
        <span className="grid size-3.5 shrink-0 place-items-center">
          {refreshing && <Spinner className="size-3.5" aria-label="Refreshing" />}
        </span>
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
      {empty ? (
        <p className="text-muted-foreground px-3 py-6 text-sm">No files changed.</p>
      ) : listing ? (
        <ul className="min-h-0 flex-1 overflow-y-auto px-3 pb-4">
          {files.map((file) => {
            const current = selected === file.path;
            return (
              <li key={`${file.kind}:${file.path}`}>
                <button
                  type="button"
                  aria-current={current ? true : undefined}
                  className={cn(
                    "flex w-full items-baseline justify-between gap-3 border-b py-2 text-left text-xs",
                    current && "bg-muted/40",
                  )}
                  onClick={() => onOpenFile(file.path)}
                >
                  <span className="flex min-w-0 items-baseline gap-2">
                    <span
                      className={cn(
                        "w-3 shrink-0 font-mono",
                        file.kind === "added" && "text-success-foreground-muted",
                        file.kind === "modified" && "text-info-foreground-muted",
                        file.kind === "deleted" && "text-critical-foreground-muted",
                        file.kind === "renamed" && "text-warning-foreground-muted",
                      )}
                      aria-label={file.kind}
                    >
                      {FILE_KIND_LETTER[file.kind]}
                    </span>
                    <code className="min-w-0 truncate" title={file.path}>
                      {fileLabel(file)}
                    </code>
                  </span>
                  <FileStat file={file} />
                </button>
              </li>
            );
          })}
        </ul>
      ) : null}
    </div>
  );
}

function FileStat({ file }: { file: CodeFileChange }) {
  if (file.insertions === 0 && file.deletions === 0) {
    return (
      <span className="shrink-0 tabular-nums">
        <span className="text-success">+0</span>
      </span>
    );
  }
  return (
    <span className="shrink-0 tabular-nums">
      {file.insertions > 0 && (
        <span className="text-success">+{file.insertions}</span>
      )}
      {file.insertions > 0 && file.deletions > 0 && " "}
      {file.deletions > 0 && (
        <span className="text-critical">−{file.deletions}</span>
      )}
    </span>
  );
}

function fileLabel(file: CodeFileChange): string {
  if (file.kind === "renamed" && file.previous_path) {
    return `${file.previous_path} → ${file.path}`;
  }
  return file.path;
}
