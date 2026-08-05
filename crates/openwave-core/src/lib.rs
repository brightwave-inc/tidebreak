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

/// Upper bound on the filename lookup when matching files to existing
/// outputs. Far above the catalog's own display cap. Shared between the
/// output scan, the agent's filename resolution for output write-backs, and a
/// background run's submission so all three match the same window; it lives
/// here, ungated, because the agent compiles without the `tools` feature the
/// scan is behind.
pub const OUTPUT_LOOKUP_LIMIT: u64 = 1_000;

pub mod agent;
pub mod agent_tools;
pub mod approval;
#[cfg(feature = "blob-fs")]
pub mod blob;
pub mod cancel;
pub mod citation;
pub mod client_tools;
pub mod config;
pub mod connected_app;
pub mod context;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub mod db;
pub mod deliverable;
// Host-side acceptance writes bytes into private scratch, so it depends on the
// capability filesystem and async runtime that only the `tools` feature pulls
// in. The persisted contract and migration below stay available without it.
#[cfg(feature = "tools")]
pub mod deliverable_acceptance;
pub mod error;
pub mod event;
pub mod id;
pub mod image;
#[cfg(feature = "keychain")]
pub mod keychain;
pub mod local_app;
pub mod model;
#[cfg(feature = "tools")]
pub mod output_scan;
pub mod plan_mode;
pub mod preview;
pub mod provider;
mod renderer_tool;
pub mod secret_cache;
pub mod semantic_checkpoint;
pub mod steer;
pub mod storage;
pub mod tool;
#[cfg(feature = "tools")]
pub mod tools;
pub mod user_questions;

