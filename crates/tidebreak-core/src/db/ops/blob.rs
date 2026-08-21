use chrono::Utc;
use sea_orm::sea_query::{ExprTrait, OnConflict};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
    TransactionTrait, TryInsertResult,
};

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

/// Whether any table still holds a live reference to `blob_id`.
///
/// Blob ids are content-derived, so one blob legitimately backs many referrers:
/// two conversations attaching identical bytes share a single blob. Liveness is
/// therefore the *union* across every referring table, never "does this one
/// table reference it". Getting that wrong is silent data loss — the orphan
/// auditor would delete bytes another table still needs once the grace period
/// elapses.
///
/// Every caller that decides whether a blob may be retired or deleted goes
/// through this function, so a future referring table is added here once rather
/// than at each decision site. Callers must already hold the retirement write
/// lock, so the result is stable for the rest of their transaction.
pub(in crate::db) async fn is_referenced_on<C>(conn: &C, blob_id: uuid::Uuid) -> Result<bool>
where
    C: ConnectionTrait,
{
    let by_document = entities::document::Entity::find()
        .select_only()
        .column(entities::document::Column::Id)
        .filter(entities::document::Column::SourceBlobId.eq(blob_id))
        .into_tuple::<uuid::Uuid>()
        .one(conn)
        .await
        .map_err(store_err)?
        .is_some();
    if by_document {
        return Ok(true);
    }
    let by_tool_preview = tool_preview_references(conn, blob_id).await?;
    if by_tool_preview {
        return Ok(true);
    }
    let by_chat_publication = entities::chat_image_publication::Entity::find()
        .select_only()
        .column(entities::chat_image_publication::Column::ChatId)
        .filter(entities::chat_image_publication::Column::BlobId.eq(blob_id))
        .into_tuple::<uuid::Uuid>()
        .one(conn)
        .await
        .map_err(store_err)?
        .is_some();
    if by_chat_publication {
        return Ok(true);
    }
    let by_attachment = entities::message_attachment::Entity::find()
        .select_only()
        .column(entities::message_attachment::Column::MessageId)
        .filter(entities::message_attachment::Column::BlobId.eq(blob_id))
        .into_tuple::<uuid::Uuid>()
        .one(conn)
        .await
        .map_err(store_err)?
        .is_some();
    if by_attachment {
        return Ok(true);
    }
    let by_code_turn = entities::code_turn_attachment::Entity::find()
        .select_only()
        .column(entities::code_turn_attachment::Column::TurnId)
        .filter(entities::code_turn_attachment::Column::BlobId.eq(blob_id))
        .into_tuple::<uuid::Uuid>()
        .one(conn)
        .await
        .map_err(store_err)?
        .is_some();
    if by_code_turn {
        return Ok(true);
    }
    // The file-change journal keeps the only surviving copy of bytes the agent
    // overwrote in a user's folder. Reaping one of these deletes the thing undo
    // restores, so it belongs in the union like any other referrer.
    super::exec_file_change::references_blob_on(conn, blob_id).await
}

