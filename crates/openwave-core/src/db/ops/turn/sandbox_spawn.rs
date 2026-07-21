use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};

use crate::agent_tools::{
    parse_canonical_spawn_sandbox_agent_arguments, SpawnSandboxAgentArgs, SpawnSandboxAgentResult,
    SPAWN_SANDBOX_AGENT_TOOL,
};
use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::model::{
    AgentRun, SandboxSpawnCheckpoint, SandboxSpawnCheckpointRequest, ToolCallExecution,
    ToolCallRecord, ToolCallStatus, TurnCheckpointProgress, TurnRunStatus, TurnSteerStatus,
};
use crate::storage::{AdmitSandboxAgentRunOutcome, CheckpointSandboxSpawnOutcome};
use crate::{AgentRunId, ChatId, ToolOutput};

use super::super::super::{entities, store_err, DbStore};
use super::super::{
    acquire_chat_write_lock,
    agent_run::{
        acquire_agent_run_claim_lock, admit_sandbox_agent_run_on, agent_run_from_model,
        database_now,
    },
    client_execution::tool_call_from_model,
    conversation::append_event_on,
    next_tool_history_order_on,
};
use super::{canonical_db_timestamp, turn_run_from_model};

pub(in crate::db) async fn checkpoint_sandbox_spawn(
    store: &DbStore,
    request: &SandboxSpawnCheckpointRequest,
    now: chrono::DateTime<Utc>,
) -> Result<Option<CheckpointSandboxSpawnOutcome>> {
    if request.call_id.0.is_nil() {
        return Err(AgentError::Store(
            "sandbox spawn checkpoint call id must not be nil".into(),
        ));
    }
    canonical_db_timestamp(now)?;

    // The immutable receipt is authoritative after an ambiguous commit and is
    // intentionally consulted before any mutable lease or steer classification.
    if let Some(outcome) = recover_existing(&store.conn, request).await? {
        return Ok(Some(outcome));
    }
    validate_request(request)?;

    let Some(scope) = entities::turn_run::Entity::find_by_id(request.origin_turn_id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    let transaction = store.conn.begin().await.map_err(store_err)?;
    // Match every child terminal/delivery path: scheduler first, then chat and
    // turn. The exact checkpoint recovery above intentionally remains before
    // these mutable locks.
    acquire_agent_run_claim_lock(&transaction).await?;
    if !acquire_chat_write_lock(&transaction, ChatId(scope.chat_id)).await?
        || !super::super::acquire_turn_write_lock(&transaction, request.origin_turn_id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    if let Some(outcome) = recover_existing(&transaction, request).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(outcome));
    }

    let turn = entities::turn_run::Entity::find_by_id(request.origin_turn_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked turn exists");
    let db_now = database_now(&transaction).await?;

    let outcome = checkpoint_on(&transaction, &turn, request, db_now).await;
    match outcome {
        Ok(outcome) => {
            transaction.commit().await.map_err(store_err)?;
            Ok(Some(outcome))
        }
        Err(error) => {
            transaction.rollback().await.map_err(store_err)?;
            // A database error can be an ambiguous commit at the adapter
            // boundary. Recover exact identity before the caller classifies
            // mutable liveness on a retry.
            if let Some(outcome) = recover_existing(&store.conn, request).await? {
                Ok(Some(outcome))
            } else {
                Err(error)
            }
        }
    }
}

async fn checkpoint_on<C>(
    conn: &C,
    turn: &entities::turn_run::Model,
    request: &SandboxSpawnCheckpointRequest,
    now: chrono::DateTime<Utc>,
) -> Result<CheckpointSandboxSpawnOutcome>
where
    C: ConnectionTrait,
{
    let Some(claim) = entities::turn_claim::Entity::find_by_id(request.lease_token)
        .one(conn)
        .await
        .map_err(store_err)?
        .filter(|claim| claim.turn_id == turn.id)
    else {
        return Ok(CheckpointSandboxSpawnOutcome::LeaseLost);
    };
    if turn.status != TurnRunStatus::Running.as_str()
        || turn.attempt_count != claim.attempt_count
        || turn.claim_count != claim.claim_count
        || turn.lease_token != Some(request.lease_token)
        || turn
            .lease_expires_at
            .is_none_or(|lease_expires_at| lease_expires_at <= now)
        || turn.updated_at > now
    {
        return Ok(CheckpointSandboxSpawnOutcome::LeaseLost);
    }
    let steer_pending = entities::turn_steer::Entity::find()
        .filter(entities::turn_steer::Column::TurnId.eq(turn.id))
        .filter(entities::turn_steer::Column::Status.eq(TurnSteerStatus::Pending.as_str()))
        .one(conn)
        .await
        .map_err(store_err)?
        .is_some();
    if steer_pending || turn.steer_revision != request.expected_steer_revision {
        let turn = turn_run_from_model(turn.clone())?;
        return Ok(if steer_pending {
            CheckpointSandboxSpawnOutcome::SteerPending(turn)
        } else {
            CheckpointSandboxSpawnOutcome::OutputSuperseded(turn)
        });
    }

    let last_attempt_event = entities::event::Entity::find()
        .filter(entities::event::Column::LeaseToken.eq(request.lease_token))
        .order_by_desc(entities::event::Column::AttemptEventOrdinal)
        .one(conn)
        .await
        .map_err(store_err)?;
    let Some(last_ordinal) = last_attempt_event.and_then(|event| event.attempt_event_ordinal)
    else {
        return Ok(CheckpointSandboxSpawnOutcome::IdentityConflict);
    };
    let next_ordinal = last_ordinal
        .checked_add(1)
        .ok_or_else(|| AgentError::Store("turn attempt event ordinal exhausted".into()))?;
    if request.event_ordinal != next_ordinal {
        return Ok(CheckpointSandboxSpawnOutcome::IdentityConflict);
    }
    if entities::tool_call::Entity::find_by_id(request.call_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .is_some()
    {
        return Ok(CheckpointSandboxSpawnOutcome::IdentityConflict);
    }

    let arguments = canonical_arguments(request)?;
    let admission = admit_sandbox_agent_run_on(
        conn,
        turn,
        request.call_id,
        &arguments.task,
        arguments.resource.as_ref(),
        request.lease_token,
        request.expected_steer_revision,
        AgentRun::DEFAULT_MAX_OUTSTANDING_CHILDREN,
        now,
    )
    .await?;
    let child = match admission {
        AdmitSandboxAgentRunOutcome::Accepted { child, .. } => child,
        // A child created through the blocking/standalone boundary must never
        // be retrofitted into this stronger atomic receipt.
        AdmitSandboxAgentRunOutcome::Existing { .. }
        | AdmitSandboxAgentRunOutcome::IdentityConflict => {
            return Ok(CheckpointSandboxSpawnOutcome::IdentityConflict)
        }
        AdmitSandboxAgentRunOutcome::ParentUnavailable => {
            return Ok(CheckpointSandboxSpawnOutcome::ParentUnavailable)
        }
        AdmitSandboxAgentRunOutcome::DelegatedResourceUnavailable => {
            return Ok(CheckpointSandboxSpawnOutcome::DelegatedResourceUnavailable)
        }
        AdmitSandboxAgentRunOutcome::LeaseLost => {
            return Ok(CheckpointSandboxSpawnOutcome::LeaseLost)
        }
        AdmitSandboxAgentRunOutcome::AtCapacity => {
            return Ok(CheckpointSandboxSpawnOutcome::AtCapacity)
        }
        AdmitSandboxAgentRunOutcome::SteerPending(turn) => {
            return Ok(CheckpointSandboxSpawnOutcome::SteerPending(turn))
        }
        AdmitSandboxAgentRunOutcome::OutputSuperseded(turn) => {
            return Ok(CheckpointSandboxSpawnOutcome::OutputSuperseded(turn))
        }
    };

    let totals = checked_totals(turn, request.progress)?;
    let created_at = now.max(child.created_at);
    let history_order = next_tool_history_order_on(conn, ChatId(turn.chat_id)).await?;
    let call_model = entities::tool_call::ActiveModel {
        id: Set(request.call_id.0),
        chat_id: Set(turn.chat_id),
        turn_id: Set(turn.id),
        provider_id: Set(request.provider_id.clone()),
        history_order: Set(history_order),
        name: Set(SPAWN_SANDBOX_AGENT_TOOL.into()),
        arguments: Set(request.arguments.clone()),
        execution: Set(ToolCallExecution::Orchestration.as_str().into()),
        status: Set(ToolCallStatus::Completed.as_str().into()),
        result: Set(Some(request.result.clone())),
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
        created_at: Set(created_at),
        resolved_at: Set(Some(created_at)),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    let call = tool_call_from_model(call_model)?;
    let payload = AgentEvent::ToolCallCompleted {
        call_id: request.call_id,
        output: ToolOutput::text(request.result.clone()),
    };
    let event_seq = append_event_on(
        conn,
        ChatId(turn.chat_id),
        Some(request.origin_turn_id),
        Some(request.lease_token),
        Some(request.event_ordinal),
        None,
        &payload,
    )
    .await?;

    let checkpoint_model = entities::sandbox_spawn_checkpoint::ActiveModel {
        call_id: Set(request.call_id.0),
        child_run_id: Set(child.id.0),
        parent_run_id: Set(turn.agent_run_id),
        origin_turn_id: Set(turn.id),
        chat_id: Set(turn.chat_id),
        lease_token: Set(request.lease_token),
        attempt_count: Set(turn.attempt_count),
        claim_count: Set(turn.claim_count),
        provider_id: Set(request.provider_id.clone()),
        history_order: Set(history_order),
        arguments: Set(request.arguments.clone()),
        result: Set(request.result.clone()),
        steer_revision: Set(request.expected_steer_revision),
        event_ordinal: Set(request.event_ordinal),
        model_steps: Set(request.progress.model_steps),
        input_tokens: Set(i64::from(request.progress.usage.input_tokens)),
        output_tokens: Set(i64::from(request.progress.usage.output_tokens)),
        cache_read_input_tokens: Set(i64::from(request.progress.usage.cache_read_input_tokens)),
        cache_creation_input_tokens: Set(i64::from(
            request.progress.usage.cache_creation_input_tokens,
        )),
        event_seq: Set(event_seq),
        committed_at: Set(created_at),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;

    let resumed = entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::Status,
            sea_orm::sea_query::Expr::value(TurnRunStatus::Resuming.as_str()),
        )
        .col_expr(
            entities::turn_run::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(created_at),
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
            sea_orm::sea_query::Expr::value(created_at),
        )
        .filter(entities::turn_run::Column::Id.eq(turn.id))
        .filter(entities::turn_run::Column::Status.eq(TurnRunStatus::Running.as_str()))
        .filter(entities::turn_run::Column::AttemptCount.eq(turn.attempt_count))
        .filter(entities::turn_run::Column::ClaimCount.eq(turn.claim_count))
        .filter(entities::turn_run::Column::LeaseToken.eq(request.lease_token))
        .filter(entities::turn_run::Column::LeaseExpiresAt.eq(turn.lease_expires_at))
        .filter(entities::turn_run::Column::LeaseExpiresAt.gt(now))
        .filter(entities::turn_run::Column::SteerRevision.eq(request.expected_steer_revision))
        .filter(entities::turn_run::Column::UpdatedAt.eq(turn.updated_at))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if resumed.rows_affected != 1 {
        return Err(AgentError::Store(
            "sandbox spawn checkpoint lost its exact foreground claim".into(),
        ));
    }
    let turn = entities::turn_run::Entity::find_by_id(turn.id)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("sandbox spawn checkpoint lost its turn".into()))?;
    let checkpoint = checkpoint_from_model(checkpoint_model)?;
    Ok(CheckpointSandboxSpawnOutcome::Checkpointed {
        child,
        turn: Box::new(turn_run_from_model(turn)?),
        call,
        checkpoint,
        event: SequencedEvent {
            seq: event_seq,
            event: payload,
        },
    })
}

async fn recover_existing<C>(
    conn: &C,
    request: &SandboxSpawnCheckpointRequest,
) -> Result<Option<CheckpointSandboxSpawnOutcome>>
where
    C: ConnectionTrait,
{
    let Some(model) = entities::sandbox_spawn_checkpoint::Entity::find_by_id(request.call_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    let checkpoint = checkpoint_from_model(model)?;
    if !checkpoint_matches(&checkpoint, request) {
        return Ok(Some(CheckpointSandboxSpawnOutcome::IdentityConflict));
    }
    let child = entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::Id.eq(checkpoint.child_run_id.0))
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("sandbox spawn receipt lost its child".into()))?;
    let admission =
        entities::sandbox_agent_admission::Entity::find_by_id(checkpoint.child_run_id.0)
            .one(conn)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store("sandbox spawn receipt lost its admission".into()))?;
    let call_model = entities::tool_call::Entity::find_by_id(checkpoint.call_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("sandbox spawn receipt lost its tool call".into()))?;
    let event = entities::event::Entity::find_by_id((checkpoint.chat_id.0, checkpoint.event_seq))
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("sandbox spawn receipt lost its event".into()))?;
    let expected_event = AgentEvent::ToolCallCompleted {
        call_id: checkpoint.call_id,
        output: ToolOutput::text(checkpoint.result.clone()),
    };
    let stored_event: AgentEvent = serde_json::from_value(event.payload)?;
    let arguments = canonical_arguments(request)?;
    let raw_call_valid = call_model.error_code.is_none()
        && call_model.error_detail.is_none()
        && call_model.approval_status.is_none()
        && call_model.approval_class.is_none()
        && call_model.approval_kind.is_none()
        && call_model.approval_reason.is_none()
        && call_model.approval_requested_at.is_none()
        && call_model.approval_decided_at.is_none()
        && call_model.approval_event_seq.is_none()
        && call_model.client_executor_id.is_none()
        && call_model.client_lease_token.is_none()
        && call_model.client_lease_expires_at.is_none();
    let raw_history_order = call_model.history_order;
    let call = tool_call_from_model(call_model)?;
    if admission.child_run_id != checkpoint.child_run_id.0
        || admission.parent_run_id != checkpoint.parent_run_id.0
        || admission.origin_turn_id != checkpoint.origin_turn_id.0
        || admission.chat_id != checkpoint.chat_id.0
        || admission.spawn_call_id != checkpoint.call_id.0
        || admission.admitted_at > checkpoint.committed_at
        || child.id != checkpoint.child_run_id.0
        || child.chat_id != checkpoint.chat_id.0
        || child.parent_id != Some(checkpoint.parent_run_id.0)
        || child.parent_depth != Some(0)
        || child.spawn_call_id != Some(checkpoint.call_id.0)
        || child.execution != crate::AgentRunExecution::Sandbox.as_str()
        || child.depth != i16::from(AgentRun::MAX_DEPTH)
        || child.input.as_deref() != Some(arguments.task.as_str())
        || admission.delegated_root_id
            != arguments
                .resource
                .as_ref()
                .map(|resource| *resource.root_id.as_uuid())
        || admission.delegated_relative_path.as_deref()
            != arguments
                .resource
                .as_ref()
                .map(|resource| resource.relative_path.as_str())
        || child.created_at > checkpoint.committed_at
        || call.id != checkpoint.call_id
        || call.chat_id != checkpoint.chat_id
        || call.turn_id != checkpoint.origin_turn_id
        || call.provider_id != checkpoint.provider_id
        || raw_history_order != checkpoint.history_order
        || call.name != SPAWN_SANDBOX_AGENT_TOOL
        || call.arguments != checkpoint.arguments
        || call.execution != ToolCallExecution::Orchestration
        || call.status != ToolCallStatus::Completed
        || call.result.as_deref() != Some(checkpoint.result.as_str())
        || call.created_at != checkpoint.committed_at
        || call.resolved_at != Some(checkpoint.committed_at)
        || !raw_call_valid
        || event.turn_id != Some(checkpoint.origin_turn_id.0)
        || event.lease_token != Some(checkpoint.lease_token)
        || event.attempt_event_ordinal != Some(checkpoint.event_ordinal)
        || event.terminal
        || stored_event != expected_event
    {
        return Err(AgentError::Store(
            "sandbox spawn receipt references inconsistent durable state".into(),
        ));
    }
    Ok(Some(CheckpointSandboxSpawnOutcome::Existing {
        child: agent_run_from_model(child)?,
        call,
        checkpoint,
        event: SequencedEvent {
            seq: event.seq,
            event: stored_event,
        },
    }))
}

