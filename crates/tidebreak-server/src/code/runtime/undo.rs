//! Undo in the worktree: checkpoint restore, file and hunk revert, discard.
//!
//! All three change files in a workspace's live checkout, so they share one
//! gate. They run only between turns: the turn lock is taken without waiting,
//! and a busy lock is a refusal rather than a queue, because the person asked
//! to undo the worktree as they see it now. A session fenced for an engine
//! that may still be alive in the checkout also refuses them, as it refuses
//! turns (record 55). A sandbox workspace has no checkout here to change.

use super::*;

use tidebreak_core::db::code::get_turn;
use tidebreak_core::{CheckpointRestoreTarget, CodeRestoreId, TurnStatus};

use crate::code::checkpoint::{self, BoundedFiles, HunkSelector, RestorePreview, RevertedChange};

/// What a restore would undo, read before anything moves.
#[derive(Debug, Clone)]
pub struct CheckpointRestorePreview {
    pub target: CheckpointRestoreTarget,
    /// The session whose transcript records the restore.
    pub session_id: SessionId,
    pub preview: RestorePreview,
}

/// A restore that landed.
#[derive(Debug, Clone)]
pub struct CheckpointRestoreOutcome {
    pub restore_id: CodeRestoreId,
    pub target: CheckpointRestoreTarget,
    pub session_id: SessionId,
    /// What the restore changed, from the replaced state to the restored one.
    pub files: BoundedFiles,
}

/// Where a restore target lives: the commit to restore and the transcript
/// that records it.
struct ResolvedRestoreTarget {
    session_id: SessionId,
    commit: String,
}

/// The locks one worktree change holds until it drops.
struct WorktreeClaim {
    workspace: CodeWorkspace,
    _write: tokio::sync::OwnedMutexGuard<()>,
    _turn: tokio::sync::OwnedMutexGuard<()>,
}

impl CodeRuntime {
    /// Read what restoring `target` would undo.
    pub(crate) async fn preview_checkpoint_restore(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        target: CheckpointRestoreTarget,
    ) -> Result<CheckpointRestorePreview, ServerError> {
        refuse_sandbox(&self.get_workspace(owner, workspace_id).await?)?;
        let workspace = self.require_live_workspace(owner, workspace_id).await?;
        let resolved = self
            .resolve_restore_target(owner, &workspace, target)
            .await?;
        let preview = checkpoint::preview_restore(
            std::path::Path::new(&workspace.worktree_path),
            &resolved.commit,
        )
        .await
        .map_err(map_checkpoint)?;
        Ok(CheckpointRestorePreview {
            target,
            session_id: resolved.session_id,
            preview,
        })
    }

    /// Put the worktree back to `target`, save what it replaces, and journal
    /// the restore in the transcript it belongs to.
    ///
    /// `expected_tree` is the `current_tree` the preview showed. A worktree
    /// that moved since is left alone, so the person never loses a change
    /// the confirmation did not list.
    pub(crate) async fn restore_checkpoint(
        &self,
        owner: &OwnerId,
        caller: &OwnerId,
        workspace_id: WorkspaceId,
        target: CheckpointRestoreTarget,
        expected_tree: Option<&str>,
    ) -> Result<CheckpointRestoreOutcome, ServerError> {
        refuse_sandbox(&self.get_workspace(owner, workspace_id).await?)?;
        let claim = self.claim_worktree(owner, workspace_id).await?;
        let resolved = self
            .resolve_restore_target(owner, &claim.workspace, target)
            .await?;
        let worktree = std::path::PathBuf::from(&claim.workspace.worktree_path);
        let restore_id = CodeRestoreId::new();
        let applied = checkpoint::restore_worktree(
            &worktree,
            &resolved.commit,
            expected_tree,
            &checkpoint::restore_point_ref(workspace_id, resolved.session_id, restore_id),
            &format!("state before restore {restore_id}"),
        )
        .await
        .map_err(map_checkpoint)?;

        // Every session's next turn diffs from the restored state, not from
        // its own last checkpoint, so no turn is credited with the restore.
        let mut resume_refs = Vec::new();
        for session in list_sessions_for_workspace(&self.db, owner, workspace_id).await? {
            if session.lifecycle == SessionLifecycle::Ended {
                continue;
            }
            let newest = list_turns(&self.db, owner, session.id)
                .await?
                .iter()
                .map(|turn| turn.ordinal)
                .max()
                .unwrap_or(0);
            resume_refs.push(checkpoint::chain_resume_ref(
                workspace_id,
                session.id,
                newest,
            ));
        }
        if let Err(error) = checkpoint::continue_chains_after_restore(
            &worktree,
            &applied,
            &format!("restore {restore_id}"),
            &resume_refs,
        )
        .await
        {
            tracing::warn!(
                workspace = %workspace_id,
                %error,
                "the next turn's diff will include this restore"
            );
        }
        drop(claim);

        let actor = (caller != owner).then(|| TurnActor::principal(caller));
        self.journal_restore(
            owner,
            resolved.session_id,
            Event::CheckpointRestored {
                restore_id,
                target,
                diffstat: applied.files.stat.clone(),
                actor,
            },
        )
        .await;
        self.announce_files_changed(owner, caller, workspace_id);
        Ok(CheckpointRestoreOutcome {
            restore_id,
            target,
            session_id: resolved.session_id,
            files: applied.files,
        })
    }

