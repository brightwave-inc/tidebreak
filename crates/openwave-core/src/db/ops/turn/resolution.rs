use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set, TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::id::TurnId;
use crate::model::{Message, Role, TurnFailureReceipt, TurnFailureRetry, TurnRun, TurnRunStatus};
use crate::storage::{
    CompleteTurnRunOutcome, FinishTurnCancellationOutcome, RecordTurnFailureOutcome,
    RequestTurnCancellationOutcome,
};

use super::super::super::{entities, store_err, DbStore};
use super::{canonical_db_timestamp, turn_run_from_model, turn_run_status_from_db};

pub(in crate::db) async fn complete_turn_run(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
    output: &Message,
) -> Result<Option<CompleteTurnRunOutcome>> {
    validate_turn_output(id, lease_token, output)?;
    let now = canonical_db_timestamp(now)?;
    let output_created_at = canonical_db_timestamp(output.created_at)?;
    if output_created_at > now {
        return Err(AgentError::Store(
            "turn output timestamp must not be after completion time".into(),
        ));
    }

    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_exact_turn_write_lock(&transaction, id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(receipt) = entities::turn_claim::Entity::find_by_id(lease_token)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .filter(|receipt| receipt.turn_id == id.0)
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let Some(existing) = entities::turn_run::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if output.chat_id.0 != existing.chat_id {
        return Err(AgentError::Store(format!(
            "turn {id} output belongs to a different chat"
        )));
    }

    if existing.status == TurnRunStatus::Completed.as_str()
        && existing.attempt_count == receipt.attempt_count
    {
        if existing.output_message_id != Some(output.id.0)
            || !exact_completed_output_on(&transaction, output).await?
        {
            return Err(AgentError::Store(format!(
                "turn {id} was already completed with different output"
            )));
        }
        let existing = turn_run_from_model(existing)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(CompleteTurnRunOutcome::Existing(existing)));
    }
    if existing.status != TurnRunStatus::Running.as_str()
        || existing.attempt_count != receipt.attempt_count
        || existing.lease_token != Some(lease_token)
        || existing
            .lease_expires_at
            .is_none_or(|expires_at| expires_at <= now)
        || existing.updated_at > now
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }

    let message = entities::message::ActiveModel {
        id: Set(output.id.0),
        chat_id: Set(output.chat_id.0),
        turn_id: Set(output.turn_id.0),
        role: Set("assistant".into()),
        content: Set(output.content.clone()),
        created_at: Set(output_created_at),
    };
    if let Err(error) = message.insert(&transaction).await {
        transaction.rollback().await.map_err(store_err)?;
        if let Some(existing) = exact_completed_turn_on(store, id, lease_token, output).await? {
            return Ok(Some(CompleteTurnRunOutcome::Existing(existing)));
        }
        return Err(store_err(error));
    }

    let completed = entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::Status,
            sea_orm::sea_query::Expr::value(TurnRunStatus::Completed.as_str()),
        )
        .col_expr(
            entities::turn_run::Column::OutputMessageId,
            sea_orm::sea_query::Expr::value(Some(output.id.0)),
        )
        .col_expr(
            entities::turn_run::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::turn_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::turn_run::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .col_expr(
            entities::turn_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::turn_run::Column::Id.eq(id.0))
        .filter(entities::turn_run::Column::Status.eq(TurnRunStatus::Running.as_str()))
        .filter(entities::turn_run::Column::AttemptCount.eq(receipt.attempt_count))
        .filter(entities::turn_run::Column::LeaseToken.eq(lease_token))
        .filter(entities::turn_run::Column::LeaseExpiresAt.eq(existing.lease_expires_at))
        .filter(entities::turn_run::Column::LeaseExpiresAt.gt(now))
        .filter(entities::turn_run::Column::UpdatedAt.eq(existing.updated_at))
        .filter(entities::turn_run::Column::UpdatedAt.lte(now))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if completed.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let completed = entities::turn_run::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("completed turn {id} disappeared")))
        .and_then(turn_run_from_model)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(CompleteTurnRunOutcome::Completed(completed)))
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn record_turn_run_failure(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
    retry: TurnFailureRetry,
    error_code: &str,
    error_detail: Option<&str>,
) -> Result<Option<RecordTurnFailureOutcome>> {
    validate_turn_failure(lease_token, error_code, error_detail)?;
    let now = canonical_db_timestamp(now)?;
    let requested_retry_at = match retry {
        TurnFailureRetry::Permanent => None,
        TurnFailureRetry::RetryAt(retry_at) => Some(canonical_db_timestamp(retry_at)?),
    };

    if let Some(existing) = exact_turn_failure_on(
        &store.conn,
        id,
        lease_token,
        requested_retry_at,
        error_code,
        error_detail,
    )
    .await?
    {
        return Ok(Some(RecordTurnFailureOutcome::Existing(existing)));
    }
    if requested_retry_at.is_some_and(|retry_at| retry_at <= now) {
        return Err(AgentError::Store(
            "turn retry time must be after failure resolution time".into(),
        ));
    }

    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_exact_turn_write_lock(&transaction, id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    if let Some(existing) = exact_turn_failure_on(
        &transaction,
        id,
        lease_token,
        requested_retry_at,
        error_code,
        error_detail,
    )
    .await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(RecordTurnFailureOutcome::Existing(existing)));
    }
    let Some(claim) = entities::turn_claim::Entity::find_by_id(lease_token)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .filter(|claim| claim.turn_id == id.0)
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let Some(turn) = entities::turn_run::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if turn.status != TurnRunStatus::Running.as_str()
        || turn.attempt_count != claim.attempt_count
        || turn.lease_token != Some(lease_token)
        || turn
            .lease_expires_at
            .is_none_or(|expires_at| expires_at <= now)
        || turn.updated_at > now
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }

    let result_status = if requested_retry_at.is_some() && turn.attempt_count < turn.max_attempts {
        TurnRunStatus::RetryWait
    } else {
        TurnRunStatus::Failed
    };
    let receipt = entities::turn_failure::ActiveModel {
        lease_token: Set(lease_token),
        turn_id: Set(id.0),
        attempt_count: Set(claim.attempt_count),
        requested_retry_at: Set(requested_retry_at),
        error_code: Set(error_code.to_owned()),
        error_detail: Set(error_detail.map(str::to_owned)),
        resolved_at: Set(now),
        result_status: Set(result_status.as_str().into()),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;

    let update = entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::Status,
            sea_orm::sea_query::Expr::value(result_status.as_str()),
        )
        .col_expr(
            entities::turn_run::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::turn_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::turn_run::Column::LastErrorCode,
            sea_orm::sea_query::Expr::value(Some(error_code.to_owned())),
        )
        .col_expr(
            entities::turn_run::Column::LastErrorDetail,
            sea_orm::sea_query::Expr::value(error_detail.map(str::to_owned)),
        )
        .col_expr(
            entities::turn_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        );
    let update = match result_status {
        TurnRunStatus::RetryWait => update.col_expr(
            entities::turn_run::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(
                requested_retry_at.expect("retry-wait failure has a retry time"),
            ),
        ),
        TurnRunStatus::Failed => update.col_expr(
            entities::turn_run::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        ),
        _ => unreachable!("failure resolution has a constrained result status"),
    };
    let updated = update
        .filter(entities::turn_run::Column::Id.eq(id.0))
        .filter(entities::turn_run::Column::Status.eq(TurnRunStatus::Running.as_str()))
        .filter(entities::turn_run::Column::AttemptCount.eq(claim.attempt_count))
        .filter(entities::turn_run::Column::LeaseToken.eq(lease_token))
        .filter(entities::turn_run::Column::LeaseExpiresAt.eq(turn.lease_expires_at))
        .filter(entities::turn_run::Column::LeaseExpiresAt.gt(now))
        .filter(entities::turn_run::Column::UpdatedAt.eq(turn.updated_at))
        .filter(entities::turn_run::Column::UpdatedAt.lte(now))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }

    let receipt = turn_failure_from_model(receipt)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(RecordTurnFailureOutcome::Recorded(receipt)))
}

