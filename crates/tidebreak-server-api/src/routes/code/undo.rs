//! Undo in a workspace's worktree: checkpoint restore, revert, and discard.
//!
//! Each one changes files between turns and refuses while a turn runs
//! (`409 turn_running`), while an engine from before a restart may still be
//! in the checkout (`409 workspace_fenced`), and in a sandbox workspace
//! (`409 workspace_remote`). A caller who may only view the workspace gets
//! `404`, as for commit and push.

use crate::code::checkpoint::{ChangedFile, RevertedChange};
use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::{Json, Path, Query};

use super::types::{
    CheckpointRestoreQuery, CodeCheckpointRestorePreview, CodeCheckpointRestoreResult,
    CodeFileChange, CodeRestoreAffectedTurn, CodeWorktreeChange, DiscardWorkspaceChangesBody,
    RestoreCheckpointBody, RevertWorkspaceChangeBody,
};
use tidebreak_core::{CheckpointRestoreTarget, WorkspaceId};

/// `GET /code/workspaces/{id}/checkpoints/restore?turn=<id>` or
/// `?restore=<id>` — what restoring that state would undo.
///
/// The list names every change since the target, whoever made it, so a
/// confirmation can say what is lost. `current_tree` is the token the restore
/// takes back as `expected_tree`.
pub async fn preview_checkpoint_restore(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Query(query): Query<CheckpointRestoreQuery>,
) -> Result<Json<CodeCheckpointRestorePreview>, ServerError> {
    let target = match (query.turn, query.restore) {
        (Some(turn_id), None) => CheckpointRestoreTarget::BeforeTurn { turn_id },
        (None, Some(restore_id)) => CheckpointRestoreTarget::BeforeRestore { restore_id },
        _ => {
            return Err(ServerError::bad_request_kind(
                "restore_target",
                "name exactly one of turn or restore",
            ))
        }
    };
    let preview = code.preview_checkpoint_restore(id, target).await?;
    Ok(Json(CodeCheckpointRestorePreview {
        target: preview.target,
        session_id: preview.session_id,
        truncated: preview.preview.files.truncated,
        stat: preview.preview.files.stat,
        files: file_changes(preview.preview.files.files),
        current_tree: preview.preview.current_tree,
        blocked: preview
            .preview
            .blocked
            .iter()
            .map(|path| path.to_wire())
            .collect(),
        affected_turns: preview
            .affected_turns
            .into_iter()
            .map(|turn| CodeRestoreAffectedTurn {
                session_id: turn.session_id,
                turn_id: turn.turn_id,
                ordinal: turn.ordinal,
                harness_kind: turn.harness_kind,
            })
            .collect(),
    }))
}

/// `POST /code/workspaces/{id}/checkpoints/restore` — put the worktree back to
/// an earlier state.
///
/// The state it replaces is saved, and the restore is journaled in the
/// transcript of the session it belongs to, so restoring `before_restore`
/// with the returned `restore_id` undoes it. Other kinds a client branches
/// on: `409 worktree_changed` (the worktree moved after the preview),
/// `409 restore_blocked` (unsaved files are in the way; the message names
/// them), `409 restore_failed` (git stopped partway), and
/// `409 nothing_to_restore`.
pub async fn restore_checkpoint(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<RestoreCheckpointBody>,
) -> Result<Json<CodeCheckpointRestoreResult>, ServerError> {
    let restored = code
        .restore_checkpoint(id, body.target, body.expected_tree)
        .await?;
    Ok(Json(CodeCheckpointRestoreResult {
        restore_id: restored.restore_id,
        target: restored.target,
        session_id: restored.session_id,
        truncated: restored.files.truncated,
        stat: restored.files.stat,
        files: file_changes(restored.files.files),
    }))
}

/// `POST /code/workspaces/{id}/revert` — undo one file's change, or one hunk
/// of it, in the workspace diff or one turn's diff.
///
/// `409 diff_changed` means the hunk the person saw is no longer in the diff;
/// `409 revert_conflict` means the worktree no longer holds the change, or a
/// later edit overlaps it; `409 revert_blocked` names unsaved files in the
/// way.
pub async fn revert_workspace_change(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<RevertWorkspaceChangeBody>,
) -> Result<Json<CodeWorktreeChange>, ServerError> {
    let hunk = body
        .hunk
        .map(|hunk| (usize::try_from(hunk.index).unwrap_or(usize::MAX), hunk.text));
    let reverted = code
        .revert_workspace_change(id, body.turn_id, body.path, hunk)
        .await?;
    Ok(Json(worktree_change(reverted)))
}

/// `POST /code/workspaces/{id}/discard` — put files back to the last commit.
///
/// Every path must have an uncommitted change, or the discard answers
/// `409 no_change` and changes nothing. `409 discard_blocked` names a folder
/// or file in the way that nobody picked.
pub async fn discard_workspace_changes(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<DiscardWorkspaceChangesBody>,
) -> Result<Json<CodeWorktreeChange>, ServerError> {
    let discarded = code
        .discard_workspace_changes(id, body.paths, body.expected_tree)
        .await?;
    Ok(Json(worktree_change(discarded)))
}

/// The wire form of a changed-file list, shared with `GET /files`.
pub(super) fn file_changes(files: Vec<ChangedFile>) -> Vec<CodeFileChange> {
    files
        .into_iter()
        .map(|file| CodeFileChange {
            path: file.path.to_wire(),
            kind: file.kind,
            insertions: file.insertions,
            deletions: file.deletions,
            previous_path: file.previous_path.map(|path| path.to_wire()),
            uncommitted: file.uncommitted,
        })
        .collect()
}

fn worktree_change(change: RevertedChange) -> CodeWorktreeChange {
    CodeWorktreeChange {
        paths: change.paths.iter().map(|path| path.to_wire()).collect(),
    }
}
