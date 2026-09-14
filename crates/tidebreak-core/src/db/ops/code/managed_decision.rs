//! Persist human decisions independently of ordinary native execution leases.

use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter, Set,
    TransactionTrait,
};
use sha2::{Digest, Sha256};

use super::super::super::{entities, store_err, DbStore};
use super::super::agent_run::database_now;
use super::approval::{
    approval_from_row, insert_approval_on, settle_approval_on_locked, ApprovalClaim,
    ApprovalSettlement,
};
use super::{acquire_code_session_write_lock, append_event_on_locked};
use crate::code::supervisor_tools::{
    human_decision_kind, validate_human_decision, validate_request, SupervisorToolResult,
};
use crate::code::{
    Approval, ApprovalDecisionKind, ApprovalId, ApprovalState, CodeIncarnationId, Event,
    InternalApprovalRequest, SequencedEvent, SessionId, SupervisorToolRequest, SupervisorToolTurn,
    TurnActor, TurnId,
};
use crate::{AgentError, OwnerId, Result};
use entities::code_managed_decision as decision;

fn invalid(message: &str) -> AgentError {
    AgentError::InvalidTarget(message.into())
}

struct LiveTurn {
    grant: uuid::Uuid,
    turn: uuid::Uuid,
    epoch: i64,
}

/// The session lock serializes answers with stop, replacement, and revocation.
async fn live_turn<C: ConnectionTrait>(
    conn: &C,
    owner: &OwnerId,
    session: SessionId,
    incarnation: CodeIncarnationId,
    identity: &SupervisorToolTurn,
) -> Result<LiveTurn> {
    if identity.native_turn == 0 || identity.runtime_id.is_nil() {
        return Err(invalid("human decision has no supervisor turn identity"));
    }
    let grant = super::native_tool_receipt::scope(conn, owner, session, incarnation).await?;
    let session_row = entities::session::Entity::find_by_id(session.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| invalid("human decision session is absent"))?;
    if session_row.lifecycle != "running"
        || session_row.execution_location != "sandbox"
        || session_row.permission_mode.as_deref() != Some("allow")
    {
        return Err(invalid(
            "human decisions require a running managed session in Allow mode",
        ));
    }
    let incarnation_row = entities::code_session_incarnation::Entity::find_by_id(incarnation.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| invalid("human decision incarnation is absent"))?;
    let ordinal = i64::from(incarnation_row.starting_turn) + i64::from(identity.native_turn) - 1;
    let turn = entities::turn::Entity::find()
        .filter(entities::turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::turn::Column::SessionId.eq(session.0))
        .filter(entities::turn::Column::Ordinal.eq(ordinal))
        .filter(entities::turn::Column::Status.eq("running"))
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| invalid("the turn that requested this decision is no longer running"))?;
    let runtime = entities::setting::Entity::find_by_id(format!(
        "code.incarnations.{incarnation}.steering_protocol"
    ))
    .one(conn)
    .await
    .map_err(store_err)?;
    if !runtime.is_some_and(|row| {
        row.value_json
            .get("runtime_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            == Some(identity.runtime_id)
            && row
                .value_json
                .get("sandbox_id")
                .and_then(serde_json::Value::as_str)
                == incarnation_row.sandbox_id.as_deref()
    }) {
        return Err(invalid(
            "the supervisor that requested this decision is no longer attached",
        ));
    }
    Ok(LiveTurn {
        grant,
        turn: turn.id,
        epoch: session_row.spawn_epoch,
    })
}

fn identity(row: &decision::Model) -> Result<SupervisorToolTurn> {
    Ok(SupervisorToolTurn {
        native_turn: u32::try_from(row.native_turn)
            .map_err(|_| invalid("invalid managed decision turn"))?,
        runtime_id: row.runtime_id,
    })
}

async fn validate_live<C: ConnectionTrait>(
    conn: &C,
    owner: &OwnerId,
    row: &decision::Model,
) -> Result<LiveTurn> {
    let live = live_turn(
        conn,
        owner,
        SessionId(row.session_id),
        CodeIncarnationId(row.incarnation_id),
        &identity(row)?,
    )
    .await?;
    if live.turn != row.turn_id || live.grant != row.grant_id || row.abandoned {
        return Err(invalid(
            "this human decision no longer belongs to the active turn",
        ));
    }
    Ok(live)
}

