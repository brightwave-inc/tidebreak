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
    activate_incarnation, create_incarnation_intent, forget_session_wip_ref, get_workspace,
    insert_turn, latest_incarnation, latest_pushed_wip_ref, latest_turn,
    mark_incarnation_terminal_events_journaled, record_incarnation_spend, save_turn,
    session_spend_microusd, stale_incarnation_intents_all_owners, stop_incarnation,
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
    /// Cumulative per-session spend ceiling in micro-USD, when one is set.
    /// Per-spawn ceilings multiply by reincarnation; this one does not.
    pub session_spend_ceiling_microusd: Option<i64>,
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
        #[cfg_attr(not(test), allow(dead_code))]
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
    /// A turn is already running. The caller queues at the turn boundary,
    /// the way every other submit path does; the driver never interleaves
    /// two running turn rows on one session.
    TurnInFlight,
    /// The environment rejected the owner's credential. Nothing was sent or
    /// provisioned; sign in and retry.
    SignInRequired,
    /// The session's cumulative spend reached the owner's ceiling. Nothing
    /// was sent or provisioned.
    SpendExhausted {
        /// Micro-USD the session's incarnations have consumed.
        spent_microusd: i64,
        /// The configured ceiling in micro-USD.
        ceiling_microusd: i64,
    },
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

/// Surface the sign-in need on the session's attention.
async fn sign_in_needed(
    db: &Arc<DbStore>,
    bus: &CodeEventBus,
    session: &CodeSession,
) -> Result<(), tidebreak_core::AgentError> {
    let _ = super::super::attention::apply_attention(
        db,
        bus,
        &session.owner,
        session.id,
        // Structured, so the stall sweep's heuristic cannot replace the
        // prompt while the user is away signing in.
        Attention::needs_you(
            "sign in to the sandbox environment",
            AttentionSource::Structured,
        ),
        false,
    )
    .await?;
    Ok(())
}

/// Journal a machine-side refusal and ask for the owner's attention.
///
/// A refused turn produced no sandbox event, so nothing else would put the
/// reason in the transcript.
async fn refusal_notice(
    db: &Arc<DbStore>,
    bus: &CodeEventBus,
    session: &CodeSession,
    message: String,
    attention: &str,
) -> Result<(), tidebreak_core::AgentError> {
    let _ = super::super::session_worker::journal_event(
        db,
        bus,
        &session.owner,
        session.id,
        session.spawn_epoch,
        tidebreak_core::CodeEvent::HarnessNotice {
            level: tidebreak_core::HarnessNoticeLevel::Warning,
            message,
        },
    )
    .await;
    let _ = super::super::attention::apply_attention(
        db,
        bus,
        &session.owner,
        session.id,
        Attention::needs_you(attention, AttentionSource::Lifecycle),
        false,
    )
    .await?;
    Ok(())
}

/// Human-recognizable names for the sessions holding cap slots: their
/// workspace titles. The owner knows workspaces, not session ids; a title
/// that is empty falls back to the branch, and a row that vanished mid-read
/// falls back to the id rather than failing the refusal.
async fn occupying_names(
    db: &Arc<DbStore>,
    owner: &OwnerId,
    running: &[CodeSessionId],
) -> Vec<String> {
    let mut names = Vec::new();
    for id in running {
        let name = match tidebreak_core::db::code::get_session(db, owner, *id).await {
            Ok(Some(session)) => match get_workspace(db, owner, session.workspace_id).await {
                Ok(Some(workspace)) if !workspace.title.is_empty() => workspace.title,
                Ok(Some(workspace)) => workspace.branch_name,
                _ => id.to_string(),
            },
            _ => id.to_string(),
        };
        names.push(name);
    }
    names.sort();
    names.dedup();
    names
}

