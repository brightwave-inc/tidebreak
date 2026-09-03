use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};

use crate::code::{
    Approval, ApprovalDecisionKind, ApprovalId, ApprovalKind, ApprovalState, Event, SequencedEvent,
    SessionId, SessionLifecycle, TurnId, TurnStatus,
};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};
use super::{acquire_code_session_write_lock, append_event_on_locked};

/// One approval row and its matching journal event committed together.
#[derive(Debug)]
pub struct ApprovalSettlement {
    /// The terminal approval row.
    pub approval: Approval,
    /// The journal event committed beside that row.
    pub event: SequencedEvent,
}

/// The exact durable claim and decision that settle one approval.
#[derive(Debug)]
pub struct ClaimedApprovalSettlement {
    /// Approval to settle.
    pub approval_id: ApprovalId,
    /// Session that owns the approval.
    pub session_id: SessionId,
    /// Worker epoch that created the approval.
    pub worker_epoch: i64,
    /// Claim token reserved before native delivery.
    pub claim: uuid::Uuid,
    /// Decision acknowledged by the engine.
    pub decision: ApprovalDecisionKind,
    /// Time when the engine acknowledged the decision.
    pub decided_at: chrono::DateTime<chrono::Utc>,
}

/// Insert an approval row under its session's owner.
pub async fn insert_approval(store: &DbStore, owner: &OwnerId, approval: &Approval) -> Result<()> {
    approval_active_model(owner, approval)?
        .insert(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

/// Insert an approval only while its exact worker and turn still own the session.
///
/// The session lock serializes this check with worker replacement. A stale
/// permission-prompt request therefore cannot create a newly actionable row
/// after its bearer and native waiter have been revoked.
pub async fn insert_approval_for_worker(
    store: &DbStore,
    owner: &OwnerId,
    approval: &Approval,
) -> Result<Option<SequencedEvent>> {
    let worker_epoch = approval.worker_epoch.ok_or_else(|| {
        AgentError::Store(format!("approval {} has no worker epoch", approval.id))
    })?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, approval.session_id).await? {
        return Ok(None);
    }
    let Some(session) = entities::session::Entity::find_by_id(approval.session_id.0)
        .filter(entities::session::Column::Owner.eq(owner.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    if session.spawn_epoch != worker_epoch
        || session.lifecycle != SessionLifecycle::Running.as_str()
    {
        return Ok(None);
    }
    let turn_is_running = entities::turn::Entity::find_by_id(approval.turn_id.0)
        .filter(entities::turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::turn::Column::SessionId.eq(approval.session_id.0))
        .filter(entities::turn::Column::Status.eq(TurnStatus::Running.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some();
    if !turn_is_running {
        return Ok(None);
    }
    approval_active_model(owner, approval)?
        .insert(&transaction)
        .await
        .map_err(store_err)?;
    let event = Event::ApprovalRequested {
        approval_id: approval.id,
        request: None,
    };
    let seq = append_event_on_locked(&transaction, owner, approval.session_id, &event).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(SequencedEvent { seq, event }))
}

fn approval_active_model(
    owner: &OwnerId,
    approval: &Approval,
) -> Result<entities::approval::ActiveModel> {
    Ok(entities::approval::ActiveModel {
        id: Set(approval.id.0),
        owner: Set(owner.as_str().to_owned()),
        session_id: Set(approval.session_id.0),
        turn_id: Set(approval.turn_id.0),
        kind: Set(serde_json::to_value(&approval.kind)?),
        harness_raw: Set(approval.harness_raw.clone()),
        native_call_id: Set(approval.native_call_id.clone()),
        server_capability: Set(approval.server_capability.clone()),
        request_sha256: Set(approval.request_sha256.clone()),
        worker_epoch: Set(approval.worker_epoch),
        decision_claim: Set(approval.decision_claim),
        claimed_at: Set(approval.claimed_at),
        state: Set(approval.state.as_str().to_owned()),
        feedback: Set(approval.feedback.clone()),
        requested_at: Set(approval.requested_at),
        decided_at: Set(approval.decided_at),
        auto_judge_status: Set(approval
            .auto_judge_status
            .map(|status| status.as_str().to_owned())),
    })
}

/// Insert an approval row inside the caller's transaction, which already
/// holds the session lock. The internal engine's turn lane mints its consent
/// cards and parks this way, beside the journal row that announces them.
pub(in crate::db) async fn insert_approval_on<C>(
    conn: &C,
    owner: &OwnerId,
    approval: &Approval,
) -> Result<()>
where
    C: ConnectionTrait,
{
    approval_active_model(owner, approval)?
        .insert(conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

/// Load one approval row inside the caller's transaction, whatever its owner.
pub(in crate::db) async fn find_approval_row_on<C>(
    conn: &C,
    id: ApprovalId,
) -> Result<Option<entities::approval::Model>>
where
    C: ConnectionTrait,
{
    entities::approval::Entity::find_by_id(id.0)
        .one(conn)
        .await
        .map_err(store_err)
}

/// Set the internal engine's judge marker on one row, inside the caller's
/// transaction. The marker is the judge's ownership of a pending card; it
/// never decides the row by itself.
pub(in crate::db) async fn set_auto_judge_status_on<C>(
    conn: &C,
    id: ApprovalId,
    status: Option<crate::approval::AutoJudgeStatus>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    entities::approval::Entity::update_many()
        .col_expr(
            entities::approval::Column::AutoJudgeStatus,
            sea_orm::sea_query::Expr::value(status.map(|status| status.as_str().to_owned())),
        )
        .filter(entities::approval::Column::Id.eq(id.0))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

/// Load one of the owner's approvals by id.
pub async fn get_approval(
    store: &DbStore,
    owner: &OwnerId,
    id: ApprovalId,
) -> Result<Option<Approval>> {
    let Some(row) = entities::approval::Entity::find_by_id(id.0)
        .filter(entities::approval::Column::Owner.eq(owner.as_str()))
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    Ok(Some(approval_from_row(row)?))
}

/// Atomically reserve one pending approval for a native decision.
///
/// A competing decision or abandonment sees no matching row. The claim stays
/// durable until the exact claimant records the native acknowledgement or
/// recovery abandons it after a restart.
pub async fn claim_approval(
    store: &DbStore,
    owner: &OwnerId,
    id: ApprovalId,
    session_id: SessionId,
    worker_epoch: i64,
    claim: uuid::Uuid,
    claimed_at: chrono::DateTime<chrono::Utc>,
) -> Result<Option<Approval>> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, session_id).await? {
        return Ok(None);
    }
    let Some(session) = entities::session::Entity::find_by_id(session_id.0)
        .filter(entities::session::Column::Owner.eq(owner.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    if session.spawn_epoch != worker_epoch
        || session.lifecycle != crate::code::SessionLifecycle::Running.as_str()
    {
        return Ok(None);
    }
    let result = entities::approval::Entity::update_many()
        .col_expr(
            entities::approval::Column::DecisionClaim,
            sea_orm::sea_query::Expr::value(Some(claim)),
        )
        .col_expr(
            entities::approval::Column::ClaimedAt,
            sea_orm::sea_query::Expr::value(Some(claimed_at)),
        )
        .filter(entities::approval::Column::Id.eq(id.0))
        .filter(entities::approval::Column::Owner.eq(owner.as_str()))
        .filter(entities::approval::Column::SessionId.eq(session_id.0))
        .filter(entities::approval::Column::State.eq(ApprovalState::Pending.as_str()))
        .filter(entities::approval::Column::DecisionClaim.is_null())
        .filter(entities::approval::Column::WorkerEpoch.eq(worker_epoch))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if result.rows_affected != 1 {
        return Ok(None);
    }
    let row = entities::approval::Entity::find_by_id(id.0)
        .filter(entities::approval::Column::Owner.eq(owner.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("claimed approval {id} disappeared")))?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(approval_from_row(row)?))
}

/// Finish the exact claimed approval and append its matching journal event.
pub async fn settle_approval_claim(
    store: &DbStore,
    owner: &OwnerId,
    settlement: ClaimedApprovalSettlement,
) -> Result<Option<ApprovalSettlement>> {
    settle_approval(
        store,
        owner,
        settlement.approval_id,
        settlement.session_id,
        settlement.worker_epoch,
        ApprovalClaim::Exact(settlement.claim),
        settlement.decision,
        settlement.decided_at,
    )
    .await
}

/// Settle a pending approval the engine decided on its own channel — a
/// standing grant or an auto-approval judge answered before any route did —
/// and append its matching event.
///
/// Only an unclaimed pending row with this engine-native call id on this
/// session settles; a row a decide route already claimed belongs to that
/// route. `None` when nothing matched.
pub async fn settle_engine_observed_approval(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    worker_epoch: i64,
    native_call_id: &str,
    decision: ApprovalDecisionKind,
    decided_at: chrono::DateTime<chrono::Utc>,
) -> Result<Option<ApprovalSettlement>> {
    let Some(row) = entities::approval::Entity::find()
        .filter(entities::approval::Column::Owner.eq(owner.as_str()))
        .filter(entities::approval::Column::SessionId.eq(session_id.0))
        .filter(entities::approval::Column::NativeCallId.eq(native_call_id))
        .filter(entities::approval::Column::State.eq(ApprovalState::Pending.as_str()))
        .filter(entities::approval::Column::DecisionClaim.is_null())
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    settle_approval(
        store,
        owner,
        ApprovalId(row.id),
        session_id,
        worker_epoch,
        ApprovalClaim::Unclaimed,
        decision,
        decided_at,
    )
    .await
}

/// Abandon one unclaimed pending approval and append its matching event.
pub async fn abandon_pending_approval(
    store: &DbStore,
    owner: &OwnerId,
    id: ApprovalId,
    session_id: SessionId,
    worker_epoch: i64,
    decided_at: chrono::DateTime<chrono::Utc>,
) -> Result<Option<ApprovalSettlement>> {
    settle_approval(
        store,
        owner,
        id,
        session_id,
        worker_epoch,
        ApprovalClaim::Unclaimed,
        ApprovalDecisionKind::Abandoned,
        decided_at,
    )
    .await
}

/// Which claim a settlement must find on the row.
#[derive(Clone, Copy)]
pub(in crate::db) enum ApprovalClaim {
    /// No decision route holds the row.
    Unclaimed,
    /// The exact claim a decision route reserved before native delivery.
    Exact(uuid::Uuid),
}

#[allow(clippy::too_many_arguments)]
async fn settle_approval(
    store: &DbStore,
    owner: &OwnerId,
    id: ApprovalId,
    session_id: SessionId,
    worker_epoch: i64,
    claim: ApprovalClaim,
    decision: ApprovalDecisionKind,
    decided_at: chrono::DateTime<chrono::Utc>,
) -> Result<Option<ApprovalSettlement>> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, session_id).await? {
        return Ok(None);
    }
    let session_exists = entities::session::Entity::find_by_id(session_id.0)
        .filter(entities::session::Column::Owner.eq(owner.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some();
    if !session_exists {
        return Ok(None);
    }
    // The approval row carries the worker epoch that created and claimed it.
    // Do not require the session's current epoch here: after native
    // acknowledgement, an old worker must still make its exact durable claim
    // terminal even if another process has already attached a replacement.
    let settlement = settle_approval_on_locked(
        &transaction,
        owner,
        id,
        session_id,
        worker_epoch,
        claim,
        decision,
        decided_at,
    )
    .await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(settlement)
}

/// Settle one pending approval row and append its `ApprovalResolved` in the
/// caller's transaction, which holds the session lock.
///
/// The one settlement path for every engine (decision 0048 step 5): the code
/// decision route, the chat routes, the internal engine's judge, and the turn
/// lane's cancellation and terminal sweeps all land here, so a card is one
/// row with one resolution row whichever surface decided it.
#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn settle_approval_on_locked<C>(
    conn: &C,
    owner: &OwnerId,
    id: ApprovalId,
    session_id: SessionId,
    worker_epoch: i64,
    claim: ApprovalClaim,
    decision: ApprovalDecisionKind,
    decided_at: chrono::DateTime<chrono::Utc>,
) -> Result<Option<ApprovalSettlement>>
where
    C: ConnectionTrait,
{
    let (state, feedback) = match &decision {
        ApprovalDecisionKind::Approve | ApprovalDecisionKind::ApprovedWithGrant { .. } => {
            (ApprovalState::Approved, None)
        }
        ApprovalDecisionKind::Deny { feedback } => (ApprovalState::Denied, feedback.clone()),
        ApprovalDecisionKind::Abandoned => (ApprovalState::Abandoned, None),
        // Structured resolutions settle the row as approved: the substance
        // (answers, plan verdict) rides the journal event and the engine's
        // own state, not this row.
        ApprovalDecisionKind::Answered { .. } => (ApprovalState::Approved, None),
        ApprovalDecisionKind::PlanDecided { approve, feedback } => (
            if *approve {
                ApprovalState::Approved
            } else {
                ApprovalState::Denied
            },
            feedback.clone(),
        ),
    };
    let mut update = entities::approval::Entity::update_many()
        .col_expr(
            entities::approval::Column::State,
            sea_orm::sea_query::Expr::value(state.as_str().to_owned()),
        )
        .col_expr(
            entities::approval::Column::Feedback,
            sea_orm::sea_query::Expr::value(feedback),
        )
        .col_expr(
            entities::approval::Column::DecidedAt,
            sea_orm::sea_query::Expr::value(Some(decided_at)),
        )
        .col_expr(
            entities::approval::Column::DecisionClaim,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::approval::Column::ClaimedAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
        )
        .filter(entities::approval::Column::Id.eq(id.0))
        .filter(entities::approval::Column::Owner.eq(owner.as_str()))
        .filter(entities::approval::Column::SessionId.eq(session_id.0))
        .filter(entities::approval::Column::WorkerEpoch.eq(worker_epoch))
        .filter(entities::approval::Column::State.eq(ApprovalState::Pending.as_str()));
    update = match claim {
        ApprovalClaim::Unclaimed => {
            update.filter(entities::approval::Column::DecisionClaim.is_null())
        }
        ApprovalClaim::Exact(claim) => {
            update.filter(entities::approval::Column::DecisionClaim.eq(claim))
        }
    };
    let result = update.exec(conn).await.map_err(store_err)?;
    if result.rows_affected != 1 {
        return Ok(None);
    }
    let row = entities::approval::Entity::find_by_id(id.0)
        .filter(entities::approval::Column::Owner.eq(owner.as_str()))
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("settled approval {id} disappeared")))?;
    // A standing grant exists only from the transaction that approves the
    // card it was chosen on: minted here, beside the row's terminal state
    // and its resolution row, never ahead of them.
    if let ApprovalDecisionKind::ApprovedWithGrant { scope } = &decision {
        mint_standing_grant_on(conn, &row, scope, decided_at).await?;
    }
    let event = Event::ApprovalResolved {
        approval_id: id,
        decision,
    };
    let seq = append_event_on_locked(conn, owner, session_id, &event).await?;
    let approval = approval_from_row(row)?;
    Ok(Some(ApprovalSettlement {
        approval,
        event: SequencedEvent { seq, event },
    }))
}

/// Write the standing grant an approved-with-grant settlement names, in
/// the settling transaction.
///
/// Only a card the internal engine minted carries the request the grant is
/// keyed on (tool name and consent kind); the level follows where the
/// conversation lives — its project when it has one, else itself. The scope
/// must be one the kind can be granted at and must cover the parked call's
/// own arguments, on the same terms the chat route's approve-and-remember
/// checks. A grant already written for this call (a retry) is left as is.
async fn mint_standing_grant_on<C>(
    conn: &C,
    row: &entities::approval::Model,
    scope: &crate::GrantScope,
    granted_at: chrono::DateTime<chrono::Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let request = crate::approval::InternalToolApprovalRequest::from_raw(&row.harness_raw)
        .ok_or_else(|| {
            AgentError::Store(format!(
                "approval {} offers no standing grant: it is not a consent card",
                row.id
            ))
        })?;
    if entities::standing_tool_grant::Entity::find_by_id(row.id)
        .one(conn)
        .await
        .map_err(store_err)?
        .is_some()
    {
        return Ok(());
    }
    let call = entities::tool_call::Entity::find_by_id(row.id)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "approval {} is parked on a call that does not exist",
                row.id
            ))
        })?;
    if call.name != request.tool_name
        || !request.kind.is_approvable()
        || !request.kind.grantable_at(scope)
        || !scope.covers_call(&call.name, &call.arguments)
    {
        return Err(AgentError::Store(format!(
            "approval {} cannot be granted at that scope",
            row.id
        )));
    }
    let project_id = entities::session::Entity::find_by_id(row.session_id)
        .one(conn)
        .await
        .map_err(store_err)?
        .and_then(|session| session.project_id);
    entities::standing_tool_grant::ActiveModel {
        source_call_id: Set(row.id),
        chat_id: Set(project_id.is_none().then_some(row.session_id)),
        project_id: Set(project_id),
        tool_name: Set(call.name),
        approval_kind: Set(request.kind.standing_grant_key().to_owned()),
        scope: Set(serde_json::to_value(scope)?),
        granted_at: Set(granted_at),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

/// Abandon every pending approval after its exact worker has stopped.
///
/// Recovery refuses to touch rows while the session still says `Running` or
/// after another worker epoch takes ownership. The same transaction settles
/// claimed and unclaimed rows, so a restart cannot expose a pending approval
/// whose native waiter disappeared with the process.
pub async fn abandon_pending_approvals_for_stopped_session(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    expected_spawn_epoch: i64,
    decided_at: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<ApprovalSettlement>> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, session_id).await? {
        return Ok(Vec::new());
    }
    let Some(session) = entities::session::Entity::find_by_id(session_id.0)
        .filter(entities::session::Column::Owner.eq(owner.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        return Ok(Vec::new());
    };
    if session.spawn_epoch != expected_spawn_epoch
        || session.lifecycle == SessionLifecycle::Running.as_str()
    {
        return Ok(Vec::new());
    }
    let waiting_turn_ids: std::collections::HashSet<uuid::Uuid> = entities::turn::Entity::find()
        .filter(entities::turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::turn::Column::SessionId.eq(session_id.0))
        .filter(entities::turn::Column::Status.eq(TurnStatus::Waiting.as_str()))
        .all(&transaction)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|row| row.id)
        .collect();
    let rows = entities::approval::Entity::find()
        .filter(entities::approval::Column::Owner.eq(owner.as_str()))
        .filter(entities::approval::Column::SessionId.eq(session_id.0))
        .filter(entities::approval::Column::WorkerEpoch.eq(expected_spawn_epoch))
        .filter(entities::approval::Column::State.eq(ApprovalState::Pending.as_str()))
        .order_by_asc(entities::approval::Column::RequestedAt)
        .all(&transaction)
        .await
        .map_err(store_err)?
        .into_iter()
        .filter(|row| !waiting_turn_ids.contains(&row.turn_id))
        .collect::<Vec<_>>();
    let mut abandoned = Vec::new();
    for row in rows {
        let claim = match row.decision_claim {
            Some(claim) => ApprovalClaim::Exact(claim),
            None => ApprovalClaim::Unclaimed,
        };
        if let Some(settlement) = settle_approval_on_locked(
            &transaction,
            owner,
            ApprovalId(row.id),
            session_id,
            expected_spawn_epoch,
            claim,
            ApprovalDecisionKind::Abandoned,
            decided_at,
        )
        .await?
        {
            abandoned.push(settlement);
        }
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(abandoned)
}

/// Point a waiting turn's pending approvals at the worker that just attached.
///
/// Recovery leaves those rows pending so the park can still resolve. The
/// attach bumps `spawn_epoch`, and decide checks that the approval's worker
/// epoch matches the live session, so you rewrite the rows onto the new
/// epoch before the worker waits on the park. You also drop any in-flight
/// claim: the native waiter that held it died with the old worker.
pub async fn rebind_pending_approvals_to_worker(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    turn_id: TurnId,
    to_epoch: i64,
) -> Result<u64> {
    let result = entities::approval::Entity::update_many()
        .col_expr(
            entities::approval::Column::WorkerEpoch,
            sea_orm::sea_query::Expr::value(Some(to_epoch)),
        )
        .col_expr(
            entities::approval::Column::DecisionClaim,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::approval::Column::ClaimedAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
        )
        .filter(entities::approval::Column::Owner.eq(owner.as_str()))
        .filter(entities::approval::Column::SessionId.eq(session_id.0))
        .filter(entities::approval::Column::TurnId.eq(turn_id.0))
        .filter(entities::approval::Column::State.eq(ApprovalState::Pending.as_str()))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected)
}

