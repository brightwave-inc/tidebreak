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
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};
use tokio::sync::{mpsc, oneshot, watch, Notify};
use tracing::{warn, Instrument as _};

use tidebreak_core::db::code::{
    append_event, bump_spawn_epoch, get_open_turn, get_session, get_session_all_owners,
    insert_approval, insert_turn, next_turn_ordinal, save_session, save_turn,
    set_session_subagents, CodeJournalError,
};
use tidebreak_core::{
    bound_subagents, Attention, AttentionSource, BlobStore, BoundedError, CodeApproval,
    CodeApprovalId, CodeApprovalKind, CodeApprovalState, CodeEvent, CodeSession, CodeSessionId,
    CodeSessionLifecycle, CodeSubagentStatus, CodeSubagentSummary, CodeTurn, CodeTurnId,
    CodeTurnStatus, DbStore, FenceReason, HarnessNoticeLevel, OwnerId, ToolOutcome,
};
use tidebreak_harness::{
    ApprovalDecision, HarnessApprovalRef, HarnessError, HarnessEvent, HarnessEventSink,
    HarnessSession, TurnImage, TurnInput, TurnOutcome,
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
    Steer {
        expected_turn_id: CodeTurnId,
        message: String,
        reply: oneshot::Sender<Result<(), WorkerError>>,
    },
    Shutdown,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkerError {
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    NoActiveTurn(String),
    #[error("{0}")]
    StaleTurn(String),
    #[error("{0}")]
    SteeringUnavailable(String),
    #[error("{0}")]
    SteeringRejected(String),
    #[error("{0}")]
    Failed(String),
    /// A sibling session holds the workspace's turn lock.
    ///
    /// Never reaches a caller. `submit_turn` parks the message and answers
    /// `Queued`, so a send is never left holding its connection open for the
    /// length of someone else's turn.
    #[error("another session in this workspace is mid-turn")]
    WorktreeBusy,
}

pub(crate) struct WorkerHandle {
    pub spawn_epoch: i64,
    pub commands: mpsc::Sender<WorkerCommand>,
    pub queue: TurnQueue,
    pub sink: Arc<LiveSink>,
}

/// How a worker gets its next turn, and when it may start one.
///
/// `pending` and `wake` are this session's own single follow-up slot
/// (record 9). `worktree` is shared with every other session in the
/// workspace, and holding it is what keeps two agents from editing one
/// checkout at the same time (record 55).
#[derive(Clone)]
pub(crate) struct TurnQueue {
    pending: Arc<std::sync::Mutex<Option<QueuedFollowUp>>>,
    wake: Arc<Notify>,
    worktree: Arc<tokio::sync::Mutex<()>>,
}

impl TurnQueue {
    fn new(worktree: Arc<tokio::sync::Mutex<()>>) -> Self {
        Self {
            pending: Arc::new(std::sync::Mutex::new(None)),
            wake: Arc::new(Notify::new()),
            worktree,
        }
    }
}

/// Who is waiting on the other end of a turn, which decides how it waits.
///
/// A `Send` has an HTTP request open on it, so it may not block on a sibling's
/// turn — it takes the lock or reports back that it could not. A `Queued` turn
/// has already been acknowledged, so it can afford to wait.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TurnWait {
    Send,
    Queued,
}

/// The workspace's turn lock, and this caller's claim on waiting for it.
///
/// They travel together because neither answers anything alone: the lock says
/// which checkout is being reserved, and the wait says what this caller is
/// allowed to do when somebody else holds it.
#[derive(Clone, Copy)]
struct WorktreeTurn<'a> {
    lock: &'a tokio::sync::Mutex<()>,
    wait: TurnWait,
}

pub(crate) struct QueuedFollowUp {
    pub message: String,
    pub model: Option<String>,
    pub attachments: Vec<tidebreak_core::CodeTurnAttachment>,
}

pub(crate) struct LiveSink {
    db: Arc<DbStore>,
    bus: Arc<CodeEventBus>,
    /// Owner of the session this sink writes for. Carried explicitly so every
    /// journal and approval write the worker makes stays inside one owner.
    owner: OwnerId,
    session_id: CodeSessionId,
    spawn_epoch: i64,
    turn_id: std::sync::Mutex<Option<CodeTurnId>>,
    /// Engine-lifetime unrecognized count already folded onto the session row.
    ///
    /// The engine's own count is cumulative for as long as it is attached, and
    /// the sink is created once per attachment, so the two share a lifetime.
    flushed_unrecognized: AtomicU64,
    /// Harness subagents observed on this session (decision 52), the
    /// in-memory copy of the row's list so a `Task` boundary never
    /// read-modify-writes the row per event. Persisted through the targeted
    /// [`set_session_subagents`] write on each boundary.
    subagents: std::sync::Mutex<Vec<CodeSubagentSummary>>,
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

