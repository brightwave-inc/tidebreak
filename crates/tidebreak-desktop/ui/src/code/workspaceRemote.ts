/** Host checkout is absent; files live in a sandbox or its checkpoint. */
export function isRemoteWorktreePath(path: string | null | undefined): boolean {
  return path === "" || Boolean(path?.startsWith("remote:"));
}

/** Local archived and released workspaces have no host worktree until restore. */
export function isArchivedLocalWorkspace(
  workspace: {
    status?: string;
    worktree_path?: string | null;
  } | null,
): boolean {
  if (!workspace || isRemoteWorktreePath(workspace.worktree_path)) {
    return false;
  }
  return workspace.status === "archived" || workspace.status === "released";
}
