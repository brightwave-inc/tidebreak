import { HttpError, type ApiClient } from "../api/client";
import type { CodeWorkspaceFiles } from "../api/types";
import { CommitBox } from "./CommitBox";
import { noteWorkspaceFilesChanged } from "./CodeUpdatesStore";
import type { CodeWorkspacePrResource } from "./useCodeWorkspacePr";

/**
 * The commit box on a live workspace: the status read says whether there is
 * anything to commit and what the default message is, and a commit shares
 * that read's lock so it never races a push or a merge from the header.
 *
 * A commit carries what the person reviewed: it names the tree the Changes
 * list was read from, and the server refuses when the worktree moved since.
 */
export function WorkspaceCommitBox({
  client,
  workspaceId,
  prResource,
  files,
  unavailableReason,
}: {
  client: Pick<ApiClient, "commitCodeWorkspace">;
  workspaceId: string;
  prResource: Pick<
    CodeWorkspacePrResource,
    "data" | "busy" | "runMutation" | "refresh"
  >;
  files: CodeWorkspaceFiles | null;
  unavailableReason?: string;
}) {
  const status = prResource.data;
  const changedFiles =
    files && !files.truncated
      ? files.files.filter((file) => file.uncommitted).length
      : undefined;
  const reviewedTree = files?.worktree_tree;
  return (
    <CommitBox
      dirty={status ? status.dirty : null}
      changedFiles={changedFiles}
      suggestedMessage={status?.suggested_commit_message}
      busy={prResource.busy !== null && prResource.busy !== "refresh"}
      unavailableReason={
        unavailableReason ??
        (files === null ? "Reading the changed files…" : undefined)
      }
      onCommit={async (message) => {
        let committed:
          | Awaited<ReturnType<ApiClient["commitCodeWorkspace"]>>
          | undefined;
        try {
          committed = await prResource.runMutation("commit", () =>
            client.commitCodeWorkspace(workspaceId, message, reviewedTree),
          );
        } catch (error) {
          // Read the list again, so the next commit carries what it shows.
          if (error instanceof HttpError && error.kind === "worktree_changed") {
            noteWorkspaceFilesChanged(workspaceId);
          }
          throw error;
        }
        if (!committed) {
          throw new Error(
            "Another Git action is running on this workspace. Try again when it finishes.",
          );
        }
        noteWorkspaceFilesChanged(workspaceId);
        void prResource.refresh();
        return committed;
      }}
    />
  );
}
