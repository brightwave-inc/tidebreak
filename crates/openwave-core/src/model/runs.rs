use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::id::{ChatId, HostRootId, ProjectId};

use super::chat_settings::{NetworkPolicy, PermissionMode, ReasoningEffort};
use super::identity::{
    ChatRootAttachment, Project, RootAttachmentOrigin, MAX_ATTACHMENT_REVISION,
    MAX_ROOT_ATTACHMENTS,
};
use super::messages::{ToolCallRecord, ToolCallResolution};
use super::turns::{TurnCheckpointProgress, TurnRun};

/// A persistent conversation with an exact, ordered host-root projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct Chat {
    /// Stable identifier.
    pub id: ChatId,
    /// The project this chat belongs to, or `None` for a loose (projectless) chat.
    pub project_id: Option<ProjectId>,
    /// Human-facing title; `None` until one is set or derived.
    pub title: Option<String>,
    /// The model this chat runs against, or `None` to use the configured default.
    pub model: Option<String>,
    /// Reasoning-effort override for this chat, honored only by models that
    /// expose the control; `None` leaves the provider's default in force.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// How much this chat lets the agent do between approvals; `None` means
    /// [`PermissionMode::Ask`].
    pub permission_mode: Option<PermissionMode>,
    /// Outbound network access for code execution in this chat.
    #[serde(default)]
    pub network_policy: NetworkPolicy,
    /// CAS revision of this conversation's exact root projection.
    pub attachment_revision: i64,
    /// Ordered opaque roots available for future broker-backed operations.
    /// Live broker authorization remains mandatory and may revoke access at any
    /// time, regardless of this projection.
    pub root_attachments: Vec<ChatRootAttachment>,
    /// When the chat was created.
    pub created_at: DateTime<Utc>,
}

pub(crate) fn validate_project_root_projection(project: &Project) -> Result<(), &'static str> {
    if !(0..=MAX_ATTACHMENT_REVISION).contains(&project.attachment_revision) {
        return Err("project attachment revision is outside the supported range");
    }
    if project.root_attachments.len() > MAX_ROOT_ATTACHMENTS {
        return Err("project root attachment count exceeds the supported limit");
    }
    if !project.root_attachments.is_empty() && project.attachment_revision == 0 {
        return Err("a nonempty project root projection must have a positive revision");
    }
    let unique = project
        .root_attachments
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    if unique.len() != project.root_attachments.len() {
        return Err("project root attachments must be unique");
    }
    Ok(())
}

pub(crate) fn validate_chat_root_projection(chat: &Chat) -> Result<(), &'static str> {
    if !(0..=MAX_ATTACHMENT_REVISION).contains(&chat.attachment_revision) {
        return Err("chat attachment revision is outside the supported range");
    }
    if chat.root_attachments.len() > MAX_ROOT_ATTACHMENTS {
        return Err("chat root attachment count exceeds the supported limit");
    }
    if !chat.root_attachments.is_empty() && chat.attachment_revision == 0 {
        return Err("a nonempty chat root projection must have a positive revision");
    }
    let unique = chat
        .root_attachments
        .iter()
        .map(|attachment| attachment.root_id)
        .collect::<std::collections::HashSet<_>>();
    if unique.len() != chat.root_attachments.len() {
        return Err("chat root attachments must be unique");
    }
    if chat.project_id.is_none()
        && chat
            .root_attachments
            .iter()
            .any(|attachment| attachment.origin == RootAttachmentOrigin::ProjectDefault)
    {
        return Err("a standalone chat cannot contain project-default roots");
    }
    let mut conversation_root_seen = false;
    for attachment in &chat.root_attachments {
        match attachment.origin {
            RootAttachmentOrigin::ProjectDefault if conversation_root_seen => {
                return Err("project-default roots must precede conversation roots");
            }
            RootAttachmentOrigin::Conversation => conversation_root_seen = true,
            RootAttachmentOrigin::ProjectDefault => {}
        }
    }
    Ok(())
}

