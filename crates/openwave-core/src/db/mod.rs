//! The default [`Store`], backed by SeaORM.
//!
//! One implementation and one migration set run on any SeaORM backend, chosen by
//! connection string — SQLite locally, Postgres for self-host. Types are native
//! per backend (uuid, timestamptz, jsonb on Postgres; the SQLite equivalents),
//! so nothing is stringly-encoded by hand. Enabled by the `sqlite` feature (which
//! compiles in the SQLite driver).

use std::path::PathBuf;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, Database, DatabaseConnection, EntityTrait,
    FromQueryResult, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait, TryInsertResult,
};
use sea_orm_migration::MigratorTrait;
use serde_json::Value;

use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::{CallId, ChatId, DocumentId, DocumentJobId, MessageId, ProjectId, TurnId};
use crate::model::{
    Chat, DocumentGeneration, DocumentJob, DocumentJobKind, DocumentJobStatus, DocumentListCursor,
    DocumentProcessingStatus, DocumentRecord, DocumentScope, DocumentSummaryRecord, DocumentUpsert,
    Message, Project, Role, ToolCallRecord,
};
use crate::storage::{DocumentIndexJobReason, EnsureDocumentIndexJobOutcome, Store};

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
    project_id: Option<uuid::Uuid>,
    source_uri: Option<String>,
    media_type: String,
    title: Option<String>,
    content_revision: i64,
    processing_status: String,
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
        entities::project::ActiveModel {
            id: Set(project.id.0),
            title: Set(project.title.clone()),
            workspace_dir: Set(project.workspace_dir.to_string_lossy().into_owned()),
            created_at: Set(project.created_at),
        }
        .insert(&self.conn)
        .await
        .map_err(store_err)?;
        Ok(())
    }

    async fn get_project(&self, id: ProjectId) -> Result<Option<Project>> {
        Ok(entities::project::Entity::find_by_id(id.0)
            .one(&self.conn)
            .await
            .map_err(store_err)?
            .map(project_from_model))
    }

    async fn list_projects(&self) -> Result<Vec<Project>> {
        Ok(entities::project::Entity::find()
            .order_by_desc(entities::project::Column::CreatedAt)
            .all(&self.conn)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(project_from_model)
            .collect())
    }

    async fn create_document(&self, document: &DocumentRecord) -> Result<()> {
        let transaction = self.conn.begin().await.map_err(store_err)?;
        acquire_document_write_lock(&transaction, document.id).await?;
        let revision_token = uuid::Uuid::new_v4();
        entities::document_generation::ActiveModel {
            document_id: Set(document.id.0),
            content_revision: Set(document.content_revision),
            revision_token: Set(revision_token),
            tombstone: Set(false),
            retirement_pending: Set(false),
        }
        .insert(&transaction)
        .await
        .map_err(store_err)?;
        entities::document::ActiveModel {
            id: Set(document.id.0),
            project_id: Set(document.project_id.map(|id| id.0)),
            source_uri: Set(document.source_uri.clone()),
            media_type: Set(document.media_type.clone()),
            title: Set(document.title.clone()),
            canonical_text: Set(document.canonical_text.clone()),
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
            DocumentScope::Unscoped => {
                query.filter(entities::document::Column::ProjectId.is_null())
            }
            DocumentScope::Project(id) => {
                query.filter(entities::document::Column::ProjectId.eq(id.0))
            }
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
            DocumentScope::Unscoped => {
                query.filter(entities::document::Column::ProjectId.is_null())
            }
            DocumentScope::Project(id) => {
                query.filter(entities::document::Column::ProjectId.eq(id.0))
            }
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
                entities::document::Column::ProjectId,
                entities::document::Column::SourceUri,
                entities::document::Column::MediaType,
                entities::document::Column::Title,
                entities::document::Column::ContentRevision,
                entities::document::Column::ProcessingStatus,
                entities::document::Column::IndexedRevision,
                entities::document::Column::IndexFingerprint,
                entities::document::Column::CreatedAt,
                entities::document::Column::UpdatedAt,
                entities::document::Column::IndexedAt,
            ])
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
            DocumentScope::Unscoped => {
                query.filter(entities::document::Column::ProjectId.is_null())
            }
            DocumentScope::Project(id) => {
                query.filter(entities::document::Column::ProjectId.eq(id.0))
            }
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
            .column(entities::document_generation::Column::ContentRevision)
            .column(entities::document_generation::Column::RevisionToken)
            .filter(entities::document_generation::Column::Tombstone.eq(true))
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
            .filter(entities::document_generation::Column::DocumentId.eq(id.0))
            .filter(
                entities::document_generation::Column::ContentRevision
                    .eq(generation.content_revision),
            )
            .filter(
                entities::document_generation::Column::RevisionToken.eq(generation.revision_token),
            )
            .filter(entities::document_generation::Column::Tombstone.eq(true))
            .filter(entities::document_generation::Column::RetirementPending.eq(true))
            .exec(&self.conn)
            .await
            .map_err(store_err)?;
        Ok(updated.rows_affected == 1)
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
            }
            transaction.commit().await.map_err(store_err)?;
            return Ok(advanced.current);
        }
    }

    async fn upsert_document(&self, document: &DocumentUpsert) -> Result<DocumentRecord> {
        loop {
            let transaction = self.conn.begin().await.map_err(store_err)?;
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

    async fn retry_document_index_job(
        &self,
        document_id: DocumentId,
        pipeline_fingerprint: &str,
        max_attempts: i32,
    ) -> Result<Option<DocumentJob>> {
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

        let transaction = self.conn.begin().await.map_err(store_err)?;
        acquire_document_write_lock(&transaction, document_id).await?;
        let Some(document) = entities::document::Entity::find_by_id(document_id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?
        else {
            transaction.commit().await.map_err(store_err)?;
            return Ok(None);
        };
        ensure_live_document_generation_on(&transaction, &document).await?;
        let candidate = entities::document_job::Entity::find()
            .filter(entities::document_job::Column::DocumentId.eq(document.id))
            .filter(entities::document_job::Column::ContentRevision.eq(document.content_revision))
            .filter(entities::document_job::Column::RevisionToken.eq(document.revision_token))
            .filter(entities::document_job::Column::Kind.eq(DocumentJobKind::Index.as_str()))
            .filter(entities::document_job::Column::PipelineFingerprint.eq(pipeline_fingerprint))
            .one(&transaction)
            .await
            .map_err(store_err)?;
        let Some(candidate) = candidate else {
            transaction.commit().await.map_err(store_err)?;
            return Ok(None);
        };
        let mut parsed = document_job_from_model(candidate.clone())?;
        if matches!(
            parsed.status,
            DocumentJobStatus::Queued | DocumentJobStatus::Running | DocumentJobStatus::RetryWait
        ) {
            let expected = if parsed.status == DocumentJobStatus::Running {
                DocumentProcessingStatus::Processing
            } else {
                DocumentProcessingStatus::Queued
            };
            if document.processing_status != expected.as_str() {
                return Err(AgentError::Store(format!(
                    "document job {} is {} but exact document {} is unexpectedly {}",
                    parsed.id,
                    parsed.status.as_str(),
                    document_id,
                    document.processing_status
                )));
            }
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(parsed));
        }
        if parsed.status != DocumentJobStatus::Failed {
            transaction.commit().await.map_err(store_err)?;
            return Ok(None);
        }
        if document.processing_status != DocumentProcessingStatus::Failed.as_str() {
            return Err(AgentError::Store(format!(
                "failed document job {} does not match failed document {}",
                parsed.id, document_id
            )));
        }

        let now = Utc::now();
        let revived = entities::document_job::Entity::update_many()
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
            .filter(entities::document_job::Column::Id.eq(candidate.id))
            .filter(entities::document_job::Column::Status.eq(DocumentJobStatus::Failed.as_str()))
            .filter(entities::document_job::Column::UpdatedAt.eq(candidate.updated_at))
            .exec(&transaction)
            .await
            .map_err(store_err)?;
        let document_queued = entities::document::Entity::update_many()
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
            .filter(entities::document::Column::Id.eq(document.id))
            .filter(entities::document::Column::ContentRevision.eq(document.content_revision))
            .filter(entities::document::Column::RevisionToken.eq(document.revision_token))
            .filter(
                entities::document::Column::ProcessingStatus
                    .eq(DocumentProcessingStatus::Failed.as_str()),
            )
            .exec(&transaction)
            .await
            .map_err(store_err)?;
        if revived.rows_affected != 1 || document_queued.rows_affected != 1 {
            return Err(AgentError::Store(format!(
                "failed document job {} changed during explicit retry",
                parsed.id
            )));
        }
        transaction.commit().await.map_err(store_err)?;
        parsed.status = DocumentJobStatus::Queued;
        parsed.attempt_count = 0;
        parsed.max_attempts = max_attempts;
        parsed.available_at = now;
        parsed.lease_token = None;
        parsed.lease_expires_at = None;
        parsed.started_at = None;
        parsed.finished_at = None;
        parsed.last_error_code = None;
        parsed.last_error_detail = None;
        parsed.updated_at = now;
        Ok(Some(parsed))
    }

    async fn claim_document_job(
        &self,
        now: chrono::DateTime<Utc>,
        lease_expires_at: chrono::DateTime<Utc>,
    ) -> Result<Option<DocumentJob>> {
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
        entities::chat::ActiveModel {
            id: Set(chat.id.0),
            project_id: Set(chat.project_id.map(|p| p.0)),
            title: Set(chat.title.clone()),
            model: Set(chat.model.clone()),
            workspace_dir: Set(chat.workspace_dir.to_string_lossy().into_owned()),
            created_at: Set(chat.created_at),
        }
        .insert(&self.conn)
        .await
        .map_err(store_err)?;
        Ok(())
    }

    async fn set_chat_model(&self, id: ChatId, model: Option<String>) -> Result<()> {
        entities::chat::Entity::update_many()
            .col_expr(
                entities::chat::Column::Model,
                sea_orm::sea_query::Expr::value(model),
            )
            .filter(entities::chat::Column::Id.eq(id.0))
            .exec(&self.conn)
            .await
            .map_err(store_err)?;
        Ok(())
    }

    async fn get_chat(&self, id: ChatId) -> Result<Option<Chat>> {
        Ok(entities::chat::Entity::find_by_id(id.0)
            .one(&self.conn)
            .await
            .map_err(store_err)?
            .map(chat_from_model))
    }

    async fn list_chats(&self) -> Result<Vec<Chat>> {
        Ok(entities::chat::Entity::find()
            .order_by_desc(entities::chat::Column::CreatedAt)
            .all(&self.conn)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(chat_from_model)
            .collect())
    }

    async fn append_message(&self, message: &Message) -> Result<()> {
        entities::message::ActiveModel {
            id: Set(message.id.0),
            chat_id: Set(message.chat_id.0),
            turn_id: Set(message.turn_id.0),
            role: Set(role_to_db(message.role).to_string()),
            content: Set(message.content.clone()),
            created_at: Set(message.created_at),
        }
        .insert(&self.conn)
        .await
        .map_err(store_err)?;
        Ok(())
    }

    async fn list_messages(&self, chat_id: ChatId) -> Result<Vec<Message>> {
        entities::message::Entity::find()
            .filter(entities::message::Column::ChatId.eq(chat_id.0))
            .order_by_asc(entities::message::Column::CreatedAt)
            .all(&self.conn)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(message_from_model)
            .collect()
    }

    async fn upsert_tool_call(&self, call: &ToolCallRecord) -> Result<()> {
        let model = entities::tool_call::ActiveModel {
            id: Set(call.id.0),
            chat_id: Set(call.chat_id.0),
            turn_id: Set(call.turn_id.0),
            provider_id: Set(call.provider_id.clone()),
            name: Set(call.name.clone()),
            arguments: Set(call.arguments.clone()),
            result: Set(call.result.clone()),
            is_error: Set(call.is_error),
            created_at: Set(call.created_at),
            completed_at: Set(call.completed_at),
        };
        entities::tool_call::Entity::insert(model)
            .on_conflict(
                OnConflict::column(entities::tool_call::Column::Id)
                    .update_columns([
                        entities::tool_call::Column::Arguments,
                        entities::tool_call::Column::Result,
                        entities::tool_call::Column::IsError,
                        entities::tool_call::Column::CompletedAt,
                    ])
                    .to_owned(),
            )
            .exec(&self.conn)
            .await
            .map_err(store_err)?;
        Ok(())
    }

    async fn list_tool_calls(&self, chat_id: ChatId) -> Result<Vec<ToolCallRecord>> {
        Ok(entities::tool_call::Entity::find()
            .filter(entities::tool_call::Column::ChatId.eq(chat_id.0))
            .order_by_asc(entities::tool_call::Column::CreatedAt)
            .all(&self.conn)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(tool_call_from_model)
            .collect())
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
        // Next seq for this chat. This assumes a single writer per chat —
        // the server enforces it by allowing only one active turn per chat at
        // a time (a concurrent message is refused, not queued behind a second
        // writer). Under that invariant read-then-insert is race-free; the
        // composite (chat_id, seq) primary key is the backstop that turns any
        // concurrent double-write into an error, never a silent dup or lost seq.
        let last = entities::event::Entity::find()
            .filter(entities::event::Column::ChatId.eq(chat_id.0))
            .order_by_desc(entities::event::Column::Seq)
            .one(&self.conn)
            .await
            .map_err(store_err)?;
        let seq = last.map_or(0, |model| model.seq) + 1;

        entities::event::ActiveModel {
            chat_id: Set(chat_id.0),
            seq: Set(seq),
            payload: Set(serde_json::to_value(event)?),
            created_at: Set(Utc::now()),
        }
        .insert(&self.conn)
        .await
        .map_err(store_err)?;
        Ok(seq)
    }

    async fn list_events(&self, chat_id: ChatId, after: i64) -> Result<Vec<SequencedEvent>> {
        entities::event::Entity::find()
            .filter(entities::event::Column::ChatId.eq(chat_id.0))
            .filter(entities::event::Column::Seq.gt(after))
            .order_by_asc(entities::event::Column::Seq)
            .all(&self.conn)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(|model| {
                Ok(SequencedEvent {
                    seq: model.seq,
                    event: serde_json::from_value(model.payload)?,
                })
            })
            .collect()
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
            sea_orm::sea_query::Expr::col(entities::document::Column::UpdatedAt).into(),
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
            sea_orm::sea_query::Expr::col(entities::document_job::Column::UpdatedAt).into(),
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
    current.project_id == document.project_id.map(|id| id.0)
        && current.source_uri == document.source_uri
        && current.media_type == document.media_type
        && current.title == document.title
        && current.canonical_text == document.canonical_text
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
        let updated = entities::document_generation::Entity::update_many()
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
            )
            .col_expr(
                entities::document_generation::Column::RetirementPending,
                sea_orm::sea_query::Expr::value(tombstone),
            )
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
        project_id: Set(document.project_id.map(|id| id.0)),
        source_uri: Set(document.source_uri.clone()),
        media_type: Set(document.media_type.clone()),
        title: Set(document.title.clone()),
        canonical_text: Set(document.canonical_text.clone()),
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

fn project_from_model(model: entities::project::Model) -> Project {
    Project {
        id: ProjectId(model.id),
        title: model.title,
        workspace_dir: PathBuf::from(model.workspace_dir),
        created_at: model.created_at,
    }
}

fn document_from_model(model: entities::document::Model) -> Result<DocumentRecord> {
    Ok(DocumentRecord {
        id: DocumentId(model.id),
        project_id: model.project_id.map(ProjectId),
        source_uri: model.source_uri,
        media_type: model.media_type,
        title: model.title,
        canonical_text: model.canonical_text,
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
    Ok(DocumentSummaryRecord {
        id: DocumentId(row.id),
        project_id: row.project_id.map(ProjectId),
        source_uri: row.source_uri,
        media_type: row.media_type,
        title: row.title,
        content_revision: row.content_revision,
        processing_status: document_processing_status_from_db(&row.processing_status)?,
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
        project_id: document.project_id,
        source_uri: document.source_uri.clone(),
        media_type: document.media_type.clone(),
        title: document.title.clone(),
        canonical_text: document.canonical_text.clone(),
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

fn chat_from_model(model: entities::chat::Model) -> Chat {
    Chat {
        id: ChatId(model.id),
        project_id: model.project_id.map(ProjectId),
        title: model.title,
        model: model.model,
        workspace_dir: PathBuf::from(model.workspace_dir),
        created_at: model.created_at,
    }
}

fn message_from_model(model: entities::message::Model) -> Result<Message> {
    Ok(Message {
        id: MessageId(model.id),
        chat_id: ChatId(model.chat_id),
        turn_id: TurnId(model.turn_id),
        role: role_from_db(&model.role)?,
        content: model.content,
        created_at: model.created_at,
    })
}

fn tool_call_from_model(model: entities::tool_call::Model) -> ToolCallRecord {
    ToolCallRecord {
        id: CallId(model.id),
        chat_id: ChatId(model.chat_id),
        turn_id: TurnId(model.turn_id),
        provider_id: model.provider_id,
        name: model.name,
        arguments: model.arguments,
        result: model.result,
        is_error: model.is_error,
        created_at: model.created_at,
        completed_at: model.completed_at,
    }
}

/// `Role` is persisted as its snake_case name (matching its serde encoding).
fn role_to_db(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn role_from_db(text: &str) -> Result<Role> {
    match text {
        "system" => Ok(Role::System),
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        "tool" => Ok(Role::Tool),
        other => Err(AgentError::Store(format!("unknown role: {other}"))),
    }
}

/// SeaORM entity models. Kept internal — the public `Store` API speaks the domain
/// types (`Chat`, `Message`), never these, so the ORM never leaks into the
/// crate's contract.
mod entities {
    pub mod document_generation {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "document_generation")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub document_id: Uuid,
            pub content_revision: i64,
            pub revision_token: Uuid,
            pub tombstone: bool,
            pub retirement_pending: bool,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod document {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "document")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub project_id: Option<Uuid>,
            pub source_uri: Option<String>,
            pub media_type: String,
            pub title: Option<String>,
            #[sea_orm(column_type = "Text")]
            pub canonical_text: String,
            pub content_revision: i64,
            pub revision_token: Uuid,
            pub processing_status: String,
            pub indexed_revision: Option<i64>,
            pub index_fingerprint: Option<String>,
            pub created_at: DateTimeUtc,
            pub updated_at: DateTimeUtc,
            pub indexed_at: Option<DateTimeUtc>,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod document_job {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "document_job")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub document_id: Uuid,
            pub content_revision: i64,
            pub revision_token: Uuid,
            pub kind: String,
            pub status: String,
            pub pipeline_fingerprint: String,
            pub attempt_count: i32,
            pub max_attempts: i32,
            pub available_at: DateTimeUtc,
            pub lease_token: Option<Uuid>,
            pub lease_expires_at: Option<DateTimeUtc>,
            pub started_at: Option<DateTimeUtc>,
            pub finished_at: Option<DateTimeUtc>,
            pub last_error_code: Option<String>,
            pub last_error_detail: Option<String>,
            pub created_at: DateTimeUtc,
            pub updated_at: DateTimeUtc,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod project {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "project")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub title: Option<String>,
            pub workspace_dir: String,
            pub created_at: DateTimeUtc,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod chat {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "chat")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub project_id: Option<Uuid>,
            pub title: Option<String>,
            pub model: Option<String>,
            pub workspace_dir: String,
            pub created_at: DateTimeUtc,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod message {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "message")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub chat_id: Uuid,
            pub turn_id: Uuid,
            pub role: String,
            pub content: String,
            pub created_at: DateTimeUtc,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod tool_call {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "tool_call")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub chat_id: Uuid,
            pub turn_id: Uuid,
            pub provider_id: String,
            pub name: String,
            #[sea_orm(column_type = "JsonBinary")]
            pub arguments: Json,
            pub result: Option<String>,
            pub is_error: bool,
            pub created_at: DateTimeUtc,
            pub completed_at: Option<DateTimeUtc>,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod setting {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "setting")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub key: String,
            // Matches the migration's `.json_binary()` (JSONB on Postgres).
            #[sea_orm(column_type = "JsonBinary")]
            pub value_json: Json,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod event {
        use sea_orm::entity::prelude::*;

        // Composite primary key `(chat_id, seq)`: `seq` is monotonic *per
        // chat*, and the pair both enforces uniqueness and indexes the
        // "this chat's events after a cursor" replay query.
        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "event")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub chat_id: Uuid,
            #[sea_orm(primary_key, auto_increment = false)]
            pub seq: i64,
            #[sea_orm(column_type = "JsonBinary")]
            pub payload: Json,
            pub created_at: DateTimeUtc,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }
}

/// Schema v1, defined once via SeaORM's schema builder; it emits dialect-correct
/// DDL for whichever backend is connected.
mod migration {
    use sea_orm_migration::prelude::*;

    use super::{DocumentJobKind, DocumentJobStatus, DocumentProcessingStatus};

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
            ]
        }
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
                        .table(Chat::Table)
                        .if_not_exists()
                        .col(ColumnDef::new(Chat::Id).uuid().not_null().primary_key())
                        .col(ColumnDef::new(Chat::Title).text())
                        .col(ColumnDef::new(Chat::WorkspaceDir).text().not_null())
                        .col(
                            ColumnDef::new(Chat::CreatedAt)
                                .timestamp_with_time_zone()
                                .not_null(),
                        )
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
                        .col(Message::CreatedAt)
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

            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .drop_table(Table::drop().table(Message::Table).to_owned())
                .await?;
            manager
                .drop_table(Table::drop().table(Setting::Table).to_owned())
                .await?;
            manager
                .drop_table(Table::drop().table(Chat::Table).to_owned())
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

    /// Adds the `project` table and the optional `chat.project_id` link.
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
                        .table(Project::Table)
                        .if_not_exists()
                        .col(ColumnDef::new(Project::Id).uuid().not_null().primary_key())
                        .col(ColumnDef::new(Project::Title).text())
                        .col(ColumnDef::new(Project::WorkspaceDir).text().not_null())
                        .col(
                            ColumnDef::new(Project::CreatedAt)
                                .timestamp_with_time_zone()
                                .not_null(),
                        )
                        .to_owned(),
                )
                .await?;

            // A nullable link, no DB-level foreign key: SQLite can't add an FK to
            // an existing table, so membership is validated at the API edge (the
            // server checks the project exists before creating the chat).
            manager
                .alter_table(
                    Table::alter()
                        .table(Chat::Table)
                        .add_column(ColumnDef::new(Chat::ProjectId).uuid())
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
                        .drop_column(Chat::ProjectId)
                        .to_owned(),
                )
                .await?;
            manager
                .drop_table(Table::drop().table(Project::Table).to_owned())
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
                        .col(ColumnDef::new(ToolCall::Name).text().not_null())
                        .col(ColumnDef::new(ToolCall::Arguments).json_binary().not_null())
                        .col(ColumnDef::new(ToolCall::Result).text())
                        .col(
                            ColumnDef::new(ToolCall::IsError)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .col(
                            ColumnDef::new(ToolCall::CreatedAt)
                                .timestamp_with_time_zone()
                                .not_null(),
                        )
                        .col(ColumnDef::new(ToolCall::CompletedAt).timestamp_with_time_zone())
                        .foreign_key(
                            ForeignKey::create()
                                .name("fk_tool_call_chat")
                                .from(ToolCall::Table, ToolCall::ChatId)
                                .to(Chat::Table, Chat::Id),
                        )
                        .to_owned(),
                )
                .await?;

            manager
                .create_index(
                    Index::create()
                        .name("idx_tool_call_chat")
                        .table(ToolCall::Table)
                        .col(ToolCall::ChatId)
                        .col(ToolCall::CreatedAt)
                        .to_owned(),
                )
                .await?;
            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
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
            let valid_index_revision = Expr::col(Document::IndexedRevision).is_null().or(
                Expr::col(Document::IndexedRevision).gte(1).and(
                    Expr::col(Document::IndexedRevision).lte(Expr::col(Document::ContentRevision)),
                ),
            );
            let watermark_absent = Expr::col(Document::IndexedRevision)
                .is_null()
                .and(Expr::col(Document::IndexFingerprint).is_null())
                .and(Expr::col(Document::IndexedAt).is_null());
            let watermark_present = Expr::col(Document::IndexedRevision)
                .is_not_null()
                .and(Expr::col(Document::IndexFingerprint).is_not_null().and(
                    Func::char_length(Expr::col(Document::IndexFingerprint)).between(
                        1,
                        crate::model::DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN as i32,
                    ),
                ))
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
                        .check(Expr::col(DocumentGeneration::ContentRevision).gte(1))
                        .check(
                            Expr::col(DocumentGeneration::RetirementPending)
                                .eq(false)
                                .or(Expr::col(DocumentGeneration::Tombstone).eq(true)),
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
                        .col(ColumnDef::new(Document::CanonicalText).text().not_null())
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
                                .string_len(crate::model::DocumentJob::MAX_ERROR_CODE_LEN as u32),
                        )
                        .col(
                            ColumnDef::new(DocumentJob::LastErrorDetail)
                                .string_len(crate::model::DocumentJob::MAX_ERROR_DETAIL_LEN as u32),
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
                        .check(
                            Expr::col(DocumentJob::Kind).is_in([DocumentJobKind::Index.as_str()]),
                        )
                        .check(
                            Func::char_length(Expr::col(DocumentJob::Kind))
                                .lte(64)
                                .and(
                                    Func::char_length(Expr::col(DocumentJob::PipelineFingerprint))
                                        .between(
                                            1,
                                            crate::model::DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN
                                                as i32,
                                        ),
                                )
                                .and(
                                    Expr::col(DocumentJob::LastErrorCode).is_null().or(
                                        Func::char_length(Expr::col(DocumentJob::LastErrorCode))
                                            .between(
                                                1,
                                                crate::model::DocumentJob::MAX_ERROR_CODE_LEN
                                                    as i32,
                                            ),
                                    ),
                                )
                                .and(
                                    Expr::col(DocumentJob::LastErrorDetail).is_null().or(
                                        Func::char_length(Expr::col(DocumentJob::LastErrorDetail))
                                            .between(
                                                1,
                                                crate::model::DocumentJob::MAX_ERROR_DETAIL_LEN
                                                    as i32,
                                            ),
                                    ),
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
                .drop_table(Table::drop().table(Document::Table).to_owned())
                .await?;
            manager
                .drop_table(Table::drop().table(DocumentGeneration::Table).to_owned())
                .await?;
            Ok(())
        }
    }

    #[derive(DeriveIden)]
    enum Project {
        Table,
        Id,
        Title,
        WorkspaceDir,
        CreatedAt,
    }

    #[derive(DeriveIden)]
    enum Document {
        Table,
        Id,
        ProjectId,
        SourceUri,
        MediaType,
        Title,
        CanonicalText,
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
        WorkspaceDir,
        CreatedAt,
    }

    #[derive(DeriveIden)]
    enum Message {
        Table,
        Id,
        ChatId,
        TurnId,
        Role,
        Content,
        CreatedAt,
    }

    #[derive(DeriveIden)]
    enum ToolCall {
        Table,
        Id,
        ChatId,
        TurnId,
        ProviderId,
        Name,
        Arguments,
        Result,
        IsError,
        CreatedAt,
        CompletedAt,
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
        Payload,
        CreatedAt,
    }
}

#[cfg(test)]
mod tests;
