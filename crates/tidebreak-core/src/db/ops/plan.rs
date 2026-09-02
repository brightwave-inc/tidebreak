//! The plan proposal as an approval row (decision 0048 step 5).
//!
//! `exit_plan_mode` parks its turn on a `code_approval` row whose kind is
//! [`CodeApprovalKind::Plan`], whose id is the call id, and whose raw
//! payload is the plan body. The reader's decision settles the row as
//! [`ApprovalDecisionKind::PlanDecided`] and completes the call, so the chat
//! plan route and the session decision route land on the same row.

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};

use crate::code::{
    ApprovalDecisionKind, CodeApproval, CodeApprovalId, CodeApprovalKind, CodeApprovalState,
    CodeEvent, CodeSessionId, CodeTurnId,
};
use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::model::{OwnerId, ToolCallExecution, ToolCallStatus, TurnRunStatus};
use crate::storage::DecidePlanOutcome;
use crate::{
    plan_decision_result, CallId, ChatId, DecidePlanRequest, ExitPlanModeArgs, PendingPlanApproval,
    PlanDecisionChoice, PlanProposalBody, TurnId, DEFAULT_ACCEPTED_PLAN_MODE, EXIT_PLAN_MODE_TOOL,
};

use super::super::{entities, store_err, DbStore};
use super::approval::{claim_of, session_row, settle_row_on};
use super::code::approval::{find_approval_row_on, insert_approval_on};
use super::turn::{canonical_db_timestamp, recover_turn_after_client_resolution_on};
use super::{acquire_chat_write_lock, acquire_tool_call_write_lock, acquire_turn_write_lock};

/// How far back a park's request row is looked for on a checkpoint retry.
/// The request is the last thing the lane journals before it parks, so it
/// is within a few rows of the tail.
const PARK_RECEIPT_SCAN: u64 = 1_024;

/// Commit the park's approval row and its journal row inside the caller's
/// already-fenced client-wait transaction.
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
    let session = session_row(conn, call.chat_id).await?;
    let owner = OwnerId::new(&session.owner)?;
    insert_approval_on(
        conn,
        &owner,
        &CodeApproval {
            id: CodeApprovalId(call.id.0),
            session_id: CodeSessionId(call.chat_id.0),
            turn_id: CodeTurnId(call.turn_id.0),
            kind: CodeApprovalKind::Plan {
                proposed_mode: DEFAULT_ACCEPTED_PLAN_MODE,
            },
            harness_raw: PlanProposalBody {
                title: arguments.title,
                plan: arguments.plan,
            }
            .to_raw()?,
            native_call_id: Some(call.id.to_string()),
            server_capability: None,
            request_sha256: None,
            worker_epoch: Some(session.spawn_epoch),
            decision_claim: None,
            claimed_at: None,
            state: CodeApprovalState::Pending,
            feedback: None,
            requested_at: proposed_at,
            decided_at: None,
            auto_judge_status: None,
        },
    )
    .await?;
    Ok(Some(SequencedEvent { seq, event }))
}

/// Recover and validate the exact committed park for an ambiguous checkpoint
/// retry.
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
    let row = find_approval_row_on(conn, CodeApprovalId(call.id.0))
        .await?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "plan checkpoint {} is missing its approval row",
                call.id
            ))
        })?;
    if row.session_id != call.chat_id.0 || row.turn_id != call.turn_id.0 {
        return Err(AgentError::Store(format!(
            "plan checkpoint {} has mismatched scope",
            call.id
        )));
    }
    let body = plan_of(&row)?;
    if body.title != expected.title || body.plan != expected.plan {
        return Err(AgentError::Store(format!(
            "plan checkpoint {} has mismatched presentation data",
            call.id
        )));
    }
    if row.state != CodeApprovalState::Pending.as_str() {
        return Ok(None);
    }
    let expected_event = AgentEvent::PlanProposed {
        call_id: call.id,
        turn_id: call.turn_id,
    };
    park_request_receipt_on(conn, call.chat_id, &expected_event).await
}