pub(crate) fn validate_chat_root_projection_against_project(
    chat: &Chat,
    project_roots: &[HostRootId],
) -> Result<(), &'static str> {
    validate_chat_root_projection(chat)?;
    if chat.project_id.is_none() && !project_roots.is_empty() {
        return Err("a standalone chat cannot snapshot project roots");
    }
    if chat.root_attachments.len() < project_roots.len() {
        return Err("chat is missing project root defaults");
    }
    for (expected, actual) in project_roots.iter().zip(&chat.root_attachments) {
        if actual.root_id != *expected || actual.origin != RootAttachmentOrigin::ProjectDefault {
            return Err("chat project root snapshot does not match current project defaults");
        }
    }
    if chat.root_attachments[project_roots.len()..]
        .iter()
        .any(|attachment| attachment.origin != RootAttachmentOrigin::Conversation)
    {
        return Err("chat-specific roots must follow project defaults");
    }
    Ok(())
}

/// One durable foreground or sandboxed background execution context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRun {
    /// Stable idempotency identity.
    pub id: crate::id::AgentRunId,
    /// Conversation that owns this run and its events.
    pub chat_id: ChatId,
    /// Foreground coordinator that owns this run. Always absent at depth zero.
    pub parent_id: Option<crate::id::AgentRunId>,
    /// Exact tool-call identity that requested a sandbox child.
    pub spawn_call_id: Option<crate::id::CallId>,
    /// Who advances the run: the foreground coordinator or the background
    /// scheduler.
    pub tier: AgentRunTier,
    /// Where the run's loop executes.
    #[serde(default)]
    pub execution_location: AgentRunExecutionLocation,
    /// Explicit bounded hierarchy depth. OpenWave v1 permits only zero or one.
    pub depth: u8,
    /// Durable lifecycle state.
    pub status: AgentRunStatus,
    /// Exact delegated task for a sandbox run. Foreground runs have no task.
    pub input: Option<String>,
    /// Model selection frozen when a sandbox run was admitted, inherited from
    /// its origin turn so the child cannot silently execute against a different
    /// model than the conversation that delegated it. Foreground coordinators
    /// carry the selection on their turns instead, and runs admitted before this
    /// was persisted read back as absent.
    pub model: Option<String>,
    /// Failure attempts already started. Reclaiming an expired lease starts a
    /// new attempt; later continuation resumptions will not.
    pub attempt_count: i32,
    /// Maximum failure attempts permitted for this run.
    pub max_attempts: i32,
    /// Exact worker lease segments issued over the run's lifetime.
    pub claim_count: i32,
    /// Earliest time queued or retry-wait work may be claimed.
    pub available_at: DateTime<Utc>,
    /// Absolute wall-clock limit for sandbox work. Foreground coordinators do
    /// not carry a scheduler deadline.
    pub deadline_at: Option<DateTime<Utc>>,
    /// Exact worker claim identity while running or cancelling.
    pub lease_token: Option<Uuid>,
    /// When the current worker claim becomes stale.
    pub lease_expires_at: Option<DateTime<Utc>>,
    /// When the first worker claim began.
    pub started_at: Option<DateTime<Utc>>,
    /// When the run entered a terminal state.
    pub finished_at: Option<DateTime<Utc>>,
    /// Stable machine-readable failure category.
    pub last_error_code: Option<String>,
    /// Bounded diagnostic detail for local operators.
    pub last_error_detail: Option<String>,
    /// When the run was durably accepted.
    pub created_at: DateTime<Utc>,
    /// When its durable state last changed.
    pub updated_at: DateTime<Utc>,
}

impl AgentRun {
    /// Recursive agent spawning is deliberately excluded from the initial model.
    pub const MAX_DEPTH: u8 = 1;
    /// Maximum persisted delegated task length.
    pub const MAX_INPUT_LEN: usize = 65_536;
    /// Maximum persisted model identifier length.
    pub const MAX_MODEL_LEN: usize = TurnRun::MAX_MODEL_LEN;
    /// Default failure-attempt budget for sandboxed work. Five attempts give
    /// the worker's exponential schedule and the provider's `Retry-After`
    /// room to stretch waits toward the run's wall-clock envelope; at three
    /// the budget was gone before either could bind.
    pub const DEFAULT_MAX_ATTEMPTS: i32 = 5;
    /// No-progress window for one sandbox run.
    ///
    /// This is a stall detector, not a total wall clock: every committed
    /// checkpoint and every settled executor batch restarts it, so a run
    /// doing real work can live far past one window and only a run that
    /// makes no durable progress for a whole window is failed by the
    /// deadline scan. It still stamps the initial `deadline_at` at
    /// admission, before the first progress event exists to extend it.
    pub const DEFAULT_MAX_DURATION: chrono::Duration = chrono::Duration::hours(1);
    /// Largest accepted scheduler concurrency bound.
    pub const MAX_CONCURRENCY_LIMIT: u32 = 1_024;
    /// Default maximum concurrently active background agents in one chat.
    pub const DEFAULT_MAX_ACTIVE_BACKGROUND_AGENTS: u32 = 5;
    /// Maximum stable failure-category length.
    pub const MAX_ERROR_CODE_LEN: usize = 128;
    /// Maximum persisted diagnostic-detail length.
    pub const MAX_ERROR_DETAIL_LEN: usize = 4_096;
    /// Maximum final text stored in an immutable sandbox result receipt.
    pub const MAX_RESULT_LEN: usize = 65_536;
}

