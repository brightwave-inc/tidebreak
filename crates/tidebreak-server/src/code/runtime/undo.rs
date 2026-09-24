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
        let (begun, claim, prepared) = self
            .begin_restore(owner, caller, workspace_id, target, expected_tree)
            .await?;
        let BegunRestore {
            worktree,
            restore_id,
            session_id,
            restored_to,
            ..
        } = &begun;
        let (worktree, restore_id, session_id, restored_to) =
            (worktree.clone(), *restore_id, *session_id, *restored_to);
        let saved_oid = prepared.saved_oid.clone();
        let applied = match prepared.apply(&worktree).await {
            Ok(applied) => applied,
            Err(failure) => {
                if matches!(failure, RestoreApplyError::Partial(_)) {
                    // Some files moved: the next turn diffs from what is
                    // really there and hears which files moved.
                    self.continue_chains_to_worktree(
                        owner,
                        workspace_id,
                        &worktree,
                        restore_id,
                        restored_to,
                        &saved_oid,
                    )
                    .await;
                }
                drop(claim);
                let (status, reason, reply) = match failure {
                    // Every path was verified to hold what it held before:
                    // the one case the row may say nothing changed.
                    RestoreApplyError::NothingChanged {
                        reason,
                        changed: true,
                    } => (
                        CheckpointRestoreStatus::Failed,
                        reason.clone(),
                        ServerError::conflict_kind(
                            "worktree_changed",
                            format!(
                                "{reason} Tidebreak put back every file it had changed, so \
                                 nothing changed. Review the restore again."
                            ),
                        ),
                    ),
                    RestoreApplyError::NothingChanged {
                        reason,
                        changed: false,
                    } => (
                        CheckpointRestoreStatus::Failed,
                        reason.clone(),
                        ServerError::conflict_kind(
                            "restore_failed",
                            format!(
                                "{reason} Tidebreak put back every file it had changed, so \
                                 nothing changed."
                            ),
                        ),
                    ),
                    RestoreApplyError::Partial(reason) => (
                        CheckpointRestoreStatus::Partial,
                        reason.clone(),
                        ServerError::conflict_kind(
                            "restore_failed",
                            format!(
                                "The restore stopped partway: {reason} To put back every file it \
                                 replaced, undo it from the conversation, or run `tidebreak \
                                 code restore --ws {workspace_id} --undo {restore_id}`."
                            ),
                        ),
                    ),
                };
                if let Err(error) = self
                    .journal_restore(
                        owner,
                        session_id,
                        begun.row(status, Some(bounded_reason(&reason))),
                    )
                    .await
                {
                    tracing::warn!(%restore_id, %error, "how the restore ended was not journaled");
                } else {
                    forget_restore_in_flight(begun.in_flight);
                }
                self.announce_files_changed(owner, caller, workspace_id);
                return Err(reply);
            }
        };

        // Every session's next turn diffs from the restored state, not from
        // its own last checkpoint, so no turn is credited with the restore.
        let resume_refs = self.resume_refs(owner, workspace_id).await?;
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
                session_id,
                begun.row(CheckpointRestoreStatus::Completed, None),
            )
            .await
        {
            tracing::warn!(%restore_id, %error, "the finished restore was not journaled");
        } else {
            forget_restore_in_flight(begun.in_flight);
        }
        self.announce_files_changed(owner, caller, workspace_id);
        Ok(CheckpointRestoreOutcome {
            restore_id,
            target,
            session_id,
            files: applied.files,
        })
    }

    /// Claim the worktree, save the state a restore replaces, and journal the
    /// restore as started. No file has moved yet.
    async fn begin_restore(
        &self,
        owner: &OwnerId,
        caller: &OwnerId,
        workspace_id: WorkspaceId,
        target: CheckpointRestoreTarget,
        expected_tree: Option<&str>,
    ) -> Result<(BegunRestore, WorktreeClaim, checkpoint::PreparedRestore), ServerError> {
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
        let mut begun = BegunRestore {
            worktree,
            restore_id,
            target,
            session_id: resolved.session_id,
            restored_to: match resolved.ordinal {
                Some(ordinal) => RestoredTo::BeforeTurn {
                    session_id: resolved.session_id,
                    ordinal,
                },
                None => RestoredTo::BeforeRestore,
            },
            diffstat: prepared.files.stat.clone(),
            actor: (caller != owner).then(|| TurnActor::principal(caller)),
            in_flight: None,
        };
        // A record the next boot finds if this process never gets to the
        // restore's last row.
        begun.in_flight = self.note_restore_in_flight(&RestoreInFlight {
            owner: owner.clone(),
            workspace_id,
            session_id: begun.session_id,
            restore_id,
            ordinal: resolved.ordinal,
            saved_oid: prepared.saved_oid.clone(),
            started: begun.row(CheckpointRestoreStatus::Started, None),
        });
        // Before any file moves, so the Undo is reachable whatever happens
        // next, a crash included.
        if let Err(error) = self
            .journal_restore(
                owner,
                begun.session_id,
                begun.row(CheckpointRestoreStatus::Started, None),
            )
            .await
        {
            forget_restore_in_flight(begun.in_flight.take());
            return Err(ServerError::internal(format!(
                "the restore could not be recorded, so it did not run: {error}"
            )));
        }
        Ok((begun, claim, prepared))
    }

    /// A restore stopped before step `killed_at` the way a killed process
    /// stops: journaled as started, some files moved, nothing after. For
    /// tests of what the next boot does with it.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn restore_checkpoint_killed_at(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        target: CheckpointRestoreTarget,
        killed_at: usize,
    ) -> Result<CodeRestoreId, ServerError> {
        let (begun, _claim, prepared) = self
            .begin_restore(owner, owner, workspace_id, target, None)
            .await?;
        prepared
            .apply_until_killed(&begun.worktree, killed_at)
            .await;
        Ok(begun.restore_id)
    }

    /// The chain ref each open session's next turn resumes from.
    async fn resume_refs(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<String>, ServerError> {
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
        Ok(resume_refs)
    }

    /// Point every open session's chain at the worktree as it stands now,
    /// after a restore that did not finish, so the next turn diffs from what
    /// is really there and hears which files moved.
    async fn continue_chains_to_worktree(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        worktree: &std::path::Path,
        restore_id: CodeRestoreId,
        restored_to: RestoredTo,
        saved_oid: &str,
    ) {
        let continued = async {
            let resume_refs = self
                .resume_refs(owner, workspace_id)
                .await
                .map_err(|error| error.message().to_owned())?;
            let tree = checkpoint::snapshot_tree(worktree)
                .await
                .map_err(|error| error.to_string())?;
            checkpoint::continue_chains_to(
                worktree,
                &tree,
                saved_oid,
                &checkpoint::chain_commit_message(restore_id, restored_to),
                &resume_refs,
            )
            .await
            .map_err(|error| error.to_string())
        };
        if let Err(error) = continued.await {
            tracing::warn!(
                workspace = %workspace_id,
                %restore_id,
                %error,
                "the next turn's diff may include this unfinished restore"
            );
        }
    }

    /// Record a restore in flight, so the next boot finds it if this process
    /// stops before the restore's last row. `None` when the record could not
    /// be written; the restore still runs, and its started row keeps its Undo.
    fn note_restore_in_flight(&self, restore: &RestoreInFlight) -> Option<std::path::PathBuf> {
        let folder = restores_in_flight_folder(&self.data_dir);
        let path = folder.join(format!("{}.json", restore.restore_id));
        let written = std::fs::create_dir_all(&folder)
            .map_err(|error| error.to_string())
            .and_then(|()| serde_json::to_vec(restore).map_err(|error| error.to_string()))
            .and_then(|bytes| {
                let temp = folder.join(format!(".{}.tmp", restore.restore_id));
                std::fs::write(&temp, bytes)
                    .and_then(|()| std::fs::rename(&temp, &path))
                    .map_err(|error| error.to_string())
            });
        match written {
            Ok(()) => Some(path),
            Err(error) => {
                tracing::warn!(
                    restore_id = %restore.restore_id,
                    %error,
                    "a crash during this restore will not be noticed at the next boot"
                );
                None
            }
        }
    }

    /// Finish what the last process left: journal every restore it never
    /// finished as stopped partway, with its Undo, and point each open
    /// session's chain at the files as they stand; then clear every local
    /// worktree's staging folder of temporary files a crash left.
    ///
    /// Runs at boot, before any worker attaches, so no turn starts from a
    /// chain that still describes the files before the restore.
    pub(super) async fn finish_interrupted_restores(&self) {
        let folder = restores_in_flight_folder(&self.data_dir);
        let entries = std::fs::read_dir(&folder)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"));
        for path in entries.collect::<Vec<_>>() {
            let restore = match std::fs::read(&path)
                .map_err(|error| error.to_string())
                .and_then(|bytes| {
                    serde_json::from_slice::<RestoreInFlight>(&bytes)
                        .map_err(|error| error.to_string())
                }) {
                Ok(restore) => restore,
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "an unreadable restore record");
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
            };
            self.finish_interrupted_restore(&restore).await;
            let _ = std::fs::remove_file(&path);
        }
        match list_workspaces_all_owners(&self.db).await {
            Ok(workspaces) => {
                for workspace in workspaces {
                    if !workspace.is_remote() && !workspace.worktree_path.is_empty() {
                        checkpoint::clear_staging_folder(std::path::Path::new(
                            &workspace.worktree_path,
                        ));
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, "code-mode: could not list workspaces to clear staging");
            }
        }
    }

    async fn finish_interrupted_restore(&self, restore: &RestoreInFlight) {
        let Event::CheckpointRestored {
            restore_id,
            target,
            diffstat,
            actor,
            ..
        } = restore.started.clone()
        else {
            return;
        };
        let restored_to = match restore.ordinal {
            Some(ordinal) => RestoredTo::BeforeTurn {
                session_id: restore.session_id,
                ordinal,
            },
            None => RestoredTo::BeforeRestore,
        };
        let workspace = get_workspace(&self.db, &restore.owner, restore.workspace_id)
            .await
            .ok()
            .flatten()
            .filter(|workspace| {
                !workspace.is_remote() && std::path::Path::new(&workspace.worktree_path).exists()
            });
        if let Some(workspace) = workspace {
            let _write = self.workspace_write_lock(workspace.id).lock_owned().await;
            if let Ok(_turn) = self.worktree_turn_lock(workspace.id).try_lock_owned() {
                self.continue_chains_to_worktree(
                    &restore.owner,
                    workspace.id,
                    std::path::Path::new(&workspace.worktree_path),
                    restore_id,
                    restored_to,
                    &restore.saved_oid,
                )
                .await;
            }
        }
        let row = Event::CheckpointRestored {
            restore_id,
            target,
            diffstat,
            actor,
            status: CheckpointRestoreStatus::Partial,
            error: Some(
                "Tidebreak quit before this restore finished, so some files may not have moved. \
                 Undo puts back every file it replaced."
                    .to_owned(),
            ),
        };
        if let Err(error) = self
            .journal_restore(&restore.owner, restore.session_id, row)
            .await
        {
            tracing::warn!(%restore_id, %error, "an unfinished restore was not journaled");
        }
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
                let commit = checkpoint::state_before_turn(workspace, &turn)
                    .await
                    .map_err(map_checkpoint)?
                    .ok_or_else(|| {
                        ServerError::conflict_kind(
                            "no_checkpoint",
                            "Tidebreak no longer has the checkpoint from just before this turn, \
                             so it cannot restore it. Restoring to an older one would also undo \
                             turns you did not pick.",
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

/// A restore that is saved and journaled as started, with no file moved yet.
struct BegunRestore {
    worktree: std::path::PathBuf,
    restore_id: CodeRestoreId,
    target: CheckpointRestoreTarget,
    /// The session whose transcript records the restore.
    session_id: SessionId,
    restored_to: RestoredTo,
    diffstat: Diffstat,
    actor: Option<TurnActor>,
    /// The record the next boot finds if this process stops first.
    in_flight: Option<std::path::PathBuf>,
}

impl BegunRestore {
    /// The restore's row in its transcript, with how far it got.
    fn row(&self, status: CheckpointRestoreStatus, error: Option<String>) -> Event {
        Event::CheckpointRestored {
            restore_id: self.restore_id,
            target: self.target,
            diffstat: self.diffstat.clone(),
            actor: self.actor.clone(),
            status,
            error,
        }
    }
}

/// A restore that has started and not yet journaled how it ended, as the
/// next boot needs it if this process stops first.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct RestoreInFlight {
    owner: OwnerId,
    workspace_id: WorkspaceId,
    /// The session whose transcript records the restore.
    session_id: SessionId,
    restore_id: CodeRestoreId,
    /// The target turn's ordinal, for a restore to before a turn.
    ordinal: Option<i64>,
    /// The commit that holds the state the restore replaces.
    saved_oid: String,
    /// The row journaled as started.
    started: Event,
}

/// `{data_dir}/code/restores`: one record per restore in flight.
fn restores_in_flight_folder(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("code").join("restores")
}

/// Drop the record of a restore whose last row is journaled.
fn forget_restore_in_flight(record: Option<std::path::PathBuf>) {
    if let Some(path) = record {
        let _ = std::fs::remove_file(path);
    }
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
