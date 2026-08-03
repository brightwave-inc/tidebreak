//! Global in-flight-work accounting for host quiescence decisions.
//!
//! The embedding host reads this before restarting itself (for example to
//! install an update). The status sets mirror the claim-side definitions of
//! liveness: every non-terminal turn status (the set `find_active_turn_on`
//! guards chat admission with), and every non-terminal background-run status.

use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};

use crate::error::Result;
use crate::model::{AgentRunStatus, AgentRunTier, TurnRunStatus};
use crate::storage::ActiveWorkSnapshot;

use super::super::{entities, store_err, DbStore};

pub(in crate::db) async fn count_active_work(store: &DbStore) -> Result<ActiveWorkSnapshot> {
    let active_turns = entities::turn_run::Entity::find()
        .filter(entities::turn_run::Column::Status.is_in([
            TurnRunStatus::Queued.as_str(),
            TurnRunStatus::Running.as_str(),
            TurnRunStatus::Cancelling.as_str(),
            TurnRunStatus::WaitingForClient.as_str(),
            TurnRunStatus::WaitingForAgentRun.as_str(),
            TurnRunStatus::CancellingClient.as_str(),
            TurnRunStatus::Resuming.as_str(),
            TurnRunStatus::RetryWait.as_str(),
        ]))
        .count(&store.conn)
        .await
        .map_err(store_err)?;

    let live_background_runs = entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::Tier.eq(AgentRunTier::Background.as_str()))
        .filter(entities::agent_run::Column::Status.is_in([
            AgentRunStatus::Queued.as_str(),
            AgentRunStatus::Running.as_str(),
            AgentRunStatus::Cancelling.as_str(),
            AgentRunStatus::Waiting.as_str(),
            AgentRunStatus::RetryWait.as_str(),
        ]))
        .count(&store.conn)
        .await
        .map_err(store_err)?;

    Ok(ActiveWorkSnapshot {
        active_turns,
        live_background_runs,
    })
}