fn validate_request(request: &SandboxSpawnCheckpointRequest) -> Result<()> {
    let labels_valid = !request.provider_id.is_empty()
        && request.provider_id.len() <= ToolCallRecord::MAX_LABEL_LEN
        && !request.provider_id.contains('\0');
    let result_valid =
        !request.result.contains('\0') && request.result.len() <= ToolCallRecord::MAX_RESULT_BYTES;
    if request.origin_turn_id.0.is_nil()
        || request.lease_token.is_nil()
        || request.call_id.0.is_nil()
        || request.expected_steer_revision < 0
        || !(2..i32::MAX).contains(&request.event_ordinal)
        || request.progress.model_steps <= 0
        || !labels_valid
        || !result_valid
        || !serde_json::to_vec(&request.arguments)
            .is_ok_and(|value| value.len() <= ToolCallRecord::MAX_ARGUMENT_BYTES)
    {
        return Err(AgentError::Store(
            "invalid non-blocking sandbox spawn checkpoint".into(),
        ));
    }
    canonical_arguments(request)?;
    Ok(())
}

fn canonical_arguments(request: &SandboxSpawnCheckpointRequest) -> Result<SpawnSandboxAgentArgs> {
    let arguments = parse_canonical_spawn_sandbox_agent_arguments(&request.arguments)
        .ok_or_else(|| AgentError::Store("sandbox spawn arguments are not canonical".into()))?;
    let child_run_id = AgentRunId::sandbox_for_spawn_call(request.call_id);
    let canonical_result = serde_json::to_string(&SpawnSandboxAgentResult {
        agent_id: child_run_id,
    })?;
    if request.result != canonical_result {
        return Err(AgentError::Store(
            "sandbox spawn result is not canonical".into(),
        ));
    }
    Ok(arguments)
}

