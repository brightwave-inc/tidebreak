use chrono::Utc;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, TransactionTrait};

use crate::code::{
    ApprovalDecisionKind, CapLevel, CodeApprovalId, CodeApprovalState, CodeEvent, CodeSession,
    CodeSessionId, CodeSessionLifecycle, CodeSubagentStatus, CodeTurnStatus, SequencedCodeEvent,
};
use crate::error::{AgentError, Result};
use crate::{Attention, AttentionSource, AttentionState, OwnerId};

use super::super::super::{entities, store_err, DbStore};
use super::{acquire_code_session_write_lock, append_event_on_locked};

/// One dead-worker recovery committed as a single database transition.
#[derive(Debug)]
pub struct InterruptedSessionRecovery {
    pub session: CodeSession,
    pub events: Vec<SequencedCodeEvent>,
}

/// Return whether you keep this turn waiting so a restarted worker can
/// call the same `resume_turn` path a live park resolution uses.
///
/// Only an engine that declares `durable_parks` checkpoints a turn across a
/// process restart. Other engines lose the in-memory wait with the worker,
/// so you still close those turns as interrupted.
#[must_use]
pub fn resumes_parked_turn_after_restart(
    status: CodeTurnStatus,
    park_ref: Option<&str>,
    durable_parks: CapLevel,
) -> bool {
    durable_parks == CapLevel::Supported && status == CodeTurnStatus::Waiting && park_ref.is_some()
}

/// Settle a dead running worker without exposing a partial recovery state.
///
/// The turn, pending approvals, matching journal events, subagents, lifecycle,
/// and attention all commit together under the session-row write lock. A
/// caller publishes `events` and the resulting session digest only after this
/// function returns.
///
/// Pass the engine's `durable_parks` flag: a waiting turn with a park on an
/// engine that declares it stays waiting so you can re-attach and resume.
/// Other engines keep the interrupted close.
pub async fn recover_interrupted_session(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    expected_spawn_epoch: i64,
    durable_parks: CapLevel,
) -> Result<Option<InterruptedSessionRecovery>> {
    settle_interrupted_session(
        store,
        owner,
        session_id,
        expected_spawn_epoch,
        RecoveryExpectation::Running,
        durable_parks,
    )
    .await
}

/// Settle a fenced session after its exact recorded process has exited.
///
/// The expected pid and creation identity keep this transition bound to the
/// process the caller just waited for. The epoch advances in the same
/// transaction after the running turn and pending approvals settle, so no
/// stale worker can write once the fence clears.
pub async fn reap_fenced_session(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    expected_spawn_epoch: i64,
    expected_child_pid: Option<i64>,
    expected_child_process_identity: Option<&str>,
) -> Result<Option<InterruptedSessionRecovery>> {
    settle_interrupted_session(
        store,
        owner,
        session_id,
        expected_spawn_epoch,
        RecoveryExpectation::Fenced {
            child_pid: expected_child_pid,
            child_process_identity: expected_child_process_identity,
        },
        // A fenced live process is not a durable-park restart. Close the
        // open turn the way a reap always has.
        CapLevel::Unsupported,
    )
    .await
}

enum RecoveryExpectation<'a> {
    Running,
    Fenced {
        child_pid: Option<i64>,
        child_process_identity: Option<&'a str>,
    },
}

impl RecoveryExpectation<'_> {
    fn lifecycle(&self) -> CodeSessionLifecycle {
        match self {
            Self::Running => CodeSessionLifecycle::Running,
            Self::Fenced { .. } => CodeSessionLifecycle::Fenced,
        }
    }

    fn advances_epoch(&self) -> bool {
        matches!(self, Self::Fenced { .. })
    }
}

