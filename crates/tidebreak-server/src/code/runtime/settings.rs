//! Session execution settings: permission mode, reasoning effort, fast mode, and model capability checks.

use super::*;

pub(super) enum LivePermissionModeOutcome {
    Unavailable,
    RelaunchRequired,
    Acknowledged(LivePermissionModeChange),
}

pub(super) struct LivePermissionModeChange {
    settlement: oneshot::Sender<PermissionModeSettlement>,
    handle: WorkerHandle,
}

pub(super) struct SelectedModelCapabilities {
    reasoning_efforts: Vec<ReasoningEffort>,
    /// The selected row's own ladder, when the engine listed that row.
    listed_model_reasoning_efforts: Option<Vec<ReasoningEffort>>,
    reasoning_known: bool,
    fast_mode: bool,
    fast_mode_known: bool,
}

impl SelectedModelCapabilities {
    pub(super) fn supports_reasoning(&self, effort: ReasoningEffort) -> bool {
        self.reasoning_efforts.contains(&effort)
    }

    pub(super) fn deactivate_unsupported(&self, settings: &mut SessionExecutionSettings) {
        if self.reasoning_known
            && settings
                .reasoning_effort
                .is_some_and(|effort| !self.supports_reasoning(effort))
        {
            settings.reasoning_effort = None;
        }
        if self.fast_mode_known && settings.fast_mode && !self.fast_mode {
            settings.fast_mode = false;
        }
    }
}

pub(super) fn normalize_model(model: Option<String>) -> Option<String> {
    model.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    })
}

pub(super) fn offered_permission_modes(caps: &tidebreak_core::HarnessCaps) -> Vec<PermissionMode> {
    PermissionMode::ALL
        .iter()
        .copied()
        .filter(|&mode| honors_permission_mode(mode, caps))
        .collect()
}

pub(super) fn honors_permission_mode(
    mode: PermissionMode,
    caps: &tidebreak_core::HarnessCaps,
) -> bool {
    // Each mode stands on its own capability flag (decision 0038): Auto is
    // never derived from the approval channel, so an engine whose only
    // honest posture is unsupervised Auto can still be driven.
    match mode {
        PermissionMode::Plan => caps.plan_mode == CapLevel::Supported,
        PermissionMode::Ask => caps.structured_approvals == CapLevel::Supported,
        PermissionMode::Auto => caps.auto_mode == CapLevel::Supported,
        PermissionMode::Allow => caps.allow_mode == CapLevel::Supported,
    }
}

pub(super) fn refuse_ceiling_with_no_offered_mode(
    ceiling: Option<PermissionMode>,
    harness: HarnessKind,
    caps: &tidebreak_core::HarnessCaps,
) -> Result<(), ServerError> {
    let Some(ceiling) = ceiling else {
        return Ok(());
    };
    if !ManagedPolicy::permission_mode_ceiling_excludes_all(
        Some(ceiling),
        offered_permission_modes(caps),
    ) {
        return Ok(());
    }
    Err(ServerError::conflict_kind(
        "permission_mode_locked",
        format!(
            "{harness} offers no permission mode at or below the managed ceiling (`{}`)",
            ceiling.as_str()
        ),
    ))
}

pub(super) fn refuse_unhonored_mode(
    harness: HarnessKind,
    mode: PermissionMode,
    caps: &tidebreak_core::HarnessCaps,
) -> Result<(), ServerError> {
    if honors_permission_mode(mode, caps) {
        return Ok(());
    }
    let reason = match mode {
        PermissionMode::Plan => format!("{harness} cannot honor plan mode"),
        PermissionMode::Ask => format!(
            "{harness} cannot honor {mode}: structured approvals are {}",
            caps.structured_approvals.as_str()
        ),
        PermissionMode::Auto => format!(
            "{harness} cannot honor {mode}: an auto posture is {}",
            caps.auto_mode.as_str()
        ),
        PermissionMode::Allow => format!(
            "{harness} cannot honor {mode}: an allow-all posture is {}",
            caps.allow_mode.as_str()
        ),
    };
    Err(ServerError::unprocessable_kind(
        "permission_mode_unavailable",
        reason,
    ))
}

pub(super) fn permission_mode_fence_reason(intent: &PermissionModeChangeIntent) -> FenceReason {
    FenceReason::ProbeAmbiguous {
        detail: format!(
            "permission mode change from {} to {} reached the engine but revision {} did not commit",
            intent.previous_mode, intent.requested_mode, intent.revision
        ),
    }
}

#[derive(Clone, Copy)]
pub(super) enum ModelCredentialScope {
    Session(SessionId),
    Grant(tidebreak_core::CodeGrantId),
}

