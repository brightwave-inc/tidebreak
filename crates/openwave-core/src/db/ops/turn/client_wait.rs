use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set,
    TransactionTrait, TryInsertResult,
};

use crate::error::{AgentError, Result};
use crate::model::{
    ClientToolCallRequest, ToolCallExecution, ToolCallStatus, TurnCheckpointProgress,
    TurnClientWait, TurnClientWaitStatus, TurnRunStatus, TurnSteerStatus,
};
use crate::storage::ParkTurnForClientCallOutcome;
use crate::{AgentEvent, ChatId, SequencedEvent, TurnId, TurnRun};

use super::super::super::{entities, store_err, DbStore};
use super::super::{acquire_chat_write_lock, acquire_turn_write_lock, next_tool_history_order_on};
use super::{canonical_db_timestamp, turn_run_from_model};

pub(in crate::db) struct ClientWaitTurnTransition {
    pub turn: TurnRun,
    pub terminal_event: Option<SequencedEvent>,
}

pub(in crate::db) async fn park_turn_for_client_tool_call(
    store: &DbStore,
    turn_id: crate::TurnId,
    lease_token: uuid::Uuid,
    expected_steer_revision: i64,
    progress: TurnCheckpointProgress,
    now: chrono::DateTime<Utc>,
    call: &ClientToolCallRequest,
) -> Result<Option<ParkTurnForClientCallOutcome>> {
    validate_request(turn_id, lease_token, progress, call)?;
    let now = canonical_db_timestamp(now)?;
    let Some(scope) = entities::turn_run::Entity::find_by_id(turn_id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    if scope.chat_id != call.chat_id.0 {
        return Ok(None);
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, call.chat_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    if !acquire_turn_write_lock(&transaction, turn_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }

    if let Some(wait) = entities::turn_client_wait::Entity::find_by_id(call.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let existing_call = entities::tool_call::Entity::find_by_id(call.id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| {
                AgentError::Store(format!("client wait {} is missing its tool call", call.id))
            })?;
        let turn = entities::turn_run::Entity::find_by_id(turn_id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store(format!("parked turn {turn_id} disappeared")))?;
        let exact = wait.turn_id == turn_id.0
            && wait.chat_id == call.chat_id.0
            && wait.park_lease_token == lease_token
            && progress_from_wait_model(&wait)? == progress
            && exact_call_request(&existing_call, call);
        let outcome = if exact {
            ParkTurnForClientCallOutcome::Existing {
                turn: turn_run_from_model(turn)?,
                call: super::super::client_execution::tool_call_from_model(existing_call)?,
                wait: wait_from_model(wait)?,
            }
        } else {
            ParkTurnForClientCallOutcome::IdentityConflict
        };
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(outcome));
    }

    let turn = entities::turn_run::Entity::find_by_id(turn_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked turn exists");
    if turn.chat_id != call.chat_id.0
        || turn.status != TurnRunStatus::Running.as_str()
        || turn.lease_token != Some(lease_token)
        || turn
            .lease_expires_at
            .is_none_or(|lease_expires_at| lease_expires_at <= now)
        || turn.updated_at > now
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let steer_pending = entities::turn_steer::Entity::find()
        .filter(entities::turn_steer::Column::TurnId.eq(turn_id.0))
        .filter(entities::turn_steer::Column::Status.eq(TurnSteerStatus::Pending.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some();
    let stale_output = turn.steer_revision != expected_steer_revision;
    if stale_output || steer_pending {
        let turn = turn_run_from_model(turn)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(if steer_pending {
            ParkTurnForClientCallOutcome::SteerPending(turn)
        } else {
            ParkTurnForClientCallOutcome::OutputSuperseded(turn)
        }));
    }
    if entities::tool_call::Entity::find_by_id(call.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some()
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(ParkTurnForClientCallOutcome::IdentityConflict));
    }
    let totals = checked_checkpoint_totals(&turn, progress)?;
    let history_order = next_tool_history_order_on(&transaction, call.chat_id).await?;

    let inserted = entities::tool_call::Entity::insert(entities::tool_call::ActiveModel {
        id: Set(call.id.0),
        chat_id: Set(call.chat_id.0),
        turn_id: Set(turn_id.0),
        provider_id: Set(call.provider_id.clone()),
        history_order: Set(history_order),
        name: Set(call.name.clone()),
        arguments: Set(call.arguments.clone()),
        execution: Set(ToolCallExecution::Client.as_str().into()),
        status: Set(ToolCallStatus::Pending.as_str().into()),
        result: Set(None),
        error_code: Set(None),
        error_detail: Set(None),
        approval_status: Set(None),
        approval_class: Set(None),
        approval_kind: Set(None),
        approval_reason: Set(None),
        approval_requested_at: Set(None),
        approval_decided_at: Set(None),
        approval_event_seq: Set(None),
        client_executor_id: Set(None),
        client_lease_token: Set(None),
        client_lease_expires_at: Set(None),
        created_at: Set(now),
        resolved_at: Set(None),
    })
    .on_conflict(
        sea_orm::sea_query::OnConflict::column(entities::tool_call::Column::Id)
            .do_nothing()
            .to_owned(),
    )
    .try_insert()
    .exec_without_returning(&transaction)
    .await
    .map_err(store_err)?;
    if !matches!(inserted, TryInsertResult::Inserted(1)) {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(Some(ParkTurnForClientCallOutcome::IdentityConflict));
    }
    let wait = entities::turn_client_wait::ActiveModel {
        call_id: Set(call.id.0),
        turn_id: Set(turn_id.0),
        chat_id: Set(call.chat_id.0),
        park_lease_token: Set(lease_token),
        attempt_count: Set(turn.attempt_count),
        claim_count: Set(turn.claim_count),
        model_steps: Set(progress.model_steps),
        input_tokens: Set(i64::from(progress.usage.input_tokens)),
        output_tokens: Set(i64::from(progress.usage.output_tokens)),
        cache_read_input_tokens: Set(i64::from(progress.usage.cache_read_input_tokens)),
        cache_creation_input_tokens: Set(i64::from(progress.usage.cache_creation_input_tokens)),
        status: Set(TurnClientWaitStatus::Waiting.as_str().into()),
        parked_at: Set(now),
        closed_at: Set(None),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    let parked = entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::Status,
            sea_orm::sea_query::Expr::value(TurnRunStatus::WaitingForClient.as_str()),
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
            entities::turn_run::Column::ModelSteps,
            sea_orm::sea_query::Expr::value(totals.model_steps),
        )
        .col_expr(
            entities::turn_run::Column::InputTokens,
            sea_orm::sea_query::Expr::value(totals.input_tokens),
        )
        .col_expr(
            entities::turn_run::Column::OutputTokens,
            sea_orm::sea_query::Expr::value(totals.output_tokens),
        )
        .col_expr(
            entities::turn_run::Column::CacheReadInputTokens,
            sea_orm::sea_query::Expr::value(totals.cache_read_input_tokens),
        )
        .col_expr(
            entities::turn_run::Column::CacheCreationInputTokens,
            sea_orm::sea_query::Expr::value(totals.cache_creation_input_tokens),
        )
        .col_expr(
            entities::turn_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::turn_run::Column::Id.eq(turn_id.0))
        .filter(entities::turn_run::Column::Status.eq(TurnRunStatus::Running.as_str()))
        .filter(entities::turn_run::Column::AttemptCount.eq(turn.attempt_count))
        .filter(entities::turn_run::Column::ClaimCount.eq(turn.claim_count))
        .filter(entities::turn_run::Column::LeaseToken.eq(lease_token))
        .filter(entities::turn_run::Column::LeaseExpiresAt.eq(turn.lease_expires_at))
        .filter(entities::turn_run::Column::LeaseExpiresAt.gt(now))
        .filter(entities::turn_run::Column::SteerRevision.eq(expected_steer_revision))
        .filter(entities::turn_run::Column::UpdatedAt.eq(turn.updated_at))
        .filter(entities::turn_run::Column::UpdatedAt.lte(now))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if parked.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let parked_turn = entities::turn_run::Entity::find_by_id(turn_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("parked turn {turn_id} disappeared")))?;
    let inserted_call = entities::tool_call::Entity::find_by_id(call.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("parked call {} disappeared", call.id)))?;
    let outcome = ParkTurnForClientCallOutcome::Parked {
        turn: turn_run_from_model(parked_turn)?,
        call: super::super::client_execution::tool_call_from_model(inserted_call)?,
        wait: wait_from_model(wait)?,
    };
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(outcome))
}

