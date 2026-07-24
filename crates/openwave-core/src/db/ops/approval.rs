use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};

use crate::approval::{
    ApprovalDecision, ApprovalRequest, ToolApproval, ToolApprovalKind, ToolApprovalPreview,
    ToolApprovalStatus,
};
use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::{CallId, ChatId, TurnId};
use crate::model::{ToolCallExecution, ToolCallStatus, TurnRunStatus};
use crate::storage::{
    DecideToolApprovalOutcome, JournaledToolApprovalOutcome, RequestToolApprovalOutcome,
};
use crate::tool::ApprovalClass;

use super::super::{entities, store_err, DbStore};
use super::turn::canonical_db_timestamp;
use super::{acquire_chat_write_lock, acquire_tool_call_write_lock, acquire_turn_write_lock};

pub(in crate::db) async fn request_and_append_event(
    store: &DbStore,
    request: &ApprovalRequest,
    lease_token: uuid::Uuid,
    event_ordinal: i32,
    _requested_at: DateTime<Utc>,
) -> Result<JournaledToolApprovalOutcome> {
    validate_request(request)?;
    if lease_token.is_nil() || !(1..i32::MAX).contains(&event_ordinal) {
        return Err(AgentError::Store(
            "approval journal identity must be valid".into(),
        ));
    }
    let event = AgentEvent::ApprovalRequired {
        call_id: request.call_id,
        tool_name: request.tool_name.clone(),
        class: request.class,
        kind: request.kind,
        preview: request.preview.clone(),
        summary: request.summary.clone(),
    };
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, request.chat_id).await?
        || !acquire_turn_write_lock(&transaction, request.turn_id).await?
        || !acquire_tool_call_write_lock(&transaction, request.call_id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(JournaledToolApprovalOutcome {
            outcome: RequestToolApprovalOutcome::Unavailable,
            required_event: None,
        });
    }
    // Lease and request ordering use the database's statement-time clock so
    // host clock skew cannot reject a valid claimed operation.
    let database_now = super::agent_run::database_now(&transaction).await?;
    let call = entities::tool_call::Entity::find_by_id(request.call_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked tool call exists");
    let turn = entities::turn_run::Entity::find_by_id(request.turn_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked turn exists");
    // Immutable receipt recovery precedes every mutable liveness check. A
    // caller that lost the commit acknowledgement can recover the exact event
    // even after cancellation, terminalization, or lease expiry won.
    if call.chat_id == request.chat_id.0
        && call.turn_id == request.turn_id.0
        && call.name == request.tool_name
        && call.execution == ToolCallExecution::Server.as_str()
        && call.approval_status.is_some()
    {
        let approval = approval_from_model(&call)?;
        if approval.class != request.class || approval.kind != request.kind {
            transaction.commit().await.map_err(store_err)?;
            return Ok(JournaledToolApprovalOutcome {
                outcome: RequestToolApprovalOutcome::IdentityConflict,
                required_event: None,
            });
        }
        let required_event = match call.approval_event_seq {
            Some(seq) => {
                exact_required_event(
                    &transaction,
                    request,
                    lease_token,
                    event_ordinal,
                    seq,
                    &event,
                )
                .await?
            }
            None => None,
        };
        transaction.commit().await.map_err(store_err)?;
        return Ok(JournaledToolApprovalOutcome {
            outcome: RequestToolApprovalOutcome::Existing(approval),
            required_event,
        });
    }
    let claim = entities::turn_claim::Entity::find_by_id(lease_token)
        .one(&transaction)
        .await
        .map_err(store_err)?;
    let claim_is_live = claim.as_ref().is_some_and(|claim| {
        claim.turn_id == request.turn_id.0
            && turn.attempt_count == claim.attempt_count
            && turn.claim_count == claim.claim_count
            && turn.lease_token == Some(lease_token)
            && turn
                .lease_expires_at
                .is_some_and(|expiry| expiry > database_now)
    });
    if call.chat_id != request.chat_id.0
        || call.turn_id != request.turn_id.0
        || call.name != request.tool_name
        || call.execution != ToolCallExecution::Server.as_str()
        || call.status != ToolCallStatus::Pending.as_str()
        || turn.chat_id != request.chat_id.0
        || turn.status != TurnRunStatus::Running.as_str()
        || !claim_is_live
        || request.kind != ToolApprovalKind::for_tool_name(&request.tool_name)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(JournaledToolApprovalOutcome {
            outcome: RequestToolApprovalOutcome::IdentityConflict,
            required_event: None,
        });
    }
    let requested_at = database_now.max(turn.updated_at).max(call.created_at);
    let seq = super::conversation::append_event_on(
        &transaction,
        request.chat_id,
        Some(request.turn_id),
        Some(lease_token),
        Some(event_ordinal),
        None,
        &event,
    )
    .await?;
    let mut active: entities::tool_call::ActiveModel = call.into();
    active.approval_status = Set(Some(ToolApprovalStatus::Pending.as_str().into()));
    active.approval_class = Set(Some(request.class.as_str().into()));
    active.approval_kind = Set(Some(request.kind.as_str().into()));
    active.approval_requested_at = Set(Some(requested_at));
    active.approval_event_seq = Set(Some(seq));
    let approval = approval_from_model(&active.update(&transaction).await.map_err(store_err)?)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(JournaledToolApprovalOutcome {
        outcome: RequestToolApprovalOutcome::Requested(approval),
        required_event: Some(SequencedEvent { seq, event }),
    })
}