pub(in crate::db) async fn request_turn_cancellation(
    store: &DbStore,
    id: TurnId,
    now: chrono::DateTime<Utc>,
) -> Result<Option<RequestTurnCancellationOutcome>> {
    let now = canonical_db_timestamp(now)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_exact_turn_write_lock(&transaction, id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(turn) = entities::turn_run::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let status = turn_run_status_from_db(&turn.status)?;
    match status {
        TurnRunStatus::Cancelling | TurnRunStatus::Cancelled => {
            let turn = turn_run_from_model(turn)?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(RequestTurnCancellationOutcome::Existing(turn)));
        }
        TurnRunStatus::Completed | TurnRunStatus::Failed => {
            let turn = turn_run_from_model(turn)?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(RequestTurnCancellationOutcome::AlreadyTerminal(turn)));
        }
        TurnRunStatus::Queued | TurnRunStatus::Running | TurnRunStatus::RetryWait => {}
    }
    if turn.updated_at > now {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }

    let next_status = if status == TurnRunStatus::Running {
        TurnRunStatus::Cancelling
    } else {
        TurnRunStatus::Cancelled
    };
    let update = entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::Status,
            sea_orm::sea_query::Expr::value(next_status.as_str()),
        )
        .col_expr(
            entities::turn_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        );
    let update = if next_status == TurnRunStatus::Cancelled {
        update
            .col_expr(
                entities::turn_run::Column::FinishedAt,
                sea_orm::sea_query::Expr::value(Some(now)),
            )
            .col_expr(
                entities::turn_run::Column::LastErrorCode,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
            .col_expr(
                entities::turn_run::Column::LastErrorDetail,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
    } else {
        update
    };
    let update = update
        .filter(entities::turn_run::Column::Id.eq(id.0))
        .filter(entities::turn_run::Column::Status.eq(&turn.status))
        .filter(entities::turn_run::Column::AttemptCount.eq(turn.attempt_count))
        .filter(entities::turn_run::Column::UpdatedAt.eq(turn.updated_at))
        .filter(entities::turn_run::Column::UpdatedAt.lte(now));
    let update = if status == TurnRunStatus::Running {
        update
            .filter(entities::turn_run::Column::LeaseToken.eq(turn.lease_token))
            .filter(entities::turn_run::Column::LeaseExpiresAt.eq(turn.lease_expires_at))
    } else {
        update
    };
    let cancelled = update.exec(&transaction).await.map_err(store_err)?;
    if cancelled.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let updated = entities::turn_run::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("cancelled turn {id} disappeared")))
        .and_then(turn_run_from_model)?;
    transaction.commit().await.map_err(store_err)?;
    if next_status == TurnRunStatus::Cancelling {
        Ok(Some(RequestTurnCancellationOutcome::Requested(updated)))
    } else {
        Ok(Some(RequestTurnCancellationOutcome::Cancelled(updated)))
    }
}

