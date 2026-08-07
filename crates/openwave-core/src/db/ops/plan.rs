use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::model::{ToolCallExecution, ToolCallStatus, TurnRunStatus};
use crate::storage::DecidePlanOutcome;
use crate::{
    plan_decision_result, CallId, ChatId, DecidePlanRequest, ExitPlanModeArgs, PendingPlanApproval,
    PlanDecisionChoice, PlanRequestStatus, TurnId, EXIT_PLAN_MODE_TOOL,
};

use super::super::{entities, store_err, DbStore};
use super::turn::{canonical_db_timestamp, recover_turn_after_client_resolution_on};
use super::{acquire_chat_write_lock, acquire_tool_call_write_lock, acquire_turn_write_lock};

/// Commit the bounded renderer projection and its journal refresh hint inside
/// the caller's already-fenced client-wait transaction.
pub(in crate::db) async fn checkpoint_on<C>(
    conn: &C,
    call: &crate::ClientToolCallRequest,
    proposed_at: DateTime<Utc>,
) -> Result<Option<SequencedEvent>>
where
    C: ConnectionTrait,
{
    if call.name != EXIT_PLAN_MODE_TOOL {
        return Ok(None);
    }
    let arguments = parse_arguments(&call.arguments)?;
    let event = AgentEvent::PlanProposed {
        call_id: call.id,
        turn_id: call.turn_id,
    };
    let seq =
        super::conversation::append_event_on(conn, call.chat_id, None, None, None, None, &event)
            .await?;
    entities::plan_request::ActiveModel {
        call_id: Set(call.id.0),
        turn_id: Set(call.turn_id.0),
        chat_id: Set(call.chat_id.0),
        status: Set(PlanRequestStatus::Pending.as_str().into()),
        event_seq: Set(seq),
        title: Set(arguments.title),
        plan: Set(arguments.plan),
        feedback: Set(None),
        proposed_at: Set(proposed_at),
        resolved_at: Set(None),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    Ok(Some(SequencedEvent { seq, event }))
}

/// Recover and validate the exact committed renderer hint for an ambiguous
/// checkpoint retry.
pub(in crate::db) async fn recover_checkpoint_on<C>(
    conn: &C,
    call: &crate::ClientToolCallRequest,
) -> Result<Option<SequencedEvent>>
where
    C: ConnectionTrait,
{
    if call.name != EXIT_PLAN_MODE_TOOL {
        return Ok(None);
    }
    let expected = parse_arguments(&call.arguments)?;
    let request = entities::plan_request::Entity::find_by_id(call.id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "plan checkpoint {} is missing its renderer receipt",
                call.id
            ))
        })?;
    if request.chat_id != call.chat_id.0 || request.turn_id != call.turn_id.0 {
        return Err(AgentError::Store(format!(
            "plan checkpoint {} has mismatched scope",
            call.id
        )));
    }
    if request.status != PlanRequestStatus::Pending.as_str() {
        if [
            PlanRequestStatus::Accepted,
            PlanRequestStatus::Rejected,
            PlanRequestStatus::Cancelled,
        ]
        .iter()
        .any(|status| request.status == status.as_str())
        {
            return Ok(None);
        }
        return Err(AgentError::Store(format!(
            "plan checkpoint {} has unknown status {}",
            call.id, request.status
        )));
    }
    if request.title != expected.title || request.plan != expected.plan {
        return Err(AgentError::Store(format!(
            "plan checkpoint {} has mismatched presentation data",
            call.id
        )));
    }
    let stored = entities::event::Entity::find_by_id((call.chat_id.0, request.event_seq))
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("plan renderer event is missing".into()))?;
    let event = serde_json::from_value::<AgentEvent>(stored.payload)?;
    let expected_event = AgentEvent::PlanProposed {
        call_id: call.id,
        turn_id: call.turn_id,
    };
    if stored.turn_id.is_some() || stored.terminal || event != expected_event {
        return Err(AgentError::Store(
            "plan renderer event does not match its checkpoint".into(),
        ));
    }
    Ok(Some(SequencedEvent {
        seq: request.event_seq,
        event,
    }))
}

