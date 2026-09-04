//! Remote and external sessions: creation, remote turns, and queue promotion.

use super::*;
use tidebreak_core::ExecutionLocation;

/// How long a machine-location message waits for its worker to promote the
/// queue head before the route answers `queued` (decision 0088).
const MACHINE_PROMOTION_POLL: std::time::Duration = std::time::Duration::from_millis(50);
const MACHINE_PROMOTION_POLLS: usize = 40;

impl CodeRuntime {
    /// Create a workspace whose checkout lives in a sandbox, not on this
    /// machine. A per-workspace `remote:<id>` worktree marker records that
    /// state ([`CodeWorkspace::is_remote`]); nothing here touches the
    /// filesystem.
    ///
    /// The authenticated remote-workspace route exposes this owner-scoped
    /// runtime path.
    pub(crate) async fn create_remote_workspace(
        &self,
        owner: &OwnerId,
        repo_id: RepoId,
        title: Option<String>,
    ) -> Result<CodeWorkspace, ServerError> {
        if self.remote.is_none() {
            return Err(ServerError::conflict_kind(
                "remote_disabled",
                "this deployment has no sandbox runtime configured",
            ));
        }
        let repo = self.get_repo(owner, repo_id).await?;
        Self::refuse_removed_repo(&repo)?;
        if repo.origin_host.is_none() || repo.origin_owner.is_none() || repo.origin_name.is_none() {
            return Err(ServerError::conflict_kind(
                "repo_origin_unknown",
                "the repository records no origin, so a sandbox cannot clone it",
            ));
        }
        let workspace = self.build_remote_workspace(owner, &repo, title).await?;
        insert_workspace(&self.db, &workspace).await?;
        Ok(workspace)
    }

    /// Validate and shape a remote workspace value without inserting it, so
    /// a caller can commit it atomically with the rows that depend on it.
    pub(super) async fn build_remote_workspace(
        &self,
        owner: &OwnerId,
        repo: &CodeRepo,
        title: Option<String>,
    ) -> Result<CodeWorkspace, ServerError> {
        let title = title
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        let id = WorkspaceId::new();
        let branch = branch_name(&repo.branch_prefix, &title, id.as_uuid());
        let existing = list_workspaces(&self.db, owner, Some(repo.id)).await?;
        if existing
            .iter()
            .any(|workspace| workspace.branch_name == branch)
        {
            return Err(ServerError::conflict_kind(
                "branch_collision",
                format!("branch {branch} already exists on this repository"),
            ));
        }
        Ok(CodeWorkspace {
            id,
            owner: owner.clone(),
            repo_id: repo.id,
            title,
            worktree_path: CodeWorkspace::remote_worktree_marker(id),
            branch_name: branch,
            base_ref: repo.default_base_ref.clone(),
            status: CodeWorkspaceStatus::Active,
            pr: None,
            created_at: Utc::now(),
            archived_at: None,
            released_at: None,
            released_tip: None,
            bundle_bytes: None,
        })
    }

    /// Shape a remote session value bound to `workspace`, uninserted.
    pub(super) fn remote_session_value(
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        harness: HarnessKind,
        settings: NewSessionSettings,
    ) -> Session {
        Session {
            id: SessionId::new(),
            owner: owner.clone(),
            workspace_id: Some(workspace_id),
            kind: SessionKind::Interactive,
            harness_kind: harness,
            harness_version: None,
            harness_resume_ref: None,
            permission_mode: settings.permission_mode,
            model: normalize_model(settings.model),
            reasoning_effort: settings.reasoning_effort,
            fast_mode: false,
            lifecycle: SessionLifecycle::Idle,
            fence_reason: None,
            child_pid: None,
            child_process_identity: None,
            spawn_epoch: 1,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: Utc::now(),
            execution_location: tidebreak_core::ExecutionLocation::Sandbox,
        }
    }

