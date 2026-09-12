//! Recover interrupted connections without submitting or replaying a turn.

use super::*;
use tidebreak_core::{AttentionState, ExecutionLocation, IncarnationState};

const RECOVERY_INTERVAL: Duration = Duration::from_secs(2);
const RECOVERY_BACKOFF: Duration = Duration::from_secs(10);
const RECOVERY_WINDOW: Duration = Duration::from_secs(5 * 60);
const MAX_RECOVERY_ATTEMPTS: u8 = 2;

pub(super) struct RecoveryAttempt {
    count: u8,
    next: Instant,
    started: Instant,
}

pub(super) struct RecoverySweepGuard(tokio::task::JoinHandle<()>);

impl Drop for RecoverySweepGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A pinned attention value does not hide the connection's underlying cause.
pub(super) fn recovery_message(session: &Session) -> String {
    if let AttentionState::NeedsYou { prompt, .. } = &session.attention.state {
        return prompt.clone();
    }
    recovery_reason_message(session)
}

fn recovery_reason_message(session: &Session) -> String {
    match &session.fence_reason {
        Some(FenceReason::OrphanAlive | FenceReason::ResumeLost { .. }) => {
            "The engine connection is recovering. Your interrupted turn will not restart automatically.".into()
        }
        Some(FenceReason::ProbeAmbiguous { detail }) => format!(
            "Recovery needs attention: {detail}. Check the previous engine before trying again."
        ),
        Some(FenceReason::RepeatedTurnFailures { detail, .. }) => {
            format!("The engine keeps failing: {detail}. Resolve this problem before sending another message.")
        }
        Some(FenceReason::IncarnationUnresolved { detail }) => {
            format!("The previous sandbox could not be confirmed: {detail}. Check its status before starting more work.")
        }
        Some(FenceReason::TerminalFlushMissing { detail }) => {
            format!("Some output from the previous sandbox is missing: {detail}. Review the sandbox before accepting the missing output.")
        }
        Some(FenceReason::SandboxLost { detail }) => {
            format!("The sandbox stopped: {detail}. Recovery is checking its final output.")
        }
        None => "The connection stopped without a recorded cause. Check the engine before trying again.".into(),
    }
}

fn recovery_cause(session: &Session) -> &str {
    match &session.fence_reason {
        Some(FenceReason::OrphanAlive) => "The previous engine process was still running",
        Some(
            FenceReason::ResumeLost { detail }
            | FenceReason::ProbeAmbiguous { detail }
            | FenceReason::RepeatedTurnFailures { detail, .. }
            | FenceReason::IncarnationUnresolved { detail }
            | FenceReason::SandboxLost { detail }
            | FenceReason::TerminalFlushMissing { detail },
        ) => detail,
        None => "The connection stopped without a recorded cause",
    }
}

