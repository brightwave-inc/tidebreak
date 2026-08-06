use futures::stream::BoxStream;
use serde::Serialize;

use crate::approval::ToolApproval;
use crate::error::Result;
use crate::event::SequencedEvent;
use crate::id::{AgentRunId, CallId, ChatId, MessageId, TurnId};
use crate::model::{
    AgentRun, AgentRunInboxEntry, AgentRunResult, Message, MessageAttachment,
    MessageDocumentAttachment, RootAttachmentChange, ToolCallRecord, TurnAgentRunWaitSet,
    TurnClientWait, TurnFailureReceipt, TurnRun, TurnSteer,
};
use crate::provider::RefusalOutcome;

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
    /// Provider-qualified model selection captured when the turn was accepted.
    pub model: String,
    /// Skills the user explicitly invoked when the turn was accepted, in
    /// submitted order. Empty for a turn that named none.
    pub invoked_skills: Vec<String>,
    /// Token accounting recorded when the turn resolved. Lets a client that
    /// opened the chat after the fact show context usage without waiting for
    /// another turn to run.
    pub usage: crate::provider::Usage,
    /// Whether any of the turn's input came from voice transcription.
    pub voice_input_used: bool,
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

/// What kind of decision one inbox item is waiting for.
///
/// The set is closed and each variant names an existing park/resume surface,
/// so a reader can route an item back to the card that owns it without the
/// inbox knowing anything about that card's contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum InboxItemKind {
    /// A Sensitive tool call parked on its approval gate.
    ToolApproval,
    /// An `ask_user_questions` continuation awaiting answers.
    Question,
    /// An `exit_plan_mode` proposal awaiting a decision.
    PlanReview,
    /// A folder-access request awaiting a native grant.
    FolderAccess,
    /// A write-back to a connected folder awaiting confirmation.
    OutputWriteback,
}

/// One thing waiting on the reader, wherever in their chats it parked.
///
/// A projection of the journal rows that already carry park/resume state —
/// deliberately not a store of its own, so resolving an item through its
/// existing route is what removes it from here. It stays as opaque as the
/// per-chat attention summary: identity, kind, and when it parked, never
/// question text, plan prose, or canonical tool arguments. Detail comes from
/// the chat the item deep-links to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxItem {
    pub chat_id: ChatId,
    /// The conversation's title, or `None` while it is still untitled.
    pub chat_title: Option<String>,
    pub turn_id: TurnId,
    /// The parked call — both the item's identity and the transcript position
    /// a deep link returns to.
    pub call_id: CallId,
    pub kind: InboxItemKind,
    /// The tool whose call parked, for a tool approval. `None` for every other
    /// kind, whose tool is implied by the kind itself.
    pub tool_name: Option<String>,
    /// When the item started waiting.
    pub requested_at: chrono::DateTime<chrono::Utc>,
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
    /// The chat already has the configured number of active background runs.
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
        /// The step's calls, in the order the model emitted them.
        calls: Vec<crate::model::SandboxToolCall>,
    },
    Existing {
        run: AgentRun,
        calls: Vec<crate::model::SandboxToolCall>,
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

/// Whether one sandbox-resident run may keep working while its host is
/// absent — a durable decision made at admission, never revisited by a
/// disconnect.
///
/// Fail closed: the default everywhere is [`AttachedOnly`], a record written
/// before this field existed reads as [`AttachedOnly`], and `Detached` can
/// only be recorded by an admission gate whose preconditions all held. The
/// wire-facing `AdmissionMode` a sandbox receives in its run init is derived
/// from this record, never from code.
///
/// [`AttachedOnly`]: SandboxAdmissionMode::AttachedOnly
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SandboxAdmissionMode {
    /// The default: the run must not work while unattached; it checkpoints
    /// and waits for its host.
    #[default]
    AttachedOnly,
    /// The run was admitted to keep working through host absence, within its
    /// bounds.
    Detached,
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
    /// The run's durable admission decision. Reconciliation and reattachment
    /// derive the sandbox's admission mode from this field, so a crash can
    /// never upgrade a run to detached.
    pub admission: SandboxAdmissionMode,
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
