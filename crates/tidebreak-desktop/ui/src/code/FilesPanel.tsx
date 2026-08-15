import { useEffect, useState } from "react";

import type { ApiClient } from "../api/client";
import type { CodeFileChange, CodeWorkspaceFiles } from "../api/types";
import { Badge } from "@/components/ui/badge";
import { friendlyErrorMessage } from "@/lib/utils";
import { DiffstatBadge } from "./TurnReviewCard";

/**
 * Changed files vs the workspace base, optionally filtered to one turn.
 * Clicking a row opens the Diff panel at that file.
 */
export function FilesPanel({
  client,
  workspaceId,
  turnId,
  onOpenFile,
}: {
  client: Pick<ApiClient, "listCodeWorkspaceFiles">;
  workspaceId: string;
  turnId?: string;
  onOpenFile: (file: string) => void;
}) {
  const [listing, setListing] = useState<CodeWorkspaceFiles | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const next = await client.listCodeWorkspaceFiles(workspaceId, turnId);
        if (!cancelled) {
          setListing(next);
          setError(null);
        }
      } catch (err) {
        if (!cancelled) {
          setError(friendlyErrorMessage(err, "Could not load changed files"));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, workspaceId, turnId]);

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
      <header className="flex shrink-0 items-center justify-between gap-2 border-b px-3 py-2">
        <h2 className="text-sm font-medium">Changed files</h2>
        {listing && <DiffstatBadge stat={listing.stat} />}
      </header>
      {error && <p className="text-critical px-3 py-2 text-sm">{error}</p>}
      {listing?.truncated && (
        <p className="text-muted-foreground px-3 py-2 text-xs">
          File list was truncated. Open a turn or a single file for a smaller view.
        </p>
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
