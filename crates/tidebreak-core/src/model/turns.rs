use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::id::{AgentRunId, CallId, MessageId, SessionId, TurnId};
use crate::provider::{Usage, VendorWebSearch};

use super::is_false;
use super::messages::ToolCallRecord;

/// Durable execution state of one user turn.
///
/// A turn is accepted once under its stable [`TurnId`], then claimed under an
/// exact lease before model or tool work begins. Keeping this state separate
/// from messages lets API acceptance, worker ownership, and terminal resolution
/// be fenced without treating append-only conversation content as a job queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRun {
    /// Stable turn and idempotency identity.
    pub id: TurnId,
    /// Conversation this turn belongs to.
    pub chat_id: SessionId,
    /// Foreground coordinator that owns this conversation segment.
    pub agent_run_id: AgentRunId,
    /// Exact persisted user message that supplied this turn's initial input.
    pub input_message_id: MessageId,
    /// Exact designated terminal assistant message committed with successful
    /// completion. The composite database FK enforces its message/chat/turn
    /// identity; [`Store::complete_turn`](crate::storage::Store::complete_turn)
    /// enforces the assistant role because a foreign key cannot bind a literal
    /// role value.
    pub output_message_id: Option<MessageId>,
    /// Model selected when the turn was accepted.
    pub model: String,
    /// Skills the user explicitly invoked for this turn, in submitted order.
    ///
    /// Empty for an ordinary turn, where the model routes on the prompt's skill
    /// catalog by itself. A non-empty list is a user instruction captured with
    /// the turn, exactly like the model selection above, so a reloaded
    /// transcript still shows what the turn was told to use.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invoked_skills: Vec<String>,
    /// Whether any of this turn's user text came from voice transcription.
    ///
    /// The boolean is retained for exact idempotency and retry. The explanatory
    /// note itself lives in the input message's model-only content.
    #[serde(default, skip_serializing_if = "is_false")]
    pub voice_input_used: bool,
    /// Durable delivery state.
    pub status: TurnRunStatus,
    /// Failure attempts already started. Client-execution resumptions stay
    /// within the same attempt and do not consume this retry budget.
    pub attempt_count: i32,
    /// Maximum failure attempts permitted for this turn.
    pub max_attempts: i32,
    /// Worker lease segments already issued, including resumptions within one
    /// failure attempt.
    pub claim_count: i32,
    /// Model calls committed through the latest durable client checkpoint.
    /// Workers use this baseline when a later lease segment resumes.
    pub model_steps: i32,
    /// Provider usage committed through the latest durable client checkpoint.
    /// The terminal event carries the final total after the last live segment.
    pub usage: Usage,
    /// Earliest time queued, retry-wait, or resuming work may be claimed.
    pub available_at: DateTime<Utc>,
    /// Exact claim identity required for heartbeat and resolution writes.
    pub lease_token: Option<Uuid>,
    /// When the current claim becomes stale.
    pub lease_expires_at: Option<DateTime<Utc>>,
    /// When the first claim began.
    pub started_at: Option<DateTime<Utc>>,
    /// When this turn entered a terminal state.
    pub finished_at: Option<DateTime<Utc>>,
    /// Stable machine-readable failure category.
    pub last_error_code: Option<String>,
    /// Bounded diagnostic detail for local operators.
    pub last_error_detail: Option<String>,
    /// Durable generation revision captured before model work begins.
    pub steer_revision: i64,
    /// When the most recent durable steer application committed.
    pub last_steer_applied_at: Option<DateTime<Utc>>,
    /// When this turn was accepted.
    pub created_at: DateTime<Utc>,
    /// When its durable state last changed.
    pub updated_at: DateTime<Utc>,
}

impl TurnRun {
    /// New turns retry transient failures while per-attempt effect provenance
    /// prevents ambiguous tool work from being replayed. Five attempts give
    /// the turn worker's exponential schedule and the provider's
    /// `Retry-After` room to stretch waits toward the ten-minute envelope; at
    /// three the budget was gone in about a second of backoff unless a hint
    /// stretched a wait.
    pub const DEFAULT_MAX_ATTEMPTS: i32 = 5;
    /// Maximum persisted model identifier length.
    pub const MAX_MODEL_LEN: usize = 512;
    /// Maximum skills one turn may explicitly invoke.
    ///
    /// A bound, not a product limit: a user picks a couple of skills for a
    /// turn, and this keeps one accepted turn's persisted instruction finite.
    pub const MAX_INVOKED_SKILLS: usize = 8;
    /// Maximum persisted invoked skill name length. Matches the skill parser's
    /// own name bound, so a name that could never name a staged skill is
    /// refused before it reaches storage.
    pub const MAX_INVOKED_SKILL_NAME_LEN: usize = 64;
    /// Maximum persisted machine-readable error code length.
    pub const MAX_ERROR_CODE_LEN: usize = 128;
    /// Maximum persisted diagnostic detail length.
    pub const MAX_ERROR_DETAIL_LEN: usize = 4096;
}