impl CodeRuntime {
    /// Change a session's permission mode through one durable intent.
    ///
    /// A live worker stays inside the mode-change command after native
    /// acknowledgement. It cannot accept another turn until the exact owner,
    /// lifecycle, worker epoch, prior mode, and revision confirm in storage.
    /// If confirmation fails, the worker stops before this method returns.
    ///
    /// Refused while a turn is running. A turn that began under one posture
    /// must not have it changed underneath it — that is the whole point of
    /// the posture.
    pub(crate) async fn set_permission_mode(
        &self,
        owner: &OwnerId,
        id: SessionId,
        mode: PermissionMode,
    ) -> Result<Session, ServerError> {
        let session = self.get_session(owner, id).await?;
        if session.permission_mode == mode {
            return Ok(session);
        }
        match session.lifecycle {
            SessionLifecycle::Running => {
                return Err(ServerError::conflict_kind(
                    "turn_running",
                    "finish or interrupt the running turn before changing the permission mode",
                ));
            }
            SessionLifecycle::Ended => {
                return Err(ServerError::conflict_kind(
                    "session_ended",
                    "this session has ended; start a new one to pick a different mode",
                ));
            }
            _ => {}
        }

        let workspace = self.session_workspace(&session).await?;
        if workspace
            .as_ref()
            .is_some_and(|workspace| workspace.is_remote())
        {
            // The sandbox carries the engine. Persist the mode on the row and
            // do not relaunch a host harness against the empty worktree.
            let mut session = session;
            session.permission_mode = mode;
            crate::code::attention::persist_session(&self.db, &self.bus, &session).await?;
            return Ok(session);
        }

        // Refuse a mode this engine cannot honor here, not at the next turn,
        // and with the same rule session creation applies (decision 0038).
        let adapter = self.adapter(session.harness_kind)?;
        let probe = self.probe(adapter.as_ref()).await;
        let caps = adapter.capabilities(&probe);
        refuse_unhonored_mode(session.harness_kind, mode, &caps)?;

        let intent = begin_permission_mode_change(&self.db, owner, &session, mode)
            .await?
            .ok_or_else(|| {
                ServerError::conflict_kind(
                    "permission_mode_changed",
                    "the session changed before the permission mode could be reserved",
                )
            })?;

        let live = match self
            .repostured_in_place(id, intent.worker_epoch, mode)
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                match cancel_permission_mode_change(&self.db, owner, &intent).await {
                    Ok(true) => return Err(error),
                    Ok(false) => {
                        self.retire_permission_mode_worker(&intent).await;
                        let _ = discard_permission_mode_change(&self.db, owner, &intent).await;
                    }
                    Err(cancel_error) => {
                        self.retire_permission_mode_worker(&intent).await;
                        tracing::warn!(
                            session = %id,
                            error = %cancel_error,
                            "could not cancel a failed permission-mode intent"
                        );
                    }
                }
                return Err(error);
            }
        };

        if let LivePermissionModeOutcome::Acknowledged(change) = live {
            return match confirm_permission_mode_change(&self.db, owner, &intent).await {
                Ok(true) => {
                    if change
                        .settlement
                        .send(PermissionModeSettlement::Confirmed)
                        .is_err()
                    {
                        self.retire_permission_mode_worker(&intent).await;
                        let session = self.get_session(owner, id).await?;
                        return self.attach_and_spawn_worker(session).await;
                    }
                    let session = self.get_session(owner, id).await?;
                    self.note_permission_mode(
                        owner,
                        &session,
                        intent.previous_mode,
                        mode,
                        intent.revision,
                    )
                    .await;
                    Ok(session)
                }
                Ok(false) => {
                    let fenced = self
                        .stop_and_fence_permission_mode_change(owner, &intent, change)
                        .await?;
                    if fenced.is_some() {
                        Err(ServerError::conflict_kind(
                            "permission_mode_unconfirmed",
                            "the engine accepted the permission mode, but the durable session changed before confirmation; reap the fenced session before another turn",
                        ))
                    } else {
                        Err(ServerError::conflict_kind(
                            "permission_mode_changed",
                            "the engine accepted the permission mode, but a newer session state superseded it",
                        ))
                    }
                }
                Err(error) => {
                    if let Err(fence_error) = self
                        .stop_and_fence_permission_mode_change(owner, &intent, change)
                        .await
                    {
                        tracing::warn!(
                            session = %id,
                            error = %fence_error.message(),
                            "could not persist the permission-mode failure fence"
                        );
                    }
                    Err(ServerError::from(error))
                }
            };
        }

        // A relaunch is the fallback, and it only works where rebuilding the
        // launch plan carries the mode. An engine that fixed its posture when
        // it created the session, and cannot re-apply it on resume, would come
        // back running the old one while the record claimed the new one.
        if !adapter.relaunch_composes_permission_mode() && session.harness_resume_ref.is_some() {
            let cancelled = matches!(
                cancel_permission_mode_change(&self.db, owner, &intent).await,
                Ok(true)
            );
            if !cancelled {
                self.retire_permission_mode_worker(&intent).await;
                let reason = permission_mode_fence_reason(&intent);
                if let Some(fenced) =
                    fence_permission_mode_change(&self.db, owner, &intent, &reason).await?
                {
                    crate::code::attention::emit_digest(&self.db, &self.bus, &fenced).await;
                } else {
                    let _ = discard_permission_mode_change(&self.db, owner, &intent).await?;
                }
            }
            return Err(ServerError::conflict_kind(
                "permission_mode_fixed",
                format!(
                    "{} fixes its permission mode when the session starts; start a new session to pick a different one",
                    session.harness_kind
                ),
            ));
        }

        let handle = self.take_worker_for_epoch(id, intent.worker_epoch);
        let decision_gate = handle
            .as_ref()
            .map(|handle| handle.approval_decisions.clone());
        let _decision_guard = match decision_gate {
            Some(gate) => Some(gate.lock_owned().await),
            None => None,
        };
        if handle.is_some() {
            self.revoke_worker_channels(id);
        }
        if let Some(handle) = handle {
            if !Self::shut_down_worker(id, handle).await {
                let reason = permission_mode_fence_reason(&intent);
                let fenced =
                    fence_permission_mode_change(&self.db, owner, &intent, &reason).await?;
                if let Some(fenced) = fenced {
                    crate::code::attention::emit_digest(&self.db, &self.bus, &fenced).await;
                } else {
                    let _ = discard_permission_mode_change(&self.db, owner, &intent).await?;
                }
                return Err(ServerError::conflict_kind(
                    "permission_mode_unconfirmed",
                    "the session worker did not stop while changing permission mode; reap the fenced session before another turn",
                ));
            }
        }

        crate::code::approval_sweep::abandon_for_restart(
            &self.db,
            &self.bus,
            owner,
            intent.session_id,
            intent.worker_epoch,
        )
        .await;
        if !confirm_permission_mode_change(&self.db, owner, &intent).await? {
            let _ = discard_permission_mode_change(&self.db, owner, &intent).await;
            return Err(ServerError::conflict_kind(
                "permission_mode_changed",
                "the session changed while its worker stopped for the permission mode update",
            ));
        }
        let session = self.get_session(owner, id).await?;
        self.note_permission_mode(owner, &session, intent.previous_mode, mode, intent.revision)
            .await;

        self.attach_and_spawn_worker(session).await
    }

    /// Ask the live engine to take a mode, then hold its worker for settlement.
    pub(super) async fn repostured_in_place(
        &self,
        id: SessionId,
        expected_spawn_epoch: i64,
        mode: PermissionMode,
    ) -> Result<LivePermissionModeOutcome, ServerError> {
        let Ok(handle) = self.require_worker(id) else {
            return Ok(LivePermissionModeOutcome::Unavailable);
        };
        if handle.spawn_epoch != expected_spawn_epoch {
            return Ok(LivePermissionModeOutcome::Unavailable);
        }
        let (reply, rx) = oneshot::channel();
        let (settlement, settle) = oneshot::channel();
        if handle
            .commands
            .send(WorkerCommand::SetPermissionMode {
                mode,
                settlement: settle,
                reply,
            })
            .await
            .is_err()
        {
            return Ok(LivePermissionModeOutcome::Unavailable);
        }
        match rx.await {
            Ok(Ok(())) => Ok(LivePermissionModeOutcome::Acknowledged(
                LivePermissionModeChange { settlement, handle },
            )),
            Ok(Err(WorkerError::RelaunchRequired(_))) => {
                Ok(LivePermissionModeOutcome::RelaunchRequired)
            }
            Ok(Err(err)) => Err(map_worker(err)),
            // The worker went away mid-request. Relaunching is the repair.
            Err(_) => Ok(LivePermissionModeOutcome::Unavailable),
        }
    }

    pub(super) async fn stop_and_fence_permission_mode_change(
        &self,
        owner: &OwnerId,
        intent: &PermissionModeChangeIntent,
        change: LivePermissionModeChange,
    ) -> Result<Option<Session>, ServerError> {
        let LivePermissionModeChange { settlement, handle } = change;
        let _ = settlement.send(PermissionModeSettlement::Abort);
        let handle = if let Some(registered) =
            self.take_worker_for_epoch(intent.session_id, intent.worker_epoch)
        {
            self.revoke_worker_channels(intent.session_id);
            drop(handle);
            registered
        } else {
            handle
        };
        let _ = Self::shut_down_worker(intent.session_id, handle).await;
        let reason = permission_mode_fence_reason(intent);
        let fenced = fence_permission_mode_change(&self.db, owner, intent, &reason).await?;
        if let Some(fenced) = &fenced {
            crate::code::attention::emit_digest(&self.db, &self.bus, fenced).await;
        } else {
            let _ = discard_permission_mode_change(&self.db, owner, intent).await;
        }
        Ok(fenced)
    }

    pub(super) async fn retire_permission_mode_worker(&self, intent: &PermissionModeChangeIntent) {
        let Some(handle) = self.take_worker_for_epoch(intent.session_id, intent.worker_epoch)
        else {
            return;
        };
        self.revoke_worker_channels(intent.session_id);
        let _ = Self::shut_down_worker(intent.session_id, handle).await;
    }

    pub(super) async fn commit_execution_settings(
        &self,
        expected: &Session,
        next: &SessionExecutionSettings,
        action: &'static str,
    ) -> Result<Session, ServerError> {
        let applies_to_future_turn = expected.lifecycle == SessionLifecycle::Running
            || get_open_turn(&self.db, &expected.owner, expected.id)
                .await?
                .is_some();
        if applies_to_future_turn {
            return replace_session_execution_settings(&self.db, &expected.owner, expected, next)
                .await?
                .ok_or_else(|| {
                    ServerError::conflict_kind(
                        "session_settings_changed",
                        format!("the session settings changed before {action}"),
                    )
                });
        }

        let handle = self.require_worker(expected.id)?;
        if handle.spawn_epoch != expected.spawn_epoch {
            return Err(ServerError::conflict_kind(
                "session_worker_changed",
                "the session worker changed before the settings update",
            ));
        }
        let (reply, response) = oneshot::channel();
        let (settlement, release) = oneshot::channel();
        handle
            .commands
            .send(WorkerCommand::SetExecutionSettings {
                settings: next.clone(),
                settlement: release,
                reply,
            })
            .await
            .map_err(|_| {
                ServerError::conflict_kind(
                    "session_worker_missing",
                    "the session worker stopped before the settings update",
                )
            })?;
        response
            .await
            .map_err(|_| {
                ServerError::conflict_kind(
                    "session_worker_missing",
                    "the session worker stopped before reserving the settings update",
                )
            })?
            .map_err(map_worker)?;

        let updated =
            match replace_session_execution_settings(&self.db, &expected.owner, expected, next)
                .await
            {
                Ok(Some(updated)) => updated,
                Ok(None) => {
                    let _ = settlement.send(ExecutionSettingsSettlement::Abort);
                    return Err(ServerError::conflict_kind(
                        "session_settings_changed",
                        format!("the session settings changed before {action}"),
                    ));
                }
                Err(error) => {
                    let _ = settlement.send(ExecutionSettingsSettlement::Abort);
                    return Err(ServerError::from(error));
                }
            };
        if settlement
            .send(ExecutionSettingsSettlement::Confirmed)
            .is_err()
        {
            if let Some(handle) = self.take_worker_for_epoch(expected.id, expected.spawn_epoch) {
                self.revoke_worker_channels(expected.id);
                let _ = Self::shut_down_worker(expected.id, handle).await;
            }
            return self.attach_and_spawn_worker(updated).await;
        }
        Ok(updated)
    }

    pub(super) fn take_worker_for_epoch(
        &self,
        id: SessionId,
        spawn_epoch: i64,
    ) -> Option<WorkerHandle> {
        let mut workers = self.workers.lock().expect("code workers");
        let exact = workers
            .get(&id)
            .is_some_and(|handle| handle.spawn_epoch == spawn_epoch);
        exact.then(|| workers.remove(&id)).flatten()
    }

    /// Journal a mode change so the transcript says when the posture moved.
    pub(super) async fn note_permission_mode(
        &self,
        owner: &OwnerId,
        session: &Session,
        previous: PermissionMode,
        mode: PermissionMode,
        revision: i64,
    ) {
        let _ = crate::code::session_worker::journal_event(
            &self.db,
            &self.bus,
            owner,
            session.id,
            session.spawn_epoch,
            Event::HarnessNotice {
                level: tidebreak_core::HarnessNoticeLevel::Info,
                message: format!(
                    "permission mode changed from {previous} to {mode} at revision {revision}"
                ),
            },
        )
        .await;
    }

    /// Change a session's reasoning effort. `None` hands the level back to the
    /// engine's own default.
    ///
    /// No relaunch and no engine call: every adapter reads the effort off the
    /// turn, so persisting it is the whole switch. Refused mid-turn all the
    /// same — a level that changed under a running turn would report a session
    /// setting the turn did not run at.
    pub(crate) async fn set_reasoning_effort(
        &self,
        owner: &OwnerId,
        id: SessionId,
        effort: Option<ReasoningEffort>,
    ) -> Result<Session, ServerError> {
        let session = self.get_session(owner, id).await?;
        match session.lifecycle {
            SessionLifecycle::Running => {
                return Err(ServerError::conflict_kind(
                    "turn_running",
                    "finish or interrupt the running turn before changing the reasoning effort",
                ));
            }
            SessionLifecycle::Ended => {
                return Err(ServerError::conflict_kind(
                    "session_ended",
                    "this session has ended; start a new one to pick a different effort",
                ));
            }
            _ => {}
        }
        if get_open_turn(&self.db, owner, id).await?.is_some() {
            return Err(ServerError::conflict_kind(
                "turn_running",
                "finish or interrupt the running turn before changing the reasoning effort",
            ));
        }
        let adapter = self.adapter(session.harness_kind)?;
        let probe = self.probe(adapter.as_ref()).await;
        let selected = self
            .selected_model_capabilities_for_scope(
                owner,
                Some(ModelCredentialScope::Session(session.id)),
                adapter.as_ref(),
                &probe,
                session.model.as_deref(),
            )
            .await;
        let mut next = SessionExecutionSettings::from(&session);
        selected.deactivate_unsupported(&mut next);
        next.reasoning_effort = effort;
        Self::validate_execution_settings(session.harness_kind, &next, &selected)?;
        if next == SessionExecutionSettings::from(&session) {
            return Ok(session);
        }
        self.commit_execution_settings(&session, &next, "the reasoning effort could be saved")
            .await
    }

    /// Arm or disarm the engine's fast mode for a session.
    ///
    /// Refused mid-turn and after the session ends, on the rule the effort
    /// route already applies. Enabling is also refused unless the selected
    /// model advertises the tier, so the session snapshot never claims that a
    /// standard-speed turn is fast.
    pub(crate) async fn set_fast_mode(
        &self,
        owner: &OwnerId,
        id: SessionId,
        fast_mode: bool,
    ) -> Result<Session, ServerError> {
        let session = self.get_session(owner, id).await?;
        match session.lifecycle {
            SessionLifecycle::Running => {
                return Err(ServerError::conflict_kind(
                    "turn_running",
                    "finish or interrupt the running turn before changing fast mode",
                ));
            }
            SessionLifecycle::Ended => {
                return Err(ServerError::conflict_kind(
                    "session_ended",
                    "this session has ended; start a new one to run it in fast mode",
                ));
            }
            _ => {}
        }
        if get_open_turn(&self.db, owner, id).await?.is_some() {
            return Err(ServerError::conflict_kind(
                "turn_running",
                "finish or interrupt the running turn before changing fast mode",
            ));
        }
        let adapter = self.adapter(session.harness_kind)?;
        let probe = self.probe(adapter.as_ref()).await;
        let selected = self
            .selected_model_capabilities_for_scope(
                owner,
                Some(ModelCredentialScope::Session(session.id)),
                adapter.as_ref(),
                &probe,
                session.model.as_deref(),
            )
            .await;
        let mut next = SessionExecutionSettings::from(&session);
        selected.deactivate_unsupported(&mut next);
        next.fast_mode = fast_mode;
        Self::validate_execution_settings(session.harness_kind, &next, &selected)?;
        if next == SessionExecutionSettings::from(&session) {
            return Ok(session);
        }
        self.commit_execution_settings(&session, &next, "fast mode could be saved")
            .await
    }

    pub(super) async fn selected_model_capabilities(
        adapter: &dyn HarnessAdapter,
        probe: &HarnessProbe,
        selected: Option<&str>,
    ) -> SelectedModelCapabilities {
        let caps = adapter.capabilities(probe);
        let listed = adapter.list_models(probe).await;
        // Empty is inconclusive: adapters also return it when model listing
        // fails. Only catalog evidence may disable an already stored setting.
        let catalog_known = !listed.is_empty();
        let model = listed.iter().find(|model| match selected {
            Some(selected) => model.id == selected,
            None => model.default,
        });
        let (reasoning_efforts, reasoning_known) = if caps.reasoning_levels == CapLevel::Supported {
            match (selected, model) {
                (_, Some(model)) => (model.reasoning_efforts.clone(), true),
                (None, None) => {
                    let mut levels = adapter.reasoning_efforts(probe);
                    if levels.is_empty() {
                        levels = listed
                            .iter()
                            .flat_map(|model| model.reasoning_efforts.iter().copied())
                            .collect::<std::collections::BTreeSet<_>>()
                            .into_iter()
                            .collect();
                    }
                    let reasoning_known = catalog_known || !levels.is_empty();
                    (levels, reasoning_known)
                }
                (Some(_), None) => (Vec::new(), catalog_known),
            }
        } else {
            (Vec::new(), true)
        };
        SelectedModelCapabilities {
            reasoning_efforts,
            listed_model_reasoning_efforts: model.map(|model| model.reasoning_efforts.clone()),
            reasoning_known,
            fast_mode: model.is_some_and(|model| model.fast_mode),
            fast_mode_known: model.is_some() || catalog_known,
        }
    }

    pub(super) async fn selected_model_capabilities_for_scope(
        &self,
        owner: &OwnerId,
        scope: Option<ModelCredentialScope>,
        adapter: &dyn HarnessAdapter,
        probe: &HarnessProbe,
        selected: Option<&str>,
    ) -> SelectedModelCapabilities {
        let mut capabilities = Self::selected_model_capabilities(adapter, probe, selected).await;
        let Some(selected) = selected else {
            return capabilities;
        };
        if adapter.capabilities(probe).reasoning_levels != CapLevel::Supported {
            return capabilities;
        }
        let snapshot = match (self.harness_llm.as_ref(), scope) {
            (Some(relay), Some(ModelCredentialScope::Session(id))) => {
                relay.catalog_for_session(owner, id).await.ok().flatten()
            }
            (Some(relay), Some(ModelCredentialScope::Grant(id))) => {
                relay.catalog_for_grant(owner, id).await.ok().flatten()
            }
            _ => self.gateway_model_snapshot(owner).await,
        };
        let Some(snapshot) = snapshot else {
            return capabilities;
        };
        let Some(model_efforts) =
            crate::providers::gateway_reasoning_efforts_for_model(&snapshot, selected)
        else {
            return capabilities;
        };
        let engine_efforts = adapter.reasoning_efforts(probe);
        capabilities.reasoning_efforts = crate::providers::effective_gateway_reasoning_efforts(
            self.harness_llm.is_some(),
            capabilities.listed_model_reasoning_efforts.as_deref(),
            &engine_efforts,
            model_efforts,
        );
        capabilities.reasoning_known = true;
        tracing::debug!(
            harness = %adapter.kind(),
            model = selected,
            efforts = ?capabilities.reasoning_efforts,
            "using the gateway model's reasoning effort ladder"
        );
        capabilities
    }

    pub(super) fn validate_execution_settings(
        harness: HarnessKind,
        settings: &SessionExecutionSettings,
        selected: &SelectedModelCapabilities,
    ) -> Result<(), ServerError> {
        let model = settings.model.as_deref().unwrap_or("the default model");
        if let Some(effort) = settings.reasoning_effort {
            if !selected.supports_reasoning(effort) {
                return Err(ServerError::unprocessable_kind(
                    "reasoning_effort_unsupported",
                    format!(
                        "{harness} model {model} does not support reasoning effort {}",
                        effort.as_str()
                    ),
                ));
            }
        }
        if settings.fast_mode && !selected.fast_mode {
            return Err(ServerError::unprocessable_kind(
                "fast_mode_unsupported",
                format!("{harness} model {model} does not support fast mode"),
            ));
        }
        Ok(())
    }

    /// Refuse to mint a session that will 401 on its first turn (issue 2653).
    ///
    /// Fires only on a definitive signed-out observation with no other auth
    /// mode in sight: the relay carries covered engines as the caller
    /// (decision 71), and an API key or gateway endpoint override in the
    /// environment or engine config authenticates without any vendor login
    /// (issue 2749). An unobserved sign-in state (`None`) stays allowed — a
    /// false refusal on a working machine is worse than the first-turn
    /// failure this replaces, which the worker at least maps to the same
    /// legible message.
    pub(super) fn refuse_signed_out_harness(
        &self,
        harness: HarnessKind,
        probe: &HarnessProbe,
    ) -> Result<(), ServerError> {
        Self::signed_out_harness_refusal(self.harness_llm.is_some(), harness, probe)
    }

    pub(super) fn signed_out_harness_refusal(
        relay_active: bool,
        harness: HarnessKind,
        probe: &HarnessProbe,
    ) -> Result<(), ServerError> {
        if probe.authenticated != Some(false) {
            return Ok(());
        }
        if relay_active && crate::code::harness_llm::relay_covered(harness) {
            return Ok(());
        }
        if tidebreak_harness::auth_override_present(harness, &probe.env) {
            return Ok(());
        }
        let label = crate::code::harness_label(harness);
        Err(ServerError::unprocessable_kind(
            "harness_not_authenticated",
            format!(
                "{label} is not signed in on this machine. \
                 Sign in to {label} in your own terminal, then start the session again."
            ),
        ))
    }
}