    /// Track subagent spans (decision 52): a top-level `Task` call opens one,
    /// its completion closes it. A terminal parent turn also settles any child
    /// whose own completion was lost, so the rail never claims work survived a
    /// turn that has already ended. Boundaries are rare, so each one persists
    /// the list through the targeted write and restates the session's digest.
    async fn note_subagent_boundary(&self, event: &CodeEvent) {
        let changed = match event {
            CodeEvent::ToolStarted {
                call_id,
                name,
                detail,
                parent_call_id: None,
            } if name == "Task" => {
                let mut subagents = self.subagents.lock().expect("code sink subagents");
                let display = if detail.subject().trim().is_empty() {
                    name.clone()
                } else {
                    detail.subject().to_owned()
                };
                subagents.retain(|entry| entry.call_id != *call_id);
                subagents.push(CodeSubagentSummary {
                    call_id: call_id.clone(),
                    name: display,
                    status: CodeSubagentStatus::Running,
                });
                bound_subagents(&mut subagents);
                Some(subagents.clone())
            }
            CodeEvent::ToolCompleted {
                call_id,
                outcome,
                detail,
                ..
            } => {
                let mut subagents = self.subagents.lock().expect("code sink subagents");
                match subagents.iter_mut().find(|entry| entry.call_id == *call_id) {
                    Some(entry) => {
                        entry.status = match outcome {
                            ToolOutcome::Succeeded => CodeSubagentStatus::Done,
                            ToolOutcome::Failed | ToolOutcome::Denied => CodeSubagentStatus::Failed,
                        };
                        // The started call streams in before its arguments, so
                        // the description often lands only on the completion's
                        // corrected detail. Better a late name than none.
                        if let Some(subject) = detail
                            .as_ref()
                            .map(|detail| detail.subject().trim())
                            .filter(|subject| !subject.is_empty())
                        {
                            entry.name = subject.to_owned();
                        }
                        Some(subagents.clone())
                    }
                    None => None,
                }
            }
            CodeEvent::TurnCompleted { .. } => {
                let mut subagents = self.subagents.lock().expect("code sink subagents");
                settle_running_subagents(&mut subagents, CodeSubagentStatus::Done)
                    .then(|| subagents.clone())
            }
            CodeEvent::TurnFailed { .. } | CodeEvent::TurnInterrupted => {
                let mut subagents = self.subagents.lock().expect("code sink subagents");
                settle_running_subagents(&mut subagents, CodeSubagentStatus::Failed)
                    .then(|| subagents.clone())
            }
            _ => None,
        };
        let Some(subagents) = changed else {
            return;
        };
        let _ = set_session_subagents(&self.db, &self.owner, self.session_id, &subagents).await;
        if let Ok(Some(session)) = get_session(&self.db, &self.owner, self.session_id).await {
            super::attention::emit_digest(&self.db, &self.bus, &session).await;
        }
    }

