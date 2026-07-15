use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set, TransactionTrait,
};

use crate::error::{AgentError, AgentErrorInfo, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::{ChatId, TurnId};
use crate::model::{
    Message, Role, TurnFailureReceipt, TurnFailureRetry, TurnRun, TurnRunStatus, TurnSteerStatus,
};
use crate::provider::{StopReason, Usage};
use crate::storage::{
    CompleteTurnRunOutcome, FinishTurnCancellationOutcome, JournaledTurnOutcome,
    RecordTurnFailureOutcome, RequestTurnCancellationOutcome,
};

use super::super::super::{entities, store_err, DbStore};
use super::super::{
    acquire_chat_write_lock, acquire_turn_write_lock,
    conversation::{
        append_event_on, next_message_seq_on, reserve_message_identity_on,
        MESSAGE_IDENTITY_OWNER_MESSAGE,
    },
};
use super::{canonical_db_timestamp, turn_run_from_model, turn_run_status_from_db};

mod cancellation;

pub(in crate::db) use cancellation::{
    finish_turn_cancellation, finish_turn_cancellation_and_append_event, request_turn_cancellation,
    request_turn_cancellation_and_append_event,
};

pub(in crate::db) async fn complete_turn_run(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    expected_steer_revision: i64,
    now: chrono::DateTime<Utc>,
    output: &Message,
) -> Result<Option<CompleteTurnRunOutcome>> {
    Ok(complete_turn_run_inner(
        store,
        id,
        lease_token,
        expected_steer_revision,
        now,
        output,
        None,
    )
    .await?
    .map(|resolution| resolution.outcome))
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn complete_turn_run_and_append_event(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    expected_steer_revision: i64,
    now: chrono::DateTime<Utc>,
    output: &Message,
    usage: Usage,
    stop_reason: StopReason,
) -> Result<Option<JournaledTurnOutcome<CompleteTurnRunOutcome>>> {
    let event = AgentEvent::TurnCompleted { usage, stop_reason };
    complete_turn_run_inner(
        store,
        id,
        lease_token,
        expected_steer_revision,
        now,
        output,
        Some(&event),
    )
    .await
}

async fn complete_turn_run_inner(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    expected_steer_revision: i64,
    now: chrono::DateTime<Utc>,
    output: &Message,
    terminal_event: Option<&AgentEvent>,
) -> Result<Option<JournaledTurnOutcome<CompleteTurnRunOutcome>>> {
    validate_turn_output(id, lease_token, output)?;
    let now = canonical_db_timestamp(now)?;
    let output_created_at = canonical_db_timestamp(output.created_at)?;
    if output_created_at > now {
        return Err(AgentError::Store(
            "turn output timestamp must not be after completion time".into(),
        ));
    }

    let Some(journal_chat_id) = journal_chat_id(store, id, true).await? else {
        return Ok(None);
    };
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, journal_chat_id).await? {
        return Err(AgentError::Store(format!(
            "turn {id} references missing chat {journal_chat_id}"
        )));
    }
    if !acquire_turn_write_lock(&transaction, id).await? {
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
        && existing.claim_count == receipt.claim_count
    {
        if existing.output_message_id != Some(output.id.0)
            || !exact_completed_output_on(&transaction, output).await?
        {
            return Err(AgentError::Store(format!(
                "turn {id} was already completed with different output"
            )));
        }
        let sequenced_event =
            exact_terminal_event_on(&transaction, id, Some(lease_token), terminal_event).await?;
        let existing = turn_run_from_model(existing)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(JournaledTurnOutcome {
            outcome: CompleteTurnRunOutcome::Existing(existing),
            terminal_event: sequenced_event,
        }));
    }
    if existing.status != TurnRunStatus::Running.as_str()
        || existing.attempt_count != receipt.attempt_count
        || existing.claim_count != receipt.claim_count
        || existing.lease_token != Some(lease_token)
        || existing
            .lease_expires_at
            .is_none_or(|expires_at| expires_at <= now)
        || existing.updated_at > now
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    if let Some(AgentEvent::TurnCompleted { usage, .. }) = terminal_event {
        validate_terminal_usage(*usage, super::usage_from_turn_model(&existing)?)?;
    }
    let stale_output = existing.steer_revision != expected_steer_revision;
    let steer_pending = entities::turn_steer::Entity::find()
        .filter(entities::turn_steer::Column::TurnId.eq(id.0))
        .filter(entities::turn_steer::Column::Status.eq(TurnSteerStatus::Pending.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some();
    if stale_output || steer_pending {
        let existing = turn_run_from_model(existing)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(JournaledTurnOutcome {
            outcome: if steer_pending {
                CompleteTurnRunOutcome::SteerPending(existing)
            } else {
                CompleteTurnRunOutcome::OutputSuperseded(existing)
            },
            terminal_event: None,
        }));
    }

    if !reserve_message_identity_on(
        &transaction,
        output.id,
        output.chat_id,
        output.turn_id,
        MESSAGE_IDENTITY_OWNER_MESSAGE,
    )
    .await?
    {
        transaction.rollback().await.map_err(store_err)?;
        if let Some(existing) = exact_completed_turn_on(store, id, lease_token, output).await? {
            let sequenced_event =
                exact_terminal_event_on(&store.conn, id, Some(lease_token), terminal_event).await?;
            return Ok(Some(JournaledTurnOutcome {
                outcome: CompleteTurnRunOutcome::Existing(existing),
                terminal_event: sequenced_event,
            }));
        }
        return Err(AgentError::Store(format!(
            "turn output message identity {} is already reserved",
            output.id
        )));
    }
    let message = entities::message::ActiveModel {
        id: Set(output.id.0),
        chat_id: Set(output.chat_id.0),
        turn_id: Set(output.turn_id.0),
        seq: Set(next_message_seq_on(&transaction, output.chat_id).await?),
        role: Set("assistant".into()),
        content: Set(output.content.clone()),
        created_at: Set(output_created_at),
    };
    if let Err(error) = message.insert(&transaction).await {
        transaction.rollback().await.map_err(store_err)?;
        if let Some(existing) = exact_completed_turn_on(store, id, lease_token, output).await? {
            let sequenced_event =
                exact_terminal_event_on(&store.conn, id, Some(lease_token), terminal_event).await?;
            return Ok(Some(JournaledTurnOutcome {
                outcome: CompleteTurnRunOutcome::Existing(existing),
                terminal_event: sequenced_event,
            }));
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
        .filter(entities::turn_run::Column::ClaimCount.eq(receipt.claim_count))
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
    super::steer::reject_pending_turn_steers_on(&transaction, id, now).await?;
    let sequenced_event = append_terminal_event_on(
        &transaction,
        id,
        ChatId(existing.chat_id),
        Some(lease_token),
        terminal_event,
    )
    .await?;
    let completed = entities::turn_run::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("completed turn {id} disappeared")))
        .and_then(turn_run_from_model)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(JournaledTurnOutcome {
        outcome: CompleteTurnRunOutcome::Completed(completed),
        terminal_event: sequenced_event,
    }))
}

fn validate_terminal_usage(total: Usage, checkpoint: Usage) -> Result<()> {
    let covers_checkpoint = total.input_tokens >= checkpoint.input_tokens
        && total.output_tokens >= checkpoint.output_tokens
        && total.cache_read_input_tokens >= checkpoint.cache_read_input_tokens
        && total.cache_creation_input_tokens >= checkpoint.cache_creation_input_tokens;
    if !covers_checkpoint {
        return Err(AgentError::Store(
            "terminal turn usage regresses its durable checkpoint".into(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn record_turn_run_failure(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
    retry: TurnFailureRetry,
    model_steps: i32,
    usage: Usage,
    error_code: &str,
    error_detail: Option<&str>,
) -> Result<Option<RecordTurnFailureOutcome>> {
    Ok(record_turn_run_failure_inner(
        store,
        id,
        lease_token,
        now,
        retry,
        model_steps,
        usage,
        error_code,
        error_detail,
        None,
    )
    .await?
    .map(|resolution| resolution.outcome))
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn record_turn_run_failure_and_append_event(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
    retry: TurnFailureRetry,
    model_steps: i32,
    usage: Usage,
    error_code: &str,
    error_detail: Option<&str>,
) -> Result<Option<JournaledTurnOutcome<RecordTurnFailureOutcome>>> {
    let event = AgentEvent::TurnFailed {
        error: AgentErrorInfo {
            kind: error_code.to_owned(),
            message: error_detail.unwrap_or(error_code).to_owned(),
        },
    };
    record_turn_run_failure_inner(
        store,
        id,
        lease_token,
        now,
        retry,
        model_steps,
        usage,
        error_code,
        error_detail,
        Some(&event),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn record_turn_run_failure_inner(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
    retry: TurnFailureRetry,
    model_steps: i32,
    usage: Usage,
    error_code: &str,
    error_detail: Option<&str>,
    terminal_event: Option<&AgentEvent>,
) -> Result<Option<JournaledTurnOutcome<RecordTurnFailureOutcome>>> {
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
        model_steps,
        usage,
        error_code,
        error_detail,
    )
    .await?
    {
        let sequenced_event = exact_failure_terminal_event_on(
            &store.conn,
            id,
            lease_token,
            &existing,
            terminal_event,
        )
        .await?;
        return Ok(Some(JournaledTurnOutcome {
            outcome: RecordTurnFailureOutcome::Existing(existing),
            terminal_event: sequenced_event,
        }));
    }
    if requested_retry_at.is_some_and(|retry_at| retry_at <= now) {
        return Err(AgentError::Store(
            "turn retry time must be after failure resolution time".into(),
        ));
    }

    let journal_chat_id = journal_chat_id(store, id, terminal_event.is_some()).await?;
    if terminal_event.is_some() && journal_chat_id.is_none() {
        return Ok(None);
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if let Some(chat_id) = journal_chat_id {
        if !acquire_chat_write_lock(&transaction, chat_id).await? {
            return Err(AgentError::Store(format!(
                "turn {id} references missing chat {chat_id}"
            )));
        }
    }
    if !acquire_turn_write_lock(&transaction, id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    if let Some(existing) = exact_turn_failure_on(
        &transaction,
        id,
        lease_token,
        requested_retry_at,
        model_steps,
        usage,
        error_code,
        error_detail,
    )
    .await?
    {
        let sequenced_event = exact_failure_terminal_event_on(
            &transaction,
            id,
            lease_token,
            &existing,
            terminal_event,
        )
        .await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(JournaledTurnOutcome {
            outcome: RecordTurnFailureOutcome::Existing(existing),
            terminal_event: sequenced_event,
        }));
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
        || turn.claim_count != claim.claim_count
        || turn.lease_token != Some(lease_token)
        || turn
            .lease_expires_at
            .is_none_or(|expires_at| expires_at <= now)
        || turn.updated_at > now
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    if model_steps < turn.model_steps {
        return Err(AgentError::Store(
            "failed turn model steps regress its durable checkpoint".into(),
        ));
    }
    validate_terminal_usage(
        usage,
        Usage {
            input_tokens: u32::try_from(turn.input_tokens).map_err(|_| {
                AgentError::Store("turn input token checkpoint is out of range".into())
            })?,
            output_tokens: u32::try_from(turn.output_tokens).map_err(|_| {
                AgentError::Store("turn output token checkpoint is out of range".into())
            })?,
            cache_read_input_tokens: u32::try_from(turn.cache_read_input_tokens).map_err(|_| {
                AgentError::Store("turn cache-read token checkpoint is out of range".into())
            })?,
            cache_creation_input_tokens: u32::try_from(turn.cache_creation_input_tokens).map_err(
                |_| AgentError::Store("turn cache-create token checkpoint is out of range".into()),
            )?,
        },
    )?;

    let result_status = if requested_retry_at.is_some() && turn.attempt_count < turn.max_attempts {
        TurnRunStatus::RetryWait
    } else {
        TurnRunStatus::Failed
    };
    let receipt = entities::turn_failure::ActiveModel {
        lease_token: Set(lease_token),
        turn_id: Set(id.0),
        attempt_count: Set(claim.attempt_count),
        model_steps: Set(model_steps),
        input_tokens: Set(i64::from(usage.input_tokens)),
        output_tokens: Set(i64::from(usage.output_tokens)),
        cache_read_input_tokens: Set(i64::from(usage.cache_read_input_tokens)),
        cache_creation_input_tokens: Set(i64::from(usage.cache_creation_input_tokens)),
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
            entities::turn_run::Column::ModelSteps,
            sea_orm::sea_query::Expr::value(model_steps),
        )
        .col_expr(
            entities::turn_run::Column::InputTokens,
            sea_orm::sea_query::Expr::value(i64::from(usage.input_tokens)),
        )
        .col_expr(
            entities::turn_run::Column::OutputTokens,
            sea_orm::sea_query::Expr::value(i64::from(usage.output_tokens)),
        )
        .col_expr(
            entities::turn_run::Column::CacheReadInputTokens,
            sea_orm::sea_query::Expr::value(i64::from(usage.cache_read_input_tokens)),
        )
        .col_expr(
            entities::turn_run::Column::CacheCreationInputTokens,
            sea_orm::sea_query::Expr::value(i64::from(usage.cache_creation_input_tokens)),
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
        .filter(entities::turn_run::Column::ClaimCount.eq(claim.claim_count))
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

    let sequenced_event = if result_status == TurnRunStatus::Failed {
        super::steer::reject_pending_turn_steers_on(&transaction, id, now).await?;
        append_terminal_event_on(
            &transaction,
            id,
            ChatId(turn.chat_id),
            Some(lease_token),
            terminal_event,
        )
        .await?
    } else {
        None
    };
    let receipt = turn_failure_from_model(receipt)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(JournaledTurnOutcome {
        outcome: RecordTurnFailureOutcome::Recorded(receipt),
        terminal_event: sequenced_event,
    }))
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

#[allow(clippy::too_many_arguments)]
async fn exact_turn_failure_on<C>(
    conn: &C,
    id: TurnId,
    lease_token: uuid::Uuid,
    requested_retry_at: Option<chrono::DateTime<Utc>>,
    model_steps: i32,
    usage: Usage,
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
        || existing.model_steps != model_steps
        || existing.input_tokens != i64::from(usage.input_tokens)
        || existing.output_tokens != i64::from(usage.output_tokens)
        || existing.cache_read_input_tokens != i64::from(usage.cache_read_input_tokens)
        || existing.cache_creation_input_tokens != i64::from(usage.cache_creation_input_tokens)
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
        model_steps: model.model_steps,
        usage: Usage {
            input_tokens: u32::try_from(model.input_tokens).map_err(|_| {
                AgentError::Store("turn failure input token total is out of range".into())
            })?,
            output_tokens: u32::try_from(model.output_tokens).map_err(|_| {
                AgentError::Store("turn failure output token total is out of range".into())
            })?,
            cache_read_input_tokens: u32::try_from(model.cache_read_input_tokens).map_err(
                |_| AgentError::Store("turn failure cache-read token total is out of range".into()),
            )?,
            cache_creation_input_tokens: u32::try_from(model.cache_creation_input_tokens).map_err(
                |_| {
                    AgentError::Store(
                        "turn failure cache-create token total is out of range".into(),
                    )
                },
            )?,
        },
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
        || existing.claim_count != receipt.claim_count
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

async fn journal_chat_id(
    store: &DbStore,
    id: TurnId,
    journal_terminal: bool,
) -> Result<Option<ChatId>> {
    if !journal_terminal {
        return Ok(None);
    }
    Ok(entities::turn_run::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(|turn| ChatId(turn.chat_id)))
}

pub(super) async fn append_terminal_event_on<C>(
    conn: &C,
    id: TurnId,
    chat_id: ChatId,
    lease_token: Option<uuid::Uuid>,
    terminal_event: Option<&AgentEvent>,
) -> Result<Option<SequencedEvent>>
where
    C: ConnectionTrait,
{
    let Some(event) = terminal_event else {
        return Ok(None);
    };
    let seq = append_event_on(
        conn,
        chat_id,
        Some(id),
        lease_token,
        lease_token.map(|_| i32::MAX),
        None,
        event,
    )
    .await?;
    Ok(Some(SequencedEvent {
        seq,
        event: event.clone(),
    }))
}

async fn exact_terminal_event_on<C>(
    conn: &C,
    id: TurnId,
    lease_token: Option<uuid::Uuid>,
    expected: Option<&AgentEvent>,
) -> Result<Option<SequencedEvent>>
where
    C: ConnectionTrait,
{
    let Some(expected) = expected else {
        return Ok(None);
    };
    let stored = entities::event::Entity::find()
        .filter(entities::event::Column::TurnId.eq(id.0))
        .filter(entities::event::Column::Terminal.eq(true))
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("terminal turn {id} is missing its event")))?;
    let event = serde_json::from_value::<AgentEvent>(stored.payload)?;
    if stored.lease_token != lease_token
        || stored.attempt_event_ordinal != lease_token.map(|_| i32::MAX)
        || event != *expected
    {
        return Err(AgentError::Store(format!(
            "turn {id} has a different terminal event"
        )));
    }
    Ok(Some(SequencedEvent {
        seq: stored.seq,
        event,
    }))
}

async fn exact_failure_terminal_event_on<C>(
    conn: &C,
    id: TurnId,
    lease_token: uuid::Uuid,
    receipt: &TurnFailureReceipt,
    terminal_event: Option<&AgentEvent>,
) -> Result<Option<SequencedEvent>>
where
    C: ConnectionTrait,
{
    if receipt.result_status == TurnRunStatus::Failed {
        exact_terminal_event_on(conn, id, Some(lease_token), terminal_event).await
    } else {
        Ok(None)
    }
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