    /// Undo one file's change, or one hunk of it, in the diff the person is
    /// reading: the workspace against its base when `turn_id` is `None`,
    /// otherwise that turn's own change.
    pub(crate) async fn revert_workspace_change(
        &self,
        owner: &OwnerId,
        caller: &OwnerId,
        workspace_id: WorkspaceId,
        turn_id: Option<TurnId>,
        path: &str,
        hunk: Option<HunkSelector<'_>>,
    ) -> Result<RevertedChange, ServerError> {
        refuse_sandbox(&self.get_workspace(owner, workspace_id).await?)?;
        let claim = self.claim_worktree(owner, workspace_id).await?;
        let (worktree, from, to, _) =
            checkpoint::resolve_diff_range(&self.db, &claim.workspace, turn_id)
                .await
                .map_err(map_checkpoint)?;
        let reverted = checkpoint::revert_change(&worktree, &from, &to, path, hunk)
            .await
            .map_err(map_checkpoint)?;
        drop(claim);
        self.announce_files_changed(owner, caller, workspace_id);
        Ok(reverted)
    }

    /// Put files back to the last commit, dropping their uncommitted changes.
    pub(crate) async fn discard_workspace_changes(
        &self,
        owner: &OwnerId,
        caller: &OwnerId,
        workspace_id: WorkspaceId,
        paths: &[String],
    ) -> Result<RevertedChange, ServerError> {
        refuse_sandbox(&self.get_workspace(owner, workspace_id).await?)?;
        let claim = self.claim_worktree(owner, workspace_id).await?;
        let discarded =
            checkpoint::discard_paths(std::path::Path::new(&claim.workspace.worktree_path), paths)
                .await
                .map_err(map_checkpoint)?;
        drop(claim);
        self.announce_files_changed(owner, caller, workspace_id);
        Ok(discarded)
    }

    /// Hold the worktree for one change, or say why it cannot be changed now.
    ///
    /// The write lock comes first and is waited for: saves and archive hold it
    /// only briefly. The turn lock is only tried, the order auto-recovery uses,
    /// so this never waits behind a turn and never deadlocks with one.
    async fn claim_worktree(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
    ) -> Result<WorktreeClaim, ServerError> {
        let write = self.workspace_write_lock(workspace_id).lock_owned().await;
        let turn = self
            .worktree_turn_lock(workspace_id)
            .try_lock_owned()
            .map_err(|_| turn_running())?;
        let workspace = self.require_live_workspace(owner, workspace_id).await?;
        for session in list_sessions_for_workspace(&self.db, owner, workspace_id).await? {
            if session.lifecycle == SessionLifecycle::Running {
                return Err(turn_running());
            }
            let engine_may_be_alive = session.lifecycle == SessionLifecycle::Fenced
                && session
                    .fence_reason
                    .as_ref()
                    .is_none_or(FenceReason::blocks_workspace);
            if engine_may_be_alive {
                return Err(ServerError::conflict_kind(
                    "workspace_fenced",
                    "An engine from before a restart may still be working in this workspace. \
                     Recover or end that session first.",
                ));
            }
        }
        Ok(WorktreeClaim {
            workspace,
            _write: write,
            _turn: turn,
        })
    }

