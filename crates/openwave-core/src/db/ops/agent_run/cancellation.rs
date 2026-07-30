use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::id::{AgentRunId, ChatId};
use crate::model::{
    AgentRunCancellationReason, AgentRunCancellationSignal, AgentRunInboxStatus,
    AgentRunResultPayload, AgentRunStatus, AgentRunTier,
};
use crate::storage::{FinishAgentRunCancellationOutcome, RequestAgentRunCancellationOutcome};

use super::super::super::{entities, store_err, DbStore};
use super::{
    acquire_agent_run_claim_lock, agent_run_from_model, agent_run_result_display_text,
    agent_run_result_from_model, agent_run_result_payload_json, agent_run_result_payload_kind,
    agent_run_status_from_db, database_now, find_agent_run_inbox_on, find_by_id_on,
    load_agent_run_inbox_by_ids_on, load_agent_run_inbox_entry_on,
};

#[derive(Clone, Copy)]
struct CancellationIdentity {
    lease_token: uuid::Uuid,
    attempt_count: i32,
    claim_count: i32,
}

// Reserved for provenance minted when a child is cancelled before any worker
// claim. Operational run claim counts are schema-bounded below this sentinel,
// so a terminal synthetic receipt can never be mistaken for scheduler state.
const UNCLAIMED_CANCELLATION_CLAIM_COUNT: i32 = i32::MAX;

pub(in crate::db) async fn get_agent_run_cancellation_signal(
    store: &DbStore,
    id: AgentRunId,
) -> Result<Option<AgentRunCancellationSignal>> {
    let receipt = entities::agent_run_cancellation::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(receipt.map(|receipt| AgentRunCancellationSignal {
        agent_run_id: AgentRunId(receipt.agent_run_id),
        lease_token: receipt.lease_token,
        attempt_count: receipt.attempt_count,
        claim_count: receipt.claim_count,
    }))
}

pub(in crate::db) async fn request_agent_run_cancellation(
    store: &DbStore,
    id: AgentRunId,
) -> Result<Option<RequestAgentRunCancellationOutcome>> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    let Some(run) = find_by_id_on(&transaction, id).await? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if run.tier != AgentRunTier::Background.as_str() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let cancellation = entities::agent_run_cancellation::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?;
    if cancellation.is_some() {
        if run.status == AgentRunStatus::Cancelling.as_str() {
            validate_cancellation_request_on(&transaction, &run, None).await?;
        } else {
            validate_cancellation_delivery_on(&transaction, &run, None).await?;
        }
        let run = agent_run_from_model(run)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(RequestAgentRunCancellationOutcome::Existing(run)));
    }
    let status = agent_run_status_from_db(&run.status)?;
    match status {
        AgentRunStatus::Cancelling => {
            let run = agent_run_from_model(run)?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(RequestAgentRunCancellationOutcome::Existing(run)));
        }
        AgentRunStatus::Cancelled => {
            return Err(AgentError::Store(format!(
                "cancelled sandbox run {id} is missing its durable cancellation delivery"
            )));
        }
        AgentRunStatus::Completed | AgentRunStatus::Failed => {
            let run = agent_run_from_model(run)?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(RequestAgentRunCancellationOutcome::AlreadyTerminal(
                run,
            )));
        }
        AgentRunStatus::Queued
        | AgentRunStatus::Waiting
        | AgentRunStatus::RetryWait
        | AgentRunStatus::Running => {}
        AgentRunStatus::Active => {
            transaction.commit().await.map_err(store_err)?;
            return Ok(None);
        }
    }
    if run.updated_at > now {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let immediate = status != AgentRunStatus::Running
        || run.lease_expires_at.is_none_or(|expiry| expiry <= now)
        || run.deadline_at.is_none_or(|deadline| deadline <= now);
    if immediate {
        let (reason, inbox_status) = cancellation_context_on(&transaction, &run).await?;
        if !terminalize_cancellation_on(&transaction, &run, now, reason, inbox_status).await? {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(None);
        }
        let run = find_by_id_on(&transaction, id)
            .await?
            .expect("cancelled run exists");
        validate_cancellation_delivery_on(&transaction, &run, None).await?;
        let run = agent_run_from_model(run)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(RequestAgentRunCancellationOutcome::Cancelled(run)));
    }

    let reason = cancellation_context_on(&transaction, &run).await?.0;
    let identity = cancellation_identity_on(&transaction, &run, now).await?;
    insert_cancellation_request_on(&transaction, &run, identity, reason, now).await?;
    let updated = entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(AgentRunStatus::Cancelling.as_str()),
        )
        .col_expr(
            entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::agent_run::Column::Id.eq(id.0))
        .filter(entities::agent_run::Column::Status.eq(AgentRunStatus::Running.as_str()))
        .filter(entities::agent_run::Column::AttemptCount.eq(run.attempt_count))
        .filter(entities::agent_run::Column::ClaimCount.eq(run.claim_count))
        .filter(entities::agent_run::Column::LeaseToken.eq(run.lease_token))
        .filter(entities::agent_run::Column::LeaseExpiresAt.eq(run.lease_expires_at))
        .filter(entities::agent_run::Column::LeaseExpiresAt.gt(now))
        .filter(entities::agent_run::Column::DeadlineAt.gt(now))
        .filter(entities::agent_run::Column::UpdatedAt.eq(run.updated_at))
        .filter(entities::agent_run::Column::UpdatedAt.lte(now))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let run = find_by_id_on(&transaction, id)
        .await?
        .expect("cancelling run exists");
    let run = agent_run_from_model(run)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(RequestAgentRunCancellationOutcome::Requested(run)))
}

