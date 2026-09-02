//! Durable state for the memory maintenance sweep.
//!
//! The sweep is try-based (decisions 50 and 60): it reads its work list from
//! the record rows every pass, so the only state worth persisting is what a
//! restart cannot recompute — the per-scope fingerprint of the last completed
//! consolidation try, the proposal that try stored, the time of the last
//! utility-model step, and the last completed pass per owner.

use chrono::{DateTime, Utc};
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set};

use crate::error::{AgentError, Result};
use crate::memory::{
    MemoryRecordId, MemoryScope, MemorySweepOutcome, MemorySweepRun, MemorySweepScopeState,
};
use crate::model::TurnRunStatus;
use crate::{OwnerId, RepoId};

use super::super::{entities, store_err, DbStore};
use super::conversation::internal_sessions;

fn scope_ref(scope: MemoryScope) -> String {
    scope
        .repo_id()
        .map_or_else(String::new, |repo_id| repo_id.to_string())
}

fn parse_scope(kind: &str, reference: &str) -> Result<MemoryScope> {
    match kind {
        "personal" => Ok(MemoryScope::Personal),
        "repo" => Ok(MemoryScope::Repo {
            repo_id: RepoId(
                reference
                    .parse()
                    .map_err(|_| AgentError::Store(format!("memory sweep repo {reference:?}")))?,
            ),
        }),
        other => Err(AgentError::Store(format!("memory sweep scope {other:?}"))),
    }
}

/// Every owner holding at least one memory record, in stable order.
///
/// A system path for the sweep driver; nothing reachable from a route may
/// call it.
pub async fn list_memory_owners_all(store: &DbStore) -> Result<Vec<OwnerId>> {
    entities::memory_record::Entity::find()
        .select_only()
        .column(entities::memory_record::Column::Owner)
        .distinct()
        .order_by_asc(entities::memory_record::Column::Owner)
        .into_tuple::<String>()
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .iter()
        .map(|owner| OwnerId::new(owner))
        .collect()
}

/// Every per-scope sweep row one owner holds.
pub async fn list_sweep_scope_states(
    store: &DbStore,
    owner: &OwnerId,
) -> Result<Vec<MemorySweepScopeState>> {
    entities::memory_sweep_scope::Entity::find()
        .filter(entities::memory_sweep_scope::Column::Owner.eq(owner.as_str()))
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|row| {
            Ok(MemorySweepScopeState {
                scope: parse_scope(&row.scope_kind, &row.scope_ref)?,
                fingerprint: row.fingerprint,
                proposal_id: row.proposal_id.map(MemoryRecordId),
                last_model_step_at: row.last_model_step_at,
            })
        })
        .collect()
}

/// Upsert one scope's sweep state after a completed try.
pub async fn save_sweep_scope_state(
    store: &DbStore,
    owner: &OwnerId,
    state: &MemorySweepScopeState,
    now: DateTime<Utc>,
) -> Result<()> {
    entities::memory_sweep_scope::Entity::insert(entities::memory_sweep_scope::ActiveModel {
        owner: Set(owner.as_str().to_owned()),
        scope_kind: Set(state.scope.kind_str().to_owned()),
        scope_ref: Set(scope_ref(state.scope)),
        fingerprint: Set(state.fingerprint.clone()),
        proposal_id: Set(state.proposal_id.map(|id| id.0)),
        last_model_step_at: Set(state.last_model_step_at),
        created_at: Set(now),
        updated_at: Set(now),
    })
    .on_conflict(
        OnConflict::columns([
            entities::memory_sweep_scope::Column::Owner,
            entities::memory_sweep_scope::Column::ScopeKind,
            entities::memory_sweep_scope::Column::ScopeRef,
        ])
        .update_columns([
            entities::memory_sweep_scope::Column::Fingerprint,
            entities::memory_sweep_scope::Column::ProposalId,
            entities::memory_sweep_scope::Column::LastModelStepAt,
            entities::memory_sweep_scope::Column::UpdatedAt,
        ])
        .to_owned(),
    )
    .exec_without_returning(&store.conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

/// Upsert the owner's last completed pass.
pub async fn record_sweep_run(
    store: &DbStore,
    owner: &OwnerId,
    run: &MemorySweepRun,
) -> Result<()> {
    entities::memory_sweep_run::Entity::insert(entities::memory_sweep_run::ActiveModel {
        owner: Set(owner.as_str().to_owned()),
        ran_at: Set(run.ran_at),
        scope_kind: Set(run.scope.map(|scope| scope.kind_str().to_owned())),
        scope_ref: Set(run.scope.map(scope_ref)),
        outcome: Set(run.outcome.as_str().to_owned()),
        expired: Set(i64::from(run.expired)),
        proposed: Set(i64::from(run.proposed)),
        created_at: Set(run.ran_at),
        updated_at: Set(run.ran_at),
    })
    .on_conflict(
        OnConflict::column(entities::memory_sweep_run::Column::Owner)
            .update_columns([
                entities::memory_sweep_run::Column::RanAt,
                entities::memory_sweep_run::Column::ScopeKind,
                entities::memory_sweep_run::Column::ScopeRef,
                entities::memory_sweep_run::Column::Outcome,
                entities::memory_sweep_run::Column::Expired,
                entities::memory_sweep_run::Column::Proposed,
                entities::memory_sweep_run::Column::UpdatedAt,
            ])
            .to_owned(),
    )
    .exec_without_returning(&store.conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

/// The owner's last completed pass, or `None` before the first.
pub async fn latest_sweep_run(store: &DbStore, owner: &OwnerId) -> Result<Option<MemorySweepRun>> {
    let Some(row) = entities::memory_sweep_run::Entity::find_by_id(owner.as_str())
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    let scope = match (row.scope_kind.as_deref(), row.scope_ref.as_deref()) {
        (Some(kind), Some(reference)) => Some(parse_scope(kind, reference)?),
        _ => None,
    };
    Ok(Some(MemorySweepRun {
        ran_at: row.ran_at,
        scope,
        outcome: MemorySweepOutcome::parse(&row.outcome)
            .map_err(|error| AgentError::Store(error.to_string()))?,
        expired: u32::try_from(row.expired)
            .map_err(|_| AgentError::Store("memory sweep expired count".to_owned()))?,
        proposed: u32::try_from(row.proposed)
            .map_err(|_| AgentError::Store("memory sweep proposed count".to_owned()))?,
    }))
}

/// Whether the owner has a live work-mode turn.
///
/// Reads [`TurnRunStatus::LIVE`] — the one definition of "busy" — joined to
/// the owner's internal-engine sessions, because `turn_run` carries no owner
/// column of its own.
pub async fn owner_has_live_chat_turn(store: &DbStore, owner: &OwnerId) -> Result<bool> {
    let live = entities::turn_run::Entity::find()
        .filter(
            entities::turn_run::Column::Status
                .is_in(TurnRunStatus::LIVE.iter().map(|status| status.as_str())),
        )
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    if live.is_empty() {
        return Ok(false);
    }
    let owned = entities::code_session::Entity::find()
        .select_only()
        .column(entities::code_session::Column::Id)
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .filter(internal_sessions())
        .into_tuple::<uuid::Uuid>()
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    Ok(live.iter().any(|run| owned.contains(&run.chat_id)))
}
