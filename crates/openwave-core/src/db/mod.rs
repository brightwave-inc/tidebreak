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
    FromQueryResult, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait, TryInsertResult,
};
use sea_orm_migration::MigratorTrait;
use serde_json::Value;

use crate::approval::{ApprovalDecision, ApprovalRequest, ToolApproval};
use crate::deliverable::{
    CreateOutput, NewOutputRevision, OutputCitationSnapshot, OutputRecord, OutputRevision,
};
use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
#[cfg(test)]
use crate::id::MessageId;
use crate::id::{
    AgentRunId, CallId, ChatId, DocumentId, DocumentJobId, HostRootId, OutputId, OutputRevisionId,
    ProjectId, RootAttachmentChangeId, TurnId, TurnSteerId,
};
use crate::image::ImageRef;
#[cfg(test)]
use crate::model::Role;
use crate::model::{
    validate_project_root_projection, AgentRun, AgentRunInboxEntry, AgentRunStatus, AgentRunTier,
    AgentRunWaitSetCandidate, BeginRootAttachmentChange, BlobRetirement, BlobRetirementStatus,
    Chat, DocumentGeneration, DocumentJob, DocumentJobKind, DocumentJobStatus, DocumentListCursor,
    DocumentParseOutput, DocumentProcessingStatus, DocumentRecord, DocumentScope,
    DocumentSourceBlob, DocumentSourceUpsert, DocumentSummaryRecord, DocumentUpsert, Message,
    MessageAttachment, PermissionMode, Project, ReasoningEffort, RootAttachmentChange,
    RootAttachmentChangeTerminal, SandboxToolCall, SandboxToolCallReceipt, SandboxToolCallRequest,
    SourceRegion, ToolCallRecord, ToolCallResolution, TurnAgentRunWaitStatus,
    TurnCheckpointProgress, TurnClientWaitStatus, TurnFailureRetry, TurnRun, TurnRunStatus,
    TurnSteerStatus, MAX_ROOT_ATTACHMENTS,
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
    DecideToolApprovalOutcome, DeleteChatOutcome, DeleteProjectOutcome, DocumentIndexJobReason,
    EnsureDocumentIndexJobOutcome, EnsureDocumentParseJobOutcome, FailAgentRunOutcome,
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
/// canonical text or revision token by accident.
#[derive(Debug, FromQueryResult)]
struct DocumentSummaryRow {
    id: uuid::Uuid,
    chat_id: Option<uuid::Uuid>,
    project_id: Option<uuid::Uuid>,
    source_uri: Option<String>,
    media_type: String,
    title: Option<String>,
    source_byte_len: Option<i64>,
    content_revision: i64,
    processing_status: String,
    /// Emptiness of the canonical text, evaluated in the database so a listing
    /// still never transfers the text itself.
    has_canonical_text: bool,
    indexed_revision: Option<i64>,
    index_fingerprint: Option<String>,
    created_at: chrono::DateTime<Utc>,
    updated_at: chrono::DateTime<Utc>,
    indexed_at: Option<chrono::DateTime<Utc>>,
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
        validate_document_source_regions(&document.canonical_text, &document.source_regions)?;
        let source_byte_len = document
            .source_blob
            .as_ref()
            .map(validate_document_source_blob)
            .transpose()?;
        let transaction = self.conn.begin().await.map_err(store_err)?;
        ops::require_document_scope_write_lock(&transaction, document.chat_id, document.project_id)
            .await?;
        acquire_document_write_lock(&transaction, document.id).await?;
        let revision_token = uuid::Uuid::new_v4();
        entities::document_generation::ActiveModel {
            document_id: Set(document.id.0),
            content_revision: Set(document.content_revision),
            revision_token: Set(revision_token),
            tombstone: Set(false),
            retirement_pending: Set(false),
            retirement_content_revision: Set(None),
            retirement_revision_token: Set(None),
        }
        .insert(&transaction)
        .await
        .map_err(store_err)?;
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
            canonical_fingerprint: Set(document.canonical_fingerprint.clone()),
            source_regions: Set(source_regions_to_db(&document.source_regions)),
            content_revision: Set(document.content_revision),
            revision_token: Set(revision_token),
            processing_status: Set(document.processing_status.as_str().into()),
            indexed_revision: Set(document.indexed_revision),
            index_fingerprint: Set(document.index_fingerprint.clone()),
            created_at: Set(document.created_at),
            updated_at: Set(document.updated_at),
            indexed_at: Set(document.indexed_at),
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
                entities::document::Column::ContentRevision,
                entities::document::Column::ProcessingStatus,
                entities::document::Column::IndexedRevision,
                entities::document::Column::IndexFingerprint,
                entities::document::Column::CreatedAt,
                entities::document::Column::UpdatedAt,
                entities::document::Column::IndexedAt,
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

    async fn get_document_generation(&self, id: DocumentId) -> Result<Option<DocumentGeneration>> {
        Ok(entities::document_generation::Entity::find_by_id(id.0)
            .one(&self.conn)
            .await
            .map_err(store_err)?
            .map(|generation| DocumentGeneration {
                content_revision: generation.content_revision,
                revision_token: generation.revision_token,
            }))
    }

    async fn list_pending_document_retirements(
        &self,
        after: Option<DocumentId>,
        limit: u64,
    ) -> Result<Vec<(DocumentId, DocumentGeneration)>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut query = entities::document_generation::Entity::find()
            .select_only()
            .column(entities::document_generation::Column::DocumentId)
            .column(entities::document_generation::Column::RetirementContentRevision)
            .column(entities::document_generation::Column::RetirementRevisionToken)
            .filter(entities::document_generation::Column::RetirementPending.eq(true));
        if let Some(after) = after {
            query = query.filter(entities::document_generation::Column::DocumentId.gt(after.0));
        }
        Ok(query
            .order_by_asc(entities::document_generation::Column::DocumentId)
            .limit(limit)
            .into_tuple::<(uuid::Uuid, i64, uuid::Uuid)>()
            .all(&self.conn)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(|(id, content_revision, revision_token)| {
                (
                    DocumentId(id),
                    DocumentGeneration {
                        content_revision,
                        revision_token,
                    },
                )
            })
            .collect())
    }

    async fn complete_document_retirement(
        &self,
        id: DocumentId,
        generation: DocumentGeneration,
    ) -> Result<bool> {
        let updated = entities::document_generation::Entity::update_many()
            .col_expr(
                entities::document_generation::Column::RetirementPending,
                sea_orm::sea_query::Expr::value(false),
            )
            .col_expr(
                entities::document_generation::Column::RetirementContentRevision,
                sea_orm::sea_query::Expr::value(Option::<i64>::None),
            )
            .col_expr(
                entities::document_generation::Column::RetirementRevisionToken,
                sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
            )
            .filter(entities::document_generation::Column::DocumentId.eq(id.0))
            .filter(
                entities::document_generation::Column::RetirementContentRevision
                    .eq(generation.content_revision),
            )
            .filter(
                entities::document_generation::Column::RetirementRevisionToken
                    .eq(generation.revision_token),
            )
            .filter(entities::document_generation::Column::RetirementPending.eq(true))
            .exec(&self.conn)
            .await
            .map_err(store_err)?;
        Ok(updated.rows_affected == 1)
    }

    async fn get_pending_document_retirement(
        &self,
        id: DocumentId,
    ) -> Result<Option<DocumentGeneration>> {
        let Some(generation) = entities::document_generation::Entity::find_by_id(id.0)
            .one(&self.conn)
            .await
            .map_err(store_err)?
        else {
            return Ok(None);
        };
        if !generation.retirement_pending {
            return Ok(None);
        }
        let (Some(content_revision), Some(revision_token)) = (
            generation.retirement_content_revision,
            generation.retirement_revision_token,
        ) else {
            return Err(AgentError::Store(format!(
                "document {id} has an incomplete pending retirement watermark"
            )));
        };
        Ok(Some(DocumentGeneration {
            content_revision,
            revision_token,
        }))
    }

    async fn delete_document(&self, id: DocumentId) -> Result<DocumentGeneration> {
        loop {
            let transaction = self.conn.begin().await.map_err(store_err)?;
            acquire_document_write_lock(&transaction, id).await?;
            let document = entities::document::Entity::find_by_id(id.0)
                .one(&transaction)
                .await
                .map_err(store_err)?;
            let retained = entities::document_generation::Entity::find_by_id(id.0)
                .one(&transaction)
                .await
                .map_err(store_err)?;
            if document.is_none() && retained.as_ref().is_some_and(|row| row.tombstone) {
                let retained = retained.unwrap();
                transaction.commit().await.map_err(store_err)?;
                return Ok(DocumentGeneration {
                    content_revision: retained.content_revision,
                    revision_token: retained.revision_token,
                });
            }
            if let Some(document) = document.as_ref() {
                ensure_live_document_generation_on(&transaction, document).await?;
            }

            let Some(advanced) = try_advance_document_generation_on(&transaction, id, true).await?
            else {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            };
            if let Some(document) = document {
                let live_generation = DocumentGeneration {
                    content_revision: document.content_revision,
                    revision_token: document.revision_token,
                };
                if advanced.previous != Some(live_generation) {
                    return Err(AgentError::Store(format!(
                        "document {id} does not match its retained generation clock"
                    )));
                }
                let deleted = entities::document::Entity::delete_many()
                    .filter(entities::document::Column::Id.eq(id.0))
                    .filter(
                        entities::document::Column::ContentRevision.eq(document.content_revision),
                    )
                    .filter(entities::document::Column::RevisionToken.eq(document.revision_token))
                    .exec(&transaction)
                    .await
                    .map_err(store_err)?;
                if deleted.rows_affected != 1 {
                    transaction.rollback().await.map_err(store_err)?;
                    continue;
                }
                if let Some(blob_id) = document.source_blob_id {
                    ops::blob::enqueue_on(&transaction, blob_id).await?;
                }
            }
            transaction.commit().await.map_err(store_err)?;
            return Ok(advanced.current);
        }
    }

    async fn upsert_document(&self, document: &DocumentUpsert) -> Result<DocumentRecord> {
        validate_document_source_regions(&document.canonical_text, &document.source_regions)?;
        loop {
            let transaction = self.conn.begin().await.map_err(store_err)?;
            ops::require_document_scope_write_lock(
                &transaction,
                document.chat_id,
                document.project_id,
            )
            .await?;
            acquire_document_write_lock(&transaction, document.id).await?;
            match try_upsert_document_on(&transaction, document).await? {
                Some(record) => {
                    transaction.commit().await.map_err(store_err)?;
                    return Ok(record);
                }
                None => {
                    transaction.rollback().await.map_err(store_err)?;
                }
            }
        }
    }

    async fn upsert_document_and_enqueue_index(
        &self,
        document: &DocumentUpsert,
        pipeline_fingerprint: &str,
        max_attempts: i32,
    ) -> Result<(DocumentRecord, DocumentJob)> {
        validate_document_source_regions(&document.canonical_text, &document.source_regions)?;
        if pipeline_fingerprint.is_empty()
            || pipeline_fingerprint.chars().count() > DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN
        {
            return Err(AgentError::Store(
                "document job pipeline fingerprint must contain 1 to 512 characters".into(),
            ));
        }
        if max_attempts < 1 {
            return Err(AgentError::Store(
                "document job max_attempts must be at least one".into(),
            ));
        }

        loop {
            let transaction = self.conn.begin().await.map_err(store_err)?;
            ops::require_document_scope_write_lock(
                &transaction,
                document.chat_id,
                document.project_id,
            )
            .await?;
            acquire_document_write_lock(&transaction, document.id).await?;
            if let Some(current) = entities::document::Entity::find_by_id(document.id.0)
                .one(&transaction)
                .await
                .map_err(store_err)?
                .filter(|current| document_upsert_matches(current, document))
            {
                if let Some(job) = entities::document_job::Entity::find()
                    .filter(entities::document_job::Column::DocumentId.eq(current.id))
                    .filter(
                        entities::document_job::Column::ContentRevision
                            .eq(current.content_revision),
                    )
                    .filter(
                        entities::document_job::Column::RevisionToken.eq(current.revision_token),
                    )
                    .filter(
                        entities::document_job::Column::Kind.eq(DocumentJobKind::Index.as_str()),
                    )
                    .filter(
                        entities::document_job::Column::PipelineFingerprint
                            .eq(pipeline_fingerprint),
                    )
                    .one(&transaction)
                    .await
                    .map_err(store_err)?
                {
                    ensure_live_document_generation_on(&transaction, &current).await?;
                    let current = document_from_model(current)?;
                    let job = document_job_from_model(job)?;
                    transaction.commit().await.map_err(store_err)?;
                    return Ok((current, job));
                }
            }

            let record = match try_upsert_document_on(&transaction, document).await? {
                Some(record) => record,
                None => {
                    transaction.rollback().await.map_err(store_err)?;
                    continue;
                }
            };
            let workflow_now = Utc::now();
            entities::document_job::Entity::update_many()
                .col_expr(
                    entities::document_job::Column::Status,
                    sea_orm::sea_query::Expr::value(DocumentJobStatus::Cancelled.as_str()),
                )
                .col_expr(
                    entities::document_job::Column::LeaseToken,
                    sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
                )
                .col_expr(
                    entities::document_job::Column::LeaseExpiresAt,
                    sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
                )
                .col_expr(
                    entities::document_job::Column::FinishedAt,
                    sea_orm::sea_query::Expr::value(Some(workflow_now)),
                )
                .col_expr(
                    entities::document_job::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(workflow_now),
                )
                .filter(entities::document_job::Column::DocumentId.eq(record.id.0))
                .filter(entities::document_job::Column::Status.is_in([
                    DocumentJobStatus::Queued.as_str(),
                    DocumentJobStatus::Running.as_str(),
                    DocumentJobStatus::RetryWait.as_str(),
                ]))
                .exec(&transaction)
                .await
                .map_err(store_err)?;

            let job = DocumentJob {
                id: DocumentJobId::new(),
                document_id: record.id,
                content_revision: record.content_revision,
                revision_token: record.revision_token,
                kind: DocumentJobKind::Index,
                status: DocumentJobStatus::Queued,
                pipeline_fingerprint: pipeline_fingerprint.into(),
                attempt_count: 0,
                max_attempts,
                available_at: workflow_now,
                lease_token: None,
                lease_expires_at: None,
                started_at: None,
                finished_at: None,
                last_error_code: None,
                last_error_detail: None,
                created_at: workflow_now,
                updated_at: workflow_now,
            };
            document_job_active_model(&job)
                .insert(&transaction)
                .await
                .map_err(store_err)?;
            transaction.commit().await.map_err(store_err)?;
            return Ok((record, job));
        }
    }

    async fn accept_document_source_and_enqueue_parse(
        &self,
        document: &DocumentSourceUpsert,
        parser_fingerprint: &str,
        max_attempts: i32,
    ) -> Result<(DocumentRecord, DocumentJob)> {
        ops::document::accept_source_and_enqueue_parse(
            self,
            document,
            parser_fingerprint,
            max_attempts,
        )
        .await
    }

    async fn complete_document_parse_job_and_enqueue_index(
        &self,
        id: DocumentJobId,
        lease_token: uuid::Uuid,
        completed_at: chrono::DateTime<Utc>,
        output: &DocumentParseOutput,
        index_fingerprint: &str,
        index_max_attempts: i32,
    ) -> Result<Option<(DocumentRecord, DocumentJob)>> {
        ops::document::complete_parse_and_enqueue_index(
            self,
            id,
            lease_token,
            completed_at,
            output,
            index_fingerprint,
            index_max_attempts,
        )
        .await
    }

    async fn get_document_job(&self, id: DocumentJobId) -> Result<Option<DocumentJob>> {
        entities::document_job::Entity::find_by_id(id.0)
            .one(&self.conn)
            .await
            .map_err(store_err)?
            .map(document_job_from_model)
            .transpose()
    }

    async fn ensure_document_index_job(
        &self,
        document_id: DocumentId,
        expected_generation: DocumentGeneration,
        pipeline_fingerprint: &str,
        max_attempts: i32,
        reason: DocumentIndexJobReason,
    ) -> Result<EnsureDocumentIndexJobOutcome> {
        if pipeline_fingerprint.is_empty()
            || pipeline_fingerprint.chars().count() > DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN
        {
            return Err(AgentError::Store(
                "document job pipeline fingerprint must contain 1 to 512 characters".into(),
            ));
        }
        if max_attempts < 1 {
            return Err(AgentError::Store(
                "document job max_attempts must be at least one".into(),
            ));
        }

        loop {
            let transaction = self.conn.begin().await.map_err(store_err)?;
            acquire_document_write_lock(&transaction, document_id).await?;
            let Some(document) = entities::document::Entity::find_by_id(document_id.0)
                .one(&transaction)
                .await
                .map_err(store_err)?
            else {
                transaction.commit().await.map_err(store_err)?;
                return Ok(EnsureDocumentIndexJobOutcome::MissingDocument);
            };
            ensure_live_document_generation_on(&transaction, &document).await?;
            let current_generation = DocumentGeneration {
                content_revision: document.content_revision,
                revision_token: document.revision_token,
            };

            if current_generation != expected_generation {
                if reason.advances_generation()
                    && expected_generation
                        .content_revision
                        .checked_add(1)
                        .is_some_and(|revision| revision == document.content_revision)
                {
                    if let Some(job) = find_exact_document_index_job_on(
                        &transaction,
                        document_id,
                        current_generation,
                        pipeline_fingerprint,
                    )
                    .await?
                    {
                        let job = document_job_from_model(job)?;
                        transaction.commit().await.map_err(store_err)?;
                        return Ok(if job.status == DocumentJobStatus::Failed {
                            EnsureDocumentIndexJobOutcome::Failed(job)
                        } else {
                            EnsureDocumentIndexJobOutcome::Existing(job)
                        });
                    }
                }
                transaction.commit().await.map_err(store_err)?;
                return Ok(EnsureDocumentIndexJobOutcome::GenerationChanged(
                    current_generation,
                ));
            }

            if document.source_blob_id.is_some() && document.canonical_fingerprint.is_none() {
                let parse_job = entities::document_job::Entity::find()
                    .filter(entities::document_job::Column::DocumentId.eq(document_id.0))
                    .filter(
                        entities::document_job::Column::ContentRevision
                            .eq(current_generation.content_revision),
                    )
                    .filter(
                        entities::document_job::Column::RevisionToken
                            .eq(current_generation.revision_token),
                    )
                    .filter(
                        entities::document_job::Column::Kind.eq(DocumentJobKind::Parse.as_str()),
                    )
                    .order_by_desc(entities::document_job::Column::CreatedAt)
                    .order_by_desc(entities::document_job::Column::Id)
                    .one(&transaction)
                    .await
                    .map_err(store_err)?
                    .ok_or_else(|| {
                        AgentError::Store(format!(
                            "unparsed document {document_id} has no current parse job"
                        ))
                    })?;
                let parse_job = document_job_from_model(parse_job)?;
                let outcome = if parse_job.status == DocumentJobStatus::Failed {
                    EnsureDocumentIndexJobOutcome::Failed(parse_job)
                } else if matches!(
                    parse_job.status,
                    DocumentJobStatus::Queued
                        | DocumentJobStatus::Running
                        | DocumentJobStatus::RetryWait
                ) {
                    EnsureDocumentIndexJobOutcome::Parsing(parse_job)
                } else {
                    return Err(AgentError::Store(format!(
                        "unparsed document {document_id} has terminal parse state {}",
                        parse_job.status.as_str()
                    )));
                };
                transaction.commit().await.map_err(store_err)?;
                return Ok(outcome);
            }

            if let Some(candidate) = find_exact_document_index_job_on(
                &transaction,
                document_id,
                current_generation,
                pipeline_fingerprint,
            )
            .await?
            {
                let parsed = document_job_from_model(candidate.clone())?;
                if matches!(
                    parsed.status,
                    DocumentJobStatus::Queued
                        | DocumentJobStatus::Running
                        | DocumentJobStatus::RetryWait
                ) || (reason == DocumentIndexJobReason::PipelineChanged
                    && parsed.status == DocumentJobStatus::Succeeded)
                {
                    transaction.commit().await.map_err(store_err)?;
                    return Ok(EnsureDocumentIndexJobOutcome::Existing(parsed));
                }
                if parsed.status == DocumentJobStatus::Failed {
                    transaction.commit().await.map_err(store_err)?;
                    return Ok(EnsureDocumentIndexJobOutcome::Failed(parsed));
                }
                if reason == DocumentIndexJobReason::DerivedStateMissing {
                    let now = Utc::now();
                    reset_document_index_job_on(
                        &transaction,
                        candidate.id,
                        &candidate.status,
                        max_attempts,
                        now,
                    )
                    .await?;
                    clear_document_index_watermark_on(
                        &transaction,
                        document_id,
                        current_generation,
                    )
                    .await?;
                    let job = entities::document_job::Entity::find_by_id(candidate.id)
                        .one(&transaction)
                        .await
                        .map_err(store_err)?
                        .ok_or_else(|| {
                            AgentError::Store("reset document job disappeared".into())
                        })?;
                    let job = document_job_from_model(job)?;
                    transaction.commit().await.map_err(store_err)?;
                    return Ok(EnsureDocumentIndexJobOutcome::Enqueued(job));
                }
            }

            let target_generation = if reason.advances_generation() {
                let Some(advanced) =
                    try_advance_document_generation_on(&transaction, document_id, false).await?
                else {
                    transaction.rollback().await.map_err(store_err)?;
                    continue;
                };
                if advanced.previous != Some(current_generation) {
                    return Err(AgentError::Store(format!(
                        "document {document_id} does not match its retained generation clock"
                    )));
                }
                let updated = entities::document::Entity::update_many()
                    .col_expr(
                        entities::document::Column::ContentRevision,
                        sea_orm::sea_query::Expr::value(advanced.current.content_revision),
                    )
                    .col_expr(
                        entities::document::Column::RevisionToken,
                        sea_orm::sea_query::Expr::value(advanced.current.revision_token),
                    )
                    .col_expr(
                        entities::document::Column::ProcessingStatus,
                        sea_orm::sea_query::Expr::value(DocumentProcessingStatus::Queued.as_str()),
                    )
                    .col_expr(
                        entities::document::Column::IndexedRevision,
                        sea_orm::sea_query::Expr::value(Option::<i64>::None),
                    )
                    .col_expr(
                        entities::document::Column::IndexFingerprint,
                        sea_orm::sea_query::Expr::value(Option::<String>::None),
                    )
                    .col_expr(
                        entities::document::Column::IndexedAt,
                        sea_orm::sea_query::Expr::value(
                            Option::<chrono::DateTime<chrono::Utc>>::None,
                        ),
                    )
                    .filter(entities::document::Column::Id.eq(document_id.0))
                    .filter(
                        entities::document::Column::ContentRevision
                            .eq(current_generation.content_revision),
                    )
                    .filter(
                        entities::document::Column::RevisionToken
                            .eq(current_generation.revision_token),
                    )
                    .exec(&transaction)
                    .await
                    .map_err(store_err)?;
                if updated.rows_affected != 1 {
                    transaction.rollback().await.map_err(store_err)?;
                    continue;
                }
                advanced.current
            } else {
                clear_document_index_watermark_on(&transaction, document_id, current_generation)
                    .await?;
                current_generation
            };

            let now = Utc::now();
            cancel_live_document_jobs_on(&transaction, document_id, now).await?;
            let job = new_document_index_job(
                document_id,
                target_generation,
                pipeline_fingerprint,
                max_attempts,
                now,
            );
            document_job_active_model(&job)
                .insert(&transaction)
                .await
                .map_err(store_err)?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(EnsureDocumentIndexJobOutcome::Enqueued(job));
        }
    }

    async fn ensure_document_parse_job(
        &self,
        document_id: DocumentId,
        expected_generation: DocumentGeneration,
        pipeline_fingerprint: &str,
        max_attempts: i32,
    ) -> Result<EnsureDocumentParseJobOutcome> {
        ops::document::ensure_parse_job(
            self,
            document_id,
            expected_generation,
            pipeline_fingerprint,
            max_attempts,
        )
        .await
    }

    async fn list_document_jobs(&self, document_id: DocumentId) -> Result<Vec<DocumentJob>> {
        entities::document_job::Entity::find()
            .filter(entities::document_job::Column::DocumentId.eq(document_id.0))
            .order_by_asc(entities::document_job::Column::ContentRevision)
            .order_by_asc(entities::document_job::Column::CreatedAt)
            .order_by_asc(entities::document_job::Column::Id)
            .all(&self.conn)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(document_job_from_model)
            .collect()
    }

    async fn retry_document_job(
        &self,
        document_id: DocumentId,
        expected_generation: DocumentGeneration,
        kind: DocumentJobKind,
        pipeline_fingerprint: &str,
        max_attempts: i32,
    ) -> Result<Option<DocumentJob>> {
        ops::document::retry_document_job(
            self,
            document_id,
            expected_generation,
            kind,
            pipeline_fingerprint,
            max_attempts,
        )
        .await
    }

    async fn claim_document_job(
        &self,
        now: chrono::DateTime<Utc>,
        lease_expires_at: chrono::DateTime<Utc>,
    ) -> Result<Option<DocumentJob>> {
        // Scoped here (not module-level) so the blanket `ExprTrait` impl does
        // not leak into the test submodules' `use super::*` and make their
        // `Ord::min`/`max` calls ambiguous.
        use sea_orm::sea_query::ExprTrait;
        if lease_expires_at <= now {
            return Err(AgentError::Store(
                "document job lease expiry must be after claim time".into(),
            ));
        }

        loop {
            let transaction = self.conn.begin().await.map_err(store_err)?;
            acquire_document_job_write_lock(&transaction).await?;
            let due = entities::document_job::Entity::find()
                .filter(entities::document_job::Column::Status.is_in([
                    DocumentJobStatus::Queued.as_str(),
                    DocumentJobStatus::RetryWait.as_str(),
                ]))
                .filter(entities::document_job::Column::AvailableAt.lte(now))
                .filter(
                    sea_orm::sea_query::Expr::col(entities::document_job::Column::AttemptCount).lt(
                        sea_orm::sea_query::Expr::col(entities::document_job::Column::MaxAttempts),
                    ),
                )
                .order_by_asc(entities::document_job::Column::AvailableAt)
                .order_by_asc(entities::document_job::Column::CreatedAt)
                .order_by_asc(entities::document_job::Column::Id)
                .one(&transaction)
                .await
                .map_err(store_err)?;
            let expired = entities::document_job::Entity::find()
                .filter(
                    entities::document_job::Column::Status.eq(DocumentJobStatus::Running.as_str()),
                )
                .filter(entities::document_job::Column::LeaseExpiresAt.lte(now))
                .order_by_asc(entities::document_job::Column::LeaseExpiresAt)
                .order_by_asc(entities::document_job::Column::CreatedAt)
                .order_by_asc(entities::document_job::Column::Id)
                .one(&transaction)
                .await
                .map_err(store_err)?;
            let candidate = match (due, expired) {
                (Some(due), Some(expired)) => {
                    if document_job_due_order(&due, &expired).is_le() {
                        Some(due)
                    } else {
                        Some(expired)
                    }
                }
                (candidate @ Some(_), None) | (None, candidate @ Some(_)) => candidate,
                (None, None) => None,
            };
            let Some(candidate) = candidate else {
                transaction.commit().await.map_err(store_err)?;
                return Ok(None);
            };

            // Enqueue takes the document lock before mutating jobs. Claims use
            // the same order to avoid a Postgres doc↔job deadlock, then reload
            // the candidate because it may have changed while this lock waited.
            acquire_document_write_lock(&transaction, DocumentId(candidate.document_id)).await?;
            let candidate = entities::document_job::Entity::find_by_id(candidate.id)
                .one(&transaction)
                .await
                .map_err(store_err)?;
            let Some(candidate) = candidate.filter(|candidate| document_job_is_due(candidate, now))
            else {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            };

            let reclaiming = candidate.status == DocumentJobStatus::Running.as_str();
            let expected_document_status = if reclaiming {
                DocumentProcessingStatus::Processing
            } else {
                DocumentProcessingStatus::Queued
            };
            let current = entities::document::Entity::find_by_id(candidate.document_id)
                .one(&transaction)
                .await
                .map_err(store_err)?;
            let identity_matches = current.as_ref().is_some_and(|document| {
                document.content_revision == candidate.content_revision
                    && document.revision_token == candidate.revision_token
            });
            if !identity_matches {
                cancel_document_job_on(&transaction, candidate.id, &candidate.status, now).await?;
                transaction.commit().await.map_err(store_err)?;
                continue;
            }
            let current_status = &current.as_ref().unwrap().processing_status;
            if current_status != expected_document_status.as_str() {
                return Err(AgentError::Store(format!(
                    "document job {} is {} but exact document {} is unexpectedly {}",
                    candidate.id, candidate.status, candidate.document_id, current_status
                )));
            }

            if reclaiming && candidate.attempt_count >= candidate.max_attempts {
                let failed = entities::document_job::Entity::update_many()
                    .col_expr(
                        entities::document_job::Column::Status,
                        sea_orm::sea_query::Expr::value(DocumentJobStatus::Failed.as_str()),
                    )
                    .col_expr(
                        entities::document_job::Column::LeaseToken,
                        sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
                    )
                    .col_expr(
                        entities::document_job::Column::LeaseExpiresAt,
                        sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
                    )
                    .col_expr(
                        entities::document_job::Column::FinishedAt,
                        sea_orm::sea_query::Expr::value(Some(now)),
                    )
                    .col_expr(
                        entities::document_job::Column::LastErrorCode,
                        sea_orm::sea_query::Expr::value(Some("lease_expired".to_owned())),
                    )
                    .col_expr(
                        entities::document_job::Column::LastErrorDetail,
                        sea_orm::sea_query::Expr::value(Some(
                            "final worker lease expired".to_owned(),
                        )),
                    )
                    .col_expr(
                        entities::document_job::Column::UpdatedAt,
                        sea_orm::sea_query::Expr::value(now),
                    )
                    .filter(entities::document_job::Column::Id.eq(candidate.id))
                    .filter(
                        entities::document_job::Column::Status
                            .eq(DocumentJobStatus::Running.as_str()),
                    )
                    .filter(
                        entities::document_job::Column::AttemptCount.eq(candidate.attempt_count),
                    )
                    .filter(entities::document_job::Column::LeaseToken.eq(candidate.lease_token))
                    .filter(
                        entities::document_job::Column::LeaseExpiresAt
                            .eq(candidate.lease_expires_at),
                    )
                    .exec(&transaction)
                    .await
                    .map_err(store_err)?;
                let document_failed = entities::document::Entity::update_many()
                    .col_expr(
                        entities::document::Column::ProcessingStatus,
                        sea_orm::sea_query::Expr::value(DocumentProcessingStatus::Failed.as_str()),
                    )
                    .filter(entities::document::Column::Id.eq(candidate.document_id))
                    .filter(
                        entities::document::Column::ContentRevision.eq(candidate.content_revision),
                    )
                    .filter(entities::document::Column::RevisionToken.eq(candidate.revision_token))
                    .filter(
                        entities::document::Column::ProcessingStatus
                            .eq(DocumentProcessingStatus::Processing.as_str()),
                    )
                    .exec(&transaction)
                    .await
                    .map_err(store_err)?;
                if failed.rows_affected != 1 || document_failed.rows_affected != 1 {
                    transaction.rollback().await.map_err(store_err)?;
                    continue;
                }
                transaction.commit().await.map_err(store_err)?;
                continue;
            }

            let lease_token = uuid::Uuid::new_v4();
            let next_attempt = candidate.attempt_count.checked_add(1).ok_or_else(|| {
                AgentError::Store(format!("document job {} attempt overflow", candidate.id))
            })?;
            let claim = entities::document_job::Entity::update_many()
                .col_expr(
                    entities::document_job::Column::Status,
                    sea_orm::sea_query::Expr::value(DocumentJobStatus::Running.as_str()),
                )
                .col_expr(
                    entities::document_job::Column::AttemptCount,
                    sea_orm::sea_query::Expr::value(next_attempt),
                )
                .col_expr(
                    entities::document_job::Column::LeaseToken,
                    sea_orm::sea_query::Expr::value(Some(lease_token)),
                )
                .col_expr(
                    entities::document_job::Column::LeaseExpiresAt,
                    sea_orm::sea_query::Expr::value(Some(lease_expires_at)),
                )
                .col_expr(
                    entities::document_job::Column::StartedAt,
                    sea_orm::sea_query::Expr::value(Some(candidate.started_at.unwrap_or(now))),
                )
                .col_expr(
                    entities::document_job::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(now),
                )
                .filter(entities::document_job::Column::Id.eq(candidate.id))
                .filter(entities::document_job::Column::Status.eq(&candidate.status))
                .filter(entities::document_job::Column::AttemptCount.eq(candidate.attempt_count));
            let claim = if reclaiming {
                claim
                    .col_expr(
                        entities::document_job::Column::LastErrorCode,
                        sea_orm::sea_query::Expr::value(Some("lease_expired".to_owned())),
                    )
                    .col_expr(
                        entities::document_job::Column::LastErrorDetail,
                        sea_orm::sea_query::Expr::value(Some(
                            "previous worker lease expired".to_owned(),
                        )),
                    )
                    .filter(entities::document_job::Column::LeaseToken.eq(candidate.lease_token))
                    .filter(
                        entities::document_job::Column::LeaseExpiresAt
                            .eq(candidate.lease_expires_at),
                    )
            } else {
                claim.filter(entities::document_job::Column::AvailableAt.lte(now))
            };
            let claimed = claim.exec(&transaction).await.map_err(store_err)?;
            if claimed.rows_affected != 1 {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            }

            let document_claimed = entities::document::Entity::update_many()
                .col_expr(
                    entities::document::Column::ProcessingStatus,
                    sea_orm::sea_query::Expr::value(DocumentProcessingStatus::Processing.as_str()),
                )
                .filter(entities::document::Column::Id.eq(candidate.document_id))
                .filter(entities::document::Column::ContentRevision.eq(candidate.content_revision))
                .filter(entities::document::Column::RevisionToken.eq(candidate.revision_token))
                .filter(
                    entities::document::Column::ProcessingStatus
                        .eq(expected_document_status.as_str()),
                )
                .exec(&transaction)
                .await
                .map_err(store_err)?;
            if document_claimed.rows_affected != 1 {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            }

            let job = entities::document_job::Entity::find_by_id(candidate.id)
                .one(&transaction)
                .await
                .map_err(store_err)?
                .ok_or_else(|| {
                    AgentError::Store(format!("claimed job {} disappeared", candidate.id))
                })
                .and_then(document_job_from_model)?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(job));
        }
    }

    async fn heartbeat_document_job(
        &self,
        id: DocumentJobId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
        lease_expires_at: chrono::DateTime<Utc>,
    ) -> Result<bool> {
        if lease_expires_at <= now {
            return Err(AgentError::Store(
                "document job lease expiry must be after heartbeat time".into(),
            ));
        }
        let result = entities::document_job::Entity::update_many()
            .col_expr(
                entities::document_job::Column::LeaseExpiresAt,
                sea_orm::sea_query::Expr::value(Some(lease_expires_at)),
            )
            .col_expr(
                entities::document_job::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .filter(entities::document_job::Column::Id.eq(id.0))
            .filter(entities::document_job::Column::Status.eq(DocumentJobStatus::Running.as_str()))
            .filter(entities::document_job::Column::LeaseToken.eq(lease_token))
            .filter(entities::document_job::Column::LeaseExpiresAt.gt(now))
            .filter(entities::document_job::Column::LeaseExpiresAt.lt(lease_expires_at))
            .filter(entities::document_job::Column::UpdatedAt.lte(now))
            .exec(&self.conn)
            .await
            .map_err(store_err)?;
        Ok(result.rows_affected == 1)
    }

    async fn complete_document_index_job(
        &self,
        id: DocumentJobId,
        lease_token: uuid::Uuid,
        completed_at: chrono::DateTime<Utc>,
    ) -> Result<bool> {
        loop {
            let transaction = self.conn.begin().await.map_err(store_err)?;
            acquire_document_job_write_lock(&transaction).await?;
            let candidate = entities::document_job::Entity::find_by_id(id.0)
                .one(&transaction)
                .await
                .map_err(store_err)?;
            let Some(candidate) = candidate else {
                transaction.rollback().await.map_err(store_err)?;
                return Ok(false);
            };
            acquire_document_write_lock(&transaction, DocumentId(candidate.document_id)).await?;
            let candidate = entities::document_job::Entity::find_by_id(id.0)
                .one(&transaction)
                .await
                .map_err(store_err)?;
            let Some(candidate) =
                candidate.filter(|job| document_job_lease_is_live(job, lease_token, completed_at))
            else {
                transaction.rollback().await.map_err(store_err)?;
                return Ok(false);
            };
            if candidate.kind != DocumentJobKind::Index.as_str() {
                return Err(AgentError::Store(format!(
                    "document job {id} is not an index job"
                )));
            }
            ensure_resolution_document_matches(&transaction, &candidate).await?;

            let resolved = entities::document_job::Entity::update_many()
                .col_expr(
                    entities::document_job::Column::Status,
                    sea_orm::sea_query::Expr::value(DocumentJobStatus::Succeeded.as_str()),
                )
                .col_expr(
                    entities::document_job::Column::LeaseToken,
                    sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
                )
                .col_expr(
                    entities::document_job::Column::LeaseExpiresAt,
                    sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
                )
                .col_expr(
                    entities::document_job::Column::FinishedAt,
                    sea_orm::sea_query::Expr::value(Some(completed_at)),
                )
                .col_expr(
                    entities::document_job::Column::LastErrorCode,
                    sea_orm::sea_query::Expr::value(Option::<String>::None),
                )
                .col_expr(
                    entities::document_job::Column::LastErrorDetail,
                    sea_orm::sea_query::Expr::value(Option::<String>::None),
                )
                .col_expr(
                    entities::document_job::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(completed_at),
                )
                .filter(entities::document_job::Column::Id.eq(candidate.id))
                .filter(
                    entities::document_job::Column::Status.eq(DocumentJobStatus::Running.as_str()),
                )
                .filter(entities::document_job::Column::LeaseToken.eq(Some(lease_token)))
                .filter(
                    entities::document_job::Column::LeaseExpiresAt.eq(candidate.lease_expires_at),
                )
                .filter(entities::document_job::Column::UpdatedAt.eq(candidate.updated_at))
                .exec(&transaction)
                .await
                .map_err(store_err)?;
            if resolved.rows_affected != 1 {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            }
            let document_resolved = entities::document::Entity::update_many()
                .col_expr(
                    entities::document::Column::ProcessingStatus,
                    sea_orm::sea_query::Expr::value(DocumentProcessingStatus::Ready.as_str()),
                )
                .col_expr(
                    entities::document::Column::IndexedRevision,
                    sea_orm::sea_query::Expr::value(Some(candidate.content_revision)),
                )
                .col_expr(
                    entities::document::Column::IndexFingerprint,
                    sea_orm::sea_query::Expr::value(Some(candidate.pipeline_fingerprint.clone())),
                )
                .col_expr(
                    entities::document::Column::IndexedAt,
                    sea_orm::sea_query::Expr::value(Some(completed_at)),
                )
                .filter(entities::document::Column::Id.eq(candidate.document_id))
                .filter(entities::document::Column::ContentRevision.eq(candidate.content_revision))
                .filter(entities::document::Column::RevisionToken.eq(candidate.revision_token))
                .filter(
                    entities::document::Column::ProcessingStatus
                        .eq(DocumentProcessingStatus::Processing.as_str()),
                )
                .exec(&transaction)
                .await
                .map_err(store_err)?;
            if document_resolved.rows_affected != 1 {
                transaction.rollback().await.map_err(store_err)?;
                return Err(AgentError::Store(format!(
                    "document job {} lost its exact processing document during completion",
                    candidate.id
                )));
            }
            transaction.commit().await.map_err(store_err)?;
            return Ok(true);
        }
    }

    async fn record_document_job_failure(
        &self,
        id: DocumentJobId,
        lease_token: uuid::Uuid,
        failed_at: chrono::DateTime<Utc>,
        retry_at: Option<chrono::DateTime<Utc>>,
        error_code: &str,
        error_detail: Option<&str>,
    ) -> Result<Option<DocumentJobStatus>> {
        validate_document_job_error(error_code, error_detail)?;
        if retry_at.is_some_and(|retry_at| retry_at <= failed_at) {
            return Err(AgentError::Store(
                "document job retry time must be after failure time".into(),
            ));
        }

        loop {
            let transaction = self.conn.begin().await.map_err(store_err)?;
            acquire_document_job_write_lock(&transaction).await?;
            let candidate = entities::document_job::Entity::find_by_id(id.0)
                .one(&transaction)
                .await
                .map_err(store_err)?;
            let Some(candidate) = candidate else {
                transaction.rollback().await.map_err(store_err)?;
                return Ok(None);
            };
            acquire_document_write_lock(&transaction, DocumentId(candidate.document_id)).await?;
            let candidate = entities::document_job::Entity::find_by_id(id.0)
                .one(&transaction)
                .await
                .map_err(store_err)?;
            let Some(candidate) =
                candidate.filter(|job| document_job_lease_is_live(job, lease_token, failed_at))
            else {
                transaction.rollback().await.map_err(store_err)?;
                return Ok(None);
            };
            ensure_resolution_document_matches(&transaction, &candidate).await?;

            let will_retry = retry_at.is_some() && candidate.attempt_count < candidate.max_attempts;
            let next_status = if will_retry {
                DocumentJobStatus::RetryWait
            } else {
                DocumentJobStatus::Failed
            };
            let update = entities::document_job::Entity::update_many()
                .col_expr(
                    entities::document_job::Column::Status,
                    sea_orm::sea_query::Expr::value(next_status.as_str()),
                )
                .col_expr(
                    entities::document_job::Column::LeaseToken,
                    sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
                )
                .col_expr(
                    entities::document_job::Column::LeaseExpiresAt,
                    sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
                )
                .col_expr(
                    entities::document_job::Column::LastErrorCode,
                    sea_orm::sea_query::Expr::value(Some(error_code.to_owned())),
                )
                .col_expr(
                    entities::document_job::Column::LastErrorDetail,
                    sea_orm::sea_query::Expr::value(error_detail.map(str::to_owned)),
                )
                .col_expr(
                    entities::document_job::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(failed_at),
                );
            let update = if let Some(retry_at) = retry_at.filter(|_| will_retry) {
                update.col_expr(
                    entities::document_job::Column::AvailableAt,
                    sea_orm::sea_query::Expr::value(retry_at),
                )
            } else {
                update.col_expr(
                    entities::document_job::Column::FinishedAt,
                    sea_orm::sea_query::Expr::value(Some(failed_at)),
                )
            };
            let resolved = update
                .filter(entities::document_job::Column::Id.eq(candidate.id))
                .filter(
                    entities::document_job::Column::Status.eq(DocumentJobStatus::Running.as_str()),
                )
                .filter(entities::document_job::Column::LeaseToken.eq(Some(lease_token)))
                .filter(
                    entities::document_job::Column::LeaseExpiresAt.eq(candidate.lease_expires_at),
                )
                .filter(entities::document_job::Column::UpdatedAt.eq(candidate.updated_at))
                .exec(&transaction)
                .await
                .map_err(store_err)?;
            if resolved.rows_affected != 1 {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            }

            let next_document_status = if will_retry {
                DocumentProcessingStatus::Queued
            } else {
                DocumentProcessingStatus::Failed
            };
            let document_resolved = entities::document::Entity::update_many()
                .col_expr(
                    entities::document::Column::ProcessingStatus,
                    sea_orm::sea_query::Expr::value(next_document_status.as_str()),
                )
                .filter(entities::document::Column::Id.eq(candidate.document_id))
                .filter(entities::document::Column::ContentRevision.eq(candidate.content_revision))
                .filter(entities::document::Column::RevisionToken.eq(candidate.revision_token))
                .filter(
                    entities::document::Column::ProcessingStatus
                        .eq(DocumentProcessingStatus::Processing.as_str()),
                )
                .exec(&transaction)
                .await
                .map_err(store_err)?;
            if document_resolved.rows_affected != 1 {
                transaction.rollback().await.map_err(store_err)?;
                return Err(AgentError::Store(format!(
                    "document job {} lost its exact processing document during failure",
                    candidate.id
                )));
            }
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(next_status));
        }
    }

    async fn mark_document_indexed(
        &self,
        id: DocumentId,
        revision: i64,
        revision_token: uuid::Uuid,
        fingerprint: &str,
        indexed_at: chrono::DateTime<Utc>,
    ) -> Result<bool> {
        if fingerprint.is_empty()
            || fingerprint.chars().count() > crate::model::DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN
        {
            return Err(AgentError::Store(
                "document index fingerprint must contain 1 to 512 characters".into(),
            ));
        }
        let result = entities::document::Entity::update_many()
            .col_expr(
                entities::document::Column::IndexedRevision,
                sea_orm::sea_query::Expr::value(Some(revision)),
            )
            .col_expr(
                entities::document::Column::IndexFingerprint,
                sea_orm::sea_query::Expr::value(Some(fingerprint.to_string())),
            )
            .col_expr(
                entities::document::Column::IndexedAt,
                sea_orm::sea_query::Expr::value(Some(indexed_at)),
            )
            .col_expr(
                entities::document::Column::ProcessingStatus,
                sea_orm::sea_query::Expr::value(DocumentProcessingStatus::Ready.as_str()),
            )
            .filter(entities::document::Column::Id.eq(id.0))
            .filter(entities::document::Column::ContentRevision.eq(revision))
            .filter(entities::document::Column::RevisionToken.eq(revision_token))
            .exec(&self.conn)
            .await
            .map_err(store_err)?;
        Ok(result.rows_affected == 1)
    }

    async fn clear_document_index(
        &self,
        id: DocumentId,
        revision: i64,
        revision_token: uuid::Uuid,
    ) -> Result<bool> {
        let result = entities::document::Entity::update_many()
            .col_expr(
                entities::document::Column::IndexedRevision,
                sea_orm::sea_query::Expr::value(Option::<i64>::None),
            )
            .col_expr(
                entities::document::Column::IndexFingerprint,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
            .col_expr(
                entities::document::Column::IndexedAt,
                sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
            )
            .col_expr(
                entities::document::Column::ProcessingStatus,
                sea_orm::sea_query::Expr::value(DocumentProcessingStatus::Queued.as_str()),
            )
            .filter(entities::document::Column::Id.eq(id.0))
            .filter(entities::document::Column::ContentRevision.eq(revision))
            .filter(entities::document::Column::RevisionToken.eq(revision_token))
            .exec(&self.conn)
            .await
            .map_err(store_err)?;
        Ok(result.rows_affected == 1)
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
        citation_format: Option<Option<crate::citation::CitationFormat>>,
    ) -> Result<bool> {
        ops::conversation::update_chat_metadata(
            self,
            id,
            title,
            model,
            reasoning_effort,
            permission_mode,
            citation_format,
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

    async fn list_output_revision_citations(
        &self,
        revision_id: OutputRevisionId,
    ) -> Result<Vec<OutputCitationSnapshot>> {
        ops::output::list_output_revision_citations(self, revision_id).await
    }

    async fn delete_output(&self, id: OutputId, deleted_at: chrono::DateTime<Utc>) -> Result<bool> {
        ops::output::delete_output(self, id, deleted_at).await
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
    ) -> Result<Option<AgentRun>> {
        ops::agent_run::claim_container_agent_run(self, id, lease_token, lease_duration).await
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
        ops::turn::accept_turn(self, id, chat_id, model, content, &[]).await
    }

    async fn accept_turn_with_attachments(
        &self,
        id: TurnId,
        chat_id: ChatId,
        model: &str,
        content: &str,
        images: &[ImageRef],
    ) -> Result<AcceptTurnOutcome> {
        ops::turn::accept_turn(self, id, chat_id, model, content, images).await
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
        preceding_citations: &[crate::AssistantCitationReference],
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
        citations: &[crate::AssistantCitationReference],
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
        citations: &[crate::AssistantCitationReference],
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
        references: &[crate::AssistantCitationReference],
    ) -> Result<()> {
        ops::citation::append_assistant_message(self, message, references).await
    }

    async fn append_claimed_assistant_message_with_citations(
        &self,
        message: &Message,
        references: &[crate::AssistantCitationReference],
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

    async fn resolve_server_tool_call_with_evidence(
        &self,
        id: CallId,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<Utc>,
        evidence: &[crate::RetrievalEvidenceInput],
    ) -> Result<ResolveToolCallOutcome> {
        ops::client_execution::resolve_server_tool_call_with_evidence(
            self,
            id,
            resolution,
            resolved_at,
            evidence,
            None,
        )
        .await
    }

    async fn resolve_server_tool_call_with_artifacts(
        &self,
        id: CallId,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<Utc>,
        evidence: &[crate::RetrievalEvidenceInput],
        preview: Option<&crate::ToolResultPreview>,
    ) -> Result<ResolveToolCallOutcome> {
        ops::client_execution::resolve_server_tool_call_with_evidence(
            self,
            id,
            resolution,
            resolved_at,
            evidence,
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
        evidence: &[crate::RetrievalEvidenceInput],
        preview: Option<&crate::ToolResultPreview>,
    ) -> Result<ResolveToolCallOutcome> {
        ops::client_execution::resolve_claimed_server_tool_call_with_evidence(
            self,
            id,
            chat_id,
            turn_id,
            lease_token,
            now,
            resolution,
            resolved_at,
            evidence,
            preview,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn resolve_claimed_server_tool_call_with_evidence(
        &self,
        id: CallId,
        chat_id: ChatId,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        now: chrono::DateTime<Utc>,
        resolution: &ToolCallResolution,
        resolved_at: chrono::DateTime<Utc>,
        evidence: &[crate::RetrievalEvidenceInput],
    ) -> Result<ResolveToolCallOutcome> {
        ops::client_execution::resolve_claimed_server_tool_call_with_evidence(
            self,
            id,
            chat_id,
            turn_id,
            lease_token,
            now,
            resolution,
            resolved_at,
            evidence,
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

    async fn list_retrieval_evidence(&self, id: CallId) -> Result<Vec<crate::RetrievalEvidence>> {
        ops::client_execution::list_retrieval_evidence(self, id).await
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
        citations: &[crate::AssistantCitationReference],
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

/// Acquire the database writer/row lock before the enqueue transaction reads.
///
/// On SQLite, even a no-match UPDATE starts the transaction as a writer, so two
/// enqueues cannot both establish read snapshots and later fail their read→write
/// upgrade with `SQLITE_BUSY_SNAPSHOT`. On Postgres an existing document row is
/// locked; first inserts remain protected by the unique-key/CAS loop.
async fn acquire_document_write_lock<C>(conn: &C, id: DocumentId) -> Result<()>
where
    C: ConnectionTrait,
{
    entities::document::Entity::update_many()
        .col_expr(
            entities::document::Column::UpdatedAt,
            sea_orm::sea_query::Expr::col(entities::document::Column::UpdatedAt),
        )
        .filter(entities::document::Column::Id.eq(id.0))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

async fn find_exact_document_index_job_on<C>(
    conn: &C,
    document_id: DocumentId,
    generation: DocumentGeneration,
    pipeline_fingerprint: &str,
) -> Result<Option<entities::document_job::Model>>
where
    C: ConnectionTrait,
{
    entities::document_job::Entity::find()
        .filter(entities::document_job::Column::DocumentId.eq(document_id.0))
        .filter(entities::document_job::Column::ContentRevision.eq(generation.content_revision))
        .filter(entities::document_job::Column::RevisionToken.eq(generation.revision_token))
        .filter(entities::document_job::Column::Kind.eq(DocumentJobKind::Index.as_str()))
        .filter(entities::document_job::Column::PipelineFingerprint.eq(pipeline_fingerprint))
        .one(conn)
        .await
        .map_err(store_err)
}

async fn clear_document_index_watermark_on<C>(
    conn: &C,
    document_id: DocumentId,
    generation: DocumentGeneration,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let updated = entities::document::Entity::update_many()
        .col_expr(
            entities::document::Column::ProcessingStatus,
            sea_orm::sea_query::Expr::value(DocumentProcessingStatus::Queued.as_str()),
        )
        .col_expr(
            entities::document::Column::IndexedRevision,
            sea_orm::sea_query::Expr::value(Option::<i64>::None),
        )
        .col_expr(
            entities::document::Column::IndexFingerprint,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::document::Column::IndexedAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .filter(entities::document::Column::Id.eq(document_id.0))
        .filter(entities::document::Column::ContentRevision.eq(generation.content_revision))
        .filter(entities::document::Column::RevisionToken.eq(generation.revision_token))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "document {document_id} generation changed during index maintenance"
        )));
    }
    Ok(())
}

async fn cancel_live_document_jobs_on<C>(
    conn: &C,
    document_id: DocumentId,
    now: chrono::DateTime<Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    entities::document_job::Entity::update_many()
        .col_expr(
            entities::document_job::Column::Status,
            sea_orm::sea_query::Expr::value(DocumentJobStatus::Cancelled.as_str()),
        )
        .col_expr(
            entities::document_job::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::document_job::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::document_job::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .col_expr(
            entities::document_job::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::document_job::Column::DocumentId.eq(document_id.0))
        .filter(entities::document_job::Column::Status.is_in([
            DocumentJobStatus::Queued.as_str(),
            DocumentJobStatus::Running.as_str(),
            DocumentJobStatus::RetryWait.as_str(),
        ]))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

async fn reset_document_index_job_on<C>(
    conn: &C,
    id: uuid::Uuid,
    expected_status: &str,
    max_attempts: i32,
    now: chrono::DateTime<Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let updated = entities::document_job::Entity::update_many()
        .col_expr(
            entities::document_job::Column::Status,
            sea_orm::sea_query::Expr::value(DocumentJobStatus::Queued.as_str()),
        )
        .col_expr(
            entities::document_job::Column::AttemptCount,
            sea_orm::sea_query::Expr::value(0),
        )
        .col_expr(
            entities::document_job::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(max_attempts),
        )
        .col_expr(
            entities::document_job::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .col_expr(
            entities::document_job::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::document_job::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::document_job::Column::StartedAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::document_job::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::document_job::Column::LastErrorCode,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::document_job::Column::LastErrorDetail,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::document_job::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::document_job::Column::Id.eq(id))
        .filter(entities::document_job::Column::Status.eq(expected_status))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "document job {id} changed during index maintenance"
        )));
    }
    Ok(())
}

fn new_document_index_job(
    document_id: DocumentId,
    generation: DocumentGeneration,
    pipeline_fingerprint: &str,
    max_attempts: i32,
    now: chrono::DateTime<Utc>,
) -> DocumentJob {
    DocumentJob {
        id: DocumentJobId::new(),
        document_id,
        content_revision: generation.content_revision,
        revision_token: generation.revision_token,
        kind: DocumentJobKind::Index,
        status: DocumentJobStatus::Queued,
        pipeline_fingerprint: pipeline_fingerprint.into(),
        attempt_count: 0,
        max_attempts,
        available_at: now,
        lease_token: None,
        lease_expires_at: None,
        started_at: None,
        finished_at: None,
        last_error_code: None,
        last_error_detail: None,
        created_at: now,
        updated_at: now,
    }
}

/// Start SQLite's write transaction before claim candidate reads.
///
/// The impossible primary-key predicate deliberately locks no Postgres row;
/// Postgres claims serialize on the selected document row instead.
async fn acquire_document_job_write_lock<C>(conn: &C) -> Result<()>
where
    C: ConnectionTrait,
{
    entities::document_job::Entity::update_many()
        .col_expr(
            entities::document_job::Column::UpdatedAt,
            sea_orm::sea_query::Expr::col(entities::document_job::Column::UpdatedAt),
        )
        .filter(entities::document_job::Column::Id.is_null())
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

fn document_job_is_due(job: &entities::document_job::Model, now: chrono::DateTime<Utc>) -> bool {
    match job.status.as_str() {
        status
            if status == DocumentJobStatus::Queued.as_str()
                || status == DocumentJobStatus::RetryWait.as_str() =>
        {
            job.available_at <= now && job.attempt_count < job.max_attempts
        }
        status if status == DocumentJobStatus::Running.as_str() => {
            job.lease_expires_at.is_some_and(|expiry| expiry <= now)
        }
        _ => false,
    }
}

fn document_job_due_order(
    left: &entities::document_job::Model,
    right: &entities::document_job::Model,
) -> std::cmp::Ordering {
    let effective_due = |job: &entities::document_job::Model| {
        if job.status == DocumentJobStatus::Running.as_str() {
            job.lease_expires_at.unwrap_or(job.available_at)
        } else {
            job.available_at
        }
    };
    effective_due(left)
        .cmp(&effective_due(right))
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.id.cmp(&right.id))
}

fn document_job_lease_is_live(
    job: &entities::document_job::Model,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
) -> bool {
    job.status == DocumentJobStatus::Running.as_str()
        && job.lease_token == Some(lease_token)
        && job.lease_expires_at.is_some_and(|expiry| expiry > now)
        && job.updated_at <= now
}

async fn ensure_resolution_document_matches<C>(
    conn: &C,
    job: &entities::document_job::Model,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let document = entities::document::Entity::find_by_id(job.document_id)
        .one(conn)
        .await
        .map_err(store_err)?;
    let Some(document) = document else {
        return Err(AgentError::Store(format!(
            "running document job {} has no source document",
            job.id
        )));
    };
    if document.content_revision != job.content_revision
        || document.revision_token != job.revision_token
        || document.processing_status != DocumentProcessingStatus::Processing.as_str()
    {
        return Err(AgentError::Store(format!(
            "running document job {} does not match its exact processing document {}",
            job.id, job.document_id
        )));
    }
    Ok(())
}

fn validate_document_job_error(error_code: &str, error_detail: Option<&str>) -> Result<()> {
    let code_len = error_code.chars().count();
    if !(1..=DocumentJob::MAX_ERROR_CODE_LEN).contains(&code_len) {
        return Err(AgentError::Store(
            "document job error code must contain 1 to 128 characters".into(),
        ));
    }
    if error_detail.is_some_and(|detail| {
        !(1..=DocumentJob::MAX_ERROR_DETAIL_LEN).contains(&detail.chars().count())
    }) {
        return Err(AgentError::Store(
            "document job error detail must contain 1 to 4096 characters".into(),
        ));
    }
    Ok(())
}

fn document_upsert_matches(current: &entities::document::Model, document: &DocumentUpsert) -> bool {
    current.source_blob_id.is_none()
        && current.source_sha256.is_none()
        && current.source_byte_len.is_none()
        && current.canonical_fingerprint.is_none()
        && current.chat_id == document.chat_id.map(|id| id.0)
        && current.project_id == document.project_id.map(|id| id.0)
        && current.source_uri == document.source_uri
        && current.media_type == document.media_type
        && current.title == document.title
        && current.canonical_text == document.canonical_text
        && current.source_regions == source_regions_to_db(&document.source_regions)
}

struct AdvancedDocumentGeneration {
    previous: Option<DocumentGeneration>,
    current: DocumentGeneration,
}

async fn ensure_live_document_generation_on<C>(
    conn: &C,
    document: &entities::document::Model,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let generation = entities::document_generation::Entity::find_by_id(document.id)
        .one(conn)
        .await
        .map_err(store_err)?;
    if generation.as_ref().is_none_or(|generation| {
        generation.tombstone
            || generation.content_revision != document.content_revision
            || generation.revision_token != document.revision_token
    }) {
        return Err(AgentError::Store(format!(
            "document {} does not match its retained generation clock",
            document.id
        )));
    }
    Ok(())
}

async fn try_advance_document_generation_on<C>(
    conn: &C,
    id: DocumentId,
    tombstone: bool,
) -> Result<Option<AdvancedDocumentGeneration>>
where
    C: ConnectionTrait,
{
    let previous = entities::document_generation::Entity::find_by_id(id.0)
        .one(conn)
        .await
        .map_err(store_err)?;
    if let Some(previous) = previous {
        let next_revision = previous
            .content_revision
            .checked_add(1)
            .ok_or_else(|| AgentError::Store(format!("document {id} revision overflow")))?;
        let revision_token = uuid::Uuid::new_v4();
        let mut update = entities::document_generation::Entity::update_many()
            .col_expr(
                entities::document_generation::Column::ContentRevision,
                sea_orm::sea_query::Expr::value(next_revision),
            )
            .col_expr(
                entities::document_generation::Column::RevisionToken,
                sea_orm::sea_query::Expr::value(revision_token),
            )
            .col_expr(
                entities::document_generation::Column::Tombstone,
                sea_orm::sea_query::Expr::value(tombstone),
            );
        if tombstone {
            update = update
                .col_expr(
                    entities::document_generation::Column::RetirementPending,
                    sea_orm::sea_query::Expr::value(true),
                )
                .col_expr(
                    entities::document_generation::Column::RetirementContentRevision,
                    sea_orm::sea_query::Expr::value(next_revision),
                )
                .col_expr(
                    entities::document_generation::Column::RetirementRevisionToken,
                    sea_orm::sea_query::Expr::value(revision_token),
                );
        }
        let updated = update
            .filter(entities::document_generation::Column::DocumentId.eq(id.0))
            .filter(
                entities::document_generation::Column::ContentRevision
                    .eq(previous.content_revision),
            )
            .filter(
                entities::document_generation::Column::RevisionToken.eq(previous.revision_token),
            )
            .exec(conn)
            .await
            .map_err(store_err)?;
        if updated.rows_affected != 1 {
            return Ok(None);
        }
        return Ok(Some(AdvancedDocumentGeneration {
            previous: Some(DocumentGeneration {
                content_revision: previous.content_revision,
                revision_token: previous.revision_token,
            }),
            current: DocumentGeneration {
                content_revision: next_revision,
                revision_token,
            },
        }));
    }

    let current = DocumentGeneration {
        content_revision: 1,
        revision_token: uuid::Uuid::new_v4(),
    };
    let inserted =
        entities::document_generation::Entity::insert(entities::document_generation::ActiveModel {
            document_id: Set(id.0),
            content_revision: Set(current.content_revision),
            revision_token: Set(current.revision_token),
            tombstone: Set(tombstone),
            retirement_pending: Set(tombstone),
            retirement_content_revision: Set(tombstone.then_some(current.content_revision)),
            retirement_revision_token: Set(tombstone.then_some(current.revision_token)),
        })
        .on_conflict_do_nothing()
        .exec_without_returning(conn)
        .await
        .map_err(store_err)?;
    if !matches!(inserted, TryInsertResult::Inserted(1)) {
        return Ok(None);
    }
    Ok(Some(AdvancedDocumentGeneration {
        previous: None,
        current,
    }))
}

async fn try_upsert_document_on<C>(
    conn: &C,
    document: &DocumentUpsert,
) -> Result<Option<DocumentRecord>>
where
    C: ConnectionTrait,
{
    // `None` means a concurrent writer won either generation or source CAS.
    // Callers must roll back the outer transaction and repeat semantic checks.
    let existing = entities::document::Entity::find_by_id(document.id.0)
        .one(conn)
        .await
        .map_err(store_err)?;
    if let Some(existing) = existing.as_ref() {
        if existing.source_blob_id.is_some()
            || existing.source_sha256.is_some()
            || existing.source_byte_len.is_some()
            || existing.canonical_fingerprint.is_some()
        {
            return Err(AgentError::Store(
                "raw-source documents require the staged source workflow".into(),
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
        ensure_live_document_generation_on(conn, existing).await?;
    }
    let Some(advanced) = try_advance_document_generation_on(conn, document.id, false).await? else {
        return Ok(None);
    };

    if let Some(existing) = existing {
        let existing_generation = DocumentGeneration {
            content_revision: existing.content_revision,
            revision_token: existing.revision_token,
        };
        if advanced.previous != Some(existing_generation) {
            return Err(AgentError::Store(format!(
                "document {} does not match its retained generation clock",
                document.id
            )));
        }
        let result = entities::document::Entity::update_many()
            .col_expr(
                entities::document::Column::ChatId,
                sea_orm::sea_query::Expr::value(document.chat_id.map(|id| id.0)),
            )
            .col_expr(
                entities::document::Column::ProjectId,
                sea_orm::sea_query::Expr::value(document.project_id.map(|id| id.0)),
            )
            .col_expr(
                entities::document::Column::SourceUri,
                sea_orm::sea_query::Expr::value(document.source_uri.clone()),
            )
            .col_expr(
                entities::document::Column::MediaType,
                sea_orm::sea_query::Expr::value(document.media_type.clone()),
            )
            .col_expr(
                entities::document::Column::Title,
                sea_orm::sea_query::Expr::value(document.title.clone()),
            )
            .col_expr(
                entities::document::Column::CanonicalText,
                sea_orm::sea_query::Expr::value(document.canonical_text.clone()),
            )
            .col_expr(
                entities::document::Column::SourceRegions,
                sea_orm::sea_query::Expr::value(source_regions_to_db(&document.source_regions)),
            )
            .col_expr(
                entities::document::Column::ContentRevision,
                sea_orm::sea_query::Expr::value(advanced.current.content_revision),
            )
            .col_expr(
                entities::document::Column::RevisionToken,
                sea_orm::sea_query::Expr::value(advanced.current.revision_token),
            )
            .col_expr(
                entities::document::Column::ProcessingStatus,
                sea_orm::sea_query::Expr::value(DocumentProcessingStatus::Queued.as_str()),
            )
            .col_expr(
                entities::document::Column::IndexedRevision,
                sea_orm::sea_query::Expr::value(Option::<i64>::None),
            )
            .col_expr(
                entities::document::Column::IndexFingerprint,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
            .col_expr(
                entities::document::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(document.updated_at),
            )
            .col_expr(
                entities::document::Column::IndexedAt,
                sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
            )
            .filter(entities::document::Column::Id.eq(document.id.0))
            .filter(entities::document::Column::ContentRevision.eq(existing.content_revision))
            .filter(entities::document::Column::RevisionToken.eq(existing.revision_token))
            .exec(conn)
            .await
            .map_err(store_err)?;
        if result.rows_affected != 1 {
            return Ok(None);
        }
        return Ok(Some(document_from_upsert(
            document,
            existing.created_at,
            advanced.current.content_revision,
            advanced.current.revision_token,
        )));
    }

    let inserted = entities::document::Entity::insert(entities::document::ActiveModel {
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
        canonical_fingerprint: Set(None),
        source_regions: Set(source_regions_to_db(&document.source_regions)),
        content_revision: Set(advanced.current.content_revision),
        revision_token: Set(advanced.current.revision_token),
        processing_status: Set(DocumentProcessingStatus::Queued.as_str().into()),
        indexed_revision: Set(None),
        index_fingerprint: Set(None),
        created_at: Set(document.updated_at),
        updated_at: Set(document.updated_at),
        indexed_at: Set(None),
    })
    .on_conflict_do_nothing()
    .exec_without_returning(conn)
    .await
    .map_err(store_err)?;
    if !matches!(inserted, TryInsertResult::Inserted(1)) {
        return Ok(None);
    }
    Ok(Some(document_from_upsert(
        document,
        document.updated_at,
        advanced.current.content_revision,
        advanced.current.revision_token,
    )))
}

async fn cancel_document_job_on<C>(
    conn: &C,
    id: uuid::Uuid,
    expected_status: &str,
    finished_at: chrono::DateTime<Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    entities::document_job::Entity::update_many()
        .col_expr(
            entities::document_job::Column::Status,
            sea_orm::sea_query::Expr::value(DocumentJobStatus::Cancelled.as_str()),
        )
        .col_expr(
            entities::document_job::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::document_job::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::document_job::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(finished_at)),
        )
        .col_expr(
            entities::document_job::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(finished_at),
        )
        .filter(entities::document_job::Column::Id.eq(id))
        .filter(entities::document_job::Column::Status.eq(expected_status))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(())
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

fn validate_document_source_regions(text: &str, regions: &[SourceRegion]) -> Result<()> {
    crate::model::validate_source_regions(text, regions)
        .map_err(|message| AgentError::Store(format!("invalid document source regions: {message}")))
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

fn source_regions_to_db(regions: &[SourceRegion]) -> Value {
    serde_json::to_value(regions).expect("SourceRegion serialization is infallible")
}

fn source_regions_from_db(value: Value) -> Result<Vec<SourceRegion>> {
    serde_json::from_value(value).map_err(|error| {
        AgentError::Store(format!("invalid stored document source regions: {error}"))
    })
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
    let source_regions = source_regions_from_db(model.source_regions)?;
    validate_document_source_regions(&model.canonical_text, &source_regions)?;
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
        canonical_fingerprint: model.canonical_fingerprint,
        source_regions,
        content_revision: model.content_revision,
        revision_token: model.revision_token,
        processing_status: document_processing_status_from_db(&model.processing_status)?,
        indexed_revision: model.indexed_revision,
        index_fingerprint: model.index_fingerprint,
        created_at: model.created_at,
        updated_at: model.updated_at,
        indexed_at: model.indexed_at,
    })
}

fn document_summary_from_row(row: DocumentSummaryRow) -> Result<DocumentSummaryRecord> {
    let processing_status = document_processing_status_from_db(&row.processing_status)?;
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
        content_revision: row.content_revision,
        processing_status,
        searchable: processing_status == DocumentProcessingStatus::Ready && row.has_canonical_text,
        indexed_revision: row.indexed_revision,
        index_fingerprint: row.index_fingerprint,
        created_at: row.created_at,
        updated_at: row.updated_at,
        indexed_at: row.indexed_at,
    })
}

fn document_from_upsert(
    document: &DocumentUpsert,
    created_at: chrono::DateTime<Utc>,
    content_revision: i64,
    revision_token: uuid::Uuid,
) -> DocumentRecord {
    DocumentRecord {
        id: document.id,
        chat_id: document.chat_id,
        project_id: document.project_id,
        source_uri: document.source_uri.clone(),
        media_type: document.media_type.clone(),
        title: document.title.clone(),
        source_blob: None,
        canonical_text: document.canonical_text.clone(),
        canonical_fingerprint: None,
        source_regions: document.source_regions.clone(),
        content_revision,
        revision_token,
        processing_status: DocumentProcessingStatus::Queued,
        indexed_revision: None,
        index_fingerprint: None,
        created_at,
        updated_at: document.updated_at,
        indexed_at: None,
    }
}

fn document_processing_status_from_db(text: &str) -> Result<DocumentProcessingStatus> {
    match text {
        "queued" => Ok(DocumentProcessingStatus::Queued),
        "processing" => Ok(DocumentProcessingStatus::Processing),
        "ready" => Ok(DocumentProcessingStatus::Ready),
        "failed" => Ok(DocumentProcessingStatus::Failed),
        other => Err(AgentError::Store(format!(
            "unknown document processing status: {other}"
        ))),
    }
}

fn document_job_active_model(job: &DocumentJob) -> entities::document_job::ActiveModel {
    entities::document_job::ActiveModel {
        id: Set(job.id.0),
        document_id: Set(job.document_id.0),
        content_revision: Set(job.content_revision),
        revision_token: Set(job.revision_token),
        kind: Set(job.kind.as_str().into()),
        status: Set(job.status.as_str().into()),
        pipeline_fingerprint: Set(job.pipeline_fingerprint.clone()),
        attempt_count: Set(job.attempt_count),
        max_attempts: Set(job.max_attempts),
        available_at: Set(job.available_at),
        lease_token: Set(job.lease_token),
        lease_expires_at: Set(job.lease_expires_at),
        started_at: Set(job.started_at),
        finished_at: Set(job.finished_at),
        last_error_code: Set(job.last_error_code.clone()),
        last_error_detail: Set(job.last_error_detail.clone()),
        created_at: Set(job.created_at),
        updated_at: Set(job.updated_at),
    }
}

fn document_job_from_model(model: entities::document_job::Model) -> Result<DocumentJob> {
    Ok(DocumentJob {
        id: DocumentJobId(model.id),
        document_id: DocumentId(model.document_id),
        content_revision: model.content_revision,
        revision_token: model.revision_token,
        kind: match model.kind.as_str() {
            "parse" => DocumentJobKind::Parse,
            "index" => DocumentJobKind::Index,
            other => {
                return Err(AgentError::Store(format!(
                    "unknown document job kind: {other}"
                )))
            }
        },
        status: match model.status.as_str() {
            "queued" => DocumentJobStatus::Queued,
            "running" => DocumentJobStatus::Running,
            "retry_wait" => DocumentJobStatus::RetryWait,
            "succeeded" => DocumentJobStatus::Succeeded,
            "failed" => DocumentJobStatus::Failed,
            "cancelled" => DocumentJobStatus::Cancelled,
            other => {
                return Err(AgentError::Store(format!(
                    "unknown document job status: {other}"
                )))
            }
        },
        pipeline_fingerprint: model.pipeline_fingerprint,
        attempt_count: model.attempt_count,
        max_attempts: model.max_attempts,
        available_at: model.available_at,
        lease_token: model.lease_token,
        lease_expires_at: model.lease_expires_at,
        started_at: model.started_at,
        finished_at: model.finished_at,
        last_error_code: model.last_error_code,
        last_error_detail: model.last_error_detail,
        created_at: model.created_at,
        updated_at: model.updated_at,
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
