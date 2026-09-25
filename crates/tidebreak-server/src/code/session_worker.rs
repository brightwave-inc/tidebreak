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
use tracing::{error, warn, Instrument as _};

use tidebreak_core::code::QueuedTurn;
use tidebreak_core::db::code::{
    accept_trigger_turn_delivery, append_event, append_event_with_notification,
    begin_permission_mode_change, bump_spawn_epoch, cancel_permission_mode_change, clear_turn_park,
    confirm_permission_mode_change, delete_queued_turn, get_open_turn, get_session,
    get_session_all_owners, get_workspace, insert_approval_for_worker, insert_turn, list_approvals,
    next_turn_ordinal, promote_queued_turn, queue_paused, queued_turn_head,
    rebind_pending_approvals_to_worker, save_session, save_turn, set_queue_paused,
    set_session_child_process, set_session_harness_resume_ref, set_session_subagents,
    settle_engine_observed_approval, store_turn_park, JournalError, SessionExecutionSettings,
};
use tidebreak_core::{
    bound_subagents, Approval, ApprovalId, ApprovalKind, ApprovalState, Attention, AttentionSource,
    BlobStore, BoundedError, CodeSubagentStatus, CodeSubagentSummary, CodeWorkspaceStatus, DbStore,
    Event, FenceReason, HarnessKind, HarnessNoticeLevel, OwnerId, PermissionMode, Session,
    SessionId, SessionLifecycle, SettleChildSessionWaitOutcome, Store, ToolOutcome, Turn,
    TurnFailure, TurnFailureCategory, TurnId, TurnStatus,
};
use tidebreak_harness::{
    ApprovalDecision, HarnessApprovalRef, HarnessError, HarnessEvent, HarnessEventSink,
    HarnessSession, TurnImage, TurnInput, TurnOutcome,
};

use super::bus::{CodeEventBus, CodeLiveEvent};
use crate::stack::boxed;

pub(crate) enum WorkerCommand {
    RunTurn {
        message: String,
        attachments: Vec<tidebreak_core::ImageRef>,
        /// Who submitted it (decision 0086). `None` for a path that knows of
        /// no one but the owner.
        actor: Option<tidebreak_core::TurnActor>,
        trigger_delivery: Option<TriggerDeliveryClaim>,
        reply: oneshot::Sender<Result<Turn, WorkerError>>,
    },
    SetPermissionMode {
        mode: PermissionMode,
        settlement: oneshot::Receiver<PermissionModeSettlement>,
        reply: oneshot::Sender<Result<(), WorkerError>>,
    },
    SetExecutionSettings {
        settings: SessionExecutionSettings,
        settlement: oneshot::Receiver<ExecutionSettingsSettlement>,
        reply: oneshot::Sender<Result<(), WorkerError>>,
    },
    Decide {
        approval: HarnessApprovalRef,
        // Boxed: the grant-carrying decision variants dwarf every other
        // command (clippy::large_enum_variant).
        decision: Box<ApprovalDecision>,
        reply: oneshot::Sender<Result<(), WorkerError>>,
    },
    Interrupt {
        reply: oneshot::Sender<Result<(), WorkerError>>,
    },
    Steer {
        expected_turn_id: TurnId,
        message: String,
        /// Durable external admission to settle, when this steer came from
        /// the Slack messages endpoint.
        admission: Option<SteerAdmission>,
        reply: oneshot::Sender<Result<(), WorkerError>>,
    },
    Shutdown,
}

/// One durable external steering admission the worker owes a settlement for.
#[derive(Clone)]
pub(crate) struct SteerAdmission {
    pub db: Arc<DbStore>,
    /// Owner of the external event row.
    pub owner: OwnerId,
    /// Session the event row belongs to.
    pub session_id: SessionId,
    pub spawn_epoch: i64,
    /// Channel event id, the idempotency key of the admission.
    pub event_id: String,
    /// The active Tidebreak turn the instruction targets.
    pub expected_turn_id: TurnId,
    /// Caller correlation id.
    pub correlation_uuid: Option<uuid::Uuid>,
}

/// The durable outcome that releases a worker after native mode acceptance.
pub(crate) enum PermissionModeSettlement {
    Confirmed,
    Abort,
}

/// The durable outcome that releases a worker after a settings reservation.
pub(crate) enum ExecutionSettingsSettlement {
    Confirmed,
    Abort,
}

/// One live outbox lease propagated only to the sink acceptance boundary.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TriggerDeliveryClaim {
    pub delivery_id: tidebreak_core::CodeTriggerDeliveryId,
    pub lease_token: uuid::Uuid,
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
    ApprovalDeliveryFailed(String),
    #[error("{0}")]
    ApprovalDeliveryUnknown(String),
    /// The engine fixes its posture at launch, so the caller relaunches.
    #[error("{0}")]
    RelaunchRequired(String),
    /// A queued row was edited, reordered, or retracted after the worker
    /// snapshotted it. Never reaches a caller: the drain loop re-reads the
    /// head and tries again.
    #[error("the queued turn changed before it could start")]
    QueuedTurnStale,
    /// A stop arrived while a queued turn waited for the workspace checkout.
    /// Never reaches a caller: the drain loop pauses the queue so the rows
    /// visibly hold until the reader resumes or fires send-now.
    #[error("the queued turn was stopped before it could start")]
    QueuedTurnStopped,
    #[error("{0}")]
    Failed(String),
    #[error("trigger delivery was already accepted")]
    TriggerDeliveryAccepted,
    /// A sibling session holds the workspace's turn lock.
    ///
    /// Never reaches a caller. `submit_turn` parks the message and answers
    /// `Queued`, so a send is never left holding its connection open for the
    /// length of someone else's turn.
    #[error("another session in this workspace is mid-turn")]
    WorktreeBusy,
    /// The process is quiescing for a restart-to-update, so no new turn may
    /// start. A send is parked as a durable queue row by `submit_turn`; a
    /// queued row stays in the queue and drains after the relaunch.
    #[error("Tidebreak is restarting for an update; the turn starts after the relaunch")]
    UpdateQuiesced,
}

#[derive(Clone)]
pub(crate) struct WorkerHandle {
    pub spawn_epoch: i64,
    /// The engine executable this worker launches, copied from the probe at
    /// spawn. A managed install or a channel flip that moves the selected
    /// binary compares against this to find the workers still on the old
    /// file. `None` for an in-process engine.
    pub binary: Option<std::path::PathBuf>,
    /// Whether the engine this worker launches loads the repository's own
    /// engine config, copied from the repository's trust decision at spawn.
    /// A changed decision compares against this to find the workers still
    /// launched under the old one.
    pub project_config: tidebreak_harness::ProjectConfig,
    pub commands: mpsc::Sender<WorkerCommand>,
    pub queue: TurnQueue,
    pub sink: Arc<LiveSink>,
    /// Serializes native approval delivery and durable finalization with
    /// every path that stops or replaces this worker.
    pub approval_decisions: Arc<tokio::sync::Mutex<()>>,
    /// Abort the worker task. Tests use this to simulate a crash that
    /// never runs the interrupt close.
    pub abort: tokio::task::AbortHandle,
}

/// How a worker gets its next turn, and when it may start one.
///
/// Queued follow-ups are durable `code_queued_turn` rows the worker drains
/// FIFO (decision 69); `wake` is how an enqueue, a resume, or a send-now
/// tells the worker to look again. `worktree` is shared with every other
/// session in the workspace, and holding it is what keeps two agents from
/// editing one checkout at the same time (record 55).
#[derive(Clone)]
pub(crate) struct TurnQueue {
    pub(crate) wake: Arc<Notify>,
    worktree: Arc<tokio::sync::Mutex<()>>,
    /// Flips true while the process quiesces for a restart-to-update: the
    /// drain holds, no new turn starts, and the idle loop parks the engine
    /// child immediately instead of waiting out the idle timer.
    quiesce: watch::Receiver<bool>,
}

impl TurnQueue {
    fn new(worktree: Arc<tokio::sync::Mutex<()>>, quiesce: watch::Receiver<bool>) -> Self {
        Self {
            wake: Arc::new(Notify::new()),
            worktree,
            quiesce,
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
    /// The process-wide update-quiesce flag, re-read after the lock is won:
    /// a turn must not start while a restart-to-update waits for boundaries.
    quiesce: &'a watch::Receiver<bool>,
}

/// Where a turn's attachments come from, and where they can be put.
///
/// Two routes, and the engine picks which. Claude Code takes image bytes on
/// its own protocol, so they ride `TurnInput::images`. Every other engine
/// reads attachments off disk with the file tools it already has, so the bytes
/// are written under Tidebreak's private data directory and the prompt carries
/// absolute paths. Git cannot index those files.
#[derive(Clone)]
pub(crate) struct AttachmentStore {
    /// Published bytes, addressed by blob id.
    pub blobs: Option<Arc<dyn BlobStore>>,
    /// Private storage for this workspace, outside every Git worktree.
    pub private_root: super::scratch::ScratchRoot,
    /// Whether this engine consumes images over its own protocol.
    pub engine_reads_images: bool,
}

pub(crate) struct QueuedFollowUp<'a> {
    pub message: String,
    pub attachments: Vec<tidebreak_core::ImageRef>,
    /// Who submitted the message (decision 0086). A promoted queue row
    /// carries its own, which wins over this.
    pub actor: Option<tidebreak_core::TurnActor>,
    pub trigger_delivery: Option<TriggerDeliveryClaim>,
    /// The durable queue row this turn promotes, when the message came from
    /// the queue rather than a live send. The turn is inserted under the
    /// row's id, in the transaction that deletes the row.
    pub queued_row: Option<Box<QueuedTurn>>,
    /// The sender waiting to hear that the turn was accepted.
    ///
    /// A send answers here as soon as the turn row exists and the start is
    /// journaled, not when the engine finishes: the reply can take minutes,
    /// and the caller follows it on the event bus. The turn takes the sender
    /// when it answers, so a sender still in the slot afterwards belongs to a
    /// turn that never started, and its caller hears why.
    pub accepted: Option<&'a mut Option<TurnReply>>,
}

/// The answer a live send waits for: the accepted turn, or why it did not
/// start.
pub(crate) type TurnReply = oneshot::Sender<Result<Turn, WorkerError>>;

pub(crate) struct LiveSink {
    db: Arc<DbStore>,
    bus: Arc<CodeEventBus>,
    /// Owner of the session this sink writes for. Carried explicitly so every
    /// journal and approval write the worker makes stays inside one owner.
    owner: OwnerId,
    session_id: SessionId,
    spawn_epoch: i64,
    /// Which engine this session runs, for copy that names it.
    harness: HarnessKind,
    /// Whether the engine writes the session's journal itself.
    ///
    /// The internal engine runs the chat turn lane, and that lane appends
    /// its rows — turn start, deltas, tool calls, steers, the terminal
    /// event — straight into the session's journal and publishes them on
    /// the session bus (decision 0048 step 5). What the engine then reports
    /// through this sink is already on disk, so the sink applies the side
    /// effects a report carries (closing the turn row with its usage,
    /// sweeping approvals) and writes nothing a second time. The approval
    /// rows an engine report mints are the exception: those are the
    /// worker's own facts, and they journal as they always did.
    native_journal: bool,
    /// Whether this session's inference rides the on-behalf-of relay
    /// (decision 71). The relay's refusals are already legible and name the
    /// gateway, so [`LiveSink::legible_turn_error`] leaves them untouched.
    relay_wired: bool,
    turn_id: std::sync::Mutex<Option<TurnId>>,
    /// The model the current turn asked for, so a failure about the model
    /// can name it.
    turn_model: std::sync::Mutex<Option<String>>,
    /// Resume ref reported during engine startup but not yet proven durable.
    ///
    /// Codex creates a thread before it writes that thread to disk. The first
    /// turn event promotes this candidate into the session row, so a restart
    /// keeps real context without trying to resume an unused thread.
    pending_resume_ref: std::sync::Mutex<Option<String>>,
    /// Where tests point `gh`; `None` outside tests. Snapshotted at attach so
    /// the post-turn fact detector confirms against the same binary every
    /// other gh call in the process resolves (decision 77).
    gh_search_path: Option<String>,
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
    /// Derives the turn's recap once it completes. `None` in headless
    /// deployments and tests that install none, which simply have no recaps.
    recap: Option<Arc<dyn super::recap::TurnRecap>>,
    /// Derives a lucid rewrite of the closing message once the turn
    /// completes. `None` in headless deployments and tests that install none.
    rewrite: Option<Arc<dyn super::rewrite::TurnRewrite>>,
    /// Derives memory proposals once the turn completes.
    memory_capture: Option<Arc<dyn super::memory_capture::TurnMemoryCapture>>,
    /// The runtime's hot pull-request tier (decision 66). A turn whose fact
    /// detector confirms a push or a create marks this workspace, so the
    /// next hot pass reads the head the turn just moved (issue 2799).
    hot_prs: super::pr_refresh::HotPullRequests,
    /// When the engine last reported activity from a turn it started on its
    /// own. That work keeps the child busy the way a turn does, so the idle
    /// park counts from it too (decision 0064).
    last_background_activity: std::sync::Mutex<Option<tokio::time::Instant>>,
}

impl LiveSink {
    /// The principal the session belongs to.
    pub(crate) fn owner(&self) -> &OwnerId {
        &self.owner
    }

    pub(crate) fn set_turn(&self, turn_id: TurnId) {
        *self.turn_id.lock().expect("code sink turn") = Some(turn_id);
    }

    /// Remember the model the current turn asked for.
    pub(crate) fn set_turn_model(&self, model: Option<String>) {
        *self.turn_model.lock().expect("code sink turn model") = model;
    }

    /// Whether the engine did anything on its own within the last `window`.
    fn active_on_its_own_within(&self, window: Duration) -> bool {
        self.last_background_activity
            .lock()
            .expect("code sink background activity")
            .is_some_and(|at| at.elapsed() < window)
    }

    /// How many unrecognized events the engine has counted since the last
    /// flush. Reporting a total below the watermark (an engine that reset its
    /// own counter) yields zero rather than a negative correction.
    fn take_unrecognized_delta(&self, total: u64) -> u64 {
        let flushed = self.flushed_unrecognized.swap(total, Ordering::SeqCst);
        total.saturating_sub(flushed)
    }

    /// The engine's report of a failed turn, made legible and classified.
    ///
    /// Every external engine's failure names that engine, so a reader never
    /// sees "the engine". The adapter's own classification stands; one it
    /// could not make reads as `unknown`, unless the words are a provider
    /// authentication failure (issue 2653). Those become `engine_auth`, and
    /// because the vendor's raw 401 body cannot tell the reader "sign this
    /// harness in" from "the provider is down", the sentence the create-time
    /// refusal uses leads and the engine's own words follow. A turn riding
    /// the relay keeps its message untouched — the relay's refusals already
    /// name the gateway, and a hosted machine has no sign-in to send anyone
    /// to.
    fn legible_turn_error(&self, error: BoundedError) -> BoundedError {
        let reads_as_signed_out = provider_auth_failure(&error.message);
        let mut failure = error
            .failure
            .unwrap_or_else(|| TurnFailure::new(TurnFailureCategory::Unknown));
        if self.harness != HarnessKind::Internal && failure.engine.is_none() {
            failure.engine = Some(self.harness);
        }
        if failure.category == TurnFailureCategory::Unknown && reads_as_signed_out {
            failure.category = TurnFailureCategory::EngineAuth;
        }
        // A failure about the model names the one the turn asked for, when
        // the engine did not name one itself.
        if matches!(
            failure.category,
            TurnFailureCategory::ModelUnavailable | TurnFailureCategory::ContextOverflow
        ) && failure.model.is_none()
        {
            failure.model = self
                .turn_model
                .lock()
                .expect("code sink turn model")
                .clone();
        }
        let message = if self.relay_wired || !reads_as_signed_out {
            error.message
        } else {
            let label = crate::code::harness_label(self.harness);
            format!(
                "{label} is not signed in on this machine. Sign in to {label} from \
                 Settings > Coding engines, then try again. The engine reported: {}",
                error.message
            )
        };
        BoundedError::new(message).with_failure(failure)
    }

