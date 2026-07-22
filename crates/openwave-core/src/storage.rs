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
use serde::Serialize;
use serde_json::Value;

use crate::approval::{ApprovalDecision, ApprovalRequest, ToolApproval};
use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::{
    AgentRunId, CallId, ChatId, DocumentId, DocumentJobId, ProjectId, RootAttachmentChangeId,
    TurnId, TurnSteerId,
};
use crate::model::{
    AgentRun, AgentRunExecution, AgentRunInboxEntry, AgentRunResult, AgentRunWaitSetCandidate,
    BeginRootAttachmentChange, BlobRetirement, BlobRetirementStatus, Chat, ClientToolCallRequest,
    DocumentGeneration, DocumentJob, DocumentJobKind, DocumentJobStatus, DocumentListCursor,
    DocumentParseOutput, DocumentRecord, DocumentScope, DocumentSourceUpsert,
    DocumentSummaryRecord, DocumentUpsert, Message, Project, RootAttachmentChange,
    RootAttachmentChangeTerminal, ToolCallRecord, ToolCallResolution, TurnAgentRunWait,
    TurnAgentRunWaitSet, TurnCheckpointProgress, TurnClientWait, TurnFailureReceipt,
    TurnFailureRetry, TurnRun, TurnSteer,
};
use crate::provider::{StopReason, Usage};

/// Largest pending attachment-reconciliation page accepted by [`Store`].
pub const MAX_PENDING_ROOT_ATTACHMENT_CHANGES: u64 = 256;

/// A mutually consistent conversation transcript and event-journal cursor.
///
/// The cursor is captured under the same per-chat fence as `messages`, so a
/// renderer can hydrate durable text and then subscribe after this cursor
/// without dropping an event committed during the handoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatTranscriptSnapshot {
    pub messages: Vec<Message>,
    /// Ordered renderer-safe sources keyed to their assistant message.
    pub citations: Vec<crate::AssistantCitationSnapshot>,
    /// A renderer-safe historical projection. It contains fixed titles and
    /// lifecycle timestamps only; canonical tool records never leave storage.
    pub tool_activity: Vec<ChatToolActivitySnapshot>,
    pub last_event_seq: i64,
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

/// Fixed lifecycle vocabulary exposed for a historical tool card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatToolActivityStatus {
    Completed,
    Failed,
    Cancelled,
}

/// A completed tool invocation with no arguments, results, tool identity,
/// provider metadata, executor identity, lease, or diagnostic detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatToolActivitySnapshot {
    /// Fixed allowlisted presentation title, never a provider-supplied name.
    pub title: &'static str,
    pub status: ChatToolActivityStatus,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Why maintenance determined that a document needs an index job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentIndexJobReason {
    /// The operational watermark is current, but the derived generation is absent.
    DerivedStateMissing,
    /// The desired generation exists only partially and cannot be safely reused.
    DerivedStateIncomplete,
    /// The configured chunking/embedding pipeline differs from the indexed one.
    PipelineChanged,
}

impl DocumentIndexJobReason {
    /// Whether this repair must publish under a fresh generation fence.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) const fn advances_generation(self) -> bool {
        matches!(self, Self::DerivedStateIncomplete | Self::PipelineChanged)
    }
}

/// Result of atomically ensuring one desired document index job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureDocumentIndexJobOutcome {
    /// A new job was inserted or an exact terminal job was reset to queued.
    Enqueued(DocumentJob),
    /// The desired current job already exists and requires no state change.
    Existing(DocumentJob),
    /// The desired current job failed and requires an explicit user retry.
    Failed(DocumentJob),
    /// Canonical content is still owned by the current parse stage.
    Parsing(DocumentJob),
    /// The source document no longer exists.
    MissingDocument,
    /// The caller inspected an obsolete source generation.
    GenerationChanged(DocumentGeneration),
}

/// Result of atomically ensuring canonical output from one parser pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureDocumentParseJobOutcome {
    /// A new Parse job was inserted for the desired generation.
    Enqueued(DocumentJob),
    /// The desired current Parse job already exists and remains live.
    Existing(DocumentJob),
    /// The desired current Parse job exhausted its attempts.
    Failed(DocumentJob),
    /// Canonical output already came from the desired parser.
    CanonicalCurrent,
    /// Reparse was requested for a document without retained source bytes.
    SourceUnavailable,
    /// The source document no longer exists.
    MissingDocument,
    /// The caller inspected an obsolete source generation.
    GenerationChanged(DocumentGeneration),
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