/// Commit one question or plan, its approval, and its journal event together.
pub async fn enqueue_managed_decision(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    incarnation: CodeIncarnationId,
    request: &SupervisorToolRequest,
) -> Result<(ApprovalId, Option<SequencedEvent>)> {
    validate_request(request).map_err(AgentError::InvalidTarget)?;
    if request.cancelled {
        return Err(invalid(
            "cancelled proposals cannot create a human decision",
        ));
    }
    let kind = human_decision_kind(&request.tool, &request.arguments)
        .map_err(AgentError::InvalidTarget)?;
    let identity = request
        .turn
        .as_ref()
        .ok_or_else(|| invalid("human decision has no supervisor turn identity"))?;
    let tx = store.conn.begin().await.map_err(store_err)?;
    let live = live_turn(&tx, owner, session, incarnation, identity).await?;
    let prior = decision::Entity::find()
        .filter(decision::Column::Owner.eq(owner.as_str()))
        .filter(decision::Column::SessionId.eq(session.0))
        .filter(decision::Column::IncarnationId.eq(incarnation.0))
        .filter(decision::Column::RequestId.eq(&request.request_id))
        .one(&tx)
        .await
        .map_err(store_err)?;
    if let Some(prior) = prior {
        if prior.tool != request.tool
            || prior.arguments != request.arguments
            || prior.runtime_id != identity.runtime_id
            || prior.native_turn != i64::from(identity.native_turn)
            || prior.turn_id != live.turn
            || prior.grant_id != live.grant
            || prior.abandoned
        {
            return Err(invalid(
                "human decision replay changed its turn, payload, or grant",
            ));
        }
        decision::Entity::update_many()
            .col_expr(
                decision::Column::Delivered,
                sea_orm::sea_query::Expr::value(false),
            )
            .filter(decision::Column::ApprovalId.eq(prior.approval_id))
            .exec(&tx)
            .await
            .map_err(store_err)?;
        tx.commit().await.map_err(store_err)?;
        return Ok((ApprovalId(prior.approval_id), None));
    }
    let ordinary = entities::code_native_tool_receipt::Entity::find()
        .filter(entities::code_native_tool_receipt::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_native_tool_receipt::Column::SessionId.eq(session.0))
        .filter(entities::code_native_tool_receipt::Column::IncarnationId.eq(incarnation.0))
        .filter(entities::code_native_tool_receipt::Column::RequestId.eq(&request.request_id))
        .count(&tx)
        .await
        .map_err(store_err)?;
    if ordinary != 0 {
        return Err(invalid(
            "request_id already belongs to an ordinary native call",
        ));
    }
    let pending = decision::Entity::find()
        .filter(decision::Column::Owner.eq(owner.as_str()))
        .filter(decision::Column::SessionId.eq(session.0))
        .filter(decision::Column::IncarnationId.eq(incarnation.0))
        .filter(decision::Column::Abandoned.eq(false))
        .filter(decision::Column::Delivered.eq(false))
        .count(&tx)
        .await
        .map_err(store_err)?;
    if pending >= 8 {
        return Err(invalid(super::NATIVE_TOOL_QUEUE_FULL));
    }
    let approval = Approval {
        id: ApprovalId::new(),
        session_id: session,
        turn_id: TurnId(live.turn),
        kind,
        harness_raw: request.arguments.clone(),
        native_call_id: Some(request.request_id.clone()),
        server_capability: None,
        request_sha256: Some(
            Sha256::digest(serde_json::to_vec(request)?)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        ),
        worker_epoch: Some(live.epoch),
        decision_claim: None,
        claimed_at: None,
        state: ApprovalState::Pending,
        feedback: None,
        requested_at: database_now(&tx).await?,
        decided_at: None,
        auto_judge_status: None,
        actor: None,
    };
    insert_approval_on(&tx, owner, &approval).await?;
    decision::ActiveModel {
        approval_id: Set(approval.id.0),
        owner: Set(owner.to_string()),
        session_id: Set(session.0),
        turn_id: Set(live.turn),
        incarnation_id: Set(incarnation.0),
        grant_id: Set(live.grant),
        runtime_id: Set(identity.runtime_id),
        native_turn: Set(i64::from(identity.native_turn)),
        request_id: Set(request.request_id.clone()),
        tool: Set(request.tool.clone()),
        arguments: Set(request.arguments.clone()),
        result: Set(None),
        abandoned: Set(false),
        delivered: Set(false),
    }
    .insert(&tx)
    .await
    .map_err(store_err)?;
    let event = Event::ApprovalRequested {
        approval_id: approval.id,
        request: Some(InternalApprovalRequest::from_kind(
            &approval.kind,
            approval.turn_id,
            false,
        )),
    };
    let seq = append_event_on_locked(&tx, owner, session, &event).await?;
    tx.commit().await.map_err(store_err)?;
    Ok((approval.id, Some(SequencedEvent { seq, event })))
}