pub(in crate::db) async fn finish_turn_cancellation(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
) -> Result<Option<FinishTurnCancellationOutcome>> {
    if lease_token.is_nil() {
        return Err(AgentError::Store("turn lease token must not be nil".into()));
    }
    let now = canonical_db_timestamp(now)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_exact_turn_write_lock(&transaction, id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(claim) = entities::turn_claim::Entity::find_by_id(lease_token)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .filter(|claim| claim.turn_id == id.0)
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let Some(turn) = entities::turn_run::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if turn.status == TurnRunStatus::Cancelled.as_str() && turn.attempt_count == claim.attempt_count
    {
        let turn = turn_run_from_model(turn)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(FinishTurnCancellationOutcome::Existing(turn)));
    }
    if turn.status != TurnRunStatus::Cancelling.as_str()
        || turn.attempt_count != claim.attempt_count
        || turn.lease_token != Some(lease_token)
        || turn.updated_at > now
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }

    let cancelled = entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::Status,
            sea_orm::sea_query::Expr::value(TurnRunStatus::Cancelled.as_str()),
        )
        .col_expr(
            entities::turn_run::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::turn_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::turn_run::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .col_expr(
            entities::turn_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::turn_run::Column::Id.eq(id.0))
        .filter(entities::turn_run::Column::Status.eq(TurnRunStatus::Cancelling.as_str()))
        .filter(entities::turn_run::Column::AttemptCount.eq(claim.attempt_count))
        .filter(entities::turn_run::Column::LeaseToken.eq(lease_token))
        .filter(entities::turn_run::Column::LeaseExpiresAt.eq(turn.lease_expires_at))
        .filter(entities::turn_run::Column::UpdatedAt.eq(turn.updated_at))
        .filter(entities::turn_run::Column::UpdatedAt.lte(now))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if cancelled.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let cancelled = entities::turn_run::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("cancelled turn {id} disappeared")))
        .and_then(turn_run_from_model)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(FinishTurnCancellationOutcome::Cancelled(cancelled)))
}

async fn acquire_exact_turn_write_lock<C>(conn: &C, id: TurnId) -> Result<bool>
where
    C: ConnectionTrait,
{
    let locked = entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::col(entities::turn_run::Column::UpdatedAt).into(),
        )
        .filter(entities::turn_run::Column::Id.eq(id.0))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(locked.rows_affected == 1)
}

