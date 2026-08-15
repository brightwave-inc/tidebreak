//! One worker task per running code-mode session.
//!
//! The worker owns the engine session and is the only journal writer for that
//! session. Events are persisted (epoch-fenced) before they are published on
//! the live bus.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::{mpsc, oneshot, Notify};
use tracing::warn;

use tidebreak_core::db::code::{
    append_event, bump_spawn_epoch, get_open_turn, get_session, insert_turn, next_turn_ordinal,
    save_session, save_turn, CodeJournalError,
};
use tidebreak_core::{
    Attention, AttentionSource, BoundedError, CodeApprovalId, CodeEvent, CodeSession,
    CodeSessionId, CodeSessionLifecycle, CodeTurn, CodeTurnId, CodeTurnStatus, DbStore,
};
use tidebreak_harness::{HarnessError, HarnessEvent, HarnessEventSink, HarnessSession, TurnInput};

use super::bus::CodeEventBus;

pub(crate) enum WorkerCommand {
    RunTurn {
        message: String,
        reply: oneshot::Sender<Result<CodeTurn, WorkerError>>,
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
    pub pending: Arc<std::sync::Mutex<Option<String>>>,
    pub wake: Arc<Notify>,
}

struct LiveSink {
    db: Arc<DbStore>,
    bus: Arc<CodeEventBus>,
    session_id: CodeSessionId,
    spawn_epoch: i64,
    turn_id: std::sync::Mutex<Option<CodeTurnId>>,
}

#[async_trait]
impl HarnessEventSink for LiveSink {
    async fn emit(&self, event: HarnessEvent) {
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
        pending.clone(),
        wake.clone(),
        rx,
    ));
    WorkerHandle {
        spawn_epoch,
        commands: tx,
        pending,
        wake,
    }
}

/// Park one follow-up. Returns `false` when the single slot is already taken.
pub(crate) fn queue_follow_up(handle: &WorkerHandle, message: String) -> bool {
    let mut pending = handle.pending.lock().expect("code turn queue");
    if pending.is_some() {
        return false;
    }
    *pending = Some(message);
    handle.wake.notify_one();
    true
}

async fn run_worker(
    db: Arc<DbStore>,
    bus: Arc<CodeEventBus>,
    mut session: CodeSession,
    mut engine: Box<dyn HarnessSession>,
    pending: Arc<std::sync::Mutex<Option<String>>>,
    wake: Arc<Notify>,
    mut commands: mpsc::Receiver<WorkerCommand>,
) {
    if let Some(pid) = engine.child_pid() {
        session.child_pid = Some(pid);
        let _ = save_session(&db, &session).await;
    }

    loop {
        drain_queued(&db, &bus, &mut session, engine.as_mut(), &pending).await;
        tokio::select! {
            _ = wake.notified() => {}
            command = commands.recv() => match command {
                Some(WorkerCommand::RunTurn { message, reply }) => {
                    let result =
                        drive_turn(&db, &bus, &mut session, engine.as_mut(), message).await;
                    let _ = reply.send(result);
                }
                Some(WorkerCommand::Interrupt { reply }) => {
                    let result = engine
                        .interrupt()
                        .await
                        .map_err(|err| WorkerError::Failed(err.to_string()));
                    let _ = reply.send(result);
                }
                Some(WorkerCommand::Shutdown) | None => break,
            },
        }
    }
    let _ = engine.shutdown().await;
}

async fn drain_queued(
    db: &Arc<DbStore>,
    bus: &Arc<CodeEventBus>,
    session: &mut CodeSession,
    engine: &mut dyn HarnessSession,
    pending: &Arc<std::sync::Mutex<Option<String>>>,
) {
    loop {
        let next = pending.lock().expect("code turn queue").take();
        let Some(message) = next else {
            break;
        };
        let _ = drive_turn(db, bus, session, engine, message).await;
    }
}

async fn drive_turn(
    db: &Arc<DbStore>,
    bus: &Arc<CodeEventBus>,
    session: &mut CodeSession,
    engine: &mut dyn HarnessSession,
    message: String,
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

    session.lifecycle = CodeSessionLifecycle::Running;
    session.attention = Attention::working(AttentionSource::Lifecycle);
    if let Some(pid) = engine.child_pid() {
        session.child_pid = Some(pid);
    }
    save_session(db, session)
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

    let run = engine.run_turn(TurnInput { text: message }).await;

    if let Some(pid) = engine.child_pid() {
        session.child_pid = Some(pid);
    } else {
        session.child_pid = None;
    }
    if let Some(resume) = engine.resume_ref() {
        session.harness_resume_ref = Some(resume);
    }

    // Re-read the turn: the sink may have already closed it.
    if let Ok(Some(updated)) = get_open_turn(db, session.id).await {
        turn = updated;
    } else if let Ok(Some(current)) = tidebreak_core::db::code::get_turn(db, turn.id).await {
        turn = current;
    }

    match run {
        Ok(()) => {
            if turn.status == CodeTurnStatus::Running {
                turn.status = CodeTurnStatus::Completed;
                turn.ended_at = Some(Utc::now());
                let _ = save_turn(db, &turn).await;
                let _ = persist_and_publish(
                    db,
                    bus,
                    session.id,
                    session.spawn_epoch,
                    CodeEvent::TurnCompleted {
                        usage: turn.usage.clone().unwrap_or_default(),
                        checkpoint: None,
                    },
                )
                .await;
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
            session.lifecycle = CodeSessionLifecycle::Idle;
            session.attention =
                Attention::needs_you("the engine turn failed", AttentionSource::Lifecycle);
            let _ = save_session(db, session).await;
            return Err(WorkerError::Failed(err.to_string()));
        }
    }

    if let Ok(Some(current)) = tidebreak_core::db::code::get_turn(db, turn.id).await {
        turn = current;
    }
    if turn.status == CodeTurnStatus::Interrupted {
        session.attention =
            Attention::needs_you("the turn was interrupted", AttentionSource::Lifecycle);
    } else if turn.status == CodeTurnStatus::Failed {
        session.attention =
            Attention::needs_you("the engine turn failed", AttentionSource::Lifecycle);
    } else {
        session.attention = Attention::new(
            tidebreak_core::AttentionState::DoneUnreviewed,
            AttentionSource::Lifecycle,
        );
    }
    session.lifecycle = CodeSessionLifecycle::Idle;
    let _ = save_session(db, session).await;
    Ok(turn)
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
    session.attention = Attention::working(AttentionSource::Lifecycle);
    save_session(db, &session)
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
) -> Arc<dyn HarnessEventSink> {
    Arc::new(LiveSink {
        db,
        bus,
        session_id,
        spawn_epoch,
        turn_id: std::sync::Mutex::new(turn_id),
    })
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
    bus.publish(
        session_id,
        tidebreak_core::SequencedCodeEvent { seq, event },
    );
    Ok(())
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
        HarnessEvent::ApprovalRequested { .. } => CodeEvent::ApprovalRequested {
            approval_id: CodeApprovalId::new(),
        },
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