/// Record exact-request cancellation, including a tombstone before admission.
pub async fn cancel_managed_decision(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    incarnation: CodeIncarnationId,
    request: &SupervisorToolRequest,
) -> Result<Option<SequencedEvent>> {
    validate_request(request).map_err(AgentError::InvalidTarget)?;
    if !request.cancelled {
        return Err(invalid(
            "human cancellation requires its cancellation marker",
        ));
    }
    human_decision_kind(&request.tool, &request.arguments).map_err(AgentError::InvalidTarget)?;
    let identity = request
        .turn
        .as_ref()
        .ok_or_else(|| invalid("human cancellation has no turn"))?;
    let tx = store.conn.begin().await.map_err(store_err)?;
    let live = live_turn(&tx, owner, session, incarnation, identity).await?;
    let prior = decision::Entity::find()
        .filter(decision::Column::Owner.eq(owner.as_str()))
        .filter(decision::Column::SessionId.eq(session.0))
        .filter(decision::Column::IncarnationId.eq(incarnation.0))
        .filter(decision::Column::RequestId.eq(&request.request_id))
        .one(&tx)
        .await
        .map_err(store_err)?;
    let mut event = None;
    if let Some(prior) = prior {
        if prior.tool != request.tool
            || prior.arguments != request.arguments
            || prior.runtime_id != identity.runtime_id
            || prior.native_turn != i64::from(identity.native_turn)
            || prior.turn_id != live.turn
            || prior.grant_id != live.grant
        {
            return Err(invalid("human cancellation changed its original request"));
        }
        if !prior.abandoned {
            // A settled actor and answer remain immutable, but delivery is cancelled.
            if prior.result.is_none() {
                if let Some(settlement) = settle_approval_on_locked(
                    &tx,
                    owner,
                    ApprovalId(prior.approval_id),
                    session,
                    live.epoch,
                    ApprovalClaim::Unclaimed,
                    ApprovalDecisionKind::Abandoned,
                    database_now(&tx).await?,
                    None,
                )
                .await?
                {
                    event = Some(settlement.event);
                }
            }
            decision::Entity::update_many()
                .col_expr(
                    decision::Column::Abandoned,
                    sea_orm::sea_query::Expr::value(true),
                )
                .filter(decision::Column::ApprovalId.eq(prior.approval_id))
                .exec(&tx)
                .await
                .map_err(store_err)?;
        }
    } else {
        let ordinary = entities::code_native_tool_receipt::Entity::find()
            .filter(entities::code_native_tool_receipt::Column::Owner.eq(owner.as_str()))
            .filter(entities::code_native_tool_receipt::Column::SessionId.eq(session.0))
            .filter(entities::code_native_tool_receipt::Column::IncarnationId.eq(incarnation.0))
            .filter(entities::code_native_tool_receipt::Column::RequestId.eq(&request.request_id))
            .count(&tx)
            .await
            .map_err(store_err)?;
        if ordinary != 0 {
            return Err(invalid(
                "request_id already belongs to an ordinary native call",
            ));
        }
        // This row has no approval card. A delayed proposal finds the tombstone.
        decision::ActiveModel {
            approval_id: Set(uuid::Uuid::new_v4()),
            owner: Set(owner.to_string()),
            session_id: Set(session.0),
            turn_id: Set(live.turn),
            incarnation_id: Set(incarnation.0),
            grant_id: Set(live.grant),
            runtime_id: Set(identity.runtime_id),
            native_turn: Set(i64::from(identity.native_turn)),
            request_id: Set(request.request_id.clone()),
            tool: Set(request.tool.clone()),
            arguments: Set(request.arguments.clone()),
            result: Set(None),
            abandoned: Set(true),
            delivered: Set(false),
        }
        .insert(&tx)
        .await
        .map_err(store_err)?;
    }
    tx.commit().await.map_err(store_err)?;
    Ok(event)
}

