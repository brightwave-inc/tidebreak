use chrono::Utc;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, TransactionTrait};

use crate::code::{
    ApprovalDecisionKind, CodeApprovalId, CodeApprovalState, CodeEvent, CodeSession, CodeSessionId,
    CodeSessionLifecycle, CodeSubagentStatus, CodeTurnStatus, SequencedCodeEvent,
};
use crate::error::{AgentError, Result};
use crate::{Attention, AttentionSource, OwnerId};

use super::super::super::{entities, store_err, DbStore};
use super::{acquire_code_session_write_lock, append_event_on_locked};

/// One dead-worker recovery committed as a single database transition.
#[derive(Debug)]
pub struct InterruptedSessionRecovery {
    pub session: CodeSession,
    pub events: Vec<SequencedCodeEvent>,
}

/// Settle a dead running worker without exposing a partial recovery state.
///
/// The turn, pending approvals, matching journal events, subagents, lifecycle,
/// and attention all commit together under the session-row write lock. A
/// caller publishes `events` and the resulting session digest only after this
/// function returns.
pub async fn recover_interrupted_session(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    expected_spawn_epoch: i64,
) -> Result<Option<InterruptedSessionRecovery>> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, session_id).await? {
        return Ok(None);
    }
    let Some(row) = entities::code_session::Entity::find_by_id(session_id.0)
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    if row.spawn_epoch != expected_spawn_epoch
        || row.lifecycle != CodeSessionLifecycle::Running.as_str()
    {
        return Ok(None);
    }
    let mut session = super::session::session_from_row(row)?;
    let now = Utc::now();
    let running_turns = entities::code_turn::Entity::find()
        .filter(entities::code_turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_turn::Column::SessionId.eq(session_id.0))
        .filter(entities::code_turn::Column::Status.eq(CodeTurnStatus::Running.as_str()))
        .order_by_desc(entities::code_turn::Column::Ordinal)
        .limit(2)
        .all(&transaction)
        .await
        .map_err(store_err)?;
    if running_turns.len() > 1 {
        return Err(AgentError::Store(format!(
            "code session {session_id} has more than one running turn during recovery"
        )));
    }

    let approvals = entities::code_approval::Entity::find()
        .filter(entities::code_approval::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_approval::Column::SessionId.eq(session_id.0))
        .filter(entities::code_approval::Column::State.eq(CodeApprovalState::Pending.as_str()))
        .order_by_asc(entities::code_approval::Column::RequestedAt)
        .all(&transaction)
        .await
        .map_err(store_err)?;

    if let Some(turn) = running_turns.first() {
        let updated = entities::code_turn::Entity::update_many()
            .col_expr(
                entities::code_turn::Column::Status,
                sea_orm::sea_query::Expr::value(CodeTurnStatus::Interrupted.as_str()),
            )
            .col_expr(
                entities::code_turn::Column::EndedAt,
                sea_orm::sea_query::Expr::value(Some(now)),
            )
            .filter(entities::code_turn::Column::Id.eq(turn.id))
            .filter(entities::code_turn::Column::Owner.eq(owner.as_str()))
            .filter(entities::code_turn::Column::Status.eq(CodeTurnStatus::Running.as_str()))
            .exec(&transaction)
            .await
            .map_err(store_err)?;
        if updated.rows_affected != 1 {
            return Err(AgentError::Store(format!(
                "running turn {} changed during recovery",
                turn.id
            )));
        }
    }

    if !approvals.is_empty() {
        let updated = entities::code_approval::Entity::update_many()
            .col_expr(
                entities::code_approval::Column::State,
                sea_orm::sea_query::Expr::value(CodeApprovalState::Abandoned.as_str()),
            )
            .col_expr(
                entities::code_approval::Column::DecidedAt,
                sea_orm::sea_query::Expr::value(Some(now)),
            )
            .col_expr(
                entities::code_approval::Column::DecisionClaim,
                sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
            )
            .col_expr(
                entities::code_approval::Column::ClaimedAt,
                sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
            )
            .filter(entities::code_approval::Column::Owner.eq(owner.as_str()))
            .filter(entities::code_approval::Column::SessionId.eq(session_id.0))
            .filter(entities::code_approval::Column::State.eq(CodeApprovalState::Pending.as_str()))
            .exec(&transaction)
            .await
            .map_err(store_err)?;
        let expected = u64::try_from(approvals.len()).map_err(|_| {
            AgentError::Store(format!(
                "pending approval count overflow during recovery for session {session_id}"
            ))
        })?;
        if updated.rows_affected != expected {
            return Err(AgentError::Store(format!(
                "pending approvals changed during recovery for session {session_id}"
            )));
        }
    }

    for subagent in &mut session.subagents {
        if subagent.status == CodeSubagentStatus::Running {
            subagent.status = CodeSubagentStatus::Failed;
        }
    }
    session.lifecycle = CodeSessionLifecycle::Idle;
    session.child_pid = None;
    session.fence_reason = None;
    let recovered_attention = Attention::needs_you(
        "session recovered after the engine process exited",
        AttentionSource::Lifecycle,
    );
    if crate::attention::should_replace(&session.attention, &recovered_attention) {
        session.attention = recovered_attention;
    }
    let updated = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::Lifecycle,
            sea_orm::sea_query::Expr::value(session.lifecycle.as_str()),
        )
        .col_expr(
            entities::code_session::Column::ChildPid,
            sea_orm::sea_query::Expr::value(Option::<i64>::None),
        )
        .col_expr(
            entities::code_session::Column::FenceReason,
            sea_orm::sea_query::Expr::value(Option::<serde_json::Value>::None),
        )
        .col_expr(
            entities::code_session::Column::AttentionState,
            sea_orm::sea_query::Expr::value(serde_json::to_value(&session.attention.state)?),
        )
        .col_expr(
            entities::code_session::Column::AttentionSource,
            sea_orm::sea_query::Expr::value(session.attention.source.as_str()),
        )
        .col_expr(
            entities::code_session::Column::Subagents,
            sea_orm::sea_query::Expr::value(if session.subagents.is_empty() {
                None
            } else {
                Some(serde_json::to_value(&session.subagents)?)
            }),
        )
        .filter(entities::code_session::Column::Id.eq(session_id.0))
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_session::Column::SpawnEpoch.eq(expected_spawn_epoch))
        .filter(
            entities::code_session::Column::Lifecycle.eq(CodeSessionLifecycle::Running.as_str()),
        )
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "code session {session_id} changed during recovery"
        )));
    }

    let turn_event_count = if running_turns.is_empty() { 0 } else { 1 };
    let mut events = Vec::with_capacity(turn_event_count + approvals.len());
    if !running_turns.is_empty() {
        let event = CodeEvent::TurnInterrupted;
        let seq = append_event_on_locked(&transaction, owner, session_id, &event).await?;
        events.push(SequencedCodeEvent { seq, event });
    }
    for approval in approvals {
        let event = CodeEvent::ApprovalResolved {
            approval_id: CodeApprovalId(approval.id),
            decision: ApprovalDecisionKind::Abandoned,
        };
        let seq = append_event_on_locked(&transaction, owner, session_id, &event).await?;
        events.push(SequencedCodeEvent { seq, event });
    }

    transaction.commit().await.map_err(store_err)?;
    Ok(Some(InterruptedSessionRecovery { session, events }))
}