pub(in crate::db) async fn finish_agent_run_cancellation(
    store: &DbStore,
    id: AgentRunId,
    lease_token: uuid::Uuid,
) -> Result<Option<FinishAgentRunCancellationOutcome>> {
    if lease_token.is_nil() {
        return Err(AgentError::Store(
            "agent-run cancellation requires a non-nil lease token".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    let Some(run) = find_by_id_on(&transaction, id).await? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if entities::agent_run_cancellation::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some()
    {
        let receipt = if run.status == AgentRunStatus::Cancelled.as_str() {
            validate_cancellation_delivery_on(&transaction, &run, Some(lease_token)).await?
        } else {
            validate_cancellation_request_on(&transaction, &run, Some(lease_token)).await?
        };
        if receipt.is_none() {
            transaction.commit().await.map_err(store_err)?;
            return Ok(None);
        }
        if run.status == AgentRunStatus::Cancelled.as_str() {
            let run = agent_run_from_model(run)?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(FinishAgentRunCancellationOutcome::Existing(run)));
        }
    }
    let Some(claim) = entities::agent_run_claim::Entity::find_by_id(lease_token)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .filter(|claim| claim.agent_run_id == Some(id.0))
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let valid = run.tier == AgentRunTier::Background.as_str()
        && run.status == AgentRunStatus::Cancelling.as_str()
        && run.lease_token == Some(lease_token)
        && Some(run.attempt_count) == claim.attempt_count
        && Some(run.claim_count) == claim.claim_count
        && run.lease_expires_at.is_some_and(|expiry| expiry > now)
        && run.deadline_at.is_some_and(|deadline| deadline > now)
        && run.updated_at <= now;
    if !valid {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let (reason, inbox_status) = cancellation_context_on(&transaction, &run).await?;
    if !terminalize_cancellation_on(&transaction, &run, now, reason, inbox_status).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let run = find_by_id_on(&transaction, id)
        .await?
        .expect("cancelled run exists");
    validate_cancellation_delivery_on(&transaction, &run, Some(lease_token)).await?;
    let run = agent_run_from_model(run)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(FinishAgentRunCancellationOutcome::Cancelled(run)))
}

/// Cancel and fence every child admitted by one exact foreground turn.
///
/// The caller must hold the scheduler, chat, and turn locks in that order.
/// Keeping the outer scheduler lock makes cancellation-request provenance and
/// the child lifecycle transition one serial decision with worker claims,
/// direct cancellation, acknowledgements, and reaping.
pub(in crate::db) async fn cancel_sandbox_children_for_origin_turn_on<C>(
    conn: &C,
    turn: &entities::turn_run::Model,
    now: chrono::DateTime<Utc>,
    reason: AgentRunCancellationReason,
) -> Result<bool>
where
    C: sea_orm::ConnectionTrait,
{
    let admissions = entities::sandbox_agent_admission::Entity::find()
        .filter(entities::sandbox_agent_admission::Column::OriginTurnId.eq(turn.id))
        .order_by_asc(entities::sandbox_agent_admission::Column::AdmittedAt)
        .order_by_asc(entities::sandbox_agent_admission::Column::ChildRunId)
        .all(conn)
        .await
        .map_err(store_err)?;
    for admission in admissions {
        if admission.parent_run_id != turn.agent_run_id || admission.chat_id != turn.chat_id {
            return Err(AgentError::Store(format!(
                "sandbox admission {} does not match origin turn {}",
                admission.child_run_id, turn.id
            )));
        }
        let child_id = AgentRunId(admission.child_run_id);
        let Some(child) = find_by_id_on(conn, child_id).await? else {
            return Err(AgentError::Store(format!(
                "sandbox admission references missing child {child_id}"
            )));
        };
        if child.parent_id != Some(turn.agent_run_id)
            || child.chat_id != turn.chat_id
            || child.tier != AgentRunTier::Background.as_str()
            || child.spawn_call_id != Some(admission.spawn_call_id)
        {
            return Err(AgentError::Store(format!(
                "sandbox child {child_id} does not match its admission"
            )));
        }
        agent_run_from_model(child.clone())?;
        match agent_run_status_from_db(&child.status)? {
            AgentRunStatus::Completed | AgentRunStatus::Failed => {
                retire_inbox_on(conn, turn.agent_run_id, &child).await?;
            }
            AgentRunStatus::Cancelled => {
                validate_cancellation_delivery_on(conn, &child, None).await?;
                retire_inbox_on(conn, turn.agent_run_id, &child).await?;
            }
            AgentRunStatus::Running
                if child.lease_expires_at.is_some_and(|expiry| expiry > now)
                    && child.deadline_at.is_some_and(|deadline| deadline > now) =>
            {
                let identity = cancellation_identity_on(conn, &child, now).await?;
                insert_cancellation_request_on(conn, &child, identity, reason, now).await?;
                if !mark_live_run_cancelling_on(conn, &child, now).await? {
                    return Ok(false);
                }
            }
            AgentRunStatus::Cancelling
                if child.lease_expires_at.is_some_and(|expiry| expiry > now)
                    && child.deadline_at.is_some_and(|deadline| deadline > now) =>
            {
                validate_cancellation_request_on(conn, &child, None).await?;
            }
            AgentRunStatus::Queued
            | AgentRunStatus::Running
            | AgentRunStatus::Cancelling
            | AgentRunStatus::Waiting
            | AgentRunStatus::RetryWait => {
                if !terminalize_cancellation_on(
                    conn,
                    &child,
                    now,
                    reason,
                    AgentRunInboxStatus::Cancelled,
                )
                .await?
                {
                    return Ok(false);
                }
            }
            AgentRunStatus::Active => {
                return Err(AgentError::Store(format!(
                    "sandbox admission references foreground child {child_id}"
                )));
            }
        }
    }
    Ok(true)
}

/// Validate every child admitted by one exact foreground turn and return the
/// ones whose terminal delivery is not yet consumed or explicitly retired.
///
/// The caller holds the scheduler, chat, and turn locks in that order. Stable
/// admission ordering makes the typed completion fence deterministic across
/// retries and database backends.
pub(in crate::db) async fn unsettled_sandbox_children_for_origin_turn_on<C>(
    conn: &C,
    turn: &entities::turn_run::Model,
) -> Result<Vec<AgentRunId>>
where
    C: sea_orm::ConnectionTrait,
{
    let parent_id = AgentRunId(turn.agent_run_id);
    let parent = find_by_id_on(conn, parent_id).await?.ok_or_else(|| {
        AgentError::Store(format!(
            "origin turn {} is missing its foreground agent",
            turn.id
        ))
    })?;
    if parent.id != turn.agent_run_id
        || parent.chat_id != turn.chat_id
        || parent.tier != AgentRunTier::Foreground.as_str()
        || parent.depth != 0
        || parent.parent_id.is_some()
    {
        return Err(AgentError::Store(format!(
            "origin turn {} does not match its foreground agent",
            turn.id
        )));
    }

    let admissions = entities::sandbox_agent_admission::Entity::find()
        .filter(entities::sandbox_agent_admission::Column::OriginTurnId.eq(turn.id))
        .order_by_asc(entities::sandbox_agent_admission::Column::AdmittedAt)
        .order_by_asc(entities::sandbox_agent_admission::Column::ChildRunId)
        .all(conn)
        .await
        .map_err(store_err)?;
    let mut unsettled = Vec::with_capacity(admissions.len());
    for admission in admissions {
        if admission.parent_run_id != turn.agent_run_id
            || admission.chat_id != turn.chat_id
            || AgentRunId(admission.child_run_id)
                != AgentRunId::sandbox_for_spawn_call(crate::CallId(admission.spawn_call_id))
        {
            return Err(AgentError::Store(format!(
                "sandbox admission {} does not match origin turn {}",
                admission.child_run_id, turn.id
            )));
        }
        let child_id = AgentRunId(admission.child_run_id);
        let child = find_by_id_on(conn, child_id).await?.ok_or_else(|| {
            AgentError::Store(format!(
                "sandbox admission references missing child {child_id}"
            ))
        })?;
        if child.parent_id != Some(turn.agent_run_id)
            || child.chat_id != turn.chat_id
            || child.tier != AgentRunTier::Background.as_str()
            || child.depth != 1
            || child.spawn_call_id != Some(admission.spawn_call_id)
        {
            return Err(AgentError::Store(format!(
                "sandbox child {child_id} does not match its admission"
            )));
        }
        agent_run_from_model(child.clone())?;

        let status = agent_run_status_from_db(&child.status)?;
        if !status.is_terminal() {
            if find_agent_run_inbox_on(conn, parent_id, child_id)
                .await?
                .is_some()
            {
                return Err(AgentError::Store(format!(
                    "nonterminal sandbox child {child_id} has a terminal inbox delivery"
                )));
            }
            unsettled.push(child_id);
            continue;
        }

        if status == AgentRunStatus::Cancelled {
            validate_cancellation_delivery_on(conn, &child, None).await?;
        }
        let inbox = load_agent_run_inbox_by_ids_on(conn, parent_id, child_id)
            .await?
            .ok_or_else(|| {
                AgentError::Store(format!(
                    "terminal sandbox child {child_id} is missing its inbox delivery"
                ))
            })?;
        let result_claim = entities::agent_run_claim::Entity::find_by_id(inbox.result.lease_token)
            .one(conn)
            .await
            .map_err(store_err)?
            .ok_or_else(|| {
                AgentError::Store(format!(
                    "terminal sandbox child {child_id} result is missing claim provenance"
                ))
            })?;
        if inbox.chat_id != ChatId(turn.chat_id)
            || inbox.result.agent_run_id != child_id
            || (status != AgentRunStatus::Cancelled
                && (inbox.result.attempt_count != child.attempt_count
                    || inbox.result.claim_count != child.claim_count
                    || result_claim.agent_run_id != Some(child.id)
                    || result_claim.attempt_count != Some(child.attempt_count)
                    || result_claim.claim_count != Some(child.claim_count)))
        {
            return Err(AgentError::Store(format!(
                "terminal sandbox child {child_id} has mismatched result provenance"
            )));
        }
        if !matches!(
            inbox.status,
            AgentRunInboxStatus::Consumed | AgentRunInboxStatus::Cancelled
        ) {
            unsettled.push(child_id);
        }
    }
    Ok(unsettled)
}

pub(super) async fn reap_expired_cancelling_on<C>(
    conn: &C,
    candidate: &entities::agent_run::Model,
    now: chrono::DateTime<Utc>,
) -> Result<bool>
where
    C: sea_orm::ConnectionTrait,
{
    let (reason, inbox_status) = cancellation_context_on(conn, candidate).await?;
    terminalize_cancellation_on(conn, candidate, now, reason, inbox_status).await
}

async fn mark_live_run_cancelling_on<C>(
    conn: &C,
    run: &entities::agent_run::Model,
    now: chrono::DateTime<Utc>,
) -> Result<bool>
where
    C: sea_orm::ConnectionTrait,
{
    let updated = entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(AgentRunStatus::Cancelling.as_str()),
        )
        .col_expr(
            entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::agent_run::Column::Id.eq(run.id))
        .filter(entities::agent_run::Column::Status.eq(AgentRunStatus::Running.as_str()))
        .filter(entities::agent_run::Column::AttemptCount.eq(run.attempt_count))
        .filter(entities::agent_run::Column::ClaimCount.eq(run.claim_count))
        .filter(entities::agent_run::Column::LeaseToken.eq(run.lease_token))
        .filter(entities::agent_run::Column::LeaseExpiresAt.eq(run.lease_expires_at))
        .filter(entities::agent_run::Column::LeaseExpiresAt.gt(now))
        .filter(entities::agent_run::Column::DeadlineAt.gt(now))
        .filter(entities::agent_run::Column::UpdatedAt.eq(run.updated_at))
        .filter(entities::agent_run::Column::UpdatedAt.lte(now))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(updated.rows_affected == 1)
}

async fn terminalize_cancellation_on<C>(
    conn: &C,
    run: &entities::agent_run::Model,
    now: chrono::DateTime<Utc>,
    reason: AgentRunCancellationReason,
    inbox_status: AgentRunInboxStatus,
) -> Result<bool>
where
    C: sea_orm::ConnectionTrait,
{
    if run.tier != AgentRunTier::Background.as_str() {
        return Err(AgentError::Store(
            "only sandbox runs can be cancelled".into(),
        ));
    }
    let status = agent_run_status_from_db(&run.status)?;
    if status.is_terminal() || status == AgentRunStatus::Active {
        return Ok(false);
    }
    let Some(parent_id) = run.parent_id else {
        return Err(AgentError::Store(format!(
            "sandbox run {} is missing its parent",
            run.id
        )));
    };
    let admission = entities::sandbox_agent_admission::Entity::find_by_id(run.id)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("sandbox run {} is missing admission", run.id)))?;
    if admission.parent_run_id != parent_id || admission.chat_id != run.chat_id {
        return Err(AgentError::Store(format!(
            "sandbox run {} has a mismatched admission",
            run.id
        )));
    }
    let existing_receipt = entities::agent_run_cancellation::Entity::find_by_id(run.id)
        .one(conn)
        .await
        .map_err(store_err)?;
    let (identity, reason) = if let Some(receipt) = existing_receipt {
        validate_cancellation_request_on(conn, run, None).await?;
        let stored_reason =
            AgentRunCancellationReason::parse(&receipt.reason).ok_or_else(|| {
                AgentError::Store(format!(
                    "sandbox cancellation {} has invalid reason",
                    run.id
                ))
            })?;
        (
            CancellationIdentity {
                lease_token: receipt.lease_token,
                attempt_count: receipt.attempt_count,
                claim_count: receipt.claim_count,
            },
            stored_reason,
        )
    } else {
        let identity = cancellation_identity_on(conn, run, now).await?;
        insert_cancellation_request_on(conn, run, identity, reason, now).await?;
        (identity, reason)
    };
    if status == AgentRunStatus::Waiting {
        super::super::sandbox_tool::cancel_sandbox_tool_call_for_run_on(
            conn,
            AgentRunId(run.id),
            now,
        )
        .await?;
    }
    let update = entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(AgentRunStatus::Cancelled.as_str()),
        )
        .col_expr(
            entities::agent_run::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::agent_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::agent_run::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorCode,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorDetail,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::agent_run::Column::Id.eq(run.id))
        .filter(entities::agent_run::Column::Status.eq(run.status.clone()))
        .filter(entities::agent_run::Column::AttemptCount.eq(run.attempt_count))
        .filter(entities::agent_run::Column::ClaimCount.eq(run.claim_count))
        .filter(entities::agent_run::Column::UpdatedAt.eq(run.updated_at))
        .filter(entities::agent_run::Column::UpdatedAt.lte(now));
    let update = if let Some(token) = run.lease_token {
        update
            .filter(entities::agent_run::Column::LeaseToken.eq(token))
            .filter(entities::agent_run::Column::LeaseExpiresAt.eq(run.lease_expires_at))
    } else {
        update
            .filter(entities::agent_run::Column::LeaseToken.is_null())
            .filter(entities::agent_run::Column::LeaseExpiresAt.is_null())
    };
    if update.exec(conn).await.map_err(store_err)?.rows_affected != 1 {
        return Ok(false);
    }

    let payload = AgentRunResultPayload::Cancelled { reason };
    let text = agent_run_result_display_text(&payload);
    entities::agent_run_result::ActiveModel {
        agent_run_id: Set(run.id),
        lease_token: Set(identity.lease_token),
        attempt_count: Set(identity.attempt_count),
        claim_count: Set(identity.claim_count),
        payload_kind: Set(agent_run_result_payload_kind(&payload).into()),
        payload_json: Set(agent_run_result_payload_json(&payload)?),
        text: Set(text),
        submitted_at: Set(now),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    entities::agent_run_inbox::ActiveModel {
        child_run_id: Set(run.id),
        parent_run_id: Set(parent_id),
        chat_id: Set(run.chat_id),
        parent_depth: Set(0),
        result_lease_token: Set(identity.lease_token),
        result_attempt_count: Set(identity.attempt_count),
        result_claim_count: Set(identity.claim_count),
        status: Set(inbox_status.as_str().into()),
        claim_count: Set(0),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        consumed_lease_token: Set(None),
        consumed_at: Set(None),
        delivered_at: Set(now),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    // A cancelled container run owes its container a teardown in the same
    // transaction — this is what lets a parent's cancellation cascade over an
    // unattached container child without leaking the container.
    if run.execution_location == crate::AgentRunExecutionLocation::Container.as_str() {
        super::super::sandbox_provision::enqueue_teardown_on(conn, run.id).await?;
    }
    Ok(true)
}

async fn cancellation_identity_on<C>(
    conn: &C,
    run: &entities::agent_run::Model,
    now: chrono::DateTime<Utc>,
) -> Result<CancellationIdentity>
where
    C: sea_orm::ConnectionTrait,
{
    if let Some(lease_token) = run.lease_token {
        if run.attempt_count < 1 || run.claim_count < run.attempt_count {
            return Err(AgentError::Store(format!(
                "leased sandbox run {} has invalid provenance",
                run.id
            )));
        }
        let claim = entities::agent_run_claim::Entity::find_by_id(lease_token)
            .one(conn)
            .await
            .map_err(store_err)?;
        if !claim.is_some_and(|claim| {
            claim.agent_run_id == Some(run.id)
                && claim.attempt_count == Some(run.attempt_count)
                && claim.claim_count == Some(run.claim_count)
        }) {
            return Err(AgentError::Store(format!(
                "sandbox run {} is missing its exact claim receipt",
                run.id
            )));
        }
        return Ok(CancellationIdentity {
            lease_token,
            attempt_count: run.attempt_count,
            claim_count: run.claim_count,
        });
    }
    if run.attempt_count >= 1 && run.claim_count >= run.attempt_count {
        if let Some(claim) = entities::agent_run_claim::Entity::find()
            .filter(entities::agent_run_claim::Column::AgentRunId.eq(Some(run.id)))
            .filter(entities::agent_run_claim::Column::AttemptCount.eq(Some(run.attempt_count)))
            .filter(entities::agent_run_claim::Column::ClaimCount.eq(Some(run.claim_count)))
            .order_by_desc(entities::agent_run_claim::Column::ClaimedAt)
            .one(conn)
            .await
            .map_err(store_err)?
        {
            return Ok(CancellationIdentity {
                lease_token: claim.token,
                attempt_count: run.attempt_count,
                claim_count: run.claim_count,
            });
        }
    }
    if run.attempt_count != 0 || run.claim_count != 0 {
        return Err(AgentError::Store(format!(
            "unleased sandbox run {} is missing its claim provenance",
            run.id
        )));
    }
    let lease_token = uuid::Uuid::new_v4();
    entities::agent_run_claim::ActiveModel {
        token: Set(lease_token),
        agent_run_id: Set(Some(run.id)),
        attempt_count: Set(Some(1)),
        claim_count: Set(Some(UNCLAIMED_CANCELLATION_CLAIM_COUNT)),
        claimed_at: Set(now),
        lease_expires_at: Set(Some(now + chrono::Duration::seconds(1))),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    Ok(CancellationIdentity {
        lease_token,
        attempt_count: 1,
        claim_count: UNCLAIMED_CANCELLATION_CLAIM_COUNT,
    })
}

async fn insert_cancellation_request_on<C>(
    conn: &C,
    run: &entities::agent_run::Model,
    identity: CancellationIdentity,
    reason: AgentRunCancellationReason,
    now: chrono::DateTime<Utc>,
) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    entities::agent_run_cancellation::ActiveModel {
        agent_run_id: Set(run.id),
        lease_token: Set(identity.lease_token),
        attempt_count: Set(identity.attempt_count),
        claim_count: Set(identity.claim_count),
        reason: Set(reason.as_str().into()),
        requested_at: Set(now),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

async fn cancellation_context_on<C>(
    conn: &C,
    run: &entities::agent_run::Model,
) -> Result<(AgentRunCancellationReason, AgentRunInboxStatus)>
where
    C: sea_orm::ConnectionTrait,
{
    let admission = entities::sandbox_agent_admission::Entity::find_by_id(run.id)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("sandbox run {} is missing admission", run.id)))?;
    let turn = entities::turn_run::Entity::find_by_id(admission.origin_turn_id)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "sandbox admission {} is missing its origin turn",
                run.id
            ))
        })?;
    if turn.agent_run_id != admission.parent_run_id || turn.chat_id != admission.chat_id {
        return Err(AgentError::Store(format!(
            "sandbox admission {} does not match its origin turn",
            run.id
        )));
    }
    let parent_cancelled = matches!(
        turn.status.as_str(),
        "cancelling" | "cancelling_client" | "cancelled"
    );
    let parent_failed = turn.status == "failed";
    let parent_usable = !matches!(
        turn.status.as_str(),
        "cancelling" | "cancelling_client" | "cancelled" | "completed" | "failed"
    );
    Ok((
        if parent_cancelled {
            AgentRunCancellationReason::ParentTurnCancelled
        } else if parent_failed {
            AgentRunCancellationReason::ParentTurnFailed
        } else {
            AgentRunCancellationReason::Requested
        },
        if parent_usable {
            AgentRunInboxStatus::Pending
        } else {
            AgentRunInboxStatus::Cancelled
        },
    ))
}