    /// Bind an external conversation to a session, creating the remote
    /// workspace, session, and binding together on first contact
    /// (docs/slack-sessions.md, stage 2).
    ///
    /// Idempotent across the channel's retries: a bound conversation
    /// answers with its session, an ended one answers `Ended` rather than
    /// resurrecting, and a binding under another grant refuses. Two racing
    /// creates converge on one session through the binding's unique
    /// conversation key.
    #[allow(clippy::too_many_arguments)]
    pub async fn external_get_or_create(
        &self,
        owner: &OwnerId,
        grant_id: tidebreak_core::CodeGrantId,
        channel_kind: &str,
        external_key: &str,
        repo_id: RepoId,
        title: Option<String>,
        harness: HarnessKind,
        settings: NewSessionSettings,
    ) -> Result<tidebreak_core::ExternalSessionResolution, ServerError> {
        if channel_kind.trim().is_empty() || external_key.trim().is_empty() {
            return Err(ServerError::conflict_kind(
                "binding_key_invalid",
                "a binding needs a channel kind and a conversation key",
            ));
        }
        // The fast path costs one read and builds nothing.
        if let Some(binding) = tidebreak_core::db::code::get_external_binding(
            &self.db,
            owner,
            channel_kind,
            external_key,
        )
        .await?
        {
            if binding.grant_id != grant_id {
                return Ok(tidebreak_core::ExternalSessionResolution::GrantMismatch);
            }
            let session = self.get_session(owner, binding.session_id).await?;
            if session.lifecycle == SessionLifecycle::Ended {
                return Ok(tidebreak_core::ExternalSessionResolution::Ended {
                    session_id: binding.session_id,
                });
            }
            return Ok(tidebreak_core::ExternalSessionResolution::Existing(
                Box::new(binding),
            ));
        }
        let repo = self.get_repo(owner, repo_id).await?;
        Self::refuse_removed_repo(&repo)?;
        match self.external_execution_location() {
            ExecutionLocation::Sandbox => {
                if repo.origin_host.is_none()
                    || repo.origin_owner.is_none()
                    || repo.origin_name.is_none()
                {
                    return Err(ServerError::conflict_kind(
                        "repo_origin_unknown",
                        "the repository records no origin, so a sandbox cannot clone it",
                    ));
                }
                let workspace = self.build_remote_workspace(owner, &repo, title).await?;
                let session = Self::remote_session_value(owner, workspace.id, harness, settings);
                Ok(tidebreak_core::db::code::resolve_external_session(
                    &self.db,
                    owner,
                    grant_id,
                    channel_kind,
                    external_key,
                    &workspace,
                    &session,
                )
                .await?)
            }
            ExecutionLocation::Machine => {
                // The machine's own engine: the ordinary local workspace and
                // session, then the binding. The channel's `Allow` is a
                // sandbox posture; on the machine the session takes the
                // deployment's default mode, and the owner decides approvals
                // from the desktop or the web until the channel can carry
                // them (decision 0088).
                let settings = NewSessionSettings {
                    permission_mode: tidebreak_core::PermissionMode::default(),
                    ..settings
                };
                let workspace = self
                    .create_workspace(owner, repo_id, title, None, None)
                    .await?;
                let session = self
                    .create_session(owner, workspace.id, harness, settings)
                    .await?;
                Ok(tidebreak_core::db::code::bind_external_session(
                    &self.db,
                    owner,
                    grant_id,
                    channel_kind,
                    external_key,
                    session.id,
                )
                .await?)
            }
        }
    }

    /// Where an external session runs on this deployment (decision 0088):
    /// a gateway sandbox when a sandbox runtime is configured, else the
    /// machine's own engine. The machine is the floor, not an interim path.
    #[must_use]
    pub fn external_execution_location(&self) -> ExecutionLocation {
        if self.remote.is_some() {
            ExecutionLocation::Sandbox
        } else {
            ExecutionLocation::Machine
        }
    }