struct CheckpointTotals {
    model_steps: i32,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
}

fn checked_totals(
    turn: &entities::turn_run::Model,
    progress: TurnCheckpointProgress,
) -> Result<CheckpointTotals> {
    let model_steps = turn
        .model_steps
        .checked_add(progress.model_steps)
        .filter(|value| *value >= 0)
        .ok_or_else(|| AgentError::Store("turn model-step checkpoint overflowed".into()))?;
    Ok(CheckpointTotals {
        model_steps,
        input_tokens: checked_token("input", turn.input_tokens, progress.usage.input_tokens)?,
        output_tokens: checked_token("output", turn.output_tokens, progress.usage.output_tokens)?,
        cache_read_input_tokens: checked_token(
            "cache-read input",
            turn.cache_read_input_tokens,
            progress.usage.cache_read_input_tokens,
        )?,
        cache_creation_input_tokens: checked_token(
            "cache-creation input",
            turn.cache_creation_input_tokens,
            progress.usage.cache_creation_input_tokens,
        )?,
    })
}

fn checked_token(field: &str, current: i64, delta: u32) -> Result<i64> {
    current
        .checked_add(i64::from(delta))
        .filter(|value| u32::try_from(*value).is_ok())
        .ok_or_else(|| AgentError::Store(format!("turn {field} accounting overflowed")))
}