pub(in crate::db) async fn list_pending(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<PendingPlanApproval>> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(Vec::new());
    }
    let requests = entities::plan_request::Entity::find()
        .filter(entities::plan_request::Column::ChatId.eq(chat_id.0))
        .filter(entities::plan_request::Column::Status.eq(PlanRequestStatus::Pending.as_str()))
        .order_by_asc(entities::plan_request::Column::ProposedAt)
        .order_by_asc(entities::plan_request::Column::CallId)
        .all(&transaction)
        .await
        .map_err(store_err)?;
    let mut pending = Vec::with_capacity(requests.len());
    for request in requests {
        let call = entities::tool_call::Entity::find_by_id(request.call_id)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store("pending plan call is missing".into()))?;
        let wait = entities::turn_client_wait::Entity::find_by_id(request.call_id)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store("pending plan wait is missing".into()))?;
        let turn = entities::turn_run::Entity::find_by_id(request.turn_id)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store("pending plan turn is missing".into()))?;
        if call.chat_id != request.chat_id
            || call.turn_id != request.turn_id
            || call.name != EXIT_PLAN_MODE_TOOL
            || call.execution != ToolCallExecution::Orchestration.as_str()
            || call.status != ToolCallStatus::Pending.as_str()
            || call.client_executor_id.is_some()
            || wait.chat_id != request.chat_id
            || wait.turn_id != request.turn_id
            || wait.status != crate::TurnClientWaitStatus::Waiting.as_str()
            || turn.chat_id != request.chat_id
            || turn.status != TurnRunStatus::WaitingForClient.as_str()
            || turn.attempt_count != wait.attempt_count
            || turn.claim_count != wait.claim_count
        {
            return Err(AgentError::Store(
                "pending plan projection does not match its live continuation".into(),
            ));
        }
        pending.push(PendingPlanApproval {
            call_id: CallId(request.call_id),
            turn_id: TurnId(request.turn_id),
            title: request.title,
            plan: request.plan,
            proposed_at: request.proposed_at,
        });
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(pending)
}

pub(in crate::db) async fn decide(
    store: &DbStore,
    request: &DecidePlanRequest,
    decided_at: DateTime<Utc>,
) -> Result<DecidePlanOutcome> {
    if request.chat_id.0.is_nil()
        || request.call_id.0.is_nil()
        || !request.decision.shape_is_well_formed()
    {
        return Ok(DecidePlanOutcome::InvalidDecision);
    }
    let requested_at = canonical_db_timestamp(decided_at)?;
    let Some(scope) = entities::plan_request::Entity::find_by_id(request.call_id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(DecidePlanOutcome::Unavailable);
    };
    if scope.chat_id != request.chat_id.0 {
        return Ok(DecidePlanOutcome::Unavailable);
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, request.chat_id).await?
        || !acquire_turn_write_lock(&transaction, TurnId(scope.turn_id)).await?
        || !acquire_tool_call_write_lock(&transaction, request.call_id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(DecidePlanOutcome::Unavailable);
    }
    let call = entities::tool_call::Entity::find_by_id(request.call_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked plan call exists");
    if call.chat_id != request.chat_id.0
        || call.name != EXIT_PLAN_MODE_TOOL
        || call.execution != ToolCallExecution::Orchestration.as_str()
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(DecidePlanOutcome::Unavailable);
    }
    let plan_request = entities::plan_request::Entity::find_by_id(request.call_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "plan call {} is missing its request",
                request.call_id
            ))
        })?;
    if plan_request.chat_id != request.chat_id.0 || plan_request.turn_id != call.turn_id {
        transaction.commit().await.map_err(store_err)?;
        return Ok(DecidePlanOutcome::Unavailable);
    }
    let decided_status = match request.decision.decision {
        PlanDecisionChoice::Accept => PlanRequestStatus::Accepted,
        PlanDecisionChoice::Reject => PlanRequestStatus::Rejected,
    };
    let result = serde_json::to_string(&plan_decision_result(&request.decision))?;

    if plan_request.status != PlanRequestStatus::Pending.as_str() {
        if plan_request.status != decided_status.as_str() {
            transaction.commit().await.map_err(store_err)?;
            return Ok(DecidePlanOutcome::DecisionConflict);
        }
        let exact = plan_request.feedback == request.decision.feedback
            && call.status == ToolCallStatus::Completed.as_str()
            && call.result.as_deref() == Some(result.as_str());
        if !exact {
            transaction.commit().await.map_err(store_err)?;
            return Ok(DecidePlanOutcome::DecisionConflict);
        }
        let transition = recover_turn_after_client_resolution_on(&transaction, &call)
            .await?
            .ok_or_else(|| {
                AgentError::Store(format!(
                    "decided plan {} is missing its client wait",
                    request.call_id
                ))
            })?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(DecidePlanOutcome::Existing(transition.turn));
    }
    if call.status != ToolCallStatus::Pending.as_str()
        || call.client_executor_id.is_some()
        || call.client_lease_token.is_some()
        || call.client_lease_expires_at.is_some()
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(DecidePlanOutcome::Unavailable);
    }
    let turn = entities::turn_run::Entity::find_by_id(call.turn_id)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked plan turn exists");
    if turn.chat_id != request.chat_id.0
        || turn.status != TurnRunStatus::WaitingForClient.as_str()
        || plan_request.turn_id != turn.id
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(DecidePlanOutcome::Unavailable);
    }
    let database_now = super::agent_run::database_now(&transaction).await?;
    let decided_at = requested_at
        .max(database_now)
        .max(plan_request.proposed_at)
        .max(call.created_at)
        .max(turn.updated_at);

    // Accepting is the one place a permission mode changes as a side effect:
    // the chat leaves plan mode inside the same transaction that completes
    // the call, so the resumed turn can never read the plan surface again
    // while believing the plan was accepted.
    if let Some(mode) = request.decision.mode_after() {
        let chat = entities::chat::Entity::find_by_id(request.chat_id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store("plan chat disappeared".into()))?;
        let mut active_chat: entities::chat::ActiveModel = chat.into();
        active_chat.permission_mode = Set(Some(mode.as_str().to_owned()));
        active_chat.update(&transaction).await.map_err(store_err)?;
    }

    let plan_title = plan_request.title.clone();
    let plan = plan_request.plan.clone();
    let mut active_request: entities::plan_request::ActiveModel = plan_request.into();
    active_request.status = Set(decided_status.as_str().into());
    active_request.feedback = Set(request.decision.feedback.clone());
    active_request.resolved_at = Set(Some(decided_at));
    active_request
        .update(&transaction)
        .await
        .map_err(store_err)?;

    let mut active_call: entities::tool_call::ActiveModel = call.into();
    active_call.status = Set(ToolCallStatus::Completed.as_str().into());
    active_call.result = Set(Some(result));
    // Read back on history rehydration, so reopening the chat shows the same
    // recap the live transcript settled to.
    let preview = crate::ToolResultPreview::PlanDecision {
        title: plan_title,
        plan,
        accepted: matches!(request.decision.decision, PlanDecisionChoice::Accept),
        feedback: request.decision.feedback.clone(),
    };
    active_call.result_preview = Set(Some(serde_json::to_value(&preview)?));
    active_call.error_code = Set(None);
    active_call.error_detail = Set(None);
    active_call.resolved_at = Set(Some(decided_at));
    let resolved = active_call.update(&transaction).await.map_err(store_err)?;
    // The plan call resolves outside the agent loop, so nothing else ever
    // announces that it finished: the resumed worker reads the committed
    // result straight into the model transcript and never revisits the call.
    // Without this event the renderer showed the card waiting from
    // `PlanProposed` until the turn's terminal hydration finally settled it —
    // the decision looked lost exactly when it had committed.
    //
    // Journaled here, in the transaction that makes the row terminal, so the
    // event cannot disagree with the row it describes. An exact retry returns
    // above as `Existing` without reaching this, so the call announces itself
    // once. Chat-scoped like its `PlanProposed`: the turn is parked with no
    // lease, so no attempt owns this event.
    let completion_event = AgentEvent::ToolCallCompleted {
        call_id: request.call_id,
        output: crate::ToolOutput::text(resolved.result.clone().ok_or_else(|| {
            AgentError::Store(format!(
                "decided plan {} committed no result",
                request.call_id
            ))
        })?),
        action: crate::ToolActionPreview::build(&resolved.name, &resolved.arguments),
        result: Some(preview),
    };
    let seq = super::conversation::append_event_on(
        &transaction,
        ChatId(resolved.chat_id),
        None,
        None,
        None,
        None,
        &completion_event,
    )
    .await?;
    let transition =
        super::turn::advance_turn_after_client_resolution_on(&transaction, &resolved, decided_at)
            .await?
            .ok_or_else(|| {
                AgentError::Store(format!(
                    "decided plan {} is missing its client wait",
                    request.call_id
                ))
            })?;
    transaction.commit().await.map_err(store_err)?;
    Ok(DecidePlanOutcome::Decided {
        turn: transition.turn,
        completion_event: Box::new(SequencedEvent {
            seq,
            event: completion_event,
        }),
    })
}

