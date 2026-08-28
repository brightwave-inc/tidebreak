//! Drive a remote session's sandbox: turns, reincarnation, reap, and the
//! stale-intent sweep.
//!
//! A remote session's engine lives in a sandbox the environment provisions;
//! this module owns the lifecycle decisions the transport-only provisioner
//! deliberately does not. A turn reaches a live sandbox as an inbox message.
//! When the sandbox is gone, the next turn reincarnates: reserve the
//! incarnation (the durable equivalent of the per-workspace turn lock),
//! spawn against the predecessor's last WIP checkpoint ref, and record the
//! sandbox on the row before the first event is read. Stop and reincarnate
//! serialize through the incarnation record — a turn that lands while the
//! predecessor's terminal events are still in flight waits rather than
//! resuming without them.

use std::sync::Arc;

use tracing::warn;

use tidebreak_core::db::code::{
    activate_incarnation, create_incarnation_intent, insert_turn, latest_incarnation, latest_turn,
    save_turn, stale_incarnation_intents_all_owners, stop_incarnation,
};
use tidebreak_core::{
    Attention, AttentionSource, CodeRepo, CodeSession, CodeSessionId, CodeSessionIncarnation,
    CodeSessionLifecycle, CodeTurn, CodeTurnId, CodeTurnStatus, CodeWorkspace, DbStore,
    FenceReason, IncarnationAdmission, IncarnationState, OwnerId,
};

use super::super::attention::persist_session;
use super::super::bus::CodeEventBus;
use super::super::recovery;
use super::ingest::{ingest_events, IngestBinding, IngestOutcome};
use super::wire::{EventCursor, SandboxMessage, SpawnArguments};
use super::{RemoteSandboxError, SandboxProvisioner};

/// How long an unactivated intent may sit before the sweep closes it: long
/// enough for a slow spawn round trip, short enough that a crashed server
/// does not hold a concurrency slot for an afternoon.
pub(crate) const STALE_INTENT_AGE: chrono::Duration = chrono::Duration::minutes(10);

/// Spawn-time settings for one remote session's sandboxes.
#[derive(Clone, Debug)]
pub(crate) struct RemoteSpawnSettings {
    /// Administrator-defined profile on the runtime endpoint.
    pub profile: String,
    /// Concurrent live incarnations one owner may hold.
    pub incarnation_cap: usize,
    /// Per-spawn spend ceiling in micro-USD, when one is set.
    pub spend_ceiling_microusd: Option<i64>,
}

/// What one submitted turn became.
#[derive(Debug)]
pub(crate) enum RemoteTurnOutcome {
    /// The turn reached the live sandbox's inbox.
    Delivered {
        /// The turn row, running. Boxed: the rows dwarf the refusals.
        turn: Box<CodeTurn>,
    },
    /// No sandbox was live; one was provisioned to carry this turn.
    Reincarnated {
        /// The turn row, running.
        turn: Box<CodeTurn>,
        /// The incarnation now active.
        incarnation: Box<CodeSessionIncarnation>,
    },
    /// The owner's concurrency cap refused, naming what runs.
    CapExhausted {
        /// Sessions holding the live incarnations.
        running: Vec<CodeSessionId>,
    },
    /// The predecessor stopped but its terminal events are not journaled
    /// yet. Retry after a pump; resuming now would miss its last output.
    FlushPending,
    /// A reincarnation is already in flight for this session.
    ReincarnationInFlight,
}

/// The driver one remote session's lifecycle calls go through: the store,
/// the live bus, the transport, and the spawn settings, borrowed together
/// so every operation reads the same world.
pub(crate) struct RemoteDriver<'a> {
    /// The store every row lives in.
    pub db: &'a Arc<DbStore>,
    /// Live-update bus the journal publishes to.
    pub bus: &'a CodeEventBus,
    /// The environment transport.
    pub provisioner: &'a dyn SandboxProvisioner,
    /// Spawn-time settings.
    pub settings: &'a RemoteSpawnSettings,
}

/// Whether a refusal's own description names this ref.
fn refusal_names(error: &RemoteSandboxError, reference: &str) -> bool {
    match error {
        RemoteSandboxError::Refused { message, .. } => message.contains(reference),
        _ => false,
    }
}

