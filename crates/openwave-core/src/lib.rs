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
pub mod agent_tools;
pub mod approval;
#[cfg(feature = "blob-fs")]
pub mod blob;
pub mod cancel;
pub mod citation;
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

pub use agent::{
    Agent, AgentConfig, AgentTurnOutcome, ClaimedAgentEvent, SandboxAgentSpawnRequest, ToolRegistry,
};
pub use agent_tools::{
    sandbox_web_search_tool_spec, spawn_sandbox_agent_tool_spec,
    validate_spawn_sandbox_agent_arguments, SpawnSandboxAgentArgs, MAX_SANDBOX_AGENT_TASK_CHARS,
    SANDBOX_WEB_SEARCH_TOOL, SPAWN_SANDBOX_AGENT_TOOL,
};
pub use approval::{
    ApprovalDecision, ApprovalFuture, ApprovalGate, ApprovalJournalIdentity, ApprovalRegistration,
    ApprovalRegistrationFuture, ApprovalRequest, ApprovalRequiredPublication, AutoApproveGate,
    RefuseGate, ToolApproval, ToolApprovalKind, ToolApprovalStatus,
};
#[cfg(feature = "blob-fs")]
pub use blob::FsBlobStore;
pub use cancel::{CancelToken, Cancelled};
pub use citation::{
    format_source_reference, parse_assistant_citations, AssistantCitationReference,
    AssistantCitationSnapshot, ParsedAssistantCitations, MAX_ASSISTANT_CITATIONS,
    MAX_CITATION_EXCERPT_CHARS, MAX_CITATION_HEADING_CHARS, MAX_CITATION_PAGES,
};
pub use client_tools::{
    list_connected_folders_tool_spec, list_folder_tool_spec, read_connected_file_tool_spec,
    request_folder_access_tool_spec, sandbox_folder_access_proposal_tool_spec,
    validate_list_connected_folders_arguments, validate_list_folder_arguments,
    validate_read_connected_file_arguments, validate_request_folder_access_arguments,
    ListConnectedFoldersArgs, ListFolderArgs, ReadConnectedFileArgs, RequestFolderAccessArgs,
    RequestFolderAccessResult, RequestedFolderCapability, RequestedFolderHint,
    LIST_CONNECTED_FOLDERS_TOOL, LIST_FOLDER_TOOL, MAX_CONNECTED_FOLDER_PATH_BYTES,
    MAX_FOLDER_ACCESS_REASON_CHARS, READ_CONNECTED_FILE_TOOL, REQUEST_FOLDER_ACCESS_TOOL,
};
pub use config::{Config, Profile};
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub use db::DbStore;
pub use error::{AgentError, AgentErrorInfo, Result};
pub use event::{AgentEvent, SequencedEvent};
pub use id::{
    AgentRunId, AssistantCitationId, CallId, ChatId, ChunkId, DocumentId, DocumentJobId,
    HostRootId, HostRootIdError, MessageId, ProjectId, RootAttachmentChangeId,
    RootAttachmentChangeIdError, StepId, TurnId, TurnSteerId,
};
#[cfg(feature = "keychain")]
pub use keychain::KeychainSecretProvider;
pub use model::{
    validate_source_regions, AgentRun, AgentRunCancellationReason, AgentRunCancellationSignal,
    AgentRunExecution, AgentRunInboxEntry, AgentRunInboxStatus, AgentRunResult,
    AgentRunResultPayload, AgentRunStatus, AgentRunWaitCondition, BeginRootAttachmentChange,
    BlobRetirement, BlobRetirementStatus, ByteSpan, Chat, ChatRootAttachment,
    ClientToolCallRequest, DocumentGeneration, DocumentJob, DocumentJobKind, DocumentJobStatus,
    DocumentListCursor, DocumentParseOutput, DocumentProcessingStatus, DocumentRecord,
    DocumentScope, DocumentSourceBlob, DocumentSourceUpsert, DocumentSummaryRecord, DocumentUpsert,
    Message, Project, RetrievalEvidence, RetrievalEvidenceInput, RetrievalEvidenceSource, Role,
    RootAttachmentChange, RootAttachmentChangeAction, RootAttachmentChangeFailure,
    RootAttachmentChangePhase, RootAttachmentChangeTerminal, RootAttachmentOrigin,
    RootAttachmentSubjectKind, SandboxAgentAdmission, SandboxToolCall, SandboxToolCallReceipt,
    SandboxToolCallRequest, SandboxToolCallStatus, SourceLocation, SourceRegion, ToolCallExecution,
    ToolCallRecord, ToolCallResolution, ToolCallStatus, TurnAgentRunWait, TurnAgentRunWaitSet,
    TurnAgentRunWaitStatus, TurnCheckpointProgress, TurnClientWait, TurnClientWaitStatus,
    TurnFailureReceipt, TurnFailureRetry, TurnRun, TurnRunStatus, TurnSteer, TurnSteerStatus,
    MAX_ATTACHMENT_REVISION, MAX_ROOT_ATTACHMENTS,
};
pub use provider::{
    ChatMessage, ChatRequest, ContentBlock, ModelProvider, ProviderEvent, ProviderId, StopReason,
    Usage,
};
pub use steer::{SteerInbox, SteerMessage};
pub use storage::{
    AcceptAgentRunOutcome, AcceptSandboxAgentRunAndParkTurnOutcome, AcceptToolCallOutcome,
    AcceptTurnOutcome, AcceptTurnSteerOutcome, AdmitSandboxAgentRunOutcome, ApplyTurnSteerOutcome,
    BeginRootAttachmentChangeOutcome, BlobStore, ChatToolActivitySnapshot, ChatToolActivityStatus,
    ChatTranscriptSnapshot, ClaimAgentRunInboxOutcome, ClaimClientToolCallOutcome,
    ClaimSandboxToolCallOutcome, ClaimScanTerminalEvent, ClaimTurnRunOutcome, ClientToolCallClaim,
    CompleteTurnRunOutcome, ConsumeAgentRunInboxAndResumeTurnOutcome, ConsumeAgentRunInboxOutcome,
    DecideToolApprovalOutcome, DeleteChatOutcome, DocumentIndexJobReason,
    EnsureDocumentIndexJobOutcome, EnsureDocumentParseJobOutcome, FailAgentRunOutcome,
    FinishAgentRunCancellationOutcome, FinishRootAttachmentChangeOutcome,
    FinishTurnCancellationOutcome, HeartbeatClientToolCallOutcome, JournaledClientToolCallOutcome,
    JournaledToolApprovalOutcome, JournaledTurnOutcome, JournaledTurnSteerOutcome,
    ParkSandboxToolCallOutcome, ParkTurnForAgentRunInboxOutcome, ParkTurnForAgentRunWaitSetOutcome,
    ParkTurnForClientCallOutcome, RecordTurnFailureOutcome, RequestAgentRunCancellationOutcome,
    RequestToolApprovalOutcome, RequestTurnCancellationOutcome, ResolveSandboxToolCallOutcome,
    ResolveToolCallOutcome, ResumeTurnForAgentRunWaitSetOutcome, SecretProvider, Store,
    SubmitAgentRunResultOutcome, MAX_PENDING_ROOT_ATTACHMENT_CHANGES,
};
pub use tool::{ApprovalClass, Tool, ToolCtx, ToolOutput, ToolScratch, ToolSpec};
#[cfg(feature = "tools")]
pub use tools::{ListDir, ReadFile, WriteFile};