/// Durable delivery state of a [`TurnRun`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TurnRunStatus {
    /// Accepted durably and eligible to be claimed at `available_at`.
    Queued,
    /// Currently owned by the exact lease token and expiry on the turn.
    Running,
    /// Cancellation was requested while a worker still owns the exact lease.
    /// The chat remains occupied until that worker acknowledges quiescence or
    /// the expired lease is cleaned up.
    Cancelling,
    /// The worker checkpointed safely and released its lease while one exact
    /// durable client call executes on the host.
    WaitingForClient,
    /// The worker checkpointed safely and released its lease while one exact
    /// sandbox child result is awaited in the foreground inbox.
    WaitingForAgentRun,
    /// Cancellation was requested after the client call may have started. The
    /// chat stays occupied until that exact call reports a terminal result.
    CancellingClient,
    /// The blocking client call resolved and the checkpoint is eligible for a
    /// fresh worker lease without consuming another failure attempt.
    Resuming,
    /// Failed safely before an ambiguous side effect and awaits another claim.
    RetryWait,
    /// Produced a final answer successfully.
    Completed,
    /// Failed permanently or cannot be replayed safely.
    Failed,
    /// Cancelled before producing a final answer.
    Cancelled,
}

impl TurnRunStatus {
    /// Every status that means the conversation is still working.
    ///
    /// One definition, because "busy" must mean the same thing to the host's
    /// quiescence check and to the reader's attention badge. A new
    /// non-terminal status added without landing here would make a live
    /// conversation look settled in one of them.
    pub const LIVE: &'static [Self] = &[
        Self::Queued,
        Self::Running,
        Self::Cancelling,
        Self::WaitingForClient,
        Self::WaitingForAgentRun,
        Self::CancellingClient,
        Self::Resuming,
        Self::RetryWait,
    ];

    /// Whether this status means the conversation is still working.
    #[must_use]
    pub fn is_live(self) -> bool {
        Self::LIVE.contains(&self)
    }

    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Cancelling => "cancelling",
            Self::WaitingForClient => "waiting_for_client",
            Self::WaitingForAgentRun => "waiting_for_agent_run",
            Self::CancellingClient => "cancelling_client",
            Self::Resuming => "resuming",
            Self::RetryWait => "retry_wait",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether no worker may claim this turn without an explicit transition.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// Database tokens that mean this turn was cancelled.
    ///
    /// Code writes `interrupted`; chat historically wrote `cancelled`. Both
    /// remain in the merged status check, so readers have to match both.
    pub const CANCELLED: &'static [&'static str] = &["cancelled", "interrupted"];

    /// Database tokens that mean this turn is finished.
    pub const TERMINAL: &'static [&'static str] =
        &["completed", "failed", "cancelled", "interrupted"];
}

/// Explicit completion policy for a durable multi-child foreground wait.
///
/// The first local fan-out contract deliberately supports only `All`: the
/// parent resumes once every requested child has delivered a terminal inbox
/// result. Keeping the policy in the receipt makes request identity explicit
/// and leaves an additive path for an eventual `Any` policy without changing
/// the atomic checkpoint shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentRunWaitCondition {
    /// Resume only after every child in request order has delivered a result.
    All,
}

impl AgentRunWaitCondition {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
        }
    }
}