    /// Persist a reported resume ref after the engine proves a turn started.
    fn persist_pending_resume_ref<'fut>(&'fut self) -> BoxFuture<'fut, ()> {
        Box::pin(async move {
            let candidate = self
                .pending_resume_ref
                .lock()
                .expect("code sink resume ref")
                .clone();
            let Some(candidate) = candidate else {
                return;
            };
            match set_session_harness_resume_ref(
                &self.db,
                &self.owner,
                self.session_id,
                self.spawn_epoch,
                &candidate,
            )
            .await
            {
                Ok(true) => {
                    let mut pending = self
                        .pending_resume_ref
                        .lock()
                        .expect("code sink resume ref");
                    if pending.as_deref() == Some(candidate.as_str()) {
                        *pending = None;
                    }
                }
                Ok(false) => {
                    // This worker no longer owns a running session. Do not retry
                    // the stale write on every later event.
                    *self
                        .pending_resume_ref
                        .lock()
                        .expect("code sink resume ref") = None;
                }
                Err(error) => warn!(
                    session = %self.session_id,
                    error = %error,
                    "could not persist the code-session resume ref"
                ),
            }
        })
    }

    /// Track subagent spans (decision 52): a top-level `Task` call opens one,
    /// its completion closes it. A terminal parent turn also settles any child
    /// whose own completion was lost, so the rail never claims work survived a
    /// turn that has already ended. Boundaries are rare, so each one persists
    /// the list through the targeted write and restates the session's digest.
    fn note_subagent_boundary<'fut>(&'fut self, event: &'fut Event) -> BoxFuture<'fut, ()> {
        Box::pin(async move {
            let changed = match event {
                Event::ToolStarted {
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
                Event::ToolCompleted {
                    call_id,
                    outcome,
                    detail,
                    ..
                } => {
                    let mut subagents = self.subagents.lock().expect("code sink subagents");
                    match subagents.iter_mut().find(|entry| entry.call_id == *call_id) {
                        Some(entry) if entry.status == CodeSubagentStatus::Running => {
                            entry.status = match outcome {
                                ToolOutcome::Succeeded => CodeSubagentStatus::Done,
                                ToolOutcome::Failed | ToolOutcome::Denied => {
                                    CodeSubagentStatus::Failed
                                }
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
                        Some(_) | None => None,
                    }
                }
                Event::TurnCompleted { .. } | Event::TurnRefused { .. } => {
                    let mut subagents = self.subagents.lock().expect("code sink subagents");
                    settle_running_subagents(&mut subagents, CodeSubagentStatus::Done)
                        .then(|| subagents.clone())
                }
                Event::TurnFailed { .. } | Event::TurnInterrupted { .. } => {
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
        })
    }

    pub(crate) fn record_external_approval<'fut>(
        &'fut self,
        approval_id: ApprovalId,
        harness_ref: &'fut tidebreak_harness::HarnessApprovalRef,
        raw: &'fut serde_json::Value,
    ) -> BoxFuture<'fut, Result<Approval, WorkerError>> {
        Box::pin(async move {
            let Some(capability) = harness_ref.capability.as_ref() else {
                return Err(WorkerError::Failed(
                    "external approval is missing its server capability".into(),
                ));
            };
            if capability.approval_id != approval_id.to_string()
                || capability.owner_id != self.owner.as_str()
                || capability.session_id != self.session_id.to_string()
                || capability.spawn_epoch != self.spawn_epoch
            {
                return Err(WorkerError::Failed(
                    "approval capability does not match its session, row, and worker epoch".into(),
                ));
            }
            self.record_approval(approval_id, harness_ref, raw, None)
                .await
        })
    }

    /// Settle a pending approval the engine decided itself, when one is
    /// still waiting under this call id.
    fn settle_engine_observed<'fut>(
        &'fut self,
        call_id: &'fut str,
        decision: ApprovalDecision,
    ) -> BoxFuture<'fut, ()> {
        Box::pin(async move {
            match settle_engine_observed_approval(
                &self.db,
                &self.owner,
                self.session_id,
                self.spawn_epoch,
                call_id,
                tidebreak_core::ApprovalDecisionKind::from(decision),
                Utc::now(),
            )
            .await
            {
                Ok(Some(settlement)) => {
                    self.bus.publish(self.session_id, settlement.event);
                    let _ = super::attention::note_activity(
                        &self.db,
                        &self.bus,
                        &self.owner,
                        self.session_id,
                    )
                    .await;
                }
                Ok(None) => {}
                Err(error) => warn!(
                    session = %self.session_id,
                    call_id,
                    error = %error,
                    "could not settle an engine-decided approval"
                ),
            }
        })
    }

    fn record_approval<'fut>(
        &'fut self,
        approval_id: ApprovalId,
        harness_ref: &'fut tidebreak_harness::HarnessApprovalRef,
        raw: &'fut serde_json::Value,
        kind: Option<&'fut tidebreak_core::ApprovalKind>,
    ) -> BoxFuture<'fut, Result<Approval, WorkerError>> {
        Box::pin(async move {
            let existing = *self.turn_id.lock().expect("code sink turn");
            let turn_id = match existing {
                Some(id) => id,
                None => match get_open_turn(&self.db, &self.owner, self.session_id).await {
                    Ok(Some(turn)) => turn.id,
                    Ok(None) => {
                        return Err(WorkerError::Failed(format!(
                            "session {} has no open turn for approval {approval_id}",
                            self.session_id
                        )));
                    }
                    Err(err) => return Err(WorkerError::Failed(err.to_string())),
                },
            };
            if let Some(capability) = harness_ref.capability.as_ref() {
                if capability.turn_id != turn_id.to_string() {
                    return Err(WorkerError::Failed(
                        "approval capability does not match its open turn".into(),
                    ));
                }
            }
            let capability = harness_ref.capability.as_ref();
            let approval = Approval {
                actor: None,
                id: approval_id,
                session_id: self.session_id,
                turn_id,
                // An engine that classifies its own request precisely ships the
                // kind on the event; otherwise the server guesses from raw.
                kind: kind.cloned().unwrap_or_else(|| kind_from_raw(raw)),
                harness_raw: persist_harness_raw(&harness_ref.call_id, raw),
                native_call_id: Some(harness_ref.call_id.clone()),
                server_capability: capability.map(|binding| binding.token.clone()),
                request_sha256: capability.map(|binding| binding.request_sha256.clone()),
                worker_epoch: Some(self.spawn_epoch),
                decision_claim: None,
                claimed_at: None,
                state: ApprovalState::Pending,
                feedback: None,
                requested_at: Utc::now(),
                decided_at: None,
                auto_judge_status: None,
            };
            let Some(event) = insert_approval_for_worker(&self.db, &self.owner, &approval)
                .await
                .map_err(|err| WorkerError::Failed(err.to_string()))?
            else {
                return Err(WorkerError::Failed(
                    "the approval worker or turn is no longer active".into(),
                ));
            };
            self.bus.publish(self.session_id, event);
            let _ =
                super::attention::note_activity(&self.db, &self.bus, &self.owner, self.session_id)
                    .await;
            let _ = super::attention::apply_attention(
                &self.db,
                &self.bus,
                &self.owner,
                self.session_id,
                Attention::needs_you("an approval is waiting", AttentionSource::Structured),
                false,
            )
            .await;
            Ok(approval)
        })
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
        if matches!(event, HarnessEvent::BackgroundActivity { .. }) {
            *self
                .last_background_activity
                .lock()
                .expect("code sink background activity") = Some(tokio::time::Instant::now());
        }
        if let HarnessEvent::SessionStarted {
            resume_ref: Some(resume_ref),
            ..
        } = &event
        {
            *self
                .pending_resume_ref
                .lock()
                .expect("code sink resume ref") = Some(resume_ref.clone());
        }
        if matches!(
            &event,
            HarnessEvent::TurnStarted
                | HarnessEvent::AssistantDelta { .. }
                | HarnessEvent::AssistantMessage { .. }
                | HarnessEvent::ReasoningDelta { .. }
                | HarnessEvent::ToolStarted { .. }
                | HarnessEvent::ToolCompleted { .. }
                | HarnessEvent::FileChanged { .. }
                | HarnessEvent::ApprovalRequested { .. }
                | HarnessEvent::UserSteered { .. }
                | HarnessEvent::TurnCompleted { .. }
                | HarnessEvent::TurnInterrupted
        ) {
            self.persist_pending_resume_ref().await;
        }
        if let HarnessEvent::ApprovalRequested {
            harness_ref,
            raw,
            kind,
        } = &event
        {
            if let Err(error) = self
                .record_approval(ApprovalId::new(), harness_ref, raw, kind.as_ref())
                .await
            {
                warn!(
                    session = %self.session_id,
                    call_id = %harness_ref.call_id,
                    error = %error,
                    "could not persist an engine approval request"
                );
            }
            return;
        }
        if let HarnessEvent::ApprovalResolved {
            harness_ref,
            decision,
        } = &event
        {
            // The decision route journals ApprovalResolved after the harness
            // observes the decision, so a decided row is left alone. A row
            // still pending and unclaimed was decided on the engine's own
            // channel — a standing grant, an auto-approval judge — and this
            // report is the only settlement it will get.
            self.settle_engine_observed(&harness_ref.call_id, decision.clone())
                .await;
            return;
        }
        if matches!(event, HarnessEvent::TurnStarted) && self.turn_id.lock().unwrap().is_some() {
            // The worker already journaled TurnStarted for this turn.
            return;
        }
        let turn_id = *self.turn_id.lock().unwrap();
        if let HarnessEvent::UserSteered {
            correlation_uuid: Some(correlation_uuid),
            ..
        } = &event
        {
            let Some(expected_turn_id) = turn_id else {
                return;
            };
            match tidebreak_core::db::code::settle_machine_external_steer_ack(
                &self.db,
                &self.owner,
                self.session_id,
                self.spawn_epoch,
                expected_turn_id,
                *correlation_uuid,
            )
            .await
            {
                Ok(true) => {}
                Ok(false) => return,
                Err(error) => {
                    warn!(
                        session = %self.session_id,
                        %correlation_uuid,
                        %error,
                        "could not persist a correlated machine steering acknowledgment"
                    );
                    return;
                }
            }
        }
        let Some(session_event) = map_event(event, turn_id) else {
            return;
        };
        // An engine that fails a turn can pass the provider's raw 401 body
        // straight through. Swap it for the legible sentence before the
        // journal keeps it.
        let session_event = match session_event {
            Event::TurnFailed { error, .. } => Event::TurnFailed {
                error: self.legible_turn_error(error),
                detail: None,
            },
            other => other,
        };
        // Assistant deltas stream and are gone. The `assistant_message` that
        // closes the run repeats them exactly, so a row here would store the
        // same words a second time (record 57).
        if matches!(session_event, Event::AssistantDelta { .. }) {
            if self.native_journal {
                return;
            }
            self.bus.publish_transient(self.session_id, session_event);
            let _ =
                super::attention::note_activity(&self.db, &self.bus, &self.owner, self.session_id)
                    .await;
            return;
        }
        self.note_subagent_boundary(&session_event).await;
        // An approval is parked on the call this completion names. Reconcile
        // it after the completion lands, so the journal reads in the order it
        // happened. See `approval_sweep`.
        let completed_call = match &session_event {
            Event::ToolCompleted { call_id, .. } => Some(call_id.clone()),
            _ => None,
        };
        // A completed turn is the moment its recap has everything it needs and
        // the reader has most likely stopped watching. Started below rather
        // than here, so a journal write that was dropped never produces a line
        // describing a turn the database does not agree finished.
        let completed_turn = matches!(session_event, Event::TurnCompleted { .. })
            .then_some(turn_id)
            .flatten();
        let notification_turn = matches!(
            &session_event,
            Event::TurnCompleted { .. } | Event::TurnFailed { .. }
        )
        .then_some(turn_id)
        .flatten();
        let write = if let Some(turn_id) = notification_turn {
            persist_turn_and_publish(
                &self.db,
                &self.bus,
                &self.owner,
                self.session_id,
                self.spawn_epoch,
                turn_id,
                session_event,
                self.native_journal,
            )
            .await
        } else {
            persist_and_publish(
                &self.db,
                &self.bus,
                &self.owner,
                self.session_id,
                self.spawn_epoch,
                session_event,
                self.native_journal,
            )
            .await
        };
        let journaled = match write {
            Ok(()) => !self.native_journal,
            Err(JournalError::StaleSpawnEpoch { .. }) => {
                warn!(
                    session = %self.session_id,
                    "dropping event from a superseded code-session worker"
                );
                false
            }
            Err(err) => {
                warn!(session = %self.session_id, error = %err, "failed to journal engine event");
                false
            }
        };
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
        if let (true, Some(turn_id), Some(recap)) = (journaled, completed_turn, self.recap.as_ref())
        {
            recap.spawn(self.owner.clone(), self.session_id, turn_id);
        }
        if let (true, Some(turn_id), Some(rewrite)) =
            (journaled, completed_turn, self.rewrite.as_ref())
        {
            rewrite.spawn(self.owner.clone(), self.session_id, turn_id);
        }
        if let (true, Some(turn_id), Some(capture)) =
            (journaled, completed_turn, self.memory_capture.as_ref())
        {
            capture.spawn(self.owner.clone(), self.session_id, turn_id);
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
    session: Session,
    engine: Box<dyn HarnessSession>,
    sink: Arc<LiveSink>,
    attachments: AttachmentStore,
    worktree_turn: Arc<tokio::sync::Mutex<()>>,
    quiesce: watch::Receiver<bool>,
) -> WorkerHandle {
    let (tx, rx) = mpsc::channel(8);
    let spawn_epoch = session.spawn_epoch;
    let queue = TurnQueue::new(worktree_turn, quiesce);
    let approval_decisions = Arc::new(tokio::sync::Mutex::new(()));
    let task = tokio::spawn(run_worker(
        session,
        engine,
        sink.clone(),
        queue.clone(),
        attachments,
        rx,
    ));
    WorkerHandle {
        spawn_epoch,
        binary: None,
        project_config: tidebreak_harness::ProjectConfig::Skip,
        commands: tx,
        queue,
        sink,
        approval_decisions,
        abort: task.abort_handle(),
    }
}

fn record_child_process(session: &mut Session, pid: Option<i64>) {
    session.child_pid = pid;
    session.child_process_identity = pid.and_then(|pid| {
        tidebreak_harness::spawned_process_identity(pid).or_else(|| {
            tidebreak_harness::current_process_identity(pid)
                .ok()
                .flatten()
        })
    });
}

/// Tell the worker the durable queue changed: a row landed, the pause
/// cleared, or send-now reordered the head. The worker re-reads the queue at
/// its next drain.
pub(crate) fn wake_queue(handle: &WorkerHandle) {
    handle.queue.wake.notify_one();
}

async fn run_worker(
    mut session: Session,
    engine: Box<dyn HarnessSession>,
    sink: Arc<LiveSink>,
    queue: TurnQueue,
    store: AttachmentStore,
    mut commands: mpsc::Receiver<WorkerCommand>,
) {
    if engine.child_pid().is_some() {
        record_child_process(&mut session, engine.child_pid());
        let _ = save_session(&sink.db, &session).await;
    }

    if let Ok(Some(open)) = get_open_turn(&sink.db, &session.owner, session.id).await {
        if matches!(open.status, TurnStatus::Waiting | TurnStatus::Resuming)
            && open.park_ref.is_some()
            && open.park_wait.is_some()
        {
            if let Err(error) = boxed(|| {
                continue_parked_turn(
                    &mut session,
                    engine.as_ref(),
                    &sink,
                    &mut commands,
                    &queue,
                    open,
                )
            })
            .await
            {
                warn!(
                    session = %session.id,
                    error = %error,
                    "could not resume the parked turn after a worker restart"
                );
            }
        } else if session.harness_kind == HarnessKind::Internal
            && open.status == TurnStatus::Running
        {
            let lease_token = uuid::Uuid::new_v4();
            let now = Utc::now();
            let claimed = sink
                .db
                .take_lease_on_turn_with_input_message(
                    tidebreak_core::TurnId(open.id.0),
                    lease_token,
                    now,
                    now + chrono::Duration::seconds(60),
                    &open.user_input,
                )
                .await;
            match claimed {
                Ok(Some(())) => {
                    let input = TurnInput {
                        turn_id: Some(open.id),
                        text: open.user_input.clone(),
                        model: session.model.clone(),
                        reasoning_effort: session.reasoning_effort,
                        fast_mode: session.fast_mode,
                        images: Vec::new(),
                    };
                    if let Err(error) = engine.run_turn(input).await {
                        warn!(
                            session = %session.id,
                            error = %error,
                            "could not resume the running internal turn after a worker restart"
                        );
                    }
                }
                Ok(None) => {
                    warn!(
                        session = %session.id,
                        turn = %open.id,
                        "could not reclaim the running internal turn after a worker restart"
                    );
                }
                Err(error) => {
                    warn!(
                        session = %session.id,
                        error = %error,
                        "could not reclaim the running internal turn after a worker restart"
                    );
                }
            }
        } else if session.harness_kind == HarnessKind::Internal
            && open.status == TurnStatus::RetryWait
        {
            // The turn was waiting on the server's own retry when the worker
            // stopped. The engine waits out what is left of it and takes the
            // next attempt, so the journaled `turn_retrying` gets its outcome.
            let input = TurnInput {
                turn_id: Some(open.id),
                text: open.user_input.clone(),
                model: session.model.clone(),
                reasoning_effort: session.reasoning_effort,
                fast_mode: session.fast_mode,
                images: Vec::new(),
            };
            if let Err(error) = engine.run_turn(input).await {
                warn!(
                    session = %session.id,
                    error = %error,
                    "could not retry the waiting internal turn after a worker restart"
                );
            }
        }
    }

    let mut quiesce = queue.quiesce.clone();
    // A dropped quiesce sender (tests, teardown) must not hot-loop the select.
    let mut quiesce_live = true;
    loop {
        if session_was_ended(&sink.db, &mut session).await {
            break;
        }
        let quiescing = *quiesce.borrow_and_update();
        if quiescing {
            // A restart-to-update is waiting on this session: hold the drain
            // and release the engine child now rather than on the idle timer.
            // Rows in the durable queue stay put and drain after the relaunch.
            if engine.child_pid().is_some() {
                park_idle_engine(&mut session, engine.as_ref(), &sink).await;
            }
        } else {
            boxed(|| {
                drain_queued(
                    &mut session,
                    engine.as_ref(),
                    &sink,
                    &queue,
                    &store,
                    &mut commands,
                )
            })
            .await;
        }
        tokio::select! {
            _ = queue.wake.notified() => {}
            changed = quiesce.changed(), if quiesce_live => {
                if changed.is_err() {
                    quiesce_live = false;
                }
            }
            command = commands.recv() => match command {
                Some(WorkerCommand::RunTurn {
                    message,
                    attachments,
                    actor,
                    trigger_delivery,
                    reply,
                }) => {
                    // The turn answers the sender itself once it is accepted.
                    // Whatever is still here afterwards is a turn that never
                    // started, and the sender hears why.
                    let mut reply = Some(reply);
                    let result = drive_turn(
                        &mut session,
                        engine.as_ref(),
                        &sink,
                        &store,
                        &mut commands,
                        WorktreeTurn {
                            lock: &queue.worktree,
                            wait: TurnWait::Send,
                            quiesce: &queue.quiesce,
                        },
                        QueuedFollowUp {
                            message,
                            attachments,
                            actor,
                            trigger_delivery,
                            queued_row: None,
                            accepted: Some(&mut reply),
                        },
                    )
                    .await;
                    if let Some(reply) = reply {
                        let _ = reply.send(result);
                    }
                }
                Some(WorkerCommand::SetPermissionMode {
                    mode,
                    settlement,
                    reply,
                }) => {
                    let result = set_permission_mode(engine.as_ref(), mode).await;
                    let accepted = result.is_ok();
                    let _ = reply.send(result);
                    if accepted {
                        // Stay inside this command until the matching database
                        // intent commits. A turn sent during confirmation can
                        // queue on the channel, but it cannot reach the engine.
                        if await_permission_mode_settlement(settlement, &mut commands).await {
                            session.permission_mode = mode;
                        } else {
                            break;
                        }
                    }
                }
                Some(WorkerCommand::SetExecutionSettings {
                    settings,
                    settlement,
                    reply,
                }) => {
                    if !reserve_execution_settings(
                        &mut session,
                        settings,
                        settlement,
                        reply,
                    )
                    .await
                    {
                        break;
                    }
                }
                Some(command) => {
                    if apply_control(engine.as_ref(), command, None, None).await
                        == ControlFlow::Shutdown
                    {
                        break;
                    }
                }
                None => break,
            },
            // An idle engine child is cache, not an invariant (decision 0064).
            // The timer only exists while the engine reports a live child, so
            // a parked session — or an engine with no between-turn child —
            // arms nothing. During an update quiesce the timer is only the
            // retry for a park that failed above, so it comes back fast.
            _ = tokio::time::sleep(if quiescing { QUIESCE_PARK_RETRY } else { PARK_AFTER_IDLE }), if engine.child_pid().is_some() => {
                // Work the engine did on its own, such as the turn Claude
                // Code runs when a background task ends, is activity too:
                // the idle window starts again after it.
                if !quiescing && sink.active_on_its_own_within(PARK_AFTER_IDLE) {
                    continue;
                }
                park_idle_engine(&mut session, engine.as_ref(), &sink).await;
            }
        }
    }
    let _ = engine.shutdown().await;
}

