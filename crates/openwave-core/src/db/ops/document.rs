use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait};

use crate::error::{AgentError, Result};
use crate::id::{DocumentId, DocumentJobId};
use crate::model::{
    DocumentGeneration, DocumentJob, DocumentJobKind, DocumentJobStatus, DocumentParseOutput,
    DocumentProcessingStatus, DocumentRecord, DocumentSourceUpsert,
};
use crate::storage::EnsureDocumentParseJobOutcome;

use super::super::{
    acquire_document_write_lock, cancel_live_document_jobs_on, document_from_model,
    document_job_active_model, document_job_from_model, document_job_lease_is_live,
    ensure_live_document_generation_on, ensure_resolution_document_matches, entities,
    source_regions_to_db, store_err, try_advance_document_generation_on,
    validate_document_source_blob, validate_document_source_regions, DbStore,
};
use super::blob as blob_ops;
use super::require_document_scope_write_lock;

pub(in crate::db) async fn accept_source_and_enqueue_parse(
    store: &DbStore,
    source: &DocumentSourceUpsert,
    parser_fingerprint: &str,
    max_attempts: i32,
) -> Result<(DocumentRecord, DocumentJob)> {
    validate_source_input(source, parser_fingerprint, max_attempts)?;

    loop {
        let transaction = store.conn.begin().await.map_err(store_err)?;
        require_document_scope_write_lock(&transaction, source.chat_id, source.project_id).await?;
        acquire_document_write_lock(&transaction, source.id).await?;
        let existing = entities::document::Entity::find_by_id(source.id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?;

        if let Some(current) = existing.as_ref() {
            if current.chat_id != source.chat_id.map(|id| id.0)
                || current.project_id != source.project_id.map(|id| id.0)
            {
                return Err(AgentError::Store(format!(
                    "document {} cannot move between document corpora",
                    source.id
                )));
            }
            ensure_live_document_generation_on(&transaction, current).await?;
            if source_matches(current, source) {
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
                        entities::document_job::Column::Kind.eq(DocumentJobKind::Parse.as_str()),
                    )
                    .filter(
                        entities::document_job::Column::PipelineFingerprint.eq(parser_fingerprint),
                    )
                    .one(&transaction)
                    .await
                    .map_err(store_err)?
                {
                    let record = document_from_model(current.clone())?;
                    let job = document_job_from_model(job)?;
                    blob_ops::cancel_on(&transaction, source.source_blob.id).await?;
                    transaction.commit().await.map_err(store_err)?;
                    return Ok((record, job));
                }
            }
        }

        let Some(advanced) =
            try_advance_document_generation_on(&transaction, source.id, false).await?
        else {
            transaction.rollback().await.map_err(store_err)?;
            continue;
        };
        if let Some(current) = existing.as_ref() {
            let current_generation = DocumentGeneration {
                content_revision: current.content_revision,
                revision_token: current.revision_token,
            };
            if advanced.previous != Some(current_generation) {
                return Err(AgentError::Store(format!(
                    "document {} does not match its retained generation clock",
                    source.id
                )));
            }
        }

        let workflow_now = Utc::now();
        let byte_len = i64::try_from(source.source_blob.byte_len)
            .map_err(|_| AgentError::Store("document source is too large".into()))?;
        let active = entities::document::ActiveModel {
            id: Set(source.id.0),
            chat_id: Set(source.chat_id.map(|id| id.0)),
            project_id: Set(source.project_id.map(|id| id.0)),
            source_uri: Set(source.source_uri.clone()),
            media_type: Set(source.media_type.clone()),
            title: Set(source.title.clone()),
            source_blob_id: Set(Some(source.source_blob.id)),
            source_sha256: Set(Some(source.source_blob.sha256.to_vec())),
            source_byte_len: Set(Some(byte_len)),
            canonical_text: Set(String::new()),
            canonical_fingerprint: Set(None),
            source_regions: Set(source_regions_to_db(&[])),
            content_revision: Set(advanced.current.content_revision),
            revision_token: Set(advanced.current.revision_token),
            processing_status: Set(DocumentProcessingStatus::Queued.as_str().into()),
            indexed_revision: Set(None),
            index_fingerprint: Set(None),
            created_at: Set(existing
                .as_ref()
                .map_or(source.updated_at, |current| current.created_at)),
            updated_at: Set(source.updated_at),
            indexed_at: Set(None),
        };
        if existing.is_some() {
            active.update(&transaction).await.map_err(store_err)?;
        } else {
            active.insert(&transaction).await.map_err(store_err)?;
        }

        blob_ops::replace_reference_on(
            &transaction,
            existing.as_ref().and_then(|current| current.source_blob_id),
            source.source_blob.id,
        )
        .await?;

        cancel_live_document_jobs_on(&transaction, source.id, workflow_now).await?;
        let job = new_job(
            source.id,
            advanced.current,
            DocumentJobKind::Parse,
            parser_fingerprint,
            max_attempts,
            workflow_now,
        );
        document_job_active_model(&job)
            .insert(&transaction)
            .await
            .map_err(store_err)?;
        let record = entities::document::Entity::find_by_id(source.id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store("accepted source document disappeared".into()))?;
        let record = document_from_model(record)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok((record, job));
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn complete_parse_and_enqueue_index(
    store: &DbStore,
    id: DocumentJobId,
    lease_token: uuid::Uuid,
    completed_at: chrono::DateTime<Utc>,
    output: &DocumentParseOutput,
    index_fingerprint: &str,
    index_max_attempts: i32,
) -> Result<Option<(DocumentRecord, DocumentJob)>> {
    validate_document_source_regions(&output.canonical_text, &output.source_regions)?;
    validate_job_input(index_fingerprint, index_max_attempts)?;

    let Some(candidate) = entities::document_job::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    if candidate.kind != DocumentJobKind::Parse.as_str() {
        return Err(AgentError::Store(format!(
            "document job {id} is not a parse job"
        )));
    }

    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_document_write_lock(&transaction, DocumentId(candidate.document_id)).await?;
    let Some(job) = entities::document_job::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if job.kind != DocumentJobKind::Parse.as_str() {
        return Err(AgentError::Store(format!(
            "document job {id} changed semantic kind during parse completion"
        )));
    }
    if !document_job_lease_is_live(&job, lease_token, completed_at) {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    ensure_resolution_document_matches(&transaction, &job).await?;

    let completed = entities::document_job::Entity::update_many()
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
        .filter(entities::document_job::Column::Id.eq(id.0))
        .filter(entities::document_job::Column::Status.eq(DocumentJobStatus::Running.as_str()))
        .filter(entities::document_job::Column::LeaseToken.eq(lease_token))
        .filter(entities::document_job::Column::LeaseExpiresAt.eq(job.lease_expires_at))
        .filter(entities::document_job::Column::UpdatedAt.eq(job.updated_at))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if completed.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }

    let published = entities::document::Entity::update_many()
        .col_expr(
            entities::document::Column::CanonicalText,
            sea_orm::sea_query::Expr::value(output.canonical_text.clone()),
        )
        .col_expr(
            entities::document::Column::CanonicalFingerprint,
            sea_orm::sea_query::Expr::value(Some(job.pipeline_fingerprint.clone())),
        )
        .col_expr(
            entities::document::Column::SourceRegions,
            sea_orm::sea_query::Expr::value(source_regions_to_db(&output.source_regions)),
        )
        .col_expr(
            entities::document::Column::ProcessingStatus,
            sea_orm::sea_query::Expr::value(DocumentProcessingStatus::Queued.as_str()),
        )
        .col_expr(
            entities::document::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(completed_at),
        )
        .filter(entities::document::Column::Id.eq(job.document_id))
        .filter(entities::document::Column::ContentRevision.eq(job.content_revision))
        .filter(entities::document::Column::RevisionToken.eq(job.revision_token))
        .filter(entities::document::Column::SourceBlobId.is_not_null())
        .filter(entities::document::Column::SourceSha256.is_not_null())
        .filter(entities::document::Column::SourceByteLen.is_not_null())
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if published.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "parse job {id} lost its exact document generation"
        )));
    }

    let generation = DocumentGeneration {
        content_revision: job.content_revision,
        revision_token: job.revision_token,
    };
    let index_job = new_job(
        DocumentId(job.document_id),
        generation,
        DocumentJobKind::Index,
        index_fingerprint,
        index_max_attempts,
        completed_at,
    );
    document_job_active_model(&index_job)
        .insert(&transaction)
        .await
        .map_err(store_err)?;
    let record = entities::document::Entity::find_by_id(job.document_id)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("parsed source document disappeared".into()))?;
    let record = document_from_model(record)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some((record, index_job)))
}