/// One durable, ordered line of live progress published by a background run.
///
/// A background run is otherwise only observable at its edges — the state it is
/// in, the checkpoint it currently sits on, and the result it eventually
/// submits. This is the stream in between: bounded prose the run itself
/// produced, ordered by a per-run [`sequence`](Self::sequence) so a reader can
/// resume from what it already has rather than re-reading everything.
///
/// It is deliberately not correctness state. No transition reads it, and a
/// dropped line costs an observer one gap, never a wrong decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRunProgressEntry {
    pub run_id: crate::id::AgentRunId,
    /// Monotonic per-run ordering, starting at one. Gaps are possible once
    /// retention trims the oldest lines; order never is.
    pub sequence: i64,
    pub text: String,
    pub created_at: DateTime<Utc>,
}

impl AgentRunProgressEntry {
    /// Maximum persisted characters in one line. A progress line is a sentence
    /// of narration, not a transcript; anything longer is truncated on the way
    /// in rather than rejected, because losing the line entirely would be the
    /// worse outcome for an observer.
    pub const MAX_TEXT_LEN: usize = 2_048;
    /// Lines retained per run. A long-running child can narrate indefinitely,
    /// and this stream is disposable observation, so the oldest lines are
    /// dropped rather than allowed to grow the journal without bound.
    pub const RETAINED_PER_RUN: i64 = 200;
    /// Maximum lines one read may return.
    pub const MAX_PAGE: u64 = 200;
    /// Lines one read returns when the caller does not ask for a bound.
    pub const DEFAULT_PAGE: u64 = 50;
    /// Maximum length of a producer's own identity for a line.
    pub const MAX_SOURCE_KEY_LEN: usize = 96;
}

/// Immutable ownership receipt for one admitted sandbox child.
///
/// The origin turn is intentionally distinct from the long-lived foreground
/// coordinator. A foreground run can span many turns, while every sandbox
/// child belongs to the exact turn and model call that delegated its task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxAgentAdmission {
    /// Deterministic child identity derived from [`Self::spawn_call_id`].
    pub child_run_id: crate::id::AgentRunId,
    /// Foreground coordinator that owned the origin turn.
    pub parent_run_id: crate::id::AgentRunId,
    /// Exact foreground turn that admitted the child.
    pub origin_turn_id: crate::id::TurnId,
    /// Conversation shared by the origin turn, parent, and child.
    pub chat_id: ChatId,
    /// Exact model call that requested the child.
    pub spawn_call_id: crate::id::CallId,
    /// Optional exact file identity delegated only to this child.
    ///
    /// This immutable receipt is not host authority and does not imply that a
    /// sandbox file-read capability exists.
    pub resource: Option<crate::agent_tools::SandboxAgentFileResource>,
    /// Database time at which admission committed.
    pub admitted_at: DateTime<Utc>,
}

/// Immutable receipt for one non-blocking foreground sandbox spawn.
///
/// The receipt binds model output, exact turn-claim provenance, child
/// admission, transcript tool history, accounting, and journal order. It is
/// read before mutable lease or steer checks so an ambiguous commit retry can
/// always recover the original transition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxSpawnCheckpoint {
    pub call_id: crate::id::CallId,
    pub child_run_id: crate::id::AgentRunId,
    pub parent_run_id: crate::id::AgentRunId,
    pub origin_turn_id: crate::id::TurnId,
    pub chat_id: ChatId,
    pub lease_token: Uuid,
    pub attempt_count: i32,
    pub claim_count: i32,
    pub provider_id: String,
    pub history_order: i64,
    pub arguments: serde_json::Value,
    pub result: String,
    pub steer_revision: i64,
    pub event_ordinal: i32,
    pub progress: TurnCheckpointProgress,
    /// The rest of the batch this spawn came from, in model order, still
    /// ungated. Carried durably so the claim that resumes the parked turn
    /// gates them under their original call ids instead of asking the model
    /// again.
    pub remaining_requests: Vec<crate::agent::SandboxAgentSpawnRequest>,
    pub event_seq: i64,
    pub committed_at: DateTime<Utc>,
}