/// Whether any tool call's stored result preview still shows `blob_id`.
///
/// Only previews whose stored text carries the blob's id are read back, which
/// keeps the work proportional to the rows that might match rather than to the
/// whole call history — a walk the retirement lock is held across.
///
/// What makes that pre-filter safe is not a property of UUIDs: `uuid`'s
/// deserializer accepts unhyphenated, uppercase, braced, and `urn:uuid:`
/// spellings, any of which would slip past a `LIKE` on the canonical form. The
/// invariant is that every preview is written by serializing a
/// [`ToolResultPreview`], which emits the hyphenated lowercase form and nothing
/// else, including for previews a client posts — those are deserialized into
/// the typed enum and re-serialized before they are stored. A future writer
/// that hand-builds preview JSON breaks this and must not.
///
/// A candidate row that will not parse counts as a reference. The two failure
/// directions here are not symmetric: treating an unreadable preview as
/// "no reference" deletes bytes some card still renders, while treating it as
/// a reference only keeps a blob alive longer than needed.
async fn tool_preview_references<C>(conn: &C, blob_id: uuid::Uuid) -> Result<bool>
where
    C: ConnectionTrait,
{
    let mentions = format!("%{blob_id}%");
    let candidates = entities::tool_call::Entity::find()
        .select_only()
        .column(entities::tool_call::Column::ResultPreview)
        .filter(entities::tool_call::Column::ResultPreview.is_not_null())
        .filter(
            sea_orm::sea_query::Expr::col(entities::tool_call::Column::ResultPreview)
                .cast_as(sea_orm::sea_query::Alias::new("text"))
                .like(mentions),
        )
        .into_tuple::<Option<serde_json::Value>>()
        .all(conn)
        .await
        .map_err(store_err)?;
    Ok(candidates.into_iter().flatten().any(|value| {
        match serde_json::from_value::<crate::ToolResultPreview>(value) {
            Ok(crate::ToolResultPreview::Exec { images, .. }) => {
                images.iter().any(|image| image.blob_id == blob_id)
            }
            Ok(crate::ToolResultPreview::ScreenCapture { image, .. }) => image.blob_id == blob_id,
            Ok(_) => false,
            Err(_) => true,
        }
    }))
}

