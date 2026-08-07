//! The persisted conversation model.
//!
//! Mirrors the conversation tables of `Store` schema v1. A
//! [`Chat`] is a durable conversation with an ordered, pathless host-root
//! projection; a
//! [`TurnRun`] is one durably scheduled agent turn, and a [`Message`] is one
//! user input or assistant answer within it. Steps remain runtime concepts of
//! the agent loop.

mod chat_settings;
mod documents;
mod identity;
mod messages;
mod runs;
mod turns;

pub(crate) fn is_false(value: &bool) -> bool {
    !*value
}

pub use chat_settings::{NetworkPolicy, PermissionMode, ReasoningEffort, Role};
pub use documents::{
    BlobRetirement, BlobRetirementStatus, DocumentBlob, DocumentListCursor, DocumentReadiness,
    DocumentRecord, DocumentScope, DocumentSourceUpsert, DocumentSummaryRecord, DocumentUpsert,
    ExecFileChange, ExecFileRejection, ExecFileRejectionReason, ExecFileRejectionRecord,
    ExecFileSnapshot, ExecFileSnapshotRecord, ExecUndoState, EXEC_SNAPSHOT_RETAINED_TURNS,
    MAX_EXEC_SNAPSHOT_BYTES,
};
pub use identity::{
    BeginRootAttachmentChange, ChatRootAttachment, OwnerId, Project, RootAttachmentChange,
    RootAttachmentChangeAction, RootAttachmentChangeFailure, RootAttachmentChangePhase,
    RootAttachmentChangeTerminal, RootAttachmentOrigin, RootAttachmentSubjectKind,
    MAX_ATTACHMENT_REVISION, MAX_ROOT_ATTACHMENTS,
};
pub use messages::{
    exec_attachment_file_name, Message, MessageAttachment, MessageDocumentAttachment,
    ToolCallExecution, ToolCallRecord, ToolCallResolution, ToolCallStatus,
    MAX_EXEC_WORKSPACE_FILE_BYTES, MAX_MESSAGE_ATTACHMENTS,
};
pub use runs::{
    AgentRun, AgentRunCancellationReason, AgentRunCancellationSignal, AgentRunExecutionLocation,
    AgentRunInboxEntry, AgentRunInboxStatus, AgentRunProgressEntry, AgentRunResult,
    AgentRunResultPayload, AgentRunStatus, AgentRunSubmittedOutput, AgentRunTier, Chat,
    DelegatedFileReadClaim, SandboxAgentAdmission, SandboxSpawnCheckpoint,
    SandboxSpawnCheckpointRequest, SandboxToolCall, SandboxToolCallParkEntry,
    SandboxToolCallReceipt, SandboxToolCallRequest, SandboxToolCallStatus,
};
pub use turns::{
    AgentRunWaitCondition, AgentRunWaitSetCandidate, AgentRunWaitSetCheckpointRequest,
    ClientToolCallRequest, TurnAgentRunWaitSet, TurnAgentRunWaitStatus, TurnCheckpointProgress,
    TurnClientWait, TurnClientWaitStatus, TurnFailureReceipt, TurnFailureRetry, TurnRun,
    TurnRunStatus, TurnSteer, TurnSteerStatus,
};

pub(crate) use messages::user_message_llm_content;
pub(crate) use runs::{
    validate_chat_root_projection, validate_chat_root_projection_against_project,
    validate_project_root_projection,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_work_statuses_have_stable_snake_case_values() {
        assert_eq!(
            serde_json::to_string(&BlobRetirementStatus::RetryWait).unwrap(),
            "\"retry_wait\""
        );
        assert!(BlobRetirementStatus::Cancelled.is_terminal());
        assert!(!BlobRetirementStatus::Queued.is_terminal());
        assert_eq!(
            serde_json::to_string(&TurnRunStatus::RetryWait).unwrap(),
            "\"retry_wait\""
        );
        assert!(TurnRunStatus::Completed.is_terminal());
        assert!(!TurnRunStatus::Running.is_terminal());
    }
}
