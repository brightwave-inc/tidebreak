use sea_orm::DatabaseBackend;
use sea_orm_migration::prelude::*;

use super::{
    AgentRunStatus, BlobRetirementStatus, TurnAgentRunWaitStatus, TurnClientWaitStatus,
    TurnRunStatus, TurnSteerStatus,
};
use crate::model::{AgentRunWaitCondition, SandboxToolCallStatus};

const LEGACY_DOCUMENT_PIPELINE_FINGERPRINT_LEN: usize = 512;
const LEGACY_DOCUMENT_JOB_ERROR_CODE_LEN: usize = 128;
const LEGACY_DOCUMENT_JOB_ERROR_DETAIL_LEN: usize = 4096;

#[derive(Clone, Copy)]
enum DocumentProcessingStatus {
    Queued,
    Processing,
    Ready,
    Failed,
}

impl DocumentProcessingStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Processing => "processing",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy)]
enum DocumentJobStatus {
    Queued,
    Running,
    RetryWait,
    Succeeded,
    Failed,
    Cancelled,
}

impl DocumentJobStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::RetryWait => "retry_wait",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(Init),
            Box::new(AddEventJournal),
            Box::new(AddProjects),
            Box::new(AddChatModel),
            Box::new(AddToolCalls),
            Box::new(AddDocuments),
            Box::new(AddSandboxToolCalls),
            Box::new(AddAgentRunResultPayload),
            Box::new(AddClaimedTurnEffectLeases),
            Box::new(AddChatReasoningEffort),
            Box::new(AddDocumentChatScope),
            Box::new(AddUserQuestions),
            Box::new(AddConversationOutputs),
            Box::new(AddMessageAttachments),
            Box::new(AddOutputRevisionCitations),
            Box::new(AddContextCheckpoints),
            Box::new(AddStandingToolGrants),
            Box::new(AddContextCheckpointUsage),
            Box::new(AddAgentRunModel),
            Box::new(AddToolResultPreviews),
            Box::new(SplitAgentRunExecution),
            Box::new(AddOperationLog),
            Box::new(ExtendOutputRevisionsForBinary),
            Box::new(AddChatPermissionMode),
            Box::new(AddToolCallAutoJudgeStatus),
            Box::new(AddChatCitationFormat),
            Box::new(AddEvidenceLocation),
            Box::new(AllowContainerExecutionLocation),
            Box::new(WidenStandingGrantScope),
            Box::new(RetireDocumentIndexing),
            Box::new(LightweightCitations),
            Box::new(RetireDocumentPipeline),
            Box::new(AddMessageDocumentAttachments),
            Box::new(AddSandboxProvision),
            Box::new(AddLateResultEvidence),
            Box::new(AddPlanRequests),
            Box::new(AddExecFileSnapshots),
            Box::new(AddExecFileRejections),
            Box::new(AddLocalApps),
            Box::new(AddAppGrants),
            Box::new(AddToolCallRawArguments),
            Box::new(RaiseAttemptBudgets),
            Box::new(AddChatNetworkPolicy),
            Box::new(ExtendUserQuestions),
        ]
    }
}

/// Adds the provider-neutral per-chat code-execution network policy. Existing
/// conversations stay at the native sandbox's historical behavior: off.
struct AddChatNetworkPolicy;