/// Whether this owner has a managed helper association for the approval.
pub async fn is_managed_decision(
    store: &DbStore,
    owner: &OwnerId,
    approval: ApprovalId,
) -> Result<bool> {
    Ok(decision::Entity::find_by_id(approval.0)
        .filter(decision::Column::Owner.eq(owner.as_str()))
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .is_some())
}

/// Save the typed answer and approval resolution before releasing the helper.
pub async fn settle_managed_decision(
    store: &DbStore,
    owner: &OwnerId,
    approval_id: ApprovalId,
    answer: ApprovalDecisionKind,
    actor: Option<TurnActor>,
) -> Result<ApprovalSettlement> {
    let initial = decision::Entity::find_by_id(approval_id.0)
        .filter(decision::Column::Owner.eq(owner.as_str()))
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| invalid("managed decision is absent"))?;
    let tx = store.conn.begin().await.map_err(store_err)?;
    let live = validate_live(&tx, owner, &initial).await?;
    let row = decision::Entity::find_by_id(approval_id.0)
        .one(&tx)
        .await
        .map_err(store_err)?
        .ok_or_else(|| invalid("managed decision is absent"))?;
    if row.result.is_some() || row.abandoned {
        return Err(invalid("this card is no longer awaiting a decision"));
    }
    let approval = entities::approval::Entity::find_by_id(approval_id.0)
        .filter(entities::approval::Column::Owner.eq(owner.as_str()))
        .one(&tx)
        .await
        .map_err(store_err)?
        .ok_or_else(|| invalid("managed approval is absent"))?;
    let approval = approval_from_row(approval)?;
    if approval.turn_id.0 != row.turn_id || approval.worker_epoch != Some(live.epoch) {
        return Err(invalid("managed approval belongs to a replaced worker"));
    }
    validate_human_decision(&approval.kind, &answer).map_err(AgentError::InvalidTarget)?;
    let (note, data) = match &answer {
        ApprovalDecisionKind::Answered { answers } => ("The user answered these questions. Continue using the supplied answers.", serde_json::json!({"decision":"answered","answers":answers})),
        ApprovalDecisionKind::PlanDecided { approve: true, feedback } => ("The user accepted the plan. Begin executing it with the session's existing Allow permissions.", serde_json::json!({"decision":"accepted","feedback":feedback,"permission_mode":"allow"})),
        ApprovalDecisionKind::PlanDecided { approve: false, feedback } => ("The user rejected the plan. Do not execute it. Revise it using the feedback before requesting approval again.", serde_json::json!({"decision":"rejected","feedback":feedback})),
        ApprovalDecisionKind::Deny { feedback } => ("The user declined this request. Do not treat it as consent or an answer.", serde_json::json!({"decision":"rejected","feedback":feedback})),
        _ => return Err(invalid("invalid human decision")),
    };
    let result = SupervisorToolResult {
        request: Some(SupervisorToolRequest {
            cancelled: false,
            request_id: row.request_id.clone(),
            tool: row.tool.clone(),
            arguments: row.arguments.clone(),
            turn: Some(identity(&row)?),
        }),
        request_id: row.request_id,
        output: serde_json::json!({"content":note,"is_error":false,"data":data}),
        artifacts: vec![],
    };
    result.validate().map_err(AgentError::InvalidTarget)?;
    let settlement = settle_approval_on_locked(
        &tx,
        owner,
        approval_id,
        SessionId(row.session_id),
        live.epoch,
        ApprovalClaim::Unclaimed,
        answer,
        database_now(&tx).await?,
        actor,
    )
    .await?
    .ok_or_else(|| invalid("this card is no longer awaiting a decision"))?;
    decision::Entity::update_many()
        .col_expr(
            decision::Column::Result,
            sea_orm::sea_query::Expr::value(serde_json::to_value(&result)?),
        )
        .filter(decision::Column::ApprovalId.eq(approval_id.0))
        .exec(&tx)
        .await
        .map_err(store_err)?;
    tx.commit().await.map_err(store_err)?;
    Ok(settlement)
}