async fn await_permission_mode_settlement(
    settlement: oneshot::Receiver<PermissionModeSettlement>,
    commands: &mut mpsc::Receiver<WorkerCommand>,
) -> bool {
    match settlement.await {
        Ok(PermissionModeSettlement::Confirmed) => true,
        Ok(PermissionModeSettlement::Abort) | Err(_) => {
            reject_pending_commands_after_permission_mode_abort(commands).await;
            false
        }
    }
}

async fn reject_pending_commands_after_permission_mode_abort(
    commands: &mut mpsc::Receiver<WorkerCommand>,
) {
    commands.close();
    while let Some(command) = commands.recv().await {
        let turn_error = || {
            WorkerError::Conflict(
                "the turn was not accepted because the permission mode change did not commit"
                    .into(),
            )
        };
        let command_error = || {
            WorkerError::Conflict(
                "the command was not accepted because the permission mode change did not commit"
                    .into(),
            )
        };
        match command {
            WorkerCommand::RunTurn { reply, .. } => {
                let _ = reply.send(Err(turn_error()));
            }
            WorkerCommand::SetPermissionMode { reply, .. }
            | WorkerCommand::SetExecutionSettings { reply, .. }
            | WorkerCommand::Decide { reply, .. }
            | WorkerCommand::Interrupt { reply }
            | WorkerCommand::Steer { reply, .. } => {
                let _ = reply.send(Err(command_error()));
            }
            WorkerCommand::Shutdown => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlFlow {
    Continue,
    Shutdown,
}

#[cfg(not(any(test, feature = "test-support")))]
const STEER_CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(any(test, feature = "test-support"))]
const STEER_CONTROL_TIMEOUT: Duration = Duration::from_millis(100);

/// How often a parked turn rereads its park when nothing wakes it.
///
/// A settlement wakes the worker itself: it publishes on the session's live
/// channel, and a child session that stops running wakes its parent through
/// [`CodeEventBus::wake_parked`]. This poll only catches a settlement that
/// reaches neither, such as one made by another process.
const PARK_SAFETY_POLL: Duration = Duration::from_secs(5);

#[cfg(not(any(test, feature = "test-support")))]
const APPROVAL_CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(any(test, feature = "test-support"))]
const APPROVAL_CONTROL_TIMEOUT: Duration = Duration::from_secs(1);

/// How long a session sits between turns — no turn, no queued follow-up, no
/// command — before its engine child is released (decision 0064). Sits well
/// past the documented provider prompt-cache TTL, so a wake in practice pays
/// spawn latency and not tokens a warm child would have saved.
#[cfg(not(any(test, feature = "test-support")))]
const PARK_AFTER_IDLE: Duration = Duration::from_secs(15 * 60);
#[cfg(any(test, feature = "test-support"))]
const PARK_AFTER_IDLE: Duration = Duration::from_millis(150);

/// Retry cadence for a park that failed while an update quiesce is waiting
/// on this session. Outside a quiesce a failed park just waits for the next
/// idle window.
#[cfg(not(any(test, feature = "test-support")))]
const QUIESCE_PARK_RETRY: Duration = Duration::from_millis(500);
#[cfg(any(test, feature = "test-support"))]
const QUIESCE_PARK_RETRY: Duration = Duration::from_millis(50);

/// Release an idle engine child (decision 0064) and clear the row's pid so
/// nothing reads the dead process as live. The next turn respawns and
/// resumes. A park that fails keeps the child and simply retries on the next
/// idle window.
///
/// An engine busy with a turn of its own between the person's turns — Claude
/// Code runs one when a background task ends — keeps its child the same way.
/// The row keeps its pid, so an update quiesce waits for that turn as it
/// waits for any running turn (decision 0080).
async fn park_idle_engine(session: &mut Session, engine: &dyn HarnessSession, sink: &LiveSink) {
    match engine.park().await {
        Ok(()) => {}
        Err(HarnessError::EngineBusy(detail)) => {
            tracing::debug!(
                session = %session.id,
                %detail,
                "left the engine child running until its own turn ends"
            );
            return;
        }
        Err(error) => {
            warn!(
                session = %session.id,
                error = %error,
                "could not park the idle engine child"
            );
            return;
        }
    }
    tracing::debug!(session = %session.id, "parked the idle engine child");
    if session.child_pid.take().is_some() {
        session.child_process_identity = None;
        let _ = save_session(&sink.db, session).await;
    }
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

/// How often a running internal turn's lease is extended. The turn takes a
/// one-minute lease as it starts, so the first extension waits one period.
const TURN_LEASE_HEARTBEAT: Duration = Duration::from_secs(15);

/// Store writes the turn loop makes while a leg runs, on tasks beside the
/// engine rather than in front of it.
///
/// The loop never awaits the store inside its `select!`. The engine may hold
/// the store's only writer connection there, or be first in line for it, and
/// its write only finishes once the loop polls the engine again. A write the
/// loop awaited would wait on the engine, and the engine on the loop, until
/// the pool gave up 30 seconds later, and every other write in the process
/// would wait behind them.
#[derive(Default)]
struct SideWrites {
    /// The newest child-process write. Each waits for the one before it, so
    /// the row ends on the newest pid.
    child_process: Option<tokio::task::JoinHandle<()>>,
    /// The lease extension in flight. One still waiting for the writer
    /// covers the next tick too.
    heartbeat: Option<tokio::task::JoinHandle<()>>,
}

impl SideWrites {
    /// Record `session`'s current child process on its row.
    fn child_process(&mut self, db: &Arc<DbStore>, session: &Session) {
        let previous = self.child_process.take();
        let db = db.clone();
        let owner = session.owner.clone();
        let session_id = session.id;
        let spawn_epoch = session.spawn_epoch;
        let pid = session.child_pid;
        let identity = session.child_process_identity.clone();
        self.child_process = Some(tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let _ = set_session_child_process(&db, &owner, session_id, spawn_epoch, pid, identity)
                .await;
        }));
    }

    /// Extend a running internal turn's lease by another minute.
    fn heartbeat(&mut self, db: &Arc<DbStore>, turn_id: TurnId, lease_token: uuid::Uuid) {
        if self
            .heartbeat
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return;
        }
        let db = db.clone();
        self.heartbeat = Some(tokio::spawn(async move {
            let now = Utc::now();
            let _ = db
                .heartbeat_turn_lease(
                    tidebreak_core::TurnId(turn_id.0),
                    lease_token,
                    now,
                    now + chrono::Duration::seconds(60),
                )
                .await;
        }));
    }

    /// Wait for every write to land.
    ///
    /// Call it once the legs and the control calls have finished. The tasks
    /// run on their own, but they queue for the same writer, so a future on
    /// this task that still held it would keep them waiting.
    async fn settle(&mut self) {
        for task in [self.child_process.take(), self.heartbeat.take()]
            .into_iter()
            .flatten()
        {
            let _ = task.await;
        }
    }
}

/// How a wait for the workspace's turn lock ended.
///
/// The wait keeps answering control commands: a queued turn can sit on a
/// sibling's turn for minutes, and parking on a bare `lock()` would leave the
/// worker deaf for that whole stretch — an interrupt sent while the message
/// was still queued would reach the engine only once the turn had started,
/// which reads as the stop being ignored.
enum WorktreeWait<'a> {
    /// The checkout is ours; the turn starts.
    Acquired(tokio::sync::MutexGuard<'a, ()>),
    /// A stop arrived first. The turn must not start, and the caller owes the
    /// reader an account of what happens to the queue it came from.
    Stopped,
    /// The worker is going away; leave everything exactly as it is.
    Shutdown,
}

async fn await_worktree_turn<'a>(
    session: &mut Session,
    engine: &dyn HarnessSession,
    worktree_turn: &'a tokio::sync::Mutex<()>,
    commands: &mut mpsc::Receiver<WorkerCommand>,
) -> WorktreeWait<'a> {
    loop {
        tokio::select! {
            guard = worktree_turn.lock() => return WorktreeWait::Acquired(guard),
            command = commands.recv() => match command {
                // Stopping something that has not started needs no engine
                // call: declining to start is the stop. A queued row is not
                // consumed by this — it stays in the durable queue.
                Some(WorkerCommand::Interrupt { reply }) => {
                    let _ = reply.send(Ok(()));
                    return WorktreeWait::Stopped;
                }
                Some(WorkerCommand::Shutdown) | None => return WorktreeWait::Shutdown,
                Some(WorkerCommand::SetExecutionSettings {
                    settings,
                    settlement,
                    reply,
                }) => {
                    if !reserve_execution_settings(session, settings, settlement, reply).await {
                        return WorktreeWait::Shutdown;
                    }
                }
                // There is no turn yet, so steering has nothing to steer, a
                // second RunTurn is a conflict, and a permission-mode switch
                // belongs to whichever loop owns the session row.
                // `apply_control` answers all three that way already.
                Some(command) => {
                    if apply_control(engine, command, None, None).await == ControlFlow::Shutdown {
                        return WorktreeWait::Shutdown;
                    }
                }
            },
        }
    }
}

/// Hold the worker's session copy until the matching targeted write settles.
///
/// A queued turn can wait on a sibling's worktree turn while the session is
/// otherwise idle. The worker must accept settings reservations during that
/// wait, or the live route cannot use the same reserve-commit-release boundary
/// that protects ordinary idle updates.
async fn reserve_execution_settings(
    session: &mut Session,
    settings: SessionExecutionSettings,
    settlement: oneshot::Receiver<ExecutionSettingsSettlement>,
    reply: oneshot::Sender<Result<(), WorkerError>>,
) -> bool {
    let _ = reply.send(Ok(()));
    match settlement.await {
        Ok(ExecutionSettingsSettlement::Confirmed) => {
            session.model = settings.model;
            session.reasoning_effort = settings.reasoning_effort;
            session.fast_mode = settings.fast_mode;
            true
        }
        Ok(ExecutionSettingsSettlement::Abort) => true,
        Err(_) => false,
    }
}

/// Decisions this turn already handed the engine, keyed by the engine's own
/// call id.
///
/// A `Decide` can arrive after `ApprovalRequested` is published and before
/// the engine returns `Parked`. That command is admitted on the running
/// leg, so no second `Decide` follows. The park wait has to resume from
/// this record or the turn stays `waiting`.
type DeliveredDecisions =
    Arc<std::sync::Mutex<std::collections::HashMap<String, ApprovalDecision>>>;

fn resume_if_already_delivered(
    wait: &tidebreak_core::TurnParkWait,
    delivered: &DeliveredDecisions,
) -> Option<tidebreak_harness::ResumeInput> {
    let tidebreak_core::TurnParkWait::Approval { call_id } = wait else {
        return None;
    };
    let decision = delivered
        .lock()
        .expect("delivered decisions")
        .get(call_id)
        .cloned()?;
    Some(tidebreak_harness::ResumeInput::ApprovalDecided {
        call_id: call_id.clone(),
        decision,
    })
}

/// Deliver one approval decision over the engine's native channel, with the
/// same timeout and ambiguity classification wherever the decision is taken
/// from — the concurrent control path or a parked turn's wait.
async fn deliver_decision(
    engine: &dyn HarnessSession,
    approval: HarnessApprovalRef,
    decision: ApprovalDecision,
    delivered: Option<DeliveredDecisions>,
) -> Result<(), WorkerError> {
    let call_id = approval.call_id.clone();
    let recorded = decision.clone();
    match tokio::time::timeout(APPROVAL_CONTROL_TIMEOUT, engine.decide(approval, decision)).await {
        Ok(Ok(())) => {
            if let Some(delivered) = delivered {
                delivered
                    .lock()
                    .expect("delivered decisions")
                    .insert(call_id, recorded);
            }
            Ok(())
        }
        Ok(Err(HarnessError::ApprovalAcknowledgementLost(message))) => {
            Err(WorkerError::ApprovalDeliveryUnknown(message))
        }
        Ok(Err(err)) => Err(WorkerError::ApprovalDeliveryFailed(err.to_string())),
        Err(_) => Err(WorkerError::ApprovalDeliveryUnknown(
            "the native approval decision timed out; delivery could not be confirmed".into(),
        )),
    }
}

/// Persist a parked turn: status, the engine's checkpoint token, and the
/// awaited dependency, mapped into the durable park-wait shape.
fn persist_turn_park<'fut>(
    db: &'fut DbStore,
    session: &'fut Session,
    turn: &'fut mut Turn,
    park_ref: &'fut str,
    waiting_on: &'fut tidebreak_harness::ParkWait,
) -> BoxFuture<'fut, Result<tidebreak_core::TurnParkWait, String>> {
    Box::pin(async move {
        let wait = match waiting_on {
            tidebreak_harness::ParkWait::Approval { call_id } => {
                tidebreak_core::TurnParkWait::Approval {
                    call_id: call_id.clone(),
                }
            }
            tidebreak_harness::ParkWait::ClientToolCall { call_id } => {
                tidebreak_core::TurnParkWait::ClientToolCall {
                    call_id: call_id.clone(),
                }
            }
            tidebreak_harness::ParkWait::AgentRuns { run_ids } => {
                tidebreak_core::TurnParkWait::AgentRuns {
                    run_ids: run_ids.clone(),
                }
            }
            tidebreak_harness::ParkWait::ChildSessions { session_ids } => {
                tidebreak_core::TurnParkWait::ChildSessions {
                    session_ids: session_ids.clone(),
                }
            }
        };
        let status = store_turn_park(db, &session.owner, turn.id, park_ref, &wait)
            .await
            .map_err(|err| format!("persisting the parked turn failed: {err}"))?
            .ok_or_else(|| format!("persisting the parked turn {} lost its row", turn.id))?;
        turn.status = status;
        turn.park_ref = Some(park_ref.to_owned());
        turn.park_wait = Some(wait.clone());
        Ok(wait)
    })
}

fn clear_persisted_turn_park<'fut>(
    db: &'fut DbStore,
    session: &'fut Session,
    turn: &'fut mut Turn,
    park_ref: &'fut str,
    wait: &'fut tidebreak_core::TurnParkWait,
) -> BoxFuture<'fut, Result<(), String>> {
    Box::pin(async move {
        clear_turn_park(db, &session.owner, turn.id, park_ref, wait)
            .await
            .map_err(|err| format!("clearing the parked turn failed: {err}"))?
            .ok_or_else(|| format!("clearing the parked turn {} lost its row", turn.id))?;
        // The worker is about to start the resumed leg. Do not restore the
        // transient `waiting` or `resuming` database status in its live snapshot.
        turn.status = TurnStatus::Running;
        turn.park_ref = None;
        turn.park_wait = None;
        Ok(())
    })
}

