//! One worker task per running code-mode session.
//!
//! The worker owns the engine session and is the only journal writer for that
//! session. Events are persisted (epoch-fenced) before they are published on
//! the live bus.
//!
//! `run_turn` does not own the command loop. While a turn is in flight the
//! worker keeps selecting on [`WorkerCommand`] so `decide` and `interrupt`
//! reach the engine mid-turn — every adapter rides this, not just Claude.
//! Applying a control command runs *alongside* the turn, never in front of
//! it: an interrupt's grace period is exactly when the child's stdout most
//! needs draining.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};
use tokio::sync::{mpsc, oneshot, watch, Notify};
use tracing::warn;

use tidebreak_core::db::code::{
    append_event, bump_spawn_epoch, get_open_turn, get_session, insert_approval, insert_turn,
    next_turn_ordinal, save_session, save_turn, CodeJournalError,
};
use tidebreak_core::{
    Attention, AttentionSource, BoundedError, CodeApproval, CodeApprovalId, CodeApprovalKind,
    CodeApprovalState, CodeEvent, CodeSession, CodeSessionId, CodeSessionLifecycle, CodeTurn,
    CodeTurnId, CodeTurnStatus, DbStore, FenceReason, HarnessNoticeLevel,
};
use tidebreak_harness::{
    ApprovalDecision, HarnessApprovalRef, HarnessError, HarnessEvent, HarnessEventSink,
    HarnessSession, TurnInput, TurnOutcome,
};

use super::bus::CodeEventBus;

pub(crate) enum WorkerCommand {
    RunTurn {
        message: String,
        model: Option<String>,
        attachments: Vec<tidebreak_core::CodeTurnAttachment>,
        reply: oneshot::Sender<Result<CodeTurn, WorkerError>>,
    },
    Decide {
        approval: HarnessApprovalRef,
        decision: ApprovalDecision,
        reply: oneshot::Sender<Result<(), WorkerError>>,
    },
    Interrupt {
        reply: oneshot::Sender<Result<(), WorkerError>>,
    },
    Shutdown,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkerError {
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Failed(String),
}

pub(crate) struct WorkerHandle {
    pub spawn_epoch: i64,
    pub commands: mpsc::Sender<WorkerCommand>,
    /// Single follow-up parked while a turn is running (queue-default).
    pub pending: Arc<std::sync::Mutex<Option<QueuedFollowUp>>>,
    pub wake: Arc<Notify>,
    pub sink: Arc<LiveSink>,
}

pub(crate) struct QueuedFollowUp {
    pub message: String,
    pub model: Option<String>,
    pub attachments: Vec<tidebreak_core::CodeTurnAttachment>,
}

pub(crate) struct LiveSink {
    db: Arc<DbStore>,
    bus: Arc<CodeEventBus>,
    session_id: CodeSessionId,
    spawn_epoch: i64,
    turn_id: std::sync::Mutex<Option<CodeTurnId>>,
    /// Engine-lifetime unrecognized count already folded onto the session row.
    ///
    /// The engine's own count is cumulative for as long as it is attached, and
    /// the sink is created once per attachment, so the two share a lifetime.
    flushed_unrecognized: AtomicU64,
}

impl LiveSink {
    pub(crate) fn set_turn(&self, turn_id: CodeTurnId) {
        *self.turn_id.lock().expect("code sink turn") = Some(turn_id);
    }

    /// How many unrecognized events the engine has counted since the last
    /// flush. Reporting a total below the watermark (an engine that reset its
    /// own counter) yields zero rather than a negative correction.
    fn take_unrecognized_delta(&self, total: u64) -> u64 {
        let flushed = self.flushed_unrecognized.swap(total, Ordering::SeqCst);
        total.saturating_sub(flushed)
    }

