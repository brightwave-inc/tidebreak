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
    accept_trigger_turn_delivery, append_event, append_event_with_notification,
    begin_permission_mode_change, bump_spawn_epoch, cancel_permission_mode_change,
    confirm_permission_mode_change, delete_queued_turn, get_open_turn, get_session,
    get_session_all_owners, get_workspace, insert_approval_for_worker, insert_turn, list_approvals,
    next_turn_ordinal, promote_queued_turn, queue_paused, queued_turn_head,
    rebind_pending_approvals_to_worker, save_session, save_turn, set_queue_paused,
    set_session_harness_resume_ref, set_session_subagents, settle_engine_observed_approval,
    CodeJournalError, CodeSessionExecutionSettings,
};
use tidebreak_core::{
    bound_subagents, Attention, AttentionSource, BlobStore, BoundedError, CodeApproval,
    CodeApprovalId, CodeApprovalKind, CodeApprovalState, CodeEvent, CodeQueuedTurn, CodeSession,
    CodeSessionId, CodeSessionLifecycle, CodeSubagentStatus, CodeSubagentSummary, CodeTurn,
    CodeTurnId, CodeTurnStatus, CodeWorkspaceStatus, DbStore, FenceReason, HarnessKind,
    HarnessNoticeLevel, OwnerId, PermissionMode, ToolOutcome,
};
use tidebreak_harness::{
    ApprovalDecision, HarnessApprovalRef, HarnessError, HarnessEvent, HarnessEventSink,
    HarnessSession, TurnImage, TurnInput, TurnOutcome,
};

use super::bus::{CodeEventBus, CodeLiveEvent};

pub(crate) enum WorkerCommand {
    RunTurn {
        message: String,
        attachments: Vec<tidebreak_core::ImageRef>,
        trigger_delivery: Option<TriggerDeliveryClaim>,
        reply: oneshot::Sender<Result<CodeTurn, WorkerError>>,
    },
    SetPermissionMode {
        mode: PermissionMode,
        settlement: oneshot::Receiver<PermissionModeSettlement>,
        reply: oneshot::Sender<Result<(), WorkerError>>,
    },
    SetExecutionSettings {
        settings: CodeSessionExecutionSettings,
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
        expected_turn_id: CodeTurnId,
        message: String,
        reply: oneshot::Sender<Result<(), WorkerError>>,
    },
    Shutdown,
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

pub(crate) struct WorkerHandle {
    pub spawn_epoch: i64,
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

pub(crate) struct QueuedFollowUp {
    pub message: String,
    pub attachments: Vec<tidebreak_core::ImageRef>,
    pub trigger_delivery: Option<TriggerDeliveryClaim>,
    /// The durable queue row this turn promotes, when the message came from
    /// the queue rather than a live send. The turn is inserted under the
    /// row's id, in the transaction that deletes the row.
    pub queued_row: Option<Box<CodeQueuedTurn>>,
}

pub(crate) struct LiveSink {
    db: Arc<DbStore>,
    bus: Arc<CodeEventBus>,
    /// Owner of the session this sink writes for. Carried explicitly so every
    /// journal and approval write the worker makes stays inside one owner.
    owner: OwnerId,
    session_id: CodeSessionId,
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
    turn_id: std::sync::Mutex<Option<CodeTurnId>>,
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
    /// The runtime's hot pull-request tier (decision 66). A turn whose fact
    /// detector confirms a push or a create marks this workspace, so the
    /// next hot pass reads the head the turn just moved (issue 2799).
    hot_prs: super::pr_refresh::HotPullRequests,
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

    /// The engine's report of a failed turn, made legible when it is a
    /// provider authentication failure (issue 2653): the vendor's raw 401
    /// body cannot tell the reader "sign this harness in" from "the
    /// provider is down", so the sentence the create-time refusal uses
    /// leads and the engine's own words follow. A turn riding the relay
    /// keeps its message untouched — the relay's refusals already name the
    /// gateway, and "sign in in your own terminal" is wrong on a hosted
    /// machine.
    fn legible_turn_error(&self, message: String) -> BoundedError {
        if self.relay_wired || !provider_auth_failure(&message) {
            return BoundedError { message };
        }
        let label = crate::code::harness_label(self.harness);
        BoundedError {
            message: format!(
                "{label} is not signed in on this machine. Sign in to {label} in your own \
                 terminal, then try again. The engine reported: {message}"
            ),
        }
    }