/// Abandon stale waits without touching queued turns or a replacement incarnation.
pub async fn reconcile_managed_decisions(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
) -> Result<Vec<SequencedEvent>> {
    let tx = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&tx, session).await? {
        return Ok(vec![]);
    }
    let rows = decision::Entity::find()
        .filter(decision::Column::Owner.eq(owner.as_str()))
        .filter(decision::Column::SessionId.eq(session.0))
        .filter(decision::Column::Abandoned.eq(false))
        .filter(decision::Column::Delivered.eq(false))
        .all(&tx)
        .await
        .map_err(store_err)?;
    let mut events = Vec::new();
    for row in rows {
        let approval = entities::approval::Entity::find_by_id(row.approval_id)
            .one(&tx)
            .await
            .map_err(store_err)?;
        let externally_abandoned = approval
            .as_ref()
            .is_none_or(|approval| approval.state == "abandoned");
        let live = match validate_live(&tx, owner, &row).await {
            Ok(_) => true,
            Err(AgentError::InvalidTarget(_)) => false,
            Err(error) => return Err(error),
        };
        if live && !externally_abandoned {
            continue;
        }
        if let Some(approval) = approval {
            if let Some(epoch) = approval.worker_epoch {
                if let Some(settlement) = settle_approval_on_locked(
                    &tx,
                    owner,
                    ApprovalId(row.approval_id),
                    session,
                    epoch,
                    ApprovalClaim::Unclaimed,
                    ApprovalDecisionKind::Abandoned,
                    database_now(&tx).await?,
                    None,
                )
                .await?
                {
                    events.push(settlement.event);
                }
            }
        }
        decision::Entity::update_many()
            .col_expr(
                decision::Column::Abandoned,
                sea_orm::sea_query::Expr::value(true),
            )
            .filter(decision::Column::ApprovalId.eq(row.approval_id))
            .exec(&tx)
            .await
            .map_err(store_err)?;
    }
    tx.commit().await.map_err(store_err)?;
    Ok(events)
}

/// Return only recorded decisions for their still-running owner turn.
pub async fn managed_decision_results(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    incarnation: CodeIncarnationId,
) -> Result<Vec<SupervisorToolResult>> {
    let tx = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&tx, session).await? {
        return Ok(vec![]);
    }
    let rows = decision::Entity::find()
        .filter(decision::Column::Owner.eq(owner.as_str()))
        .filter(decision::Column::SessionId.eq(session.0))
        .filter(decision::Column::IncarnationId.eq(incarnation.0))
        .filter(decision::Column::Abandoned.eq(false))
        .filter(decision::Column::Delivered.eq(false))
        .filter(decision::Column::Result.is_not_null())
        .all(&tx)
        .await
        .map_err(store_err)?;
    let mut results = Vec::new();
    for row in rows {
        match validate_live(&tx, owner, &row).await {
            Ok(_) => (),
            Err(AgentError::InvalidTarget(_)) => continue,
            Err(error) => return Err(error),
        }
        let result: SupervisorToolResult =
            serde_json::from_value(row.result.expect("query requires result"))?;
        result.validate().map_err(AgentError::InvalidTarget)?;
        if result.request_id != row.request_id {
            return Err(invalid("human decision result correlation changed"));
        }
        results.push(result);
    }
    tx.commit().await.map_err(store_err)?;
    Ok(results)
}

/// Validate a result immediately before each delivery frame or acknowledgment.
pub async fn authorize_managed_decision_delivery(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    incarnation: CodeIncarnationId,
    request: &str,
    mark_delivered: bool,
) -> Result<bool> {
    let tx = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&tx, session).await? {
        return Ok(false);
    }
    let row = decision::Entity::find()
        .filter(decision::Column::Owner.eq(owner.as_str()))
        .filter(decision::Column::SessionId.eq(session.0))
        .filter(decision::Column::IncarnationId.eq(incarnation.0))
        .filter(decision::Column::RequestId.eq(request))
        .one(&tx)
        .await
        .map_err(store_err)?;
    let Some(row) = row else {
        return Ok(false);
    };
    validate_live(&tx, owner, &row).await?;
    if row.result.is_none() {
        return Err(invalid("human decision has no recorded result"));
    }
    if mark_delivered {
        decision::Entity::update_many()
            .col_expr(
                decision::Column::Delivered,
                sea_orm::sea_query::Expr::value(true),
            )
            .filter(decision::Column::ApprovalId.eq(row.approval_id))
            .exec(&tx)
            .await
            .map_err(store_err)?;
    }
    tx.commit().await.map_err(store_err)?;
    Ok(true)
}