/// The journal row that announced a park, found from the tail of the
/// session's journal. Errors when the row is not within reach: a pending park
/// whose request is missing is a broken receipt, not an absent one.
pub(in crate::db::ops) async fn park_request_receipt_on<C>(
    conn: &C,
    chat_id: ChatId,
    expected: &AgentEvent,
) -> Result<Option<SequencedEvent>>
where
    C: ConnectionTrait,
{
    let expected_row = serde_json::to_value(crate::chat_journal::journal_row(expected))?;
    let rows = entities::code_event::Entity::find()
        .filter(entities::code_event::Column::SessionId.eq(chat_id.0))
        .order_by_desc(entities::code_event::Column::Seq)
        .limit(PARK_RECEIPT_SCAN)
        .all(conn)
        .await
        .map_err(store_err)?;
    for stored in rows {
        if stored.event != expected_row {
            continue;
        }
        if stored.turn_id.is_some() || stored.terminal {
            return Err(AgentError::Store(
                "park request event does not match its checkpoint".into(),
            ));
        }
        let event = serde_json::from_value::<CodeEvent>(stored.event)?;
        return Ok(
            crate::chat_journal::chat_event(event)?.map(|event| SequencedEvent {
                seq: stored.seq,
                event,
            }),
        );
    }
    Err(AgentError::Store(
        "park request event is missing from the journal".into(),
    ))
}

