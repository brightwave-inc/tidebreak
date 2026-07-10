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
use crate::storage::Store;

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
mod tests {
    use super::*;
    use crate::model::DocumentJobKind;
    use chrono::{DateTime, Utc};

    async fn temp_store() -> (tempfile::TempDir, DbStore) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display());
        let store = DbStore::connect(&url).await.unwrap();
        (dir, store)
    }

    fn sample_chat() -> Chat {
        Chat {
            id: ChatId::new(),
            project_id: None,
            title: Some("hello".into()),
            model: None,
            workspace_dir: PathBuf::from("/tmp/ws"),
            created_at: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
        }
    }

    fn sample_project() -> Project {
        Project {
            id: ProjectId::new(),
            title: Some("proj".into()),
            workspace_dir: PathBuf::from("/tmp/proj"),
            created_at: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
        }
    }

    fn sample_document(project_id: Option<ProjectId>) -> DocumentRecord {
        let created_at = DateTime::<Utc>::from_timestamp(1_700_000_100, 0).unwrap();
        DocumentRecord {
            id: DocumentId::new(),
            project_id,
            source_uri: Some("file:///資料/notes.md".into()),
            media_type: "text/markdown".into(),
            title: Some("Résumé 📈".into()),
            canonical_text: "# Résumé\n\n売上 grew by 10%.".into(),
            content_revision: 1,
            revision_token: uuid::Uuid::new_v4(),
            processing_status: DocumentProcessingStatus::Queued,
            indexed_revision: None,
            index_fingerprint: None,
            created_at,
            updated_at: created_at,
            indexed_at: None,
        }
    }

    #[tokio::test]
    async fn projects_roundtrip_and_a_chat_can_belong_to_one() {
        let (_dir, store) = temp_store().await;
        let project = sample_project();
        store.create_project(&project).await.unwrap();

        assert_eq!(
            store.get_project(project.id).await.unwrap().as_ref(),
            Some(&project)
        );
        assert_eq!(store.list_projects().await.unwrap(), vec![project.clone()]);
        assert_eq!(store.get_project(ProjectId::new()).await.unwrap(), None);

        // A chat carrying the project link round-trips it; a loose chat stays None.
        let mut in_project = sample_chat();
        in_project.project_id = Some(project.id);
        store.create_chat(&in_project).await.unwrap();
        assert_eq!(
            store
                .get_chat(in_project.id)
                .await
                .unwrap()
                .unwrap()
                .project_id,
            Some(project.id)
        );

        let loose = sample_chat();
        store.create_chat(&loose).await.unwrap();
        assert_eq!(
            store.get_chat(loose.id).await.unwrap().unwrap().project_id,
            None
        );

        // The project link survives a list, not just a by-id fetch.
        let listed = store.list_chats().await.unwrap();
        let listed_link = |id| {
            listed
                .iter()
                .find(|c| c.id == id)
                .and_then(|c| c.project_id)
        };
        assert_eq!(listed_link(in_project.id), Some(project.id));
        assert_eq!(listed_link(loose.id), None);
    }

    #[tokio::test]
    async fn set_chat_model_updates_then_clears() {
        let (_dir, store) = temp_store().await;
        let chat = sample_chat();
        store.create_chat(&chat).await.unwrap();
        assert_eq!(store.get_chat(chat.id).await.unwrap().unwrap().model, None);

        store
            .set_chat_model(chat.id, Some("claude-x".into()))
            .await
            .unwrap();
        assert_eq!(
            store
                .get_chat(chat.id)
                .await
                .unwrap()
                .unwrap()
                .model
                .as_deref(),
            Some("claude-x")
        );

        store.set_chat_model(chat.id, None).await.unwrap();
        assert_eq!(store.get_chat(chat.id).await.unwrap().unwrap().model, None);
    }

    #[tokio::test]
    async fn list_projects_is_newest_first() {
        let (_dir, store) = temp_store().await;
        let mut older = sample_project();
        older.created_at = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
        let mut newer = sample_project();
        newer.created_at = DateTime::<Utc>::from_timestamp(2_000, 0).unwrap();
        store.create_project(&older).await.unwrap();
        store.create_project(&newer).await.unwrap();
        assert_eq!(store.list_projects().await.unwrap(), vec![newer, older]);
    }

    #[tokio::test]
    async fn documents_roundtrip_and_list_by_corpus_scope() {
        let (_dir, store) = temp_store().await;
        let project_a = sample_project();
        let mut project_b = sample_project();
        project_b.id = ProjectId::new();
        store.create_project(&project_a).await.unwrap();
        store.create_project(&project_b).await.unwrap();

        let mut unscoped = sample_document(None);
        unscoped.created_at = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
        let mut in_a = sample_document(Some(project_a.id));
        in_a.created_at = DateTime::<Utc>::from_timestamp(2_000, 0).unwrap();
        in_a.processing_status = DocumentProcessingStatus::Ready;
        in_a.indexed_revision = Some(1);
        in_a.index_fingerprint = Some("parser=v1;chunker=v1;embed=test".into());
        in_a.indexed_at = Some(DateTime::<Utc>::from_timestamp(2_001, 0).unwrap());
        let mut in_b = sample_document(Some(project_b.id));
        in_b.created_at = DateTime::<Utc>::from_timestamp(3_000, 0).unwrap();

        for document in [&unscoped, &in_a, &in_b] {
            store.create_document(document).await.unwrap();
        }
        unscoped = store.get_document(unscoped.id).await.unwrap().unwrap();
        in_a = store.get_document(in_a.id).await.unwrap().unwrap();
        in_b = store.get_document(in_b.id).await.unwrap().unwrap();

        assert_eq!(
            store.get_document(in_a.id).await.unwrap().as_ref(),
            Some(&in_a)
        );
        assert_eq!(store.get_document(DocumentId::new()).await.unwrap(), None);
        assert_eq!(
            store
                .list_documents(DocumentScope::Project(project_a.id))
                .await
                .unwrap(),
            vec![in_a.clone()]
        );
        assert_eq!(
            store
                .list_documents(DocumentScope::Project(project_b.id))
                .await
                .unwrap(),
            vec![in_b.clone()]
        );
        assert_eq!(
            store.list_documents(DocumentScope::Unscoped).await.unwrap(),
            vec![unscoped.clone()]
        );
        assert_eq!(
            store.list_documents(DocumentScope::All).await.unwrap(),
            vec![in_b, in_a, unscoped.clone()]
        );

        store.delete_document(unscoped.id).await.unwrap();
        store.delete_document(unscoped.id).await.unwrap();
        assert_eq!(store.get_document(unscoped.id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn document_summaries_page_by_created_at_then_id_without_gaps() {
        let (_dir, store) = temp_store().await;
        // Keep both groups inside one microsecond so cursor implementations
        // that truncate sub-microsecond precision would skip the older group.
        let newer = DateTime::<Utc>::from_timestamp(2_000, 900).unwrap();
        let older = DateTime::<Utc>::from_timestamp(2_000, 700).unwrap();
        let fixtures = [
            (3_u128, newer, "newest tie"),
            (2, newer, "middle tie"),
            (1, newer, "last tie"),
            (5, older, "older high id"),
            (4, older, "older low id"),
        ];
        for (raw_id, created_at, title) in fixtures {
            let mut document = sample_document(None);
            document.id = DocumentId(uuid::Uuid::from_u128(raw_id));
            document.title = Some(title.into());
            document.canonical_text = format!("content that listings must not load: {title}");
            document.created_at = created_at;
            document.updated_at = created_at;
            store.create_document(&document).await.unwrap();
        }

        let first = store
            .list_document_summaries(DocumentScope::All, None, 2)
            .await
            .unwrap();
        assert_eq!(
            first
                .iter()
                .map(|document| document.id.0)
                .collect::<Vec<_>>(),
            vec![uuid::Uuid::from_u128(3), uuid::Uuid::from_u128(2)]
        );
        let second = store
            .list_document_summaries(
                DocumentScope::All,
                Some(DocumentListCursor {
                    created_at: first[1].created_at,
                    id: first[1].id,
                }),
                2,
            )
            .await
            .unwrap();
        assert_eq!(
            second
                .iter()
                .map(|document| document.id.0)
                .collect::<Vec<_>>(),
            vec![uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(5)]
        );
        let third = store
            .list_document_summaries(
                DocumentScope::All,
                Some(DocumentListCursor {
                    created_at: second[1].created_at,
                    id: second[1].id,
                }),
                2,
            )
            .await
            .unwrap();
        assert_eq!(
            third
                .iter()
                .map(|document| document.id.0)
                .collect::<Vec<_>>(),
            vec![uuid::Uuid::from_u128(4)]
        );
        assert!(store
            .list_document_summaries(DocumentScope::All, None, 0)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn document_project_fk_rejects_orphans_and_direct_project_deletion() {
        let (_dir, store) = temp_store().await;
        let orphan = sample_document(Some(ProjectId::new()));
        assert!(store.create_document(&orphan).await.is_err());

        let project = sample_project();
        store.create_project(&project).await.unwrap();
        let document = sample_document(Some(project.id));
        store.create_document(&document).await.unwrap();
        assert!(entities::project::Entity::delete_by_id(project.id.0)
            .exec(&store.conn)
            .await
            .is_err());
        assert!(store.get_project(project.id).await.unwrap().is_some());
        assert_eq!(
            store
                .get_document(document.id)
                .await
                .unwrap()
                .unwrap()
                .generation(),
            store
                .get_document_generation(document.id)
                .await
                .unwrap()
                .unwrap()
        );
    }

    #[tokio::test]
    async fn document_constraints_reject_invalid_catalog_state() {
        let (_dir, store) = temp_store().await;

        let mut empty_media_type = sample_document(None);
        empty_media_type.media_type.clear();
        assert!(store.create_document(&empty_media_type).await.is_err());

        let mut empty_source_uri = sample_document(None);
        empty_source_uri.source_uri = Some(String::new());
        assert!(store.create_document(&empty_source_uri).await.is_err());

        let mut invalid_revision = sample_document(None);
        invalid_revision.content_revision = 0;
        assert!(store.create_document(&invalid_revision).await.is_err());

        let mut future_index = sample_document(None);
        future_index.indexed_revision = Some(2);
        future_index.index_fingerprint = Some("v1".into());
        future_index.indexed_at = Some(Utc::now());
        assert!(store.create_document(&future_index).await.is_err());

        let mut partial_watermark = sample_document(None);
        partial_watermark.indexed_revision = Some(1);
        assert!(store.create_document(&partial_watermark).await.is_err());

        let mut empty_fingerprint = sample_document(None);
        empty_fingerprint.processing_status = DocumentProcessingStatus::Ready;
        empty_fingerprint.indexed_revision = Some(1);
        empty_fingerprint.index_fingerprint = Some(String::new());
        empty_fingerprint.indexed_at = Some(Utc::now());
        assert!(store.create_document(&empty_fingerprint).await.is_err());

        let mut oversized_fingerprint = sample_document(None);
        oversized_fingerprint.processing_status = DocumentProcessingStatus::Ready;
        oversized_fingerprint.indexed_revision = Some(1);
        oversized_fingerprint.index_fingerprint =
            Some("x".repeat(crate::model::DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN + 1));
        oversized_fingerprint.indexed_at = Some(Utc::now());
        assert!(store.create_document(&oversized_fingerprint).await.is_err());
    }

    #[tokio::test]
    async fn document_job_schema_enforces_delivery_and_idempotency_invariants() {
        let (_dir, store) = temp_store().await;
        let document = sample_document(None);
        store.create_document(&document).await.unwrap();
        let document = store.get_document(document.id).await.unwrap().unwrap();
        let now = DateTime::<Utc>::from_timestamp(1_752_148_800, 0).unwrap();
        let make_job =
            |document: &DocumentRecord, fingerprint: &str| entities::document_job::ActiveModel {
                id: Set(uuid::Uuid::new_v4()),
                document_id: Set(document.id.0),
                content_revision: Set(document.content_revision),
                revision_token: Set(document.revision_token),
                kind: Set(DocumentJobKind::Index.as_str().into()),
                status: Set(DocumentJobStatus::Queued.as_str().into()),
                pipeline_fingerprint: Set(fingerprint.into()),
                attempt_count: Set(0),
                max_attempts: Set(5),
                available_at: Set(now),
                lease_token: Set(None),
                lease_expires_at: Set(None),
                started_at: Set(None),
                finished_at: Set(None),
                last_error_code: Set(None),
                last_error_detail: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
            };
        let first = make_job(&document, "pipeline-v1")
            .insert(&store.conn)
            .await
            .unwrap();

        // A document has only one nonterminal pipeline stage at a time.
        assert!(make_job(&document, "pipeline-v2")
            .insert(&store.conn)
            .await
            .is_err());

        // State-dependent attempt, lease, and timestamp rules are independent.
        let another_document = sample_document(None);
        store.create_document(&another_document).await.unwrap();
        let another_document = store
            .get_document(another_document.id)
            .await
            .unwrap()
            .unwrap();
        let mut running_without_lease = make_job(&another_document, "pipeline-v1");
        running_without_lease.status = Set(DocumentJobStatus::Running.as_str().into());
        running_without_lease.attempt_count = Set(1);
        running_without_lease.started_at = Set(Some(now));
        assert!(running_without_lease.insert(&store.conn).await.is_err());
        let mut running_without_attempt = make_job(&another_document, "pipeline-v1");
        running_without_attempt.status = Set(DocumentJobStatus::Running.as_str().into());
        running_without_attempt.lease_token = Set(Some(uuid::Uuid::new_v4()));
        running_without_attempt.lease_expires_at = Set(Some(now + chrono::Duration::minutes(5)));
        assert!(running_without_attempt.insert(&store.conn).await.is_err());
        let mut exhausted_retry = make_job(&another_document, "pipeline-v1");
        exhausted_retry.status = Set(DocumentJobStatus::RetryWait.as_str().into());
        exhausted_retry.attempt_count = Set(5);
        exhausted_retry.started_at = Set(Some(now));
        assert!(exhausted_retry.insert(&store.conn).await.is_err());
        let mut terminal_without_finish = make_job(&another_document, "pipeline-v1");
        terminal_without_finish.status = Set(DocumentJobStatus::Failed.as_str().into());
        terminal_without_finish.attempt_count = Set(5);
        terminal_without_finish.started_at = Set(Some(now));
        assert!(terminal_without_finish.insert(&store.conn).await.is_err());
        let mut terminal_without_attempt = make_job(&another_document, "pipeline-v1");
        terminal_without_attempt.status = Set(DocumentJobStatus::Succeeded.as_str().into());
        terminal_without_attempt.finished_at = Set(Some(now));
        assert!(terminal_without_attempt.insert(&store.conn).await.is_err());

        let mut unknown_kind = make_job(&another_document, "pipeline-v1");
        unknown_kind.kind = Set("unknown".into());
        assert!(unknown_kind.insert(&store.conn).await.is_err());
        assert!(make_job(&another_document, "")
            .insert(&store.conn)
            .await
            .is_err());
        assert!(make_job(&another_document, &"x".repeat(513))
            .insert(&store.conn)
            .await
            .is_err());
        let mut oversized_error = make_job(&another_document, "pipeline-v1");
        oversized_error.last_error_code = Set(Some("e".repeat(129)));
        assert!(oversized_error.insert(&store.conn).await.is_err());
        let mut empty_error = make_job(&another_document, "pipeline-v1");
        empty_error.last_error_code = Set(Some(String::new()));
        assert!(empty_error.insert(&store.conn).await.is_err());
        let mut empty_detail = make_job(&another_document, "pipeline-v1");
        empty_detail.last_error_detail = Set(Some(String::new()));
        assert!(empty_detail.insert(&store.conn).await.is_err());
        let mut oversized_detail = make_job(&another_document, "pipeline-v1");
        oversized_detail.last_error_detail = Set(Some("d".repeat(4097)));
        assert!(oversized_detail.insert(&store.conn).await.is_err());

        let valid_running_document = sample_document(None);
        store
            .create_document(&valid_running_document)
            .await
            .unwrap();
        let valid_running_document = store
            .get_document(valid_running_document.id)
            .await
            .unwrap()
            .unwrap();
        let mut valid_running = make_job(&valid_running_document, "pipeline-v1");
        valid_running.status = Set(DocumentJobStatus::Running.as_str().into());
        valid_running.attempt_count = Set(1);
        valid_running.started_at = Set(Some(now));
        valid_running.lease_token = Set(Some(uuid::Uuid::new_v4()));
        valid_running.lease_expires_at = Set(Some(now + chrono::Duration::minutes(5)));
        valid_running.insert(&store.conn).await.unwrap();

        entities::document_job::Entity::update_many()
            .col_expr(
                entities::document_job::Column::Status,
                sea_orm::sea_query::Expr::value(DocumentJobStatus::Succeeded.as_str()),
            )
            .col_expr(
                entities::document_job::Column::FinishedAt,
                sea_orm::sea_query::Expr::value(Some(now)),
            )
            .col_expr(
                entities::document_job::Column::AttemptCount,
                sea_orm::sea_query::Expr::value(1),
            )
            .col_expr(
                entities::document_job::Column::StartedAt,
                sea_orm::sea_query::Expr::value(Some(now)),
            )
            .filter(entities::document_job::Column::Id.eq(first.id))
            .exec(&store.conn)
            .await
            .unwrap();

        // Terminal history frees the active slot, but the same semantic job is
        // still deduplicated by exact revision, kind, and pipeline fingerprint.
        assert!(make_job(&document, "pipeline-v1")
            .insert(&store.conn)
            .await
            .is_err());
        make_job(&document, "pipeline-v2")
            .insert(&store.conn)
            .await
            .unwrap();

        store.delete_document(document.id).await.unwrap();
        let remaining = entities::document_job::Entity::find()
            .filter(entities::document_job::Column::DocumentId.eq(document.id.0))
            .all(&store.conn)
            .await
            .unwrap();
        assert!(remaining.is_empty());
    }

    #[tokio::test]
    async fn source_revision_and_index_job_commit_and_supersede_together() {
        let (_dir, store) = temp_store().await;
        let document_id = DocumentId::new();
        let first_at = DateTime::<Utc>::from_timestamp(10_000, 0).unwrap();
        let first_source = DocumentUpsert {
            id: document_id,
            project_id: None,
            source_uri: Some("file:///async.txt".into()),
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "first".into(),
            updated_at: first_at,
        };

        let (first_revision, first_job) = store
            .upsert_document_and_enqueue_index(&first_source, "pipeline-v1", 5)
            .await
            .unwrap();
        assert_eq!(first_revision.content_revision, 1);
        assert_eq!(
            first_revision.processing_status,
            DocumentProcessingStatus::Queued
        );
        assert_eq!(first_job.document_id, first_revision.id);
        assert_eq!(first_job.content_revision, first_revision.content_revision);
        assert_eq!(first_job.revision_token, first_revision.revision_token);
        assert_eq!(
            store.get_document_job(first_job.id).await.unwrap(),
            Some(first_job.clone())
        );

        // A request retry after an ambiguous response must return the exact
        // committed revision/job even when the source timestamp was refreshed.
        let retry_source = DocumentUpsert {
            updated_at: first_at + chrono::Duration::seconds(1),
            ..first_source.clone()
        };
        let retried = store
            .upsert_document_and_enqueue_index(&retry_source, "pipeline-v1", 5)
            .await
            .unwrap();
        assert_eq!(retried, (first_revision.clone(), first_job.clone()));
        assert_eq!(
            store.list_document_jobs(document_id).await.unwrap().len(),
            1
        );

        // Simulate a claimed first job; a new source revision must fence and
        // terminally cancel it before the replacement queued job is inserted.
        let lease = uuid::Uuid::new_v4();
        let claimed_at = first_job.created_at;
        entities::document_job::Entity::update_many()
            .col_expr(
                entities::document_job::Column::Status,
                sea_orm::sea_query::Expr::value(DocumentJobStatus::Running.as_str()),
            )
            .col_expr(
                entities::document_job::Column::AttemptCount,
                sea_orm::sea_query::Expr::value(1),
            )
            .col_expr(
                entities::document_job::Column::LeaseToken,
                sea_orm::sea_query::Expr::value(Some(lease)),
            )
            .col_expr(
                entities::document_job::Column::LeaseExpiresAt,
                sea_orm::sea_query::Expr::value(Some(claimed_at + chrono::Duration::minutes(5))),
            )
            .col_expr(
                entities::document_job::Column::StartedAt,
                sea_orm::sea_query::Expr::value(Some(claimed_at)),
            )
            .filter(entities::document_job::Column::Id.eq(first_job.id.0))
            .exec(&store.conn)
            .await
            .unwrap();

        let second_at = DateTime::<Utc>::from_timestamp(20_000, 0).unwrap();
        let second_source = DocumentUpsert {
            canonical_text: "second".into(),
            updated_at: second_at,
            ..first_source
        };
        let (second_revision, second_job) = store
            .upsert_document_and_enqueue_index(&second_source, "pipeline-v1", 3)
            .await
            .unwrap();
        assert_eq!(second_revision.content_revision, 2);
        assert_eq!(second_job.content_revision, 2);
        assert_eq!(second_job.max_attempts, 3);
        assert_eq!(second_job.revision_token, second_revision.revision_token);

        let jobs = store.list_document_jobs(document_id).await.unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].id, first_job.id);
        assert_eq!(jobs[0].status, DocumentJobStatus::Cancelled);
        assert_eq!(jobs[0].lease_token, None);
        assert_eq!(jobs[0].lease_expires_at, None);
        assert!(jobs[0].finished_at.is_some_and(|at| at >= claimed_at));
        assert_eq!(jobs[0].finished_at, Some(second_job.created_at));
        assert_eq!(jobs[1], second_job);

        let unknown = DocumentUpsert {
            id: DocumentId::new(),
            ..second_source
        };
        assert!(store
            .upsert_document_and_enqueue_index(&unknown, "", 5)
            .await
            .is_err());
        assert_eq!(store.get_document(unknown.id).await.unwrap(), None);
        assert!(store
            .upsert_document_and_enqueue_index(&unknown, "pipeline-v1", 0)
            .await
            .is_err());
        assert_eq!(store.list_document_jobs(unknown.id).await.unwrap(), vec![]);

        let orphan = DocumentUpsert {
            id: DocumentId::new(),
            project_id: Some(ProjectId::new()),
            ..unknown
        };
        assert!(store
            .upsert_document_and_enqueue_index(&orphan, "pipeline-v1", 5)
            .await
            .is_err());
        assert_eq!(store.get_document(orphan.id).await.unwrap(), None);
        assert_eq!(store.list_document_jobs(orphan.id).await.unwrap(), vec![]);
    }

    #[tokio::test]
    async fn enqueue_rolls_back_source_when_job_insert_fails() {
        let (_dir, store) = temp_store().await;
        store
            .conn
            .execute_unprepared(
                "CREATE TRIGGER fail_document_job_insert
                 BEFORE INSERT ON document_job
                 BEGIN
                     SELECT RAISE(FAIL, 'injected document job failure');
                 END;",
            )
            .await
            .unwrap();

        let source = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: Some("file:///rollback.txt".into()),
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "must roll back".into(),
            updated_at: DateTime::<Utc>::from_timestamp(30_000, 0).unwrap(),
        };
        assert!(store
            .upsert_document_and_enqueue_index(&source, "pipeline-v1", 5)
            .await
            .is_err());
        assert_eq!(store.get_document(source.id).await.unwrap(), None);
        assert_eq!(
            store.get_document_generation(source.id).await.unwrap(),
            None
        );
        assert_eq!(store.list_document_jobs(source.id).await.unwrap(), vec![]);
    }

    #[tokio::test]
    async fn replacement_enqueue_failure_restores_source_and_live_job() {
        let (_dir, store) = temp_store().await;
        let source = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: Some("file:///replacement-rollback.txt".into()),
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "original".into(),
            updated_at: DateTime::<Utc>::from_timestamp(40_000, 0).unwrap(),
        };
        let (_, job) = store
            .upsert_document_and_enqueue_index(&source, "pipeline-v1", 5)
            .await
            .unwrap();
        let claimed_at = job.created_at;
        let lease_token = uuid::Uuid::new_v4();
        entities::document_job::Entity::update_many()
            .col_expr(
                entities::document_job::Column::Status,
                sea_orm::sea_query::Expr::value(DocumentJobStatus::Running.as_str()),
            )
            .col_expr(
                entities::document_job::Column::AttemptCount,
                sea_orm::sea_query::Expr::value(1),
            )
            .col_expr(
                entities::document_job::Column::LeaseToken,
                sea_orm::sea_query::Expr::value(Some(lease_token)),
            )
            .col_expr(
                entities::document_job::Column::LeaseExpiresAt,
                sea_orm::sea_query::Expr::value(Some(claimed_at + chrono::Duration::minutes(5))),
            )
            .col_expr(
                entities::document_job::Column::StartedAt,
                sea_orm::sea_query::Expr::value(Some(claimed_at)),
            )
            .filter(entities::document_job::Column::Id.eq(job.id.0))
            .exec(&store.conn)
            .await
            .unwrap();
        entities::document::Entity::update_many()
            .col_expr(
                entities::document::Column::ProcessingStatus,
                sea_orm::sea_query::Expr::value(DocumentProcessingStatus::Processing.as_str()),
            )
            .filter(entities::document::Column::Id.eq(source.id.0))
            .exec(&store.conn)
            .await
            .unwrap();
        let original_document = store.get_document(source.id).await.unwrap().unwrap();
        let original_job = store.get_document_job(job.id).await.unwrap().unwrap();
        let original_generation = store
            .get_document_generation(source.id)
            .await
            .unwrap()
            .unwrap();

        store
            .conn
            .execute_unprepared(
                "CREATE TRIGGER fail_replacement_document_job_insert
                 BEFORE INSERT ON document_job
                 BEGIN
                     SELECT RAISE(FAIL, 'injected replacement job failure');
                 END;",
            )
            .await
            .unwrap();
        let replacement = DocumentUpsert {
            canonical_text: "replacement".into(),
            updated_at: source.updated_at + chrono::Duration::seconds(1),
            ..source
        };
        assert!(store
            .upsert_document_and_enqueue_index(&replacement, "pipeline-v1", 5)
            .await
            .is_err());

        assert_eq!(
            store.get_document(replacement.id).await.unwrap(),
            Some(original_document)
        );
        assert_eq!(
            store.get_document_job(job.id).await.unwrap(),
            Some(original_job.clone())
        );
        assert_eq!(
            store.list_document_jobs(replacement.id).await.unwrap(),
            vec![original_job]
        );
        assert_eq!(
            store.get_document_generation(replacement.id).await.unwrap(),
            Some(original_generation)
        );
    }

    #[tokio::test]
    async fn document_delete_failure_rolls_back_tombstone_source_and_jobs() {
        let (_dir, store) = temp_store().await;
        let source = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: Some("file:///delete-rollback.txt".into()),
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "retain on failure".into(),
            updated_at: DateTime::<Utc>::from_timestamp(45_000, 0).unwrap(),
        };
        let (document, job) = store
            .upsert_document_and_enqueue_index(&source, "pipeline-v1", 5)
            .await
            .unwrap();
        store
            .conn
            .execute_unprepared(
                "CREATE TRIGGER fail_document_delete
                 BEFORE DELETE ON document
                 BEGIN
                     SELECT RAISE(FAIL, 'injected document delete failure');
                 END;",
            )
            .await
            .unwrap();

        assert!(store.delete_document(source.id).await.is_err());
        assert_eq!(
            store.get_document(source.id).await.unwrap(),
            Some(document.clone())
        );
        assert_eq!(
            store.get_document_generation(source.id).await.unwrap(),
            Some(document.generation())
        );
        assert_eq!(store.get_document_job(job.id).await.unwrap(), Some(job));
    }

    #[tokio::test]
    async fn concurrent_source_enqueues_leave_one_current_revision_and_job() {
        let (_dir, store) = temp_store().await;
        let store = std::sync::Arc::new(store);
        let document_id = DocumentId::new();
        let writes = (1..=8).map(|revision| {
            let store = store.clone();
            tokio::spawn(async move {
                store
                    .upsert_document_and_enqueue_index(
                        &DocumentUpsert {
                            id: document_id,
                            project_id: None,
                            source_uri: Some("file:///concurrent-async.txt".into()),
                            media_type: "text/plain".into(),
                            title: None,
                            canonical_text: format!("writer {revision}"),
                            updated_at: DateTime::<Utc>::from_timestamp(revision, 0).unwrap(),
                        },
                        "pipeline-v1",
                        5,
                    )
                    .await
            })
        });

        let mut revisions = Vec::new();
        for result in futures::future::join_all(writes).await {
            let (document, job) = result.unwrap().unwrap();
            assert_eq!(job.content_revision, document.content_revision);
            assert_eq!(job.revision_token, document.revision_token);
            revisions.push(document.content_revision);
        }
        revisions.sort_unstable();
        assert_eq!(revisions, (1..=8).collect::<Vec<_>>());

        let current = store.get_document(document_id).await.unwrap().unwrap();
        let jobs = store.list_document_jobs(document_id).await.unwrap();
        assert_eq!(jobs.len(), 8);
        let active: Vec<_> = jobs
            .iter()
            .filter(|job| job.status == DocumentJobStatus::Queued)
            .collect();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].content_revision, current.content_revision);
        assert_eq!(active[0].revision_token, current.revision_token);
        assert_eq!(
            jobs.iter()
                .filter(|job| job.status == DocumentJobStatus::Cancelled)
                .count(),
            7
        );
    }

    #[tokio::test]
    async fn concurrent_identical_first_enqueues_reuse_one_revision_and_job() {
        let (_dir, store) = temp_store().await;
        let store = std::sync::Arc::new(store);
        let document_id = DocumentId::new();
        let writes = (1..=8).map(|request| {
            let store = store.clone();
            tokio::spawn(async move {
                store
                    .upsert_document_and_enqueue_index(
                        &DocumentUpsert {
                            id: document_id,
                            project_id: None,
                            source_uri: Some("file:///identical-concurrent.txt".into()),
                            media_type: "text/plain".into(),
                            title: None,
                            canonical_text: "same source".into(),
                            // Source observation time is deliberately not part of
                            // semantic request identity.
                            updated_at: DateTime::<Utc>::from_timestamp(request, 0).unwrap(),
                        },
                        "pipeline-v1",
                        5,
                    )
                    .await
            })
        });

        let results = futures::future::join_all(writes).await;
        let first = results[0].as_ref().unwrap().as_ref().unwrap().clone();
        for result in results {
            assert_eq!(result.unwrap().unwrap(), first);
        }
        assert_eq!(first.0.content_revision, 1);
        assert_eq!(
            store.list_document_jobs(document_id).await.unwrap(),
            vec![first.1]
        );
    }

    #[tokio::test]
    async fn document_job_claim_and_heartbeat_require_the_live_lease() {
        let (_dir, store) = temp_store().await;
        let source = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: Some("file:///claim.txt".into()),
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "claim me".into(),
            updated_at: DateTime::<Utc>::from_timestamp(50_000, 0).unwrap(),
        };
        let (_, queued) = store
            .upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)
            .await
            .unwrap();
        let now = queued.available_at + chrono::Duration::seconds(1);
        assert!(store.claim_document_job(now, now).await.is_err());

        let lease_expires_at = now + chrono::Duration::minutes(5);
        let claimed = store
            .claim_document_job(now, lease_expires_at)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, queued.id);
        assert_eq!(claimed.status, DocumentJobStatus::Running);
        assert_eq!(claimed.attempt_count, 1);
        assert_eq!(claimed.started_at, Some(now));
        assert_eq!(claimed.lease_expires_at, Some(lease_expires_at));
        let lease_token = claimed.lease_token.unwrap();
        assert_eq!(
            store
                .get_document(source.id)
                .await
                .unwrap()
                .unwrap()
                .processing_status,
            DocumentProcessingStatus::Processing
        );
        assert_eq!(
            store
                .claim_document_job(now, lease_expires_at)
                .await
                .unwrap(),
            None
        );

        let heartbeat_at = now + chrono::Duration::minutes(1);
        assert!(!store
            .heartbeat_document_job(
                claimed.id,
                uuid::Uuid::new_v4(),
                heartbeat_at,
                lease_expires_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap());
        assert!(!store
            .heartbeat_document_job(claimed.id, lease_token, heartbeat_at, lease_expires_at)
            .await
            .unwrap());
        assert!(!store
            .heartbeat_document_job(
                claimed.id,
                lease_token,
                now - chrono::Duration::seconds(1),
                lease_expires_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap());
        assert!(store
            .heartbeat_document_job(claimed.id, lease_token, heartbeat_at, heartbeat_at)
            .await
            .is_err());

        let extended = lease_expires_at + chrono::Duration::minutes(5);
        assert!(store
            .heartbeat_document_job(claimed.id, lease_token, heartbeat_at, extended)
            .await
            .unwrap());
        let heartbeated = store.get_document_job(claimed.id).await.unwrap().unwrap();
        assert_eq!(heartbeated.lease_expires_at, Some(extended));
        assert_eq!(heartbeated.updated_at, heartbeat_at);
        assert!(!store
            .heartbeat_document_job(
                claimed.id,
                lease_token,
                extended,
                extended + chrono::Duration::minutes(5),
            )
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn live_document_job_completion_atomically_publishes_ready_watermark() {
        let (_dir, store) = temp_store().await;
        let source = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: Some("file:///complete-job.txt".into()),
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "complete me".into(),
            updated_at: DateTime::<Utc>::from_timestamp(55_000, 0).unwrap(),
        };
        let (revision, queued) = store
            .upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)
            .await
            .unwrap();
        let claimed_at = queued.available_at + chrono::Duration::seconds(1);
        let lease_expires_at = claimed_at + chrono::Duration::minutes(5);
        let claimed = store
            .claim_document_job(claimed_at, lease_expires_at)
            .await
            .unwrap()
            .unwrap();
        let completed_at = claimed_at + chrono::Duration::minutes(1);
        assert!(!store
            .complete_document_index_job(claimed.id, uuid::Uuid::new_v4(), completed_at)
            .await
            .unwrap());
        assert!(!store
            .complete_document_index_job(
                claimed.id,
                claimed.lease_token.unwrap(),
                claimed_at - chrono::Duration::seconds(1),
            )
            .await
            .unwrap());
        assert!(store
            .complete_document_index_job(claimed.id, claimed.lease_token.unwrap(), completed_at,)
            .await
            .unwrap());
        assert!(!store
            .complete_document_index_job(claimed.id, claimed.lease_token.unwrap(), completed_at,)
            .await
            .unwrap());

        let succeeded = store.get_document_job(claimed.id).await.unwrap().unwrap();
        assert_eq!(succeeded.status, DocumentJobStatus::Succeeded);
        assert_eq!(succeeded.lease_token, None);
        assert_eq!(succeeded.lease_expires_at, None);
        assert_eq!(succeeded.finished_at, Some(completed_at));
        assert_eq!(succeeded.last_error_code, None);
        assert_eq!(succeeded.last_error_detail, None);
        let ready = store.get_document(source.id).await.unwrap().unwrap();
        assert_eq!(ready.processing_status, DocumentProcessingStatus::Ready);
        assert_eq!(ready.indexed_revision, Some(revision.content_revision));
        assert_eq!(ready.index_fingerprint.as_deref(), Some("pipeline-v1"));
        assert_eq!(ready.indexed_at, Some(completed_at));
    }

    #[tokio::test]
    async fn live_document_job_failure_retries_then_fails_permanently() {
        let (_dir, store) = temp_store().await;
        let source = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: Some("file:///fail-job.txt".into()),
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "fail and retry".into(),
            updated_at: DateTime::<Utc>::from_timestamp(56_000, 0).unwrap(),
        };
        let (_, queued) = store
            .upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)
            .await
            .unwrap();
        let first_at = queued.available_at + chrono::Duration::seconds(1);
        let first = store
            .claim_document_job(first_at, first_at + chrono::Duration::minutes(5))
            .await
            .unwrap()
            .unwrap();
        let failed_at = first_at + chrono::Duration::minutes(1);
        let retry_at = failed_at + chrono::Duration::minutes(2);
        assert_eq!(
            store
                .record_document_job_failure(
                    first.id,
                    first.lease_token.unwrap(),
                    failed_at,
                    Some(retry_at),
                    "embed_timeout",
                    Some("provider timed out"),
                )
                .await
                .unwrap(),
            Some(DocumentJobStatus::RetryWait)
        );
        let waiting = store.get_document_job(first.id).await.unwrap().unwrap();
        assert_eq!(waiting.status, DocumentJobStatus::RetryWait);
        assert_eq!(waiting.attempt_count, 1);
        assert_eq!(waiting.available_at, retry_at);
        assert_eq!(waiting.finished_at, None);
        assert_eq!(waiting.lease_token, None);
        assert_eq!(waiting.last_error_code.as_deref(), Some("embed_timeout"));
        assert_eq!(
            store
                .get_document(source.id)
                .await
                .unwrap()
                .unwrap()
                .processing_status,
            DocumentProcessingStatus::Queued
        );
        assert_eq!(
            store
                .claim_document_job(failed_at, failed_at + chrono::Duration::minutes(5))
                .await
                .unwrap(),
            None
        );

        let second = store
            .claim_document_job(retry_at, retry_at + chrono::Duration::minutes(5))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.attempt_count, 2);
        let permanent_at = retry_at + chrono::Duration::minutes(1);
        assert_eq!(
            store
                .record_document_job_failure(
                    second.id,
                    second.lease_token.unwrap(),
                    permanent_at,
                    None,
                    "invalid_source",
                    None,
                )
                .await
                .unwrap(),
            Some(DocumentJobStatus::Failed)
        );
        let failed = store.get_document_job(second.id).await.unwrap().unwrap();
        assert_eq!(failed.status, DocumentJobStatus::Failed);
        assert_eq!(failed.finished_at, Some(permanent_at));
        assert_eq!(failed.last_error_code.as_deref(), Some("invalid_source"));
        assert_eq!(
            store
                .get_document(source.id)
                .await
                .unwrap()
                .unwrap()
                .processing_status,
            DocumentProcessingStatus::Failed
        );
    }

    #[tokio::test]
    async fn document_job_failure_validates_details_and_exhausts_retry_budget() {
        let (_dir, store) = temp_store().await;
        let source = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: Some("file:///exhaust-failure.txt".into()),
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "one attempt".into(),
            updated_at: DateTime::<Utc>::from_timestamp(56_500, 0).unwrap(),
        };
        let (_, queued) = store
            .upsert_document_and_enqueue_index(&source, "pipeline-v1", 1)
            .await
            .unwrap();
        let claimed_at = queued.available_at + chrono::Duration::seconds(1);
        let claimed = store
            .claim_document_job(claimed_at, claimed_at + chrono::Duration::minutes(5))
            .await
            .unwrap()
            .unwrap();
        let failed_at = claimed_at + chrono::Duration::minutes(1);
        assert!(store
            .record_document_job_failure(
                claimed.id,
                claimed.lease_token.unwrap(),
                failed_at,
                Some(failed_at),
                "timeout",
                None,
            )
            .await
            .is_err());
        assert!(store
            .record_document_job_failure(
                claimed.id,
                claimed.lease_token.unwrap(),
                failed_at,
                None,
                "",
                None,
            )
            .await
            .is_err());
        assert!(store
            .record_document_job_failure(
                claimed.id,
                claimed.lease_token.unwrap(),
                failed_at,
                None,
                "timeout",
                Some(""),
            )
            .await
            .is_err());
        assert_eq!(
            store
                .record_document_job_failure(
                    claimed.id,
                    uuid::Uuid::new_v4(),
                    failed_at,
                    None,
                    "timeout",
                    None,
                )
                .await
                .unwrap(),
            None
        );

        assert_eq!(
            store
                .record_document_job_failure(
                    claimed.id,
                    claimed.lease_token.unwrap(),
                    failed_at,
                    Some(failed_at + chrono::Duration::minutes(1)),
                    "timeout",
                    Some("retry budget is exhausted"),
                )
                .await
                .unwrap(),
            Some(DocumentJobStatus::Failed)
        );
        let failed = store.get_document_job(claimed.id).await.unwrap().unwrap();
        assert_eq!(failed.status, DocumentJobStatus::Failed);
        assert_eq!(failed.finished_at, Some(failed_at));
    }

    #[tokio::test]
    async fn explicit_retry_only_revives_current_failed_index_job() {
        let (_dir, store) = temp_store().await;
        let source = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: None,
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "retry me".into(),
            updated_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
        };
        let (_, queued) = store
            .upsert_document_and_enqueue_index(&source, "pipeline-v1", 2)
            .await
            .unwrap();
        assert_eq!(
            store
                .retry_document_index_job(source.id, "pipeline-v1", 9)
                .await
                .unwrap(),
            Some(queued.clone())
        );

        let claim_at = queued.available_at + chrono::Duration::seconds(1);
        let running = store
            .claim_document_job(claim_at, claim_at + chrono::Duration::minutes(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            store
                .retry_document_index_job(source.id, "pipeline-v1", 9)
                .await
                .unwrap(),
            Some(running.clone())
        );
        assert_eq!(
            store
                .record_document_job_failure(
                    running.id,
                    running.lease_token.unwrap(),
                    claim_at + chrono::Duration::seconds(1),
                    None,
                    "embedding_failed",
                    Some("service unavailable"),
                )
                .await
                .unwrap(),
            Some(DocumentJobStatus::Failed)
        );
        assert_eq!(
            store
                .retry_document_index_job(source.id, "other-pipeline", 4)
                .await
                .unwrap(),
            None
        );

        let retried = store
            .retry_document_index_job(source.id, "pipeline-v1", 4)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retried.id, queued.id);
        assert_eq!(retried.status, DocumentJobStatus::Queued);
        assert_eq!(retried.attempt_count, 0);
        assert_eq!(retried.max_attempts, 4);
        assert_eq!(retried.lease_token, None);
        assert_eq!(retried.lease_expires_at, None);
        assert_eq!(retried.started_at, None);
        assert_eq!(retried.finished_at, None);
        assert_eq!(retried.last_error_code, None);
        assert_eq!(retried.last_error_detail, None);
        let document = store.get_document(source.id).await.unwrap().unwrap();
        assert_eq!(document.processing_status, DocumentProcessingStatus::Queued);
        assert_eq!(document.indexed_revision, None);
        assert_eq!(document.index_fingerprint, None);
        assert_eq!(document.indexed_at, None);
        assert_eq!(
            store
                .retry_document_index_job(source.id, "pipeline-v1", 8)
                .await
                .unwrap(),
            Some(retried.clone())
        );

        let retry_claim_at = retried.available_at + chrono::Duration::seconds(1);
        let retry_running = store
            .claim_document_job(
                retry_claim_at,
                retry_claim_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(store
            .complete_document_index_job(
                retry_running.id,
                retry_running.lease_token.unwrap(),
                retry_claim_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap());
        assert_eq!(
            store
                .retry_document_index_job(source.id, "pipeline-v1", 4)
                .await
                .unwrap(),
            None
        );

        let replacement = DocumentUpsert {
            canonical_text: "replacement".into(),
            updated_at: source.updated_at + chrono::Duration::seconds(1),
            ..source.clone()
        };
        let (_, cancelled) = store
            .upsert_document_and_enqueue_index(&replacement, "pipeline-v2", 2)
            .await
            .unwrap();
        assert_eq!(
            store
                .retry_document_index_job(replacement.id, "pipeline-v1", 4)
                .await
                .unwrap(),
            None
        );
        store
            .upsert_document_and_enqueue_index(
                &DocumentUpsert {
                    canonical_text: "newer replacement".into(),
                    updated_at: source.updated_at + chrono::Duration::seconds(2),
                    ..replacement
                },
                "pipeline-v3",
                2,
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .get_document_job(cancelled.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            DocumentJobStatus::Cancelled
        );
        assert_eq!(
            store
                .retry_document_index_job(source.id, "pipeline-v2", 4)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn completion_document_failure_rolls_back_the_job_transition() {
        let (_dir, store) = temp_store().await;
        let source = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: Some("file:///complete-rollback.txt".into()),
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "roll back completion".into(),
            updated_at: DateTime::<Utc>::from_timestamp(57_000, 0).unwrap(),
        };
        let (_, queued) = store
            .upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)
            .await
            .unwrap();
        let claimed_at = queued.available_at + chrono::Duration::seconds(1);
        let claimed = store
            .claim_document_job(claimed_at, claimed_at + chrono::Duration::minutes(5))
            .await
            .unwrap()
            .unwrap();
        store
            .conn
            .execute_unprepared(
                "CREATE TRIGGER fail_document_ready
                 BEFORE UPDATE OF processing_status ON document
                 WHEN NEW.processing_status = 'ready'
                 BEGIN
                     SELECT RAISE(FAIL, 'injected document completion failure');
                 END;",
            )
            .await
            .unwrap();

        let completed_at = claimed_at + chrono::Duration::minutes(1);
        assert!(store
            .complete_document_index_job(claimed.id, claimed.lease_token.unwrap(), completed_at,)
            .await
            .is_err());
        assert_eq!(
            store.get_document_job(claimed.id).await.unwrap(),
            Some(claimed)
        );
        let document = store.get_document(source.id).await.unwrap().unwrap();
        assert_eq!(
            document.processing_status,
            DocumentProcessingStatus::Processing
        );
        assert_eq!(document.indexed_revision, None);
    }

    #[tokio::test]
    async fn expired_document_job_leases_are_reclaimed_then_fail_at_the_attempt_limit() {
        let (_dir, store) = temp_store().await;
        let source = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: Some("file:///lease-recovery.txt".into()),
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "recover me".into(),
            updated_at: DateTime::<Utc>::from_timestamp(60_000, 0).unwrap(),
        };
        let (_, queued) = store
            .upsert_document_and_enqueue_index(&source, "pipeline-v1", 2)
            .await
            .unwrap();
        let first_at = queued.available_at + chrono::Duration::seconds(1);
        let first_expiry = first_at + chrono::Duration::minutes(1);
        let first = store
            .claim_document_job(first_at, first_expiry)
            .await
            .unwrap()
            .unwrap();

        let second_expiry = first_expiry + chrono::Duration::minutes(1);
        let second = store
            .claim_document_job(first_expiry, second_expiry)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.id, first.id);
        assert_eq!(second.attempt_count, 2);
        assert_eq!(second.started_at, first.started_at);
        assert_ne!(second.lease_token, first.lease_token);
        assert_eq!(second.lease_expires_at, Some(second_expiry));
        assert!(!store
            .heartbeat_document_job(
                first.id,
                first.lease_token.unwrap(),
                first_expiry,
                second_expiry + chrono::Duration::minutes(1),
            )
            .await
            .unwrap());

        let fallback_source = DocumentUpsert {
            id: DocumentId::new(),
            source_uri: Some("file:///after-exhausted-lease.txt".into()),
            canonical_text: "claim after cleanup".into(),
            ..source.clone()
        };
        let (_, fallback) = store
            .upsert_document_and_enqueue_index(&fallback_source, "pipeline-v1", 2)
            .await
            .unwrap();
        let fallback_due = second_expiry + chrono::Duration::seconds(1);
        entities::document_job::Entity::update_many()
            .col_expr(
                entities::document_job::Column::AvailableAt,
                sea_orm::sea_query::Expr::value(fallback_due),
            )
            .filter(entities::document_job::Column::Id.eq(fallback.id.0))
            .exec(&store.conn)
            .await
            .unwrap();
        let final_claim_at = fallback_due + chrono::Duration::seconds(1);
        let claimed_after_cleanup = store
            .claim_document_job(
                final_claim_at,
                final_claim_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed_after_cleanup.id, fallback.id);

        let failed = store.get_document_job(first.id).await.unwrap().unwrap();
        assert_eq!(failed.status, DocumentJobStatus::Failed);
        assert_eq!(failed.attempt_count, 2);
        assert_eq!(failed.lease_token, None);
        assert_eq!(failed.lease_expires_at, None);
        assert_eq!(failed.finished_at, Some(final_claim_at));
        assert_eq!(failed.last_error_code.as_deref(), Some("lease_expired"));
        assert_eq!(
            store
                .get_document(source.id)
                .await
                .unwrap()
                .unwrap()
                .processing_status,
            DocumentProcessingStatus::Failed
        );
    }

    #[tokio::test]
    async fn claim_cancels_a_superseded_candidate_then_claims_the_next_job() {
        let (_dir, store) = temp_store().await;
        let first_source = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: Some("file:///stale-claim.txt".into()),
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "stale".into(),
            updated_at: DateTime::<Utc>::from_timestamp(70_000, 0).unwrap(),
        };
        let (_, stale_job) = store
            .upsert_document_and_enqueue_index(&first_source, "pipeline-v1", 3)
            .await
            .unwrap();
        let second_source = DocumentUpsert {
            id: DocumentId::new(),
            source_uri: Some("file:///next-claim.txt".into()),
            canonical_text: "next".into(),
            ..first_source.clone()
        };
        let (_, next_job) = store
            .upsert_document_and_enqueue_index(&second_source, "pipeline-v1", 3)
            .await
            .unwrap();

        entities::document::Entity::update_many()
            .col_expr(
                entities::document::Column::ContentRevision,
                sea_orm::sea_query::Expr::value(2_i64),
            )
            .col_expr(
                entities::document::Column::RevisionToken,
                sea_orm::sea_query::Expr::value(uuid::Uuid::new_v4()),
            )
            .filter(entities::document::Column::Id.eq(first_source.id.0))
            .exec(&store.conn)
            .await
            .unwrap();

        let now = next_job.available_at + chrono::Duration::seconds(1);
        let claimed = store
            .claim_document_job(now, now + chrono::Duration::minutes(5))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, next_job.id);
        assert_eq!(
            store
                .get_document_job(stale_job.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            DocumentJobStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn claim_reports_exact_identity_status_corruption_without_cancelling() {
        let (_dir, store) = temp_store().await;
        let source = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: Some("file:///claim-corruption.txt".into()),
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "exact but inconsistent".into(),
            updated_at: DateTime::<Utc>::from_timestamp(75_000, 0).unwrap(),
        };
        let (_, queued) = store
            .upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)
            .await
            .unwrap();
        entities::document::Entity::update_many()
            .col_expr(
                entities::document::Column::ProcessingStatus,
                sea_orm::sea_query::Expr::value(DocumentProcessingStatus::Processing.as_str()),
            )
            .filter(entities::document::Column::Id.eq(source.id.0))
            .exec(&store.conn)
            .await
            .unwrap();

        let now = queued.available_at + chrono::Duration::seconds(1);
        assert!(store
            .claim_document_job(now, now + chrono::Duration::minutes(5))
            .await
            .is_err());
        assert_eq!(
            store.get_document_job(queued.id).await.unwrap(),
            Some(queued)
        );
    }

    #[tokio::test]
    async fn claim_orders_expired_and_queued_jobs_by_effective_due_time() {
        let (_dir, store) = temp_store().await;
        let running_source = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: Some("file:///expired-first.txt".into()),
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "expired first".into(),
            updated_at: DateTime::<Utc>::from_timestamp(76_000, 0).unwrap(),
        };
        let (_, running_queued) = store
            .upsert_document_and_enqueue_index(&running_source, "pipeline-v1", 3)
            .await
            .unwrap();
        let first_claim_at = running_queued.available_at + chrono::Duration::seconds(1);
        let first_expiry = first_claim_at + chrono::Duration::minutes(1);
        let running = store
            .claim_document_job(first_claim_at, first_expiry)
            .await
            .unwrap()
            .unwrap();

        let queued_source = DocumentUpsert {
            id: DocumentId::new(),
            source_uri: Some("file:///queued-second.txt".into()),
            canonical_text: "queued second".into(),
            ..running_source
        };
        let (_, queued) = store
            .upsert_document_and_enqueue_index(&queued_source, "pipeline-v1", 3)
            .await
            .unwrap();
        // The running job's original `available_at` is older, but its effective
        // due time is the later lease expiry. The queued job must win.
        let queued_due = first_expiry - chrono::Duration::seconds(30);
        entities::document_job::Entity::update_many()
            .col_expr(
                entities::document_job::Column::AvailableAt,
                sea_orm::sea_query::Expr::value(queued_due),
            )
            .filter(entities::document_job::Column::Id.eq(queued.id.0))
            .exec(&store.conn)
            .await
            .unwrap();

        let now = first_expiry + chrono::Duration::minutes(1);
        let claimed = store
            .claim_document_job(now, now + chrono::Duration::minutes(5))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, queued.id);
        assert_eq!(claimed.attempt_count, 1);
        assert_eq!(
            store
                .get_document_job(running.id)
                .await
                .unwrap()
                .unwrap()
                .attempt_count,
            1
        );
    }

    #[tokio::test]
    async fn concurrent_document_job_claimers_never_share_a_job() {
        let (_dir, store) = temp_store().await;
        let store = std::sync::Arc::new(store);
        let mut latest_available_at = DateTime::<Utc>::from_timestamp(0, 0).unwrap();
        for index in 0..6 {
            let (_, job) = store
                .upsert_document_and_enqueue_index(
                    &DocumentUpsert {
                        id: DocumentId::new(),
                        project_id: None,
                        source_uri: Some(format!("file:///claim-{index}.txt")),
                        media_type: "text/plain".into(),
                        title: None,
                        canonical_text: format!("document {index}"),
                        updated_at: DateTime::<Utc>::from_timestamp(80_000 + index, 0).unwrap(),
                    },
                    "pipeline-v1",
                    3,
                )
                .await
                .unwrap();
            latest_available_at = latest_available_at.max(job.available_at);
        }
        let now = latest_available_at + chrono::Duration::seconds(1);
        let claims = (0..12).map(|_| {
            let store = store.clone();
            tokio::spawn(async move {
                store
                    .claim_document_job(now, now + chrono::Duration::minutes(5))
                    .await
            })
        });

        let mut claimed_ids = Vec::new();
        for result in futures::future::join_all(claims).await {
            if let Some(job) = result.unwrap().unwrap() {
                claimed_ids.push(job.id);
            }
        }
        assert_eq!(claimed_ids.len(), 6);
        claimed_ids.sort_by_key(|id| id.0);
        claimed_ids.dedup();
        assert_eq!(claimed_ids.len(), 6);
    }

    #[tokio::test]
    async fn concurrent_claim_and_replacement_enqueue_preserve_one_current_job() {
        let (_dir, store) = temp_store().await;
        let store = std::sync::Arc::new(store);
        for iteration in 0..8 {
            let source = DocumentUpsert {
                id: DocumentId::new(),
                project_id: None,
                source_uri: Some(format!("file:///claim-enqueue-{iteration}.txt")),
                media_type: "text/plain".into(),
                title: None,
                canonical_text: "first".into(),
                updated_at: DateTime::<Utc>::from_timestamp(90_000 + iteration * 2, 0).unwrap(),
            };
            let (_, queued) = store
                .upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)
                .await
                .unwrap();
            let now = queued.available_at + chrono::Duration::minutes(1);
            let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));

            let claim_store = store.clone();
            let claim_barrier = barrier.clone();
            let claim = tokio::spawn(async move {
                claim_barrier.wait().await;
                claim_store
                    .claim_document_job(now, now + chrono::Duration::minutes(5))
                    .await
            });
            let enqueue_store = store.clone();
            let enqueue_barrier = barrier.clone();
            let replacement = DocumentUpsert {
                canonical_text: "replacement".into(),
                updated_at: source.updated_at + chrono::Duration::seconds(1),
                ..source.clone()
            };
            let enqueue = tokio::spawn(async move {
                enqueue_barrier.wait().await;
                enqueue_store
                    .upsert_document_and_enqueue_index(&replacement, "pipeline-v1", 3)
                    .await
            });
            barrier.wait().await;

            claim.await.unwrap().unwrap().unwrap();
            enqueue.await.unwrap().unwrap();
            let current = store.get_document(source.id).await.unwrap().unwrap();
            let jobs = store.list_document_jobs(source.id).await.unwrap();
            let active: Vec<_> = jobs
                .iter()
                .filter(|job| !job.status.is_terminal())
                .collect();
            assert_eq!(active.len(), 1);
            assert_eq!(active[0].content_revision, current.content_revision);
            assert_eq!(active[0].revision_token, current.revision_token);
        }
    }

    #[tokio::test]
    async fn concurrent_delete_and_enqueue_leave_one_coherent_generation() {
        let (_dir, store) = temp_store().await;
        let store = std::sync::Arc::new(store);
        for iteration in 0..8 {
            let source = DocumentUpsert {
                id: DocumentId::new(),
                project_id: None,
                source_uri: Some(format!("file:///delete-enqueue-{iteration}.txt")),
                media_type: "text/plain".into(),
                title: None,
                canonical_text: "first".into(),
                updated_at: DateTime::<Utc>::from_timestamp(110_000 + iteration * 2, 0).unwrap(),
            };
            let (first, _) = store
                .upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)
                .await
                .unwrap();
            assert_eq!(first.content_revision, 1);
            let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));

            let delete_store = store.clone();
            let delete_barrier = barrier.clone();
            let id = source.id;
            let deletion = tokio::spawn(async move {
                delete_barrier.wait().await;
                delete_store.delete_document(id).await
            });
            let enqueue_store = store.clone();
            let enqueue_barrier = barrier.clone();
            let replacement = DocumentUpsert {
                canonical_text: "replacement".into(),
                updated_at: source.updated_at + chrono::Duration::seconds(1),
                ..source.clone()
            };
            let enqueue = tokio::spawn(async move {
                enqueue_barrier.wait().await;
                enqueue_store
                    .upsert_document_and_enqueue_index(&replacement, "pipeline-v1", 3)
                    .await
            });
            barrier.wait().await;

            let tombstone = deletion.await.unwrap().unwrap();
            let (enqueued, _) = enqueue.await.unwrap().unwrap();
            let retained = store
                .get_document_generation(source.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(retained.content_revision, 3);
            assert_eq!(
                retained.content_revision,
                tombstone.content_revision.max(enqueued.content_revision)
            );
            match store.get_document(source.id).await.unwrap() {
                Some(current) => {
                    assert_eq!(current.generation(), retained);
                    let jobs = store.list_document_jobs(source.id).await.unwrap();
                    assert_eq!(jobs.len(), 1);
                    assert_eq!(jobs[0].generation(), retained);
                }
                None => {
                    assert_eq!(retained, tombstone);
                    assert!(store
                        .list_document_jobs(source.id)
                        .await
                        .unwrap()
                        .is_empty());
                }
            }
        }
    }

    #[tokio::test]
    async fn document_upsert_revisions_and_index_watermark_are_compare_and_set() {
        let (_dir, store) = temp_store().await;
        let id = DocumentId::derive("file:///report.txt");
        let first_at = DateTime::<Utc>::from_timestamp(10_000, 0).unwrap();
        let first = DocumentUpsert {
            id,
            project_id: None,
            source_uri: Some("file:///report.txt".into()),
            media_type: "text/plain".into(),
            title: Some("Report".into()),
            canonical_text: "first version".into(),
            updated_at: first_at,
        };

        let revision_one = store.upsert_document(&first).await.unwrap();
        assert_eq!(revision_one.content_revision, 1);
        assert_eq!(revision_one.created_at, first_at);
        assert_eq!(revision_one.indexed_revision, None);
        assert_eq!(
            revision_one.processing_status,
            DocumentProcessingStatus::Queued
        );
        assert!(store
            .mark_document_indexed(id, 1, revision_one.revision_token, "", first_at)
            .await
            .is_err());
        assert!(store
            .mark_document_indexed(
                id,
                1,
                revision_one.revision_token,
                &"x".repeat(crate::model::DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN + 1),
                first_at,
            )
            .await
            .is_err());
        assert!(store
            .mark_document_indexed(id, 1, revision_one.revision_token, "index-v1", first_at,)
            .await
            .unwrap());

        let second_at = DateTime::<Utc>::from_timestamp(20_000, 0).unwrap();
        let second = DocumentUpsert {
            canonical_text: "second version".into(),
            updated_at: second_at,
            ..first
        };
        let revision_two = store.upsert_document(&second).await.unwrap();
        assert_eq!(revision_two.content_revision, 2);
        assert_eq!(revision_two.created_at, first_at);
        assert_eq!(revision_two.updated_at, second_at);
        assert_ne!(revision_two.revision_token, revision_one.revision_token);
        assert_eq!(revision_two.indexed_revision, None);
        assert_eq!(
            revision_two.processing_status,
            DocumentProcessingStatus::Queued
        );
        assert_eq!(revision_two.index_fingerprint, None);
        assert_eq!(revision_two.indexed_at, None);

        // A late indexer for revision one cannot mark revision two current.
        assert!(!store
            .mark_document_indexed(id, 1, revision_one.revision_token, "stale", second_at)
            .await
            .unwrap());
        assert!(store
            .mark_document_indexed(id, 2, revision_two.revision_token, "index-v2", second_at,)
            .await
            .unwrap());
        let indexed = store.get_document(id).await.unwrap().unwrap();
        assert_eq!(indexed.indexed_revision, Some(2));
        assert_eq!(indexed.processing_status, DocumentProcessingStatus::Ready);
        assert_eq!(indexed.index_fingerprint.as_deref(), Some("index-v2"));
        assert_eq!(indexed.indexed_at, Some(second_at));
        assert!(!store
            .clear_document_index(id, 2, revision_one.revision_token)
            .await
            .unwrap());
        assert!(store
            .clear_document_index(id, 2, revision_two.revision_token)
            .await
            .unwrap());
        let cleared = store.get_document(id).await.unwrap().unwrap();
        assert_eq!(cleared.indexed_revision, None);
        assert_eq!(cleared.processing_status, DocumentProcessingStatus::Queued);
        assert_eq!(cleared.index_fingerprint, None);
        assert_eq!(cleared.indexed_at, None);

        assert!(!store
            .mark_document_indexed(
                DocumentId::new(),
                1,
                uuid::Uuid::new_v4(),
                "missing",
                second_at,
            )
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn stale_revision_token_cannot_mark_a_recreated_document_indexed() {
        let (_dir, store) = temp_store().await;
        let first = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: None,
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "old lifecycle".into(),
            updated_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
        };
        let old = store.upsert_document(&first).await.unwrap();
        store.delete_document(first.id).await.unwrap();
        let recreated_at = DateTime::<Utc>::from_timestamp(2, 0).unwrap();
        let recreated = store
            .upsert_document(&DocumentUpsert {
                id: old.id,
                project_id: old.project_id,
                source_uri: old.source_uri.clone(),
                media_type: old.media_type.clone(),
                title: old.title.clone(),
                canonical_text: "new lifecycle".into(),
                updated_at: recreated_at,
            })
            .await
            .unwrap();

        assert_eq!(recreated.content_revision, 3);
        assert_ne!(recreated.revision_token, old.revision_token);
        assert!(!store
            .mark_document_indexed(
                recreated.id,
                old.content_revision,
                old.revision_token,
                "stale",
                recreated.updated_at,
            )
            .await
            .unwrap());
        assert!(store
            .mark_document_indexed(
                recreated.id,
                recreated.content_revision,
                recreated.revision_token,
                "current",
                recreated.updated_at,
            )
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn document_generation_clock_survives_unknown_delete_and_recreation() {
        let (_dir, store) = temp_store().await;
        let id = DocumentId::new();

        let unknown_tombstone = store.delete_document(id).await.unwrap();
        assert_eq!(unknown_tombstone.content_revision, 1);
        assert_eq!(store.delete_document(id).await.unwrap(), unknown_tombstone);
        assert_eq!(
            store.get_document_generation(id).await.unwrap(),
            Some(unknown_tombstone)
        );
        assert_eq!(store.get_document(id).await.unwrap(), None);

        let source = DocumentUpsert {
            id,
            project_id: None,
            source_uri: Some("file:///generation-clock.txt".into()),
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "first live source".into(),
            updated_at: DateTime::<Utc>::from_timestamp(100_000, 0).unwrap(),
        };
        let first = store.upsert_document(&source).await.unwrap();
        assert_eq!(first.content_revision, 2);
        assert_ne!(first.revision_token, unknown_tombstone.revision_token);
        let second = store
            .upsert_document(&DocumentUpsert {
                canonical_text: "second live source".into(),
                ..source.clone()
            })
            .await
            .unwrap();
        assert_eq!(second.content_revision, 3);

        let tombstone = store.delete_document(id).await.unwrap();
        assert_eq!(tombstone.content_revision, 4);
        assert_eq!(store.delete_document(id).await.unwrap(), tombstone);
        let recreated = store.upsert_document(&source).await.unwrap();
        assert_eq!(recreated.content_revision, 5);
        assert_ne!(recreated.revision_token, tombstone.revision_token);
        assert_eq!(
            recreated.generation(),
            store.get_document_generation(id).await.unwrap().unwrap()
        );
    }

    #[tokio::test]
    async fn pending_document_retirement_survives_reopen_and_uses_exact_cas() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("retirement.db").display()
        );
        let id = DocumentId::new();
        let source = DocumentUpsert {
            id,
            project_id: None,
            source_uri: None,
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "retire me".into(),
            updated_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
        };
        let store = DbStore::connect(&url).await.unwrap();
        store.upsert_document(&source).await.unwrap();
        let tombstone = store.delete_document(id).await.unwrap();
        assert_eq!(store.delete_document(id).await.unwrap(), tombstone);
        assert_eq!(
            store
                .list_pending_document_retirements(None, 0)
                .await
                .unwrap(),
            vec![]
        );
        drop(store);

        let store = DbStore::connect(&url).await.unwrap();
        assert_eq!(
            store
                .list_pending_document_retirements(None, 10)
                .await
                .unwrap(),
            vec![(id, tombstone)]
        );

        let recreated = store
            .upsert_document(&DocumentUpsert {
                canonical_text: "new lifecycle".into(),
                updated_at: DateTime::<Utc>::from_timestamp(2, 0).unwrap(),
                ..source
            })
            .await
            .unwrap();
        assert!(store
            .list_pending_document_retirements(None, 10)
            .await
            .unwrap()
            .is_empty());
        assert!(!store
            .complete_document_retirement(id, tombstone)
            .await
            .unwrap());

        let current_tombstone = store.delete_document(id).await.unwrap();
        assert_eq!(
            current_tombstone.content_revision,
            recreated.content_revision + 1
        );
        assert!(!store
            .complete_document_retirement(id, tombstone)
            .await
            .unwrap());
        assert!(store
            .complete_document_retirement(id, current_tombstone)
            .await
            .unwrap());
        assert!(!store
            .complete_document_retirement(id, current_tombstone)
            .await
            .unwrap());
        assert!(store
            .list_pending_document_retirements(None, 10)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn pending_document_retirement_cursor_advances_and_can_wrap() {
        let (_dir, store) = temp_store().await;
        let ids = [1_u128, 2, 3].map(|value| DocumentId(uuid::Uuid::from_u128(value)));
        let mut generations = Vec::new();
        for id in ids {
            generations.push(store.delete_document(id).await.unwrap());
        }

        assert_eq!(
            store
                .list_pending_document_retirements(None, 2)
                .await
                .unwrap(),
            vec![(ids[0], generations[0]), (ids[1], generations[1])]
        );
        assert_eq!(
            store
                .list_pending_document_retirements(Some(ids[1]), 2)
                .await
                .unwrap(),
            vec![(ids[2], generations[2])]
        );
        assert!(store
            .list_pending_document_retirements(Some(ids[2]), 2)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .list_pending_document_retirements(None, 1)
                .await
                .unwrap(),
            vec![(ids[0], generations[0])]
        );
    }

    #[tokio::test]
    async fn document_generation_overflow_leaves_source_and_clock_unchanged() {
        let (_dir, store) = temp_store().await;
        let source = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: None,
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "maximum generation".into(),
            updated_at: DateTime::<Utc>::from_timestamp(101_000, 0).unwrap(),
        };
        let first = store.upsert_document(&source).await.unwrap();
        entities::document_generation::Entity::update_many()
            .col_expr(
                entities::document_generation::Column::ContentRevision,
                sea_orm::sea_query::Expr::value(i64::MAX),
            )
            .filter(entities::document_generation::Column::DocumentId.eq(source.id.0))
            .exec(&store.conn)
            .await
            .unwrap();
        entities::document::Entity::update_many()
            .col_expr(
                entities::document::Column::ContentRevision,
                sea_orm::sea_query::Expr::value(i64::MAX),
            )
            .filter(entities::document::Column::Id.eq(source.id.0))
            .exec(&store.conn)
            .await
            .unwrap();
        let before = store.get_document(source.id).await.unwrap().unwrap();
        assert_eq!(before.content_revision, i64::MAX);
        assert_eq!(before.revision_token, first.revision_token);

        assert!(store.upsert_document(&source).await.is_err());
        assert_eq!(
            store.get_document(source.id).await.unwrap(),
            Some(before.clone())
        );
        assert_eq!(
            store.get_document_generation(source.id).await.unwrap(),
            Some(before.generation())
        );
    }

    #[tokio::test]
    async fn concurrent_first_document_upserts_allocate_distinct_revisions() {
        let (_dir, store) = temp_store().await;
        let first = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: None,
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "a".into(),
            updated_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
        };
        let second = DocumentUpsert {
            canonical_text: "b".into(),
            ..first.clone()
        };

        let (first, second) = tokio::join!(
            store.upsert_document(&first),
            store.upsert_document(&second)
        );
        let mut revisions = [
            first.unwrap().content_revision,
            second.unwrap().content_revision,
        ];
        revisions.sort_unstable();
        assert_eq!(revisions, [1, 2]);
    }

    #[tokio::test]
    async fn document_upsert_rolls_back_when_project_is_unknown() {
        let (_dir, store) = temp_store().await;
        let upsert = DocumentUpsert {
            id: DocumentId::new(),
            project_id: Some(ProjectId::new()),
            source_uri: None,
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "content".into(),
            updated_at: Utc::now(),
        };
        assert!(store.upsert_document(&upsert).await.is_err());
        assert_eq!(store.get_document(upsert.id).await.unwrap(), None);
        assert_eq!(
            store.get_document_generation(upsert.id).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn concurrent_document_upserts_allocate_distinct_revisions() {
        let (_dir, store) = temp_store().await;
        let base = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: None,
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "base".into(),
            updated_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
        };
        let id = base.id;
        assert_eq!(
            store.upsert_document(&base).await.unwrap().content_revision,
            1
        );
        let a = DocumentUpsert {
            canonical_text: "a".into(),
            updated_at: DateTime::<Utc>::from_timestamp(2, 0).unwrap(),
            ..base.clone()
        };
        let b = DocumentUpsert {
            canonical_text: "b".into(),
            updated_at: DateTime::<Utc>::from_timestamp(3, 0).unwrap(),
            ..base
        };

        let (a, b) = tokio::join!(store.upsert_document(&a), store.upsert_document(&b));
        let mut revisions = [a.unwrap().content_revision, b.unwrap().content_revision];
        revisions.sort_unstable();
        assert_eq!(revisions, [2, 3]);
        let current = store.get_document(id).await.unwrap().unwrap();
        assert_eq!(current.content_revision, 3);
        assert!(matches!(current.canonical_text.as_str(), "a" | "b"));
        assert_eq!(current.indexed_revision, None);
    }

    #[tokio::test]
    async fn high_contention_document_upserts_do_not_drop_writers() {
        let (_dir, store) = temp_store().await;
        let id = DocumentId::new();
        let writes = (0..64).map(|i| {
            let store = store.clone();
            async move {
                store
                    .upsert_document(&DocumentUpsert {
                        id,
                        project_id: None,
                        source_uri: None,
                        media_type: "text/plain".into(),
                        title: None,
                        canonical_text: format!("writer {i}"),
                        updated_at: DateTime::<Utc>::from_timestamp(i, 0).unwrap(),
                    })
                    .await
                    .unwrap()
                    .content_revision
            }
        });

        let mut revisions = futures::future::join_all(writes).await;
        revisions.sort_unstable();
        assert_eq!(revisions, (1..=64).collect::<Vec<_>>());
        assert_eq!(
            store
                .get_document(id)
                .await
                .unwrap()
                .unwrap()
                .content_revision,
            64
        );
    }

    #[tokio::test]
    async fn m0006_upgrades_an_existing_store_without_losing_records() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("upgrade.db").display()
        );
        let conn = Database::connect(&url).await.unwrap();
        conn.execute_unprepared("PRAGMA foreign_keys=ON;")
            .await
            .unwrap();
        migration::Migrator::up(&conn, Some(5)).await.unwrap();
        let store = DbStore { conn: conn.clone() };
        let chat = sample_chat();
        store.create_chat(&chat).await.unwrap();

        migration::Migrator::up(&conn, None).await.unwrap();

        assert_eq!(store.get_chat(chat.id).await.unwrap().as_ref(), Some(&chat));
        let mut document = sample_document(None);
        let supplied_token = document.revision_token;
        store.create_document(&document).await.unwrap();
        let stored = store.get_document(document.id).await.unwrap().unwrap();
        assert_ne!(stored.revision_token, supplied_token);
        document.revision_token = stored.revision_token;
        assert_eq!(stored, document);
    }

    #[tokio::test]
    async fn chats_and_messages_roundtrip() {
        let (_dir, store) = temp_store().await;
        let chat = sample_chat();
        store.create_chat(&chat).await.unwrap();

        assert_eq!(store.get_chat(chat.id).await.unwrap().as_ref(), Some(&chat));
        assert_eq!(store.list_chats().await.unwrap(), vec![chat.clone()]);
        assert_eq!(store.get_chat(ChatId::new()).await.unwrap(), None);

        let msg = Message {
            id: MessageId::new(),
            chat_id: chat.id,
            turn_id: TurnId::new(),
            role: Role::User,
            content: "hi there".into(),
            created_at: DateTime::<Utc>::from_timestamp(1_700_000_001, 0).unwrap(),
        };
        store.append_message(&msg).await.unwrap();
        assert_eq!(store.list_messages(chat.id).await.unwrap(), vec![msg]);
    }

    #[tokio::test]
    async fn settings_roundtrip_and_overwrite() {
        let (_dir, store) = temp_store().await;
        assert_eq!(store.get_setting("model").await.unwrap(), None);
        store
            .set_setting("model", &serde_json::json!("claude"))
            .await
            .unwrap();
        assert_eq!(
            store.get_setting("model").await.unwrap(),
            Some(serde_json::json!("claude"))
        );
        store
            .set_setting("model", &serde_json::json!("gpt"))
            .await
            .unwrap();
        assert_eq!(
            store.get_setting("model").await.unwrap(),
            Some(serde_json::json!("gpt"))
        );
    }

    #[tokio::test]
    async fn list_chats_is_newest_first_and_messages_oldest_first() {
        let (_dir, store) = temp_store().await;
        let mut older = sample_chat();
        older.created_at = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
        let mut newer = sample_chat();
        newer.created_at = DateTime::<Utc>::from_timestamp(2_000, 0).unwrap();
        store.create_chat(&older).await.unwrap();
        store.create_chat(&newer).await.unwrap();
        // list_chats is newest-first.
        assert_eq!(
            store.list_chats().await.unwrap(),
            vec![newer.clone(), older.clone()]
        );

        // Messages come back oldest-first regardless of insert order.
        let msg = |ts: i64| Message {
            id: MessageId::new(),
            chat_id: newer.id,
            turn_id: TurnId::new(),
            role: Role::User,
            content: format!("t{ts}"),
            created_at: DateTime::<Utc>::from_timestamp(ts, 0).unwrap(),
        };
        let (m1, m2) = (msg(20), msg(10));
        store.append_message(&m1).await.unwrap();
        store.append_message(&m2).await.unwrap();
        let listed = store.list_messages(newer.id).await.unwrap();
        assert_eq!(listed, vec![m2, m1]);
    }

    #[tokio::test]
    async fn event_journal_assigns_per_chat_seq_and_replays_after_cursor() {
        use crate::event::AgentEvent;
        use crate::id::TurnId;
        use crate::provider::{StopReason, Usage};

        let (_dir, store) = temp_store().await;
        let chat = sample_chat();
        store.create_chat(&chat).await.unwrap();

        let started = AgentEvent::TurnStarted {
            turn_id: TurnId::new(),
        };
        let completed = AgentEvent::TurnCompleted {
            usage: Usage::default(),
            stop_reason: StopReason::EndTurn,
        };
        assert_eq!(store.append_event(chat.id, &started).await.unwrap(), 1);
        assert_eq!(store.append_event(chat.id, &completed).await.unwrap(), 2);

        // From the start: both events, in order, with their seq.
        let all = store.list_events(chat.id, 0).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!((all[0].seq, all[1].seq), (1, 2));
        assert_eq!(all[0].event, started);

        // After a cursor: only the newer event (what a reconnecting client needs).
        let tail = store.list_events(chat.id, 1).await.unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].seq, 2);
        assert_eq!(tail[0].event, completed);

        // A second chat's seq restarts at 1 and its journal is isolated.
        let other = sample_chat();
        store.create_chat(&other).await.unwrap();
        assert_eq!(store.append_event(other.id, &started).await.unwrap(), 1);
        assert_eq!(store.list_events(chat.id, 0).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn event_for_unknown_chat_is_rejected() {
        use crate::event::AgentEvent;

        let (_dir, store) = temp_store().await;
        // No create_chat first: the `event -> chat` foreign key must reject
        // the orphan write. (The in-memory MemStore test double does *not* model
        // this constraint, so orphan-rejection is only guaranteed by DbStore.)
        let event = AgentEvent::TurnStarted {
            turn_id: TurnId::new(),
        };
        assert!(store.append_event(ChatId::new(), &event).await.is_err());
    }

    #[tokio::test]
    async fn all_roles_round_trip() {
        let (_dir, store) = temp_store().await;
        let chat = sample_chat();
        store.create_chat(&chat).await.unwrap();
        let roles = [Role::System, Role::User, Role::Assistant, Role::Tool];
        for (i, role) in roles.iter().enumerate() {
            store
                .append_message(&Message {
                    id: MessageId::new(),
                    chat_id: chat.id,
                    turn_id: TurnId::new(),
                    role: *role,
                    content: String::new(),
                    created_at: DateTime::<Utc>::from_timestamp(i as i64, 0).unwrap(),
                })
                .await
                .unwrap();
        }
        let got: Vec<Role> = store
            .list_messages(chat.id)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.role)
            .collect();
        assert_eq!(got, roles);
    }

    #[tokio::test]
    async fn tool_calls_roundtrip_and_upsert_preserves_created_at() {
        let (_dir, store) = temp_store().await;
        let chat = sample_chat();
        store.create_chat(&chat).await.unwrap();

        let created = DateTime::<Utc>::from_timestamp(1_700_000_010, 0).unwrap();
        let call = ToolCallRecord {
            id: CallId::new(),
            chat_id: chat.id,
            turn_id: TurnId::new(),
            provider_id: "tu_1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "note.txt"}),
            result: None,
            is_error: false,
            created_at: created,
            completed_at: None,
        };
        store.upsert_tool_call(&call).await.unwrap();

        let completed = DateTime::<Utc>::from_timestamp(1_700_000_011, 0).unwrap();
        store
            .upsert_tool_call(&ToolCallRecord {
                result: Some("hello".into()),
                is_error: false,
                created_at: Utc::now(), // must not overwrite the original
                completed_at: Some(completed),
                ..call.clone()
            })
            .await
            .unwrap();

        let listed = store.list_tool_calls(chat.id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].created_at, created);
        assert_eq!(listed[0].completed_at, Some(completed));
        assert_eq!(listed[0].result.as_deref(), Some("hello"));
        assert_eq!(listed[0].arguments, serde_json::json!({"path": "note.txt"}));
    }
}