    async fn resolve_restore_target(
        &self,
        owner: &OwnerId,
        workspace: &CodeWorkspace,
        target: CheckpointRestoreTarget,
    ) -> Result<ResolvedRestoreTarget, ServerError> {
        match target {
            CheckpointRestoreTarget::BeforeTurn { turn_id } => {
                let not_here = || ServerError::not_found("turn not found in this workspace");
                let turn = get_turn(&self.db, owner, turn_id)
                    .await?
                    .ok_or_else(not_here)?;
                let session = get_session(&self.db, owner, turn.session_id)
                    .await?
                    .ok_or_else(not_here)?;
                if session.workspace_id != Some(workspace.id) {
                    return Err(not_here());
                }
                if !matches!(
                    turn.status,
                    TurnStatus::Completed | TurnStatus::Failed | TurnStatus::Interrupted
                ) {
                    return Err(turn_running());
                }
                let commit = checkpoint::state_before_turn(&self.db, workspace, &turn)
                    .await
                    .map_err(map_checkpoint)?
                    .ok_or_else(|| {
                        ServerError::conflict_kind(
                            "no_checkpoint",
                            "Tidebreak has no checkpoint from before this turn, so it cannot \
                             restore it.",
                        )
                    })?;
                Ok(ResolvedRestoreTarget {
                    session_id: session.id,
                    commit,
                })
            }
            CheckpointRestoreTarget::BeforeRestore { restore_id } => {
                let (session_id, commit) = checkpoint::find_restore_point(
                    std::path::Path::new(&workspace.worktree_path),
                    workspace.id,
                    restore_id,
                )
                .await
                .map_err(map_checkpoint)?
                .ok_or_else(|| {
                    ServerError::not_found("that restore is no longer in this workspace")
                })?;
                Ok(ResolvedRestoreTarget { session_id, commit })
            }
        }
    }

    /// Journal a restore where its transcript shows it. The restore already
    /// happened, so a session that cannot take the row costs only the row.
    async fn journal_restore(&self, owner: &OwnerId, session_id: SessionId, event: Event) {
        let Ok(Some(session)) = get_session(&self.db, owner, session_id).await else {
            tracing::warn!(session = %session_id, "the restore was not journaled");
            return;
        };
        match tidebreak_core::db::code::append_event(
            &self.db,
            &session.owner,
            session.id,
            session.spawn_epoch,
            &event,
        )
        .await
        {
            Ok(seq) => self
                .bus
                .publish(session.id, tidebreak_core::SequencedEvent { seq, event }),
            Err(error) => tracing::warn!(
                session = %session.id,
                %error,
                "the restore was not journaled"
            ),
        }
    }

    /// Tell every view of the worktree to read it again, the way a save does.
    fn announce_files_changed(&self, owner: &OwnerId, caller: &OwnerId, workspace_id: WorkspaceId) {
        self.bus.publish_update(
            owner,
            crate::code::bus::CodeLiveUpdate::FilesChanged(workspace_id),
        );
        if caller != owner {
            self.bus.publish_update(
                caller,
                crate::code::bus::CodeLiveUpdate::FilesChanged(workspace_id),
            );
        }
    }
}

fn turn_running() -> ServerError {
    ServerError::conflict_kind(
        "turn_running",
        "A turn is running in this workspace. Wait for it to finish, or stop it, then try again.",
    )
}

fn refuse_sandbox(workspace: &CodeWorkspace) -> Result<(), ServerError> {
    if workspace.is_remote() {
        return Err(ServerError::conflict_kind(
            "workspace_remote",
            "This workspace runs in a sandbox. Ask the agent to make the change.",
        ));
    }
    Ok(())
}
