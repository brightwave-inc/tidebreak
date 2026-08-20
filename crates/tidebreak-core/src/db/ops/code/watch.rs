//! Persistence for durable watch tasks.

use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};

use crate::code::{CodeSessionId, CodeWatch, CodeWatchId, CodeWatchState, WorkspaceId};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};

/// Insert a watch row. The row belongs to `watch.owner`, denormalized from
/// the workspace it watches.
pub async fn insert_watch(store: &DbStore, watch: &CodeWatch) -> Result<()> {
    entities::code_watch::ActiveModel {
        id: Set(watch.id.0),
        owner: Set(watch.owner.as_str().to_owned()),
        workspace_id: Set(watch.workspace_id.0),
        session_id: Set(watch.session_id.0),
        pr_number: Set(i64::try_from(watch.pr_number)
            .map_err(|_| AgentError::Store(format!("code_watch {} pr number", watch.id)))?),
        state: Set(watch.state.as_str().to_owned()),
        detail: Set(watch.detail.clone()),
        last_fix_head: Set(watch.last_fix_head.clone()),
        cycles: Set(watch.cycles),
        created_at: Set(watch.created_at),
        updated_at: Set(watch.updated_at),
    }
    .insert(&store.conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

/// The owner's most recent watch on one workspace, terminal or not.
pub async fn latest_watch_for_workspace(
    store: &DbStore,
    owner: &OwnerId,
    workspace_id: WorkspaceId,
) -> Result<Option<CodeWatch>> {
    let row = entities::code_watch::Entity::find()
        .filter(entities::code_watch::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_watch::Column::WorkspaceId.eq(workspace_id.0))
        .order_by_desc(entities::code_watch::Column::CreatedAt)
        .one(&store.conn)
        .await
        .map_err(store_err)?;
    row.map(watch_from_row).transpose()
}

/// Every non-terminal watch on the machine.
///
/// A system path, not a request path: the watch sweep drives every owner's
/// watches, the way boot recovery re-attaches every owner's sessions.
/// Nothing reachable from a route may call it.
pub async fn list_active_watches_all_owners(store: &DbStore) -> Result<Vec<CodeWatch>> {
    entities::code_watch::Entity::find()
        .filter(
            entities::code_watch::Column::State
                .is_in(["watching", "fixing", "blocked"].map(str::to_owned)),
        )
        .order_by_asc(entities::code_watch::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(watch_from_row)
        .collect()
}

/// Persist mutable watch fields. Identity and `created_at` stay as stored,
/// and a terminal state is never overwritten. Returns `false` when nothing
/// was written.
pub async fn save_watch(store: &DbStore, watch: &CodeWatch) -> Result<bool> {
    let result = entities::code_watch::Entity::update_many()
        .col_expr(
            entities::code_watch::Column::State,
            sea_orm::sea_query::Expr::value(watch.state.as_str().to_owned()),
        )
        .col_expr(
            entities::code_watch::Column::Detail,
            sea_orm::sea_query::Expr::value(watch.detail.clone()),
        )
        .col_expr(
            entities::code_watch::Column::LastFixHead,
            sea_orm::sea_query::Expr::value(watch.last_fix_head.clone()),
        )
        .col_expr(
            entities::code_watch::Column::Cycles,
            sea_orm::sea_query::Expr::value(watch.cycles),
        )
        .col_expr(
            entities::code_watch::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(watch.updated_at),
        )
        .filter(entities::code_watch::Column::Id.eq(watch.id.0))
        .filter(entities::code_watch::Column::Owner.eq(watch.owner.as_str()))
        .filter(
            entities::code_watch::Column::State
                .is_in(["watching", "fixing", "blocked"].map(str::to_owned)),
        )
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

fn watch_from_row(row: entities::code_watch::Model) -> Result<CodeWatch> {
    let state = CodeWatchState::from_str(&row.state).ok_or_else(|| {
        AgentError::Store(format!(
            "code_watch {} has unknown state {}",
            row.id, row.state
        ))
    })?;
    let pr_number = u64::try_from(row.pr_number)
        .map_err(|_| AgentError::Store(format!("code_watch {} pr number", row.id)))?;
    Ok(CodeWatch {
        id: CodeWatchId(row.id),
        owner: OwnerId::new(&row.owner)?,
        workspace_id: WorkspaceId(row.workspace_id),
        session_id: CodeSessionId(row.session_id),
        pr_number,
        state,
        detail: row.detail,
        last_fix_head: row.last_fix_head,
        cycles: row.cycles,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}
