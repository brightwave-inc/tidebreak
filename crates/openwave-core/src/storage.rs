//! The storage seams every client sits on.
//!
//! Three traits, deliberately backend-agnostic so a profile can wire different
//! implementations without touching callers:
//!
//! - [`Store`] — durable metadata/state (chats, messages, settings). The
//!   default impl is SQLite; the same trait maps to Postgres for self-host.
//! - [`SecretProvider`] — credentials (model API keys, connection tokens). These
//!   live in the OS keychain (desktop) or a KMS/Vault (server) and are **never**
//!   written to the [`Store`]; the store only holds opaque secret references.
//! - [`BlobStore`] — bytes (documents, images, exports), served locally or from
//!   object storage.
//!
//! Only the entities that exist today are modeled here. Persistence for
//! connections, documents, and skills is added alongside the slices that
//! introduce those record types.

use async_trait::async_trait;
use futures::{
    stream::{self, BoxStream},
    StreamExt,
};
use serde::Serialize;
use serde_json::Value;
use std::ops::Range;

use crate::approval::{ApprovalDecision, ApprovalRequest, StandingGrant, ToolApproval};
use crate::connected_app::{ConnectedApp, ConnectedAppKind};
use crate::deliverable::{CreateOutput, NewOutputRevision, OutputRecord, OutputRevision};
use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::{
    AgentRunId, AppId, AppRevisionId, CallId, ChatId, DocumentId, MessageId, OutputId,
    OutputRevisionId, ProjectId, RootAttachmentChangeId, TurnId, TurnSteerId,
};
use crate::image::ImageRef;
use crate::local_app::{AppGrant, AppRecord, AppRevision, CreateApp, NewAppRevision};
use crate::model::{
    AgentRun, AgentRunInboxEntry, AgentRunResult, AgentRunTier, AgentRunWaitSetCandidate,
    BeginRootAttachmentChange, BlobRetirement, BlobRetirementStatus, Chat, ClientToolCallRequest,
    DocumentListCursor, DocumentRecord, DocumentScope, DocumentSourceBlob, DocumentSourceUpsert,
    DocumentSummaryRecord, DocumentUpsert, ExecFileRejection, ExecFileRejectionRecord,
    ExecFileSnapshot, ExecFileSnapshotRecord, Message, MessageAttachment,
    MessageDocumentAttachment, NetworkPolicy, PermissionMode, Project, ReasoningEffort,
    RootAttachmentChange, RootAttachmentChangeTerminal, ToolCallRecord, ToolCallResolution,
    TurnAgentRunWait, TurnAgentRunWaitSet, TurnCheckpointProgress, TurnClientWait,
    TurnFailureReceipt, TurnFailureRetry, TurnRun, TurnSteer,
};
use crate::provider::{RefusalOutcome, StopReason, Usage};
use crate::semantic_checkpoint::{ContextCheckpoint, SaveContextCheckpointOutcome};
use crate::{AnswerUserQuestionsRequest, PendingUserQuestions};

/// Largest pending attachment-reconciliation page accepted by [`Store`].
pub const MAX_PENDING_ROOT_ATTACHMENT_CHANGES: u64 = 256;

/// Chunks supplied to or read from a [`BlobStore`] without requiring either
/// side to buffer a whole source in memory.
///
/// Dropping a storage-backed bounded-read stream must cancel any in-flight
/// storage read.
pub type BlobStream = BoxStream<'static, Result<Vec<u8>>>;

/// A mutually consistent conversation transcript and event-journal cursor.
///
/// The cursor is captured under the same per-chat fence as `messages`, so a
/// renderer can hydrate durable text and then subscribe after this cursor
/// without dropping an event committed during the handoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatTranscriptSnapshot {
    pub messages: Vec<Message>,
    /// Durable image identities grouped into the renderer transcript under the
    /// same per-chat fence as `messages`.
    pub message_attachments: Vec<MessageAttachment>,
    /// Durable document identities grouped into the renderer transcript under
    /// the same per-chat fence as `messages`.
    pub message_document_attachments: Vec<MessageDocumentAttachment>,
    /// Ordered renderer-safe sources keyed to their assistant message.
    pub citations: Vec<ChatCitationSnapshot>,
    /// Every terminal turn, including outcomes that committed no assistant
    /// message. Keeping status and streamed content together prevents each new
    /// terminal outcome from requiring another transcript side table.
    pub terminal_turns: Vec<ChatTerminalTurnSnapshot>,
    /// A renderer-safe historical projection. It contains fixed tool identity,
    /// closed previews and lifecycle timestamps only; canonical tool records
    /// never leave storage.
    pub tool_activity: Vec<ChatToolActivitySnapshot>,
    pub last_event_seq: i64,
}

/// One renderer-safe citation paired with its transcript message.
///
/// Message identity is transcript assembly metadata, not part of the citation
/// shape exposed to renderer clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatCitationSnapshot {
    pub message_id: MessageId,
    pub citation: crate::AssistantCitationSnapshot,
}

/// One terminal turn and the visible stream it produced.
///
/// Completed turns point at their committed assistant message, which remains
/// authoritative for final prose. Failed and cancelled turns have no message,
/// so their partial prose and reasoning are rebuilt from the journal. Refusal
/// and failure details stay outcome metadata on the same turn rather than
/// becoming more transcript side tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatTerminalTurnSnapshot {
    pub turn_id: TurnId,
    pub message_id: Option<MessageId>,
    pub status: ChatTerminalTurnStatus,
    pub partial_content: String,
    pub reasoning: String,
    pub refusal: Option<RefusalOutcome>,
    /// Stable internal failure kind. Renderer projections must classify this
    /// before it crosses the server boundary.
    pub failure_kind: Option<String>,
    pub finished_at: chrono::DateTime<chrono::Utc>,
}

/// Terminal states visible in a conversation transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatTerminalTurnStatus {
    Completed,
    Failed,
    Cancelled,
}

/// Opaque indication that a conversation is parked on a renderer-owned prompt.
///
/// This is intentionally a summary rather than a prompt projection: it lets a
/// shell mark conversations needing attention without exposing question text,
/// folder-access arguments, executor state, or any other canonical tool data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingChatPrompt {
    pub chat_id: ChatId,
    pub question_call_ids: Vec<CallId>,
    pub plan_call_ids: Vec<CallId>,
    pub folder_access_call_ids: Vec<CallId>,
    pub output_writeback_call_ids: Vec<CallId>,
}

/// Result of a fail-closed conversation deletion request.
///
/// A conversation is only removable after every turn is terminal and no host
/// root remains attached. Root detachment is a broker-backed operation, so the
/// deletion path never tries to revoke native authority as an incidental side
/// effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteChatOutcome {
    /// The conversation and its terminal product history were removed.
    Deleted,
    /// No conversation owns this id.
    NotFound,
    /// A foreground turn or sandboxed run can still make progress.
    ActiveWork,
    /// A root is still projected into the conversation's broker context.
    RootsAttached,
    /// Broker history cannot conclusively prove every detached root is gone.
    ///
    /// Every terminal change records what the broker held when it settled, so in
    /// practice this means a change is still in flight — transient, and worth
    /// retrying — rather than a permanently unknowable observation.
    RootAttachmentStateUnresolved,
}

/// Result of a fail-closed project deletion request.
///
/// Project deletion never cascades conversations, documents, or host-root
/// projections. Callers must explicitly remove each owned resource through its
/// lifecycle-aware API before the empty project record can be removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteProjectOutcome {
    /// The empty project was removed.
    Deleted,
    /// No project owns this id.
    NotFound,
    /// Conversations, documents, or host-root defaults still belong to it.
    NotEmpty,
}

/// Counts of in-flight work the embedding host process must stay alive to
/// supervise.
///
/// A host that wants to restart itself (for example to install an update)
/// reads this to decide whether the process is quiescent. The counts are
/// deliberately strict: every non-terminal turn counts, including turns parked
/// on the client, and every live background-tier run counts regardless of
/// execution location. In-process background runs do not survive a host
/// restart — their lease expires and the claim-scan reaper fails them — so a
/// loose definition here would silently kill work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveWorkSnapshot {
    /// Turns in any non-terminal [`crate::TurnRunStatus`], across every chat.
    pub active_turns: u64,
    /// Background-tier agent runs in any non-terminal
    /// [`crate::AgentRunStatus`], across every chat. Foreground coordinator
    /// runs are excluded: their live work is already counted as turns.
    pub live_background_runs: u64,
}

impl ActiveWorkSnapshot {
    /// True when the host supervises no in-flight work and may restart
    /// without interrupting or stranding anything.
    #[must_use]
    pub const fn is_quiescent(self) -> bool {
        self.active_turns == 0 && self.live_background_runs == 0
    }
}

/// Fixed lifecycle vocabulary exposed for a historical tool card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ChatToolActivityStatus {
    Completed,
    Failed,
    /// The reader rejected the call's approval, so it never ran.
    ///
    /// Durably this is a `Failed` call whose error code says the user
    /// declined; the projection splits it out because "you said no" and
    /// "the tool broke" are different facts to show a reader, and folding
    /// them made a decline indistinguishable from a crash in history.
    Denied,
    Cancelled,
}

/// A completed tool invocation with no arbitrary result text, provider
/// metadata, executor identity, lease, or diagnostic detail. The only action or
/// result it can carry is one a tool explicitly projects through a closed type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct ChatToolActivitySnapshot {
    /// Canonical call id, the same [`crate::id::CallId`] the live event stream
    /// carried for this call.
    ///
    /// History withholds arbitrary call detail, but not this identity: the MCP
    /// App payload route already keys renderer-readable data on exactly this id
    /// for the same authenticated client, and a rehydrated app view must present
    /// it to resolve its payload. Without it, history cards invented a local id
    /// and every replayed app view fetched a payload the server could only
    /// reject.
    pub call_id: crate::id::CallId,
    /// Allowlisted renderer tool name, never a provider-supplied one.
    ///
    /// A name rather than display copy: the renderer already derives a live
    /// call's wording from its name, and sending prose here made a copy change
    /// silently break history hydration.
    ///
    /// Typed as the vocabulary rather than a string so the generated TypeScript
    /// stays a union. As `&'static str` it generated as `string`, which compiles
    /// on both sides while silently dropping the allowlist the renderer's copy
    /// and icon tables are keyed on.
    pub tool: crate::RendererToolName,
    /// Closed projection of what the call did, when its tool has one. Rebuilt
    /// from the arguments it ran with, so history describes the same action
    /// the live stream did.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub action: Option<crate::preview::ToolActionPreview>,
    /// Closed projection of an actionable result. Arbitrary result text is
    /// never included.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub result: Option<crate::preview::ToolResultPreview>,
    /// Set when this call retained a projection that no longer deserializes.
    ///
    /// The projection is a closed union that is allowed to move, and rows
    /// written before a change may no longer parse against it. Distinguishing
    /// that from "this call projected nothing" is the difference between a card
    /// that says its result can no longer be shown and one that silently
    /// vanishes — which would read as the call never having produced anything.
    ///
    /// A property of reading storage, not of the result: the live stream builds
    /// its projection in memory and can never set this.
    pub result_unreadable: bool,
    // Present only for the fixed `spawn_sandbox_agent` renderer tool. It lets
    // the transcript attach the durable child status without exposing a
    // canonical tool record, delegated task, or executor identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub background_agent_run_id: Option<crate::id::AgentRunId>,
    pub status: ChatToolActivityStatus,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Result of atomically beginning one broker-backed root attachment change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeginRootAttachmentChangeOutcome {
    /// Intent and immutable derived metadata were committed by this call.
    Begun(RootAttachmentChange),
    /// This exact identity and caller request were already committed.
    Existing(RootAttachmentChange),
    /// The change identity was already used for different immutable request data.
    IdentityConflict,
    /// The conversation does not exist.
    ChatNotFound,
    /// The caller's attachment revision no longer matches authoritative state.
    RevisionConflict { current_attachment_revision: i64 },
    /// Adding the root would exceed the bounded conversation projection.
    CapacityExceeded,
    /// The required intent or rollback transition cannot advance the revision.
    RevisionExhausted,
    /// Another awaiting operation owns this chat's single mutation slot.
    /// Pending operation details are available only through executor-scoped
    /// recovery scans, never through a competing begin request.
    ChatBusy,
}

/// Result of atomically finishing one exact root attachment change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishRootAttachmentChangeOutcome {
    /// Terminal broker observation and final projection committed by this call.
    Finished(RootAttachmentChange),
    /// An exact ambiguous retry recovered the same terminal change.
    Existing(RootAttachmentChange),
    /// No change exists under this idempotency identity.
    NotFound,
    /// The stable executor does not own this change.
    ExecutorMismatch,
    /// The change was already terminal under a different terminal payload.
    AlreadyTerminal(RootAttachmentChange),
    /// A terminal broker observation contradicts success or rollback state.
    BrokerStateMismatch,
}

/// Result of atomically accepting one exact client turn request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptTurnOutcome {
    /// The input message and queued turn were committed by this call.
    Accepted(TurnRun),
    /// This exact turn identity and request were already committed.
    Existing(TurnRun),
    /// The turn identity was already committed for different request data.
    IdentityConflict,
    /// Another nonterminal turn already owns the chat's single live slot.
    ChatBusy(TurnRun),
}

/// Result of atomically accepting one durable foreground or sandboxed agent run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptAgentRunOutcome {
    /// This call committed a new run.
    Accepted(AgentRun),
    /// The exact id and immutable request were already committed.
    Existing(AgentRun),
    /// The id was already committed for different immutable request data.
    IdentityConflict,
    /// A chat may have only one foreground coordinator.
    ForegroundExists(AgentRun),
    /// The requested sandbox parent is missing, cross-chat, or not foreground.
    ParentUnavailable,
}

/// Result of transactionally admitting one bounded sandbox child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmitSandboxAgentRunOutcome {
    /// The child and immutable origin ownership were committed together.
    Accepted {
        child: AgentRun,
        admission: crate::model::SandboxAgentAdmission,
    },
    /// An exact ambiguous retry recovered the committed child and ownership.
    Existing {
        child: AgentRun,
        admission: crate::model::SandboxAgentAdmission,
    },
    /// The child or spawn-call identity is bound to different immutable input.
    IdentityConflict,
    /// The origin turn does not have an eligible foreground coordinator.
    ParentUnavailable,
    /// The delegated file root is not attached to the foreground chat.
    DelegatedResourceUnavailable,
    /// The origin turn is not owned by the supplied live worker lease.
    LeaseLost,
    /// This origin turn already owns the configured number of nonterminal children.
    AtCapacity,
    /// A durable steer won the admission race and must be applied first.
    SteerPending(TurnRun),
    /// The request came from provider output generated before an applied steer.
    OutputSuperseded(TurnRun),
}

/// Result of atomically checkpointing one non-blocking sandbox spawn.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckpointSandboxSpawnOutcome {
    /// Child admission, tool history, journal event, progress, and turn yield
    /// committed together.
    Checkpointed {
        child: AgentRun,
        turn: Box<TurnRun>,
        call: ToolCallRecord,
        checkpoint: crate::model::SandboxSpawnCheckpoint,
        event: SequencedEvent,
    },
    /// An exact retry recovered the immutable committed transition.
    Existing {
        child: AgentRun,
        call: ToolCallRecord,
        checkpoint: crate::model::SandboxSpawnCheckpoint,
        event: SequencedEvent,
    },
    /// This call or one of its bound identities was reused with different
    /// provider output, accounting, provenance, or journal order.
    IdentityConflict,
    /// The origin turn does not have an eligible foreground coordinator.
    ParentUnavailable,
    /// The delegated file root is not attached to the foreground chat.
    DelegatedResourceUnavailable,
    /// The exact foreground claim is missing, stale, or no longer live.
    LeaseLost,
    /// Four nonterminal children already belong to this origin turn.
    AtCapacity,
    /// A durable steer won the checkpoint race and must be applied first.
    SteerPending(TurnRun),
    /// The model output belongs to an older steering generation.
    OutputSuperseded(TurnRun),
}