fn validate_request(
    turn_id: crate::TurnId,
    lease_token: uuid::Uuid,
    progress: TurnCheckpointProgress,
    call: &ClientToolCallRequest,
) -> Result<()> {
    if turn_id.0.is_nil()
        || lease_token.is_nil()
        || call.turn_id != turn_id
        || progress.model_steps <= 0
        || !call.is_well_formed()
    {
        return Err(AgentError::Store(
            "invalid client tool call checkpoint request".into(),
        ));
    }
    Ok(())
}

fn exact_call_request(
    existing: &entities::tool_call::Model,
    request: &ClientToolCallRequest,
) -> bool {
    existing.id == request.id.0
        && existing.chat_id == request.chat_id.0
        && existing.turn_id == request.turn_id.0
        && existing.provider_id == request.provider_id
        && existing.name == request.name
        && existing.arguments == request.arguments
        && existing.execution == ToolCallExecution::Client.as_str()
}

pub(super) fn wait_from_model(model: entities::turn_client_wait::Model) -> Result<TurnClientWait> {
    let status = match model.status.as_str() {
        "waiting" => TurnClientWaitStatus::Waiting,
        "resumed" => TurnClientWaitStatus::Resumed,
        "cancelled" => TurnClientWaitStatus::Cancelled,
        other => {
            return Err(AgentError::Store(format!(
                "unknown durable turn client wait status: {other}"
            )))
        }
    };
    Ok(TurnClientWait {
        call_id: crate::CallId(model.call_id),
        turn_id: crate::TurnId(model.turn_id),
        chat_id: crate::ChatId(model.chat_id),
        park_lease_token: model.park_lease_token,
        attempt_count: model.attempt_count,
        claim_count: model.claim_count,
        progress: progress_from_wait_model(&model)?,
        status,
        parked_at: model.parked_at,
        closed_at: model.closed_at,
    })
}