/// The pending parks on one chat, oldest first, inside the caller's
/// transaction. Consent cards are excluded by kind.
pub(in crate::db::ops) async fn pending_park_rows_on<C>(
    conn: &C,
    chat_id: ChatId,
) -> Result<Vec<entities::code_approval::Model>>
where
    C: ConnectionTrait,
{
    entities::code_approval::Entity::find()
        .filter(entities::code_approval::Column::SessionId.eq(chat_id.0))
        .filter(entities::code_approval::Column::State.eq(CodeApprovalState::Pending.as_str()))
        .order_by_asc(entities::code_approval::Column::RequestedAt)
        .order_by_asc(entities::code_approval::Column::Id)
        .all(conn)
        .await
        .map_err(store_err)
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
    let rows = pending_park_rows_on(&transaction, chat_id).await?;
    let mut pending = Vec::with_capacity(rows.len());
    for row in rows {
        let Ok(body) = plan_of(&row) else {
            continue;
        };
        let call = entities::tool_call::Entity::find_by_id(row.id)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store("pending plan call is missing".into()))?;
        let wait = entities::turn_client_wait::Entity::find_by_id(row.id)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store("pending plan wait is missing".into()))?;
        let turn = entities::turn_run::Entity::find_by_id(row.turn_id)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store("pending plan turn is missing".into()))?;
        if call.chat_id != row.session_id
            || call.turn_id != row.turn_id
            || call.name != EXIT_PLAN_MODE_TOOL
            || call.execution != ToolCallExecution::Orchestration.as_str()
            || call.status != ToolCallStatus::Pending.as_str()
            || call.client_executor_id.is_some()
            || wait.chat_id != row.session_id
            || wait.turn_id != row.turn_id
            || wait.status != crate::TurnClientWaitStatus::Waiting.as_str()
            || turn.chat_id != row.session_id
            || turn.status != TurnRunStatus::WaitingForClient.as_str()
            || turn.attempt_count != wait.attempt_count
            || turn.claim_count != wait.claim_count
        {
            return Err(AgentError::Store(
                "pending plan projection does not match its live continuation".into(),
            ));
        }
        pending.push(PendingPlanApproval {
            call_id: CallId(row.id),
            turn_id: TurnId(row.turn_id),
            title: body.title,
            plan: body.plan,
            proposed_at: row.requested_at,
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
    let Some(scope) = find_approval_row_on(&store.conn, CodeApprovalId(request.call_id.0)).await?
    else {
        return Ok(DecidePlanOutcome::Unavailable);
    };
    if scope.session_id != request.chat_id.0 {
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
    let row = find_approval_row_on(&transaction, CodeApprovalId(request.call_id.0))
        .await?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "plan call {} is missing its approval row",
                request.call_id
            ))
        })?;
    if row.session_id != request.chat_id.0 || row.turn_id != call.turn_id {
        transaction.commit().await.map_err(store_err)?;
        return Ok(DecidePlanOutcome::Unavailable);
    }
    let Ok(body) = plan_of(&row) else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(DecidePlanOutcome::Unavailable);
    };
    let accepted = matches!(request.decision.decision, PlanDecisionChoice::Accept);
    let decided_state = if accepted {
        CodeApprovalState::Approved
    } else {
        CodeApprovalState::Denied
    };
    let result = serde_json::to_string(&plan_decision_result(&request.decision))?;

    if row.state != CodeApprovalState::Pending.as_str() {
        if row.state != decided_state.as_str() {
            transaction.commit().await.map_err(store_err)?;
            return Ok(DecidePlanOutcome::DecisionConflict);
        }
        let exact = row.feedback == request.decision.feedback
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
        || row.turn_id != turn.id
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(DecidePlanOutcome::Unavailable);
    }
    let database_now = super::agent_run::database_now(&transaction).await?;
    let decided_at = requested_at
        .max(database_now)
        .max(row.requested_at)
        .max(call.created_at)
        .max(turn.updated_at);

    // Accepting is the one place a permission mode changes as a side effect:
    // the chat leaves plan mode inside the same transaction that completes
    // the call, so the resumed turn can never read the plan surface again
    // while believing the plan was accepted.
    if let Some(mode) = request.decision.mode_after() {
        let chat = entities::code_session::Entity::find_by_id(request.chat_id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store("plan chat disappeared".into()))?;
        let mut active_chat: entities::code_session::ActiveModel = chat.into();
        active_chat.permission_mode = Set(Some(mode.as_str().to_owned()));
        active_chat.update(&transaction).await.map_err(store_err)?;
    }

    let settlement = settle_row_on(
        &transaction,
        &row,
        claim_of(&row),
        ApprovalDecisionKind::PlanDecided {
            approve: accepted,
            feedback: request.decision.feedback.clone(),
        },
        decided_at,
    )
    .await?
    .ok_or_else(|| AgentError::Store(format!("plan {} could not be settled", request.call_id)))?;

    let mut active_call: entities::tool_call::ActiveModel = call.into();
    active_call.status = Set(ToolCallStatus::Completed.as_str().into());
    active_call.result = Set(Some(result));
    // Read back on history rehydration, so reopening the chat shows the same
    // recap the live transcript settled to.
    let preview = crate::ToolResultPreview::PlanDecision {
        title: body.title,
        plan: body.plan,
        accepted,
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
    // Journaled here, in the transaction that makes the row terminal, so the
    // event cannot disagree with the row it describes. Chat-scoped like its
    // request: the turn is parked with no lease, so no attempt owns it.
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
        resolution: Box::new(settlement.event),
    })
}

fn parse_arguments(value: &serde_json::Value) -> Result<ExitPlanModeArgs> {
    let arguments: ExitPlanModeArgs = serde_json::from_value(value.clone())?;
    if !arguments.is_well_formed() {
        return Err(AgentError::Store("invalid durable plan arguments".into()));
    }
    Ok(arguments)
}

/// The plan a row carries, or an error for a row of another kind.
fn plan_of(row: &entities::code_approval::Model) -> Result<PlanProposalBody> {
    match serde_json::from_value::<CodeApprovalKind>(row.kind.clone())? {
        CodeApprovalKind::Plan { .. } => PlanProposalBody::from_raw(&row.harness_raw)
            .ok_or_else(|| AgentError::Store(format!("plan {} has no body", row.id))),
        _ => Err(AgentError::Store(format!(
            "approval {} is not a plan",
            row.id
        ))),
    }
}