    /// Create a session on a remote workspace: a row and nothing else. No
    /// local harness is probed or spawned — the sandbox carries the engine,
    /// and the first turn provisions it.
    ///
    /// The authenticated remote-session route exposes this owner-scoped
    /// runtime path.
    pub(crate) async fn create_remote_session(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        harness: HarnessKind,
        settings: NewSessionSettings,
    ) -> Result<Session, ServerError> {
        if self.remote.is_none() {
            return Err(ServerError::conflict_kind(
                "remote_disabled",
                "this deployment has no sandbox runtime configured",
            ));
        }
        let lifecycle = self.workspace_lifecycle_lock(workspace_id);
        let _lifecycle_guard = lifecycle.lock().await;
        let workspace = self.get_workspace(owner, workspace_id).await?;
        if !workspace.is_remote() {
            return Err(ServerError::conflict_kind(
                "workspace_not_remote",
                "this workspace has a local checkout; create a local session on it",
            ));
        }
        if workspace.status != CodeWorkspaceStatus::Active {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                format!("workspace is {}", workspace.status.as_str()),
            ));
        }
        let session = Self::remote_session_value(owner, workspace_id, harness, settings);
        insert_session(&self.db, &session).await?;
        Ok(session)
    }

    /// Submit one turn to a remote session's sandbox (`docs/slack-sessions.md`).
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn submit_remote_turn(
        &self,
        owner: &OwnerId,
        mut session: Session,
        workspace: &CodeWorkspace,
        message: String,
        model: Option<String>,
        reasoning_effort: Option<Option<ReasoningEffort>>,
        attachments: Vec<tidebreak_core::ImageRef>,
        trigger_delivery: Option<TriggerDeliveryClaim>,
        queue_if_busy: bool,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        let Some(remote) = self.remote_sessions() else {
            return Err(ServerError::conflict_kind(
                "remote_disabled",
                "this deployment has no sandbox runtime configured",
            ));
        };
        if trigger_delivery.is_some() {
            // Trigger delivery is at-most-once. The runtime's spawn and inbox
            // calls accept no idempotency key and expose no replay result, so
            // retrying an ambiguous response could run one trigger twice.
            return Err(ServerError::conflict_kind(
                "remote_triggers_unsupported",
                "remote trigger turns are disabled because sandbox spawn and inbox calls have no idempotency key; submit the turn manually",
            ));
        }
        if !attachments.is_empty() {
            // Remote messages carry text only. Until the runtime provides a
            // bounded, owner-scoped file transfer, attachment bytes have no
            // safe path into the sandbox.
            return Err(ServerError::conflict_kind(
                "remote_attachments_unsupported",
                "remote sessions cannot stage attachment bytes because the sandbox message contract carries text only; send the turn without attachments",
            ));
        }
        // Settings stick without local capability validation: the sandbox
        // engine reads them off the spawn, and no local harness answers for
        // a remote one.
        let mut next = SessionExecutionSettings::from(&session);
        if let Some(model) = normalize_model(model) {
            next.model = Some(model);
        }
        if let Some(effort) = reasoning_effort {
            next.reasoning_effort = effort;
        }
        if next != SessionExecutionSettings::from(&session) {
            session = replace_session_execution_settings(&self.db, owner, &session, &next)
                .await?
                .ok_or_else(|| {
                    ServerError::conflict_kind(
                        "session_settings_changed",
                        "the session settings changed before the turn could reserve them",
                    )
                })?;
        }
        // Queue-default, exactly as the local path: a busy session parks the
        // send as a durable row the remote sweep promotes at the next idle.
        let in_flight = session.lifecycle == SessionLifecycle::Running
            || get_open_turn(&self.db, owner, session.id).await?.is_some();
        let backlog = !tidebreak_core::db::code::list_queued_turns(&self.db, owner, session.id)
            .await?
            .is_empty();
        if in_flight || backlog {
            if !queue_if_busy {
                return Err(ServerError::conflict_kind(
                    "trigger_turn_busy",
                    "the turn was not accepted because the session is busy",
                ));
            }
            return self.park_remote_follow_up(owner, &session, message).await;
        }
        let repo = self.get_repo(owner, workspace.repo_id).await?;
        let driver = remote.driver(&self.db, self.bus.as_ref());
        let outcome = driver
            .submit_turn(&mut session, workspace, &repo, &message)
            .await?;
        // A provisioned or delivered turn has events to drain and a parked
        // one has a head to promote; either way the sweep should look now,
        // not at its next floor.
        remote.wake_sweep();
        self.relay_remote_outcome(owner, &session, outcome, message, queue_if_busy)
            .await
    }

    /// Translate a driver outcome into the submit answer the routes speak.
    pub(super) async fn relay_remote_outcome(
        &self,
        owner: &OwnerId,
        session: &Session,
        outcome: crate::code::remote::driver::RemoteTurnOutcome,
        message: String,
        queue_if_busy: bool,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        use crate::code::remote::driver::RemoteTurnOutcome as Outcome;
        match outcome {
            Outcome::Delivered { turn } | Outcome::Reincarnated { turn, .. } => {
                Ok(SubmitTurnOutcome::Ran(turn))
            }
            Outcome::TurnInFlight | Outcome::ReincarnationInFlight | Outcome::FlushPending => {
                if !queue_if_busy {
                    return Err(ServerError::conflict_kind(
                        "trigger_turn_busy",
                        "the turn was not accepted because the session is busy",
                    ));
                }
                self.park_remote_follow_up(owner, session, message).await
            }
            Outcome::CapExhausted { running } => Err(ServerError::conflict_kind(
                "sandbox_cap_exhausted",
                format!(
                    "the sandbox cap is full: {} session(s) hold the slots",
                    running.len()
                ),
            )),
            Outcome::SpendExhausted {
                spent_microusd,
                ceiling_microusd,
            } => Err(ServerError::conflict_kind(
                "session_spend_exhausted",
                format!(
                    "this session has spent {spent_microusd} of its {ceiling_microusd} micro-USD ceiling and takes no more turns"
                ),
            )),
            Outcome::SignInRequired => Err(ServerError::conflict_kind(
                "sign_in_required",
                "sign in to the sandbox environment, then retry",
            )),
        }
    }

    /// Park a message for a remote session. The remote sweep promotes the
    /// head at the next idle; there is no worker to nudge.
    pub(super) async fn park_remote_follow_up(
        &self,
        owner: &OwnerId,
        session: &Session,
        message: String,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        let queued = tidebreak_core::db::code::list_queued_turns(&self.db, owner, session.id)
            .await
            .map_err(ServerError::from)?;
        if queued.len() >= QueuedTurn::MAX_PER_SESSION {
            return Err(ServerError::conflict_kind(
                "queue_full",
                format!(
                    "this session may queue at most {} messages",
                    QueuedTurn::MAX_PER_SESSION
                ),
            ));
        }
        let now = chrono::Utc::now();
        let row = tidebreak_core::db::code::enqueue_queued_turn(
            &self.db,
            owner,
            &QueuedTurn {
                id: TurnId::new(),
                session_id: session.id,
                message,
                attachments: Vec::new(),
                position: 0,
                created_at: now,
                updated_at: now,
            },
        )
        .await
        .map_err(ServerError::from)?;
        if let Some(remote) = self.remote_sessions() {
            remote.wake_sweep();
        }
        Ok(SubmitTurnOutcome::Queued(Box::new(row)))
    }

    /// Promote the queue head of every idle remote session that has one.
    /// Called from the remote sweep; local sessions drain their own queues
    /// through their workers and are skipped here.
    pub(crate) async fn promote_remote_queue_heads(&self) -> Result<(), ServerError> {
        if self.remote.is_none() {
            return Ok(());
        }
        // Only sessions holding a queue can have a head to promote, so the
        // pass reads those rather than every session on the machine.
        for (owner, session_id) in
            tidebreak_core::db::code::sessions_with_queued_turns_all_owners(&self.db).await?
        {
            let Some(session) = get_session(&self.db, &owner, session_id).await? else {
                continue;
            };
            self.try_promote_remote_head(session).await?;
        }
        Ok(())
    }

    /// Promote a freshly recorded external message by the session's
    /// location (decision 0088). A sandbox session hands the head to its
    /// lease; a machine session wakes its worker, which drains the queue the
    /// way it does after any turn, and this waits briefly for the head to
    /// become a turn so the channel hears `new_turn` rather than `queued`
    /// for an idle session.
    async fn promote_external_head(
        &self,
        session: Session,
        turn_id: tidebreak_core::TurnId,
    ) -> Result<(), ServerError> {
        match session.execution_location {
            ExecutionLocation::Sandbox => self.try_promote_remote_head(session).await,
            ExecutionLocation::Machine => {
                let owner = session.owner.clone();
                let session_id = session.id;
                self.wake_session_queue(session_id);
                for _ in 0..MACHINE_PROMOTION_POLLS {
                    let still_queued =
                        tidebreak_core::db::code::list_queued_turns(&self.db, &owner, session_id)
                            .await?
                            .iter()
                            .any(|row| row.id == turn_id);
                    if !still_queued {
                        break;
                    }
                    tokio::time::sleep(MACHINE_PROMOTION_POLL).await;
                }
                Ok(())
            }
        }
    }

    /// Promote one idle remote session's queue head, when it has one and
    /// nothing holds promotion. Shared by the sweep and the external
    /// messages path, which tries the head immediately after enqueueing
    /// rather than waiting out a sweep tick.
    pub(super) async fn try_promote_remote_head(
        &self,
        mut session: Session,
    ) -> Result<(), ServerError> {
        let Some(remote) = self.remote_sessions() else {
            return Ok(());
        };
        if session.lifecycle != SessionLifecycle::Idle {
            return Ok(());
        }
        let Ok(Some(workspace)) = self.session_workspace(&session).await else {
            return Ok(());
        };
        if !workspace.is_remote() {
            return Ok(());
        }
        if tidebreak_core::db::code::queue_paused(&self.db, &session.owner, session.id).await? {
            return Ok(());
        }
        if remote.promotion_held(session.id) {
            return Ok(());
        }
        let Some(head) = queued_turn_head(&self.db, &session.owner, session.id).await? else {
            return Ok(());
        };
        let Ok(repo) = self.get_repo(&session.owner, workspace.repo_id).await else {
            return Ok(());
        };
        let driver = remote.driver(&self.db, self.bus.as_ref());
        let message = head.message.clone();
        use crate::code::remote::driver::RemoteTurnOutcome as Outcome;
        match driver
            .submit_turn_from(&mut session, &workspace, &repo, &message, Some(&head))
            .await
        {
            // The claim was the atomic promotion; nothing to delete. A
            // reincarnation has a fresh sandbox to pump, so the sweep
            // looks again now rather than at its next floor.
            Ok(Outcome::Delivered { .. }) | Ok(Outcome::Reincarnated { .. }) => {
                remote.clear_promotion_hold(session.id);
                remote.wake_sweep();
            }
            // Permanent for this session: nothing exposes a way to raise
            // the ceiling, and every retry would re-journal the refusal
            // and re-cancel the sandbox. Pause the queue so the tray
            // shows why nothing moves; unpausing retries deliberately.
            Ok(Outcome::SpendExhausted { .. }) => {
                let _ = tidebreak_core::db::code::set_queue_paused(
                    &self.db,
                    &session.owner,
                    session.id,
                    true,
                )
                .await;
            }
            // Transient machine-side refusals: hold retries so the
            // notice and attention do not repeat every sweep tick. The
            // hold expiring retries on its own once the slot may be
            // free or the owner has signed in.
            Ok(Outcome::CapExhausted { .. }) | Ok(Outcome::SignInRequired) => {
                remote.hold_promotion(session.id);
            }
            // Busy shapes: the row stays queued for the next idle.
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    session = %session.id,
                    %error,
                    "promoting a queued remote message failed; the row stays queued"
                );
                remote.hold_promotion(session.id);
            }
        }
        Ok(())
    }

    /// Take one external message for a bound session
    /// (`docs/slack-sessions.md`, stage 2).
    ///
    /// Idempotent across the channel's retries: the event id commits with
    /// the queue row it causes, and a replay derives its answer from that
    /// row's current state — still queued, promoted into a turn, or
    /// retracted — without writing a second row. An idle session promotes
    /// the head immediately; a busy one queues durably.
    pub async fn external_submit_message(
        &self,
        owner: &OwnerId,
        grant_id: tidebreak_core::CodeGrantId,
        session_id: SessionId,
        message: String,
        event_id: &str,
        channel_ts: &str,
    ) -> Result<ExternalMessageOutcome, ServerError> {
        if !tidebreak_core::db::code::session_bound_to_grant(&self.db, owner, session_id, grant_id)
            .await?
        {
            return Err(ServerError::conflict_kind(
                "grant_scope",
                "this grant holds no binding to that session",
            ));
        }
        let session = self.get_session(owner, session_id).await?;
        match session.lifecycle {
            SessionLifecycle::Ended => {
                return Err(ServerError::conflict_kind(
                    "session_ended",
                    "the bound session has ended; the conversation is closed",
                ));
            }
            SessionLifecycle::Fenced => {
                return Err(ServerError::conflict_kind(
                    "session_fenced",
                    "the bound session is fenced pending a reap",
                ));
            }
            _ => {}
        }
        let record = tidebreak_core::db::code::record_external_message(
            &self.db, owner, session_id, event_id, channel_ts, &message,
        )
        .await?;
        let (turn_id, fresh) = match &record {
            tidebreak_core::ExternalMessageRecord::Recorded(row) => (row.id, true),
            tidebreak_core::ExternalMessageRecord::Replay { turn_id } => (*turn_id, false),
        };
        if fresh {
            // Best effort: a refusal leaves the row queued for the sweep.
            let session = self.get_session(owner, session_id).await?;
            if let Err(error) = self.promote_external_head(session, turn_id).await {
                tracing::warn!(
                    session = %session_id,
                    ?error,
                    "promoting an external message failed; the row stays queued"
                );
            }
        }
        if let Some(row) = tidebreak_core::db::code::list_queued_turns(&self.db, owner, session_id)
            .await?
            .into_iter()
            .find(|row| row.id == turn_id)
        {
            return Ok(ExternalMessageOutcome::Queued(Box::new(row)));
        }
        if let Some(turn) = tidebreak_core::db::code::get_turn(&self.db, owner, turn_id).await? {
            return Ok(ExternalMessageOutcome::NewTurn(Box::new(turn)));
        }
        // The row the first delivery caused was retracted before it ran.
        Ok(ExternalMessageOutcome::Dropped)
    }

    pub(super) async fn interrupt_remote(&self, session: &Session) -> Result<(), ServerError> {
        let Some(remote) = self.remote_sessions() else {
            return Err(ServerError::conflict_kind(
                "remote_disabled",
                "this deployment has no sandbox runtime configured",
            ));
        };
        let Some(row) =
            tidebreak_core::db::code::latest_incarnation(&self.db, &session.owner, session.id)
                .await?
        else {
            return Err(ServerError::conflict_kind(
                "no_active_turn",
                "there is no active turn to interrupt",
            ));
        };
        if row.state != tidebreak_core::IncarnationState::Active {
            return Err(ServerError::conflict_kind(
                "no_active_turn",
                "there is no active turn to interrupt",
            ));
        }
        let Some(sandbox_id) = row.sandbox_id.as_deref() else {
            return Err(ServerError::conflict_kind(
                "no_active_turn",
                "there is no active turn to interrupt",
            ));
        };
        let message = crate::code::remote::wire::SandboxMessage {
            body: "stop".to_owned(),
            interrupt: true,
        };
        match remote
            .provisioner
            .send(&session.owner, sandbox_id, &message)
            .await
        {
            Ok(_) => Ok(()),
            Err(crate::code::remote::RemoteSandboxError::SignInRequired(_)) => {
                Err(ServerError::conflict_kind(
                    "sign_in_required",
                    "sign in to the sandbox environment, then retry",
                ))
            }
            Err(error) => Err(ServerError::internal(error.to_string())),
        }
    }

    /// Best-effort stop of a remote session's sandbox. Used when the session
    /// row is ending, so the environment does not keep spending.
    pub(super) async fn cancel_remote_sandbox(&self, session: &Session) {
        let Some(remote) = self.remote_sessions() else {
            return;
        };
        let Ok(Some(row)) =
            tidebreak_core::db::code::latest_incarnation(&self.db, &session.owner, session.id)
                .await
        else {
            return;
        };
        if let Some(sandbox_id) = row.sandbox_id.as_deref() {
            if let Err(error) = remote.provisioner.cancel(&session.owner, sandbox_id).await {
                tracing::warn!(
                    session = %session.id,
                    %error,
                    "could not cancel a remote sandbox while ending the session"
                );
            }
        }
        if row.state != tidebreak_core::IncarnationState::Stopped {
            let _ = tidebreak_core::db::code::stop_incarnation(
                &self.db,
                &session.owner,
                row.id,
                Some("session_ended"),
            )
            .await;
        }
    }
}