fn progress_from_wait_model(
    model: &entities::turn_client_wait::Model,
) -> Result<TurnCheckpointProgress> {
    if model.model_steps <= 0 {
        return Err(AgentError::Store(format!(
            "client wait {} has invalid model-step progress",
            crate::CallId(model.call_id)
        )));
    }
    Ok(TurnCheckpointProgress {
        model_steps: model.model_steps,
        usage: crate::provider::Usage {
            input_tokens: checkpoint_tokens_from_db(
                model.call_id,
                "input_tokens",
                model.input_tokens,
            )?,
            output_tokens: checkpoint_tokens_from_db(
                model.call_id,
                "output_tokens",
                model.output_tokens,
            )?,
            cache_read_input_tokens: checkpoint_tokens_from_db(
                model.call_id,
                "cache_read_input_tokens",
                model.cache_read_input_tokens,
            )?,
            cache_creation_input_tokens: checkpoint_tokens_from_db(
                model.call_id,
                "cache_creation_input_tokens",
                model.cache_creation_input_tokens,
            )?,
        },
    })
}

fn checkpoint_tokens_from_db(call_id: uuid::Uuid, field: &str, value: i64) -> Result<u32> {
    u32::try_from(value).map_err(|_| {
        AgentError::Store(format!(
            "client wait {} has invalid {field}",
            crate::CallId(call_id)
        ))
    })
}

struct CheckpointTotals {
    model_steps: i32,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
}

