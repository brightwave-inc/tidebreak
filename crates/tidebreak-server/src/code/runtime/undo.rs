//! Undo in the worktree: checkpoint restore, file and hunk revert, discard.
//!
//! All three change files in a workspace's live checkout, so they share one
//! gate. They run only between turns: the turn lock is taken without waiting,
//! and a busy lock is a refusal rather than a queue, because the person asked
//! to undo the worktree as they see it now. A session fenced for an engine
//! that may still be alive in the checkout also refuses them, as it refuses
//! turns (record 55). A sandbox workspace has no checkout here to change.
//!
//! The routes run each of them on a task of its own, so a client that goes
//! away mid-request cannot stop git halfway through the worktree.

use super::*;

use tidebreak_core::db::code::get_turn;
use tidebreak_core::{
    CheckpointRestoreStatus, CheckpointRestoreTarget, CodeRestoreId, HarnessKind, TurnStatus,
};

use crate::code::checkpoint::{
    self, BoundedFiles, HunkSelector, RestoreApplyError, RestorePreview, RestoredTo, RevertedChange,
};

/// A turn a restore undoes that its confirmation would not otherwise name: a
/// turn of another agent in the workspace, or any turn since the restore an
/// undo reverses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AffectedTurn {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub ordinal: i64,
    pub harness_kind: HarnessKind,
}

/// What a restore would undo, read before anything moves.
#[derive(Debug, Clone)]
pub struct CheckpointRestorePreview {
    pub target: CheckpointRestoreTarget,
    /// The session whose transcript records the restore.
    pub session_id: SessionId,
    pub preview: RestorePreview,
    pub affected_turns: Vec<AffectedTurn>,
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
    /// The target turn's ordinal, for a restore to before a turn.
    ordinal: Option<i64>,
}

/// The locks one worktree change holds until it drops.
struct WorktreeClaim {
    workspace: CodeWorkspace,
    _write: tokio::sync::OwnedMutexGuard<()>,
    _turn: tokio::sync::OwnedMutexGuard<()>,
}