    /// Persist a reported resume ref after the engine proves a turn started.
    async fn persist_pending_resume_ref(&self) {
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
                    Some(entry) if entry.status == CodeSubagentStatus::Running => {
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
                    Some(_) | None => None,
                }
            }
            CodeEvent::TurnCompleted { .. } | CodeEvent::TurnRefused { .. } => {
                let mut subagents = self.subagents.lock().expect("code sink subagents");
                settle_running_subagents(&mut subagents, CodeSubagentStatus::Done)
                    .then(|| subagents.clone())
            }
            CodeEvent::TurnFailed { .. } | CodeEvent::TurnInterrupted { .. } => {
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

    pub(crate) async fn record_external_approval(
        &self,
        approval_id: CodeApprovalId,
        harness_ref: &tidebreak_harness::HarnessApprovalRef,
        raw: &serde_json::Value,
    ) -> Result<CodeApproval, WorkerError> {
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
    }

    /// Settle a pending approval the engine decided itself, when one is
    /// still waiting under this call id.
    async fn settle_engine_observed(&self, call_id: &str, decision: ApprovalDecision) {
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
    }

    async fn record_approval(
        &self,
        approval_id: CodeApprovalId,
        harness_ref: &tidebreak_harness::HarnessApprovalRef,
        raw: &serde_json::Value,
        kind: Option<&tidebreak_core::CodeApprovalKind>,
    ) -> Result<CodeApproval, WorkerError> {
        let existing = *self.turn_id.lock().expect("code sink turn");
        let turn_id = match existing {
            Some(id) => id,
            None => match get_open_turn(&self.db, &self.owner, self.session_id).await {
                Ok(Some(turn)) => turn.id,
                Ok(None) => {
                    return Err(WorkerError::Failed(format!(
                        "session {} has no open turn for approval {approval_id}",
                        self.session_id
                    )))
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
        let approval = CodeApproval {
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
            state: CodeApprovalState::Pending,
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
        let _ = super::attention::note_activity(&self.db, &self.bus, &self.owner, self.session_id)
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
                .record_approval(CodeApprovalId::new(), harness_ref, raw, kind.as_ref())
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
        let Some(code_event) = map_event(event, turn_id) else {
            return;
        };
        // An engine that fails a turn can pass the provider's raw 401 body
        // straight through. Swap it for the legible sentence before the
        // journal keeps it.
        let code_event = match code_event {
            CodeEvent::TurnFailed { error, .. } => CodeEvent::TurnFailed {
                error: self.legible_turn_error(error.message),
                detail: None,
            },
            other => other,
        };
        // Assistant deltas stream and are gone. The `assistant_message` that
        // closes the run repeats them exactly, so a row here would store the
        // same words a second time (record 57).
        if matches!(code_event, CodeEvent::AssistantDelta { .. }) {
            if self.native_journal {
                return;
            }
            self.bus.publish_transient(self.session_id, code_event);
            let _ =
                super::attention::note_activity(&self.db, &self.bus, &self.owner, self.session_id)
                    .await;
            return;
        }
        self.note_subagent_boundary(&code_event).await;
        // An approval is parked on the call this completion names. Reconcile
        // it after the completion lands, so the journal reads in the order it
        // happened. See `approval_sweep`.
        let completed_call = match &code_event {
            CodeEvent::ToolCompleted { call_id, .. } => Some(call_id.clone()),
            _ => None,
        };
        // A completed turn is the moment its recap has everything it needs and
        // the reader has most likely stopped watching. Started below rather
        // than here, so a journal write that was dropped never produces a line
        // describing a turn the database does not agree finished.
        let completed_turn = matches!(code_event, CodeEvent::TurnCompleted { .. })
            .then_some(turn_id)
            .flatten();
        let notification_turn = matches!(
            &code_event,
            CodeEvent::TurnCompleted { .. } | CodeEvent::TurnFailed { .. }
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
                code_event,
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
                code_event,
                self.native_journal,
            )
            .await
        };
        let journaled = match write {
            Ok(()) => !self.native_journal,
            Err(CodeJournalError::StaleSpawnEpoch { .. }) => {
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
        commands: tx,
        queue,
        sink,
        approval_decisions,
        abort: task.abort_handle(),
    }
}

fn record_child_process(session: &mut CodeSession, pid: Option<i64>) {
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
    mut session: CodeSession,
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
        if open.status == CodeTurnStatus::Waiting
            && open.park_ref.is_some()
            && open.park_wait.is_some()
        {
            if let Err(error) = continue_parked_turn(
                &mut session,
                engine.as_ref(),
                &sink,
                &mut commands,
                &queue,
                open,
            )
            .await
            {
                warn!(
                    session = %session.id,
                    error = %error,
                    "could not resume the parked turn after a worker restart"
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
            drain_queued(
                &mut session,
                engine.as_ref(),
                &sink,
                &queue,
                &store,
                &mut commands,
            )
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
                    trigger_delivery,
                    reply,
                }) => {
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
                            trigger_delivery,
                            queued_row: None,
                        },
                    )
                    .await;
                    let _ = reply.send(result);
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

#[cfg(not(test))]
const STEER_CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
const STEER_CONTROL_TIMEOUT: Duration = Duration::from_millis(100);
#[cfg(not(test))]
const APPROVAL_CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
const APPROVAL_CONTROL_TIMEOUT: Duration = Duration::from_secs(1);

/// How long a session sits between turns — no turn, no queued follow-up, no
/// command — before its engine child is released (decision 0064). Sits well
/// past the documented provider prompt-cache TTL, so a wake in practice pays
/// spawn latency and not tokens a warm child would have saved.
#[cfg(not(test))]
const PARK_AFTER_IDLE: Duration = Duration::from_secs(15 * 60);
#[cfg(test)]
const PARK_AFTER_IDLE: Duration = Duration::from_millis(150);

/// Retry cadence for a park that failed while an update quiesce is waiting
/// on this session. Outside a quiesce a failed park just waits for the next
/// idle window.
#[cfg(not(test))]
const QUIESCE_PARK_RETRY: Duration = Duration::from_millis(500);
#[cfg(test)]
const QUIESCE_PARK_RETRY: Duration = Duration::from_millis(50);

/// Release an idle engine child (decision 0064) and clear the row's pid so
/// nothing reads the dead process as live. The next turn respawns and
/// resumes. A park that fails keeps the child and simply retries on the next
/// idle window.
async fn park_idle_engine(session: &mut CodeSession, engine: &dyn HarnessSession, sink: &LiveSink) {
    if let Err(error) = engine.park().await {
        warn!(
            session = %session.id,
            error = %error,
            "could not park the idle engine child"
        );
        return;
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
    session: &mut CodeSession,
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
    session: &mut CodeSession,
    settings: CodeSessionExecutionSettings,
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
async fn persist_turn_park(
    db: &DbStore,
    session: &CodeSession,
    turn: &mut CodeTurn,
    park_ref: &str,
    waiting_on: &tidebreak_harness::ParkWait,
) -> Result<tidebreak_core::TurnParkWait, String> {
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
    };
    turn.status = CodeTurnStatus::Waiting;
    turn.park_ref = Some(park_ref.to_owned());
    turn.park_wait = Some(wait.clone());
    save_turn(db, &session.owner, turn)
        .await
        .map_err(|err| format!("persisting the parked turn failed: {err}"))?;
    Ok(wait)
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
async fn apply_accepted_plan_mode(
    db: &DbStore,
    bus: &CodeEventBus,
    session: &mut CodeSession,
    engine: &dyn HarnessSession,
    input: &tidebreak_harness::ResumeInput,
) {
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
                (Some(native), CodeApprovalKind::Plan { proposed_mode }) if native == call_id => {
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
                CodeEvent::HarnessNotice {
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
                CodeEvent::HarnessNotice {
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
    session_id: CodeSessionId,
    call_id: &str,
) -> Option<tidebreak_harness::ResumeInput> {
    let approval = list_approvals(db, owner, None, Some(session_id))
        .await
        .ok()?
        .into_iter()
        .find(|approval| approval.native_call_id.as_deref() == Some(call_id))?;
    if approval.state.is_pending() {
        return None;
    }
    let journaled = tidebreak_core::db::code::list_recent_events(db, owner, session_id, 256)
        .await
        .ok()?
        .into_iter()
        .find_map(|row| match row.event {
            CodeEvent::ApprovalResolved {
                approval_id,
                decision,
            } if approval_id == approval.id => Some(decision),
            _ => None,
        });
    let decision = match journaled {
        Some(decision) => decision,
        None => match approval.state {
            CodeApprovalState::Approved => tidebreak_core::ApprovalDecisionKind::Approve,
            CodeApprovalState::Denied => tidebreak_core::ApprovalDecisionKind::Deny {
                feedback: approval.feedback,
            },
            CodeApprovalState::Abandoned | CodeApprovalState::Pending => {
                tidebreak_core::ApprovalDecisionKind::Abandoned
            }
        },
    };
    Some(tidebreak_harness::ResumeInput::ApprovalDecided {
        call_id: call_id.to_owned(),
        decision: resume_decision(decision),
    })
}

#[allow(clippy::too_many_arguments)]
async fn await_park_resolution<'a>(
    engine: &'a dyn HarnessSession,
    db: &DbStore,
    bus: &CodeEventBus,
    session: &CodeSession,
    commands: &mut mpsc::Receiver<WorkerCommand>,
    controls: &mut FuturesUnordered<BoxFuture<'a, ControlFlow>>,
    interrupted: &mut bool,
    commands_closed: &mut bool,
    wait: &tidebreak_core::TurnParkWait,
    turn_id: CodeTurnId,
    delivered: &DeliveredDecisions,
) -> Option<ParkResolution> {
    if let Some(input) = resume_if_already_delivered(wait, delivered) {
        return Some(ParkResolution {
            input,
            delivered_here: true,
        });
    }
    // Subscribe before the read below, so a settlement between the two
    // cannot slip past both.
    let (mut live, _tail) = bus.attach(session.id);
    let waited_call = match wait {
        tidebreak_core::TurnParkWait::Approval { call_id } => Some(call_id.clone()),
        _ => None,
    };
    if let Some(call_id) = waited_call.as_deref() {
        if let Some(input) = resume_from_settled_row(db, &session.owner, session.id, call_id).await
        {
            return Some(ParkResolution {
                input,
                delivered_here: false,
            });
        }
    }
    loop {
        tokio::select! {
            biased;
            Some(flow) = controls.next(), if !controls.is_empty() => {
                if flow == ControlFlow::Shutdown {
                    *interrupted = true;
                    return None;
                }
                // The opening leg may still be delivering the awaited
                // decision when the engine parks. That future lives in
                // `controls`; once it finishes, resume from the record.
                if let Some(input) = resume_if_already_delivered(wait, delivered) {
                    return Some(ParkResolution {
                        input,
                        delivered_here: true,
                    });
                }
            }
            published = live.recv(), if waited_call.is_some() => {
                let call_id = waited_call.as_deref().unwrap_or_default();
                let settled = match published {
                    Ok(CodeLiveEvent {
                        event: CodeEvent::ApprovalResolved { approval_id, .. },
                        ..
                    }) => {
                        // The row the resolution names must be the one this
                        // park waits on; another card's decision is not it.
                        matches!(
                            tidebreak_core::db::code::get_approval(db, &session.owner, approval_id).await,
                            Ok(Some(approval)) if approval.native_call_id.as_deref() == Some(call_id)
                        )
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => true,
                    _ => false,
                };
                if settled {
                    if let Some(input) =
                        resume_from_settled_row(db, &session.owner, session.id, call_id).await
                    {
                        return Some(ParkResolution {
                            input,
                            delivered_here: false,
                        });
                    }
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
                        return Some(ParkResolution {
                            input: tidebreak_harness::ResumeInput::ApprovalDecided {
                                call_id,
                                decision: decided,
                            },
                            delivered_here: true,
                        });
                    }
                }
                Some(WorkerCommand::Interrupt { reply }) => {
                    *interrupted = true;
                    let result = engine
                        .interrupt()
                        .await
                        .map_err(|err| WorkerError::Failed(err.to_string()));
                    let _ = reply.send(result);
                    return None;
                }
                Some(WorkerCommand::Shutdown) => {
                    *interrupted = true;
                    return None;
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
                    return None;
                }
            },
        }
    }
}

async fn apply_control(
    engine: &dyn HarnessSession,
    command: WorkerCommand,
    active_turn_id: Option<CodeTurnId>,
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
    session: &mut CodeSession,
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
            trigger_delivery: None,
            queued_row: Some(Box::new(head.clone())),
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
                    CodeEvent::HarnessNotice {
                        level: HarnessNoticeLevel::Error,
                        message: format!("The queued turn could not start: {detail}"),
                    },
                    false,
                )
                .await;
                if session.lifecycle != CodeSessionLifecycle::Fenced {
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
    session: &mut CodeSession,
    engine: &dyn HarnessSession,
    sink: &LiveSink,
    commands: &mut mpsc::Receiver<WorkerCommand>,
    queue: &TurnQueue,
    mut turn: CodeTurn,
) -> Result<CodeTurn, WorkerError> {
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

    session.lifecycle = CodeSessionLifecycle::Running;
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
        CodeEvent::TurnResumed { turn_id: turn.id },
        false,
    )
    .await
    .map_err(|err| WorkerError::Failed(err.to_string()))?;

    let mut next_resume: Option<(String, tidebreak_harness::ResumeInput)>;
    let mut pid_changes = engine.child_pid_changes();
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
        &wait,
        turn.id,
        &delivered,
    )
    .await
    {
        Some(ParkResolution {
            input,
            delivered_here,
        }) => {
            if delivered_here {
                apply_accepted_plan_mode(db, bus, session, engine, &input).await;
            }
            turn.status = CodeTurnStatus::Running;
            turn.park_ref = None;
            turn.park_wait = None;
            let _ = save_turn(db, &session.owner, &turn).await;
            next_resume = Some((park_ref, input));
        }
        None => {
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
                        let _ = save_session(db, session).await;
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
            &wait,
            turn.id,
            &delivered,
        )
        .await
        {
            Some(ParkResolution {
                input,
                delivered_here,
            }) => {
                if delivered_here {
                    apply_accepted_plan_mode(db, bus, session, engine, &input).await;
                }
                turn.status = CodeTurnStatus::Running;
                turn.park_ref = None;
                turn.park_wait = None;
                let _ = save_turn(db, &session.owner, &turn).await;
                next_resume = Some((park_ref, input));
            }
            None => break 'legs Ok(TurnOutcome::Clean),
        }
    };
    while controls.next().await.is_some() {}
    close_open_turn(session, engine, sink, turn, run, interrupted, None).await
}

#[allow(clippy::too_many_arguments)]
async fn close_open_turn(
    session: &mut CodeSession,
    engine: &dyn HarnessSession,
    sink: &LiveSink,
    mut turn: CodeTurn,
    run: Result<TurnOutcome, HarnessError>,
    interrupted: bool,
    attachment_cleanup_error: Option<String>,
) -> Result<CodeTurn, WorkerError> {
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
            let detail = match outcome {
                TurnOutcome::Clean => None,
                TurnOutcome::Incomplete { detail } => Some(detail),
                TurnOutcome::Parked { .. } => {
                    Some("the engine parked a turn the worker was no longer waiting on".into())
                }
            };
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
                    false,
                )
                .await;
            }
            if turn.status.is_open() {
                let (status, event) = if interrupted {
                    (
                        CodeTurnStatus::Interrupted,
                        CodeEvent::TurnInterrupted { usage: None },
                    )
                } else if let Some(detail) = detail {
                    (
                        CodeTurnStatus::Failed,
                        CodeEvent::TurnFailed {
                            error: sink.legible_turn_error(detail),
                            detail: None,
                        },
                    )
                } else {
                    (
                        CodeTurnStatus::Completed,
                        CodeEvent::TurnCompleted {
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
                let write = if matches!(event, CodeEvent::TurnInterrupted { .. }) {
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
                turn.status = CodeTurnStatus::Failed;
                turn.ended_at = Some(Utc::now());
                let _ = save_turn(db, &session.owner, &turn).await;
                let event = CodeEvent::TurnFailed {
                    error: sink.legible_turn_error(err.to_string()),
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
            super::pr_facts::sweep_turn_for_pull_request_acts(
                db,
                session,
                turn.id,
                sink.gh_search_path.as_deref(),
                Some(&sink.hot_prs),
            )
            .await;
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
    super::pr_facts::sweep_turn_for_pull_request_acts(
        db,
        session,
        turn.id,
        sink.gh_search_path.as_deref(),
        Some(&sink.hot_prs),
    )
    .await;
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
    if turn.status == CodeTurnStatus::Interrupted {
        super::attention::replace_attention(
            session,
            Attention::needs_you("the turn was interrupted", AttentionSource::Lifecycle),
            false,
        );
    } else if turn.status == CodeTurnStatus::Failed {
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

async fn drive_turn(
    session: &mut CodeSession,
    engine: &dyn HarnessSession,
    sink: &LiveSink,
    store: &AttachmentStore,
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
    store: &AttachmentStore,
    commands: &mut mpsc::Receiver<WorkerCommand>,
    worktree: WorktreeTurn<'_>,
    QueuedFollowUp {
        message,
        attachments,
        trigger_delivery,
        queued_row,
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
    // Bytes first, before the worktree lock: a blob read and a decode should
    // never be held against a sibling session waiting for the checkout.
    let hydrated = hydrate_turn_images(store.blobs.as_deref(), &attachments).await?;

    // A queued row is already accepted. Bring the worker up to the committed
    // settings before it waits for the checkout. Reservations accepted during
    // that wait then update this same session copy, so the turn sees the last
    // committed settings when the lock becomes available.
    if matches!(worktree.wait, TurnWait::Queued) {
        let current = get_session(db, &session.owner, session.id)
            .await
            .map_err(|err| WorkerError::Failed(err.to_string()))?
            .ok_or_else(|| WorkerError::Failed(format!("session {} not found", session.id)))?;
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
        TurnWait::Queued => {
            match await_worktree_turn(session, engine, worktree.lock, commands).await {
                WorktreeWait::Acquired(guard) => guard,
                WorktreeWait::Stopped => return Err(WorkerError::QueuedTurnStopped),
                WorktreeWait::Shutdown => {
                    return Err(WorkerError::Conflict(
                        "the session worker is shutting down".into(),
                    ))
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
    if let Some(workspace_id) = session.workspace_id {
        let workspace = get_workspace(&sink.db, &session.owner, workspace_id)
            .await
            .map_err(|error| WorkerError::Failed(error.to_string()))?
            .ok_or_else(|| WorkerError::Conflict("workspace no longer exists".into()))?;
        if workspace.status != CodeWorkspaceStatus::Active {
            return Err(WorkerError::Conflict(format!(
                "workspace is {}",
                workspace.status.as_str()
            )));
        }
    }

    // An idle send resolves settings after it owns the worktree, so a
    // reservation that committed while this request was in flight reaches the
    // engine. A queued row uses the worker copy initialized before the wait and
    // updated by every confirmed reservation accepted during that wait.
    let turn_settings = match worktree.wait {
        TurnWait::Queued => CodeSessionExecutionSettings::from(&*session),
        TurnWait::Send => {
            let current = get_session(db, &session.owner, session.id)
                .await
                .map_err(|err| WorkerError::Failed(err.to_string()))?
                .ok_or_else(|| WorkerError::Failed(format!("session {} not found", session.id)))?;
            if current.spawn_epoch != session.spawn_epoch {
                return Err(WorkerError::Conflict(
                    "the session worker was superseded before the turn started".into(),
                ));
            }
            session.model = current.model;
            session.reasoning_effort = current.reasoning_effort;
            session.fast_mode = current.fast_mode;
            CodeSessionExecutionSettings::from(&*session)
        }
    };

    let ordinal = next_turn_ordinal(db, &session.owner, session.id)
        .await
        .map_err(|err| WorkerError::Failed(err.to_string()))?;
    let mut turn = CodeTurn {
        // A promoted queue row already carries the turn's id: inserting under
        // it is what lets the row deletion and the turn insertion commit as
        // one write (decision 69).
        id: queued_row
            .as_ref()
            .map_or_else(CodeTurnId::new, |row| row.id),
        session_id: session.id,
        ordinal,
        status: CodeTurnStatus::Running,
        model: turn_settings.model.clone(),
        fast_mode: turn_settings.fast_mode,
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
    let mut staged_attachments = if store.engine_reads_images {
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

    session.lifecycle = CodeSessionLifecycle::Running;
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
        CodeEvent::TurnStarted { turn_id: turn.id },
        sink.native_journal,
    )
    .await
    .map_err(|err| WorkerError::Failed(err.to_string()))?;

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
    let mut next_input = Some(TurnInput {
        text: engine_text,
        model: turn_settings.model.clone(),
        reasoning_effort: turn_settings.reasoning_effort,
        fast_mode: turn_settings.fast_mode,
        images,
    });
    let mut next_resume: Option<(String, tidebreak_harness::ResumeInput)> = None;
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
                        let _ = save_session(db, session).await;
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
            &wait,
            turn.id,
            &delivered,
        )
        .await
        {
            Some(ParkResolution {
                input,
                delivered_here,
            }) => {
                // An accepted plan re-postures the session before the turn
                // continues: the decision itself never changes the mode, the
                // engine's own channel does, and the row must say so too. A
                // settlement another surface made carried its own posture.
                if delivered_here {
                    apply_accepted_plan_mode(db, bus, session, engine, &input).await;
                }
                turn.status = CodeTurnStatus::Running;
                turn.park_ref = None;
                turn.park_wait = None;
                let _ = save_turn(db, &session.owner, &turn).await;
                next_resume = Some((park_ref, input));
            }
            // Interrupted or shut down while parked: fall through to the
            // shared closing code, which closes an open turn as interrupted.
            None => break 'legs Ok(TurnOutcome::Clean),
        }
    };
    // A control command still in flight has a caller waiting on its reply.
    // Dropping it here would answer them with a dead channel.
    while controls.next().await.is_some() {}

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
            let detail = match outcome {
                TurnOutcome::Clean => None,
                TurnOutcome::Incomplete { detail } => Some(detail),
                // Parks are intercepted by the leg loop above; one arriving
                // here means the engine parked after the worker stopped
                // waiting, and failing loudly beats stranding the turn open.
                TurnOutcome::Parked { .. } => {
                    Some("the engine parked a turn the worker was no longer waiting on".into())
                }
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
                    false,
                )
                .await;
            }
            if turn.status.is_open() {
                // The stream ended without closing the turn. Only the worker
                // knows whether that was asked for.
                let (status, event) = if interrupted {
                    (
                        CodeTurnStatus::Interrupted,
                        CodeEvent::TurnInterrupted { usage: None },
                    )
                } else if let Some(detail) = detail {
                    (
                        CodeTurnStatus::Failed,
                        CodeEvent::TurnFailed {
                            error: sink.legible_turn_error(detail),
                            detail: None,
                        },
                    )
                } else {
                    (
                        CodeTurnStatus::Completed,
                        CodeEvent::TurnCompleted {
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
                let write = if matches!(event, CodeEvent::TurnInterrupted { .. }) {
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
                turn.status = CodeTurnStatus::Failed;
                turn.ended_at = Some(Utc::now());
                let _ = save_turn(db, &session.owner, &turn).await;
                let event = CodeEvent::TurnFailed {
                    error: sink.legible_turn_error(err.to_string()),
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
            super::pr_facts::sweep_turn_for_pull_request_acts(
                db,
                session,
                turn.id,
                sink.gh_search_path.as_deref(),
                Some(&sink.hot_prs),
            )
            .await;
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
    super::pr_facts::sweep_turn_for_pull_request_acts(
        db,
        session,
        turn.id,
        sink.gh_search_path.as_deref(),
        Some(&sink.hot_prs),
    )
    .await;
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

/// Sweep crash-leftover attachment scopes while the caller holds the worktree.
///
/// A failed sweep leaves private bytes behind. Persist the fence before
/// returning so another turn cannot reuse the same private root.
async fn sweep_attachment_leftovers_or_fence(
    db: &DbStore,
    bus: &CodeEventBus,
    session: &mut CodeSession,
    private_root: &super::scratch::ScratchRoot,
) -> Result<(), WorkerError> {
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
}

fn code_turn_outcome(result: &Result<CodeTurn, WorkerError>) -> &'static str {
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

fn code_turn_is_error(result: &Result<CodeTurn, WorkerError>) -> bool {
    matches!(
        result,
        Ok(CodeTurn {
            status: CodeTurnStatus::Failed,
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
    session_id: CodeSessionId,
    turn_id: CodeTurnId,
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
        CodeEvent::TurnFailed { error, .. } => Some(error.message),
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
    session.child_process_identity = None;
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
    if kind != tidebreak_core::HarnessKind::Codex {
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
    session_id: CodeSessionId,
    spawn_epoch: i64,
    harness: HarnessKind,
    relay_wired: bool,
    turn_id: Option<CodeTurnId>,
    subagents: Vec<CodeSubagentSummary>,
    gh_search_path: Option<String>,
    recap: Option<Arc<dyn super::recap::TurnRecap>>,
    rewrite: Option<Arc<dyn super::rewrite::TurnRewrite>>,
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
        pending_resume_ref: std::sync::Mutex::new(None),
        gh_search_path,
        flushed_unrecognized: AtomicU64::new(0),
        subagents: std::sync::Mutex::new(subagents),
        recap,
        rewrite,
        hot_prs,
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
    persist_and_publish(db, bus, owner, session_id, spawn_epoch, event, false).await
}

/// Journal one event for the session, or — when `native_journal` says the
/// engine already wrote it — apply only what the row's arrival means for
/// the worker's own state. See [`LiveSink::native_journal`].
#[allow(clippy::too_many_arguments)]
async fn persist_and_publish(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    session_id: CodeSessionId,
    spawn_epoch: i64,
    event: CodeEvent,
    native_journal: bool,
) -> Result<(), CodeJournalError> {
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
    .await
}

#[allow(clippy::too_many_arguments)]
async fn persist_turn_and_publish(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    session_id: CodeSessionId,
    spawn_epoch: i64,
    turn_id: CodeTurnId,
    event: CodeEvent,
    native_journal: bool,
) -> Result<(), CodeJournalError> {
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
    .await
}

#[allow(clippy::too_many_arguments)]
async fn persist_and_publish_inner(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    session_id: CodeSessionId,
    spawn_epoch: i64,
    notification_turn_id: Option<CodeTurnId>,
    event: CodeEvent,
    native_journal: bool,
) -> Result<(), CodeJournalError> {
    if native_journal {
        // The row is already in the journal and on the bus; only the
        // worker's bookkeeping is left: the turn row closes with the usage
        // the engine reported, and a settled turn sweeps its approvals.
        apply_side_effects(db, owner, session_id, spawn_epoch, &event).await?;
        if matches!(
            &event,
            CodeEvent::TurnCompleted { .. }
                | CodeEvent::TurnRefused { .. }
                | CodeEvent::TurnFailed { .. }
                | CodeEvent::TurnInterrupted { .. }
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
        CodeEvent::ToolStarted {
            parent_call_id: None,
            ..
        } | CodeEvent::ToolCompleted {
            parent_call_id: None,
            ..
        }
    );
    apply_side_effects(db, owner, session_id, spawn_epoch, &event).await?;
    let seq = if let Some(turn_id) = notification_turn_id {
        append_event_with_notification(db, owner, session_id, spawn_epoch, turn_id, &event).await?
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
        CodeEvent::TurnCompleted { .. }
            | CodeEvent::TurnRefused { .. }
            | CodeEvent::TurnFailed { .. }
            | CodeEvent::TurnInterrupted { .. }
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
async fn settle_streamed_text(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    session_id: CodeSessionId,
    spawn_epoch: i64,
    event: &CodeEvent,
) {
    if !matches!(
        event,
        CodeEvent::TurnCompleted { .. }
            | CodeEvent::TurnRefused { .. }
            | CodeEvent::TurnFailed { .. }
            | CodeEvent::TurnInterrupted { .. }
    ) {
        return;
    }
    let streamed = bus.take_assistant_tail(session_id);
    if streamed.is_empty() {
        return;
    }
    let recovered = CodeEvent::AssistantMessage {
        text: streamed,
        parent_call_id: None,
    };
    match append_event(db, owner, session_id, spawn_epoch, &recovered).await {
        Ok(seq) => bus.publish(
            session_id,
            tidebreak_core::SequencedCodeEvent {
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
}

fn is_activity(event: &CodeEvent) -> bool {
    matches!(
        event,
        CodeEvent::AssistantMessage { .. }
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
        CodeEvent::TurnCompleted {
            usage, checkpoint, ..
        } => {
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
        // A refusal ends the turn the way a completion does: the model
        // answered, and its answer was to decline.
        CodeEvent::TurnRefused { usage, .. } => {
            if let Ok(Some(mut turn)) = get_open_turn(db, owner, session_id).await {
                turn.status = CodeTurnStatus::Completed;
                turn.ended_at = Some(Utc::now());
                turn.usage = Some(usage.clone());
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
        CodeEvent::TurnInterrupted { .. } => {
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
    let paths = approval_file_paths(raw, &input);
    let path = paths.first().map(String::as_str).unwrap_or("");
    match tool {
        "Write" | "Edit" | "NotebookEdit" | "write" | "edit" => {
            CodeApprovalKind::FileWrite { paths }
        }
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

fn approval_file_paths(raw: &serde_json::Value, input: &serde_json::Value) -> Vec<String> {
    let metadata = raw.get("metadata").unwrap_or(&serde_json::Value::Null);
    let cwd = metadata
        .get("cwd")
        .or_else(|| input.get("cwd"))
        .or_else(|| raw.get("cwd"))
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.trim().is_empty());
    let direct = metadata
        .get("filepath")
        .or_else(|| metadata.get("file_path"))
        .or_else(|| input.get("file_path"))
        .or_else(|| input.get("path"))
        .or_else(|| raw.get("path"))
        .and_then(serde_json::Value::as_str);
    let candidates = direct.into_iter().chain(
        raw.get("patterns")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .filter(|path| !path.chars().any(|ch| "*?[]{}".contains(ch))),
    );
    let mut paths = Vec::new();
    for candidate in candidates {
        let candidate = candidate.trim();
        if candidate.is_empty() {
            continue;
        }
        let normalized = normalize_approval_path(candidate, cwd);
        if !normalized.is_empty() && !paths.contains(&normalized) {
            paths.push(normalized);
        }
    }
    paths
}

fn normalize_approval_path(path: &str, cwd: Option<&str>) -> String {
    let render = |path: &std::path::Path| path.to_string_lossy().replace('\\', "/");
    let path = std::path::Path::new(path);
    if let Ok(relative) = path.strip_prefix("/workspace") {
        if let Some(cwd) = cwd.filter(|cwd| !cwd.trim().is_empty()) {
            return render(&std::path::Path::new(cwd).join(relative));
        }
    }
    if path.is_relative() {
        if let Some(cwd) = cwd.filter(|cwd| !cwd.trim().is_empty()) {
            return render(&std::path::Path::new(cwd).join(path));
        }
    }
    render(path)
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
            decision: decision.into(),
        },
        HarnessEvent::UserSteered { text } => CodeEvent::UserSteered {
            text,
            message_id: None,
        },
        HarnessEvent::TurnCompleted { usage } => CodeEvent::TurnCompleted {
            usage,
            checkpoint: None,
            stop_reason: None,
        },
        HarnessEvent::TurnFailed { error } => CodeEvent::TurnFailed {
            error,
            detail: None,
        },
        HarnessEvent::TurnInterrupted => CodeEvent::TurnInterrupted { usage: None },
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
    use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
    use chrono::Utc;
    use tidebreak_core::db::code::{
        enqueue_queued_turn, get_session, insert_repo, insert_session, insert_workspace,
        list_approvals, list_events, list_turns, replace_session_execution_settings,
        MAX_REPLAY_EVENTS,
    };
    use tidebreak_core::{
        CodeRepo, CodeSessionKind, CodeUsage, CodeWorkspace, CodeWorkspaceStatus, HarnessKind,
        ImageMediaType, ImageRef, PermissionMode, ReasoningEffort, RepoId, ToolDetail, WorkspaceId,
    };
    use tidebreak_harness::{HarnessAdapter as _, SessionSpec};

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

    #[tokio::test]
    async fn an_aborted_permission_mode_settlement_rejects_a_turn_queued_behind_it() {
        let (commands, mut pending) = mpsc::channel(1);
        let (reply, outcome) = oneshot::channel();
        assert!(commands
            .send(WorkerCommand::RunTurn {
                message: "must not disappear".into(),
                attachments: Vec::new(),
                trigger_delivery: None,
                reply,
            })
            .await
            .is_ok());
        let (settle, settlement) = oneshot::channel();
        assert!(settle.send(PermissionModeSettlement::Abort).is_ok());

        assert!(!await_permission_mode_settlement(settlement, &mut pending).await);
        match outcome.await.unwrap() {
            Err(WorkerError::Conflict(message)) => assert_eq!(
                message,
                "the turn was not accepted because the permission mode change did not commit"
            ),
            Err(error) => panic!("unexpected turn rejection: {error:?}"),
            Ok(_) => panic!("the queued turn unexpectedly ran"),
        }
        assert!(commands.is_closed());
    }

    async fn seeded_session(
        harness_kind: HarnessKind,
        harness_version: Option<&str>,
    ) -> (
        tempfile::TempDir,
        Arc<DbStore>,
        Arc<CodeEventBus>,
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
                cloned_from: None,
                origin_host: None,
                origin_owner: None,
                origin_name: None,
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
                workspace_id: Some(workspace_id),
                kind: CodeSessionKind::Interactive,
                harness_kind,
                harness_version: harness_version.map(str::to_owned),
                harness_resume_ref: None,
                permission_mode: PermissionMode::Plan,
                model: None,
                reasoning_effort: None,
                fast_mode: false,
                lifecycle: CodeSessionLifecycle::Running,
                fence_reason: None,
                child_pid: None,
                child_process_identity: None,
                spawn_epoch: 1,
                attention: Attention::working(AttentionSource::Lifecycle),
                unrecognized_event_count: 0,
                subagents: Vec::new(),
                created_at: Utc::now(),
            },
        )
        .await
        .unwrap();
        (
            directory,
            store,
            Arc::new(CodeEventBus::default()),
            session_id,
        )
    }

    async fn seeded_sink() -> (
        tempfile::TempDir,
        Arc<DbStore>,
        Arc<LiveSink>,
        CodeSessionId,
    ) {
        let (directory, store, bus, session_id) =
            seeded_session(HarnessKind::ClaudeCode, Some("2.1.237")).await;
        let sink = sink_for(
            store.clone(),
            bus,
            OwnerId::local(),
            session_id,
            1,
            HarnessKind::ClaudeCode,
            false,
            None,
            Vec::new(),
            None,
            None,
            None,
            crate::code::pr_refresh::HotPullRequests::default(),
        );
        (directory, store, sink, session_id)
    }

    /// An engine that answers its own approval — a standing grant, an
    /// auto-approval judge — reports the decision on the stream, and that
    /// report is the only settlement the row it opened will ever get.
    #[tokio::test]
    async fn an_engine_observed_decision_settles_its_own_approval_row() {
        let (directory, store, sink, session_id) = seeded_sink().await;
        let owner = OwnerId::local();
        let mut session = get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap();
        session.lifecycle = CodeSessionLifecycle::Idle;
        assert!(save_session(&store, &session).await.unwrap());

        let worktree = directory.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let private = directory.path().join("private");
        std::fs::create_dir(&private).unwrap();
        let private_root =
            super::super::scratch::ScratchRoot::open_for_test(&private).expect("scratch root");
        let script = vec![
            HarnessEvent::TurnStarted,
            HarnessEvent::ApprovalRequested {
                harness_ref: tidebreak_harness::HarnessApprovalRef::engine("call-1"),
                raw: serde_json::Value::Null,
                kind: Some(tidebreak_core::CodeApprovalKind::Other {
                    summary: "exec".into(),
                }),
            },
            HarnessEvent::ApprovalResolved {
                harness_ref: tidebreak_harness::HarnessApprovalRef::engine("call-1"),
                decision: ApprovalDecision::Approve,
            },
            HarnessEvent::AssistantMessage {
                text: "ran under a standing grant".into(),
                parent_call_id: None,
            },
            HarnessEvent::TurnCompleted {
                usage: CodeUsage::default(),
            },
        ];
        let adapter = ScriptedAdapter::new(script).with_unattended_approvals();
        let engine = adapter
            .launch(SessionSpec {
                owner: tidebreak_core::OwnerId::local(),
                session_id: tidebreak_core::CodeSessionId::new(),
                worktree,
                allowed_read_roots: Vec::new(),
                permission_mode: session.permission_mode,
                model: session.model.clone(),
                reasoning_effort: session.reasoning_effort,
                fast_mode: session.fast_mode,
                resume_ref: None,
                extra_argv: Vec::new(),
                extra_env: Vec::new(),
                relay_key_env: None,
                env: Vec::new(),
                approval: None,
                binary: Some(std::path::PathBuf::from("/scripted/engine")),
                sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
                browser: None,
            })
            .await
            .unwrap();
        let handle = spawn_session_worker(
            session.clone(),
            engine,
            sink,
            AttachmentStore {
                blobs: None,
                private_root,
                engine_reads_images: false,
            },
            Arc::new(tokio::sync::Mutex::new(())),
            tokio::sync::watch::channel(false).1,
        );

        let (turn_reply, turn_response) = oneshot::channel();
        handle
            .commands
            .send(WorkerCommand::RunTurn {
                message: "run it".into(),
                attachments: Vec::new(),
                trigger_delivery: None,
                reply: turn_reply,
            })
            .await
            .unwrap();
        let turn = tokio::time::timeout(Duration::from_secs(5), turn_response)
            .await
            .expect("the turn completes")
            .unwrap()
            .unwrap();
        assert_eq!(turn.status, CodeTurnStatus::Completed);

        let approvals = list_approvals(&store, &owner, None, Some(session_id))
            .await
            .unwrap();
        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].state, CodeApprovalState::Approved);
        assert_eq!(approvals[0].native_call_id.as_deref(), Some("call-1"));
        let events = list_events(&store, &owner, session_id, 0, MAX_REPLAY_EVENTS)
            .await
            .unwrap();
        assert!(
            events.events.iter().any(|event| matches!(
                &event.event,
                CodeEvent::ApprovalResolved {
                    approval_id,
                    decision: tidebreak_core::ApprovalDecisionKind::Approve,
                } if *approval_id == approvals[0].id
            )),
            "the journal records the engine's own decision"
        );
        let _ = handle.commands.send(WorkerCommand::Shutdown).await;
    }

    #[tokio::test]
    async fn a_parked_turn_waits_durably_and_resumes_on_the_awaited_decision() {
        let (directory, store, sink, session_id) = seeded_sink().await;
        let owner = OwnerId::local();
        let mut session = get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap();
        session.lifecycle = CodeSessionLifecycle::Idle;
        assert!(save_session(&store, &session).await.unwrap());

        let worktree = directory.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let private = directory.path().join("private");
        std::fs::create_dir(&private).unwrap();
        let private_root =
            super::super::scratch::ScratchRoot::open_for_test(&private).expect("scratch root");
        let script = vec![
            HarnessEvent::TurnStarted,
            HarnessEvent::ApprovalRequested {
                harness_ref: tidebreak_harness::HarnessApprovalRef::engine("call-1"),
                raw: serde_json::json!({ "tool_name": "Write" }),
                kind: None,
            },
            HarnessEvent::AssistantMessage {
                text: "resumed after the decision".into(),
                parent_call_id: None,
            },
            HarnessEvent::TurnCompleted {
                usage: CodeUsage::default(),
            },
        ];
        // The engine checkpoints after the request instead of blocking on it.
        let adapter = ScriptedAdapter::new(script)
            .with_unattended_approvals()
            .with_parked_turn(
                2,
                "cp-1",
                tidebreak_harness::ParkWait::Approval {
                    call_id: "call-1".into(),
                },
            );
        let engine = adapter
            .launch(SessionSpec {
                owner: tidebreak_core::OwnerId::local(),
                session_id: tidebreak_core::CodeSessionId::new(),
                worktree,
                allowed_read_roots: Vec::new(),
                permission_mode: session.permission_mode,
                model: session.model.clone(),
                reasoning_effort: session.reasoning_effort,
                fast_mode: session.fast_mode,
                resume_ref: None,
                extra_argv: Vec::new(),
                extra_env: Vec::new(),
                relay_key_env: None,
                env: Vec::new(),
                approval: None,
                binary: Some(std::path::PathBuf::from("/scripted/engine")),
                sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
                browser: None,
            })
            .await
            .unwrap();
        let handle = spawn_session_worker(
            session.clone(),
            engine,
            sink,
            AttachmentStore {
                blobs: None,
                private_root,
                engine_reads_images: false,
            },
            Arc::new(tokio::sync::Mutex::new(())),
            tokio::sync::watch::channel(false).1,
        );

        let (turn_reply, turn_response) = oneshot::channel();
        handle
            .commands
            .send(WorkerCommand::RunTurn {
                message: "park then resume".into(),
                attachments: Vec::new(),
                trigger_delivery: None,
                reply: turn_reply,
            })
            .await
            .unwrap();

        // The turn must reach the durable park before the decision arrives.
        let parked = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let turns = list_turns(&store, &owner, session_id).await.unwrap();
                if let Some(turn) = turns.iter().find(|t| t.status == CodeTurnStatus::Waiting) {
                    break turn.clone();
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the turn parks");
        assert_eq!(parked.park_ref.as_deref(), Some("cp-1"));
        assert_eq!(
            parked.park_wait,
            Some(tidebreak_core::TurnParkWait::Approval {
                call_id: "call-1".into()
            })
        );

        let (decide_reply, decide_response) = oneshot::channel();
        handle
            .commands
            .send(WorkerCommand::Decide {
                approval: tidebreak_harness::HarnessApprovalRef::engine("call-1"),
                decision: Box::new(tidebreak_harness::ApprovalDecision::Approve),
                reply: decide_reply,
            })
            .await
            .unwrap();
        decide_response.await.unwrap().unwrap();

        let turn = tokio::time::timeout(Duration::from_secs(5), turn_response)
            .await
            .expect("the turn completes after the resume")
            .unwrap()
            .unwrap();
        assert_eq!(turn.status, CodeTurnStatus::Completed);
        assert_eq!(turn.park_ref, None, "the resume clears the park");
        assert_eq!(turn.park_wait, None);
        assert_eq!(adapter.resumes().len(), 1, "one resume for one park");
        let events = list_events(&store, &owner, session_id, 0, MAX_REPLAY_EVENTS)
            .await
            .unwrap();
        assert!(
            events
                .events
                .iter()
                .any(|event| matches!(event.event, CodeEvent::TurnCompleted { .. })),
            "the resumed leg reaches the journal"
        );
        let _ = handle.commands.send(WorkerCommand::Shutdown).await;
    }

    #[tokio::test]
    async fn a_decision_on_the_running_leg_resumes_the_park() {
        let (directory, store, sink, session_id) = seeded_sink().await;
        let owner = OwnerId::local();
        let mut session = get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap();
        session.lifecycle = CodeSessionLifecycle::Idle;
        assert!(save_session(&store, &session).await.unwrap());

        let worktree = directory.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let private = directory.path().join("private");
        std::fs::create_dir(&private).unwrap();
        let private_root =
            super::super::scratch::ScratchRoot::open_for_test(&private).expect("scratch root");
        let script = vec![
            HarnessEvent::TurnStarted,
            HarnessEvent::ApprovalRequested {
                harness_ref: tidebreak_harness::HarnessApprovalRef::engine("call-1"),
                raw: serde_json::json!({ "tool_name": "Write" }),
                kind: None,
            },
            // Holds the opening leg open after the request is journaled so
            // Decide is admitted there, before `Parked` is persisted.
            HarnessEvent::AssistantDelta {
                text: "still running".into(),
            },
            HarnessEvent::AssistantMessage {
                text: "resumed after the decision".into(),
                parent_call_id: None,
            },
            HarnessEvent::TurnCompleted {
                usage: CodeUsage::default(),
            },
        ];
        let adapter = ScriptedAdapter::new(script)
            .with_unattended_approvals()
            .with_delay(Duration::from_millis(150))
            .with_approval_ack_delay(Duration::from_millis(150))
            .with_parked_turn(
                3,
                "cp-1",
                tidebreak_harness::ParkWait::Approval {
                    call_id: "call-1".into(),
                },
            );
        let engine = adapter
            .launch(SessionSpec {
                owner: tidebreak_core::OwnerId::local(),
                session_id: tidebreak_core::CodeSessionId::new(),
                worktree,
                allowed_read_roots: Vec::new(),
                permission_mode: session.permission_mode,
                model: session.model.clone(),
                reasoning_effort: session.reasoning_effort,
                fast_mode: session.fast_mode,
                resume_ref: None,
                extra_argv: Vec::new(),
                extra_env: Vec::new(),
                relay_key_env: None,
                env: Vec::new(),
                approval: None,
                binary: Some(std::path::PathBuf::from("/scripted/engine")),
                sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
                browser: None,
            })
            .await
            .unwrap();
        let handle = spawn_session_worker(
            session.clone(),
            engine,
            sink,
            AttachmentStore {
                blobs: None,
                private_root,
                engine_reads_images: false,
            },
            Arc::new(tokio::sync::Mutex::new(())),
            tokio::sync::watch::channel(false).1,
        );

        let (turn_reply, turn_response) = oneshot::channel();
        handle
            .commands
            .send(WorkerCommand::RunTurn {
                message: "decide before the park".into(),
                attachments: Vec::new(),
                trigger_delivery: None,
                reply: turn_reply,
            })
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let pending = list_approvals(
                    &store,
                    &owner,
                    Some(CodeApprovalState::Pending),
                    Some(session_id),
                )
                .await
                .unwrap();
                if !pending.is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the approval is published");
        let turns = list_turns(&store, &owner, session_id).await.unwrap();
        assert!(
            turns
                .iter()
                .any(|turn| turn.status == CodeTurnStatus::Running),
            "Decide must reach the opening leg while the turn is still running"
        );

        let (decide_reply, decide_response) = oneshot::channel();
        handle
            .commands
            .send(WorkerCommand::Decide {
                approval: tidebreak_harness::HarnessApprovalRef::engine("call-1"),
                decision: Box::new(tidebreak_harness::ApprovalDecision::Approve),
                reply: decide_reply,
            })
            .await
            .unwrap();
        decide_response.await.unwrap().unwrap();

        let turn = tokio::time::timeout(Duration::from_secs(5), turn_response)
            .await
            .expect("the turn completes from the already-delivered decision")
            .unwrap()
            .unwrap();
        assert_eq!(turn.status, CodeTurnStatus::Completed);
        assert_eq!(turn.park_ref, None, "the resume clears the park");
        assert_eq!(turn.park_wait, None);
        assert_eq!(adapter.resumes().len(), 1, "one resume for one park");
        let _ = handle.commands.send(WorkerCommand::Shutdown).await;
    }

    #[tokio::test]
    async fn an_interrupt_closes_a_parked_turn() {
        let (directory, store, sink, session_id) = seeded_sink().await;
        let owner = OwnerId::local();
        let mut session = get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap();
        session.lifecycle = CodeSessionLifecycle::Idle;
        assert!(save_session(&store, &session).await.unwrap());
        let worktree = directory.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let private = directory.path().join("private");
        std::fs::create_dir(&private).unwrap();
        let private_root =
            super::super::scratch::ScratchRoot::open_for_test(&private).expect("scratch root");
        let adapter = ScriptedAdapter::new(vec![HarnessEvent::TurnStarted]).with_parked_turn(
            1,
            "cp-1",
            tidebreak_harness::ParkWait::Approval {
                call_id: "call-1".into(),
            },
        );
        let engine = adapter
            .launch(SessionSpec {
                owner: tidebreak_core::OwnerId::local(),
                session_id: tidebreak_core::CodeSessionId::new(),
                worktree,
                allowed_read_roots: Vec::new(),
                permission_mode: session.permission_mode,
                model: None,
                reasoning_effort: None,
                fast_mode: false,
                resume_ref: None,
                extra_argv: Vec::new(),
                extra_env: Vec::new(),
                relay_key_env: None,
                env: Vec::new(),
                approval: None,
                binary: Some(std::path::PathBuf::from("/scripted/engine")),
                sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
                browser: None,
            })
            .await
            .unwrap();
        let handle = spawn_session_worker(
            session.clone(),
            engine,
            sink,
            AttachmentStore {
                blobs: None,
                private_root,
                engine_reads_images: false,
            },
            Arc::new(tokio::sync::Mutex::new(())),
            tokio::sync::watch::channel(false).1,
        );
        let (turn_reply, turn_response) = oneshot::channel();
        handle
            .commands
            .send(WorkerCommand::RunTurn {
                message: "park then interrupt".into(),
                attachments: Vec::new(),
                trigger_delivery: None,
                reply: turn_reply,
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(turn) = get_open_turn(&store, &owner, session_id).await.unwrap() {
                    if turn.status == CodeTurnStatus::Waiting {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the turn parks");
        let (stop_reply, stop_response) = oneshot::channel();
        handle
            .commands
            .send(WorkerCommand::Interrupt { reply: stop_reply })
            .await
            .unwrap();
        stop_response.await.unwrap().unwrap();
        let turn = tokio::time::timeout(Duration::from_secs(5), turn_response)
            .await
            .expect("the parked turn closes")
            .unwrap()
            .unwrap();
        assert_eq!(turn.status, CodeTurnStatus::Interrupted);
        let _ = handle.commands.send(WorkerCommand::Shutdown).await;
    }

    #[tokio::test]
    async fn a_confirmed_setting_reservation_wins_over_an_already_queued_idle_turn() {
        let (directory, store, sink, session_id) = seeded_sink().await;
        let owner = OwnerId::local();
        let mut session = get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap();
        session.lifecycle = CodeSessionLifecycle::Idle;
        assert!(save_session(&store, &session).await.unwrap());
        let stale = CodeSessionExecutionSettings {
            model: Some("stale".into()),
            reasoning_effort: Some(ReasoningEffort::High),
            fast_mode: true,
        };
        session = replace_session_execution_settings(&store, &owner, &session, &stale)
            .await
            .unwrap()
            .expect("the initial settings commit");

        let worktree = directory.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let private = directory.path().join("private");
        std::fs::create_dir(&private).unwrap();
        let private_root =
            super::super::scratch::ScratchRoot::open_for_test(&private).expect("scratch root");
        let adapter = ScriptedAdapter::new(plain_text_script());
        let engine = adapter
            .launch(SessionSpec {
                owner: tidebreak_core::OwnerId::local(),
                session_id: tidebreak_core::CodeSessionId::new(),
                worktree,
                allowed_read_roots: Vec::new(),
                permission_mode: session.permission_mode,
                model: session.model.clone(),
                reasoning_effort: session.reasoning_effort,
                fast_mode: session.fast_mode,
                resume_ref: None,
                extra_argv: Vec::new(),
                extra_env: Vec::new(),
                relay_key_env: None,
                env: Vec::new(),
                approval: None,
                binary: Some(std::path::PathBuf::from("/scripted/engine")),
                sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
                browser: None,
            })
            .await
            .unwrap();
        let handle = spawn_session_worker(
            session.clone(),
            engine,
            sink,
            AttachmentStore {
                blobs: None,
                private_root,
                engine_reads_images: false,
            },
            Arc::new(tokio::sync::Mutex::new(())),
            tokio::sync::watch::channel(false).1,
        );

        let committed = CodeSessionExecutionSettings {
            model: Some("committed".into()),
            reasoning_effort: None,
            fast_mode: false,
        };
        let (reservation_reply, reservation_response) = oneshot::channel();
        let (settlement, release) = oneshot::channel();
        handle
            .commands
            .send(WorkerCommand::SetExecutionSettings {
                settings: committed.clone(),
                settlement: release,
                reply: reservation_reply,
            })
            .await
            .unwrap();
        reservation_response.await.unwrap().unwrap();

        let (turn_reply, turn_response) = oneshot::channel();
        handle
            .commands
            .send(WorkerCommand::RunTurn {
                message: "use the committed settings".into(),
                attachments: Vec::new(),
                trigger_delivery: None,
                reply: turn_reply,
            })
            .await
            .unwrap();

        let updated = replace_session_execution_settings(&store, &owner, &session, &committed)
            .await
            .unwrap()
            .expect("the reserved settings commit");
        assert!(settlement
            .send(ExecutionSettingsSettlement::Confirmed)
            .is_ok());

        let turn = tokio::time::timeout(Duration::from_secs(5), turn_response)
            .await
            .expect("the turn completes")
            .unwrap()
            .unwrap();
        assert_eq!(turn.model, updated.model);
        assert_eq!(turn.fast_mode, updated.fast_mode);
        assert_eq!(adapter.turn_efforts(), vec![updated.reasoning_effort]);
        let inputs = adapter.turn_inputs();
        assert_eq!(inputs[0].model, updated.model);
        assert_eq!(inputs[0].fast_mode, updated.fast_mode);
        let _ = handle.commands.send(WorkerCommand::Shutdown).await;
    }

    #[tokio::test]
    async fn a_queued_turn_uses_a_later_setting_committed_before_promotion() {
        let (directory, store, sink, session_id) = seeded_sink().await;
        let owner = OwnerId::local();
        let mut session = get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap();
        session.lifecycle = CodeSessionLifecycle::Idle;
        assert!(save_session(&store, &session).await.unwrap());
        let held = CodeSessionExecutionSettings {
            model: Some("held".into()),
            reasoning_effort: Some(ReasoningEffort::High),
            fast_mode: true,
        };
        session = replace_session_execution_settings(&store, &owner, &session, &held)
            .await
            .unwrap()
            .expect("the held settings commit");

        let worktree = directory.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let private = directory.path().join("private");
        std::fs::create_dir(&private).unwrap();
        let private_root =
            super::super::scratch::ScratchRoot::open_for_test(&private).expect("scratch root");
        let adapter = ScriptedAdapter::new(plain_text_script());
        let engine = adapter
            .launch(SessionSpec {
                owner: tidebreak_core::OwnerId::local(),
                session_id: tidebreak_core::CodeSessionId::new(),
                worktree,
                allowed_read_roots: Vec::new(),
                permission_mode: session.permission_mode,
                model: session.model.clone(),
                reasoning_effort: session.reasoning_effort,
                fast_mode: session.fast_mode,
                resume_ref: None,
                extra_argv: Vec::new(),
                extra_env: Vec::new(),
                relay_key_env: None,
                env: Vec::new(),
                approval: None,
                binary: Some(std::path::PathBuf::from("/scripted/engine")),
                sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
                browser: None,
            })
            .await
            .unwrap();
        let now = Utc::now();
        enqueue_queued_turn(
            &store,
            &owner,
            &CodeQueuedTurn {
                id: CodeTurnId::new(),
                session_id,
                message: "use the held settings".into(),
                attachments: Vec::new(),
                position: 0,
                created_at: now,
                updated_at: now,
            },
        )
        .await
        .unwrap();
        let worktree_lock = Arc::new(tokio::sync::Mutex::new(()));
        let checkout = worktree_lock.lock().await;
        let handle = spawn_session_worker(
            session.clone(),
            engine,
            sink,
            AttachmentStore {
                blobs: None,
                private_root,
                engine_reads_images: false,
            },
            worktree_lock.clone(),
            tokio::sync::watch::channel(false).1,
        );

        let later = CodeSessionExecutionSettings {
            model: Some("later".into()),
            reasoning_effort: None,
            fast_mode: false,
        };
        let (reservation_reply, reservation_response) = oneshot::channel();
        let (settlement, release) = oneshot::channel();
        handle
            .commands
            .send(WorkerCommand::SetExecutionSettings {
                settings: later.clone(),
                settlement: release,
                reply: reservation_reply,
            })
            .await
            .unwrap();
        reservation_response.await.unwrap().unwrap();
        let _updated = replace_session_execution_settings(&store, &owner, &session, &later)
            .await
            .unwrap()
            .expect("the later settings commit");
        assert!(settlement
            .send(ExecutionSettingsSettlement::Confirmed)
            .is_ok());

        drop(checkout);
        let turn = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let turns = list_turns(&store, &owner, session_id).await.unwrap();
                if let Some(turn) = turns.into_iter().find(|turn| {
                    turn.status != CodeTurnStatus::Running && !turn.user_input.is_empty()
                }) {
                    return turn;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the queued turn completes");
        assert_eq!(turn.model, later.model);
        assert_eq!(turn.fast_mode, later.fast_mode);
        assert_eq!(adapter.turn_efforts(), vec![later.reasoning_effort]);
        let inputs = adapter.turn_inputs();
        assert_eq!(inputs[0].model, later.model);
        assert_eq!(inputs[0].fast_mode, later.fast_mode);
        let _ = handle.commands.send(WorkerCommand::Shutdown).await;
    }

    #[tokio::test]
    async fn codex_attachment_journals_one_start_after_the_thread_is_known() {
        let (_directory, store, bus, session_id) =
            seeded_session(HarnessKind::Codex, Some("codex-cli 0.147.0")).await;
        let attached = attach_engine(
            &store,
            &bus,
            session_id,
            HarnessKind::Codex,
            Some("0.147.0".into()),
            None,
        )
        .await
        .unwrap();
        let owner = OwnerId::local();
        assert_eq!(attached.harness_version.as_deref(), Some("0.147.0"));

        assert!(
            list_events(&store, &owner, session_id, 0, MAX_REPLAY_EVENTS)
                .await
                .unwrap()
                .events
                .iter()
                .all(|event| !matches!(&event.event, CodeEvent::SessionStarted { .. }))
        );

        let sink = sink_for(
            store.clone(),
            bus,
            owner.clone(),
            session_id,
            attached.spawn_epoch,
            HarnessKind::Codex,
            false,
            None,
            attached.subagents,
            None,
            None,
            None,
            crate::code::pr_refresh::HotPullRequests::default(),
        );
        sink.emit(HarnessEvent::SessionStarted {
            harness_kind: HarnessKind::Codex,
            harness_version: "0.147.0".into(),
            resume_ref: Some("thread-1".into()),
        })
        .await;

        let started = list_events(&store, &owner, session_id, 0, MAX_REPLAY_EVENTS)
            .await
            .unwrap()
            .events
            .into_iter()
            .filter_map(|event| match event.event {
                CodeEvent::SessionStarted {
                    harness_kind,
                    harness_version,
                    resume_ref,
                } => Some((harness_kind, harness_version, resume_ref)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            started,
            vec![(
                HarnessKind::Codex,
                "0.147.0".into(),
                Some("thread-1".into())
            )]
        );
    }

    #[tokio::test]
    async fn non_codex_attachment_keeps_the_eager_session_start() {
        let (_directory, store, bus, session_id) =
            seeded_session(HarnessKind::ClaudeCode, Some("2.1.237")).await;

        attach_engine(
            &store,
            &bus,
            session_id,
            HarnessKind::ClaudeCode,
            Some("2.1.237".into()),
            None,
        )
        .await
        .unwrap();

        let events = list_events(&store, &OwnerId::local(), session_id, 0, MAX_REPLAY_EVENTS)
            .await
            .unwrap()
            .events;
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(&event.event, CodeEvent::SessionStarted { .. }))
                .count(),
            1
        );
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

            // Codex can publish a child result after the parent boundary.
            // The parent already settled the span, so that late result must
            // not revise the recorded outcome.
            sink.emit(HarnessEvent::ToolCompleted {
                call_id: call_id.into(),
                outcome: if expected == CodeSubagentStatus::Done {
                    ToolOutcome::Failed
                } else {
                    ToolOutcome::Succeeded
                },
                preview: "late child result".into(),
                detail: None,
                parent_call_id: None,
            })
            .await;
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

    #[tokio::test]
    async fn sink_persists_a_resume_ref_only_after_turn_activity_starts() {
        let (_directory, store, sink, session_id) = seeded_sink().await;
        let owner = OwnerId::local();

        sink.emit(HarnessEvent::SessionStarted {
            harness_kind: HarnessKind::Codex,
            harness_version: "0.147.0".into(),
            resume_ref: Some("thread-1".into()),
        })
        .await;
        assert_eq!(
            get_session(&store, &owner, session_id)
                .await
                .unwrap()
                .unwrap()
                .harness_resume_ref,
            None,
            "an unused Codex thread is not a safe resume target"
        );

        sink.emit(HarnessEvent::TurnStarted).await;
        assert_eq!(
            get_session(&store, &owner, session_id)
                .await
                .unwrap()
                .unwrap()
                .harness_resume_ref
                .as_deref(),
            Some("thread-1")
        );
    }

    #[tokio::test]
    async fn assistant_activity_persists_resume_refs_for_harnesses_without_turn_started() {
        let (_directory, store, sink, session_id) = seeded_sink().await;
        let owner = OwnerId::local();
        let mut worker_session = get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(worker_session.harness_resume_ref, None);

        sink.emit(HarnessEvent::SessionStarted {
            harness_kind: HarnessKind::ClaudeCode,
            harness_version: "2.1.237".into(),
            resume_ref: Some("session-1".into()),
        })
        .await;
        sink.emit(HarnessEvent::AssistantDelta {
            text: "Working".into(),
        })
        .await;

        // A child pid may arrive after the sink writes the resume ref. Mirror
        // the real worker path and prove that its stale session copy keeps the
        // ref instead of replacing it with NULL during the full-row save.
        worker_session.child_pid = Some(4242);
        assert!(save_session(&store, &worker_session).await.unwrap());
        assert_eq!(
            get_session(&store, &owner, session_id)
                .await
                .unwrap()
                .unwrap()
                .harness_resume_ref
                .as_deref(),
            Some("session-1")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_pre_turn_attachment_sweep_fences_the_session() {
        use std::os::unix::fs::PermissionsExt as _;

        let (directory, store, sink, session_id) = seeded_sink().await;
        let private_path = directory.path().join("private");
        std::fs::create_dir(&private_path).unwrap();
        let private_root =
            super::super::scratch::ScratchRoot::open_for_test(&private_path).expect("scratch root");
        let attachment_root = private_root.path().join(ATTACHMENTS_DIR);
        let leftover = attachment_root.join(CodeSessionId::new().to_string());
        std::fs::create_dir_all(&leftover).unwrap();
        std::fs::write(leftover.join("private.png"), b"private").unwrap();
        std::fs::set_permissions(&attachment_root, std::fs::Permissions::from_mode(0o500)).unwrap();

        let mut session = get_session(&store, &OwnerId::local(), session_id)
            .await
            .unwrap()
            .unwrap();
        let result =
            sweep_attachment_leftovers_or_fence(&store, &sink.bus, &mut session, &private_root)
                .await;

        std::fs::set_permissions(&attachment_root, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(result, Err(WorkerError::Failed(_))));
        let stored = get_session(&store, &OwnerId::local(), session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.lifecycle, CodeSessionLifecycle::Fenced);
        assert!(matches!(
            stored.fence_reason,
            Some(FenceReason::ProbeAmbiguous { ref detail })
                if detail.starts_with("sweep attachments:")
        ));
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
                "permission": "edit",
                "metadata": {
                    "filepath": "/workspace/docs/approval.md",
                    "cwd": "/worktree"
                },
                "patterns": ["docs/approval.md", "*.md"]
            })),
            CodeApprovalKind::FileWrite {
                paths: vec!["/worktree/docs/approval.md".into()],
            }
        );
        assert_eq!(
            kind_from_raw(&serde_json::json!({
                "permission": "edit",
                "cwd": "/worktree",
                "patterns": ["docs/fallback.md", "*"]
            })),
            CodeApprovalKind::FileWrite {
                paths: vec!["/worktree/docs/fallback.md".into()],
            }
        );
        assert_eq!(
            kind_from_raw(&serde_json::json!({
                "permission": "edit",
                "patterns": ["*"]
            })),
            CodeApprovalKind::FileWrite { paths: Vec::new() }
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

    #[tokio::test]
    async fn fallback_images_live_only_in_the_session_turn_scope() {
        let private = tempfile::tempdir().unwrap();
        let private_root = super::super::scratch::ScratchRoot::open_for_test(private.path())
            .expect("scratch root");
        let session_id = CodeSessionId::new();
        let turn_id = CodeTurnId::new();
        let attachment = ImageRef {
            blob_id: uuid::Uuid::new_v4(),
            media_type: ImageMediaType::Png,
            width: 1,
            height: 1,
            byte_len: 4,
        };
        let staged = write_turn_attachments(
            &private_root,
            session_id,
            turn_id,
            std::slice::from_ref(&attachment),
            &[TurnImage {
                media_type: "image/png".into(),
                bytes: vec![1, 2, 3, 4],
            }],
        )
        .await
        .unwrap();
        let expected = private
            .path()
            .join(ATTACHMENTS_DIR)
            .join(session_id.to_string())
            .join(turn_id.to_string())
            .join(format!("{}.png", attachment.blob_id))
            .display()
            .to_string();
        assert_eq!(staged.paths, vec![expected.clone()]);
        assert_eq!(std::fs::read(&expected).unwrap(), [1, 2, 3, 4]);

        let mut staged = staged;
        staged.scope.cleanup().unwrap();

        assert!(!private
            .path()
            .join(ATTACHMENTS_DIR)
            .join(session_id.to_string())
            .exists());
    }

    #[test]
    fn attachment_paths_are_named_after_the_message_in_order() {
        let message =
            message_naming_attachments("compare these", &["first.png".into(), "second.png".into()]);
        assert_eq!(
            message,
            "compare these\n\nimages attached to this message:\n- `first.png`\n- `second.png`"
        );
    }

    #[test]
    fn provider_auth_failures_are_recognized_and_other_failures_are_not() {
        // The vendor bodies the shipped engines actually pass through.
        assert!(provider_auth_failure(
            "Missing bearer or basic authentication in header"
        ));
        assert!(provider_auth_failure(
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#
        ));
        assert!(provider_auth_failure("Invalid API key · Please run /login"));
        assert!(provider_auth_failure("HTTP 401 Unauthorized"));
        // Everything else keeps the engine's own words.
        assert!(!provider_auth_failure(
            "overloaded_error: try again shortly"
        ));
        assert!(!provider_auth_failure("engine exited with status 1"));
    }

    #[tokio::test]
    async fn a_raw_401_turn_failure_reads_as_a_sign_in_problem() {
        let (_directory, _store, sink, _session_id) = seeded_sink().await;
        let mapped =
            sink.legible_turn_error("Missing bearer or basic authentication in header".into());
        assert!(
            mapped
                .message
                .starts_with("Claude Code is not signed in on this machine."),
            "got: {}",
            mapped.message
        );
        // The engine's own words survive for whoever debugs the transcript.
        assert!(mapped
            .message
            .contains("Missing bearer or basic authentication in header"));
        let untouched = sink.legible_turn_error("engine exited with status 1".into());
        assert_eq!(untouched.message, "engine exited with status 1");
    }

    #[tokio::test]
    async fn a_relay_wired_session_keeps_the_relays_own_refusal() {
        let (_directory, store, bus, session_id) =
            seeded_session(HarnessKind::Codex, Some("0.147.0")).await;
        let sink = sink_for(
            store,
            bus,
            OwnerId::local(),
            session_id,
            1,
            HarnessKind::Codex,
            true,
            None,
            Vec::new(),
            None,
            None,
            None,
            crate::code::pr_refresh::HotPullRequests::default(),
        );
        // The relay's refusals already name the gateway; "sign in in your
        // own terminal" would be wrong on a hosted machine.
        let kept = sink.legible_turn_error("authentication_error: sign in required".into());
        assert_eq!(kept.message, "authentication_error: sign in required");
    }

    #[tokio::test]
    async fn an_update_quiesce_refuses_new_turns_until_resumed() {
        let (directory, store, sink, session_id) = seeded_sink().await;
        let owner = OwnerId::local();
        let mut session = get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap();
        session.lifecycle = CodeSessionLifecycle::Idle;
        assert!(save_session(&store, &session).await.unwrap());

        let worktree = directory.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let private = directory.path().join("private");
        std::fs::create_dir(&private).unwrap();
        let private_root =
            super::super::scratch::ScratchRoot::open_for_test(&private).expect("scratch root");
        let adapter = ScriptedAdapter::new(plain_text_script());
        let engine = adapter
            .launch(SessionSpec {
                owner: tidebreak_core::OwnerId::local(),
                session_id: tidebreak_core::CodeSessionId::new(),
                worktree,
                allowed_read_roots: Vec::new(),
                permission_mode: session.permission_mode,
                model: session.model.clone(),
                reasoning_effort: session.reasoning_effort,
                fast_mode: session.fast_mode,
                resume_ref: None,
                extra_argv: Vec::new(),
                extra_env: Vec::new(),
                relay_key_env: None,
                env: Vec::new(),
                approval: None,
                binary: Some(std::path::PathBuf::from("/scripted/engine")),
                sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
                browser: None,
            })
            .await
            .unwrap();
        let (quiesce_tx, quiesce_rx) = watch::channel(true);
        let handle = spawn_session_worker(
            session.clone(),
            engine,
            sink,
            AttachmentStore {
                blobs: None,
                private_root,
                engine_reads_images: false,
            },
            Arc::new(tokio::sync::Mutex::new(())),
            quiesce_rx,
        );

        // While the quiesce holds, a send must not start a turn: the caller
        // (submit_turn) parks it as a durable queue row instead.
        let (reply, refused) = oneshot::channel();
        handle
            .commands
            .send(WorkerCommand::RunTurn {
                message: "hello".into(),
                attachments: Vec::new(),
                trigger_delivery: None,
                reply,
            })
            .await
            .unwrap();
        match refused.await.unwrap() {
            Err(WorkerError::UpdateQuiesced) => {}
            other => panic!("expected an update-quiesced refusal, got {other:?}"),
        }
        assert!(list_turns(&store, &owner, session_id)
            .await
            .unwrap()
            .is_empty());

        // Ending the quiesce (a failed install) reopens admission; the same
        // send now runs to completion.
        quiesce_tx.send_replace(false);
        let (reply, ran) = oneshot::channel();
        handle
            .commands
            .send(WorkerCommand::RunTurn {
                message: "hello again".into(),
                attachments: Vec::new(),
                trigger_delivery: None,
                reply,
            })
            .await
            .unwrap();
        ran.await.unwrap().expect("the turn runs after the resume");
        assert_eq!(
            list_turns(&store, &owner, session_id).await.unwrap().len(),
            1
        );
    }
}