/// Result of atomically parking a sandbox run on immutable tool work.
#[derive(Debug, Clone, PartialEq)]
pub enum ParkSandboxToolCallOutcome {
    Parked {
        run: AgentRun,
        call: crate::model::SandboxToolCall,
    },
    Existing {
        run: AgentRun,
        call: crate::model::SandboxToolCall,
    },
    IdentityConflict,
    DelegatedResourceUnavailable,
    LeaseLost,
}

/// Result of claiming durable sandbox tool work.
#[derive(Debug, Clone, PartialEq)]
pub enum ClaimSandboxToolCallOutcome {
    Claimed(crate::model::SandboxToolCall),
    Existing(crate::model::SandboxToolCall),
    Unavailable,
}

/// Result of claiming a native delegated-file read after revalidating its
/// immutable child admission and the chat's current root projection.
#[derive(Debug, Clone, PartialEq)]
pub enum ClaimDelegatedFileReadOutcome {
    Claimed(crate::model::DelegatedFileReadClaim),
    Existing(crate::model::DelegatedFileReadClaim),
    Unavailable,
}

/// Result of parking a claimed sandbox tool call for its single bounded retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrySandboxToolCallOutcome {
    /// The call is parked in `retry_wait` and becomes claimable at its
    /// `retry_at`. Its waiting sandbox run is untouched.
    Scheduled,
    /// The lease no longer authorizes the call — cancellation, expiry, a
    /// terminal receipt, or a competing executor already won.
    LeaseLost,
}

/// Result of resolving sandbox tool work under its exact executor lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveSandboxToolCallOutcome {
    Resolved,
    Existing,
    NotFound,
    AlreadyTerminal,
    LeaseLost,
}

/// Result of atomically accepting one sandbox child and checkpointing its
/// owning foreground turn on that child's inbox delivery.
#[derive(Debug, Clone, PartialEq)]
pub enum AcceptSandboxAgentRunAndParkTurnOutcome {
    /// The child, immutable wait receipt, and foreground lease release
    /// committed in one transaction.
    Parked {
        child: AgentRun,
        turn: TurnRun,
        wait: TurnAgentRunWait,
    },
    /// An exact retry recovered the same already-committed transition.
    Existing {
        child: AgentRun,
        turn: TurnRun,
        wait: TurnAgentRunWait,
    },
    /// The child id or spawn-call identity is bound to a different request,
    /// or was accepted outside this atomic transition.
    IdentityConflict,
    /// The turn's foreground coordinator is no longer an eligible sandbox
    /// parent.
    ParentUnavailable,
    /// The origin turn already owns the configured number of nonterminal children.
    AtCapacity,
    /// A durable steer won the checkpoint race and must be applied first.
    SteerPending(TurnRun),
    /// The request came from provider output generated before an applied steer.
    OutputSuperseded(TurnRun),
}

/// Result of requesting durable cancellation for one sandbox run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestAgentRunCancellationOutcome {
    /// Unclaimed work was cancelled immediately.
    Cancelled(AgentRun),
    /// A live worker retained its lease and must acknowledge cancellation.
    Requested(AgentRun),
    /// The run was already cancelling or cancelled.
    Existing(AgentRun),
    /// A successful result or permanent failure already won.
    AlreadyTerminal(AgentRun),
}

/// Result of a sandbox worker acknowledging cancellation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishAgentRunCancellationOutcome {
    /// This exact live lease committed terminal cancellation.
    Cancelled(AgentRun),
    /// This exact lease already committed terminal cancellation.
    Existing(AgentRun),
}

/// Result of a sandbox worker submitting final text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitAgentRunResultOutcome {
    /// This exact live lease committed the immutable terminal result.
    Completed(AgentRunResult),
    /// The same lease and payload were already committed.
    Existing(AgentRunResult),
}

/// Resolution of one exact sandbox execution failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailAgentRunOutcome {
    /// The leased attempt was released for a bounded retry.
    RetryScheduled(AgentRun),
    /// The leased attempt exhausted its budget and delivered a terminal failure
    /// receipt to the parent inbox.
    Failed(AgentRunResult),
    /// An exact ambiguous retry recovered the already-recorded retry or final
    /// failure transition.
    ExistingRetry(AgentRun),
    /// An exact ambiguous retry recovered the final failure receipt.
    ExistingFailed(AgentRunResult),
}

/// Result of acquiring durable ownership of one exact parent inbox delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimAgentRunInboxOutcome {
    /// This call acquired the continuation lease.
    Claimed(AgentRunInboxEntry),
    /// The exact live lease was already acquired by this caller.
    Existing(AgentRunInboxEntry),
}

/// Result of consuming one exact parent inbox delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsumeAgentRunInboxOutcome {
    /// This exact live continuation lease committed consumption.
    Consumed(AgentRunInboxEntry),
    /// This exact lease already committed consumption.
    Existing(AgentRunInboxEntry),
}

/// Result of atomically consuming a child inbox delivery and waking its parked
/// foreground turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsumeAgentRunInboxAndResumeTurnOutcome {
    /// The exact continuation lease consumed the delivery and queued the turn
    /// for a fresh foreground worker claim.
    Resumed {
        /// Immutable inbox receipt now marked consumed.
        inbox: AgentRunInboxEntry,
        /// Foreground turn now durably ready to resume.
        turn: TurnRun,
    },
    /// An ambiguous retry recovered the exact prior consumption and wake.
    Existing {
        /// Immutable inbox receipt consumed by this lease.
        inbox: AgentRunInboxEntry,
        /// Foreground turn already durably ready to resume.
        turn: TurnRun,
    },
}

/// Result of atomically accepting one exact steering instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptTurnSteerOutcome {
    /// This call committed the pending instruction.
    Accepted(TurnSteer),
    /// This exact instruction identity and payload were already committed.
    Existing(TurnSteer),
    /// The identity was already used for different request data or a message.
    IdentityConflict,
    /// The target is missing, cross-chat, expired, cancelling, or terminal.
    TurnUnavailable,
}

/// Result of atomically applying one exact pending steering instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyTurnSteerOutcome {
    /// This call committed the user message and application receipt.
    Applied(TurnSteer),
    /// This exact worker lease already committed the same application.
    Existing(TurnSteer),
}

/// One applied steering result and the replay event committed with it.
#[derive(Debug, Clone, PartialEq)]
pub struct JournaledTurnSteerOutcome {
    /// The steer application result.
    pub outcome: ApplyTurnSteerOutcome,
    /// The exact nonterminal journal row committed by the application.
    pub event: SequencedEvent,
}

/// Result of atomically completing one exact claimed turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompleteTurnRunOutcome {
    /// This call committed the terminal message and state transition.
    Completed(TurnRun),
    /// This exact completion was already committed by an earlier call.
    Existing(TurnRun),
    /// Completion was fenced until every child admitted by this turn has a
    /// consumed or explicitly retired terminal delivery.
    ChildrenOutstanding {
        /// The still-live foreground turn.
        turn: TurnRun,
        /// Stable admission-order identities that still need settlement.
        child_run_ids: Vec<crate::id::AgentRunId>,
    },
    /// Completion was fenced because an accepted steer still needs application.
    SteerPending(TurnRun),
    /// The output was generated from an older steer revision and must be regenerated.
    OutputSuperseded(TurnRun),
}

/// Result of recording one exact claimed turn failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordTurnFailureOutcome {
    /// This call committed the receipt and state transition.
    Recorded(TurnFailureReceipt),
    /// This exact failure request was already committed by an earlier call.
    Existing(TurnFailureReceipt),
}

/// Result of requesting durable cancellation for one exact turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestTurnCancellationOutcome {
    /// Queued, retry-wait, or unclaimed client work was cancelled immediately.
    Cancelled(TurnRun),
    /// Running work or claimed client work entered a durable cancelling phase.
    Requested(TurnRun),
    /// This turn was already cancelling or cancelled.
    Existing(TurnRun),
    /// Successful completion or terminal failure won before cancellation.
    AlreadyTerminal(TurnRun),
}

/// Result of a worker acknowledging that cancellation has quiesced execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishTurnCancellationOutcome {
    /// This call committed the terminal cancellation transition.
    Cancelled(TurnRun),
    /// This exact claimed attempt already reached terminal cancellation.
    Existing(TurnRun),
}

/// Result of durably accepting canonical tool-call arguments.
#[derive(Debug, Clone, PartialEq)]
pub enum AcceptToolCallOutcome {
    /// The identity and immutable request were inserted.
    Accepted(ToolCallRecord),
    /// An exact retry found the same immutable request.
    Existing(ToolCallRecord),
    /// The call identity already names different immutable request bytes.
    IdentityConflict,
}

/// Result of accepting a tool call under an exact live turn lease.
#[derive(Debug, Clone, PartialEq)]
pub enum AcceptClaimedToolCallOutcome {
    /// The request and its originating turn lease were committed together.
    Accepted(ToolCallRecord),
    /// An exact retry recovered the request committed by this same lease.
    Existing(ToolCallRecord),
    /// The call identity already names different request bytes or another lease.
    IdentityConflict,
    /// The exact turn lease was no longer current at commit time.
    LeaseLost,
}

/// Result of appending an intermediate assistant message under a turn lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendClaimedMessageOutcome {
    /// The message and citations were committed under the lease.
    Appended,
    /// An exact retry recovered the same message, citations, and lease owner.
    Existing,
    /// The message identity names different bytes or another lease.
    IdentityConflict,
    /// The exact turn lease was no longer current at commit time.
    LeaseLost,
}

/// Result of registering one exact durable approval request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestToolApprovalOutcome {
    /// This call entered the pending approval state.
    Requested(ToolApproval),
    /// An exact retry recovered the same pending or decided request.
    Existing(ToolApproval),
    /// A previously persisted standing grant authorized this exact call. No
    /// approval card was created, but the automatic authorization is recorded
    /// on the call for crash recovery and audit.
    Granted(ToolApproval),
    /// The call exists but its canonical identity differs from the request.
    IdentityConflict,
    /// The call is missing, terminal, or not a server-executed call.
    Unavailable,
}

/// Approval registration plus its exact atomically committed required event.
#[derive(Debug, Clone, PartialEq)]
pub struct JournaledToolApprovalOutcome {
    pub outcome: RequestToolApprovalOutcome,
    /// Present only for a new commit or an exact retry of the same claim slot.
    pub required_event: Option<SequencedEvent>,
}

/// Result of deciding one exact durable approval request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecideToolApprovalOutcome {
    /// This request transitioned from pending to the supplied decision.
    Decided(ToolApproval),
    /// An exact retry recovered the same decision bytes.
    Existing(ToolApproval),
    /// The request was already decided differently.
    DecisionConflict,
    /// No pending or decided approval exists under this chat and call identity.
    Unavailable,
}

/// A client claim and its secret per-claim fencing receipt.
///
/// The token is returned only by claim, never by general pending/history reads
/// or by serializing [`ToolCallRecord`].
#[derive(Clone, PartialEq)]
pub struct ClientToolCallClaim {
    /// Canonical committed work and visible lease metadata.
    pub call: ToolCallRecord,
    /// Secret capability required to heartbeat or resolve this claim.
    pub lease_token: uuid::Uuid,
}

impl std::fmt::Debug for ClientToolCallClaim {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientToolCallClaim")
            .field("call", &self.call)
            .field("lease_token", &"[redacted]")
            .finish()
    }
}

/// Result of claiming one pending client-executed call.
#[derive(Debug, Clone, PartialEq)]
pub enum ClaimClientToolCallOutcome {
    /// This executor acquired the first lease.
    Claimed(ClientToolCallClaim),
    /// An exact retry by the same executor recovered its live lease.
    Existing(ClientToolCallClaim),
    /// The call is missing, terminal, server-executed, owned by another client,
    /// or has an expired ambiguous client lease.
    Unavailable,
}

/// Result of extending one exact client-execution lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatClientToolCallOutcome {
    /// The lease expiry advanced.
    Extended,
    /// An exact retry found that expiry already installed.
    Existing,
    /// The call, pending state, executor, or live lease no longer matches.
    LeaseLost,
}

/// Result of deciding one exact durable plan proposal.
#[derive(Debug, Clone, PartialEq)]
pub enum DecidePlanOutcome {
    /// The decision completed the tool call and made the turn resumable. An
    /// accepted decision also moved the chat out of plan mode.
    Decided(TurnRun),
    /// An ambiguous retry recovered the same committed decision.
    Existing(TurnRun),
    /// The request already committed a different decision.
    DecisionConflict,
    /// The decision shape is invalid.
    InvalidDecision,
    /// The request is missing, cancelled, terminal, or scoped to another chat.
    Unavailable,
}

/// Result of answering one exact durable foreground question request.
#[derive(Debug, Clone, PartialEq)]
pub enum AnswerUserQuestionsOutcome {
    /// The exact answers completed the tool call and made the turn resumable.
    Answered {
        /// The resumable turn.
        turn: TurnRun,
        /// The call's journaled completion, committed with the answer so a
        /// live renderer settles the card now rather than at the turn's end.
        completion_event: Box<SequencedEvent>,
    },
    /// An ambiguous retry recovered the same committed answers.
    Existing(TurnRun),
    /// The request already committed different answers.
    AnswerConflict,
    /// The answer shape, coverage, or selected option is invalid.
    InvalidAnswer,
    /// The request is missing, cancelled, terminal, or scoped to another chat.
    Unavailable,
}

/// Result of resolving one tool call under its required authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveToolCallOutcome {
    /// The pending call became terminal.
    Resolved,
    /// An exact ambiguous retry recovered the same terminal payload.
    Existing,
    /// The call identity was not found.
    NotFound,
    /// The call was already terminal under a different payload.
    AlreadyTerminal,
    /// The requested execution surface or exact live client lease did not own it.
    LeaseLost,
}

/// Result of checkpointing a live worker on one exact client tool call.
#[derive(Debug, Clone, PartialEq)]
pub enum ParkTurnForClientCallOutcome {
    /// The call, immutable wait receipt, and lease release committed together.
    Parked {
        turn: TurnRun,
        call: ToolCallRecord,
        wait: TurnClientWait,
        /// Renderer refresh hint committed in the same transaction, when this
        /// client continuation has a renderer-owned presentation.
        renderer_event: Option<SequencedEvent>,
    },
    /// An exact retry recovered the previously committed checkpoint.
    Existing {
        turn: TurnRun,
        call: ToolCallRecord,
        wait: TurnClientWait,
        /// Exact renderer event recovered after an ambiguous commit response.
        renderer_event: Option<SequencedEvent>,
    },
    /// The call identity already names a different immutable request.
    IdentityConflict,
    /// A durable steer won the checkpoint race and must be applied first.
    SteerPending(TurnRun),
    /// The request came from provider output generated before an applied steer.
    OutputSuperseded(TurnRun),
}

/// Result of checkpointing a foreground turn while it awaits one sandbox child.
#[derive(Debug, Clone, PartialEq)]
pub enum ParkTurnForAgentRunInboxOutcome {
    /// The immutable wait receipt and foreground lease release committed together.
    Parked {
        turn: TurnRun,
        wait: TurnAgentRunWait,
    },
    /// An exact retry recovered the previously committed checkpoint.
    Existing {
        turn: TurnRun,
        wait: TurnAgentRunWait,
    },
    /// The child delivery identity is already bound to another checkpoint.
    IdentityConflict,
    /// A durable steer won the checkpoint race and must be applied first.
    SteerPending(TurnRun),
    /// The request came from provider output generated before an applied steer.
    OutputSuperseded(TurnRun),
}

