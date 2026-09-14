import { archiveForceKind, type ApiClient } from "../api/client";
import type { CodeWorkspaceSnapshot } from "../api/types";
import type { ConfirmOptions } from "@/components/ConfirmDialog";
import { friendlyErrorMessage } from "@/lib/utils";

export type WorkspaceArchiveFailure = {
  workspace: CodeWorkspaceSnapshot;
  error: unknown;
};

export function bulkArchiveDiscardConfirmation(
  blocked: readonly WorkspaceArchiveFailure[],
): ConfirmOptions {
  const count = blocked.length;
  return {
    title: `Discard leftover work in ${count} ${count === 1 ? "workspace" : "workspaces"}?`,
    description: (
      <>
        These workspaces need permission to discard leftover work. Commit and
        push anything you want to keep before continuing.
        <span className="mt-3 block max-h-48 space-y-2 overflow-y-auto">
          {blocked.map(({ workspace, error }) => (
            <span key={workspace.id} className="block break-words">
              <span className="font-medium text-foreground">
                {workspace.title}
              </span>
              <span className="block">
                {friendlyErrorMessage(error, "Archive needs confirmation")}
              </span>
            </span>
          ))}
        </span>
      </>
    ),
    confirmLabel: "Discard and archive",
    destructive: true,
  };
}

/** Retry only confirmed discard blockers; preserve other failures for retry. */
export async function bulkArchiveWorkspaces(options: {
  client: Pick<ApiClient, "archiveCodeWorkspace">;
  workspaces: readonly CodeWorkspaceSnapshot[];
  force: boolean;
  confirm: (options: ConfirmOptions) => Promise<boolean>;
  onArchived: (workspace: CodeWorkspaceSnapshot) => void;
  onProgress?: (completed: number, total: number) => void;
}): Promise<{
  archivedIds: Set<string>;
  failed: WorkspaceArchiveFailure[];
}> {
  const archivedIds = new Set<string>();
  const failed: WorkspaceArchiveFailure[] = [];
  const blocked: WorkspaceArchiveFailure[] = [];

  async function archive(workspace: CodeWorkspaceSnapshot, force: boolean) {
    try {
      const archived = await options.client.archiveCodeWorkspace(
        workspace.id,
        force,
      );
      archivedIds.add(workspace.id);
      options.onArchived(archived);
    } catch (error) {
      const failure = { workspace, error };
      if (!force && archiveForceKind(error)) blocked.push(failure);
      else failed.push(failure);
    }
  }

  // Workspaces can share a repository. Keep Git cleanup in request order.
  for (const [index, workspace] of options.workspaces.entries()) {
    options.onProgress?.(index, options.workspaces.length);
    await archive(workspace, options.force);
  }
  options.onProgress?.(options.workspaces.length, options.workspaces.length);

  if (
    blocked.length > 0 &&
    (await options.confirm(bulkArchiveDiscardConfirmation(blocked)))
  ) {
    for (const [index, { workspace }] of blocked.entries()) {
      options.onProgress?.(index, blocked.length);
      await archive(workspace, true);
    }
    options.onProgress?.(blocked.length, blocked.length);
  }
  return { archivedIds, failed };
}