/// The longest failure reason a restore row carries.
const MAX_RESTORE_ERROR_CHARS: usize = 600;

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
        let affected_turns = self
            .affected_turns(owner, &workspace, target, &resolved)
            .await?;
        Ok(CheckpointRestorePreview {
            target,
            session_id: resolved.session_id,
            preview,
            affected_turns,
        })
    }

    /// Put the worktree back to `target`, save what it replaces, and journal
    /// the restore in the transcript it belongs to.
    ///
    /// `expected_tree` is the `current_tree` the preview showed. A worktree
    /// that moved since is left alone, so the person never loses a change
    /// the confirmation did not list. The restore is journaled as started
    /// before any file moves, and again with how it ended, so its Undo is
    /// reachable even when it stops partway.
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
        let prepared = checkpoint::prepare_restore(
            &worktree,
            &resolved.commit,
            expected_tree,
            &checkpoint::restore_point_ref(workspace_id, resolved.session_id, restore_id),
            &format!("state before restore {restore_id}"),
        )
        .await
        .map_err(map_checkpoint)?;
        let actor = (caller != owner).then(|| TurnActor::principal(caller));
        let diffstat = prepared.files.stat.clone();
        let row =
            |status: CheckpointRestoreStatus, error: Option<String>| Event::CheckpointRestored {
                restore_id,
                target,
                diffstat: diffstat.clone(),
                actor: actor.clone(),
                status,
                error,
            };
        // Before any file moves, so the Undo is reachable whatever happens
        // next, a crash included.
        self.journal_restore(
            owner,
            resolved.session_id,
            row(CheckpointRestoreStatus::Started, None),
        )
        .await
        .map_err(|error| {
            ServerError::internal(format!(
                "the restore could not be recorded, so it did not run: {error}"
            ))
        })?;

        let applied = match prepared.apply(&worktree).await {
            Ok(applied) => applied,
            Err(failure) => {
                drop(claim);
                let (status, reason, reply) = match failure {
                    RestoreApplyError::Unchanged(error) => (
                        CheckpointRestoreStatus::Failed,
                        error.to_string(),
                        map_checkpoint(error),
                    ),
                    RestoreApplyError::RolledBack(reason) => (
                        CheckpointRestoreStatus::Failed,
                        reason,
                        ServerError::conflict_kind(
                            "restore_failed",
                            "Git could not finish the restore, so Tidebreak put back the files \
                             it had changed. Nothing changed.",
                        ),
                    ),
                    RestoreApplyError::Partial(reason) => (
                        CheckpointRestoreStatus::Partial,
                        reason,
                        ServerError::conflict_kind(
                            "restore_failed",
                            format!(
                                "The restore stopped partway. To put back every file it \
                                 replaced, undo it from the conversation, or run `tidebreak \
                                 code restore --ws {workspace_id} --undo {restore_id}`."
                            ),
                        ),
                    ),
                };
                if let Err(error) = self
                    .journal_restore(
                        owner,
                        resolved.session_id,
                        row(status, Some(bounded_reason(&reason))),
                    )
                    .await
                {
                    tracing::warn!(%restore_id, %error, "how the restore ended was not journaled");
                }
                self.announce_files_changed(owner, caller, workspace_id);
                return Err(reply);
            }
        };

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
        let restored_to = match resolved.ordinal {
            Some(ordinal) => RestoredTo::BeforeTurn {
                session_id: resolved.session_id,
                ordinal,
            },
            None => RestoredTo::BeforeRestore,
        };
        if let Err(error) = checkpoint::continue_chains_after_restore(
            &worktree,
            &applied,
            &checkpoint::chain_commit_message(restore_id, restored_to),
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

        if let Err(error) = self
            .journal_restore(
                owner,
                resolved.session_id,
                row(CheckpointRestoreStatus::Completed, None),
            )
            .await
        {
            tracing::warn!(%restore_id, %error, "the finished restore was not journaled");
        }
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
    ///
    /// `expected_tree` is the `worktree_tree` of the file list the person
    /// reviewed; a named file that changed since is left alone.
    pub(crate) async fn discard_workspace_changes(
        &self,
        owner: &OwnerId,
        caller: &OwnerId,
        workspace_id: WorkspaceId,
        paths: &[String],
        expected_tree: Option<&str>,
    ) -> Result<RevertedChange, ServerError> {
        refuse_sandbox(&self.get_workspace(owner, workspace_id).await?)?;
        let claim = self.claim_worktree(owner, workspace_id).await?;
        let discarded = checkpoint::discard_paths(
            std::path::Path::new(&claim.workspace.worktree_path),
            paths,
            expected_tree,
        )
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
                    ordinal: Some(turn.ordinal),
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
                Ok(ResolvedRestoreTarget {
                    session_id,
                    commit,
                    ordinal: None,
                })
            }
        }
    }

    /// The turns a restore undoes beyond what its confirmation already says.
    ///
    /// A restore to before a turn obviously undoes that turn and the later
    /// turns of the same agent. In a workspace several agents share, it also
    /// undoes their turns that ran since, which the confirmation must name.
    /// An undo reverses every turn that ran since its restore.
    async fn affected_turns(
        &self,
        owner: &OwnerId,
        workspace: &CodeWorkspace,
        target: CheckpointRestoreTarget,
        resolved: &ResolvedRestoreTarget,
    ) -> Result<Vec<AffectedTurn>, ServerError> {
        let sessions = list_sessions_for_workspace(&self.db, owner, workspace.id).await?;
        // When the target state was taken: the end of the turn before the
        // target turn, the session's start for its first turn, and for an
        // undo, when the restore saved it.
        let since = match resolved.ordinal {
            Some(ordinal) => {
                let previous = list_turns(&self.db, owner, resolved.session_id)
                    .await?
                    .into_iter()
                    .find(|turn| turn.ordinal == ordinal - 1);
                match previous {
                    Some(turn) => turn.ended_at.unwrap_or(turn.started_at),
                    None => sessions
                        .iter()
                        .find(|session| session.id == resolved.session_id)
                        .map(|session| session.created_at)
                        .unwrap_or_default(),
                }
            }
            None => {
                let seconds = checkpoint::commit_time(
                    std::path::Path::new(&workspace.worktree_path),
                    &resolved.commit,
                )
                .await
                .map_err(map_checkpoint)?;
                chrono::DateTime::from_timestamp(seconds, 0).unwrap_or_default()
            }
        };
        let mut affected = Vec::new();
        for session in &sessions {
            for turn in list_turns(&self.db, owner, session.id).await? {
                let implied = matches!(target, CheckpointRestoreTarget::BeforeTurn { .. })
                    && session.id == resolved.session_id;
                let ended = matches!(
                    turn.status,
                    TurnStatus::Completed | TurnStatus::Failed | TurnStatus::Interrupted
                );
                let changed_nothing = turn.diffstat.as_ref().is_some_and(|stat| {
                    stat.files == 0 && stat.insertions == 0 && stat.deletions == 0
                });
                if implied || !ended || changed_nothing || turn.started_at <= since {
                    continue;
                }
                affected.push((
                    turn.started_at,
                    AffectedTurn {
                        session_id: session.id,
                        turn_id: turn.id,
                        ordinal: turn.ordinal,
                        harness_kind: session.harness_kind,
                    },
                ));
            }
        }
        affected.sort_by_key(|(started_at, _)| *started_at);
        Ok(affected.into_iter().map(|(_, turn)| turn).collect())
    }

    /// Journal a restore row where its transcript shows it.
    async fn journal_restore(
        &self,
        owner: &OwnerId,
        session_id: SessionId,
        event: Event,
    ) -> Result<(), String> {
        let session = get_session(&self.db, owner, session_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "the session is gone".to_owned())?;
        let seq = tidebreak_core::db::code::append_event(
            &self.db,
            &session.owner,
            session.id,
            session.spawn_epoch,
            &event,
        )
        .await
        .map_err(|error| error.to_string())?;
        self.bus
            .publish(session.id, tidebreak_core::SequencedEvent { seq, event });
        Ok(())
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

fn bounded_reason(reason: &str) -> String {
    reason.chars().take(MAX_RESTORE_ERROR_CHARS).collect()
}

/// The refusal while a turn holds the worktree.
pub(super) fn turn_running() -> ServerError {
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