pub(in crate::db) async fn ensure_parse_job(
    store: &DbStore,
    document_id: DocumentId,
    expected_generation: DocumentGeneration,
    pipeline_fingerprint: &str,
    max_attempts: i32,
) -> Result<EnsureDocumentParseJobOutcome> {
    validate_job_input(pipeline_fingerprint, max_attempts)?;

    loop {
        let transaction = store.conn.begin().await.map_err(store_err)?;
        acquire_document_write_lock(&transaction, document_id).await?;
        let Some(document) = entities::document::Entity::find_by_id(document_id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?
        else {
            transaction.commit().await.map_err(store_err)?;
            return Ok(EnsureDocumentParseJobOutcome::MissingDocument);
        };
        ensure_live_document_generation_on(&transaction, &document).await?;
        let current_generation = DocumentGeneration {
            content_revision: document.content_revision,
            revision_token: document.revision_token,
        };

        if current_generation != expected_generation {
            transaction.commit().await.map_err(store_err)?;
            return Ok(EnsureDocumentParseJobOutcome::GenerationChanged(
                current_generation,
            ));
        }
        if document.canonical_fingerprint.as_deref() == Some(pipeline_fingerprint) {
            transaction.commit().await.map_err(store_err)?;
            return Ok(EnsureDocumentParseJobOutcome::CanonicalCurrent);
        }
        if document.source_blob_id.is_none() {
            transaction.commit().await.map_err(store_err)?;
            return Ok(EnsureDocumentParseJobOutcome::SourceUnavailable);
        }
        if let Some(candidate) = find_exact_parse_job_on(
            &transaction,
            document_id,
            current_generation,
            pipeline_fingerprint,
        )
        .await?
        {
            let job = document_job_from_model(candidate)?;
            let outcome = if job.status == DocumentJobStatus::Failed {
                EnsureDocumentParseJobOutcome::Failed(job)
            } else if matches!(
                job.status,
                DocumentJobStatus::Queued
                    | DocumentJobStatus::Running
                    | DocumentJobStatus::RetryWait
            ) {
                EnsureDocumentParseJobOutcome::Existing(job)
            } else {
                return Err(AgentError::Store(format!(
                    "document {document_id} has desired parse job {} in terminal state {} without matching canonical output",
                    job.id,
                    job.status.as_str()
                )));
            };
            transaction.commit().await.map_err(store_err)?;
            return Ok(outcome);
        }

        let has_current_parse_job = entities::document_job::Entity::find()
            .filter(entities::document_job::Column::DocumentId.eq(document_id.0))
            .filter(
                entities::document_job::Column::ContentRevision
                    .eq(current_generation.content_revision),
            )
            .filter(
                entities::document_job::Column::RevisionToken.eq(current_generation.revision_token),
            )
            .filter(entities::document_job::Column::Kind.eq(DocumentJobKind::Parse.as_str()))
            .one(&transaction)
            .await
            .map_err(store_err)?
            .is_some();
        let advances_generation = document.canonical_fingerprint.is_some() || has_current_parse_job;
        let target_generation = if advances_generation {
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
            advanced.current
        } else {
            current_generation
        };

        let mut update = entities::document::Entity::update_many()
            .col_expr(
                entities::document::Column::CanonicalText,
                sea_orm::sea_query::Expr::value(String::new()),
            )
            .col_expr(
                entities::document::Column::CanonicalFingerprint,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
            .col_expr(
                entities::document::Column::SourceRegions,
                sea_orm::sea_query::Expr::value(source_regions_to_db(&[])),
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
                sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
            );
        if advances_generation {
            update = update
                .col_expr(
                    entities::document::Column::ContentRevision,
                    sea_orm::sea_query::Expr::value(target_generation.content_revision),
                )
                .col_expr(
                    entities::document::Column::RevisionToken,
                    sea_orm::sea_query::Expr::value(target_generation.revision_token),
                );
        }
        let updated = update
            .filter(entities::document::Column::Id.eq(document_id.0))
            .filter(
                entities::document::Column::ContentRevision.eq(current_generation.content_revision),
            )
            .filter(entities::document::Column::RevisionToken.eq(current_generation.revision_token))
            .filter(entities::document::Column::SourceBlobId.is_not_null())
            .exec(&transaction)
            .await
            .map_err(store_err)?;
        if updated.rows_affected != 1 {
            transaction.rollback().await.map_err(store_err)?;
            continue;
        }

        let now = Utc::now();
        cancel_live_document_jobs_on(&transaction, document_id, now).await?;
        let job = new_job(
            document_id,
            target_generation,
            DocumentJobKind::Parse,
            pipeline_fingerprint,
            max_attempts,
            now,
        );
        document_job_active_model(&job)
            .insert(&transaction)
            .await
            .map_err(store_err)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(EnsureDocumentParseJobOutcome::Enqueued(job));
    }
}

async fn find_exact_parse_job_on<C>(
    connection: &C,
    document_id: DocumentId,
    generation: DocumentGeneration,
    pipeline_fingerprint: &str,
) -> Result<Option<entities::document_job::Model>>
where
    C: sea_orm::ConnectionTrait,
{
    entities::document_job::Entity::find()
        .filter(entities::document_job::Column::DocumentId.eq(document_id.0))
        .filter(entities::document_job::Column::ContentRevision.eq(generation.content_revision))
        .filter(entities::document_job::Column::RevisionToken.eq(generation.revision_token))
        .filter(entities::document_job::Column::Kind.eq(DocumentJobKind::Parse.as_str()))
        .filter(entities::document_job::Column::PipelineFingerprint.eq(pipeline_fingerprint))
        .one(connection)
        .await
        .map_err(store_err)
}

pub(in crate::db) async fn retry_document_job(
    store: &DbStore,
    document_id: DocumentId,
    expected_generation: DocumentGeneration,
    kind: DocumentJobKind,
    pipeline_fingerprint: &str,
    max_attempts: i32,
) -> Result<Option<DocumentJob>> {
    validate_job_input(pipeline_fingerprint, max_attempts)?;

    let transaction = store.conn.begin().await.map_err(store_err)?;
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
    let current_generation = DocumentGeneration {
        content_revision: document.content_revision,
        revision_token: document.revision_token,
    };
    if current_generation != expected_generation {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let awaiting_parse =
        document.source_blob_id.is_some() && document.canonical_fingerprint.is_none();
    let stage_matches = match kind {
        DocumentJobKind::Parse => awaiting_parse,
        DocumentJobKind::Index => !awaiting_parse,
    };
    if !stage_matches {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }

    let candidate = entities::document_job::Entity::find()
        .filter(entities::document_job::Column::DocumentId.eq(document.id))
        .filter(entities::document_job::Column::ContentRevision.eq(document.content_revision))
        .filter(entities::document_job::Column::RevisionToken.eq(document.revision_token))
        .filter(entities::document_job::Column::Kind.eq(kind.as_str()))
        .filter(entities::document_job::Column::PipelineFingerprint.eq(pipeline_fingerprint))
        .one(&transaction)
        .await
        .map_err(store_err)?;
    let Some(candidate) = candidate else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let mut job = document_job_from_model(candidate.clone())?;
    if matches!(
        job.status,
        DocumentJobStatus::Queued | DocumentJobStatus::Running | DocumentJobStatus::RetryWait
    ) {
        let expected = if job.status == DocumentJobStatus::Running {
            DocumentProcessingStatus::Processing
        } else {
            DocumentProcessingStatus::Queued
        };
        if document.processing_status != expected.as_str() {
            return Err(AgentError::Store(format!(
                "document job {} is {} but exact document {} is unexpectedly {}",
                job.id,
                job.status.as_str(),
                document_id,
                document.processing_status
            )));
        }
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(job));
    }
    if job.status != DocumentJobStatus::Failed {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    if document.processing_status != DocumentProcessingStatus::Failed.as_str() {
        return Err(AgentError::Store(format!(
            "failed document job {} does not match failed document {}",
            job.id, document_id
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
            job.id
        )));
    }
    transaction.commit().await.map_err(store_err)?;
    job.status = DocumentJobStatus::Queued;
    job.attempt_count = 0;
    job.max_attempts = max_attempts;
    job.available_at = now;
    job.lease_token = None;
    job.lease_expires_at = None;
    job.started_at = None;
    job.finished_at = None;
    job.last_error_code = None;
    job.last_error_detail = None;
    job.updated_at = now;
    Ok(Some(job))
}

fn validate_source_input(
    source: &DocumentSourceUpsert,
    parser_fingerprint: &str,
    max_attempts: i32,
) -> Result<()> {
    if source.media_type.is_empty() || source.source_uri.as_deref() == Some("") {
        return Err(AgentError::Store("invalid document source metadata".into()));
    }
    validate_document_source_blob(&source.source_blob)?;
    validate_job_input(parser_fingerprint, max_attempts)
}

fn validate_job_input(fingerprint: &str, max_attempts: i32) -> Result<()> {
    if fingerprint.is_empty()
        || fingerprint.chars().count() > DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN
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
    Ok(())
}

fn source_matches(current: &entities::document::Model, source: &DocumentSourceUpsert) -> bool {
    current.chat_id == source.chat_id.map(|id| id.0)
        && current.project_id == source.project_id.map(|id| id.0)
        && current.source_uri == source.source_uri
        && current.media_type == source.media_type
        && current.title == source.title
        && current.source_blob_id == Some(source.source_blob.id)
        && current.source_sha256.as_deref() == Some(source.source_blob.sha256.as_slice())
        && u64::try_from(current.source_byte_len.unwrap_or(-1)).ok()
            == Some(source.source_blob.byte_len)
}

fn new_job(
    document_id: DocumentId,
    generation: DocumentGeneration,
    kind: DocumentJobKind,
    fingerprint: &str,
    max_attempts: i32,
    now: chrono::DateTime<Utc>,
) -> DocumentJob {
    DocumentJob {
        id: DocumentJobId::new(),
        document_id,
        content_revision: generation.content_revision,
        revision_token: generation.revision_token,
        kind,
        status: DocumentJobStatus::Queued,
        pipeline_fingerprint: fingerprint.into(),
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