async fn retire_inbox_on<C>(
    conn: &C,
    parent_id: uuid::Uuid,
    child: &entities::agent_run::Model,
) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    let child_id = AgentRunId(child.id);
    let inbox = load_agent_run_inbox_by_ids_on(conn, AgentRunId(parent_id), child_id)
        .await?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "terminal sandbox child {child_id} is missing its inbox delivery"
            ))
        })?;
    let result_claim = entities::agent_run_claim::Entity::find_by_id(inbox.result.lease_token)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "terminal sandbox child {child_id} result is missing claim provenance"
            ))
        })?;
    if inbox.parent_run_id != AgentRunId(parent_id)
        || inbox.child_run_id != child_id
        || inbox.chat_id != ChatId(child.chat_id)
        || inbox.result.agent_run_id != child_id
        || (child.status != AgentRunStatus::Cancelled.as_str()
            && (inbox.result.attempt_count != child.attempt_count
                || inbox.result.claim_count != child.claim_count
                || result_claim.agent_run_id != Some(child.id)
                || result_claim.attempt_count != Some(child.attempt_count)
                || result_claim.claim_count != Some(child.claim_count)))
    {
        return Err(AgentError::Store(format!(
            "terminal sandbox child {child_id} has mismatched delivery provenance"
        )));
    }
    let status = inbox.status;
    if matches!(
        status,
        AgentRunInboxStatus::Consumed | AgentRunInboxStatus::Cancelled
    ) {
        return Ok(());
    }
    let updated = entities::agent_run_inbox::Entity::update_many()
        .col_expr(
            entities::agent_run_inbox::Column::Status,
            sea_orm::sea_query::Expr::value(AgentRunInboxStatus::Cancelled.as_str()),
        )
        .col_expr(
            entities::agent_run_inbox::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::agent_run_inbox::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .filter(entities::agent_run_inbox::Column::ChildRunId.eq(child.id))
        .filter(entities::agent_run_inbox::Column::ParentRunId.eq(parent_id))
        .filter(entities::agent_run_inbox::Column::Status.eq(status.as_str()))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "terminal sandbox child {child_id} inbox retirement changed {} rows",
            updated.rows_affected
        )));
    }
    Ok(())
}