/// One proposed non-blocking sandbox spawn checkpoint.
///
/// `arguments` and `result` are supplied explicitly because their canonical
/// bytes are part of the immutable model-call identity. The storage layer
/// parses and validates both closed contracts before committing anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxSpawnCheckpointRequest {
    pub origin_turn_id: crate::id::TurnId,
    pub lease_token: Uuid,
    pub expected_steer_revision: i64,
    pub call_id: crate::id::CallId,
    pub provider_id: String,
    pub arguments: serde_json::Value,
    pub result: String,
    pub event_ordinal: i32,
    pub progress: TurnCheckpointProgress,
    /// The rest of the batch the model named in this step, in model order and
    /// not yet through the approval gate. The parked turn's next claim reads
    /// them back and gates them without a further model call.
    pub remaining_requests: Vec<crate::agent::SandboxAgentSpawnRequest>,
    /// Settings-resolved per-chat ceiling on nonterminal background runs.
    pub max_active_background_agents: u32,
    /// Host-resolved execution location for the child admitted by this atomic
    /// checkpoint. Existing callers use the in-process default.
    pub execution_location: AgentRunExecutionLocation,
    /// Whether the spawn parked on the approval gate first, so a pending
    /// server tool-call row already exists for `call_id` and must be finalized
    /// rather than inserted. The row's approval must read approved: admission
    /// happens strictly after the decision commits.
    pub approval_gated: bool,
}

/// Immutable final text submitted by one exact sandbox worker lease.
///
/// This receipt is intentionally separate from [`AgentRun`]: clearing the live
/// lease at terminal transition must not erase the proof needed to recover an
/// ambiguous submission retry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRunResult {
    /// The terminal sandbox run.
    pub agent_run_id: crate::id::AgentRunId,
    /// Exact worker lease that committed this result.
    pub lease_token: Uuid,
    /// Worker attempt that produced the result.
    pub attempt_count: i32,
    /// Exact claim segment that produced the result.
    pub claim_count: i32,
    /// Typed terminal payload returned to the parent in a later delivery slice.
    pub payload: AgentRunResultPayload,
    /// Bounded deterministic display text for the terminal payload.
    pub text: String,
    /// Database time at which the terminal submission committed.
    pub submitted_at: DateTime<Utc>,
}

/// One immutable terminal outcome produced by a sandbox child.
///
/// These are deliberately proposals rather than authority. A folder-access
/// proposal has no root identity, path, grant, or client-call identity; the
/// foreground parent must independently decide whether to ask the trusted
/// client through its ordinary tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentRunResultPayload {
    /// Ordinary final text from the sandbox model.
    FinalText { text: String },
    /// Files the run wrote and then submitted as its deliverables.
    Submission {
        /// Outputs the run named, in the order it named them.
        outputs: Vec<AgentRunSubmittedOutput>,
        /// Bounded prose describing what the run produced.
        summary: String,
    },
    /// A sandbox request for its foreground parent to consider folder consent.
    FolderAccessProposal {
        /// Non-authoritative, validated request arguments.
        request: crate::RequestFolderAccessArgs,
    },
    /// The sandbox was durably stopped before producing an ordinary result.
    Cancelled {
        /// Stable reason recorded by the cancellation state machine.
        reason: AgentRunCancellationReason,
    },
}

/// One file a background run submitted as a deliverable.
///
/// The filename is the identity the model worked with and the name the user
/// sees; the output id is the host's resolution of that name against the
/// conversation's own output record, captured at submission time so a later
/// rename or delete cannot rewrite what the run claimed to have produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRunSubmittedOutput {
    /// Conversation output the submitted filename resolved to.
    pub output_id: crate::id::OutputId,
    /// Filename the run wrote under `output/`, which names the output.
    pub filename: String,
}

/// Durable reason a sandbox child was cancelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunCancellationReason {
    /// Cancellation was requested for this child directly.
    Requested,
    /// The exact foreground turn that admitted the child was cancelled.
    ParentTurnCancelled,
    /// The exact foreground turn that admitted the child failed permanently.
    ParentTurnFailed,
}