/// Result of checkpointing a foreground turn on an ordered sandbox-child set.
#[derive(Debug, Clone, PartialEq)]
pub enum ParkTurnForAgentRunWaitSetOutcome {
    /// The immutable set receipt and foreground lease release committed together.
    Parked {
        turn: TurnRun,
        call: ToolCallRecord,
        wait: TurnAgentRunWaitSet,
    },
    /// An exact ambiguous retry recovered the committed checkpoint.
    Existing {
        turn: TurnRun,
        call: ToolCallRecord,
        wait: TurnAgentRunWaitSet,
    },
    /// The wait identity, turn, or ordered child set is bound differently.
    IdentityConflict,
    /// A durable steer won the checkpoint race and must be applied first.
    SteerPending(TurnRun),
    /// The request came from provider output generated before an applied steer.
    OutputSuperseded(TurnRun),
}

/// Result of atomically consuming a satisfied ordered child set and waking its turn.
#[derive(Debug, Clone, PartialEq)]
pub enum ResumeTurnForAgentRunWaitSetOutcome {
    /// Every requested result was consumed and the turn became claimable.
    Resumed {
        turn: TurnRun,
        call: ToolCallRecord,
        wait: TurnAgentRunWaitSet,
        results: Vec<AgentRunInboxEntry>,
        event: SequencedEvent,
    },
    /// The exact continuation retry recovered its prior consumption and wake.
    Existing {
        turn: TurnRun,
        call: ToolCallRecord,
        wait: TurnAgentRunWaitSet,
        results: Vec<AgentRunInboxEntry>,
        event: SequencedEvent,
    },
    /// At least one requested child has not delivered a result yet.
    NotReady(TurnAgentRunWaitSet),
    /// A child is terminal but its required immutable inbox delivery is absent.
    /// This fails closed so reconciliation can repair the delivery without the
    /// parent silently losing terminal context.
    TerminalDeliveryMissing {
        wait: TurnAgentRunWaitSet,
        child_run_id: AgentRunId,
        child_status: crate::model::AgentRunStatus,
    },
}

/// A client-call resolution together with the turn transition it triggered.
#[derive(Debug, Clone, PartialEq)]
pub struct JournaledClientToolCallOutcome {
    /// Terminal tool-call result.
    pub outcome: ResolveToolCallOutcome,
    /// Wait-backed turn after it resumed or terminalized, when applicable.
    pub turn: Option<TurnRun>,
    /// Exact terminal event committed by client-owned cancellation.
    pub terminal_event: Option<SequencedEvent>,
}

/// A durable turn transition together with any terminal event committed by it.
#[derive(Debug, Clone, PartialEq)]
pub struct JournaledTurnOutcome<T> {
    /// The state-machine result.
    pub outcome: T,
    /// The exact terminal journal row when this operation publishes one.
    pub terminal_event: Option<SequencedEvent>,
}

/// A terminal event committed while a claim scan cleans expired work.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimScanTerminalEvent {
    /// Chat whose per-chat journal assigned the sequence.
    pub chat_id: ChatId,
    /// Turn terminalized by the scan.
    pub turn_id: TurnId,
    /// Exact committed journal event.
    pub event: SequencedEvent,
}

/// Result of one durable claim action.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimTurnRunOutcome {
    /// The due turn claimed for execution, if any.
    pub turn: Option<TurnRun>,
    /// One expired turn terminalized instead of claiming work, if any.
    ///
    /// Exactly one of `turn` and `terminal_event` is present, or both are absent
    /// when no work is due. Returning after one committed action lets the worker
    /// publish the event before scanning again without losing an earlier commit
    /// behind a later scan error.
    pub terminal_event: Option<ClaimScanTerminalEvent>,
}

/// Whether a lease token still owns the exact live worker segment of a turn.
///
/// This is the read side of the lease compare-and-swap that guards durable turn
/// writes. A worker fences an intermediate tool or message side effect on the
/// current lease so that a segment whose lease was stolen after expiry can
/// neither commit a fresh effect nor replay one a later attempt already owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnLeaseFence {
    /// The turn is running (or cancelling) under exactly this unexpired lease.
    Current,
    /// The token no longer owns a live segment: it expired, was superseded by a
    /// later claim, or the turn already reached a terminal state.
    Stale,
}

fn document_storage_unavailable<T>() -> Result<T> {
    Err(AgentError::Store(
        "document storage is not implemented by this Store".into(),
    ))
}

fn output_storage_unavailable<T>() -> Result<T> {
    Err(AgentError::Store(
        "output storage is not implemented by this Store".into(),
    ))
}

fn app_storage_unavailable<T>() -> Result<T> {
    Err(AgentError::Store(
        "local-app storage is not implemented by this Store".into(),
    ))
}

fn connected_app_storage_unavailable<T>() -> Result<T> {
    Err(AgentError::Store(
        "connected-app storage is not implemented by this Store".into(),
    ))
}

fn context_checkpoint_storage_unavailable<T>() -> Result<T> {
    Err(AgentError::Store(
        "durable context-checkpoint storage is not implemented by this Store".into(),
    ))
}

fn turn_storage_unavailable<T>() -> Result<T> {
    Err(AgentError::Store(
        "durable turn storage is not implemented by this Store".into(),
    ))
}

fn agent_run_storage_unavailable<T>() -> Result<T> {
    Err(AgentError::Store(
        "durable agent-run storage is not implemented by this Store".into(),
    ))
}

fn root_attachment_storage_unavailable<T>() -> Result<T> {
    Err(AgentError::Store(
        "durable root attachment storage is not implemented by this Store".into(),
    ))
}

fn operation_log_storage_unavailable<T>() -> Result<T> {
    Err(AgentError::Store(
        "durable operation-log storage is not implemented by this Store".into(),
    ))
}

/// Where one entry of the durable reverse-RPC operation log stands.
///
/// This is the storage-tier projection of the protocol's operation state
/// machine (`openwave-sandbox-protocol::oplog`): a `Claimed` entry has been
/// dispatched but has no terminal outcome yet; `Recorded`/`Failed` are terminal.
/// The recorded body itself is an opaque blob to the store — the protocol tier
/// owns its typed shape — so this tier stays free of reverse-RPC wire types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationLogState {
    /// Dispatched, no terminal outcome recorded.
    Claimed,
    /// Terminal success; a response body is retained (unless evicted per #859).
    Recorded,
    /// Terminal failure; an error body is retained (unless evicted per #859).
    Failed,
}

/// The outcome of atomically claiming an operation identity against the durable
/// log. This is the storage half of the reverse-RPC commit predicate; the
/// protocol tier maps it onto `ClaimOutcome`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationClaimOutcome {
    /// The identity was unseen and is now `Claimed`, owned by the caller's
    /// epoch. The caller must execute the effect exactly once.
    Fresh,
    /// The identity is already terminally recorded; the opaque response body to
    /// replay without re-executing.
    Recorded(Vec<u8>),
    /// The identity already failed terminally; the opaque error body to replay.
    Failed(Vec<u8>),
    /// The identity is terminal (recorded or failed) but its body has been
    /// evicted to a commit marker (#859) and is gone. It ran exactly once and
    /// must not be re-executed; there is no body to replay. This is distinct
    /// from a backend failure — the row is intact, only its body is absent — so
    /// the caller answers "already done, do not re-execute", never the
    /// after-crash ambiguity.
    TerminalEvicted,
    /// The identity is `Claimed` by the caller's *own* epoch — a concurrent
    /// duplicate this process lifetime, which attaches to the live execution
    /// rather than re-executing.
    OwnedClaim,
    /// The identity is `Claimed` by a *different* epoch that never recorded — the
    /// after-crash ambiguity for an external-effect operation. The caller must
    /// fail conservatively rather than re-execute a possibly-spent call.
    ForeignClaim,
    /// The identity was reused for a structurally different request fingerprint.
    Conflict,
}

/// The outcome of a terminal write (`record`/`fail`) against the durable log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationLogWrite {
    /// The `Claimed` entry transitioned to the requested terminal state.
    Committed,
    /// The entry was already in the requested terminal state; an idempotent
    /// no-op, so a re-delivered terminal write is acknowledged, not rejected.
    AlreadyTerminal,
    /// No `Claimed` entry to settle — unknown, or already in the *other*
    /// terminal state.
    NotClaimed,
}

/// A read-back of one durable operation-log entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationLogEntry {
    /// The entry's state.
    pub state: OperationLogState,
    /// The recorded terminal body (response for `Recorded`, error for
    /// `Failed`), present only while the entry is terminal *and* its body is
    /// still retained. `None` while `Claimed`, or once #859 evicts the body and
    /// leaves a commit marker.
    pub body: Option<Vec<u8>>,
    /// Whether the operation carried an external effect when claimed. Retention
    /// (#859) and audit read this without re-deriving it from the request.
    pub external_effect: bool,
    /// Whether the terminal body is still retained. #859 flips this to `false`
    /// when it replaces a full body with a commit marker.
    pub retained: bool,
}

/// Where one container run's durable provisioning record stands.
///
/// The record is written *before* the backend is asked to create a sandbox and
/// carries the host-minted correlation tag, so recovery is driven by the intent
/// rather than by what the provider reports: a crash on either side of the
/// create call converges on the same terminal state through the window lapse
/// and the tag sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxProvisionState {
    /// The intent is committed and the provisioning window is running; no
    /// handle has been recorded yet.
    Intended,
    /// The backend's sandbox handle is committed onto the record; a restarted
    /// host reconciles this container instead of creating a second one.
    Committed,
    /// The sandbox owes a teardown: the run ended, the window lapsed, or the
    /// handle was learned too late. The sweep drives this to `Done`.
    Teardown,
    /// The sandbox is confirmed gone.
    Done,
}

/// One container run's durable provisioning record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxProvision {
    /// The agent run this sandbox serves. Container runs have exactly one
    /// execution attempt, so the run id keys the record.
    pub run_id: uuid::Uuid,
    /// The host-minted correlation tag stamped into the sandbox's metadata at
    /// creation; the orphan sweep reclaims by it.
    pub tag: String,
    /// Where the record stands.
    pub state: SandboxProvisionState,
    /// The backend's own reference for the sandbox, present once committed.
    pub handle: Option<String>,
    /// A well-formed result that arrived after the run was already terminal:
    /// retained as non-authoritative evidence, never committed.
    pub late_result_evidence: Option<String>,
    /// When the provisioning window lapses: an `Intended` record older than
    /// this failed its admission whether or not a create ever reached the
    /// provider.
    pub window_expires_at: chrono::DateTime<chrono::Utc>,
}

/// The outcome of committing a provisioning intent for a container run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeginSandboxProvisionOutcome {
    /// A fresh intent was committed under the caller's tag; the caller may ask
    /// the backend to create a sandbox.
    Started,
    /// A record for this run already exists — a prior driver got here first.
    /// The caller reconciles from its state instead of provisioning again.
    Existing(SandboxProvision),
}

/// Durable metadata and conversation state.
///
/// Implementations must be safe to share across threads (`Send + Sync`) and are
/// held behind `Arc<dyn Store>`, so this trait stays object-safe.
#[async_trait]
pub trait Store: Send + Sync {
    /// Persist a new project.
    async fn create_project(&self, project: &Project) -> Result<()>;

    /// Fetch a project by id, or `None` if it doesn't exist.
    async fn get_project(&self, id: ProjectId) -> Result<Option<Project>>;

    /// List projects, most-recently-created first.
    async fn list_projects(&self) -> Result<Vec<Project>>;

    /// Replace one project's human-facing title.
    ///
    /// Returns `false` when the project does not exist. Product adapters own
    /// title normalization and bounds before calling this storage primitive.
    async fn update_project_title(&self, _id: ProjectId, _title: Option<String>) -> Result<bool> {
        Err(AgentError::Store(
            "project metadata storage is not implemented by this Store".into(),
        ))
    }

    /// Remove one empty project without cascading owned product state.
    async fn delete_project(&self, _id: ProjectId) -> Result<DeleteProjectOutcome> {
        Err(AgentError::Store(
            "project deletion is not implemented by this Store".into(),
        ))
    }

    /// Persist a new authoritative document record.
    ///
    /// At most one of `chat_id` and `project_id` may be present, and it must
    /// identify an existing owner. A live document's ownership is immutable:
    /// callers must delete it before recreating the same id in another corpus.
    async fn create_document(&self, _document: &DocumentRecord) -> Result<()> {
        document_storage_unavailable()
    }

    /// Fetch an authoritative document by id, or `None` if it does not exist.
    async fn get_document(&self, _id: DocumentId) -> Result<Option<DocumentRecord>> {
        document_storage_unavailable()
    }

    /// List documents in `scope`, most-recently-created first.
    async fn list_documents(&self, _scope: DocumentScope) -> Result<Vec<DocumentRecord>> {
        document_storage_unavailable()
    }

    /// List document metadata in deterministic newest-first order.
    ///
    /// At most `limit` records are returned. When `after` is present, results
    /// begin strictly after its `(created_at, id)` tuple in descending display
    /// order. Implementations must not load canonical text.
    async fn list_document_summaries(
        &self,
        _scope: DocumentScope,
        _after: Option<DocumentListCursor>,
        _limit: u64,
    ) -> Result<Vec<DocumentSummaryRecord>> {
        document_storage_unavailable()
    }

    /// List document ids in `scope` without requiring canonical content.
    ///
    /// The default preserves compatibility for external stores; database-backed
    /// implementations should project only the id column for maintenance scans.
    async fn list_document_ids(&self, scope: DocumentScope) -> Result<Vec<DocumentId>> {
        Ok(self
            .list_documents(scope)
            .await?
            .into_iter()
            .map(|document| document.id)
            .collect())
    }

    /// Journal one turn's changes to granted folders and prune the chat's
    /// history back to its undo window.
    ///
    /// The prior bytes each record names must already be published to the blob
    /// store: the row is what makes them live, so a row committed ahead of its
    /// bytes points at nothing. Committing the rows cancels any retirement
    /// queued for those blobs, and drops the journal for turns outside the
    /// window, enqueueing whatever that frees.
    async fn record_exec_file_snapshots(
        &self,
        _chat_id: ChatId,
        _turn_id: TurnId,
        _files: &[ExecFileSnapshotRecord],
    ) -> Result<()> {
        document_storage_unavailable()
    }

    /// This chat's journaled file changes, newest first.
    async fn list_exec_file_snapshots(&self, _chat_id: ChatId) -> Result<Vec<ExecFileSnapshot>> {
        document_storage_unavailable()
    }

    /// Journal the staged files one turn could not safely materialize.
    async fn record_exec_file_rejections(
        &self,
        _chat_id: ChatId,
        _turn_id: TurnId,
        _files: &[ExecFileRejectionRecord],
    ) -> Result<()> {
        document_storage_unavailable()
    }

    /// This chat's rejected staged files, newest first.
    async fn list_exec_file_rejections(&self, _chat_id: ChatId) -> Result<Vec<ExecFileRejection>> {
        document_storage_unavailable()
    }

    /// Read the coalesced retirement state for one source blob.
    async fn get_blob_retirement(&self, _blob_id: uuid::Uuid) -> Result<Option<BlobRetirement>> {
        document_storage_unavailable()
    }

    /// Ensure an old filesystem orphan has a durable retirement candidate.
    ///
    /// Returns `true` only when a missing, succeeded, or cancelled episode was
    /// queued. Referenced blobs, active work, and exhausted failures are left
    /// unchanged. Filesystem auditors must hold the publisher/retirer blob guard.
    async fn ensure_orphan_blob_retirement(&self, _blob_id: uuid::Uuid) -> Result<bool> {
        document_storage_unavailable()
    }