/// One pump of a remote session: read events after the durable cursor,
/// project them, settle turn rows, and close the incarnation when the
/// environment says the sandbox ended.
#[derive(Debug, Default)]
pub(crate) struct PumpReport {
    /// Sandbox event sequences ingested this pump.
    pub ingested: u64,
    /// Whether the incarnation was closed this pump.
    pub incarnation_stopped: bool,
    /// The fence applied, when the read demanded one.
    pub fenced: Option<FenceReason>,
}

/// The stop reason recorded when the environment ends a sandbox.
fn state_token<'a>(
    _events: &[super::wire::SandboxEvent],
    state: super::wire::SandboxState,
) -> &'a str {
    match state {
        super::wire::SandboxState::Completed => "completed",
        super::wire::SandboxState::Failed => "failed",
        super::wire::SandboxState::Cancelled => "cancelled",
        super::wire::SandboxState::Expired => "expired",
        super::wire::SandboxState::CeilingExceeded => "ceiling_exceeded",
        _ => "ended",
    }
}

/// Settle the running turn row from the batch's terminal turn events.
async fn settle_turn_rows(
    db: &Arc<DbStore>,
    owner: &OwnerId,
    running_turn: Option<CodeTurn>,
    events: &[super::wire::SandboxEvent],
) -> Result<(), tidebreak_core::AgentError> {
    let Some(mut turn) = running_turn else {
        return Ok(());
    };
    for event in events {
        let status = match event.kind.as_str() {
            "turn_completed" => {
                let success = event
                    .payload
                    .get("exit_code")
                    .and_then(serde_json::Value::as_i64)
                    == Some(0);
                if success {
                    CodeTurnStatus::Completed
                } else {
                    CodeTurnStatus::Failed
                }
            }
            "turn_interrupted" => CodeTurnStatus::Interrupted,
            _ => continue,
        };
        turn.status = status;
        turn.ended_at = Some(chrono::Utc::now());
        save_turn(db, owner, &turn).await?;
        break;
    }
    Ok(())
}