async fn exact_required_event<C>(
    conn: &C,
    request: &ApprovalRequest,
    lease_token: uuid::Uuid,
    event_ordinal: i32,
    seq: i64,
    expected: &AgentEvent,
) -> Result<Option<SequencedEvent>>
where
    C: sea_orm::ConnectionTrait,
{
    let stored = entities::event::Entity::find_by_id((request.chat_id.0, seq))
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("approval event receipt is missing".into()))?;
    let payload = serde_json::from_value::<AgentEvent>(stored.payload)?;
    if stored.turn_id != Some(request.turn_id.0) || stored.terminal || payload != *expected {
        return Err(AgentError::Store(
            "approval event receipt does not match its request".into(),
        ));
    }
    Ok((stored.lease_token == Some(lease_token)
        && stored.attempt_event_ordinal == Some(event_ordinal))
    .then_some(SequencedEvent {
        seq,
        event: payload,
    }))
}

/// Close every unresolved server call when its turn becomes terminal. Approval
/// and tool state move together so no waiter or recovery card can survive the
/// terminal transition.
pub(in crate::db::ops) async fn close_pending_for_terminal_turn_on<C>(
    conn: &C,
    turn_id: TurnId,
    now: DateTime<Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let calls = entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::TurnId.eq(turn_id.0))
        .filter(entities::tool_call::Column::Execution.eq(ToolCallExecution::Server.as_str()))
        .filter(entities::tool_call::Column::Status.eq(ToolCallStatus::Pending.as_str()))
        .order_by_asc(entities::tool_call::Column::Id)
        .all(conn)
        .await
        .map_err(store_err)?;
    for call in calls {
        if !acquire_tool_call_write_lock(conn, CallId(call.id)).await? {
            return Err(AgentError::Store(format!(
                "pending tool call {} disappeared during turn terminalization",
                CallId(call.id)
            )));
        }
        let resolved_at = now.max(call.created_at);
        let mut active: entities::tool_call::ActiveModel = call.clone().into();
        active.status = Set(ToolCallStatus::Cancelled.as_str().into());
        active.result = Set(Some("turn ended before tool completion".into()));
        active.resolved_at = Set(Some(resolved_at));
        if call.approval_status.as_deref() == Some(ToolApprovalStatus::Pending.as_str()) {
            let requested_at = call.approval_requested_at.ok_or_else(|| {
                AgentError::Store("pending approval is missing requested_at".into())
            })?;
            active.approval_status = Set(Some(ToolApprovalStatus::Rejected.as_str().into()));
            active.approval_reason = Set(Some("turn ended before approval".into()));
            active.approval_decided_at = Set(Some(resolved_at.max(requested_at)));
        }
        active.update(conn).await.map_err(store_err)?;
    }
    Ok(())
}