    /// Claim the oldest effective-due blob retirement under a fresh lease.
    ///
    /// `lease_expires_at` must be after `now`. Expired running work is reclaimed
    /// with a new token and attempt; an expired final attempt becomes failed and
    /// the claim scan continues to the next candidate.
    async fn claim_blob_retirement(
        &self,
        _now: chrono::DateTime<chrono::Utc>,
        _lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<BlobRetirement>> {
        document_storage_unavailable()
    }

    /// Extend one exact live blob-retirement lease monotonically.
    async fn heartbeat_blob_retirement(
        &self,
        _blob_id: uuid::Uuid,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
        _lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        document_storage_unavailable()
    }

    /// Revalidate one exact live retirement lease immediately before deletion.
    ///
    /// This atomically cancels the retirement if an authoritative document
    /// reference exists. Callers must hold the same cross-process blob guard
    /// used by source publishers until deletion and resolution finish.
    async fn validate_blob_retirement_lease(
        &self,
        _blob_id: uuid::Uuid,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        document_storage_unavailable()
    }

    /// Mark one exact live blob-retirement lease as successfully deleted.
    ///
    /// Returns `false` if the row is no longer running under the exact,
    /// unexpired lease or `completed_at` would regress durable state.
    async fn complete_blob_retirement(
        &self,
        _blob_id: uuid::Uuid,
        _lease_token: uuid::Uuid,
        _completed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        document_storage_unavailable()
    }

    /// Record a deletion failure for one exact live blob-retirement lease.
    ///
    /// A future `retry_at` moves work with attempts remaining to `retry_wait`;
    /// no retry time, or an exhausted attempt budget, moves it to `failed`.
    /// Returns the resulting state, or `None` when the lease lost ownership.
    async fn record_blob_retirement_failure(
        &self,
        _blob_id: uuid::Uuid,
        _lease_token: uuid::Uuid,
        _failed_at: chrono::DateTime<chrono::Utc>,
        _retry_at: Option<chrono::DateTime<chrono::Utc>>,
        _error_code: &str,
        _error_detail: Option<&str>,
    ) -> Result<Option<BlobRetirementStatus>> {
        document_storage_unavailable()
    }

    /// Hard-delete source content.
    async fn delete_document(&self, _id: DocumentId) -> Result<()> {
        document_storage_unavailable()
    }

    /// Create or replace authoritative document content.
    ///
    /// Replacements preserve `created_at` and use last-write-wins semantics.
    /// `project_id`, when present, must identify an existing project. A live
    /// document cannot move between corpora.
    async fn upsert_document(&self, _document: &DocumentUpsert) -> Result<DocumentRecord> {
        document_storage_unavailable()
    }

    /// Atomically accept an already-published source blob and decoded text.
    async fn accept_document_source(
        &self,
        _document: &DocumentSourceUpsert,
    ) -> Result<DocumentRecord> {
        document_storage_unavailable()
    }

    /// Persist a new chat.
    ///
    /// The ordered projection must be valid. When `project_id` is set, the
    /// project must exist and the leading `project_default` roots must exactly
    /// snapshot its current ordered defaults. The insertion is atomic.
    async fn create_chat(&self, chat: &Chat) -> Result<()>;

    /// Persist a new chat while atomically deriving its project-default roots.
    ///
    /// `chat` must carry revision zero and an empty projection. Implementations
    /// resolve the current project inside the same atomic operation that inserts
    /// the chat, returning the exact persisted snapshot.
    async fn create_chat_with_project_defaults(&self, chat: &Chat) -> Result<Chat>;

    /// Fetch a chat by id, or `None` if it doesn't exist.
    async fn get_chat(&self, id: ChatId) -> Result<Option<Chat>>;

    /// List chats, most-recently-created first.
    async fn list_chats(&self) -> Result<Vec<Chat>>;

    /// Remove a conversation and its terminal product history atomically.
    ///
    /// This deliberately fails closed while any turn can still run, while any
    /// root remains attached, or while broker reconciliation is pending. The
    /// caller must first finish cancellation and use the durable root-detach
    /// flow; deletion never guesses at native broker state. Conversation-owned
    /// documents are removed and retained source blobs are enqueued for
    /// asynchronous retirement.
    async fn delete_chat(&self, _id: ChatId) -> Result<DeleteChatOutcome> {
        Err(AgentError::Store(
            "conversation deletion is not implemented by this Store".into(),
        ))
    }

    /// Atomically load a chat's durable messages and its event-journal
    /// watermark. Returns `None` when the chat does not exist.
    async fn get_chat_transcript(&self, id: ChatId) -> Result<Option<ChatTranscriptSnapshot>>;

    /// Set (or clear, with `None`) a chat's model override. A no-op if the chat
    /// doesn't exist.
    async fn set_chat_model(&self, id: ChatId, model: Option<String>) -> Result<()>;

    /// Set (or clear, with `None`) a chat's human-facing title. A no-op if the
    /// chat doesn't exist.
    async fn set_chat_title(&self, id: ChatId, title: Option<String>) -> Result<()>;

    /// Set a chat's title only while it has none, reporting whether it applied.
    ///
    /// This is the write a derived title must use. A user rename is the
    /// authoritative one, and it can land while a derived title is still being
    /// produced; an unconditional write would replace the name the user just
    /// typed with a guess. Whoever names the conversation first keeps it, which
    /// also makes renaming a chat the way to opt out of ever being renamed for.
    async fn set_chat_title_if_unset(&self, id: ChatId, title: &str) -> Result<bool>;

    /// Atomically update whichever user-editable chat metadata fields are
    /// present. An outer `None` leaves that field alone; an inner `None`
    /// clears it. Returns `false` if the chat does not exist.
    async fn update_chat_metadata(
        &self,
        id: ChatId,
        title: Option<Option<String>>,
        model: Option<Option<String>>,
        reasoning_effort: Option<Option<ReasoningEffort>>,
        permission_mode: Option<Option<PermissionMode>>,
        network_policy: Option<NetworkPolicy>,
    ) -> Result<bool>;

    /// Create a conversation output together with its first revision.
    ///
    /// The caller has already written the revision's bytes to conversation
    /// private scratch under [`crate::deliverable::output_revision_relative_path`]
    /// and supplies their exact length and digest. Reusing `request.id` with
    /// identical content returns the original record so an ambiguous store
    /// response can be retried; reusing it with different content is rejected.
    async fn create_output(&self, _request: &CreateOutput) -> Result<OutputRecord> {
        output_storage_unavailable()
    }

    /// Append an immutable revision and publish it as the output's current one.
    ///
    /// The previous revision is retained and stays addressable by its own id,
    /// so an update can never destroy the bytes it replaced. Reusing
    /// `revision.id` with identical content is an exact retry.
    async fn append_output_revision(
        &self,
        _output_id: OutputId,
        _revision: &NewOutputRevision,
    ) -> Result<OutputRecord> {
        output_storage_unavailable()
    }

    /// Fetch one output by opaque id, including a soft-deleted one.
    async fn get_output(&self, _id: OutputId) -> Result<Option<OutputRecord>> {
        output_storage_unavailable()
    }

    /// List a conversation's live outputs, most recently updated first.
    async fn list_outputs(&self, _chat_id: ChatId, _limit: u64) -> Result<Vec<OutputRecord>> {
        output_storage_unavailable()
    }

    /// List one output's revisions, newest first.
    async fn list_output_revisions(&self, _output_id: OutputId) -> Result<Vec<OutputRevision>> {
        output_storage_unavailable()
    }

    /// Fetch one revision by opaque id.
    async fn get_output_revision(&self, _id: OutputRevisionId) -> Result<Option<OutputRevision>> {
        output_storage_unavailable()
    }

    /// Soft-delete an output, hiding it from the catalog while retaining its
    /// revisions. Returns `false` only when the output does not exist; deleting
    /// an already-deleted output is the same durable outcome, not a conflict.
    async fn delete_output(
        &self,
        _id: OutputId,
        _deleted_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        output_storage_unavailable()
    }

    /// Restore a soft-deleted output, returning it to the catalog. This is the
    /// exact inverse of [`Store::delete_output`], so retracting an auto-merged
    /// output is reversible. Returns `false` only when the output does not
    /// exist; restoring a live output is the same durable outcome, not a
    /// conflict. Nothing about the revision history changes.
    async fn restore_output(
        &self,
        _id: OutputId,
        _restored_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        output_storage_unavailable()
    }

    /// Republish an existing revision of an output as its current one.
    ///
    /// This is the revert primitive: it moves the current-revision pointer to
    /// any revision already recorded for the output without appending or
    /// destroying anything, so it is fully reversible. The revision must belong
    /// to the output, and the output must be live. The revision count is
    /// unchanged; only the current pointer and update time move.
    async fn set_current_output_revision(
        &self,
        _output_id: OutputId,
        _revision_id: OutputRevisionId,
        _updated_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<OutputRecord> {
        output_storage_unavailable()
    }

    /// Create a profile-scoped local app together with its first revision.
    ///
    /// The caller has already published the bundle bytes under the profile
    /// data directory at [`crate::local_app::app_revision_relative_path`] and
    /// supplies their exact length and digest; the manifest is validated
    /// structurally before anything is stored. Reusing `request.id` with
    /// identical content returns the original record so an ambiguous store
    /// response can be retried; reusing it with different content is rejected.
    async fn create_app(&self, _request: &CreateApp) -> Result<AppRecord> {
        app_storage_unavailable()
    }

    /// Append an immutable revision and publish it as the app's current one.
    ///
    /// The previous revision is retained and stays addressable by its own id,
    /// so an update can never destroy the bundle it replaced. Reusing
    /// `revision.id` with identical content is an exact retry; reaching the
    /// revision cap refuses the write rather than dropping history.
    async fn append_app_revision(
        &self,
        _app_id: AppId,
        _revision: &NewAppRevision,
    ) -> Result<AppRecord> {
        app_storage_unavailable()
    }

    /// Fetch one app by opaque id, including a soft-deleted one.
    async fn get_app(&self, _id: AppId) -> Result<Option<AppRecord>> {
        app_storage_unavailable()
    }

    /// List the profile's live apps, most recently updated first.
    async fn list_apps(&self, _limit: u64) -> Result<Vec<AppRecord>> {
        app_storage_unavailable()
    }

    /// List one app's revisions, newest first.
    async fn list_app_revisions(&self, _app_id: AppId) -> Result<Vec<AppRevision>> {
        app_storage_unavailable()
    }

    /// Fetch one app revision by opaque id.
    async fn get_app_revision(&self, _id: AppRevisionId) -> Result<Option<AppRevision>> {
        app_storage_unavailable()
    }

    /// Soft-delete an app, hiding it from the library while retaining its
    /// revisions. Returns `false` only when the app does not exist; deleting
    /// an already-deleted app is the same durable outcome, not a conflict.
    async fn delete_app(
        &self,
        _id: AppId,
        _deleted_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        app_storage_unavailable()
    }

    /// Restore a soft-deleted app, returning it to the library. The exact
    /// inverse of [`Store::delete_app`]; the revision history is untouched.
    /// Returns `false` only when the app does not exist; restoring a live app
    /// is the same durable outcome, not a conflict.
    async fn restore_app(
        &self,
        _id: AppId,
        _restored_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        app_storage_unavailable()
    }

    /// Record explicit user consent for one app, replacing any previous grant.
    ///
    /// The grant is host-computed from the app's current manifest and the
    /// server definitions current at consent time; implementations validate
    /// its bindings with the manifest grammar and refuse a missing or deleted
    /// app. There is at most one grant per app.
    async fn put_app_grant(&self, _grant: &AppGrant) -> Result<()> {
        app_storage_unavailable()
    }

    /// Fetch one app's grant, when the user has consented and not revoked.
    async fn get_app_grant(&self, _app_id: AppId) -> Result<Option<AppGrant>> {
        app_storage_unavailable()
    }

    /// Revoke one app's grant. Returns `false` when no grant existed;
    /// revoking twice is the same durable outcome, not a conflict.
    async fn delete_app_grant(&self, _app_id: AppId) -> Result<bool> {
        app_storage_unavailable()
    }

    /// List every connected app the profile holds, oldest first.
    ///
    /// Kind-specific definitions come back as the bounded JSON the owning
    /// layer stored; callers parse per kind and fail closed per record.
    async fn list_connected_apps(&self) -> Result<Vec<ConnectedApp>> {
        connected_app_storage_unavailable()
    }

    /// Replace the profile's connected apps of one kind wholesale.
    ///
    /// Mirrors the settings surfaces that edit a complete list: rows of
    /// `kind` absent from `apps` are deleted, present ids are updated in
    /// place (keeping their `created_at`), and new ids are inserted. Records
    /// of other kinds are untouched. Implementations validate each record's
    /// kind-independent contract and refuse a mixed-kind call.
    async fn replace_connected_apps(
        &self,
        _kind: ConnectedAppKind,
        _apps: &[ConnectedApp],
    ) -> Result<()> {
        connected_app_storage_unavailable()
    }

    /// Persist the next versioned context checkpoint for one conversation.
    ///
    /// Implementations verify that the inclusive source-message boundary
    /// belongs to `checkpoint.chat_id`, and serialize writes per chat. An exact
    /// retry recovers the durable record; stale and conflicting rewrites are
    /// returned as typed outcomes instead of replacing newer context.
    async fn save_context_checkpoint(
        &self,
        _checkpoint: &ContextCheckpoint,
    ) -> Result<SaveContextCheckpointOutcome> {
        context_checkpoint_storage_unavailable()
    }

    /// Fetch the one current semantic checkpoint for a conversation.
    ///
    /// This record is intentionally distinct from visible messages. Consumers
    /// that later project it into a provider request must treat it as bounded,
    /// untrusted historical data rather than as a capability grant.
    async fn get_context_checkpoint(&self, _chat_id: ChatId) -> Result<Option<ContextCheckpoint>> {
        context_checkpoint_storage_unavailable()
    }

    /// Atomically begin one exact broker-backed attachment change.
    ///
    /// Implementations validate `request`, lock authoritative chat/projection
    /// state, derive broker subject and prior projection metadata, enforce one
    /// awaiting change per chat, and durably project intent before returning.
    /// Transport adapters must derive `executor_id` from authenticated native
    /// control; it is not renderer-selected authorization.
    async fn begin_root_attachment_change(
        &self,
        _request: &BeginRootAttachmentChange,
    ) -> Result<BeginRootAttachmentChangeOutcome> {
        root_attachment_storage_unavailable()
    }

    /// Atomically finish one exact change under its stable executor.
    ///
    /// Exact terminal retries return `Existing`. Implementations apply the
    /// final projection, terminal receipt, and result revision together. The
    /// server-owned finish time is clamped to the immutable creation time under
    /// the operation lock so wall-clock skew cannot wedge pending work.
    /// Adapters must first bind the broker receipt to this exact persisted
    /// operation; arbitrary transport failures are not durable broker failures.
    async fn finish_root_attachment_change(
        &self,
        _id: RootAttachmentChangeId,
        _executor_id: uuid::Uuid,
        _terminal: &RootAttachmentChangeTerminal,
        _finished_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<FinishRootAttachmentChangeOutcome> {
        root_attachment_storage_unavailable()
    }

    /// Fetch one attachment change by exact idempotency identity.
    async fn get_root_attachment_change(
        &self,
        _id: RootAttachmentChangeId,
    ) -> Result<Option<RootAttachmentChange>> {
        root_attachment_storage_unavailable()
    }

    /// List up to `limit` awaiting changes owned by one stable native executor.
    ///
    /// `limit` must be in `1..=MAX_PENDING_ROOT_ATTACHMENT_CHANGES` and results
    /// are returned in deterministic oldest-first order.
    async fn list_pending_root_attachment_changes(
        &self,
        _executor_id: uuid::Uuid,
        _limit: u64,
    ) -> Result<Vec<RootAttachmentChange>> {
        root_attachment_storage_unavailable()
    }

    /// Atomically accept one foreground coordinator or sandboxed child run.
    ///
    /// `id` is the run's stable idempotency identity. Foreground runs require no
    /// parent, spawn call, or input and become active immediately. Sandboxed
    /// runs require a unique `spawn_call_id`, non-empty task, and active
    /// depth-zero foreground parent in the same chat; they are accepted as
    /// queued depth-one work. An exact spawn-call retry recovers the original
    /// run even if the caller supplies a fresh run id. Recursive children are
    /// rejected by construction.
    async fn accept_agent_run(
        &self,
        _id: AgentRunId,
        _chat_id: ChatId,
        _parent_id: Option<AgentRunId>,
        _spawn_call_id: Option<CallId>,
        _tier: AgentRunTier,
        _input: Option<&str>,
    ) -> Result<AcceptAgentRunOutcome> {
        agent_run_storage_unavailable()
    }

    /// Admit one depth-one sandbox child without advancing its origin turn.
    ///
    /// The child id is derived from `spawn_call_id`; callers cannot choose a
    /// second identity for the same model request. The origin turn, foreground
    /// parent, child, and immutable admission receipt commit together under the
    /// chat/turn write lock. Existing exact receipts are recovered before the
    /// bounded outstanding-child check, making an ambiguous commit retry safe.
    /// A non-blocking checkpoint may additionally bind one exact root-relative
    /// file identity after validating its root against the locked chat
    /// attachment projection; the receipt itself grants no host authority.
    /// The stronger checkpoint boundary below composes this admission with the
    /// foreground transcript, progress, event, and immediate continuation.
    #[allow(clippy::too_many_arguments)]
    async fn admit_sandbox_agent_run(
        &self,
        _origin_turn_id: TurnId,
        _spawn_call_id: CallId,
        _input: &str,
        _lease_token: uuid::Uuid,
        _expected_steer_revision: i64,
        _max_outstanding_children: u32,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<AdmitSandboxAgentRunOutcome>> {
        agent_run_storage_unavailable()
    }

    /// Admit one depth-one sandbox child that executes inside a sandbox-resident
    /// container rather than in process.
    ///
    /// Identical to [`Store::admit_sandbox_agent_run`] except the child's
    /// [`AgentRunExecutionLocation`](crate::model::AgentRunExecutionLocation) is
    /// `Container`, so the in-process scheduler leaves it and the
    /// sandbox-resident driver claims it with
    /// [`Store::claim_container_agent_run`], provisions a container, attaches,
    /// proxies model inference back over the reverse channel, and commits the
    /// result through the same fenced result path.
    #[allow(clippy::too_many_arguments)]
    async fn admit_sandbox_container_agent_run(
        &self,
        _origin_turn_id: TurnId,
        _spawn_call_id: CallId,
        _input: &str,
        _lease_token: uuid::Uuid,
        _expected_steer_revision: i64,
        _max_outstanding_children: u32,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<AdmitSandboxAgentRunOutcome>> {
        agent_run_storage_unavailable()
    }

    /// Claim one specific queued sandbox-resident container run by id under an
    /// exact bounded lease.
    ///
    /// The sandbox-resident driver calls this for the exact run it is
    /// provisioning a container for; unlike [`Store::claim_agent_run`], which the
    /// in-process scheduler uses to select the oldest due in-process run, this
    /// only transitions a fresh `queued` `container` run to `running`. Reusing
    /// `lease_token` recovers its original still-live claim and never claims
    /// different work. The returned lease fences the run's result commit exactly
    /// as an in-process claim does. Refuses — leaving the run queued — while
    /// `max_running_containers` container runs are already running; container
    /// runs bypass the in-process scheduler's limits, so this claim is where
    /// their own bound is enforced.
    async fn claim_container_agent_run(
        &self,
        _id: AgentRunId,
        _lease_token: uuid::Uuid,
        _lease_duration: chrono::Duration,
        _max_running_containers: u32,
    ) -> Result<Option<AgentRun>> {
        agent_run_storage_unavailable()
    }

    /// List bounded oldest-first candidates for the container-run worker.
    ///
    /// This scan is only latency and recovery plumbing: every returned id must
    /// still pass [`Store::claim_container_agent_run`]'s transactional status,
    /// deadline, admission, and concurrency checks before any container is
    /// provisioned.
    async fn list_container_agent_run_candidates(&self, _limit: u64) -> Result<Vec<AgentRunId>> {
        agent_run_storage_unavailable()
    }

    /// List container-located runs whose driver died: `running` under an
    /// expired lease with the deadline still open. The in-process lease reaper
    /// deliberately exempts container runs, so this scan feeds the recovery
    /// pass that replaces it.
    async fn list_reclaimable_container_agent_runs(
        &self,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<AgentRun>> {
        agent_run_storage_unavailable()
    }

    /// Reclaim one expired-lease container run under a fresh bounded lease,
    /// **without** a second execution attempt: exactly one container was ever
    /// asked to run it, so recovery re-drives that same attempt through the
    /// durable provisioning record and the operation log. Refuses a live lease
    /// and a crossed deadline. Reusing `lease_token` recovers only its original
    /// still-live claim.
    async fn reclaim_container_agent_run(
        &self,
        _id: AgentRunId,
        _lease_token: uuid::Uuid,
        _lease_duration: chrono::Duration,
    ) -> Result<Option<AgentRun>> {
        agent_run_storage_unavailable()
    }

    /// Commit a durable provisioning intent for one container run, before the
    /// backend is asked to create anything. Returns the existing record instead
    /// when one is already present, so a restarted host reconciles rather than
    /// provisioning a second sandbox for the same single-attempt run.
    async fn begin_sandbox_provision(
        &self,
        _run_id: uuid::Uuid,
        _tag: &str,
        _window_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<BeginSandboxProvisionOutcome> {
        agent_run_storage_unavailable()
    }

    /// Commit the backend's handle onto the run's `Intended` record. Returns
    /// `false` if the record is no longer `Intended` — the window lapsed and the
    /// sweep claimed it first — in which case the caller owns a sandbox the
    /// durable state has already disowned and must destroy it.
    async fn commit_sandbox_provision_handle(
        &self,
        _run_id: uuid::Uuid,
        _handle: &str,
    ) -> Result<bool> {
        agent_run_storage_unavailable()
    }

    /// Move one run's provisioning record to `Teardown`, whatever non-`Done`
    /// state it is in, returning it. `None` if no record exists or the sandbox
    /// is already confirmed gone.
    async fn enqueue_sandbox_teardown(
        &self,
        _run_id: uuid::Uuid,
    ) -> Result<Option<SandboxProvision>> {
        agent_run_storage_unavailable()
    }

    /// Mark one run's `Teardown` record `Done` after its destroy confirmed.
    async fn complete_sandbox_teardown(&self, _run_id: uuid::Uuid) -> Result<()> {
        agent_run_storage_unavailable()
    }

    /// Move every `Intended` record whose window lapsed before `now` to
    /// `Teardown`, returning the lapsed records. The admission failed on the
    /// intent whether or not a create ever reached the provider; the tag sweep
    /// reclaims whatever the provider holds under those tags.
    async fn lapse_sandbox_provisions(
        &self,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<SandboxProvision>> {
        agent_run_storage_unavailable()
    }

    /// Every record currently owing a teardown.
    async fn list_sandbox_teardowns(&self) -> Result<Vec<SandboxProvision>> {
        agent_run_storage_unavailable()
    }

    /// One run's provisioning record, if any.
    async fn get_sandbox_provision(&self, _run_id: uuid::Uuid) -> Result<Option<SandboxProvision>> {
        agent_run_storage_unavailable()
    }

    /// Retain a well-formed result that failed the fenced commit predicate —
    /// the run was already terminal or the lease was gone — as
    /// non-authoritative evidence on the provisioning record. First writer
    /// wins; returns whether this call retained it. Never commits anything.
    async fn record_late_container_result_evidence(
        &self,
        _run_id: uuid::Uuid,
        _text: &str,
    ) -> Result<bool> {
        agent_run_storage_unavailable()
    }

    /// The correlation tags of every live provisioning record — `Intended`
    /// within its window plus `Committed` — the set the orphan sweep must not
    /// reclaim. An `Intended` tag stays live until [`lapse_sandbox_provisions`]
    /// moves it, so the sweep can never race a slow in-flight create.
    ///
    /// [`lapse_sandbox_provisions`]: Store::lapse_sandbox_provisions
    async fn live_sandbox_tags(&self) -> Result<Vec<String>> {
        agent_run_storage_unavailable()
    }

    /// Atomically admit one depth-one sandbox child and yield the foreground
    /// turn at a non-blocking spawn boundary.
    ///
    /// Exact receipt recovery runs before mutable lease and steering checks.
    /// A successful transition writes one terminal, non-executable
    /// orchestration tool call and its `ToolCallCompleted` event, applies one
    /// progress delta, then moves `running` to `resuming` with no live lease.
    /// Foreground orchestration advertises this together with the explicit
    /// ordered wait boundary; sandbox agents receive neither contract.
    async fn checkpoint_sandbox_spawn(
        &self,
        _request: &crate::model::SandboxSpawnCheckpointRequest,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<CheckpointSandboxSpawnOutcome>> {
        agent_run_storage_unavailable()
    }

    /// Fetch immutable origin ownership for an admitted sandbox child.
    async fn get_sandbox_agent_admission(
        &self,
        _child_run_id: AgentRunId,
    ) -> Result<Option<crate::model::SandboxAgentAdmission>> {
        agent_run_storage_unavailable()
    }

    /// Atomically accept one depth-one sandbox child and release the exact
    /// owning foreground turn claim into a matching child-result wait.
    ///
    /// The parent is derived from the turn rather than supplied by a caller,
    /// so a sandbox run cannot be parked against a turn owned by another
    /// coordinator. Exact retries recover the immutable child and checkpoint
    /// receipt; a child accepted through any other path is never retrofitted
    /// into this transition.
    #[allow(clippy::too_many_arguments)] // This durable checkpoint contract intentionally keeps its receipt fields explicit.
    async fn accept_sandbox_agent_run_and_park_turn(
        &self,
        _child_run_id: AgentRunId,
        _turn_id: TurnId,
        _spawn_call_id: CallId,
        _input: &str,
        _lease_token: uuid::Uuid,
        _expected_steer_revision: i64,
        _progress: TurnCheckpointProgress,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<AcceptSandboxAgentRunAndParkTurnOutcome>> {
        agent_run_storage_unavailable()
    }

    /// Fetch one agent run by its exact idempotency identity.
    async fn get_agent_run(&self, _id: AgentRunId) -> Result<Option<AgentRun>> {
        agent_run_storage_unavailable()
    }

    /// List a chat's runs in deterministic creation order.
    async fn list_agent_runs(&self, _chat_id: ChatId) -> Result<Vec<AgentRun>> {
        agent_run_storage_unavailable()
    }

    /// Atomically claim the oldest due sandbox run under exact bounded lease
    /// ownership.
    ///
    /// The global scheduler lock makes global and per-chat concurrency limits
    /// race-safe across processes. Expired leases are reclaimed only while the
    /// attempt budget remains; exhausted attempts and wall-clock deadlines are
    /// terminalized before scanning continues. Reusing `lease_token` recovers
    /// only its original still-live claim and can never claim different work.
    async fn claim_agent_run(
        &self,
        _lease_token: uuid::Uuid,
        _lease_duration: chrono::Duration,
        _max_running_global: u32,
        _max_running_per_chat: u32,
    ) -> Result<Option<AgentRun>> {
        agent_run_storage_unavailable()
    }

    /// Monotonically extend one exact live sandbox lease without resurrecting
    /// expiry or crossing the run's absolute deadline.
    async fn heartbeat_agent_run(
        &self,
        _id: AgentRunId,
        _lease_token: uuid::Uuid,
        _lease_duration: chrono::Duration,
    ) -> Result<bool> {
        agent_run_storage_unavailable()
    }

    /// Atomically accept canonical sandbox tool arguments, record the exact
    /// originating sandbox lease, and release that lease into `waiting`.
    /// Exact retries recover the checkpoint after the call resolves.
    async fn park_agent_run_for_sandbox_tool_call(
        &self,
        _agent_run_id: AgentRunId,
        _lease_token: uuid::Uuid,
        _call: &crate::model::SandboxToolCallRequest,
    ) -> Result<ParkSandboxToolCallOutcome> {
        agent_run_storage_unavailable()
    }

    /// Claim one accepted sandbox tool call under an exact expiring executor
    /// lease. The executor token is a capability and is never included in
    /// ordinary history reads.
    async fn claim_sandbox_tool_call(
        &self,
        _id: CallId,
        _lease_token: uuid::Uuid,
        _lease_duration: chrono::Duration,
    ) -> Result<ClaimSandboxToolCallOutcome> {
        agent_run_storage_unavailable()
    }

    /// Claim one accepted sandbox call only when its immutable tool name is
    /// exactly `name`. Executors use this filtered authority so one tool lane
    /// can never terminalize another tool's durable work.
    async fn claim_sandbox_tool_call_named(
        &self,
        _id: CallId,
        _name: &str,
        _lease_token: uuid::Uuid,
        _lease_duration: chrono::Duration,
    ) -> Result<ClaimSandboxToolCallOutcome> {
        agent_run_storage_unavailable()
    }

    /// Claim only the fixed delegated-file tool and atomically recover its
    /// pathless-root authority from a still-attached immutable admission.
    async fn claim_delegated_file_read(
        &self,
        _id: CallId,
        _lease_token: uuid::Uuid,
        _lease_duration: chrono::Duration,
    ) -> Result<ClaimDelegatedFileReadOutcome> {
        agent_run_storage_unavailable()
    }

    /// Extend a live executor lease only for the fixed delegated-file lane.
    async fn heartbeat_delegated_file_read(
        &self,
        _id: CallId,
        _lease_token: uuid::Uuid,
        _lease_duration: chrono::Duration,
    ) -> Result<Option<chrono::Duration>> {
        agent_run_storage_unavailable()
    }

    /// Resolve a live executor lease only for the fixed delegated-file lane.
    async fn resolve_delegated_file_read(
        &self,
        _id: CallId,
        _lease_token: uuid::Uuid,
        _resolution: &ToolCallResolution,
    ) -> Result<ResolveSandboxToolCallOutcome> {
        agent_run_storage_unavailable()
    }

    /// Revalidate one exact live sandbox-tool executor lease against the
    /// database clock and extend it up to its sandbox run deadline.
    ///
    /// This is the final cancellation/deadline fence before an executor may
    /// begin an external operation. `None` means cancellation, expiry, a
    /// terminal receipt, or a competing executor already won. `Some` returns
    /// the remaining lease budget calculated from the same database-clock
    /// transaction, so an executor need not compare host wall time to a stored
    /// absolute expiry.
    async fn heartbeat_sandbox_tool_call(
        &self,
        _id: CallId,
        _lease_token: uuid::Uuid,
        _lease_duration: chrono::Duration,
    ) -> Result<Option<chrono::Duration>> {
        agent_run_storage_unavailable()
    }

    /// Park a claimed sandbox tool call for its single bounded retry under
    /// the exact live executor lease. The call moves to `retry_wait` with a
    /// `retry_at` of the database clock plus `delay`, releases its executor
    /// lease, and becomes claimable again once `retry_at` passes; its waiting
    /// sandbox run is untouched. A call that already spent its retry cannot be
    /// parked again — that is an executor invariant breach, not a race.
    async fn retry_sandbox_tool_call(
        &self,
        _id: CallId,
        _lease_token: uuid::Uuid,
        _delay: chrono::Duration,
    ) -> Result<RetrySandboxToolCallOutcome> {
        agent_run_storage_unavailable()
    }

    /// Atomically write one immutable terminal receipt under the exact live
    /// executor lease and make its sandbox run claimable for continuation.
    /// Exact ambiguous retries recover the same receipt.
    async fn resolve_sandbox_tool_call(
        &self,
        _id: CallId,
        _lease_token: uuid::Uuid,
        _resolution: &ToolCallResolution,
    ) -> Result<ResolveSandboxToolCallOutcome> {
        agent_run_storage_unavailable()
    }

    /// Fetch a sandbox tool checkpoint by its stable model-visible identity.
    async fn get_sandbox_tool_call(
        &self,
        _id: CallId,
    ) -> Result<Option<crate::model::SandboxToolCall>> {
        agent_run_storage_unavailable()
    }

    /// Fetch the immutable terminal receipt, if sandbox tool work resolved.
    async fn get_sandbox_tool_call_receipt(
        &self,
        _id: CallId,
    ) -> Result<Option<crate::model::SandboxToolCallReceipt>> {
        agent_run_storage_unavailable()
    }

    /// List immutable sandbox tool checkpoints for one isolated run in creation
    /// order. A resumed sandbox rebuilds only its own tool transcript from
    /// these durable records and their terminal receipts.
    async fn list_sandbox_tool_calls_for_agent_run(
        &self,
        _agent_run_id: AgentRunId,
    ) -> Result<Vec<crate::model::SandboxToolCall>> {
        agent_run_storage_unavailable()
    }

    /// List bounded oldest-first accepted work and expired claims for durable
    /// executor recovery. Claiming remains the authority for ownership.
    async fn list_sandbox_tool_call_candidates(
        &self,
        _limit: u64,
    ) -> Result<Vec<crate::model::SandboxToolCall>> {
        agent_run_storage_unavailable()
    }

    /// List bounded oldest-first candidates for one exact immutable sandbox
    /// tool name. The matching claim method remains the ownership authority.
    async fn list_sandbox_tool_call_candidates_named(
        &self,
        _name: &str,
        _limit: u64,
    ) -> Result<Vec<crate::model::SandboxToolCall>> {
        agent_run_storage_unavailable()
    }

    /// Request cancellation using the database clock. Queued, waiting, and
    /// retry-wait runs become terminal immediately; a running worker retains
    /// its exact lease in `cancelling` until it acknowledges quiescence.
    async fn request_agent_run_cancellation(
        &self,
        _id: AgentRunId,
    ) -> Result<Option<RequestAgentRunCancellationOutcome>> {
        agent_run_storage_unavailable()
    }

    /// Fetch the exact immutable worker identity retained by a cancellation
    /// request. Trusted runtimes use this only for best-effort local wakeups;
    /// the cancellation row and run state remain authoritative.
    async fn get_agent_run_cancellation_signal(
        &self,
        _id: AgentRunId,
    ) -> Result<Option<crate::model::AgentRunCancellationSignal>> {
        agent_run_storage_unavailable()
    }

    /// Acknowledge cancellation with one exact live sandbox lease.
    async fn finish_agent_run_cancellation(
        &self,
        _id: AgentRunId,
        _lease_token: uuid::Uuid,
    ) -> Result<Option<FinishAgentRunCancellationOutcome>> {
        agent_run_storage_unavailable()
    }

    /// Atomically persist immutable final text and complete one exact live
    /// sandbox lease. An exact ambiguous retry returns the original receipt;
    /// stale, cancelled, or differently-payloaded submissions return `None`.
    async fn submit_agent_run_result(
        &self,
        _id: AgentRunId,
        _lease_token: uuid::Uuid,
        _text: &str,
    ) -> Result<Option<SubmitAgentRunResultOutcome>> {
        agent_run_storage_unavailable()
    }

    /// Atomically submit one validated folder-consent proposal as a sandbox's
    /// typed terminal receipt. This only wakes the foreground parent through
    /// its durable inbox; it cannot grant host access or invoke a client tool.
    async fn submit_agent_run_folder_access_proposal(
        &self,
        _id: AgentRunId,
        _lease_token: uuid::Uuid,
        _request: &crate::RequestFolderAccessArgs,
    ) -> Result<Option<SubmitAgentRunResultOutcome>> {
        agent_run_storage_unavailable()
    }

    /// Fence one exact sandbox lease after an execution failure. Attempts below
    /// the run budget become replay-safe retry work; the final attempt writes a
    /// parent-visible terminal receipt in the same transaction as `failed`.
    async fn fail_agent_run(
        &self,
        _id: AgentRunId,
        _lease_token: uuid::Uuid,
        _error_code: &str,
        _error_detail: &str,
        _retry_delay: chrono::Duration,
    ) -> Result<Option<FailAgentRunOutcome>> {
        agent_run_storage_unavailable()
    }

    /// List immutable child results delivered to one foreground coordinator.
    /// Consuming or waking a parent continuation is intentionally a separate
    /// state-machine transition.
    async fn list_agent_run_inbox(
        &self,
        _parent_run_id: AgentRunId,
    ) -> Result<Vec<AgentRunInboxEntry>> {
        agent_run_storage_unavailable()
    }

    /// List a bounded set of parent deliveries that may need durable
    /// continuation work. This is an advisory scan: an exact claim remains the
    /// authority for both pending delivery and expired-lease recovery.
    async fn list_agent_run_inbox_candidates(
        &self,
        _limit: u64,
    ) -> Result<Vec<AgentRunInboxEntry>> {
        agent_run_storage_unavailable()
    }

    /// List a bounded set of ordered child waits for which every immutable
    /// result appears ready. This scan is advisory and never claims member
    /// inboxes; the exact wait-set resume transition remains authoritative.
    async fn list_ready_agent_run_wait_set_candidates(
        &self,
        _limit: u64,
    ) -> Result<Vec<AgentRunWaitSetCandidate>> {
        turn_storage_unavailable()
    }

    /// Acquire an expiring, exact lease to advance one immutable parent inbox
    /// delivery. Repeating the same live lease recovers its ownership; an
    /// expired lease may be reclaimed by a different continuation worker.
    async fn claim_agent_run_inbox_entry(
        &self,
        _parent_run_id: AgentRunId,
        _child_run_id: AgentRunId,
        _lease_token: uuid::Uuid,
        _lease_duration: chrono::Duration,
    ) -> Result<Option<ClaimAgentRunInboxOutcome>> {
        agent_run_storage_unavailable()
    }

    /// Consume one exact inbox delivery under its live continuation lease.
    /// An ambiguous exact retry recovers the consumed receipt, while a stale
    /// lease cannot consume or overwrite a reclaimed delivery.
    async fn consume_agent_run_inbox_entry(
        &self,
        _parent_run_id: AgentRunId,
        _child_run_id: AgentRunId,
        _lease_token: uuid::Uuid,
    ) -> Result<Option<ConsumeAgentRunInboxOutcome>> {
        agent_run_storage_unavailable()
    }

    /// Atomically consume one exact child result and wake the foreground turn
    /// that checkpointed on it. The durable turn transition is the wake signal;
    /// callers may use ordinary turn claiming after this commit and never rely
    /// on a process-local notification.
    async fn consume_agent_run_inbox_entry_and_resume_turn(
        &self,
        _parent_run_id: AgentRunId,
        _child_run_id: AgentRunId,
        _lease_token: uuid::Uuid,
    ) -> Result<Option<ConsumeAgentRunInboxAndResumeTurnOutcome>> {
        agent_run_storage_unavailable()
    }

    /// Fetch one durable turn by its exact idempotency identity.
    async fn get_turn_run(&self, _id: TurnId) -> Result<Option<TurnRun>> {
        turn_storage_unavailable()
    }

    /// List a chat's durable turn history in deterministic creation-time order.
    async fn list_turn_runs(&self, _chat_id: ChatId) -> Result<Vec<TurnRun>> {
        turn_storage_unavailable()
    }

    /// Count in-flight work across every chat: non-terminal turns plus live
    /// background-tier agent runs. See [`ActiveWorkSnapshot`] for what counts
    /// and why the definition is strict. Callers gating a host restart must
    /// treat an error as "not quiescent".
    async fn count_active_work(&self) -> Result<ActiveWorkSnapshot> {
        turn_storage_unavailable()
    }

    /// Atomically persist a user's initial message and queue its exact turn.
    ///
    /// `id` is a non-nil caller-visible idempotency identity. Repeating the same id,
    /// chat, model, and byte-exact content returns [`AcceptTurnOutcome::Existing`]
    /// without another message or turn. Reusing an id with a different chat,
    /// model, or byte-exact content returns
    /// [`AcceptTurnOutcome::IdentityConflict`]. A different live turn for the
    /// chat returns [`AcceptTurnOutcome::ChatBusy`].
    async fn accept_turn(
        &self,
        id: TurnId,
        chat_id: ChatId,
        model: &str,
        content: &str,
    ) -> Result<AcceptTurnOutcome> {
        self.accept_turn_with_attachments(id, chat_id, model, content, &[], &[])
            .await
    }

    /// Accept a turn whose input message also carries image or file attachments.
    ///
    /// The attachments commit in the same transaction as the message and turn,
    /// and they participate in the same idempotency proof: a retry with the same
    /// id but different attachments is an [`AcceptTurnOutcome::IdentityConflict`],
    /// not a silent acceptance of the first submission. Each attachment is
    /// recorded at its position in its media-specific list, which is the order
    /// a reloaded transcript replays it in.
    ///
    /// Recording an attachment makes its blob live: any queued retirement for
    /// that blob is cancelled in the same transaction. Because blob ids are
    /// content-derived, re-submitting identical bytes re-references the existing
    /// blob rather than storing a second copy.
    async fn accept_turn_with_attachments(
        &self,
        _id: TurnId,
        _chat_id: ChatId,
        _model: &str,
        _content: &str,
        _images: &[ImageRef],
        _documents: &[DocumentId],
    ) -> Result<AcceptTurnOutcome> {
        turn_storage_unavailable()
    }

    /// Perform one durable claim action under a fresh exact lease.
    ///
    /// `lease_token` is the caller's idempotency identity: retrying it while its
    /// lease remains live returns the same running turn. Callers must retain it
    /// across an ambiguous commit and use a fresh token for a new claim attempt.
    /// Every successful claim increments `claim_count` and moves the turn to
    /// `running`. Queued, retry-wait, and expired-running claims also increment
    /// `attempt_count`; resuming claims retain the current failure attempt.
    /// Expired work is reclaimed only while another attempt is permitted. An
    /// expired cancellation or final attempt is terminalized with its exact
    /// routed journal event and returned instead of claiming another turn; the
    /// caller publishes it before scanning again. `lease_expires_at` must be
    /// after `now`.
    async fn claim_turn_run(
        &self,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
        _lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ClaimTurnRunOutcome> {
        turn_storage_unavailable()
    }

    /// Extend one exact live turn lease monotonically.
    ///
    /// Returns `false` if the turn is not running, the token differs, the lease
    /// already expired, or the proposed expiry does not extend the current one.
    async fn heartbeat_turn_run(
        &self,
        _id: TurnId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
        _lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        turn_storage_unavailable()
    }

    /// Report whether `lease_token` still owns the exact live segment of a turn.
    ///
    /// Returns [`TurnLeaseFence::Current`] only while the turn is running or
    /// cancelling under this exact token, its claim receipt still matches the
    /// turn's attempt and claim counters, and the lease has not expired at
    /// `now`. Any other state — a superseding claim, an expired lease, or a
    /// terminal turn — is [`TurnLeaseFence::Stale`]. This is a read-only fence a
    /// worker consults before committing an intermediate tool or message effect;
    /// it never mutates durable state.
    async fn fence_turn_lease(
        &self,
        _id: TurnId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<TurnLeaseFence> {
        turn_storage_unavailable()
    }

    /// Atomically accept one idempotent steering instruction for a live turn.
    ///
    /// The non-nil caller-supplied `id` also names the eventual user message.
    /// Exact retries compare chat, turn, byte-exact content, and interrupt intent.
    /// Queued, running, resuming, and retry-wait turns accept instructions;
    /// cancelling or terminal turns return
    /// [`AcceptTurnSteerOutcome::TurnUnavailable`].
    async fn accept_turn_steer(
        &self,
        _id: TurnSteerId,
        _turn_id: TurnId,
        _chat_id: ChatId,
        _content: &str,
        _interrupt: bool,
    ) -> Result<AcceptTurnSteerOutcome> {
        turn_storage_unavailable()
    }

    /// List pending instructions only while the caller owns the exact live lease.
    ///
    /// `Some` is ordered by durable acceptance time then identity. `None` means
    /// the lease is stale, expired, cancelling, or otherwise no longer running.
    async fn list_pending_turn_steers(
        &self,
        _turn_id: TurnId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<Vec<TurnSteer>>> {
        turn_storage_unavailable()
    }

    /// Persist one pending steer as a user message under the exact live lease.
    ///
    /// An optional preceding assistant candidate, the steer message, the
    /// application receipt, the revision increment, and its [`AgentEvent::UserSteered`]
    /// journal row commit atomically in transcript order. The event ordinal is
    /// the worker's exact attempt identity. Exact retries by the same lease and
    /// ordinal return [`ApplyTurnSteerOutcome::Existing`] with the same journal
    /// row even after the turn advances. A stale lease, rejected steer, or
    /// different winning lease returns `None`.
    #[allow(clippy::too_many_arguments)]
    async fn apply_turn_steer(
        &self,
        _turn_id: TurnId,
        _lease_token: uuid::Uuid,
        _steer_id: TurnSteerId,
        _attempt_event_ordinal: i32,
        _preceding_assistant: Option<&Message>,
        _preceding_citations: &[crate::AssistantCitationInput],
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<JournaledTurnSteerOutcome>> {
        turn_storage_unavailable()
    }

    /// Atomically persist the final assistant message and complete its turn.
    ///
    /// The exact claim must still be live at the fresh operational `now`, and
    /// the output cannot be dated after it. Repeating the same token and
    /// exact output identity, content, and database-normalized timestamp after
    /// an ambiguous commit returns the completed turn even after lease expiry,
    /// without inserting another message. Returns
    /// `None` when the token never owned this turn, its lease was lost, or
    /// another terminal outcome already won. Pending steering and stale model
    /// output return explicit nonterminal outcomes so callers can continue the
    /// same live attempt rather than mistaking them for lease loss. The caller
    /// must pass the `steer_revision` captured before generation;
    /// completion is fenced if another steer was applied in the meantime.
    async fn complete_turn_run(
        &self,
        _id: TurnId,
        _lease_token: uuid::Uuid,
        _expected_steer_revision: i64,
        _now: chrono::DateTime<chrono::Utc>,
        _output: &Message,
    ) -> Result<Option<CompleteTurnRunOutcome>> {
        turn_storage_unavailable()
    }

    /// Complete one claimed turn and append its terminal event atomically.
    ///
    /// Exact ambiguous retries recover both the completed turn and the same
    /// journal sequence. No terminal event is visible unless the output message
    /// and terminal state transition commit with it.
    #[allow(clippy::too_many_arguments)]
    async fn complete_turn_run_and_append_event(
        &self,
        _id: TurnId,
        _lease_token: uuid::Uuid,
        _expected_steer_revision: i64,
        _now: chrono::DateTime<chrono::Utc>,
        _output: &Message,
        _usage: Usage,
        _stop_reason: StopReason,
    ) -> Result<Option<JournaledTurnOutcome<CompleteTurnRunOutcome>>> {
        turn_storage_unavailable()
    }

    /// Complete one claimed turn with ordered evidence-backed assistant sources.
    ///
    /// The clean message, resolved same-turn citations, terminal transition, and
    /// journal event commit together. Unknown opaque references are ignored.
    #[allow(clippy::too_many_arguments)]
    async fn complete_turn_run_with_citations_and_append_event(
        &self,
        id: TurnId,
        lease_token: uuid::Uuid,
        expected_steer_revision: i64,
        now: chrono::DateTime<chrono::Utc>,
        output: &Message,
        citations: &[crate::AssistantCitationInput],
        usage: Usage,
        stop_reason: StopReason,
    ) -> Result<Option<JournaledTurnOutcome<CompleteTurnRunOutcome>>> {
        if citations.is_empty() {
            self.complete_turn_run_and_append_event(
                id,
                lease_token,
                expected_steer_revision,
                now,
                output,
                usage,
                stop_reason,
            )
            .await
        } else {
            turn_storage_unavailable()
        }
    }

    /// Complete one claimed turn as a refusal and append that structured
    /// terminal event atomically with its partial-or-empty assistant output.
    #[allow(clippy::too_many_arguments)]
    async fn complete_refused_turn_run_with_citations_and_append_event(
        &self,
        _id: TurnId,
        _lease_token: uuid::Uuid,
        _expected_steer_revision: i64,
        _now: chrono::DateTime<chrono::Utc>,
        _output: &Message,
        _citations: &[crate::AssistantCitationInput],
        _usage: Usage,
        _refusal: RefusalOutcome,
    ) -> Result<Option<JournaledTurnOutcome<CompleteTurnRunOutcome>>> {
        turn_storage_unavailable()
    }

    /// Atomically record a failure for one exact live claimed attempt.
    ///
    /// `now` is a fresh operational lease fence and is not part of the stable
    /// request identity. An exact retry is identified by the turn, claim token,
    /// retry intent, cumulative model steps and usage, error code, and error
    /// detail; it returns `Existing` even if a later attempt has already advanced
    /// the mutable turn. Reusing a token with different request data is an error.
    /// A requested retry moves the turn to `retry_wait` only while attempts
    /// remain; otherwise the result is terminally `failed`. Returns `None` when
    /// this claim did not win the live attempt or another resolution already did.
    #[allow(clippy::too_many_arguments)]
    async fn record_turn_run_failure(
        &self,
        _id: TurnId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
        _retry: TurnFailureRetry,
        _model_steps: i32,
        _usage: Usage,
        _error_code: &str,
        _error_detail: Option<&str>,
    ) -> Result<Option<RecordTurnFailureOutcome>> {
        turn_storage_unavailable()
    }

    /// Resolve one claimed failure and append its terminal event atomically.
    ///
    /// Retry-wait outcomes do not publish a terminal event. Terminal failures
    /// commit their receipt, turn transition, and `TurnFailed` journal row in
    /// one transaction, and exact ambiguous retries recover the original
    /// journal sequence.
    #[allow(clippy::too_many_arguments)]
    async fn record_turn_run_failure_and_append_event(
        &self,
        _id: TurnId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
        _retry: TurnFailureRetry,
        _model_steps: i32,
        _usage: Usage,
        _error_code: &str,
        _error_detail: Option<&str>,
    ) -> Result<Option<JournaledTurnOutcome<RecordTurnFailureOutcome>>> {
        turn_storage_unavailable()
    }

    /// Durably request cancellation for one exact turn.
    ///
    /// Queued, retry-wait, and resuming work becomes terminal immediately.
    /// Running work enters `cancelling` while retaining its exact lease, so the
    /// database's one-live-turn-per-chat invariant remains held until the
    /// cooperative worker actually stops. The empty-payload request converges
    /// on the exact turn identity, so cancelling/cancelled retries return
    /// `Existing`.
    async fn request_turn_cancellation(
        &self,
        _id: TurnId,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<RequestTurnCancellationOutcome>> {
        turn_storage_unavailable()
    }

    /// Request cancellation and publish an immediate terminal outcome atomically.
    ///
    /// Queued, retry-wait, and resuming turns commit `TurnCancelled` with their
    /// terminal transition. Running turns only enter `cancelling`; their worker
    /// publishes the terminal event when it acknowledges quiescence.
    async fn request_turn_cancellation_and_append_event(
        &self,
        _id: TurnId,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<JournaledTurnOutcome<RequestTurnCancellationOutcome>>> {
        turn_storage_unavailable()
    }

    /// Acknowledge that one exact cancelling worker has quiesced.
    ///
    /// The immutable claim receipt and terminal attempt make exact retries
    /// recoverable after lease expiry. Returns `None` for a stale token, a turn
    /// that is not cancelling, or a first-time acknowledgement with regressing
    /// operational time.
    async fn finish_turn_cancellation(
        &self,
        _id: TurnId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<FinishTurnCancellationOutcome>> {
        turn_storage_unavailable()
    }

    /// Acknowledge cancellation and publish its terminal event atomically.
    ///
    /// Exact ambiguous retries recover both the cancelled turn and the same
    /// journal sequence, including the usage recorded by the original worker.
    ///
    /// `output` carries the prose the cancelled turn had already streamed; a
    /// non-empty output commits as the turn's durable assistant message in the
    /// same transaction, so reload and the next model turn keep what the user
    /// was reading when they stopped the run (#1182).
    async fn finish_turn_cancellation_and_append_event(
        &self,
        _id: TurnId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
        _usage: Usage,
        _output: Option<&Message>,
        _citations: &[crate::AssistantCitationInput],
    ) -> Result<Option<JournaledTurnOutcome<FinishTurnCancellationOutcome>>> {
        turn_storage_unavailable()
    }

    /// Atomically persist one client-executed tool call, record the exact
    /// originating worker claim, and release the turn lease.
    ///
    /// Exact retries recover through the immutable wait receipt even after the
    /// client call resolves or the turn advances. The exact progress delta is
    /// part of that retry identity and is folded into turn-wide checkpoint
    /// accounting at most once. A pending steer fences the checkpoint so the
    /// worker can apply that instruction first.
    async fn park_turn_for_client_tool_call(
        &self,
        _turn_id: TurnId,
        _lease_token: uuid::Uuid,
        _expected_steer_revision: i64,
        _progress: TurnCheckpointProgress,
        _now: chrono::DateTime<chrono::Utc>,
        _call: &ClientToolCallRequest,
    ) -> Result<Option<ParkTurnForClientCallOutcome>> {
        turn_storage_unavailable()
    }

    /// Persist a foreground turn checkpoint while it awaits one exact sandbox
    /// child result. The live worker lease is released in the same transaction,
    /// and its committed progress becomes the baseline for the resumed worker.
    async fn park_turn_for_agent_run_inbox(
        &self,
        _turn_id: TurnId,
        _child_run_id: AgentRunId,
        _lease_token: uuid::Uuid,
        _expected_steer_revision: i64,
        _progress: TurnCheckpointProgress,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<ParkTurnForAgentRunInboxOutcome>> {
        turn_storage_unavailable()
    }

    /// Persist an ordered, unique, bounded child set and release a claimed
    /// foreground turn in the same transaction. Every child must carry an
    /// immutable sandbox admission owned by this exact origin turn. Exact
    /// retries recover the receipt before lease expiry or steering state is
    /// considered.
    async fn park_turn_for_agent_run_wait_set(
        &self,
        _request: &crate::model::AgentRunWaitSetCheckpointRequest,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<ParkTurnForAgentRunWaitSetOutcome>> {
        turn_storage_unavailable()
    }

    /// Consume every matching child inbox result exactly once and wake the
    /// foreground turn when the committed completion condition is satisfied.
    /// Results are returned in immutable request order, never delivery order.
    /// An exact retry with `resume_token` recovers the prior transition before
    /// mutable parent liveness is checked.
    async fn resume_turn_for_agent_run_wait_set(
        &self,
        _wait_id: CallId,
        _resume_token: uuid::Uuid,
    ) -> Result<Option<ResumeTurnForAgentRunWaitSetOutcome>> {
        turn_storage_unavailable()
    }

    /// Append a message to its chat.
    async fn append_message(&self, message: &Message) -> Result<()>;

    /// Atomically append a clean assistant message and its exact evidence-backed sources.
    async fn append_assistant_message_with_citations(
        &self,
        message: &Message,
        references: &[crate::AssistantCitationInput],
    ) -> Result<()> {
        if references.is_empty() {
            self.append_message(message).await
        } else {
            Err(AgentError::Store(
                "assistant citation storage is not implemented by this Store".into(),
            ))
        }
    }

    /// Atomically append one intermediate assistant message and its citations
    /// only while `lease_token` owns the exact live turn segment.
    async fn append_claimed_assistant_message_with_citations(
        &self,
        _message: &Message,
        _references: &[crate::AssistantCitationInput],
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<AppendClaimedMessageOutcome> {
        turn_storage_unavailable()
    }

    /// List a chat's messages in creation order.
    async fn list_messages(&self, chat_id: ChatId) -> Result<Vec<Message>>;

    /// Output message ids of the chat's cancelled turns (#1182).
    ///
    /// Context assembly appends an interruption note to these messages so the
    /// model is told the user stopped the response there, rather than left to
    /// infer it from a mid-sentence cut. Best-effort: the default keeps stores
    /// without turn state serving unannotated transcripts.
    async fn list_cancelled_output_message_ids(&self, _chat_id: ChatId) -> Result<Vec<MessageId>> {
        Ok(Vec::new())
    }

    /// List a chat's image attachments, ordered by message then position.
    ///
    /// The block transcript is rebuilt on load rather than stored, so this is
    /// how history regains the images a turn was submitted with. Stores without
    /// attachment support report none, which degrades a reloaded turn to its
    /// text rather than failing the load.
    async fn list_message_attachments(&self, _chat_id: ChatId) -> Result<Vec<MessageAttachment>> {
        Ok(Vec::new())
    }

    /// List a chat's file attachments, ordered by message then position.
    async fn list_message_document_attachments(
        &self,
        _chat_id: ChatId,
    ) -> Result<Vec<MessageDocumentAttachment>> {
        Ok(Vec::new())
    }

    /// Accept immutable canonical tool-call identity and arguments exactly once.
    async fn accept_tool_call(&self, call: &ToolCallRecord) -> Result<AcceptToolCallOutcome>;

    /// Atomically accept one server tool call only while its exact originating
    /// turn lease remains live. The stored lease is private replay state.
    async fn accept_claimed_tool_call(
        &self,
        _call: &ToolCallRecord,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<AcceptClaimedToolCallOutcome> {
        turn_storage_unavailable()
    }

    /// Register a Sensitive server tool call for durable human review.
    async fn request_tool_call_approval(
        &self,
        _request: &ApprovalRequest,
        _requested_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<RequestToolApprovalOutcome> {
        Err(AgentError::Store(
            "durable tool approval storage is not implemented by this Store".into(),
        ))
    }

    /// Register an approval and append `ApprovalRequired` in one claimed-turn
    /// transaction. Exact retries recover the same event sequence.
    async fn request_tool_call_approval_and_append_event(
        &self,
        _request: &ApprovalRequest,
        _lease_token: uuid::Uuid,
        _event_ordinal: i32,
        _requested_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<JournaledToolApprovalOutcome> {
        Err(AgentError::Store(
            "journaled durable tool approval storage is not implemented by this Store".into(),
        ))
    }

    /// Decide a previously registered approval exactly once.
    async fn decide_tool_call_approval(
        &self,
        _chat_id: ChatId,
        _call_id: CallId,
        _decision: &ApprovalDecision,
        _decided_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<DecideToolApprovalOutcome> {
        Err(AgentError::Store(
            "durable tool approval storage is not implemented by this Store".into(),
        ))
    }

    /// Decide a pending approval and persist its chosen chat-scoped standing
    /// grant in the same transaction. A grant can only be added while this
    /// exact call is pending; a later retry may not widen a one-shot decision.
    async fn decide_tool_call_approval_with_grant(
        &self,
        _chat_id: ChatId,
        _call_id: CallId,
        _decision: &ApprovalDecision,
        _grant: &StandingGrant,
        _decided_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<DecideToolApprovalOutcome> {
        Err(AgentError::Store(
            "durable standing-grant storage is not implemented by this Store".into(),
        ))
    }

    /// Read private approval state for exact recovery.
    async fn get_tool_call_approval(&self, _call_id: CallId) -> Result<Option<ToolApproval>> {
        Err(AgentError::Store(
            "durable tool approval storage is not implemented by this Store".into(),
        ))
    }

    /// A bounded page of calls the Auto-mode judge currently owns, oldest
    /// first, across all chats.
    async fn list_judging_tool_call_approvals(&self, _limit: u64) -> Result<Vec<ToolApproval>> {
        Err(AgentError::Store(
            "durable tool approval storage is not implemented by this Store".into(),
        ))
    }

    /// Land the Auto-mode judge's verdict on one parked call. `false` means
    /// the judge no longer owns it (a human got there first, or it resolved).
    async fn resolve_tool_call_approval_from_judge(
        &self,
        _chat_id: ChatId,
        _call_id: CallId,
        _approved: bool,
    ) -> Result<bool> {
        Err(AgentError::Store(
            "durable tool approval storage is not implemented by this Store".into(),
        ))
    }

    /// Every durable standing grant, newest first, across all chats.
    ///
    /// A malformed row is skipped, never surfaced: what cannot be described
    /// cannot be knowingly kept, and it already fails to authorize anything
    /// at match time for the same reason.
    async fn list_standing_tool_grants(&self) -> Result<Vec<crate::approval::StandingGrantRecord>> {
        Err(AgentError::Store(
            "durable standing-grant storage is not implemented by this Store".into(),
        ))
    }

    /// Withdraw one standing grant by the approval that created it. Later
    /// matching calls park on the gate again. Returns `false` when no such
    /// grant exists (already revoked, or never granted).
    async fn revoke_standing_tool_grant(&self, _source_call_id: CallId) -> Result<bool> {
        Err(AgentError::Store(
            "durable standing-grant storage is not implemented by this Store".into(),
        ))
    }

    /// List a bounded page of pending approvals for one chat.
    async fn list_pending_tool_call_approvals(
        &self,
        _chat_id: ChatId,
        _limit: u64,
    ) -> Result<Vec<ToolApproval>> {
        Err(AgentError::Store(
            "durable tool approval storage is not implemented by this Store".into(),
        ))
    }

    /// Claim the first lease with a caller-generated secret fencing token.
    /// A retry with the same executor and token recovers the original live
    /// claim even when the caller proposes a newly calculated expiry.
    async fn claim_client_tool_call(
        &self,
        id: CallId,
        chat_id: ChatId,
        executor_id: uuid::Uuid,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ClaimClientToolCallOutcome>;

    /// Monotonically extend an exact live client-execution lease.
    async fn heartbeat_client_tool_call(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<HeartbeatClientToolCallOutcome>;

    /// Resolve a pending server-executed tool call exactly once.
    async fn resolve_server_tool_call(
        &self,
        id: CallId,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ResolveToolCallOutcome>;

    /// Resolve a server call and retain the renderer projection it produced.
    async fn resolve_server_tool_call_with_artifacts(
        &self,
        id: CallId,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
        _preview: Option<&crate::ToolResultPreview>,
    ) -> Result<ResolveToolCallOutcome> {
        self.resolve_server_tool_call(id, resolution, resolved_at)
            .await
    }

    /// Resolve a server tool result only if the same live turn lease that
    /// accepted the call still owns the turn.
    #[allow(clippy::too_many_arguments)]
    async fn resolve_claimed_server_tool_call(
        &self,
        _id: CallId,
        _chat_id: ChatId,
        _turn_id: TurnId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
        _resolution: &ToolCallResolution,
        _resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ResolveToolCallOutcome> {
        turn_storage_unavailable()
    }

    /// The claimed-lease counterpart of
    /// [`Self::resolve_server_tool_call_with_artifacts`].
    #[allow(clippy::too_many_arguments)]
    async fn resolve_claimed_server_tool_call_with_artifacts(
        &self,
        id: CallId,
        chat_id: ChatId,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
        _preview: Option<&crate::ToolResultPreview>,
    ) -> Result<ResolveToolCallOutcome> {
        self.resolve_claimed_server_tool_call(
            id,
            chat_id,
            turn_id,
            lease_token,
            now,
            resolution,
            resolved_at,
        )
        .await
    }

    /// Resolve a pending server call recovered at worker startup without
    /// executing it again. An exact live lease for the same turn may commit
    /// this conservative interrupted result, including after a process restart
    /// that retained the lease.
    #[allow(clippy::too_many_arguments)]
    async fn abandon_inherited_server_tool_call(
        &self,
        _id: CallId,
        _chat_id: ChatId,
        _turn_id: TurnId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
        _resolution: &ToolCallResolution,
        _resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ResolveToolCallOutcome> {
        turn_storage_unavailable()
    }

    /// Resolve a pending client call under its exact unexpired executor lease.
    /// Once committed, the token and terminal payload are the stable retry
    /// identity; `resolved_at` records the first commit and is not compared on
    /// an ambiguous retry.
    async fn resolve_client_tool_call(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ResolveToolCallOutcome> {
        Ok(self
            .resolve_client_tool_call_and_append_event(
                id,
                chat_id,
                lease_token,
                now,
                resolution,
                resolved_at,
            )
            .await?
            .outcome)
    }

    /// Resolve a live client call and return any atomic turn transition receipt.
    /// Exact retries recover the same terminal event when client-owned
    /// cancellation completed with this resolution.
    async fn resolve_client_tool_call_and_append_event(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<JournaledClientToolCallOutcome>;

    /// Resolve a live client call, retaining the rows it reported.
    ///
    /// `rows` is the executor's *unvalidated* `{entries, failures}` payload, not
    /// a projection. The store builds the projection from it against the call's
    /// own stored name, so the allowlist and every clamp are applied here rather
    /// than trusted from the executor — a client cannot award itself a card for
    /// a tool that has none, nor an unbounded row.
    ///
    /// The default drops the rows, which costs the card and nothing else.
    #[allow(clippy::too_many_arguments)]
    async fn resolve_client_tool_call_and_append_event_with_rows(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
        _rows: Option<&serde_json::Value>,
    ) -> Result<JournaledClientToolCallOutcome> {
        self.resolve_client_tool_call_and_append_event(
            id,
            chat_id,
            lease_token,
            now,
            resolution,
            resolved_at,
        )
        .await
    }

    /// Resolve a known outcome after the exact client lease expired.
    ///
    /// This is the explicit recovery path for an ambiguous native interaction;
    /// it never transfers the call to another executor.
    async fn resolve_expired_client_tool_call(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ResolveToolCallOutcome> {
        Ok(self
            .resolve_expired_client_tool_call_and_append_event(
                id,
                chat_id,
                lease_token,
                now,
                resolution,
                resolved_at,
            )
            .await?
            .outcome)
    }

    /// Reconcile an expired client call and return any atomic turn transition
    /// receipt, with the same retry behavior as the live resolution path.
    async fn resolve_expired_client_tool_call_and_append_event(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<JournaledClientToolCallOutcome>;

    /// The expired-lease counterpart of
    /// [`Self::resolve_client_tool_call_and_append_event_with_rows`].
    #[allow(clippy::too_many_arguments)]
    async fn resolve_expired_client_tool_call_and_append_event_with_rows(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
        _rows: Option<&serde_json::Value>,
    ) -> Result<JournaledClientToolCallOutcome> {
        self.resolve_expired_client_tool_call_and_append_event(
            id,
            chat_id,
            lease_token,
            now,
            resolution,
            resolved_at,
        )
        .await
    }

    /// List unclaimed and claimed client work for authoritative recovery.
    async fn list_pending_client_tool_calls(&self, chat_id: ChatId) -> Result<Vec<ToolCallRecord>>;

    /// List only validated renderer-safe foreground question cards.
    async fn list_pending_user_questions(
        &self,
        _chat_id: ChatId,
    ) -> Result<Vec<PendingUserQuestions>> {
        turn_storage_unavailable()
    }

    /// List every conversation that has a renderer-owned prompt awaiting the
    /// user. The result carries opaque call ids only; callers fetch detail for
    /// an individual open conversation through its dedicated recovery route.
    async fn list_pending_chat_prompts(&self) -> Result<Vec<PendingChatPrompt>> {
        turn_storage_unavailable()
    }

    /// Atomically commit exact answers, complete the same tool call, and move
    /// its blocked turn to the shared resumable state.
    async fn answer_user_questions(
        &self,
        _request: &AnswerUserQuestionsRequest,
        _answered_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<AnswerUserQuestionsOutcome> {
        turn_storage_unavailable()
    }

    /// List only validated renderer-safe pending plan proposals.
    async fn list_pending_plan_approvals(
        &self,
        _chat_id: ChatId,
    ) -> Result<Vec<crate::PendingPlanApproval>> {
        turn_storage_unavailable()
    }

    /// Atomically commit one exact plan decision, complete the same tool
    /// call, move its blocked turn to the shared resumable state, and — on
    /// accept — move the chat out of plan mode.
    async fn decide_plan(
        &self,
        _request: &crate::DecidePlanRequest,
        _decided_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<DecidePlanOutcome> {
        turn_storage_unavailable()
    }

    /// List a chat's tool calls in creation order.
    async fn list_tool_calls(&self, chat_id: ChatId) -> Result<Vec<ToolCallRecord>>;

    /// Read a setting (profile, model prefs, approval policy), or `None`.
    async fn get_setting(&self, key: &str) -> Result<Option<Value>>;

    /// Write a setting.
    async fn set_setting(&self, key: &str, value: &Value) -> Result<()>;

    /// Append an event for the legacy direct-execution path.
    ///
    /// Sequence numbers are per-chat and monotonic (starting at 1). This method
    /// rejects a chat once it has any durable turn history; durable workers must
    /// use [`append_turn_event`](Self::append_turn_event) so stale attempts are
    /// fenced and ambiguous retries recover the original sequence.
    async fn append_event(&self, chat_id: ChatId, event: &AgentEvent) -> Result<i64>;

    /// Append a nonterminal event owned by an exact live turn attempt.
    ///
    /// `(lease_token, attempt_event_ordinal)` is the idempotency identity. An
    /// exact retry returns the original sequence even after lease loss; reusing
    /// it with different data is an error. A first append succeeds only while
    /// the matching attempt still owns a live running lease. Completed, failed,
    /// and cancelled events are reserved for atomic turn resolution.
    async fn append_turn_event(
        &self,
        _chat_id: ChatId,
        _turn_id: TurnId,
        _lease_token: uuid::Uuid,
        _attempt_event_ordinal: i32,
        _now: chrono::DateTime<chrono::Utc>,
        _event: &AgentEvent,
    ) -> Result<Option<i64>> {
        turn_storage_unavailable()
    }

    /// Recover a terminal event only when it was committed by this exact lease
    /// with the byte-equivalent payload.
    ///
    /// This distinguishes an ambiguous response after this worker's commit from
    /// a claim scanner or competing terminal resolution that reached the same
    /// status with a different immutable receipt. Returns `None` for any
    /// different terminal identity.
    async fn recover_exact_turn_terminal_event(
        &self,
        _turn_id: TurnId,
        _lease_token: uuid::Uuid,
        _event: &AgentEvent,
    ) -> Result<Option<SequencedEvent>> {
        turn_storage_unavailable()
    }

    /// Recover a completed turn only when its output, ordered citations, and
    /// terminal event match the exact request whose response was ambiguous.
    ///
    /// Stores without structured citation support retain the legacy recovery
    /// path for citation-free outputs. Citation-aware stores must override this
    /// method so a matching message identity cannot conceal different sources.
    async fn recover_exact_completed_turn_event(
        &self,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        _output: &Message,
        citations: &[crate::AssistantCitationInput],
        event: &AgentEvent,
    ) -> Result<Option<SequencedEvent>> {
        if citations.is_empty() {
            self.recover_exact_turn_terminal_event(turn_id, lease_token, event)
                .await
        } else {
            turn_storage_unavailable()
        }
    }

    /// List a chat's journaled events with `seq` greater than `after`, in
    /// sequence order. Pass `0` to replay from the start.
    async fn list_events(&self, chat_id: ChatId, after: i64) -> Result<Vec<SequencedEvent>>;

    // --- Durable reverse-RPC operation log (issue #858) ---
    //
    // These back the crash-safe `OperationStore` seam of
    // `openwave-sandbox-protocol`. The store persists an opaque
    // `(fingerprint, body)` pair keyed by `(run_id, operation_id)` and enforces
    // the commit predicate transactionally; the protocol tier owns the typed
    // meaning of those bytes and the mapping to `ClaimOutcome`. Retention and
    // body eviction are #859; `evict_operation` is that seam.

    /// Atomically claim `operation_id` under `run_id` for `fingerprint`, or
    /// observe its existing state, in a single transaction.
    ///
    /// `owner_epoch` identifies the claiming process lifetime: a `Claimed` entry
    /// found under a *different* epoch is the after-crash ambiguity
    /// ([`OperationClaimOutcome::ForeignClaim`]) for an `external_effect`
    /// operation; under the *same* epoch it is a concurrent duplicate
    /// ([`OperationClaimOutcome::OwnedClaim`]). A foreign `Claimed` with no
    /// external effect is safe to re-drive, so ownership is taken over and the
    /// claim reported [`OperationClaimOutcome::Fresh`].
    async fn claim_operation(
        &self,
        _run_id: uuid::Uuid,
        _operation_id: uuid::Uuid,
        _fingerprint: &[u8],
        _external_effect: bool,
        _owner_epoch: uuid::Uuid,
    ) -> Result<OperationClaimOutcome> {
        operation_log_storage_unavailable()
    }

    /// Settle a `Claimed` entry to `Recorded` with `body`, transactionally.
    /// Idempotent: a re-delivered record for an already-`Recorded` entry is
    /// acknowledged ([`OperationLogWrite::AlreadyTerminal`]) without overwriting
    /// the first-committed body.
    async fn record_operation(
        &self,
        _run_id: uuid::Uuid,
        _operation_id: uuid::Uuid,
        _body: &[u8],
    ) -> Result<OperationLogWrite> {
        operation_log_storage_unavailable()
    }

    /// Settle a `Claimed` entry to `Failed` with `body`, transactionally.
    /// Idempotent for an already-`Failed` entry.
    async fn fail_operation(
        &self,
        _run_id: uuid::Uuid,
        _operation_id: uuid::Uuid,
        _body: &[u8],
    ) -> Result<OperationLogWrite> {
        operation_log_storage_unavailable()
    }

    /// The current state of an operation-log entry, if the log knows it.
    async fn operation_state(
        &self,
        _run_id: uuid::Uuid,
        _operation_id: uuid::Uuid,
    ) -> Result<Option<OperationLogEntry>> {
        operation_log_storage_unavailable()
    }

    /// Drop an entry once the sandbox can no longer re-issue it. This slice
    /// removes the row; #859 owns *when* eviction is safe and may instead null
    /// the body and clear `retained` to leave a commit marker.
    async fn evict_operation(&self, _run_id: uuid::Uuid, _operation_id: uuid::Uuid) -> Result<()> {
        operation_log_storage_unavailable()
    }

    /// How many operation-log entries a run currently retains. For tests and,
    /// later, retention accounting.
    async fn operation_log_len(&self, _run_id: uuid::Uuid) -> Result<usize> {
        operation_log_storage_unavailable()
    }
}

/// Credential custody: secrets keyed by a stable reference string (e.g.
/// `provider.anthropic.credential`). Backed by the OS keychain on desktop, a
/// KMS/Vault on a server — never the [`Store`].
#[async_trait]
pub trait SecretProvider: Send + Sync {
    /// Fetch a secret by key, or `None` if unset.
    async fn get_secret(&self, key: &str) -> Result<Option<String>>;

    /// Store (or overwrite) a secret.
    async fn set_secret(&self, key: &str, value: &str) -> Result<()>;

    /// Remove a secret; a no-op if it doesn't exist.
    async fn delete_secret(&self, key: &str) -> Result<()>;
}

/// Metadata that can be read without materializing a blob's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobMetadata {
    /// Number of bytes in the immutable blob.
    pub byte_len: u64,
}

/// Opaque byte storage for documents, images, and exports.
#[async_trait]
pub trait BlobStore: Send + Sync {
    /// Publish immutable bytes under `id`.
    ///
    /// Repeating the same publication is a no-op; publishing different bytes
    /// under an existing id fails without changing the stored value. Callers
    /// allocate a new id when content changes.
    async fn put(&self, id: uuid::Uuid, bytes: Vec<u8>) -> Result<()>;

    /// Publish a content-addressed source from a stream of chunks.
    ///
    /// Filesystem-backed storage overrides this to write each chunk directly
    /// to its durable temporary file. Other implementations retain a correct
    /// fallback while they add their own streaming primitive.
    async fn put_stream(&self, source: DocumentSourceBlob, mut chunks: BlobStream) -> Result<()> {
        let mut bytes = Vec::new();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk?;
            let chunk_len = u64::try_from(chunk.len())
                .map_err(|_| AgentError::Store("blob chunk length exceeds u64".into()))?;
            let next_len = u64::try_from(bytes.len())
                .map_err(|_| AgentError::Store("blob length exceeds u64".into()))?
                .checked_add(chunk_len)
                .ok_or_else(|| AgentError::Store("blob length exceeds u64".into()))?;
            if next_len > source.byte_len {
                return Err(AgentError::Store(
                    "streamed blob exceeds its declared byte length".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if DocumentSourceBlob::from_bytes(&bytes) != source {
            return Err(AgentError::Store(
                "streamed blob does not match its declared digest".into(),
            ));
        }
        self.put(source.id, bytes).await
    }

    /// Fetch bytes by `id`, or `None` if absent.
    async fn get(&self, id: uuid::Uuid) -> Result<Option<Vec<u8>>>;

    /// Fetch a blob's length without reading its bytes.
    ///
    /// Backends should override this when their storage can obtain metadata
    /// independently. The compatibility implementation keeps existing custom
    /// stores correct while they adopt the bounded-read API.
    async fn metadata(&self, id: uuid::Uuid) -> Result<Option<BlobMetadata>> {
        self.get(id).await.map(|bytes| {
            bytes.map(|bytes| BlobMetadata {
                byte_len: u64::try_from(bytes.len()).expect("usize always fits in u64"),
            })
        })
    }

    /// Read the half-open byte `range` without materializing bytes outside it.
    ///
    /// A backend that cannot yet stream uses the compatibility implementation;
    /// production stores should override it so response ranges remain bounded.
    async fn read_range(&self, id: uuid::Uuid, range: Range<u64>) -> Result<Option<BlobStream>> {
        let Some(bytes) = self.get(id).await? else {
            return Ok(None);
        };
        let start = usize::try_from(range.start)
            .map_err(|_| AgentError::Store("blob range start exceeds usize".into()))?;
        let end = usize::try_from(range.end)
            .map_err(|_| AgentError::Store("blob range end exceeds usize".into()))?;
        let bytes = bytes
            .get(start..end)
            .ok_or_else(|| AgentError::Store("requested byte range is outside the blob".into()))?
            .to_vec();
        Ok(Some(stream::once(async move { Ok(bytes) }).boxed()))
    }

    /// Delete a blob synchronously; a no-op if it doesn't exist.
    ///
    /// Async callers must move this operation to a blocking executor. This
    /// boundary lets a lifecycle guard remain owned by the blocking operation
    /// even when its awaiting worker is cancelled.
    fn delete(&self, id: uuid::Uuid) -> Result<()>;
}

#[cfg(test)]
mod tests;