/// One durable checkpoint for an ordered set of sandbox children.
///
/// Child order is part of immutable request identity. Results are returned in
/// this order even when the children finish in a different order.
///
/// The provider-facing identity of the request — provider id, history order,
/// and canonical arguments — lives on the `wait_for_agents` tool call this
/// shares an id with, and is not mirrored here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnAgentRunWaitSet {
    /// Stable model-call identity for this wait request.
    pub id: crate::id::CallId,
    /// Foreground coordinator shared by the turn and every child.
    pub parent_run_id: crate::id::AgentRunId,
    /// Exact origin turn that admitted every child and owns this checkpoint.
    pub turn_id: TurnId,
    /// Conversation shared by the turn and every child.
    pub chat_id: SessionId,
    /// Bounded, unique children in caller-requested order.
    pub child_run_ids: Vec<crate::id::AgentRunId>,
    /// Completion policy committed as immutable request identity.
    pub condition: AgentRunWaitCondition,
    /// Exact foreground lease that created the checkpoint.
    pub park_lease_token: Uuid,
    /// Steering generation observed by the model output being checkpointed.
    pub expected_steer_revision: i64,
    /// Failure attempt containing the checkpoint.
    pub attempt_count: i32,
    /// Exact lease-segment ordinal containing the checkpoint.
    pub claim_count: i32,
    /// Progress committed before releasing the foreground worker.
    pub progress: TurnCheckpointProgress,
    /// Exact attempt-event ordinal reserved for the terminal tool result.
    pub event_ordinal: i32,
    /// Per-chat journal receipt for the terminal tool result, once closed.
    pub event_seq: Option<i64>,
    /// Durable lifecycle of this ordered wait.
    pub status: TurnAgentRunWaitStatus,
    /// Database time at which the checkpoint committed.
    pub parked_at: DateTime<Utc>,
    /// Database time at which all inbox results resumed the turn, if any.
    pub closed_at: Option<DateTime<Utc>>,
    /// Exact continuation identity that consumed all results, if resumed.
    pub resume_token: Option<Uuid>,
}

/// One proposed atomic ordered sandbox-child wait checkpoint.
///
/// Canonical arguments are retained explicitly as immutable model-call
/// identity. Storage validates that they encode the same ordered child list as
/// `child_run_ids` before committing the pending orchestration call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRunWaitSetCheckpointRequest {
    pub call_id: crate::id::CallId,
    pub origin_turn_id: crate::id::TurnId,
    pub child_run_ids: Vec<crate::id::AgentRunId>,
    pub condition: AgentRunWaitCondition,
    pub lease_token: Uuid,
    pub expected_steer_revision: i64,
    pub provider_id: String,
    pub arguments: serde_json::Value,
    pub event_ordinal: i32,
    pub progress: TurnCheckpointProgress,
}

/// Minimal recovery hint for an ordered sandbox-child wait that appears ready.
///
/// This projection deliberately carries no ownership or consumption authority:
/// workers must pass a fresh continuation token to the exact resume transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentRunWaitSetCandidate {
    /// Stable wait-call identity.
    pub wait_id: crate::id::CallId,
    /// Database time at which the last required child result was delivered.
    pub ready_at: DateTime<Utc>,
}

impl TurnAgentRunWaitSet {
    /// Keep one ordered wait within the durable result-envelope budget.
    ///
    /// Spawn admission and `max_active_background_agents` share this ceiling
    /// ([`super::runs::AgentRun::MAX_ACTIVE_BACKGROUND_AGENTS`]) so a turn
    /// cannot hold more unsettled children than one wait can consume.
    pub const MAX_CHILDREN: usize = 4;
}

/// Durable lifecycle of a [`TurnAgentRunWaitSet`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TurnAgentRunWaitStatus {
    /// The foreground turn is durably parked awaiting the child result.
    Waiting,
    /// The exact child inbox delivery was consumed and woke the turn.
    Resumed,
    /// The turn was cancelled before the child result could wake it.
    Cancelled,
}

impl TurnAgentRunWaitStatus {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Resumed => "resumed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Immutable request for one tool operation that must execute in a trusted
/// client rather than the server process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientToolCallRequest {
    /// Caller-supplied idempotency identity.
    pub id: CallId,
    /// Owning conversation.
    pub chat_id: SessionId,
    /// Turn checkpointed for this call.
    pub turn_id: TurnId,
    /// Provider/tool namespace that produced the request.
    pub provider_id: String,
    /// Tool name understood by the trusted client.
    pub name: String,
    /// Canonical model-supplied arguments.
    pub arguments: serde_json::Value,
}

impl ClientToolCallRequest {
    /// Whether this request fits the durable client-execution contract.
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
            && self.chat_id.0 != Uuid::nil()
            && self.turn_id.0 != Uuid::nil()
            && labels_valid
            && serde_json::to_vec(&self.arguments)
                .is_ok_and(|arguments| arguments.len() <= ToolCallRecord::MAX_ARGUMENT_BYTES)
    }
}

/// Progress atomically committed with one client-execution checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnCheckpointProgress {
    /// Model calls consumed since the preceding durable checkpoint.
    pub model_steps: i32,
    /// Provider usage incurred since the preceding durable checkpoint.
    pub usage: Usage,
}

