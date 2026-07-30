//! The default [`Store`], backed by SeaORM.
//!
//! One implementation and one migration set run on any SeaORM backend, chosen by
//! connection string — SQLite locally, Postgres for self-host. Types are native
//! per backend (uuid, timestamptz, jsonb on Postgres; the SQLite equivalents),
//! so nothing is stringly-encoded by hand. Enabled by the `sqlite` feature (which
//! compiles in the SQLite driver).

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, Database, DatabaseConnection, EntityTrait,
    FromQueryResult, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use sea_orm_migration::MigratorTrait;
use serde_json::Value;

use crate::approval::{ApprovalDecision, ApprovalRequest, ToolApproval};
use crate::deliverable::{CreateOutput, NewOutputRevision, OutputRecord, OutputRevision};
use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
#[cfg(test)]
use crate::id::MessageId;
use crate::id::{
    AgentRunId, AppId, AppRevisionId, CallId, ChatId, DocumentId, HostRootId, OutputId,
    OutputRevisionId, ProjectId, RootAttachmentChangeId, TurnId, TurnSteerId,
};
use crate::image::ImageRef;
use crate::local_app::{AppGrant, AppRecord, AppRevision, CreateApp, NewAppRevision};
#[cfg(test)]
use crate::model::Role;
use crate::model::{
    validate_project_root_projection, AgentRun, AgentRunInboxEntry, AgentRunStatus, AgentRunTier,
    AgentRunWaitSetCandidate, BeginRootAttachmentChange, BlobRetirement, BlobRetirementStatus,
    Chat, DocumentListCursor, DocumentRecord, DocumentScope, DocumentSourceBlob,
    DocumentSourceUpsert, DocumentSummaryRecord, DocumentUpsert, Message, MessageAttachment,
    MessageDocumentAttachment, PermissionMode, Project, ReasoningEffort, RootAttachmentChange,
    RootAttachmentChangeTerminal, SandboxToolCall, SandboxToolCallReceipt, SandboxToolCallRequest,
    ToolCallRecord, ToolCallResolution, TurnAgentRunWaitStatus, TurnCheckpointProgress,
    TurnClientWaitStatus, TurnFailureRetry, TurnRun, TurnRunStatus, TurnSteerStatus,
    MAX_ROOT_ATTACHMENTS,
};
use crate::provider::{StopReason, Usage};
use crate::semantic_checkpoint::{ContextCheckpoint, SaveContextCheckpointOutcome};
use crate::storage::{
    AcceptAgentRunOutcome, AcceptClaimedToolCallOutcome, AcceptSandboxAgentRunAndParkTurnOutcome,
    AcceptToolCallOutcome, AcceptTurnOutcome, AcceptTurnSteerOutcome, AdmitSandboxAgentRunOutcome,
    AppendClaimedMessageOutcome, BeginRootAttachmentChangeOutcome, CheckpointSandboxSpawnOutcome,
    ClaimAgentRunInboxOutcome, ClaimClientToolCallOutcome, ClaimDelegatedFileReadOutcome,
    ClaimSandboxToolCallOutcome, ClaimTurnRunOutcome, CompleteTurnRunOutcome,
    ConsumeAgentRunInboxAndResumeTurnOutcome, ConsumeAgentRunInboxOutcome,
    DecideToolApprovalOutcome, DeleteChatOutcome, DeleteProjectOutcome, FailAgentRunOutcome,
    FinishAgentRunCancellationOutcome, FinishRootAttachmentChangeOutcome,
    FinishTurnCancellationOutcome, HeartbeatClientToolCallOutcome, JournaledClientToolCallOutcome,
    JournaledToolApprovalOutcome, JournaledTurnOutcome, JournaledTurnSteerOutcome,
    OperationClaimOutcome, OperationLogEntry, OperationLogWrite, ParkSandboxToolCallOutcome,
    ParkTurnForAgentRunInboxOutcome, ParkTurnForAgentRunWaitSetOutcome,
    ParkTurnForClientCallOutcome, RecordTurnFailureOutcome, RequestAgentRunCancellationOutcome,
    RequestToolApprovalOutcome, RequestTurnCancellationOutcome, ResolveSandboxToolCallOutcome,
    ResolveToolCallOutcome, ResumeTurnForAgentRunWaitSetOutcome, Store,
    SubmitAgentRunResultOutcome, TurnLeaseFence,
};

mod ops;

/// Map any SeaORM failure into an [`AgentError::Store`].
fn store_err(err: impl std::fmt::Display) -> AgentError {
    AgentError::Store(err.to_string())
}

/// A [`Store`] backed by a SeaORM connection (SQLite today, Postgres-ready).
#[derive(Clone)]
pub struct DbStore {
    conn: DatabaseConnection,
}

/// Projected row for metadata-only document listings. Keeping this distinct
/// from the entity model makes it impossible for this query to select the
/// canonical text by accident.
#[derive(Debug, FromQueryResult)]
struct DocumentSummaryRow {
    id: uuid::Uuid,
    chat_id: Option<uuid::Uuid>,
    project_id: Option<uuid::Uuid>,
    source_uri: Option<String>,
    media_type: String,
    title: Option<String>,
    source_byte_len: Option<i64>,
    /// Emptiness of the canonical text, evaluated in the database so a listing
    /// still never transfers the text itself.
    has_canonical_text: bool,
    created_at: chrono::DateTime<Utc>,
    updated_at: chrono::DateTime<Utc>,
}

impl DbStore {
    /// Connect to `url` and run migrations. For a SQLite file that should be
    /// created if missing, include `?mode=rwc` (e.g.
    /// `sqlite:///path/openwave.db?mode=rwc`).
    pub async fn connect(url: &str) -> Result<Self> {
        let conn = Database::connect(url).await.map_err(store_err)?;
        // WAL lets a reader (e.g. the UI listing chats) proceed concurrently
        // with a writer (a turn appending messages). SQLite-only; it's a
        // persistent, file-level setting, so running it once at connect suffices.
        if conn.get_database_backend() == sea_orm::DatabaseBackend::Sqlite {
            conn.execute_unprepared("PRAGMA journal_mode=WAL;")
                .await
                .map_err(store_err)?;
        }
        migration::Migrator::up(&conn, None)
            .await
            .map_err(store_err)?;
        Ok(Self { conn })
    }
}

#[async_trait]
impl Store for DbStore {
    async fn create_project(&self, project: &Project) -> Result<()> {
        validate_project_attachments(project)?;
        let transaction = self.conn.begin().await.map_err(store_err)?;
        entities::project::ActiveModel {
            id: Set(project.id.0),
            title: Set(project.title.clone()),
            attachment_revision: Set(project.attachment_revision),
            created_at: Set(project.created_at),
        }
        .insert(&transaction)
        .await
        .map_err(store_err)?;
        for (position, root_id) in project.root_attachments.iter().copied().enumerate() {
            entities::project_root_attachment::ActiveModel {
                project_id: Set(project.id.0),
                root_id: Set(*root_id.as_uuid()),
                position: Set(i32::try_from(position)
                    .map_err(|_| AgentError::Store("project root position exceeds i32".into()))?),
            }
            .insert(&transaction)
            .await
            .map_err(store_err)?;
        }
        transaction.commit().await.map_err(store_err)?;
        Ok(())
    }

    async fn get_project(&self, id: ProjectId) -> Result<Option<Project>> {
        let mut rows = entities::project::Entity::find_by_id(id.0)
            .find_with_related(entities::project_root_attachment::Entity)
            .order_by_asc(entities::project_root_attachment::Column::Position)
            .all(&self.conn)
            .await
            .map_err(store_err)?;
        rows.pop()
            .map(|(model, roots)| project_from_models(model, roots))
            .transpose()
    }