impl CodeRuntime {
    pub(super) fn session_recovery_lock(&self, id: SessionId) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.recovery_locks.lock().expect("session recovery locks");
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&id).and_then(std::sync::Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(id, Arc::downgrade(&lock));
        lock
    }

    pub(super) fn ensure_recovery_sweep(self: &Arc<Self>) {
        if self.recovery_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let weak = Arc::downgrade(self);
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(RECOVERY_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Some(runtime) = weak.upgrade() else {
                    return;
                };
                if let Err(error) = runtime.recover_fenced_sessions().await {
                    tracing::warn!(error = %error.message(), "could not recover interrupted engine connections");
                }
            }
        });
        *self.recovery_sweep.lock().expect("recovery sweep") = Some(RecoverySweepGuard(task));
    }

    pub(super) async fn recover_fenced_sessions(&self) -> Result<(), ServerError> {
        if self.update_quiesce_active() {
            return Ok(());
        }
        let sessions = tidebreak_core::db::code::list_sessions_by_lifecycle_all_owners(
            &self.db,
            SessionLifecycle::Fenced,
        )
        .await?;
        for session in sessions {
            let _workspace_guard = match session.workspace_id {
                Some(id) => Some(self.workspace_lifecycle_lock(id).lock_owned().await),
                None => None,
            };
            // The per-session lock also covers explicit recovery, permission
            // mode changes, and ending a session. Re-read after acquiring it.
            let lock = self.session_recovery_lock(session.id);
            let Ok(_guard) = lock.try_lock_owned() else {
                continue;
            };
            let Some(session) = get_session(&self.db, &session.owner, session.id).await? else {
                continue;
            };
            if session.lifecycle != SessionLifecycle::Fenced || self.update_quiesce_active() {
                continue;
            }
            if let Some(workspace) = self.session_workspace(&session).await? {
                if workspace.status != CodeWorkspaceStatus::Active {
                    continue;
                }
            }
            self.recover_fenced_session(session).await?;
        }
        Ok(())
    }

    async fn recover_fenced_session(&self, mut session: Session) -> Result<(), ServerError> {
        let eligible = match (&session.execution_location, &session.fence_reason) {
            (
                ExecutionLocation::Machine,
                Some(FenceReason::OrphanAlive | FenceReason::ResumeLost { .. }),
            ) => true,
            (
                ExecutionLocation::Sandbox,
                Some(FenceReason::SandboxLost { .. } | FenceReason::TerminalFlushMissing { .. }),
            ) => {
                let mut row = tidebreak_core::db::code::latest_incarnation(
                    &self.db,
                    &session.owner,
                    session.id,
                )
                .await?;
                if let Some(stopped) = row.as_mut() {
                    if !stopped.terminal_events_journaled
                        && self
                            .completed_idle_scratch_is_drained(&session, stopped)
                            .await?
                    {
                        tidebreak_core::db::code::mark_incarnation_terminal_events_journaled(
                            &self.db,
                            &session.owner,
                            stopped.id,
                        )
                        .await?;
                        stopped.terminal_events_journaled = true;
                    }
                }
                match row {
                    Some(row)
                        if row.state == IncarnationState::Stopped
                            && row.terminal_events_journaled =>
                    {
                        if let Some((_, message)) = crate::code::remote::driver::recovery_block(
                            &row,
                            session.workspace_id.is_some(),
                        ) {
                            session.fence_reason = Some(FenceReason::IncarnationUnresolved {
                                detail: message.into(),
                            });
                            false
                        } else {
                            true
                        }
                    }
                    Some(row) if row.state == IncarnationState::Stopped => {
                        session.fence_reason = Some(FenceReason::TerminalFlushMissing {
                            detail: "The stopped sandbox did not deliver its final events".into(),
                        });
                        false
                    }
                    _ => {
                        session.fence_reason = Some(FenceReason::IncarnationUnresolved {
                            detail: "The previous sandbox has not been confirmed stopped".into(),
                        });
                        false
                    }
                }
            }
            _ => false,
        };
        if !eligible {
            return self.publish_recovery_blocker(session).await;
        }

        let now = Instant::now();
        let attempt = {
            let mut attempts = self.recovery_attempts.lock().expect("recovery attempts");
            let attempt = attempts.entry(session.id).or_insert(RecoveryAttempt {
                count: 0,
                next: now,
                started: now,
            });
            if now.duration_since(attempt.started) >= RECOVERY_WINDOW {
                *attempt = RecoveryAttempt {
                    count: 0,
                    next: now,
                    started: now,
                };
            }
            if now < attempt.next {
                return Ok(());
            }
            if attempt.count >= MAX_RECOVERY_ATTEMPTS {
                None
            } else {
                attempt.count += 1;
                attempt.next = now + RECOVERY_BACKOFF;
                Some(attempt.count)
            }
        };
        let Some(attempt) = attempt else {
            let detail = format!(
                "Automatic recovery failed again. Original cause: {}",
                recovery_cause(&session)
            );
            session.fence_reason = Some(
                if session.execution_location == ExecutionLocation::Sandbox {
                    FenceReason::IncarnationUnresolved { detail }
                } else {
                    FenceReason::ProbeAmbiguous { detail }
                },
            );
            return self.publish_recovery_blocker(session).await;
        };

        let already_paused =
            tidebreak_core::db::code::queue_paused(&self.db, &session.owner, session.id).await?;
        // A resumed worker normally drains its queue immediately. Recovery
        // never grants permission to run queued or interrupted work.
        tidebreak_core::db::code::set_queue_paused(&self.db, &session.owner, session.id, true)
            .await?;
        let result = if session.execution_location == ExecutionLocation::Sandbox {
            // Do not call remote driver.reap: it accepts missing terminal
            // output and cancellation failure on behalf of an explicit user.
            recovery::reap_session(&self.db, &self.bus, session.clone())
                .await
                .map_err(|error| {
                    ServerError::conflict_kind("session_not_reaped", error.to_string())
                })
        } else {
            self.reap_inner(&session.owner, session.id).await
        };
        match result {
            Ok(recovered) => {
                if !already_paused {
                    tidebreak_core::db::code::resume_empty_recovered_queue(
                        &self.db,
                        &recovered.owner,
                        recovered.id,
                    )
                    .await?;
                }
                // Preserve a manual pin. Otherwise show queued work that needs
                // review, or let the composer accept a fresh message.
                let queued = tidebreak_core::db::code::list_queued_turns(
                    &self.db,
                    &recovered.owner,
                    recovered.id,
                )
                .await?;
                let attention = if queued.is_empty() {
                    Attention::new(AttentionState::Idle, AttentionSource::Lifecycle)
                } else {
                    Attention::needs_you(
                        "Connection restored. Review the paused messages before sending them.",
                        AttentionSource::Lifecycle,
                    )
                };
                crate::code::attention::apply_attention(
                    &self.db,
                    &self.bus,
                    &recovered.owner,
                    recovered.id,
                    attention,
                    false,
                )
                .await?;
            }
            Err(error) => {
                let Some(mut current) = get_session(&self.db, &session.owner, session.id).await?
                else {
                    return Ok(());
                };
                if current.lifecycle == SessionLifecycle::Ended {
                    return Ok(());
                }
                let detail = error.message().to_owned();
                if attempt >= MAX_RECOVERY_ATTEMPTS || error.kind() == "session_not_reaped" {
                    let detail = format!(
                        "Automatic recovery failed: {detail}. {}",
                        recovery_cause(&session)
                    );
                    current.fence_reason = Some(
                        if current.execution_location == ExecutionLocation::Sandbox {
                            FenceReason::IncarnationUnresolved { detail }
                        } else {
                            FenceReason::ProbeAmbiguous { detail }
                        },
                    );
                    let reason = current
                        .fence_reason
                        .clone()
                        .expect("recovery failure has a cause");
                    recovery::fence_session(&self.db, &self.bus, &mut current, reason).await?;
                    self.publish_recovery_blocker(current).await?;
                } else {
                    // A launch can advance the epoch before failing. Retain
                    // that current identity and prevent turn admission until
                    // the bounded retry has settled it.
                    let reason = current
                        .fence_reason
                        .clone()
                        .or(session.fence_reason.clone())
                        .unwrap_or(FenceReason::ProbeAmbiguous { detail });
                    recovery::fence_session(&self.db, &self.bus, &mut current, reason).await?;
                }
            }
        }
        Ok(())
    }

    // An idle shutdown can omit the supervisor's goodbye after a successful
    // turn. Recover only scratch sessions whose complete output is durable.
    async fn completed_idle_scratch_is_drained(
        &self,
        session: &Session,
        row: &tidebreak_core::CodeSessionIncarnation,
    ) -> Result<bool, ServerError> {
        use crate::code::remote::wire::SandboxState;
        use tidebreak_core::db::code::{latest_incarnation, latest_turn, record_incarnation_spend};

        if session.workspace_id.is_some()
            || row.state != IncarnationState::Stopped
            || !matches!(row.stop_reason.as_deref(), Some("failed" | "expired"))
            || row.events_cursor <= 0
        {
            return Ok(false);
        }
        let Some(turn) = latest_turn(&self.db, &session.owner, session.id).await? else {
            return Ok(false);
        };
        if turn.status != tidebreak_core::TurnStatus::Completed
            || turn.ordinal < i64::from(row.starting_turn)
        {
            return Ok(false);
        }
        let (Some(remote), Some(sandbox_id)) = (self.remote_sessions(), row.sandbox_id.as_deref())
        else {
            return Ok(false);
        };
        {
            let now = Instant::now();
            let mut probes = self.recovery_probe_times.lock().expect("recovery probes");
            probes.retain(|_, next| *next > now);
            if probes.contains_key(&row.id) {
                return Ok(false);
            }
            probes.insert(row.id, now + RECOVERY_BACKOFF);
        }
        let status = tokio::time::timeout(
            Duration::from_secs(5),
            remote
                .provisioner
                .status(&session.owner, session.id, sandbox_id),
        )
        .await;
        let Ok(Ok(status)) = status else {
            return Ok(false);
        };
        if status.sandbox_id != sandbox_id
            || !matches!(status.state, SandboxState::Failed | SandboxState::Expired)
            || status.failure_reason.as_deref() != Some("idle_ceiling")
            || status.pending_messages != 0
            || status.repository_url.is_some()
            || status.latest_event_seq <= 0
            || status
                .spend_microusd
                .zip(status.spend_ceiling_microusd)
                .is_some_and(|(spend, ceiling)| spend >= ceiling)
        {
            return Ok(false);
        }
        if row.events_cursor < status.latest_event_seq {
            // Fenced sessions have no pump. Catch up one page per probe while
            // keeping the fence and all queue holds in place.
            let drain = tokio::time::timeout(
                Duration::from_secs(5),
                remote
                    .driver(&self.db, self.bus.as_ref())
                    .drain_stopped_events(session, row.id),
            )
            .await;
            if !matches!(drain, Ok(Ok(true))) {
                return Ok(false);
            }
        }
        // Status is remote I/O. Recheck the durable identity and turn before
        // accepting the drain; a newer incarnation cannot inherit this proof.
        let current = latest_incarnation(&self.db, &session.owner, session.id).await?;
        let latest = latest_turn(&self.db, &session.owner, session.id).await?;
        if !current.is_some_and(|current| {
            current.id == row.id
                && current.state == IncarnationState::Stopped
                && current.sandbox_id.as_deref() == Some(sandbox_id)
                && current.events_cursor >= status.latest_event_seq
        }) || !latest.is_some_and(|latest| {
            latest.id == turn.id && latest.status == tidebreak_core::TurnStatus::Completed
        }) {
            return Ok(false);
        }
        if let Some(spend) = status.spend_microusd {
            record_incarnation_spend(&self.db, &session.owner, row.id, spend).await?;
        }
        Ok(true)
    }

    pub(super) async fn retain_failed_recovery(
        &self,
        previous: &Session,
        detail: &str,
    ) -> Result<(), ServerError> {
        let Some(mut current) = get_session(&self.db, &previous.owner, previous.id).await? else {
            return Ok(());
        };
        if current.lifecycle == SessionLifecycle::Ended {
            return Ok(());
        }
        let detail = format!("Recovery failed: {detail}. {}", recovery_cause(previous));
        let reason = if current.execution_location == ExecutionLocation::Sandbox {
            FenceReason::IncarnationUnresolved { detail }
        } else {
            FenceReason::ProbeAmbiguous { detail }
        };
        recovery::fence_session(&self.db, &self.bus, &mut current, reason).await?;
        self.publish_recovery_blocker(current).await
    }

    async fn publish_recovery_blocker(&self, mut session: Session) -> Result<(), ServerError> {
        // Manual pins remain untouched; fence_reason still carries the cause
        // for connection controls, independently of attention badges.
        let message = recovery_reason_message(&session);
        crate::code::attention::replace_attention(
            &mut session,
            Attention::needs_you(message, AttentionSource::Lifecycle),
            false,
        );
        if let Some(current) = get_session(&self.db, &session.owner, session.id).await? {
            if current.lifecycle != SessionLifecycle::Fenced
                || current.spawn_epoch != session.spawn_epoch
            {
                return Ok(());
            }
            if current.attention == session.attention
                && current.fence_reason == session.fence_reason
            {
                return Ok(());
            }
        }
        crate::code::attention::persist_session(&self.db, &self.bus, &session).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
    use async_trait::async_trait;
    use std::sync::atomic::AtomicUsize;
    use tidebreak_core::{HarnessCaps, TurnStatus};
    use tidebreak_harness::HarnessSession;

    struct CountLaunches {
        inner: ScriptedAdapter,
        launches: Arc<AtomicUsize>,
        fail: bool,
    }

    #[async_trait]
    impl HarnessAdapter for CountLaunches {
        fn kind(&self) -> HarnessKind {
            self.inner.kind()
        }
        async fn probe(&self, host: &HostEnv) -> HarnessProbe {
            self.inner.probe(host).await
        }
        fn capabilities(&self, probe: &HarnessProbe) -> HarnessCaps {
            self.inner.capabilities(probe)
        }
        async fn launch(&self, spec: SessionSpec) -> Result<Box<dyn HarnessSession>, HarnessError> {
            self.launches.fetch_add(1, Ordering::SeqCst);
            assert!(
                spec.resume_ref.is_none(),
                "recovery must discard expired context"
            );
            if self.fail {
                return Err(HarnessError::NotFound);
            }
            self.inner.launch(spec).await
        }
    }

    pub(super) async fn fixture(
        reason: FenceReason,
        fail: bool,
    ) -> (
        tempfile::TempDir,
        Arc<CodeRuntime>,
        Session,
        Arc<AtomicUsize>,
    ) {
        fixture_at(reason, fail, ExecutionLocation::Machine).await
    }

    pub(super) async fn fixture_at(
        reason: FenceReason,
        fail: bool,
        location: ExecutionLocation,
    ) -> (
        tempfile::TempDir,
        Arc<CodeRuntime>,
        Session,
        Arc<AtomicUsize>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("code.db").display()
            ))
            .await
            .unwrap(),
        );
        let launches = Arc::new(AtomicUsize::new(0));
        let mut registry = AdapterRegistry::new();
        registry.register(Arc::new(CountLaunches {
            inner: ScriptedAdapter::new(plain_text_script()),
            launches: launches.clone(),
            fail,
        }));
        let runtime = Arc::new(CodeRuntime::with_registry(
            db,
            dir.path().to_path_buf(),
            registry,
        ));
        let session = Session {
            visibility: tidebreak_core::SessionVisibility::Private,
            id: SessionId::new(),
            owner: OwnerId::local(),
            owner_kind: None,
            workspace_id: None,
            kind: SessionKind::Interactive,
            harness_kind: HarnessKind::ClaudeCode,
            harness_version: None,
            harness_resume_ref: matches!(reason, FenceReason::ResumeLost { .. })
                .then(|| "expired-context".into()),
            permission_mode: PermissionMode::Plan,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            lifecycle: SessionLifecycle::Fenced,
            fence_reason: Some(reason.clone()),
            child_pid: None,
            child_process_identity: None,
            spawn_epoch: 1,
            attention: Attention::new(
                AttentionState::Fenced { reason },
                AttentionSource::Lifecycle,
            ),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: Utc::now(),
            execution_location: location,
            acts_as: None,
        };
        insert_session(&runtime.db, &session).await.unwrap();
        (dir, runtime, session, launches)
    }

    pub(super) fn resume_lost() -> FenceReason {
        FenceReason::ResumeLost {
            detail: "saved engine session expired".into(),
        }
    }

    #[tokio::test]
    async fn recovery_is_single_flight_and_does_not_replay_open_or_queued_turns() {
        let (_dir, runtime, session, launches) = fixture(resume_lost(), false).await;
        let turn_id = TurnId::new();
        tidebreak_core::db::code::insert_turn(
            &runtime.db,
            &session.owner,
            &Turn {
                actor: None,
                id: turn_id,
                session_id: session.id,
                ordinal: 1,
                status: TurnStatus::Running,
                model: None,
                fast_mode: false,
                user_input: "do not replay this".into(),
                user_input_blob_id: None,
                attachments: Vec::new(),
                checkpoint_ref: None,
                diffstat: None,
                usage: None,
                narrative: None,
                rewrite: None,
                started_at: Utc::now(),
                ended_at: None,
                park_ref: None,
                park_wait: None,
            },
        )
        .await
        .unwrap();
        let queued = QueuedTurn {
            id: TurnId::new(),
            session_id: session.id,
            message: "do not start queued work".into(),
            actor: None,
            attachments: Vec::new(),
            position: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        tidebreak_core::db::code::enqueue_queued_turn(&runtime.db, &session.owner, &queued)
            .await
            .unwrap();
        let (first, second) = tokio::join!(
            runtime.recover_fenced_sessions(),
            runtime.recover_fenced_sessions()
        );
        first.unwrap();
        second.unwrap();
        assert_eq!(launches.load(Ordering::SeqCst), 1);
        let current = runtime
            .get_session(&session.owner, session.id)
            .await
            .unwrap();
        assert_eq!(current.lifecycle, SessionLifecycle::Idle);
        assert!(
            matches!(current.attention.state, AttentionState::NeedsYou { ref prompt, .. } if prompt.contains("paused"))
        );
        assert!(
            tidebreak_core::db::code::queue_paused(&runtime.db, &session.owner, session.id)
                .await
                .unwrap()
        );
        assert_eq!(
            tidebreak_core::db::code::get_turn(&runtime.db, &session.owner, turn_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            TurnStatus::Interrupted
        );
        assert_eq!(
            tidebreak_core::db::code::list_queued_turns(&runtime.db, &session.owner, session.id)
                .await
                .unwrap()
                .len(),
            1
        );
        let events = list_events(
            &runtime.db,
            &session.owner,
            session.id,
            0,
            MAX_REPLAY_EVENTS,
        )
        .await
        .unwrap();
        assert!(events.events.iter().any(|event| matches!(&event.event, Event::HarnessNotice { message, .. } if message.contains("fresh engine"))));
    }

    #[tokio::test]
    async fn recovery_allows_a_fresh_message_without_releasing_the_queue_pause() {
        let (_dir, runtime, session, _) = fixture(resume_lost(), false).await;
        runtime.recover_fenced_sessions().await.unwrap();
        let result = runtime
            .submit_turn(
                &session.owner,
                session.id,
                "new instruction".into(),
                None,
                None,
                vec![],
                None,
            )
            .await
            .unwrap();
        assert!(matches!(result, SubmitTurnOutcome::Ran(_)));
    }

    #[tokio::test]
    async fn failed_launch_backs_off_then_stops_with_a_concrete_blocker() {
        let (_dir, runtime, session, launches) = fixture(resume_lost(), true).await;
        runtime.recover_fenced_sessions().await.unwrap();
        runtime.recover_fenced_sessions().await.unwrap();
        assert_eq!(
            launches.load(Ordering::SeqCst),
            1,
            "the second sweep respects backoff"
        );
        runtime
            .recovery_attempts
            .lock()
            .unwrap()
            .get_mut(&session.id)
            .unwrap()
            .next = Instant::now();
        runtime.recover_fenced_sessions().await.unwrap();
        let current = runtime
            .get_session(&session.owner, session.id)
            .await
            .unwrap();
        assert!(
            matches!(&current.attention.state, AttentionState::NeedsYou { prompt, .. } if prompt.contains("engine binary not found"))
        );
        assert!(
            matches!(&current.fence_reason, Some(FenceReason::ProbeAmbiguous { detail }) if detail.starts_with("Automatic recovery failed:"))
        );
        runtime.recover_fenced_sessions().await.unwrap();
        assert_eq!(
            launches.load(Ordering::SeqCst),
            2,
            "a blocked connection cannot respawn again"
        );
    }

    #[tokio::test]
    async fn repeated_failures_preserve_manual_pins_and_do_not_respawn() {
        let reason = FenceReason::RepeatedTurnFailures {
            count: 3,
            detail: "Sign in to your provider".into(),
        };
        let (_dir, runtime, mut session, launches) = fixture(reason.clone(), false).await;
        session.attention = Attention::manual("review this later");
        tidebreak_core::db::code::replace_session_attention(
            &runtime.db,
            &session.owner,
            session.id,
            &session.attention,
            true,
        )
        .await
        .unwrap();
        runtime.recover_fenced_sessions().await.unwrap();
        let current = runtime
            .get_session(&session.owner, session.id)
            .await
            .unwrap();
        assert_eq!(current.attention, session.attention);
        assert_eq!(current.fence_reason, Some(reason.clone()));
        let mut updates = runtime.bus.subscribe_updates(&session.owner);
        crate::code::attention::emit_digest(&runtime.db, &runtime.bus, &current).await;
        let crate::code::bus::CodeLiveUpdate::Digest(digest) = updates.recv().await.unwrap() else {
            panic!("expected digest")
        };
        assert_eq!(digest.attention, session.attention);
        assert_eq!(digest.fence_reason, Some(reason));
        assert!(recovery_message(&current).contains("Sign in"));
        assert_eq!(launches.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_missing_process_identity_blocks_recovery_without_signaling() {
        let (_dir, runtime, mut session, launches) = fixture(FenceReason::OrphanAlive, false).await;
        session.child_pid = Some(i64::from(std::process::id()));
        tidebreak_core::db::code::save_session(&runtime.db, &session)
            .await
            .unwrap();
        runtime.recover_fenced_sessions().await.unwrap();
        let current = runtime
            .get_session(&session.owner, session.id)
            .await
            .unwrap();
        assert_eq!(current.lifecycle, SessionLifecycle::Fenced);
        assert_eq!(current.child_pid, session.child_pid);
        assert!(
            matches!(current.attention.state, AttentionState::NeedsYou { ref prompt, .. } if prompt.contains("identity"))
        );
        assert_eq!(launches.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_remote_sandbox_without_confirmed_final_state_needs_input() {
        let (_dir, runtime, session, launches) = fixture_at(
            FenceReason::SandboxLost {
                detail: "connection lost".into(),
            },
            false,
            ExecutionLocation::Sandbox,
        )
        .await;
        runtime.recover_fenced_sessions().await.unwrap();
        let current = runtime
            .get_session(&session.owner, session.id)
            .await
            .unwrap();
        assert!(matches!(
            current.fence_reason,
            Some(FenceReason::IncarnationUnresolved { .. })
        ));
        assert!(matches!(
            current.attention.state,
            AttentionState::NeedsYou { .. }
        ));
        assert_eq!(launches.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn foreign_reap_cannot_reset_the_retry_budget() {
        let (_dir, runtime, session, _) = fixture(resume_lost(), true).await;
        runtime.recover_fenced_sessions().await.unwrap();
        assert!(runtime
            .reap(&OwnerId::new("other").unwrap(), session.id)
            .await
            .is_err());
        assert_eq!(
            runtime
                .recovery_attempts
                .lock()
                .unwrap()
                .get(&session.id)
                .unwrap()
                .count,
            1
        );
    }

    #[tokio::test]
    async fn a_new_blocker_replaces_a_stale_approval_prompt() {
        let (_dir, runtime, session, _) = fixture(
            FenceReason::RepeatedTurnFailures {
                count: 3,
                detail: "provider signed out".into(),
            },
            false,
        )
        .await;
        tidebreak_core::db::code::replace_session_attention(
            &runtime.db,
            &session.owner,
            session.id,
            &Attention::needs_you("approve a write", AttentionSource::Structured),
            false,
        )
        .await
        .unwrap();
        runtime.recover_fenced_sessions().await.unwrap();
        let current = runtime
            .get_session(&session.owner, session.id)
            .await
            .unwrap();
        assert!(
            matches!(current.attention.state, AttentionState::NeedsYou { ref prompt, .. } if prompt.contains("provider signed out"))
        );
    }
}

#[cfg(test)]
mod remote_and_admission_tests {
    use super::tests::{fixture, fixture_at, resume_lost};
    use super::*;
    use tidebreak_core::db::code::{
        activate_incarnation, create_incarnation_intent, ingest_incarnation_event,
        latest_incarnation, stop_incarnation, IncarnationSideEffects,
    };

    #[tokio::test]
    async fn automatic_scratch_recovery_preserves_output_and_spend_gates() {
        for (stop, checkpoint, terminal, recovered) in [
            ("failed", None, true, true),
            ("expired", None, true, true),
            ("failed", None, false, false),
            ("ceiling_exceeded", Some("refs/heads/wip"), true, false),
            ("expired", Some("refs/heads/wip"), false, false),
            ("expired", Some("refs/heads/wip"), true, true),
        ] {
            let (_dir, runtime, session, launches) = fixture_at(
                FenceReason::SandboxLost {
                    detail: stop.into(),
                },
                false,
                ExecutionLocation::Sandbox,
            )
            .await;
            let tidebreak_core::IncarnationAdmission::Admitted(row) =
                create_incarnation_intent(&runtime.db, &session.owner, session.id, 1, 1)
                    .await
                    .unwrap()
            else {
                panic!("expected admission")
            };
            activate_incarnation(&runtime.db, &session.owner, row.id, "sandbox-test")
                .await
                .unwrap();
            ingest_incarnation_event(
                &runtime.db,
                &session.owner,
                session.id,
                session.spawn_epoch,
                row.id,
                1,
                IncarnationSideEffects {
                    journal: &[],
                    task_output: None,
                    wip_ref: checkpoint,
                    terminal_events_journaled: terminal,
                },
            )
            .await
            .unwrap();
            stop_incarnation(&runtime.db, &session.owner, row.id, Some(stop))
                .await
                .unwrap();
            runtime.recover_fenced_sessions().await.unwrap();
            let current = runtime
                .get_session(&session.owner, session.id)
                .await
                .unwrap();
            assert_eq!(
                current.lifecycle == SessionLifecycle::Idle,
                recovered,
                "stop={stop} checkpoint={checkpoint:?} terminal={terminal}"
            );
            let latest = latest_incarnation(&runtime.db, &session.owner, session.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(latest.id, row.id, "recovery must not provision a successor");
            assert_eq!(
                latest.terminal_events_journaled, terminal,
                "recovery must never accept missing output"
            );
            assert_eq!(latest.stop_reason.as_deref(), Some(stop));
            assert_eq!(launches.load(Ordering::SeqCst), 0);
            if recovered {
                assert!(
                    !tidebreak_core::db::code::queue_paused(
                        &runtime.db,
                        &session.owner,
                        session.id,
                    )
                    .await
                    .unwrap(),
                    "an empty recovered queue must accept fresh Slack input"
                );
            }
            if !recovered {
                assert!(matches!(
                    current.attention.state,
                    AttentionState::NeedsYou { .. }
                ));
                assert!(!current.fence_reason.unwrap().blocks_workspace());
            }
        }
    }

    #[tokio::test]
    async fn rejected_reap_leaves_a_healthy_queue_unchanged() {
        let (_dir, runtime, session, _) = fixture(resume_lost(), false).await;
        runtime.recover_fenced_sessions().await.unwrap();
        runtime
            .set_queue_paused(&session.owner, session.id, false)
            .await
            .unwrap();
        assert_eq!(
            runtime
                .reap(&session.owner, session.id)
                .await
                .unwrap_err()
                .kind(),
            "not_fenced"
        );
        assert!(
            !tidebreak_core::db::code::queue_paused(&runtime.db, &session.owner, session.id)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn turn_admission_waits_for_recovery_and_rereads_the_session() {
        let (_dir, runtime, session, _) = fixture(resume_lost(), false).await;
        runtime.recover_fenced_sessions().await.unwrap();
        let lock = runtime.session_recovery_lock(session.id).lock_owned().await;
        let task_runtime = runtime.clone();
        let owner = session.owner.clone();
        let mut turn = tokio::spawn(async move {
            task_runtime
                .submit_turn(
                    &owner,
                    session.id,
                    "new message".into(),
                    None,
                    None,
                    vec![],
                    None,
                )
                .await
        });
        assert!(tokio::time::timeout(Duration::from_millis(30), &mut turn)
            .await
            .is_err());
        let mut current = runtime
            .get_session(&session.owner, session.id)
            .await
            .unwrap();
        current.lifecycle = SessionLifecycle::Ended;
        tidebreak_core::db::code::save_session(&runtime.db, &current)
            .await
            .unwrap();
        drop(lock);
        let error = turn
            .await
            .unwrap()
            .err()
            .expect("ending during recovery refuses the turn");
        assert_eq!(error.kind(), "session_ended");
    }

    #[tokio::test]
    async fn unused_recovery_locks_are_pruned() {
        let (_dir, runtime, _, _) = fixture(resume_lost(), false).await;
        for _ in 0..100 {
            drop(runtime.session_recovery_lock(SessionId::new()));
        }
        assert_eq!(runtime.recovery_locks.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_failed_explicit_retry_keeps_the_connection_blocked() {
        let (_dir, runtime, session, _) = fixture(resume_lost(), true).await;
        assert!(runtime.reap(&session.owner, session.id).await.is_err());
        let current = runtime
            .get_session(&session.owner, session.id)
            .await
            .unwrap();
        assert_eq!(current.lifecycle, SessionLifecycle::Fenced);
        assert!(
            matches!(current.attention.state, AttentionState::NeedsYou { ref prompt, .. } if prompt.contains("engine binary not found"))
        );
        assert!(!runtime.has_worker(session.id));
    }
}