/// Immutable receipt for one turn parked on a client tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnClientWait {
    /// Exact durable client call.
    pub call_id: CallId,
    /// Turn that released its worker lease.
    pub turn_id: TurnId,
    /// Owning conversation.
    pub chat_id: SessionId,
    /// Worker lease segment that created the checkpoint.
    pub park_lease_token: Uuid,
    /// Failure attempt containing the checkpoint.
    pub attempt_count: i32,
    /// Exact lease-segment ordinal containing the checkpoint.
    pub claim_count: i32,
    /// Exact progress delta committed by this checkpoint.
    pub progress: TurnCheckpointProgress,
    /// Vendor search allowance left after the model produced this checkpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_vendor_web_search: Option<VendorWebSearch>,
    /// Durable wait lifecycle.
    pub status: TurnClientWaitStatus,
    /// Store-owned time when parking committed.
    pub parked_at: DateTime<Utc>,
    /// Store-owned time when the wait stopped blocking the turn.
    pub closed_at: Option<DateTime<Utc>>,
}

/// Durable lifecycle of a turn/client-call checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TurnClientWaitStatus {
    /// The exact client call still blocks the turn.
    Waiting,
    /// The exact client call resolved and made the turn resumable.
    Resumed,
    /// Cancellation won and the turn will not resume from this checkpoint.
    Cancelled,
}

impl TurnClientWaitStatus {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Resumed => "resumed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// One message accepted while its chat had a live turn, waiting its turn.
///
/// Immutable client request identity reserved before mutable turn admission.
///
/// The request deliberately carries only caller-supplied identity. Model
/// routing, skill availability, blob metadata, document ownership, and model
/// capabilities are mutable admission prerequisites and therefore never enter
/// this fingerprint. An exact retry can compare this record without consulting
/// any of those moving catalogs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnAdmissionRequest {
    pub id: TurnId,
    pub chat_id: SessionId,
    pub content: String,
    pub attachments: Vec<Uuid>,
    pub file_attachments: Vec<crate::id::DocumentId>,
    pub invoked_skills: Vec<String>,
    pub voice_input_used: bool,
}

impl TurnAdmissionRequest {
    /// Versioned canonical fingerprint of the exact caller request.
    ///
    /// Every variable-length field is length-prefixed and every ordered list
    /// carries its element count, so concatenation cannot create aliases. The
    /// turn id is the table key; the owning chat remains both a column and part
    /// of the fingerprint as defense in depth.
    #[must_use]
    pub fn fingerprint(&self) -> [u8; 32] {
        fn put_bytes(digest: &mut Sha256, bytes: &[u8]) {
            digest.update((bytes.len() as u64).to_be_bytes());
            digest.update(bytes);
        }

        let mut digest = Sha256::new();
        digest.update(b"tidebreak-turn-admission-v1\0");
        digest.update(self.chat_id.0.as_bytes());
        put_bytes(&mut digest, self.content.as_bytes());
        digest.update((self.attachments.len() as u64).to_be_bytes());
        for id in &self.attachments {
            digest.update(id.as_bytes());
        }
        digest.update((self.file_attachments.len() as u64).to_be_bytes());
        for id in &self.file_attachments {
            digest.update(id.0.as_bytes());
        }
        digest.update((self.invoked_skills.len() as u64).to_be_bytes());
        for skill in &self.invoked_skills {
            put_bytes(&mut digest, skill.as_bytes());
        }
        digest.update([u8::from(self.voice_input_used)]);
        digest.finalize().into()
    }
}

/// Exact ownership token for one unresolved durable admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnAdmissionLease {
    pub id: TurnId,
    pub lease_token: Uuid,
    pub lease_expires_at: DateTime<Utc>,
}

/// The id is the client-generated turn id promotion will accept under, so an
/// ambiguous promotion retry resolves to `Existing` rather than a duplicate
/// turn. Rows are FIFO by `position` within a chat and fully durable: a queue
/// survives restarts and is visible to every client on the chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct QueuedAgentTurn {
    /// The turn id this row becomes when promoted.
    pub id: TurnId,
    /// Owning chat.
    pub chat_id: SessionId,
    /// Byte-exact user message.
    pub content: String,
    /// Image-attachment ids, in display order.
    pub attachments: Vec<uuid::Uuid>,
    /// Chat-owned document ids.
    pub file_attachments: Vec<crate::id::DocumentId>,
    /// Skills the user explicitly invoked.
    pub invoked_skills: Vec<String>,
    /// Whether the message was dictated.
    pub voice_input_used: bool,
    /// FIFO order within the chat.
    pub position: i32,
    /// When the message was queued.
    pub created_at: DateTime<Utc>,
    /// When it was last edited or reordered.
    pub updated_at: DateTime<Utc>,
}