async fn settle_interrupted_session(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    expected_spawn_epoch: i64,
    expectation: RecoveryExpectation<'_>,
    durable_parks: CapLevel,
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
    if row.spawn_epoch != expected_spawn_epoch || row.lifecycle != expectation.lifecycle().as_str()
    {
        return Ok(None);
    }
    if let RecoveryExpectation::Fenced {
        child_pid,
        child_process_identity,
    } = &expectation
    {
        if row.child_pid != *child_pid
            || row.child_process_identity.as_deref() != *child_process_identity
        {
            return Ok(None);
        }
    }
    let mut session = super::session::session_from_row(row)?;
    let now = Utc::now();
    let running_turns = entities::code_turn::Entity::find()
        .filter(entities::code_turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_turn::Column::SessionId.eq(session_id.0))
        // Waiting counts as open. An engine that declares `durable_parks`
        // keeps a checkpoint you can resume; recovery then leaves that turn
        // waiting. Other engines lose the in-memory wait with the worker,
        // so you still close those turns as interrupted.
        .filter(entities::code_turn::Column::Status.is_in([
            CodeTurnStatus::Running.as_str(),
            CodeTurnStatus::Waiting.as_str(),
        ]))
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

    let resume_park = running_turns.first().is_some_and(|turn| {
        resumes_parked_turn_after_restart(
            CodeTurnStatus::from_str(&turn.status).unwrap_or(CodeTurnStatus::Running),
            turn.park_ref.as_deref(),
            durable_parks,
        )
    });

    let approvals = if resume_park {
        Vec::new()
    } else {
        entities::code_approval::Entity::find()
            .filter(entities::code_approval::Column::Owner.eq(owner.as_str()))
            .filter(entities::code_approval::Column::SessionId.eq(session_id.0))
            .filter(entities::code_approval::Column::State.eq(CodeApprovalState::Pending.as_str()))
            .order_by_asc(entities::code_approval::Column::RequestedAt)
            .all(&transaction)
            .await
            .map_err(store_err)?
    };

    if let Some(turn) = running_turns.first() {
        if !resume_park {
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
                .filter(entities::code_turn::Column::Status.is_in([
                    CodeTurnStatus::Running.as_str(),
                    CodeTurnStatus::Waiting.as_str(),
                ]))
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
    session.child_process_identity = None;
    session.fence_reason = None;
    if expectation.advances_epoch() {
        session.spawn_epoch = session.spawn_epoch.checked_add(1).ok_or_else(|| {
            AgentError::Store(format!(
                "code session {session_id} spawn epoch overflow during reap"
            ))
        })?;
    }
    let recovered_attention = if resume_park {
        Attention::needs_you(
            "the turn is waiting after the worker restarted",
            AttentionSource::Lifecycle,
        )
    } else {
        match &expectation {
            RecoveryExpectation::Running => Attention::needs_you(
                "session recovered after the engine process exited",
                AttentionSource::Lifecycle,
            ),
            RecoveryExpectation::Fenced { .. } => {
                Attention::new(AttentionState::Idle, AttentionSource::Lifecycle)
            }
        }
    };
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
            entities::code_session::Column::ChildProcessIdentity,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::code_session::Column::SpawnEpoch,
            sea_orm::sea_query::Expr::value(session.spawn_epoch),
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
        .filter(entities::code_session::Column::Lifecycle.eq(expectation.lifecycle().as_str()))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "code session {session_id} changed during recovery"
        )));
    }

    let turn_event_count = if running_turns.is_empty() || resume_park {
        0
    } else {
        1
    };
    let mut events = Vec::with_capacity(turn_event_count + approvals.len());
    if !running_turns.is_empty() && !resume_park {
        let event = CodeEvent::TurnInterrupted { usage: None };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resumes_parked_turn_after_restart_requires_durable_parks() {
        assert!(resumes_parked_turn_after_restart(
            CodeTurnStatus::Waiting,
            Some("cp-1"),
            CapLevel::Supported,
        ));
        assert!(
            !resumes_parked_turn_after_restart(
                CodeTurnStatus::Waiting,
                Some("cp-1"),
                CapLevel::Unsupported,
            ),
            "engines without durable parks still interrupt"
        );
        assert!(
            !resumes_parked_turn_after_restart(
                CodeTurnStatus::Waiting,
                Some("cp-1"),
                CapLevel::Unknown,
            ),
            "an unverified flag must not resume"
        );
        assert!(!resumes_parked_turn_after_restart(
            CodeTurnStatus::Running,
            Some("cp-1"),
            CapLevel::Supported,
        ));
        assert!(!resumes_parked_turn_after_restart(
            CodeTurnStatus::Waiting,
            None,
            CapLevel::Supported,
        ));
    }
}