/// Micro-USD rendered as dollars for a human-readable reason.
fn dollars(microusd: i64) -> String {
    format!("${:.2}", microusd as f64 / 1_000_000.0)
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
    /// The environment rejected the credential; the row is held open and
    /// the next pump after a sign-in resumes the drain.
    pub sign_in_required: bool,
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
///
/// The agent numbers turns within its own incarnation starting at 1 — the
/// spawn carries no session ordinal — so an event's `turn` payload maps to
/// session ordinal `starting_turn + turn - 1`. Only the event mapping to
/// the running turn's own ordinal settles it: a batch that still holds an
/// earlier turn's ending must not close a turn that started after it.
async fn settle_turn_rows(
    db: &Arc<DbStore>,
    owner: &OwnerId,
    starting_turn: i32,
    running_turn: Option<CodeTurn>,
    events: &[super::wire::SandboxEvent],
) -> Result<bool, tidebreak_core::AgentError> {
    let Some(mut turn) = running_turn else {
        return Ok(false);
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
        let mapped_ordinal = event
            .payload
            .get("turn")
            .and_then(serde_json::Value::as_i64)
            .map(|agent_turn| i64::from(starting_turn) + agent_turn - 1);
        if mapped_ordinal != Some(turn.ordinal) {
            continue;
        }
        turn.status = status;
        turn.ended_at = Some(chrono::Utc::now());
        save_turn(db, owner, &turn).await?;
        return Ok(true);
    }
    Ok(false)
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
        self.submit_turn_from(session, workspace, repo, text, None)
            .await
    }

    /// Submit one turn, optionally as the promotion of a queued row.
    ///
    /// Delivery comes first — the sandbox send or spawn — and the claim
    /// second, because the driver cannot know delivery will succeed before
    /// trying, and outcomes like a held flush must leave the row queued.
    /// The claim itself is the local worker's atomic promotion; see
    /// [`start_turn_row`] for what a stale claim does.
    pub(crate) async fn submit_turn_from(
        &self,
        session: &mut CodeSession,
        workspace: &CodeWorkspace,
        repo: &CodeRepo,
        text: &str,
        promoted: Option<&tidebreak_core::CodeQueuedTurn>,
    ) -> Result<RemoteTurnOutcome, tidebreak_core::AgentError> {
        let (db, bus, provisioner, settings) = (self.db, self.bus, self.provisioner, self.settings);
        let owner = session.owner.clone();
        let last = latest_turn(db, &owner, session.id).await?;
        if last
            .as_ref()
            .is_some_and(|turn| turn.status == CodeTurnStatus::Running)
        {
            return Ok(RemoteTurnOutcome::TurnInFlight);
        }
        let ordinal = last.map_or(1, |turn| turn.ordinal + 1);

        // The ledger gates every turn, delivery and spawn alike: a mention is
        // a purchase, and per-spawn ceilings multiply by reincarnation.
        if let Some(ceiling) = settings.session_spend_ceiling_microusd {
            let spent = session_spend_microusd(db, &owner, session.id).await?;
            if spent >= ceiling {
                // An exhausted session cannot take another turn, so a live
                // sandbox would hold a cap slot doing nothing. Ask the
                // environment to stop it — best effort, and the pump drains
                // the goodbye and closes the row the ordinary way.
                let mut releasing = false;
                if let Some(row) = latest_incarnation(db, &owner, session.id).await? {
                    if row.state == IncarnationState::Active {
                        if let Some(sandbox_id) = row.sandbox_id.as_deref() {
                            releasing = true;
                            if let Err(error) = provisioner.cancel(&owner, sandbox_id).await {
                                warn!(
                                    session = %session.id,
                                    %error,
                                    "could not cancel an exhausted session's sandbox; its ceilings bound it"
                                );
                            }
                        }
                    }
                }
                let release = if releasing {
                    " Its sandbox is being stopped so the slot frees for other sessions."
                } else {
                    ""
                };
                refusal_notice(
                    db,
                    bus,
                    session,
                    format!(
                        "The turn was refused: this session has spent {} of its {} ceiling and takes no more turns.{release}",
                        dollars(spent),
                        dollars(ceiling)
                    ),
                    "the session reached its spend ceiling",
                )
                .await?;
                return Ok(RemoteTurnOutcome::SpendExhausted {
                    spent_microusd: spent,
                    ceiling_microusd: ceiling,
                });
            }
        }

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
                        let turn =
                            start_turn_row(db, bus, session, ordinal, text, promoted).await?;
                        return Ok(RemoteTurnOutcome::Delivered {
                            turn: Box::new(turn),
                        });
                    }
                    Err(RemoteSandboxError::SignInRequired(detail)) => {
                        // Token expiry between pumps is not a failed turn:
                        // hold the row beside the live lease and surface the
                        // sign-in, the same way the pump path does.
                        warn!(session = %session.id, %detail, "a send needs a sign-in");
                        sign_in_needed(db, bus, session).await?;
                        return Ok(RemoteTurnOutcome::SignInRequired);
                    }
                    Err(RemoteSandboxError::Refused { code, message, .. }) => {
                        // The environment no longer takes messages for this
                        // sandbox: it ended without this server having drained
                        // the news yet. The row stays active so the pump can
                        // drain the remaining events — including the goodbye
                        // that raises the reincarnation gate — and close it
                        // against the terminal state it reads.
                        warn!(
                            session = %session.id,
                            %code,
                            "a live sandbox refused a message; the pump will drain and close it ({message})"
                        );
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
            // The gate holds only for a predecessor that actually ran: an
            // intent that never activated, or a spawn that failed, has no
            // output a resume could miss.
            if predecessor.sandbox_id.is_some() && !predecessor.terminal_events_journaled {
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
                let names = occupying_names(db, &owner, &running).await.join(", ");
                refusal_notice(
                    db,
                    bus,
                    session,
                    format!(
                        "The turn was refused: all {} sandbox slots are in use by {}. Stop one of those sessions to continue.",
                        settings.incarnation_cap, names
                    ),
                    "the sandbox cap refused this turn",
                )
                .await?;
                return Ok(RemoteTurnOutcome::CapExhausted { running });
            }
            IncarnationAdmission::AlreadyLive { .. } => {
                // Another submit won the race between observing the stopped
                // predecessor and reserving. Same answer as observing its
                // intent directly.
                return Ok(RemoteTurnOutcome::ReincarnationInFlight);
            }
        };

        // Walk back to the last incarnation that actually pushed: the row
        // between it and now may be a reservation that never ran (a failed
        // spawn, a swept intent), and resuming from the base ref because of
        // it would drop the predecessor's checkpoint.
        let pushed = latest_pushed_wip_ref(db, &owner, session.id).await?;
        let resumed_from_wip = pushed.is_some();
        let resume_ref = pushed.unwrap_or_else(|| workspace.base_ref.clone());
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
                if let Err(error) =
                    activate_incarnation(db, &owner, intent.id, &lease.sandbox_id).await
                {
                    // The protocol closed the row under us (the sweep, say).
                    // The sandbox this call holds is orphaned: cancel it, or
                    // a later turn provisions a second one for this session.
                    if let Err(cancel_error) = provisioner.cancel(&owner, &lease.sandbox_id).await {
                        warn!(
                            session = %session.id,
                            sandbox = %lease.sandbox_id,
                            %cancel_error,
                            "an orphaned sandbox could not be cancelled; its ceilings bound it"
                        );
                    }
                    return Err(error);
                }
                let incarnation = latest_incarnation(db, &owner, session.id)
                    .await?
                    .ok_or_else(|| {
                        tidebreak_core::AgentError::Store(
                            "the activated incarnation vanished".to_owned(),
                        )
                    })?;
                let turn = start_turn_row(db, bus, session, ordinal, text, promoted).await?;
                Ok(RemoteTurnOutcome::Reincarnated {
                    turn: Box::new(turn),
                    incarnation: Box::new(incarnation),
                })
            }
            Err(RemoteSandboxError::SignInRequired(detail)) => {
                // No sandbox exists, so the reservation is safe to release;
                // the retry after a sign-in reserves again.
                stop_incarnation(db, &owner, intent.id, Some("sign_in_required")).await?;
                warn!(session = %session.id, %detail, "a spawn needs a sign-in");
                sign_in_needed(db, bus, session).await?;
                Ok(RemoteTurnOutcome::SignInRequired)
            }
            Err(error) => {
                // The reservation must not outlive the spawn it reserved for.
                stop_incarnation(db, &owner, intent.id, Some("spawn_failed")).await?;
                if resumed_from_wip && refusal_names(&error, &resume_ref) {
                    // The WIP ref the predecessor pushed is gone from the
                    // origin: the resume state no longer exists, and retrying
                    // would refuse identically. Forget the ref — the local
                    // resume-lost path drops its rejected ref for the same
                    // reason — so the turn after a reap resumes from an
                    // earlier checkpoint or the base instead of looping on
                    // this refusal. Then fence so a reap starts fresh.
                    forget_session_wip_ref(db, &owner, session.id, &resume_ref).await?;
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
        // Active rows are pumped for progress. A stopped row that still has
        // events to drain — its goodbye has not raised the gate — is pumped
        // too, so a stop can never strand the terminal flush undelivered.
        let drains = match row.state {
            IncarnationState::Active => true,
            IncarnationState::Stopped => !row.terminal_events_journaled,
            IncarnationState::Intent => false,
        };
        if !drains {
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
            Err(RemoteSandboxError::SignInRequired(detail)) => {
                // Token expiry is not a lost sandbox: the lease keeps
                // running and the next read after a sign-in drains it. Hold
                // the row — closing it here would release the cap slot
                // beside a live sandbox — and surface the sign-in.
                report.sign_in_required = true;
                sign_in_needed(db, bus, session).await?;
                warn!(session = %session.id, %detail, "the sandbox stream needs a sign-in");
                return Ok(report);
            }
            Err(error) => {
                // The environment refuses this stream and will never hand it
                // over. A drain cannot happen, so parking the session on one
                // would hold it at FlushPending forever. Cancel best effort —
                // a refusal does not prove the workload is gone — then close
                // the row and fence; reap waives the gate and the next turn
                // reincarnates.
                if let Err(cancel_error) = provisioner.cancel(&owner, &sandbox_id).await {
                    warn!(
                        session = %session.id,
                        %cancel_error,
                        "could not cancel a sandbox whose stream is refused; its ceilings bound it"
                    );
                }
                if row.state == IncarnationState::Active {
                    stop_incarnation(db, &owner, row.id, Some("events_refused")).await?;
                    report.incarnation_stopped = true;
                }
                let reason = FenceReason::SandboxLost {
                    detail: format!("the environment no longer serves this sandbox: {error}"),
                };
                report.fenced = Some(reason.clone());
                recovery::fence_session(db, bus, session, reason).await?;
                return Ok(report);
            }
        };

        // Feed the spend ledger from the environment's own meter. Best
        // effort: a status fault costs one reading, and the terminal pump
        // records the final figure.
        if let Ok(status) = provisioner.status(&owner, &sandbox_id).await {
            if let Some(spend) = status.spend_microusd {
                record_incarnation_spend(db, &owner, row.id, spend).await?;
            }
        }

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

        let turn_settled =
            settle_turn_rows(db, &owner, row.starting_turn, running_turn, &read.events).await?;

        if read.state.is_terminal() && row.state == IncarnationState::Active {
            stop_incarnation(
                db,
                &owner,
                row.id,
                Some(state_token(&read.events, read.state)),
            )
            .await?;
            report.incarnation_stopped = true;
        }
        if read.state.is_terminal() {
            // A turn the dying incarnation never settled — one delivered in
            // the stop window, after its last turn event — would otherwise
            // stay Running and refuse every later submit as TurnInFlight.
            // Run dead-worker recovery when the incarnation closes: it
            // interrupts the turn, journals it, sets needs-you, and goes
            // Idle in one fenced transaction. Re-query rather than reuse
            // the pre-ingest snapshot: the open turn may have been inserted
            // while this read was in flight.
            let open = latest_turn(db, &owner, session.id)
                .await?
                .filter(|turn| turn.status == CodeTurnStatus::Running);
            if open.is_some() {
                if let Some(recovered) = recovery::recover_dead_worker(db, bus, session).await? {
                    *session = recovered;
                }
            }
        }
        // A settled turn ends the Running lifecycle whether or not the
        // sandbox itself ended: an inbox-delivered turn completes while the
        // sandbox stays live, and leaving the session Running would let the
        // stall sweep overwrite the turn's done verdict with a stall.
        if (turn_settled || read.state.is_terminal())
            && session.lifecycle == CodeSessionLifecycle::Running
        {
            // The ingest just wrote this turn's verdict (DoneUnreviewed,
            // needs-you) onto the stored row. Refresh this snapshot
            // before persisting, or the stale Working it still carries
            // from turn start would overwrite the verdict.
            if let Some(stored) =
                tidebreak_core::db::code::get_session(db, &owner, session.id).await?
            {
                *session = stored;
            }
            session.lifecycle = CodeSessionLifecycle::Idle;
            let _ = persist_session(db, bus, session).await?;
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
            if !row.terminal_events_journaled {
                // Reap is the person accepting whatever the sandbox never
                // delivered — the fence said so. Waive the gate, or the next
                // turn after a successful reap waits forever instead of
                // reincarnating on demand.
                let _ = mark_incarnation_terminal_events_journaled(db, &owner, row.id).await;
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
    promoted: Option<&tidebreak_core::CodeQueuedTurn>,
) -> Result<CodeTurn, tidebreak_core::AgentError> {
    let mut turn = CodeTurn {
        id: promoted.map_or_else(CodeTurnId::new, |row| row.id),
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
    match promoted {
        // Claim the queue row and insert its turn in one transaction, the
        // way a local worker promotes. A stale claim — the row was edited,
        // reordered, or retracted after the sweep's snapshot — writes the
        // turn under a fresh id instead: the snapshot's text already reached
        // the sandbox, so the transcript must show it, and the surviving
        // queue row keeps its own id so its later promotion cannot collide
        // with this turn.
        Some(row) => {
            if !tidebreak_core::db::code::promote_queued_turn(db, &session.owner, row, &turn)
                .await?
            {
                warn!(
                    session = %session.id,
                    "a queued message changed under its promotion; recording the delivered turn separately"
                );
                turn.id = CodeTurnId::new();
                insert_turn(db, &session.owner, &turn).await?;
            }
        }
        None => insert_turn(db, &session.owner, &turn).await?,
    }
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

    type SpawnHook = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

    #[derive(Default)]
    struct FakeProvisioner {
        /// Awaited inside the next spawn, after the intent row exists —
        /// the window an activation race lives in.
        on_spawn: Mutex<Option<SpawnHook>>,
        spawns: Mutex<Vec<SpawnArguments>>,
        spawn_results: Mutex<VecDeque<Result<SandboxLease, RemoteSandboxError>>>,
        sends: Mutex<Vec<(String, String)>>,
        send_results: Mutex<VecDeque<Result<MessageReceipt, RemoteSandboxError>>>,
        event_reads: Mutex<VecDeque<SandboxEvents>>,
        cancels: Mutex<Vec<String>>,
        /// Reported as `spend_microusd` by every status read.
        spend: Mutex<Option<i64>>,
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
            let hook = self.on_spawn.lock().unwrap().take();
            if let Some(hook) = hook {
                hook.await;
            }
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
                spend_microusd: *self.spend.lock().unwrap(),
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
            session_spend_ceiling_microusd: None,
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
        {
            let sends = fake.sends.lock().unwrap();
            assert_eq!(
                sends.as_slice(),
                &[("sb-1".to_owned(), "and then this".to_owned())]
            );
        }

        // The turn settles while the sandbox stays live: the session goes
        // Idle with the turn's verdict, so the stall sweep cannot mistake a
        // finished turn for a stall.
        fake.event_reads.lock().unwrap().push_back(read(
            SandboxState::Running,
            2,
            vec![
                event(1, "turn_started", json!({ "turn": 1 })),
                event(2, "turn_completed", json!({ "turn": 1, "exit_code": 0 })),
            ],
        ));
        driver.pump(&mut session, 0).await.unwrap();
        let live = get_session(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(live.lifecycle, CodeSessionLifecycle::Idle);
        assert_eq!(
            live.attention.state,
            tidebreak_core::AttentionState::DoneUnreviewed
        );
        // The sandbox itself is still live for the next turn.
        let row = latest_incarnation(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, IncarnationState::Active);
    }

    /// A turn delivered in the stop window — after the incarnation's last
    /// turn event, before its goodbye — is interrupted when the incarnation
    /// closes. Leaving it Running would refuse every later submit as
    /// TurnInFlight with no fence to reap.
    #[tokio::test]
    async fn closing_the_incarnation_interrupts_an_unsettled_turn() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, mut session, workspace, repo) = seed(dir.path()).await;
        super::super::fixtures::seeded_incarnation(&db, &session).await;
        let fake = FakeProvisioner::default();
        let settings = settings();
        let driver = driver!(&db, &bus, &fake, &settings);

        // The send succeeds against the still-Active row, but the sandbox
        // stops before running the turn: its goodbye carries no turn events.
        let outcome = driver
            .submit_turn(&mut session, &workspace, &repo, "too late")
            .await
            .unwrap();
        assert!(matches!(outcome, RemoteTurnOutcome::Delivered { .. }));
        fake.event_reads.lock().unwrap().push_back(read(
            SandboxState::Completed,
            1,
            vec![event(
                1,
                "supervisor_stopped",
                json!({ "reason": "turn_mode" }),
            )],
        ));
        driver.pump(&mut session, 0).await.unwrap();

        let turn = latest_turn(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(turn.status, CodeTurnStatus::Interrupted);
        let live = get_session(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(live.lifecycle, CodeSessionLifecycle::Idle);
        // The interrupt surfaces: the session must not keep showing work in
        // progress for a message the dead incarnation never ran.
        assert!(matches!(
            live.attention.state,
            tidebreak_core::AttentionState::NeedsYou { .. }
        ));

        // The session can continue: the next turn reincarnates instead of
        // refusing as TurnInFlight.
        let outcome = driver
            .submit_turn(&mut session, &workspace, &repo, "again")
            .await
            .unwrap();
        assert!(matches!(outcome, RemoteTurnOutcome::Reincarnated { .. }));
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
        // The persist after the sandbox stop must not write the snapshot's
        // stale Working back over the verdict the ingest just journaled.
        let live = tidebreak_core::db::code::get_session(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            live.attention.state,
            tidebreak_core::AttentionState::DoneUnreviewed
        );
        assert_eq!(live.lifecycle, CodeSessionLifecycle::Idle);

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
        {
            let spawns = fake.spawns.lock().unwrap();
            assert_eq!(spawns.len(), 2);
            assert_eq!(
                spawns[1].repository_ref.as_deref(),
                Some("mg-wip/sb-next-i1")
            );
        }

        // The successor numbers its own turns from 1: its first completion
        // settles session turn 2 through the incarnation's starting turn.
        fake.event_reads.lock().unwrap().push_back(read(
            SandboxState::Running,
            2,
            vec![
                event(1, "turn_started", json!({ "turn": 1 })),
                event(2, "turn_completed", json!({ "turn": 1, "exit_code": 0 })),
            ],
        ));
        driver.pump(&mut session, 0).await.unwrap();
        let settled = latest_turn(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(settled.ordinal, 2);
        assert_eq!(settled.status, CodeTurnStatus::Completed);
        // The settlement ends the Running lifecycle even though the
        // successor sandbox stays live.
        let after = get_session(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.lifecycle, CodeSessionLifecycle::Idle);
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

        // The gone checkpoint is forgotten: after a reap, the next turn
        // resumes from the base ref instead of looping on the same refusal.
        let mut recovered = driver.reap(live).await.unwrap();
        let outcome = driver
            .submit_turn(&mut recovered, &workspace, &repo, "continue")
            .await
            .unwrap();
        assert!(matches!(outcome, RemoteTurnOutcome::Reincarnated { .. }));
        let spawns = fake.spawns.lock().unwrap();
        assert_eq!(
            spawns.last().unwrap().repository_ref.as_deref(),
            Some("main")
        );
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
        // The reap waived the terminal-flush gate the sandbox never raised,
        // so the next turn reincarnates on demand instead of waiting forever.
        assert!(row.terminal_events_journaled);
        let mut recovered = recovered;
        let outcome = driver
            .submit_turn(&mut recovered, &_workspace, &_repo, "again")
            .await
            .unwrap();
        assert!(matches!(outcome, RemoteTurnOutcome::Reincarnated { .. }));
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
        // The refusal is a session event with a human-readable reason
        // naming what runs, and it asks for the owner's attention.
        let events =
            tidebreak_core::db::code::list_events(&db, &session_b.owner, session_b.id, 0, 50)
                .await
                .unwrap()
                .events;
        let notice = events
            .iter()
            .find_map(|row| match &row.event {
                tidebreak_core::CodeEvent::HarnessNotice { message, .. } => Some(message.clone()),
                _ => None,
            })
            .expect("expected a refusal notice");
        assert!(notice.contains("sandbox slots"), "{notice}");
        // The occupier is named by its workspace, which the owner
        // recognizes, not by a session id, which they do not.
        assert!(notice.contains("remote"), "{notice}");
        assert!(!notice.contains(&session_a.id.to_string()), "{notice}");
        assert!(notice.contains("Stop one of those sessions"), "{notice}");
        let live = get_session(&db, &session_b.owner, session_b.id)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            live.attention.state,
            AttentionState::NeedsYou { .. }
        ));
    }

    /// The spend ledger gates the turn before anything is sent or spawned,
    /// with a reason in dollars.
    #[tokio::test]
    async fn the_spend_ceiling_refuses_the_turn_before_any_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, mut session, workspace, repo) = seed(dir.path()).await;
        let incarnation = super::super::fixtures::seeded_incarnation(&db, &session).await;
        record_incarnation_spend(&db, &session.owner, incarnation, 2_500_000)
            .await
            .unwrap();
        let fake = FakeProvisioner::default();
        let settings = RemoteSpawnSettings {
            session_spend_ceiling_microusd: Some(2_000_000),
            ..settings()
        };
        let driver = driver!(&db, &bus, &fake, &settings);

        let outcome = driver
            .submit_turn(&mut session, &workspace, &repo, "one more")
            .await
            .unwrap();
        let RemoteTurnOutcome::SpendExhausted {
            spent_microusd,
            ceiling_microusd,
        } = outcome
        else {
            panic!("expected the ceiling to refuse");
        };
        assert_eq!(spent_microusd, 2_500_000);
        assert_eq!(ceiling_microusd, 2_000_000);
        assert!(fake.spawns.lock().unwrap().is_empty());
        assert!(fake.sends.lock().unwrap().is_empty());
        let events = tidebreak_core::db::code::list_events(&db, &session.owner, session.id, 0, 50)
            .await
            .unwrap()
            .events;
        let notice = events
            .iter()
            .find_map(|row| match &row.event {
                tidebreak_core::CodeEvent::HarnessNotice { message, .. } => Some(message.clone()),
                _ => None,
            })
            .expect("expected a refusal notice");
        assert!(notice.contains("$2.50"), "{notice}");
        assert!(notice.contains("$2.00"), "{notice}");
        // The refusal frees the cap slot instead of telling the owner to do
        // something the product does not offer: the idle sandbox is asked to
        // stop, and the copy says so.
        assert!(!notice.contains("Raise the ceiling"), "{notice}");
        assert!(notice.contains("stopped"), "{notice}");
        assert_eq!(
            fake.cancels.lock().unwrap().as_slice(),
            &["sb-1".to_owned()]
        );
    }

    /// The pump feeds the ledger from the environment's meter, and the
    /// session total accumulates across incarnations.
    #[tokio::test]
    async fn the_pump_feeds_the_ledger_across_incarnations() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, mut session, workspace, repo) = seed(dir.path()).await;
        let fake = FakeProvisioner::default();
        let settings = settings();
        let driver = driver!(&db, &bus, &fake, &settings);

        driver
            .submit_turn(&mut session, &workspace, &repo, "start")
            .await
            .unwrap();
        *fake.spend.lock().unwrap() = Some(1_500_000);
        fake.event_reads.lock().unwrap().push_back(read(
            SandboxState::Completed,
            2,
            vec![
                event(1, "turn_completed", json!({ "turn": 1, "exit_code": 0 })),
                event(2, "supervisor_stopped", json!({ "reason": "turn_mode" })),
            ],
        ));
        driver.pump(&mut session, 0).await.unwrap();

        let outcome = driver
            .submit_turn(&mut session, &workspace, &repo, "continue")
            .await
            .unwrap();
        assert!(matches!(outcome, RemoteTurnOutcome::Reincarnated { .. }));
        *fake.spend.lock().unwrap() = Some(700_000);
        fake.event_reads.lock().unwrap().push_back(read(
            SandboxState::Running,
            1,
            vec![event(1, "turn_started", json!({ "turn": 1 }))],
        ));
        driver.pump(&mut session, 0).await.unwrap();

        assert_eq!(
            session_spend_microusd(&db, &session.owner, session.id)
                .await
                .unwrap(),
            2_200_000
        );
    }

    /// A live sandbox that refuses a message stays open for the pump: the
    /// drain delivers the goodbye, closes the row, and the next turn then
    /// reincarnates instead of waiting forever.
    #[tokio::test]
    async fn a_refused_message_leaves_the_row_for_the_pump_to_drain() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, mut session, workspace, repo) = seed(dir.path()).await;
        super::super::fixtures::seeded_incarnation(&db, &session).await;
        let fake = FakeProvisioner::default();
        let settings = settings();
        let driver = driver!(&db, &bus, &fake, &settings);

        fake.send_results
            .lock()
            .unwrap()
            .push_back(Err(RemoteSandboxError::Refused {
                operation: "send",
                code: "sandbox_not_running".to_owned(),
                message: "the sandbox has ended".to_owned(),
            }));
        let outcome = driver
            .submit_turn(&mut session, &workspace, &repo, "late")
            .await
            .unwrap();
        assert!(matches!(outcome, RemoteTurnOutcome::FlushPending));
        let row = latest_incarnation(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, IncarnationState::Active);

        // The pump drains the terminal events and closes the row.
        fake.event_reads.lock().unwrap().push_back(read(
            SandboxState::Expired,
            2,
            vec![
                event(1, "turn_completed", json!({ "turn": 1, "exit_code": 0 })),
                event(2, "supervisor_stopped", json!({ "reason": "expired" })),
            ],
        ));
        let report = driver.pump(&mut session, 0).await.unwrap();
        assert!(report.incarnation_stopped);
        let outcome = driver
            .submit_turn(&mut session, &workspace, &repo, "late")
            .await
            .unwrap();
        assert!(matches!(outcome, RemoteTurnOutcome::Reincarnated { .. }));
    }

    /// A spawn that fails releases a reservation with nothing to drain: the
    /// next turn reincarnates instead of waiting on a gate no sandbox can
    /// ever raise.
    #[tokio::test]
    async fn a_failed_spawn_does_not_gate_the_next_turn() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, mut session, workspace, repo) = seed(dir.path()).await;
        let fake = FakeProvisioner::default();
        let settings = settings();
        let driver = driver!(&db, &bus, &fake, &settings);

        fake.spawn_results
            .lock()
            .unwrap()
            .push_back(Err(RemoteSandboxError::Refused {
                operation: "spawn",
                code: "profile_not_found".to_owned(),
                message: "no such profile".to_owned(),
            }));
        assert!(driver
            .submit_turn(&mut session, &workspace, &repo, "start")
            .await
            .is_err());
        let outcome = driver
            .submit_turn(&mut session, &workspace, &repo, "retry")
            .await
            .unwrap();
        assert!(matches!(outcome, RemoteTurnOutcome::Reincarnated { .. }));
    }

    /// One running turn at a time: a second submit is refused for the
    /// caller's queue, never interleaved as a second running row.
    #[tokio::test]
    async fn a_second_submit_while_a_turn_runs_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, mut session, workspace, repo) = seed(dir.path()).await;
        let fake = FakeProvisioner::default();
        let settings = settings();
        let driver = driver!(&db, &bus, &fake, &settings);

        driver
            .submit_turn(&mut session, &workspace, &repo, "first")
            .await
            .unwrap();
        let outcome = driver
            .submit_turn(&mut session, &workspace, &repo, "second")
            .await
            .unwrap();
        assert!(matches!(outcome, RemoteTurnOutcome::TurnInFlight));
        assert!(fake.sends.lock().unwrap().is_empty());
        assert_eq!(fake.spawns.lock().unwrap().len(), 1);
    }

    /// A batch still carrying an earlier turn's ending must not settle a
    /// turn that started after it.
    #[tokio::test]
    async fn an_earlier_turns_ending_does_not_settle_the_running_turn() {
        let dir = tempfile::tempdir().unwrap();
        let (db, _bus, session, _workspace, _repo) = seed(dir.path()).await;
        let running = CodeTurn {
            id: CodeTurnId::new(),
            session_id: session.id,
            ordinal: 2,
            status: CodeTurnStatus::Running,
            model: None,
            fast_mode: false,
            user_input: "second".to_owned(),
            user_input_blob_id: None,
            attachments: Vec::new(),
            checkpoint_ref: None,
            diffstat: None,
            usage: None,
            narrative: None,
            started_at: chrono::Utc::now(),
            ended_at: None,
        };
        insert_turn(&db, &session.owner, &running).await.unwrap();

        let stale = [event(
            9,
            "turn_completed",
            json!({ "turn": 1, "exit_code": 0 }),
        )];
        settle_turn_rows(&db, &session.owner, 1, Some(running.clone()), &stale)
            .await
            .unwrap();
        let row = latest_turn(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, CodeTurnStatus::Running);

        let own = [event(
            10,
            "turn_completed",
            json!({ "turn": 2, "exit_code": 0 }),
        )];
        settle_turn_rows(&db, &session.owner, 1, Some(running), &own)
            .await
            .unwrap();
        let row = latest_turn(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, CodeTurnStatus::Completed);
    }

    /// A lease whose activation loses to the protocol is cancelled, not
    /// leaked beside a released cap slot.
    #[tokio::test]
    async fn an_activation_race_cancels_the_orphaned_lease() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, mut session, workspace, repo) = seed(dir.path()).await;
        let fake = FakeProvisioner::default();
        let settings = settings();
        let driver = driver!(&db, &bus, &fake, &settings);

        // While the spawn is in flight, the sweep closes the intent.
        let hook_db = db.clone();
        let hook_owner = session.owner.clone();
        let hook_session = session.id;
        *fake.on_spawn.lock().unwrap() = Some(Box::pin(async move {
            let row = latest_incarnation(&hook_db, &hook_owner, hook_session)
                .await
                .unwrap()
                .unwrap();
            stop_incarnation(&hook_db, &hook_owner, row.id, Some("intent_expired"))
                .await
                .unwrap();
        }));

        assert!(driver
            .submit_turn(&mut session, &workspace, &repo, "start")
            .await
            .is_err());
        assert_eq!(
            fake.cancels.lock().unwrap().as_slice(),
            &["sb-next".to_owned()]
        );
    }

    /// A reservation that never ran must not cost the predecessor's
    /// checkpoint: the retry after a failed spawn still resumes from the
    /// last incarnation that actually pushed.
    #[tokio::test]
    async fn a_failed_spawn_between_incarnations_keeps_the_wip_resume_ref() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, mut session, workspace, repo) = seed(dir.path()).await;
        let fake = FakeProvisioner::default();
        let settings = settings();
        let driver = driver!(&db, &bus, &fake, &settings);

        // Incarnation 1 runs, pushes WIP, and the environment retires it.
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

        // The next reservation fails to spawn — a stopped row with no ref
        // now sits newer than the one that pushed.
        fake.spawn_results
            .lock()
            .unwrap()
            .push_back(Err(RemoteSandboxError::Unavailable {
                operation: "spawn",
                detail: "gateway restarting".to_owned(),
            }));
        assert!(driver
            .submit_turn(&mut session, &workspace, &repo, "continue")
            .await
            .is_err());

        // The retry still resumes from the pushed checkpoint, not the base.
        let outcome = driver
            .submit_turn(&mut session, &workspace, &repo, "continue")
            .await
            .unwrap();
        assert!(matches!(outcome, RemoteTurnOutcome::Reincarnated { .. }));
        let spawns = fake.spawns.lock().unwrap();
        assert_eq!(
            spawns.last().unwrap().repository_ref.as_deref(),
            Some("mg-wip/sb-next-i1")
        );
    }

    /// An event stream the environment will never serve again cannot park
    /// the session on a drain that cannot happen: the pump closes the row
    /// and fences, and a reap then unblocks reincarnation.
    #[tokio::test]
    async fn a_dead_event_stream_fences_instead_of_parking_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, mut session, workspace, repo) = seed(dir.path()).await;
        super::super::fixtures::seeded_incarnation(&db, &session).await;
        let fake = FakeProvisioner::default();
        let settings = settings();
        let driver = driver!(&db, &bus, &fake, &settings);

        // The scripted queue is empty, but make the failure explicit and
        // non-retryable rather than relying on the fake's default.
        fake.event_reads.lock().unwrap().clear();
        // FakeProvisioner returns Unavailable when unscripted; that is the
        // retryable case, so assert it holds nothing first.
        let report = driver.pump(&mut session, 0).await.unwrap();
        assert!(report.fenced.is_none());

        // Now the environment refuses the stream outright.
        struct RefusingReads<'a>(&'a FakeProvisioner);
        #[async_trait]
        impl SandboxProvisioner for RefusingReads<'_> {
            async fn spawn(
                &self,
                owner: &OwnerId,
                arguments: &SpawnArguments,
            ) -> Result<SandboxLease, RemoteSandboxError> {
                self.0.spawn(owner, arguments).await
            }
            async fn status(
                &self,
                owner: &OwnerId,
                sandbox_id: &str,
            ) -> Result<SandboxStatus, RemoteSandboxError> {
                self.0.status(owner, sandbox_id).await
            }
            async fn events(
                &self,
                _owner: &OwnerId,
                _sandbox_id: &str,
                _cursor: EventCursor,
            ) -> Result<SandboxEvents, RemoteSandboxError> {
                Err(RemoteSandboxError::Refused {
                    operation: "events",
                    code: "sandbox_not_found".to_owned(),
                    message: "no such sandbox".to_owned(),
                })
            }
            async fn send(
                &self,
                owner: &OwnerId,
                sandbox_id: &str,
                message: &SandboxMessage,
            ) -> Result<MessageReceipt, RemoteSandboxError> {
                self.0.send(owner, sandbox_id, message).await
            }
            async fn cancel(
                &self,
                owner: &OwnerId,
                sandbox_id: &str,
            ) -> Result<(), RemoteSandboxError> {
                self.0.cancel(owner, sandbox_id).await
            }
        }
        let refusing = RefusingReads(&fake);
        let driver = driver!(&db, &bus, &refusing, &settings);
        let report = driver.pump(&mut session, 0).await.unwrap();
        assert!(report.incarnation_stopped);
        assert!(matches!(
            report.fenced,
            Some(FenceReason::SandboxLost { .. })
        ));
        // The workload may still exist behind the refusal: it was cancelled
        // before the cap slot was released.
        assert_eq!(
            fake.cancels.lock().unwrap().as_slice(),
            &["sb-1".to_owned()]
        );
        let row = latest_incarnation(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, IncarnationState::Stopped);

        // Reap waives the never-raised gate; the next turn reincarnates.
        let reloaded = tidebreak_core::db::code::get_session(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        let mut recovered = driver.reap(reloaded).await.unwrap();
        let outcome = driver
            .submit_turn(&mut recovered, &workspace, &repo, "again")
            .await
            .unwrap();
        assert!(matches!(outcome, RemoteTurnOutcome::Reincarnated { .. }));
    }

    /// An expired credential is not a lost sandbox: the row is held open
    /// beside the live lease, nothing is cancelled or fenced, and the
    /// sign-in need is surfaced.
    #[tokio::test]
    async fn a_rejected_credential_holds_the_row_and_asks_for_sign_in() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, mut session, _workspace, _repo) = seed(dir.path()).await;
        super::super::fixtures::seeded_incarnation(&db, &session).await;
        let fake = FakeProvisioner::default();
        let settings = settings();

        struct ExpiredToken<'a>(&'a FakeProvisioner);
        #[async_trait]
        impl SandboxProvisioner for ExpiredToken<'_> {
            async fn spawn(
                &self,
                owner: &OwnerId,
                arguments: &SpawnArguments,
            ) -> Result<SandboxLease, RemoteSandboxError> {
                self.0.spawn(owner, arguments).await
            }
            async fn status(
                &self,
                owner: &OwnerId,
                sandbox_id: &str,
            ) -> Result<SandboxStatus, RemoteSandboxError> {
                self.0.status(owner, sandbox_id).await
            }
            async fn events(
                &self,
                _owner: &OwnerId,
                _sandbox_id: &str,
                _cursor: EventCursor,
            ) -> Result<SandboxEvents, RemoteSandboxError> {
                Err(RemoteSandboxError::SignInRequired(
                    "token expired".to_owned(),
                ))
            }
            async fn send(
                &self,
                owner: &OwnerId,
                sandbox_id: &str,
                message: &SandboxMessage,
            ) -> Result<MessageReceipt, RemoteSandboxError> {
                self.0.send(owner, sandbox_id, message).await
            }
            async fn cancel(
                &self,
                owner: &OwnerId,
                sandbox_id: &str,
            ) -> Result<(), RemoteSandboxError> {
                self.0.cancel(owner, sandbox_id).await
            }
        }
        let expired = ExpiredToken(&fake);
        let driver = driver!(&db, &bus, &expired, &settings);

        let report = driver.pump(&mut session, 0).await.unwrap();
        assert!(report.sign_in_required);
        assert!(report.fenced.is_none());
        assert!(!report.incarnation_stopped);
        assert!(fake.cancels.lock().unwrap().is_empty());
        let row = latest_incarnation(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, IncarnationState::Active);
        let live = tidebreak_core::db::code::get_session(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            live.attention.state,
            tidebreak_core::AttentionState::NeedsYou { .. }
        ));
    }

    /// A send that needs a sign-in fails no turn: the row stays open, no
    /// turn row is inserted, and the sign-in need is surfaced.
    #[tokio::test]
    async fn a_send_that_needs_sign_in_holds_the_turn_and_surfaces_it() {
        let dir = tempfile::tempdir().unwrap();
        let (db, bus, mut session, workspace, repo) = seed(dir.path()).await;
        super::super::fixtures::seeded_incarnation(&db, &session).await;
        let fake = FakeProvisioner::default();
        let settings = settings();
        let driver = driver!(&db, &bus, &fake, &settings);

        fake.send_results
            .lock()
            .unwrap()
            .push_back(Err(RemoteSandboxError::SignInRequired(
                "token expired".to_owned(),
            )));
        let outcome = driver
            .submit_turn(&mut session, &workspace, &repo, "held")
            .await
            .unwrap();
        assert!(matches!(outcome, RemoteTurnOutcome::SignInRequired));
        assert!(latest_turn(&db, &session.owner, session.id)
            .await
            .unwrap()
            .is_none());
        let row = latest_incarnation(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, IncarnationState::Active);
        let live = tidebreak_core::db::code::get_session(&db, &session.owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            live.attention.state,
            tidebreak_core::AttentionState::NeedsYou { .. }
        ));
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