/// Validate the immutable cancellation receipt, result, inbox, admission, and
/// terminal child as one recoverable transition. `Ok(None)` means an exact
/// worker token did not own this already-committed cancellation.
async fn validate_cancellation_request_on<C>(
    conn: &C,
    run: &entities::agent_run::Model,
    expected_lease_token: Option<uuid::Uuid>,
) -> Result<Option<entities::agent_run_cancellation::Model>>
where
    C: sea_orm::ConnectionTrait,
{
    let receipt = entities::agent_run_cancellation::Entity::find_by_id(run.id)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "cancelling sandbox run {} is missing its request receipt",
                run.id
            ))
        })?;
    if expected_lease_token.is_some_and(|token| token != receipt.lease_token) {
        return Ok(None);
    }
    if AgentRunCancellationReason::parse(&receipt.reason).is_none() {
        return Err(AgentError::Store(format!(
            "sandbox cancellation {} has invalid reason",
            run.id
        )));
    }
    let claim = entities::agent_run_claim::Entity::find_by_id(receipt.lease_token)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "sandbox cancellation {} is missing claim provenance",
                run.id
            ))
        })?;
    let admission = entities::sandbox_agent_admission::Entity::find_by_id(run.id)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "cancelling sandbox run {} is missing admission",
                run.id
            ))
        })?;
    let valid = run.tier == AgentRunTier::Background.as_str()
        && run.status == AgentRunStatus::Cancelling.as_str()
        && run.lease_token == Some(receipt.lease_token)
        && run.attempt_count == receipt.attempt_count
        && run.claim_count == receipt.claim_count
        && claim.agent_run_id == Some(run.id)
        && claim.attempt_count == Some(receipt.attempt_count)
        && claim.claim_count == Some(receipt.claim_count)
        && admission.parent_run_id == run.parent_id.unwrap_or_default()
        && admission.chat_id == run.chat_id;
    if !valid {
        return Err(AgentError::Store(format!(
            "sandbox cancellation request does not match live run {}",
            run.id
        )));
    }
    Ok(Some(receipt))
}