impl RemoteDriver<'_> {
    /// Submit one user turn to a remote session.
    ///
    /// The session row is updated (lifecycle, attention) on success; the caller
    /// persists nothing else. `session`, `workspace`, and `repo` are the current
    /// rows — the caller owns loading and authorization.
    pub(crate) async fn submit_turn(
        &self,
        session: &mut CodeSession,
        workspace: &CodeWorkspace,
        repo: &CodeRepo,
        text: &str,
    ) -> Result<RemoteTurnOutcome, tidebreak_core::AgentError> {
        let (db, bus, provisioner, settings) = (self.db, self.bus, self.provisioner, self.settings);
        let owner = session.owner.clone();
        let ordinal = latest_turn(db, &owner, session.id)
            .await?
            .map_or(1, |turn| turn.ordinal + 1);

        let current = latest_incarnation(db, &owner, session.id).await?;
        match current.as_ref().map(|row| row.state) {
            Some(IncarnationState::Intent) => return Ok(RemoteTurnOutcome::ReincarnationInFlight),
            Some(IncarnationState::Active) => {
                let row = current.as_ref().expect("state was just observed");
                let Some(sandbox_id) = row.sandbox_id.as_deref() else {
                    return Err(tidebreak_core::AgentError::Store(format!(
                        "active incarnation {} has no sandbox id",
                        row.id
                    )));
                };
                let message = SandboxMessage {
                    body: text.to_owned(),
                    interrupt: false,
                };
                message
                    .validate()
                    .map_err(tidebreak_core::AgentError::Store)?;
                match provisioner.send(&owner, sandbox_id, &message).await {
                    Ok(_) => {
                        let turn = start_turn_row(db, bus, session, ordinal, text).await?;
                        return Ok(RemoteTurnOutcome::Delivered {
                            turn: Box::new(turn),
                        });
                    }
                    Err(RemoteSandboxError::Refused { code, message, .. }) => {
                        // The environment no longer takes messages for this
                        // sandbox: it ended without this server having drained
                        // the news yet. Close the record and fall through to
                        // reincarnation only once its terminal events are in.
                        warn!(
                            session = %session.id,
                            %code,
                            "a live sandbox refused a message; closing the incarnation ({message})"
                        );
                        stop_incarnation(db, &owner, row.id, Some(&code)).await?;
                        return Ok(RemoteTurnOutcome::FlushPending);
                    }
                    Err(error) => {
                        return Err(tidebreak_core::AgentError::Store(error.to_string()));
                    }
                }
            }
            Some(IncarnationState::Stopped) | None => {}
        }
        if let Some(predecessor) = &current {
            if !predecessor.terminal_events_journaled {
                return Ok(RemoteTurnOutcome::FlushPending);
            }
        }

        // Reserve before provisioning: the intent row is the durable equivalent
        // of the per-workspace turn lock, and it is also the owner's cap slot.
        let admission = create_incarnation_intent(
            db,
            &owner,
            session.id,
            i32::try_from(ordinal).unwrap_or(1),
            settings.incarnation_cap,
        )
        .await?;
        let intent = match admission {
            IncarnationAdmission::Admitted(row) => *row,
            IncarnationAdmission::CapExhausted { running } => {
                return Ok(RemoteTurnOutcome::CapExhausted { running });
            }
        };

        let resume_ref = current
            .as_ref()
            .and_then(|row| row.last_wip_ref.clone())
            .unwrap_or_else(|| workspace.base_ref.clone());
        let resumed_from_wip = current
            .as_ref()
            .is_some_and(|row| row.last_wip_ref.is_some());
        let arguments = SpawnArguments {
            profile: settings.profile.clone(),
            harness: "custom".to_owned(),
            mode: Some("turn".to_owned()),
            task: text.to_owned(),
            repository: Some(repository_url(repo)?),
            repository_ref: Some(resume_ref.clone()),
            repositories: Vec::new(),
            apps: Vec::new(),
            model: session.model.clone(),
            reasoning_effort: session
                .reasoning_effort
                .map(|effort| effort.as_str().to_owned()),
            subscription: None,
            idle_timeout_seconds: None,
            wall_clock_timeout_seconds: None,
            spend_ceiling_microusd: settings.spend_ceiling_microusd,
            max_turns: None,
        };
        match provisioner.spawn(&owner, &arguments).await {
            Ok(lease) => {
                activate_incarnation(db, &owner, intent.id, &lease.sandbox_id).await?;
                let incarnation = latest_incarnation(db, &owner, session.id)
                    .await?
                    .ok_or_else(|| {
                        tidebreak_core::AgentError::Store(
                            "the activated incarnation vanished".to_owned(),
                        )
                    })?;
                let turn = start_turn_row(db, bus, session, ordinal, text).await?;
                Ok(RemoteTurnOutcome::Reincarnated {
                    turn: Box::new(turn),
                    incarnation: Box::new(incarnation),
                })
            }
            Err(error) => {
                // The reservation must not outlive the spawn it reserved for.
                stop_incarnation(db, &owner, intent.id, Some("spawn_failed")).await?;
                if resumed_from_wip && refusal_names(&error, &resume_ref) {
                    // The WIP ref the predecessor pushed is gone from the
                    // origin: the resume state no longer exists, and retrying
                    // would refuse identically. Fence so a reap starts fresh.
                    recovery::fence_session(
                        db,
                        bus,
                        session,
                        FenceReason::ResumeLost {
                            detail: format!("the WIP checkpoint ref {resume_ref} is gone"),
                        },
                    )
                    .await?;
                }
                Err(tidebreak_core::AgentError::Store(error.to_string()))
            }
        }
    }

    /// Read and apply everything new from the session's live sandbox.
    ///
    /// Idle when no incarnation is active. The caller schedules pumps; this
    /// function is safe to call on any cadence because the cursor is durable
    /// and replays are no-ops.
    pub(crate) async fn pump(
        &self,
        session: &mut CodeSession,
        wait_seconds: u16,
    ) -> Result<PumpReport, tidebreak_core::AgentError> {
        let (db, bus, provisioner) = (self.db, self.bus, self.provisioner);
        let owner = session.owner.clone();
        let mut report = PumpReport::default();
        let Some(row) = latest_incarnation(db, &owner, session.id).await? else {
            return Ok(report);
        };
        if row.state != IncarnationState::Active {
            return Ok(report);
        }
        let Some(sandbox_id) = row.sandbox_id.clone() else {
            return Ok(report);
        };

        let read = match provisioner
            .events(
                &owner,
                &sandbox_id,
                EventCursor {
                    after_seq: Some(row.events_cursor),
                    limit: None,
                    wait_seconds: Some(u32::from(wait_seconds)),
                },
            )
            .await
        {
            Ok(read) => read,
            Err(error) if error.is_retryable() => {
                // A transport fault is the next pump's problem, not a lifecycle
                // signal.
                return Ok(report);
            }
            Err(error) => {
                return Err(tidebreak_core::AgentError::Store(error.to_string()));
            }
        };

        let running_turn = latest_turn(db, &owner, session.id)
            .await?
            .filter(|turn| turn.status == CodeTurnStatus::Running);
        let binding = IngestBinding {
            owner: owner.clone(),
            session_id: session.id,
            spawn_epoch: session.spawn_epoch,
            incarnation: row.id,
            harness_kind: session.harness_kind,
            turn_id: running_turn.as_ref().map(|turn| turn.id),
        };
        let outcome: IngestOutcome = ingest_events(db, bus, &binding, &read).await?;
        report.ingested = outcome.ingested;

        settle_turn_rows(db, &owner, running_turn, &read.events).await?;

        if read.state.is_terminal() {
            stop_incarnation(
                db,
                &owner,
                row.id,
                Some(state_token(&read.events, read.state)),
            )
            .await?;
            report.incarnation_stopped = true;
            if session.lifecycle == CodeSessionLifecycle::Running {
                session.lifecycle = CodeSessionLifecycle::Idle;
                let _ = persist_session(db, bus, session).await?;
            }
        }
        if let Some(reason) = outcome.fence {
            report.fenced = Some(reason.clone());
            recovery::fence_session(db, bus, session, reason).await?;
        }
        Ok(report)
    }

    /// Reap a fenced remote session: cancel whatever the environment still
    /// holds, close the incarnation record, and resolve the fence.
    ///
    /// Unlike a local reap, nothing is relaunched — the next turn reincarnates
    /// on demand.
    pub(crate) async fn reap(
        &self,
        session: CodeSession,
    ) -> Result<CodeSession, recovery::ReapSessionError> {
        let (db, bus, provisioner) = (self.db, self.bus, self.provisioner);
        let owner = session.owner.clone();
        if let Ok(Some(row)) = latest_incarnation(db, &owner, session.id).await {
            if row.state != IncarnationState::Stopped {
                if let Some(sandbox_id) = row.sandbox_id.as_deref() {
                    // Best effort: the reap must not hang on an environment
                    // that is already gone.
                    if let Err(error) = provisioner.cancel(&owner, sandbox_id).await {
                        warn!(
                            session = %session.id,
                            %error,
                            "a reap could not cancel the remote sandbox; its own ceilings bound it"
                        );
                    }
                }
                let _ = stop_incarnation(db, &owner, row.id, Some("reaped")).await;
            }
        }
        recovery::reap_session(db, bus, session).await
    }
}