pub(in crate::db) async fn ensure_orphan(store: &DbStore, blob_id: uuid::Uuid) -> Result<bool> {
    loop {
        let transaction = store.conn.begin().await.map_err(store_err)?;
        acquire_write_lock(&transaction).await?;
        let candidate = entities::blob_retirement::Entity::find_by_id(blob_id)
            .one(&transaction)
            .await
            .map_err(store_err)?;
        let referenced = is_referenced_on(&transaction, blob_id).await?;
        if referenced {
            if let Some(candidate) = candidate
                .filter(|candidate| candidate.status != BlobRetirementStatus::Cancelled.as_str())
            {
                if !cancel_candidate_on(&transaction, &candidate, Utc::now()).await? {
                    transaction.rollback().await.map_err(store_err)?;
                    continue;
                }
            }
            transaction.commit().await.map_err(store_err)?;
            return Ok(false);
        }
        let queued = match candidate.as_ref() {
            None => matches!(
                entities::blob_retirement::Entity::insert(retirement_model(blob_id, Utc::now()))
                    .on_conflict_do_nothing()
                    .exec_without_returning(&transaction)
                    .await
                    .map_err(store_err)?,
                TryInsertResult::Inserted(1)
            ),
            Some(candidate) if matches!(candidate.status.as_str(), "succeeded" | "cancelled") => {
                requeue_candidate_on(&transaction, candidate, Utc::now()).await?
            }
            Some(_) => {
                transaction.commit().await.map_err(store_err)?;
                return Ok(false);
            }
        };
        if !queued {
            transaction.rollback().await.map_err(store_err)?;
            continue;
        }
        transaction.commit().await.map_err(store_err)?;
        return Ok(true);
    }
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

pub(in crate::db) async fn claim(
    store: &DbStore,
    now: chrono::DateTime<Utc>,
    lease_expires_at: chrono::DateTime<Utc>,
) -> Result<Option<BlobRetirement>> {
    if lease_expires_at <= now {
        return Err(AgentError::Store(
            "blob retirement lease expiry must be after claim time".into(),
        ));
    }

    // Idle fast path. The locked scan below is still authoritative, but a scan
    // with no claimable retirement has nothing it can decide. Taking the no-op
    // write lock first makes every idle worker poll join SQLite's single-writer
    // queue and hold a pooled connection while it waits. A candidate that
    // appears after this read is handled by the worker wake or the next bounded
    // poll.
    if !any_claimable_on(&store.conn, now).await? {
        return Ok(None);
    }

    loop {
        let transaction = store.conn.begin().await.map_err(store_err)?;
        acquire_write_lock(&transaction).await?;
        let due = entities::blob_retirement::Entity::find()
            .filter(due_candidate_condition(now))
            .order_by_asc(entities::blob_retirement::Column::AvailableAt)
            .order_by_asc(entities::blob_retirement::Column::CreatedAt)
            .order_by_asc(entities::blob_retirement::Column::BlobId)
            .one(&transaction)
            .await
            .map_err(store_err)?;
        let expired = entities::blob_retirement::Entity::find()
            .filter(expired_candidate_condition(now))
            .order_by_asc(entities::blob_retirement::Column::LeaseExpiresAt)
            .order_by_asc(entities::blob_retirement::Column::CreatedAt)
            .order_by_asc(entities::blob_retirement::Column::BlobId)
            .one(&transaction)
            .await
            .map_err(store_err)?;
        let candidate = match (due, expired) {
            (Some(due), Some(expired)) => {
                if effective_due(&due)? <= effective_due(&expired)? {
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
        let candidate = entities::blob_retirement::Entity::find_by_id(candidate.blob_id)
            .one(&transaction)
            .await
            .map_err(store_err)?;
        let Some(candidate) = candidate.filter(|candidate| is_due(candidate, now)) else {
            transaction.rollback().await.map_err(store_err)?;
            continue;
        };
        let referenced = is_referenced_on(&transaction, candidate.blob_id).await?;
        if referenced {
            if !cancel_candidate_on(&transaction, &candidate, now).await? {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            }
            transaction.commit().await.map_err(store_err)?;
            continue;
        }
        let reclaiming = candidate.status == BlobRetirementStatus::Running.as_str();
        if reclaiming && candidate.attempt_count >= candidate.max_attempts {
            let failed = entities::blob_retirement::Entity::update_many()
                .col_expr(
                    entities::blob_retirement::Column::Status,
                    sea_orm::sea_query::Expr::value(BlobRetirementStatus::Failed.as_str()),
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
                    sea_orm::sea_query::Expr::value(Some("lease_expired".to_owned())),
                )
                .col_expr(
                    entities::blob_retirement::Column::LastErrorDetail,
                    sea_orm::sea_query::Expr::value(Some(
                        "final blob retirement lease expired".to_owned(),
                    )),
                )
                .col_expr(
                    entities::blob_retirement::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(now),
                )
                .filter(entities::blob_retirement::Column::BlobId.eq(candidate.blob_id))
                .filter(
                    entities::blob_retirement::Column::Status
                        .eq(BlobRetirementStatus::Running.as_str()),
                )
                .filter(entities::blob_retirement::Column::AttemptCount.eq(candidate.attempt_count))
                .filter(entities::blob_retirement::Column::LeaseToken.eq(candidate.lease_token))
                .filter(
                    entities::blob_retirement::Column::LeaseExpiresAt
                        .eq(candidate.lease_expires_at),
                )
                .filter(entities::blob_retirement::Column::UpdatedAt.eq(candidate.updated_at))
                .exec(&transaction)
                .await
                .map_err(store_err)?;
            if failed.rows_affected != 1 {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            }
            transaction.commit().await.map_err(store_err)?;
            continue;
        }

        let next_attempt = candidate.attempt_count.checked_add(1).ok_or_else(|| {
            AgentError::Store(format!(
                "blob retirement {} attempt overflow",
                candidate.blob_id
            ))
        })?;
        let lease_token = uuid::Uuid::new_v4();
        let claim = entities::blob_retirement::Entity::update_many()
            .col_expr(
                entities::blob_retirement::Column::Status,
                sea_orm::sea_query::Expr::value(BlobRetirementStatus::Running.as_str()),
            )
            .col_expr(
                entities::blob_retirement::Column::AttemptCount,
                sea_orm::sea_query::Expr::value(next_attempt),
            )
            .col_expr(
                entities::blob_retirement::Column::LeaseToken,
                sea_orm::sea_query::Expr::value(Some(lease_token)),
            )
            .col_expr(
                entities::blob_retirement::Column::LeaseExpiresAt,
                sea_orm::sea_query::Expr::value(Some(lease_expires_at)),
            )
            .col_expr(
                entities::blob_retirement::Column::StartedAt,
                sea_orm::sea_query::Expr::value(Some(candidate.started_at.unwrap_or(now))),
            )
            .col_expr(
                entities::blob_retirement::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .filter(entities::blob_retirement::Column::BlobId.eq(candidate.blob_id))
            .filter(entities::blob_retirement::Column::Status.eq(&candidate.status))
            .filter(entities::blob_retirement::Column::AttemptCount.eq(candidate.attempt_count))
            .filter(entities::blob_retirement::Column::UpdatedAt.eq(candidate.updated_at));
        let claim = if reclaiming {
            claim
                .col_expr(
                    entities::blob_retirement::Column::LastErrorCode,
                    sea_orm::sea_query::Expr::value(Some("lease_expired".to_owned())),
                )
                .col_expr(
                    entities::blob_retirement::Column::LastErrorDetail,
                    sea_orm::sea_query::Expr::value(Some(
                        "previous blob retirement lease expired".to_owned(),
                    )),
                )
                .filter(entities::blob_retirement::Column::LeaseToken.eq(candidate.lease_token))
                .filter(
                    entities::blob_retirement::Column::LeaseExpiresAt
                        .eq(candidate.lease_expires_at),
                )
        } else {
            claim.filter(entities::blob_retirement::Column::AvailableAt.lte(now))
        };
        let claimed = claim.exec(&transaction).await.map_err(store_err)?;
        if claimed.rows_affected != 1 {
            transaction.rollback().await.map_err(store_err)?;
            continue;
        }
        let claimed = entities::blob_retirement::Entity::find_by_id(candidate.blob_id)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| {
                AgentError::Store(format!(
                    "claimed blob retirement {} disappeared",
                    candidate.blob_id
                ))
            })
            .and_then(from_model)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(claimed));
    }
}

fn due_candidate_condition(now: chrono::DateTime<Utc>) -> sea_orm::Condition {
    sea_orm::Condition::all()
        .add(entities::blob_retirement::Column::Status.is_in([
            BlobRetirementStatus::Queued.as_str(),
            BlobRetirementStatus::RetryWait.as_str(),
        ]))
        .add(entities::blob_retirement::Column::AvailableAt.lte(now))
        .add(
            sea_orm::sea_query::Expr::col(entities::blob_retirement::Column::AttemptCount).lt(
                sea_orm::sea_query::Expr::col(entities::blob_retirement::Column::MaxAttempts),
            ),
        )
}

fn expired_candidate_condition(now: chrono::DateTime<Utc>) -> sea_orm::Condition {
    sea_orm::Condition::all()
        .add(entities::blob_retirement::Column::Status.eq(BlobRetirementStatus::Running.as_str()))
        .add(entities::blob_retirement::Column::LeaseExpiresAt.lte(now))
}

async fn any_claimable_on<C>(conn: &C, now: chrono::DateTime<Utc>) -> Result<bool>
where
    C: ConnectionTrait,
{
    Ok(entities::blob_retirement::Entity::find()
        .filter(
            sea_orm::Condition::any()
                .add(due_candidate_condition(now))
                .add(expired_candidate_condition(now)),
        )
        .one(conn)
        .await
        .map_err(store_err)?
        .is_some())
}

pub(in crate::db) async fn heartbeat(
    store: &DbStore,
    blob_id: uuid::Uuid,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
    lease_expires_at: chrono::DateTime<Utc>,
) -> Result<bool> {
    if lease_expires_at <= now {
        return Err(AgentError::Store(
            "blob retirement lease expiry must be after heartbeat time".into(),
        ));
    }
    let updated = entities::blob_retirement::Entity::update_many()
        .col_expr(
            entities::blob_retirement::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Some(lease_expires_at)),
        )
        .col_expr(
            entities::blob_retirement::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::blob_retirement::Column::BlobId.eq(blob_id))
        .filter(
            entities::blob_retirement::Column::Status.eq(BlobRetirementStatus::Running.as_str()),
        )
        .filter(entities::blob_retirement::Column::LeaseToken.eq(lease_token))
        .filter(entities::blob_retirement::Column::LeaseExpiresAt.gt(now))
        .filter(entities::blob_retirement::Column::LeaseExpiresAt.lt(lease_expires_at))
        .filter(entities::blob_retirement::Column::UpdatedAt.lte(now))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(updated.rows_affected == 1)
}

pub(in crate::db) async fn validate_lease(
    store: &DbStore,
    blob_id: uuid::Uuid,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
) -> Result<bool> {
    loop {
        let transaction = store.conn.begin().await.map_err(store_err)?;
        acquire_write_lock(&transaction).await?;
        let candidate = entities::blob_retirement::Entity::find_by_id(blob_id)
            .one(&transaction)
            .await
            .map_err(store_err)?;
        let Some(candidate) =
            candidate.filter(|candidate| lease_is_live(candidate, lease_token, now))
        else {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(false);
        };
        let referenced = is_referenced_on(&transaction, blob_id).await?;
        if referenced {
            if !cancel_candidate_on(&transaction, &candidate, now).await? {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            }
            transaction.commit().await.map_err(store_err)?;
            return Ok(false);
        }
        transaction.commit().await.map_err(store_err)?;
        return Ok(true);
    }
}

pub(in crate::db) async fn complete(
    store: &DbStore,
    blob_id: uuid::Uuid,
    lease_token: uuid::Uuid,
    completed_at: chrono::DateTime<Utc>,
) -> Result<bool> {
    let completed = entities::blob_retirement::Entity::update_many()
        .col_expr(
            entities::blob_retirement::Column::Status,
            sea_orm::sea_query::Expr::value(BlobRetirementStatus::Succeeded.as_str()),
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
            sea_orm::sea_query::Expr::value(Some(completed_at)),
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
            sea_orm::sea_query::Expr::value(completed_at),
        )
        .filter(entities::blob_retirement::Column::BlobId.eq(blob_id))
        .filter(
            entities::blob_retirement::Column::Status.eq(BlobRetirementStatus::Running.as_str()),
        )
        .filter(entities::blob_retirement::Column::LeaseToken.eq(lease_token))
        .filter(entities::blob_retirement::Column::LeaseExpiresAt.gt(completed_at))
        .filter(entities::blob_retirement::Column::UpdatedAt.lte(completed_at))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(completed.rows_affected == 1)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn record_failure(
    store: &DbStore,
    blob_id: uuid::Uuid,
    lease_token: uuid::Uuid,
    failed_at: chrono::DateTime<Utc>,
    retry_at: Option<chrono::DateTime<Utc>>,
    error_code: &str,
    error_detail: Option<&str>,
) -> Result<Option<BlobRetirementStatus>> {
    validate_error(error_code, error_detail)?;
    if retry_at.is_some_and(|retry_at| retry_at <= failed_at) {
        return Err(AgentError::Store(
            "blob retirement retry time must be after failure time".into(),
        ));
    }

    loop {
        let transaction = store.conn.begin().await.map_err(store_err)?;
        acquire_write_lock(&transaction).await?;
        let candidate = entities::blob_retirement::Entity::find_by_id(blob_id)
            .one(&transaction)
            .await
            .map_err(store_err)?;
        let Some(candidate) =
            candidate.filter(|candidate| lease_is_live(candidate, lease_token, failed_at))
        else {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(None);
        };
        let will_retry = retry_at.is_some() && candidate.attempt_count < candidate.max_attempts;
        let next_status = if will_retry {
            BlobRetirementStatus::RetryWait
        } else {
            BlobRetirementStatus::Failed
        };
        let update = entities::blob_retirement::Entity::update_many()
            .col_expr(
                entities::blob_retirement::Column::Status,
                sea_orm::sea_query::Expr::value(next_status.as_str()),
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
                entities::blob_retirement::Column::LastErrorCode,
                sea_orm::sea_query::Expr::value(Some(error_code.to_owned())),
            )
            .col_expr(
                entities::blob_retirement::Column::LastErrorDetail,
                sea_orm::sea_query::Expr::value(error_detail.map(str::to_owned)),
            )
            .col_expr(
                entities::blob_retirement::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(failed_at),
            );
        let update = if let Some(retry_at) = retry_at.filter(|_| will_retry) {
            update.col_expr(
                entities::blob_retirement::Column::AvailableAt,
                sea_orm::sea_query::Expr::value(retry_at),
            )
        } else {
            update.col_expr(
                entities::blob_retirement::Column::FinishedAt,
                sea_orm::sea_query::Expr::value(Some(failed_at)),
            )
        };
        let resolved = update
            .filter(entities::blob_retirement::Column::BlobId.eq(blob_id))
            .filter(
                entities::blob_retirement::Column::Status
                    .eq(BlobRetirementStatus::Running.as_str()),
            )
            .filter(entities::blob_retirement::Column::AttemptCount.eq(candidate.attempt_count))
            .filter(entities::blob_retirement::Column::LeaseToken.eq(lease_token))
            .filter(
                entities::blob_retirement::Column::LeaseExpiresAt.eq(candidate.lease_expires_at),
            )
            .filter(entities::blob_retirement::Column::UpdatedAt.eq(candidate.updated_at))
            .exec(&transaction)
            .await
            .map_err(store_err)?;
        if resolved.rows_affected != 1 {
            transaction.rollback().await.map_err(store_err)?;
            continue;
        }
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(next_status));
    }
}

async fn cancel_candidate_on<C>(
    conn: &C,
    candidate: &entities::blob_retirement::Model,
    now: chrono::DateTime<Utc>,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    let cancelled = entities::blob_retirement::Entity::update_many()
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
        .filter(entities::blob_retirement::Column::BlobId.eq(candidate.blob_id))
        .filter(entities::blob_retirement::Column::Status.eq(&candidate.status))
        .filter(entities::blob_retirement::Column::AttemptCount.eq(candidate.attempt_count))
        .filter(entities::blob_retirement::Column::UpdatedAt.eq(candidate.updated_at))
        .filter(match candidate.lease_token {
            Some(lease_token) => entities::blob_retirement::Column::LeaseToken.eq(lease_token),
            None => entities::blob_retirement::Column::LeaseToken.is_null(),
        })
        .filter(match candidate.lease_expires_at {
            Some(lease_expires_at) => {
                entities::blob_retirement::Column::LeaseExpiresAt.eq(lease_expires_at)
            }
            None => entities::blob_retirement::Column::LeaseExpiresAt.is_null(),
        })
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(cancelled.rows_affected == 1)
}

/// Start a new retirement episode only if `candidate` is still the exact row
/// observed by the orphan audit. This prevents a stale completed snapshot from
/// fencing a worker that has since claimed a newer episode.
pub(in crate::db) async fn requeue_candidate_on<C>(
    conn: &C,
    candidate: &entities::blob_retirement::Model,
    now: chrono::DateTime<Utc>,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    let requeued = entities::blob_retirement::Entity::update_many()
        .col_expr(
            entities::blob_retirement::Column::Status,
            sea_orm::sea_query::Expr::value(BlobRetirementStatus::Queued.as_str()),
        )
        .col_expr(
            entities::blob_retirement::Column::AttemptCount,
            sea_orm::sea_query::Expr::value(0),
        )
        .col_expr(
            entities::blob_retirement::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(BlobRetirement::DEFAULT_MAX_ATTEMPTS),
        )
        .col_expr(
            entities::blob_retirement::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(now),
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
            entities::blob_retirement::Column::StartedAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::blob_retirement::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
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
        .filter(entities::blob_retirement::Column::BlobId.eq(candidate.blob_id))
        .filter(entities::blob_retirement::Column::Status.eq(&candidate.status))
        .filter(entities::blob_retirement::Column::AttemptCount.eq(candidate.attempt_count))
        .filter(entities::blob_retirement::Column::UpdatedAt.eq(candidate.updated_at))
        .filter(match candidate.lease_token {
            Some(lease_token) => entities::blob_retirement::Column::LeaseToken.eq(lease_token),
            None => entities::blob_retirement::Column::LeaseToken.is_null(),
        })
        .filter(match candidate.lease_expires_at {
            Some(lease_expires_at) => {
                entities::blob_retirement::Column::LeaseExpiresAt.eq(lease_expires_at)
            }
            None => entities::blob_retirement::Column::LeaseExpiresAt.is_null(),
        })
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(requeued.rows_affected == 1)
}

async fn acquire_write_lock<C>(conn: &C) -> Result<()>
where
    C: ConnectionTrait,
{
    entities::blob_retirement::Entity::update_many()
        .col_expr(
            entities::blob_retirement::Column::UpdatedAt,
            sea_orm::sea_query::Expr::col(entities::blob_retirement::Column::UpdatedAt),
        )
        .filter(entities::blob_retirement::Column::BlobId.is_null())
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

fn effective_due(retirement: &entities::blob_retirement::Model) -> Result<chrono::DateTime<Utc>> {
    match retirement.status.as_str() {
        "queued" | "retry_wait" => Ok(retirement.available_at),
        "running" => retirement.lease_expires_at.ok_or_else(|| {
            AgentError::Store(format!(
                "running blob retirement {} has no lease expiry",
                retirement.blob_id
            ))
        }),
        other => Err(AgentError::Store(format!(
            "non-claimable blob retirement {} has status {other}",
            retirement.blob_id
        ))),
    }
}

fn is_due(retirement: &entities::blob_retirement::Model, now: chrono::DateTime<Utc>) -> bool {
    match retirement.status.as_str() {
        "queued" | "retry_wait" => {
            retirement.available_at <= now && retirement.attempt_count < retirement.max_attempts
        }
        "running" => retirement
            .lease_expires_at
            .is_some_and(|lease_expires_at| lease_expires_at <= now),
        _ => false,
    }
}

fn lease_is_live(
    retirement: &entities::blob_retirement::Model,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
) -> bool {
    retirement.status == BlobRetirementStatus::Running.as_str()
        && retirement.lease_token == Some(lease_token)
        && retirement
            .lease_expires_at
            .is_some_and(|lease_expires_at| lease_expires_at > now)
        && retirement.updated_at <= now
}

fn validate_error(error_code: &str, error_detail: Option<&str>) -> Result<()> {
    let code_len = error_code.chars().count();
    if !(1..=BlobRetirement::MAX_ERROR_CODE_LEN).contains(&code_len) {
        return Err(AgentError::Store(
            "blob retirement error code must contain 1 to 128 characters".into(),
        ));
    }
    if error_detail.is_some_and(|detail| {
        !(1..=BlobRetirement::MAX_ERROR_DETAIL_LEN).contains(&detail.chars().count())
    }) {
        return Err(AgentError::Store(
            "blob retirement error detail must contain 1 to 4096 characters".into(),
        ));
    }
    Ok(())
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
    entities::blob_retirement::Entity::insert(retirement_model(blob_id, now))
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

fn retirement_model(
    blob_id: uuid::Uuid,
    now: chrono::DateTime<Utc>,
) -> entities::blob_retirement::ActiveModel {
    entities::blob_retirement::ActiveModel {
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
    }
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