impl QueuedAgentTurn {
    /// Maximum queued messages one chat may hold.
    pub const MAX_PER_CHAT: usize = 32;

    /// Exact caller payload identity, excluding queue-assigned metadata.
    pub fn same_request(&self, other: &Self) -> bool {
        self.id == other.id
            && self.chat_id == other.chat_id
            && self.content == other.content
            && self.attachments == other.attachments
            && self.file_attachments == other.file_attachments
            && self.invoked_skills == other.invoked_skills
            && self.voice_input_used == other.voice_input_used
    }
}

/// One durably accepted steering instruction for an active turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnSteer {
    /// Caller-supplied idempotency identity.
    pub id: crate::id::TurnSteerId,
    /// Exact turn that receives this instruction.
    pub turn_id: TurnId,
    /// Owning chat, duplicated so the database can enforce turn/message scope.
    pub chat_id: SessionId,
    /// Byte-exact user instruction.
    pub content: String,
    /// Skills the user explicitly named for this instruction.
    ///
    /// Scoped to the steer alone: invocation is a per-message directive, so a
    /// steer neither inherits the turn's opening list nor spends its budget.
    pub invoked_skills: Vec<String>,
    /// Whether the instruction was dictated and transcribed from speech.
    pub voice_input_used: bool,
    /// Whether delivery should preempt the current model stream.
    pub interrupt: bool,
    /// Durable delivery state.
    pub status: TurnSteerStatus,
    /// Exact worker lease that applied the instruction.
    pub applied_lease_token: Option<Uuid>,
    /// User message committed atomically with application.
    ///
    /// When present, this carries the same UUID as `id`, so one caller identity
    /// names both the instruction and its eventual conversation message.
    pub message_id: Option<MessageId>,
    /// When the instruction was accepted.
    pub created_at: DateTime<Utc>,
    /// When it was applied or rejected.
    pub resolved_at: Option<DateTime<Utc>>,
}

impl TurnSteer {
    /// Maximum accepted instruction size in Unicode scalar values.
    pub const MAX_CONTENT_LEN: usize = 65_536;
}

/// Durable delivery state of a [`TurnSteer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TurnSteerStatus {
    /// Accepted and waiting for the exact live worker to apply it.
    Pending,
    /// User message and delivery receipt committed atomically.
    Applied,
    /// The turn stopped accepting instructions before this one could be applied.
    Rejected,
}

impl TurnSteerStatus {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applied => "applied",
            Self::Rejected => "rejected",
        }
    }
}

/// Retry intent attached to one exact turn-attempt failure.
///
/// Workers must retain this value across an ambiguous database commit. A new
/// backoff timestamp is a different failure request and is rejected if the
/// original request already committed under the same lease token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnFailureRetry {
    /// Do not claim this turn again automatically.
    Permanent,
    /// Make the turn eligible for another claim at the exact requested time.
    RetryAt(DateTime<Utc>),
}

/// Immutable proof that one exact claimed attempt recorded a failure.
///
/// The mutable turn can advance to a later attempt after a retryable failure,
/// so this receipt is the durable idempotency record for ambiguous retries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnFailureReceipt {
    /// Exact claim identity that submitted the failure.
    pub lease_token: Uuid,
    /// Turn resolved by the claim.
    pub turn_id: TurnId,
    /// Attempt number recorded in the immutable claim receipt.
    pub attempt_count: i32,
    /// Cumulative model calls consumed when the failure committed.
    pub model_steps: i32,
    /// Cumulative provider usage when the failure committed.
    pub usage: Usage,
    /// Requested retry time, retained even when exhaustion made the failure
    /// terminal. `None` represents an explicitly permanent failure.
    pub requested_retry_at: Option<DateTime<Utc>>,
    /// Stable machine-readable failure category.
    pub error_code: String,
    /// Bounded diagnostic detail for local operators.
    pub error_detail: Option<String>,
    /// Fresh operational time at which the first resolution committed.
    pub resolved_at: DateTime<Utc>,
    /// Historical result of this resolution (`retry_wait` or `failed`).
    pub result_status: TurnRunStatus,
}
