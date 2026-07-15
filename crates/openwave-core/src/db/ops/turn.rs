use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait, TryInsertResult,
};

use crate::error::{AgentError, AgentErrorInfo, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::{ChatId, MessageId, TurnId};
use crate::model::{TurnRun, TurnRunStatus};
use crate::storage::{AcceptTurnOutcome, ClaimScanTerminalEvent, ClaimTurnRunOutcome};

use super::super::{entities, store_err, DbStore};
use super::{
    acquire_chat_write_lock,
    conversation::{
        append_event_on, next_message_seq_on, reserve_message_identity_on,
        MESSAGE_IDENTITY_OWNER_MESSAGE,
    },
};

mod resolution;
mod steer;

pub(in crate::db) use resolution::{
    complete_turn_run, complete_turn_run_and_append_event, finish_turn_cancellation,
    finish_turn_cancellation_and_append_event, record_turn_run_failure,
    record_turn_run_failure_and_append_event, request_turn_cancellation,
    request_turn_cancellation_and_append_event,
};
pub(in crate::db) use steer::{accept_turn_steer, apply_turn_steer, list_pending_turn_steers};

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

pub(in crate::db) async fn recover_exact_terminal_event(
    store: &DbStore,
    turn_id: TurnId,
    lease_token: uuid::Uuid,
    expected: &AgentEvent,
) -> Result<Option<SequencedEvent>> {
    let Some(stored) = entities::event::Entity::find()
        .filter(entities::event::Column::TurnId.eq(turn_id.0))
        .filter(entities::event::Column::Terminal.eq(true))
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    if stored.lease_token != Some(lease_token) || stored.attempt_event_ordinal != Some(i32::MAX) {
        return Ok(None);
    }
    let Ok(event) = serde_json::from_value::<AgentEvent>(stored.payload) else {
        return Ok(None);
    };
    if event != *expected {
        return Ok(None);
    }
    Ok(Some(SequencedEvent {
        seq: stored.seq,
        event,
    }))
}

pub(in crate::db) async fn accept_turn(
    store: &DbStore,
    id: TurnId,
    chat_id: ChatId,
    model: &str,
    content: &str,
) -> Result<AcceptTurnOutcome> {
    validate_turn_input(id, model, content)?;

    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
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
        return Ok(existing);
    }

    if let Some(active) = find_active_turn_on(&transaction, chat_id).await? {
        let active = turn_run_from_model(active)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(AcceptTurnOutcome::ChatBusy(active));
    }

    let now = canonical_db_timestamp(Utc::now())?;
    let input_message_id = MessageId::new();
    if !reserve_message_identity_on(
        &transaction,
        input_message_id,
        chat_id,
        id,
        MESSAGE_IDENTITY_OWNER_MESSAGE,
    )
    .await?
    {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "turn input message identity {input_message_id} is already reserved"
        )));
    }
    let message = entities::message::ActiveModel {
        id: Set(input_message_id.0),
        chat_id: Set(chat_id.0),
        turn_id: Set(id.0),
        seq: Set(next_message_seq_on(&transaction, chat_id).await?),
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
        steer_revision: Set(0),
        last_steer_applied_at: Set(None),
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
                return exact_accepted_turn_on(&store.conn, existing, chat_id, model, content)
                    .await;
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
) -> Result<ClaimTurnRunOutcome> {
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
        if let Some(existing) = entities::event::Entity::find()
            .filter(entities::event::Column::ScanToken.eq(lease_token))
            .one(&transaction)
            .await
            .map_err(store_err)?
        {
            let terminal_event = claim_scan_terminal_event_from_model(existing, lease_token)?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(ClaimTurnRunOutcome {
                turn: None,
                terminal_event: Some(terminal_event),
            });
        }
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
            return Ok(ClaimTurnRunOutcome {
                turn: existing,
                terminal_event: None,
            });
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
            .filter(entities::turn_run::Column::Status.is_in([
                TurnRunStatus::Running.as_str(),
                TurnRunStatus::Cancelling.as_str(),
            ]))
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
            return Ok(ClaimTurnRunOutcome {
                turn: None,
                terminal_event: None,
            });
        };

        if candidate.status == TurnRunStatus::Cancelling.as_str() {
            let chat_id = ChatId(candidate.chat_id);
            if !acquire_chat_write_lock(&transaction, chat_id).await? {
                return Err(AgentError::Store(format!(
                    "turn {} references missing chat {chat_id}",
                    TurnId(candidate.id)
                )));
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
                .filter(entities::turn_run::Column::Id.eq(candidate.id))
                .filter(entities::turn_run::Column::Status.eq(TurnRunStatus::Cancelling.as_str()))
                .filter(entities::turn_run::Column::AttemptCount.eq(candidate.attempt_count))
                .filter(entities::turn_run::Column::LeaseToken.eq(candidate.lease_token))
                .filter(entities::turn_run::Column::LeaseExpiresAt.eq(candidate.lease_expires_at))
                .filter(entities::turn_run::Column::LeaseExpiresAt.lte(now))
                .filter(entities::turn_run::Column::UpdatedAt.eq(candidate.updated_at))
                .filter(entities::turn_run::Column::UpdatedAt.lte(now))
                .exec(&transaction)
                .await
                .map_err(store_err)?;
            if cancelled.rows_affected != 1 {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            }
            steer::reject_pending_turn_steers_on(&transaction, TurnId(candidate.id), now).await?;
            let event = AgentEvent::TurnCancelled {
                usage: crate::provider::Usage::default(),
            };
            let sequenced_event = append_claim_scan_terminal_event_on(
                &transaction,
                &candidate,
                chat_id,
                lease_token,
                &event,
            )
            .await?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(ClaimTurnRunOutcome {
                turn: None,
                terminal_event: Some(ClaimScanTerminalEvent {
                    chat_id,
                    turn_id: TurnId(candidate.id),
                    event: sequenced_event,
                }),
            });
        }

        let reclaiming = candidate.status == TurnRunStatus::Running.as_str();
        if reclaiming && candidate.attempt_count >= candidate.max_attempts {
            let chat_id = ChatId(candidate.chat_id);
            if !acquire_chat_write_lock(&transaction, chat_id).await? {
                return Err(AgentError::Store(format!(
                    "turn {} references missing chat {chat_id}",
                    TurnId(candidate.id)
                )));
            }
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
            steer::reject_pending_turn_steers_on(&transaction, TurnId(candidate.id), now).await?;
            let event = AgentEvent::TurnFailed {
                error: AgentErrorInfo {
                    kind: "lease_expired".into(),
                    message: "final worker lease expired".into(),
                },
            };
            let claim_token = candidate.lease_token.ok_or_else(|| {
                AgentError::Store(format!(
                    "terminalized turn {} is missing its claim token",
                    TurnId(candidate.id)
                ))
            })?;
            entities::turn_failure::ActiveModel {
                lease_token: Set(claim_token),
                turn_id: Set(candidate.id),
                attempt_count: Set(candidate.attempt_count),
                requested_retry_at: Set(None),
                error_code: Set("lease_expired".into()),
                error_detail: Set(Some("final worker lease expired".into())),
                resolved_at: Set(now),
                result_status: Set(TurnRunStatus::Failed.as_str().into()),
            }
            .insert(&transaction)
            .await
            .map_err(store_err)?;
            let sequenced_event = append_claim_scan_terminal_event_on(
                &transaction,
                &candidate,
                chat_id,
                lease_token,
                &event,
            )
            .await?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(ClaimTurnRunOutcome {
                turn: None,
                terminal_event: Some(ClaimScanTerminalEvent {
                    chat_id,
                    turn_id: TurnId(candidate.id),
                    event: sequenced_event,
                }),
            });
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
        .on_conflict(
            sea_orm::sea_query::OnConflict::new()
                .do_nothing()
                .to_owned(),
        )
        .do_nothing()
        .exec_without_returning(&transaction)
        .await
        .map_err(store_err)?;
        if !matches!(receipt, TryInsertResult::Inserted(1)) {
            transaction.rollback().await.map_err(store_err)?;
            if entities::turn_claim::Entity::find_by_id(lease_token)
                .one(&store.conn)
                .await
                .map_err(store_err)?
                .is_some()
            {
                continue;
            }
            let conflicting_attempt = entities::turn_claim::Entity::find()
                .filter(entities::turn_claim::Column::TurnId.eq(candidate.id))
                .filter(entities::turn_claim::Column::AttemptCount.eq(next_attempt))
                .one(&store.conn)
                .await
                .map_err(store_err)?;
            let current = entities::turn_run::Entity::find_by_id(candidate.id)
                .one(&store.conn)
                .await
                .map_err(store_err)?;
            if conflicting_attempt.is_some()
                && current
                    .as_ref()
                    .is_some_and(|turn| turn.attempt_count >= next_attempt)
            {
                continue;
            }
            return Err(AgentError::Store(format!(
                "turn {} claim receipt for attempt {next_attempt} exists before the turn advanced",
                TurnId(candidate.id)
            )));
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
        return Ok(ClaimTurnRunOutcome {
            turn: Some(claimed),
            terminal_event: None,
        });
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

pub(in crate::db) fn canonical_db_timestamp(
    timestamp: chrono::DateTime<Utc>,
) -> Result<chrono::DateTime<Utc>> {
    chrono::DateTime::from_timestamp_micros(timestamp.timestamp_micros())
        .ok_or_else(|| AgentError::Store("timestamp is outside the database range".into()))
}

async fn acquire_turn_claim_write_lock<C>(conn: &C) -> Result<()>
where
    C: ConnectionTrait,
{
    let locked = entities::turn_claim_lock::Entity::update_many()
        .col_expr(
            entities::turn_claim_lock::Column::Id,
            sea_orm::sea_query::Expr::col(entities::turn_claim_lock::Column::Id).into(),
        )
        .filter(entities::turn_claim_lock::Column::Id.eq(1))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if locked.rows_affected != 1 {
        return Err(AgentError::Store(
            "durable turn claim lock is missing".into(),
        ));
    }
    Ok(())
}

async fn append_claim_scan_terminal_event_on<C>(
    conn: &C,
    candidate: &entities::turn_run::Model,
    chat_id: ChatId,
    scan_token: uuid::Uuid,
    event: &AgentEvent,
) -> Result<SequencedEvent>
where
    C: ConnectionTrait,
{
    let lease_token = candidate.lease_token.ok_or_else(|| {
        AgentError::Store(format!(
            "terminalized turn {} is missing its claim token",
            TurnId(candidate.id)
        ))
    })?;
    let seq = append_event_on(
        conn,
        chat_id,
        Some(TurnId(candidate.id)),
        Some(lease_token),
        Some(i32::MAX),
        Some(scan_token),
        event,
    )
    .await?;
    Ok(SequencedEvent {
        seq,
        event: event.clone(),
    })
}

fn claim_scan_terminal_event_from_model(
    model: entities::event::Model,
    scan_token: uuid::Uuid,
) -> Result<ClaimScanTerminalEvent> {
    if model.scan_token != Some(scan_token) || !model.terminal {
        return Err(AgentError::Store(format!(
            "claim scan token {scan_token} references an invalid journal receipt"
        )));
    }
    let turn_id = model.turn_id.map(TurnId).ok_or_else(|| {
        AgentError::Store(format!(
            "claim scan token {scan_token} references an event without a turn"
        ))
    })?;
    Ok(ClaimScanTerminalEvent {
        chat_id: ChatId(model.chat_id),
        turn_id,
        event: SequencedEvent {
            seq: model.seq,
            event: serde_json::from_value(model.payload)?,
        },
    })
}

fn turn_run_due_order(
    left: &entities::turn_run::Model,
    right: &entities::turn_run::Model,
) -> std::cmp::Ordering {
    let due_at = |run: &entities::turn_run::Model| {
        if run.status == TurnRunStatus::Running.as_str()
            || run.status == TurnRunStatus::Cancelling.as_str()
        {
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

fn validate_turn_input(id: TurnId, model: &str, content: &str) -> Result<()> {
    if id.0.is_nil() {
        return Err(AgentError::Store("turn id must not be nil".into()));
    }
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
            TurnRunStatus::Cancelling.as_str(),
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
) -> Result<AcceptTurnOutcome>
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
    if message.chat_id != existing.chat_id
        || message.turn_id != existing.id
        || message.role != "user"
    {
        return Err(AgentError::Store(format!(
            "turn {} has inconsistent accepted input",
            TurnId(existing.id)
        )));
    }
    let exact =
        existing.chat_id == chat_id.0 && existing.model == model && message.content == content;
    Ok(if exact {
        AcceptTurnOutcome::Existing(turn_run_from_model(existing)?)
    } else {
        AcceptTurnOutcome::IdentityConflict
    })
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
        steer_revision: model.steer_revision,
        last_steer_applied_at: model.last_steer_applied_at,
        created_at: model.created_at,
        updated_at: model.updated_at,
    })
}

fn turn_run_status_from_db(text: &str) -> Result<TurnRunStatus> {
    match text {
        "queued" => Ok(TurnRunStatus::Queued),
        "running" => Ok(TurnRunStatus::Running),
        "cancelling" => Ok(TurnRunStatus::Cancelling),
        "retry_wait" => Ok(TurnRunStatus::RetryWait),
        "completed" => Ok(TurnRunStatus::Completed),
        "failed" => Ok(TurnRunStatus::Failed),
        "cancelled" => Ok(TurnRunStatus::Cancelled),
        other => Err(AgentError::Store(format!(
            "unknown durable turn status: {other}"
        ))),
    }
}
