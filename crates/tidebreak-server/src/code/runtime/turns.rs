//! Turn submission, the durable queue, steering, reaping, and update quiesce.

use super::*;

impl CodeRuntime {
    /// Begin an update quiesce: session workers hold their queue drains, no
    /// new turn starts, and idle engine children park immediately. Turns
    /// already in flight run to their boundary; [`Self::await_update_quiesce`]
    /// is how the caller waits for that. See `crate::update_quiesce`.
    pub fn begin_update_quiesce(&self) {
        self.update_quiesce.send_replace(true);
        self.wake_all_workers();
    }

    /// Reopen turn admission after an update that did not install. Parked
    /// children stay parked; the next turn respawns and resumes them exactly
    /// as an idle park does (decision 0064).
    pub fn end_update_quiesce(&self) {
        self.update_quiesce.send_replace(false);
        self.wake_all_workers();
    }

    pub fn update_quiesce_active(&self) -> bool {
        *self.update_quiesce.borrow()
    }

    /// Wait until no local session is mid-turn and no engine child is live,
    /// or fail with a sentence the updater can show as-is. Remote sessions
    /// run their engine in a sandbox and survive a restart on their own, so
    /// they never appear here — the worker map only holds local sessions.
    pub async fn await_update_quiesce(&self, deadline: Duration) -> Result<(), String> {
        let deadline_at = Instant::now() + deadline;
        // Turn starts are fenced by the worktree lock, not by the database:
        // a starting turn holds its workspace's lock from before it re-reads
        // the quiesce flag until the turn ends, and the flag is already up.
        // Acquiring and releasing every lock therefore proves each workspace
        // is past any start that raced the flag — whoever takes a lock after
        // this sees the flag and refuses. Without this pass, a poll of the
        // stored lifecycle could observe Idle in the window between a turn
        // winning its lock and persisting Running, and the update would exit
        // over an engine turn that was about to start.
        let worktree_turns: Vec<Arc<tokio::sync::Mutex<()>>> = self
            .worktree_turns
            .lock()
            .expect("code worktree turn locks")
            .values()
            .cloned()
            .collect();
        for worktree_turn in worktree_turns {
            let remaining = deadline_at.saturating_duration_since(Instant::now());
            match tokio::time::timeout(remaining, worktree_turn.lock()).await {
                Ok(guard) => drop(guard),
                Err(_) => {
                    return Err(
                        "A code session is still working on a turn. Try again once it \
                                finishes — the update stays ready."
                            .to_owned(),
                    );
                }
            }
        }
        // Past every turn boundary; now wait for the workers to park their
        // engine children and for the stored rows to agree.
        loop {
            let ids: Vec<SessionId> = self
                .workers
                .lock()
                .expect("code workers")
                .keys()
                .copied()
                .collect();
            let mut busy = 0usize;
            for id in ids {
                match tidebreak_core::db::code::get_session_all_owners(&self.db, id).await {
                    Ok(Some(session)) => {
                        if session.lifecycle == SessionLifecycle::Running
                            || session.child_pid.is_some()
                        {
                            busy += 1;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        return Err(format!(
                            "could not read code sessions while preparing the update: {error}"
                        ));
                    }
                }
            }
            if busy == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline_at {
                return Err(if busy == 1 {
                    "A code session is still working on a turn. Try again once it finishes — \
                     the update stays ready."
                        .to_owned()
                } else {
                    format!(
                        "{busy} code sessions are still working on turns. Try again once they \
                         finish — the update stays ready."
                    )
                });
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    pub async fn submit_turn(
        &self,
        owner: &OwnerId,
        id: SessionId,
        message: String,
        model: Option<String>,
        reasoning_effort: Option<Option<ReasoningEffort>>,
        attachments: Vec<tidebreak_core::ImageRef>,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        self.submit_turn_inner(
            owner,
            id,
            message,
            model,
            reasoning_effort,
            attachments,
            None,
            true,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn submit_turn_inner(
        &self,
        owner: &OwnerId,
        id: SessionId,
        message: String,
        model: Option<String>,
        reasoning_effort: Option<Option<ReasoningEffort>>,
        attachments: Vec<tidebreak_core::ImageRef>,
        trigger_delivery: Option<TriggerDeliveryClaim>,
        queue_if_busy: bool,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        if let Some(claim) = trigger_delivery {
            if tidebreak_core::db::code::trigger_delivery_accepted(
                &self.db,
                owner,
                claim.delivery_id,
            )
            .await?
            {
                return Ok(SubmitTurnOutcome::AlreadyDelivered);
            }
        }
        let mut session = self.get_session(owner, id).await?;
        let workspace = self.session_workspace(&session).await?;
        if let Some(workspace) = &workspace {
            if workspace.status != CodeWorkspaceStatus::Active {
                return Err(ServerError::conflict_kind(
                    "workspace_not_ready",
                    format!("workspace is {}", workspace.status.as_str()),
                ));
            }
        }
        if session.lifecycle == SessionLifecycle::Fenced {
            return Err(ServerError::conflict_kind(
                "session_fenced",
                "session is fenced until it is reaped",
            ));
        }
        if session.lifecycle == SessionLifecycle::Ended {
            return Err(ServerError::conflict_kind(
                "session_ended",
                "session has ended",
            ));
        }
        if let Some(workspace) = workspace.as_ref().filter(|workspace| workspace.is_remote()) {
            // The sandbox path: no local worker, no worktree lock, no
            // harness probe. Everything below this branch assumes a checkout
            // on this machine.
            return self
                .submit_remote_turn(
                    owner,
                    session,
                    workspace,
                    message,
                    model,
                    reasoning_effort,
                    attachments,
                    trigger_delivery,
                    queue_if_busy,
                )
                .await;
        }
        // No capability gate on attachments. An engine that states image input
        // is handed the bytes on its own protocol; every other one is handed a
        // private file and an absolute path in the prompt. The worker picks
        // between the two.
        // Both stick: a composer choice is the session's from here on, exactly
        // as the engines' own pickers behave. The outer `Option` on effort is
        // what lets a turn say "back to the engine default" rather than "no
        // opinion" — the inner `None` is a real choice.
        let requested_model = normalize_model(model);
        let model_changed = requested_model
            .as_deref()
            .is_some_and(|model| session.model.as_deref() != Some(model));
        let requested_effort = reasoning_effort;
        let mut next = SessionExecutionSettings::from(&session);
        if let Some(model) = requested_model {
            next.model = Some(model);
        }
        if let Some(effort) = reasoning_effort {
            next.reasoning_effort = effort;
        }
        if model_changed
            || requested_effort.is_some()
            || next.reasoning_effort.is_some()
            || next.fast_mode
        {
            let adapter = self.adapter(session.harness_kind)?;
            let probe = self.probe(adapter.as_ref()).await;
            let selected = self
                .selected_model_capabilities_for_owner(
                    owner,
                    adapter.as_ref(),
                    &probe,
                    next.model.as_deref(),
                )
                .await;
            if requested_effort.is_some() {
                let requested = SessionExecutionSettings {
                    model: next.model.clone(),
                    reasoning_effort: next.reasoning_effort,
                    fast_mode: false,
                };
                Self::validate_execution_settings(session.harness_kind, &requested, &selected)?;
            }
            selected.deactivate_unsupported(&mut next);
        }
        if next != SessionExecutionSettings::from(&session) {
            session = self
                .commit_execution_settings(&session, &next, "the turn could reserve them")
                .await?;
        }
        let handle = self.require_worker(id)?;
        if handle.spawn_epoch != session.spawn_epoch {
            return Err(ServerError::conflict_kind(
                "session_worker_changed",
                "the session worker changed before the turn could start",
            ));
        }
        // A sibling fenced for an unaccounted engine — an orphan from a
        // previous boot, an ambiguous pid, a lost resume — may still be alive
        // in this checkout, outside every lock this process holds. The turn
        // lock cannot see it, so nothing in the workspace writes until it is
        // reaped (record 55). A sibling fenced for repeated turn failures is
        // not that: its engine answered every time, so it does not stop us.
        if let Some(reason) = self.workspace_fence_reason(owner, &session).await? {
            return Err(ServerError::conflict_kind("workspace_fenced", reason));
        }
        // Queue-default (0009, 0065): a send while a turn is in flight parks
        // as a durable queue row. This does not consult mid_turn_steering —
        // that cap gates the separate /steer route only. A backlog parks the
        // send even with no open turn: rows ahead of this message must run
        // first (FIFO), and the worker may already be holding the head while
        // it waits on a sibling's worktree turn.
        let in_flight = session.lifecycle == SessionLifecycle::Running
            || get_open_turn(&self.db, owner, id).await?.is_some();
        let backlog = !tidebreak_core::db::code::list_queued_turns(&self.db, owner, id)
            .await?
            .is_empty();
        // An update quiesce parks the send too: the row survives the restart
        // and drains after the relaunch, so nothing typed during the short
        // install window is lost or refused.
        if in_flight || backlog || self.update_quiesce_active() {
            if !queue_if_busy {
                return Err(ServerError::conflict_kind(
                    "trigger_turn_busy",
                    "the trigger turn was not accepted because the session became busy",
                ));
            }
            return self
                .park_follow_up(owner, &handle, &session, message, attachments)
                .await;
        }
        let (reply, rx) = oneshot::channel();
        handle
            .commands
            .send(WorkerCommand::RunTurn {
                message: message.clone(),
                attachments: attachments.clone(),
                trigger_delivery,
                reply,
            })
            .await
            .map_err(|_| ServerError::internal("session worker is gone"))?;
        let turn = match rx
            .await
            .map_err(|_| ServerError::internal("session worker dropped the turn"))?
        {
            Ok(turn) => turn,
            // A sibling holds the workspace's turn lock. Taking that lock is
            // the reservation, so this is the first moment either send can
            // know which one won — a check before the send would let two idle
            // siblings both believe the checkout was free. The loser parks
            // exactly as a busy session does, and the route answers now rather
            // than holding the connection open for someone else's turn.
            Err(WorkerError::WorktreeBusy) => {
                if !queue_if_busy {
                    return Err(ServerError::conflict_kind(
                        "trigger_turn_busy",
                        "the trigger turn was not accepted because the workspace became busy",
                    ));
                }
                return self
                    .park_follow_up(owner, &handle, &session, message, attachments)
                    .await;
            }
            // The quiesce flag flipped while this send was in flight. Park it
            // the same way: the durable row runs after the relaunch.
            Err(WorkerError::UpdateQuiesced) => {
                if !queue_if_busy {
                    return Err(ServerError::conflict_kind(
                        "trigger_turn_busy",
                        "the trigger turn was not accepted because the app is restarting to update",
                    ));
                }
                return self
                    .park_follow_up(owner, &handle, &session, message, attachments)
                    .await;
            }
            Err(WorkerError::TriggerDeliveryAccepted) => {
                return Ok(SubmitTurnOutcome::AlreadyDelivered);
            }
            Err(err) => return Err(map_worker(err)),
        };
        Ok(SubmitTurnOutcome::Ran(Box::new(turn)))
    }

    /// Submit a turn created by one durable trigger delivery.
    pub async fn submit_trigger_turn(
        &self,
        owner: &OwnerId,
        id: SessionId,
        message: String,
        delivery_id: tidebreak_core::CodeTriggerDeliveryId,
        lease_token: uuid::Uuid,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        self.submit_turn_inner(
            owner,
            id,
            message,
            None,
            None,
            Vec::new(),
            Some(TriggerDeliveryClaim {
                delivery_id,
                lease_token,
            }),
            false,
        )
        .await
    }

    /// Park a message as a durable queue row (decision 69).
    ///
    /// The row id is minted here and becomes the promoted turn's id. The cap
    /// is checked before the insert so an overfull queue answers with a typed
    /// conflict rather than a store error; the store re-checks under the
    /// session write lock, so a racing pair can overshoot by at most one row.
    pub(super) async fn park_follow_up(
        &self,
        owner: &OwnerId,
        handle: &WorkerHandle,
        session: &Session,
        message: String,
        attachments: Vec<tidebreak_core::ImageRef>,
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
                actor: None,
                id: TurnId::new(),
                session_id: session.id,
                message,
                attachments,
                position: 0,
                created_at: now,
                updated_at: now,
            },
        )
        .await
        .map_err(ServerError::from)?;
        wake_queue(handle);
        Ok(SubmitTurnOutcome::Queued(Box::new(row)))
    }

    /// The session's queued messages plus whether promotion is paused.
    pub async fn list_queued_turns(
        &self,
        owner: &OwnerId,
        id: SessionId,
    ) -> Result<(Vec<QueuedTurn>, bool), ServerError> {
        let _ = self.get_session(owner, id).await?;
        let queued = tidebreak_core::db::code::list_queued_turns(&self.db, owner, id).await?;
        let paused = tidebreak_core::db::code::queue_paused(&self.db, owner, id).await?;
        Ok((queued, paused))
    }

    /// Edit or reorder one queued message. `None` when the row is gone.
    pub async fn update_queued_turn(
        &self,
        owner: &OwnerId,
        id: SessionId,
        queued_id: TurnId,
        message: Option<&str>,
        position: Option<i32>,
    ) -> Result<Option<QueuedTurn>, ServerError> {
        let _ = self.get_session(owner, id).await?;
        Ok(tidebreak_core::db::code::update_queued_turn(
            &self.db, owner, id, queued_id, message, position,
        )
        .await?)
    }

    pub async fn interrupt(&self, id: SessionId) -> Result<(), ServerError> {
        // Interrupt stops only the active turn. The worker and logical code
        // session continue, so its browser capfile and native capability must
        // remain live for later turns.
        if self.workers.lock().expect("code workers").contains_key(&id) {
            let handle = self.require_worker(id)?;
            let (reply, rx) = oneshot::channel();
            handle
                .commands
                .send(WorkerCommand::Interrupt { reply })
                .await
                .map_err(|_| ServerError::internal("session worker is gone"))?;
            return rx
                .await
                .map_err(|_| ServerError::internal("session worker dropped the interrupt"))?
                .map_err(map_worker);
        }
        // No host worker: a remote session's engine lives in a sandbox.
        let sessions = list_sessions_all_owners(&self.db).await?;
        let Some(session) = sessions.into_iter().find(|session| session.id == id) else {
            return Err(ServerError::conflict_kind(
                "session_worker_missing",
                "session worker is not running",
            ));
        };
        let workspace = self.session_workspace(&session).await?;
        if !workspace.is_some_and(|workspace| workspace.is_remote()) {
            return Err(ServerError::conflict_kind(
                "session_worker_missing",
                "session worker is not running",
            ));
        }
        self.interrupt_remote(&session).await
    }

    /// Retract one queued message. `false` when the row is gone.
    pub async fn delete_queued_turn(
        &self,
        owner: &OwnerId,
        id: SessionId,
        queued_id: TurnId,
    ) -> Result<bool, ServerError> {
        let _ = self.get_session(owner, id).await?;
        Ok(tidebreak_core::db::code::delete_queued_turn(&self.db, owner, id, queued_id).await?)
    }

    /// Pause or release the session's queue. A release wakes the worker so a
    /// waiting head starts without a new send.
    pub async fn set_queue_paused(
        &self,
        owner: &OwnerId,
        id: SessionId,
        paused: bool,
    ) -> Result<(), ServerError> {
        let _ = self.get_session(owner, id).await?;
        tidebreak_core::db::code::set_queue_paused(&self.db, owner, id, paused).await?;
        if !paused {
            self.wake_session_queue(id);
        }
        Ok(())
    }

    /// Clear the session's queue pause so the worker's next drain starts the
    /// head row. The tray composes send-now client-side exactly as chat does:
    /// pause, move the row first, stop the live turn, then this.
    pub async fn send_queued_now(&self, owner: &OwnerId, id: SessionId) -> Result<(), ServerError> {
        let _ = self.get_session(owner, id).await?;
        tidebreak_core::db::code::set_queue_paused(&self.db, owner, id, false).await?;
        self.wake_session_queue(id);
        Ok(())
    }

    /// Nudge a live worker to re-read its queue. A session with no worker has
    /// nothing to wake; its next spawn drains the queue first thing.
    pub(super) fn wake_session_queue(&self, id: SessionId) {
        if let Ok(handle) = self.require_worker(id) {
            wake_queue(&handle);
        }
    }

    /// Why this session's workspace is closed to turns, if it is.
    ///
    /// The turn lock lives in this process, so it can only order the workers
    /// this process owns. A fenced session is the one case where that is not
    /// enough: it means a harness child outlived a restart and may still be
    /// writing to the checkout. Until it is reaped, no sibling may start a
    /// turn there — otherwise the single-writer rule holds only for the
    /// sessions we happen to know about.
    pub(super) async fn workspace_fence_reason(
        &self,
        owner: &OwnerId,
        session: &Session,
    ) -> Result<Option<String>, ServerError> {
        let Some(workspace_id) = session.workspace_id else {
            return Ok(None);
        };
        let siblings = list_sessions_for_workspace(&self.db, owner, workspace_id).await?;
        Ok(siblings
            .iter()
            .find(|other| {
                other.id != session.id
                    && other.lifecycle == SessionLifecycle::Fenced
                    // Only a fence that implies an unaccounted engine process
                    // stops the workspace. A sibling fenced for repeated turn
                    // failures answered every time; its worktree is not at
                    // risk, so this session keeps working.
                    && other
                        .fence_reason
                        .as_ref()
                        .is_none_or(FenceReason::blocks_workspace)
            })
            .map(|fenced| {
                format!(
                    "another session in this workspace is fenced until it is reaped ({})",
                    fenced.id
                )
            }))
    }

    pub async fn steer(
        &self,
        owner: &OwnerId,
        id: SessionId,
        expected_turn_id: TurnId,
        message: String,
    ) -> Result<(), ServerError> {
        self.steer_inner(owner, id, expected_turn_id, message, None)
            .await
    }

    pub(super) async fn steer_inner(
        &self,
        owner: &OwnerId,
        id: SessionId,
        expected_turn_id: TurnId,
        message: String,
        trigger_delivery: Option<TriggerDeliveryClaim>,
    ) -> Result<(), ServerError> {
        if let Some(claim) = trigger_delivery {
            if tidebreak_core::db::code::trigger_delivery_accepted(
                &self.db,
                owner,
                claim.delivery_id,
            )
            .await?
            {
                return Ok(());
            }
        }
        let session = self.get_session(owner, id).await?;
        if session.lifecycle != SessionLifecycle::Running {
            return Err(ServerError::conflict_kind(
                "no_active_turn",
                "there is no active turn to steer; the message was not queued",
            ));
        }
        let Some(active_turn) = get_open_turn(&self.db, owner, id).await? else {
            return Err(ServerError::conflict_kind(
                "no_active_turn",
                "there is no active turn to steer; the message was not queued",
            ));
        };
        if active_turn.id != expected_turn_id {
            return Err(ServerError::conflict_kind(
                "stale_turn",
                format!(
                    "turn {expected_turn_id} is no longer active; current turn is {}; the message was not queued",
                    active_turn.id
                ),
            ));
        }
        let adapter = self.adapter(session.harness_kind)?;
        let probe = self.probe(adapter.as_ref()).await;
        let level = adapter.capabilities(&probe).mid_turn_steering;
        if level != CapLevel::Supported {
            return Err(ServerError::unprocessable_kind(
                "steering_unavailable",
                format!(
                    "{harness} mid-turn steering is {level}; the message was not queued",
                    harness = session.harness_kind,
                    level = level.as_str(),
                ),
            ));
        }
        let handle = self.require_worker(id)?;
        let mut accepted_trigger_delivery = None;
        if let Some(claim) = trigger_delivery {
            let accepted = tidebreak_core::db::code::accept_trigger_delivery(
                &self.db,
                owner,
                claim.delivery_id,
                claim.lease_token,
                tidebreak_core::CodeTriggerDeliverySink::Steer,
                id,
                Some(expected_turn_id),
                Utc::now(),
            )
            .await?;
            if !accepted {
                return Ok(());
            }
            accepted_trigger_delivery = Some(claim.delivery_id);
        }
        let (reply, rx) = oneshot::channel();
        let result = async {
            handle
                .commands
                .send(WorkerCommand::Steer {
                    expected_turn_id,
                    message,
                    reply,
                })
                .await
                .map_err(|_| ServerError::internal("session worker is gone"))?;
            rx.await
                .map_err(|_| ServerError::internal("session worker dropped the steer"))?
                .map_err(map_worker)
        }
        .await;
        if let Some(delivery_id) = accepted_trigger_delivery {
            // Engine steering has no durable enqueue boundary. The receipt is
            // therefore the at-most-once acceptance point: after it commits,
            // a send failure or ambiguous engine response must not retry and
            // risk applying the same instruction twice.
            if let Err(error) = result {
                tracing::warn!(
                    delivery = %delivery_id,
                    session = %id,
                    turn = %expected_turn_id,
                    error = %error.message(),
                    "trigger steering failed after durable acceptance"
                );
            }
            return Ok(());
        }
        result
    }

    /// Steer an active turn for one durable trigger delivery.
    pub async fn steer_trigger(
        &self,
        owner: &OwnerId,
        id: SessionId,
        expected_turn_id: TurnId,
        message: String,
        delivery_id: tidebreak_core::CodeTriggerDeliveryId,
        lease_token: uuid::Uuid,
    ) -> Result<(), ServerError> {
        self.steer_inner(
            owner,
            id,
            expected_turn_id,
            message,
            Some(TriggerDeliveryClaim {
                delivery_id,
                lease_token,
            }),
        )
        .await
    }

    pub async fn reap(&self, owner: &OwnerId, id: SessionId) -> Result<Session, ServerError> {
        let session = self.get_session(owner, id).await?;
        if session.lifecycle != SessionLifecycle::Fenced {
            return Err(ServerError::conflict_kind(
                "not_fenced",
                "only a fenced session can be reaped",
            ));
        }
        let workspace = self.session_workspace(&session).await?;
        if workspace
            .as_ref()
            .is_some_and(|workspace| workspace.is_remote())
        {
            // No worker to shut down and nothing to relaunch: the driver
            // cancels whatever the environment still holds, closes the
            // incarnation, and resolves the fence. The next turn
            // reincarnates on demand.
            let Some(remote) = self.remote_sessions() else {
                return Err(ServerError::conflict_kind(
                    "remote_disabled",
                    "this deployment has no sandbox runtime configured",
                ));
            };
            let driver = remote.driver(&self.db, self.bus.as_ref());
            return driver.reap(session).await.map_err(|error| match error {
                crate::code::remote::RemoteReapError::Store(error) => ServerError::from(error),
                other => ServerError::conflict_kind("session_not_reaped", other.to_string()),
            });
        }
        let handle = self.workers.lock().expect("code workers").remove(&id);
        let decision_gate = handle
            .as_ref()
            .map(|handle| handle.approval_decisions.clone());
        let _decision_guard = match decision_gate {
            Some(gate) => Some(gate.lock_owned().await),
            None => None,
        };
        self.revoke_worker_channels(id);
        let session = match handle {
            // The outgoing worker writes its own final state as it stops, and
            // the new spawn must not be started against a row it is still
            // moving. Wait for it, then reap the row as it stands now.
            Some(handle) => {
                Self::shut_down_worker(id, handle).await;
                self.get_session(owner, id).await?
            }
            None => session,
        };
        let session = recovery::reap_session(&self.db, &self.bus, session)
            .await
            .map_err(|error| match error {
                recovery::ReapSessionError::Store(error) => ServerError::from(error),
                other => ServerError::conflict_kind("session_not_reaped", other.to_string()),
            })?;
        self.attach_and_spawn_worker(session).await
    }
}