/// Close presentation state when cancellation terminalizes the unclaimed
/// plan call through the shared client-wait state machine.
pub(in crate::db) async fn cancel_for_call_on<C>(
    conn: &C,
    call_id: CallId,
    cancelled_at: DateTime<Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let Some(request) = entities::plan_request::Entity::find_by_id(call_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(());
    };
    if request.status != PlanRequestStatus::Pending.as_str() {
        return Ok(());
    }
    let mut active: entities::plan_request::ActiveModel = request.into();
    active.status = Set(PlanRequestStatus::Cancelled.as_str().into());
    active.resolved_at = Set(Some(cancelled_at));
    active.update(conn).await.map_err(store_err)?;
    Ok(())
}

pub(in crate::db) async fn close_pending_for_terminal_turn_on<C>(
    conn: &C,
    turn_id: TurnId,
    terminal_at: DateTime<Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let requests = entities::plan_request::Entity::find()
        .filter(entities::plan_request::Column::TurnId.eq(turn_id.0))
        .filter(entities::plan_request::Column::Status.eq(PlanRequestStatus::Pending.as_str()))
        .order_by_asc(entities::plan_request::Column::CallId)
        .all(conn)
        .await
        .map_err(store_err)?;
    for request in requests {
        let resolved_at = terminal_at.max(request.proposed_at);
        let mut active: entities::plan_request::ActiveModel = request.into();
        active.status = Set(PlanRequestStatus::Cancelled.as_str().into());
        active.resolved_at = Set(Some(resolved_at));
        active.update(conn).await.map_err(store_err)?;
    }
    Ok(())
}

fn parse_arguments(value: &serde_json::Value) -> Result<ExitPlanModeArgs> {
    let arguments: ExitPlanModeArgs = serde_json::from_value(value.clone())?;
    if !arguments.is_well_formed() {
        return Err(AgentError::Store("invalid durable plan arguments".into()));
    }
    Ok(arguments)
}