/// Hold a parked turn until its wait resolves, the worker is interrupted, or
/// the command channel closes.
///
/// A decision for the awaited approval is delivered to the engine first —
/// the same native channel and classification as the concurrent control path
/// — and only a confirmed delivery resumes the turn. Every other command
/// takes the ordinary control path.
/// Follow an accepted plan with the permission-mode change it proposed.
///
/// The plan approval names the mode the engine would continue under; the
/// decision alone leaves the posture untouched (decision 0048 step 5). So
/// before the resume, the worker moves the engine onto that mode and
/// persists it on the session row. A refusal from the engine is journaled
/// and the turn still resumes: the plan was accepted, and the posture the
/// user sees is the one the row keeps.
fn apply_accepted_plan_mode<'fut>(
    db: &'fut DbStore,
    bus: &'fut CodeEventBus,
    session: &'fut mut Session,
    engine: &'fut dyn HarnessSession,
    input: &'fut tidebreak_harness::ResumeInput,
) -> BoxFuture<'fut, ()> {
    Box::pin(async move {
        let tidebreak_harness::ResumeInput::ApprovalDecided {
            call_id,
            decision: ApprovalDecision::PlanDecision { approve: true, .. },
        } = input
        else {
            return;
        };
        let proposed = match list_approvals(db, &session.owner, None, Some(session.id)).await {
            Ok(approvals) => approvals.into_iter().find_map(|approval| {
                match (&approval.native_call_id, &approval.kind) {
                    (Some(native), ApprovalKind::Plan { proposed_mode }) if native == call_id => {
                        Some(*proposed_mode)
                    }
                    _ => None,
                }
            }),
            Err(error) => {
                warn!(session = %session.id, error = %error, "could not read the accepted plan");
                None
            }
        };
        let Some(mode) = proposed else {
            return;
        };
        if mode == session.permission_mode {
            return;
        }
        // The row's own intent protocol: reserve, re-posture the engine, then
        // confirm — so a route-level change racing this one loses cleanly.
        let intent = match begin_permission_mode_change(db, &session.owner, session, mode).await {
            Ok(Some(intent)) => intent,
            Ok(None) => {
                warn!(session = %session.id, "the plan's permission mode change lost to a concurrent one");
                return;
            }
            Err(error) => {
                warn!(session = %session.id, error = %error, "could not reserve the plan's permission mode");
                return;
            }
        };
        match engine.set_permission_mode(mode).await {
            Ok(()) => {
                match confirm_permission_mode_change(db, &session.owner, &intent).await {
                    Ok(true) => session.permission_mode = mode,
                    Ok(false) => {
                        warn!(session = %session.id, "the plan's permission mode change was not confirmed");
                        return;
                    }
                    Err(error) => {
                        warn!(session = %session.id, error = %error, "could not confirm the plan's permission mode");
                        return;
                    }
                }
                if let Err(error) = super::attention::persist_session(db, bus, session).await {
                    warn!(session = %session.id, error = %error, "could not publish the plan's permission mode");
                }
                let _ = persist_and_publish(
                    db,
                    bus,
                    &session.owner,
                    session.id,
                    session.spawn_epoch,
                    Event::HarnessNotice {
                        level: HarnessNoticeLevel::Info,
                        message: format!(
                            "Plan accepted; the session continues in {} mode.",
                            mode.as_str()
                        ),
                    },
                    false,
                )
                .await;
            }
            Err(error) => {
                let _ = cancel_permission_mode_change(db, &session.owner, &intent).await;
                let _ = persist_and_publish(
                    db,
                    bus,
                    &session.owner,
                    session.id,
                    session.spawn_epoch,
                    Event::HarnessNotice {
                        level: HarnessNoticeLevel::Warning,
                        message: format!(
                            "Plan accepted, but the engine kept its {} posture: {error}",
                            session.permission_mode.as_str()
                        ),
                    },
                    false,
                )
                .await;
            }
        }
    })
}

#[allow(clippy::too_many_arguments)]
/// How a park's wait was resolved.
struct ParkResolution {
    input: tidebreak_harness::ResumeInput,
    /// The decision reached the engine through this worker's `Decide`, so
    /// the worker still owes the engine the decision's side effects (an
    /// accepted plan's posture). A row settled by another surface — the
    /// chat routes, a sweep — already carried them out.
    delivered_here: bool,
}

enum DurableParkState {
    Pending,
    Resolved(ParkResolution),
    Closed,
}

/// The engine-channel decision a settled row's resolution carries.
fn resume_decision(decision: tidebreak_core::ApprovalDecisionKind) -> ApprovalDecision {
    use tidebreak_core::ApprovalDecisionKind as Kind;
    match decision {
        Kind::Approve => ApprovalDecision::Approve,
        Kind::Deny { feedback } => ApprovalDecision::Deny { feedback },
        Kind::Abandoned => ApprovalDecision::Deny { feedback: None },
        Kind::ApprovedWithGrant { scope } => ApprovalDecision::ApproveWithGrant { scope },
        Kind::Answered { answers } => ApprovalDecision::Answers { answers },
        Kind::PlanDecided { approve, feedback } => {
            ApprovalDecision::PlanDecision { approve, feedback }
        }
    }
}

/// The resume a row settled by another surface hands the park, when the
/// row parked on `call_id` is no longer pending.
///
/// An engine with durable parks keeps the park on the row, and any surface
/// may settle it — the chat routes answer a questions card or decide a plan
/// on the same row the session route would. The decision itself is read
/// from the row's resolution in the journal; a row settled without one
/// (recovery, an older build) resumes from its state alone.
async fn resume_from_settled_row(
    db: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    call_id: &str,
) -> Result<Option<tidebreak_harness::ResumeInput>, WorkerError> {
    let approval = list_approvals(db, owner, None, Some(session_id))
        .await
        .map_err(|error| WorkerError::Failed(error.to_string()))?
        .into_iter()
        .find(|approval| approval.native_call_id.as_deref() == Some(call_id));
    let Some(approval) = approval else {
        return Ok(None);
    };
    if approval.state.is_pending() {
        return Ok(None);
    }
    let journaled = tidebreak_core::db::code::list_recent_events(db, owner, session_id, 256)
        .await
        .map_err(|error| WorkerError::Failed(error.to_string()))?
        .into_iter()
        .find_map(|row| match row.event {
            Event::ApprovalResolved {
                approval_id,
                decision,
                ..
            } if approval_id == approval.id => Some(decision),
            _ => None,
        });
    let decision = match journaled {
        Some(decision) => decision,
        None => match approval.state {
            ApprovalState::Approved => tidebreak_core::ApprovalDecisionKind::Approve,
            ApprovalState::Denied => tidebreak_core::ApprovalDecisionKind::Deny {
                feedback: approval.feedback,
            },
            ApprovalState::Abandoned | ApprovalState::Pending => {
                tidebreak_core::ApprovalDecisionKind::Abandoned
            }
        },
    };
    Ok(Some(tidebreak_harness::ResumeInput::ApprovalDecided {
        call_id: call_id.to_owned(),
        decision: resume_decision(decision),
    }))
}

async fn durable_park_state(
    db: &DbStore,
    session: &Session,
    park_ref: &str,
    wait: &tidebreak_core::TurnParkWait,
    turn_id: TurnId,
    delivered: &DeliveredDecisions,
) -> Result<DurableParkState, WorkerError> {
    if let Some(input) = resume_if_already_delivered(wait, delivered) {
        return Ok(DurableParkState::Resolved(ParkResolution {
            input,
            delivered_here: true,
        }));
    }
    let turn = tidebreak_core::db::code::get_turn(db, &session.owner, turn_id)
        .await
        .map_err(|error| WorkerError::Failed(error.to_string()))?
        .ok_or_else(|| WorkerError::Failed(format!("parked turn {turn_id} disappeared")))?;
    if !turn.status.is_open() {
        return Ok(DurableParkState::Closed);
    }
    if turn.park_ref.as_deref() != Some(park_ref) || turn.park_wait.as_ref() != Some(wait) {
        return Err(WorkerError::Failed(format!(
            "turn {turn_id} changed its durable park while the worker waited"
        )));
    }
    let input = match wait {
        tidebreak_core::TurnParkWait::Approval { call_id } => {
            resume_from_settled_row(db, &session.owner, session.id, call_id).await?
        }
        tidebreak_core::TurnParkWait::ClientToolCall { call_id }
            if turn.status == TurnStatus::Resuming =>
        {
            Some(tidebreak_harness::ResumeInput::ClientToolCompleted {
                call_id: call_id.clone(),
            })
        }
        tidebreak_core::TurnParkWait::AgentRuns { run_ids }
            if turn.status == TurnStatus::Resuming =>
        {
            Some(tidebreak_harness::ResumeInput::AgentRunsSettled {
                run_ids: run_ids.clone(),
            })
        }
        tidebreak_core::TurnParkWait::ChildSessions { session_ids }
            if turn.status == TurnStatus::Resuming =>
        {
            Some(tidebreak_harness::ResumeInput::ChildSessionsSettled {
                session_ids: session_ids.clone(),
            })
        }
        // Still parked: read the children and settle the wait when the last
        // of them is done. The children's own rows are the durable truth, so
        // a restarted worker settles the same park from the same evidence.
        tidebreak_core::TurnParkWait::ChildSessions { session_ids } => {
            settle_child_sessions_park(db, session, park_ref, session_ids).await?
        }
        tidebreak_core::TurnParkWait::ClientToolCall { .. }
        | tidebreak_core::TurnParkWait::AgentRuns { .. } => None,
    };
    if let Some(input) = input {
        return Ok(DurableParkState::Resolved(ParkResolution {
            input,
            delivered_here: false,
        }));
    }
    if matches!(
        turn.status,
        TurnStatus::Waiting | TurnStatus::CancellingClient
    ) {
        Ok(DurableParkState::Pending)
    } else {
        // A legacy or damaged wait cannot resume safely. Close it through the
        // normal turn path so one bad row cannot keep the session worker live.
        Ok(DurableParkState::Closed)
    }
}

/// Settle a parked child-session wait once every child it named is done.
///
/// Returns the resume input when this call (or an earlier one) completed the
/// wait, and `None` while a child is still running. A malformed park resolves
/// to nothing here and is closed by the caller's ordinary fallback rather
/// than holding the worker open.
async fn settle_child_sessions_park(
    db: &DbStore,
    session: &Session,
    park_ref: &str,
    session_ids: &[String],
) -> Result<Option<tidebreak_harness::ResumeInput>, WorkerError> {
    let Ok(call_id) = park_ref.parse::<tidebreak_core::CallId>() else {
        return Ok(None);
    };
    let mut children = Vec::with_capacity(session_ids.len());
    for raw in session_ids {
        let Ok(id) = raw.parse::<SessionId>() else {
            return Ok(None);
        };
        children.push(id);
    }
    let result = super::session_tools::settled_child_wait_result(db, &session.owner, &children)
        .await
        .map_err(|error| WorkerError::Failed(error.message().to_owned()))?;
    let Some(result) = result else {
        return Ok(None);
    };
    let settled = tidebreak_core::Store::settle_child_session_wait(
        db,
        session.id,
        call_id,
        &result,
        chrono::Utc::now(),
    )
    .await
    .map_err(|error| WorkerError::Failed(error.to_string()))?;
    match settled {
        SettleChildSessionWaitOutcome::Settled { .. }
        | SettleChildSessionWaitOutcome::Existing(_) => {
            Ok(Some(tidebreak_harness::ResumeInput::ChildSessionsSettled {
                session_ids: session_ids.to_vec(),
            }))
        }
        SettleChildSessionWaitOutcome::ResultConflict
        | SettleChildSessionWaitOutcome::Unavailable => Ok(None),
    }
}

#[allow(clippy::too_many_arguments)]
fn await_park_resolution<'fut, 'a: 'fut>(
    engine: &'a dyn HarnessSession,
    db: &'fut DbStore,
    bus: &'fut CodeEventBus,
    session: &'fut Session,
    commands: &'fut mut mpsc::Receiver<WorkerCommand>,
    controls: &'fut mut FuturesUnordered<BoxFuture<'a, ControlFlow>>,
    interrupted: &'fut mut bool,
    commands_closed: &'fut mut bool,
    park_ref: &'fut str,
    wait: &'fut tidebreak_core::TurnParkWait,
    turn_id: TurnId,
    delivered: &'fut DeliveredDecisions,
) -> BoxFuture<'fut, Result<Option<ParkResolution>, WorkerError>> {
    Box::pin(async move {
        // Subscribe before the read below, so a settlement between the two
        // cannot slip past both.
        let (mut live, _tail) = bus.attach(session.id);
        let park_wake = bus.park_wake(session.id);
        match durable_park_state(db, session, park_ref, wait, turn_id, delivered).await? {
            DurableParkState::Pending => {}
            DurableParkState::Resolved(resolution) => return Ok(Some(resolution)),
            DurableParkState::Closed => return Ok(None),
        }
        let mut safety_poll = tokio::time::interval(PARK_SAFETY_POLL);
        safety_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        safety_poll.tick().await;
        loop {
            tokio::select! {
                biased;
                Some(flow) = controls.next(), if !controls.is_empty() => {
                    if flow == ControlFlow::Shutdown {
                        *interrupted = true;
                        return Ok(None);
                    }
                    match durable_park_state(db, session, park_ref, wait, turn_id, delivered).await? {
                        DurableParkState::Pending => {}
                        DurableParkState::Resolved(resolution) => return Ok(Some(resolution)),
                        DurableParkState::Closed => return Ok(None),
                    }
                }
                published = live.recv() => {
                    match published {
                        Ok(CodeLiveEvent { .. })
                        | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            return Err(WorkerError::Failed(
                                "the live event channel closed while a turn was parked".into(),
                            ));
                        }
                    }
                    match durable_park_state(db, session, park_ref, wait, turn_id, delivered).await? {
                        DurableParkState::Pending => {}
                        DurableParkState::Resolved(resolution) => return Ok(Some(resolution)),
                        DurableParkState::Closed => return Ok(None),
                    }
                }
                () = park_wake.notified() => {
                    match durable_park_state(db, session, park_ref, wait, turn_id, delivered).await? {
                        DurableParkState::Pending => {}
                        DurableParkState::Resolved(resolution) => return Ok(Some(resolution)),
                        DurableParkState::Closed => return Ok(None),
                    }
                }
                _ = safety_poll.tick() => {
                    match durable_park_state(db, session, park_ref, wait, turn_id, delivered).await? {
                        DurableParkState::Pending => {}
                        DurableParkState::Resolved(resolution) => return Ok(Some(resolution)),
                        DurableParkState::Closed => return Ok(None),
                    }
                }
                command = commands.recv(), if !*commands_closed => match command {
                    Some(WorkerCommand::Decide { approval, decision, reply }) => {
                        let call_id = approval.call_id.clone();
                        let decided = (*decision).clone();
                        let result =
                            deliver_decision(engine, approval, *decision, Some(delivered.clone())).await;
                        let delivered = result.is_ok();
                        let _ = reply.send(result);
                        let awaited = matches!(
                            wait,
                            tidebreak_core::TurnParkWait::Approval { call_id: waited }
                                if *waited == call_id
                        );
                        if delivered && awaited {
                            return Ok(Some(ParkResolution {
                                input: tidebreak_harness::ResumeInput::ApprovalDecided {
                                    call_id,
                                    decision: decided,
                                },
                                delivered_here: true,
                            }));
                        }
                    }
                    Some(WorkerCommand::Interrupt { reply }) => {
                        *interrupted = true;
                        let result = engine
                            .interrupt()
                            .await
                            .map_err(|err| WorkerError::Failed(err.to_string()));
                        let _ = reply.send(result);
                        return Ok(None);
                    }
                    Some(WorkerCommand::Shutdown) => {
                        *interrupted = true;
                        return Ok(None);
                    }
                    Some(other) => {
                        controls.push(Box::pin(apply_control(
                            engine,
                            other,
                            Some(turn_id),
                            Some(delivered.clone()),
                        )));
                    }
                    None => {
                        *commands_closed = true;
                        *interrupted = true;
                        return Ok(None);
                    }
                },
            }
        }
    })
}