async fn validate_cancellation_delivery_on<C>(
    conn: &C,
    run: &entities::agent_run::Model,
    expected_lease_token: Option<uuid::Uuid>,
) -> Result<Option<entities::agent_run_cancellation::Model>>
where
    C: sea_orm::ConnectionTrait,
{
    let receipt = entities::agent_run_cancellation::Entity::find_by_id(run.id)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "cancelled sandbox run {} is missing its receipt",
                run.id
            ))
        })?;
    if expected_lease_token.is_some_and(|token| token != receipt.lease_token) {
        return Ok(None);
    }
    let reason = AgentRunCancellationReason::parse(&receipt.reason).ok_or_else(|| {
        AgentError::Store(format!(
            "sandbox cancellation {} has invalid reason",
            run.id
        ))
    })?;
    let admission = entities::sandbox_agent_admission::Entity::find_by_id(run.id)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "cancelled sandbox run {} is missing admission",
                run.id
            ))
        })?;
    let result_model = entities::agent_run_result::Entity::find_by_id(run.id)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "cancelled sandbox run {} is missing result",
                run.id
            ))
        })?;
    let result = agent_run_result_from_model(result_model)?;
    let claim = entities::agent_run_claim::Entity::find_by_id(receipt.lease_token)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "sandbox cancellation {} is missing claim provenance",
                run.id
            ))
        })?;
    let origin_turn = entities::turn_run::Entity::find_by_id(admission.origin_turn_id)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "cancelled sandbox run {} is missing origin turn",
                run.id
            ))
        })?;
    let inbox_model = find_agent_run_inbox_on(
        conn,
        AgentRunId(admission.parent_run_id),
        AgentRunId(run.id),
    )
    .await?
    .ok_or_else(|| {
        AgentError::Store(format!(
            "cancelled sandbox run {} is missing inbox delivery",
            run.id
        ))
    })?;
    let inbox = load_agent_run_inbox_entry_on(conn, inbox_model).await?;
    let inbox_lifecycle_valid = match reason {
        AgentRunCancellationReason::ParentTurnCancelled
        | AgentRunCancellationReason::ParentTurnFailed => {
            inbox.status == AgentRunInboxStatus::Cancelled
        }
        // A direct cancellation may be delivered while its origin is live and
        // recovered after that turn later completes, fails, or is cancelled.
        // The immutable delivery remains valid in every internally valid inbox
        // lifecycle; parent cancellation separately retires unconsumed rows.
        AgentRunCancellationReason::Requested => matches!(
            inbox.status,
            AgentRunInboxStatus::Pending
                | AgentRunInboxStatus::Claimed
                | AgentRunInboxStatus::Consumed
                | AgentRunInboxStatus::Cancelled
        ),
    };
    let run_provenance_valid = if run.attempt_count == 0 && run.claim_count == 0 {
        receipt.attempt_count == 1 && receipt.claim_count == UNCLAIMED_CANCELLATION_CLAIM_COUNT
    } else {
        receipt.attempt_count == run.attempt_count && receipt.claim_count == run.claim_count
    };
    let valid = run.status == AgentRunStatus::Cancelled.as_str()
        && run.lease_token.is_none()
        && run.lease_expires_at.is_none()
        && run
            .finished_at
            .is_some_and(|finished| finished >= receipt.requested_at)
        && run_provenance_valid
        && receipt.lease_token == result.lease_token
        && receipt.attempt_count == result.attempt_count
        && receipt.claim_count == result.claim_count
        && receipt.requested_at <= result.submitted_at
        && claim.agent_run_id == Some(run.id)
        && claim.attempt_count == Some(receipt.attempt_count)
        && claim.claim_count == Some(receipt.claim_count)
        && result.payload == AgentRunResultPayload::Cancelled { reason }
        && inbox.result == result
        && inbox.parent_run_id == AgentRunId(admission.parent_run_id)
        && inbox.chat_id == ChatId(admission.chat_id)
        && admission.child_run_id == run.id
        && admission.parent_run_id == run.parent_id.unwrap_or_default()
        && admission.chat_id == run.chat_id
        && origin_turn.agent_run_id == admission.parent_run_id
        && origin_turn.chat_id == admission.chat_id
        && inbox_lifecycle_valid;
    if !valid {
        return Err(AgentError::Store(format!(
            "sandbox cancellation delivery does not match terminal run {}",
            run.id
        )));
    }
    Ok(Some(receipt))
}