fn checkpoint_matches(
    checkpoint: &SandboxSpawnCheckpoint,
    request: &SandboxSpawnCheckpointRequest,
) -> bool {
    checkpoint.call_id == request.call_id
        && checkpoint.child_run_id == AgentRunId::sandbox_for_spawn_call(request.call_id)
        && checkpoint.origin_turn_id == request.origin_turn_id
        && checkpoint.lease_token == request.lease_token
        && checkpoint.provider_id == request.provider_id
        && checkpoint.arguments == request.arguments
        && checkpoint.result == request.result
        && checkpoint.steer_revision == request.expected_steer_revision
        && checkpoint.event_ordinal == request.event_ordinal
        && checkpoint.progress == request.progress
}

fn checkpoint_from_model(
    model: entities::sandbox_spawn_checkpoint::Model,
) -> Result<SandboxSpawnCheckpoint> {
    fn token(value: i64, field: &str) -> Result<u32> {
        u32::try_from(value)
            .map_err(|_| AgentError::Store(format!("invalid sandbox spawn {field} token count")))
    }
    Ok(SandboxSpawnCheckpoint {
        call_id: crate::CallId(model.call_id),
        child_run_id: AgentRunId(model.child_run_id),
        parent_run_id: AgentRunId(model.parent_run_id),
        origin_turn_id: crate::TurnId(model.origin_turn_id),
        chat_id: ChatId(model.chat_id),
        lease_token: model.lease_token,
        attempt_count: model.attempt_count,
        claim_count: model.claim_count,
        provider_id: model.provider_id,
        history_order: model.history_order,
        arguments: model.arguments,
        result: model.result,
        steer_revision: model.steer_revision,
        event_ordinal: model.event_ordinal,
        progress: TurnCheckpointProgress {
            model_steps: model.model_steps,
            usage: crate::Usage {
                input_tokens: token(model.input_tokens, "input")?,
                output_tokens: token(model.output_tokens, "output")?,
                cache_read_input_tokens: token(model.cache_read_input_tokens, "cache-read input")?,
                cache_creation_input_tokens: token(
                    model.cache_creation_input_tokens,
                    "cache-creation input",
                )?,
            },
        },
        event_seq: model.event_seq,
        committed_at: model.committed_at,
    })
}