async fn apply_control(
    engine: &dyn HarnessSession,
    command: WorkerCommand,
    active_turn_id: Option<TurnId>,
    delivered: Option<DeliveredDecisions>,
) -> ControlFlow {
    match command {
        WorkerCommand::Decide {
            approval,
            decision,
            reply,
        } => {
            let result = deliver_decision(engine, approval, *decision, delivered).await;
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
            admission,
            reply,
        } => {
            let admission = admission.as_ref();
            let mut result = match active_turn_id {
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
                    engine.steer_with_correlation(message, admission.and_then(|a| a.correlation_uuid)),
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
                    Err(_) => {
                        let detail = "the engine did not acknowledge steering before the control timeout".to_owned();
                        Err(if admission.is_some() {
                            WorkerError::Failed(detail)
                        } else {
                            WorkerError::SteeringRejected(detail)
                        })
                    },
                },
            };
            if let Some(admission) = admission {
                use tidebreak_core::code::{ExternalSteerAdmission, ExternalSteerQueuedReason};
                let settlement = match &result {
                    Ok(()) => Some((ExternalSteerAdmission::Steered, None)),
                    Err(WorkerError::NoActiveTurn(_) | WorkerError::StaleTurn(_)) => Some((
                        ExternalSteerAdmission::Queued,
                        Some(ExternalSteerQueuedReason::StaleTurn),
                    )),
                    Err(WorkerError::SteeringUnavailable(_) | WorkerError::SteeringRejected(_)) => {
                        Some((
                            ExternalSteerAdmission::Queued,
                            Some(ExternalSteerQueuedReason::SteerUnsupported),
                        ))
                    }
                    _ => None,
                };
                if let Some((outcome, reason)) = settlement {
                    let settled = if let Some(correlation_uuid) = admission.correlation_uuid {
                        tidebreak_core::db::code::settle_machine_external_steer_admission(
                            &admission.db,
                            &admission.owner,
                            admission.session_id,
                            admission.spawn_epoch,
                            admission.expected_turn_id,
                            correlation_uuid,
                            outcome,
                            reason,
                        )
                        .await
                    } else {
                        Ok(false)
                    };
                    match settled {
                        Ok(true) => {}
                        Ok(false) => {
                            warn!(
                                session = %admission.session_id,
                                event_id = %admission.event_id,
                                "dropping a steering settlement from a mismatched worker, turn, or outcome"
                            );
                            result = Err(WorkerError::Failed(
                                "The steering receipt no longer matches this worker, turn, or outcome.".into(),
                            ));
                        }
                        Err(error) => result = Err(WorkerError::Failed(error.to_string())),
                    }
                }
            }
            let _ = reply.send(result);
            ControlFlow::Continue
        }
        WorkerCommand::RunTurn { reply, .. } => {
            let _ = reply.send(Err(WorkerError::Conflict(
                "a turn is already running on this session".into(),
            )));
            ControlFlow::Continue
        }
        // Only reachable mid-turn: the two idle loops answer this themselves,
        // because accepting it means recording the new mode on the session
        // copy they own. The route refuses a switch while a turn runs, so this
        // says the same thing rather than re-posturing under a running turn.
        WorkerCommand::SetPermissionMode { reply, .. } => {
            let _ = reply.send(Err(WorkerError::Conflict(
                "finish or interrupt the running turn before changing the permission mode".into(),
            )));
            ControlFlow::Continue
        }
        WorkerCommand::SetExecutionSettings { reply, .. } => {
            let _ = reply.send(Err(WorkerError::Conflict(
                "finish or interrupt the running turn before changing session settings".into(),
            )));
            ControlFlow::Continue
        }
        WorkerCommand::Shutdown => ControlFlow::Shutdown,
    }
}

/// Ask the engine to re-posture itself, translating a refusal into the one
/// thing the caller can do about it.
async fn set_permission_mode(
    engine: &dyn HarnessSession,
    mode: PermissionMode,
) -> Result<(), WorkerError> {
    match engine.set_permission_mode(mode).await {
        Ok(()) => Ok(()),
        Err(HarnessError::PermissionModeSwitchUnsupported) => Err(WorkerError::RelaunchRequired(
            "this engine sets its permission mode at launch".into(),
        )),
        Err(HarnessError::PermissionModeUnsupported(mode)) => Err(WorkerError::Conflict(format!(
            "this engine cannot honor {mode}"
        ))),
        Err(HarnessError::PermissionModeSwitchFailed(detail)) => Err(WorkerError::Conflict(detail)),
        Err(other) => Err(WorkerError::Failed(other.to_string())),
    }
}

/// Run every promotable queued row, oldest first (decision 69).
///
/// Each round snapshots the durable FIFO head and drives it as a turn;
/// [`promote_queued_turn`] deletes the row and inserts the turn together, so
/// an edit, reorder, or retraction that lands after the snapshot surfaces as
/// [`WorkerError::QueuedTurnStale`] and the loop simply re-reads. Settings
/// resolve when the worker promotes the row after acquiring the worktree: the
/// turn runs under the session's model, effort, and fast mode then, including
/// confirmed reservations accepted during the wait. Same contract as the chat
/// queue. A pause holds the whole drain; the resume, the next enqueue, or
/// send-now wakes the worker again.
async fn drain_queued(
    session: &mut Session,
    engine: &dyn HarnessSession,
    sink: &LiveSink,
    queue: &TurnQueue,
    store: &AttachmentStore,
    commands: &mut mpsc::Receiver<WorkerCommand>,
) {
    loop {
        if session_was_ended(&sink.db, session).await {
            return;
        }
        // An update quiesce holds the whole drain: the rows stay put, visibly
        // queued, and the relaunched worker drains them.
        if *queue.quiesce.borrow() {
            return;
        }
        match queue_paused(&sink.db, &session.owner, session.id).await {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                warn!(
                    session = %session.id,
                    error = %error,
                    "could not read the queue pause state"
                );
                return;
            }
        }
        let head = match queued_turn_head(&sink.db, &session.owner, session.id).await {
            Ok(Some(head)) => head,
            Ok(None) => return,
            Err(error) => {
                warn!(
                    session = %session.id,
                    error = %error,
                    "could not read the queued turns"
                );
                return;
            }
        };
        let follow_up = QueuedFollowUp {
            message: head.message.clone(),
            attachments: head.attachments.clone(),
            actor: head.actor.clone(),
            trigger_delivery: None,
            queued_row: Some(Box::new(head.clone())),
            // The row was acknowledged when it was queued; nobody is
            // waiting on this start.
            accepted: None,
        };
        let result = drive_turn(
            session,
            engine,
            sink,
            store,
            commands,
            WorktreeTurn {
                lock: &queue.worktree,
                wait: TurnWait::Queued,
                quiesce: &queue.quiesce,
            },
            follow_up,
        )
        .await;
        match result {
            Ok(_) => {}
            // The row moved beneath the snapshot; the next read is current.
            Err(WorkerError::QueuedTurnStale) => {}
            // A stop aimed at work that had not started holds the whole
            // queue: nothing here will run until the reader says so, and the
            // pause is what makes the tray say exactly that. Without it the
            // rows would sit looking live while no wake is coming — sibling
            // turn completion releases the checkout but notifies nobody.
            // Resume or send-now clears the pause and wakes this worker.
            Err(WorkerError::QueuedTurnStopped) => {
                if let Err(error) =
                    set_queue_paused(&sink.db, &session.owner, session.id, true).await
                {
                    warn!(
                        session = %session.id,
                        error = %error,
                        "could not pause the queue after a stop"
                    );
                }
                return;
            }
            Err(WorkerError::Failed(detail)) => {
                warn!(
                    session = %session.id,
                    error = %detail,
                    "a queued code turn could not start"
                );
                // Drop the row rather than retry it: the failure is almost
                // always the row's own (a dead attachment blob), and a retry
                // loop would burn the worker. The promote transaction already
                // removed it when the failure came later; this covers the
                // earlier failures. Chat drops unpromotable rows the same way.
                if let Err(error) =
                    delete_queued_turn(&sink.db, &session.owner, session.id, head.id).await
                {
                    warn!(
                        session = %session.id,
                        error = %error,
                        "could not drop the failed queued turn"
                    );
                    return;
                }
                let _ = persist_and_publish(
                    &sink.db,
                    &sink.bus,
                    &session.owner,
                    session.id,
                    session.spawn_epoch,
                    Event::HarnessNotice {
                        level: HarnessNoticeLevel::Error,
                        message: format!("The queued turn could not start: {detail}"),
                    },
                    false,
                )
                .await;
                if session.lifecycle != SessionLifecycle::Fenced {
                    super::attention::replace_attention(
                        session,
                        Attention::needs_you(
                            "the queued turn could not start",
                            AttentionSource::Lifecycle,
                        ),
                        false,
                    );
                    let _ = super::attention::persist_session(&sink.db, &sink.bus, session).await;
                }
            }
            // Stopped before it started, the session ended, or the engine
            // needs a relaunch. The rows stay put — retracting them is the
            // reader's call, and the next wake retries.
            Err(_) => return,
        }
    }
}

/// Pick a waiting turn back up after a worker restart and drive the same
/// `resume_turn` path a live park resolution uses.
async fn continue_parked_turn(
    session: &mut Session,
    engine: &dyn HarnessSession,
    sink: &LiveSink,
    commands: &mut mpsc::Receiver<WorkerCommand>,
    queue: &TurnQueue,
    mut turn: Turn,
) -> Result<Turn, WorkerError> {
    let db = &sink.db;
    let bus = &sink.bus;
    let Some(park_ref) = turn.park_ref.clone() else {
        return Err(WorkerError::Failed(
            "waiting turn is missing its park ref".into(),
        ));
    };
    let Some(wait) = turn.park_wait.clone() else {
        return Err(WorkerError::Failed(
            "waiting turn is missing its park wait".into(),
        ));
    };
    let _worktree = match await_worktree_turn(session, engine, &queue.worktree, commands).await {
        WorktreeWait::Acquired(guard) => guard,
        WorktreeWait::Stopped => {
            return Err(WorkerError::QueuedTurnStopped);
        }
        WorktreeWait::Shutdown => {
            return Err(WorkerError::Conflict(
                "the session worker is shutting down".into(),
            ));
        }
    };
    if session_was_ended(db, session).await {
        return Err(WorkerError::Conflict("session has ended".into()));
    }

    session.lifecycle = SessionLifecycle::Running;
    super::attention::replace_attention(
        session,
        Attention::working(AttentionSource::Lifecycle),
        false,
    );
    record_child_process(session, engine.child_pid());
    super::attention::persist_session(db, bus, session)
        .await
        .map_err(|err| WorkerError::Failed(err.to_string()))?;
    sink.set_turn(turn.id);
    sink.set_turn_model(turn.model.clone().or_else(|| session.model.clone()));
    let _ = rebind_pending_approvals_to_worker(
        db,
        &session.owner,
        session.id,
        turn.id,
        session.spawn_epoch,
    )
    .await;
    persist_and_publish(
        db,
        bus,
        &session.owner,
        session.id,
        session.spawn_epoch,
        Event::TurnResumed { turn_id: turn.id },
        false,
    )
    .await
    .map_err(|err| WorkerError::Failed(err.to_string()))?;

    let mut next_resume: Option<(String, tidebreak_harness::ResumeInput)>;
    let mut pid_changes = engine.child_pid_changes();
    let mut side_writes = SideWrites::default();
    let mut controls: FuturesUnordered<BoxFuture<'_, ControlFlow>> = FuturesUnordered::new();
    let mut interrupted = false;
    let mut commands_closed = false;
    let delivered: DeliveredDecisions =
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    match await_park_resolution(
        engine,
        db,
        bus,
        session,
        commands,
        &mut controls,
        &mut interrupted,
        &mut commands_closed,
        &park_ref,
        &wait,
        turn.id,
        &delivered,
    )
    .await
    {
        Ok(Some(ParkResolution {
            input,
            delivered_here,
        })) => {
            if delivered_here {
                apply_accepted_plan_mode(db, bus, session, engine, &input).await;
            }
            clear_persisted_turn_park(db, session, &mut turn, &park_ref, &wait)
                .await
                .map_err(WorkerError::Failed)?;
            next_resume = Some((park_ref, input));
        }
        Ok(None) => {
            while controls.next().await.is_some() {}
            return close_open_turn(
                session,
                engine,
                sink,
                turn,
                Ok(TurnOutcome::Clean),
                interrupted,
                None,
            )
            .await;
        }
        Err(error) => return Err(error),
    }

    let run = 'legs: loop {
        let Some((park_ref, input)) = next_resume.take() else {
            unreachable!("every recovered resume leg starts with a park");
        };
        let leg: BoxFuture<'_, Result<TurnOutcome, HarnessError>> =
            Box::pin(engine.resume_turn(park_ref, input));
        tokio::pin!(leg);
        let result = loop {
            tokio::select! {
                biased;
                Some(flow) = controls.next(), if !controls.is_empty() => {
                    if flow == ControlFlow::Shutdown {
                        interrupted = true;
                        controls.push(Box::pin(interrupt_engine(engine)));
                    }
                }
                result = &mut leg => break result,
                command = commands.recv(), if !commands_closed => match command {
                    Some(command) => {
                        interrupted |= matches!(command, WorkerCommand::Interrupt { .. });
                        controls.push(Box::pin(apply_control(
                            engine,
                            command,
                            Some(turn.id),
                            Some(delivered.clone()),
                        )));
                    }
                    None => {
                        commands_closed = true;
                        interrupted = true;
                        controls.push(Box::pin(interrupt_engine(engine)));
                    }
                },
                pid = next_child_pid(pid_changes.as_mut()) => {
                    if session.child_pid != pid {
                        record_child_process(session, pid);
                        side_writes.child_process(db, session);
                    }
                }
            }
        };
        let Ok(TurnOutcome::Parked {
            park_ref,
            waiting_on,
        }) = result
        else {
            break 'legs result;
        };
        if interrupted {
            break 'legs Ok(TurnOutcome::Clean);
        }
        let wait = match persist_turn_park(db, session, &mut turn, &park_ref, &waiting_on).await {
            Ok(wait) => wait,
            Err(error) => break 'legs Err(HarnessError::Other(error)),
        };
        match await_park_resolution(
            engine,
            db,
            bus,
            session,
            commands,
            &mut controls,
            &mut interrupted,
            &mut commands_closed,
            &park_ref,
            &wait,
            turn.id,
            &delivered,
        )
        .await
        {
            Ok(Some(ParkResolution {
                input,
                delivered_here,
            })) => {
                if delivered_here {
                    apply_accepted_plan_mode(db, bus, session, engine, &input).await;
                }
                if let Err(error) =
                    clear_persisted_turn_park(db, session, &mut turn, &park_ref, &wait).await
                {
                    break 'legs Err(HarnessError::Other(error));
                }
                next_resume = Some((park_ref, input));
            }
            Ok(None) => break 'legs Ok(TurnOutcome::Clean),
            Err(error) => break 'legs Err(HarnessError::Other(error.to_string())),
        }
    };
    while controls.next().await.is_some() {}
    side_writes.settle().await;
    close_open_turn(session, engine, sink, turn, run, interrupted, None).await
}

/// Scan a closed turn for pull-request acts (decision 77), then restate the
/// digest of every workspace whose pull-request column a confirmed fact
/// rewrote.
///
/// A plain function that returns the work boxed, so the turn driver's frame
/// and future hold only a pointer. The driver's futures are already deep, and
/// the detector's grows with the store's merge; built inline in the driver,
/// it overflowed a test thread's stack.
fn record_turn_pull_request_acts<'a>(
    sink: &'a LiveSink,
    session: &'a Session,
    turn_id: TurnId,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        let rewritten = super::pr_facts::sweep_turn_for_pull_request_acts(
            &sink.db,
            session,
            turn_id,
            sink.gh_search_path.as_deref(),
            Some(&sink.hot_prs),
        )
        .await;
        for workspace in rewritten {
            super::attention::emit_workspace_digests(
                &sink.db,
                &sink.bus,
                &session.owner,
                workspace,
            )
            .await;
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn close_open_turn<'fut>(
    session: &'fut mut Session,
    engine: &'fut dyn HarnessSession,
    sink: &'fut LiveSink,
    mut turn: Turn,
    run: Result<TurnOutcome, HarnessError>,
    interrupted: bool,
    attachment_cleanup_error: Option<String>,
) -> BoxFuture<'fut, Result<Turn, WorkerError>> {
    Box::pin(async move {
        let db = &sink.db;
        let bus = &sink.bus;
        record_child_process(session, engine.child_pid());
        if let Some(resume) = engine.resume_ref() {
            session.harness_resume_ref = Some(resume);
        }
        let dropped = sink.take_unrecognized_delta(engine.unrecognized_events());
        if dropped > 0 {
            session.unrecognized_event_count = session
                .unrecognized_event_count
                .saturating_add(i64::try_from(dropped).unwrap_or(i64::MAX));
        }
        if let Ok(Some(updated)) = get_open_turn(db, &session.owner, session.id).await {
            turn = updated;
        } else if let Ok(Some(current)) =
            tidebreak_core::db::code::get_turn(db, &session.owner, turn.id).await
        {
            turn = current;
        }

        match run {
            Ok(outcome) => {
                let (detail, failure) = match outcome {
                    TurnOutcome::Clean => (None, None),
                    TurnOutcome::Incomplete { detail, failure } => (Some(detail), failure),
                    TurnOutcome::Parked { .. } => (
                        Some("the engine parked a turn the worker was no longer waiting on".into()),
                        None,
                    ),
                };
                if let (Some(detail), false) = (detail.as_ref(), interrupted) {
                    let _ = persist_and_publish(
                        db,
                        bus,
                        &session.owner,
                        session.id,
                        session.spawn_epoch,
                        Event::HarnessNotice {
                            level: HarnessNoticeLevel::Error,
                            message: detail.clone(),
                        },
                        false,
                    )
                    .await;
                }
                if turn.status.is_open() {
                    let (status, event) = if interrupted {
                        (
                            TurnStatus::Interrupted,
                            Event::TurnInterrupted { usage: None },
                        )
                    } else if let Some(detail) = detail {
                        let mut error = BoundedError::new(detail);
                        error.failure = failure;
                        (
                            TurnStatus::Failed,
                            Event::TurnFailed {
                                error: sink.legible_turn_error(error),
                                detail: None,
                            },
                        )
                    } else {
                        (
                            TurnStatus::Completed,
                            Event::TurnCompleted {
                                usage: turn.usage.clone().unwrap_or_default(),
                                checkpoint: None,
                                stop_reason: None,
                            },
                        )
                    };
                    turn.status = status;
                    turn.ended_at = Some(Utc::now());
                    let _ = save_turn(db, &session.owner, &turn).await;
                    sink.note_subagent_boundary(&event).await;
                    let write = if matches!(event, Event::TurnInterrupted { .. }) {
                        persist_and_publish(
                            db,
                            bus,
                            &session.owner,
                            session.id,
                            session.spawn_epoch,
                            event,
                            sink.native_journal,
                        )
                        .await
                    } else {
                        persist_turn_and_publish(
                            db,
                            bus,
                            &session.owner,
                            session.id,
                            session.spawn_epoch,
                            turn.id,
                            event,
                            sink.native_journal,
                        )
                        .await
                    };
                    let _ = write;
                }
            }
            Err(err) => {
                if turn.status.is_open() {
                    turn.status = TurnStatus::Failed;
                    turn.ended_at = Some(Utc::now());
                    let _ = save_turn(db, &session.owner, &turn).await;
                    // An adapter that classified the refusal hands back the
                    // error itself; anything else is only its words.
                    let error = match &err {
                        HarnessError::TurnFailed(error) => error.clone(),
                        other => BoundedError::new(other.to_string()),
                    };
                    let event = Event::TurnFailed {
                        error: sink.legible_turn_error(error),
                        detail: None,
                    };
                    sink.note_subagent_boundary(&event).await;
                    let _ = persist_turn_and_publish(
                        db,
                        bus,
                        &session.owner,
                        session.id,
                        session.spawn_epoch,
                        turn.id,
                        event,
                        sink.native_journal,
                    )
                    .await;
                }
                super::checkpoint::after_turn_ended(db, bus, session, &mut turn).await;
                record_turn_pull_request_acts(sink, session, turn.id).await;
                if let Some(detail) = attachment_cleanup_error.as_ref() {
                    let _ = super::recovery::fence_session(
                        db,
                        bus,
                        session,
                        FenceReason::ProbeAmbiguous {
                            detail: detail.clone(),
                        },
                    )
                    .await;
                    return Err(WorkerError::Failed(detail.clone()));
                }
                if !session_was_ended(db, session).await {
                    if let HarnessError::ResumeLost(detail) = &err {
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
                    session.lifecycle = SessionLifecycle::Idle;
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

        if let Ok(Some(current)) =
            tidebreak_core::db::code::get_turn(db, &session.owner, turn.id).await
        {
            turn = current;
        }
        super::checkpoint::after_turn_ended(db, bus, session, &mut turn).await;
        record_turn_pull_request_acts(sink, session, turn.id).await;
        if let Some(detail) = attachment_cleanup_error {
            let _ = super::recovery::fence_session(
                db,
                bus,
                session,
                FenceReason::ProbeAmbiguous {
                    detail: detail.clone(),
                },
            )
            .await;
            return Err(WorkerError::Failed(detail));
        }
        if session_was_ended(db, session).await {
            return Ok(turn);
        }
        if turn.status == TurnStatus::Interrupted {
            super::attention::replace_attention(
                session,
                Attention::needs_you("the turn was interrupted", AttentionSource::Lifecycle),
                false,
            );
        } else if turn.status == TurnStatus::Failed {
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
        session.lifecycle = SessionLifecycle::Idle;
        let _ = super::attention::persist_session(db, bus, session).await;
        Ok(turn)
    })
}

/// Drive one turn to its end inside the turn's tracing span.
///
/// Boxed, so the worker loop and the queue drain each hold a pointer rather
/// than the turn's whole future in their own frames.
fn drive_turn<'a>(
    session: &'a mut Session,
    engine: &'a dyn HarnessSession,
    sink: &'a LiveSink,
    store: &'a AttachmentStore,
    commands: &'a mut mpsc::Receiver<WorkerCommand>,
    worktree: WorktreeTurn<'a>,
    follow_up: QueuedFollowUp<'a>,
) -> BoxFuture<'a, Result<Turn, WorkerError>> {
    Box::pin(async move {
        let session_id = session.id;
        let wait = match worktree.wait {
            TurnWait::Send => "send",
            TurnWait::Queued => "queued",
        };
        let span = tracing::info_span!(
            target: crate::diagnostics::EVENT_TARGET,
            "tidebreak.turn",
            otel.name = "tidebreak.turn",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            tidebreak.code.session_id = %session_id,
            tidebreak.code.wait = wait,
            tidebreak.outcome = tracing::field::Empty,
            tidebreak.duration_ms = tracing::field::Empty,
        );
        let started = Instant::now();
        let result = drive_turn_inner(session, engine, sink, store, commands, worktree, follow_up)
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
                event_name = "tidebreak.turn.completed",
                outcome,
                duration_ms,
                "turn completed"
            );
        });
        result
    })
}