fn validate_turn_output(id: TurnId, lease_token: uuid::Uuid, output: &Message) -> Result<()> {
    if lease_token.is_nil() {
        return Err(AgentError::Store("turn lease token must not be nil".into()));
    }
    if output.id.0.is_nil() {
        return Err(AgentError::Store(
            "turn output message id must not be nil".into(),
        ));
    }
    if output.turn_id != id || output.role != Role::Assistant {
        return Err(AgentError::Store(
            "turn output must be an assistant message for the completed turn".into(),
        ));
    }
    if output.content.contains('\0') {
        return Err(AgentError::Store(
            "turn output must contain no NUL characters".into(),
        ));
    }
    Ok(())
}

fn validate_turn_failure(
    lease_token: uuid::Uuid,
    error_code: &str,
    error_detail: Option<&str>,
) -> Result<()> {
    if lease_token.is_nil() {
        return Err(AgentError::Store("turn lease token must not be nil".into()));
    }
    let code_len = error_code.chars().count();
    if error_code.contains('\0') || !(1..=TurnRun::MAX_ERROR_CODE_LEN).contains(&code_len) {
        return Err(AgentError::Store(
            "turn error code must contain 1 to 128 non-NUL characters".into(),
        ));
    }
    if error_detail.is_some_and(|detail| {
        detail.contains('\0')
            || !(1..=TurnRun::MAX_ERROR_DETAIL_LEN).contains(&detail.chars().count())
    }) {
        return Err(AgentError::Store(
            "turn error detail must contain 1 to 4096 non-NUL characters".into(),
        ));
    }
    Ok(())
}

async fn exact_turn_failure_on<C>(
    conn: &C,
    id: TurnId,
    lease_token: uuid::Uuid,
    requested_retry_at: Option<chrono::DateTime<Utc>>,
    error_code: &str,
    error_detail: Option<&str>,
) -> Result<Option<TurnFailureReceipt>>
where
    C: ConnectionTrait,
{
    let Some(existing) = entities::turn_failure::Entity::find_by_id(lease_token)
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    if existing.turn_id != id.0
        || existing.requested_retry_at != requested_retry_at
        || existing.error_code != error_code
        || existing.error_detail.as_deref() != error_detail
    {
        return Err(AgentError::Store(format!(
            "turn failure token {lease_token} was already recorded with different data"
        )));
    }
    Ok(Some(turn_failure_from_model(existing)?))
}

fn turn_failure_from_model(model: entities::turn_failure::Model) -> Result<TurnFailureReceipt> {
    let result_status = turn_run_status_from_db(&model.result_status)?;
    if !matches!(
        result_status,
        TurnRunStatus::RetryWait | TurnRunStatus::Failed
    ) {
        return Err(AgentError::Store(format!(
            "invalid durable turn failure result: {}",
            model.result_status
        )));
    }
    Ok(TurnFailureReceipt {
        lease_token: model.lease_token,
        turn_id: TurnId(model.turn_id),
        attempt_count: model.attempt_count,
        requested_retry_at: model.requested_retry_at,
        error_code: model.error_code,
        error_detail: model.error_detail,
        resolved_at: model.resolved_at,
        result_status,
    })
}

async fn exact_completed_turn_on(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    output: &Message,
) -> Result<Option<TurnRun>> {
    let Some(receipt) = entities::turn_claim::Entity::find_by_id(lease_token)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .filter(|receipt| receipt.turn_id == id.0)
    else {
        return Ok(None);
    };
    let Some(existing) = entities::turn_run::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    if existing.status != TurnRunStatus::Completed.as_str()
        || existing.attempt_count != receipt.attempt_count
    {
        return Ok(None);
    }
    if existing.output_message_id != Some(output.id.0)
        || !exact_completed_output_on(&store.conn, output).await?
    {
        return Err(AgentError::Store(format!(
            "turn {id} was already completed with different output"
        )));
    }
    Ok(Some(turn_run_from_model(existing)?))
}

async fn exact_completed_output_on<C>(conn: &C, output: &Message) -> Result<bool>
where
    C: ConnectionTrait,
{
    let created_at = canonical_db_timestamp(output.created_at)?;
    Ok(entities::message::Entity::find_by_id(output.id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .is_some_and(|message| {
            message.chat_id == output.chat_id.0
                && message.turn_id == output.turn_id.0
                && message.role == "assistant"
                && message.content == output.content
                && message.created_at == created_at
        }))
}