    async fn list_projects(&self) -> Result<Vec<Project>> {
        let mut projects = entities::project::Entity::find()
            .find_with_related(entities::project_root_attachment::Entity)
            .order_by_asc(entities::project_root_attachment::Column::Position)
            .all(&self.conn)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(|(model, roots)| project_from_models(model, roots))
            .collect::<Result<Vec<_>>>()?;
        projects.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.id.0.cmp(&left.id.0))
        });
        Ok(projects)
    }

    async fn update_project_title(&self, id: ProjectId, title: Option<String>) -> Result<bool> {
        let result = entities::project::Entity::update_many()
            .set(entities::project::ActiveModel {
                title: Set(title),
                ..Default::default()
            })
            .filter(entities::project::Column::Id.eq(id.0))
            .exec(&self.conn)
            .await
            .map_err(store_err)?;
        Ok(result.rows_affected == 1)
    }

    async fn delete_project(&self, id: ProjectId) -> Result<DeleteProjectOutcome> {
        let transaction = self.conn.begin().await.map_err(store_err)?;
        if !ops::acquire_project_write_lock(&transaction, id).await? {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(DeleteProjectOutcome::NotFound);
        }

        let has_chats = entities::chat::Entity::find()
            .filter(entities::chat::Column::ProjectId.eq(id.0))
            .one(&transaction)
            .await
            .map_err(store_err)?
            .is_some();
        let has_documents = entities::document::Entity::find()
            .filter(entities::document::Column::ProjectId.eq(id.0))
            .one(&transaction)
            .await
            .map_err(store_err)?
            .is_some();
        let has_roots = entities::project_root_attachment::Entity::find()
            .filter(entities::project_root_attachment::Column::ProjectId.eq(id.0))
            .one(&transaction)
            .await
            .map_err(store_err)?
            .is_some();
        if has_chats || has_documents || has_roots {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(DeleteProjectOutcome::NotEmpty);
        }

        let deleted = entities::project::Entity::delete_by_id(id.0)
            .exec(&transaction)
            .await
            .map_err(store_err)?;
        if deleted.rows_affected != 1 {
            transaction.rollback().await.map_err(store_err)?;
            return Err(AgentError::Store(format!(
                "project {id} disappeared while locked"
            )));
        }
        transaction.commit().await.map_err(store_err)?;
        Ok(DeleteProjectOutcome::Deleted)
    }

    async fn create_document(&self, document: &DocumentRecord) -> Result<()> {
        let source_byte_len = document
            .source_blob
            .as_ref()
            .map(validate_document_source_blob)
            .transpose()?;
        let transaction = self.conn.begin().await.map_err(store_err)?;
        ops::require_document_scope_write_lock(&transaction, document.chat_id, document.project_id)
            .await?;
        entities::document::ActiveModel {
            id: Set(document.id.0),
            chat_id: Set(document.chat_id.map(|id| id.0)),
            project_id: Set(document.project_id.map(|id| id.0)),
            source_uri: Set(document.source_uri.clone()),
            media_type: Set(document.media_type.clone()),
            title: Set(document.title.clone()),
            source_blob_id: Set(document.source_blob.as_ref().map(|blob| blob.id)),
            source_sha256: Set(document
                .source_blob
                .as_ref()
                .map(|blob| blob.sha256.to_vec())),
            source_byte_len: Set(source_byte_len),
            canonical_text: Set(document.canonical_text.clone()),
            created_at: Set(document.created_at),
            updated_at: Set(document.updated_at),
        }
        .insert(&transaction)
        .await
        .map_err(store_err)?;
        if let Some(source_blob) = document.source_blob.as_ref() {
            ops::blob::cancel_on(&transaction, source_blob.id).await?;
        }
        transaction.commit().await.map_err(store_err)?;
        Ok(())
    }

    async fn get_document(&self, id: DocumentId) -> Result<Option<DocumentRecord>> {
        entities::document::Entity::find_by_id(id.0)
            .one(&self.conn)
            .await
            .map_err(store_err)?
            .map(document_from_model)
            .transpose()
    }

    async fn list_documents(&self, scope: DocumentScope) -> Result<Vec<DocumentRecord>> {
        let mut query = entities::document::Entity::find();
        query = match scope {
            DocumentScope::All => query,
            DocumentScope::Unscoped => query
                .filter(entities::document::Column::ChatId.is_null())
                .filter(entities::document::Column::ProjectId.is_null()),
            DocumentScope::Project(id) => query
                .filter(entities::document::Column::ChatId.is_null())
                .filter(entities::document::Column::ProjectId.eq(id.0)),
            DocumentScope::Chat(id) => query.filter(entities::document::Column::ChatId.eq(id.0)),
        };
        query
            .order_by_desc(entities::document::Column::CreatedAt)
            .all(&self.conn)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(document_from_model)
            .collect()
    }

    async fn list_document_summaries(
        &self,
        scope: DocumentScope,
        after: Option<DocumentListCursor>,
        limit: u64,
    ) -> Result<Vec<DocumentSummaryRecord>> {
        let mut query = entities::document::Entity::find();
        query = match scope {
            DocumentScope::All => query,
            DocumentScope::Unscoped => query
                .filter(entities::document::Column::ChatId.is_null())
                .filter(entities::document::Column::ProjectId.is_null()),
            DocumentScope::Project(id) => query
                .filter(entities::document::Column::ChatId.is_null())
                .filter(entities::document::Column::ProjectId.eq(id.0)),
            DocumentScope::Chat(id) => query.filter(entities::document::Column::ChatId.eq(id.0)),
        };
        if let Some(cursor) = after {
            query = query.filter(
                sea_orm::Condition::any()
                    .add(entities::document::Column::CreatedAt.lt(cursor.created_at))
                    .add(
                        sea_orm::Condition::all()
                            .add(entities::document::Column::CreatedAt.eq(cursor.created_at))
                            .add(entities::document::Column::Id.lt(cursor.id.0)),
                    ),
            );
        }

        query
            .select_only()
            .columns([
                entities::document::Column::Id,
                entities::document::Column::ChatId,
                entities::document::Column::ProjectId,
                entities::document::Column::SourceUri,
                entities::document::Column::MediaType,
                entities::document::Column::Title,
                entities::document::Column::SourceByteLen,
                entities::document::Column::CreatedAt,
                entities::document::Column::UpdatedAt,
            ])
            .column_as(
                sea_orm::sea_query::ExprTrait::ne(
                    sea_orm::sea_query::Expr::col(entities::document::Column::CanonicalText),
                    "",
                ),
                "has_canonical_text",
            )
            .order_by_desc(entities::document::Column::CreatedAt)
            .order_by_desc(entities::document::Column::Id)
            .limit(limit)
            .into_model::<DocumentSummaryRow>()
            .all(&self.conn)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(document_summary_from_row)
            .collect()
    }

    async fn list_document_ids(&self, scope: DocumentScope) -> Result<Vec<DocumentId>> {
        let mut query = entities::document::Entity::find()
            .select_only()
            .column(entities::document::Column::Id);
        query = match scope {
            DocumentScope::All => query,
            DocumentScope::Unscoped => query
                .filter(entities::document::Column::ChatId.is_null())
                .filter(entities::document::Column::ProjectId.is_null()),
            DocumentScope::Project(id) => query
                .filter(entities::document::Column::ChatId.is_null())
                .filter(entities::document::Column::ProjectId.eq(id.0)),
            DocumentScope::Chat(id) => query.filter(entities::document::Column::ChatId.eq(id.0)),
        };
        Ok(query
            .order_by_desc(entities::document::Column::CreatedAt)
            .into_tuple::<uuid::Uuid>()
            .all(&self.conn)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(DocumentId)
            .collect())
    }

    async fn record_exec_file_snapshots(
        &self,
        chat_id: ChatId,
        turn_id: TurnId,
        files: &[crate::model::ExecFileSnapshotRecord],
    ) -> Result<()> {
        ops::exec_file_snapshot::record(self, chat_id, turn_id, files).await
    }

    async fn list_exec_file_snapshots(
        &self,
        chat_id: ChatId,
    ) -> Result<Vec<crate::model::ExecFileSnapshot>> {
        ops::exec_file_snapshot::list_for_chat(self, chat_id).await
    }

    async fn get_blob_retirement(&self, blob_id: uuid::Uuid) -> Result<Option<BlobRetirement>> {
        ops::blob::get(self, blob_id).await
    }

    async fn ensure_orphan_blob_retirement(&self, blob_id: uuid::Uuid) -> Result<bool> {
        ops::blob::ensure_orphan(self, blob_id).await
    }

    async fn claim_blob_retirement(
        &self,
        now: chrono::DateTime<Utc>,
        lease_expires_at: chrono::DateTime<Utc>,
    ) -> Result<Option<BlobRetirement>> {
        ops::blob::claim(self, now, lease_expires_at).await
    }

    async fn heartbeat_blob_retirement(
        &self,
        blob_id: uuid::Uuid,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
        lease_expires_at: chrono::DateTime<Utc>,
    ) -> Result<bool> {
        ops::blob::heartbeat(self, blob_id, lease_token, now, lease_expires_at).await
    }

    async fn validate_blob_retirement_lease(
        &self,
        blob_id: uuid::Uuid,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
    ) -> Result<bool> {
        ops::blob::validate_lease(self, blob_id, lease_token, now).await
    }

    async fn complete_blob_retirement(
        &self,
        blob_id: uuid::Uuid,
        lease_token: uuid::Uuid,
        completed_at: chrono::DateTime<Utc>,
    ) -> Result<bool> {
        ops::blob::complete(self, blob_id, lease_token, completed_at).await
    }

    async fn record_blob_retirement_failure(
        &self,
        blob_id: uuid::Uuid,
        lease_token: uuid::Uuid,
        failed_at: chrono::DateTime<Utc>,
        retry_at: Option<chrono::DateTime<Utc>>,
        error_code: &str,
        error_detail: Option<&str>,
    ) -> Result<Option<BlobRetirementStatus>> {
        ops::blob::record_failure(
            self,
            blob_id,
            lease_token,
            failed_at,
            retry_at,
            error_code,
            error_detail,
        )
        .await
    }

    async fn delete_document(&self, id: DocumentId) -> Result<()> {
        let transaction = self.conn.begin().await.map_err(store_err)?;
        let document = entities::document::Entity::find_by_id(id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?;
        if let Some(document) = document {
            entities::document::Entity::delete_by_id(id.0)
                .exec(&transaction)
                .await
                .map_err(store_err)?;
            if let Some(blob_id) = document.source_blob_id {
                ops::blob::enqueue_on(&transaction, blob_id).await?;
            }
        }
        transaction.commit().await.map_err(store_err)?;
        Ok(())
    }

    async fn upsert_document(&self, document: &DocumentUpsert) -> Result<DocumentRecord> {
        let transaction = self.conn.begin().await.map_err(store_err)?;
        ops::require_document_scope_write_lock(&transaction, document.chat_id, document.project_id)
            .await?;
        let record = upsert_document_on(&transaction, document).await?;
        transaction.commit().await.map_err(store_err)?;
        Ok(record)
    }

    async fn accept_document_source(
        &self,
        document: &DocumentSourceUpsert,
    ) -> Result<DocumentRecord> {
        ops::document::accept_source(self, document).await
    }

    async fn create_chat(&self, chat: &Chat) -> Result<()> {
        ops::conversation::create_chat(self, chat).await
    }

    async fn create_chat_with_project_defaults(&self, chat: &Chat) -> Result<Chat> {
        ops::conversation::create_chat_with_project_defaults(self, chat).await
    }

    async fn set_chat_model(&self, id: ChatId, model: Option<String>) -> Result<()> {
        ops::conversation::set_chat_model(self, id, model).await
    }

    async fn set_chat_title(&self, id: ChatId, title: Option<String>) -> Result<()> {
        ops::conversation::set_chat_title(self, id, title).await
    }

    async fn set_chat_title_if_unset(&self, id: ChatId, title: &str) -> Result<bool> {
        ops::conversation::set_chat_title_if_unset(self, id, title).await
    }

    async fn update_chat_metadata(
        &self,
        id: ChatId,
        title: Option<Option<String>>,
        model: Option<Option<String>>,
        reasoning_effort: Option<Option<ReasoningEffort>>,
        permission_mode: Option<Option<PermissionMode>>,
    ) -> Result<bool> {
        ops::conversation::update_chat_metadata(
            self,
            id,
            title,
            model,
            reasoning_effort,
            permission_mode,
        )
        .await
    }

    async fn get_chat(&self, id: ChatId) -> Result<Option<Chat>> {
        ops::conversation::get_chat(self, id).await
    }

    async fn list_chats(&self) -> Result<Vec<Chat>> {
        ops::conversation::list_chats(self).await
    }

    async fn delete_chat(&self, id: ChatId) -> Result<DeleteChatOutcome> {
        ops::conversation::delete_chat(self, id).await
    }

    async fn get_chat_transcript(
        &self,
        id: ChatId,
    ) -> Result<Option<crate::storage::ChatTranscriptSnapshot>> {
        ops::conversation::get_chat_transcript(self, id).await
    }

    async fn create_output(&self, request: &CreateOutput) -> Result<OutputRecord> {
        ops::output::create_output(self, request).await
    }

    async fn append_output_revision(
        &self,
        output_id: OutputId,
        revision: &NewOutputRevision,
    ) -> Result<OutputRecord> {
        ops::output::append_output_revision(self, output_id, revision).await
    }

    async fn get_output(&self, id: OutputId) -> Result<Option<OutputRecord>> {
        ops::output::get_output(self, id).await
    }

    async fn list_outputs(&self, chat_id: ChatId, limit: u64) -> Result<Vec<OutputRecord>> {
        ops::output::list_outputs(self, chat_id, limit).await
    }

    async fn list_output_revisions(&self, output_id: OutputId) -> Result<Vec<OutputRevision>> {
        ops::output::list_output_revisions(self, output_id).await
    }

    async fn get_output_revision(&self, id: OutputRevisionId) -> Result<Option<OutputRevision>> {
        ops::output::get_output_revision(self, id).await
    }

    async fn delete_output(&self, id: OutputId, deleted_at: chrono::DateTime<Utc>) -> Result<bool> {
        ops::output::delete_output(self, id, deleted_at).await
    }

    async fn restore_output(
        &self,
        id: OutputId,
        restored_at: chrono::DateTime<Utc>,
    ) -> Result<bool> {
        ops::output::restore_output(self, id, restored_at).await
    }

    async fn set_current_output_revision(
        &self,
        output_id: OutputId,
        revision_id: OutputRevisionId,
        updated_at: chrono::DateTime<Utc>,
    ) -> Result<OutputRecord> {
        ops::output::set_current_output_revision(self, output_id, revision_id, updated_at).await
    }

    async fn create_app(&self, request: &CreateApp) -> Result<AppRecord> {
        ops::app::create_app(self, request).await
    }

    async fn append_app_revision(
        &self,
        app_id: AppId,
        revision: &NewAppRevision,
    ) -> Result<AppRecord> {
        ops::app::append_app_revision(self, app_id, revision).await
    }

    async fn get_app(&self, id: AppId) -> Result<Option<AppRecord>> {
        ops::app::get_app(self, id).await
    }

    async fn list_apps(&self, limit: u64) -> Result<Vec<AppRecord>> {
        ops::app::list_apps(self, limit).await
    }

    async fn list_app_revisions(&self, app_id: AppId) -> Result<Vec<AppRevision>> {
        ops::app::list_app_revisions(self, app_id).await
    }

    async fn get_app_revision(&self, id: AppRevisionId) -> Result<Option<AppRevision>> {
        ops::app::get_app_revision(self, id).await
    }

    async fn delete_app(&self, id: AppId, deleted_at: chrono::DateTime<Utc>) -> Result<bool> {
        ops::app::delete_app(self, id, deleted_at).await
    }

    async fn restore_app(&self, id: AppId, restored_at: chrono::DateTime<Utc>) -> Result<bool> {
        ops::app::restore_app(self, id, restored_at).await
    }

    async fn put_app_grant(&self, grant: &AppGrant) -> Result<()> {
        ops::app::put_app_grant(self, grant).await
    }

    async fn get_app_grant(&self, app_id: AppId) -> Result<Option<AppGrant>> {
        ops::app::get_app_grant(self, app_id).await
    }

    async fn delete_app_grant(&self, app_id: AppId) -> Result<bool> {
        ops::app::delete_app_grant(self, app_id).await
    }

    async fn save_context_checkpoint(
        &self,
        checkpoint: &ContextCheckpoint,
    ) -> Result<SaveContextCheckpointOutcome> {
        ops::context_checkpoint::save_context_checkpoint(self, checkpoint).await
    }

    async fn get_context_checkpoint(&self, chat_id: ChatId) -> Result<Option<ContextCheckpoint>> {
        ops::context_checkpoint::get_context_checkpoint(self, chat_id).await
    }

    async fn begin_root_attachment_change(
        &self,
        request: &BeginRootAttachmentChange,
    ) -> Result<BeginRootAttachmentChangeOutcome> {
        ops::root_attachment::begin_root_attachment_change(self, request).await
    }

    async fn finish_root_attachment_change(
        &self,
        id: RootAttachmentChangeId,
        executor_id: uuid::Uuid,
        terminal: &RootAttachmentChangeTerminal,
        finished_at: chrono::DateTime<Utc>,
    ) -> Result<FinishRootAttachmentChangeOutcome> {
        ops::root_attachment::finish_root_attachment_change(
            self,
            id,
            executor_id,
            terminal,
            finished_at,
        )
        .await
    }

    async fn get_root_attachment_change(
        &self,
        id: RootAttachmentChangeId,
    ) -> Result<Option<RootAttachmentChange>> {
        ops::root_attachment::get_root_attachment_change(self, id).await
    }

    async fn list_pending_root_attachment_changes(
        &self,
        executor_id: uuid::Uuid,
        limit: u64,
    ) -> Result<Vec<RootAttachmentChange>> {
        ops::root_attachment::list_pending_root_attachment_changes(self, executor_id, limit).await
    }

    async fn accept_agent_run(
        &self,
        id: AgentRunId,
        chat_id: ChatId,
        parent_id: Option<AgentRunId>,
        spawn_call_id: Option<CallId>,
        tier: AgentRunTier,
        input: Option<&str>,
    ) -> Result<AcceptAgentRunOutcome> {
        ops::agent_run::accept_agent_run(self, id, chat_id, parent_id, spawn_call_id, tier, input)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn admit_sandbox_agent_run(
        &self,
        origin_turn_id: TurnId,
        spawn_call_id: CallId,
        input: &str,
        lease_token: uuid::Uuid,
        expected_steer_revision: i64,
        max_outstanding_children: u32,
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<AdmitSandboxAgentRunOutcome>> {
        ops::agent_run::admit_sandbox_agent_run(
            self,
            origin_turn_id,
            spawn_call_id,
            input,
            lease_token,
            expected_steer_revision,
            max_outstanding_children,
            now,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn admit_sandbox_container_agent_run(
        &self,
        origin_turn_id: TurnId,
        spawn_call_id: CallId,
        input: &str,
        lease_token: uuid::Uuid,
        expected_steer_revision: i64,
        max_outstanding_children: u32,
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<AdmitSandboxAgentRunOutcome>> {
        ops::agent_run::admit_sandbox_container_agent_run(
            self,
            origin_turn_id,
            spawn_call_id,
            input,
            lease_token,
            expected_steer_revision,
            max_outstanding_children,
            now,
        )
        .await
    }

    async fn claim_container_agent_run(
        &self,
        id: AgentRunId,
        lease_token: uuid::Uuid,
        lease_duration: chrono::Duration,
        max_running_containers: u32,
    ) -> Result<Option<AgentRun>> {
        ops::agent_run::claim_container_agent_run(
            self,
            id,
            lease_token,
            lease_duration,
            max_running_containers,
        )
        .await
    }

    async fn list_container_agent_run_candidates(&self, limit: u64) -> Result<Vec<AgentRunId>> {
        ops::agent_run::list_container_agent_run_candidates(self, limit).await
    }

    async fn list_reclaimable_container_agent_runs(
        &self,
        now: chrono::DateTime<Utc>,
    ) -> Result<Vec<AgentRun>> {
        ops::agent_run::list_reclaimable_container_agent_runs(self, now).await
    }

    async fn reclaim_container_agent_run(
        &self,
        id: AgentRunId,
        lease_token: uuid::Uuid,
        lease_duration: chrono::Duration,
    ) -> Result<Option<AgentRun>> {
        ops::agent_run::reclaim_container_agent_run(self, id, lease_token, lease_duration).await
    }

    async fn begin_sandbox_provision(
        &self,
        run_id: uuid::Uuid,
        tag: &str,
        window_expires_at: chrono::DateTime<Utc>,
    ) -> Result<crate::storage::BeginSandboxProvisionOutcome> {
        ops::sandbox_provision::begin(self, run_id, tag, window_expires_at).await
    }

    async fn commit_sandbox_provision_handle(
        &self,
        run_id: uuid::Uuid,
        handle: &str,
    ) -> Result<bool> {
        ops::sandbox_provision::commit_handle(self, run_id, handle).await
    }

    async fn enqueue_sandbox_teardown(
        &self,
        run_id: uuid::Uuid,
    ) -> Result<Option<crate::storage::SandboxProvision>> {
        ops::sandbox_provision::enqueue_teardown(self, run_id).await
    }

    async fn complete_sandbox_teardown(&self, run_id: uuid::Uuid) -> Result<()> {
        ops::sandbox_provision::complete_teardown(self, run_id).await
    }

    async fn lapse_sandbox_provisions(
        &self,
        now: chrono::DateTime<Utc>,
    ) -> Result<Vec<crate::storage::SandboxProvision>> {
        ops::sandbox_provision::lapse(self, now).await
    }

    async fn list_sandbox_teardowns(&self) -> Result<Vec<crate::storage::SandboxProvision>> {
        ops::sandbox_provision::list_teardowns(self).await
    }

    async fn get_sandbox_provision(
        &self,
        run_id: uuid::Uuid,
    ) -> Result<Option<crate::storage::SandboxProvision>> {
        ops::sandbox_provision::get(self, run_id).await
    }

    async fn record_late_container_result_evidence(
        &self,
        run_id: uuid::Uuid,
        text: &str,
    ) -> Result<bool> {
        ops::sandbox_provision::record_late_result_evidence(self, run_id, text).await
    }

    async fn live_sandbox_tags(&self) -> Result<Vec<String>> {
        ops::sandbox_provision::live_tags(self).await
    }

    async fn checkpoint_sandbox_spawn(
        &self,
        request: &crate::model::SandboxSpawnCheckpointRequest,
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<CheckpointSandboxSpawnOutcome>> {
        ops::turn::checkpoint_sandbox_spawn(self, request, now).await
    }

    async fn get_sandbox_agent_admission(
        &self,
        child_run_id: AgentRunId,
    ) -> Result<Option<crate::model::SandboxAgentAdmission>> {
        ops::agent_run::get_sandbox_agent_admission(self, child_run_id).await
    }

    #[allow(clippy::too_many_arguments)] // Mirrors the Store checkpoint contract.
    async fn accept_sandbox_agent_run_and_park_turn(
        &self,
        child_run_id: AgentRunId,
        turn_id: TurnId,
        spawn_call_id: CallId,
        input: &str,
        lease_token: uuid::Uuid,
        expected_steer_revision: i64,
        progress: TurnCheckpointProgress,
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<AcceptSandboxAgentRunAndParkTurnOutcome>> {
        ops::agent_run::accept_sandbox_agent_run_and_park_turn(
            self,
            child_run_id,
            turn_id,
            spawn_call_id,
            input,
            lease_token,
            expected_steer_revision,
            progress,
            now,
        )
        .await
    }

    async fn get_agent_run(&self, id: AgentRunId) -> Result<Option<AgentRun>> {
        ops::agent_run::get_agent_run(self, id).await
    }

    async fn list_agent_runs(&self, chat_id: ChatId) -> Result<Vec<AgentRun>> {
        ops::agent_run::list_agent_runs(self, chat_id).await
    }

    async fn claim_agent_run(
        &self,
        lease_token: uuid::Uuid,
        lease_duration: chrono::Duration,
        max_running_global: u32,
        max_running_per_chat: u32,
    ) -> Result<Option<AgentRun>> {
        ops::agent_run::claim_agent_run(
            self,
            lease_token,
            lease_duration,
            max_running_global,
            max_running_per_chat,
        )
        .await
    }

    async fn heartbeat_agent_run(
        &self,
        id: AgentRunId,
        lease_token: uuid::Uuid,
        lease_duration: chrono::Duration,
    ) -> Result<bool> {
        ops::agent_run::heartbeat_agent_run(self, id, lease_token, lease_duration).await
    }

    async fn park_agent_run_for_sandbox_tool_call(
        &self,
        agent_run_id: AgentRunId,
        lease_token: uuid::Uuid,
        call: &SandboxToolCallRequest,
    ) -> Result<ParkSandboxToolCallOutcome> {
        ops::sandbox_tool::park_agent_run_for_sandbox_tool_call(
            self,
            agent_run_id,
            lease_token,
            call,
        )
        .await
    }

    async fn claim_sandbox_tool_call(
        &self,
        id: CallId,
        lease_token: uuid::Uuid,
        lease_duration: chrono::Duration,
    ) -> Result<ClaimSandboxToolCallOutcome> {
        ops::sandbox_tool::claim_sandbox_tool_call(self, id, lease_token, lease_duration).await
    }

    async fn claim_sandbox_tool_call_named(
        &self,
        id: CallId,
        name: &str,
        lease_token: uuid::Uuid,
        lease_duration: chrono::Duration,
    ) -> Result<ClaimSandboxToolCallOutcome> {
        ops::sandbox_tool::claim_sandbox_tool_call_named(
            self,
            id,
            name,
            lease_token,
            lease_duration,
        )
        .await
    }

    async fn claim_delegated_file_read(
        &self,
        id: CallId,
        lease_token: uuid::Uuid,
        lease_duration: chrono::Duration,
    ) -> Result<ClaimDelegatedFileReadOutcome> {
        ops::sandbox_tool::claim_delegated_file_read(self, id, lease_token, lease_duration).await
    }

    async fn heartbeat_delegated_file_read(
        &self,
        id: CallId,
        lease_token: uuid::Uuid,
        lease_duration: chrono::Duration,
    ) -> Result<Option<chrono::Duration>> {
        ops::sandbox_tool::heartbeat_delegated_file_read(self, id, lease_token, lease_duration)
            .await
    }

    async fn resolve_delegated_file_read(
        &self,
        id: CallId,
        lease_token: uuid::Uuid,
        resolution: &ToolCallResolution,
    ) -> Result<ResolveSandboxToolCallOutcome> {
        ops::sandbox_tool::resolve_delegated_file_read(self, id, lease_token, resolution).await
    }

    async fn heartbeat_sandbox_tool_call(
        &self,
        id: CallId,
        lease_token: uuid::Uuid,
        lease_duration: chrono::Duration,
    ) -> Result<Option<chrono::Duration>> {
        ops::sandbox_tool::heartbeat_sandbox_tool_call(self, id, lease_token, lease_duration).await
    }

    async fn resolve_sandbox_tool_call(
        &self,
        id: CallId,
        lease_token: uuid::Uuid,
        resolution: &ToolCallResolution,
    ) -> Result<ResolveSandboxToolCallOutcome> {
        ops::sandbox_tool::resolve_sandbox_tool_call(self, id, lease_token, resolution).await
    }

    async fn get_sandbox_tool_call(&self, id: CallId) -> Result<Option<SandboxToolCall>> {
        ops::sandbox_tool::get_sandbox_tool_call(self, id).await
    }

    async fn get_sandbox_tool_call_receipt(
        &self,
        id: CallId,
    ) -> Result<Option<SandboxToolCallReceipt>> {
        ops::sandbox_tool::get_sandbox_tool_call_receipt(self, id).await
    }

    async fn list_sandbox_tool_calls_for_agent_run(
        &self,
        agent_run_id: AgentRunId,
    ) -> Result<Vec<SandboxToolCall>> {
        ops::sandbox_tool::list_sandbox_tool_calls_for_agent_run(self, agent_run_id).await
    }

    async fn list_sandbox_tool_call_candidates(&self, limit: u64) -> Result<Vec<SandboxToolCall>> {
        ops::sandbox_tool::list_sandbox_tool_call_candidates(self, limit).await
    }

    async fn list_sandbox_tool_call_candidates_named(
        &self,
        name: &str,
        limit: u64,
    ) -> Result<Vec<SandboxToolCall>> {
        ops::sandbox_tool::list_sandbox_tool_call_candidates_named(self, name, limit).await
    }

    async fn request_agent_run_cancellation(
        &self,
        id: AgentRunId,
    ) -> Result<Option<RequestAgentRunCancellationOutcome>> {
        ops::agent_run::request_agent_run_cancellation(self, id).await
    }

    async fn get_agent_run_cancellation_signal(
        &self,
        id: AgentRunId,
    ) -> Result<Option<crate::model::AgentRunCancellationSignal>> {
        ops::agent_run::get_agent_run_cancellation_signal(self, id).await
    }

    async fn finish_agent_run_cancellation(
        &self,
        id: AgentRunId,
        lease_token: uuid::Uuid,
    ) -> Result<Option<FinishAgentRunCancellationOutcome>> {
        ops::agent_run::finish_agent_run_cancellation(self, id, lease_token).await
    }

    async fn submit_agent_run_result(
        &self,
        id: AgentRunId,
        lease_token: uuid::Uuid,
        text: &str,
    ) -> Result<Option<SubmitAgentRunResultOutcome>> {
        ops::agent_run::submit_agent_run_result(self, id, lease_token, text).await
    }

    async fn submit_agent_run_folder_access_proposal(
        &self,
        id: AgentRunId,
        lease_token: uuid::Uuid,
        request: &crate::RequestFolderAccessArgs,
    ) -> Result<Option<SubmitAgentRunResultOutcome>> {
        ops::agent_run::submit_agent_run_folder_access_proposal(self, id, lease_token, request)
            .await
    }

    async fn fail_agent_run(
        &self,
        id: AgentRunId,
        lease_token: uuid::Uuid,
        error_code: &str,
        error_detail: &str,
        retry_delay: chrono::Duration,
    ) -> Result<Option<FailAgentRunOutcome>> {
        ops::agent_run::fail_agent_run(self, id, lease_token, error_code, error_detail, retry_delay)
            .await
    }

    async fn list_agent_run_inbox(
        &self,
        parent_run_id: AgentRunId,
    ) -> Result<Vec<AgentRunInboxEntry>> {
        ops::agent_run::list_agent_run_inbox(self, parent_run_id).await
    }

    async fn list_agent_run_inbox_candidates(&self, limit: u64) -> Result<Vec<AgentRunInboxEntry>> {
        ops::agent_run::list_agent_run_inbox_candidates(self, limit).await
    }

    async fn list_ready_agent_run_wait_set_candidates(
        &self,
        limit: u64,
    ) -> Result<Vec<AgentRunWaitSetCandidate>> {
        ops::turn::list_ready_agent_run_wait_set_candidates(self, limit).await
    }

    async fn claim_agent_run_inbox_entry(
        &self,
        parent_run_id: AgentRunId,
        child_run_id: AgentRunId,
        lease_token: uuid::Uuid,
        lease_duration: chrono::Duration,
    ) -> Result<Option<ClaimAgentRunInboxOutcome>> {
        ops::agent_run::claim_agent_run_inbox_entry(
            self,
            parent_run_id,
            child_run_id,
            lease_token,
            lease_duration,
        )
        .await
    }

    async fn consume_agent_run_inbox_entry(
        &self,
        parent_run_id: AgentRunId,
        child_run_id: AgentRunId,
        lease_token: uuid::Uuid,
    ) -> Result<Option<ConsumeAgentRunInboxOutcome>> {
        ops::agent_run::consume_agent_run_inbox_entry(
            self,
            parent_run_id,
            child_run_id,
            lease_token,
        )
        .await
    }

    async fn consume_agent_run_inbox_entry_and_resume_turn(
        &self,
        parent_run_id: AgentRunId,
        child_run_id: AgentRunId,
        lease_token: uuid::Uuid,
    ) -> Result<Option<ConsumeAgentRunInboxAndResumeTurnOutcome>> {
        ops::agent_run::consume_agent_run_inbox_entry_and_resume_turn(
            self,
            parent_run_id,
            child_run_id,
            lease_token,
        )
        .await
    }

    async fn get_turn_run(&self, id: TurnId) -> Result<Option<TurnRun>> {
        ops::turn::get_turn_run(self, id).await
    }

    async fn list_turn_runs(&self, chat_id: ChatId) -> Result<Vec<TurnRun>> {
        ops::turn::list_turn_runs(self, chat_id).await
    }

    async fn accept_turn(
        &self,
        id: TurnId,
        chat_id: ChatId,
        model: &str,
        content: &str,
    ) -> Result<AcceptTurnOutcome> {
        ops::turn::accept_turn(self, id, chat_id, model, content, &[], &[]).await
    }

    async fn accept_turn_with_attachments(
        &self,
        id: TurnId,
        chat_id: ChatId,
        model: &str,
        content: &str,
        images: &[ImageRef],
        documents: &[DocumentId],
    ) -> Result<AcceptTurnOutcome> {
        ops::turn::accept_turn(self, id, chat_id, model, content, images, documents).await
    }

    async fn claim_turn_run(
        &self,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
        lease_expires_at: chrono::DateTime<Utc>,
    ) -> Result<ClaimTurnRunOutcome> {
        ops::turn::claim_turn_run(self, lease_token, now, lease_expires_at).await
    }

    async fn heartbeat_turn_run(
        &self,
        id: TurnId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
        lease_expires_at: chrono::DateTime<Utc>,
    ) -> Result<bool> {
        ops::turn::heartbeat_turn_run(self, id, lease_token, now, lease_expires_at).await
    }

    async fn fence_turn_lease(
        &self,
        id: TurnId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
    ) -> Result<TurnLeaseFence> {
        ops::turn::fence_turn_lease(self, id, lease_token, now).await
    }

    async fn accept_turn_steer(
        &self,
        id: TurnSteerId,
        turn_id: TurnId,
        chat_id: ChatId,
        content: &str,
        interrupt: bool,
    ) -> Result<AcceptTurnSteerOutcome> {
        ops::turn::accept_turn_steer(self, id, turn_id, chat_id, content, interrupt).await
    }

    async fn list_pending_turn_steers(
        &self,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<Vec<crate::model::TurnSteer>>> {
        ops::turn::list_pending_turn_steers(self, turn_id, lease_token, now).await
    }

    async fn apply_turn_steer(
        &self,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        steer_id: TurnSteerId,
        attempt_event_ordinal: i32,
        preceding_assistant: Option<&Message>,
        preceding_citations: &[crate::AssistantCitationInput],
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<JournaledTurnSteerOutcome>> {
        ops::turn::apply_turn_steer(
            self,
            turn_id,
            lease_token,
            steer_id,
            attempt_event_ordinal,
            preceding_assistant,
            preceding_citations,
            now,
        )
        .await
    }

    async fn complete_turn_run(
        &self,
        id: TurnId,
        lease_token: uuid::Uuid,
        expected_steer_revision: i64,
        now: chrono::DateTime<Utc>,
        output: &Message,
    ) -> Result<Option<CompleteTurnRunOutcome>> {
        ops::turn::complete_turn_run(self, id, lease_token, expected_steer_revision, now, output)
            .await
    }

    async fn complete_turn_run_and_append_event(
        &self,
        id: TurnId,
        lease_token: uuid::Uuid,
        expected_steer_revision: i64,
        now: chrono::DateTime<Utc>,
        output: &Message,
        usage: Usage,
        stop_reason: StopReason,
    ) -> Result<Option<JournaledTurnOutcome<CompleteTurnRunOutcome>>> {
        ops::turn::complete_turn_run_and_append_event(
            self,
            id,
            lease_token,
            expected_steer_revision,
            now,
            output,
            usage,
            stop_reason,
        )
        .await
    }

    async fn complete_turn_run_with_citations_and_append_event(
        &self,
        id: TurnId,
        lease_token: uuid::Uuid,
        expected_steer_revision: i64,
        now: chrono::DateTime<Utc>,
        output: &Message,
        citations: &[crate::AssistantCitationInput],
        usage: Usage,
        stop_reason: StopReason,
    ) -> Result<Option<JournaledTurnOutcome<CompleteTurnRunOutcome>>> {
        ops::turn::complete_turn_run_with_citations_and_append_event(
            self,
            id,
            lease_token,
            expected_steer_revision,
            now,
            output,
            citations,
            usage,
            stop_reason,
        )
        .await
    }

    async fn complete_refused_turn_run_with_citations_and_append_event(
        &self,
        id: TurnId,
        lease_token: uuid::Uuid,
        expected_steer_revision: i64,
        now: chrono::DateTime<Utc>,
        output: &Message,
        citations: &[crate::AssistantCitationInput],
        usage: Usage,
        refusal: crate::RefusalOutcome,
    ) -> Result<Option<JournaledTurnOutcome<CompleteTurnRunOutcome>>> {
        ops::turn::complete_refused_turn_run_with_citations_and_append_event(
            self,
            id,
            lease_token,
            expected_steer_revision,
            now,
            output,
            citations,
            usage,
            refusal,
        )
        .await
    }

    async fn record_turn_run_failure(
        &self,
        id: TurnId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
        retry: TurnFailureRetry,
        model_steps: i32,
        usage: Usage,
        error_code: &str,
        error_detail: Option<&str>,
    ) -> Result<Option<RecordTurnFailureOutcome>> {
        ops::turn::record_turn_run_failure(
            self,
            id,
            lease_token,
            now,
            retry,
            model_steps,
            usage,
            error_code,
            error_detail,
        )
        .await
    }

    async fn record_turn_run_failure_and_append_event(
        &self,
        id: TurnId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
        retry: TurnFailureRetry,
        model_steps: i32,
        usage: Usage,
        error_code: &str,
        error_detail: Option<&str>,
    ) -> Result<Option<JournaledTurnOutcome<RecordTurnFailureOutcome>>> {
        ops::turn::record_turn_run_failure_and_append_event(
            self,
            id,
            lease_token,
            now,
            retry,
            model_steps,
            usage,
            error_code,
            error_detail,
        )
        .await
    }

    async fn request_turn_cancellation(
        &self,
        id: TurnId,
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<RequestTurnCancellationOutcome>> {
        ops::turn::request_turn_cancellation(self, id, now).await
    }

    async fn request_turn_cancellation_and_append_event(
        &self,
        id: TurnId,
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<JournaledTurnOutcome<RequestTurnCancellationOutcome>>> {
        ops::turn::request_turn_cancellation_and_append_event(self, id, now).await
    }

    async fn finish_turn_cancellation(
        &self,
        id: TurnId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<FinishTurnCancellationOutcome>> {
        ops::turn::finish_turn_cancellation(self, id, lease_token, now).await
    }

    async fn finish_turn_cancellation_and_append_event(
        &self,
        id: TurnId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
        usage: Usage,
    ) -> Result<Option<JournaledTurnOutcome<FinishTurnCancellationOutcome>>> {
        ops::turn::finish_turn_cancellation_and_append_event(self, id, lease_token, now, usage)
            .await
    }

    async fn park_turn_for_client_tool_call(
        &self,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        expected_steer_revision: i64,
        progress: TurnCheckpointProgress,
        now: chrono::DateTime<Utc>,
        call: &crate::model::ClientToolCallRequest,
    ) -> Result<Option<ParkTurnForClientCallOutcome>> {
        ops::turn::park_turn_for_client_tool_call(
            self,
            turn_id,
            lease_token,
            expected_steer_revision,
            progress,
            now,
            call,
        )
        .await
    }

    async fn park_turn_for_agent_run_inbox(
        &self,
        turn_id: TurnId,
        child_run_id: AgentRunId,
        lease_token: uuid::Uuid,
        expected_steer_revision: i64,
        progress: TurnCheckpointProgress,
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<ParkTurnForAgentRunInboxOutcome>> {
        ops::turn::park_turn_for_agent_run_inbox(
            self,
            turn_id,
            child_run_id,
            lease_token,
            expected_steer_revision,
            progress,
            now,
        )
        .await
    }

    async fn park_turn_for_agent_run_wait_set(
        &self,
        request: &crate::model::AgentRunWaitSetCheckpointRequest,
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<ParkTurnForAgentRunWaitSetOutcome>> {
        ops::turn::park_turn_for_agent_run_wait_set(self, request, now).await
    }

    async fn resume_turn_for_agent_run_wait_set(
        &self,
        wait_id: CallId,
        resume_token: uuid::Uuid,
    ) -> Result<Option<ResumeTurnForAgentRunWaitSetOutcome>> {
        ops::turn::resume_turn_for_agent_run_wait_set(self, wait_id, resume_token).await
    }

    async fn append_message(&self, message: &Message) -> Result<()> {
        ops::conversation::append_message(self, message).await
    }

    async fn append_assistant_message_with_citations(
        &self,
        message: &Message,
        references: &[crate::AssistantCitationInput],
    ) -> Result<()> {
        ops::citation::append_assistant_message(self, message, references).await
    }

    async fn append_claimed_assistant_message_with_citations(
        &self,
        message: &Message,
        references: &[crate::AssistantCitationInput],
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
    ) -> Result<AppendClaimedMessageOutcome> {
        ops::citation::append_claimed_assistant_message(self, message, references, lease_token, now)
            .await
    }

    async fn list_messages(&self, chat_id: ChatId) -> Result<Vec<Message>> {
        ops::conversation::list_messages(self, chat_id).await
    }

    async fn list_message_attachments(&self, chat_id: ChatId) -> Result<Vec<MessageAttachment>> {
        ops::message_attachment::list_for_chat(self, chat_id).await
    }

    async fn list_message_document_attachments(
        &self,
        chat_id: ChatId,
    ) -> Result<Vec<MessageDocumentAttachment>> {
        ops::message_document_attachment::list_for_chat(self, chat_id).await
    }

    async fn accept_tool_call(&self, call: &ToolCallRecord) -> Result<AcceptToolCallOutcome> {
        ops::client_execution::accept_tool_call(self, call).await
    }

    async fn accept_claimed_tool_call(
        &self,
        call: &ToolCallRecord,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
    ) -> Result<AcceptClaimedToolCallOutcome> {
        ops::client_execution::accept_claimed_tool_call(self, call, lease_token, now).await
    }

    async fn request_tool_call_approval(
        &self,
        request: &ApprovalRequest,
        requested_at: chrono::DateTime<Utc>,
    ) -> Result<RequestToolApprovalOutcome> {
        ops::approval::request(self, request, requested_at).await
    }

    async fn request_tool_call_approval_and_append_event(
        &self,
        request: &ApprovalRequest,
        lease_token: uuid::Uuid,
        event_ordinal: i32,
        requested_at: chrono::DateTime<Utc>,
    ) -> Result<JournaledToolApprovalOutcome> {
        ops::approval::request_and_append_event(
            self,
            request,
            lease_token,
            event_ordinal,
            requested_at,
        )
        .await
    }

    async fn decide_tool_call_approval(
        &self,
        chat_id: ChatId,
        call_id: CallId,
        decision: &ApprovalDecision,
        decided_at: chrono::DateTime<Utc>,
    ) -> Result<DecideToolApprovalOutcome> {
        ops::approval::decide(self, chat_id, call_id, decision, decided_at).await
    }

    async fn decide_tool_call_approval_with_grant(
        &self,
        chat_id: ChatId,
        call_id: CallId,
        decision: &ApprovalDecision,
        grant: &crate::StandingGrant,
        decided_at: chrono::DateTime<Utc>,
    ) -> Result<DecideToolApprovalOutcome> {
        ops::approval::decide_with_grant(self, chat_id, call_id, decision, grant, decided_at).await
    }

    async fn get_tool_call_approval(&self, call_id: CallId) -> Result<Option<ToolApproval>> {
        ops::approval::get(self, call_id).await
    }

    async fn list_pending_tool_call_approvals(
        &self,
        chat_id: ChatId,
        limit: u64,
    ) -> Result<Vec<ToolApproval>> {
        ops::approval::list_pending(self, chat_id, limit).await
    }

    async fn list_judging_tool_call_approvals(&self, limit: u64) -> Result<Vec<ToolApproval>> {
        ops::approval::list_judging(self, limit).await
    }

    async fn resolve_tool_call_approval_from_judge(
        &self,
        chat_id: ChatId,
        call_id: CallId,
        approved: bool,
    ) -> Result<bool> {
        ops::approval::resolve_from_judge(self, chat_id, call_id, approved).await
    }

    async fn list_standing_tool_grants(&self) -> Result<Vec<crate::approval::StandingGrantRecord>> {
        ops::approval::list_standing_grants(self).await
    }

    async fn revoke_standing_tool_grant(&self, source_call_id: CallId) -> Result<bool> {
        ops::approval::revoke_standing_grant(self, source_call_id).await
    }

    async fn claim_client_tool_call(
        &self,
        id: CallId,
        chat_id: ChatId,
        executor_id: uuid::Uuid,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
        lease_expires_at: chrono::DateTime<Utc>,
    ) -> Result<ClaimClientToolCallOutcome> {
        ops::client_execution::claim_client_tool_call(
            self,
            id,
            chat_id,
            executor_id,
            lease_token,
            now,
            lease_expires_at,
        )
        .await
    }

    async fn heartbeat_client_tool_call(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
        lease_expires_at: chrono::DateTime<Utc>,
    ) -> Result<HeartbeatClientToolCallOutcome> {
        ops::client_execution::heartbeat_client_tool_call(
            self,
            id,
            chat_id,
            lease_token,
            now,
            lease_expires_at,
        )
        .await
    }

    async fn resolve_server_tool_call(
        &self,
        id: CallId,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<Utc>,
    ) -> Result<ResolveToolCallOutcome> {
        ops::client_execution::resolve_server_tool_call(self, id, resolution, resolved_at).await
    }

    async fn resolve_server_tool_call_with_artifacts(
        &self,
        id: CallId,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<Utc>,
        preview: Option<&crate::ToolResultPreview>,
    ) -> Result<ResolveToolCallOutcome> {
        ops::client_execution::resolve_server_tool_call_with_preview(
            self,
            id,
            resolution,
            resolved_at,
            preview,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn resolve_claimed_server_tool_call_with_artifacts(
        &self,
        id: CallId,
        chat_id: ChatId,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<Utc>,
        preview: Option<&crate::ToolResultPreview>,
    ) -> Result<ResolveToolCallOutcome> {
        ops::client_execution::resolve_claimed_server_tool_call(
            self,
            id,
            chat_id,
            turn_id,
            lease_token,
            now,
            resolution,
            resolved_at,
            preview,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn resolve_claimed_server_tool_call(
        &self,
        id: CallId,
        chat_id: ChatId,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<Utc>,
    ) -> Result<ResolveToolCallOutcome> {
        ops::client_execution::resolve_claimed_server_tool_call(
            self,
            id,
            chat_id,
            turn_id,
            lease_token,
            now,
            resolution,
            resolved_at,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn abandon_inherited_server_tool_call(
        &self,
        id: CallId,
        chat_id: ChatId,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<Utc>,
    ) -> Result<ResolveToolCallOutcome> {
        ops::client_execution::abandon_inherited_server_tool_call(
            self,
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

    async fn resolve_client_tool_call_and_append_event(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<Utc>,
    ) -> Result<JournaledClientToolCallOutcome> {
        ops::client_execution::resolve_client_tool_call_and_append_event(
            self,
            id,
            chat_id,
            lease_token,
            now,
            resolution,
            resolved_at,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn resolve_client_tool_call_and_append_event_with_rows(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<Utc>,
        rows: Option<&serde_json::Value>,
    ) -> Result<JournaledClientToolCallOutcome> {
        ops::client_execution::resolve_client_tool_call_and_append_event(
            self,
            id,
            chat_id,
            lease_token,
            now,
            resolution,
            resolved_at,
            rows,
        )
        .await
    }

    async fn resolve_expired_client_tool_call_and_append_event(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<Utc>,
    ) -> Result<JournaledClientToolCallOutcome> {
        ops::client_execution::resolve_expired_client_tool_call_and_append_event(
            self,
            id,
            chat_id,
            lease_token,
            now,
            resolution,
            resolved_at,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn resolve_expired_client_tool_call_and_append_event_with_rows(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<Utc>,
        rows: Option<&serde_json::Value>,
    ) -> Result<JournaledClientToolCallOutcome> {
        ops::client_execution::resolve_expired_client_tool_call_and_append_event(
            self,
            id,
            chat_id,
            lease_token,
            now,
            resolution,
            resolved_at,
            rows,
        )
        .await
    }

    async fn list_pending_client_tool_calls(&self, chat_id: ChatId) -> Result<Vec<ToolCallRecord>> {
        ops::client_execution::list_pending_client_tool_calls(self, chat_id).await
    }

    async fn list_pending_user_questions(
        &self,
        chat_id: ChatId,
    ) -> Result<Vec<crate::PendingUserQuestions>> {
        ops::user_question::list_pending(self, chat_id).await
    }

    async fn list_pending_chat_prompts(&self) -> Result<Vec<crate::PendingChatPrompt>> {
        ops::chat_prompt::list_pending_chat_prompts(self).await
    }

    async fn answer_user_questions(
        &self,
        request: &crate::AnswerUserQuestionsRequest,
        answered_at: chrono::DateTime<Utc>,
    ) -> Result<crate::AnswerUserQuestionsOutcome> {
        ops::user_question::answer(self, request, answered_at).await
    }

    async fn list_pending_plan_approvals(
        &self,
        chat_id: ChatId,
    ) -> Result<Vec<crate::PendingPlanApproval>> {
        ops::plan::list_pending(self, chat_id).await
    }

    async fn decide_plan(
        &self,
        request: &crate::DecidePlanRequest,
        decided_at: chrono::DateTime<Utc>,
    ) -> Result<crate::storage::DecidePlanOutcome> {
        ops::plan::decide(self, request, decided_at).await
    }

    async fn list_tool_calls(&self, chat_id: ChatId) -> Result<Vec<ToolCallRecord>> {
        ops::conversation::list_tool_calls(self, chat_id).await
    }

    async fn get_setting(&self, key: &str) -> Result<Option<Value>> {
        Ok(entities::setting::Entity::find_by_id(key.to_string())
            .one(&self.conn)
            .await
            .map_err(store_err)?
            .map(|model| model.value_json))
    }

    async fn set_setting(&self, key: &str, value: &Value) -> Result<()> {
        let model = entities::setting::ActiveModel {
            key: Set(key.to_string()),
            value_json: Set(value.clone()),
        };
        entities::setting::Entity::insert(model)
            .on_conflict(
                OnConflict::column(entities::setting::Column::Key)
                    .update_column(entities::setting::Column::ValueJson)
                    .to_owned(),
            )
            .exec(&self.conn)
            .await
            .map_err(store_err)?;
        Ok(())
    }

    async fn append_event(&self, chat_id: ChatId, event: &AgentEvent) -> Result<i64> {
        ops::conversation::append_event(self, chat_id, event).await
    }

    async fn append_turn_event(
        &self,
        chat_id: ChatId,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        attempt_event_ordinal: i32,
        now: chrono::DateTime<Utc>,
        event: &AgentEvent,
    ) -> Result<Option<i64>> {
        ops::conversation::append_turn_event(
            self,
            chat_id,
            turn_id,
            lease_token,
            attempt_event_ordinal,
            now,
            event,
        )
        .await
    }

    async fn recover_exact_turn_terminal_event(
        &self,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        event: &AgentEvent,
    ) -> Result<Option<SequencedEvent>> {
        ops::turn::recover_exact_terminal_event(self, turn_id, lease_token, event).await
    }

    async fn recover_exact_completed_turn_event(
        &self,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        output: &Message,
        citations: &[crate::AssistantCitationInput],
        event: &AgentEvent,
    ) -> Result<Option<SequencedEvent>> {
        ops::turn::recover_exact_completed_turn_event(
            self,
            turn_id,
            lease_token,
            output,
            citations,
            event,
        )
        .await
    }

    async fn list_events(&self, chat_id: ChatId, after: i64) -> Result<Vec<SequencedEvent>> {
        ops::conversation::list_events(self, chat_id, after).await
    }

    async fn claim_operation(
        &self,
        run_id: uuid::Uuid,
        operation_id: uuid::Uuid,
        fingerprint: &[u8],
        external_effect: bool,
        owner_epoch: uuid::Uuid,
    ) -> Result<OperationClaimOutcome> {
        ops::operation_log::claim(
            self,
            run_id,
            operation_id,
            fingerprint,
            external_effect,
            owner_epoch,
        )
        .await
    }

    async fn record_operation(
        &self,
        run_id: uuid::Uuid,
        operation_id: uuid::Uuid,
        body: &[u8],
    ) -> Result<OperationLogWrite> {
        ops::operation_log::record(self, run_id, operation_id, body).await
    }

    async fn fail_operation(
        &self,
        run_id: uuid::Uuid,
        operation_id: uuid::Uuid,
        body: &[u8],
    ) -> Result<OperationLogWrite> {
        ops::operation_log::fail(self, run_id, operation_id, body).await
    }

    async fn operation_state(
        &self,
        run_id: uuid::Uuid,
        operation_id: uuid::Uuid,
    ) -> Result<Option<OperationLogEntry>> {
        ops::operation_log::state(self, run_id, operation_id).await
    }

    async fn evict_operation(&self, run_id: uuid::Uuid, operation_id: uuid::Uuid) -> Result<()> {
        ops::operation_log::evict(self, run_id, operation_id).await
    }

    async fn operation_log_len(&self, run_id: uuid::Uuid) -> Result<usize> {
        ops::operation_log::len(self, run_id).await
    }
}
async fn upsert_document_on<C>(conn: &C, document: &DocumentUpsert) -> Result<DocumentRecord>
where
    C: ConnectionTrait,
{
    let existing = entities::document::Entity::find_by_id(document.id.0)
        .one(conn)
        .await
        .map_err(store_err)?;
    if let Some(existing) = existing.as_ref() {
        if existing.source_blob_id.is_some()
            || existing.source_sha256.is_some()
            || existing.source_byte_len.is_some()
        {
            return Err(AgentError::Store(
                "raw-source documents require the synchronous source workflow".into(),
            ));
        }
        if existing.chat_id != document.chat_id.map(|id| id.0)
            || existing.project_id != document.project_id.map(|id| id.0)
        {
            return Err(AgentError::Store(format!(
                "document {} cannot move between document corpora",
                document.id
            )));
        }
    }

    let active = entities::document::ActiveModel {
        id: Set(document.id.0),
        chat_id: Set(document.chat_id.map(|id| id.0)),
        project_id: Set(document.project_id.map(|id| id.0)),
        source_uri: Set(document.source_uri.clone()),
        media_type: Set(document.media_type.clone()),
        title: Set(document.title.clone()),
        source_blob_id: Set(None),
        source_sha256: Set(None),
        source_byte_len: Set(None),
        canonical_text: Set(document.canonical_text.clone()),
        created_at: Set(existing
            .as_ref()
            .map_or(document.updated_at, |current| current.created_at)),
        updated_at: Set(document.updated_at),
    };
    if existing.is_some() {
        active.update(conn).await.map_err(store_err)?;
    } else {
        active.insert(conn).await.map_err(store_err)?;
    }

    entities::document::Entity::find_by_id(document.id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("upserted document disappeared".into()))
        .and_then(document_from_model)
}

pub(in crate::db) fn project_from_models(
    model: entities::project::Model,
    roots: Vec<entities::project_root_attachment::Model>,
) -> Result<Project> {
    let root_attachments = hydrate_project_root_attachments(ProjectId(model.id), roots)?;
    let project = Project {
        id: ProjectId(model.id),
        title: model.title,
        attachment_revision: model.attachment_revision,
        root_attachments,
        created_at: model.created_at,
    };
    validate_project_attachments(&project)?;
    Ok(project)
}

fn hydrate_project_root_attachments(
    project_id: ProjectId,
    rows: Vec<entities::project_root_attachment::Model>,
) -> Result<Vec<HostRootId>> {
    if rows.len() > MAX_ROOT_ATTACHMENTS {
        return Err(AgentError::Store(format!(
            "project {project_id} exceeds the root attachment limit"
        )));
    }
    rows.into_iter()
        .enumerate()
        .map(|(expected, row)| {
            if usize::try_from(row.position).ok() != Some(expected) {
                return Err(AgentError::Store(format!(
                    "project {project_id} root positions are not contiguous"
                )));
            }
            HostRootId::from_uuid(row.root_id).map_err(|error| {
                AgentError::Store(format!(
                    "project {project_id} has an invalid root id: {error}"
                ))
            })
        })
        .collect()
}

fn validate_project_attachments(project: &Project) -> Result<()> {
    validate_project_root_projection(project).map_err(|message| AgentError::Store(message.into()))
}

fn validate_document_source_blob(blob: &DocumentSourceBlob) -> Result<i64> {
    if !blob.has_content_addressed_id() {
        return Err(AgentError::Store(
            "document source blob id does not match its SHA-256 digest".into(),
        ));
    }
    i64::try_from(blob.byte_len)
        .map_err(|_| AgentError::Store("document source is too large".into()))
}

fn source_blob_from_model(
    id: Option<uuid::Uuid>,
    sha256: Option<Vec<u8>>,
    byte_len: Option<i64>,
) -> Result<Option<DocumentSourceBlob>> {
    match (id, sha256, byte_len) {
        (None, None, None) => Ok(None),
        (Some(id), Some(sha256), Some(byte_len)) => {
            let sha256: [u8; 32] = sha256.try_into().map_err(|_| {
                AgentError::Store("stored document source digest must contain 32 bytes".into())
            })?;
            let byte_len = u64::try_from(byte_len).map_err(|_| {
                AgentError::Store("stored document source length must be nonnegative".into())
            })?;
            let blob = DocumentSourceBlob {
                id,
                sha256,
                byte_len,
            };
            if !blob.has_content_addressed_id() {
                return Err(AgentError::Store(
                    "stored document source blob id does not match its SHA-256 digest".into(),
                ));
            }
            Ok(Some(blob))
        }
        _ => Err(AgentError::Store(
            "stored document source descriptor is incomplete".into(),
        )),
    }
}

fn document_from_model(model: entities::document::Model) -> Result<DocumentRecord> {
    Ok(DocumentRecord {
        id: DocumentId(model.id),
        chat_id: model.chat_id.map(ChatId),
        project_id: model.project_id.map(ProjectId),
        source_uri: model.source_uri,
        media_type: model.media_type,
        title: model.title,
        source_blob: source_blob_from_model(
            model.source_blob_id,
            model.source_sha256,
            model.source_byte_len,
        )?,
        canonical_text: model.canonical_text,
        created_at: model.created_at,
        updated_at: model.updated_at,
    })
}

fn document_summary_from_row(row: DocumentSummaryRow) -> Result<DocumentSummaryRecord> {
    Ok(DocumentSummaryRecord {
        id: DocumentId(row.id),
        chat_id: row.chat_id.map(ChatId),
        project_id: row.project_id.map(ProjectId),
        source_uri: row.source_uri,
        media_type: row.media_type,
        title: row.title,
        source_byte_len: row
            .source_byte_len
            .map(u64::try_from)
            .transpose()
            .map_err(|_| {
                AgentError::Store("stored document source length must be nonnegative".into())
            })?,
        readable: row.has_canonical_text,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// SeaORM entity models. Kept internal — the public `Store` API speaks the domain
/// types (`Chat`, `Message`), never these, so the ORM never leaks into the
/// crate's contract.
mod entities;

/// Schema v1, defined once via SeaORM's schema builder; it emits dialect-correct
/// DDL for whichever backend is connected.
mod migration;

#[cfg(test)]
mod tests;