/// What a started turn hands to the loop that runs it.
struct StartedTurn<'w> {
    /// The workspace's checkout, held until the turn ends (record 55).
    worktree: tokio::sync::MutexGuard<'w, ()>,
    turn: Turn,
    /// Private attachment files for an engine that reads images off disk.
    staged_attachments: Option<StagedTurnAttachments>,
    /// The lease an internal turn heartbeats while it runs.
    lease_token: Option<uuid::Uuid>,
    /// What the engine is handed first.
    input: TurnInput,
}

/// Everything before the engine runs: the checks, the checkout, the turn row,
/// the start in the journal, and the engine's input.
///
/// A plain function that returns the work boxed. Built inline, its awaits
/// would sit in the driver's frame for the whole turn, beneath the engine's
/// own call chain, and that chain is what runs out of stack.
#[allow(clippy::too_many_arguments)]
fn start_turn<'b, 'w: 'b>(
    session: &'b mut Session,
    engine: &'b dyn HarnessSession,
    sink: &'b LiveSink,
    store: &'b AttachmentStore,
    commands: &'b mut mpsc::Receiver<WorkerCommand>,
    worktree: WorktreeTurn<'w>,
    QueuedFollowUp {
        message,
        attachments,
        actor,
        trigger_delivery,
        queued_row,
        accepted,
    }: QueuedFollowUp<'b>,
) -> BoxFuture<'b, Result<StartedTurn<'w>, WorkerError>> {
    Box::pin(async move {
        let db = &sink.db;
        let bus = &sink.bus;
        let current = get_session(db, &session.owner, session.id)
            .await
            .map_err(|error| WorkerError::Failed(error.to_string()))?
            .ok_or_else(|| WorkerError::Conflict("session no longer exists".into()))?;
        if session.execution_location != tidebreak_core::ExecutionLocation::Machine
            || current.execution_location != tidebreak_core::ExecutionLocation::Machine
        {
            return Err(WorkerError::Conflict(
                "this session runs in a sandbox; its queued messages stay with the sandbox".into(),
            ));
        }
        if session.lifecycle == SessionLifecycle::Running {
            return Err(WorkerError::Conflict(
                "a turn is already running on this session".into(),
            ));
        }
        if session.lifecycle == SessionLifecycle::Fenced {
            return Err(WorkerError::Conflict(
                "session is fenced until it is reaped".into(),
            ));
        }
        if session.lifecycle == SessionLifecycle::Ended {
            return Err(WorkerError::Conflict("session has ended".into()));
        }
        // Bytes first, before the worktree lock: a blob read and a decode should
        // never be held against a sibling session waiting for the checkout.
        let hydrated = hydrate_turn_images(store.blobs.as_deref(), &attachments).await?;

        // A queued row is already accepted. Bring the worker up to the committed
        // settings before it waits for the checkout. Reservations accepted during
        // that wait then update this same session copy, so the turn sees the last
        // committed settings when the lock becomes available.
        if matches!(worktree.wait, TurnWait::Queued) {
            if current.spawn_epoch != session.spawn_epoch {
                return Err(WorkerError::Conflict(
                    "the session worker was superseded before the turn started".into(),
                ));
            }
            session.model = current.model;
            session.reasoning_effort = current.reasoning_effort;
            session.fast_mode = current.fast_mode;
        }

        // The workspace's checkout takes one turn at a time (record 55). Taking
        // the lock *is* the reservation: a pre-flight database read cannot be,
        // because two idle siblings both pass it before either marks itself
        // running. Blobs are hydrated first so a decode never holds the tree.
        let checkout = match worktree.wait {
            // A send has its request open, so it never waits on a sibling's turn.
            // The caller parks the message and answers `Queued` instead.
            TurnWait::Send => worktree
                .lock
                .try_lock()
                .map_err(|_| WorkerError::WorktreeBusy)?,
            // Already acknowledged, so waiting costs nobody a connection. The wait
            // still listens for control: an interrupt that arrives while a turn is
            // queued has to stop it before it starts, not after.
            TurnWait::Queued => {
                match await_worktree_turn(session, engine, worktree.lock, commands).await {
                    WorktreeWait::Acquired(guard) => guard,
                    WorktreeWait::Stopped => return Err(WorkerError::QueuedTurnStopped),
                    WorktreeWait::Shutdown => {
                        return Err(WorkerError::Conflict(
                            "the session worker is shutting down".into(),
                        ));
                    }
                }
            }
        };
        // Ending a session during that wait has to win. The lifecycle checks above
        // read a session that may be minutes stale by now.
        if session_was_ended(&sink.db, session).await {
            return Err(WorkerError::Conflict("session has ended".into()));
        }
        // So does a restart-to-update that started during the wait: it is
        // counting turn boundaries, and a turn that starts now holds it for the
        // turn's whole length. The message is not lost — a send is parked by the
        // route, and a queued row stays in the queue for the relaunch.
        if *worktree.quiesce.borrow() {
            return Err(WorkerError::UpdateQuiesced);
        }
        // The repository this read names is also the one the turn's memory
        // comes from. Nothing after the sender hears the turn is accepted may
        // fail, so the memory step below reuses it rather than reading again.
        let repo_id = if let Some(workspace_id) = session.workspace_id {
            let workspace = get_workspace(&sink.db, &session.owner, workspace_id)
                .await
                .map_err(|error| WorkerError::Failed(error.to_string()))?
                .ok_or_else(|| WorkerError::Conflict("workspace no longer exists".into()))?;
            if workspace.is_remote() {
                return Err(WorkerError::Conflict(
                    "a local worker cannot run in a sandbox workspace".into(),
                ));
            }
            if workspace.status != CodeWorkspaceStatus::Active {
                return Err(WorkerError::Conflict(format!(
                    "workspace is {}",
                    workspace.status.as_str()
                )));
            }
            Some(workspace.repo_id)
        } else {
            None
        };

        // An internal turn that parked for a client or an agent run hands its
        // lease back and leaves the session idle, but the turn is still open. A
        // second turn inserted beside it could never take its transcript message
        // (one live turn per session may own one), so refuse it up front.
        if session.harness_kind == HarnessKind::Internal {
            if let Some(open) = get_open_turn(db, &session.owner, session.id)
                .await
                .map_err(|err| WorkerError::Failed(err.to_string()))?
            {
                return Err(WorkerError::Conflict(format!(
                    "turn {} is still {}; finish it before sending again",
                    open.id,
                    open.status.as_str()
                )));
            }
        }

        // An idle send resolves settings after it owns the worktree, so a
        // reservation that committed while this request was in flight reaches the
        // engine. A queued row uses the worker copy initialized before the wait and
        // updated by every confirmed reservation accepted during that wait.
        let turn_settings = match worktree.wait {
            TurnWait::Queued => SessionExecutionSettings::from(&*session),
            TurnWait::Send => {
                let current = get_session(db, &session.owner, session.id)
                    .await
                    .map_err(|err| WorkerError::Failed(err.to_string()))?
                    .ok_or_else(|| {
                        WorkerError::Failed(format!("session {} not found", session.id))
                    })?;
                if current.spawn_epoch != session.spawn_epoch {
                    return Err(WorkerError::Conflict(
                        "the session worker was superseded before the turn started".into(),
                    ));
                }
                session.model = current.model;
                session.reasoning_effort = current.reasoning_effort;
                session.fast_mode = current.fast_mode;
                SessionExecutionSettings::from(&*session)
            }
        };

        let ordinal = next_turn_ordinal(db, &session.owner, session.id)
            .await
            .map_err(|err| WorkerError::Failed(err.to_string()))?;
        let turn = Turn {
            // A promoted queue row already carries the turn's id: inserting under
            // it is what lets the row deletion and the turn insertion commit as
            // one write (decision 69).
            id: queued_row.as_ref().map_or_else(TurnId::new, |row| row.id),
            session_id: session.id,
            ordinal,
            status: TurnStatus::Running,
            model: turn_settings.model.clone(),
            fast_mode: turn_settings.fast_mode,
            actor: queued_row
                .as_ref()
                .and_then(|row| row.actor.clone())
                .or(actor),
            user_input: message.clone(),
            user_input_blob_id: None,
            attachments,
            checkpoint_ref: None,
            diffstat: None,
            usage: None,
            narrative: None,
            rewrite: None,
            started_at: Utc::now(),
            ended_at: None,
            park_ref: None,
            park_wait: None,
        };

        // Clear files a crashed worker left behind before this turn exposes any
        // new bytes. The worktree lock proves no live turn in this checkout still
        // owns one of these directories.
        sweep_attachment_leftovers_or_fence(db, bus, session, &store.private_root).await?;
        let staged_attachments = if store.engine_reads_images {
            None
        } else {
            if hydrated.is_empty() {
                None
            } else {
                Some(
                    write_turn_attachments(
                        &store.private_root,
                        session.id,
                        turn.id,
                        &turn.attachments,
                        &hydrated,
                    )
                    .await
                    .map_err(|err| WorkerError::Failed(format!("write attachment: {err}")))?,
                )
            }
        };

        // All fallible file preparation finishes before the database records a
        // running turn. If staging fails, the session stays idle and the scope
        // removes any partial files before this function releases the worktree.
        if let Some(claim) = trigger_delivery {
            if !accept_trigger_turn_delivery(
                db,
                &session.owner,
                claim.delivery_id,
                claim.lease_token,
                &turn,
                Utc::now(),
            )
            .await
            .map_err(|err| WorkerError::Failed(err.to_string()))?
            {
                return Err(WorkerError::TriggerDeliveryAccepted);
            }
        } else if let Some(row) = queued_row {
            // Deletes the row and inserts the turn together; `false` means the
            // row was edited, reordered, or retracted after the drain snapshot,
            // and nothing was written.
            if !promote_queued_turn(db, &session.owner, &row, &turn)
                .await
                .map_err(|err| WorkerError::Failed(err.to_string()))?
            {
                return Err(WorkerError::QueuedTurnStale);
            }
        } else {
            insert_turn(db, &session.owner, &turn)
                .await
                .map_err(|err| WorkerError::Failed(err.to_string()))?;
        }
        sink.set_turn(turn.id);
        sink.set_turn_model(turn.model.clone().or_else(|| session.model.clone()));

        let lease_token = if session.harness_kind == HarnessKind::Internal {
            let lease_token = uuid::Uuid::new_v4();
            let now = Utc::now();
            let lease_expires_at = now + chrono::Duration::seconds(60);
            let claimed = db
                .take_lease_on_turn_with_input_message(
                    tidebreak_core::TurnId(turn.id.0),
                    lease_token,
                    now,
                    lease_expires_at,
                    &message,
                )
                .await
                .map_err(|err| WorkerError::Failed(err.to_string()))?;
            if claimed.is_none() {
                return Err(WorkerError::Failed(format!(
                    "could not claim a lease on turn {}",
                    turn.id
                )));
            }
            Some(lease_token)
        } else {
            None
        };

        session.lifecycle = SessionLifecycle::Running;
        super::attention::replace_attention(
            session,
            Attention::working(AttentionSource::Lifecycle),
            false,
        );
        record_child_process(session, engine.child_pid());
        super::attention::persist_session(db, bus, session)
            .await
            .map_err(|err| WorkerError::Failed(err.to_string()))?;

        persist_and_publish(
            db,
            bus,
            &session.owner,
            session.id,
            session.spawn_epoch,
            Event::TurnStarted { turn_id: turn.id },
            sink.native_journal,
        )
        .await
        .map_err(|err| WorkerError::Failed(err.to_string()))?;

        // Accepted: the turn row exists, the session reads running, and the start
        // is in the journal. The sender hears that now and follows the rest on
        // the event bus; holding its request until the engine finished kept a
        // client's socket and draft waiting on a reply it did not need.
        //
        // Nothing below may fail. An error returned from here on would reach
        // no sender and journal no end, leaving a running turn that nothing
        // finishes. Memory degrades to none instead, and its repository came
        // from the workspace read above.
        if let Some(reply) = accepted.and_then(Option::take) {
            let _ = reply.send(Ok(turn.clone()));
        }

        let memory_enabled = db
            .get_setting(crate::runtime_settings::MEMORY_ENABLED_SETTING)
            .await
            .ok()
            .flatten()
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let memory_dir = if memory_enabled {
            // Memory is an aid, not a precondition. A store or filesystem fault
            // stays in diagnostics and does not interrupt the turn or transcript.
            match super::memory::materialize_session_memory(
                db.as_ref(),
                &session.owner,
                repo_id,
                &store.private_root,
            )
            .await
            {
                Ok(memory_dir) => memory_dir,
                Err(err) => {
                    tracing::warn!(
                        "tidebreak: could not materialize memory for code session {}: {err}",
                        session.id
                    );
                    None
                }
            }
        } else {
            None
        };

        // What the engine is handed is not always what the person wrote: an engine
        // that cannot take images over its own protocol is given paths instead, and
        // `turn.user_input` above keeps the message as typed.
        let (engine_text, images) = if store.engine_reads_images || hydrated.is_empty() {
            (message, hydrated)
        } else {
            let staged = staged_attachments
                .as_ref()
                .expect("non-native image delivery staged before turn insertion");
            (
                message_naming_attachments(&message, &staged.paths),
                Vec::new(),
            )
        };
        let engine_text = match (ordinal, memory_dir.as_deref()) {
            (1, Some(memory_dir)) => format!(
                "{engine_text}\n\n{}",
                super::memory::first_turn_memory_line(memory_dir)
            ),
            _ => engine_text,
        };
        // A restore since this session's last turn moved files the engine
        // still remembers writing. It hears which, before the person's
        // message.
        let engine_text = match Box::pin(restore_note_for_turn(db, session, ordinal)).await {
            Some(note) => format!("{note}\n\n{engine_text}"),
            None => engine_text,
        };
        let input = TurnInput {
            turn_id: Some(turn.id),
            text: engine_text,
            model: turn_settings.model.clone(),
            reasoning_effort: turn_settings.reasoning_effort,
            fast_mode: turn_settings.fast_mode,
            images,
        };
        Ok(StartedTurn {
            worktree: checkout,
            turn,
            staged_attachments,
            lease_token,
            input,
        })
    })
}