fn checked_checkpoint_totals(
    turn: &entities::turn_run::Model,
    progress: TurnCheckpointProgress,
) -> Result<CheckpointTotals> {
    let model_steps = turn
        .model_steps
        .checked_add(progress.model_steps)
        .filter(|total| *total >= 0)
        .ok_or_else(|| AgentError::Store("turn model-step accounting overflowed".into()))?;
    Ok(CheckpointTotals {
        model_steps,
        input_tokens: checked_token_total(
            "input_tokens",
            turn.input_tokens,
            progress.usage.input_tokens,
        )?,
        output_tokens: checked_token_total(
            "output_tokens",
            turn.output_tokens,
            progress.usage.output_tokens,
        )?,
        cache_read_input_tokens: checked_token_total(
            "cache_read_input_tokens",
            turn.cache_read_input_tokens,
            progress.usage.cache_read_input_tokens,
        )?,
        cache_creation_input_tokens: checked_token_total(
            "cache_creation_input_tokens",
            turn.cache_creation_input_tokens,
            progress.usage.cache_creation_input_tokens,
        )?,
    })
}

fn checked_token_total(field: &str, current: i64, delta: u32) -> Result<i64> {
    let total = current
        .checked_add(i64::from(delta))
        .filter(|total| u32::try_from(*total).is_ok())
        .ok_or_else(|| AgentError::Store(format!("turn {field} accounting overflowed")))?;
    Ok(total)
}

pub(in crate::db) async fn advance_turn_after_client_resolution_on<C>(
    conn: &C,
    call: &entities::tool_call::Model,
    resolved_at: chrono::DateTime<Utc>,
) -> Result<Option<ClientWaitTurnTransition>>
where
    C: ConnectionTrait,
{
    let Some(wait) = entities::turn_client_wait::Entity::find_by_id(call.id)
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    if wait.status != TurnClientWaitStatus::Waiting.as_str() {
        return recover_turn_after_client_resolution_on(conn, call).await;
    }
    if wait.chat_id != call.chat_id || wait.turn_id != call.turn_id {
        return Err(AgentError::Store(format!(
            "client wait {} is scoped differently from its tool call",
            crate::CallId(call.id)
        )));
    }
    let turn_id = TurnId(wait.turn_id);
    if !acquire_turn_write_lock(conn, turn_id).await? {
        return Err(AgentError::Store(format!(
            "client wait {} references missing turn {turn_id}",
            crate::CallId(call.id)
        )));
    }
    let turn = entities::turn_run::Entity::find_by_id(wait.turn_id)
        .one(conn)
        .await
        .map_err(store_err)?
        .expect("locked client-wait turn exists");
    if turn.chat_id != wait.chat_id
        || !matches!(
            turn.status.as_str(),
            "waiting_for_client" | "cancelling_client"
        )
        || turn.attempt_count != wait.attempt_count
        || turn.claim_count != wait.claim_count
        || turn.lease_token.is_some()
        || turn.lease_expires_at.is_some()
        || resolved_at < wait.parked_at
        || resolved_at < turn.updated_at
    {
        return Err(AgentError::Store(format!(
            "client wait {} does not match its blocking turn state",
            crate::CallId(call.id)
        )));
    }
    let cancelling = turn.status == TurnRunStatus::CancellingClient.as_str();
    let wait_status = if cancelling {
        TurnClientWaitStatus::Cancelled
    } else {
        TurnClientWaitStatus::Resumed
    };
    let closed = entities::turn_client_wait::Entity::update_many()
        .col_expr(
            entities::turn_client_wait::Column::Status,
            sea_orm::sea_query::Expr::value(wait_status.as_str()),
        )
        .col_expr(
            entities::turn_client_wait::Column::ClosedAt,
            sea_orm::sea_query::Expr::value(Some(resolved_at)),
        )
        .filter(entities::turn_client_wait::Column::CallId.eq(call.id))
        .filter(
            entities::turn_client_wait::Column::Status.eq(TurnClientWaitStatus::Waiting.as_str()),
        )
        .filter(entities::turn_client_wait::Column::ClosedAt.is_null())
        .exec(conn)
        .await
        .map_err(store_err)?;
    if closed.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "client wait {} changed while closing",
            crate::CallId(call.id)
        )));
    }
    let next_status = if cancelling {
        TurnRunStatus::Cancelled
    } else {
        TurnRunStatus::Resuming
    };
    let mut update = entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::Status,
            sea_orm::sea_query::Expr::value(next_status.as_str()),
        )
        .col_expr(
            entities::turn_run::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(resolved_at),
        )
        .col_expr(
            entities::turn_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(resolved_at),
        )
        .filter(entities::turn_run::Column::Id.eq(turn.id))
        .filter(entities::turn_run::Column::Status.eq(&turn.status))
        .filter(entities::turn_run::Column::AttemptCount.eq(turn.attempt_count))
        .filter(entities::turn_run::Column::ClaimCount.eq(turn.claim_count))
        .filter(entities::turn_run::Column::UpdatedAt.eq(turn.updated_at));
    if cancelling {
        update = update.col_expr(
            entities::turn_run::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(resolved_at)),
        );
    }
    let advanced = update.exec(conn).await.map_err(store_err)?;
    if advanced.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "client wait {} lost its blocking turn transition",
            crate::CallId(call.id)
        )));
    }
    let terminal_event = if cancelling {
        super::steer::reject_pending_turn_steers_on(conn, turn_id, resolved_at).await?;
        let event = AgentEvent::TurnCancelled {
            usage: super::usage_from_turn_model(&turn)?,
        };
        super::resolution::append_terminal_event_on(
            conn,
            turn_id,
            ChatId(turn.chat_id),
            None,
            Some(&event),
        )
        .await?
    } else {
        None
    };
    let updated = entities::turn_run::Entity::find_by_id(turn.id)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!("resolved client-wait turn {turn_id} disappeared"))
        })?;
    Ok(Some(ClientWaitTurnTransition {
        turn: turn_run_from_model(updated)?,
        terminal_event,
    }))
}

