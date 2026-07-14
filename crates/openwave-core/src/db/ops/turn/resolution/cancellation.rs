use super::*;

pub(in crate::db) async fn request_turn_cancellation(
    store: &DbStore,
    id: TurnId,
    now: chrono::DateTime<Utc>,
) -> Result<Option<RequestTurnCancellationOutcome>> {
    Ok(request_turn_cancellation_inner(store, id, now, None)
        .await?
        .map(|resolution| resolution.outcome))
}

pub(in crate::db) async fn request_turn_cancellation_and_append_event(
    store: &DbStore,
    id: TurnId,
    now: chrono::DateTime<Utc>,
) -> Result<Option<JournaledTurnOutcome<RequestTurnCancellationOutcome>>> {
    let event = AgentEvent::TurnCancelled {
        usage: Usage::default(),
    };
    request_turn_cancellation_inner(store, id, now, Some(&event)).await
}

async fn request_turn_cancellation_inner(
    store: &DbStore,
    id: TurnId,
    now: chrono::DateTime<Utc>,
    terminal_event: Option<&AgentEvent>,
) -> Result<Option<JournaledTurnOutcome<RequestTurnCancellationOutcome>>> {
    let now = canonical_db_timestamp(now)?;
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
            let sequenced_event = if status == TurnRunStatus::Cancelled {
                existing_cancellation_event_on(&transaction, id, terminal_event.is_some()).await?
            } else {
                None
            };
            let turn = turn_run_from_model(turn)?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(JournaledTurnOutcome {
                outcome: RequestTurnCancellationOutcome::Existing(turn),
                terminal_event: sequenced_event,
            }));
        }
        TurnRunStatus::Completed | TurnRunStatus::Failed => {
            let turn = turn_run_from_model(turn)?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(JournaledTurnOutcome {
                outcome: RequestTurnCancellationOutcome::AlreadyTerminal(turn),
                terminal_event: None,
            }));
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
    let sequenced_event = if next_status == TurnRunStatus::Cancelled {
        append_terminal_event_on(&transaction, id, ChatId(turn.chat_id), None, terminal_event)
            .await?
    } else {
        None
    };
    transaction.commit().await.map_err(store_err)?;
    let outcome = if next_status == TurnRunStatus::Cancelling {
        RequestTurnCancellationOutcome::Requested(updated)
    } else {
        RequestTurnCancellationOutcome::Cancelled(updated)
    };
    Ok(Some(JournaledTurnOutcome {
        outcome,
        terminal_event: sequenced_event,
    }))
}

pub(in crate::db) async fn finish_turn_cancellation(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
) -> Result<Option<FinishTurnCancellationOutcome>> {
    Ok(
        finish_turn_cancellation_inner(store, id, lease_token, now, None)
            .await?
            .map(|resolution| resolution.outcome),
    )
}

pub(in crate::db) async fn finish_turn_cancellation_and_append_event(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
    usage: Usage,
) -> Result<Option<JournaledTurnOutcome<FinishTurnCancellationOutcome>>> {
    let event = AgentEvent::TurnCancelled { usage };
    finish_turn_cancellation_inner(store, id, lease_token, now, Some(&event)).await
}

async fn finish_turn_cancellation_inner(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
    terminal_event: Option<&AgentEvent>,
) -> Result<Option<JournaledTurnOutcome<FinishTurnCancellationOutcome>>> {
    if lease_token.is_nil() {
        return Err(AgentError::Store("turn lease token must not be nil".into()));
    }
    let now = canonical_db_timestamp(now)?;
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
        let sequenced_event =
            exact_terminal_event_on(&transaction, id, Some(lease_token), terminal_event).await?;
        let turn = turn_run_from_model(turn)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(JournaledTurnOutcome {
            outcome: FinishTurnCancellationOutcome::Existing(turn),
            terminal_event: sequenced_event,
        }));
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
    let sequenced_event = append_terminal_event_on(
        &transaction,
        id,
        ChatId(turn.chat_id),
        Some(lease_token),
        terminal_event,
    )
    .await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(JournaledTurnOutcome {
        outcome: FinishTurnCancellationOutcome::Cancelled(cancelled),
        terminal_event: sequenced_event,
    }))
}

async fn existing_cancellation_event_on<C>(
    conn: &C,
    id: TurnId,
    expected: bool,
) -> Result<Option<SequencedEvent>>
where
    C: ConnectionTrait,
{
    if !expected {
        return Ok(None);
    }
    let stored = entities::event::Entity::find()
        .filter(entities::event::Column::TurnId.eq(id.0))
        .filter(entities::event::Column::Terminal.eq(true))
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("cancelled turn {id} is missing its event")))?;
    let event = serde_json::from_value::<AgentEvent>(stored.payload)?;
    if !matches!(event, AgentEvent::TurnCancelled { .. }) {
        return Err(AgentError::Store(format!(
            "cancelled turn {id} has a different terminal event"
        )));
    }
    Ok(Some(SequencedEvent {
        seq: stored.seq,
        event,
    }))
}