async fn drive_turn_inner(
    session: &mut Session,
    engine: &dyn HarnessSession,
    sink: &LiveSink,
    store: &AttachmentStore,
    commands: &mut mpsc::Receiver<WorkerCommand>,
    worktree: WorktreeTurn<'_>,
    follow_up: QueuedFollowUp<'_>,
) -> Result<Turn, WorkerError> {
    // The start and the close run as boxed futures of their own. This frame
    // stays under the engine for the whole turn, so it keeps only the legs.
    let StartedTurn {
        worktree: _worktree,
        mut turn,
        staged_attachments,
        lease_token,
        input,
    } = start_turn(session, engine, sink, store, commands, worktree, follow_up).await?;
    let db = &sink.db;
    let bus = &sink.bus;
    let mut next_input = Some(input);
    let mut next_resume: Option<(String, tidebreak_harness::ResumeInput)> = None;
    // Adapters that spawn one child per turn have no pid to report until the
    // turn is under way. Record every transition as it happens: the session
    // row's pid is what boot recovery probes, and a NULL pid there is read as
    // "the engine is gone" — which would re-attach a worker to a worktree a
    // live child is still writing to.
    let mut pid_changes = engine.child_pid_changes();
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + TURN_LEASE_HEARTBEAT,
        TURN_LEASE_HEARTBEAT,
    );
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut side_writes = SideWrites::default();
    // Control commands run concurrently with the turn. Awaiting one inline
    // would stop draining the child's stdout — during an interrupt's grace
    // period that is what turns a clean abort into a kill.
    let mut controls: FuturesUnordered<BoxFuture<'_, ControlFlow>> = FuturesUnordered::new();
    let mut interrupted = false;
    let mut commands_closed = false;
    let delivered: DeliveredDecisions =
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    // One leg per engine future: the opening `run_turn`, then one
    // `resume_turn` per resolved park. An engine that never parks runs one
    // leg, exactly the old shape.
    let run = 'legs: loop {
        let leg: BoxFuture<'_, Result<TurnOutcome, HarnessError>> =
            if let Some(input) = next_input.take() {
                Box::pin(engine.run_turn(input))
            } else if let Some((park_ref, input)) = next_resume.take() {
                Box::pin(engine.resume_turn(park_ref, input))
            } else {
                unreachable!("every leg starts with an input or a resume");
            };
        tokio::pin!(leg);
        let result = loop {
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
                result = &mut leg => break result,
                command = commands.recv(), if !commands_closed => match command {
                    Some(command) => {
                        interrupted |= matches!(command, WorkerCommand::Interrupt { .. });
                        controls.push(Box::pin(apply_control(
                            engine,
                            command,
                            Some(turn.id),
                            Some(delivered.clone()),
                        )));
                    }
                    None => {
                        commands_closed = true;
                        interrupted = true;
                        controls.push(Box::pin(interrupt_engine(engine)));
                    }
                },
                pid = next_child_pid(pid_changes.as_mut()) => {
                    if session.child_pid != pid {
                        record_child_process(session, pid);
                        side_writes.child_process(db, session);
                    }
                }
                _ = heartbeat.tick() => {
                    if let Some(lease_token) = lease_token {
                        side_writes.heartbeat(db, turn.id, lease_token);
                    }
                }
            }
        };
        let Ok(TurnOutcome::Parked {
            park_ref,
            waiting_on,
        }) = result
        else {
            break 'legs result;
        };
        // The engine checkpointed durably and released the turn. Persist the
        // park so the row says what the turn waits for, then hold here for
        // the resolution. The session stays Running: a parked turn is open,
        // and the decide route requires a live worker.
        if interrupted {
            // A stop raced the park; close the turn instead of waiting.
            break 'legs Ok(TurnOutcome::Clean);
        }
        let wait = match persist_turn_park(db, session, &mut turn, &park_ref, &waiting_on).await {
            Ok(wait) => wait,
            Err(error) => break 'legs Err(HarnessError::Other(error)),
        };
        match await_park_resolution(
            engine,
            db,
            bus,
            session,
            commands,
            &mut controls,
            &mut interrupted,
            &mut commands_closed,
            &park_ref,
            &wait,
            turn.id,
            &delivered,
        )
        .await
        {
            Ok(Some(ParkResolution {
                input,
                delivered_here,
            })) => {
                // An accepted plan re-postures the session before the turn
                // continues: the decision itself never changes the mode, the
                // engine's own channel does, and the row must say so too. A
                // settlement another surface made carried its own posture.
                if delivered_here {
                    apply_accepted_plan_mode(db, bus, session, engine, &input).await;
                }
                if let Err(error) =
                    clear_persisted_turn_park(db, session, &mut turn, &park_ref, &wait).await
                {
                    break 'legs Err(HarnessError::Other(error));
                }
                next_resume = Some((park_ref, input));
            }
            // Interrupted or shut down while parked: fall through to the
            // shared closing code, which closes an open turn as interrupted.
            Ok(None) => break 'legs Ok(TurnOutcome::Clean),
            Err(error) => break 'legs Err(HarnessError::Other(error.to_string())),
        }
    };
    // A control command still in flight has a caller waiting on its reply.
    // Dropping it here would answer them with a dead channel.
    while controls.next().await.is_some() {}
    // Nothing on this task holds the writer now, so the side writes can land
    // before the turn closes and a late one cannot reach the next turn.
    side_writes.settle().await;
    finish_turn(
        session,
        engine,
        sink,
        turn,
        run,
        interrupted,
        staged_attachments,
    )
    .await
}

/// Everything after the engine returns: attachment cleanup, the turn's
/// terminal row, its checkpoint, and the session's next attention.
///
/// Boxed for the same reason as [`start_turn`]: its awaits stay out of the
/// frame that sits under the engine.
fn finish_turn<'a>(
    session: &'a mut Session,
    engine: &'a dyn HarnessSession,
    sink: &'a LiveSink,
    mut turn: Turn,
    run: Result<TurnOutcome, HarnessError>,
    interrupted: bool,
    mut staged_attachments: Option<StagedTurnAttachments>,
) -> BoxFuture<'a, Result<Turn, WorkerError>> {
    Box::pin(async move {
        let db = &sink.db;
        let bus = &sink.bus;

        // The engine has returned and no control call can still need the paths.
        // Remove the plaintext before checkpointing or releasing the worktree.
        let attachment_cleanup_error = if let Some(staged) = staged_attachments.as_mut() {
            match staged.scope.cleanup() {
                Ok(()) => None,
                Err(first) => match staged.scope.cleanup() {
                    Ok(()) => {
                        warn!(
                            session = %session.id,
                            turn = %turn.id,
                            error = %first,
                            "removing staged turn attachments succeeded on retry"
                        );
                        None
                    }
                    Err(second) => Some(format!(
                        "could not remove staged turn attachments after retry: {second} (first attempt: {first})"
                    )),
                },
            }
        } else {
            None
        };

        record_child_process(session, engine.child_pid());
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
                let (detail, failure) = match outcome {
                    TurnOutcome::Clean => (None, None),
                    TurnOutcome::Incomplete { detail, failure } => (Some(detail), failure),
                    // Parks are intercepted by the leg loop above; one arriving
                    // here means the engine parked after the worker stopped
                    // waiting, and failing loudly beats stranding the turn open.
                    TurnOutcome::Parked { .. } => (
                        Some("the engine parked a turn the worker was no longer waiting on".into()),
                        None,
                    ),
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
                        Event::HarnessNotice {
                            level: HarnessNoticeLevel::Error,
                            message: detail.clone(),
                        },
                        false,
                    )
                    .await;
                }
                if turn.status.is_open() {
                    // The stream ended without closing the turn. Only the worker
                    // knows whether that was asked for. Durable parks return to
                    // the leg loop before this point, so every remaining open
                    // state needs a terminal result.
                    let (status, event) = if interrupted {
                        (
                            TurnStatus::Interrupted,
                            Event::TurnInterrupted { usage: None },
                        )
                    } else if let Some(detail) = detail {
                        let mut error = BoundedError::new(detail);
                        error.failure = failure;
                        (
                            TurnStatus::Failed,
                            Event::TurnFailed {
                                error: sink.legible_turn_error(error),
                                detail: None,
                            },
                        )
                    } else {
                        (
                            TurnStatus::Completed,
                            Event::TurnCompleted {
                                usage: turn.usage.clone().unwrap_or_default(),
                                checkpoint: None,
                                stop_reason: None,
                            },
                        )
                    };
                    turn.status = status;
                    turn.ended_at = Some(Utc::now());
                    let _ = save_turn(db, &session.owner, &turn).await;
                    sink.note_subagent_boundary(&event).await;
                    let write = if matches!(event, Event::TurnInterrupted { .. }) {
                        persist_and_publish(
                            db,
                            bus,
                            &session.owner,
                            session.id,
                            session.spawn_epoch,
                            event,
                            sink.native_journal,
                        )
                        .await
                    } else {
                        persist_turn_and_publish(
                            db,
                            bus,
                            &session.owner,
                            session.id,
                            session.spawn_epoch,
                            turn.id,
                            event,
                            sink.native_journal,
                        )
                        .await
                    };
                    let _ = write;
                }
            }
            Err(err) => {
                if turn.status.is_open() {
                    turn.status = TurnStatus::Failed;
                    turn.ended_at = Some(Utc::now());
                    let _ = save_turn(db, &session.owner, &turn).await;
                    // An adapter that classified the refusal hands back the
                    // error itself; anything else is only its words.
                    let error = match &err {
                        HarnessError::TurnFailed(error) => error.clone(),
                        other => BoundedError::new(other.to_string()),
                    };
                    let event = Event::TurnFailed {
                        error: sink.legible_turn_error(error),
                        detail: None,
                    };
                    sink.note_subagent_boundary(&event).await;
                    let _ = persist_turn_and_publish(
                        db,
                        bus,
                        &session.owner,
                        session.id,
                        session.spawn_epoch,
                        turn.id,
                        event,
                        sink.native_journal,
                    )
                    .await;
                }
                // A harness error returns from here, so this is the only place
                // the turn's edits can still be checkpointed. The engine may have
                // rewritten files before the stream broke.
                super::checkpoint::after_turn_ended(db, bus, session, &mut turn).await;
                record_turn_pull_request_acts(sink, session, turn.id).await;
                if let Some(detail) = attachment_cleanup_error.as_ref() {
                    let _ = super::recovery::fence_session(
                        db,
                        bus,
                        session,
                        FenceReason::ProbeAmbiguous {
                            detail: detail.clone(),
                        },
                    )
                    .await;
                    return Err(WorkerError::Failed(detail.clone()));
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
                    session.lifecycle = SessionLifecycle::Idle;
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

        if let Ok(Some(current)) =
            tidebreak_core::db::code::get_turn(db, &session.owner, turn.id).await
        {
            turn = current;
        }
        super::checkpoint::after_turn_ended(db, bus, session, &mut turn).await;
        record_turn_pull_request_acts(sink, session, turn.id).await;
        if let Some(detail) = attachment_cleanup_error {
            let _ = super::recovery::fence_session(
                db,
                bus,
                session,
                FenceReason::ProbeAmbiguous {
                    detail: detail.clone(),
                },
            )
            .await;
            return Err(WorkerError::Failed(detail));
        }
        if session_was_ended(db, session).await {
            return Ok(turn);
        }
        if turn.status == TurnStatus::Interrupted {
            super::attention::replace_attention(
                session,
                Attention::needs_you("the turn was interrupted", AttentionSource::Lifecycle),
                false,
            );
        } else if turn.status == TurnStatus::Failed {
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
        session.lifecycle = SessionLifecycle::Idle;
        let _ = super::attention::persist_session(db, bus, session).await;
        Ok(turn)
    })
}

/// Sweep crash-leftover attachment scopes while the caller holds the worktree.
///
/// A failed sweep leaves private bytes behind. Persist the fence before
/// returning so another turn cannot reuse the same private root.
fn sweep_attachment_leftovers_or_fence<'fut>(
    db: &'fut DbStore,
    bus: &'fut CodeEventBus,
    session: &'fut mut Session,
    private_root: &'fut super::scratch::ScratchRoot,
) -> BoxFuture<'fut, Result<(), WorkerError>> {
    Box::pin(async move {
        let Err(error) = super::scratch::sweep_scopes(private_root, ATTACHMENTS_DIR) else {
            return Ok(());
        };
        let detail = format!("sweep attachments: {error}");
        super::recovery::fence_session(
            db,
            bus,
            session,
            FenceReason::ProbeAmbiguous {
                detail: detail.clone(),
            },
        )
        .await
        .map_err(|fence| {
            WorkerError::Failed(format!(
                "{detail}; could not persist cleanup fence: {fence}"
            ))
        })?;
        Err(WorkerError::Failed(detail))
    })
}

fn code_turn_outcome(result: &Result<Turn, WorkerError>) -> &'static str {
    match result {
        Ok(turn) => turn.status.as_str(),
        Err(WorkerError::Conflict(_)) => "conflict",
        Err(WorkerError::NoActiveTurn(_)) => "no_active_turn",
        Err(WorkerError::StaleTurn(_)) => "stale_turn",
        Err(WorkerError::SteeringUnavailable(_)) => "steering_unavailable",
        Err(WorkerError::SteeringRejected(_)) => "steering_rejected",
        Err(WorkerError::ApprovalDeliveryFailed(_)) => "approval_delivery_failed",
        Err(WorkerError::ApprovalDeliveryUnknown(_)) => "approval_delivery_unknown",
        Err(WorkerError::RelaunchRequired(_)) => "relaunch_required",
        Err(WorkerError::QueuedTurnStale) => "queued_turn_stale",
        Err(WorkerError::QueuedTurnStopped) => "queued_turn_stopped",
        Err(WorkerError::Failed(_)) => "error",
        Err(WorkerError::TriggerDeliveryAccepted) => "trigger_delivery_accepted",
        Err(WorkerError::WorktreeBusy) => "worktree_busy",
        Err(WorkerError::UpdateQuiesced) => "update_quiesced",
    }
}

fn code_turn_is_error(result: &Result<Turn, WorkerError>) -> bool {
    matches!(
        result,
        Ok(Turn {
            status: TurnStatus::Failed,
            ..
        }) | Err(WorkerError::Failed(_))
    )
}

/// Directory holding a turn's attachments below the workspace's private root.
pub(crate) const ATTACHMENTS_DIR: &str = "attachments";

struct StagedTurnAttachments {
    scope: super::scratch::ScratchScope,
    paths: Vec<String>,
}

/// Write a turn's attachments outside the checkout, returning absolute paths
/// in the order they were attached.
///
/// The session and turn path prevents a later session from inheriting files
/// from this one. The returned scope removes the directory on every exit.
async fn write_turn_attachments(
    private_root: &super::scratch::ScratchRoot,
    session_id: SessionId,
    turn_id: TurnId,
    attachments: &[tidebreak_core::ImageRef],
    images: &[TurnImage],
) -> std::io::Result<StagedTurnAttachments> {
    let scope =
        super::scratch::scratch_scope(private_root, ATTACHMENTS_DIR, session_id.0, turn_id.0)?;
    let mut written = Vec::with_capacity(images.len());
    for (attachment, image) in attachments.iter().zip(images) {
        let name = format!(
            "{}.{}",
            attachment.blob_id,
            attachment.media_type.extension()
        );
        scope
            .publish(std::ffi::OsStr::new(&name), &image.bytes)
            .await?;
        written.push(
            private_root
                .path()
                .join(ATTACHMENTS_DIR)
                .join(session_id.to_string())
                .join(turn_id.to_string())
                .join(name)
                .display()
                .to_string(),
        );
    }
    Ok(StagedTurnAttachments {
        scope,
        paths: written,
    })
}

/// Name the attachment paths after the message, the way a fork names the
/// transcript it wrote.
///
/// The engine reads them from disk, so the prompt carries paths and nothing
/// else. An empty list leaves the message exactly as the reader wrote it.
/// What the engine must hear before turn `ordinal`: which files a checkpoint
/// restore moved since its last turn. `None` when nothing moved, or when this
/// session has no local worktree.
async fn restore_note_for_turn(
    db: &tidebreak_core::DbStore,
    session: &Session,
    ordinal: i64,
) -> Option<String> {
    let workspace_id = session.workspace_id?;
    let workspace = get_workspace(db, &session.owner, workspace_id)
        .await
        .ok()
        .flatten()?;
    if workspace.is_remote() {
        return None;
    }
    match super::checkpoint::restore_note(
        std::path::Path::new(&workspace.worktree_path),
        workspace_id,
        session.id,
        ordinal,
    )
    .await
    {
        Ok(note) => note,
        Err(err) => {
            warn!(
                session = %session.id,
                error = %err,
                "could not tell the engine what a restore moved"
            );
            None
        }
    }
}