pub(in crate::db) async fn recover_turn_after_client_resolution_on<C>(
    conn: &C,
    call: &entities::tool_call::Model,
) -> Result<Option<ClientWaitTurnTransition>>
where
    C: ConnectionTrait,
{
    let Some(wait) = entities::turn_client_wait::Entity::find_by_id(call.id)
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    if wait.chat_id != call.chat_id || wait.turn_id != call.turn_id {
        return Err(AgentError::Store(format!(
            "client wait {} is scoped differently from its tool call",
            crate::CallId(call.id)
        )));
    }
    if wait.status == TurnClientWaitStatus::Waiting.as_str() {
        return Err(AgentError::Store(format!(
            "terminal client call {} still has an open wait",
            crate::CallId(call.id)
        )));
    }
    let turn_id = TurnId(wait.turn_id);
    let turn = entities::turn_run::Entity::find_by_id(wait.turn_id)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "closed client wait {} references missing turn {turn_id}",
                crate::CallId(call.id)
            ))
        })?;
    if turn.chat_id != wait.chat_id {
        return Err(AgentError::Store(format!(
            "closed client wait {} no longer matches its turn scope",
            crate::CallId(call.id)
        )));
    }
    let terminal_event = if wait.status == TurnClientWaitStatus::Cancelled.as_str() {
        if turn.status != TurnRunStatus::Cancelled.as_str() {
            return Err(AgentError::Store(format!(
                "cancelled client wait {} has a non-cancelled turn",
                crate::CallId(call.id)
            )));
        }
        Some(existing_client_cancellation_event_on(conn, turn_id).await?)
    } else if wait.status == TurnClientWaitStatus::Resumed.as_str() {
        None
    } else {
        return Err(AgentError::Store(format!(
            "client wait {} has unknown status {}",
            crate::CallId(call.id),
            wait.status
        )));
    };
    Ok(Some(ClientWaitTurnTransition {
        turn: turn_run_from_model(turn)?,
        terminal_event,
    }))
}

async fn existing_client_cancellation_event_on<C>(
    conn: &C,
    turn_id: TurnId,
) -> Result<SequencedEvent>
where
    C: ConnectionTrait,
{
    let stored = entities::event::Entity::find()
        .filter(entities::event::Column::TurnId.eq(turn_id.0))
        .filter(entities::event::Column::Terminal.eq(true))
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "client-cancelled turn {turn_id} is missing its terminal event"
            ))
        })?;
    let event = serde_json::from_value::<AgentEvent>(stored.payload)?;
    if !matches!(event, AgentEvent::TurnCancelled { .. }) {
        return Err(AgentError::Store(format!(
            "client-cancelled turn {turn_id} has a different terminal event"
        )));
    }
    Ok(SequencedEvent {
        seq: stored.seq,
        event,
    })
}
