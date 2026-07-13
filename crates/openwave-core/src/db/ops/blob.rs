use chrono::Utc;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set};

use crate::error::{AgentError, Result};
use crate::model::{BlobRetirement, BlobRetirementStatus};

use super::super::{entities, store_err, DbStore};

pub(in crate::db) async fn get(
    store: &DbStore,
    blob_id: uuid::Uuid,
) -> Result<Option<BlobRetirement>> {
    entities::blob_retirement::Entity::find_by_id(blob_id)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(from_model)
        .transpose()
}

/// Replace one document's blob reference using a global blob-id lock order.
///
/// PostgreSQL row locks acquired by the retirement upsert and cancellation can
/// otherwise deadlock when two documents concurrently swap `A -> B` and
/// `B -> A`. Applying the old/new mutations in ascending UUID order makes the
/// lock graph acyclic. The caller owns the surrounding document transaction.
pub(in crate::db) async fn replace_reference_on<C>(
    conn: &C,
    old_blob_id: Option<uuid::Uuid>,
    new_blob_id: uuid::Uuid,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let Some(old_blob_id) = old_blob_id.filter(|old| *old != new_blob_id) else {
        return cancel_on(conn, new_blob_id).await;
    };
    if old_blob_id < new_blob_id {
        enqueue_on(conn, old_blob_id).await?;
        cancel_on(conn, new_blob_id).await
    } else {
        cancel_on(conn, new_blob_id).await?;
        enqueue_on(conn, old_blob_id).await
    }
}

/// Coalesce a dropped source reference into one fresh retirement episode.
///
/// This runs inside the document mutation transaction. A unique blob key makes
/// concurrent drops idempotent, and clearing any prior lease fences a worker
/// from completing an earlier episode after this requeue.
pub(in crate::db) async fn enqueue_on<C>(conn: &C, blob_id: uuid::Uuid) -> Result<()>
where
    C: ConnectionTrait,
{
    let now = Utc::now();
    entities::blob_retirement::Entity::insert(entities::blob_retirement::ActiveModel {
        blob_id: Set(blob_id),
        status: Set(BlobRetirementStatus::Queued.as_str().into()),
        attempt_count: Set(0),
        max_attempts: Set(BlobRetirement::DEFAULT_MAX_ATTEMPTS),
        available_at: Set(now),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        started_at: Set(None),
        finished_at: Set(None),
        last_error_code: Set(None),
        last_error_detail: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    })
    .on_conflict(
        OnConflict::column(entities::blob_retirement::Column::BlobId)
            .update_columns([
                entities::blob_retirement::Column::Status,
                entities::blob_retirement::Column::AttemptCount,
                entities::blob_retirement::Column::MaxAttempts,
                entities::blob_retirement::Column::AvailableAt,
                entities::blob_retirement::Column::LeaseToken,
                entities::blob_retirement::Column::LeaseExpiresAt,
                entities::blob_retirement::Column::StartedAt,
                entities::blob_retirement::Column::FinishedAt,
                entities::blob_retirement::Column::LastErrorCode,
                entities::blob_retirement::Column::LastErrorDetail,
                entities::blob_retirement::Column::UpdatedAt,
            ])
            .to_owned(),
    )
    .exec_without_returning(conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

/// Cancel any retirement state when a transaction establishes a live reference.
pub(in crate::db) async fn cancel_on<C>(conn: &C, blob_id: uuid::Uuid) -> Result<()>
where
    C: ConnectionTrait,
{
    let now = Utc::now();
    entities::blob_retirement::Entity::update_many()
        .col_expr(
            entities::blob_retirement::Column::Status,
            sea_orm::sea_query::Expr::value(BlobRetirementStatus::Cancelled.as_str()),
        )
        .col_expr(
            entities::blob_retirement::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::blob_retirement::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::blob_retirement::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .col_expr(
            entities::blob_retirement::Column::LastErrorCode,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::blob_retirement::Column::LastErrorDetail,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::blob_retirement::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::blob_retirement::Column::BlobId.eq(blob_id))
        .filter(
            entities::blob_retirement::Column::Status.ne(BlobRetirementStatus::Cancelled.as_str()),
        )
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

pub(in crate::db) fn from_model(model: entities::blob_retirement::Model) -> Result<BlobRetirement> {
    let status = match model.status.as_str() {
        "queued" => BlobRetirementStatus::Queued,
        "running" => BlobRetirementStatus::Running,
        "retry_wait" => BlobRetirementStatus::RetryWait,
        "succeeded" => BlobRetirementStatus::Succeeded,
        "failed" => BlobRetirementStatus::Failed,
        "cancelled" => BlobRetirementStatus::Cancelled,
        other => {
            return Err(AgentError::Store(format!(
                "unknown blob retirement status: {other}"
            )))
        }
    };
    Ok(BlobRetirement {
        blob_id: model.blob_id,
        status,
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