fn message_naming_attachments(message: &str, paths: &[String]) -> String {
    if paths.is_empty() {
        return message.to_owned();
    }
    let list = paths
        .iter()
        .map(|path| format!("- `{path}`"))
        .collect::<Vec<_>>()
        .join("\n");
    let noun = if paths.len() == 1 { "image" } else { "images" };
    let body = format!("{noun} attached to this message:\n{list}");
    let text = message.trim();
    if text.is_empty() {
        body
    } else {
        format!("{text}\n\n{body}")
    }
}

async fn hydrate_turn_images(
    blobs: Option<&dyn BlobStore>,
    attachments: &[tidebreak_core::ImageRef],
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
    session: &Session,
    turn: &Turn,
) -> Option<FenceReason> {
    let turns = tidebreak_core::db::code::list_turns(db, &session.owner, session.id)
        .await
        .ok()?;
    let mut count = 0u32;
    for candidate in turns.iter().rev() {
        match candidate.status {
            TurnStatus::Failed => count += 1,
            // A turn still running is the one we are closing out; anything
            // else ends the streak.
            TurnStatus::Running if candidate.id == turn.id => count += 1,
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
async fn last_failure_detail(db: &DbStore, session: &Session) -> Option<String> {
    let events = tidebreak_core::db::code::list_recent_events(db, &session.owner, session.id, 64)
        .await
        .ok()?;
    events.into_iter().find_map(|item| match item.event {
        Event::TurnFailed { error, .. } => Some(error.message),
        _ => None,
    })
}

async fn session_was_ended(db: &DbStore, session: &mut Session) -> bool {
    match get_session(db, &session.owner, session.id).await {
        Ok(Some(current)) if current.lifecycle == SessionLifecycle::Ended => {
            *session = current;
            true
        }
        _ => false,
    }
}

/// Bump the spawn epoch and record the process metadata.
///
/// Call this once, before the engine is launched, so the event sink and the
/// session row share the same epoch. Pass `child_pid` after launch via
/// [`save_session`] if the adapter only exposes a pid later. Codex reports
/// `SessionStarted` after its thread exists, so its parser journals that event
/// with the resume ref. Other adapters keep the eager event at attachment.
pub(crate) async fn attach_engine(
    db: &DbStore,
    bus: &CodeEventBus,
    session_id: SessionId,
    kind: tidebreak_core::HarnessKind,
    version: Option<String>,
    child_pid: Option<i64>,
) -> Result<Session, WorkerError> {
    let epoch = bump_spawn_epoch(db, session_id, child_pid)
        .await
        .map_err(|err| WorkerError::Failed(err.to_string()))?;
    let mut session = get_session_all_owners(db, session_id)
        .await
        .map_err(|err| WorkerError::Failed(err.to_string()))?
        .ok_or_else(|| WorkerError::Failed(format!("session {session_id} not found")))?;
    session.spawn_epoch = epoch;
    session.child_pid = child_pid;
    session.child_process_identity = None;
    session.harness_version = version.or(session.harness_version);
    session.lifecycle = SessionLifecycle::Idle;
    // Attaching an engine makes it ready for a turn, not active. Restore
    // automatic active attention left by an earlier attachment. Preserve
    // unread work and user pins.
    if session.attention.source.is_automatic()
        && matches!(
            session.attention.state,
            tidebreak_core::AttentionState::Working
                | tidebreak_core::AttentionState::Stalled { .. }
        )
    {
        let restored = super::attention::compute_attention(
            db,
            bus,
            &session,
            super::attention::ComputeOpts {
                reviewed: true,
                ..Default::default()
            },
        )
        .await
        .map_err(|err| WorkerError::Failed(err.to_string()))?;
        super::attention::replace_attention(&mut session, restored, false);
    }
    super::attention::persist_session(db, bus, &session)
        .await
        .map_err(|err| WorkerError::Failed(err.to_string()))?;
    if kind != tidebreak_core::HarnessKind::Codex {
        persist_and_publish(
            db,
            bus,
            &session.owner,
            session_id,
            epoch,
            Event::SessionStarted {
                harness_kind: kind,
                harness_version: session
                    .harness_version
                    .clone()
                    .unwrap_or_else(|| "unknown".into()),
                resume_ref: session.harness_resume_ref.clone(),
            },
            false,
        )
        .await
        .map_err(|err| WorkerError::Failed(err.to_string()))?;
    }
    Ok(session)
}

/// Whether an engine's failure message reads as the provider refusing its
/// credentials, as opposed to any other turn failure. Matches the raw 401
/// bodies and sign-in errors the shipped engines actually print; anything
/// unrecognized passes through untouched.
fn provider_auth_failure(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        // OpenAI's raw 401 body, what a signed-out Codex prints.
        "missing bearer or basic authentication",
        // The error type the vendors' 401 bodies carry.
        "authentication_error",
        // What a signed-out Claude Code prints in its result line.
        "invalid api key",
        "please run /login",
        // Anthropic's refusal of a rejected bearer.
        "invalid bearer token",
        "401 unauthorized",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn sink_for(
    db: Arc<DbStore>,
    bus: Arc<CodeEventBus>,
    owner: OwnerId,
    session_id: SessionId,
    spawn_epoch: i64,
    harness: HarnessKind,
    relay_wired: bool,
    turn_id: Option<TurnId>,
    subagents: Vec<CodeSubagentSummary>,
    gh_search_path: Option<String>,
    recap: Option<Arc<dyn super::recap::TurnRecap>>,
    rewrite: Option<Arc<dyn super::rewrite::TurnRewrite>>,
    memory_capture: Option<Arc<dyn super::memory_capture::TurnMemoryCapture>>,
    hot_prs: super::pr_refresh::HotPullRequests,
) -> Arc<LiveSink> {
    Arc::new(LiveSink {
        db,
        bus,
        owner,
        session_id,
        spawn_epoch,
        native_journal: harness == HarnessKind::Internal,
        harness,
        relay_wired,
        turn_id: std::sync::Mutex::new(turn_id),
        turn_model: std::sync::Mutex::new(None),
        pending_resume_ref: std::sync::Mutex::new(None),
        gh_search_path,
        flushed_unrecognized: AtomicU64::new(0),
        subagents: std::sync::Mutex::new(subagents),
        recap,
        rewrite,
        memory_capture,
        hot_prs,
        last_background_activity: std::sync::Mutex::new(None),
    })
}

pub(crate) fn journal_event<'a>(
    db: &'a DbStore,
    bus: &'a CodeEventBus,
    owner: &'a OwnerId,
    session_id: SessionId,
    spawn_epoch: i64,
    event: Event,
) -> BoxFuture<'a, Result<(), JournalError>> {
    persist_and_publish(db, bus, owner, session_id, spawn_epoch, event, false)
}

/// Journal one event for the session, or — when `native_journal` says the
/// engine already wrote it — apply only what the row's arrival means for
/// the worker's own state. See [`LiveSink::native_journal`].
///
/// Every journal write on the turn path comes through here, so the write is
/// boxed once, in [`persist_and_publish_inner`], and callers hold a pointer.
#[allow(clippy::too_many_arguments)]
pub(crate) fn persist_and_publish<'a>(
    db: &'a DbStore,
    bus: &'a CodeEventBus,
    owner: &'a OwnerId,
    session_id: SessionId,
    spawn_epoch: i64,
    event: Event,
    native_journal: bool,
) -> BoxFuture<'a, Result<(), JournalError>> {
    persist_and_publish_inner(
        db,
        bus,
        owner,
        session_id,
        spawn_epoch,
        None,
        event,
        native_journal,
    )
}

#[allow(clippy::too_many_arguments)]
fn persist_turn_and_publish<'a>(
    db: &'a DbStore,
    bus: &'a CodeEventBus,
    owner: &'a OwnerId,
    session_id: SessionId,
    spawn_epoch: i64,
    turn_id: TurnId,
    event: Event,
    native_journal: bool,
) -> BoxFuture<'a, Result<(), JournalError>> {
    persist_and_publish_inner(
        db,
        bus,
        owner,
        session_id,
        spawn_epoch,
        Some(turn_id),
        event,
        native_journal,
    )
}

#[allow(clippy::too_many_arguments)]
fn persist_and_publish_inner<'fut>(
    db: &'fut DbStore,
    bus: &'fut CodeEventBus,
    owner: &'fut OwnerId,
    session_id: SessionId,
    spawn_epoch: i64,
    notification_turn_id: Option<TurnId>,
    event: Event,
    native_journal: bool,
) -> BoxFuture<'fut, Result<(), JournalError>> {
    Box::pin(async move {
        if native_journal {
            // The row is already in the journal and on the bus; only the
            // worker's bookkeeping is left: the turn row closes with the usage
            // the engine reported, and a settled turn sweeps its approvals.
            apply_side_effects(db, owner, session_id, spawn_epoch, &event).await?;
            if matches!(
                &event,
                Event::TurnCompleted { .. }
                    | Event::TurnRefused { .. }
                    | Event::TurnFailed { .. }
                    | Event::TurnInterrupted { .. }
            ) {
                super::approval_sweep::abandon_for_settled_turns(
                    db,
                    bus,
                    owner,
                    session_id,
                    spawn_epoch,
                )
                .await;
            }
            return Ok(());
        }
        settle_streamed_text(db, bus, owner, session_id, spawn_epoch, &event).await;
        let activity_boundary = matches!(
            &event,
            Event::ToolStarted {
                parent_call_id: None,
                ..
            } | Event::ToolCompleted {
                parent_call_id: None,
                ..
            }
        );
        apply_side_effects(db, owner, session_id, spawn_epoch, &event).await?;
        let seq = if let Some(turn_id) = notification_turn_id {
            append_event_with_notification(db, owner, session_id, spawn_epoch, turn_id, &event)
                .await?
        } else {
            append_event(db, owner, session_id, spawn_epoch, &event).await?
        };
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
            Event::TurnCompleted { .. }
                | Event::TurnRefused { .. }
                | Event::TurnFailed { .. }
                | Event::TurnInterrupted { .. }
        );
        bus.publish(
            session_id,
            tidebreak_core::code::SequencedEvent { seq, event },
        );
        if closes_turn {
            super::approval_sweep::abandon_for_settled_turns(
                db,
                bus,
                owner,
                session_id,
                spawn_epoch,
            )
            .await;
        }
        if activity_boundary {
            if let Ok(Some(session)) = get_session(db, owner, session_id).await {
                super::attention::emit_digest(db, bus, &session).await;
            }
        }
        Ok(())
    })
}

/// Write down assistant text the engine streamed but never stated.
///
/// Deltas are live-only, so the `assistant_message` closing a run is what the
/// journal keeps. A turn that ends mid-sentence — interrupted, or failed —
/// never produces that message, and the words the reader watched arrive would
/// otherwise be gone on reload. Synthesizing the message the engine did not
/// send is safe where replaying the deltas would not be: the renderer and the
/// CLI both treat a message as a *replacement* for the text they streamed, so
/// a client that already has it shows it once.
///
/// Everything else that ends a run — the engine's own message, a parent-level
/// tool call, the next turn — already carries the text or discards it, and
/// [`CodeEventBus::publish`] retires the buffer when it goes by.
///
/// Best-effort on purpose: a recovery write that fails must not stop the
/// terminal event that follows it from being journaled.
fn settle_streamed_text<'fut>(
    db: &'fut DbStore,
    bus: &'fut CodeEventBus,
    owner: &'fut OwnerId,
    session_id: SessionId,
    spawn_epoch: i64,
    event: &'fut Event,
) -> BoxFuture<'fut, ()> {
    Box::pin(async move {
        if !matches!(
            event,
            Event::TurnCompleted { .. }
                | Event::TurnRefused { .. }
                | Event::TurnFailed { .. }
                | Event::TurnInterrupted { .. }
        ) {
            return;
        }
        let streamed = bus.take_assistant_tail(session_id);
        if streamed.is_empty() {
            return;
        }
        let recovered = Event::AssistantMessage {
            text: streamed,
            parent_call_id: None,
        };
        match append_event(db, owner, session_id, spawn_epoch, &recovered).await {
            Ok(seq) => bus.publish(
                session_id,
                tidebreak_core::code::SequencedEvent {
                    seq,
                    event: recovered,
                },
            ),
            Err(err) => warn!(
                session = %session_id,
                error = %err,
                "could not journal the text a turn streamed before it ended"
            ),
        }
    })
}

fn is_activity(event: &Event) -> bool {
    matches!(
        event,
        Event::AssistantMessage { .. }
            | Event::ReasoningDelta { .. }
            | Event::ToolStarted { .. }
            | Event::ToolCompleted { .. }
            | Event::FileChanged { .. }
            | Event::ApprovalRequested { .. }
            | Event::UserSteered { .. }
    )
}

async fn apply_side_effects(
    db: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    _spawn_epoch: i64,
    event: &Event,
) -> Result<(), JournalError> {
    match event {
        Event::TurnCompleted {
            usage, checkpoint, ..
        } => {
            if let Ok(Some(mut turn)) = get_open_turn(db, owner, session_id).await {
                turn.status = TurnStatus::Completed;
                turn.ended_at = Some(Utc::now());
                turn.usage = Some(usage.clone());
                if let Some(hint) = checkpoint {
                    turn.checkpoint_ref = hint.checkpoint_ref.clone();
                    turn.diffstat = hint.diffstat.clone();
                }
                save_terminal_turn(db, owner, session_id, &turn).await?;
            }
        }
        // A refusal ends the turn the way a completion does: the model
        // answered, and its answer was to decline.
        Event::TurnRefused { usage, .. } => {
            if let Ok(Some(mut turn)) = get_open_turn(db, owner, session_id).await {
                turn.status = TurnStatus::Completed;
                turn.ended_at = Some(Utc::now());
                turn.usage = Some(usage.clone());
                save_terminal_turn(db, owner, session_id, &turn).await?;
            }
        }
        Event::TurnFailed { .. } => {
            if let Ok(Some(mut turn)) = get_open_turn(db, owner, session_id).await {
                turn.status = TurnStatus::Failed;
                turn.ended_at = Some(Utc::now());
                save_terminal_turn(db, owner, session_id, &turn).await?;
            }
        }
        Event::TurnInterrupted { .. } => {
            if let Ok(Some(mut turn)) = get_open_turn(db, owner, session_id).await {
                turn.status = TurnStatus::Interrupted;
                turn.ended_at = Some(Utc::now());
                save_terminal_turn(db, owner, session_id, &turn).await?;
            }
        }
        _ => {}
    }
    Ok(())
}

async fn save_terminal_turn(
    db: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    turn: &Turn,
) -> Result<(), JournalError> {
    match save_turn(db, owner, turn).await {
        Ok(true) => Ok(()),
        Ok(false) => {
            error!(
                session = %session_id,
                turn = %turn.id,
                status = %turn.status.as_str(),
                "terminal event could not close its turn because the row disappeared"
            );
            Err(JournalError::SessionNotFound { session_id })
        }
        Err(error) => {
            error!(
                session = %session_id,
                turn = %turn.id,
                status = %turn.status.as_str(),
                error = %error,
                "terminal event could not close its turn"
            );
            Err(JournalError::Store(error))
        }
    }
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

fn kind_from_raw(raw: &serde_json::Value) -> ApprovalKind {
    tidebreak_core::code::supervisor_tools::native_approval_kind(raw)
}

fn map_event(event: HarnessEvent, turn_id: Option<TurnId>) -> Option<Event> {
    Some(match event {
        HarnessEvent::SessionStarted {
            harness_kind,
            harness_version,
            resume_ref,
        } => Event::SessionStarted {
            harness_kind,
            harness_version,
            resume_ref,
        },
        HarnessEvent::ModelReported { model } => Event::ModelReported { model },
        HarnessEvent::TurnStarted => Event::TurnStarted { turn_id: turn_id? },
        HarnessEvent::AssistantDelta { text } => Event::AssistantDelta { text },
        HarnessEvent::AssistantMessage {
            text,
            parent_call_id,
        } => Event::AssistantMessage {
            text,
            parent_call_id,
        },
        HarnessEvent::ReasoningDelta { text } => Event::ReasoningDelta { text },
        HarnessEvent::ToolStarted {
            call_id,
            name,
            detail,
            parent_call_id,
        } => Event::ToolStarted {
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
        } => Event::ToolCompleted {
            call_id,
            outcome,
            preview,
            output: None,
            action: None,
            result: None,
            detail,
            parent_call_id,
        },
        HarnessEvent::FileChanged {
            path,
            kind,
            diffstat,
        } => Event::FileChanged {
            path,
            kind,
            diffstat,
        },
        HarnessEvent::ApprovalRequested { .. } => {
            return None;
        }
        HarnessEvent::ApprovalResolved { decision, .. } => Event::ApprovalResolved {
            approval_id: ApprovalId::new(),
            decision: decision.into(),
            actor: None,
        },
        HarnessEvent::UserSteered { text, .. } => Event::UserSteered {
            text,
            message_id: None,
        },
        HarnessEvent::TurnCompleted { usage } => Event::TurnCompleted {
            usage,
            checkpoint: None,
            stop_reason: None,
        },
        HarnessEvent::TurnFailed { error } => Event::TurnFailed {
            error,
            detail: None,
        },
        HarnessEvent::TurnInterrupted => Event::TurnInterrupted { usage: None },
        HarnessEvent::HarnessNotice { level, message } => Event::HarnessNotice { level, message },
        // Work the engine did on its own belongs to no turn, whichever turn
        // is open while it arrives.
        HarnessEvent::BackgroundActivity { event } => Event::BackgroundActivity {
            event: Box::new(map_event(*event, None)?),
        },
    })
}

impl From<HarnessError> for WorkerError {
    fn from(err: HarnessError) -> Self {
        Self::Failed(err.to_string())
    }
}

#[cfg(test)]
mod tests;