/// Immutable executor identity retained by a sandbox cancellation request.
///
/// This is operational fencing data for trusted workers and is never part of
/// the renderer-facing cancellation response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentRunCancellationSignal {
    pub agent_run_id: crate::id::AgentRunId,
    pub lease_token: Uuid,
    pub attempt_count: i32,
    pub claim_count: i32,
}

impl AgentRunCancellationReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::ParentTurnCancelled => "parent_turn_cancelled",
            Self::ParentTurnFailed => "parent_turn_failed",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "requested" => Some(Self::Requested),
            "parent_turn_cancelled" => Some(Self::ParentTurnCancelled),
            "parent_turn_failed" => Some(Self::ParentTurnFailed),
            _ => None,
        }
    }
}

/// One immutable result delivered from a sandbox child to its foreground parent.
///
/// Delivery is written in the same transaction as the child's terminal result;
/// waking or consuming a parent continuation is deliberately a later concern.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRunInboxEntry {
    /// Foreground coordinator that owns this child result.
    pub parent_run_id: crate::id::AgentRunId,
    /// Completed sandbox child. One child has exactly one inbox entry.
    pub child_run_id: crate::id::AgentRunId,
    /// Chat shared by parent and child.
    pub chat_id: ChatId,
    /// Exact result receipt that was delivered.
    pub result: AgentRunResult,
    /// Durable continuation state for this exact child result.
    pub status: AgentRunInboxStatus,
    /// Number of distinct continuation leases issued for this delivery.
    pub claim_count: i32,
    /// Exact live continuation lease, when a worker currently owns it.
    pub lease_token: Option<Uuid>,
    /// Database-clock expiry of the live continuation lease.
    pub lease_expires_at: Option<DateTime<Utc>>,
    /// Exact lease that durably consumed this delivery.
    pub consumed_lease_token: Option<Uuid>,
    /// Database time at which consumption committed.
    pub consumed_at: Option<DateTime<Utc>>,
    /// Database time when the durable parent delivery committed.
    pub delivered_at: DateTime<Utc>,
}

/// Durable continuation lifecycle for one parent inbox delivery.
///
/// A delivery is immutable; only its fenced consumption state advances. A
/// continuation lease may be reclaimed after expiry, while a consumed receipt
/// remains available for an ambiguous exact retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentRunInboxStatus {
    /// The parent has not yet claimed this child result.
    Pending,
    /// One exact continuation lease owns this child result.
    Claimed,
    /// One exact continuation lease durably consumed this child result.
    Consumed,
    /// The parent turn was cancelled before this delivery could resume it.
    Cancelled,
}

impl AgentRunInboxStatus {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Consumed => "consumed",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "claimed" => Some(Self::Claimed),
            "consumed" => Some(Self::Consumed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// Run tier of an [`AgentRun`]: who advances the run.
///
/// Formerly one half of `AgentRunExecution` (`foreground | sandbox`), which
/// fused this axis with [`AgentRunExecutionLocation`]. The two agreed only
/// while every run executed in-process, so the field split before a second
/// location could exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentRunTier {
    /// Conversation coordinator advanced by foreground turn work.
    Foreground,
    /// Isolated background work advanced by the background-run scheduler.
    Background,
}

impl AgentRunTier {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Background => "background",
        }
    }
}

/// Where an [`AgentRun`]'s loop executes.
///
/// Every run executes inside the OpenWave server process today. A run
/// executing inside an execution provider's boundary adds a variant here
/// rather than a second meaning to [`AgentRunTier`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentRunExecutionLocation {
    /// The loop runs inside the OpenWave server process.
    #[default]
    InProcess,
    /// The loop runs inside a sandbox-resident container, host-driven over the
    /// versioned sandbox-agent wire protocol. The in-process scheduler does not
    /// advance these runs; the sandbox-resident driver provisions the container,
    /// attaches, proxies model inference back over the reverse channel, and
    /// commits the result through the same fenced result path.
    Container,
}

impl AgentRunExecutionLocation {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InProcess => "in_process",
            Self::Container => "container",
        }
    }
}