/// Result of registering one exact durable approval request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestToolApprovalOutcome {
    /// This call entered the pending approval state.
    Requested(ToolApproval),
    /// An exact retry recovered the same pending or decided request.
    Existing(ToolApproval),
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
    },
    /// An exact retry recovered the previously committed checkpoint.
    Existing {
        turn: TurnRun,
        call: ToolCallRecord,
        wait: TurnClientWait,
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
    /// `project_id`, when present, must identify an existing project. The default
    /// database store enforces this with a restricting foreign key, so projects
    /// cannot be deleted while they still own documents. The store replaces the
    /// supplied `revision_token` with a fresh token so a deleted document
    /// lifecycle cannot be recreated with stale identity. A live document's
    /// ownership is immutable: callers must delete it before recreating the same
    /// id in another corpus.
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
    /// order. Implementations must not load canonical text or revision tokens.
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

    /// Read the newest durable generation, including a hard-delete tombstone.
    async fn get_document_generation(&self, _id: DocumentId) -> Result<Option<DocumentGeneration>> {
        document_storage_unavailable()
    }

    /// List durable tombstone watermarks whose retrieval retirement is unfinished.
    ///
    /// Results are ordered by document id, strictly after `after` when present,
    /// and bounded by `limit`. A worker can advance past a poison entry and wrap
    /// to the beginning by issuing a later scan with `after = None`.
    async fn list_pending_document_retirements(
        &self,
        _after: Option<DocumentId>,
        _limit: u64,
    ) -> Result<Vec<(DocumentId, DocumentGeneration)>> {
        document_storage_unavailable()
    }

    /// Read the exact retirement watermark currently pending for one document.
    async fn get_pending_document_retirement(
        &self,
        _id: DocumentId,
    ) -> Result<Option<DocumentGeneration>> {
        document_storage_unavailable()
    }

    /// Mark one exact tombstone generation's retrieval retirement complete.
    ///
    /// Returns `false` when that exact generation is no longer pending. A live
    /// recreation may coexist with an older pending retirement watermark.
    async fn complete_document_retirement(
        &self,
        _id: DocumentId,
        _generation: DocumentGeneration,
    ) -> Result<bool> {
        document_storage_unavailable()
    }

    /// Hard-delete source content and return its durable tombstone generation.
    ///
    /// The generation clock is retained without source content. Repeated deletion
    /// returns the same tombstone; deleting a never-seen id creates revision one.
    async fn delete_document(&self, _id: DocumentId) -> Result<DocumentGeneration> {
        document_storage_unavailable()
    }

    /// Create or replace authoritative document content.
    ///
    /// A never-seen id starts at revision one. Replacing or recreating an id
    /// increments its retained generation atomically, preserves `created_at` only
    /// for a live replacement, and clears the index watermark. `project_id`, when
    /// present, must identify an existing project. A live document cannot move
    /// between the unscoped and project corpora, or between projects; direct
    /// upserts enforce the same ownership rules as the enqueueing write path.
    async fn upsert_document(&self, _document: &DocumentUpsert) -> Result<DocumentRecord> {
        document_storage_unavailable()
    }

    /// Atomically persist a new source revision and enqueue its index job.
    ///
    /// Any older nonterminal job for the document is cancelled in the same
    /// transaction. The returned job is bound to the returned record's exact
    /// `(content_revision, revision_token)` identity. Repeating identical source
    /// content and pipeline fingerprint returns that exact revision/job without
    /// allocating another, including its original `max_attempts` and terminal
    /// status. Intentional reprocessing/retry is an explicit job-state transition,
    /// not another source write. `DocumentUpsert::updated_at` is source metadata
    /// and is deliberately excluded from retry identity. Workflow timestamps are
    /// owned by the store rather than copied from source metadata. `project_id`
    /// must identify an existing project when present, and ownership of a live
    /// document is immutable until that document is deleted.
    async fn upsert_document_and_enqueue_index(
        &self,
        _document: &DocumentUpsert,
        _pipeline_fingerprint: &str,
        _max_attempts: i32,
    ) -> Result<(DocumentRecord, DocumentJob)> {
        document_storage_unavailable()
    }

    /// Atomically accept immutable raw source bytes and enqueue their parse job.
    ///
    /// The blob must already be durably published. Repeating an identical source
    /// and parser fingerprint returns the exact existing generation and job.
    /// Any source or parser change advances the generation, clears canonical and
    /// index state, and cancels older nonterminal work in the same transaction.
    async fn accept_document_source_and_enqueue_parse(
        &self,
        _document: &DocumentSourceUpsert,
        _parser_fingerprint: &str,
        _max_attempts: i32,
    ) -> Result<(DocumentRecord, DocumentJob)> {
        document_storage_unavailable()
    }

    /// Atomically publish canonical parser output and enqueue the index stage.
    ///
    /// The transition succeeds only for the exact live, unexpired parse lease.
    /// On success the parse job becomes terminal, canonical state becomes
    /// authoritative, and one index job is queued in the same transaction.
    async fn complete_document_parse_job_and_enqueue_index(
        &self,
        _id: DocumentJobId,
        _lease_token: uuid::Uuid,
        _completed_at: chrono::DateTime<chrono::Utc>,
        _output: &DocumentParseOutput,
        _index_fingerprint: &str,
        _index_max_attempts: i32,
    ) -> Result<Option<(DocumentRecord, DocumentJob)>> {
        document_storage_unavailable()
    }

    /// Atomically establish the current index job requested by an auditor.
    ///
    /// `expected_generation` is a compare-and-swap fence around the auditor's
    /// observation. Missing derived state requeues the exact current generation;
    /// incomplete derived state and a changed pipeline advance the source
    /// generation once without changing source fields. Failed jobs remain failed
    /// until an explicit retry.
    async fn ensure_document_index_job(
        &self,
        _document_id: DocumentId,
        _expected_generation: DocumentGeneration,
        _pipeline_fingerprint: &str,
        _max_attempts: i32,
        _reason: DocumentIndexJobReason,
    ) -> Result<EnsureDocumentIndexJobOutcome> {
        document_storage_unavailable()
    }

    /// Atomically establish the desired Parse job for retained source bytes.
    ///
    /// The caller's observed generation is a compare-and-swap fence. Missing
    /// work for pending canonical output is repaired in that generation; a
    /// parser change advances the generation once, clears derived canonical and
    /// index state, and enqueues Parse without changing retained source fields.
    /// Failed work remains failed until an explicit retry.
    async fn ensure_document_parse_job(
        &self,
        _document_id: DocumentId,
        _expected_generation: DocumentGeneration,
        _pipeline_fingerprint: &str,
        _max_attempts: i32,
    ) -> Result<EnsureDocumentParseJobOutcome> {
        document_storage_unavailable()
    }

    /// Fetch one durable document job by id.
    async fn get_document_job(&self, _id: DocumentJobId) -> Result<Option<DocumentJob>> {
        document_storage_unavailable()
    }

    /// List a document's semantic job history, oldest first.
    async fn list_document_jobs(&self, _document_id: DocumentId) -> Result<Vec<DocumentJob>> {
        document_storage_unavailable()
    }

    /// Explicitly retry one exact-generation failed semantic job.
    ///
    /// A matching failed job is reset to a fresh queued delivery using a
    /// store-owned timestamp and `max_attempts`. Repeating the request while that
    /// exact job is already nonterminal returns it unchanged. The observed
    /// generation, semantic kind, fingerprint, and document stage must all still
    /// agree. Succeeded, cancelled, superseded, missing, or mismatched jobs are
    /// not revived and return `None`.
    async fn retry_document_job(
        &self,
        _document_id: DocumentId,
        _expected_generation: DocumentGeneration,
        _kind: DocumentJobKind,
        _pipeline_fingerprint: &str,
        _max_attempts: i32,
    ) -> Result<Option<DocumentJob>> {
        document_storage_unavailable()
    }

    /// Atomically claim the oldest due document job and its exact source revision.
    ///
    /// A successful claim increments `attempt_count`, installs a fresh lease,
    /// moves the matching document to `processing`, and returns the running job.
    /// Expired running leases are reclaimed while attempts remain; an expired
    /// final attempt atomically fails the exact current job/document and scanning
    /// continues. Superseded candidates are terminally cancelled rather than
    /// left to block the active-job slot. An exact-identity document with an
    /// impossible lifecycle status is reported as corruption, never cancelled.
    /// `retry_wait` remains user-visible as `queued` during backoff.
    async fn claim_document_job(
        &self,
        _now: chrono::DateTime<chrono::Utc>,
        _lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<DocumentJob>> {
        document_storage_unavailable()
    }

    /// Extend a live lease owned by `lease_token` without resurrecting expiry.
    ///
    /// Returns `false` if the job is not running, the token differs, the lease
    /// already expired, or the proposed expiry does not extend the current one.
    async fn heartbeat_document_job(
        &self,
        _id: DocumentJobId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
        _lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        document_storage_unavailable()
    }

    /// Atomically succeed a live index job and publish its exact document
    /// revision as ready in the operational store.
    ///
    /// Returns `false` when the job is no longer running under the exact,
    /// unexpired lease or its timestamp would regress durable state.
    async fn complete_document_index_job(
        &self,
        _id: DocumentJobId,
        _lease_token: uuid::Uuid,
        _completed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        document_storage_unavailable()
    }

    /// Atomically record a live job failure and its matching document state.
    ///
    /// A future `retry_at` moves a job with attempts remaining to `retry_wait`
    /// and its document to `queued`; no retry time, or an exhausted attempt
    /// budget, moves both to terminal `failed`. Returns the resulting job status,
    /// or `None` when the exact live lease no longer owns the job.
    async fn record_document_job_failure(
        &self,
        _id: DocumentJobId,
        _lease_token: uuid::Uuid,
        _failed_at: chrono::DateTime<chrono::Utc>,
        _retry_at: Option<chrono::DateTime<chrono::Utc>>,
        _error_code: &str,
        _error_detail: Option<&str>,
    ) -> Result<Option<DocumentJobStatus>> {
        document_storage_unavailable()
    }

    /// Mark an exact `(revision, revision_token)` as indexed with `fingerprint`.
    ///
    /// `fingerprint` must not be empty.
    ///
    /// Returns `false` without modifying the row when the document is missing,
    /// the lifecycle token differs, or a newer content revision won the race.
    async fn mark_document_indexed(
        &self,
        _id: DocumentId,
        _revision: i64,
        _revision_token: uuid::Uuid,
        _fingerprint: &str,
        _indexed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        document_storage_unavailable()
    }

    /// Clear the index watermark for an exact content revision and lifecycle.
    ///
    /// Returns `false` without modifying the row when the document or exact
    /// revision identity is no longer current.
    async fn clear_document_index(
        &self,
        _id: DocumentId,
        _revision: i64,
        _revision_token: uuid::Uuid,
    ) -> Result<bool> {
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
    /// flow; deletion never guesses at native broker state.
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

    /// Atomically update whichever user-editable chat metadata fields are
    /// present. An outer `None` leaves that field alone; an inner `None`
    /// clears it. Returns `false` if the chat does not exist.
    async fn update_chat_metadata(
        &self,
        id: ChatId,
        title: Option<Option<String>>,
        model: Option<Option<String>>,
    ) -> Result<bool>;

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
        _execution: AgentRunExecution,
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
        _id: TurnId,
        _chat_id: ChatId,
        _model: &str,
        _content: &str,
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
        _preceding_citations: &[crate::AssistantCitationReference],
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
        citations: &[crate::AssistantCitationReference],
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
    async fn finish_turn_cancellation_and_append_event(
        &self,
        _id: TurnId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
        _usage: Usage,
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
        references: &[crate::AssistantCitationReference],
    ) -> Result<()> {
        if references.is_empty() {
            self.append_message(message).await
        } else {
            Err(AgentError::Store(
                "assistant citation storage is not implemented by this Store".into(),
            ))
        }
    }

    /// List a chat's messages in creation order.
    async fn list_messages(&self, chat_id: ChatId) -> Result<Vec<Message>>;

    /// Accept immutable canonical tool-call identity and arguments exactly once.
    async fn accept_tool_call(&self, call: &ToolCallRecord) -> Result<AcceptToolCallOutcome>;

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

    /// Read private approval state for exact recovery.
    async fn get_tool_call_approval(&self, _call_id: CallId) -> Result<Option<ToolApproval>> {
        Err(AgentError::Store(
            "durable tool approval storage is not implemented by this Store".into(),
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

    /// Resolve one server search call and atomically retain its private evidence.
    async fn resolve_server_tool_call_with_evidence(
        &self,
        id: CallId,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
        evidence: &[crate::RetrievalEvidenceInput],
    ) -> Result<ResolveToolCallOutcome> {
        if !evidence.is_empty() {
            return Err(AgentError::Store(
                "retrieval evidence persistence is unavailable".into(),
            ));
        }
        self.resolve_server_tool_call(id, resolution, resolved_at)
            .await
    }

    /// Read private evidence for trusted server-side citation assembly.
    async fn list_retrieval_evidence(&self, _id: CallId) -> Result<Vec<crate::RetrievalEvidence>> {
        Err(AgentError::Store(
            "retrieval evidence persistence is unavailable".into(),
        ))
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

    /// List unclaimed and claimed client work for authoritative recovery.
    async fn list_pending_client_tool_calls(&self, chat_id: ChatId) -> Result<Vec<ToolCallRecord>>;

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
        citations: &[crate::AssistantCitationReference],
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

/// Opaque byte storage for documents, images, and exports.
#[async_trait]
pub trait BlobStore: Send + Sync {
    /// Publish immutable bytes under `id`.
    ///
    /// Repeating the same publication is a no-op; publishing different bytes
    /// under an existing id fails without changing the stored value. Callers
    /// allocate a new id when content changes.
    async fn put(&self, id: uuid::Uuid, bytes: Vec<u8>) -> Result<()>;

    /// Fetch bytes by `id`, or `None` if absent.
    async fn get(&self, id: uuid::Uuid) -> Result<Option<Vec<u8>>>;

    /// Delete a blob synchronously; a no-op if it doesn't exist.
    ///
    /// Async callers must move this operation to a blocking executor. This
    /// boundary lets a lifecycle guard remain owned by the blocking operation
    /// even when its awaiting worker is cancelled.
    fn delete(&self, id: uuid::Uuid) -> Result<()>;
}

#[cfg(test)]
mod tests;