/// Close intent rows whose spawn outcome nothing recorded, and fence their
/// sessions so the person sees why nothing is running.
///
/// An intent that never activated is a crash between provision and store.
/// The sandbox it may have spawned is unknown to this server, so it cannot
/// be cancelled from here; the ceilings requested at spawn bound it.
pub(crate) async fn sweep_stale_intents(
    db: &Arc<DbStore>,
    bus: &CodeEventBus,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<u64, tidebreak_core::AgentError> {
    let cutoff = now - STALE_INTENT_AGE;
    let stale = stale_incarnation_intents_all_owners(db, cutoff).await?;
    let mut closed = 0;
    for row in stale {
        stop_incarnation(db, &row.owner, row.id, Some("intent_expired")).await?;
        closed += 1;
        let Some(mut session) =
            tidebreak_core::db::code::get_session(db, &row.owner, row.session_id).await?
        else {
            continue;
        };
        recovery::fence_session(
            db,
            bus,
            &mut session,
            FenceReason::IncarnationUnresolved {
                detail: format!(
                    "a sandbox reservation from {} never recorded its spawn",
                    row.created_at.format("%Y-%m-%d %H:%M UTC")
                ),
            },
        )
        .await?;
    }
    Ok(closed)
}

/// The HTTPS clone URL spawn declares, from the repo's recorded origin.
fn repository_url(repo: &CodeRepo) -> Result<String, tidebreak_core::AgentError> {
    match (&repo.origin_host, &repo.origin_owner, &repo.origin_name) {
        (Some(host), Some(owner), Some(name)) => Ok(format!("https://{host}/{owner}/{name}")),
        _ => Err(tidebreak_core::AgentError::Store(format!(
            "repo {} has no recorded origin; a remote session needs one to clone",
            repo.id
        ))),
    }
}

/// Insert the running turn row and mark the session working.
async fn start_turn_row(
    db: &Arc<DbStore>,
    bus: &CodeEventBus,
    session: &mut CodeSession,
    ordinal: i64,
    text: &str,
) -> Result<CodeTurn, tidebreak_core::AgentError> {
    let turn = CodeTurn {
        id: CodeTurnId::new(),
        session_id: session.id,
        ordinal,
        status: CodeTurnStatus::Running,
        model: session.model.clone(),
        fast_mode: session.fast_mode,
        user_input: text.to_owned(),
        user_input_blob_id: None,
        attachments: Vec::new(),
        checkpoint_ref: None,
        diffstat: None,
        usage: None,
        narrative: None,
        started_at: chrono::Utc::now(),
        ended_at: None,
    };
    insert_turn(db, &session.owner, &turn).await?;
    session.lifecycle = CodeSessionLifecycle::Running;
    super::super::attention::replace_attention(
        session,
        Attention::working(AttentionSource::Lifecycle),
        false,
    );
    let _ = persist_session(db, bus, session).await?;
    Ok(turn)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::json;

    use tidebreak_core::db::code::get_session;
    use tidebreak_core::AttentionState;

    use super::super::fixtures::seed;
    use super::super::wire::{
        MessageReceipt, SandboxEvent, SandboxEvents, SandboxLease, SandboxState, SandboxStatus,
    };
    use super::*;

    #[derive(Default)]
    struct FakeProvisioner {
        spawns: Mutex<Vec<SpawnArguments>>,
        spawn_results: Mutex<VecDeque<Result<SandboxLease, RemoteSandboxError>>>,
        sends: Mutex<Vec<(String, String)>>,
        send_results: Mutex<VecDeque<Result<MessageReceipt, RemoteSandboxError>>>,
        event_reads: Mutex<VecDeque<SandboxEvents>>,
        cancels: Mutex<Vec<String>>,
    }

    fn lease(sandbox_id: &str) -> SandboxLease {
        SandboxLease {
            sandbox_id: sandbox_id.to_owned(),
            state: SandboxState::Pending,
            latest_event_seq: 0,
            expires_in_seconds: 7200,
        }
    }

    fn receipt() -> MessageReceipt {
        MessageReceipt {
            seq: 1,
            interrupt: false,
            pending_messages: 0,
        }
    }

    fn event(seq: i64, kind: &str, payload: serde_json::Value) -> SandboxEvent {
        SandboxEvent {
            seq,
            kind: kind.to_owned(),
            payload,
            created_at: String::new(),
        }
    }

    fn read(state: SandboxState, latest: i64, events: Vec<SandboxEvent>) -> SandboxEvents {
        SandboxEvents {
            sandbox_id: "sb-1".to_owned(),
            state,
            latest_event_seq: latest,
            events,
        }
    }

    #[async_trait]
    impl SandboxProvisioner for FakeProvisioner {
        async fn spawn(
            &self,
            _owner: &OwnerId,
            arguments: &SpawnArguments,
        ) -> Result<SandboxLease, RemoteSandboxError> {
            self.spawns.lock().unwrap().push(arguments.clone());
            self.spawn_results
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(lease("sb-next")))
        }

        async fn status(
            &self,
            _owner: &OwnerId,
            sandbox_id: &str,
        ) -> Result<SandboxStatus, RemoteSandboxError> {
            Ok(SandboxStatus {
                sandbox_id: sandbox_id.to_owned(),
                state: SandboxState::Running,
                failure_reason: None,
                termination_reason: None,
                latest_event_seq: 0,
                pending_messages: 0,
                spend_microusd: None,
                spend_ceiling_microusd: None,
                possibly_stalled: false,
                repository_url: None,
                completed_at: None,
            })
        }

        async fn events(
            &self,
            _owner: &OwnerId,
            _sandbox_id: &str,
            _cursor: EventCursor,
        ) -> Result<SandboxEvents, RemoteSandboxError> {
            self.event_reads
                .lock()
                .unwrap()
                .pop_front()
                .ok_or(RemoteSandboxError::Unavailable {
                    operation: "events",
                    detail: "no scripted read".to_owned(),
                })
        }

        async fn send(
            &self,
            _owner: &OwnerId,
            sandbox_id: &str,
            message: &SandboxMessage,
        ) -> Result<MessageReceipt, RemoteSandboxError> {
            self.sends
                .lock()
                .unwrap()
                .push((sandbox_id.to_owned(), message.body.clone()));
            self.send_results
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(receipt()))
        }

        async fn cancel(
            &self,
            _owner: &OwnerId,
            sandbox_id: &str,
        ) -> Result<(), RemoteSandboxError> {
            self.cancels.lock().unwrap().push(sandbox_id.to_owned());
            Ok(())
        }
    }

    fn settings() -> RemoteSpawnSettings {
        RemoteSpawnSettings {
            profile: "tidebreak-remote".to_owned(),
            incarnation_cap: 2,
            spend_ceiling_microusd: Some(5_000_000),
        }
    }

    macro_rules! driver {
        ($db:expr, $bus:expr, $fake:expr, $settings:expr) => {
            RemoteDriver {
                db: $db,
                bus: $bus,
                provisioner: $fake,
                settings: $settings,
            }
        };
    }

    /// A first turn on a fresh remote session reserves, spawns from the
    /// workspace base ref, activates, and records the running turn.
    #[tokio::test]
    async fn the_first_turn_provisions_a_sandbox_from_the_base_ref() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, mut session, workspace, repo) = seed(dir.path()).await;
        let fake = FakeProvisioner::default();
        let settings = settings();
        let driver = driver!(&db, &bus, &fake, &settings);

        let outcome = driver
            .submit_turn(&mut session, &workspace, &repo, "build it")
            .await
            .unwrap();
        let RemoteTurnOutcome::Reincarnated { turn, incarnation } = outcome else {
            panic!("expected a reincarnation");
        };
        assert_eq!(turn.ordinal, 1);
        assert_eq!(turn.status, CodeTurnStatus::Running);
        assert_eq!(incarnation.incarnation, 1);
        assert_eq!(incarnation.state, IncarnationState::Active);
        assert_eq!(incarnation.sandbox_id.as_deref(), Some("sb-next"));

        let spawns = fake.spawns.lock().unwrap();
        assert_eq!(spawns.len(), 1);
        assert_eq!(
            spawns[0].repository.as_deref(),
            Some("https://github.com/acme/tools")
        );
        assert_eq!(spawns[0].repository_ref.as_deref(), Some("main"));
        assert_eq!(spawns[0].task, "build it");
        assert_eq!(spawns[0].mode.as_deref(), Some("turn"));
        assert_eq!(spawns[0].spend_ceiling_microusd, Some(5_000_000));
        assert_eq!(session.lifecycle, CodeSessionLifecycle::Running);
    }

    /// A turn while the sandbox lives is an inbox message, not a spawn.
    #[tokio::test]
    async fn a_turn_on_a_live_sandbox_is_an_inbox_message() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, mut session, workspace, repo) = seed(dir.path()).await;
        super::super::fixtures::seeded_incarnation(&db, &session).await;
        let fake = FakeProvisioner::default();
        let settings = settings();
        let driver = driver!(&db, &bus, &fake, &settings);

        let outcome = driver
            .submit_turn(&mut session, &workspace, &repo, "and then this")
            .await
            .unwrap();
        let RemoteTurnOutcome::Delivered { turn } = outcome else {
            panic!("expected delivery");
        };
        assert_eq!(turn.ordinal, 1);
        assert!(fake.spawns.lock().unwrap().is_empty());
        let sends = fake.sends.lock().unwrap();
        assert_eq!(
            sends.as_slice(),
            &[("sb-1".to_owned(), "and then this".to_owned())]
        );
    }

    /// The session survives a sandbox stop: the pump closes the incarnation
    /// once terminal events land, and the next turn resumes from the WIP ref
    /// the predecessor pushed, one turn later.
    #[tokio::test]
    async fn the_next_turn_resumes_from_the_predecessors_wip_ref() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, mut session, workspace, repo) = seed(dir.path()).await;
        let fake = FakeProvisioner::default();
        let settings = settings();
        let driver = driver!(&db, &bus, &fake, &settings);

        // Turn 1 provisions.
        driver
            .submit_turn(&mut session, &workspace, &repo, "start")
            .await
            .unwrap();
        // The sandbox works the turn, pushes WIP, says goodbye, and the
        // environment retires it.
        fake.event_reads.lock().unwrap().push_back(read(
            SandboxState::Completed,
            4,
            vec![
                event(1, "turn_started", json!({ "turn": 1 })),
                event(2, "wip_pushed", json!({ "ref": "mg-wip/sb-next-i1" })),
                event(3, "turn_completed", json!({ "turn": 1, "exit_code": 0 })),
                event(4, "supervisor_stopped", json!({ "reason": "turn_mode" })),
            ],
        ));
        let report = driver.pump(&mut session, 0).await.unwrap();
        assert_eq!(report.ingested, 4);
        assert!(report.incarnation_stopped);
        assert!(report.fenced.is_none());
        let turn = latest_turn(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(turn.status, CodeTurnStatus::Completed);

        // Turn 2 reincarnates from the pushed ref, starting at turn 2.
        let outcome = driver
            .submit_turn(&mut session, &workspace, &repo, "continue")
            .await
            .unwrap();
        let RemoteTurnOutcome::Reincarnated { turn, incarnation } = outcome else {
            panic!("expected a reincarnation");
        };
        assert_eq!(turn.ordinal, 2);
        assert_eq!(incarnation.incarnation, 2);
        assert_eq!(incarnation.starting_turn, 2);
        let spawns = fake.spawns.lock().unwrap();
        assert_eq!(spawns.len(), 2);
        assert_eq!(
            spawns[1].repository_ref.as_deref(),
            Some("mg-wip/sb-next-i1")
        );
    }

    /// A stopped predecessor whose terminal events are not journaled yet
    /// holds the next turn instead of resuming without its last output.
    #[tokio::test]
    async fn a_turn_waits_for_the_predecessors_terminal_flush() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, mut session, workspace, repo) = seed(dir.path()).await;
        let incarnation = super::super::fixtures::seeded_incarnation(&db, &session).await;
        stop_incarnation(&db, &session.owner, incarnation, Some("expired"))
            .await
            .unwrap();
        let fake = FakeProvisioner::default();
        let settings = settings();
        let driver = driver!(&db, &bus, &fake, &settings);

        let outcome = driver
            .submit_turn(&mut session, &workspace, &repo, "resume")
            .await
            .unwrap();
        assert!(matches!(outcome, RemoteTurnOutcome::FlushPending));
        assert!(fake.spawns.lock().unwrap().is_empty());
    }

    /// A spawn refusal naming the WIP ref fences `ResumeLost`: the resume
    /// state is gone from the origin and retrying would refuse identically.
    #[tokio::test]
    async fn a_refusal_naming_the_wip_ref_fences_resume_lost() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, mut session, workspace, repo) = seed(dir.path()).await;
        let fake = FakeProvisioner::default();
        let settings = settings();
        let driver = driver!(&db, &bus, &fake, &settings);
        driver
            .submit_turn(&mut session, &workspace, &repo, "start")
            .await
            .unwrap();
        fake.event_reads.lock().unwrap().push_back(read(
            SandboxState::Completed,
            3,
            vec![
                event(1, "wip_pushed", json!({ "ref": "mg-wip/sb-next-i1" })),
                event(2, "turn_completed", json!({ "turn": 1, "exit_code": 0 })),
                event(3, "supervisor_stopped", json!({ "reason": "turn_mode" })),
            ],
        ));
        driver.pump(&mut session, 0).await.unwrap();

        fake.spawn_results
            .lock()
            .unwrap()
            .push_back(Err(RemoteSandboxError::Refused {
                operation: "spawn",
                code: "invalid_repository_ref".to_owned(),
                message: "the remote does not advertise mg-wip/sb-next-i1".to_owned(),
            }));
        let error = driver
            .submit_turn(&mut session, &workspace, &repo, "continue")
            .await;
        assert!(error.is_err());
        let live = get_session(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(live.lifecycle, CodeSessionLifecycle::Fenced);
        assert!(matches!(
            live.fence_reason,
            Some(FenceReason::ResumeLost { .. })
        ));
        // The failed reservation was released.
        let row = latest_incarnation(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, IncarnationState::Stopped);
    }

    /// Reap cancels the sandbox, closes the record, and resolves the fence
    /// without relaunching anything.
    #[tokio::test]
    async fn reap_cancels_and_recovers_without_a_relaunch() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, mut session, _workspace, _repo) = seed(dir.path()).await;
        super::super::fixtures::seeded_incarnation(&db, &session).await;
        recovery::fence_session(
            &db,
            &bus,
            &mut session,
            FenceReason::SandboxLost {
                detail: "the environment reports the sandbox failed".to_owned(),
            },
        )
        .await
        .unwrap();
        let fake = FakeProvisioner::default();
        let settings = settings();
        let driver = driver!(&db, &bus, &fake, &settings);

        let recovered = driver.reap(session.clone()).await.unwrap();
        assert_eq!(recovered.lifecycle, CodeSessionLifecycle::Idle);
        assert!(recovered.fence_reason.is_none());
        assert_eq!(
            fake.cancels.lock().unwrap().as_slice(),
            &["sb-1".to_owned()]
        );
        let row = latest_incarnation(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, IncarnationState::Stopped);
        assert_eq!(row.stop_reason.as_deref(), Some("reaped"));
    }

    /// At the owner's cap the refusal names the sessions holding the live
    /// incarnations, so the surface can say what to stop.
    #[tokio::test]
    async fn the_cap_refusal_names_the_running_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, session_a, workspace, repo) = seed(dir.path()).await;
        super::super::fixtures::seeded_incarnation(&db, &session_a).await;
        let mut session_b = super::super::fixtures::session_value();
        session_b.workspace_id = workspace.id;
        tidebreak_core::db::code::insert_session(&db, &session_b)
            .await
            .unwrap();
        let fake = FakeProvisioner::default();
        let settings = RemoteSpawnSettings {
            incarnation_cap: 1,
            ..settings()
        };
        let driver = driver!(&db, &bus, &fake, &settings);

        let outcome = driver
            .submit_turn(&mut session_b, &workspace, &repo, "queue-jump")
            .await
            .unwrap();
        let RemoteTurnOutcome::CapExhausted { running } = outcome else {
            panic!("expected the cap to refuse");
        };
        assert_eq!(running, vec![session_a.id]);
        assert!(fake.spawns.lock().unwrap().is_empty());
    }

    /// The sweep closes intents that never activated and fences their
    /// sessions so the person sees why nothing runs.
    #[tokio::test]
    async fn the_sweep_closes_stale_intents_and_fences_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, session, _workspace, _repo) = seed(dir.path()).await;
        let admission = create_incarnation_intent(&db, &session.owner, session.id, 1, 4)
            .await
            .unwrap();
        let IncarnationAdmission::Admitted(_intent) = admission else {
            panic!("expected admission");
        };

        // Young intents are left alone.
        let closed = sweep_stale_intents(&db, &bus, chrono::Utc::now())
            .await
            .unwrap();
        assert_eq!(closed, 0);

        let closed = sweep_stale_intents(&db, &bus, chrono::Utc::now() + STALE_INTENT_AGE * 2)
            .await
            .unwrap();
        assert_eq!(closed, 1);
        let row = latest_incarnation(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, IncarnationState::Stopped);
        assert_eq!(row.stop_reason.as_deref(), Some("intent_expired"));
        let live = get_session(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            live.attention.state,
            AttentionState::Fenced { .. }
        ));
        assert!(matches!(
            live.fence_reason,
            Some(FenceReason::IncarnationUnresolved { .. })
        ));
    }
}
