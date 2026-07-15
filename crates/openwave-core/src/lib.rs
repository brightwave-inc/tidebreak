//! OpenWave core — the open-core seam every client sits on.
//!
//! Holds the agent loop, tool registry, the `AgentEvent` stream, and the trait
//! contracts (`Tool`, `ModelProvider`, `Store`, `BlobStore`, `SecretProvider`)
//! plus their default local impls. Concrete model-provider adapters live in
//! `openwave-router`, not here.
//!
//! This crate must never depend on a specific client, and is independently
//! publishable on crates.io.
//!
//! Built incrementally with the M0 walking skeleton. Present so far: [`id`]
//! (typed identifiers), [`error`], [`model`] (the persisted chat/message
//! model), [`tool`] (the tool contract), and [`provider`] (the model-provider
//! contract).

pub mod agent;
pub mod approval;
#[cfg(feature = "blob-fs")]
pub mod blob;
pub mod cancel;
pub mod client_tools;
pub mod config;
pub mod context;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub mod db;
pub mod error;
pub mod event;
pub mod id;
#[cfg(feature = "keychain")]
pub mod keychain;
pub mod model;
pub mod provider;
pub mod steer;
pub mod storage;
pub mod tool;
#[cfg(feature = "tools")]
pub mod tools;

pub use agent::{Agent, AgentConfig, AgentTurnOutcome, ClaimedAgentEvent, ToolRegistry};
pub use approval::{
    ApprovalDecision, ApprovalFuture, ApprovalGate, ApprovalRequest, AutoApproveGate, RefuseGate,
};
#[cfg(feature = "blob-fs")]
pub use blob::FsBlobStore;
pub use cancel::{CancelToken, Cancelled};
pub use client_tools::{
    request_folder_access_tool_spec, validate_request_folder_access_arguments,
    RequestFolderAccessArgs, RequestFolderAccessResult, RequestedFolderCapability,
    RequestedFolderHint, MAX_FOLDER_ACCESS_REASON_CHARS, REQUEST_FOLDER_ACCESS_TOOL,
};
pub use config::{Config, Profile};
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub use db::DbStore;
pub use error::{AgentError, AgentErrorInfo, Result};
pub use event::{AgentEvent, SequencedEvent};
pub use id::{
    AgentRunId, CallId, ChatId, DocumentId, DocumentJobId, HostRootId, HostRootIdError, MessageId,
    ProjectId, RootAttachmentChangeId, RootAttachmentChangeIdError, StepId, TurnId, TurnSteerId,
};
#[cfg(feature = "keychain")]
pub use keychain::KeychainSecretProvider;
pub use model::{
    validate_source_regions, AgentRun, AgentRunExecution, AgentRunInboxEntry, AgentRunInboxStatus,
    AgentRunResult, AgentRunStatus, BeginRootAttachmentChange, BlobRetirement,
    BlobRetirementStatus, ByteSpan, Chat, ChatRootAttachment, ClientToolCallRequest,
    DocumentGeneration, DocumentJob, DocumentJobKind, DocumentJobStatus, DocumentListCursor,
    DocumentParseOutput, DocumentProcessingStatus, DocumentRecord, DocumentScope,
    DocumentSourceBlob, DocumentSourceUpsert, DocumentSummaryRecord, DocumentUpsert, Message,
    Project, Role, RootAttachmentChange, RootAttachmentChangeAction, RootAttachmentChangeFailure,
    RootAttachmentChangePhase, RootAttachmentChangeTerminal, RootAttachmentOrigin,
    RootAttachmentSubjectKind, SourceLocation, SourceRegion, ToolCallExecution, ToolCallRecord,
    ToolCallResolution, ToolCallStatus, TurnAgentRunWait, TurnAgentRunWaitStatus,
    TurnCheckpointProgress, TurnClientWait, TurnClientWaitStatus, TurnFailureReceipt,
    TurnFailureRetry, TurnRun, TurnRunStatus, TurnSteer, TurnSteerStatus, MAX_ATTACHMENT_REVISION,
    MAX_ROOT_ATTACHMENTS,
};
pub use provider::{
    ChatMessage, ChatRequest, ContentBlock, ModelProvider, ProviderEvent, ProviderId, StopReason,
    Usage,
};
pub use steer::{SteerInbox, SteerMessage};
pub use storage::{
    AcceptAgentRunOutcome, AcceptSandboxAgentRunAndParkTurnOutcome, AcceptToolCallOutcome,
    AcceptTurnOutcome, AcceptTurnSteerOutcome, ApplyTurnSteerOutcome,
    BeginRootAttachmentChangeOutcome, BlobStore, ClaimAgentRunInboxOutcome,
    ClaimClientToolCallOutcome, ClaimScanTerminalEvent, ClaimTurnRunOutcome, ClientToolCallClaim,
    CompleteTurnRunOutcome, ConsumeAgentRunInboxAndResumeTurnOutcome, ConsumeAgentRunInboxOutcome,
    DocumentIndexJobReason, EnsureDocumentIndexJobOutcome, EnsureDocumentParseJobOutcome,
    FinishAgentRunCancellationOutcome, FinishRootAttachmentChangeOutcome,
    FinishTurnCancellationOutcome, HeartbeatClientToolCallOutcome, JournaledClientToolCallOutcome,
    JournaledTurnOutcome, JournaledTurnSteerOutcome, ParkTurnForAgentRunInboxOutcome,
    ParkTurnForClientCallOutcome, RecordTurnFailureOutcome, RequestAgentRunCancellationOutcome,
    RequestTurnCancellationOutcome, ResolveToolCallOutcome, SecretProvider, Store,
    SubmitAgentRunResultOutcome, MAX_PENDING_ROOT_ATTACHMENT_CHANGES,
};
pub use tool::{ApprovalClass, Tool, ToolCtx, ToolOutput, ToolScratch, ToolSpec};
#[cfg(feature = "tools")]
pub use tools::{ListDir, ReadFile, WriteFile};
