//! Startup recovery: reconcile stored sessions and workspaces with the host.

use super::*;

impl CodeRuntime {
    /// Revoke the session browser token and permanently tombstone its native
    /// adapter authority. Idempotent — safe to call multiple times.
    ///
    /// Terminal paths pass the database-backed session scope explicitly so
    /// the adapter is still tombstoned when a prior launch failure already
    /// removed the transient bearer token and capfile.
    pub(super) fn revoke_browser_session(&self, session: &CodeSession) {
        self.browser_tokens.revoke(session.id);
        self.approvals.revoke_session(session.id);
        if let Some(relay) = &self.harness_llm {
            relay.revoke(session.id);
        }
        if let (Some(runtime), Some(workspace)) = (&self.browser_runtime, session.workspace_id) {
            let scope = crate::code::browser_runtime::BrowserRuntimeScope {
                owner: session.owner.clone(),
                workspace,
                session: session.id,
            };
            runtime.revoke_session(&scope);
        }
    }

    /// Revoke only the outgoing worker's transient browser and approval channels.
    ///
    /// Reap and launch-failure paths replace a worker while preserving the
    /// same logical code session. Its native browser capability therefore
    /// stays live and can be reused by the fresh channel. Only terminal end
    /// paths call [`Self::revoke_browser_session`] and plant the adapter's
    /// enduring tombstone.
    pub(super) fn revoke_worker_channels(&self, session_id: CodeSessionId) {
        self.browser_tokens.revoke(session_id);
        self.approvals.revoke_session(session_id);
        if let Some(relay) = &self.harness_llm {
            relay.revoke(session_id);
        }
    }

    pub(crate) async fn recover(&self) -> Result<Vec<RecoveryAction>, ServerError> {
        self.ensure_stall_sweep();
        let mut actions = Vec::new();
        let mut recovery_owners = list_sessions_all_owners(&self.db)
            .await?
            .into_iter()
            .map(|session| session.owner)
            .collect::<Vec<_>>();
        recovery_owners.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        recovery_owners.dedup();
        for owner in &recovery_owners {
            for pending in list_pending_permission_mode_changes(&self.db, owner).await? {
                let reason = FenceReason::ProbeAmbiguous {
                    detail: format!(
                        "permission mode change from {} to {} stopped before revision {} committed",
                        pending.intent.previous_mode,
                        pending.intent.requested_mode,
                        pending.intent.revision
                    ),
                };
                if let Some(fenced) =
                    fence_permission_mode_change(&self.db, owner, &pending.intent, &reason).await?
                {
                    crate::code::attention::emit_digest(&self.db, &self.bus, &fenced).await;
                    actions.push(RecoveryAction::Fenced {
                        session: fenced.id.to_string(),
                    });
                } else {
                    let _ =
                        discard_permission_mode_change(&self.db, owner, &pending.intent).await?;
                }
            }
        }
        actions.extend(
            recovery::recover_running_sessions(&self.db, &self.bus)
                .await
                .map_err(ServerError::from)?,
        );
        for workspace in
            list_workspaces_by_status_all_owners(&self.db, CodeWorkspaceStatus::Archiving).await?
        {
            let sessions =
                list_sessions_for_workspace(&self.db, &workspace.owner, workspace.id).await?;
            if sessions.iter().any(|session| {
                matches!(
                    session.lifecycle,
                    CodeSessionLifecycle::Running | CodeSessionLifecycle::Fenced
                )
            }) {
                tracing::warn!(
                    workspace = %workspace.id,
                    "code-mode: archive recovery kept lifecycle exclusion because a worker may still exist"
                );
                continue;
            }
            if !std::path::Path::new(&workspace.worktree_path).exists() {
                let repo = self.get_repo(&workspace.owner, workspace.repo_id).await?;
                match self.finalize_removed_workspace(workspace, &repo).await {
                    Ok(workspace) => self.forget_workspace_turn_lock(workspace.id),
                    Err(error) => {
                        tracing::warn!(
                            "code-mode: archive recovery could not finalize a missing checkout: {}",
                            error.message()
                        );
                    }
                }
                continue;
            }
            if !compare_and_set_workspace_status(
                &self.db,
                &workspace.owner,
                workspace.id,
                CodeWorkspaceStatus::Archiving,
                CodeWorkspaceStatus::Active,
            )
            .await?
            {
                tracing::warn!(
                    workspace = %workspace.id,
                    "code-mode: archive recovery lost its lifecycle compare-and-set"
                );
            }
        }
        let recovered_sessions = list_sessions_all_owners(&self.db).await?;
        for session in &recovered_sessions {
            crate::code::approval_sweep::abandon_for_restart(
                &self.db,
                &self.bus,
                &session.owner,
                session.id,
                session.spawn_epoch,
            )
            .await;
            self.refresh_approval_attention(&session.owner, session.id)
                .await;
        }
        // Do not sweep repository-wide checkpoint refs here. Another
        // Tidebreak profile can manage the same repository from a separate
        // database, so this process cannot identify global orphans safely.
        // Recovery only mutates rows. Re-attach a worker for every session
        // that is still usable so submit_turn is not stuck after a restart.
        // Concurrently: each attach launches an engine child, so a serial pass
        // charged app launch the sum of every restored session.
        // Remote sessions have no local worker to re-attach: their engine
        // lives in a sandbox, and spawning a host harness against the empty
        // worktree path would fail loudly for a session that is healthy.
        let mut remote_workspaces = std::collections::HashSet::new();
        let mut checked_workspaces = std::collections::HashSet::new();
        for session in &recovered_sessions {
            if !checked_workspaces.insert(session.workspace_id) {
                continue;
            }
            if let Ok(Some(workspace)) = self.session_workspace(session).await {
                if workspace.is_remote() {
                    remote_workspaces.insert(session.workspace_id);
                }
            }
        }
        let resumable: Vec<CodeSession> = recovered_sessions
            .into_iter()
            .filter(|session| {
                !matches!(
                    session.lifecycle,
                    CodeSessionLifecycle::Ended | CodeSessionLifecycle::Fenced
                ) && !remote_workspaces.contains(&session.workspace_id)
                    && !self
                        .workers
                        .lock()
                        .expect("code workers")
                        .contains_key(&session.id)
            })
            .collect();
        futures::future::join_all(resumable.into_iter().map(|session| async move {
            if let Err(error) = self.attach_and_spawn_worker(session).await {
                tracing::warn!(
                    "code-mode: could not resume a recovered session worker: {}",
                    error.message()
                );
            }
        }))
        .await;
        Ok(actions)
    }
}