    async fn record_approval(
        &self,
        harness_ref: &tidebreak_harness::HarnessApprovalRef,
        raw: &serde_json::Value,
    ) {
        let existing = *self.turn_id.lock().expect("code sink turn");
        let turn_id = match existing {
            Some(id) => id,
            None => match get_open_turn(&self.db, &self.owner, self.session_id).await {
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
        if insert_approval(&self.db, &self.owner, &approval)
            .await
            .is_err()
        {
            return;
        }
        let _ = super::attention::apply_attention(
            &self.db,
            &self.bus,
            &self.owner,
            self.session_id,
            Attention::needs_you("an approval is waiting", AttentionSource::Structured),
            false,
        )
        .await;
        let _ = persist_and_publish(
            &self.db,
            &self.bus,
            &self.owner,
            self.session_id,
            self.spawn_epoch,
            CodeEvent::ApprovalRequested {
                approval_id: approval.id,
            },
        )
        .await;
    }
}

/// Settle every still-running child at a parent lifecycle boundary.
///
/// A harness can lose the final `Task` result when its process exits or the
/// app restarts. The parent boundary is still authoritative: success means the
/// child stopped without an observed failure, while failure/interruption means
/// its unfinished work cannot honestly remain Running.
pub(crate) fn settle_running_subagents(
    subagents: &mut [CodeSubagentSummary],
    status: CodeSubagentStatus,
) -> bool {
    debug_assert!(status != CodeSubagentStatus::Running);
    let mut changed = false;
    for subagent in subagents {
        if subagent.status == CodeSubagentStatus::Running {
            subagent.status = status;
            changed = true;
        }
    }
    changed
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
        self.note_subagent_boundary(&code_event).await;
        // An approval is parked on the call this completion names. Reconcile
        // it after the completion lands, so the journal reads in the order it
        // happened. See `approval_sweep`.
        let completed_call = match &code_event {
            CodeEvent::ToolCompleted { call_id, .. } => Some(call_id.clone()),
            _ => None,
        };
        match persist_and_publish(
            &self.db,
            &self.bus,
            &self.owner,
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
        if let Some(call_id) = completed_call {
            super::approval_sweep::abandon_for_call(
                &self.db,
                &self.bus,
                &self.owner,
                self.session_id,
                self.spawn_epoch,
                &call_id,
            )
            .await;
        }
    }
}

/// Start the worker for a session.
///
/// `worktree_turn` is the workspace's turn lock, shared with every other
/// session in the same checkout. The worker holds it for the length of a
/// turn, which is what keeps two agents from editing one worktree at once;
/// see record 55.
pub(crate) fn spawn_session_worker(
    session: CodeSession,
    engine: Box<dyn HarnessSession>,
    sink: Arc<LiveSink>,
    blobs: Option<Arc<dyn BlobStore>>,
    worktree_turn: Arc<tokio::sync::Mutex<()>>,
) -> WorkerHandle {
    let (tx, rx) = mpsc::channel(8);
    let spawn_epoch = session.spawn_epoch;
    let queue = TurnQueue::new(worktree_turn);
    tokio::spawn(run_worker(
        session,
        engine,
        sink.clone(),
        queue.clone(),
        blobs,
        rx,
    ));
    WorkerHandle {
        spawn_epoch,
        commands: tx,
        queue,
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
    let mut pending = handle.queue.pending.lock().expect("code turn queue");
    if pending.is_some() {
        return false;
    }
    *pending = Some(QueuedFollowUp {
        message,
        model,
        attachments,
    });
    handle.queue.wake.notify_one();
    true
}

async fn run_worker(
    mut session: CodeSession,
    engine: Box<dyn HarnessSession>,
    sink: Arc<LiveSink>,
    queue: TurnQueue,
    blobs: Option<Arc<dyn BlobStore>>,
    mut commands: mpsc::Receiver<WorkerCommand>,
) {
    if let Some(pid) = engine.child_pid() {
        session.child_pid = Some(pid);
        let _ = save_session(&sink.db, &session).await;
    }

    loop {
        if session_was_ended(&sink.db, &mut session).await {
            break;
        }
        drain_queued(
            &mut session,
            engine.as_ref(),
            &sink,
            &queue,
            blobs.as_deref(),
            &mut commands,
        )
        .await;
        tokio::select! {
            _ = queue.wake.notified() => {}
            command = commands.recv() => match command {
                Some(WorkerCommand::RunTurn {
                    message,
                    model,
                    attachments,
                    reply,
                }) => {
                    let result = drive_turn(
                        &mut session,
                        engine.as_ref(),
                        &sink,
                        blobs.as_deref(),
                        &mut commands,
                        WorktreeTurn {
                            lock: &queue.worktree,
                            wait: TurnWait::Send,
                        },
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
                    if apply_control(engine.as_ref(), command, None).await
                        == ControlFlow::Shutdown
                    {
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

#[cfg(not(test))]
const STEER_CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
const STEER_CONTROL_TIMEOUT: Duration = Duration::from_millis(100);

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

/// Wait for the workspace's turn lock, still answering control commands.
///
/// A queued turn can sit here for as long as a sibling's turn runs. Parking on
/// a bare `lock()` would leave the worker deaf for that whole stretch: an
/// interrupt sent while the message was still queued would be delivered to the
/// engine only once the turn had started, which reads as the stop being
/// ignored. Returns `None` when the turn should not start at all.
async fn await_worktree_turn<'a>(
    engine: &dyn HarnessSession,
    worktree_turn: &'a tokio::sync::Mutex<()>,
    commands: &mut mpsc::Receiver<WorkerCommand>,
) -> Option<tokio::sync::MutexGuard<'a, ()>> {
    loop {
        tokio::select! {
            guard = worktree_turn.lock() => return Some(guard),
            command = commands.recv() => match command {
                // Stopping something that has not started needs no engine
                // call: dropping the turn here is the stop.
                Some(WorkerCommand::Interrupt { reply }) => {
                    let _ = reply.send(Ok(()));
                    return None;
                }
                Some(WorkerCommand::Shutdown) | None => return None,
                // There is no turn yet, so steering has nothing to steer and
                // a second RunTurn is a conflict. `apply_control` answers both
                // that way already.
                Some(command) => {
                    if apply_control(engine, command, None).await == ControlFlow::Shutdown {
                        return None;
                    }
                }
            },
        }
    }
}

async fn apply_control(
    engine: &dyn HarnessSession,
    command: WorkerCommand,
    active_turn_id: Option<CodeTurnId>,
) -> ControlFlow {
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
        WorkerCommand::Steer {
            expected_turn_id,
            message,
            reply,
        } => {
            let result = match active_turn_id {
                None => Err(WorkerError::NoActiveTurn(
                    "there is no active turn to steer; the message was not queued".into(),
                )),
                Some(active) if active != expected_turn_id => Err(WorkerError::StaleTurn(
                    format!(
                        "turn {expected_turn_id} is no longer active; current turn is {active}; the message was not queued"
                    ),
                )),
                Some(_) => match tokio::time::timeout(
                    STEER_CONTROL_TIMEOUT,
                    engine.steer(message),
                )
                .await
                {
                    Ok(result) => result.map_err(|err| match err {
                        HarnessError::SteeringUnsupported => WorkerError::SteeringUnavailable(
                            "mid-turn steering is not available on this engine; the message was not queued"
                                .into(),
                        ),
                        HarnessError::SteeringRejected(detail) => {
                            WorkerError::SteeringRejected(detail)
                        }
                        other => WorkerError::Failed(other.to_string()),
                    }),
                    Err(_) => Err(WorkerError::SteeringRejected(
                        "the engine did not acknowledge steering before the control timeout; the message was not queued"
                            .into(),
                    )),
                },
            };
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
    session: &mut CodeSession,
    engine: &dyn HarnessSession,
    sink: &LiveSink,
    queue: &TurnQueue,
    blobs: Option<&dyn BlobStore>,
    commands: &mut mpsc::Receiver<WorkerCommand>,
) {
    loop {
        let next = queue.pending.lock().expect("code turn queue").take();
        let Some(follow_up) = next else {
            break;
        };
        let _ = drive_turn(
            session,
            engine,
            sink,
            blobs,
            commands,
            WorktreeTurn {
                lock: &queue.worktree,
                wait: TurnWait::Queued,
            },
            follow_up,
        )
        .await;
    }
}

async fn drive_turn(
    session: &mut CodeSession,
    engine: &dyn HarnessSession,
    sink: &LiveSink,
    blobs: Option<&dyn BlobStore>,
    commands: &mut mpsc::Receiver<WorkerCommand>,
    worktree: WorktreeTurn<'_>,
    follow_up: QueuedFollowUp,
) -> Result<CodeTurn, WorkerError> {
    let session_id = session.id;
    let wait = match worktree.wait {
        TurnWait::Send => "send",
        TurnWait::Queued => "queued",
    };
    let span = tracing::info_span!(
        target: crate::diagnostics::EVENT_TARGET,
        "tidebreak.code_turn",
        otel.name = "tidebreak.code_turn",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        tidebreak.code.session_id = %session_id,
        tidebreak.code.wait = wait,
        tidebreak.outcome = tracing::field::Empty,
        tidebreak.duration_ms = tracing::field::Empty,
    );
    let started = Instant::now();
    let result = drive_turn_inner(session, engine, sink, blobs, commands, worktree, follow_up)
        .instrument(span.clone())
        .await;
    let duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let outcome = code_turn_outcome(&result);
    span.record("tidebreak.outcome", outcome);
    span.record("tidebreak.duration_ms", duration_ms);
    span.record(
        "otel.status_code",
        if code_turn_is_error(&result) {
            "ERROR"
        } else {
            "OK"
        },
    );
    span.in_scope(|| {
        tracing::info!(
            target: crate::diagnostics::EVENT_TARGET,
            event_name = "tidebreak.code_turn.completed",
            outcome,
            duration_ms,
            "code turn completed"
        );
    });
    result
}

async fn drive_turn_inner(
    session: &mut CodeSession,
    engine: &dyn HarnessSession,
    sink: &LiveSink,
    blobs: Option<&dyn BlobStore>,
    commands: &mut mpsc::Receiver<WorkerCommand>,
    worktree: WorktreeTurn<'_>,
    QueuedFollowUp {
        message,
        model,
        attachments,
    }: QueuedFollowUp,
) -> Result<CodeTurn, WorkerError> {
    let db = &sink.db;
    let bus = &sink.bus;
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
    let images = hydrate_turn_images(blobs, &attachments).await?;

    // The workspace's checkout takes one turn at a time (record 55). Taking
    // the lock *is* the reservation: a pre-flight database read cannot be,
    // because two idle siblings both pass it before either marks itself
    // running. Blobs are hydrated first so a decode never holds the tree.
    let _worktree = match worktree.wait {
        // A send has its request open, so it never waits on a sibling's turn.
        // The caller parks the message and answers `Queued` instead.
        TurnWait::Send => worktree
            .lock
            .try_lock()
            .map_err(|_| WorkerError::WorktreeBusy)?,
        // Already acknowledged, so waiting costs nobody a connection. The wait
        // still listens for control: an interrupt that arrives while a turn is
        // queued has to stop it before it starts, not after.
        TurnWait::Queued => match await_worktree_turn(engine, worktree.lock, commands).await {
            Some(guard) => guard,
            None => return Err(WorkerError::Conflict("the queued turn was stopped".into())),
        },
    };
    // Ending a session during that wait has to win. The lifecycle checks above
    // read a session that may be minutes stale by now.
    if session_was_ended(&sink.db, session).await {
        return Err(WorkerError::Conflict("session has ended".into()));
    }

    if let Some(model) = model.clone() {
        session.model = Some(model);
    }

    let ordinal = next_turn_ordinal(db, &session.owner, session.id)
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
    insert_turn(db, &session.owner, &turn)
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
        &session.owner,
        session.id,
        session.spawn_epoch,
        CodeEvent::TurnStarted { turn_id: turn.id },
    )
    .await
    .map_err(|err| WorkerError::Failed(err.to_string()))?;

    let run = engine.run_turn(TurnInput {
        text: message,
        model: model.or_else(|| session.model.clone()),
        images,
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
            // Once a control has been accepted from the command channel, poll
            // it before allowing a simultaneously-terminal turn to win. Native
            // steering registers its response waiter on that first poll; the
            // turn reader can then drain and demultiplex the acknowledgement.
            biased;
            Some(flow) = controls.next(), if !controls.is_empty() => {
                if flow == ControlFlow::Shutdown {
                    interrupted = true;
                    controls.push(Box::pin(interrupt_engine(engine)));
                }
            }
            // A terminal result wins over commands that have not yet been
            // admitted. This prevents guidance for turn A from entering the
            // control set after A has already completed and then reaching B.
            result = &mut run => break result,
            command = commands.recv(), if !commands_closed => match command {
                Some(command) => {
                    interrupted |= matches!(command, WorkerCommand::Interrupt { .. });
                    controls.push(Box::pin(apply_control(engine, command, Some(turn.id))));
                }
                None => {
                    commands_closed = true;
                    interrupted = true;
                    controls.push(Box::pin(interrupt_engine(engine)));
                }
            },
            pid = next_child_pid(pid_changes.as_mut()) => {
                if session.child_pid != pid {
                    session.child_pid = pid;
                    let _ = save_session(db, session).await;
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
    if let Ok(Some(updated)) = get_open_turn(db, &session.owner, session.id).await {
        turn = updated;
    } else if let Ok(Some(current)) =
        tidebreak_core::db::code::get_turn(db, &session.owner, turn.id).await
    {
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
                    &session.owner,
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
                let _ = save_turn(db, &session.owner, &turn).await;
                sink.note_subagent_boundary(&event).await;
                let _ = persist_and_publish(
                    db,
                    bus,
                    &session.owner,
                    session.id,
                    session.spawn_epoch,
                    event,
                )
                .await;
            }
        }
        Err(err) => {
            if turn.status == CodeTurnStatus::Running {
                turn.status = CodeTurnStatus::Failed;
                turn.ended_at = Some(Utc::now());
                let _ = save_turn(db, &session.owner, &turn).await;
                let event = CodeEvent::TurnFailed {
                    error: BoundedError {
                        message: err.to_string(),
                    },
                };
                sink.note_subagent_boundary(&event).await;
                let _ = persist_and_publish(
                    db,
                    bus,
                    &session.owner,
                    session.id,
                    session.spawn_epoch,
                    event,
                )
                .await;
            }
            // A harness error returns from here, so this is the only place
            // the turn's edits can still be checkpointed. The engine may have
            // rewritten files before the stream broke.
            super::checkpoint::after_turn_ended(db, bus, session, &mut turn).await;
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

    if let Ok(Some(current)) = tidebreak_core::db::code::get_turn(db, &session.owner, turn.id).await
    {
        turn = current;
    }
    super::checkpoint::after_turn_ended(db, bus, session, &mut turn).await;
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
        // Whatever broke may not be about this prompt. An expired credential
        // or an unreachable provider fails every turn identically, and a
        // session that keeps saying "idle" invites the user to retry into it
        // forever. Once enough turns fail back to back, fence and offer a
        // reap.
        if let Some(reason) = repeated_failure_fence(db, session, &turn).await {
            let _ = super::recovery::fence_session(db, bus, session, reason).await;
            return Ok(turn);
        }
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

fn code_turn_outcome(result: &Result<CodeTurn, WorkerError>) -> &'static str {
    match result {
        Ok(turn) => turn.status.as_str(),
        Err(WorkerError::Conflict(_)) => "conflict",
        Err(WorkerError::NoActiveTurn(_)) => "no_active_turn",
        Err(WorkerError::StaleTurn(_)) => "stale_turn",
        Err(WorkerError::SteeringUnavailable(_)) => "steering_unavailable",
        Err(WorkerError::SteeringRejected(_)) => "steering_rejected",
        Err(WorkerError::Failed(_)) => "error",
        Err(WorkerError::WorktreeBusy) => "worktree_busy",
    }
}

fn code_turn_is_error(result: &Result<CodeTurn, WorkerError>) -> bool {
    matches!(
        result,
        Ok(CodeTurn {
            status: CodeTurnStatus::Failed,
            ..
        }) | Err(WorkerError::Failed(_))
    )
}

async fn hydrate_turn_images(
    blobs: Option<&dyn BlobStore>,
    attachments: &[tidebreak_core::CodeTurnAttachment],
) -> Result<Vec<TurnImage>, WorkerError> {
    if attachments.is_empty() {
        return Ok(Vec::new());
    }
    let Some(blobs) = blobs else {
        return Err(WorkerError::Failed(
            "image attachments require blob storage".into(),
        ));
    };
    let mut images = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        let bytes = blobs
            .get(attachment.blob_id)
            .await
            .map_err(|err| WorkerError::Failed(format!("blob read: {err}")))?
            .ok_or_else(|| {
                WorkerError::Failed(format!("attachment blob {} is missing", attachment.blob_id))
            })?;
        images.push(TurnImage {
            media_type: attachment.media_type.as_str().to_owned(),
            bytes,
        });
    }
    Ok(images)
}

/// How many turns must fail back to back before the session is fenced.
///
/// Three, not one: a single failure is ordinary — a bad prompt, a transient
/// provider hiccup, a tool that blew up — and fencing on it would be worse
/// than the problem. Three in a row with no success between them is a
/// property of the session, not of any one turn.
const REPEATED_FAILURE_FENCE: u32 = 3;

/// A fence reason when this failure is the latest in an unbroken run of them.
///
/// Counts backwards over the session's turns and stops at the first one that
/// did not fail, so a session that recovers starts the count over.
async fn repeated_failure_fence(
    db: &DbStore,
    session: &CodeSession,
    turn: &CodeTurn,
) -> Option<FenceReason> {
    let turns = tidebreak_core::db::code::list_turns(db, &session.owner, session.id)
        .await
        .ok()?;
    let mut count = 0u32;
    for candidate in turns.iter().rev() {
        match candidate.status {
            CodeTurnStatus::Failed => count += 1,
            // A turn still running is the one we are closing out; anything
            // else ends the streak.
            CodeTurnStatus::Running if candidate.id == turn.id => count += 1,
            _ => break,
        }
    }
    if count < REPEATED_FAILURE_FENCE {
        return None;
    }
    let detail = last_failure_detail(db, session).await.unwrap_or_default();
    Some(FenceReason::RepeatedTurnFailures { count, detail })
}

/// The message from the most recent `TurnFailed` in the journal.
///
/// The turn row keeps no error column, so the journal is the only place the
/// reason survives. Without it the fence would say only that turns failed.
async fn last_failure_detail(db: &DbStore, session: &CodeSession) -> Option<String> {
    let events = tidebreak_core::db::code::list_recent_events(db, &session.owner, session.id, 64)
        .await
        .ok()?;
    events.into_iter().find_map(|item| match item.event {
        CodeEvent::TurnFailed { error } => Some(error.message),
        _ => None,
    })
}

async fn session_was_ended(db: &DbStore, session: &mut CodeSession) -> bool {
    match get_session(db, &session.owner, session.id).await {
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
    let mut session = get_session_all_owners(db, session_id)
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
        &session.owner,
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
    owner: OwnerId,
    session_id: CodeSessionId,
    spawn_epoch: i64,
    turn_id: Option<CodeTurnId>,
    subagents: Vec<CodeSubagentSummary>,
) -> Arc<LiveSink> {
    Arc::new(LiveSink {
        db,
        bus,
        owner,
        session_id,
        spawn_epoch,
        turn_id: std::sync::Mutex::new(turn_id),
        flushed_unrecognized: AtomicU64::new(0),
        subagents: std::sync::Mutex::new(subagents),
    })
}

pub(crate) async fn journal_event(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    session_id: CodeSessionId,
    spawn_epoch: i64,
    event: CodeEvent,
) -> Result<(), CodeJournalError> {
    persist_and_publish(db, bus, owner, session_id, spawn_epoch, event).await
}

async fn persist_and_publish(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    session_id: CodeSessionId,
    spawn_epoch: i64,
    event: CodeEvent,
) -> Result<(), CodeJournalError> {
    let activity_boundary = matches!(
        &event,
        CodeEvent::ToolStarted {
            parent_call_id: None,
            ..
        } | CodeEvent::ToolCompleted {
            parent_call_id: None,
            ..
        }
    );
    apply_side_effects(db, owner, session_id, spawn_epoch, &event).await?;
    let seq = append_event(db, owner, session_id, spawn_epoch, &event).await?;
    if is_activity(&event) {
        let _ = super::attention::note_activity(db, bus, owner, session_id).await;
    }
    // A tool call the engine dropped without reporting a completion leaves
    // its approval pending. The turn is over, so nothing can decide it now.
    // Every route that closes a turn passes through here, and each does so
    // before writing the turn's own attention verdict, which supersedes the
    // sweep's. Swept after this event is published so the resolution it
    // journals keeps its later sequence number on the live stream too.
    let closes_turn = matches!(
        &event,
        CodeEvent::TurnCompleted { .. } | CodeEvent::TurnFailed { .. } | CodeEvent::TurnInterrupted
    );
    bus.publish(
        session_id,
        tidebreak_core::SequencedCodeEvent { seq, event },
    );
    if closes_turn {
        super::approval_sweep::abandon_for_settled_turns(db, bus, owner, session_id, spawn_epoch)
            .await;
    }
    if activity_boundary {
        if let Ok(Some(session)) = get_session(db, owner, session_id).await {
            super::attention::emit_digest(db, bus, &session).await;
        }
    }
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
    owner: &OwnerId,
    session_id: CodeSessionId,
    _spawn_epoch: i64,
    event: &CodeEvent,
) -> Result<(), CodeJournalError> {
    match event {
        CodeEvent::TurnCompleted { usage, checkpoint } => {
            if let Ok(Some(mut turn)) = get_open_turn(db, owner, session_id).await {
                turn.status = CodeTurnStatus::Completed;
                turn.ended_at = Some(Utc::now());
                turn.usage = Some(usage.clone());
                if let Some(hint) = checkpoint {
                    turn.checkpoint_ref = hint.checkpoint_ref.clone();
                    turn.diffstat = hint.diffstat.clone();
                }
                let _ = save_turn(db, owner, &turn).await;
            }
        }
        CodeEvent::TurnFailed { .. } => {
            if let Ok(Some(mut turn)) = get_open_turn(db, owner, session_id).await {
                turn.status = CodeTurnStatus::Failed;
                turn.ended_at = Some(Utc::now());
                let _ = save_turn(db, owner, &turn).await;
            }
        }
        CodeEvent::TurnInterrupted => {
            if let Ok(Some(mut turn)) = get_open_turn(db, owner, session_id).await {
                turn.status = CodeTurnStatus::Interrupted;
                turn.ended_at = Some(Utc::now());
                let _ = save_turn(db, owner, &turn).await;
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
    let input = raw
        .get("input")
        .or_else(|| raw.get("metadata"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    // Codex puts the command on the request itself, not under `input`.
    if let Some(cmd) = raw
        .get("command")
        .and_then(serde_json::Value::as_str)
        .filter(|cmd| !cmd.is_empty())
    {
        return CodeApprovalKind::Command {
            cmd: cmd.to_owned(),
            cwd: raw
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        };
    }
    if let Some(cmd) = input
        .get("command")
        .and_then(serde_json::Value::as_str)
        .filter(|cmd| !cmd.is_empty())
    {
        return CodeApprovalKind::Command {
            cmd: cmd.to_owned(),
            cwd: input
                .get("cwd")
                .or_else(|| raw.get("cwd"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        };
    }
    let tool = raw
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .or_else(|| raw.get("permission").and_then(serde_json::Value::as_str))
        .unwrap_or("");
    let path = input
        .get("file_path")
        .or_else(|| input.get("path"))
        .or_else(|| raw.get("path"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    match tool {
        "Write" | "Edit" | "NotebookEdit" | "write" | "edit" => CodeApprovalKind::FileWrite {
            paths: vec![path.to_owned()],
        },
        "Bash" | "bash" => CodeApprovalKind::Command {
            cmd: String::new(),
            cwd: input
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        },
        "WebFetch" | "WebSearch" | "webfetch" | "websearch" => CodeApprovalKind::Network {
            summary: tool.to_owned(),
        },
        "Read" | "read" | "Grep" | "grep" | "Glob" | "glob" | "NotebookRead" => {
            CodeApprovalKind::Other {
                summary: if path.is_empty() {
                    tool.to_owned()
                } else {
                    format!("{tool} {path}")
                },
            }
        }
        "" | "unknown" => CodeApprovalKind::Other {
            summary: "The engine needs approval".to_owned(),
        },
        other => CodeApprovalKind::Other {
            summary: other.to_owned(),
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
        HarnessEvent::AssistantMessage {
            text,
            parent_call_id,
        } => CodeEvent::AssistantMessage {
            text,
            parent_call_id,
        },
        HarnessEvent::ReasoningDelta { text } => CodeEvent::ReasoningDelta { text },
        HarnessEvent::ToolStarted {
            call_id,
            name,
            detail,
            parent_call_id,
        } => CodeEvent::ToolStarted {
            call_id,
            name,
            detail,
            parent_call_id,
        },
        HarnessEvent::ToolCompleted {
            call_id,
            outcome,
            preview,
            detail,
            parent_call_id,
        } => CodeEvent::ToolCompleted {
            call_id,
            outcome,
            preview,
            detail,
            parent_call_id,
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
    use chrono::Utc;
    use tidebreak_core::db::code::{get_session, insert_repo, insert_session, insert_workspace};
    use tidebreak_core::{
        CodePermissionMode, CodeRepo, CodeSessionKind, CodeUsage, CodeWorkspace,
        CodeWorkspaceStatus, HarnessKind, RepoId, ToolDetail, WorkspaceId,
    };

    fn subagent(call_id: &str, status: CodeSubagentStatus) -> CodeSubagentSummary {
        CodeSubagentSummary {
            call_id: call_id.into(),
            name: call_id.into(),
            status,
        }
    }

    #[test]
    fn parent_boundaries_settle_only_running_subagents() {
        let mut completed = vec![
            subagent("running", CodeSubagentStatus::Running),
            subagent("done", CodeSubagentStatus::Done),
            subagent("failed", CodeSubagentStatus::Failed),
        ];
        assert!(settle_running_subagents(
            &mut completed,
            CodeSubagentStatus::Done
        ));
        assert_eq!(completed[0].status, CodeSubagentStatus::Done);
        assert_eq!(completed[1].status, CodeSubagentStatus::Done);
        assert_eq!(completed[2].status, CodeSubagentStatus::Failed);
        assert!(!settle_running_subagents(
            &mut completed,
            CodeSubagentStatus::Failed
        ));

        let mut failed = vec![subagent("running", CodeSubagentStatus::Running)];
        assert!(settle_running_subagents(
            &mut failed,
            CodeSubagentStatus::Failed
        ));
        assert_eq!(failed[0].status, CodeSubagentStatus::Failed);
    }

    async fn seeded_sink() -> (
        tempfile::TempDir,
        Arc<DbStore>,
        Arc<LiveSink>,
        CodeSessionId,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let owner = OwnerId::local();
        let repo_id = RepoId::new();
        insert_repo(
            &store,
            &CodeRepo {
                id: repo_id,
                owner: owner.clone(),
                root_path: directory.path().join("repo").display().to_string(),
                display_name: "example".into(),
                default_base_ref: "main".into(),
                branch_prefix: "tidebreak/".into(),
                setup_script: None,
                archive_script: None,
                quick_actions: Vec::new(),
                created_at: Utc::now(),
                removed_at: None,
            },
        )
        .await
        .unwrap();
        let workspace_id = WorkspaceId::new();
        insert_workspace(
            &store,
            &CodeWorkspace {
                id: workspace_id,
                owner: owner.clone(),
                repo_id,
                title: "first".into(),
                worktree_path: directory.path().join("wt").display().to_string(),
                branch_name: "tidebreak/first".into(),
                base_ref: "main".into(),
                status: CodeWorkspaceStatus::Active,
                pr: None,
                created_at: Utc::now(),
                archived_at: None,
                released_at: None,
                released_tip: None,
                bundle_bytes: None,
            },
        )
        .await
        .unwrap();
        let session_id = CodeSessionId::new();
        insert_session(
            &store,
            &CodeSession {
                id: session_id,
                owner: owner.clone(),
                workspace_id,
                kind: CodeSessionKind::Interactive,
                harness_kind: HarnessKind::ClaudeCode,
                harness_version: Some("2.1.237".into()),
                harness_resume_ref: None,
                permission_mode: CodePermissionMode::Plan,
                model: None,
                lifecycle: CodeSessionLifecycle::Running,
                fence_reason: None,
                child_pid: None,
                spawn_epoch: 1,
                attention: Attention::working(AttentionSource::Lifecycle),
                unrecognized_event_count: 0,
                subagents: Vec::new(),
                created_at: Utc::now(),
            },
        )
        .await
        .unwrap();
        let sink = sink_for(
            store.clone(),
            Arc::new(CodeEventBus::default()),
            owner,
            session_id,
            1,
            None,
            Vec::new(),
        );
        (directory, store, sink, session_id)
    }

    #[tokio::test]
    async fn sink_settles_unclosed_tasks_at_each_terminal_parent_boundary() {
        let (_directory, store, sink, session_id) = seeded_sink().await;
        let cases = [
            (
                "completed",
                HarnessEvent::TurnCompleted {
                    usage: CodeUsage::default(),
                },
                CodeSubagentStatus::Done,
            ),
            (
                "failed",
                HarnessEvent::TurnFailed {
                    error: BoundedError {
                        message: "engine failed".into(),
                    },
                },
                CodeSubagentStatus::Failed,
            ),
            (
                "interrupted",
                HarnessEvent::TurnInterrupted,
                CodeSubagentStatus::Failed,
            ),
        ];

        for (call_id, boundary, expected) in cases {
            sink.emit(HarnessEvent::ToolStarted {
                call_id: call_id.into(),
                name: "Task".into(),
                detail: ToolDetail::Other {
                    summary: format!("{call_id} child"),
                },
                parent_call_id: None,
            })
            .await;
            sink.emit(boundary).await;
            let session = get_session(&store, &OwnerId::local(), session_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                session
                    .subagents
                    .iter()
                    .find(|entry| entry.call_id == call_id)
                    .map(|entry| entry.status),
                Some(expected)
            );
        }
    }

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

    #[test]
    fn kind_from_raw_reads_codex_and_opencode_payloads() {
        assert_eq!(
            kind_from_raw(&serde_json::json!({
                "command": "/bin/zsh -lc rg foo",
                "cwd": "/workspace",
            })),
            CodeApprovalKind::Command {
                cmd: "/bin/zsh -lc rg foo".into(),
                cwd: Some("/workspace".into()),
            }
        );
        assert_eq!(
            kind_from_raw(&serde_json::json!({
                "permission": "bash",
                "metadata": { "command": "rg foo" },
            })),
            CodeApprovalKind::Command {
                cmd: "rg foo".into(),
                cwd: None,
            }
        );
        assert_eq!(
            kind_from_raw(&serde_json::json!({
                "tool_name": "Read",
                "input": { "file_path": "/workspace/README.md" },
            })),
            CodeApprovalKind::Other {
                summary: "Read /workspace/README.md".into(),
            }
        );
        assert_eq!(
            kind_from_raw(&serde_json::Value::Null),
            CodeApprovalKind::Other {
                summary: "The engine needs approval".into(),
            }
        );
    }
}