    async fn record_approval(
        &self,
        harness_ref: &tidebreak_harness::HarnessApprovalRef,
        raw: &serde_json::Value,
    ) {
        let existing = *self.turn_id.lock().expect("code sink turn");
        let turn_id = match existing {
            Some(id) => id,
            None => match get_open_turn(&self.db, self.session_id).await {
                Ok(Some(turn)) => turn.id,
                _ => return,
            },
        };
        let approval = CodeApproval {
            id: CodeApprovalId::new(),
            session_id: self.session_id,
            turn_id,
            kind: kind_from_raw(raw),
            harness_raw: persist_harness_raw(&harness_ref.call_id, raw),
            state: CodeApprovalState::Pending,
            feedback: None,
            requested_at: Utc::now(),
            decided_at: None,
        };
        if insert_approval(&self.db, &approval).await.is_err() {
            return;
        }
        let _ = super::attention::apply_attention(
            &self.db,
            &self.bus,
            self.session_id,
            Attention::needs_you("an approval is waiting", AttentionSource::Structured),
            false,
        )
        .await;
        let _ = persist_and_publish(
            &self.db,
            &self.bus,
            self.session_id,
            self.spawn_epoch,
            CodeEvent::ApprovalRequested {
                approval_id: approval.id,
            },
        )
        .await;
    }
}

#[async_trait]
impl HarnessEventSink for LiveSink {
    async fn emit(&self, event: HarnessEvent) {
        if let HarnessEvent::ApprovalRequested { harness_ref, raw } = &event {
            self.record_approval(harness_ref, raw).await;
            return;
        }
        if matches!(event, HarnessEvent::ApprovalResolved { .. }) {
            // The decision route journals ApprovalResolved after the harness
            // observes the decision. Ignore a duplicate emit from the engine.
            return;
        }
        if matches!(event, HarnessEvent::TurnStarted) && self.turn_id.lock().unwrap().is_some() {
            // The worker already journaled TurnStarted for this turn.
            return;
        }
        let turn_id = *self.turn_id.lock().unwrap();
        let Some(code_event) = map_event(event, turn_id) else {
            return;
        };
        match persist_and_publish(
            &self.db,
            &self.bus,
            self.session_id,
            self.spawn_epoch,
            code_event,
        )
        .await
        {
            Ok(()) => {}
            Err(CodeJournalError::StaleSpawnEpoch { .. }) => {
                warn!(
                    session = %self.session_id,
                    "dropping event from a superseded code-session worker"
                );
            }
            Err(err) => {
                warn!(session = %self.session_id, error = %err, "failed to journal engine event");
            }
        }
    }
}

pub(crate) fn spawn_session_worker(
    db: Arc<DbStore>,
    bus: Arc<CodeEventBus>,
    session: CodeSession,
    engine: Box<dyn HarnessSession>,
    sink: Arc<LiveSink>,
) -> WorkerHandle {
    let (tx, rx) = mpsc::channel(8);
    let spawn_epoch = session.spawn_epoch;
    let pending = Arc::new(std::sync::Mutex::new(None));
    let wake = Arc::new(Notify::new());
    tokio::spawn(run_worker(
        db,
        bus,
        session,
        engine,
        sink.clone(),
        pending.clone(),
        wake.clone(),
        rx,
    ));
    WorkerHandle {
        spawn_epoch,
        commands: tx,
        pending,
        wake,
        sink,
    }
}

/// Park one follow-up. Returns `false` when the single slot is already taken.
pub(crate) fn queue_follow_up(
    handle: &WorkerHandle,
    message: String,
    model: Option<String>,
    attachments: Vec<tidebreak_core::CodeTurnAttachment>,
) -> bool {
    let mut pending = handle.pending.lock().expect("code turn queue");
    if pending.is_some() {
        return false;
    }
    *pending = Some(QueuedFollowUp {
        message,
        model,
        attachments,
    });
    handle.wake.notify_one();
    true
}

#[allow(clippy::too_many_arguments)]
async fn run_worker(
    db: Arc<DbStore>,
    bus: Arc<CodeEventBus>,
    mut session: CodeSession,
    engine: Box<dyn HarnessSession>,
    sink: Arc<LiveSink>,
    pending: Arc<std::sync::Mutex<Option<QueuedFollowUp>>>,
    wake: Arc<Notify>,
    mut commands: mpsc::Receiver<WorkerCommand>,
) {
    if let Some(pid) = engine.child_pid() {
        session.child_pid = Some(pid);
        let _ = save_session(&db, &session).await;
    }

    loop {
        if session_was_ended(&db, &mut session).await {
            break;
        }
        drain_queued(
            &db,
            &bus,
            &mut session,
            engine.as_ref(),
            &sink,
            &pending,
            &mut commands,
        )
        .await;
        tokio::select! {
            _ = wake.notified() => {}
            command = commands.recv() => match command {
                Some(WorkerCommand::RunTurn {
                    message,
                    model,
                    attachments,
                    reply,
                }) => {
                    let result = drive_turn(
                        &db,
                        &bus,
                        &mut session,
                        engine.as_ref(),
                        &sink,
                        &mut commands,
                        QueuedFollowUp {
                            message,
                            model,
                            attachments,
                        },
                    )
                    .await;
                    let _ = reply.send(result);
                }
                Some(command) => {
                    if apply_control(engine.as_ref(), command).await == ControlFlow::Shutdown {
                        break;
                    }
                }
                None => break,
            },
        }
    }
    let _ = engine.shutdown().await;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlFlow {
    Continue,
    Shutdown,
}

/// Stop the engine's current turn, discarding the adapter's error: the turn
/// is ending either way, and the outcome is what the worker journals.
async fn interrupt_engine(engine: &dyn HarnessSession) -> ControlFlow {
    let _ = engine.interrupt().await;
    ControlFlow::Continue
}

/// The next pid the adapter publishes, or a future that never resolves when
/// the adapter has no per-turn child to report.
async fn next_child_pid(changes: Option<&mut watch::Receiver<Option<i64>>>) -> Option<i64> {
    // A dropped sender (or no sender at all) has nothing more to say.
    let Some(changes) = changes else {
        return std::future::pending().await;
    };
    if changes.changed().await.is_err() {
        return std::future::pending().await;
    }
    *changes.borrow()
}

async fn apply_control(engine: &dyn HarnessSession, command: WorkerCommand) -> ControlFlow {
    match command {
        WorkerCommand::Decide {
            approval,
            decision,
            reply,
        } => {
            let result = engine
                .decide(approval, decision)
                .await
                .map_err(|err| WorkerError::Failed(err.to_string()));
            let _ = reply.send(result);
            ControlFlow::Continue
        }
        WorkerCommand::Interrupt { reply } => {
            let result = engine
                .interrupt()
                .await
                .map_err(|err| WorkerError::Failed(err.to_string()));
            let _ = reply.send(result);
            ControlFlow::Continue
        }
        WorkerCommand::RunTurn { reply, .. } => {
            let _ = reply.send(Err(WorkerError::Conflict(
                "a turn is already running on this session".into(),
            )));
            ControlFlow::Continue
        }
        WorkerCommand::Shutdown => ControlFlow::Shutdown,
    }
}

async fn drain_queued(
    db: &Arc<DbStore>,
    bus: &Arc<CodeEventBus>,
    session: &mut CodeSession,
    engine: &dyn HarnessSession,
    sink: &LiveSink,
    pending: &Arc<std::sync::Mutex<Option<QueuedFollowUp>>>,
    commands: &mut mpsc::Receiver<WorkerCommand>,
) {
    loop {
        let next = pending.lock().expect("code turn queue").take();
        let Some(follow_up) = next else {
            break;
        };
        let _ = drive_turn(db, bus, session, engine, sink, commands, follow_up).await;
    }
}

async fn drive_turn(
    db: &Arc<DbStore>,
    bus: &Arc<CodeEventBus>,
    session: &mut CodeSession,
    engine: &dyn HarnessSession,
    sink: &LiveSink,
    commands: &mut mpsc::Receiver<WorkerCommand>,
    QueuedFollowUp {
        message,
        model,
        attachments,
    }: QueuedFollowUp,
) -> Result<CodeTurn, WorkerError> {
    if session.lifecycle == CodeSessionLifecycle::Running {
        return Err(WorkerError::Conflict(
            "a turn is already running on this session".into(),
        ));
    }
    if session.lifecycle == CodeSessionLifecycle::Fenced {
        return Err(WorkerError::Conflict(
            "session is fenced until it is reaped".into(),
        ));
    }
    if session.lifecycle == CodeSessionLifecycle::Ended {
        return Err(WorkerError::Conflict("session has ended".into()));
    }
    if let Some(model) = model.clone() {
        session.model = Some(model);
    }

    let ordinal = next_turn_ordinal(db, session.id)
        .await
        .map_err(|err| WorkerError::Failed(err.to_string()))?;
    let mut turn = CodeTurn {
        id: CodeTurnId::new(),
        session_id: session.id,
        ordinal,
        status: CodeTurnStatus::Running,
        user_input: message.clone(),
        user_input_blob_id: None,
        attachments,
        checkpoint_ref: None,
        diffstat: None,
        usage: None,
        narrative: None,
        started_at: Utc::now(),
        ended_at: None,
    };
    insert_turn(db, &turn)
        .await
        .map_err(|err| WorkerError::Failed(err.to_string()))?;
    sink.set_turn(turn.id);

    session.lifecycle = CodeSessionLifecycle::Running;
    super::attention::replace_attention(
        session,
        Attention::working(AttentionSource::Lifecycle),
        false,
    );
    if let Some(pid) = engine.child_pid() {
        session.child_pid = Some(pid);
    }
    super::attention::persist_session(db, bus, session)
        .await
        .map_err(|err| WorkerError::Failed(err.to_string()))?;

    persist_and_publish(
        db,
        bus,
        session.id,
        session.spawn_epoch,
        CodeEvent::TurnStarted { turn_id: turn.id },
    )
    .await
    .map_err(|err| WorkerError::Failed(err.to_string()))?;

    let run = engine.run_turn(TurnInput {
        text: message,
        model: model.or_else(|| session.model.clone()),
    });
    tokio::pin!(run);
    // Adapters that spawn one child per turn have no pid to report until the
    // turn is under way. Record every transition as it happens: the session
    // row's pid is what boot recovery probes, and a NULL pid there is read as
    // "the engine is gone" — which would re-attach a worker to a worktree a
    // live child is still writing to.
    let mut pid_changes = engine.child_pid_changes();
    // Control commands run concurrently with the turn. Awaiting one inline
    // would stop draining the child's stdout — during an interrupt's grace
    // period that is what turns a clean abort into a kill.
    let mut controls: FuturesUnordered<BoxFuture<'_, ControlFlow>> = FuturesUnordered::new();
    let mut interrupted = false;
    let mut commands_closed = false;
    let run = loop {
        tokio::select! {
            result = &mut run => break result,
            Some(flow) = controls.next(), if !controls.is_empty() => {
                if flow == ControlFlow::Shutdown {
                    interrupted = true;
                    controls.push(Box::pin(interrupt_engine(engine)));
                }
            }
            pid = next_child_pid(pid_changes.as_mut()) => {
                if session.child_pid != pid {
                    session.child_pid = pid;
                    let _ = save_session(db, session).await;
                }
            }
            command = commands.recv(), if !commands_closed => match command {
                Some(command) => {
                    interrupted |= matches!(command, WorkerCommand::Interrupt { .. });
                    controls.push(Box::pin(apply_control(engine, command)));
                }
                None => {
                    commands_closed = true;
                    interrupted = true;
                    controls.push(Box::pin(interrupt_engine(engine)));
                }
            }
        }
    };
    // A control command still in flight has a caller waiting on its reply.
    // Dropping it here would answer them with a dead channel.
    while controls.next().await.is_some() {}

    if let Some(pid) = engine.child_pid() {
        session.child_pid = Some(pid);
    } else {
        session.child_pid = None;
    }
    if let Some(resume) = engine.resume_ref() {
        session.harness_resume_ref = Some(resume);
    }
    // End of turn is where the parser's unrecognized count becomes durable.
    // The engine reports a running total, so only the delta since the last
    // flush is added — the row accumulates across engine restarts.
    let dropped = sink.take_unrecognized_delta(engine.unrecognized_events());
    if dropped > 0 {
        session.unrecognized_event_count = session
            .unrecognized_event_count
            .saturating_add(i64::try_from(dropped).unwrap_or(i64::MAX));
    }

    // Re-read the turn: the sink may have already closed it.
    if let Ok(Some(updated)) = get_open_turn(db, session.id).await {
        turn = updated;
    } else if let Ok(Some(current)) = tidebreak_core::db::code::get_turn(db, turn.id).await {
        turn = current;
    }

    match run {
        Ok(outcome) => {
            let detail = match outcome {
                TurnOutcome::Clean => None,
                TurnOutcome::Incomplete { detail } => Some(detail),
            };
            // An engine that died on us says why on stderr. That belongs in
            // the journal, not only in a log line nobody reading the session
            // will see. A stop the user asked for is not news.
            if let (Some(detail), false) = (detail.as_ref(), interrupted) {
                let _ = persist_and_publish(
                    db,
                    bus,
                    session.id,
                    session.spawn_epoch,
                    CodeEvent::HarnessNotice {
                        level: HarnessNoticeLevel::Error,
                        message: detail.clone(),
                    },
                )
                .await;
            }
            if turn.status == CodeTurnStatus::Running {
                // The stream ended without closing the turn. Only the worker
                // knows whether that was asked for.
                let (status, event) = if interrupted {
                    (CodeTurnStatus::Interrupted, CodeEvent::TurnInterrupted)
                } else if let Some(detail) = detail {
                    (
                        CodeTurnStatus::Failed,
                        CodeEvent::TurnFailed {
                            error: BoundedError { message: detail },
                        },
                    )
                } else {
                    (
                        CodeTurnStatus::Completed,
                        CodeEvent::TurnCompleted {
                            usage: turn.usage.clone().unwrap_or_default(),
                            checkpoint: None,
                        },
                    )
                };
                turn.status = status;
                turn.ended_at = Some(Utc::now());
                let _ = save_turn(db, &turn).await;
                let _ = persist_and_publish(db, bus, session.id, session.spawn_epoch, event).await;
            }
        }
        Err(err) => {
            if turn.status == CodeTurnStatus::Running {
                turn.status = CodeTurnStatus::Failed;
                turn.ended_at = Some(Utc::now());
                let _ = save_turn(db, &turn).await;
                let _ = persist_and_publish(
                    db,
                    bus,
                    session.id,
                    session.spawn_epoch,
                    CodeEvent::TurnFailed {
                        error: BoundedError {
                            message: err.to_string(),
                        },
                    },
                )
                .await;
            }
            if !session_was_ended(db, session).await {
                if let HarnessError::ResumeLost(detail) = &err {
                    // The engine has lost this session: every later turn would
                    // fail the same way. Fence it so the user is offered a
                    // reap instead of a session that is idle and broken.
                    let _ = super::recovery::fence_session(
                        db,
                        bus,
                        session,
                        FenceReason::ResumeLost {
                            detail: detail.clone(),
                        },
                    )
                    .await;
                    return Err(WorkerError::Failed(err.to_string()));
                }
                session.lifecycle = CodeSessionLifecycle::Idle;
                super::attention::replace_attention(
                    session,
                    Attention::needs_you("the engine turn failed", AttentionSource::Lifecycle),
                    false,
                );
                let _ = super::attention::persist_session(db, bus, session).await;
            }
            return Err(WorkerError::Failed(err.to_string()));
        }
    }

    if let Ok(Some(current)) = tidebreak_core::db::code::get_turn(db, turn.id).await {
        turn = current;
    }
    super::checkpoint::after_turn_completed(db, bus, session, &mut turn).await;
    if session_was_ended(db, session).await {
        return Ok(turn);
    }
    if turn.status == CodeTurnStatus::Interrupted {
        super::attention::replace_attention(
            session,
            Attention::needs_you("the turn was interrupted", AttentionSource::Lifecycle),
            false,
        );
    } else if turn.status == CodeTurnStatus::Failed {
        super::attention::replace_attention(
            session,
            Attention::needs_you("the engine turn failed", AttentionSource::Lifecycle),
            false,
        );
    } else {
        super::attention::replace_attention(
            session,
            Attention::new(
                tidebreak_core::AttentionState::DoneUnreviewed,
                AttentionSource::Lifecycle,
            ),
            false,
        );
    }
    session.lifecycle = CodeSessionLifecycle::Idle;
    let _ = super::attention::persist_session(db, bus, session).await;
    Ok(turn)
}

async fn session_was_ended(db: &DbStore, session: &mut CodeSession) -> bool {
    match get_session(db, session.id).await {
        Ok(Some(current)) if current.lifecycle == CodeSessionLifecycle::Ended => {
            *session = current;
            true
        }
        _ => false,
    }
}

/// Bump the spawn epoch, record pid/version, and journal SessionStarted.
///
/// Call this once, before the engine is launched, so the event sink and the
/// session row share the same epoch. Pass `child_pid` after launch via
/// [`save_session`] if the adapter only exposes a pid later.
pub(crate) async fn attach_engine(
    db: &DbStore,
    bus: &CodeEventBus,
    session_id: CodeSessionId,
    kind: tidebreak_core::HarnessKind,
    version: Option<String>,
    child_pid: Option<i64>,
) -> Result<CodeSession, WorkerError> {
    let epoch = bump_spawn_epoch(db, session_id, child_pid)
        .await
        .map_err(|err| WorkerError::Failed(err.to_string()))?;
    let mut session = get_session(db, session_id)
        .await
        .map_err(|err| WorkerError::Failed(err.to_string()))?
        .ok_or_else(|| WorkerError::Failed(format!("session {session_id} not found")))?;
    session.spawn_epoch = epoch;
    session.child_pid = child_pid;
    session.harness_version = version.or(session.harness_version);
    session.lifecycle = CodeSessionLifecycle::Idle;
    super::attention::replace_attention(
        &mut session,
        Attention::working(AttentionSource::Lifecycle),
        false,
    );
    super::attention::persist_session(db, bus, &session)
        .await
        .map_err(|err| WorkerError::Failed(err.to_string()))?;
    persist_and_publish(
        db,
        bus,
        session_id,
        epoch,
        CodeEvent::SessionStarted {
            harness_kind: kind,
            harness_version: session
                .harness_version
                .clone()
                .unwrap_or_else(|| "unknown".into()),
            resume_ref: session.harness_resume_ref.clone(),
        },
    )
    .await
    .map_err(|err| WorkerError::Failed(err.to_string()))?;
    Ok(session)
}

pub(crate) fn sink_for(
    db: Arc<DbStore>,
    bus: Arc<CodeEventBus>,
    session_id: CodeSessionId,
    spawn_epoch: i64,
    turn_id: Option<CodeTurnId>,
) -> Arc<LiveSink> {
    Arc::new(LiveSink {
        db,
        bus,
        session_id,
        spawn_epoch,
        turn_id: std::sync::Mutex::new(turn_id),
        flushed_unrecognized: AtomicU64::new(0),
    })
}

pub(crate) async fn journal_event(
    db: &DbStore,
    bus: &CodeEventBus,
    session_id: CodeSessionId,
    spawn_epoch: i64,
    event: CodeEvent,
) -> Result<(), CodeJournalError> {
    persist_and_publish(db, bus, session_id, spawn_epoch, event).await
}

async fn persist_and_publish(
    db: &DbStore,
    bus: &CodeEventBus,
    session_id: CodeSessionId,
    spawn_epoch: i64,
    event: CodeEvent,
) -> Result<(), CodeJournalError> {
    apply_side_effects(db, session_id, spawn_epoch, &event).await?;
    let seq = append_event(db, session_id, spawn_epoch, &event).await?;
    if is_activity(&event) {
        let _ = super::attention::note_activity(db, bus, session_id).await;
    }
    bus.publish(
        session_id,
        tidebreak_core::SequencedCodeEvent { seq, event },
    );
    Ok(())
}

fn is_activity(event: &CodeEvent) -> bool {
    matches!(
        event,
        CodeEvent::AssistantDelta { .. }
            | CodeEvent::AssistantMessage { .. }
            | CodeEvent::ReasoningDelta { .. }
            | CodeEvent::ToolStarted { .. }
            | CodeEvent::ToolCompleted { .. }
            | CodeEvent::FileChanged { .. }
            | CodeEvent::ApprovalRequested { .. }
            | CodeEvent::UserSteered { .. }
    )
}

async fn apply_side_effects(
    db: &DbStore,
    session_id: CodeSessionId,
    _spawn_epoch: i64,
    event: &CodeEvent,
) -> Result<(), CodeJournalError> {
    match event {
        CodeEvent::TurnCompleted { usage, checkpoint } => {
            if let Ok(Some(mut turn)) = get_open_turn(db, session_id).await {
                turn.status = CodeTurnStatus::Completed;
                turn.ended_at = Some(Utc::now());
                turn.usage = Some(usage.clone());
                if let Some(hint) = checkpoint {
                    turn.checkpoint_ref = hint.checkpoint_ref.clone();
                    turn.diffstat = hint.diffstat.clone();
                }
                let _ = save_turn(db, &turn).await;
            }
        }
        CodeEvent::TurnFailed { .. } => {
            if let Ok(Some(mut turn)) = get_open_turn(db, session_id).await {
                turn.status = CodeTurnStatus::Failed;
                turn.ended_at = Some(Utc::now());
                let _ = save_turn(db, &turn).await;
            }
        }
        CodeEvent::TurnInterrupted => {
            if let Ok(Some(mut turn)) = get_open_turn(db, session_id).await {
                turn.status = CodeTurnStatus::Interrupted;
                turn.ended_at = Some(Utc::now());
                let _ = save_turn(db, &turn).await;
            }
        }
        _ => {}
    }
    Ok(())
}

const MAX_HARNESS_RAW_BYTES: usize = 16 * 1024;

/// Size-capped preview plus `call_id` as a sibling. Decide reads `call_id`,
/// never the preview: a Write/Edit payload larger than the cap would otherwise
/// truncate `tool_use_id` away and leave the row undecidable.
fn persist_harness_raw(call_id: &str, raw: &serde_json::Value) -> serde_json::Value {
    let mut stored = match cap_raw(raw) {
        serde_json::Value::Object(map) => map,
        other => {
            let mut map = serde_json::Map::new();
            map.insert("payload".to_owned(), other);
            map
        }
    };
    stored.insert(
        "call_id".to_owned(),
        serde_json::Value::String(call_id.to_owned()),
    );
    serde_json::Value::Object(stored)
}

fn cap_raw(raw: &serde_json::Value) -> serde_json::Value {
    let rendered = raw.to_string();
    if rendered.len() <= MAX_HARNESS_RAW_BYTES {
        return raw.clone();
    }
    let end = rendered.floor_char_boundary(MAX_HARNESS_RAW_BYTES);
    serde_json::json!({ "truncated": true, "preview": &rendered[..end] })
}

fn kind_from_raw(raw: &serde_json::Value) -> CodeApprovalKind {
    let tool = raw
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let input = raw.get("input").cloned().unwrap_or(serde_json::Value::Null);
    match tool {
        "Write" | "Edit" | "NotebookEdit" => {
            let path = input
                .get("file_path")
                .or_else(|| input.get("path"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned();
            CodeApprovalKind::FileWrite { paths: vec![path] }
        }
        "Bash" => CodeApprovalKind::Command {
            cmd: input
                .get("command")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned(),
            cwd: input
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        },
        "WebFetch" | "WebSearch" => CodeApprovalKind::Network {
            summary: tool.to_owned(),
        },
        _ => CodeApprovalKind::Other {
            summary: tool.to_owned(),
        },
    }
}

fn map_event(event: HarnessEvent, turn_id: Option<CodeTurnId>) -> Option<CodeEvent> {
    Some(match event {
        HarnessEvent::SessionStarted {
            harness_kind,
            harness_version,
            resume_ref,
        } => CodeEvent::SessionStarted {
            harness_kind,
            harness_version,
            resume_ref,
        },
        HarnessEvent::TurnStarted => CodeEvent::TurnStarted { turn_id: turn_id? },
        HarnessEvent::AssistantDelta { text } => CodeEvent::AssistantDelta { text },
        HarnessEvent::AssistantMessage { text } => CodeEvent::AssistantMessage { text },
        HarnessEvent::ReasoningDelta { text } => CodeEvent::ReasoningDelta { text },
        HarnessEvent::ToolStarted {
            call_id,
            name,
            detail,
        } => CodeEvent::ToolStarted {
            call_id,
            name,
            detail,
        },
        HarnessEvent::ToolCompleted {
            call_id,
            outcome,
            preview,
        } => CodeEvent::ToolCompleted {
            call_id,
            outcome,
            preview,
        },
        HarnessEvent::FileChanged {
            path,
            kind,
            diffstat,
        } => CodeEvent::FileChanged {
            path,
            kind,
            diffstat,
        },
        HarnessEvent::ApprovalRequested { .. } => {
            return None;
        }
        HarnessEvent::ApprovalResolved { decision, .. } => CodeEvent::ApprovalResolved {
            approval_id: CodeApprovalId::new(),
            decision: match decision {
                tidebreak_harness::ApprovalDecision::Approve => {
                    tidebreak_core::ApprovalDecisionKind::Approve
                }
                tidebreak_harness::ApprovalDecision::Deny { feedback } => {
                    tidebreak_core::ApprovalDecisionKind::Deny { feedback }
                }
            },
        },
        HarnessEvent::UserSteered { text } => CodeEvent::UserSteered { text },
        HarnessEvent::TurnCompleted { usage } => CodeEvent::TurnCompleted {
            usage,
            checkpoint: None,
        },
        HarnessEvent::TurnFailed { error } => CodeEvent::TurnFailed { error },
        HarnessEvent::TurnInterrupted => CodeEvent::TurnInterrupted,
        HarnessEvent::HarnessNotice { level, message } => {
            CodeEvent::HarnessNotice { level, message }
        }
    })
}

impl From<HarnessError> for WorkerError {
    fn from(err: HarnessError) -> Self {
        Self::Failed(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_raw_truncates_on_a_char_boundary() {
        // `{"xx":"` is 7 bytes; a string of `é` (2 bytes each) then places a
        // mid-character byte at MAX_HARNESS_RAW_BYTES. Slicing there panics.
        let raw = serde_json::json!({ "xx": "é".repeat(MAX_HARNESS_RAW_BYTES) });
        assert!(raw.to_string().len() > MAX_HARNESS_RAW_BYTES);
        assert!(!raw.to_string().is_char_boundary(MAX_HARNESS_RAW_BYTES));
        let capped = cap_raw(&raw);
        assert_eq!(capped["truncated"], true);
        let preview = capped["preview"].as_str().expect("preview is a string");
        assert!(preview.len() <= MAX_HARNESS_RAW_BYTES);
        assert!(preview.is_char_boundary(preview.len()));
    }

    #[test]
    fn persist_harness_raw_keeps_call_id_when_the_payload_is_capped() {
        let raw = serde_json::json!({
            "tool_name": "Write",
            "input": {
                "file_path": "/workspace/big.txt",
                "content": "x".repeat(MAX_HARNESS_RAW_BYTES + 64),
            },
            "tool_use_id": "toolu_oversized",
        });
        let stored = persist_harness_raw("toolu_oversized", &raw);
        assert_eq!(stored["truncated"], true);
        assert_eq!(stored["call_id"], "toolu_oversized");
        assert!(stored.get("tool_use_id").is_none());
    }
}