/// Revoke unresolved consent as soon as cancellation is accepted while
/// leaving the live tool call for the worker's ordinary quiescence path.
pub(in crate::db::ops) async fn reject_pending_for_cancelling_turn_on<C>(
    conn: &C,
    turn_id: TurnId,
    now: DateTime<Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let calls = entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::TurnId.eq(turn_id.0))
        .filter(
            entities::tool_call::Column::ApprovalStatus.eq(ToolApprovalStatus::Pending.as_str()),
        )
        .order_by_asc(entities::tool_call::Column::Id)
        .all(conn)
        .await
        .map_err(store_err)?;
    for call in calls {
        if !acquire_tool_call_write_lock(conn, CallId(call.id)).await? {
            return Err(AgentError::Store(format!(
                "pending approval {} disappeared during cancellation",
                CallId(call.id)
            )));
        }
        let requested_at = call
            .approval_requested_at
            .ok_or_else(|| AgentError::Store("pending approval is missing requested_at".into()))?;
        let mut active: entities::tool_call::ActiveModel = call.into();
        active.approval_status = Set(Some(ToolApprovalStatus::Rejected.as_str().into()));
        active.approval_reason = Set(Some("turn cancellation revoked approval".into()));
        active.approval_decided_at = Set(Some(now.max(requested_at)));
        active.update(conn).await.map_err(store_err)?;
    }
    Ok(())
}

pub(in crate::db) async fn request(
    store: &DbStore,
    request: &ApprovalRequest,
    requested_at: DateTime<Utc>,
) -> Result<RequestToolApprovalOutcome> {
    validate_request(request)?;
    let requested_at = canonical_db_timestamp(requested_at)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, request.chat_id).await?
        || !acquire_tool_call_write_lock(&transaction, request.call_id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(RequestToolApprovalOutcome::Unavailable);
    }
    let existing = entities::tool_call::Entity::find_by_id(request.call_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked tool call exists");
    if existing.chat_id != request.chat_id.0
        || existing.turn_id != request.turn_id.0
        || existing.name != request.tool_name
        || existing.execution != ToolCallExecution::Server.as_str()
        || existing.status != ToolCallStatus::Pending.as_str()
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(RequestToolApprovalOutcome::IdentityConflict);
    }
    if existing.approval_status.is_some() {
        let approval = approval_from_model(&existing)?;
        transaction.commit().await.map_err(store_err)?;
        return if approval.class == request.class && approval.kind == request.kind {
            Ok(RequestToolApprovalOutcome::Existing(approval))
        } else {
            Ok(RequestToolApprovalOutcome::IdentityConflict)
        };
    }
    if request.kind != ToolApprovalKind::for_tool_name(&request.tool_name) {
        transaction.commit().await.map_err(store_err)?;
        return Ok(RequestToolApprovalOutcome::IdentityConflict);
    }
    let mut active: entities::tool_call::ActiveModel = existing.into();
    active.approval_status = Set(Some(ToolApprovalStatus::Pending.as_str().into()));
    active.approval_class = Set(Some(request.class.as_str().into()));
    active.approval_kind = Set(Some(request.kind.as_str().into()));
    active.approval_requested_at = Set(Some(requested_at));
    let inserted = active.update(&transaction).await.map_err(store_err)?;
    let approval = approval_from_model(&inserted)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(RequestToolApprovalOutcome::Requested(approval))
}

pub(in crate::db) async fn decide(
    store: &DbStore,
    chat_id: ChatId,
    call_id: CallId,
    decision: &ApprovalDecision,
    _decided_at: DateTime<Utc>,
) -> Result<DecideToolApprovalOutcome> {
    validate_decision(decision)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await?
        || !acquire_tool_call_write_lock(&transaction, call_id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(DecideToolApprovalOutcome::Unavailable);
    }
    let existing = entities::tool_call::Entity::find_by_id(call_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked tool call exists");
    if existing.chat_id != chat_id.0 || existing.approval_status.is_none() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(DecideToolApprovalOutcome::Unavailable);
    }
    let current = approval_from_model(&existing)?;
    if current.status != ToolApprovalStatus::Pending {
        transaction.commit().await.map_err(store_err)?;
        return if current.status == decision.status()
            && current.reason.as_deref() == decision.reason()
        {
            Ok(DecideToolApprovalOutcome::Existing(current))
        } else {
            Ok(DecideToolApprovalOutcome::DecisionConflict)
        };
    }
    if existing.status != ToolCallStatus::Pending.as_str()
        || existing.execution != ToolCallExecution::Server.as_str()
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(DecideToolApprovalOutcome::Unavailable);
    }
    // The wall clock can move backwards between request and decision. Preserve
    // the immutable ordering invariant rather than stranding an otherwise
    // valid decision on the table check.
    let decided_at = super::agent_run::database_now(&transaction)
        .await?
        .max(current.requested_at);
    let mut active: entities::tool_call::ActiveModel = existing.into();
    active.approval_status = Set(Some(decision.status().as_str().into()));
    active.approval_reason = Set(decision.reason().map(str::to_owned));
    active.approval_decided_at = Set(Some(decided_at));
    let decided = active.update(&transaction).await.map_err(store_err)?;
    let approval = approval_from_model(&decided)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(DecideToolApprovalOutcome::Decided(approval))
}

