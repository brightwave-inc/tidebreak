import { useCallback, useMemo } from "react";
import type { CodeWorkspaceSnapshot } from "../../api/types";
import { useWorkspaceTurnRunning } from "../CodeUpdatesStore";
import type { ChangeRowActions } from "../DiffOverview";
import type { DiffRevertActions } from "../DiffPanel";
import { isRemoteWorktreePath } from "../workspaceRemote";
import {
  TURN_RUNNING_REASON,
  useWorktreeUndo,
  type WorktreeUndoClient,
} from "../worktreeUndo";

/**
 * The actions that change one workspace's live worktree, shaped for the
 * transcript, the diff, and Source control, and whether any of them is on
 * offer right now.
 */
export function useWorktreeActions({
  client,
  workspaceId,
  workspace,
}: {
  client: WorktreeUndoClient;
  workspaceId: string;
  workspace: CodeWorkspaceSnapshot | null;
}) {
  // Undo, revert, discard, and commit change the live worktree. A sandbox
  // workspace has none here, and a running turn holds it: the server refuses
  // then, and the controls say why before the reader tries.
  const undo = useWorktreeUndo({ client, workspaceId });
  const turnRunning = useWorkspaceTurnRunning(workspaceId);
  const worktreeChangeable =
    workspace?.status === "active" &&
    !isRemoteWorktreePath(workspace.worktree_path);
  const worktreeUnavailableReason = turnRunning
    ? TURN_RUNNING_REASON
    : undefined;
  const { restore: restoreCheckpoint } = undo;
  const restoreBeforeTurn = useCallback(
    (turnId: string) =>
      void restoreCheckpoint({ kind: "before_turn", turn_id: turnId }),
    [restoreCheckpoint],
  );
  const undoRestore = useCallback(
    (restoreId: string) =>
      void restoreCheckpoint({
        kind: "before_restore",
        restore_id: restoreId,
      }),
    [restoreCheckpoint],
  );
  const changeActions = useMemo<ChangeRowActions | undefined>(
    () =>
      worktreeChangeable
        ? {
            onRevertFile: undo.revertFile,
            onDiscard: undo.discard,
            unavailableReason: worktreeUnavailableReason,
          }
        : undefined,
    [
      worktreeChangeable,
      undo.revertFile,
      undo.discard,
      worktreeUnavailableReason,
    ],
  );
  const diffRevert = useMemo<DiffRevertActions | undefined>(
    () =>
      worktreeChangeable
        ? {
            onRevertFile: undo.revertFile,
            onRevertHunk: undo.revertHunk,
            unavailableReason: worktreeUnavailableReason,
          }
        : undefined,
    [
      worktreeChangeable,
      undo.revertFile,
      undo.revertHunk,
      worktreeUnavailableReason,
    ],
  );

  return {
    undo,
    worktreeChangeable,
    worktreeUnavailableReason,
    restoreBeforeTurn,
    undoRestore,
    changeActions,
    diffRevert,
  };
}