pub use agent::{
    Agent, AgentConfig, AgentTurnOutcome, ClaimedAgentEvent, ForegroundAgentWaitRequest,
    SandboxAgentSpawnRequest, ToolRegistry, UtilityModel,
};
pub use agent_tools::{
    sandbox_done_tool_spec, sandbox_exec_tool_spec, sandbox_read_delegated_file_tool_spec,
    sandbox_web_search_tool_spec, spawn_sandbox_agent_tool_spec, validate_sandbox_done_arguments,
    validate_sandbox_exec_arguments, validate_sandbox_read_delegated_file_arguments,
    validate_spawn_sandbox_agent_arguments, validate_wait_for_agents_arguments,
    wait_for_agents_tool_spec, web_extract_tool_spec, web_search_tool_spec,
    SandboxAgentFileResource, SandboxDoneArgs, SandboxExecArgs, SpawnSandboxAgentArgs,
    SpawnSandboxAgentResult, WaitForAgentResult, WaitForAgentsArgs, WaitForAgentsResult,
    WebExtractArgs, WebSearchArgs, DEFAULT_WEB_SEARCH_RESULTS, MAX_SANDBOX_AGENT_TASK_CHARS,
    MAX_SANDBOX_DONE_OUTPUTS, MAX_SANDBOX_DONE_SUMMARY_CHARS, MAX_SANDBOX_EXEC_ARGUMENTS,
    MAX_SANDBOX_EXEC_COMMAND_BYTES, MAX_SANDBOX_EXEC_CWD_BYTES, MAX_SANDBOX_TOOL_CALLS,
    MAX_WAIT_FOR_AGENTS_CHILDREN, MAX_WEB_EXTRACT_URL_BYTES, MAX_WEB_SEARCH_DOMAINS,
    MAX_WEB_SEARCH_QUERY_CHARS, MAX_WEB_SEARCH_RESULTS, SANDBOX_DONE_TOOL, SANDBOX_EXEC_TOOL,
    SANDBOX_READ_DELEGATED_FILE_TOOL, SANDBOX_WEB_SEARCH_TOOL, SPAWN_SANDBOX_AGENT_TOOL,
    WAIT_FOR_AGENTS_TOOL, WEB_EXTRACT_TOOL, WEB_SEARCH_TOOL,
};
pub use approval::{
    ApprovalDecision, ApprovalFuture, ApprovalGate, ApprovalJournalIdentity, ApprovalRegistration,
    ApprovalRegistrationFuture, ApprovalRequest, ApprovalRequiredPublication, AutoApproveGate,
    AutoJudgeStatus, GrantLevel, GrantScope, RefuseGate, StandingGrant, StandingGrantRecord,
    StandingGrants, ToolApproval, ToolApprovalKind, ToolApprovalStatus,
};
#[cfg(feature = "blob-fs")]
pub use blob::FsBlobStore;
pub use cancel::{CancelToken, Cancelled};
pub use citation::{
    citation_authoring_instruction, format_citation_directive, parse_assistant_citations,
    AssistantCitationInput, AssistantCitationSnapshot, CitationLocator, ParsedAssistantCitations,
    MAX_ASSISTANT_CITATIONS,
};
pub use client_tools::{
    import_connected_file_tool_spec, list_connected_folders_tool_spec, list_folder_tool_spec,
    read_connected_file_tool_spec, request_folder_access_tool_spec,
    sandbox_folder_access_proposal_tool_spec, validate_import_connected_file_arguments,
    validate_list_connected_folders_arguments, validate_list_folder_arguments,
    validate_read_connected_file_arguments, validate_request_folder_access_arguments,
    validate_write_output_to_connected_folder_arguments,
    write_output_to_connected_folder_tool_spec, GrantedFolderCapability, ImportConnectedFileArgs,
    ImportConnectedFileResult, ListConnectedFoldersArgs, ListFolderArgs, OutputWriteMode,
    ReadConnectedFileArgs, RequestFolderAccessArgs, RequestFolderAccessResult,
    RequestedFolderCapability, RequestedFolderHint, WriteOutputToConnectedFolderArgs,
    WriteOutputToConnectedFolderProposal, IMPORT_CONNECTED_FILE_TOOL, LIST_CONNECTED_FOLDERS_TOOL,
    LIST_FOLDER_TOOL, MAX_CONNECTED_FOLDER_PATH_BYTES, MAX_FOLDER_ACCESS_REASON_CHARS,
    READ_CONNECTED_FILE_TOOL, REQUEST_FOLDER_ACCESS_TOOL, WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL,
};
pub use config::{Config, Profile};
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub use db::DbStore;
pub use deliverable::{
    binary_media_type_for_extension, deliverable_media_type, media_type_is_text,
    output_revision_relative_path, revision_byte_ceiling, validate_binary_deliverable,
    validate_deliverable_media_type, validate_deliverable_name, validate_portable_filename,
    CreateOutput, DeliverableKind, NewOutputRevision, OutputRecord, OutputRevision,
    RevisionProducer, MAX_BINARY_DELIVERABLE_BYTES, MAX_DELIVERABLE_BYTES,
    MAX_DELIVERABLE_MEDIA_TYPE_CHARS, MAX_DELIVERABLE_NAME_CHARS, MAX_OUTPUT_REVISIONS,
    OUTPUTS_DIRECTORY,
};
#[cfg(feature = "tools")]
pub use deliverable_acceptance::{
    accept_workspace_artifact, restore_output_to_revision, WorkspaceArtifactProposal,
};
pub use error::{AgentError, AgentErrorInfo, ProviderErrorInfo, ProviderFailure, Result};
pub use event::{AgentEvent, SequencedEvent};
pub use id::{
    AgentRunId, AssistantCitationId, CallId, ChatId, ChunkId, DocumentId, HostRootId,
    HostRootIdError, MessageId, OutputCitationId, OutputId, OutputRevisionId, ProjectId,
    RootAttachmentChangeId, RootAttachmentChangeIdError, StepId, TurnId, TurnSteerId,
};
pub use image::{
    ImageAttachments, ImageData, ImageMediaType, ImageRef, MAX_IMAGE_BYTES, MAX_IMAGE_DIMENSION,
};
#[cfg(feature = "keychain")]
pub use keychain::KeychainSecretProvider;
pub use model::{
    exec_attachment_file_name, AgentRun, AgentRunCancellationReason, AgentRunCancellationSignal,
    AgentRunExecutionLocation, AgentRunInboxEntry, AgentRunInboxStatus, AgentRunResult,
    AgentRunResultPayload, AgentRunStatus, AgentRunSubmittedOutput, AgentRunTier,
    AgentRunWaitCondition, AgentRunWaitSetCandidate, AgentRunWaitSetCheckpointRequest,
    BeginRootAttachmentChange, BlobRetirement, BlobRetirementStatus, Chat, ChatRootAttachment,
    ClientToolCallRequest, DelegatedFileReadClaim, DocumentListCursor, DocumentRecord,
    DocumentScope, DocumentSourceBlob, DocumentSourceUpsert, DocumentSummaryRecord, DocumentUpsert,
    ExecFileChange, ExecFileRejection, ExecFileRejectionReason, ExecFileRejectionRecord,
    ExecFileSnapshot, ExecFileSnapshotRecord, ExecUndoState, Message, MessageAttachment,
    MessageDocumentAttachment, NetworkPolicy, OwnerId, PermissionMode, Project, ReasoningEffort,
    Role, RootAttachmentChange, RootAttachmentChangeAction, RootAttachmentChangeFailure,
    RootAttachmentChangePhase, RootAttachmentChangeTerminal, RootAttachmentOrigin,
    RootAttachmentSubjectKind, SandboxAgentAdmission, SandboxSpawnCheckpoint,
    SandboxSpawnCheckpointRequest, SandboxToolCall, SandboxToolCallReceipt, SandboxToolCallRequest,
    SandboxToolCallStatus, SourceReadiness, ToolCallExecution, ToolCallRecord, ToolCallResolution,
    ToolCallStatus, TurnAgentRunWaitSet, TurnAgentRunWaitStatus, TurnCheckpointProgress,
    TurnClientWait, TurnClientWaitStatus, TurnFailureReceipt, TurnFailureRetry, TurnRun,
    TurnRunStatus, TurnSteer, TurnSteerStatus, EXEC_SNAPSHOT_RETAINED_TURNS,
    MAX_ATTACHMENT_REVISION, MAX_EXEC_SNAPSHOT_BYTES, MAX_EXEC_WORKSPACE_FILE_BYTES,
    MAX_MESSAGE_ATTACHMENTS, MAX_ROOT_ATTACHMENTS,
};
#[cfg(feature = "tools")]
pub use output_scan::{
    sync_output_directory, OutputDirectorySync, OutputSyncEntry, OutputSyncStatus,
    EXEC_OUTPUT_DIRECTORY, MAX_OUTPUT_SCAN_FILES,
};
pub use plan_mode::{
    exit_plan_mode_tool_spec, plan_decision_result, validate_exit_plan_mode_arguments,
    DecidePlanRequest, ExitPlanModeArgs, PendingPlanApproval, PlanDecision, PlanDecisionChoice,
    PlanRequestStatus, DEFAULT_ACCEPTED_PLAN_MODE, EXIT_PLAN_MODE_TOOL, MAX_PLAN_CONTENT_CHARS,
    MAX_PLAN_FEEDBACK_CHARS, MAX_PLAN_TITLE_CHARS, MIN_PLAN_CONTENT_CHARS,
};
pub use preview::{
    format_bytes, ExecDegradation, ResultEntry, ResultEntryKind, ResultFailure, ToolActionPreview,
    ToolResultPreview, MAX_RESULT_ENTRIES, MAX_RESULT_ENTRY_CHARS,
};
pub use provider::{
    ChatMessage, ChatRequest, ContentBlock, MessageReasoning, ModelProvider, ProviderEvent,
    ProviderId, ReasoningOrigin, RefusalDetails, RefusalOutcome, ResponseFormat, StopReason,
    ToolChoice, Usage,
};
pub use renderer_tool::RendererToolName;
pub use secret_cache::CachingSecretProvider;
pub use semantic_checkpoint::{
    ContextCheckpoint, ContextCheckpointPayloadV1, SaveContextCheckpointOutcome,
    CONTEXT_CHECKPOINT_FORMAT_V1, MAX_CONTEXT_CHECKPOINT_BYTES, MAX_CONTEXT_CHECKPOINT_ITEMS,
    MAX_CONTEXT_CHECKPOINT_ITEM_BYTES,
};
pub use steer::{SteerInbox, SteerMessage};
pub use storage::{
    AcceptAgentRunOutcome, AcceptClaimedToolCallOutcome, AcceptToolCallOutcome, AcceptTurnOutcome,
    AcceptTurnSteerOutcome, AdmitSandboxAgentRunOutcome, AnswerUserQuestionsOutcome,
    AppendClaimedMessageOutcome, ApplyTurnSteerOutcome, BeginRootAttachmentChangeOutcome,
    BeginSandboxProvisionOutcome, BlobMetadata, BlobStore, BlobStream, ChatCitationSnapshot,
    ChatTerminalTurnSnapshot, ChatTerminalTurnStatus, ChatToolActivitySnapshot,
    ChatToolActivityStatus, ChatTranscriptSnapshot, CheckpointSandboxSpawnOutcome,
    ClaimClientToolCallOutcome, ClaimDelegatedFileReadOutcome, ClaimSandboxToolCallOutcome,
    ClaimScanTerminalEvent, ClaimTurnRunOutcome, ClientToolCallClaim, CompleteTurnRunOutcome,
    DecideToolApprovalOutcome, DeleteChatOutcome, DeleteProjectOutcome, FailAgentRunOutcome,
    FinishAgentRunCancellationOutcome, FinishRootAttachmentChangeOutcome,
    FinishTurnCancellationOutcome, HeartbeatClientToolCallOutcome, InboxItem, InboxItemKind,
    JournaledClientToolCallOutcome, JournaledToolApprovalOutcome, JournaledTurnOutcome,
    JournaledTurnSteerOutcome, OperationClaimOutcome, OperationLogEntry, OperationLogState,
    OperationLogWrite, ParkSandboxToolCallOutcome, ParkTurnForAgentRunWaitSetOutcome,
    ParkTurnForClientCallOutcome, PendingChatPrompt, RecordTurnFailureOutcome,
    RequestAgentRunCancellationOutcome, RequestToolApprovalOutcome, RequestTurnCancellationOutcome,
    ResolveSandboxToolCallOutcome, ResolveToolCallOutcome, ResumeTurnForAgentRunWaitSetOutcome,
    RetrySandboxToolCallOutcome, SandboxAdmissionMode, SandboxProvision, SandboxProvisionState,
    SecretProvider, Store, SubmitAgentRunResultOutcome, TurnLeaseFence,
    MAX_PENDING_ROOT_ATTACHMENT_CHANGES,
};
pub use tool::{
    input_schema_for, strict_json_schema, ApprovalClass, OptionalProperties, Tool, ToolCtx,
    ToolErrorCategory, ToolOutput, ToolScratch, ToolSpec, ToolUiView,
};
#[cfg(feature = "tools")]
pub use tools::{create_app_tool_spec, CreateAppTool, ListDir, ReadFile, WriteFile};
pub use user_questions::{
    ask_user_questions_tool_spec, validate_ask_user_questions_arguments, AnswerUserQuestions,
    AnswerUserQuestionsRequest, AskUserQuestionsArgs, PendingUserQuestions, UserQuestion,
    UserQuestionAnswer, UserQuestionOption, UserQuestionRequestStatus, UserQuestionType,
    ASK_USER_QUESTIONS_TOOL, MAX_ADDITIONAL_USER_CONTEXT_CHARS, MAX_FREE_FORM_ANSWER_CHARS,
    MAX_QUESTION_HEADER_CHARS, MAX_QUESTION_ID_CHARS, MAX_QUESTION_OPTIONS,
    MAX_QUESTION_OPTION_DESCRIPTION_CHARS, MAX_QUESTION_OPTION_ID_CHARS,
    MAX_QUESTION_OPTION_LABEL_CHARS, MAX_QUESTION_PROMPT_CHARS, MAX_USER_QUESTIONS,
};