pub(in crate::db) async fn get(store: &DbStore, call_id: CallId) -> Result<Option<ToolApproval>> {
    entities::tool_call::Entity::find_by_id(call_id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .filter(|row| row.approval_status.is_some())
        .as_ref()
        .map(approval_from_model)
        .transpose()
}

pub(in crate::db) async fn list_pending(
    store: &DbStore,
    chat_id: ChatId,
    limit: u64,
) -> Result<Vec<ToolApproval>> {
    if limit == 0 || limit > 100 {
        return Err(AgentError::Store(
            "pending approval limit must be between 1 and 100".into(),
        ));
    }
    entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::ChatId.eq(chat_id.0))
        .filter(
            entities::tool_call::Column::ApprovalStatus.eq(ToolApprovalStatus::Pending.as_str()),
        )
        .order_by_asc(entities::tool_call::Column::ApprovalRequestedAt)
        .order_by_asc(entities::tool_call::Column::Id)
        .limit(limit)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .iter()
        .map(approval_from_model)
        .collect()
}

fn validate_request(request: &ApprovalRequest) -> Result<()> {
    if request.call_id.0.is_nil()
        || request.chat_id.0.is_nil()
        || request.turn_id.0.is_nil()
        || request.tool_name.is_empty()
        || request.tool_name.len() > crate::model::ToolCallRecord::MAX_LABEL_LEN
        || request.class != ApprovalClass::Sensitive
    {
        return Err(AgentError::Store("invalid tool approval request".into()));
    }
    Ok(())
}

fn validate_decision(decision: &ApprovalDecision) -> Result<()> {
    if decision
        .reason()
        .is_some_and(|reason| !ToolApproval::valid_reason(reason))
    {
        return Err(AgentError::Store("invalid tool approval decision".into()));
    }
    Ok(())
}

fn approval_from_model(model: &entities::tool_call::Model) -> Result<ToolApproval> {
    let status = match model.approval_status.as_deref() {
        Some("pending") => ToolApprovalStatus::Pending,
        Some("approved") => ToolApprovalStatus::Approved,
        Some("rejected") => ToolApprovalStatus::Rejected,
        _ => {
            return Err(AgentError::Store(
                "invalid durable tool approval status".into(),
            ))
        }
    };
    let class = match model.approval_class.as_deref() {
        Some("sensitive") => ApprovalClass::Sensitive,
        _ => {
            return Err(AgentError::Store(
                "invalid durable tool approval class".into(),
            ))
        }
    };
    let kind = match model.approval_kind.as_deref() {
        Some("search_may_share_query_and_excerpts") if model.name.starts_with("mcp__") => {
            ToolApprovalKind::ExternalMcpMayCallServer
        }
        Some("search_may_share_query_and_excerpts") if model.name == "web_search" => {
            ToolApprovalKind::WebSearchMayShareQuery
        }
        Some("search_may_share_query_and_excerpts") => {
            ToolApprovalKind::SearchMayShareQueryAndExcerpts
        }
        Some("web_search_may_share_query") if model.name == "web_search" => {
            ToolApprovalKind::WebSearchMayShareQuery
        }
        Some("exec_may_run_networked_command") => ToolApprovalKind::ExecMayRunNetworkedCommand,
        Some("unsupported") => ToolApprovalKind::Unsupported,
        _ => {
            return Err(AgentError::Store(
                "invalid durable tool approval kind".into(),
            ))
        }
    };
    Ok(ToolApproval {
        call_id: CallId(model.id),
        chat_id: ChatId(model.chat_id),
        turn_id: TurnId(model.turn_id),
        tool_name: model.name.clone(),
        class,
        kind,
        // Rebuilt from the arguments the call is durably parked on rather than
        // stored separately, so a recovered card can never describe a different
        // action from the one that will run.
        preview: ToolApprovalPreview::build(&model.name, &model.arguments),
        status,
        reason: model.approval_reason.clone(),
        requested_at: model
            .approval_requested_at
            .ok_or_else(|| AgentError::Store("approval is missing requested_at".into()))?,
        decided_at: model.approval_decided_at,
    })
}
