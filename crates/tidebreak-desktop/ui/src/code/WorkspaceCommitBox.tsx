import type { ApiClient } from "../api/client";
import type { CodeWorkspaceFiles } from "../api/types";
import { CommitBox } from "./CommitBox";
import { noteWorkspaceFilesChanged } from "./CodeUpdatesStore";
import type { CodeWorkspacePrResource } from "./useCodeWorkspacePr";

/**
 * The commit box on a live workspace: the status read says whether there is
 * anything to commit and what the default message is, and a commit shares
 * that read's lock so it never races a push or a merge from the header.
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
  return (
    <CommitBox
      dirty={status ? status.dirty : null}
      changedFiles={changedFiles}
      suggestedMessage={status?.suggested_commit_message}
      busy={prResource.busy !== null && prResource.busy !== "refresh"}
      unavailableReason={unavailableReason}
      onCommit={async (message) => {
        const committed = await prResource.runMutation("commit", () =>
          client.commitCodeWorkspace(workspaceId, message),
        );
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