/// The owner's approvals, optionally filtered by state and session.
pub async fn list_approvals(
    store: &DbStore,
    owner: &OwnerId,
    state: Option<ApprovalState>,
    session_id: Option<SessionId>,
) -> Result<Vec<Approval>> {
    let mut query = entities::approval::Entity::find()
        .filter(entities::approval::Column::Owner.eq(owner.as_str()));
    if let Some(state) = state {
        query = query.filter(entities::approval::Column::State.eq(state.as_str().to_owned()));
    }
    if let Some(session_id) = session_id {
        query = query.filter(entities::approval::Column::SessionId.eq(session_id.0));
    }
    let rows = query.all(&store.conn).await.map_err(store_err)?;
    rows.into_iter().map(approval_from_row).collect()
}

pub(in crate::db) fn approval_from_row(row: entities::approval::Model) -> Result<Approval> {
    let state = ApprovalState::from_str(&row.state).ok_or_else(|| {
        AgentError::Store(format!(
            "approval {} has unknown state {}",
            row.id, row.state
        ))
    })?;
    let kind = serde_json::from_value::<ApprovalKind>(row.kind)
        .map_err(|err| AgentError::Store(format!("approval {} kind: {err}", row.id)))?;
    Ok(Approval {
        id: ApprovalId(row.id),
        session_id: SessionId(row.session_id),
        turn_id: TurnId(row.turn_id),
        kind,
        harness_raw: row.harness_raw,
        native_call_id: row.native_call_id,
        server_capability: row.server_capability,
        request_sha256: row.request_sha256,
        worker_epoch: row.worker_epoch,
        decision_claim: row.decision_claim,
        claimed_at: row.claimed_at,
        state,
        feedback: row.feedback,
        requested_at: row.requested_at,
        decided_at: row.decided_at,
        auto_judge_status: match row.auto_judge_status.as_deref() {
            None => None,
            Some(token) => Some(
                crate::approval::AutoJudgeStatus::from_str(token).ok_or_else(|| {
                    AgentError::Store(format!(
                        "approval {} has unknown auto-judge status {token}",
                        row.id
                    ))
                })?,
            ),
        },
    })
}