/// Durable lifecycle of an [`AgentRun`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentRunStatus {
    /// Foreground coordinator is available to own chat turns.
    Active,
    /// Sandboxed work was accepted and awaits a bounded scheduler slot.
    Queued,
    /// One exact scheduler lease currently owns the run.
    Running,
    /// Cancellation was requested while an exact worker lease remained live.
    Cancelling,
    /// The run checkpointed and released its worker for a durable dependency.
    Waiting,
    /// Replay-safe work awaits another scheduler claim.
    RetryWait,
    /// The run submitted its final result successfully.
    Completed,
    /// The run failed permanently or cannot be replayed safely.
    Failed,
    /// The run was cancelled and has quiesced.
    Cancelled,
}

impl AgentRunStatus {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Cancelling => "cancelling",
            Self::Waiting => "waiting",
            Self::RetryWait => "retry_wait",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether no worker may advance this run without a new explicit command.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Immutable request for one tool operation checkpointed by a sandbox agent.
///
/// This is intentionally separate from foreground [`ToolCallRecord`]: a
/// sandbox has no foreground turn id, and its checkpoint must be fenced by the
/// sandbox worker lease that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxToolCallRequest {
    pub id: crate::id::CallId,
    pub agent_run_id: crate::id::AgentRunId,
    pub chat_id: ChatId,
    pub provider_id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

impl SandboxToolCallRequest {
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        let labels_valid = [self.provider_id.as_str(), self.name.as_str()]
            .into_iter()
            .all(|value| {
                !value.is_empty()
                    && value.len() <= ToolCallRecord::MAX_LABEL_LEN
                    && !value.contains('\0')
            });
        self.id.0 != Uuid::nil()
            && self.agent_run_id.0 != Uuid::nil()
            && self.chat_id.0 != Uuid::nil()
            && labels_valid
            && serde_json::to_vec(&self.arguments)
                .is_ok_and(|arguments| arguments.len() <= ToolCallRecord::MAX_ARGUMENT_BYTES)
    }
}

/// One entry in a sandbox checkpoint park.
///
/// `resolution: Some(_)` inserts the row already terminal together with its
/// receipt in the same transaction: the host answered the call itself and no
/// executor lane will ever see it.
#[derive(Debug, Clone, PartialEq)]
pub struct SandboxToolCallParkEntry {
    pub call: SandboxToolCallRequest,
    pub resolution: Option<ToolCallResolution>,
}

/// Durable lifecycle of sandbox-owned tool work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SandboxToolCallStatus {
    Accepted,
    Claimed,
    /// A classified-transient failure parked awaiting its single bounded
    /// retry. The call becomes claimable again once `retry_at` passes; a
    /// second failure resolves terminally.
    RetryWait,
    Completed,
    Failed,
    Cancelled,
}

impl SandboxToolCallStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Claimed => "claimed",
            Self::RetryWait => "retry_wait",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// One persisted sandbox tool checkpoint and its current execution lease.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxToolCall {
    pub id: crate::id::CallId,
    pub agent_run_id: crate::id::AgentRunId,
    pub chat_id: ChatId,
    pub provider_id: String,
    pub name: String,
    pub arguments: serde_json::Value,
    pub status: SandboxToolCallStatus,
    pub park_lease_token: Uuid,
    pub park_attempt_count: i32,
    pub park_claim_count: i32,
    /// Position of this call within the model step that parked it, from zero.
    /// The transcript replays a step's calls in this order.
    pub batch_ordinal: i16,
    pub executor_lease_token: Option<Uuid>,
    pub executor_lease_expires_at: Option<DateTime<Utc>>,
    /// When the call's single bounded retry becomes claimable. Set exactly
    /// once, by the transient failure that scheduled the retry, and kept
    /// through the second attempt as the spent-retry marker.
    pub retry_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

impl SandboxToolCall {
    pub const MAX_RESULT_BYTES: usize = ToolCallRecord::MAX_RESULT_BYTES;
}

/// Exact host-broker identity exposed only by a trusted native claim.
///
/// The root remains opaque and the path remains relative. Neither field is
/// copied into renderer activity, pending-work projections, or receipts.
#[derive(Debug, Clone, PartialEq)]
pub struct DelegatedFileReadClaim {
    pub call: SandboxToolCall,
    pub root_id: crate::id::HostRootId,
    pub relative_path: String,
}

/// Immutable terminal result receipt for sandbox tool work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxToolCallReceipt {
    pub call_id: crate::id::CallId,
    pub executor_lease_token: Uuid,
    pub status: SandboxToolCallStatus,
    pub result: String,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    pub resolved_at: DateTime<Utc>,
}