#[cfg(test)]
mod selected_model_capabilities_tests {
    use super::*;
    use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
    use tidebreak_harness::ListedHarnessModel;

    #[derive(Default)]
    struct NoSecrets;

    #[async_trait::async_trait]
    impl tidebreak_core::SecretProvider for NoSecrets {
        async fn get_secret(&self, _key: &str) -> tidebreak_core::Result<Option<String>> {
            Ok(None)
        }

        async fn set_secret(&self, _key: &str, _value: &str) -> tidebreak_core::Result<()> {
            Ok(())
        }

        async fn delete_secret(&self, _key: &str) -> tidebreak_core::Result<()> {
            Ok(())
        }
    }

    async fn runtime_with_gateway_snapshot(
        gateway_url: &str,
        policy_url: &str,
    ) -> (CodeRuntime, tempfile::TempDir) {
        let directory = tempfile::tempdir().expect("data dir");
        let db = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("code.db").display()
            ))
            .await
            .expect("db"),
        );
        crate::providers::write_gateway_snapshot(
            &*db,
            &crate::providers::GatewayModelSnapshot {
                gateway_url: gateway_url.into(),
                installation_id: None,
                models: Vec::new(),
                model_protocols: Default::default(),
                model_reasoning_efforts: std::collections::BTreeMap::from([(
                    "glm-5.3".into(),
                    vec![
                        ReasoningEffort::Low,
                        ReasoningEffort::High,
                        ReasoningEffort::Max,
                    ],
                )]),
                member_catalog: Some("v1".into()),
                catalog_etag: None,
            },
        )
        .await
        .expect("gateway snapshot");
        let provisioned = crate::managed_policy::MemoryProvisionedPolicy::new();
        crate::managed_policy::provision(&*provisioned, policy_url).expect("managed policy");
        let gateway = crate::gateway_runtime::GatewayRuntime::new(
            db.clone(),
            Arc::new(NoSecrets),
            provisioned,
            Arc::new(crate::managed_policy::NoOsPolicy),
        );
        let runtime =
            CodeRuntime::with_registry(db, directory.path().to_path_buf(), AdapterRegistry::new())
                .with_gateway_runtime(gateway);
        (runtime, directory)
    }

    fn scripted_probe() -> HarnessProbe {
        HarnessProbe {
            found: true,
            binary_path: Some(PathBuf::from("/scripted")),
            version: Some("1.0.0".into()),
            authenticated: Some(true),
            stderr: String::new(),
            env: Vec::new(),
            commands: Vec::new(),
        }
    }

    #[tokio::test]
    async fn an_unavailable_catalog_does_not_clear_committed_settings() {
        let adapter =
            ScriptedAdapter::new(plain_text_script()).with_reasoning_levels(CapLevel::Supported);
        let probe = HarnessProbe {
            found: true,
            binary_path: Some(PathBuf::from("/scripted")),
            version: Some("1.0.0".into()),
            authenticated: Some(true),
            stderr: String::new(),
            env: Vec::new(),
            commands: Vec::new(),
        };
        let capabilities =
            CodeRuntime::selected_model_capabilities(&adapter, &probe, Some("configured")).await;
        let mut settings = SessionExecutionSettings {
            model: Some("configured".into()),
            reasoning_effort: Some(ReasoningEffort::High),
            fast_mode: true,
        };

        capabilities.deactivate_unsupported(&mut settings);

        assert_eq!(settings.reasoning_effort, Some(ReasoningEffort::High));
        assert!(settings.fast_mode);
    }

    #[tokio::test]
    async fn a_gateway_model_uses_the_model_and_engine_effort_intersection() {
        let (runtime, _directory) =
            runtime_with_gateway_snapshot("https://gateway.example/", "https://gateway.example/")
                .await;
        let adapter =
            ScriptedAdapter::new(plain_text_script()).with_reasoning_levels(CapLevel::Supported);

        let capabilities = runtime
            .selected_model_capabilities_for_scope(
                &OwnerId::new("alice").unwrap(),
                None,
                &adapter,
                &scripted_probe(),
                Some("model-gateway-model-gateway/glm-5.3"),
            )
            .await;

        assert_eq!(
            capabilities.reasoning_efforts,
            vec![
                ReasoningEffort::Low,
                ReasoningEffort::High,
                ReasoningEffort::Max,
            ]
        );
        assert!(capabilities.reasoning_known);
    }

    #[tokio::test]
    async fn a_codex_rows_ladder_wins_over_the_engine_wide_ladder() {
        let (runtime, _directory) =
            runtime_with_gateway_snapshot("https://gateway.example/", "https://gateway.example/")
                .await;
        let adapter = ScriptedAdapter::new(plain_text_script())
            .with_kind(HarnessKind::Codex)
            .with_reasoning_levels(CapLevel::Supported)
            .with_models(vec![ListedHarnessModel {
                id: "model-gateway-model-gateway/glm-5.3".into(),
                label: "GLM 5.3".into(),
                default: true,
                reasoning_efforts: vec![
                    ReasoningEffort::Low,
                    ReasoningEffort::High,
                    ReasoningEffort::Ultra,
                ],
                fast_mode: false,
            }]);

        let capabilities = runtime
            .selected_model_capabilities_for_scope(
                &OwnerId::new("alice").unwrap(),
                None,
                &adapter,
                &scripted_probe(),
                Some("model-gateway-model-gateway/glm-5.3"),
            )
            .await;

        assert_eq!(
            capabilities.reasoning_efforts,
            vec![ReasoningEffort::Low, ReasoningEffort::High]
        );
    }

    #[tokio::test]
    async fn a_snapshot_from_another_gateway_does_not_change_engine_efforts() {
        let (runtime, _directory) = runtime_with_gateway_snapshot(
            "https://old.gateway.example/",
            "https://gateway.example/",
        )
        .await;
        let adapter =
            ScriptedAdapter::new(plain_text_script()).with_reasoning_levels(CapLevel::Supported);

        let capabilities = runtime
            .selected_model_capabilities_for_scope(
                &OwnerId::new("alice").unwrap(),
                None,
                &adapter,
                &scripted_probe(),
                Some("model-gateway-model-gateway/glm-5.3"),
            )
            .await;

        assert!(capabilities.reasoning_efforts.is_empty());
        assert!(!capabilities.reasoning_known);
    }

    #[test]
    fn an_authoritative_catalog_clears_unsupported_settings() {
        let capabilities = SelectedModelCapabilities {
            reasoning_efforts: vec![ReasoningEffort::Low],
            listed_model_reasoning_efforts: Some(vec![ReasoningEffort::Low]),
            reasoning_known: true,
            fast_mode: false,
            fast_mode_known: true,
        };
        let mut settings = SessionExecutionSettings {
            model: Some("steady".into()),
            reasoning_effort: Some(ReasoningEffort::High),
            fast_mode: true,
        };

        capabilities.deactivate_unsupported(&mut settings);

        assert_eq!(settings.reasoning_effort, None);
        assert!(!settings.fast_mode);
    }
}

