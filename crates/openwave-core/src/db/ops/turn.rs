use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait, TryInsertResult,
};

use crate::error::{AgentError, Result};
use crate::id::{ChatId, MessageId, TurnId};
use crate::model::{Message, Role, TurnRun, TurnRunStatus};
use crate::storage::{AcceptTurnOutcome, CompleteTurnRunOutcome};

use super::super::{entities, store_err, DbStore};

pub(in crate::db) async fn get_turn_run(store: &DbStore, id: TurnId) -> Result<Option<TurnRun>> {
    entities::turn_run::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(turn_run_from_model)
        .transpose()
}

pub(in crate::db) async fn list_turn_runs(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<TurnRun>> {
    entities::turn_run::Entity::find()
        .filter(entities::turn_run::Column::ChatId.eq(chat_id.0))
        .order_by_asc(entities::turn_run::Column::CreatedAt)
        .order_by_asc(entities::turn_run::Column::Id)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(turn_run_from_model)
        .collect()
}

pub(in crate::db) async fn accept_turn(
    store: &DbStore,
    id: TurnId,
    chat_id: ChatId,
    model: &str,
    content: &str,
) -> Result<AcceptTurnOutcome> {
    validate_turn_input(model, content)?;

    let transaction = store.conn.begin().await.map_err(store_err)?;
    let chat_lock = entities::chat::Entity::update_many()
        .col_expr(
            entities::chat::Column::Title,
            sea_orm::sea_query::Expr::col(entities::chat::Column::Title).into(),
        )
        .filter(entities::chat::Column::Id.eq(chat_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if chat_lock.rows_affected != 1 {
        return Err(AgentError::Store(format!("chat {chat_id} does not exist")));
    }

    if let Some(existing) = entities::turn_run::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let existing =
            exact_accepted_turn_on(&transaction, existing, chat_id, model, content).await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(AcceptTurnOutcome::Existing(existing));
    }

    if let Some(active) = find_active_turn_on(&transaction, chat_id).await? {
        let active = turn_run_from_model(active)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(AcceptTurnOutcome::ChatBusy(active));
    }

    let now = canonical_db_timestamp(Utc::now())?;
    let input_message_id = MessageId::new();
    let message = entities::message::ActiveModel {
        id: Set(input_message_id.0),
        chat_id: Set(chat_id.0),
        turn_id: Set(id.0),
        role: Set("user".into()),
        content: Set(content.into()),
        created_at: Set(now),
    };
    if let Err(error) = message.insert(&transaction).await {
        transaction.rollback().await.map_err(store_err)?;
        return Err(store_err(error));
    }

    let run = entities::turn_run::ActiveModel {
        id: Set(id.0),
        chat_id: Set(chat_id.0),
        input_message_id: Set(input_message_id.0),
        output_message_id: Set(None),
        model: Set(model.into()),
        status: Set(TurnRunStatus::Queued.as_str().into()),
        attempt_count: Set(0),
        max_attempts: Set(TurnRun::DEFAULT_MAX_ATTEMPTS),
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
    let inserted = match run.insert(&transaction).await {
        Ok(inserted) => inserted,
        Err(error) => {
            transaction.rollback().await.map_err(store_err)?;
            if let Some(existing) = entities::turn_run::Entity::find_by_id(id.0)
                .one(&store.conn)
                .await
                .map_err(store_err)?
            {
                let existing =
                    exact_accepted_turn_on(&store.conn, existing, chat_id, model, content).await?;
                return Ok(AcceptTurnOutcome::Existing(existing));
            }
            if let Some(active) = find_active_turn_on(&store.conn, chat_id).await? {
                return Ok(AcceptTurnOutcome::ChatBusy(turn_run_from_model(active)?));
            }
            return Err(store_err(error));
        }
    };

    transaction.commit().await.map_err(store_err)?;
    Ok(AcceptTurnOutcome::Accepted(turn_run_from_model(inserted)?))
}

pub(in crate::db) async fn claim_turn_run(
    store: &DbStore,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
    lease_expires_at: chrono::DateTime<Utc>,
) -> Result<Option<TurnRun>> {
    let now = canonical_db_timestamp(now)?;
    let lease_expires_at = canonical_db_timestamp(lease_expires_at)?;
    if lease_token.is_nil() {
        return Err(AgentError::Store("turn lease token must not be nil".into()));
    }
    if lease_expires_at <= now {
        return Err(AgentError::Store(
            "turn lease expiry must be after claim time".into(),
        ));
    }

    loop {
        let transaction = store.conn.begin().await.map_err(store_err)?;
        acquire_turn_claim_write_lock(&transaction).await?;
        if let Some(receipt) = entities::turn_claim::Entity::find_by_id(lease_token)
            .one(&transaction)
            .await
            .map_err(store_err)?
        {
            let existing = entities::turn_run::Entity::find_by_id(receipt.turn_id)
                .one(&transaction)
                .await
                .map_err(store_err)?
                .filter(|run| {
                    run.status == TurnRunStatus::Running.as_str()
                        && run.attempt_count == receipt.attempt_count
                        && run.lease_token == Some(lease_token)
                        && run
                            .lease_expires_at
                            .is_some_and(|lease_expires_at| lease_expires_at > now)
                })
                .map(turn_run_from_model)
                .transpose()?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(existing);
        }
        let due = entities::turn_run::Entity::find()
            .filter(entities::turn_run::Column::Status.is_in([
                TurnRunStatus::Queued.as_str(),
                TurnRunStatus::RetryWait.as_str(),
            ]))
            .filter(entities::turn_run::Column::AvailableAt.lte(now))
            .filter(entities::turn_run::Column::UpdatedAt.lte(now))
            .filter(
                sea_orm::sea_query::Expr::col(entities::turn_run::Column::AttemptCount).lt(
                    sea_orm::sea_query::Expr::col(entities::turn_run::Column::MaxAttempts),
                ),
            )
            .order_by_asc(entities::turn_run::Column::AvailableAt)
            .order_by_asc(entities::turn_run::Column::CreatedAt)
            .order_by_asc(entities::turn_run::Column::Id)
            .one(&transaction)
            .await
            .map_err(store_err)?;
        let expired = entities::turn_run::Entity::find()
            .filter(entities::turn_run::Column::Status.eq(TurnRunStatus::Running.as_str()))
            .filter(entities::turn_run::Column::LeaseExpiresAt.lte(now))
            .filter(entities::turn_run::Column::UpdatedAt.lte(now))
            .order_by_asc(entities::turn_run::Column::LeaseExpiresAt)
            .order_by_asc(entities::turn_run::Column::CreatedAt)
            .order_by_asc(entities::turn_run::Column::Id)
            .one(&transaction)
            .await
            .map_err(store_err)?;
        let candidate = match (due, expired) {
            (Some(due), Some(expired)) => {
                if turn_run_due_order(&due, &expired).is_le() {
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

        let reclaiming = candidate.status == TurnRunStatus::Running.as_str();
        if reclaiming && candidate.attempt_count >= candidate.max_attempts {
            let failed = entities::turn_run::Entity::update_many()
                .col_expr(
                    entities::turn_run::Column::Status,
                    sea_orm::sea_query::Expr::value(TurnRunStatus::Failed.as_str()),
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
                    entities::turn_run::Column::LastErrorCode,
                    sea_orm::sea_query::Expr::value(Some("lease_expired".to_owned())),
                )
                .col_expr(
                    entities::turn_run::Column::LastErrorDetail,
                    sea_orm::sea_query::Expr::value(Some("final worker lease expired".to_owned())),
                )
                .col_expr(
                    entities::turn_run::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(now),
                )
                .filter(entities::turn_run::Column::Id.eq(candidate.id))
                .filter(entities::turn_run::Column::Status.eq(TurnRunStatus::Running.as_str()))
                .filter(entities::turn_run::Column::AttemptCount.eq(candidate.attempt_count))
                .filter(entities::turn_run::Column::LeaseToken.eq(candidate.lease_token))
                .filter(entities::turn_run::Column::LeaseExpiresAt.eq(candidate.lease_expires_at))
                .filter(entities::turn_run::Column::UpdatedAt.eq(candidate.updated_at))
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
            AgentError::Store(format!("turn {} attempt overflow", TurnId(candidate.id)))
        })?;
        let receipt = entities::turn_claim::Entity::insert(entities::turn_claim::ActiveModel {
            token: Set(lease_token),
            turn_id: Set(candidate.id),
            attempt_count: Set(next_attempt),
            claimed_at: Set(now),
            lease_expires_at: Set(lease_expires_at),
        })
        .on_conflict_do_nothing()
        .exec_without_returning(&transaction)
        .await
        .map_err(store_err)?;
        if !matches!(receipt, TryInsertResult::Inserted(1)) {
            transaction.rollback().await.map_err(store_err)?;
            continue;
        }
        let claim = entities::turn_run::Entity::update_many()
            .col_expr(
                entities::turn_run::Column::Status,
                sea_orm::sea_query::Expr::value(TurnRunStatus::Running.as_str()),
            )
            .col_expr(
                entities::turn_run::Column::AttemptCount,
                sea_orm::sea_query::Expr::value(next_attempt),
            )
            .col_expr(
                entities::turn_run::Column::LeaseToken,
                sea_orm::sea_query::Expr::value(Some(lease_token)),
            )
            .col_expr(
                entities::turn_run::Column::LeaseExpiresAt,
                sea_orm::sea_query::Expr::value(Some(lease_expires_at)),
            )
            .col_expr(
                entities::turn_run::Column::StartedAt,
                sea_orm::sea_query::Expr::value(Some(candidate.started_at.unwrap_or(now))),
            )
            .col_expr(
                entities::turn_run::Column::LastErrorCode,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
            .col_expr(
                entities::turn_run::Column::LastErrorDetail,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
            .col_expr(
                entities::turn_run::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .filter(entities::turn_run::Column::Id.eq(candidate.id))
            .filter(entities::turn_run::Column::Status.eq(&candidate.status))
            .filter(entities::turn_run::Column::AttemptCount.eq(candidate.attempt_count))
            .filter(entities::turn_run::Column::UpdatedAt.eq(candidate.updated_at));
        let claim = if reclaiming {
            claim
                .filter(entities::turn_run::Column::LeaseToken.eq(candidate.lease_token))
                .filter(entities::turn_run::Column::LeaseExpiresAt.eq(candidate.lease_expires_at))
        } else {
            claim.filter(entities::turn_run::Column::AvailableAt.lte(now))
        };
        let claimed = claim.exec(&transaction).await.map_err(store_err)?;
        if claimed.rows_affected != 1 {
            transaction.rollback().await.map_err(store_err)?;
            continue;
        }

        let claimed = entities::turn_run::Entity::find_by_id(candidate.id)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| {
                AgentError::Store(format!("claimed turn {} disappeared", TurnId(candidate.id)))
            })
            .and_then(turn_run_from_model)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(claimed));
    }
}

pub(in crate::db) async fn heartbeat_turn_run(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
    lease_expires_at: chrono::DateTime<Utc>,
) -> Result<bool> {
    let now = canonical_db_timestamp(now)?;
    let lease_expires_at = canonical_db_timestamp(lease_expires_at)?;
    if lease_expires_at <= now {
        return Err(AgentError::Store(
            "turn lease expiry must be after heartbeat time".into(),
        ));
    }
    let heartbeat = entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Some(lease_expires_at)),
        )
        .col_expr(
            entities::turn_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::turn_run::Column::Id.eq(id.0))
        .filter(entities::turn_run::Column::Status.eq(TurnRunStatus::Running.as_str()))
        .filter(entities::turn_run::Column::LeaseToken.eq(lease_token))
        .filter(entities::turn_run::Column::LeaseExpiresAt.gt(now))
        .filter(entities::turn_run::Column::LeaseExpiresAt.lt(lease_expires_at))
        .filter(entities::turn_run::Column::UpdatedAt.lte(now))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(heartbeat.rows_affected == 1)
}

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

fn canonical_db_timestamp(timestamp: chrono::DateTime<Utc>) -> Result<chrono::DateTime<Utc>> {
    chrono::DateTime::from_timestamp_micros(timestamp.timestamp_micros()).ok_or_else(|| {
        AgentError::Store("turn output timestamp is outside the database range".into())
    })
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

async fn acquire_turn_claim_write_lock<C>(conn: &C) -> Result<()>
where
    C: ConnectionTrait,
{
    entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::col(entities::turn_run::Column::UpdatedAt).into(),
        )
        .filter(entities::turn_run::Column::Id.is_null())
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

fn turn_run_due_order(
    left: &entities::turn_run::Model,
    right: &entities::turn_run::Model,
) -> std::cmp::Ordering {
    let due_at = |run: &entities::turn_run::Model| {
        if run.status == TurnRunStatus::Running.as_str() {
            run.lease_expires_at.unwrap_or(run.available_at)
        } else {
            run.available_at
        }
    };
    due_at(left)
        .cmp(&due_at(right))
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.id.cmp(&right.id))
}

fn validate_turn_input(model: &str, content: &str) -> Result<()> {
    if model.trim().is_empty()
        || model.contains('\0')
        || model.chars().count() > TurnRun::MAX_MODEL_LEN
    {
        return Err(AgentError::Store(format!(
            "turn model must contain 1 to {} non-NUL characters",
            TurnRun::MAX_MODEL_LEN
        )));
    }
    if content.trim().is_empty() || content.contains('\0') {
        return Err(AgentError::Store(
            "turn content must be non-empty and contain no NUL characters".into(),
        ));
    }
    Ok(())
}

async fn find_active_turn_on<C>(
    conn: &C,
    chat_id: ChatId,
) -> Result<Option<entities::turn_run::Model>>
where
    C: ConnectionTrait,
{
    entities::turn_run::Entity::find()
        .filter(entities::turn_run::Column::ChatId.eq(chat_id.0))
        .filter(entities::turn_run::Column::Status.is_in([
            TurnRunStatus::Queued.as_str(),
            TurnRunStatus::Running.as_str(),
            TurnRunStatus::RetryWait.as_str(),
        ]))
        .one(conn)
        .await
        .map_err(store_err)
}

async fn exact_accepted_turn_on<C>(
    conn: &C,
    existing: entities::turn_run::Model,
    chat_id: ChatId,
    model: &str,
    content: &str,
) -> Result<TurnRun>
where
    C: ConnectionTrait,
{
    let message = entities::message::Entity::find_by_id(existing.input_message_id)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "turn {} is missing its input message",
                TurnId(existing.id)
            ))
        })?;
    if existing.chat_id != chat_id.0
        || existing.model != model
        || message.chat_id != chat_id.0
        || message.turn_id != existing.id
        || message.role != "user"
        || message.content != content
    {
        return Err(AgentError::Store(format!(
            "turn {} was already accepted with different input",
            TurnId(existing.id)
        )));
    }
    turn_run_from_model(existing)
}

fn turn_run_from_model(model: entities::turn_run::Model) -> Result<TurnRun> {
    Ok(TurnRun {
        id: TurnId(model.id),
        chat_id: ChatId(model.chat_id),
        input_message_id: MessageId(model.input_message_id),
        output_message_id: model.output_message_id.map(MessageId),
        model: model.model,
        status: turn_run_status_from_db(&model.status)?,
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

fn turn_run_status_from_db(text: &str) -> Result<TurnRunStatus> {
    match text {
        "queued" => Ok(TurnRunStatus::Queued),
        "running" => Ok(TurnRunStatus::Running),
        "retry_wait" => Ok(TurnRunStatus::RetryWait),
        "completed" => Ok(TurnRunStatus::Completed),
        "failed" => Ok(TurnRunStatus::Failed),
        "cancelled" => Ok(TurnRunStatus::Cancelled),
        other => Err(AgentError::Store(format!(
            "unknown durable turn status: {other}"
        ))),
    }
}