impl MigrationName for AddChatNetworkPolicy {
    fn name(&self) -> &str {
        "m20260730_000039_add_chat_network_policy"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddChatNetworkPolicy {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Chat::Table)
                    .add_column(
                        ColumnDef::new(Chat::NetworkPolicy)
                            .text()
                            .not_null()
                            .default(r#"{"mode":"off"}"#),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Chat::Table)
                    .drop_column(Chat::NetworkPolicy)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

/// Extends the first question checkpoint with skip, multi-select, and
/// additional-context support without rewriting already answered rows.
///
/// The original scalar answer columns remain as the compatibility projection
/// for databases created before this migration. New answers use the explicit
/// response columns, which can represent selected options plus custom text and
/// an explicit skipped question.
struct ExtendUserQuestions;

impl MigrationName for ExtendUserQuestions {
    fn name(&self) -> &str {
        "m20260730_000040_extend_user_questions"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for ExtendUserQuestions {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(UserQuestionRequest::Table)
                    .add_column(
                        ColumnDef::new(UserQuestionRequest::AdditionalUserContext)
                            .string_len(crate::MAX_ADDITIONAL_USER_CONTEXT_CHARS as u32),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(UserQuestion::Table)
                    .add_column(
                        ColumnDef::new(UserQuestion::QuestionType)
                            .string_len(16)
                            .not_null()
                            .default(crate::UserQuestionType::SingleSelect.as_str()),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(UserQuestion::Table)
                    .add_column(ColumnDef::new(UserQuestion::AnswerSelectedOptionIds).json_binary())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(UserQuestion::Table)
                    .add_column(
                        ColumnDef::new(UserQuestion::AnswerCustomAnswer)
                            .string_len(crate::MAX_FREE_FORM_ANSWER_CHARS as u32),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(UserQuestion::Table)
                    .add_column(
                        ColumnDef::new(UserQuestion::ResponseRecordedAt).timestamp_with_time_zone(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(UserQuestion::Table)
                    .drop_column(UserQuestion::ResponseRecordedAt)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(UserQuestion::Table)
                    .drop_column(UserQuestion::AnswerCustomAnswer)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(UserQuestion::Table)
                    .drop_column(UserQuestion::AnswerSelectedOptionIds)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(UserQuestion::Table)
                    .drop_column(UserQuestion::QuestionType)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(UserQuestionRequest::Table)
                    .drop_column(UserQuestionRequest::AdditionalUserContext)
                    .to_owned(),
            )
            .await
    }
}

/// Adds the profile-scoped local-app record: an `app` row per app plus
/// insert-only `app_revision` rows pairing a bounded manifest with the length
/// and digest of write-once bundle bytes.
///
/// Follows `output`/`output_revision` with one deliberate difference: there is
/// no chat foreign key anywhere. The profile owns the app; `chat_id` on a
/// revision is nullable provenance without a constraint, so an app and its
/// history survive deletion of the conversation that authored them. Producer
/// attribution reuses the outputs discipline — `turn_id` XOR
/// `producing_run_id`, enforced by CHECK. Purely additive, symmetric `down`.
struct AddLocalApps;

impl MigrationName for AddLocalApps {
    fn name(&self) -> &str {
        "m20260730_000037_add_local_apps"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddLocalApps {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(App::Table)
                    .col(ColumnDef::new(App::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(App::Name).text().not_null())
                    .col(ColumnDef::new(App::CurrentRevisionId).uuid().not_null())
                    .col(ColumnDef::new(App::RevisionCount).integer().not_null())
                    .col(
                        ColumnDef::new(App::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(App::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(App::DeletedAt).timestamp_with_time_zone())
                    .check(
                        Expr::col(App::RevisionCount)
                            .between(1, crate::local_app::MAX_APP_REVISIONS as i32),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_app_updated")
                    .table(App::Table)
                    .col(App::UpdatedAt)
                    .col(App::Id)
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(AppRevision::Table)
                    .col(
                        ColumnDef::new(AppRevision::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(AppRevision::AppId).uuid().not_null())
                    .col(ColumnDef::new(AppRevision::Ordinal).integer().not_null())
                    .col(
                        ColumnDef::new(AppRevision::ManifestJson)
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AppRevision::ByteLen)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AppRevision::Sha256)
                            .binary_len(32)
                            .not_null(),
                    )
                    .col(ColumnDef::new(AppRevision::TurnId).uuid())
                    .col(ColumnDef::new(AppRevision::ProducingRunId).uuid())
                    // Provenance only: no foreign key, so the revision outlives
                    // the conversation that authored it.
                    .col(ColumnDef::new(AppRevision::ChatId).uuid())
                    .col(
                        ColumnDef::new(AppRevision::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_app_revision_app")
                            .from_tbl(AppRevision::Table)
                            .from_col(AppRevision::AppId)
                            .to_tbl(App::Table)
                            .to_col(App::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .check(
                        Expr::col(AppRevision::Ordinal)
                            .between(1, crate::local_app::MAX_APP_REVISIONS as i32),
                    )
                    .check(
                        Expr::col(AppRevision::ByteLen)
                            .between(1, crate::local_app::MAX_APP_BUNDLE_BYTES as i64),
                    )
                    // A revision records the foreground turn or the background
                    // run that produced it, never both.
                    .check(
                        Expr::col(AppRevision::TurnId)
                            .is_null()
                            .or(Expr::col(AppRevision::ProducingRunId).is_null()),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_app_revision_ordinal")
                    .table(AppRevision::Table)
                    .col(AppRevision::AppId)
                    .col(AppRevision::Ordinal)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AppRevision::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(App::Table).to_owned())
            .await
    }
}

/// Adds the durable app-grant consent record: at most one row per app — the
/// app id is the primary key — carrying the granted `(server, tools[])`
/// bindings with each bound server's definition fingerprint as JSON.
///
/// The grant is host-computed policy, replaced wholesale by a fresh consent
/// and deleted by revocation, so the table needs no history and no surrogate
/// identity. Cascade delete follows the app row: a grant never outlives the
/// thing it consented to. Purely additive, symmetric `down`.
struct AddAppGrants;

impl MigrationName for AddAppGrants {
    fn name(&self) -> &str {
        "m20260730_000038_add_app_grants"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddAppGrants {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(AppGrant::Table)
                    .col(
                        ColumnDef::new(AppGrant::AppId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AppGrant::BindingsJson)
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AppGrant::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_app_grant_app")
                            .from_tbl(AppGrant::Table)
                            .from_col(AppGrant::AppId)
                            .to_tbl(App::Table)
                            .to_col(App::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AppGrant::Table).to_owned())
            .await
    }
}

/// Adds the journal of file changes a turn applied to a granted folder (issue
/// #1075), one row per file, carrying the content address of the bytes the
/// folder held beforehand.
///
/// `prior_blob_id` is the schema's third class of live blob reference, after
/// `document.source_blob_id` and `message_attachment.blob_id`, and the index on
/// it exists for the same reason theirs do: every retirement decision asks
/// whether any table still references the blob, and that question must not
/// scan. A snapshot the auditor cannot see is a user file we promised to be
/// able to restore and then deleted the only copy of.
///
/// `undo_state` is explicit rather than derived from a null blob id because a
/// file with no prior bytes (a creation) and a file whose prior bytes were too
/// large to keep are both blob-less and mean opposite things to the user.
/// Purely additive (a new table), with a symmetric `down`.
struct AddExecFileSnapshots;

impl MigrationName for AddExecFileSnapshots {
    fn name(&self) -> &str {
        "m20260730_000036_add_exec_file_snapshots"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddExecFileSnapshots {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ExecFileSnapshot::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ExecFileSnapshot::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ExecFileSnapshot::ChatId).uuid().not_null())
                    .col(ColumnDef::new(ExecFileSnapshot::TurnId).uuid().not_null())
                    .col(
                        ColumnDef::new(ExecFileSnapshot::FolderPath)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ExecFileSnapshot::RelativePath)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ExecFileSnapshot::ChangeKind)
                            .string_len(32)
                            .not_null(),
                    )
                    .col(ColumnDef::new(ExecFileSnapshot::PriorBlobId).uuid().null())
                    .col(
                        ColumnDef::new(ExecFileSnapshot::PriorByteLen)
                            .big_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(ExecFileSnapshot::NewSha256)
                            .string_len(64)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(ExecFileSnapshot::UndoState)
                            .string_len(32)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ExecFileSnapshot::RecordedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_exec_file_snapshot_chat")
                            .from(ExecFileSnapshot::Table, ExecFileSnapshot::ChatId)
                            .to(Chat::Table, Chat::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .check(Expr::col(ExecFileSnapshot::PriorBlobId).ne(uuid::Uuid::nil()))
                    .check(Expr::col(ExecFileSnapshot::PriorByteLen).gte(0))
                    .check(Expr::col(ExecFileSnapshot::ChangeKind).is_in([
                        crate::model::ExecFileChange::Created.as_str(),
                        crate::model::ExecFileChange::Overwritten.as_str(),
                        crate::model::ExecFileChange::Deleted.as_str(),
                    ]))
                    .check(Expr::col(ExecFileSnapshot::UndoState).is_in([
                        crate::model::ExecUndoState::Available.as_str(),
                        crate::model::ExecUndoState::PriorTooLarge.as_str(),
                        crate::model::ExecUndoState::PriorUnreadable.as_str(),
                    ]))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_exec_file_snapshot_blob")
                    .table(ExecFileSnapshot::Table)
                    .col(ExecFileSnapshot::PriorBlobId)
                    .to_owned(),
            )
            .await?;
        // Retention prunes the oldest turns of one chat, and undo reads the
        // newest; both walk this index rather than the whole journal.
        manager
            .create_index(
                Index::create()
                    .name("idx_exec_file_snapshot_chat_turn")
                    .table(ExecFileSnapshot::Table)
                    .col(ExecFileSnapshot::ChatId)
                    .col(ExecFileSnapshot::RecordedAt)
                    .col(ExecFileSnapshot::TurnId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ExecFileSnapshot::Table).to_owned())
            .await
    }
}

/// Persists staged writes that were deliberately left out of the user's
/// folder. These rows contain report metadata only; successful writes continue
/// to use `exec_file_snapshot`, whose blob references power undo.
struct AddExecFileRejections;

impl MigrationName for AddExecFileRejections {
    fn name(&self) -> &str {
        "m20260730_000039_add_exec_file_rejections"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddExecFileRejections {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ExecFileRejection::Table)
                    .col(
                        ColumnDef::new(ExecFileRejection::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ExecFileRejection::ChatId).uuid().not_null())
                    .col(ColumnDef::new(ExecFileRejection::TurnId).uuid().not_null())
                    .col(
                        ColumnDef::new(ExecFileRejection::FolderPath)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ExecFileRejection::RelativePath)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ExecFileRejection::Reason)
                            .string_len(32)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ExecFileRejection::RecordedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_exec_file_rejection_chat")
                            .from(ExecFileRejection::Table, ExecFileRejection::ChatId)
                            .to(Chat::Table, Chat::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .check(Expr::col(ExecFileRejection::Reason).is_in([
                        crate::model::ExecFileRejectionReason::Stale.as_str(),
                        crate::model::ExecFileRejectionReason::SnapshotUnavailable.as_str(),
                        crate::model::ExecFileRejectionReason::StagedFileTooLarge.as_str(),
                        crate::model::ExecFileRejectionReason::TrashUnavailable.as_str(),
                        crate::model::ExecFileRejectionReason::Unavailable.as_str(),
                    ]))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_exec_file_rejection_chat_turn")
                    .table(ExecFileRejection::Table)
                    .col(ExecFileRejection::ChatId)
                    .col(ExecFileRejection::RecordedAt)
                    .col(ExecFileRejection::TurnId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ExecFileRejection::Table).to_owned())
            .await
    }
}

/// Keeps the exact bytes a provider streamed for a tool call whose arguments
/// would not parse (issue #1142). Dispatch refuses such a call, but the
/// durable record still wrote the lenient coerced `{}` down and kept the
/// fragment nowhere — so post-hoc debugging of a garbled stream had nothing
/// to look at. The column holds the raw fragment, bounded at the ops layer
/// and treated as untrusted text that is never re-parsed.
///
/// Nullable because every call recorded before this migration streamed no
/// recoverable fragment, and because well-formed calls — the overwhelming
/// majority — have none. Purely additive, with a symmetric `down`.
struct AddToolCallRawArguments;

impl MigrationName for AddToolCallRawArguments {
    fn name(&self) -> &str {
        "m20260730_000037_add_tool_call_raw_arguments"
    }
}

/// Raises the durable attempt budgets of in-flight work from 3 to 5
/// (issue #1147), matching the `DEFAULT_MAX_ATTEMPTS` constants new rows are
/// accepted with. Only non-terminal rows move: finished rows keep the budget
/// they actually ran against, and the raised budget is what lets a parked
/// `retry_wait` row spend the worker's exponential schedule and wall-clock
/// envelope instead of failing on the next transient error. Follows the
/// row-bump pattern of `AddClaimedTurnEffectLeases`; the column default needs
/// no alter because every accept path writes `max_attempts` explicitly.
///
/// The `down` is a deliberate no-op: shrinking a row that has already spent
/// the extra attempts would strand it with `attempt_count > max_attempts`,
/// which the shape checks reject.
struct RaiseAttemptBudgets;

impl MigrationName for RaiseAttemptBudgets {
    fn name(&self) -> &str {
        "m20260730_000037_raise_attempt_budgets"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddToolCallRawArguments {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ToolCall::Table)
                    .add_column(ColumnDef::new(ToolCall::RawArguments).text())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ToolCall::Table)
                    .drop_column(ToolCall::RawArguments)
                    .to_owned(),
            )
            .await
    }
}

#[async_trait::async_trait]
impl MigrationTrait for RaiseAttemptBudgets {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE turn_run SET max_attempts = 5 \
                 WHERE max_attempts = 3 AND status IN \
                 ('queued', 'running', 'retry_wait', 'resuming', 'waiting_for_client', \
                  'waiting_for_agent_run', 'cancelling', 'cancelling_client')",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE agent_run SET max_attempts = 5 \
                 WHERE max_attempts = 3 AND status IN \
                 ('active', 'queued', 'running', 'cancelling', 'waiting', 'retry_wait')",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

/// Adds the late-result evidence column to the sandbox provisioning record
/// (issue #920): a well-formed result that arrives after its container run is
/// already terminal fails the fenced commit predicate — it must never commit —
/// but it is still evidence of what the container produced, retained here for
/// diagnosis. Purely additive nullable column with a symmetric `down`.
struct AddLateResultEvidence;

impl MigrationName for AddLateResultEvidence {
    fn name(&self) -> &str {
        "m20260730_000034_add_late_result_evidence"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddLateResultEvidence {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(SandboxProvision::Table)
                    .add_column(
                        ColumnDef::new(SandboxProvision::LateResultEvidence)
                            .text()
                            .null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(SandboxProvision::Table)
                    .drop_column(SandboxProvision::LateResultEvidence)
                    .to_owned(),
            )
            .await
    }
}

/// Adds the durable sandbox provisioning record (issue #920): the intent a
/// container run commits — carrying its host-minted correlation tag and
/// provisioning window — *before* the backend is asked to create a sandbox, the
/// handle committed onto it afterwards, and the teardown obligation the sweep
/// drives to completion.
///
/// Recovery is driven by this row, not by what the provider reports: an
/// `intended` record whose window lapses failed its admission whether or not a
/// create ever reached the provider, and the orphan sweep reclaims any provider
/// sandbox whose tag names no live record. Purely additive (a new table), with a
/// symmetric `down` and identical shape on SQLite and PostgreSQL.
struct AddSandboxProvision;

impl MigrationName for AddSandboxProvision {
    fn name(&self) -> &str {
        "m20260730_000033_add_sandbox_provision"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddSandboxProvision {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(SandboxProvision::Table)
                    .col(
                        ColumnDef::new(SandboxProvision::RunId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SandboxProvision::Tag)
                            .string_len(64)
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(SandboxProvision::State)
                            .string_len(16)
                            .not_null(),
                    )
                    .col(ColumnDef::new(SandboxProvision::Handle).string().null())
                    .col(
                        ColumnDef::new(SandboxProvision::WindowExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxProvision::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxProvision::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .check(Expr::col(SandboxProvision::State).is_in([
                        "intended",
                        "committed",
                        "teardown",
                        "done",
                    ]))
                    // An intent has no handle until the backend's create returns.
                    .check(
                        Expr::col(SandboxProvision::State)
                            .ne("intended")
                            .or(Expr::col(SandboxProvision::Handle).is_null()),
                    )
                    // A committed record always has one.
                    .check(
                        Expr::col(SandboxProvision::State)
                            .ne("committed")
                            .or(Expr::col(SandboxProvision::Handle).is_not_null()),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(SandboxProvision::Table).to_owned())
            .await
    }
}

/// Adds the per-kind location payload to retrieval evidence (issue #884).
///
/// Evidence is now a taxonomy rather than one shape, and each kind addresses
/// its source differently: a cell range on a named sheet, or a path into a
/// structured document. Those carry a payload the existing columns cannot hold,
/// so it goes in one nullable JSON column.
///
/// Document content — every row that exists today, and everything produced
/// today — keeps its headings and source regions in the columns it always used
/// and leaves this one null. So the column is additive in the strongest sense:
/// no existing row is read differently, and none is rewritten.
struct AddEvidenceLocation;

impl MigrationName for AddEvidenceLocation {
    fn name(&self) -> &str {
        "m20260729_000026_add_evidence_location"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddEvidenceLocation {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(RetrievalEvidence::Table)
                    .add_column(ColumnDef::new(RetrievalEvidence::Location).json_binary())
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(RetrievalEvidence::Table)
                    .drop_column(RetrievalEvidence::Location)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

/// Adds the durable reverse-RPC operation log (issue #858): a crash-safe,
/// idempotency-keyed record of host-mediated operations a sandbox-resident run
/// requested back over the reverse channel.
///
/// The table is keyed by `(run_id, operation_id)` and holds the operation's
/// state machine (`claimed -> recorded | failed`), the request `fingerprint`
/// that fences a re-issue, an `external_effect` flag, the claiming process
/// lifetime's `owner_epoch` (which separates a concurrent duplicate from an
/// after-crash re-issue), and the recorded terminal `body`.
///
/// The schema is deliberately shaped for the retention follow-up (#859): `body`
/// is nullable and paired with a `retained` flag, so #859 can evict a full
/// response body down to a commit marker — clearing `body` and `retained` while
/// keeping the row's state, `external_effect`, and timestamps — without a
/// migration rewrite. This is a purely additive migration (a new table, no
/// change to existing shapes), with a symmetric `down` and identical shape on
/// SQLite and PostgreSQL.
struct AddOperationLog;

impl MigrationName for AddOperationLog {
    fn name(&self) -> &str {
        "m20260728_000022_add_operation_log"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddOperationLog {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(OperationLog::Table)
                    .col(ColumnDef::new(OperationLog::RunId).uuid().not_null())
                    .col(ColumnDef::new(OperationLog::OperationId).uuid().not_null())
                    .col(
                        ColumnDef::new(OperationLog::State)
                            .string_len(16)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OperationLog::Fingerprint)
                            .binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OperationLog::ExternalEffect)
                            .boolean()
                            .not_null(),
                    )
                    .col(ColumnDef::new(OperationLog::OwnerEpoch).uuid().not_null())
                    .col(ColumnDef::new(OperationLog::Body).binary().null())
                    .col(
                        ColumnDef::new(OperationLog::Retained)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(OperationLog::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OperationLog::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .name("pk_operation_log")
                            .col(OperationLog::RunId)
                            .col(OperationLog::OperationId),
                    )
                    .check(Expr::col(OperationLog::State).is_in(["claimed", "recorded", "failed"]))
                    // A claimed entry has not recorded a body yet.
                    .check(
                        Expr::col(OperationLog::State)
                            .ne("claimed")
                            .or(Expr::col(OperationLog::Body).is_null()),
                    )
                    // An unretained entry keeps only a commit marker, never a body.
                    .check(
                        Expr::col(OperationLog::Retained)
                            .or(Expr::col(OperationLog::Body).is_null()),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(OperationLog::Table).to_owned())
            .await
    }
}

/// Splits `agent_run.execution` into a run tier and an execution location.
///
/// The retired column named its variants (`foreground | sandbox`) by who
/// advances the run, while reading as where the run executes. The two axes
/// agree only while every run executes inside the server process, so the
/// field splits before a second location exists. Existing `sandbox` rows map
/// to `(background, in_process)` and `foreground` rows to
/// `(foreground, in_process)`.
///
/// SQLite cannot alter CHECK constraints, so it rebuilds the table with the
/// shape constraints re-expressed over the two new columns. The rebuild is
/// one raw multi-statement batch because it depends on connection state:
/// `PRAGMA foreign_keys` is per-connection (and a no-op inside a
/// transaction), SQLite migrations run outside an automatic transaction, and
/// with enforcement off the drop-and-rename leaves every child table's
/// `REFERENCES agent_run` clause untouched. PostgreSQL alters the table in
/// place inside the migration's transaction.
struct SplitAgentRunExecution;

impl MigrationName for SplitAgentRunExecution {
    fn name(&self) -> &str {
        "m20260728_000021_split_agent_run_execution"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for SplitAgentRunExecution {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() == DatabaseBackend::Sqlite {
            return rebuild_agent_run_sqlite(manager, true).await;
        }
        let split_shape = render_postgres_check(agent_run_shape_check(true));
        let location = render_postgres_check(agent_run_location_check());
        let statements = [
            "ALTER TABLE agent_run ADD COLUMN tier varchar(16)".to_owned(),
            "ALTER TABLE agent_run ADD COLUMN execution_location varchar(16)".to_owned(),
            "UPDATE agent_run SET tier = CASE execution WHEN 'sandbox' THEN 'background' \
             ELSE 'foreground' END, execution_location = 'in_process'"
                .to_owned(),
            "ALTER TABLE agent_run ALTER COLUMN tier SET NOT NULL".to_owned(),
            "ALTER TABLE agent_run ALTER COLUMN execution_location SET NOT NULL".to_owned(),
            // The baseline's shape constraint is unnamed, so find it by the
            // column it constrains. No other check mentions `execution`, and
            // the replacement constraints are added only after this drop.
            "DO $$
             DECLARE name text;
             BEGIN
                 FOR name IN
                     SELECT conname FROM pg_constraint
                     WHERE conrelid = 'agent_run'::regclass
                       AND contype = 'c'
                       AND pg_get_constraintdef(oid) LIKE '%execution%'
                 LOOP
                     EXECUTE format('ALTER TABLE agent_run DROP CONSTRAINT %I', name);
                 END LOOP;
             END $$"
                .to_owned(),
            format!(
                "ALTER TABLE agent_run ADD CONSTRAINT chk_agent_run_shape CHECK ({split_shape})"
            ),
            format!(
                "ALTER TABLE agent_run ADD CONSTRAINT chk_agent_run_execution_location \
                 CHECK ({location})"
            ),
            "DROP INDEX idx_agent_run_one_foreground".to_owned(),
            "CREATE UNIQUE INDEX idx_agent_run_one_foreground ON agent_run (chat_id) \
             WHERE tier = 'foreground'"
                .to_owned(),
            "ALTER TABLE agent_run DROP COLUMN execution".to_owned(),
        ];
        for statement in statements {
            manager
                .get_connection()
                .execute_unprepared(&statement)
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() == DatabaseBackend::Sqlite {
            return rebuild_agent_run_sqlite(manager, false).await;
        }
        let legacy_shape = render_postgres_check(agent_run_shape_check(false));
        let statements = [
            "ALTER TABLE agent_run ADD COLUMN execution varchar(16)".to_owned(),
            "UPDATE agent_run SET execution = CASE tier WHEN 'background' THEN 'sandbox' \
             ELSE 'foreground' END"
                .to_owned(),
            "ALTER TABLE agent_run ALTER COLUMN execution SET NOT NULL".to_owned(),
            "ALTER TABLE agent_run DROP CONSTRAINT chk_agent_run_shape".to_owned(),
            "ALTER TABLE agent_run DROP CONSTRAINT chk_agent_run_execution_location".to_owned(),
            format!("ALTER TABLE agent_run ADD CONSTRAINT agent_run_check CHECK ({legacy_shape})"),
            "DROP INDEX idx_agent_run_one_foreground".to_owned(),
            "CREATE UNIQUE INDEX idx_agent_run_one_foreground ON agent_run (chat_id) \
             WHERE execution = 'foreground'"
                .to_owned(),
            "ALTER TABLE agent_run DROP COLUMN tier".to_owned(),
            "ALTER TABLE agent_run DROP COLUMN execution_location".to_owned(),
        ];
        for statement in statements {
            manager
                .get_connection()
                .execute_unprepared(&statement)
                .await?;
        }
        Ok(())
    }
}

/// The table name used while rebuilding `agent_run` on SQLite.
const AGENT_RUN_REBUILD: &str = "agent_run_split";

async fn rebuild_agent_run_sqlite(manager: &SchemaManager<'_>, split: bool) -> Result<(), DbErr> {
    let mut statements = vec![
        "PRAGMA foreign_keys=OFF".to_owned(),
        "BEGIN IMMEDIATE".to_owned(),
        agent_run_rebuild_table(split).to_string(SqliteQueryBuilder),
        agent_run_rebuild_copy_sql(split),
        "DROP TABLE agent_run".to_owned(),
        format!("ALTER TABLE {AGENT_RUN_REBUILD} RENAME TO agent_run"),
    ];
    statements.extend(
        agent_run_rebuild_indexes(split)
            .iter()
            .map(|index| index.to_string(SqliteQueryBuilder)),
    );
    statements.push("COMMIT".to_owned());
    statements.push("PRAGMA foreign_keys=ON".to_owned());
    manager
        .get_connection()
        .execute_unprepared(&format!("{};", statements.join(";\n")))
        .await?;
    Ok(())
}

/// Widens `agent_run.execution_location` from `in_process` only to
/// `in_process | container`, so a background run can be admitted to run inside a
/// sandbox-resident container (issue #874). Purely a domain widening: no row
/// changes, and the in-process scheduler already selects only `in_process` runs,
/// so nothing it advances is affected. The `down` narrows the domain back, which
/// is safe only while no `container` rows exist.
///
/// SQLite cannot alter a CHECK constraint, so it rebuilds the table with the two
/// post-split columns copied straight across (the split already retired the old
/// `execution` column) under the widened location check; PostgreSQL drops and
/// re-adds the named constraint in place.
struct AllowContainerExecutionLocation;

impl MigrationName for AllowContainerExecutionLocation {
    fn name(&self) -> &str {
        "m20260729_000027_allow_container_execution_location"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AllowContainerExecutionLocation {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() == DatabaseBackend::Sqlite {
            return rebuild_agent_run_sqlite_relocate(manager, true).await;
        }
        let location = render_postgres_check(agent_run_location_check_container());
        for statement in [
            "ALTER TABLE agent_run DROP CONSTRAINT chk_agent_run_execution_location".to_owned(),
            format!(
                "ALTER TABLE agent_run ADD CONSTRAINT chk_agent_run_execution_location \
                 CHECK ({location})"
            ),
        ] {
            manager
                .get_connection()
                .execute_unprepared(&statement)
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Narrowing the domain is only meaningful with no `container` rows left.
        // Refuse up front with a clear error rather than failing partway through
        // — on SQLite a mid-rebuild failure would strand the scratch table and
        // break the next `up`, and on PostgreSQL it would leave the location
        // column with no constraint at all.
        reject_down_with_container_rows(manager).await?;
        if manager.get_database_backend() == DatabaseBackend::Sqlite {
            return rebuild_agent_run_sqlite_relocate(manager, false).await;
        }
        // Re-add before dropping, under a distinct name, so no window exists in
        // which the location column is unconstrained: if either statement fails,
        // the migration's transaction rolls back with a constraint still in
        // place. The narrow constraint then takes the canonical name.
        let location = render_postgres_check(agent_run_location_check());
        for statement in [
            format!(
                "ALTER TABLE agent_run ADD CONSTRAINT chk_agent_run_execution_location_narrow \
                 CHECK ({location})"
            ),
            "ALTER TABLE agent_run DROP CONSTRAINT chk_agent_run_execution_location".to_owned(),
            "ALTER TABLE agent_run RENAME CONSTRAINT chk_agent_run_execution_location_narrow \
             TO chk_agent_run_execution_location"
                .to_owned(),
        ] {
            manager
                .get_connection()
                .execute_unprepared(&statement)
                .await?;
        }
        Ok(())
    }
}

/// Refuse a rollback of the container-location widening while any `container`
/// row remains, on either backend.
///
/// Without this the narrowing fails deep inside the rebuild (SQLite) or the
/// constraint validation (PostgreSQL), leaving a half-migrated schema. Failing
/// here instead names the actual problem and leaves the schema untouched.
async fn reject_down_with_container_rows(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let conn = manager.get_connection();
    let backend = manager.get_database_backend();
    // sea-orm 2.0: `query_one` takes a `StatementBuilder`; a prepared raw
    // `Statement` goes through `query_one_raw`.
    let remaining = conn
        .query_one_raw(sea_orm::Statement::from_string(
            backend,
            "SELECT COUNT(*) AS remaining FROM agent_run WHERE execution_location = 'container'",
        ))
        .await?;
    let remaining = match remaining {
        // The count comes back as i64 on both backends.
        Some(row) => row.try_get::<i64>("", "remaining")?,
        None => 0,
    };
    if remaining > 0 {
        return Err(DbErr::Custom(format!(
            "cannot narrow agent_run.execution_location: {remaining} container run(s) remain; \
             terminalize or delete them before rolling this migration back"
        )));
    }
    Ok(())
}

/// Rebuild the post-split `agent_run` table under a chosen execution-location
/// domain, copying the two split columns straight across (unlike
/// [`rebuild_agent_run_sqlite`], which maps the retired `execution` column).
async fn rebuild_agent_run_sqlite_relocate(
    manager: &SchemaManager<'_>,
    allow_container: bool,
) -> Result<(), DbErr> {
    let columns = "id, chat_id, parent_id, parent_depth, spawn_call_id, tier, \
         execution_location, depth, status, input, model, attempt_count, max_attempts, \
         claim_count, available_at, deadline_at, lease_token, lease_expires_at, started_at, \
         finished_at, last_error_code, last_error_detail, created_at, updated_at";
    let mut statements = vec![
        "PRAGMA foreign_keys=OFF".to_owned(),
        "BEGIN IMMEDIATE".to_owned(),
        agent_run_rebuild_table_with_location(true, allow_container).to_string(SqliteQueryBuilder),
        format!("INSERT INTO {AGENT_RUN_REBUILD} ({columns}) SELECT {columns} FROM agent_run"),
        "DROP TABLE agent_run".to_owned(),
        format!("ALTER TABLE {AGENT_RUN_REBUILD} RENAME TO agent_run"),
    ];
    statements.extend(
        agent_run_rebuild_indexes(true)
            .iter()
            .map(|index| index.to_string(SqliteQueryBuilder)),
    );
    statements.push("COMMIT".to_owned());
    statements.push("PRAGMA foreign_keys=ON".to_owned());
    manager
        .get_connection()
        .execute_unprepared(&format!("{};", statements.join(";\n")))
        .await?;
    Ok(())
}

fn agent_run_rebuild_copy_sql(split: bool) -> String {
    let (new_columns, mapped) = if split {
        (
            "tier, execution_location",
            "CASE execution WHEN 'sandbox' THEN 'background' ELSE 'foreground' END, 'in_process'",
        )
    } else {
        (
            "execution",
            "CASE tier WHEN 'background' THEN 'sandbox' ELSE 'foreground' END",
        )
    };
    format!(
        "INSERT INTO {AGENT_RUN_REBUILD} \
         (id, chat_id, parent_id, parent_depth, spawn_call_id, {new_columns}, depth, status, \
          input, model, attempt_count, max_attempts, claim_count, available_at, deadline_at, \
          lease_token, lease_expires_at, started_at, finished_at, last_error_code, \
          last_error_detail, created_at, updated_at) \
         SELECT id, chat_id, parent_id, parent_depth, spawn_call_id, {mapped}, depth, status, \
          input, model, attempt_count, max_attempts, claim_count, available_at, deadline_at, \
          lease_token, lease_expires_at, started_at, finished_at, last_error_code, \
          last_error_detail, created_at, updated_at \
         FROM agent_run"
    )
}

fn agent_run_rebuild_table(split: bool) -> TableCreateStatement {
    agent_run_rebuild_table_with_location(split, false)
}

/// [`agent_run_rebuild_table`] with an explicit choice of location-domain check,
/// so the container-location migration can rebuild the table with the widened
/// domain while every historical caller keeps the strict one.
fn agent_run_rebuild_table_with_location(
    split: bool,
    allow_container: bool,
) -> TableCreateStatement {
    let rebuild = Alias::new(AGENT_RUN_REBUILD);
    let mut statement = Table::create();
    statement
        .table(rebuild.clone())
        .col(ColumnDef::new(AgentRun::Id).uuid().not_null())
        .col(ColumnDef::new(AgentRun::ChatId).uuid().not_null())
        .col(ColumnDef::new(AgentRun::ParentId).uuid())
        .col(ColumnDef::new(AgentRun::ParentDepth).small_integer())
        .col(ColumnDef::new(AgentRun::SpawnCallId).uuid());
    if split {
        statement
            .col(ColumnDef::new(AgentRun::Tier).string_len(16).not_null())
            .col(
                ColumnDef::new(AgentRun::ExecutionLocation)
                    .string_len(16)
                    .not_null(),
            );
    } else {
        statement.col(
            ColumnDef::new(AgentRun::Execution)
                .string_len(16)
                .not_null(),
        );
    }
    statement
        .col(ColumnDef::new(AgentRun::Depth).small_integer().not_null())
        .col(ColumnDef::new(AgentRun::Status).string_len(32).not_null())
        .col(ColumnDef::new(AgentRun::Input).text())
        .col(
            ColumnDef::new(AgentRun::Model)
                .string_len(crate::model::AgentRun::MAX_MODEL_LEN as u32),
        )
        .col(ColumnDef::new(AgentRun::AttemptCount).integer().not_null())
        .col(ColumnDef::new(AgentRun::MaxAttempts).integer().not_null())
        .col(ColumnDef::new(AgentRun::ClaimCount).integer().not_null())
        .col(
            ColumnDef::new(AgentRun::AvailableAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(ColumnDef::new(AgentRun::DeadlineAt).timestamp_with_time_zone())
        .col(ColumnDef::new(AgentRun::LeaseToken).uuid())
        .col(ColumnDef::new(AgentRun::LeaseExpiresAt).timestamp_with_time_zone())
        .col(ColumnDef::new(AgentRun::StartedAt).timestamp_with_time_zone())
        .col(ColumnDef::new(AgentRun::FinishedAt).timestamp_with_time_zone())
        .col(
            ColumnDef::new(AgentRun::LastErrorCode)
                .string_len(crate::model::AgentRun::MAX_ERROR_CODE_LEN as u32),
        )
        .col(
            ColumnDef::new(AgentRun::LastErrorDetail)
                .string_len(crate::model::AgentRun::MAX_ERROR_DETAIL_LEN as u32),
        )
        .col(
            ColumnDef::new(AgentRun::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(
            ColumnDef::new(AgentRun::UpdatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .primary_key(
            Index::create()
                .name("pk_agent_run")
                .col(AgentRun::Id)
                .col(AgentRun::ChatId)
                .col(AgentRun::Depth),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_agent_run_chat")
                .from(rebuild.clone(), AgentRun::ChatId)
                .to(Chat::Table, Chat::Id)
                .on_delete(ForeignKeyAction::Restrict),
        )
        // The self-reference names the final table: with foreign-key
        // enforcement off, neither the drop nor the rename rewrites
        // REFERENCES clauses, so it resolves to this table once renamed.
        .foreign_key(
            ForeignKey::create()
                .name("fk_agent_run_parent")
                .from_tbl(rebuild.clone())
                .from_col(AgentRun::ParentId)
                .from_col(AgentRun::ChatId)
                .from_col(AgentRun::ParentDepth)
                .to_tbl(AgentRun::Table)
                .to_col(AgentRun::Id)
                .to_col(AgentRun::ChatId)
                .to_col(AgentRun::Depth)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_agent_run_live_claim")
                .from_tbl(rebuild)
                .from_col(AgentRun::LeaseToken)
                .from_col(AgentRun::Id)
                .from_col(AgentRun::AttemptCount)
                .from_col(AgentRun::ClaimCount)
                .to_tbl(AgentRunClaim::Table)
                .to_col(AgentRunClaim::Token)
                .to_col(AgentRunClaim::AgentRunId)
                .to_col(AgentRunClaim::AttemptCount)
                .to_col(AgentRunClaim::ClaimCount)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .check(agent_run_shape_check(split))
        .check(agent_run_lease_check())
        .check(agent_run_finished_check())
        .check(agent_run_error_check())
        .check(
            Expr::col(AgentRun::LastErrorDetail)
                .is_null()
                .or(Expr::col(AgentRun::LastErrorCode).is_not_null()),
        )
        .check(Expr::col(AgentRun::UpdatedAt).gte(Expr::col(AgentRun::CreatedAt)));
    if split {
        statement.check(if allow_container {
            agent_run_location_check_container()
        } else {
            agent_run_location_check()
        });
    }
    statement.to_owned()
}

fn agent_run_rebuild_indexes(split: bool) -> Vec<IndexCreateStatement> {
    let one_foreground = if split {
        Expr::col(AgentRun::Tier).eq("foreground")
    } else {
        Expr::col(AgentRun::Execution).eq("foreground")
    };
    vec![
        Index::create()
            .name("idx_agent_run_id")
            .table(AgentRun::Table)
            .col(AgentRun::Id)
            .unique()
            .to_owned(),
        Index::create()
            .name("idx_agent_run_id_parent_chat")
            .table(AgentRun::Table)
            .col(AgentRun::Id)
            .col(AgentRun::ParentId)
            .col(AgentRun::ChatId)
            .unique()
            .to_owned(),
        Index::create()
            .name("idx_agent_run_admission_identity")
            .table(AgentRun::Table)
            .col(AgentRun::Id)
            .col(AgentRun::ParentId)
            .col(AgentRun::ChatId)
            .col(AgentRun::SpawnCallId)
            .unique()
            .to_owned(),
        Index::create()
            .name("idx_agent_run_spawn_call")
            .table(AgentRun::Table)
            .col(AgentRun::SpawnCallId)
            .unique()
            .to_owned(),
        Index::create()
            .name("idx_agent_run_one_foreground")
            .table(AgentRun::Table)
            .col(AgentRun::ChatId)
            .unique()
            .and_where(one_foreground)
            .to_owned(),
        Index::create()
            .name("idx_agent_run_parent")
            .table(AgentRun::Table)
            .col(AgentRun::ParentId)
            .col(AgentRun::CreatedAt)
            .to_owned(),
        Index::create()
            .name("idx_agent_run_chat_history")
            .table(AgentRun::Table)
            .col(AgentRun::ChatId)
            .col(AgentRun::CreatedAt)
            .col(AgentRun::Id)
            .to_owned(),
        Index::create()
            .name("idx_agent_run_claimable")
            .table(AgentRun::Table)
            .col(AgentRun::Status)
            .col(AgentRun::AvailableAt)
            .col(AgentRun::CreatedAt)
            .col(AgentRun::Id)
            .to_owned(),
        Index::create()
            .name("idx_agent_run_live_by_chat")
            .table(AgentRun::Table)
            .col(AgentRun::Status)
            .col(AgentRun::ChatId)
            .col(AgentRun::LeaseExpiresAt)
            .to_owned(),
    ]
}

/// The foreground/background row-shape constraint, expressed over the split
/// tier column (`split`) or the retired execution column (for rollback).
fn agent_run_shape_check(split: bool) -> SimpleExpr {
    let (foreground_marker, background_marker) = if split {
        (
            Expr::col(AgentRun::Tier).eq("foreground"),
            Expr::col(AgentRun::Tier).eq("background"),
        )
    } else {
        (
            Expr::col(AgentRun::Execution).eq("foreground"),
            Expr::col(AgentRun::Execution).eq("sandbox"),
        )
    };
    let foreground_status = Expr::col(AgentRun::Status).is_in([
        AgentRunStatus::Active.as_str(),
        AgentRunStatus::Completed.as_str(),
        AgentRunStatus::Failed.as_str(),
        AgentRunStatus::Cancelled.as_str(),
    ]);
    let background_status = Expr::col(AgentRun::Status).is_in([
        AgentRunStatus::Queued.as_str(),
        AgentRunStatus::Running.as_str(),
        AgentRunStatus::Cancelling.as_str(),
        AgentRunStatus::Waiting.as_str(),
        AgentRunStatus::RetryWait.as_str(),
        AgentRunStatus::Completed.as_str(),
        AgentRunStatus::Failed.as_str(),
        AgentRunStatus::Cancelled.as_str(),
    ]);
    let foreground_shape = foreground_marker
        .and(Expr::col(AgentRun::Depth).eq(0))
        .and(Expr::col(AgentRun::ParentId).is_null())
        .and(Expr::col(AgentRun::ParentDepth).is_null())
        .and(Expr::col(AgentRun::SpawnCallId).is_null())
        .and(Expr::col(AgentRun::Input).is_null())
        .and(Expr::col(AgentRun::AttemptCount).eq(0))
        .and(Expr::col(AgentRun::MaxAttempts).eq(0))
        .and(Expr::col(AgentRun::ClaimCount).eq(0))
        .and(Expr::col(AgentRun::AvailableAt).eq(Expr::col(AgentRun::CreatedAt)))
        .and(Expr::col(AgentRun::DeadlineAt).is_null())
        .and(Expr::col(AgentRun::StartedAt).is_null())
        .and(foreground_status);
    let background_shape = background_marker
        .and(Expr::col(AgentRun::Depth).eq(i32::from(crate::model::AgentRun::MAX_DEPTH)))
        .and(Expr::col(AgentRun::ParentId).is_not_null())
        .and(Expr::col(AgentRun::ParentDepth).eq(0))
        .and(Expr::col(AgentRun::SpawnCallId).is_not_null())
        .and(Expr::col(AgentRun::Input).is_not_null())
        .and(Expr::col(AgentRun::MaxAttempts).gte(1))
        .and(Expr::col(AgentRun::AttemptCount).gte(0))
        .and(Expr::col(AgentRun::AttemptCount).lte(Expr::col(AgentRun::MaxAttempts)))
        .and(Expr::col(AgentRun::ClaimCount).gte(Expr::col(AgentRun::AttemptCount)))
        .and(Expr::col(AgentRun::ClaimCount).lt(i32::MAX))
        .and(Expr::col(AgentRun::AvailableAt).gte(Expr::col(AgentRun::CreatedAt)))
        .and(Expr::col(AgentRun::DeadlineAt).gt(Expr::col(AgentRun::CreatedAt)))
        .and(
            Func::char_length(Expr::col(AgentRun::Input))
                .between(1, crate::model::AgentRun::MAX_INPUT_LEN as i32),
        )
        .and(background_status);
    foreground_shape.or(background_shape)
}

fn agent_run_location_check() -> SimpleExpr {
    Expr::col(AgentRun::ExecutionLocation).eq("in_process")
}

/// The widened execution-location domain: in-process, or resident in a
/// container the sandbox-resident driver provisions and drives.
fn agent_run_location_check_container() -> SimpleExpr {
    Expr::col(AgentRun::ExecutionLocation).is_in(["in_process", "container"])
}

fn agent_run_lease_check() -> SimpleExpr {
    let active_lease = Expr::col(AgentRun::Status)
        .is_in([
            AgentRunStatus::Running.as_str(),
            AgentRunStatus::Cancelling.as_str(),
        ])
        .and(Expr::col(AgentRun::LeaseToken).is_not_null())
        .and(Expr::col(AgentRun::LeaseExpiresAt).is_not_null())
        .and(Expr::col(AgentRun::AttemptCount).gte(1))
        .and(Expr::col(AgentRun::StartedAt).is_not_null());
    let no_lease = Expr::col(AgentRun::Status)
        .is_not_in([
            AgentRunStatus::Running.as_str(),
            AgentRunStatus::Cancelling.as_str(),
        ])
        .and(Expr::col(AgentRun::LeaseToken).is_null())
        .and(Expr::col(AgentRun::LeaseExpiresAt).is_null());
    active_lease.or(no_lease)
}

fn agent_run_finished_check() -> SimpleExpr {
    let terminal_finished = Expr::col(AgentRun::Status)
        .is_in([
            AgentRunStatus::Completed.as_str(),
            AgentRunStatus::Failed.as_str(),
            AgentRunStatus::Cancelled.as_str(),
        ])
        .and(Expr::col(AgentRun::FinishedAt).is_not_null());
    let nonterminal_unfinished = Expr::col(AgentRun::Status)
        .is_in([
            AgentRunStatus::Active.as_str(),
            AgentRunStatus::Queued.as_str(),
            AgentRunStatus::Running.as_str(),
            AgentRunStatus::Cancelling.as_str(),
            AgentRunStatus::Waiting.as_str(),
            AgentRunStatus::RetryWait.as_str(),
        ])
        .and(Expr::col(AgentRun::FinishedAt).is_null());
    terminal_finished.or(nonterminal_unfinished)
}

fn agent_run_error_check() -> SimpleExpr {
    let failure_has_error = Expr::col(AgentRun::Status)
        .is_in([
            AgentRunStatus::RetryWait.as_str(),
            AgentRunStatus::Failed.as_str(),
        ])
        .and(Expr::col(AgentRun::LastErrorCode).is_not_null());
    let success_has_no_error = Expr::col(AgentRun::Status)
        .is_not_in([
            AgentRunStatus::RetryWait.as_str(),
            AgentRunStatus::Failed.as_str(),
        ])
        .and(Expr::col(AgentRun::LastErrorCode).is_null())
        .and(Expr::col(AgentRun::LastErrorDetail).is_null());
    failure_has_error.or(success_has_no_error)
}

/// Renders a check expression as PostgreSQL SQL for `ADD CONSTRAINT`.
///
/// sea-query offers no standalone expression renderer, so this prints the
/// expression through a bare `SELECT` and strips the keyword.
fn render_postgres_check(check: SimpleExpr) -> String {
    let rendered = Query::select().expr(check).to_string(PostgresQueryBuilder);
    rendered
        .strip_prefix("SELECT ")
        .expect("a bare select renders its expression after the keyword")
        .to_owned()
}

/// Retains the renderer projection of what a tool call produced.
///
/// Terminal activity was rebuilt from the stored failure code alone, which
/// recovers one enumerated setup signal and nothing else — so reopening a chat
/// lost every result card, including a command's own output. The projection is
/// already closed and clamped when it is built, so what lands here is exactly
/// what crossed the boundary live and nothing more.
///
/// Deliberately the *projected* form rather than the tool output it was built
/// from. Brightwave persists the internal result and projects on read, which
/// lets the projection change without a migration; that trade needs a boundary
/// that keeps arbitrary tool data out on the way *out*, and ours keeps it out
/// on the way *in*. Storing only what already crossed means a later bug in the
/// read path has nothing to leak.
///
/// Nullable because every call resolved before this migration has no projection
/// to recover, and because most calls never project one at all.
struct AddToolResultPreviews;

impl MigrationName for AddToolResultPreviews {
    fn name(&self) -> &str {
        "m20260728_000020_add_tool_result_previews"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddToolResultPreviews {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ToolCall::Table)
                    .add_column(ColumnDef::new(ToolCall::ResultPreview).json_binary())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ToolCall::Table)
                    .drop_column(ToolCall::ResultPreview)
                    .to_owned(),
            )
            .await
    }
}

/// Records the model a sandbox run executes against.
///
/// The column is nullable because runs admitted before this migration have no
/// recorded selection, and because foreground coordinators carry their model on
/// each turn instead.
struct AddAgentRunModel;

impl MigrationName for AddAgentRunModel {
    fn name(&self) -> &str {
        "m20260728_000019_add_agent_run_model"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddAgentRunModel {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AgentRun::Table)
                    .add_column(
                        ColumnDef::new(AgentRun::Model)
                            .string_len(crate::model::AgentRun::MAX_MODEL_LEN as u32),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AgentRun::Table)
                    .drop_column(AgentRun::Model)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

/// Keeps maintenance-model usage out of foreground turn accounting.
struct AddContextCheckpointUsage;

impl MigrationName for AddContextCheckpointUsage {
    fn name(&self) -> &str {
        "m20260728_000018_add_context_checkpoint_usage"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddContextCheckpointUsage {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in [
            ContextCheckpoint::InputTokens,
            ContextCheckpoint::OutputTokens,
            ContextCheckpoint::CacheReadInputTokens,
            ContextCheckpoint::CacheCreationInputTokens,
        ] {
            manager
                .alter_table(
                    Table::alter()
                        .table(ContextCheckpoint::Table)
                        .add_column(ColumnDef::new(column).big_integer().not_null().default(0))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in [
            ContextCheckpoint::InputTokens,
            ContextCheckpoint::OutputTokens,
            ContextCheckpoint::CacheReadInputTokens,
            ContextCheckpoint::CacheCreationInputTokens,
        ] {
            manager
                .alter_table(
                    Table::alter()
                        .table(ContextCheckpoint::Table)
                        .drop_column(column)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

/// Persists chat-scoped standing consent for Sensitive tool calls.
///
/// A grant names only the closed preview scope the reviewer selected. The
/// source call id makes retrying a decision idempotent without permitting a
/// later retry to widen a one-shot approval after the call has run.
struct AddStandingToolGrants;

impl MigrationName for AddStandingToolGrants {
    fn name(&self) -> &str {
        "m20260727_000017_add_standing_tool_grants"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddStandingToolGrants {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ToolCall::Table)
                    .add_column(ColumnDef::new(ToolCall::ApprovalGrantSourceCallId).uuid())
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(StandingToolGrant::Table)
                    .col(
                        ColumnDef::new(StandingToolGrant::SourceCallId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(StandingToolGrant::ChatId).uuid().not_null())
                    .col(
                        ColumnDef::new(StandingToolGrant::ToolName)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(StandingToolGrant::ApprovalKind)
                            .string_len(64)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(StandingToolGrant::Scope)
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(StandingToolGrant::GrantedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_standing_tool_grant_chat")
                            .from(StandingToolGrant::Table, StandingToolGrant::ChatId)
                            .to(Chat::Table, Chat::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .check(
                        Func::char_length(Expr::col(StandingToolGrant::ToolName))
                            .between(1, crate::model::ToolCallRecord::MAX_LABEL_LEN as i32),
                    )
                    .check(
                        Expr::col(StandingToolGrant::ApprovalKind).is_in([
                            crate::ToolApprovalKind::SearchMayShareQueryAndExcerpts
                                .standing_grant_key(),
                            crate::ToolApprovalKind::WebSearchMayShareQuery.standing_grant_key(),
                            crate::ToolApprovalKind::ExecMayRunNetworkedCommand
                                .standing_grant_key(),
                        ]),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_standing_tool_grant_lookup")
                    .table(StandingToolGrant::Table)
                    .col(StandingToolGrant::ChatId)
                    .col(StandingToolGrant::ToolName)
                    .col(StandingToolGrant::ApprovalKind)
                    .col(StandingToolGrant::GrantedAt)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(StandingToolGrant::Table).to_owned())
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ToolCall::Table)
                    .drop_column(ToolCall::ApprovalGrantSourceCallId)
                    .to_owned(),
            )
            .await
    }
}

struct AddUserQuestions;

impl MigrationName for AddUserQuestions {
    fn name(&self) -> &str {
        "m20260724_000012_add_user_questions"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddUserQuestions {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(UserQuestionRequest::Table)
                    .col(
                        ColumnDef::new(UserQuestionRequest::CallId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(UserQuestionRequest::TurnId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(UserQuestionRequest::ChatId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(UserQuestionRequest::Status)
                            .string_len(16)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(UserQuestionRequest::EventSeq)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(UserQuestionRequest::AskedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(UserQuestionRequest::ResolvedAt).timestamp_with_time_zone())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_user_question_request_call")
                            .from(UserQuestionRequest::Table, UserQuestionRequest::CallId)
                            .to(ToolCall::Table, ToolCall::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_user_question_request_turn")
                            .from(UserQuestionRequest::Table, UserQuestionRequest::TurnId)
                            .to(TurnRun::Table, TurnRun::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_user_question_request_chat")
                            .from(UserQuestionRequest::Table, UserQuestionRequest::ChatId)
                            .to(Chat::Table, Chat::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_user_question_request_event")
                            .from_tbl(UserQuestionRequest::Table)
                            .from_col(UserQuestionRequest::ChatId)
                            .from_col(UserQuestionRequest::EventSeq)
                            .to_tbl(Event::Table)
                            .to_col(Event::ChatId)
                            .to_col(Event::Seq)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .check(Expr::col(UserQuestionRequest::Status).is_in([
                        crate::UserQuestionRequestStatus::Pending.as_str(),
                        crate::UserQuestionRequestStatus::Answered.as_str(),
                        crate::UserQuestionRequestStatus::Cancelled.as_str(),
                    ]))
                    .check(
                        Expr::col(UserQuestionRequest::Status)
                            .eq(crate::UserQuestionRequestStatus::Pending.as_str())
                            .and(Expr::col(UserQuestionRequest::ResolvedAt).is_null())
                            .or(Expr::col(UserQuestionRequest::Status)
                                .ne(crate::UserQuestionRequestStatus::Pending.as_str())
                                .and(Expr::col(UserQuestionRequest::ResolvedAt).is_not_null())),
                    )
                    .check(
                        Expr::col(UserQuestionRequest::ResolvedAt)
                            .is_null()
                            .or(Expr::col(UserQuestionRequest::ResolvedAt)
                                .gte(Expr::col(UserQuestionRequest::AskedAt))),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_user_question_request_pending")
                    .table(UserQuestionRequest::Table)
                    .col(UserQuestionRequest::ChatId)
                    .col(UserQuestionRequest::AskedAt)
                    .col(UserQuestionRequest::CallId)
                    .and_where(
                        Expr::col(UserQuestionRequest::Status)
                            .eq(crate::UserQuestionRequestStatus::Pending.as_str()),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_user_question_request_chat")
                    .table(UserQuestionRequest::Table)
                    .col(UserQuestionRequest::ChatId)
                    .col(UserQuestionRequest::CallId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_user_question_request_turn")
                    .table(UserQuestionRequest::Table)
                    .col(UserQuestionRequest::TurnId)
                    .col(UserQuestionRequest::CallId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_user_question_request_event")
                    .table(UserQuestionRequest::Table)
                    .col(UserQuestionRequest::ChatId)
                    .col(UserQuestionRequest::EventSeq)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(UserQuestion::Table)
                    .col(ColumnDef::new(UserQuestion::CallId).uuid().not_null())
                    .col(
                        ColumnDef::new(UserQuestion::QuestionId)
                            .string_len(crate::MAX_QUESTION_ID_CHARS as u32)
                            .not_null(),
                    )
                    .col(ColumnDef::new(UserQuestion::Position).integer().not_null())
                    .col(
                        ColumnDef::new(UserQuestion::Header)
                            .string_len(crate::MAX_QUESTION_HEADER_CHARS as u32)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(UserQuestion::Prompt)
                            .string_len(crate::MAX_QUESTION_PROMPT_CHARS as u32)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(UserQuestion::Options)
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(UserQuestion::AllowFreeForm)
                            .boolean()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(UserQuestion::AnswerOptionId)
                            .string_len(crate::MAX_QUESTION_OPTION_ID_CHARS as u32),
                    )
                    .col(
                        ColumnDef::new(UserQuestion::AnswerFreeForm)
                            .string_len(crate::MAX_FREE_FORM_ANSWER_CHARS as u32),
                    )
                    .col(ColumnDef::new(UserQuestion::AnsweredAt).timestamp_with_time_zone())
                    .primary_key(
                        Index::create()
                            .name("pk_user_question")
                            .col(UserQuestion::CallId)
                            .col(UserQuestion::QuestionId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_user_question_request")
                            .from(UserQuestion::Table, UserQuestion::CallId)
                            .to(UserQuestionRequest::Table, UserQuestionRequest::CallId)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .check(
                        Expr::col(UserQuestion::Position)
                            .between(0, crate::MAX_USER_QUESTIONS as i32 - 1),
                    )
                    .check(
                        Expr::col(UserQuestion::AnswerOptionId)
                            .is_null()
                            .and(Expr::col(UserQuestion::AnswerFreeForm).is_null())
                            .and(Expr::col(UserQuestion::AnsweredAt).is_null())
                            .or(Expr::col(UserQuestion::AnswerOptionId)
                                .is_not_null()
                                .and(Expr::col(UserQuestion::AnswerFreeForm).is_null())
                                .and(Expr::col(UserQuestion::AnsweredAt).is_not_null()))
                            .or(Expr::col(UserQuestion::AnswerOptionId)
                                .is_null()
                                .and(Expr::col(UserQuestion::AnswerFreeForm).is_not_null())
                                .and(Expr::col(UserQuestion::AnsweredAt).is_not_null())),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_user_question_order")
                    .table(UserQuestion::Table)
                    .col(UserQuestion::CallId)
                    .col(UserQuestion::Position)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(UserQuestion::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(UserQuestionRequest::Table).to_owned())
            .await
    }
}

/// Gives conversation outputs a durable record with an opaque identity and an
/// append-only revision history.
///
/// Before this, an output was a filename in private scratch and an update
/// replaced the previous bytes in place. Existing loose files stay on disk but
/// are no longer a catalog: the record is authoritative from here on.
struct AddConversationOutputs;

impl MigrationName for AddConversationOutputs {
    fn name(&self) -> &str {
        "m20260724_000013_add_conversation_outputs"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddConversationOutputs {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Output::Table)
                    .col(ColumnDef::new(Output::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Output::ChatId).uuid().not_null())
                    .col(ColumnDef::new(Output::Filename).text().not_null())
                    .col(ColumnDef::new(Output::MediaType).text().not_null())
                    .col(ColumnDef::new(Output::CurrentRevisionId).uuid().not_null())
                    .col(ColumnDef::new(Output::RevisionCount).integer().not_null())
                    .col(
                        ColumnDef::new(Output::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Output::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Output::DeletedAt).timestamp_with_time_zone())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_output_chat")
                            .from_tbl(Output::Table)
                            .from_col(Output::ChatId)
                            .to_tbl(Chat::Table)
                            .to_col(Chat::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .check(
                        Expr::col(Output::RevisionCount)
                            .between(1, crate::deliverable::MAX_OUTPUT_REVISIONS as i32),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_output_chat_created")
                    .table(Output::Table)
                    .col(Output::ChatId)
                    .col(Output::CreatedAt)
                    .col(Output::Id)
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(OutputRevision::Table)
                    .col(
                        ColumnDef::new(OutputRevision::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(OutputRevision::OutputId).uuid().not_null())
                    .col(ColumnDef::new(OutputRevision::Ordinal).integer().not_null())
                    .col(
                        ColumnDef::new(OutputRevision::ByteLen)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OutputRevision::Sha256)
                            .binary_len(32)
                            .not_null(),
                    )
                    .col(ColumnDef::new(OutputRevision::TurnId).uuid())
                    .col(
                        ColumnDef::new(OutputRevision::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_output_revision_output")
                            .from_tbl(OutputRevision::Table)
                            .from_col(OutputRevision::OutputId)
                            .to_tbl(Output::Table)
                            .to_col(Output::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .check(
                        Expr::col(OutputRevision::Ordinal)
                            .between(1, crate::deliverable::MAX_OUTPUT_REVISIONS as i32),
                    )
                    .check(Expr::col(OutputRevision::ByteLen).gte(0))
                    .check(
                        Expr::col(OutputRevision::ByteLen)
                            .lte(crate::deliverable::MAX_DELIVERABLE_BYTES as i64),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_output_revision_ordinal")
                    .table(OutputRevision::Table)
                    .col(OutputRevision::OutputId)
                    .col(OutputRevision::Ordinal)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(OutputRevision::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Output::Table).to_owned())
            .await
    }
}

/// Persists the bounded evidence lineage of each immutable output revision.
///
/// The cited evidence is scoped to the producing chat and turn, rather than
/// relying on a source token alone. This preserves the exact source sequence
/// without allowing a revision to reference another conversation's evidence.
struct AddOutputRevisionCitations;

impl MigrationName for AddOutputRevisionCitations {
    fn name(&self) -> &str {
        "m20260727_000015_add_output_revision_citations"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddOutputRevisionCitations {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(OutputRevisionCitation::Table)
                    .col(
                        ColumnDef::new(OutputRevisionCitation::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(OutputRevisionCitation::OutputRevisionId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OutputRevisionCitation::Ordinal)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OutputRevisionCitation::ChatId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OutputRevisionCitation::TurnId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OutputRevisionCitation::EvidenceCallId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OutputRevisionCitation::EvidenceRank)
                            .integer()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_output_revision_citation_revision")
                            .from_tbl(OutputRevisionCitation::Table)
                            .from_col(OutputRevisionCitation::OutputRevisionId)
                            .to_tbl(OutputRevision::Table)
                            .to_col(OutputRevision::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_output_revision_citation_evidence")
                            .from_tbl(OutputRevisionCitation::Table)
                            .from_col(OutputRevisionCitation::EvidenceCallId)
                            .from_col(OutputRevisionCitation::EvidenceRank)
                            .from_col(OutputRevisionCitation::ChatId)
                            .from_col(OutputRevisionCitation::TurnId)
                            .to_tbl(RetrievalEvidence::Table)
                            .to_col(RetrievalEvidence::CallId)
                            .to_col(RetrievalEvidence::Rank)
                            .to_col(RetrievalEvidence::ChatId)
                            .to_col(RetrievalEvidence::TurnId)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .check(Expr::col(OutputRevisionCitation::Ordinal).between(1, 64))
                    .check(Expr::col(OutputRevisionCitation::EvidenceRank).between(1, 20))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_output_revision_citation_ordinal")
                    .table(OutputRevisionCitation::Table)
                    .col(OutputRevisionCitation::OutputRevisionId)
                    .col(OutputRevisionCitation::Ordinal)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_output_revision_citation_evidence")
                    .table(OutputRevisionCitation::Table)
                    .col(OutputRevisionCitation::OutputRevisionId)
                    .col(OutputRevisionCitation::EvidenceCallId)
                    .col(OutputRevisionCitation::EvidenceRank)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(OutputRevisionCitation::Table)
                    .to_owned(),
            )
            .await
    }
}

/// Extends the output-revision shape so the record can hold host-accepted binary
/// workspace artifacts and can attribute a revision to a producing background
/// run.
///
/// Two additive changes to `output_revision`:
///
/// - `producing_run_id` (nullable) records the background run that produced a
///   revision, alongside the existing `turn_id`. A revision names at most one
///   producer, enforced by a check; existing rows keep only their `turn_id`.
/// - the coarse `byte_len` upper bound is raised from the 512 KiB text cap to
///   the 16 MiB binary cap. The tight per-kind ceiling (text stays 512 KiB) is
///   enforced in application validation; the database bound is the outer limit
///   that admits a binary artifact at all.
///
/// SQLite cannot alter a check constraint in place, so it rebuilds the table
/// under foreign-key suppression the same way the agent-run split does; the
/// `output_revision_citation` foreign key resolves to the rebuilt table once it
/// is renamed. PostgreSQL alters the column and constraints directly. The `down`
/// is symmetric and restores the original shape.
struct ExtendOutputRevisionsForBinary;

impl MigrationName for ExtendOutputRevisionsForBinary {
    fn name(&self) -> &str {
        "m20260729_000023_extend_output_revisions_for_binary"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for ExtendOutputRevisionsForBinary {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() == DatabaseBackend::Sqlite {
            return rebuild_output_revision_sqlite(manager, true).await;
        }
        let statements = [
            "ALTER TABLE output_revision ADD COLUMN producing_run_id uuid".to_owned(),
            // The baseline's byte_len checks are unnamed; drop both by the column
            // they constrain, then re-add named ones so a re-up after down is
            // idempotent.
            "DO $$
             DECLARE name text;
             BEGIN
                 FOR name IN
                     SELECT conname FROM pg_constraint
                     WHERE conrelid = 'output_revision'::regclass
                       AND contype = 'c'
                       AND pg_get_constraintdef(oid) LIKE '%byte_len%'
                 LOOP
                     EXECUTE format('ALTER TABLE output_revision DROP CONSTRAINT %I', name);
                 END LOOP;
             END $$"
                .to_owned(),
            "ALTER TABLE output_revision ADD CONSTRAINT chk_output_revision_byte_len_nonneg \
             CHECK (byte_len >= 0)"
                .to_owned(),
            format!(
                "ALTER TABLE output_revision ADD CONSTRAINT chk_output_revision_byte_len_max \
                 CHECK (byte_len <= {})",
                crate::deliverable::MAX_BINARY_DELIVERABLE_BYTES as i64
            ),
            "ALTER TABLE output_revision ADD CONSTRAINT chk_output_revision_producer \
             CHECK (turn_id IS NULL OR producing_run_id IS NULL)"
                .to_owned(),
        ];
        for statement in statements {
            manager
                .get_connection()
                .execute_unprepared(&statement)
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() == DatabaseBackend::Sqlite {
            return rebuild_output_revision_sqlite(manager, false).await;
        }
        let statements = [
            "ALTER TABLE output_revision DROP CONSTRAINT chk_output_revision_producer".to_owned(),
            "DO $$
             DECLARE name text;
             BEGIN
                 FOR name IN
                     SELECT conname FROM pg_constraint
                     WHERE conrelid = 'output_revision'::regclass
                       AND contype = 'c'
                       AND pg_get_constraintdef(oid) LIKE '%byte_len%'
                 LOOP
                     EXECUTE format('ALTER TABLE output_revision DROP CONSTRAINT %I', name);
                 END LOOP;
             END $$"
                .to_owned(),
            "ALTER TABLE output_revision ADD CONSTRAINT chk_output_revision_byte_len_nonneg \
             CHECK (byte_len >= 0)"
                .to_owned(),
            format!(
                "ALTER TABLE output_revision ADD CONSTRAINT chk_output_revision_byte_len_max \
                 CHECK (byte_len <= {})",
                crate::deliverable::MAX_DELIVERABLE_BYTES as i64
            ),
            "ALTER TABLE output_revision DROP COLUMN producing_run_id".to_owned(),
        ];
        for statement in statements {
            manager
                .get_connection()
                .execute_unprepared(&statement)
                .await?;
        }
        Ok(())
    }
}

/// The table name used while rebuilding `output_revision` on SQLite.
const OUTPUT_REVISION_REBUILD: &str = "output_revision_rebuild";

async fn rebuild_output_revision_sqlite(
    manager: &SchemaManager<'_>,
    binary: bool,
) -> Result<(), DbErr> {
    let statements = vec![
        "PRAGMA foreign_keys=OFF".to_owned(),
        "BEGIN IMMEDIATE".to_owned(),
        output_revision_rebuild_table(binary).to_string(SqliteQueryBuilder),
        output_revision_rebuild_copy_sql(binary),
        "DROP TABLE output_revision".to_owned(),
        format!("ALTER TABLE {OUTPUT_REVISION_REBUILD} RENAME TO output_revision"),
        output_revision_ordinal_index().to_string(SqliteQueryBuilder),
        "COMMIT".to_owned(),
        "PRAGMA foreign_keys=ON".to_owned(),
    ];
    manager
        .get_connection()
        .execute_unprepared(&format!("{};", statements.join(";\n")))
        .await?;
    Ok(())
}

fn output_revision_rebuild_table(binary: bool) -> TableCreateStatement {
    let rebuild = Alias::new(OUTPUT_REVISION_REBUILD);
    let byte_ceiling = if binary {
        crate::deliverable::MAX_BINARY_DELIVERABLE_BYTES as i64
    } else {
        crate::deliverable::MAX_DELIVERABLE_BYTES as i64
    };
    let mut statement = Table::create();
    statement
        .table(rebuild.clone())
        .col(
            ColumnDef::new(OutputRevision::Id)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(ColumnDef::new(OutputRevision::OutputId).uuid().not_null())
        .col(ColumnDef::new(OutputRevision::Ordinal).integer().not_null())
        .col(
            ColumnDef::new(OutputRevision::ByteLen)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(OutputRevision::Sha256)
                .binary_len(32)
                .not_null(),
        )
        .col(ColumnDef::new(OutputRevision::TurnId).uuid());
    if binary {
        statement.col(ColumnDef::new(OutputRevision::ProducingRunId).uuid());
    }
    statement
        .col(
            ColumnDef::new(OutputRevision::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_output_revision_output")
                .from(rebuild, OutputRevision::OutputId)
                .to(Output::Table, Output::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .check(
            Expr::col(OutputRevision::Ordinal)
                .between(1, crate::deliverable::MAX_OUTPUT_REVISIONS as i32),
        )
        .check(Expr::col(OutputRevision::ByteLen).gte(0))
        .check(Expr::col(OutputRevision::ByteLen).lte(byte_ceiling));
    if binary {
        statement.check(
            Expr::col(OutputRevision::TurnId)
                .is_null()
                .or(Expr::col(OutputRevision::ProducingRunId).is_null()),
        );
    }
    statement.to_owned()
}

fn output_revision_rebuild_copy_sql(binary: bool) -> String {
    if binary {
        format!(
            "INSERT INTO {OUTPUT_REVISION_REBUILD} \
             (id, output_id, ordinal, byte_len, sha256, turn_id, producing_run_id, created_at) \
             SELECT id, output_id, ordinal, byte_len, sha256, turn_id, NULL, created_at \
             FROM output_revision"
        )
    } else {
        format!(
            "INSERT INTO {OUTPUT_REVISION_REBUILD} \
             (id, output_id, ordinal, byte_len, sha256, turn_id, created_at) \
             SELECT id, output_id, ordinal, byte_len, sha256, turn_id, created_at \
             FROM output_revision"
        )
    }
}

fn output_revision_ordinal_index() -> IndexCreateStatement {
    Index::create()
        .name("idx_output_revision_ordinal")
        .table(OutputRevision::Table)
        .col(OutputRevision::OutputId)
        .col(OutputRevision::Ordinal)
        .unique()
        .to_owned()
}

/// Records the images a message was submitted with, so a reloaded conversation
/// replays the same turn rather than a text-only approximation of it.
///
/// Only identity is stored: a content-addressed blob id plus the bounded
/// metadata a renderer or provider adapter needs. Filesystem paths are
/// deliberately absent — the bytes live in the blob store and are reachable
/// only through the blob id.
///
/// This makes `message_attachment.blob_id` a second class of live blob
/// reference alongside `document.source_blob_id`. Blob liveness is a union
/// across both, computed in one place; see `db::ops::blob::is_referenced_on`.
struct AddMessageAttachments;

impl MigrationName for AddMessageAttachments {
    fn name(&self) -> &str {
        "m20260724_000014_add_message_attachments"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddMessageAttachments {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(MessageAttachment::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(MessageAttachment::MessageId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MessageAttachment::Ordinal)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(MessageAttachment::ChatId).uuid().not_null())
                    .col(ColumnDef::new(MessageAttachment::BlobId).uuid().not_null())
                    .col(
                        ColumnDef::new(MessageAttachment::MediaType)
                            .string_len(64)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MessageAttachment::Width)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MessageAttachment::Height)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MessageAttachment::ByteLen)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MessageAttachment::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    // The composite key is the ordering contract: one image per
                    // position per message, so a retried submit cannot leave a
                    // message holding two images at the same index.
                    .primary_key(
                        Index::create()
                            .col(MessageAttachment::MessageId)
                            .col(MessageAttachment::Ordinal),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_message_attachment_message")
                            .from(MessageAttachment::Table, MessageAttachment::MessageId)
                            .to(Message::Table, Message::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_message_attachment_chat")
                            .from(MessageAttachment::Table, MessageAttachment::ChatId)
                            .to(Chat::Table, Chat::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .check(Expr::col(MessageAttachment::BlobId).ne(uuid::Uuid::nil()))
                    .check(Expr::col(MessageAttachment::Ordinal).gte(0))
                    .check(
                        Expr::col(MessageAttachment::Ordinal)
                            .lt(crate::model::MAX_MESSAGE_ATTACHMENTS as i32),
                    )
                    .check(Expr::col(MessageAttachment::MediaType).is_in([
                        crate::image::ImageMediaType::Png.as_str(),
                        crate::image::ImageMediaType::Jpeg.as_str(),
                        crate::image::ImageMediaType::Webp.as_str(),
                        crate::image::ImageMediaType::Gif.as_str(),
                    ]))
                    .check(
                        Expr::col(MessageAttachment::Width)
                            .between(1, crate::image::MAX_IMAGE_DIMENSION as i32),
                    )
                    .check(
                        Expr::col(MessageAttachment::Height)
                            .between(1, crate::image::MAX_IMAGE_DIMENSION as i32),
                    )
                    .check(
                        Expr::col(MessageAttachment::ByteLen)
                            .between(1, crate::image::MAX_IMAGE_BYTES as i64),
                    )
                    .to_owned(),
            )
            .await?;
        // The orphan auditor and every retirement decision ask "does any
        // attachment still reference this blob?"; that lookup must not scan.
        manager
            .create_index(
                Index::create()
                    .name("idx_message_attachment_blob")
                    .table(MessageAttachment::Table)
                    .col(MessageAttachment::BlobId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_message_attachment_chat")
                    .table(MessageAttachment::Table)
                    .col(MessageAttachment::ChatId)
                    .col(MessageAttachment::MessageId)
                    .col(MessageAttachment::Ordinal)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(MessageAttachment::Table).to_owned())
            .await
    }
}

/// Gives a plan-mode proposal a durable continuation record.
///
/// Mirrors `user_question_request`: the row is the renderer-safe projection of
/// one parked `exit_plan_mode` call, and the decision resolves the same tool
/// call and turn through the shared client-wait state machine.
struct AddPlanRequests;

impl MigrationName for AddPlanRequests {
    fn name(&self) -> &str {
        "m20260730_000035_add_plan_requests"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddPlanRequests {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(PlanRequest::Table)
                    .col(
                        ColumnDef::new(PlanRequest::CallId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(PlanRequest::TurnId).uuid().not_null())
                    .col(ColumnDef::new(PlanRequest::ChatId).uuid().not_null())
                    .col(
                        ColumnDef::new(PlanRequest::Status)
                            .string_len(16)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PlanRequest::EventSeq)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PlanRequest::Title)
                            .string_len(crate::MAX_PLAN_TITLE_CHARS as u32)
                            .not_null(),
                    )
                    .col(ColumnDef::new(PlanRequest::Plan).text().not_null())
                    .col(
                        ColumnDef::new(PlanRequest::Feedback)
                            .string_len(crate::MAX_PLAN_FEEDBACK_CHARS as u32),
                    )
                    .col(
                        ColumnDef::new(PlanRequest::ProposedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(PlanRequest::ResolvedAt).timestamp_with_time_zone())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_plan_request_call")
                            .from(PlanRequest::Table, PlanRequest::CallId)
                            .to(ToolCall::Table, ToolCall::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_plan_request_turn")
                            .from(PlanRequest::Table, PlanRequest::TurnId)
                            .to(TurnRun::Table, TurnRun::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_plan_request_chat")
                            .from(PlanRequest::Table, PlanRequest::ChatId)
                            .to(Chat::Table, Chat::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_plan_request_event")
                            .from_tbl(PlanRequest::Table)
                            .from_col(PlanRequest::ChatId)
                            .from_col(PlanRequest::EventSeq)
                            .to_tbl(Event::Table)
                            .to_col(Event::ChatId)
                            .to_col(Event::Seq)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .check(Expr::col(PlanRequest::Status).is_in([
                        crate::PlanRequestStatus::Pending.as_str(),
                        crate::PlanRequestStatus::Accepted.as_str(),
                        crate::PlanRequestStatus::Rejected.as_str(),
                        crate::PlanRequestStatus::Cancelled.as_str(),
                    ]))
                    .check(
                        Expr::col(PlanRequest::Status)
                            .eq(crate::PlanRequestStatus::Pending.as_str())
                            .and(Expr::col(PlanRequest::ResolvedAt).is_null())
                            .or(Expr::col(PlanRequest::Status)
                                .ne(crate::PlanRequestStatus::Pending.as_str())
                                .and(Expr::col(PlanRequest::ResolvedAt).is_not_null())),
                    )
                    .check(Expr::col(PlanRequest::ResolvedAt).is_null().or(
                        Expr::col(PlanRequest::ResolvedAt).gte(Expr::col(PlanRequest::ProposedAt)),
                    ))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_plan_request_pending")
                    .table(PlanRequest::Table)
                    .col(PlanRequest::ChatId)
                    .col(PlanRequest::ProposedAt)
                    .col(PlanRequest::CallId)
                    .and_where(
                        Expr::col(PlanRequest::Status)
                            .eq(crate::PlanRequestStatus::Pending.as_str()),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_plan_request_turn")
                    .table(PlanRequest::Table)
                    .col(PlanRequest::TurnId)
                    .col(PlanRequest::CallId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_plan_request_event")
                    .table(PlanRequest::Table)
                    .col(PlanRequest::ChatId)
                    .col(PlanRequest::EventSeq)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(PlanRequest::Table).to_owned())
            .await
    }
}

/// Links imported source documents to the user message that introduced them.
struct AddMessageDocumentAttachments;

impl MigrationName for AddMessageDocumentAttachments {
    fn name(&self) -> &str {
        "m20260729_000032_add_message_document_attachments"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddMessageDocumentAttachments {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(MessageDocumentAttachment::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(MessageDocumentAttachment::MessageId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MessageDocumentAttachment::Ordinal)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MessageDocumentAttachment::ChatId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MessageDocumentAttachment::DocumentId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MessageDocumentAttachment::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(MessageDocumentAttachment::MessageId)
                            .col(MessageDocumentAttachment::Ordinal),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_message_document_attachment_message")
                            .from(
                                MessageDocumentAttachment::Table,
                                MessageDocumentAttachment::MessageId,
                            )
                            .to(Message::Table, Message::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_message_document_attachment_document")
                            .from(
                                MessageDocumentAttachment::Table,
                                MessageDocumentAttachment::DocumentId,
                            )
                            .to(Document::Table, Document::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_message_document_attachment_chat")
                            .from(
                                MessageDocumentAttachment::Table,
                                MessageDocumentAttachment::ChatId,
                            )
                            .to(Chat::Table, Chat::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .check(Expr::col(MessageDocumentAttachment::DocumentId).ne(uuid::Uuid::nil()))
                    .check(Expr::col(MessageDocumentAttachment::Ordinal).gte(0))
                    .check(
                        Expr::col(MessageDocumentAttachment::Ordinal)
                            .lt(crate::model::MAX_MESSAGE_ATTACHMENTS as i32),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_message_document_attachment_chat")
                    .table(MessageDocumentAttachment::Table)
                    .col(MessageDocumentAttachment::ChatId)
                    .col(MessageDocumentAttachment::MessageId)
                    .col(MessageDocumentAttachment::Ordinal)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_message_document_attachment_document")
                    .table(MessageDocumentAttachment::Table)
                    .col(MessageDocumentAttachment::DocumentId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_message_document_attachment_unique")
                    .table(MessageDocumentAttachment::Table)
                    .col(MessageDocumentAttachment::MessageId)
                    .col(MessageDocumentAttachment::DocumentId)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(MessageDocumentAttachment::Table)
                    .to_owned(),
            )
            .await
    }
}

/// Stores one bounded, versioned semantic checkpoint per conversation.
///
/// It is separate from `message`: checkpoints are agent-maintained provider
/// context, not user-visible transcript entries. The source sequence is copied
/// from the validated source message solely to make stale-writer rejection
/// atomic; source text is never copied into this schema by the migration.
struct AddContextCheckpoints;

impl MigrationName for AddContextCheckpoints {
    fn name(&self) -> &str {
        "m20260727_000016_add_context_checkpoints"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddContextCheckpoints {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ContextCheckpoint::Table)
                    .col(
                        ColumnDef::new(ContextCheckpoint::ChatId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ContextCheckpoint::SourceMessageId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ContextCheckpoint::SourceMessageSeq)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ContextCheckpoint::FormatVersion)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ContextCheckpoint::Content)
                            .string_len(
                                crate::semantic_checkpoint::MAX_CONTEXT_CHECKPOINT_BYTES as u32,
                            )
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ContextCheckpoint::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_context_checkpoint_chat")
                            .from(ContextCheckpoint::Table, ContextCheckpoint::ChatId)
                            .to(Chat::Table, Chat::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_context_checkpoint_source_message")
                            .from(ContextCheckpoint::Table, ContextCheckpoint::SourceMessageId)
                            .to(Message::Table, Message::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .check(Expr::col(ContextCheckpoint::SourceMessageSeq).gt(0))
                    .check(
                        Expr::col(ContextCheckpoint::FormatVersion)
                            .eq(crate::semantic_checkpoint::CONTEXT_CHECKPOINT_FORMAT_V1 as i32),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ContextCheckpoint::Table).to_owned())
            .await
    }
}

/// Adds an explicit conversation boundary to document metadata. Existing
/// project and unscoped rows remain conversationless and are available only
/// through their legacy APIs; new conversation routes always populate this
/// column and retrieval filters it directly.
struct AddDocumentChatScope;

impl MigrationName for AddDocumentChatScope {
    fn name(&self) -> &str {
        "m20260723_000011_add_document_chat_scope"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddDocumentChatScope {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Document::Table)
                    .add_column(ColumnDef::new(Document::ChatId).uuid())
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_document_chat_created")
                    .table(Document::Table)
                    .col(Document::ChatId)
                    .col(Document::CreatedAt)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_document_chat_created")
                    .table(Document::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Document::Table)
                    .drop_column(Document::ChatId)
                    .to_owned(),
            )
            .await
    }
}

async fn create_agent_run_table(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(AgentRunClaimLock::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(AgentRunClaimLock::Id)
                        .integer()
                        .not_null()
                        .primary_key(),
                )
                .check(Expr::col(AgentRunClaimLock::Id).eq(1))
                .to_owned(),
        )
        .await?;
    manager
        .get_connection()
        .execute_unprepared(
            "INSERT INTO agent_run_claim_lock (id) VALUES (1) ON CONFLICT DO NOTHING",
        )
        .await?;

    manager
        .create_table(
            Table::create()
                .table(AgentRunClaim::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(AgentRunClaim::Token)
                        .uuid()
                        .not_null()
                        .primary_key(),
                )
                .col(ColumnDef::new(AgentRunClaim::AgentRunId).uuid())
                .col(ColumnDef::new(AgentRunClaim::AttemptCount).integer())
                .col(ColumnDef::new(AgentRunClaim::ClaimCount).integer())
                .col(
                    ColumnDef::new(AgentRunClaim::ClaimedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .col(ColumnDef::new(AgentRunClaim::LeaseExpiresAt).timestamp_with_time_zone())
                .check(
                    Expr::col(AgentRunClaim::AgentRunId)
                        .is_null()
                        .and(Expr::col(AgentRunClaim::AttemptCount).is_null())
                        .and(Expr::col(AgentRunClaim::ClaimCount).is_null())
                        .and(Expr::col(AgentRunClaim::LeaseExpiresAt).is_null())
                        .or(Expr::col(AgentRunClaim::AgentRunId)
                            .is_not_null()
                            .and(Expr::col(AgentRunClaim::AttemptCount).is_not_null())
                            .and(Expr::col(AgentRunClaim::AttemptCount).gte(1))
                            .and(
                                Expr::col(AgentRunClaim::ClaimCount).is_not_null().and(
                                    Expr::col(AgentRunClaim::ClaimCount)
                                        .gte(Expr::col(AgentRunClaim::AttemptCount)),
                                ),
                            )
                            .and(
                                Expr::col(AgentRunClaim::LeaseExpiresAt).is_not_null().and(
                                    Expr::col(AgentRunClaim::LeaseExpiresAt)
                                        .gt(Expr::col(AgentRunClaim::ClaimedAt)),
                                ),
                            )),
                )
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_agent_run_claim_identity")
                .table(AgentRunClaim::Table)
                .col(AgentRunClaim::Token)
                .col(AgentRunClaim::AgentRunId)
                .col(AgentRunClaim::AttemptCount)
                .col(AgentRunClaim::ClaimCount)
                .unique()
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_agent_run_claim_count")
                .table(AgentRunClaim::Table)
                .col(AgentRunClaim::AgentRunId)
                .col(AgentRunClaim::ClaimCount)
                .unique()
                .to_owned(),
        )
        .await?;

    let foreground_status = Expr::col(AgentRun::Status).is_in([
        AgentRunStatus::Active.as_str(),
        AgentRunStatus::Completed.as_str(),
        AgentRunStatus::Failed.as_str(),
        AgentRunStatus::Cancelled.as_str(),
    ]);
    let sandbox_status = Expr::col(AgentRun::Status).is_in([
        AgentRunStatus::Queued.as_str(),
        AgentRunStatus::Running.as_str(),
        AgentRunStatus::Cancelling.as_str(),
        AgentRunStatus::Waiting.as_str(),
        AgentRunStatus::RetryWait.as_str(),
        AgentRunStatus::Completed.as_str(),
        AgentRunStatus::Failed.as_str(),
        AgentRunStatus::Cancelled.as_str(),
    ]);
    let foreground_shape = Expr::col(AgentRun::Execution)
        // The baseline predates the tier/location split; its shape is frozen
        // over the original single execution column.
        .eq("foreground")
        .and(Expr::col(AgentRun::Depth).eq(0))
        .and(Expr::col(AgentRun::ParentId).is_null())
        .and(Expr::col(AgentRun::ParentDepth).is_null())
        .and(Expr::col(AgentRun::SpawnCallId).is_null())
        .and(Expr::col(AgentRun::Input).is_null())
        .and(Expr::col(AgentRun::AttemptCount).eq(0))
        .and(Expr::col(AgentRun::MaxAttempts).eq(0))
        .and(Expr::col(AgentRun::ClaimCount).eq(0))
        .and(Expr::col(AgentRun::AvailableAt).eq(Expr::col(AgentRun::CreatedAt)))
        .and(Expr::col(AgentRun::DeadlineAt).is_null())
        .and(Expr::col(AgentRun::StartedAt).is_null())
        .and(foreground_status);
    let sandbox_shape = Expr::col(AgentRun::Execution)
        .eq("sandbox")
        .and(Expr::col(AgentRun::Depth).eq(i32::from(crate::model::AgentRun::MAX_DEPTH)))
        .and(Expr::col(AgentRun::ParentId).is_not_null())
        .and(Expr::col(AgentRun::ParentDepth).eq(0))
        .and(Expr::col(AgentRun::SpawnCallId).is_not_null())
        .and(Expr::col(AgentRun::Input).is_not_null())
        .and(Expr::col(AgentRun::MaxAttempts).gte(1))
        .and(Expr::col(AgentRun::AttemptCount).gte(0))
        .and(Expr::col(AgentRun::AttemptCount).lte(Expr::col(AgentRun::MaxAttempts)))
        .and(Expr::col(AgentRun::ClaimCount).gte(Expr::col(AgentRun::AttemptCount)))
        .and(Expr::col(AgentRun::ClaimCount).lt(i32::MAX))
        .and(Expr::col(AgentRun::AvailableAt).gte(Expr::col(AgentRun::CreatedAt)))
        .and(Expr::col(AgentRun::DeadlineAt).gt(Expr::col(AgentRun::CreatedAt)))
        .and(
            Func::char_length(Expr::col(AgentRun::Input))
                .between(1, crate::model::AgentRun::MAX_INPUT_LEN as i32),
        )
        .and(sandbox_status);
    let active_lease = Expr::col(AgentRun::Status)
        .is_in([
            AgentRunStatus::Running.as_str(),
            AgentRunStatus::Cancelling.as_str(),
        ])
        .and(Expr::col(AgentRun::LeaseToken).is_not_null())
        .and(Expr::col(AgentRun::LeaseExpiresAt).is_not_null())
        .and(Expr::col(AgentRun::AttemptCount).gte(1))
        .and(Expr::col(AgentRun::StartedAt).is_not_null());
    let no_lease = Expr::col(AgentRun::Status)
        .is_not_in([
            AgentRunStatus::Running.as_str(),
            AgentRunStatus::Cancelling.as_str(),
        ])
        .and(Expr::col(AgentRun::LeaseToken).is_null())
        .and(Expr::col(AgentRun::LeaseExpiresAt).is_null());
    let terminal_finished = Expr::col(AgentRun::Status)
        .is_in([
            AgentRunStatus::Completed.as_str(),
            AgentRunStatus::Failed.as_str(),
            AgentRunStatus::Cancelled.as_str(),
        ])
        .and(Expr::col(AgentRun::FinishedAt).is_not_null());
    let nonterminal_unfinished = Expr::col(AgentRun::Status)
        .is_in([
            AgentRunStatus::Active.as_str(),
            AgentRunStatus::Queued.as_str(),
            AgentRunStatus::Running.as_str(),
            AgentRunStatus::Cancelling.as_str(),
            AgentRunStatus::Waiting.as_str(),
            AgentRunStatus::RetryWait.as_str(),
        ])
        .and(Expr::col(AgentRun::FinishedAt).is_null());
    let failure_has_error = Expr::col(AgentRun::Status)
        .is_in([
            AgentRunStatus::RetryWait.as_str(),
            AgentRunStatus::Failed.as_str(),
        ])
        .and(Expr::col(AgentRun::LastErrorCode).is_not_null());
    let success_has_no_error = Expr::col(AgentRun::Status)
        .is_not_in([
            AgentRunStatus::RetryWait.as_str(),
            AgentRunStatus::Failed.as_str(),
        ])
        .and(Expr::col(AgentRun::LastErrorCode).is_null())
        .and(Expr::col(AgentRun::LastErrorDetail).is_null());

    manager
        .create_table(
            Table::create()
                .table(AgentRun::Table)
                .if_not_exists()
                .col(ColumnDef::new(AgentRun::Id).uuid().not_null())
                .col(ColumnDef::new(AgentRun::ChatId).uuid().not_null())
                .col(ColumnDef::new(AgentRun::ParentId).uuid())
                .col(ColumnDef::new(AgentRun::ParentDepth).small_integer())
                .col(ColumnDef::new(AgentRun::SpawnCallId).uuid())
                .col(
                    ColumnDef::new(AgentRun::Execution)
                        .string_len(16)
                        .not_null(),
                )
                .col(ColumnDef::new(AgentRun::Depth).small_integer().not_null())
                .col(ColumnDef::new(AgentRun::Status).string_len(32).not_null())
                .col(ColumnDef::new(AgentRun::Input).text())
                .col(ColumnDef::new(AgentRun::AttemptCount).integer().not_null())
                .col(ColumnDef::new(AgentRun::MaxAttempts).integer().not_null())
                .col(ColumnDef::new(AgentRun::ClaimCount).integer().not_null())
                .col(
                    ColumnDef::new(AgentRun::AvailableAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .col(ColumnDef::new(AgentRun::DeadlineAt).timestamp_with_time_zone())
                .col(ColumnDef::new(AgentRun::LeaseToken).uuid())
                .col(ColumnDef::new(AgentRun::LeaseExpiresAt).timestamp_with_time_zone())
                .col(ColumnDef::new(AgentRun::StartedAt).timestamp_with_time_zone())
                .col(ColumnDef::new(AgentRun::FinishedAt).timestamp_with_time_zone())
                .col(
                    ColumnDef::new(AgentRun::LastErrorCode)
                        .string_len(crate::model::AgentRun::MAX_ERROR_CODE_LEN as u32),
                )
                .col(
                    ColumnDef::new(AgentRun::LastErrorDetail)
                        .string_len(crate::model::AgentRun::MAX_ERROR_DETAIL_LEN as u32),
                )
                .col(
                    ColumnDef::new(AgentRun::CreatedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(AgentRun::UpdatedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .primary_key(
                    Index::create()
                        .name("pk_agent_run")
                        .col(AgentRun::Id)
                        .col(AgentRun::ChatId)
                        .col(AgentRun::Depth),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_agent_run_chat")
                        .from(AgentRun::Table, AgentRun::ChatId)
                        .to(Chat::Table, Chat::Id)
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_agent_run_parent")
                        .from_tbl(AgentRun::Table)
                        .from_col(AgentRun::ParentId)
                        .from_col(AgentRun::ChatId)
                        .from_col(AgentRun::ParentDepth)
                        .to_tbl(AgentRun::Table)
                        .to_col(AgentRun::Id)
                        .to_col(AgentRun::ChatId)
                        .to_col(AgentRun::Depth)
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_agent_run_live_claim")
                        .from_tbl(AgentRun::Table)
                        .from_col(AgentRun::LeaseToken)
                        .from_col(AgentRun::Id)
                        .from_col(AgentRun::AttemptCount)
                        .from_col(AgentRun::ClaimCount)
                        .to_tbl(AgentRunClaim::Table)
                        .to_col(AgentRunClaim::Token)
                        .to_col(AgentRunClaim::AgentRunId)
                        .to_col(AgentRunClaim::AttemptCount)
                        .to_col(AgentRunClaim::ClaimCount)
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .check(foreground_shape.or(sandbox_shape))
                .check(active_lease.or(no_lease))
                .check(terminal_finished.or(nonterminal_unfinished))
                .check(failure_has_error.or(success_has_no_error))
                .check(
                    Expr::col(AgentRun::LastErrorDetail)
                        .is_null()
                        .or(Expr::col(AgentRun::LastErrorCode).is_not_null()),
                )
                .check(Expr::col(AgentRun::UpdatedAt).gte(Expr::col(AgentRun::CreatedAt)))
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_agent_run_id")
                .table(AgentRun::Table)
                .col(AgentRun::Id)
                .unique()
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_agent_run_id_parent_chat")
                .table(AgentRun::Table)
                .col(AgentRun::Id)
                .col(AgentRun::ParentId)
                .col(AgentRun::ChatId)
                .unique()
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_agent_run_admission_identity")
                .table(AgentRun::Table)
                .col(AgentRun::Id)
                .col(AgentRun::ParentId)
                .col(AgentRun::ChatId)
                .col(AgentRun::SpawnCallId)
                .unique()
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_agent_run_spawn_call")
                .table(AgentRun::Table)
                .col(AgentRun::SpawnCallId)
                .unique()
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_agent_run_one_foreground")
                .table(AgentRun::Table)
                .col(AgentRun::ChatId)
                .unique()
                .and_where(Expr::col(AgentRun::Execution).eq("foreground"))
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_agent_run_parent")
                .table(AgentRun::Table)
                .col(AgentRun::ParentId)
                .col(AgentRun::CreatedAt)
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_agent_run_chat_history")
                .table(AgentRun::Table)
                .col(AgentRun::ChatId)
                .col(AgentRun::CreatedAt)
                .col(AgentRun::Id)
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_agent_run_claimable")
                .table(AgentRun::Table)
                .col(AgentRun::Status)
                .col(AgentRun::AvailableAt)
                .col(AgentRun::CreatedAt)
                .col(AgentRun::Id)
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_agent_run_live_by_chat")
                .table(AgentRun::Table)
                .col(AgentRun::Status)
                .col(AgentRun::ChatId)
                .col(AgentRun::LeaseExpiresAt)
                .to_owned(),
        )
        .await?;
    Ok(())
}

async fn create_agent_run_result_table(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(AgentRunResult::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(AgentRunResult::AgentRunId)
                        .uuid()
                        .not_null()
                        .primary_key(),
                )
                .col(ColumnDef::new(AgentRunResult::LeaseToken).uuid().not_null())
                .col(
                    ColumnDef::new(AgentRunResult::AttemptCount)
                        .integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(AgentRunResult::ClaimCount)
                        .integer()
                        .not_null(),
                )
                .col(ColumnDef::new(AgentRunResult::Text).text().not_null())
                .col(
                    ColumnDef::new(AgentRunResult::SubmittedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_agent_run_result_run")
                        .from(AgentRunResult::Table, AgentRunResult::AgentRunId)
                        .to(AgentRun::Table, AgentRun::Id)
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_agent_run_result_claim")
                        .from_tbl(AgentRunResult::Table)
                        .from_col(AgentRunResult::LeaseToken)
                        .from_col(AgentRunResult::AgentRunId)
                        .from_col(AgentRunResult::AttemptCount)
                        .from_col(AgentRunResult::ClaimCount)
                        .to_tbl(AgentRunClaim::Table)
                        .to_col(AgentRunClaim::Token)
                        .to_col(AgentRunClaim::AgentRunId)
                        .to_col(AgentRunClaim::AttemptCount)
                        .to_col(AgentRunClaim::ClaimCount)
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .check(Expr::col(AgentRunResult::AttemptCount).gte(1))
                .check(
                    Expr::col(AgentRunResult::ClaimCount)
                        .gte(Expr::col(AgentRunResult::AttemptCount)),
                )
                .check(
                    Func::char_length(Expr::col(AgentRunResult::Text))
                        .between(1, crate::model::AgentRun::MAX_RESULT_LEN as i32),
                )
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_agent_run_result_identity")
                .table(AgentRunResult::Table)
                .col(AgentRunResult::AgentRunId)
                .col(AgentRunResult::LeaseToken)
                .col(AgentRunResult::AttemptCount)
                .col(AgentRunResult::ClaimCount)
                .unique()
                .to_owned(),
        )
        .await
}

async fn create_agent_run_inbox_table(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(AgentRunInbox::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(AgentRunInbox::ChildRunId)
                        .uuid()
                        .not_null()
                        .primary_key(),
                )
                .col(ColumnDef::new(AgentRunInbox::ParentRunId).uuid().not_null())
                .col(ColumnDef::new(AgentRunInbox::ChatId).uuid().not_null())
                .col(
                    ColumnDef::new(AgentRunInbox::ParentDepth)
                        .small_integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(AgentRunInbox::ResultLeaseToken)
                        .uuid()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(AgentRunInbox::ResultAttemptCount)
                        .integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(AgentRunInbox::ResultClaimCount)
                        .integer()
                        .not_null(),
                )
                .col(ColumnDef::new(AgentRunInbox::Status).string().not_null())
                .col(
                    ColumnDef::new(AgentRunInbox::ClaimCount)
                        .integer()
                        .not_null(),
                )
                .col(ColumnDef::new(AgentRunInbox::LeaseToken).uuid().null())
                .col(
                    ColumnDef::new(AgentRunInbox::LeaseExpiresAt)
                        .timestamp_with_time_zone()
                        .null(),
                )
                .col(
                    ColumnDef::new(AgentRunInbox::ConsumedLeaseToken)
                        .uuid()
                        .null(),
                )
                .col(
                    ColumnDef::new(AgentRunInbox::ConsumedAt)
                        .timestamp_with_time_zone()
                        .null(),
                )
                .col(
                    ColumnDef::new(AgentRunInbox::DeliveredAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_agent_run_inbox_child")
                        .from_tbl(AgentRunInbox::Table)
                        .from_col(AgentRunInbox::ChildRunId)
                        .from_col(AgentRunInbox::ParentRunId)
                        .from_col(AgentRunInbox::ChatId)
                        .to_tbl(AgentRun::Table)
                        .to_col(AgentRun::Id)
                        .to_col(AgentRun::ParentId)
                        .to_col(AgentRun::ChatId)
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_agent_run_inbox_parent")
                        .from_tbl(AgentRunInbox::Table)
                        .from_col(AgentRunInbox::ParentRunId)
                        .from_col(AgentRunInbox::ChatId)
                        .from_col(AgentRunInbox::ParentDepth)
                        .to_tbl(AgentRun::Table)
                        .to_col(AgentRun::Id)
                        .to_col(AgentRun::ChatId)
                        .to_col(AgentRun::Depth)
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_agent_run_inbox_result")
                        .from_tbl(AgentRunInbox::Table)
                        .from_col(AgentRunInbox::ChildRunId)
                        .from_col(AgentRunInbox::ResultLeaseToken)
                        .from_col(AgentRunInbox::ResultAttemptCount)
                        .from_col(AgentRunInbox::ResultClaimCount)
                        .to_tbl(AgentRunResult::Table)
                        .to_col(AgentRunResult::AgentRunId)
                        .to_col(AgentRunResult::LeaseToken)
                        .to_col(AgentRunResult::AttemptCount)
                        .to_col(AgentRunResult::ClaimCount)
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .check(Expr::col(AgentRunInbox::ParentDepth).eq(0))
                .check(Expr::col(AgentRunInbox::ResultAttemptCount).gte(1))
                .check(
                    Expr::col(AgentRunInbox::ResultClaimCount)
                        .gte(Expr::col(AgentRunInbox::ResultAttemptCount)),
                )
                .check(Expr::col(AgentRunInbox::Status).is_in([
                    "pending",
                    "claimed",
                    "consumed",
                    "cancelled",
                ]))
                .check(Expr::col(AgentRunInbox::ClaimCount).gte(0))
                .check(
                    Expr::col(AgentRunInbox::Status)
                        .eq("pending")
                        .and(Expr::col(AgentRunInbox::ClaimCount).eq(0))
                        .and(Expr::col(AgentRunInbox::LeaseToken).is_null())
                        .and(Expr::col(AgentRunInbox::LeaseExpiresAt).is_null())
                        .and(Expr::col(AgentRunInbox::ConsumedLeaseToken).is_null())
                        .and(Expr::col(AgentRunInbox::ConsumedAt).is_null())
                        .or(Expr::col(AgentRunInbox::Status)
                            .eq("claimed")
                            .and(Expr::col(AgentRunInbox::ClaimCount).gte(1))
                            .and(Expr::col(AgentRunInbox::LeaseToken).is_not_null())
                            .and(Expr::col(AgentRunInbox::LeaseExpiresAt).is_not_null())
                            .and(Expr::col(AgentRunInbox::ConsumedLeaseToken).is_null())
                            .and(Expr::col(AgentRunInbox::ConsumedAt).is_null()))
                        .or(Expr::col(AgentRunInbox::Status)
                            .eq("consumed")
                            .and(Expr::col(AgentRunInbox::ClaimCount).gte(1))
                            .and(Expr::col(AgentRunInbox::LeaseToken).is_null())
                            .and(Expr::col(AgentRunInbox::LeaseExpiresAt).is_null())
                            .and(Expr::col(AgentRunInbox::ConsumedLeaseToken).is_not_null())
                            .and(Expr::col(AgentRunInbox::ConsumedAt).is_not_null()))
                        .or(Expr::col(AgentRunInbox::Status)
                            .eq("cancelled")
                            .and(Expr::col(AgentRunInbox::LeaseToken).is_null())
                            .and(Expr::col(AgentRunInbox::LeaseExpiresAt).is_null())
                            .and(Expr::col(AgentRunInbox::ConsumedLeaseToken).is_null())
                            .and(Expr::col(AgentRunInbox::ConsumedAt).is_null())),
                )
                .to_owned(),
        )
        .await
}

async fn create_turn_agent_run_wait_set_tables(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(TurnAgentRunWaitLock::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(TurnAgentRunWaitLock::Id)
                        .integer()
                        .not_null()
                        .primary_key(),
                )
                .check(Expr::col(TurnAgentRunWaitLock::Id).eq(1))
                .to_owned(),
        )
        .await?;
    manager
        .get_connection()
        .execute_unprepared(
            "INSERT INTO turn_agent_run_wait_lock (id) VALUES (1) ON CONFLICT DO NOTHING",
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_sandbox_agent_admission_wait_owner")
                .table(SandboxAgentAdmission::Table)
                .col(SandboxAgentAdmission::ChildRunId)
                .col(SandboxAgentAdmission::OriginTurnId)
                .col(SandboxAgentAdmission::ParentRunId)
                .col(SandboxAgentAdmission::ChatId)
                .unique()
                .to_owned(),
        )
        .await?;
    manager
        .create_table(
            Table::create()
                .table(TurnAgentRunWaitSet::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(TurnAgentRunWaitSet::Id)
                        .uuid()
                        .not_null()
                        .primary_key(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitSet::ParentRunId)
                        .uuid()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitSet::TurnId)
                        .uuid()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitSet::ChatId)
                        .uuid()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitSet::ProviderId)
                        .text()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitSet::HistoryOrder)
                        .big_integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitSet::Arguments)
                        .json_binary()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitSet::Condition)
                        .text()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitSet::ParkLeaseToken)
                        .uuid()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitSet::ExpectedSteerRevision)
                        .big_integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitSet::AttemptCount)
                        .integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitSet::ClaimCount)
                        .integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitSet::ModelSteps)
                        .integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitSet::InputTokens)
                        .big_integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitSet::OutputTokens)
                        .big_integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitSet::CacheReadInputTokens)
                        .big_integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitSet::CacheCreationInputTokens)
                        .big_integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitSet::EventOrdinal)
                        .integer()
                        .not_null(),
                )
                .col(ColumnDef::new(TurnAgentRunWaitSet::EventSeq).big_integer())
                .col(
                    ColumnDef::new(TurnAgentRunWaitSet::Status)
                        .text()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitSet::ParkedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .col(ColumnDef::new(TurnAgentRunWaitSet::ClosedAt).timestamp_with_time_zone())
                .col(ColumnDef::new(TurnAgentRunWaitSet::ResumeToken).uuid())
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_turn_agent_run_wait_set_turn")
                        .from_tbl(TurnAgentRunWaitSet::Table)
                        .from_col(TurnAgentRunWaitSet::TurnId)
                        .from_col(TurnAgentRunWaitSet::ChatId)
                        .from_col(TurnAgentRunWaitSet::ParentRunId)
                        .to_tbl(TurnRun::Table)
                        .to_col(TurnRun::Id)
                        .to_col(TurnRun::ChatId)
                        .to_col(TurnRun::AgentRunId)
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_turn_agent_run_wait_set_tool")
                        .from_tbl(TurnAgentRunWaitSet::Table)
                        .from_col(TurnAgentRunWaitSet::Id)
                        .from_col(TurnAgentRunWaitSet::ChatId)
                        .from_col(TurnAgentRunWaitSet::HistoryOrder)
                        .to_tbl(ToolCall::Table)
                        .to_col(ToolCall::Id)
                        .to_col(ToolCall::ChatId)
                        .to_col(ToolCall::HistoryOrder)
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_turn_agent_run_wait_set_event")
                        .from_tbl(TurnAgentRunWaitSet::Table)
                        .from_col(TurnAgentRunWaitSet::ChatId)
                        .from_col(TurnAgentRunWaitSet::EventSeq)
                        .to_tbl(Event::Table)
                        .to_col(Event::ChatId)
                        .to_col(Event::Seq)
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_turn_agent_run_wait_set_claim")
                        .from_tbl(TurnAgentRunWaitSet::Table)
                        .from_col(TurnAgentRunWaitSet::ParkLeaseToken)
                        .from_col(TurnAgentRunWaitSet::TurnId)
                        .from_col(TurnAgentRunWaitSet::AttemptCount)
                        .from_col(TurnAgentRunWaitSet::ClaimCount)
                        .to_tbl(TurnClaim::Table)
                        .to_col(TurnClaim::Token)
                        .to_col(TurnClaim::TurnId)
                        .to_col(TurnClaim::AttemptCount)
                        .to_col(TurnClaim::ClaimCount)
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .check(
                    Expr::col(TurnAgentRunWaitSet::Condition)
                        .eq(AgentRunWaitCondition::All.as_str()),
                )
                .check(Expr::col(TurnAgentRunWaitSet::ExpectedSteerRevision).gte(0))
                .check(Expr::col(TurnAgentRunWaitSet::HistoryOrder).gt(0))
                .check(Expr::col(TurnAgentRunWaitSet::EventOrdinal).between(2, i32::MAX - 1))
                .check(
                    Func::char_length(Expr::col(TurnAgentRunWaitSet::ProviderId))
                        .between(1, crate::model::ToolCallRecord::MAX_LABEL_LEN as i32),
                )
                .check(Expr::col(TurnAgentRunWaitSet::AttemptCount).gte(1))
                .check(
                    Expr::col(TurnAgentRunWaitSet::ClaimCount)
                        .gte(Expr::col(TurnAgentRunWaitSet::AttemptCount)),
                )
                .check(
                    Expr::col(TurnAgentRunWaitSet::ModelSteps)
                        .gt(0)
                        .and(Expr::col(TurnAgentRunWaitSet::InputTokens).gte(0))
                        .and(Expr::col(TurnAgentRunWaitSet::OutputTokens).gte(0))
                        .and(Expr::col(TurnAgentRunWaitSet::CacheReadInputTokens).gte(0))
                        .and(Expr::col(TurnAgentRunWaitSet::CacheCreationInputTokens).gte(0)),
                )
                .check(
                    Expr::col(TurnAgentRunWaitSet::Status)
                        .eq(TurnAgentRunWaitStatus::Waiting.as_str())
                        .and(Expr::col(TurnAgentRunWaitSet::ClosedAt).is_null())
                        .and(Expr::col(TurnAgentRunWaitSet::ResumeToken).is_null())
                        .and(Expr::col(TurnAgentRunWaitSet::EventSeq).is_null())
                        .or(Expr::col(TurnAgentRunWaitSet::Status)
                            .eq(TurnAgentRunWaitStatus::Resumed.as_str())
                            .and(Expr::col(TurnAgentRunWaitSet::ClosedAt).is_not_null())
                            .and(Expr::col(TurnAgentRunWaitSet::ResumeToken).is_not_null())
                            .and(Expr::col(TurnAgentRunWaitSet::EventSeq).is_not_null()))
                        .or(Expr::col(TurnAgentRunWaitSet::Status)
                            .eq(TurnAgentRunWaitStatus::Cancelled.as_str())
                            .and(Expr::col(TurnAgentRunWaitSet::ClosedAt).is_not_null())
                            .and(Expr::col(TurnAgentRunWaitSet::ResumeToken).is_null())
                            .and(Expr::col(TurnAgentRunWaitSet::EventSeq).is_not_null())),
                )
                .check(
                    Expr::col(TurnAgentRunWaitSet::ClosedAt)
                        .is_null()
                        .or(Expr::col(TurnAgentRunWaitSet::ClosedAt)
                            .gte(Expr::col(TurnAgentRunWaitSet::ParkedAt))),
                )
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_turn_agent_run_wait_set_member_owner")
                .table(TurnAgentRunWaitSet::Table)
                .col(TurnAgentRunWaitSet::Id)
                .col(TurnAgentRunWaitSet::TurnId)
                .col(TurnAgentRunWaitSet::ParentRunId)
                .col(TurnAgentRunWaitSet::ChatId)
                .unique()
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_turn_agent_run_wait_set_one_open")
                .table(TurnAgentRunWaitSet::Table)
                .col(TurnAgentRunWaitSet::TurnId)
                .unique()
                .and_where(
                    Expr::col(TurnAgentRunWaitSet::Status)
                        .eq(TurnAgentRunWaitStatus::Waiting.as_str()),
                )
                .to_owned(),
        )
        .await?;
    manager
        .create_table(
            Table::create()
                .table(TurnAgentRunWaitMember::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(TurnAgentRunWaitMember::WaitId)
                        .uuid()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitMember::Position)
                        .small_integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitMember::ChildRunId)
                        .uuid()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitMember::ParentRunId)
                        .uuid()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitMember::OriginTurnId)
                        .uuid()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitMember::ChatId)
                        .uuid()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(TurnAgentRunWaitMember::Open)
                        .boolean()
                        .not_null(),
                )
                .primary_key(
                    Index::create()
                        .col(TurnAgentRunWaitMember::WaitId)
                        .col(TurnAgentRunWaitMember::Position),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_turn_agent_run_wait_member_set")
                        .from_tbl(TurnAgentRunWaitMember::Table)
                        .from_col(TurnAgentRunWaitMember::WaitId)
                        .from_col(TurnAgentRunWaitMember::OriginTurnId)
                        .from_col(TurnAgentRunWaitMember::ParentRunId)
                        .from_col(TurnAgentRunWaitMember::ChatId)
                        .to_tbl(TurnAgentRunWaitSet::Table)
                        .to_col(TurnAgentRunWaitSet::Id)
                        .to_col(TurnAgentRunWaitSet::TurnId)
                        .to_col(TurnAgentRunWaitSet::ParentRunId)
                        .to_col(TurnAgentRunWaitSet::ChatId)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_turn_agent_run_wait_member_admission")
                        .from_tbl(TurnAgentRunWaitMember::Table)
                        .from_col(TurnAgentRunWaitMember::ChildRunId)
                        .from_col(TurnAgentRunWaitMember::OriginTurnId)
                        .from_col(TurnAgentRunWaitMember::ParentRunId)
                        .from_col(TurnAgentRunWaitMember::ChatId)
                        .to_tbl(SandboxAgentAdmission::Table)
                        .to_col(SandboxAgentAdmission::ChildRunId)
                        .to_col(SandboxAgentAdmission::OriginTurnId)
                        .to_col(SandboxAgentAdmission::ParentRunId)
                        .to_col(SandboxAgentAdmission::ChatId)
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .check(Expr::col(TurnAgentRunWaitMember::Position).gte(0))
                .check(
                    Expr::col(TurnAgentRunWaitMember::Position)
                        .lt(crate::model::TurnAgentRunWaitSet::MAX_CHILDREN as i32),
                )
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_turn_agent_run_wait_member_one_open_child")
                .table(TurnAgentRunWaitMember::Table)
                .col(TurnAgentRunWaitMember::ChildRunId)
                .unique()
                .and_where(Expr::col(TurnAgentRunWaitMember::Open).eq(true))
                .to_owned(),
        )
        .await
}

async fn create_sandbox_tool_call_tables(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(SandboxToolCall::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(SandboxToolCall::Id)
                        .uuid()
                        .not_null()
                        .primary_key(),
                )
                .col(
                    ColumnDef::new(SandboxToolCall::AgentRunId)
                        .uuid()
                        .not_null(),
                )
                .col(ColumnDef::new(SandboxToolCall::ChatId).uuid().not_null())
                .col(
                    ColumnDef::new(SandboxToolCall::AgentRunDepth)
                        .small_integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(SandboxToolCall::ProviderId)
                        .text()
                        .not_null(),
                )
                .col(ColumnDef::new(SandboxToolCall::Name).text().not_null())
                .col(
                    ColumnDef::new(SandboxToolCall::Arguments)
                        .json_binary()
                        .not_null(),
                )
                .col(ColumnDef::new(SandboxToolCall::Status).text().not_null())
                .col(
                    ColumnDef::new(SandboxToolCall::ParkLeaseToken)
                        .uuid()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(SandboxToolCall::ParkAttemptCount)
                        .integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(SandboxToolCall::ParkClaimCount)
                        .integer()
                        .not_null(),
                )
                .col(ColumnDef::new(SandboxToolCall::ExecutorLeaseToken).uuid())
                .col(
                    ColumnDef::new(SandboxToolCall::ExecutorLeaseExpiresAt)
                        .timestamp_with_time_zone(),
                )
                .col(
                    ColumnDef::new(SandboxToolCall::CreatedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .col(ColumnDef::new(SandboxToolCall::ResolvedAt).timestamp_with_time_zone())
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_sandbox_tool_call_run")
                        .from_tbl(SandboxToolCall::Table)
                        .from_col(SandboxToolCall::AgentRunId)
                        .from_col(SandboxToolCall::ChatId)
                        .from_col(SandboxToolCall::AgentRunDepth)
                        .to_tbl(AgentRun::Table)
                        .to_col(AgentRun::Id)
                        .to_col(AgentRun::ChatId)
                        .to_col(AgentRun::Depth)
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_sandbox_tool_call_park_claim")
                        .from_tbl(SandboxToolCall::Table)
                        .from_col(SandboxToolCall::ParkLeaseToken)
                        .from_col(SandboxToolCall::AgentRunId)
                        .from_col(SandboxToolCall::ParkAttemptCount)
                        .from_col(SandboxToolCall::ParkClaimCount)
                        .to_tbl(AgentRunClaim::Table)
                        .to_col(AgentRunClaim::Token)
                        .to_col(AgentRunClaim::AgentRunId)
                        .to_col(AgentRunClaim::AttemptCount)
                        .to_col(AgentRunClaim::ClaimCount)
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .check(Expr::col(SandboxToolCall::AgentRunDepth).eq(1))
                .check(Expr::col(SandboxToolCall::ParkAttemptCount).gte(1))
                .check(
                    Expr::col(SandboxToolCall::ParkClaimCount)
                        .gte(Expr::col(SandboxToolCall::ParkAttemptCount)),
                )
                .check(Expr::col(SandboxToolCall::Status).is_in([
                    SandboxToolCallStatus::Accepted.as_str(),
                    SandboxToolCallStatus::Claimed.as_str(),
                    SandboxToolCallStatus::Completed.as_str(),
                    SandboxToolCallStatus::Failed.as_str(),
                    SandboxToolCallStatus::Cancelled.as_str(),
                ]))
                .check(
                    Expr::col(SandboxToolCall::ResolvedAt)
                        .is_null()
                        .or(Expr::col(SandboxToolCall::ResolvedAt)
                            .gte(Expr::col(SandboxToolCall::CreatedAt))),
                )
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_sandbox_tool_call_run")
                .table(SandboxToolCall::Table)
                .col(SandboxToolCall::AgentRunId)
                .col(SandboxToolCall::CreatedAt)
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_sandbox_tool_call_recovery")
                .table(SandboxToolCall::Table)
                .col(SandboxToolCall::Status)
                .col(SandboxToolCall::ExecutorLeaseExpiresAt)
                .col(SandboxToolCall::CreatedAt)
                .col(SandboxToolCall::Id)
                .to_owned(),
        )
        .await?;
    manager
        .create_table(
            Table::create()
                .table(SandboxToolCallReceipt::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(SandboxToolCallReceipt::CallId)
                        .uuid()
                        .not_null()
                        .primary_key(),
                )
                .col(
                    ColumnDef::new(SandboxToolCallReceipt::ExecutorLeaseToken)
                        .uuid()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(SandboxToolCallReceipt::Status)
                        .text()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(SandboxToolCallReceipt::Result)
                        .text()
                        .not_null(),
                )
                .col(ColumnDef::new(SandboxToolCallReceipt::ErrorCode).text())
                .col(ColumnDef::new(SandboxToolCallReceipt::ErrorDetail).text())
                .col(
                    ColumnDef::new(SandboxToolCallReceipt::ResolvedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_sandbox_tool_receipt_call")
                        .from(
                            SandboxToolCallReceipt::Table,
                            SandboxToolCallReceipt::CallId,
                        )
                        .to(SandboxToolCall::Table, SandboxToolCall::Id)
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .check(
                    Expr::col(SandboxToolCallReceipt::Status)
                        .eq(SandboxToolCallStatus::Failed.as_str())
                        .and(Expr::col(SandboxToolCallReceipt::ErrorCode).is_not_null())
                        .or(Expr::col(SandboxToolCallReceipt::Status)
                            .is_not_in([SandboxToolCallStatus::Failed.as_str()])
                            .and(Expr::col(SandboxToolCallReceipt::ErrorCode).is_null())
                            .and(Expr::col(SandboxToolCallReceipt::ErrorDetail).is_null())),
                )
                .to_owned(),
        )
        .await?;
    Ok(())
}

async fn create_agent_run_cancellation_table(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(AgentRunCancellation::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(AgentRunCancellation::AgentRunId)
                        .uuid()
                        .not_null()
                        .primary_key(),
                )
                .col(
                    ColumnDef::new(AgentRunCancellation::LeaseToken)
                        .uuid()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(AgentRunCancellation::AttemptCount)
                        .integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(AgentRunCancellation::ClaimCount)
                        .integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(AgentRunCancellation::Reason)
                        .string()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(AgentRunCancellation::RequestedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_agent_run_cancellation_run")
                        .from(
                            AgentRunCancellation::Table,
                            AgentRunCancellation::AgentRunId,
                        )
                        .to(AgentRun::Table, AgentRun::Id)
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_agent_run_cancellation_claim")
                        .from_tbl(AgentRunCancellation::Table)
                        .from_col(AgentRunCancellation::LeaseToken)
                        .from_col(AgentRunCancellation::AgentRunId)
                        .from_col(AgentRunCancellation::AttemptCount)
                        .from_col(AgentRunCancellation::ClaimCount)
                        .to_tbl(AgentRunClaim::Table)
                        .to_col(AgentRunClaim::Token)
                        .to_col(AgentRunClaim::AgentRunId)
                        .to_col(AgentRunClaim::AttemptCount)
                        .to_col(AgentRunClaim::ClaimCount)
                        .on_delete(ForeignKeyAction::Restrict),
                )
                .check(Expr::col(AgentRunCancellation::AttemptCount).gte(1))
                .check(
                    Expr::col(AgentRunCancellation::ClaimCount)
                        .gte(Expr::col(AgentRunCancellation::AttemptCount)),
                )
                .check(Expr::col(AgentRunCancellation::Reason).is_in([
                    crate::model::AgentRunCancellationReason::Requested.as_str(),
                    crate::model::AgentRunCancellationReason::ParentTurnCancelled.as_str(),
                    crate::model::AgentRunCancellationReason::ParentTurnFailed.as_str(),
                ]))
                .to_owned(),
        )
        .await
}

struct Init;

impl MigrationName for Init {
    fn name(&self) -> &str {
        "m0001_init"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Init {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Project::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Project::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Project::Title).text())
                    .col(
                        ColumnDef::new(Project::AttachmentRevision)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(Project::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .check(Expr::col(Project::AttachmentRevision).gte(0))
                    .check(
                        Expr::col(Project::AttachmentRevision)
                            .lte(crate::model::MAX_ATTACHMENT_REVISION),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(Chat::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Chat::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Chat::ProjectId).uuid())
                    .col(ColumnDef::new(Chat::Title).text())
                    .col(
                        ColumnDef::new(Chat::AttachmentRevision)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(Chat::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .check(Expr::col(Chat::AttachmentRevision).gte(0))
                    .check(
                        Expr::col(Chat::AttachmentRevision)
                            .lte(crate::model::MAX_ATTACHMENT_REVISION),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_chat_project")
                            .from(Chat::Table, Chat::ProjectId)
                            .to(Project::Table, Project::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .to_owned(),
            )
            .await?;

        create_agent_run_table(manager).await?;
        create_agent_run_result_table(manager).await?;
        create_agent_run_cancellation_table(manager).await?;
        create_agent_run_inbox_table(manager).await?;

        manager
            .create_table(
                Table::create()
                    .table(MessageIdentity::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(MessageIdentity::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(MessageIdentity::ChatId).uuid().not_null())
                    .col(ColumnDef::new(MessageIdentity::TurnId).uuid().not_null())
                    .col(
                        ColumnDef::new(MessageIdentity::Owner)
                            .string_len(16)
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_message_identity_chat")
                            .from(MessageIdentity::Table, MessageIdentity::ChatId)
                            .to(Chat::Table, Chat::Id),
                    )
                    .check(Expr::col(MessageIdentity::Owner).is_in(["message", "turn_steer"]))
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Message::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Message::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Message::ChatId).uuid().not_null())
                    .col(ColumnDef::new(Message::TurnId).uuid().not_null())
                    .col(ColumnDef::new(Message::Seq).big_integer().not_null())
                    .col(ColumnDef::new(Message::Role).text().not_null())
                    .col(ColumnDef::new(Message::Content).text().not_null())
                    .col(
                        ColumnDef::new(Message::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_message_chat")
                            .from(Message::Table, Message::ChatId)
                            .to(Chat::Table, Chat::Id),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_message_chat")
                    .table(Message::Table)
                    .col(Message::ChatId)
                    .col(Message::Seq)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_message_turn_identity")
                    .table(Message::Table)
                    .col(Message::Id)
                    .col(Message::ChatId)
                    .col(Message::TurnId)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(TurnClaim::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TurnClaim::Token)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(TurnClaim::TurnId).uuid().not_null())
                    .col(ColumnDef::new(TurnClaim::AttemptCount).integer().not_null())
                    .col(ColumnDef::new(TurnClaim::ClaimCount).integer().not_null())
                    .col(
                        ColumnDef::new(TurnClaim::ClaimedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnClaim::LeaseExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .check(Expr::col(TurnClaim::AttemptCount).gte(1))
                    .check(Expr::col(TurnClaim::ClaimCount).gte(Expr::col(TurnClaim::AttemptCount)))
                    .check(Expr::col(TurnClaim::LeaseExpiresAt).gt(Expr::col(TurnClaim::ClaimedAt)))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_claim_identity")
                    .table(TurnClaim::Table)
                    .col(TurnClaim::Token)
                    .col(TurnClaim::TurnId)
                    .col(TurnClaim::AttemptCount)
                    .col(TurnClaim::ClaimCount)
                    .unique()
                    .to_owned(),
            )
            .await?;
        // Failure receipts remain attempt-scoped even when one attempt spans
        // multiple worker lease segments.
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_claim_failure_identity")
                    .table(TurnClaim::Table)
                    .col(TurnClaim::Token)
                    .col(TurnClaim::TurnId)
                    .col(TurnClaim::AttemptCount)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_claim_count")
                    .table(TurnClaim::Table)
                    .col(TurnClaim::TurnId)
                    .col(TurnClaim::ClaimCount)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_claim_turn_token")
                    .table(TurnClaim::Table)
                    .col(TurnClaim::TurnId)
                    .col(TurnClaim::Token)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(TurnClaimLock::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TurnClaimLock::Id)
                            .integer()
                            .not_null()
                            .primary_key(),
                    )
                    .check(Expr::col(TurnClaimLock::Id).eq(1))
                    .to_owned(),
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared("INSERT INTO turn_claim_lock (id) VALUES (1)")
            .await?;

        let valid_turn_status = Expr::col(TurnRun::Status).is_in([
            TurnRunStatus::Queued.as_str(),
            TurnRunStatus::Running.as_str(),
            TurnRunStatus::Cancelling.as_str(),
            TurnRunStatus::WaitingForClient.as_str(),
            TurnRunStatus::WaitingForAgentRun.as_str(),
            TurnRunStatus::CancellingClient.as_str(),
            TurnRunStatus::Resuming.as_str(),
            TurnRunStatus::RetryWait.as_str(),
            TurnRunStatus::Completed.as_str(),
            TurnRunStatus::Failed.as_str(),
            TurnRunStatus::Cancelled.as_str(),
        ]);
        let active_lease = Expr::col(TurnRun::Status)
            .is_in([
                TurnRunStatus::Running.as_str(),
                TurnRunStatus::Cancelling.as_str(),
            ])
            .and(Expr::col(TurnRun::LeaseToken).is_not_null())
            .and(Expr::col(TurnRun::LeaseExpiresAt).is_not_null());
        let no_lease = Expr::col(TurnRun::Status)
            .ne(TurnRunStatus::Running.as_str())
            .and(Expr::col(TurnRun::Status).ne(TurnRunStatus::Cancelling.as_str()))
            .and(Expr::col(TurnRun::LeaseToken).is_null())
            .and(Expr::col(TurnRun::LeaseExpiresAt).is_null());
        let completed_output = Expr::col(TurnRun::Status)
            .eq(TurnRunStatus::Completed.as_str())
            .and(Expr::col(TurnRun::OutputMessageId).is_not_null());
        let no_output = Expr::col(TurnRun::Status)
            .ne(TurnRunStatus::Completed.as_str())
            .and(Expr::col(TurnRun::OutputMessageId).is_null());
        let terminal_finished = Expr::col(TurnRun::Status)
            .is_in([
                TurnRunStatus::Completed.as_str(),
                TurnRunStatus::Failed.as_str(),
                TurnRunStatus::Cancelled.as_str(),
            ])
            .and(Expr::col(TurnRun::FinishedAt).is_not_null());
        let nonterminal_unfinished = Expr::col(TurnRun::Status)
            .is_in([
                TurnRunStatus::Queued.as_str(),
                TurnRunStatus::Running.as_str(),
                TurnRunStatus::Cancelling.as_str(),
                TurnRunStatus::WaitingForClient.as_str(),
                TurnRunStatus::WaitingForAgentRun.as_str(),
                TurnRunStatus::CancellingClient.as_str(),
                TurnRunStatus::Resuming.as_str(),
                TurnRunStatus::RetryWait.as_str(),
            ])
            .and(Expr::col(TurnRun::FinishedAt).is_null());
        let queued_attempt = Expr::col(TurnRun::Status)
            .eq(TurnRunStatus::Queued.as_str())
            .and(Expr::col(TurnRun::AttemptCount).eq(0))
            .and(Expr::col(TurnRun::ClaimCount).eq(0))
            .and(Expr::col(TurnRun::StartedAt).is_null());
        let leased_attempt = Expr::col(TurnRun::Status)
            .is_in([
                TurnRunStatus::Running.as_str(),
                TurnRunStatus::Cancelling.as_str(),
            ])
            .and(Expr::col(TurnRun::AttemptCount).gte(1))
            .and(Expr::col(TurnRun::StartedAt).is_not_null());
        let retryable_attempt = Expr::col(TurnRun::Status)
            .eq(TurnRunStatus::RetryWait.as_str())
            .and(Expr::col(TurnRun::AttemptCount).gte(1))
            .and(Expr::col(TurnRun::AttemptCount).lt(Expr::col(TurnRun::MaxAttempts)))
            .and(Expr::col(TurnRun::StartedAt).is_not_null());
        let continuation_checkpoint_attempt = Expr::col(TurnRun::Status)
            .is_in([
                TurnRunStatus::WaitingForClient.as_str(),
                TurnRunStatus::WaitingForAgentRun.as_str(),
                TurnRunStatus::CancellingClient.as_str(),
                TurnRunStatus::Resuming.as_str(),
            ])
            .and(Expr::col(TurnRun::AttemptCount).gte(1))
            .and(Expr::col(TurnRun::StartedAt).is_not_null());
        let resolved_attempt = Expr::col(TurnRun::Status)
            .is_in([
                TurnRunStatus::Completed.as_str(),
                TurnRunStatus::Failed.as_str(),
            ])
            .and(Expr::col(TurnRun::AttemptCount).gte(1))
            .and(Expr::col(TurnRun::StartedAt).is_not_null());
        let cancelled_attempt = Expr::col(TurnRun::Status)
            .eq(TurnRunStatus::Cancelled.as_str())
            .and(
                Expr::col(TurnRun::AttemptCount)
                    .eq(0)
                    .and(Expr::col(TurnRun::ClaimCount).eq(0))
                    .and(Expr::col(TurnRun::StartedAt).is_null())
                    .or(Expr::col(TurnRun::AttemptCount)
                        .gte(1)
                        .and(Expr::col(TurnRun::StartedAt).is_not_null())),
            );
        let failure_has_error = Expr::col(TurnRun::Status)
            .is_in([
                TurnRunStatus::RetryWait.as_str(),
                TurnRunStatus::Failed.as_str(),
            ])
            .and(Expr::col(TurnRun::LastErrorCode).is_not_null());
        let success_has_no_error = Expr::col(TurnRun::Status)
            .is_in([
                TurnRunStatus::Queued.as_str(),
                TurnRunStatus::Running.as_str(),
                TurnRunStatus::Cancelling.as_str(),
                TurnRunStatus::WaitingForClient.as_str(),
                TurnRunStatus::WaitingForAgentRun.as_str(),
                TurnRunStatus::CancellingClient.as_str(),
                TurnRunStatus::Resuming.as_str(),
                TurnRunStatus::Completed.as_str(),
                TurnRunStatus::Cancelled.as_str(),
            ])
            .and(Expr::col(TurnRun::LastErrorCode).is_null())
            .and(Expr::col(TurnRun::LastErrorDetail).is_null());
        let coherent_steer_generation = Expr::col(TurnRun::SteerRevision)
            .eq(0)
            .and(Expr::col(TurnRun::LastSteerAppliedAt).is_null())
            .or(Expr::col(TurnRun::SteerRevision)
                .gte(1)
                .and(Expr::col(TurnRun::LastSteerAppliedAt).is_not_null()));

        manager
            .create_table(
                Table::create()
                    .table(TurnRun::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(TurnRun::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(TurnRun::ChatId).uuid().not_null())
                    .col(ColumnDef::new(TurnRun::AgentRunId).uuid().not_null())
                    .col(
                        ColumnDef::new(TurnRun::AgentRunDepth)
                            .small_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(TurnRun::InputMessageId).uuid().not_null())
                    .col(ColumnDef::new(TurnRun::OutputMessageId).uuid())
                    .col(
                        ColumnDef::new(TurnRun::Model)
                            .string_len(crate::model::TurnRun::MAX_MODEL_LEN as u32)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnRun::Status)
                            .string_len(32)
                            .not_null()
                            .default(TurnRunStatus::Queued.as_str()),
                    )
                    .col(
                        ColumnDef::new(TurnRun::AttemptCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(TurnRun::MaxAttempts)
                            .integer()
                            .not_null()
                            .default(crate::model::TurnRun::DEFAULT_MAX_ATTEMPTS),
                    )
                    .col(
                        ColumnDef::new(TurnRun::ClaimCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(TurnRun::ModelSteps)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(TurnRun::InputTokens)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(TurnRun::OutputTokens)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(TurnRun::CacheReadInputTokens)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(TurnRun::CacheCreationInputTokens)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(TurnRun::AvailableAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TurnRun::LeaseToken).uuid())
                    .col(ColumnDef::new(TurnRun::LeaseExpiresAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(TurnRun::StartedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(TurnRun::FinishedAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(TurnRun::LastErrorCode)
                            .string_len(crate::model::TurnRun::MAX_ERROR_CODE_LEN as u32),
                    )
                    .col(
                        ColumnDef::new(TurnRun::LastErrorDetail)
                            .string_len(crate::model::TurnRun::MAX_ERROR_DETAIL_LEN as u32),
                    )
                    .col(
                        ColumnDef::new(TurnRun::SteerRevision)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(TurnRun::LastSteerAppliedAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(TurnRun::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnRun::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_run_chat")
                            .from(TurnRun::Table, TurnRun::ChatId)
                            .to(Chat::Table, Chat::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_run_foreground_agent")
                            .from_tbl(TurnRun::Table)
                            .from_col(TurnRun::AgentRunId)
                            .from_col(TurnRun::ChatId)
                            .from_col(TurnRun::AgentRunDepth)
                            .to_tbl(AgentRun::Table)
                            .to_col(AgentRun::Id)
                            .to_col(AgentRun::ChatId)
                            .to_col(AgentRun::Depth)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_run_input_message")
                            .from_tbl(TurnRun::Table)
                            .from_col(TurnRun::InputMessageId)
                            .from_col(TurnRun::ChatId)
                            .from_col(TurnRun::Id)
                            .to_tbl(Message::Table)
                            .to_col(Message::Id)
                            .to_col(Message::ChatId)
                            .to_col(Message::TurnId)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_run_output_message")
                            .from_tbl(TurnRun::Table)
                            .from_col(TurnRun::OutputMessageId)
                            .from_col(TurnRun::ChatId)
                            .from_col(TurnRun::Id)
                            .to_tbl(Message::Table)
                            .to_col(Message::Id)
                            .to_col(Message::ChatId)
                            .to_col(Message::TurnId)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_run_live_claim")
                            .from_tbl(TurnRun::Table)
                            .from_col(TurnRun::LeaseToken)
                            .from_col(TurnRun::Id)
                            .from_col(TurnRun::AttemptCount)
                            .from_col(TurnRun::ClaimCount)
                            .to_tbl(TurnClaim::Table)
                            .to_col(TurnClaim::Token)
                            .to_col(TurnClaim::TurnId)
                            .to_col(TurnClaim::AttemptCount)
                            .to_col(TurnClaim::ClaimCount)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .check(
                        Func::char_length(Expr::col(TurnRun::Model))
                            .between(1, crate::model::TurnRun::MAX_MODEL_LEN as i32),
                    )
                    .check(Expr::col(TurnRun::AgentRunDepth).eq(0))
                    .check(valid_turn_status)
                    .check(
                        Expr::col(TurnRun::AttemptCount)
                            .gte(0)
                            .and(Expr::col(TurnRun::MaxAttempts).gte(1))
                            .and(
                                Expr::col(TurnRun::AttemptCount)
                                    .lte(Expr::col(TurnRun::MaxAttempts)),
                            ),
                    )
                    .check(Expr::col(TurnRun::ClaimCount).gte(Expr::col(TurnRun::AttemptCount)))
                    .check(active_lease.or(no_lease))
                    .check(completed_output.or(no_output))
                    .check(terminal_finished.or(nonterminal_unfinished))
                    .check(
                        queued_attempt
                            .or(leased_attempt)
                            .or(continuation_checkpoint_attempt)
                            .or(retryable_attempt)
                            .or(resolved_attempt)
                            .or(cancelled_attempt),
                    )
                    .check(failure_has_error.or(success_has_no_error))
                    .check(
                        Expr::col(TurnRun::LastErrorCode)
                            .is_null()
                            .or(Func::char_length(Expr::col(TurnRun::LastErrorCode))
                                .between(1, crate::model::TurnRun::MAX_ERROR_CODE_LEN as i32)),
                    )
                    .check(
                        Expr::col(TurnRun::LastErrorDetail)
                            .is_null()
                            .or(Func::char_length(Expr::col(TurnRun::LastErrorDetail))
                                .between(1, crate::model::TurnRun::MAX_ERROR_DETAIL_LEN as i32)),
                    )
                    .check(Expr::col(TurnRun::SteerRevision).gte(0))
                    .check(coherent_steer_generation)
                    .check(
                        Expr::col(TurnRun::ModelSteps)
                            .gte(0)
                            .and(Expr::col(TurnRun::ModelSteps).lte(i64::from(i32::MAX)))
                            .and(Expr::col(TurnRun::InputTokens).gte(0))
                            .and(Expr::col(TurnRun::InputTokens).lte(i64::from(u32::MAX)))
                            .and(Expr::col(TurnRun::OutputTokens).gte(0))
                            .and(Expr::col(TurnRun::OutputTokens).lte(i64::from(u32::MAX)))
                            .and(Expr::col(TurnRun::CacheReadInputTokens).gte(0))
                            .and(Expr::col(TurnRun::CacheReadInputTokens).lte(i64::from(u32::MAX)))
                            .and(Expr::col(TurnRun::CacheCreationInputTokens).gte(0))
                            .and(
                                Expr::col(TurnRun::CacheCreationInputTokens)
                                    .lte(i64::from(u32::MAX)),
                            ),
                    )
                    .check(
                        Expr::col(TurnRun::LastSteerAppliedAt)
                            .is_null()
                            .or(Expr::col(TurnRun::LastSteerAppliedAt)
                                .gte(Expr::col(TurnRun::CreatedAt))
                                .and(
                                    Expr::col(TurnRun::LastSteerAppliedAt)
                                        .lte(Expr::col(TurnRun::UpdatedAt)),
                                )),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_turn_run_chat_identity")
                    .table(TurnRun::Table)
                    .col(TurnRun::ChatId)
                    .col(TurnRun::Id)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_run_input_message")
                    .table(TurnRun::Table)
                    .col(TurnRun::InputMessageId)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_run_one_active")
                    .table(TurnRun::Table)
                    .col(TurnRun::ChatId)
                    .unique()
                    .and_where(Expr::col(TurnRun::Status).is_in([
                        TurnRunStatus::Queued.as_str(),
                        TurnRunStatus::Running.as_str(),
                        TurnRunStatus::Cancelling.as_str(),
                        TurnRunStatus::WaitingForClient.as_str(),
                        TurnRunStatus::WaitingForAgentRun.as_str(),
                        TurnRunStatus::CancellingClient.as_str(),
                        TurnRunStatus::Resuming.as_str(),
                        TurnRunStatus::RetryWait.as_str(),
                    ]))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_run_due")
                    .table(TurnRun::Table)
                    .col(TurnRun::Status)
                    .col(TurnRun::AvailableAt)
                    .col(TurnRun::CreatedAt)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_run_lease_token")
                    .table(TurnRun::Table)
                    .col(TurnRun::LeaseToken)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_run_stale_lease")
                    .table(TurnRun::Table)
                    .col(TurnRun::Status)
                    .col(TurnRun::LeaseExpiresAt)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_run_history")
                    .table(TurnRun::Table)
                    .col(TurnRun::ChatId)
                    .col(TurnRun::CreatedAt)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_run_admission_owner")
                    .table(TurnRun::Table)
                    .col(TurnRun::Id)
                    .col(TurnRun::ChatId)
                    .col(TurnRun::AgentRunId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(SandboxAgentAdmission::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SandboxAgentAdmission::ChildRunId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SandboxAgentAdmission::ParentRunId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxAgentAdmission::OriginTurnId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxAgentAdmission::ChatId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxAgentAdmission::SpawnCallId)
                            .uuid()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(SandboxAgentAdmission::DelegatedRootId).uuid())
                    .col(ColumnDef::new(SandboxAgentAdmission::DelegatedRelativePath).text())
                    .col(
                        ColumnDef::new(SandboxAgentAdmission::AdmittedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_sandbox_agent_admission_child")
                            .from_tbl(SandboxAgentAdmission::Table)
                            .from_col(SandboxAgentAdmission::ChildRunId)
                            .from_col(SandboxAgentAdmission::ParentRunId)
                            .from_col(SandboxAgentAdmission::ChatId)
                            .from_col(SandboxAgentAdmission::SpawnCallId)
                            .to_tbl(AgentRun::Table)
                            .to_col(AgentRun::Id)
                            .to_col(AgentRun::ParentId)
                            .to_col(AgentRun::ChatId)
                            .to_col(AgentRun::SpawnCallId)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_sandbox_agent_admission_origin_turn")
                            .from_tbl(SandboxAgentAdmission::Table)
                            .from_col(SandboxAgentAdmission::OriginTurnId)
                            .from_col(SandboxAgentAdmission::ChatId)
                            .from_col(SandboxAgentAdmission::ParentRunId)
                            .to_tbl(TurnRun::Table)
                            .to_col(TurnRun::Id)
                            .to_col(TurnRun::ChatId)
                            .to_col(TurnRun::AgentRunId)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .check(
                        Expr::col(SandboxAgentAdmission::DelegatedRootId)
                            .is_null()
                            .and(Expr::col(SandboxAgentAdmission::DelegatedRelativePath).is_null())
                            .or(Expr::col(SandboxAgentAdmission::DelegatedRootId)
                                .is_not_null()
                                .and(
                                    Expr::col(SandboxAgentAdmission::DelegatedRelativePath)
                                        .is_not_null(),
                                )),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_sandbox_agent_admission_outstanding")
                    .table(SandboxAgentAdmission::Table)
                    .col(SandboxAgentAdmission::OriginTurnId)
                    .col(SandboxAgentAdmission::AdmittedAt)
                    .col(SandboxAgentAdmission::ChildRunId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(TurnAgentRunWait::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TurnAgentRunWait::ChildRunId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(TurnAgentRunWait::ParentRunId)
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TurnAgentRunWait::TurnId).uuid().not_null())
                    .col(ColumnDef::new(TurnAgentRunWait::ChatId).uuid().not_null())
                    .col(
                        ColumnDef::new(TurnAgentRunWait::ParkLeaseToken)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnAgentRunWait::AtomicAdmission)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(TurnAgentRunWait::AttemptCount)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnAgentRunWait::ClaimCount)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnAgentRunWait::ModelSteps)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnAgentRunWait::InputTokens)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnAgentRunWait::OutputTokens)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnAgentRunWait::CacheReadInputTokens)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnAgentRunWait::CacheCreationInputTokens)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TurnAgentRunWait::Status).text().not_null())
                    .col(
                        ColumnDef::new(TurnAgentRunWait::ParkedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TurnAgentRunWait::ClosedAt).timestamp_with_time_zone())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_agent_run_wait_turn")
                            .from_tbl(TurnAgentRunWait::Table)
                            .from_col(TurnAgentRunWait::TurnId)
                            .to_tbl(TurnRun::Table)
                            .to_col(TurnRun::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_agent_run_wait_child")
                            .from_tbl(TurnAgentRunWait::Table)
                            .from_col(TurnAgentRunWait::ChildRunId)
                            .from_col(TurnAgentRunWait::ParentRunId)
                            .from_col(TurnAgentRunWait::ChatId)
                            .to_tbl(AgentRun::Table)
                            .to_col(AgentRun::Id)
                            .to_col(AgentRun::ParentId)
                            .to_col(AgentRun::ChatId)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_agent_run_wait_claim")
                            .from_tbl(TurnAgentRunWait::Table)
                            .from_col(TurnAgentRunWait::ParkLeaseToken)
                            .from_col(TurnAgentRunWait::TurnId)
                            .from_col(TurnAgentRunWait::AttemptCount)
                            .from_col(TurnAgentRunWait::ClaimCount)
                            .to_tbl(TurnClaim::Table)
                            .to_col(TurnClaim::Token)
                            .to_col(TurnClaim::TurnId)
                            .to_col(TurnClaim::AttemptCount)
                            .to_col(TurnClaim::ClaimCount)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .check(Expr::col(TurnAgentRunWait::Status).is_in([
                        TurnAgentRunWaitStatus::Waiting.as_str(),
                        TurnAgentRunWaitStatus::Resumed.as_str(),
                        TurnAgentRunWaitStatus::Cancelled.as_str(),
                    ]))
                    .check(
                        Expr::col(TurnAgentRunWait::Status)
                            .eq(TurnAgentRunWaitStatus::Waiting.as_str())
                            .and(Expr::col(TurnAgentRunWait::ClosedAt).is_null())
                            .or(Expr::col(TurnAgentRunWait::Status)
                                .ne(TurnAgentRunWaitStatus::Waiting.as_str())
                                .and(Expr::col(TurnAgentRunWait::ClosedAt).is_not_null())),
                    )
                    .check(Expr::col(TurnAgentRunWait::AttemptCount).gte(1))
                    .check(
                        Expr::col(TurnAgentRunWait::ClaimCount)
                            .gte(Expr::col(TurnAgentRunWait::AttemptCount)),
                    )
                    .check(
                        Expr::col(TurnAgentRunWait::ModelSteps)
                            .gt(0)
                            .and(Expr::col(TurnAgentRunWait::InputTokens).gte(0))
                            .and(Expr::col(TurnAgentRunWait::OutputTokens).gte(0))
                            .and(Expr::col(TurnAgentRunWait::CacheReadInputTokens).gte(0))
                            .and(Expr::col(TurnAgentRunWait::CacheCreationInputTokens).gte(0)),
                    )
                    .check(
                        Expr::col(TurnAgentRunWait::ClosedAt)
                            .is_null()
                            .or(Expr::col(TurnAgentRunWait::ClosedAt)
                                .gte(Expr::col(TurnAgentRunWait::ParkedAt))),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_agent_run_wait_one_open")
                    .table(TurnAgentRunWait::Table)
                    .col(TurnAgentRunWait::TurnId)
                    .unique()
                    .and_where(
                        Expr::col(TurnAgentRunWait::Status)
                            .eq(TurnAgentRunWaitStatus::Waiting.as_str()),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(TurnFailure::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TurnFailure::LeaseToken)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(TurnFailure::TurnId).uuid().not_null())
                    .col(
                        ColumnDef::new(TurnFailure::AttemptCount)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TurnFailure::ModelSteps).integer().not_null())
                    .col(
                        ColumnDef::new(TurnFailure::InputTokens)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnFailure::OutputTokens)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnFailure::CacheReadInputTokens)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnFailure::CacheCreationInputTokens)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TurnFailure::RequestedRetryAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(TurnFailure::ErrorCode)
                            .string_len(crate::model::TurnRun::MAX_ERROR_CODE_LEN as u32)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnFailure::ErrorDetail)
                            .string_len(crate::model::TurnRun::MAX_ERROR_DETAIL_LEN as u32),
                    )
                    .col(
                        ColumnDef::new(TurnFailure::ResolvedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnFailure::ResultStatus)
                            .string_len(32)
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_failure_claim")
                            .from_tbl(TurnFailure::Table)
                            .from_col(TurnFailure::LeaseToken)
                            .from_col(TurnFailure::TurnId)
                            .from_col(TurnFailure::AttemptCount)
                            .to_tbl(TurnClaim::Table)
                            .to_col(TurnClaim::Token)
                            .to_col(TurnClaim::TurnId)
                            .to_col(TurnClaim::AttemptCount)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .check(Expr::col(TurnFailure::AttemptCount).gte(1))
                    .check(Expr::col(TurnFailure::ModelSteps).gte(0))
                    .check(Expr::col(TurnFailure::ModelSteps).lte(i32::MAX))
                    .check(Expr::col(TurnFailure::InputTokens).gte(0))
                    .check(Expr::col(TurnFailure::OutputTokens).gte(0))
                    .check(Expr::col(TurnFailure::CacheReadInputTokens).gte(0))
                    .check(Expr::col(TurnFailure::CacheCreationInputTokens).gte(0))
                    .check(Expr::col(TurnFailure::InputTokens).lte(i64::from(u32::MAX)))
                    .check(Expr::col(TurnFailure::OutputTokens).lte(i64::from(u32::MAX)))
                    .check(Expr::col(TurnFailure::CacheReadInputTokens).lte(i64::from(u32::MAX)))
                    .check(
                        Expr::col(TurnFailure::CacheCreationInputTokens).lte(i64::from(u32::MAX)),
                    )
                    .check(Expr::col(TurnFailure::ResultStatus).is_in([
                        TurnRunStatus::RetryWait.as_str(),
                        TurnRunStatus::Failed.as_str(),
                    ]))
                    .check(
                        Expr::col(TurnFailure::ResultStatus)
                            .ne(TurnRunStatus::RetryWait.as_str())
                            .or(Expr::col(TurnFailure::RequestedRetryAt).is_not_null()),
                    )
                    .check(
                        Expr::col(TurnFailure::RequestedRetryAt)
                            .is_null()
                            .or(Expr::col(TurnFailure::RequestedRetryAt)
                                .gt(Expr::col(TurnFailure::ResolvedAt))),
                    )
                    .check(
                        Func::char_length(Expr::col(TurnFailure::ErrorCode))
                            .between(1, crate::model::TurnRun::MAX_ERROR_CODE_LEN as i32),
                    )
                    .check(
                        Expr::col(TurnFailure::ErrorDetail)
                            .is_null()
                            .or(Func::char_length(Expr::col(TurnFailure::ErrorDetail))
                                .between(1, crate::model::TurnRun::MAX_ERROR_DETAIL_LEN as i32)),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Setting::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Setting::Key).text().not_null().primary_key())
                    .col(ColumnDef::new(Setting::ValueJson).json_binary().not_null())
                    .to_owned(),
            )
            .await?;

        let pending_steer = Expr::col(TurnSteer::Status)
            .eq(TurnSteerStatus::Pending.as_str())
            .and(Expr::col(TurnSteer::AppliedLeaseToken).is_null())
            .and(Expr::col(TurnSteer::MessageId).is_null())
            .and(Expr::col(TurnSteer::PrecedingAssistantMessageId).is_null())
            .and(Expr::col(TurnSteer::ResolvedAt).is_null());
        let applied_steer = Expr::col(TurnSteer::Status)
            .eq(TurnSteerStatus::Applied.as_str())
            .and(Expr::col(TurnSteer::AppliedLeaseToken).is_not_null())
            .and(Expr::col(TurnSteer::MessageId).is_not_null())
            .and(Expr::col(TurnSteer::ResolvedAt).is_not_null());
        let rejected_steer = Expr::col(TurnSteer::Status)
            .eq(TurnSteerStatus::Rejected.as_str())
            .and(Expr::col(TurnSteer::AppliedLeaseToken).is_null())
            .and(Expr::col(TurnSteer::MessageId).is_null())
            .and(Expr::col(TurnSteer::PrecedingAssistantMessageId).is_null())
            .and(Expr::col(TurnSteer::ResolvedAt).is_not_null());
        manager
            .create_table(
                Table::create()
                    .table(TurnSteer::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TurnSteer::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(TurnSteer::TurnId).uuid().not_null())
                    .col(ColumnDef::new(TurnSteer::ChatId).uuid().not_null())
                    .col(ColumnDef::new(TurnSteer::Content).text().not_null())
                    .col(
                        ColumnDef::new(TurnSteer::Interrupt)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(TurnSteer::Status)
                            .string_len(16)
                            .not_null()
                            .default(TurnSteerStatus::Pending.as_str()),
                    )
                    .col(ColumnDef::new(TurnSteer::AppliedLeaseToken).uuid())
                    .col(ColumnDef::new(TurnSteer::MessageId).uuid())
                    .col(ColumnDef::new(TurnSteer::PrecedingAssistantMessageId).uuid())
                    .col(
                        ColumnDef::new(TurnSteer::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TurnSteer::ResolvedAt).timestamp_with_time_zone())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_steer_turn")
                            .from_tbl(TurnSteer::Table)
                            .from_col(TurnSteer::ChatId)
                            .from_col(TurnSteer::TurnId)
                            .to_tbl(TurnRun::Table)
                            .to_col(TurnRun::ChatId)
                            .to_col(TurnRun::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_steer_preceding_assistant")
                            .from_tbl(TurnSteer::Table)
                            .from_col(TurnSteer::PrecedingAssistantMessageId)
                            .from_col(TurnSteer::ChatId)
                            .from_col(TurnSteer::TurnId)
                            .to_tbl(Message::Table)
                            .to_col(Message::Id)
                            .to_col(Message::ChatId)
                            .to_col(Message::TurnId)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_steer_claim")
                            .from_tbl(TurnSteer::Table)
                            .from_col(TurnSteer::AppliedLeaseToken)
                            .from_col(TurnSteer::TurnId)
                            .to_tbl(TurnClaim::Table)
                            .to_col(TurnClaim::Token)
                            .to_col(TurnClaim::TurnId)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_steer_message")
                            .from_tbl(TurnSteer::Table)
                            .from_col(TurnSteer::MessageId)
                            .from_col(TurnSteer::ChatId)
                            .from_col(TurnSteer::TurnId)
                            .to_tbl(Message::Table)
                            .to_col(Message::Id)
                            .to_col(Message::ChatId)
                            .to_col(Message::TurnId)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .check(
                        Func::char_length(Expr::col(TurnSteer::Content))
                            .between(1, crate::model::TurnSteer::MAX_CONTENT_LEN as i32),
                    )
                    .check(pending_steer.or(applied_steer).or(rejected_steer))
                    .check(
                        Expr::col(TurnSteer::MessageId)
                            .is_null()
                            .or(Expr::col(TurnSteer::MessageId).eq(Expr::col(TurnSteer::Id))),
                    )
                    .check(
                        Expr::col(TurnSteer::ResolvedAt)
                            .is_null()
                            .or(Expr::col(TurnSteer::ResolvedAt)
                                .gte(Expr::col(TurnSteer::CreatedAt))),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_steer_pending")
                    .table(TurnSteer::Table)
                    .col(TurnSteer::TurnId)
                    .col(TurnSteer::Status)
                    .col(TurnSteer::CreatedAt)
                    .col(TurnSteer::Id)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_steer_message")
                    .table(TurnSteer::Table)
                    .col(TurnSteer::MessageId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(TurnSteer::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(TurnFailure::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(TurnAgentRunWait::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(SandboxAgentAdmission::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(TurnRun::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(AgentRunInbox::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(AgentRunResult::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(AgentRunCancellation::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(AgentRun::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(AgentRunClaim::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(AgentRunClaimLock::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Setting::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Message::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(MessageIdentity::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(TurnClaim::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(TurnClaimLock::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Chat::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Project::Table).to_owned())
            .await?;
        Ok(())
    }
}

/// Adds the per-chat event journal that clients replay from on connect.
struct AddEventJournal;

impl MigrationName for AddEventJournal {
    fn name(&self) -> &str {
        "m0002_event_journal"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddEventJournal {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Event::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Event::ChatId).uuid().not_null())
                    .col(ColumnDef::new(Event::Seq).big_integer().not_null())
                    .col(ColumnDef::new(Event::TurnId).uuid())
                    .col(ColumnDef::new(Event::LeaseToken).uuid())
                    .col(ColumnDef::new(Event::AttemptEventOrdinal).integer())
                    .col(ColumnDef::new(Event::ScanToken).uuid())
                    .col(
                        ColumnDef::new(Event::Terminal)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(Event::Payload).json_binary().not_null())
                    .col(
                        ColumnDef::new(Event::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(Index::create().col(Event::ChatId).col(Event::Seq))
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_event_chat")
                            .from(Event::Table, Event::ChatId)
                            .to(Chat::Table, Chat::Id),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_event_turn")
                            .from_tbl(Event::Table)
                            .from_col(Event::ChatId)
                            .from_col(Event::TurnId)
                            .to_tbl(TurnRun::Table)
                            .to_col(TurnRun::ChatId)
                            .to_col(TurnRun::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_event_turn_claim")
                            .from_tbl(Event::Table)
                            .from_col(Event::TurnId)
                            .from_col(Event::LeaseToken)
                            .to_tbl(TurnClaim::Table)
                            .to_col(TurnClaim::TurnId)
                            .to_col(TurnClaim::Token)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .check(
                        Expr::col(Event::Terminal)
                            .eq(false)
                            .or(Expr::col(Event::TurnId).is_not_null()),
                    )
                    .check(
                        Expr::col(Event::LeaseToken)
                            .is_null()
                            .and(Expr::col(Event::AttemptEventOrdinal).is_null())
                            .or(Expr::col(Event::LeaseToken).is_not_null().and(
                                Expr::col(Event::AttemptEventOrdinal)
                                    .is_not_null()
                                    .and(Expr::col(Event::TurnId).is_not_null()),
                            )),
                    )
                    .check(
                        Expr::col(Event::AttemptEventOrdinal)
                            .is_null()
                            .or(Expr::col(Event::AttemptEventOrdinal).gte(1)),
                    )
                    .check(
                        Expr::col(Event::TurnId)
                            .is_null()
                            .or(Expr::col(Event::Terminal).eq(true))
                            .or(Expr::col(Event::LeaseToken).is_not_null()),
                    )
                    .check(
                        Expr::col(Event::ScanToken)
                            .is_null()
                            .or(Expr::col(Event::Terminal).eq(true)),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_event_attempt_ordinal")
                    .table(Event::Table)
                    .col(Event::LeaseToken)
                    .col(Event::AttemptEventOrdinal)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_event_scan_token")
                    .table(Event::Table)
                    .col(Event::ScanToken)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_event_one_terminal_per_turn")
                    .table(Event::Table)
                    .col(Event::TurnId)
                    .unique()
                    .and_where(Expr::col(Event::Terminal).eq(true))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Event::Table).to_owned())
            .await?;
        Ok(())
    }
}

/// Adds ordered host-root projection rows for projects and chats.
struct AddProjects;

impl MigrationName for AddProjects {
    fn name(&self) -> &str {
        "m0003_projects"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddProjects {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ProjectRootAttachment::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ProjectRootAttachment::ProjectId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectRootAttachment::RootId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectRootAttachment::Position)
                            .integer()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(ProjectRootAttachment::ProjectId)
                            .col(ProjectRootAttachment::RootId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_project_root_attachment_project")
                            .from(
                                ProjectRootAttachment::Table,
                                ProjectRootAttachment::ProjectId,
                            )
                            .to(Project::Table, Project::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .check(Expr::col(ProjectRootAttachment::RootId).ne(uuid::Uuid::nil()))
                    .check(Expr::col(ProjectRootAttachment::Position).gte(0))
                    .check(
                        Expr::col(ProjectRootAttachment::Position)
                            .lt(crate::model::MAX_ROOT_ATTACHMENTS as i32),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_project_root_attachment_position")
                    .table(ProjectRootAttachment::Table)
                    .col(ProjectRootAttachment::ProjectId)
                    .col(ProjectRootAttachment::Position)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(ChatRootAttachment::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(ChatRootAttachment::ChatId).uuid().not_null())
                    .col(ColumnDef::new(ChatRootAttachment::RootId).uuid().not_null())
                    .col(
                        ColumnDef::new(ChatRootAttachment::Position)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ChatRootAttachment::Origin)
                            .string_len(24)
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(ChatRootAttachment::ChatId)
                            .col(ChatRootAttachment::RootId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_chat_root_attachment_chat")
                            .from(ChatRootAttachment::Table, ChatRootAttachment::ChatId)
                            .to(Chat::Table, Chat::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .check(Expr::col(ChatRootAttachment::RootId).ne(uuid::Uuid::nil()))
                    .check(Expr::col(ChatRootAttachment::Position).gte(0))
                    .check(
                        Expr::col(ChatRootAttachment::Position)
                            .lt(crate::model::MAX_ROOT_ATTACHMENTS as i32),
                    )
                    .check(
                        Expr::col(ChatRootAttachment::Origin)
                            .is_in(["project_default", "conversation"]),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_chat_root_attachment_position")
                    .table(ChatRootAttachment::Table)
                    .col(ChatRootAttachment::ChatId)
                    .col(ChatRootAttachment::Position)
                    .unique()
                    .to_owned(),
            )
            .await?;

        let projection_metadata_present = Expr::col(RootAttachmentChange::ProjectionExistedBefore)
            .eq(true)
            .or(Expr::col(RootAttachmentChange::Action).eq("attach"));
        let projection_metadata_absent = Expr::col(RootAttachmentChange::ProjectionExistedBefore)
            .eq(false)
            .and(Expr::col(RootAttachmentChange::Action).eq("detach"));
        let awaiting_broker = Expr::col(RootAttachmentChange::Phase)
            .eq("awaiting_broker")
            .and(Expr::col(RootAttachmentChange::ResultRevision).is_null())
            .and(Expr::col(RootAttachmentChange::ProjectionChanged).is_null())
            .and(Expr::col(RootAttachmentChange::BrokerChanged).is_null())
            .and(Expr::col(RootAttachmentChange::BrokerCurrentlyAttached).is_null())
            .and(Expr::col(RootAttachmentChange::FailureCode).is_null())
            .and(Expr::col(RootAttachmentChange::FailureMessage).is_null())
            .and(Expr::col(RootAttachmentChange::FailureRetryable).is_null())
            .and(Expr::col(RootAttachmentChange::FinishedAt).is_null());
        let completed = Expr::col(RootAttachmentChange::Phase)
            .eq("completed")
            .and(Expr::col(RootAttachmentChange::ResultRevision).is_not_null())
            .and(Expr::col(RootAttachmentChange::ProjectionChanged).is_not_null())
            .and(Expr::col(RootAttachmentChange::BrokerChanged).is_not_null())
            .and(Expr::col(RootAttachmentChange::BrokerCurrentlyAttached).is_not_null())
            .and(Expr::col(RootAttachmentChange::FailureCode).is_null())
            .and(Expr::col(RootAttachmentChange::FailureMessage).is_null())
            .and(Expr::col(RootAttachmentChange::FailureRetryable).is_null())
            .and(Expr::col(RootAttachmentChange::FinishedAt).is_not_null());
        let failed = Expr::col(RootAttachmentChange::Phase)
            .eq("failed")
            .and(Expr::col(RootAttachmentChange::ResultRevision).is_not_null())
            .and(Expr::col(RootAttachmentChange::ProjectionChanged).is_not_null())
            .and(Expr::col(RootAttachmentChange::FailureCode).is_not_null())
            .and(Expr::col(RootAttachmentChange::FailureMessage).is_not_null())
            .and(Expr::col(RootAttachmentChange::FailureRetryable).is_not_null())
            .and(Expr::col(RootAttachmentChange::FinishedAt).is_not_null());

        manager
            .create_table(
                Table::create()
                    .table(RootAttachmentChange::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(RootAttachmentChange::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(RootAttachmentChange::ChatId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RootAttachmentChange::SubjectKind)
                            .string_len(24)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RootAttachmentChange::SubjectId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RootAttachmentChange::ExecutorId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RootAttachmentChange::RootId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RootAttachmentChange::Action)
                            .string_len(16)
                            .not_null(),
                    )
                    .col(ColumnDef::new(RootAttachmentChange::Origin).string_len(24))
                    .col(ColumnDef::new(RootAttachmentChange::ProjectionPosition).integer())
                    .col(
                        ColumnDef::new(RootAttachmentChange::ProjectionExistedBefore)
                            .boolean()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RootAttachmentChange::ExpectedRevision)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RootAttachmentChange::BeforeRevision)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RootAttachmentChange::IntentRevision)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RootAttachmentChange::Phase)
                            .string_len(24)
                            .not_null(),
                    )
                    .col(ColumnDef::new(RootAttachmentChange::ResultRevision).big_integer())
                    .col(ColumnDef::new(RootAttachmentChange::ProjectionChanged).boolean())
                    .col(ColumnDef::new(RootAttachmentChange::BrokerChanged).boolean())
                    .col(ColumnDef::new(RootAttachmentChange::BrokerCurrentlyAttached).boolean())
                    .col(ColumnDef::new(RootAttachmentChange::FailureCode).string_len(64))
                    .col(ColumnDef::new(RootAttachmentChange::FailureMessage).string_len(256))
                    .col(ColumnDef::new(RootAttachmentChange::FailureRetryable).boolean())
                    .col(
                        ColumnDef::new(RootAttachmentChange::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RootAttachmentChange::FinishedAt).timestamp_with_time_zone(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_root_attachment_change_chat")
                            .from(RootAttachmentChange::Table, RootAttachmentChange::ChatId)
                            .to(Chat::Table, Chat::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .check(Expr::col(RootAttachmentChange::Id).ne(uuid::Uuid::nil()))
                    .check(Expr::col(RootAttachmentChange::ChatId).ne(uuid::Uuid::nil()))
                    .check(Expr::col(RootAttachmentChange::SubjectId).ne(uuid::Uuid::nil()))
                    .check(Expr::col(RootAttachmentChange::ExecutorId).ne(uuid::Uuid::nil()))
                    .check(Expr::col(RootAttachmentChange::RootId).ne(uuid::Uuid::nil()))
                    .check(
                        Expr::col(RootAttachmentChange::SubjectKind)
                            .is_in(["project", "conversation"]),
                    )
                    .check(Expr::col(RootAttachmentChange::Action).is_in(["attach", "detach"]))
                    .check(
                        Expr::col(RootAttachmentChange::Origin)
                            .is_null()
                            .or(Expr::col(RootAttachmentChange::Origin)
                                .is_in(["project_default", "conversation"])),
                    )
                    .check(
                        projection_metadata_present
                            .and(Expr::col(RootAttachmentChange::Origin).is_not_null())
                            .and(Expr::col(RootAttachmentChange::ProjectionPosition).is_not_null())
                            .or(projection_metadata_absent
                                .and(Expr::col(RootAttachmentChange::Origin).is_null())
                                .and(
                                    Expr::col(RootAttachmentChange::ProjectionPosition).is_null(),
                                )),
                    )
                    .check(
                        Expr::col(RootAttachmentChange::ProjectionPosition)
                            .is_null()
                            .or(Expr::col(RootAttachmentChange::ProjectionPosition)
                                .between(0, crate::model::MAX_ROOT_ATTACHMENTS as i32 - 1)),
                    )
                    .check(
                        Expr::col(RootAttachmentChange::Action)
                            .ne("attach")
                            .or(Expr::col(RootAttachmentChange::ProjectionExistedBefore).eq(true))
                            .or(Expr::col(RootAttachmentChange::Origin).eq("conversation")),
                    )
                    .check(
                        Expr::col(RootAttachmentChange::ExpectedRevision)
                            .between(0, crate::model::MAX_ATTACHMENT_REVISION),
                    )
                    .check(
                        Expr::col(RootAttachmentChange::BeforeRevision)
                            .between(0, crate::model::MAX_ATTACHMENT_REVISION),
                    )
                    .check(
                        Expr::col(RootAttachmentChange::IntentRevision)
                            .between(0, crate::model::MAX_ATTACHMENT_REVISION),
                    )
                    .check(
                        Expr::col(RootAttachmentChange::ResultRevision)
                            .is_null()
                            .or(Expr::col(RootAttachmentChange::ResultRevision)
                                .between(0, crate::model::MAX_ATTACHMENT_REVISION)),
                    )
                    .check(
                        Expr::col(RootAttachmentChange::ExpectedRevision)
                            .equals(RootAttachmentChange::BeforeRevision),
                    )
                    .check(
                        Expr::col(RootAttachmentChange::IntentRevision)
                            .equals(RootAttachmentChange::BeforeRevision)
                            .or(Expr::col(RootAttachmentChange::IntentRevision)
                                .eq(Expr::col(RootAttachmentChange::BeforeRevision).add(1))),
                    )
                    .check(
                        Expr::col(RootAttachmentChange::Action)
                            .ne("attach")
                            .or(Expr::col(RootAttachmentChange::ProjectionExistedBefore).eq(true))
                            .or(Expr::col(RootAttachmentChange::IntentRevision)
                                .eq(Expr::col(RootAttachmentChange::BeforeRevision).add(1))),
                    )
                    .check(
                        Expr::col(RootAttachmentChange::Action)
                            .eq("attach")
                            .and(Expr::col(RootAttachmentChange::ProjectionExistedBefore).eq(false))
                            .or(Expr::col(RootAttachmentChange::IntentRevision)
                                .equals(RootAttachmentChange::BeforeRevision)),
                    )
                    .check(
                        Expr::col(RootAttachmentChange::Action)
                            .ne("attach")
                            .or(Expr::col(RootAttachmentChange::ProjectionExistedBefore).eq(true))
                            .or(Expr::col(RootAttachmentChange::BeforeRevision)
                                .lte(crate::model::MAX_ATTACHMENT_REVISION - 2)),
                    )
                    .check(
                        Expr::col(RootAttachmentChange::Action)
                            .ne("detach")
                            .or(Expr::col(RootAttachmentChange::ProjectionExistedBefore).eq(false))
                            .or(Expr::col(RootAttachmentChange::BeforeRevision)
                                .lte(crate::model::MAX_ATTACHMENT_REVISION - 1)),
                    )
                    .check(
                        Expr::col(RootAttachmentChange::FailureCode).is_null().or(
                            Func::char_length(Expr::col(RootAttachmentChange::FailureCode))
                                .between(1, 64),
                        ),
                    )
                    .check(
                        Expr::col(RootAttachmentChange::FailureMessage)
                            .is_null()
                            .or(
                                Func::char_length(Expr::col(RootAttachmentChange::FailureMessage))
                                    .between(1, 256),
                            ),
                    )
                    .check(awaiting_broker.or(completed).or(failed))
                    .check(
                        Expr::col(RootAttachmentChange::FinishedAt)
                            .is_null()
                            .or(Expr::col(RootAttachmentChange::FinishedAt)
                                .gte(Expr::col(RootAttachmentChange::CreatedAt))),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_root_attachment_change_one_awaiting")
                    .table(RootAttachmentChange::Table)
                    .col(RootAttachmentChange::ChatId)
                    .unique()
                    .and_where(Expr::col(RootAttachmentChange::Phase).eq("awaiting_broker"))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_root_attachment_change_pending_scan")
                    .table(RootAttachmentChange::Table)
                    .col(RootAttachmentChange::ExecutorId)
                    .col(RootAttachmentChange::Phase)
                    .col(RootAttachmentChange::CreatedAt)
                    .col(RootAttachmentChange::Id)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_root_attachment_change_history")
                    .table(RootAttachmentChange::Table)
                    .col(RootAttachmentChange::ChatId)
                    .col(RootAttachmentChange::CreatedAt)
                    .col(RootAttachmentChange::Id)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(RootAttachmentChange::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(ChatRootAttachment::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(ProjectRootAttachment::Table).to_owned())
            .await?;
        Ok(())
    }
}

/// Adds the optional per-chat `model` override.
struct AddChatModel;

impl MigrationName for AddChatModel {
    fn name(&self) -> &str {
        "m0004_chat_model"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddChatModel {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Chat::Table)
                    .add_column(ColumnDef::new(Chat::Model).text())
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Chat::Table)
                    .drop_column(Chat::Model)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

/// Structured tool-call rows (args + result), distinct from text messages.
struct AddToolCalls;

async fn create_retrieval_evidence_table(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(RetrievalEvidence::Table)
                .col(ColumnDef::new(RetrievalEvidence::CallId).uuid().not_null())
                .col(ColumnDef::new(RetrievalEvidence::Rank).integer().not_null())
                .col(
                    ColumnDef::new(RetrievalEvidence::SourceToken)
                        .uuid()
                        .not_null(),
                )
                .col(ColumnDef::new(RetrievalEvidence::ChatId).uuid().not_null())
                .col(ColumnDef::new(RetrievalEvidence::TurnId).uuid().not_null())
                .col(
                    ColumnDef::new(RetrievalEvidence::DocumentId)
                        .uuid()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(RetrievalEvidence::ContentRevision)
                        .big_integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(RetrievalEvidence::RevisionToken)
                        .uuid()
                        .not_null(),
                )
                .col(ColumnDef::new(RetrievalEvidence::ChunkId).uuid().not_null())
                .col(
                    ColumnDef::new(RetrievalEvidence::SpanStart)
                        .big_integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(RetrievalEvidence::SpanEnd)
                        .big_integer()
                        .not_null(),
                )
                .col(ColumnDef::new(RetrievalEvidence::Snippet).text().not_null())
                .col(
                    ColumnDef::new(RetrievalEvidence::HeadingPath)
                        .json_binary()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(RetrievalEvidence::SourceRegions)
                        .json_binary()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(RetrievalEvidence::SourceKind)
                        .text()
                        .not_null(),
                )
                .col(ColumnDef::new(RetrievalEvidence::SourceUri).text())
                .primary_key(
                    Index::create()
                        .col(RetrievalEvidence::CallId)
                        .col(RetrievalEvidence::Rank),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_retrieval_evidence_tool_call")
                        .from_tbl(RetrievalEvidence::Table)
                        .from_col(RetrievalEvidence::CallId)
                        .from_col(RetrievalEvidence::ChatId)
                        .from_col(RetrievalEvidence::TurnId)
                        .to_tbl(ToolCall::Table)
                        .to_col(ToolCall::Id)
                        .to_col(ToolCall::ChatId)
                        .to_col(ToolCall::TurnId)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .check(Expr::col(RetrievalEvidence::Rank).between(1, 20))
                .check(Expr::col(RetrievalEvidence::ContentRevision).gte(1))
                .check(Expr::col(RetrievalEvidence::SpanStart).gte(0))
                .check(
                    Expr::col(RetrievalEvidence::SpanEnd)
                        .gt(Expr::col(RetrievalEvidence::SpanStart)),
                )
                .check(Expr::col(RetrievalEvidence::SourceKind).is_in(["uri", "inline"]))
                .check(
                    Expr::col(RetrievalEvidence::SourceKind)
                        .eq("uri")
                        .and(Expr::col(RetrievalEvidence::SourceUri).is_not_null())
                        .or(Expr::col(RetrievalEvidence::SourceKind)
                            .eq("inline")
                            .and(Expr::col(RetrievalEvidence::SourceUri).is_null())),
                )
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_retrieval_evidence_source_token")
                .table(RetrievalEvidence::Table)
                .col(RetrievalEvidence::SourceToken)
                .unique()
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_retrieval_evidence_exact_owner")
                .table(RetrievalEvidence::Table)
                .col(RetrievalEvidence::CallId)
                .col(RetrievalEvidence::Rank)
                .col(RetrievalEvidence::ChatId)
                .col(RetrievalEvidence::TurnId)
                .unique()
                .to_owned(),
        )
        .await
}

async fn create_assistant_citation_table(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(AssistantCitation::Table)
                .col(
                    ColumnDef::new(AssistantCitation::Id)
                        .uuid()
                        .not_null()
                        .primary_key(),
                )
                .col(
                    ColumnDef::new(AssistantCitation::MessageId)
                        .uuid()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(AssistantCitation::Ordinal)
                        .integer()
                        .not_null(),
                )
                .col(ColumnDef::new(AssistantCitation::ChatId).uuid().not_null())
                .col(ColumnDef::new(AssistantCitation::TurnId).uuid().not_null())
                .col(
                    ColumnDef::new(AssistantCitation::EvidenceCallId)
                        .uuid()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(AssistantCitation::EvidenceRank)
                        .integer()
                        .not_null(),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_assistant_citation_message")
                        .from_tbl(AssistantCitation::Table)
                        .from_col(AssistantCitation::MessageId)
                        .from_col(AssistantCitation::ChatId)
                        .from_col(AssistantCitation::TurnId)
                        .to_tbl(Message::Table)
                        .to_col(Message::Id)
                        .to_col(Message::ChatId)
                        .to_col(Message::TurnId)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_assistant_citation_evidence")
                        .from_tbl(AssistantCitation::Table)
                        .from_col(AssistantCitation::EvidenceCallId)
                        .from_col(AssistantCitation::EvidenceRank)
                        .from_col(AssistantCitation::ChatId)
                        .from_col(AssistantCitation::TurnId)
                        .to_tbl(RetrievalEvidence::Table)
                        .to_col(RetrievalEvidence::CallId)
                        .to_col(RetrievalEvidence::Rank)
                        .to_col(RetrievalEvidence::ChatId)
                        .to_col(RetrievalEvidence::TurnId)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .check(
                    Expr::col(AssistantCitation::Ordinal)
                        .between(1, crate::MAX_ASSISTANT_CITATIONS as i32),
                )
                .check(Expr::col(AssistantCitation::EvidenceRank).between(1, 20))
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_assistant_citation_message_ordinal")
                .table(AssistantCitation::Table)
                .col(AssistantCitation::MessageId)
                .col(AssistantCitation::Ordinal)
                .unique()
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_assistant_citation_message_evidence")
                .table(AssistantCitation::Table)
                .col(AssistantCitation::MessageId)
                .col(AssistantCitation::EvidenceCallId)
                .col(AssistantCitation::EvidenceRank)
                .unique()
                .to_owned(),
        )
        .await
}

impl MigrationName for AddToolCalls {
    fn name(&self) -> &str {
        "m0005_tool_calls"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddToolCalls {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ToolCall::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(ToolCall::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(ToolCall::ChatId).uuid().not_null())
                    .col(ColumnDef::new(ToolCall::TurnId).uuid().not_null())
                    .col(ColumnDef::new(ToolCall::ProviderId).text().not_null())
                    .col(ColumnDef::new(ToolCall::HistoryOrder).big_integer().not_null())
                    .col(ColumnDef::new(ToolCall::Name).text().not_null())
                    .col(ColumnDef::new(ToolCall::Arguments).json_binary().not_null())
                    .col(ColumnDef::new(ToolCall::Execution).text().not_null())
                    .col(ColumnDef::new(ToolCall::Status).text().not_null())
                    .col(ColumnDef::new(ToolCall::Result).text())
                    .col(ColumnDef::new(ToolCall::ErrorCode).text())
                    .col(ColumnDef::new(ToolCall::ErrorDetail).text())
                    .col(ColumnDef::new(ToolCall::ApprovalStatus).text())
                    .col(ColumnDef::new(ToolCall::ApprovalClass).text())
                    .col(ColumnDef::new(ToolCall::ApprovalKind).text())
                    .col(ColumnDef::new(ToolCall::ApprovalReason).text())
                    .col(ColumnDef::new(ToolCall::ApprovalRequestedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(ToolCall::ApprovalDecidedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(ToolCall::ApprovalEventSeq).big_integer())
                    .col(ColumnDef::new(ToolCall::ClientExecutorId).uuid())
                    .col(ColumnDef::new(ToolCall::ClientLeaseToken).uuid())
                    .col(ColumnDef::new(ToolCall::ClientLeaseExpiresAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(ToolCall::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ToolCall::ResolvedAt).timestamp_with_time_zone())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_tool_call_chat")
                            .from(ToolCall::Table, ToolCall::ChatId)
                            .to(Chat::Table, Chat::Id),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_tool_call_approval_event")
                            .from_tbl(ToolCall::Table)
                            .from_col(ToolCall::ChatId)
                            .from_col(ToolCall::ApprovalEventSeq)
                            .to_tbl(Event::Table)
                            .to_col(Event::ChatId)
                            .to_col(Event::Seq),
                    )
                    .check(Expr::col(ToolCall::Execution).is_in([
                        crate::model::ToolCallExecution::Server.as_str(),
                        crate::model::ToolCallExecution::Client.as_str(),
                        crate::model::ToolCallExecution::Orchestration.as_str(),
                    ]))
                    .check(Expr::col(ToolCall::Status).is_in([
                        crate::model::ToolCallStatus::Pending.as_str(),
                        crate::model::ToolCallStatus::Completed.as_str(),
                        crate::model::ToolCallStatus::Failed.as_str(),
                        crate::model::ToolCallStatus::Cancelled.as_str(),
                    ]))
                    .check(Expr::col(ToolCall::HistoryOrder).gt(0))
                    .check(
                        Func::char_length(Expr::col(ToolCall::ProviderId))
                            .between(1, crate::model::ToolCallRecord::MAX_LABEL_LEN as i32),
                    )
                    .check(
                        Func::char_length(Expr::col(ToolCall::Name))
                            .between(1, crate::model::ToolCallRecord::MAX_LABEL_LEN as i32),
                    )
                    .check(
                        Expr::col(ToolCall::Result).is_null().or(Func::char_length(
                            Expr::col(ToolCall::Result),
                        )
                        .lte(crate::model::ToolCallRecord::MAX_RESULT_BYTES as i32)),
                    )
                    .check(
                        Expr::col(ToolCall::ErrorCode).is_null().or(Func::char_length(
                            Expr::col(ToolCall::ErrorCode),
                        )
                        .between(
                            1,
                            crate::model::ToolCallRecord::MAX_ERROR_CODE_LEN as i32,
                        )),
                    )
                    .check(
                        Expr::col(ToolCall::ErrorDetail).is_null().or(Func::char_length(
                            Expr::col(ToolCall::ErrorDetail),
                        )
                        .between(
                            1,
                            crate::model::ToolCallRecord::MAX_ERROR_DETAIL_LEN as i32,
                        )),
                    )
                    .check(
                        Expr::col(ToolCall::ResolvedAt)
                            .is_null()
                            .or(Expr::col(ToolCall::ResolvedAt)
                                .gte(Expr::col(ToolCall::CreatedAt))),
                    )
                    .check(
                        Expr::col(ToolCall::ClientLeaseExpiresAt)
                            .is_null()
                            .or(Expr::col(ToolCall::ClientLeaseExpiresAt)
                                .gt(Expr::col(ToolCall::CreatedAt))),
                    )
                    .check(
                        Expr::col(ToolCall::Status)
                            .eq(crate::model::ToolCallStatus::Pending.as_str())
                            .and(Expr::col(ToolCall::Result).is_null())
                            .and(Expr::col(ToolCall::ErrorCode).is_null())
                            .and(Expr::col(ToolCall::ErrorDetail).is_null())
                            .and(Expr::col(ToolCall::ResolvedAt).is_null())
                            .or(Expr::col(ToolCall::Status)
                                .ne(crate::model::ToolCallStatus::Pending.as_str())
                                .and(Expr::col(ToolCall::Result).is_not_null())
                                .and(Expr::col(ToolCall::ResolvedAt).is_not_null())
                                .and(Expr::col(ToolCall::ClientLeaseExpiresAt).is_null())),
                    )
                    .check(
                        Expr::col(ToolCall::Status)
                            .eq(crate::model::ToolCallStatus::Failed.as_str())
                            .and(Expr::col(ToolCall::ErrorCode).is_not_null())
                            .or(Expr::col(ToolCall::Status)
                                .ne(crate::model::ToolCallStatus::Failed.as_str())
                                .and(Expr::col(ToolCall::ErrorCode).is_null())
                                .and(Expr::col(ToolCall::ErrorDetail).is_null())),
                    )
                    .check(
                        Expr::col(ToolCall::Execution)
                            .eq(crate::model::ToolCallExecution::Server.as_str())
                            .and(Expr::col(ToolCall::ClientExecutorId).is_null())
                            .and(Expr::col(ToolCall::ClientLeaseToken).is_null())
                            .and(Expr::col(ToolCall::ClientLeaseExpiresAt).is_null())
                            .or(Expr::col(ToolCall::Execution)
                                .eq(crate::model::ToolCallExecution::Client.as_str())
                                .and(
                                    Expr::col(ToolCall::Status)
                                        .eq(crate::model::ToolCallStatus::Pending.as_str())
                                        .and(
                                            Expr::col(ToolCall::ClientExecutorId)
                                                .is_null()
                                                .and(
                                                    Expr::col(ToolCall::ClientLeaseToken).is_null(),
                                                )
                                                .and(
                                                    Expr::col(ToolCall::ClientLeaseExpiresAt)
                                                        .is_null(),
                                                )
                                                .or(Expr::col(ToolCall::ClientExecutorId)
                                                    .is_not_null()
                                                    .and(Expr::col(ToolCall::ClientLeaseToken).is_not_null())
                                                    .and(Expr::col(ToolCall::ClientLeaseExpiresAt).is_not_null())),
                                        )
                                        .or(Expr::col(ToolCall::Status)
                                            .ne(crate::model::ToolCallStatus::Pending.as_str())
                                            .and(Expr::col(ToolCall::ClientExecutorId).is_not_null())
                                            .and(Expr::col(ToolCall::ClientLeaseToken).is_not_null())
                                            .and(Expr::col(ToolCall::ClientLeaseExpiresAt).is_null()))
                                        .or(Expr::col(ToolCall::Status)
                                            .eq(crate::model::ToolCallStatus::Cancelled.as_str())
                                            .and(Expr::col(ToolCall::ClientExecutorId).is_null())
                                            .and(Expr::col(ToolCall::ClientLeaseToken).is_null())
                                            .and(Expr::col(ToolCall::ClientLeaseExpiresAt).is_null())),
                                ))
                            .or(Expr::col(ToolCall::Execution)
                                .eq(crate::model::ToolCallExecution::Orchestration.as_str())
                                .and(Expr::col(ToolCall::Status).is_in([
                                    crate::model::ToolCallStatus::Pending.as_str(),
                                    crate::model::ToolCallStatus::Completed.as_str(),
                                    crate::model::ToolCallStatus::Cancelled.as_str(),
                                ]))
                                .and(Expr::col(ToolCall::ErrorCode).is_null())
                                .and(Expr::col(ToolCall::ErrorDetail).is_null())
                                .and(Expr::col(ToolCall::ClientExecutorId).is_null())
                                .and(Expr::col(ToolCall::ClientLeaseToken).is_null())
                                .and(Expr::col(ToolCall::ClientLeaseExpiresAt).is_null())),
                    )
                    .check(
                        Expr::col(ToolCall::ApprovalStatus)
                            .is_null()
                            .and(Expr::col(ToolCall::ApprovalClass).is_null())
                            .and(Expr::col(ToolCall::ApprovalKind).is_null())
                            .and(Expr::col(ToolCall::ApprovalReason).is_null())
                            .and(Expr::col(ToolCall::ApprovalRequestedAt).is_null())
                            .and(Expr::col(ToolCall::ApprovalDecidedAt).is_null())
                            .and(Expr::col(ToolCall::ApprovalEventSeq).is_null())
                            .or(Expr::col(ToolCall::Execution)
                                .eq(crate::model::ToolCallExecution::Server.as_str())
                                .and(Expr::col(ToolCall::ApprovalStatus).is_in([
                                    crate::ToolApprovalStatus::Pending.as_str(),
                                    crate::ToolApprovalStatus::Approved.as_str(),
                                    crate::ToolApprovalStatus::Rejected.as_str(),
                                ]))
                                .and(Expr::col(ToolCall::ApprovalClass)
                                    .eq(crate::ApprovalClass::Sensitive.as_str()))
                                .and(Expr::col(ToolCall::ApprovalKind).is_in([
                                    crate::ToolApprovalKind::SearchMayShareQueryAndExcerpts.as_str(),
                                    crate::ToolApprovalKind::ExecMayRunNetworkedCommand.as_str(),
                                    crate::ToolApprovalKind::Unsupported.as_str(),
                                ]))
                                .and(Expr::col(ToolCall::ApprovalRequestedAt).is_not_null())
                                .and(
                                    Expr::col(ToolCall::ApprovalStatus)
                                        .eq(crate::ToolApprovalStatus::Pending.as_str())
                                        .and(Expr::col(ToolCall::Status)
                                            .eq(crate::model::ToolCallStatus::Pending.as_str()))
                                        .and(Expr::col(ToolCall::ApprovalReason).is_null())
                                        .and(Expr::col(ToolCall::ApprovalDecidedAt).is_null())
                                        .or(Expr::col(ToolCall::ApprovalStatus)
                                            .eq(crate::ToolApprovalStatus::Approved.as_str())
                                            .and(Expr::col(ToolCall::ApprovalReason).is_null())
                                            .and(Expr::col(ToolCall::ApprovalDecidedAt).is_not_null()))
                                        .or(Expr::col(ToolCall::ApprovalStatus)
                                            .eq(crate::ToolApprovalStatus::Rejected.as_str())
                                            .and(Expr::col(ToolCall::ApprovalReason).is_not_null())
                                            .and(Expr::col(ToolCall::ApprovalDecidedAt).is_not_null())),
                                )),
                    )
                    .check(
                        Expr::col(ToolCall::ApprovalReason).is_null().or(Func::char_length(
                            Expr::col(ToolCall::ApprovalReason),
                        )
                        .between(1, crate::ToolApproval::MAX_REASON_CHARS as i32)),
                    )
                    .check(
                        Expr::col(ToolCall::ApprovalDecidedAt)
                            .is_null()
                            .or(Expr::col(ToolCall::ApprovalDecidedAt)
                                .gte(Expr::col(ToolCall::ApprovalRequestedAt))),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_tool_call_chat_history")
                    .table(ToolCall::Table)
                    .col(ToolCall::ChatId)
                    .col(ToolCall::HistoryOrder)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_tool_call_wait_identity")
                    .table(ToolCall::Table)
                    .col(ToolCall::Id)
                    .col(ToolCall::ChatId)
                    .col(ToolCall::TurnId)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_tool_call_checkpoint_identity")
                    .table(ToolCall::Table)
                    .col(ToolCall::Id)
                    .col(ToolCall::ChatId)
                    .col(ToolCall::HistoryOrder)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_tool_call_client_pending")
                    .table(ToolCall::Table)
                    .col(ToolCall::ChatId)
                    .col(ToolCall::Execution)
                    .col(ToolCall::Status)
                    .col(ToolCall::ClientLeaseExpiresAt)
                    .to_owned(),
            )
            .await?;

        create_turn_agent_run_wait_set_tables(manager).await?;

        manager
            .create_table(
                Table::create()
                    .table(SandboxSpawnCheckpoint::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::CallId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::ChildRunId)
                            .uuid()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::ParentRunId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::OriginTurnId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::ChatId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::LeaseToken)
                            .uuid()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::AttemptCount)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::ClaimCount)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::ProviderId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::HistoryOrder)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::Arguments)
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::Result)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::SteerRevision)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::EventOrdinal)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::ModelSteps)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::InputTokens)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::OutputTokens)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::CacheReadInputTokens)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::CacheCreationInputTokens)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::EventSeq)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SandboxSpawnCheckpoint::CommittedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_sandbox_spawn_checkpoint_admission")
                            .from_tbl(SandboxSpawnCheckpoint::Table)
                            .from_col(SandboxSpawnCheckpoint::ChildRunId)
                            .from_col(SandboxSpawnCheckpoint::OriginTurnId)
                            .from_col(SandboxSpawnCheckpoint::ParentRunId)
                            .from_col(SandboxSpawnCheckpoint::ChatId)
                            .to_tbl(SandboxAgentAdmission::Table)
                            .to_col(SandboxAgentAdmission::ChildRunId)
                            .to_col(SandboxAgentAdmission::OriginTurnId)
                            .to_col(SandboxAgentAdmission::ParentRunId)
                            .to_col(SandboxAgentAdmission::ChatId)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_sandbox_spawn_checkpoint_claim")
                            .from_tbl(SandboxSpawnCheckpoint::Table)
                            .from_col(SandboxSpawnCheckpoint::LeaseToken)
                            .from_col(SandboxSpawnCheckpoint::OriginTurnId)
                            .from_col(SandboxSpawnCheckpoint::AttemptCount)
                            .from_col(SandboxSpawnCheckpoint::ClaimCount)
                            .to_tbl(TurnClaim::Table)
                            .to_col(TurnClaim::Token)
                            .to_col(TurnClaim::TurnId)
                            .to_col(TurnClaim::AttemptCount)
                            .to_col(TurnClaim::ClaimCount)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_sandbox_spawn_checkpoint_tool")
                            .from_tbl(SandboxSpawnCheckpoint::Table)
                            .from_col(SandboxSpawnCheckpoint::CallId)
                            .from_col(SandboxSpawnCheckpoint::ChatId)
                            .from_col(SandboxSpawnCheckpoint::HistoryOrder)
                            .to_tbl(ToolCall::Table)
                            .to_col(ToolCall::Id)
                            .to_col(ToolCall::ChatId)
                            .to_col(ToolCall::HistoryOrder)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_sandbox_spawn_checkpoint_event")
                            .from_tbl(SandboxSpawnCheckpoint::Table)
                            .from_col(SandboxSpawnCheckpoint::ChatId)
                            .from_col(SandboxSpawnCheckpoint::EventSeq)
                            .to_tbl(Event::Table)
                            .to_col(Event::ChatId)
                            .to_col(Event::Seq)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .check(Expr::col(SandboxSpawnCheckpoint::AttemptCount).gte(1))
                    .check(
                        Expr::col(SandboxSpawnCheckpoint::ClaimCount)
                            .gte(Expr::col(SandboxSpawnCheckpoint::AttemptCount)),
                    )
                    .check(Expr::col(SandboxSpawnCheckpoint::SteerRevision).gte(0))
                    .check(Expr::col(SandboxSpawnCheckpoint::EventOrdinal).between(2, i32::MAX - 1))
                    .check(Expr::col(SandboxSpawnCheckpoint::ModelSteps).gt(0))
                    .check(Expr::col(SandboxSpawnCheckpoint::HistoryOrder).gt(0))
                    .check(
                        Expr::col(SandboxSpawnCheckpoint::InputTokens)
                            .between(0, i64::from(u32::MAX)),
                    )
                    .check(
                        Expr::col(SandboxSpawnCheckpoint::OutputTokens)
                            .between(0, i64::from(u32::MAX)),
                    )
                    .check(
                        Expr::col(SandboxSpawnCheckpoint::CacheReadInputTokens)
                            .between(0, i64::from(u32::MAX)),
                    )
                    .check(
                        Expr::col(SandboxSpawnCheckpoint::CacheCreationInputTokens)
                            .between(0, i64::from(u32::MAX)),
                    )
                    .check(
                        Func::char_length(Expr::col(SandboxSpawnCheckpoint::ProviderId))
                            .between(1, crate::model::ToolCallRecord::MAX_LABEL_LEN as i32),
                    )
                    .check(
                        Func::char_length(Expr::col(SandboxSpawnCheckpoint::Result))
                            .lte(crate::model::ToolCallRecord::MAX_RESULT_BYTES as i32),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_sandbox_spawn_checkpoint_event")
                    .table(SandboxSpawnCheckpoint::Table)
                    .col(SandboxSpawnCheckpoint::ChatId)
                    .col(SandboxSpawnCheckpoint::EventSeq)
                    .unique()
                    .to_owned(),
            )
            .await?;

        create_retrieval_evidence_table(manager).await?;
        create_assistant_citation_table(manager).await?;

        manager
            .create_table(
                Table::create()
                    .table(TurnClientWait::Table)
                    .col(
                        ColumnDef::new(TurnClientWait::CallId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(TurnClientWait::TurnId).uuid().not_null())
                    .col(ColumnDef::new(TurnClientWait::ChatId).uuid().not_null())
                    .col(
                        ColumnDef::new(TurnClientWait::ParkLeaseToken)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnClientWait::AttemptCount)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnClientWait::ClaimCount)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnClientWait::ModelSteps)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnClientWait::InputTokens)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnClientWait::OutputTokens)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnClientWait::CacheReadInputTokens)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnClientWait::CacheCreationInputTokens)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TurnClientWait::Status).text().not_null())
                    .col(
                        ColumnDef::new(TurnClientWait::ParkedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TurnClientWait::ClosedAt).timestamp_with_time_zone())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_client_wait_call")
                            .from_tbl(TurnClientWait::Table)
                            .from_col(TurnClientWait::CallId)
                            .from_col(TurnClientWait::ChatId)
                            .from_col(TurnClientWait::TurnId)
                            .to_tbl(ToolCall::Table)
                            .to_col(ToolCall::Id)
                            .to_col(ToolCall::ChatId)
                            .to_col(ToolCall::TurnId)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_client_wait_claim")
                            .from_tbl(TurnClientWait::Table)
                            .from_col(TurnClientWait::ParkLeaseToken)
                            .from_col(TurnClientWait::TurnId)
                            .from_col(TurnClientWait::AttemptCount)
                            .from_col(TurnClientWait::ClaimCount)
                            .to_tbl(TurnClaim::Table)
                            .to_col(TurnClaim::Token)
                            .to_col(TurnClaim::TurnId)
                            .to_col(TurnClaim::AttemptCount)
                            .to_col(TurnClaim::ClaimCount)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .check(Expr::col(TurnClientWait::Status).is_in([
                        TurnClientWaitStatus::Waiting.as_str(),
                        TurnClientWaitStatus::Resumed.as_str(),
                        TurnClientWaitStatus::Cancelled.as_str(),
                    ]))
                    .check(
                        Expr::col(TurnClientWait::Status)
                            .eq(TurnClientWaitStatus::Waiting.as_str())
                            .and(Expr::col(TurnClientWait::ClosedAt).is_null())
                            .or(Expr::col(TurnClientWait::Status)
                                .ne(TurnClientWaitStatus::Waiting.as_str())
                                .and(Expr::col(TurnClientWait::ClosedAt).is_not_null())),
                    )
                    .check(
                        Expr::col(TurnClientWait::ClosedAt)
                            .is_null()
                            .or(Expr::col(TurnClientWait::ClosedAt)
                                .gte(Expr::col(TurnClientWait::ParkedAt))),
                    )
                    .check(
                        Expr::col(TurnClientWait::ModelSteps)
                            .gt(0)
                            .and(Expr::col(TurnClientWait::ModelSteps).lte(i64::from(i32::MAX)))
                            .and(Expr::col(TurnClientWait::InputTokens).gte(0))
                            .and(Expr::col(TurnClientWait::InputTokens).lte(i64::from(u32::MAX)))
                            .and(Expr::col(TurnClientWait::OutputTokens).gte(0))
                            .and(Expr::col(TurnClientWait::OutputTokens).lte(i64::from(u32::MAX)))
                            .and(Expr::col(TurnClientWait::CacheReadInputTokens).gte(0))
                            .and(
                                Expr::col(TurnClientWait::CacheReadInputTokens)
                                    .lte(i64::from(u32::MAX)),
                            )
                            .and(Expr::col(TurnClientWait::CacheCreationInputTokens).gte(0))
                            .and(
                                Expr::col(TurnClientWait::CacheCreationInputTokens)
                                    .lte(i64::from(u32::MAX)),
                            ),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_client_wait_one_open")
                    .table(TurnClientWait::Table)
                    .col(TurnClientWait::TurnId)
                    .unique()
                    .and_where(
                        Expr::col(TurnClientWait::Status)
                            .eq(TurnClientWaitStatus::Waiting.as_str()),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_client_wait_history")
                    .table(TurnClientWait::Table)
                    .col(TurnClientWait::TurnId)
                    .col(TurnClientWait::ParkedAt)
                    .col(TurnClientWait::CallId)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(SandboxSpawnCheckpoint::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(AssistantCitation::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(RetrievalEvidence::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(TurnClientWait::Table).to_owned())
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(TurnAgentRunWaitMember::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(TurnAgentRunWaitSet::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(TurnAgentRunWaitLock::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(ToolCall::Table).to_owned())
            .await?;
        Ok(())
    }
}

/// Adds authoritative documents and their durable processing jobs. The
/// retrieval database remains derived state; lifecycle, retry, and lease
/// ownership live in the operational database.
struct AddDocuments;

impl MigrationName for AddDocuments {
    fn name(&self) -> &str {
        "m0006_document_catalog"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddDocuments {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let valid_index_revision =
            Expr::col(Document::IndexedRevision)
                .is_null()
                .or(Expr::col(Document::IndexedRevision).gte(1).and(
                    Expr::col(Document::IndexedRevision).lte(Expr::col(Document::ContentRevision)),
                ));
        let watermark_absent = Expr::col(Document::IndexedRevision)
            .is_null()
            .and(Expr::col(Document::IndexFingerprint).is_null())
            .and(Expr::col(Document::IndexedAt).is_null());
        let watermark_present = Expr::col(Document::IndexedRevision)
            .is_not_null()
            .and(
                Expr::col(Document::IndexFingerprint).is_not_null().and(
                    Func::char_length(Expr::col(Document::IndexFingerprint))
                        .between(1, LEGACY_DOCUMENT_PIPELINE_FINGERPRINT_LEN as i32),
                ),
            )
            .and(Expr::col(Document::IndexedAt).is_not_null());
        let processing_watermark_consistent = Expr::col(Document::ProcessingStatus)
            .eq(DocumentProcessingStatus::Ready.as_str())
            .and(Expr::col(Document::IndexedRevision).eq(Expr::col(Document::ContentRevision)))
            .and(watermark_present)
            .or(Expr::col(Document::ProcessingStatus)
                .ne(DocumentProcessingStatus::Ready.as_str())
                .and(watermark_absent));

        manager
            .create_table(
                Table::create()
                    .table(DocumentGeneration::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DocumentGeneration::DocumentId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(DocumentGeneration::ContentRevision)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DocumentGeneration::RevisionToken)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DocumentGeneration::Tombstone)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(DocumentGeneration::RetirementPending)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(DocumentGeneration::RetirementContentRevision).big_integer(),
                    )
                    .col(ColumnDef::new(DocumentGeneration::RetirementRevisionToken).uuid())
                    .check(Expr::col(DocumentGeneration::ContentRevision).gte(1))
                    .check(
                        Expr::col(DocumentGeneration::RetirementPending)
                            .eq(false)
                            .and(Expr::col(DocumentGeneration::RetirementContentRevision).is_null())
                            .and(Expr::col(DocumentGeneration::RetirementRevisionToken).is_null())
                            .or(Expr::col(DocumentGeneration::RetirementPending)
                                .eq(true)
                                .and(
                                    Expr::col(DocumentGeneration::RetirementContentRevision)
                                        .is_not_null(),
                                )
                                .and(
                                    Expr::col(DocumentGeneration::RetirementContentRevision).gte(1),
                                )
                                .and(
                                    Expr::col(DocumentGeneration::RetirementRevisionToken)
                                        .is_not_null(),
                                )),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Document::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Document::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Document::ProjectId).uuid())
                    .col(ColumnDef::new(Document::SourceUri).text())
                    .col(ColumnDef::new(Document::MediaType).text().not_null())
                    .col(ColumnDef::new(Document::Title).text())
                    .col(ColumnDef::new(Document::SourceBlobId).uuid())
                    .col(ColumnDef::new(Document::SourceSha256).binary())
                    .col(ColumnDef::new(Document::SourceByteLen).big_integer())
                    .col(ColumnDef::new(Document::CanonicalText).text().not_null())
                    .col(ColumnDef::new(Document::CanonicalFingerprint).text())
                    .col(
                        ColumnDef::new(Document::SourceRegions)
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Document::ContentRevision)
                            .big_integer()
                            .not_null()
                            .default(1),
                    )
                    .col(ColumnDef::new(Document::RevisionToken).uuid().not_null())
                    .col(
                        ColumnDef::new(Document::ProcessingStatus)
                            .text()
                            .not_null()
                            .default(DocumentProcessingStatus::Queued.as_str()),
                    )
                    .col(ColumnDef::new(Document::IndexedRevision).big_integer())
                    .col(ColumnDef::new(Document::IndexFingerprint).text())
                    .col(
                        ColumnDef::new(Document::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Document::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Document::IndexedAt).timestamp_with_time_zone())
                    .check(
                        Expr::col(Document::SourceBlobId)
                            .is_null()
                            .and(Expr::col(Document::SourceSha256).is_null())
                            .and(Expr::col(Document::SourceByteLen).is_null())
                            .or(Expr::col(Document::SourceBlobId)
                                .is_not_null()
                                .and(Expr::col(Document::SourceSha256).is_not_null())
                                .and(Expr::col(Document::SourceByteLen).is_not_null())
                                .and(Expr::cust("LENGTH(source_sha256) = 32"))
                                .and(Expr::col(Document::SourceByteLen).gte(0))),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_document_project")
                            .from(Document::Table, Document::ProjectId)
                            .to(Project::Table, Project::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .check(Expr::col(Document::MediaType).ne(""))
                    .check(
                        Expr::col(Document::SourceUri)
                            .is_null()
                            .or(Expr::col(Document::SourceUri).ne("")),
                    )
                    .check(
                        Expr::col(Document::CanonicalFingerprint)
                            .is_null()
                            .or(Func::char_length(Expr::col(Document::CanonicalFingerprint))
                                .between(1, LEGACY_DOCUMENT_PIPELINE_FINGERPRINT_LEN as i32)),
                    )
                    .check(Expr::col(Document::ContentRevision).gte(1))
                    .check(Expr::col(Document::ProcessingStatus).is_in([
                        DocumentProcessingStatus::Queued.as_str(),
                        DocumentProcessingStatus::Processing.as_str(),
                        DocumentProcessingStatus::Ready.as_str(),
                        DocumentProcessingStatus::Failed.as_str(),
                    ]))
                    .check(valid_index_revision)
                    .check(processing_watermark_consistent)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_document_project_created")
                    .table(Document::Table)
                    .col(Document::ProjectId)
                    .col(Document::CreatedAt)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_document_source_blob")
                    .table(Document::Table)
                    .col(Document::SourceBlobId)
                    .to_owned(),
            )
            .await?;

        let valid_blob_status = Expr::col(BlobRetirement::Status).is_in([
            BlobRetirementStatus::Queued.as_str(),
            BlobRetirementStatus::Running.as_str(),
            BlobRetirementStatus::RetryWait.as_str(),
            BlobRetirementStatus::Succeeded.as_str(),
            BlobRetirementStatus::Failed.as_str(),
            BlobRetirementStatus::Cancelled.as_str(),
        ]);
        let blob_running_lease = Expr::col(BlobRetirement::Status)
            .eq(BlobRetirementStatus::Running.as_str())
            .and(Expr::col(BlobRetirement::LeaseToken).is_not_null())
            .and(Expr::col(BlobRetirement::LeaseExpiresAt).is_not_null());
        let blob_no_lease = Expr::col(BlobRetirement::Status)
            .ne(BlobRetirementStatus::Running.as_str())
            .and(Expr::col(BlobRetirement::LeaseToken).is_null())
            .and(Expr::col(BlobRetirement::LeaseExpiresAt).is_null());
        let blob_terminal_finished = Expr::col(BlobRetirement::Status)
            .is_in([
                BlobRetirementStatus::Succeeded.as_str(),
                BlobRetirementStatus::Failed.as_str(),
                BlobRetirementStatus::Cancelled.as_str(),
            ])
            .and(Expr::col(BlobRetirement::FinishedAt).is_not_null());
        let blob_nonterminal_unfinished = Expr::col(BlobRetirement::Status)
            .is_in([
                BlobRetirementStatus::Queued.as_str(),
                BlobRetirementStatus::Running.as_str(),
                BlobRetirementStatus::RetryWait.as_str(),
            ])
            .and(Expr::col(BlobRetirement::FinishedAt).is_null());
        let blob_queued_attempt = Expr::col(BlobRetirement::Status)
            .eq(BlobRetirementStatus::Queued.as_str())
            .and(Expr::col(BlobRetirement::AttemptCount).eq(0))
            .and(Expr::col(BlobRetirement::StartedAt).is_null());
        let blob_running_attempt = Expr::col(BlobRetirement::Status)
            .eq(BlobRetirementStatus::Running.as_str())
            .and(Expr::col(BlobRetirement::AttemptCount).gte(1))
            .and(Expr::col(BlobRetirement::StartedAt).is_not_null());
        let blob_retryable_attempt = Expr::col(BlobRetirement::Status)
            .eq(BlobRetirementStatus::RetryWait.as_str())
            .and(Expr::col(BlobRetirement::AttemptCount).gte(1))
            .and(Expr::col(BlobRetirement::AttemptCount).lt(Expr::col(BlobRetirement::MaxAttempts)))
            .and(Expr::col(BlobRetirement::StartedAt).is_not_null());
        let blob_completed_attempt = Expr::col(BlobRetirement::Status)
            .is_in([
                BlobRetirementStatus::Succeeded.as_str(),
                BlobRetirementStatus::Failed.as_str(),
            ])
            .and(Expr::col(BlobRetirement::AttemptCount).gte(1))
            .and(Expr::col(BlobRetirement::StartedAt).is_not_null());
        let blob_cancelled_attempt = Expr::col(BlobRetirement::Status)
            .eq(BlobRetirementStatus::Cancelled.as_str())
            .and(
                Expr::col(BlobRetirement::AttemptCount)
                    .eq(0)
                    .and(Expr::col(BlobRetirement::StartedAt).is_null())
                    .or(Expr::col(BlobRetirement::AttemptCount)
                        .gte(1)
                        .and(Expr::col(BlobRetirement::StartedAt).is_not_null())),
            );
        manager
            .create_table(
                Table::create()
                    .table(BlobRetirement::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(BlobRetirement::BlobId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(BlobRetirement::Status)
                            .string_len(32)
                            .not_null()
                            .default(BlobRetirementStatus::Queued.as_str()),
                    )
                    .col(
                        ColumnDef::new(BlobRetirement::AttemptCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(BlobRetirement::MaxAttempts)
                            .integer()
                            .not_null()
                            .default(crate::model::BlobRetirement::DEFAULT_MAX_ATTEMPTS),
                    )
                    .col(
                        ColumnDef::new(BlobRetirement::AvailableAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(BlobRetirement::LeaseToken).uuid())
                    .col(ColumnDef::new(BlobRetirement::LeaseExpiresAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(BlobRetirement::StartedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(BlobRetirement::FinishedAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(BlobRetirement::LastErrorCode)
                            .string_len(crate::model::BlobRetirement::MAX_ERROR_CODE_LEN as u32),
                    )
                    .col(
                        ColumnDef::new(BlobRetirement::LastErrorDetail)
                            .string_len(crate::model::BlobRetirement::MAX_ERROR_DETAIL_LEN as u32),
                    )
                    .col(
                        ColumnDef::new(BlobRetirement::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(BlobRetirement::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .check(valid_blob_status)
                    .check(
                        Expr::col(BlobRetirement::AttemptCount)
                            .gte(0)
                            .and(Expr::col(BlobRetirement::MaxAttempts).gte(1))
                            .and(
                                Expr::col(BlobRetirement::AttemptCount)
                                    .lte(Expr::col(BlobRetirement::MaxAttempts)),
                            ),
                    )
                    .check(blob_running_lease.or(blob_no_lease))
                    .check(blob_terminal_finished.or(blob_nonterminal_unfinished))
                    .check(
                        blob_queued_attempt
                            .or(blob_running_attempt)
                            .or(blob_retryable_attempt)
                            .or(blob_completed_attempt)
                            .or(blob_cancelled_attempt),
                    )
                    .check(
                        Expr::col(BlobRetirement::LastErrorCode)
                            .is_null()
                            .or(Func::char_length(Expr::col(BlobRetirement::LastErrorCode))
                                .between(
                                    1,
                                    crate::model::BlobRetirement::MAX_ERROR_CODE_LEN as i32,
                                )),
                    )
                    .check(
                        Expr::col(BlobRetirement::LastErrorDetail)
                            .is_null()
                            .or(
                                Func::char_length(Expr::col(BlobRetirement::LastErrorDetail))
                                    .between(
                                        1,
                                        crate::model::BlobRetirement::MAX_ERROR_DETAIL_LEN as i32,
                                    ),
                            ),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_blob_retirement_due")
                    .table(BlobRetirement::Table)
                    .col(BlobRetirement::Status)
                    .col(BlobRetirement::AvailableAt)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_blob_retirement_stale_lease")
                    .table(BlobRetirement::Table)
                    .col(BlobRetirement::Status)
                    .col(BlobRetirement::LeaseExpiresAt)
                    .to_owned(),
            )
            .await?;

        let valid_job_status = Expr::col(DocumentJob::Status).is_in([
            DocumentJobStatus::Queued.as_str(),
            DocumentJobStatus::Running.as_str(),
            DocumentJobStatus::RetryWait.as_str(),
            DocumentJobStatus::Succeeded.as_str(),
            DocumentJobStatus::Failed.as_str(),
            DocumentJobStatus::Cancelled.as_str(),
        ]);
        let running_lease = Expr::col(DocumentJob::Status)
            .eq(DocumentJobStatus::Running.as_str())
            .and(Expr::col(DocumentJob::LeaseToken).is_not_null())
            .and(Expr::col(DocumentJob::LeaseExpiresAt).is_not_null());
        let no_lease = Expr::col(DocumentJob::Status)
            .ne(DocumentJobStatus::Running.as_str())
            .and(Expr::col(DocumentJob::LeaseToken).is_null())
            .and(Expr::col(DocumentJob::LeaseExpiresAt).is_null());
        let terminal_finished = Expr::col(DocumentJob::Status)
            .is_in([
                DocumentJobStatus::Succeeded.as_str(),
                DocumentJobStatus::Failed.as_str(),
                DocumentJobStatus::Cancelled.as_str(),
            ])
            .and(Expr::col(DocumentJob::FinishedAt).is_not_null());
        let nonterminal_unfinished = Expr::col(DocumentJob::Status)
            .is_in([
                DocumentJobStatus::Queued.as_str(),
                DocumentJobStatus::Running.as_str(),
                DocumentJobStatus::RetryWait.as_str(),
            ])
            .and(Expr::col(DocumentJob::FinishedAt).is_null());
        let queued_attempt = Expr::col(DocumentJob::Status)
            .eq(DocumentJobStatus::Queued.as_str())
            .and(Expr::col(DocumentJob::AttemptCount).eq(0))
            .and(Expr::col(DocumentJob::StartedAt).is_null());
        let running_attempt = Expr::col(DocumentJob::Status)
            .eq(DocumentJobStatus::Running.as_str())
            .and(Expr::col(DocumentJob::AttemptCount).gte(1))
            .and(Expr::col(DocumentJob::StartedAt).is_not_null());
        let retryable_attempt = Expr::col(DocumentJob::Status)
            .eq(DocumentJobStatus::RetryWait.as_str())
            .and(Expr::col(DocumentJob::AttemptCount).gte(1))
            .and(Expr::col(DocumentJob::AttemptCount).lt(Expr::col(DocumentJob::MaxAttempts)))
            .and(Expr::col(DocumentJob::StartedAt).is_not_null());
        let completed_attempt = Expr::col(DocumentJob::Status)
            .is_in([
                DocumentJobStatus::Succeeded.as_str(),
                DocumentJobStatus::Failed.as_str(),
            ])
            .and(Expr::col(DocumentJob::AttemptCount).gte(1))
            .and(Expr::col(DocumentJob::StartedAt).is_not_null());
        let cancelled_attempt = Expr::col(DocumentJob::Status)
            .eq(DocumentJobStatus::Cancelled.as_str())
            .and(
                Expr::col(DocumentJob::AttemptCount)
                    .eq(0)
                    .and(Expr::col(DocumentJob::StartedAt).is_null())
                    .or(Expr::col(DocumentJob::AttemptCount)
                        .gte(1)
                        .and(Expr::col(DocumentJob::StartedAt).is_not_null())),
            );

        manager
            .create_table(
                Table::create()
                    .table(DocumentJob::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DocumentJob::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(DocumentJob::DocumentId).uuid().not_null())
                    .col(
                        ColumnDef::new(DocumentJob::ContentRevision)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(DocumentJob::RevisionToken).uuid().not_null())
                    .col(ColumnDef::new(DocumentJob::Kind).string_len(64).not_null())
                    .col(
                        ColumnDef::new(DocumentJob::Status)
                            .string_len(32)
                            .not_null()
                            .default(DocumentJobStatus::Queued.as_str()),
                    )
                    .col(
                        ColumnDef::new(DocumentJob::PipelineFingerprint)
                            .string_len(512)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DocumentJob::AttemptCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(DocumentJob::MaxAttempts)
                            .integer()
                            .not_null()
                            .default(5),
                    )
                    .col(
                        ColumnDef::new(DocumentJob::AvailableAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(DocumentJob::LeaseToken).uuid())
                    .col(ColumnDef::new(DocumentJob::LeaseExpiresAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(DocumentJob::StartedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(DocumentJob::FinishedAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(DocumentJob::LastErrorCode)
                            .string_len(LEGACY_DOCUMENT_JOB_ERROR_CODE_LEN as u32),
                    )
                    .col(
                        ColumnDef::new(DocumentJob::LastErrorDetail)
                            .string_len(LEGACY_DOCUMENT_JOB_ERROR_DETAIL_LEN as u32),
                    )
                    .col(
                        ColumnDef::new(DocumentJob::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DocumentJob::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_document_job_document")
                            .from(DocumentJob::Table, DocumentJob::DocumentId)
                            .to(Document::Table, Document::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .check(Expr::col(DocumentJob::ContentRevision).gte(1))
                    .check(Expr::col(DocumentJob::Kind).is_in(["parse", "index"]))
                    .check(
                        Func::char_length(Expr::col(DocumentJob::Kind))
                            .lte(64)
                            .and(
                                Func::char_length(Expr::col(DocumentJob::PipelineFingerprint))
                                    .between(1, LEGACY_DOCUMENT_PIPELINE_FINGERPRINT_LEN as i32),
                            )
                            .and(
                                Expr::col(DocumentJob::LastErrorCode)
                                    .is_null()
                                    .or(Func::char_length(Expr::col(DocumentJob::LastErrorCode))
                                        .between(1, LEGACY_DOCUMENT_JOB_ERROR_CODE_LEN as i32)),
                            )
                            .and(
                                Expr::col(DocumentJob::LastErrorDetail)
                                    .is_null()
                                    .or(Func::char_length(Expr::col(DocumentJob::LastErrorDetail))
                                        .between(1, LEGACY_DOCUMENT_JOB_ERROR_DETAIL_LEN as i32)),
                            ),
                    )
                    .check(valid_job_status)
                    .check(
                        Expr::col(DocumentJob::AttemptCount)
                            .gte(0)
                            .and(Expr::col(DocumentJob::MaxAttempts).gte(1))
                            .and(
                                Expr::col(DocumentJob::AttemptCount)
                                    .lte(Expr::col(DocumentJob::MaxAttempts)),
                            ),
                    )
                    .check(running_lease.or(no_lease))
                    .check(terminal_finished.or(nonterminal_unfinished))
                    .check(
                        queued_attempt
                            .or(running_attempt)
                            .or(retryable_attempt)
                            .or(completed_attempt)
                            .or(cancelled_attempt),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_document_job_idempotency")
                    .table(DocumentJob::Table)
                    .col(DocumentJob::DocumentId)
                    .col(DocumentJob::RevisionToken)
                    .col(DocumentJob::Kind)
                    .col(DocumentJob::PipelineFingerprint)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_document_job_one_active")
                    .table(DocumentJob::Table)
                    .col(DocumentJob::DocumentId)
                    .unique()
                    .and_where(Expr::col(DocumentJob::Status).is_in([
                        DocumentJobStatus::Queued.as_str(),
                        DocumentJobStatus::Running.as_str(),
                        DocumentJobStatus::RetryWait.as_str(),
                    ]))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_document_job_due")
                    .table(DocumentJob::Table)
                    .col(DocumentJob::Status)
                    .col(DocumentJob::AvailableAt)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_document_job_stale_lease")
                    .table(DocumentJob::Table)
                    .col(DocumentJob::Status)
                    .col(DocumentJob::LeaseExpiresAt)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_document_job_history")
                    .table(DocumentJob::Table)
                    .col(DocumentJob::DocumentId)
                    .col(DocumentJob::CreatedAt)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(DocumentJob::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(BlobRetirement::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Document::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(DocumentGeneration::Table).to_owned())
            .await?;
        Ok(())
    }
}

struct AddSandboxToolCalls;

impl MigrationName for AddSandboxToolCalls {
    fn name(&self) -> &str {
        "m20260715_000007_add_sandbox_tool_calls"
    }
}

struct AddAgentRunResultPayload;

impl MigrationName for AddAgentRunResultPayload {
    fn name(&self) -> &str {
        "m20260716_000008_add_agent_run_result_payload"
    }
}

struct AddClaimedTurnEffectLeases;

impl MigrationName for AddClaimedTurnEffectLeases {
    fn name(&self) -> &str {
        "m20260722_000009_add_claimed_turn_effect_leases"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddClaimedTurnEffectLeases {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Message::Table)
                    .add_column(ColumnDef::new(Message::TurnLeaseToken).uuid())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ToolCall::Table)
                    .add_column(ColumnDef::new(ToolCall::TurnLeaseToken).uuid())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ToolCall::Table)
                    .add_column(ColumnDef::new(ToolCall::ResolutionTurnLeaseToken).uuid())
                    .to_owned(),
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE turn_run SET max_attempts = 3 \
                 WHERE max_attempts = 1 AND status IN \
                 ('queued', 'running', 'retry_wait', 'resuming', 'waiting_for_client', \
                  'waiting_for_agent_run', 'cancelling', 'cancelling_client')",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ToolCall::Table)
                    .drop_column(ToolCall::ResolutionTurnLeaseToken)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ToolCall::Table)
                    .drop_column(ToolCall::TurnLeaseToken)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Message::Table)
                    .drop_column(Message::TurnLeaseToken)
                    .to_owned(),
            )
            .await
    }
}

/// Widens a standing grant from one chat to the level it was granted at
/// (issue #937): `chat_id` becomes nullable and `project_id` joins it, with
/// exactly one of the two set. A grant made in a chat that belongs to a
/// project now covers that project, so "always allow" survives into the next
/// conversation instead of being re-asked.
///
/// Existing rows are chat grants and stay chat grants — widening someone's
/// past decision without asking is the one thing this must not do.
///
/// SQLite cannot drop a NOT NULL or add a CHECK in place, so it rebuilds the
/// table and copies every row across; PostgreSQL alters in place. The `down`
/// deletes project-level rows first, because they have no representation in
/// the narrower shape and silently rewriting them to a chat would invent a
/// scope nobody chose.
struct WidenStandingGrantScope;

impl MigrationName for WidenStandingGrantScope {
    fn name(&self) -> &str {
        "m20260729_000028_widen_standing_grant_scope"
    }
}

const STANDING_GRANT_REBUILD: &str = "standing_tool_grant_rebuild";

/// Exactly one of `chat_id` / `project_id` names the level.
fn standing_grant_level_check(widened: bool) -> SimpleExpr {
    if widened {
        Expr::col(StandingToolGrant::ChatId)
            .is_not_null()
            .and(Expr::col(StandingToolGrant::ProjectId).is_null())
            .or(Expr::col(StandingToolGrant::ChatId)
                .is_null()
                .and(Expr::col(StandingToolGrant::ProjectId).is_not_null()))
    } else {
        Expr::col(StandingToolGrant::ChatId).is_not_null()
    }
}

fn standing_grant_rebuild_table(widened: bool) -> TableCreateStatement {
    let mut table = Table::create();
    table
        .table(Alias::new(STANDING_GRANT_REBUILD))
        .col(
            ColumnDef::new(StandingToolGrant::SourceCallId)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col({
            let mut column = ColumnDef::new(StandingToolGrant::ChatId);
            column.uuid();
            if !widened {
                column.not_null();
            }
            column
        })
        .col(
            ColumnDef::new(StandingToolGrant::ToolName)
                .text()
                .not_null(),
        )
        .col(
            ColumnDef::new(StandingToolGrant::ApprovalKind)
                .string_len(64)
                .not_null(),
        )
        .col(
            ColumnDef::new(StandingToolGrant::Scope)
                .json_binary()
                .not_null(),
        )
        .col(
            ColumnDef::new(StandingToolGrant::GrantedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_standing_tool_grant_chat")
                .from(
                    Alias::new(STANDING_GRANT_REBUILD),
                    StandingToolGrant::ChatId,
                )
                .to(Chat::Table, Chat::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .check(
            Func::char_length(Expr::col(StandingToolGrant::ToolName))
                .between(1, crate::model::ToolCallRecord::MAX_LABEL_LEN as i32),
        )
        .check(Expr::col(StandingToolGrant::ApprovalKind).is_in([
            crate::ToolApprovalKind::SearchMayShareQueryAndExcerpts.standing_grant_key(),
            crate::ToolApprovalKind::WebSearchMayShareQuery.standing_grant_key(),
            crate::ToolApprovalKind::ExecMayRunNetworkedCommand.standing_grant_key(),
        ]));
    if widened {
        table
            .col(ColumnDef::new(StandingToolGrant::ProjectId).uuid())
            .foreign_key(
                ForeignKey::create()
                    .name("fk_standing_tool_grant_project")
                    .from(
                        Alias::new(STANDING_GRANT_REBUILD),
                        StandingToolGrant::ProjectId,
                    )
                    .to(Project::Table, Project::Id)
                    .on_delete(ForeignKeyAction::Cascade),
            );
    }
    table.check(standing_grant_level_check(widened));
    table.to_owned()
}

fn standing_grant_rebuild_index() -> IndexCreateStatement {
    Index::create()
        .name("idx_standing_tool_grant_lookup")
        .table(StandingToolGrant::Table)
        .col(StandingToolGrant::ChatId)
        .col(StandingToolGrant::ToolName)
        .col(StandingToolGrant::ApprovalKind)
        .col(StandingToolGrant::GrantedAt)
        .to_owned()
}

async fn rebuild_standing_grant_sqlite(
    manager: &SchemaManager<'_>,
    widened: bool,
) -> Result<(), DbErr> {
    let shared = "source_call_id, chat_id, tool_name, approval_kind, scope, granted_at";
    let copy = if widened {
        format!(
            "INSERT INTO {STANDING_GRANT_REBUILD} ({shared}, project_id) \
             SELECT {shared}, NULL FROM standing_tool_grant"
        )
    } else {
        // Project grants cannot be narrowed to a chat without inventing one.
        format!(
            "INSERT INTO {STANDING_GRANT_REBUILD} ({shared}) \
             SELECT {shared} FROM standing_tool_grant WHERE chat_id IS NOT NULL"
        )
    };
    let statements = vec![
        "PRAGMA foreign_keys=OFF".to_owned(),
        "BEGIN IMMEDIATE".to_owned(),
        // A rebuild that failed partway would otherwise leave the scratch
        // table behind and make the next attempt fail on "already exists".
        // Cheaper to make the step idempotent than to test for the stranding.
        format!("DROP TABLE IF EXISTS {STANDING_GRANT_REBUILD}"),
        standing_grant_rebuild_table(widened).to_string(SqliteQueryBuilder),
        copy,
        "DROP TABLE standing_tool_grant".to_owned(),
        format!("ALTER TABLE {STANDING_GRANT_REBUILD} RENAME TO standing_tool_grant"),
        standing_grant_rebuild_index().to_string(SqliteQueryBuilder),
        "COMMIT".to_owned(),
        "PRAGMA foreign_keys=ON".to_owned(),
    ];
    manager
        .get_connection()
        .execute_unprepared(&format!("{};", statements.join(";\n")))
        .await?;
    Ok(())
}

#[async_trait::async_trait]
impl MigrationTrait for WidenStandingGrantScope {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() == DatabaseBackend::Sqlite {
            return rebuild_standing_grant_sqlite(manager, true).await;
        }
        let level = render_postgres_check(standing_grant_level_check(true));
        let statements = [
            "ALTER TABLE standing_tool_grant ADD COLUMN project_id uuid".to_owned(),
            "ALTER TABLE standing_tool_grant ADD CONSTRAINT fk_standing_tool_grant_project \
             FOREIGN KEY (project_id) REFERENCES project(id) ON DELETE CASCADE"
                .to_owned(),
            "ALTER TABLE standing_tool_grant ALTER COLUMN chat_id DROP NOT NULL".to_owned(),
            format!(
                "ALTER TABLE standing_tool_grant ADD CONSTRAINT chk_standing_tool_grant_level \
                 CHECK ({level})"
            ),
        ];
        manager
            .get_connection()
            .execute_unprepared(&format!("{};", statements.join(";\n")))
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() == DatabaseBackend::Sqlite {
            return rebuild_standing_grant_sqlite(manager, false).await;
        }
        let statements = [
            "DELETE FROM standing_tool_grant WHERE project_id IS NOT NULL".to_owned(),
            "ALTER TABLE standing_tool_grant DROP CONSTRAINT chk_standing_tool_grant_level"
                .to_owned(),
            "ALTER TABLE standing_tool_grant ALTER COLUMN chat_id SET NOT NULL".to_owned(),
            "ALTER TABLE standing_tool_grant DROP CONSTRAINT fk_standing_tool_grant_project"
                .to_owned(),
            "ALTER TABLE standing_tool_grant DROP COLUMN project_id".to_owned(),
        ];
        manager
            .get_connection()
            .execute_unprepared(&format!("{};", statements.join(";\n")))
            .await?;
        Ok(())
    }
}

/// Retires the semantic index stage: a parsed document is ready the moment its
/// canonical text is published, because there is no derived vector store left
/// to publish into.
///
/// Index jobs are deleted rather than cancelled — they describe a stage that no
/// longer exists. Documents whose canonical output was already published (or
/// that never had raw bytes to parse) are promoted to `ready`, including rows
/// whose only failure was an index attempt. The index watermark columns and
/// their consistency checks leave the document table, and the retirement
/// watermark leaves the generation clock — retirement existed only to
/// unpublish vector generations.
struct RetireDocumentIndexing;

impl MigrationName for RetireDocumentIndexing {
    fn name(&self) -> &str {
        "m20260729_000029_retire_document_indexing"
    }
}

const DOCUMENT_REBUILD: &str = "document_rebuild";
const DOCUMENT_GENERATION_REBUILD: &str = "document_generation_rebuild";

/// Rows whose text of record is already published: canonical-only documents,
/// and raw sources whose parse completed.
const DOCUMENT_TEXT_PUBLISHED: &str = "source_blob_id IS NULL OR canonical_fingerprint IS NOT NULL";

fn document_index_revision_check() -> SimpleExpr {
    Expr::col(Document::IndexedRevision)
        .is_null()
        .or(Expr::col(Document::IndexedRevision)
            .gte(1)
            .and(Expr::col(Document::IndexedRevision).lte(Expr::col(Document::ContentRevision))))
}

fn document_processing_watermark_check() -> SimpleExpr {
    let watermark_absent = Expr::col(Document::IndexedRevision)
        .is_null()
        .and(Expr::col(Document::IndexFingerprint).is_null())
        .and(Expr::col(Document::IndexedAt).is_null());
    let watermark_present = Expr::col(Document::IndexedRevision)
        .is_not_null()
        .and(
            Expr::col(Document::IndexFingerprint).is_not_null().and(
                Func::char_length(Expr::col(Document::IndexFingerprint))
                    .between(1, LEGACY_DOCUMENT_PIPELINE_FINGERPRINT_LEN as i32),
            ),
        )
        .and(Expr::col(Document::IndexedAt).is_not_null());
    Expr::col(Document::ProcessingStatus)
        .eq(DocumentProcessingStatus::Ready.as_str())
        .and(Expr::col(Document::IndexedRevision).eq(Expr::col(Document::ContentRevision)))
        .and(watermark_present)
        .or(Expr::col(Document::ProcessingStatus)
            .ne(DocumentProcessingStatus::Ready.as_str())
            .and(watermark_absent))
}

fn document_generation_retirement_check() -> SimpleExpr {
    Expr::col(DocumentGeneration::RetirementPending)
        .eq(false)
        .and(Expr::col(DocumentGeneration::RetirementContentRevision).is_null())
        .and(Expr::col(DocumentGeneration::RetirementRevisionToken).is_null())
        .or(Expr::col(DocumentGeneration::RetirementPending)
            .eq(true)
            .and(Expr::col(DocumentGeneration::RetirementContentRevision).is_not_null())
            .and(Expr::col(DocumentGeneration::RetirementContentRevision).gte(1))
            .and(Expr::col(DocumentGeneration::RetirementRevisionToken).is_not_null()))
}

fn document_rebuild_table(indexed: bool) -> TableCreateStatement {
    let rebuild = Alias::new(DOCUMENT_REBUILD);
    let mut table = Table::create();
    table
        .table(rebuild.clone())
        .col(ColumnDef::new(Document::Id).uuid().not_null().primary_key())
        .col(ColumnDef::new(Document::ChatId).uuid())
        .col(ColumnDef::new(Document::ProjectId).uuid())
        .col(ColumnDef::new(Document::SourceUri).text())
        .col(ColumnDef::new(Document::MediaType).text().not_null())
        .col(ColumnDef::new(Document::Title).text())
        .col(ColumnDef::new(Document::SourceBlobId).uuid())
        .col(ColumnDef::new(Document::SourceSha256).binary())
        .col(ColumnDef::new(Document::SourceByteLen).big_integer())
        .col(ColumnDef::new(Document::CanonicalText).text().not_null())
        .col(ColumnDef::new(Document::CanonicalFingerprint).text())
        .col(
            ColumnDef::new(Document::SourceRegions)
                .json_binary()
                .not_null(),
        )
        .col(
            ColumnDef::new(Document::ContentRevision)
                .big_integer()
                .not_null()
                .default(1),
        )
        .col(ColumnDef::new(Document::RevisionToken).uuid().not_null())
        .col(
            ColumnDef::new(Document::ProcessingStatus)
                .text()
                .not_null()
                .default(DocumentProcessingStatus::Queued.as_str()),
        )
        .col(
            ColumnDef::new(Document::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(
            ColumnDef::new(Document::UpdatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .check(
            Expr::col(Document::SourceBlobId)
                .is_null()
                .and(Expr::col(Document::SourceSha256).is_null())
                .and(Expr::col(Document::SourceByteLen).is_null())
                .or(Expr::col(Document::SourceBlobId)
                    .is_not_null()
                    .and(Expr::col(Document::SourceSha256).is_not_null())
                    .and(Expr::col(Document::SourceByteLen).is_not_null())
                    .and(Expr::cust("LENGTH(source_sha256) = 32"))
                    .and(Expr::col(Document::SourceByteLen).gte(0))),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_document_project")
                .from(rebuild, Document::ProjectId)
                .to(Project::Table, Project::Id)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .check(Expr::col(Document::MediaType).ne(""))
        .check(
            Expr::col(Document::SourceUri)
                .is_null()
                .or(Expr::col(Document::SourceUri).ne("")),
        )
        .check(
            Expr::col(Document::CanonicalFingerprint)
                .is_null()
                .or(Func::char_length(Expr::col(Document::CanonicalFingerprint))
                    .between(1, LEGACY_DOCUMENT_PIPELINE_FINGERPRINT_LEN as i32)),
        )
        .check(Expr::col(Document::ContentRevision).gte(1))
        .check(Expr::col(Document::ProcessingStatus).is_in([
            DocumentProcessingStatus::Queued.as_str(),
            DocumentProcessingStatus::Processing.as_str(),
            DocumentProcessingStatus::Ready.as_str(),
            DocumentProcessingStatus::Failed.as_str(),
        ]));
    if indexed {
        table
            .col(ColumnDef::new(Document::IndexedRevision).big_integer())
            .col(ColumnDef::new(Document::IndexFingerprint).text())
            .col(ColumnDef::new(Document::IndexedAt).timestamp_with_time_zone())
            .check(document_index_revision_check())
            .check(document_processing_watermark_check());
    }
    table.to_owned()
}

fn document_generation_rebuild_table(retirement: bool) -> TableCreateStatement {
    let mut table = Table::create();
    table
        .table(Alias::new(DOCUMENT_GENERATION_REBUILD))
        .col(
            ColumnDef::new(DocumentGeneration::DocumentId)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(
            ColumnDef::new(DocumentGeneration::ContentRevision)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(DocumentGeneration::RevisionToken)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(DocumentGeneration::Tombstone)
                .boolean()
                .not_null()
                .default(false),
        )
        .check(Expr::col(DocumentGeneration::ContentRevision).gte(1));
    if retirement {
        table
            .col(
                ColumnDef::new(DocumentGeneration::RetirementPending)
                    .boolean()
                    .not_null()
                    .default(false),
            )
            .col(ColumnDef::new(DocumentGeneration::RetirementContentRevision).big_integer())
            .col(ColumnDef::new(DocumentGeneration::RetirementRevisionToken).uuid())
            .check(document_generation_retirement_check());
    }
    table.to_owned()
}

fn document_rebuild_indexes() -> [IndexCreateStatement; 3] {
    [
        Index::create()
            .name("idx_document_project_created")
            .table(Document::Table)
            .col(Document::ProjectId)
            .col(Document::CreatedAt)
            .to_owned(),
        Index::create()
            .name("idx_document_source_blob")
            .table(Document::Table)
            .col(Document::SourceBlobId)
            .to_owned(),
        Index::create()
            .name("idx_document_chat_created")
            .table(Document::Table)
            .col(Document::ChatId)
            .col(Document::CreatedAt)
            .to_owned(),
    ]
}

async fn rebuild_document_indexing_sqlite(
    manager: &SchemaManager<'_>,
    indexed: bool,
) -> Result<(), DbErr> {
    let shared = "id, chat_id, project_id, source_uri, media_type, title, source_blob_id, \
         source_sha256, source_byte_len, canonical_text, canonical_fingerprint, source_regions, \
         content_revision, revision_token, created_at, updated_at";
    let copy_document = if indexed {
        // Reintroducing the watermark leaves it empty, and an empty watermark
        // cannot substantiate `ready`: those rows go back to awaiting an index.
        format!(
            "INSERT INTO {DOCUMENT_REBUILD} ({shared}, processing_status, indexed_revision, \
              index_fingerprint, indexed_at) \
             SELECT {shared}, CASE processing_status WHEN 'ready' THEN 'queued' \
              ELSE processing_status END, NULL, NULL, NULL \
             FROM document"
        )
    } else {
        format!(
            "INSERT INTO {DOCUMENT_REBUILD} ({shared}, processing_status) \
             SELECT {shared}, CASE WHEN {DOCUMENT_TEXT_PUBLISHED} THEN 'ready' \
              ELSE processing_status END \
             FROM document"
        )
    };
    let generation_shared = "document_id, content_revision, revision_token, tombstone";
    let copy_generation = if indexed {
        format!(
            "INSERT INTO {DOCUMENT_GENERATION_REBUILD} ({generation_shared}, \
              retirement_pending, retirement_content_revision, retirement_revision_token) \
             SELECT {generation_shared}, FALSE, NULL, NULL FROM document_generation"
        )
    } else {
        format!(
            "INSERT INTO {DOCUMENT_GENERATION_REBUILD} ({generation_shared}) \
             SELECT {generation_shared} FROM document_generation"
        )
    };
    let mut statements = vec![
        "PRAGMA foreign_keys=OFF".to_owned(),
        "BEGIN IMMEDIATE".to_owned(),
        format!("DROP TABLE IF EXISTS {DOCUMENT_REBUILD}"),
        format!("DROP TABLE IF EXISTS {DOCUMENT_GENERATION_REBUILD}"),
        document_rebuild_table(indexed).to_string(SqliteQueryBuilder),
        copy_document,
        "DROP TABLE document".to_owned(),
        format!("ALTER TABLE {DOCUMENT_REBUILD} RENAME TO document"),
        document_generation_rebuild_table(indexed).to_string(SqliteQueryBuilder),
        copy_generation,
        "DROP TABLE document_generation".to_owned(),
        format!("ALTER TABLE {DOCUMENT_GENERATION_REBUILD} RENAME TO document_generation"),
    ];
    statements.extend(
        document_rebuild_indexes()
            .iter()
            .map(|index| index.to_string(SqliteQueryBuilder)),
    );
    statements.push("COMMIT".to_owned());
    statements.push("PRAGMA foreign_keys=ON".to_owned());
    manager
        .get_connection()
        .execute_unprepared(&format!("{};", statements.join(";\n")))
        .await?;
    Ok(())
}

#[async_trait::async_trait]
impl MigrationTrait for RetireDocumentIndexing {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DELETE FROM document_job WHERE kind = 'index'")
            .await?;
        if manager.get_database_backend() == DatabaseBackend::Sqlite {
            return rebuild_document_indexing_sqlite(manager, false).await;
        }
        let statements = [
            "ALTER TABLE document DROP COLUMN indexed_revision CASCADE, \
             DROP COLUMN index_fingerprint CASCADE, DROP COLUMN indexed_at CASCADE"
                .to_owned(),
            format!(
                "UPDATE document SET processing_status = 'ready' \
                 WHERE processing_status <> 'ready' AND ({DOCUMENT_TEXT_PUBLISHED})"
            ),
            "ALTER TABLE document_generation DROP COLUMN retirement_pending CASCADE, \
             DROP COLUMN retirement_content_revision CASCADE, \
             DROP COLUMN retirement_revision_token CASCADE"
                .to_owned(),
        ];
        manager
            .get_connection()
            .execute_unprepared(&format!("{};", statements.join(";\n")))
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() == DatabaseBackend::Sqlite {
            return rebuild_document_indexing_sqlite(manager, true).await;
        }
        let index_revision = render_postgres_check(document_index_revision_check());
        let watermark = render_postgres_check(document_processing_watermark_check());
        let retirement = render_postgres_check(document_generation_retirement_check());
        let statements = [
            // An empty watermark cannot substantiate `ready`.
            "UPDATE document SET processing_status = 'queued' WHERE processing_status = 'ready'"
                .to_owned(),
            "ALTER TABLE document ADD COLUMN indexed_revision bigint, \
             ADD COLUMN index_fingerprint text, ADD COLUMN indexed_at timestamptz"
                .to_owned(),
            format!("ALTER TABLE document ADD CONSTRAINT chk_document_index_revision CHECK ({index_revision})"),
            format!(
                "ALTER TABLE document ADD CONSTRAINT chk_document_processing_watermark CHECK ({watermark})"
            ),
            "ALTER TABLE document_generation \
             ADD COLUMN retirement_pending boolean NOT NULL DEFAULT FALSE, \
             ADD COLUMN retirement_content_revision bigint, \
             ADD COLUMN retirement_revision_token uuid"
                .to_owned(),
            format!(
                "ALTER TABLE document_generation ADD CONSTRAINT \
                 chk_document_generation_retirement CHECK ({retirement})"
            ),
        ];
        manager
            .get_connection()
            .execute_unprepared(&format!("{};", statements.join(";\n")))
            .await?;
        Ok(())
    }
}

/// Adds `tool_call.auto_judge_status` (issue #756): where the Auto-mode judge
/// stands on a parked approval (`judging` / `approved` / `declined`, `NULL`
/// when no judge was engaged). Values are code-enforced; the column carries no
/// CHECK so the vocabulary can grow without a table rebuild.
struct AddToolCallAutoJudgeStatus;

impl MigrationName for AddToolCallAutoJudgeStatus {
    fn name(&self) -> &str {
        "m20260729_000024_add_tool_call_auto_judge_status"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddToolCallAutoJudgeStatus {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ToolCall::Table)
                    .add_column(ColumnDef::new(ToolCall::AutoJudgeStatus).text())
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ToolCall::Table)
                    .drop_column(ToolCall::AutoJudgeStatus)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

/// Adds the optional per-chat `permission_mode` (issue #756): how much the
/// chat lets the agent do between approvals. `NULL` reads as `ask`.
struct AddChatPermissionMode;

impl MigrationName for AddChatPermissionMode {
    fn name(&self) -> &str {
        "m20260729_000023_add_chat_permission_mode"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddChatPermissionMode {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Chat::Table)
                    .add_column(ColumnDef::new(Chat::PermissionMode).text())
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Chat::Table)
                    .drop_column(Chat::PermissionMode)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

/// Adds the optional per-chat `citation_format` override. An absent value
/// follows the global default, so existing chats keep the behavior they had.
struct AddChatCitationFormat;

impl MigrationName for AddChatCitationFormat {
    fn name(&self) -> &str {
        "m20260729_000025_add_chat_citation_format"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddChatCitationFormat {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Chat::Table)
                    .add_column(ColumnDef::new(Chat::CitationFormat).text())
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Chat::Table)
                    .drop_column(Chat::CitationFormat)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

/// Adds the optional per-chat `reasoning_effort` override.
struct AddChatReasoningEffort;

impl MigrationName for AddChatReasoningEffort {
    fn name(&self) -> &str {
        "m20260722_000010_add_chat_reasoning_effort"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddChatReasoningEffort {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Chat::Table)
                    .add_column(ColumnDef::new(Chat::ReasoningEffort).text())
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Chat::Table)
                    .drop_column(Chat::ReasoningEffort)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddAgentRunResultPayload {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AgentRunResult::Table)
                    .add_column(
                        ColumnDef::new(AgentRunResult::PayloadKind)
                            .string_len(32)
                            .not_null()
                            .default("final_text"),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(AgentRunResult::Table)
                    .add_column(
                        ColumnDef::new(AgentRunResult::PayloadJson)
                            .text()
                            .not_null()
                            .default("{\\\"text\\\":\\\"\\\"}"),
                    )
                    .to_owned(),
            )
            .await?;
        let existing_payloads = match manager.get_database_backend() {
            DatabaseBackend::Postgres => {
                "UPDATE agent_run_result SET payload_json = json_build_object('text', text)::text"
            }
            // `DatabaseBackend` is `#[non_exhaustive]` in sea-orm 2.0; SQLite and
            // MySQL both use `json_object`, which is also the sensible default
            // for any other backend.
            _ => "UPDATE agent_run_result SET payload_json = json_object('text', text)",
        };
        manager
            .get_connection()
            .execute_unprepared(existing_payloads)
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AgentRunResult::Table)
                    .drop_column(AgentRunResult::PayloadJson)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(AgentRunResult::Table)
                    .drop_column(AgentRunResult::PayloadKind)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddSandboxToolCalls {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        create_sandbox_tool_call_tables(manager).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(SandboxToolCallReceipt::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(SandboxToolCall::Table).to_owned())
            .await?;
        Ok(())
    }
}

/// Replaces retrieval-span citations with direct document locators.
struct LightweightCitations;

impl MigrationName for LightweightCitations {
    fn name(&self) -> &str {
        "m20260729_000030_lightweight_citations"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for LightweightCitations {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(AssistantCitationLight::Table)
                    .col(
                        ColumnDef::new(AssistantCitationLight::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AssistantCitationLight::MessageId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AssistantCitationLight::Ordinal)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AssistantCitationLight::DocumentId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AssistantCitationLight::Locator)
                            .json_binary()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_assistant_citation_light_message")
                            .from(
                                AssistantCitationLight::Table,
                                AssistantCitationLight::MessageId,
                            )
                            .to(Message::Table, Message::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_assistant_citation_light_document")
                            .from(
                                AssistantCitationLight::Table,
                                AssistantCitationLight::DocumentId,
                            )
                            .to(Document::Table, Document::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .index(
                        Index::create()
                            .name("idx_assistant_citation_light_message_ordinal")
                            .col(AssistantCitationLight::MessageId)
                            .col(AssistantCitationLight::Ordinal)
                            .unique(),
                    )
                    .to_owned(),
            )
            .await?;

        let copy = match manager.get_database_backend() {
            DatabaseBackend::Sqlite => r#"
                INSERT INTO assistant_citation_light
                    (id, message_id, ordinal, document_id, locator)
                SELECT ac.id, ac.message_id, ac.ordinal, re.document_id,
                    CASE
                        WHEN json_array_length(re.source_regions) > 0
                         AND json_extract(re.source_regions, '$[0].location.kind') = 'page'
                         AND json_extract(
                             re.source_regions,
                             '$[' || (json_array_length(re.source_regions) - 1) || '].location.kind'
                         ) = 'page'
                        THEN CASE
                            WHEN json_extract(re.source_regions, '$[0].location.number')
                               = json_extract(
                                   re.source_regions,
                                   '$[' || (json_array_length(re.source_regions) - 1)
                                   || '].location.number'
                               )
                            THEN json_object(
                                'kind', 'page',
                                'page', json_extract(
                                    re.source_regions, '$[0].location.number'
                                )
                            )
                            ELSE json_object(
                                'kind', 'pages',
                                'start', json_extract(
                                    re.source_regions, '$[0].location.number'
                                ),
                                'end', json_extract(
                                    re.source_regions,
                                    '$[' || (json_array_length(re.source_regions) - 1)
                                    || '].location.number'
                                )
                            )
                        END
                        ELSE json_object('kind', 'document')
                    END
                FROM assistant_citation ac
                JOIN retrieval_evidence re
                  ON re.call_id = ac.evidence_call_id
                 AND re.rank = ac.evidence_rank
                 AND re.chat_id = ac.chat_id
                 AND re.turn_id = ac.turn_id
                "#
            .trim(),
            DatabaseBackend::Postgres => r#"
                INSERT INTO assistant_citation_light
                    (id, message_id, ordinal, document_id, locator)
                SELECT ac.id, ac.message_id, ac.ordinal, re.document_id,
                    CASE
                        WHEN jsonb_array_length(re.source_regions) > 0
                         AND re.source_regions -> 0 -> 'location' ->> 'kind' = 'page'
                         AND re.source_regions
                             -> (jsonb_array_length(re.source_regions) - 1)
                             -> 'location' ->> 'kind' = 'page'
                        THEN CASE
                            WHEN re.source_regions -> 0 -> 'location' ->> 'number'
                               = re.source_regions
                                   -> (jsonb_array_length(re.source_regions) - 1)
                                   -> 'location' ->> 'number'
                            THEN jsonb_build_object(
                                'kind', 'page',
                                'page', (
                                    re.source_regions -> 0 -> 'location' ->> 'number'
                                )::integer
                            )
                            ELSE jsonb_build_object(
                                'kind', 'pages',
                                'start', (
                                    re.source_regions -> 0 -> 'location' ->> 'number'
                                )::integer,
                                'end', (
                                    re.source_regions
                                        -> (jsonb_array_length(re.source_regions) - 1)
                                        -> 'location' ->> 'number'
                                )::integer
                            )
                        END
                        ELSE jsonb_build_object('kind', 'document')
                    END
                FROM assistant_citation ac
                JOIN retrieval_evidence re
                  ON re.call_id = ac.evidence_call_id
                 AND re.rank = ac.evidence_rank
                 AND re.chat_id = ac.chat_id
                 AND re.turn_id = ac.turn_id
                "#
            .trim(),
            backend => {
                return Err(DbErr::Custom(format!(
                    "lightweight citation migration does not support {backend:?}"
                )));
            }
        };
        manager.get_connection().execute_unprepared(copy).await?;

        manager
            .drop_table(
                Table::drop()
                    .table(OutputRevisionCitation::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(AssistantCitation::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(RetrievalEvidence::Table).to_owned())
            .await?;
        manager
            .rename_table(
                Table::rename()
                    .table(AssistantCitationLight::Table, AssistantCitation::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Chat::Table)
                    .drop_column(Chat::CitationFormat)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AssistantCitation::Table).to_owned())
            .await?;
        create_retrieval_evidence_table(manager).await?;
        AddEvidenceLocation.up(manager).await?;
        create_assistant_citation_table(manager).await?;
        AddOutputRevisionCitations.up(manager).await?;
        AddChatCitationFormat.up(manager).await
    }
}

/// Removes the durable work queue and revision clock now that document
/// decoding completes in the accepting request.
struct RetireDocumentPipeline;

impl MigrationName for RetireDocumentPipeline {
    fn name(&self) -> &str {
        "m20260729_000031_retire_document_pipeline"
    }
}

const DOCUMENT_PIPELINE_REBUILD: &str = "document_pipeline_rebuild";

fn document_pipeline_table() -> TableCreateStatement {
    let rebuild = Alias::new(DOCUMENT_PIPELINE_REBUILD);
    Table::create()
        .table(rebuild.clone())
        .col(ColumnDef::new(Document::Id).uuid().not_null().primary_key())
        .col(ColumnDef::new(Document::ChatId).uuid())
        .col(ColumnDef::new(Document::ProjectId).uuid())
        .col(ColumnDef::new(Document::SourceUri).text())
        .col(ColumnDef::new(Document::MediaType).text().not_null())
        .col(ColumnDef::new(Document::Title).text())
        .col(ColumnDef::new(Document::SourceBlobId).uuid())
        .col(ColumnDef::new(Document::SourceSha256).binary())
        .col(ColumnDef::new(Document::SourceByteLen).big_integer())
        .col(ColumnDef::new(Document::CanonicalText).text().not_null())
        .col(
            ColumnDef::new(Document::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(
            ColumnDef::new(Document::UpdatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_document_project")
                .from(rebuild, Document::ProjectId)
                .to(Project::Table, Project::Id)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .check(
            Expr::col(Document::SourceBlobId)
                .is_null()
                .and(Expr::col(Document::SourceSha256).is_null())
                .and(Expr::col(Document::SourceByteLen).is_null())
                .or(Expr::col(Document::SourceBlobId)
                    .is_not_null()
                    .and(Expr::col(Document::SourceSha256).is_not_null())
                    .and(Expr::col(Document::SourceByteLen).is_not_null())
                    .and(Expr::cust("LENGTH(source_sha256) = 32"))
                    .and(Expr::col(Document::SourceByteLen).gte(0))),
        )
        .check(Expr::col(Document::MediaType).ne(""))
        .check(
            Expr::col(Document::SourceUri)
                .is_null()
                .or(Expr::col(Document::SourceUri).ne("")),
        )
        .to_owned()
}

fn legacy_document_generation_table() -> TableCreateStatement {
    Table::create()
        .table(DocumentGeneration::Table)
        .col(
            ColumnDef::new(DocumentGeneration::DocumentId)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(
            ColumnDef::new(DocumentGeneration::ContentRevision)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(DocumentGeneration::RevisionToken)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(DocumentGeneration::Tombstone)
                .boolean()
                .not_null()
                .default(false),
        )
        .check(Expr::col(DocumentGeneration::ContentRevision).gte(1))
        .to_owned()
}

fn legacy_document_job_table() -> TableCreateStatement {
    let valid_status = Expr::col(DocumentJob::Status).is_in([
        DocumentJobStatus::Queued.as_str(),
        DocumentJobStatus::Running.as_str(),
        DocumentJobStatus::RetryWait.as_str(),
        DocumentJobStatus::Succeeded.as_str(),
        DocumentJobStatus::Failed.as_str(),
        DocumentJobStatus::Cancelled.as_str(),
    ]);
    let running_lease = Expr::col(DocumentJob::Status)
        .eq(DocumentJobStatus::Running.as_str())
        .and(Expr::col(DocumentJob::LeaseToken).is_not_null())
        .and(Expr::col(DocumentJob::LeaseExpiresAt).is_not_null());
    let no_lease = Expr::col(DocumentJob::Status)
        .ne(DocumentJobStatus::Running.as_str())
        .and(Expr::col(DocumentJob::LeaseToken).is_null())
        .and(Expr::col(DocumentJob::LeaseExpiresAt).is_null());
    let terminal_finished = Expr::col(DocumentJob::Status)
        .is_in([
            DocumentJobStatus::Succeeded.as_str(),
            DocumentJobStatus::Failed.as_str(),
            DocumentJobStatus::Cancelled.as_str(),
        ])
        .and(Expr::col(DocumentJob::FinishedAt).is_not_null());
    let nonterminal_unfinished = Expr::col(DocumentJob::Status)
        .is_in([
            DocumentJobStatus::Queued.as_str(),
            DocumentJobStatus::Running.as_str(),
            DocumentJobStatus::RetryWait.as_str(),
        ])
        .and(Expr::col(DocumentJob::FinishedAt).is_null());
    let queued_attempt = Expr::col(DocumentJob::Status)
        .eq(DocumentJobStatus::Queued.as_str())
        .and(Expr::col(DocumentJob::AttemptCount).eq(0))
        .and(Expr::col(DocumentJob::StartedAt).is_null());
    let running_attempt = Expr::col(DocumentJob::Status)
        .eq(DocumentJobStatus::Running.as_str())
        .and(Expr::col(DocumentJob::AttemptCount).gte(1))
        .and(Expr::col(DocumentJob::StartedAt).is_not_null());
    let retryable_attempt = Expr::col(DocumentJob::Status)
        .eq(DocumentJobStatus::RetryWait.as_str())
        .and(Expr::col(DocumentJob::AttemptCount).gte(1))
        .and(Expr::col(DocumentJob::AttemptCount).lt(Expr::col(DocumentJob::MaxAttempts)))
        .and(Expr::col(DocumentJob::StartedAt).is_not_null());
    let completed_attempt = Expr::col(DocumentJob::Status)
        .is_in([
            DocumentJobStatus::Succeeded.as_str(),
            DocumentJobStatus::Failed.as_str(),
        ])
        .and(Expr::col(DocumentJob::AttemptCount).gte(1))
        .and(Expr::col(DocumentJob::StartedAt).is_not_null());
    let cancelled_attempt = Expr::col(DocumentJob::Status)
        .eq(DocumentJobStatus::Cancelled.as_str())
        .and(
            Expr::col(DocumentJob::AttemptCount)
                .eq(0)
                .and(Expr::col(DocumentJob::StartedAt).is_null())
                .or(Expr::col(DocumentJob::AttemptCount)
                    .gte(1)
                    .and(Expr::col(DocumentJob::StartedAt).is_not_null())),
        );

    Table::create()
        .table(DocumentJob::Table)
        .col(
            ColumnDef::new(DocumentJob::Id)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(ColumnDef::new(DocumentJob::DocumentId).uuid().not_null())
        .col(
            ColumnDef::new(DocumentJob::ContentRevision)
                .big_integer()
                .not_null(),
        )
        .col(ColumnDef::new(DocumentJob::RevisionToken).uuid().not_null())
        .col(ColumnDef::new(DocumentJob::Kind).string_len(64).not_null())
        .col(
            ColumnDef::new(DocumentJob::Status)
                .string_len(32)
                .not_null()
                .default(DocumentJobStatus::Queued.as_str()),
        )
        .col(
            ColumnDef::new(DocumentJob::PipelineFingerprint)
                .string_len(LEGACY_DOCUMENT_PIPELINE_FINGERPRINT_LEN as u32)
                .not_null(),
        )
        .col(
            ColumnDef::new(DocumentJob::AttemptCount)
                .integer()
                .not_null()
                .default(0),
        )
        .col(
            ColumnDef::new(DocumentJob::MaxAttempts)
                .integer()
                .not_null()
                .default(5),
        )
        .col(
            ColumnDef::new(DocumentJob::AvailableAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(ColumnDef::new(DocumentJob::LeaseToken).uuid())
        .col(ColumnDef::new(DocumentJob::LeaseExpiresAt).timestamp_with_time_zone())
        .col(ColumnDef::new(DocumentJob::StartedAt).timestamp_with_time_zone())
        .col(ColumnDef::new(DocumentJob::FinishedAt).timestamp_with_time_zone())
        .col(
            ColumnDef::new(DocumentJob::LastErrorCode)
                .string_len(LEGACY_DOCUMENT_JOB_ERROR_CODE_LEN as u32),
        )
        .col(
            ColumnDef::new(DocumentJob::LastErrorDetail)
                .string_len(LEGACY_DOCUMENT_JOB_ERROR_DETAIL_LEN as u32),
        )
        .col(
            ColumnDef::new(DocumentJob::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(
            ColumnDef::new(DocumentJob::UpdatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_document_job_document")
                .from(DocumentJob::Table, DocumentJob::DocumentId)
                .to(Document::Table, Document::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .check(Expr::col(DocumentJob::ContentRevision).gte(1))
        .check(Expr::col(DocumentJob::Kind).is_in(["parse"]))
        .check(
            Func::char_length(Expr::col(DocumentJob::Kind))
                .lte(64)
                .and(
                    Func::char_length(Expr::col(DocumentJob::PipelineFingerprint))
                        .between(1, LEGACY_DOCUMENT_PIPELINE_FINGERPRINT_LEN as i32),
                )
                .and(
                    Expr::col(DocumentJob::LastErrorCode)
                        .is_null()
                        .or(Func::char_length(Expr::col(DocumentJob::LastErrorCode))
                            .between(1, LEGACY_DOCUMENT_JOB_ERROR_CODE_LEN as i32)),
                )
                .and(
                    Expr::col(DocumentJob::LastErrorDetail)
                        .is_null()
                        .or(Func::char_length(Expr::col(DocumentJob::LastErrorDetail))
                            .between(1, LEGACY_DOCUMENT_JOB_ERROR_DETAIL_LEN as i32)),
                ),
        )
        .check(valid_status)
        .check(
            Expr::col(DocumentJob::AttemptCount)
                .gte(0)
                .and(Expr::col(DocumentJob::MaxAttempts).gte(1))
                .and(Expr::col(DocumentJob::AttemptCount).lte(Expr::col(DocumentJob::MaxAttempts))),
        )
        .check(running_lease.or(no_lease))
        .check(terminal_finished.or(nonterminal_unfinished))
        .check(
            queued_attempt
                .or(running_attempt)
                .or(retryable_attempt)
                .or(completed_attempt)
                .or(cancelled_attempt),
        )
        .to_owned()
}

fn legacy_document_job_indexes() -> [IndexCreateStatement; 5] {
    [
        Index::create()
            .name("idx_document_job_idempotency")
            .table(DocumentJob::Table)
            .col(DocumentJob::DocumentId)
            .col(DocumentJob::RevisionToken)
            .col(DocumentJob::Kind)
            .col(DocumentJob::PipelineFingerprint)
            .unique()
            .to_owned(),
        Index::create()
            .name("idx_document_job_one_active")
            .table(DocumentJob::Table)
            .col(DocumentJob::DocumentId)
            .unique()
            .and_where(Expr::col(DocumentJob::Status).is_in([
                DocumentJobStatus::Queued.as_str(),
                DocumentJobStatus::Running.as_str(),
                DocumentJobStatus::RetryWait.as_str(),
            ]))
            .to_owned(),
        Index::create()
            .name("idx_document_job_due")
            .table(DocumentJob::Table)
            .col(DocumentJob::Status)
            .col(DocumentJob::AvailableAt)
            .to_owned(),
        Index::create()
            .name("idx_document_job_stale_lease")
            .table(DocumentJob::Table)
            .col(DocumentJob::Status)
            .col(DocumentJob::LeaseExpiresAt)
            .to_owned(),
        Index::create()
            .name("idx_document_job_history")
            .table(DocumentJob::Table)
            .col(DocumentJob::DocumentId)
            .col(DocumentJob::CreatedAt)
            .to_owned(),
    ]
}

async fn retire_document_pipeline_sqlite(
    manager: &SchemaManager<'_>,
    retired: bool,
) -> Result<(), DbErr> {
    let shared = "id, chat_id, project_id, source_uri, media_type, title, source_blob_id, \
        source_sha256, source_byte_len, canonical_text, created_at, updated_at";
    let mut statements = vec![
        "PRAGMA foreign_keys=OFF".to_owned(),
        "BEGIN IMMEDIATE".to_owned(),
        format!("DROP TABLE IF EXISTS {DOCUMENT_PIPELINE_REBUILD}"),
    ];
    if retired {
        statements.extend([
            "DROP TABLE document_job".to_owned(),
            "DROP TABLE document_generation".to_owned(),
            document_pipeline_table().to_string(SqliteQueryBuilder),
            format!(
                "INSERT INTO {DOCUMENT_PIPELINE_REBUILD} ({shared}) \
                 SELECT {shared} FROM document"
            ),
        ]);
    } else {
        statements.extend([
            document_rebuild_table(false).to_string(SqliteQueryBuilder),
            format!(
                "INSERT INTO {DOCUMENT_REBUILD} ({shared}, canonical_fingerprint, source_regions, \
                 content_revision, revision_token, processing_status) \
                 SELECT {shared}, NULL, '[]', 1, id, \
                 CASE WHEN canonical_text <> '' THEN 'ready' ELSE 'queued' END FROM document"
            ),
        ]);
    }
    statements.push("DROP TABLE document".to_owned());
    statements.push(if retired {
        format!("ALTER TABLE {DOCUMENT_PIPELINE_REBUILD} RENAME TO document")
    } else {
        format!("ALTER TABLE {DOCUMENT_REBUILD} RENAME TO document")
    });
    statements.extend(
        document_rebuild_indexes()
            .iter()
            .map(|index| index.to_string(SqliteQueryBuilder)),
    );
    if !retired {
        statements.extend([
            legacy_document_generation_table().to_string(SqliteQueryBuilder),
            "INSERT INTO document_generation \
             (document_id, content_revision, revision_token, tombstone) \
             SELECT id, 1, id, FALSE FROM document"
                .to_owned(),
            legacy_document_job_table().to_string(SqliteQueryBuilder),
        ]);
        statements.extend(
            legacy_document_job_indexes()
                .iter()
                .map(|index| index.to_string(SqliteQueryBuilder)),
        );
    }
    statements.extend(["COMMIT".to_owned(), "PRAGMA foreign_keys=ON".to_owned()]);
    manager
        .get_connection()
        .execute_unprepared(&format!("{};", statements.join(";\n")))
        .await?;
    Ok(())
}

#[async_trait::async_trait]
impl MigrationTrait for RetireDocumentPipeline {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() == DatabaseBackend::Sqlite {
            return retire_document_pipeline_sqlite(manager, true).await;
        }
        manager
            .get_connection()
            .execute_unprepared(
                "DROP TABLE document_job CASCADE; \
                 DROP TABLE document_generation CASCADE; \
                 ALTER TABLE document DROP COLUMN canonical_fingerprint CASCADE, \
                    DROP COLUMN source_regions CASCADE, \
                    DROP COLUMN content_revision CASCADE, \
                    DROP COLUMN revision_token CASCADE, \
                    DROP COLUMN processing_status CASCADE;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() == DatabaseBackend::Sqlite {
            return retire_document_pipeline_sqlite(manager, false).await;
        }
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE document ADD COLUMN content_revision BIGINT NOT NULL DEFAULT 1; \
                 ALTER TABLE document ADD COLUMN canonical_fingerprint TEXT; \
                 ALTER TABLE document ADD COLUMN source_regions JSONB NOT NULL DEFAULT '[]'::jsonb; \
                 ALTER TABLE document ADD COLUMN revision_token UUID; \
                 UPDATE document SET revision_token = id; \
                 ALTER TABLE document ALTER COLUMN revision_token SET NOT NULL; \
                 ALTER TABLE document ADD COLUMN processing_status TEXT; \
                 UPDATE document SET processing_status = CASE \
                    WHEN canonical_text <> '' THEN 'ready' ELSE 'queued' END; \
                 ALTER TABLE document ALTER COLUMN processing_status SET NOT NULL; \
                 ALTER TABLE document ADD CONSTRAINT chk_document_content_revision \
                    CHECK (content_revision >= 1); \
                 ALTER TABLE document ADD CONSTRAINT chk_document_processing_status \
                    CHECK (processing_status IN ('queued', 'processing', 'ready', 'failed')); \
                 ALTER TABLE document ADD CONSTRAINT chk_document_canonical_fingerprint \
                    CHECK (canonical_fingerprint IS NULL OR \
                    char_length(canonical_fingerprint) BETWEEN 1 AND 512);",
            )
            .await?;
        manager
            .create_table(legacy_document_generation_table())
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "INSERT INTO document_generation \
                 (document_id, content_revision, revision_token, tombstone) \
                 SELECT id, 1, id, FALSE FROM document;",
            )
            .await?;
        manager.create_table(legacy_document_job_table()).await?;
        for index in legacy_document_job_indexes() {
            manager.create_index(index).await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum AssistantCitationLight {
    Table,
    Id,
    MessageId,
    Ordinal,
    DocumentId,
    Locator,
}

#[derive(DeriveIden)]
enum Project {
    Table,
    Id,
    Title,
    AttachmentRevision,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Document {
    Table,
    Id,
    ChatId,
    ProjectId,
    SourceUri,
    MediaType,
    Title,
    SourceBlobId,
    SourceSha256,
    SourceByteLen,
    CanonicalText,
    CanonicalFingerprint,
    SourceRegions,
    ContentRevision,
    RevisionToken,
    ProcessingStatus,
    IndexedRevision,
    IndexFingerprint,
    CreatedAt,
    UpdatedAt,
    IndexedAt,
}

#[derive(DeriveIden)]
enum DocumentGeneration {
    Table,
    DocumentId,
    ContentRevision,
    RevisionToken,
    Tombstone,
    RetirementPending,
    RetirementContentRevision,
    RetirementRevisionToken,
}

#[derive(DeriveIden)]
enum RetrievalEvidence {
    Table,
    CallId,
    Rank,
    SourceToken,
    ChatId,
    TurnId,
    DocumentId,
    ContentRevision,
    RevisionToken,
    ChunkId,
    SpanStart,
    SpanEnd,
    Snippet,
    HeadingPath,
    SourceRegions,
    SourceKind,
    SourceUri,
    Location,
}

#[derive(DeriveIden)]
enum AssistantCitation {
    Table,
    Id,
    MessageId,
    Ordinal,
    ChatId,
    TurnId,
    EvidenceCallId,
    EvidenceRank,
}

#[derive(DeriveIden)]
enum Output {
    Table,
    Id,
    ChatId,
    Filename,
    MediaType,
    CurrentRevisionId,
    RevisionCount,
    CreatedAt,
    UpdatedAt,
    DeletedAt,
}

#[derive(DeriveIden)]
enum OutputRevision {
    Table,
    Id,
    OutputId,
    Ordinal,
    ByteLen,
    Sha256,
    TurnId,
    ProducingRunId,
    CreatedAt,
}

#[derive(DeriveIden)]
enum App {
    Table,
    Id,
    Name,
    CurrentRevisionId,
    RevisionCount,
    CreatedAt,
    UpdatedAt,
    DeletedAt,
}

#[derive(DeriveIden)]
enum AppRevision {
    Table,
    Id,
    AppId,
    Ordinal,
    ManifestJson,
    ByteLen,
    Sha256,
    TurnId,
    ProducingRunId,
    ChatId,
    CreatedAt,
}

#[derive(DeriveIden)]
enum AppGrant {
    Table,
    AppId,
    BindingsJson,
    CreatedAt,
}

#[derive(DeriveIden)]
enum OutputRevisionCitation {
    Table,
    Id,
    OutputRevisionId,
    Ordinal,
    ChatId,
    TurnId,
    EvidenceCallId,
    EvidenceRank,
}

#[derive(DeriveIden)]
enum OperationLog {
    Table,
    RunId,
    OperationId,
    State,
    Fingerprint,
    ExternalEffect,
    OwnerEpoch,
    Body,
    Retained,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum SandboxProvision {
    Table,
    RunId,
    Tag,
    State,
    Handle,
    LateResultEvidence,
    WindowExpiresAt,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum BlobRetirement {
    Table,
    BlobId,
    Status,
    AttemptCount,
    MaxAttempts,
    AvailableAt,
    LeaseToken,
    LeaseExpiresAt,
    StartedAt,
    FinishedAt,
    LastErrorCode,
    LastErrorDetail,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum DocumentJob {
    Table,
    Id,
    DocumentId,
    ContentRevision,
    RevisionToken,
    Kind,
    Status,
    PipelineFingerprint,
    AttemptCount,
    MaxAttempts,
    AvailableAt,
    LeaseToken,
    LeaseExpiresAt,
    StartedAt,
    FinishedAt,
    LastErrorCode,
    LastErrorDetail,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Chat {
    Table,
    Id,
    ProjectId,
    Title,
    Model,
    ReasoningEffort,
    PermissionMode,
    NetworkPolicy,
    CitationFormat,
    AttachmentRevision,
    CreatedAt,
}

#[derive(DeriveIden)]
enum AgentRun {
    Table,
    Id,
    ChatId,
    ParentId,
    ParentDepth,
    SpawnCallId,
    /// The pre-split execution column; only migrations up to the tier/location
    /// split reference it.
    Execution,
    Tier,
    ExecutionLocation,
    Depth,
    Status,
    Input,
    Model,
    AttemptCount,
    MaxAttempts,
    ClaimCount,
    AvailableAt,
    DeadlineAt,
    LeaseToken,
    LeaseExpiresAt,
    StartedAt,
    FinishedAt,
    LastErrorCode,
    LastErrorDetail,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum SandboxAgentAdmission {
    Table,
    ChildRunId,
    ParentRunId,
    OriginTurnId,
    ChatId,
    SpawnCallId,
    DelegatedRootId,
    DelegatedRelativePath,
    AdmittedAt,
}

#[derive(DeriveIden)]
enum SandboxSpawnCheckpoint {
    Table,
    CallId,
    ChildRunId,
    ParentRunId,
    OriginTurnId,
    ChatId,
    LeaseToken,
    AttemptCount,
    ClaimCount,
    ProviderId,
    HistoryOrder,
    Arguments,
    Result,
    SteerRevision,
    EventOrdinal,
    ModelSteps,
    InputTokens,
    OutputTokens,
    CacheReadInputTokens,
    CacheCreationInputTokens,
    EventSeq,
    CommittedAt,
}

#[derive(DeriveIden)]
enum AgentRunClaim {
    Table,
    Token,
    AgentRunId,
    AttemptCount,
    ClaimCount,
    ClaimedAt,
    LeaseExpiresAt,
}

#[derive(DeriveIden)]
enum AgentRunClaimLock {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum AgentRunResult {
    Table,
    AgentRunId,
    LeaseToken,
    AttemptCount,
    ClaimCount,
    PayloadKind,
    PayloadJson,
    Text,
    SubmittedAt,
}

#[derive(DeriveIden)]
enum AgentRunCancellation {
    Table,
    AgentRunId,
    LeaseToken,
    AttemptCount,
    ClaimCount,
    Reason,
    RequestedAt,
}

#[derive(DeriveIden)]
enum AgentRunInbox {
    Table,
    ChildRunId,
    ParentRunId,
    ChatId,
    ParentDepth,
    ResultLeaseToken,
    ResultAttemptCount,
    ResultClaimCount,
    Status,
    ClaimCount,
    LeaseToken,
    LeaseExpiresAt,
    ConsumedLeaseToken,
    ConsumedAt,
    DeliveredAt,
}

#[derive(DeriveIden)]
enum TurnAgentRunWait {
    Table,
    ChildRunId,
    ParentRunId,
    TurnId,
    ChatId,
    ParkLeaseToken,
    AtomicAdmission,
    AttemptCount,
    ClaimCount,
    ModelSteps,
    InputTokens,
    OutputTokens,
    CacheReadInputTokens,
    CacheCreationInputTokens,
    Status,
    ParkedAt,
    ClosedAt,
}

#[derive(DeriveIden)]
enum TurnAgentRunWaitSet {
    Table,
    Id,
    ParentRunId,
    TurnId,
    ChatId,
    ProviderId,
    HistoryOrder,
    Arguments,
    Condition,
    ParkLeaseToken,
    ExpectedSteerRevision,
    AttemptCount,
    ClaimCount,
    ModelSteps,
    InputTokens,
    OutputTokens,
    CacheReadInputTokens,
    CacheCreationInputTokens,
    EventOrdinal,
    EventSeq,
    Status,
    ParkedAt,
    ClosedAt,
    ResumeToken,
}

#[derive(DeriveIden)]
enum TurnAgentRunWaitLock {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum TurnAgentRunWaitMember {
    Table,
    WaitId,
    Position,
    ChildRunId,
    ParentRunId,
    OriginTurnId,
    ChatId,
    Open,
}

#[derive(DeriveIden)]
enum SandboxToolCall {
    Table,
    Id,
    AgentRunId,
    ChatId,
    AgentRunDepth,
    ProviderId,
    Name,
    Arguments,
    Status,
    ParkLeaseToken,
    ParkAttemptCount,
    ParkClaimCount,
    ExecutorLeaseToken,
    ExecutorLeaseExpiresAt,
    CreatedAt,
    ResolvedAt,
}

#[derive(DeriveIden)]
enum SandboxToolCallReceipt {
    Table,
    CallId,
    ExecutorLeaseToken,
    Status,
    Result,
    ErrorCode,
    ErrorDetail,
    ResolvedAt,
}

#[derive(DeriveIden)]
enum ProjectRootAttachment {
    Table,
    ProjectId,
    RootId,
    Position,
}

#[derive(DeriveIden)]
enum ChatRootAttachment {
    Table,
    ChatId,
    RootId,
    Position,
    Origin,
}

#[derive(DeriveIden)]
enum RootAttachmentChange {
    Table,
    Id,
    ChatId,
    SubjectKind,
    SubjectId,
    ExecutorId,
    RootId,
    Action,
    Origin,
    ProjectionPosition,
    ProjectionExistedBefore,
    ExpectedRevision,
    BeforeRevision,
    IntentRevision,
    Phase,
    ResultRevision,
    ProjectionChanged,
    BrokerChanged,
    BrokerCurrentlyAttached,
    FailureCode,
    FailureMessage,
    FailureRetryable,
    CreatedAt,
    FinishedAt,
}

#[derive(DeriveIden)]
enum Message {
    Table,
    Id,
    ChatId,
    TurnId,
    Seq,
    Role,
    Content,
    TurnLeaseToken,
    CreatedAt,
}

#[derive(DeriveIden)]
enum ContextCheckpoint {
    Table,
    ChatId,
    SourceMessageId,
    SourceMessageSeq,
    FormatVersion,
    Content,
    InputTokens,
    OutputTokens,
    CacheReadInputTokens,
    CacheCreationInputTokens,
    CreatedAt,
}

#[derive(DeriveIden)]
enum ExecFileSnapshot {
    Table,
    Id,
    ChatId,
    TurnId,
    FolderPath,
    RelativePath,
    ChangeKind,
    PriorBlobId,
    PriorByteLen,
    NewSha256,
    UndoState,
    RecordedAt,
}

#[derive(DeriveIden)]
enum ExecFileRejection {
    Table,
    Id,
    ChatId,
    TurnId,
    FolderPath,
    RelativePath,
    Reason,
    RecordedAt,
}

#[derive(DeriveIden)]
enum MessageAttachment {
    Table,
    MessageId,
    Ordinal,
    ChatId,
    BlobId,
    MediaType,
    Width,
    Height,
    ByteLen,
    CreatedAt,
}

#[derive(DeriveIden)]
enum MessageDocumentAttachment {
    Table,
    MessageId,
    Ordinal,
    ChatId,
    DocumentId,
    CreatedAt,
}

#[derive(DeriveIden)]
enum MessageIdentity {
    Table,
    Id,
    ChatId,
    TurnId,
    Owner,
}

#[derive(DeriveIden)]
enum TurnRun {
    Table,
    Id,
    ChatId,
    AgentRunId,
    AgentRunDepth,
    InputMessageId,
    OutputMessageId,
    Model,
    Status,
    AttemptCount,
    MaxAttempts,
    ClaimCount,
    ModelSteps,
    InputTokens,
    OutputTokens,
    CacheReadInputTokens,
    CacheCreationInputTokens,
    AvailableAt,
    LeaseToken,
    LeaseExpiresAt,
    StartedAt,
    FinishedAt,
    LastErrorCode,
    LastErrorDetail,
    SteerRevision,
    LastSteerAppliedAt,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum TurnClaim {
    Table,
    Token,
    TurnId,
    AttemptCount,
    ClaimCount,
    ClaimedAt,
    LeaseExpiresAt,
}

#[derive(DeriveIden)]
enum TurnClaimLock {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum TurnFailure {
    Table,
    LeaseToken,
    TurnId,
    AttemptCount,
    ModelSteps,
    InputTokens,
    OutputTokens,
    CacheReadInputTokens,
    CacheCreationInputTokens,
    RequestedRetryAt,
    ErrorCode,
    ErrorDetail,
    ResolvedAt,
    ResultStatus,
}

#[derive(DeriveIden)]
enum TurnSteer {
    Table,
    Id,
    TurnId,
    ChatId,
    Content,
    Interrupt,
    Status,
    AppliedLeaseToken,
    MessageId,
    PrecedingAssistantMessageId,
    CreatedAt,
    ResolvedAt,
}

#[derive(DeriveIden)]
enum ToolCall {
    Table,
    Id,
    ChatId,
    TurnId,
    ProviderId,
    HistoryOrder,
    Name,
    Arguments,
    RawArguments,
    Execution,
    Status,
    Result,
    ResultPreview,
    ErrorCode,
    ErrorDetail,
    ApprovalStatus,
    ApprovalClass,
    ApprovalKind,
    ApprovalReason,
    ApprovalRequestedAt,
    ApprovalDecidedAt,
    ApprovalEventSeq,
    ApprovalGrantSourceCallId,
    AutoJudgeStatus,
    ClientExecutorId,
    ClientLeaseToken,
    ClientLeaseExpiresAt,
    TurnLeaseToken,
    ResolutionTurnLeaseToken,
    CreatedAt,
    ResolvedAt,
}

#[derive(DeriveIden)]
enum StandingToolGrant {
    Table,
    SourceCallId,
    ChatId,
    ProjectId,
    ToolName,
    ApprovalKind,
    Scope,
    GrantedAt,
}

#[derive(DeriveIden)]
enum TurnClientWait {
    Table,
    CallId,
    TurnId,
    ChatId,
    ParkLeaseToken,
    AttemptCount,
    ClaimCount,
    ModelSteps,
    InputTokens,
    OutputTokens,
    CacheReadInputTokens,
    CacheCreationInputTokens,
    Status,
    ParkedAt,
    ClosedAt,
}

#[derive(DeriveIden)]
enum UserQuestionRequest {
    Table,
    CallId,
    TurnId,
    ChatId,
    Status,
    EventSeq,
    AskedAt,
    ResolvedAt,
    AdditionalUserContext,
}

#[derive(DeriveIden)]
enum PlanRequest {
    Table,
    CallId,
    TurnId,
    ChatId,
    Status,
    EventSeq,
    Title,
    Plan,
    Feedback,
    ProposedAt,
    ResolvedAt,
}

#[derive(DeriveIden)]
enum UserQuestion {
    Table,
    CallId,
    QuestionId,
    Position,
    Header,
    Prompt,
    Options,
    QuestionType,
    AllowFreeForm,
    AnswerOptionId,
    AnswerFreeForm,
    AnsweredAt,
    AnswerSelectedOptionIds,
    AnswerCustomAnswer,
    ResponseRecordedAt,
}

#[derive(DeriveIden)]
enum Setting {
    Table,
    Key,
    ValueJson,
}

#[derive(DeriveIden)]
enum Event {
    Table,
    ChatId,
    Seq,
    TurnId,
    LeaseToken,
    AttemptEventOrdinal,
    ScanToken,
    Terminal,
    Payload,
    CreatedAt,
}