#[cfg(test)]
mod signed_out_refusal_tests {
    use super::*;

    fn probe(
        authenticated: Option<bool>,
        env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> HarnessProbe {
        HarnessProbe {
            found: true,
            binary_path: Some(PathBuf::from("/scripted/engine")),
            version: Some("1.0.0".into()),
            authenticated,
            stderr: String::new(),
            env,
            commands: Vec::new(),
        }
    }

    #[test]
    fn a_definitive_signed_out_observation_refuses_with_the_typed_kind() {
        let err = CodeRuntime::signed_out_harness_refusal(
            false,
            HarnessKind::Codex,
            &probe(Some(false), Vec::new()),
        )
        .unwrap_err();
        assert_eq!(err.kind(), "harness_not_authenticated");
        assert!(
            err.message()
                .contains("Codex CLI is not signed in on this machine."),
            "{}",
            err.message()
        );
    }

    #[test]
    fn an_unverified_or_signed_in_observation_allows_create() {
        for authenticated in [None, Some(true)] {
            assert!(CodeRuntime::signed_out_harness_refusal(
                false,
                HarnessKind::ClaudeCode,
                &probe(authenticated, Vec::new()),
            )
            .is_ok());
        }
    }

    #[test]
    fn the_relay_carries_covered_engines_past_a_signed_out_probe() {
        // The #2742 guarantee: a hosted machine with the relay creates freely.
        assert!(CodeRuntime::signed_out_harness_refusal(
            true,
            HarnessKind::Codex,
            &probe(Some(false), Vec::new()),
        )
        .is_ok());
    }

    #[test]
    fn a_credential_override_in_the_environment_allows_create() {
        // A gateway-managed machine authenticates with no vendor login
        // (issue 2749); its overrides must beat the signed-out observation.
        let env = vec![(
            std::ffi::OsString::from("OPENAI_BASE_URL"),
            std::ffi::OsString::from("https://gateway.example/v1"),
        )];
        assert!(CodeRuntime::signed_out_harness_refusal(
            false,
            HarnessKind::Codex,
            &probe(Some(false), env),
        )
        .is_ok());
    }

    #[test]
    fn engines_without_a_verified_override_surface_never_refuse() {
        // opencode honors provider keys and config shapes the probe cannot
        // cheaply rule out, so a signed-out observation alone must not block.
        assert!(CodeRuntime::signed_out_harness_refusal(
            false,
            HarnessKind::Opencode,
            &probe(Some(false), Vec::new()),
        )
        .is_ok());
    }
}
