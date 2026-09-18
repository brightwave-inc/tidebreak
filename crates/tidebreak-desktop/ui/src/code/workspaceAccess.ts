import type { CodeWorkspaceSnapshot } from "../api/types";
import type { WorkspaceCommand } from "./workspaceActions";
import { isRemoteWorktreePath } from "./workspaceRemote";

const MANAGEMENT_COMMANDS = new Set<WorkspaceCommand["id"]>([
  "open",
  "rename",
  "copy-branch",
  "copy-worktree",
  "open-pr",
  "archive",
  "force-archive",
  "restore",
]);

/** Workspace management never grants host execution or access management. */
export function workspaceCommandsForAccess(
  workspace: Pick<
    CodeWorkspaceSnapshot,
    "read_only" | "is_owner" | "worktree_path"
  >,
  commands: WorkspaceCommand[],
): WorkspaceCommand[] {
  if (workspace.read_only) return [];
  if (
    !workspace.worktree_path ||
    isRemoteWorktreePath(workspace.worktree_path)
  ) {
    commands = commands.filter(
      (command) =>
        ![
          "copy-worktree",
          "open-worktree",
          "open-in-editor",
          "toggle-terminal",
          "retry-setup",
        ].includes(command.id),
    );
  }
  if (workspace.is_owner !== false) return commands;
  return commands.filter((command) => MANAGEMENT_COMMANDS.has(command.id));
}
