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

/// Version of the OpenWave product containing this crate.
///
/// Published desktop builds inject the version selected by their `vX.Y.Z` Git
/// tag. Ordinary development and independently published crates fall back to
/// Cargo package metadata.
pub const VERSION: &str = match option_env!("OPENWAVE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

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
pub mod deliverable;
pub mod error;
pub mod event;
pub mod id;
pub mod image;
#[cfg(feature = "keychain")]
pub mod keychain;
pub mod model;
pub mod preview;
pub mod provider;
mod renderer_tool;
pub mod semantic_checkpoint;
pub mod steer;
pub mod storage;
pub mod tool;
#[cfg(feature = "tools")]
pub mod tools;
pub mod user_questions;

pub use agent::{
    Agent, AgentConfig, AgentTurnOutcome, ClaimedAgentEvent, ForegroundAgentWaitRequest,
    SandboxAgentSpawnRequest, ToolRegistry,
};
pub use agent_tools::{
    sandbox_read_delegated_file_tool_spec, sandbox_web_search_tool_spec,
    spawn_sandbox_agent_tool_spec, validate_sandbox_read_delegated_file_arguments,
    validate_spawn_sandbox_agent_arguments, validate_wait_for_agents_arguments,
    wait_for_agents_tool_spec, web_search_tool_spec, SandboxAgentFileResource,
    SpawnSandboxAgentArgs, SpawnSandboxAgentResult, WaitForAgentResult, WaitForAgentsArgs,
    WaitForAgentsResult, WebSearchArgs, DEFAULT_WEB_SEARCH_RESULTS, MAX_SANDBOX_AGENT_TASK_CHARS,
    MAX_WAIT_FOR_AGENTS_CHILDREN, MAX_WEB_SEARCH_DOMAINS, MAX_WEB_SEARCH_QUERY_CHARS,
    MAX_WEB_SEARCH_RESULTS, SANDBOX_READ_DELEGATED_FILE_TOOL, SANDBOX_WEB_SEARCH_TOOL,
    SPAWN_SANDBOX_AGENT_TOOL, WAIT_FOR_AGENTS_TOOL, WEB_SEARCH_TOOL,
};
pub use approval::{
    ApprovalDecision, ApprovalFuture, ApprovalGate, ApprovalJournalIdentity, ApprovalRegistration,
    ApprovalRegistrationFuture, ApprovalRequest, ApprovalRequiredPublication, AutoApproveGate,
    GrantScope, RefuseGate, StandingGrant, StandingGrants, ToolApproval, ToolApprovalKind,
    ToolApprovalStatus,
};
#[cfg(feature = "blob-fs")]
pub use blob::FsBlobStore;
pub use cancel::{CancelToken, Cancelled};
pub use citation::{
    format_source_reference, parse_assistant_citations, AssistantCitationReference,
    AssistantCitationSnapshot, CitationSpan, ParsedAssistantCitations, MAX_ASSISTANT_CITATIONS,
    MAX_CITATION_EXCERPT_CHARS, MAX_CITATION_HEADING_CHARS, MAX_CITATION_PAGES,
};
pub use client_tools::{
    import_connected_file_tool_spec, list_connected_folders_tool_spec, list_folder_tool_spec,
    read_connected_file_tool_spec, request_folder_access_tool_spec,
    sandbox_folder_access_proposal_tool_spec, validate_import_connected_file_arguments,
    validate_list_connected_folders_arguments, validate_list_folder_arguments,
    validate_read_connected_file_arguments, validate_request_folder_access_arguments,
    ImportConnectedFileArgs, ImportConnectedFileResult, ListConnectedFoldersArgs, ListFolderArgs,
    ReadConnectedFileArgs, RequestFolderAccessArgs, RequestFolderAccessResult,
    RequestedFolderCapability, RequestedFolderHint, IMPORT_CONNECTED_FILE_TOOL,
    LIST_CONNECTED_FOLDERS_TOOL, LIST_FOLDER_TOOL, MAX_CONNECTED_FOLDER_PATH_BYTES,
    MAX_FOLDER_ACCESS_REASON_CHARS, READ_CONNECTED_FILE_TOOL, REQUEST_FOLDER_ACCESS_TOOL,
};
pub use config::{Config, Profile};
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub use db::DbStore;
pub use deliverable::{
    deliverable_media_type, output_revision_relative_path, validate_deliverable_name, CreateOutput,
    NewOutputRevision, OutputCitationSnapshot, OutputRecord, OutputRevision,
    DELIVERABLES_DIRECTORY, MAX_DELIVERABLE_BYTES, MAX_DELIVERABLE_NAME_CHARS,
    MAX_OUTPUT_CITATIONS, MAX_OUTPUT_REVISIONS, OUTPUTS_DIRECTORY,
};
pub use error::{AgentError, AgentErrorInfo, Result};
pub use event::{AgentEvent, SequencedEvent};
pub use id::{
    AgentRunId, AssistantCitationId, CallId, ChatId, ChunkId, DocumentId, DocumentJobId,
    HostRootId, HostRootIdError, MessageId, OutputCitationId, OutputId, OutputRevisionId,
    ProjectId, RootAttachmentChangeId, RootAttachmentChangeIdError, StepId, TurnId, TurnSteerId,
};
pub use image::{
    ImageAttachments, ImageData, ImageMediaType, ImageRef, MAX_IMAGE_BYTES, MAX_IMAGE_DIMENSION,
};
#[cfg(feature = "keychain")]
pub use keychain::KeychainSecretProvider;
pub use model::{
    validate_source_regions, AgentRun, AgentRunCancellationReason, AgentRunCancellationSignal,
    AgentRunExecution, AgentRunInboxEntry, AgentRunInboxStatus, AgentRunResult,
    AgentRunResultPayload, AgentRunStatus, AgentRunWaitCondition, AgentRunWaitSetCandidate,
    AgentRunWaitSetCheckpointRequest, BeginRootAttachmentChange, BlobRetirement,
    BlobRetirementStatus, ByteSpan, Chat, ChatRootAttachment, ClientToolCallRequest,
    DelegatedFileReadClaim, DocumentGeneration, DocumentJob, DocumentJobKind, DocumentJobStatus,
    DocumentListCursor, DocumentParseOutput, DocumentProcessingStatus, DocumentRecord,
    DocumentScope, DocumentSourceBlob, DocumentSourceUpsert, DocumentSummaryRecord, DocumentUpsert,
    Message, MessageAttachment, Project, ReasoningEffort, RetrievalEvidence,
    RetrievalEvidenceInput, RetrievalEvidenceSource, Role, RootAttachmentChange,
    RootAttachmentChangeAction, RootAttachmentChangeFailure, RootAttachmentChangePhase,
    RootAttachmentChangeTerminal, RootAttachmentOrigin, RootAttachmentSubjectKind,
    SandboxAgentAdmission, SandboxSpawnCheckpoint, SandboxSpawnCheckpointRequest, SandboxToolCall,
    SandboxToolCallReceipt, SandboxToolCallRequest, SandboxToolCallStatus, SourceLocation,
    SourceReadiness, SourceRegion, ToolCallExecution, ToolCallRecord, ToolCallResolution,
    ToolCallStatus, TurnAgentRunWait, TurnAgentRunWaitSet, TurnAgentRunWaitStatus,
    TurnCheckpointProgress, TurnClientWait, TurnClientWaitStatus, TurnFailureReceipt,
    TurnFailureRetry, TurnRun, TurnRunStatus, TurnSteer, TurnSteerStatus, MAX_ATTACHMENT_REVISION,
    MAX_MESSAGE_ATTACHMENTS, MAX_ROOT_ATTACHMENTS,
};
pub use preview::{ToolActionPreview, ToolResultPreview};
pub use provider::{
    ChatMessage, ChatRequest, ContentBlock, ModelProvider, ProviderEvent, ProviderId,
    RefusalDetails, RefusalOutcome, StopReason, Usage,
};
pub use renderer_tool::RendererToolName;
pub use semantic_checkpoint::{
    ContextCheckpoint, ContextCheckpointPayloadV1, SaveContextCheckpointOutcome,
    CONTEXT_CHECKPOINT_FORMAT_V1, MAX_CONTEXT_CHECKPOINT_BYTES, MAX_CONTEXT_CHECKPOINT_ITEMS,
    MAX_CONTEXT_CHECKPOINT_ITEM_BYTES,
};
pub use steer::{SteerInbox, SteerMessage};
pub use storage::{
    AcceptAgentRunOutcome, AcceptClaimedToolCallOutcome, AcceptSandboxAgentRunAndParkTurnOutcome,
    AcceptToolCallOutcome, AcceptTurnOutcome, AcceptTurnSteerOutcome, AdmitSandboxAgentRunOutcome,
    AnswerUserQuestionsOutcome, AppendClaimedMessageOutcome, ApplyTurnSteerOutcome,
    BeginRootAttachmentChangeOutcome, BlobMetadata, BlobStore, BlobStream, ChatRefusalSnapshot,
    ChatToolActivitySnapshot, ChatToolActivityStatus, ChatTranscriptSnapshot,
    CheckpointSandboxSpawnOutcome, ClaimAgentRunInboxOutcome, ClaimClientToolCallOutcome,
    ClaimDelegatedFileReadOutcome, ClaimSandboxToolCallOutcome, ClaimScanTerminalEvent,
    ClaimTurnRunOutcome, ClientToolCallClaim, CompleteTurnRunOutcome,
    ConsumeAgentRunInboxAndResumeTurnOutcome, ConsumeAgentRunInboxOutcome,
    DecideToolApprovalOutcome, DeleteChatOutcome, DeleteProjectOutcome, DocumentIndexJobReason,
    EnsureDocumentIndexJobOutcome, EnsureDocumentParseJobOutcome, FailAgentRunOutcome,
    FinishAgentRunCancellationOutcome, FinishRootAttachmentChangeOutcome,
    FinishTurnCancellationOutcome, HeartbeatClientToolCallOutcome, JournaledClientToolCallOutcome,
    JournaledToolApprovalOutcome, JournaledTurnOutcome, JournaledTurnSteerOutcome,
    ParkSandboxToolCallOutcome, ParkTurnForAgentRunInboxOutcome, ParkTurnForAgentRunWaitSetOutcome,
    ParkTurnForClientCallOutcome, PendingChatPrompt, RecordTurnFailureOutcome,
    RequestAgentRunCancellationOutcome, RequestToolApprovalOutcome, RequestTurnCancellationOutcome,
    ResolveSandboxToolCallOutcome, ResolveToolCallOutcome, ResumeTurnForAgentRunWaitSetOutcome,
    SecretProvider, Store, SubmitAgentRunResultOutcome, TurnLeaseFence,
    MAX_PENDING_ROOT_ATTACHMENT_CHANGES,
};
pub use tool::{
    input_schema_for, ApprovalClass, Tool, ToolCtx, ToolErrorCategory, ToolOutput, ToolScratch,
    ToolSpec, ToolUiView,
};
#[cfg(feature = "tools")]
pub use tools::{CreateDeliverable, ListDir, ReadFile, WriteFile};
pub use user_questions::{
    ask_user_questions_tool_spec, validate_ask_user_questions_arguments, AnswerUserQuestions,
    AnswerUserQuestionsRequest, AskUserQuestionsArgs, PendingUserQuestions, UserQuestion,
    UserQuestionAnswer, UserQuestionOption, UserQuestionRequestStatus, ASK_USER_QUESTIONS_TOOL,
    MAX_FREE_FORM_ANSWER_CHARS, MAX_QUESTION_HEADER_CHARS, MAX_QUESTION_ID_CHARS,
    MAX_QUESTION_OPTIONS, MAX_QUESTION_OPTION_DESCRIPTION_CHARS, MAX_QUESTION_OPTION_ID_CHARS,
    MAX_QUESTION_OPTION_LABEL_CHARS, MAX_QUESTION_PROMPT_CHARS, MAX_USER_QUESTIONS,
};
