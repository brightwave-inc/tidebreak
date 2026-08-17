use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};

use crate::code::{CodeWorkspace, CodeWorkspaceStatus, PullRequestDigest, RepoId, WorkspaceId};
use crate::error::{AgentError, Result};

use super::super::super::{entities, store_err, DbStore};

/// Insert a workspace.
pub async fn insert_workspace(store: &DbStore, workspace: &CodeWorkspace) -> Result<()> {
    entities::code_workspace::ActiveModel {
        id: Set(workspace.id.0),
        repo_id: Set(workspace.repo_id.0),
        title: Set(workspace.title.clone()),
        worktree_path: Set(workspace.worktree_path.clone()),
        branch_name: Set(workspace.branch_name.clone()),
        base_ref: Set(workspace.base_ref.clone()),
        status: Set(workspace.status.as_str().to_owned()),
        pr: Set(match &workspace.pr {
            Some(pr) => Some(serde_json::to_value(pr)?),
            None => None,
        }),
        created_at: Set(workspace.created_at),
        archived_at: Set(workspace.archived_at),
    }
    .insert(&store.conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

/// Load a workspace by id.
pub async fn get_workspace(store: &DbStore, id: WorkspaceId) -> Result<Option<CodeWorkspace>> {
    let Some(row) = entities::code_workspace::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    Ok(Some(workspace_from_row(row)?))
}

/// Workspaces of one repo, or every workspace when `repo_id` is `None`.
/// Most recently created first.
pub async fn list_workspaces(
    store: &DbStore,
    repo_id: Option<RepoId>,
) -> Result<Vec<CodeWorkspace>> {
    let mut query = entities::code_workspace::Entity::find();
    if let Some(repo_id) = repo_id {
        query = query.filter(entities::code_workspace::Column::RepoId.eq(repo_id.0));
    }
    query
        .order_by_desc(entities::code_workspace::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(workspace_from_row)
        .collect()
}

/// Persist mutable workspace fields. `id`, `repo_id`, and `created_at` stay as stored.
pub async fn save_workspace(store: &DbStore, workspace: &CodeWorkspace) -> Result<bool> {
    let result = entities::code_workspace::Entity::update_many()
        .col_expr(
            entities::code_workspace::Column::Title,
            sea_orm::sea_query::Expr::value(workspace.title.clone()),
        )
        .col_expr(
            entities::code_workspace::Column::WorktreePath,
            sea_orm::sea_query::Expr::value(workspace.worktree_path.clone()),
        )
        .col_expr(
            entities::code_workspace::Column::BranchName,
            sea_orm::sea_query::Expr::value(workspace.branch_name.clone()),
        )
        .col_expr(
            entities::code_workspace::Column::BaseRef,
            sea_orm::sea_query::Expr::value(workspace.base_ref.clone()),
        )
        .col_expr(
            entities::code_workspace::Column::Status,
            sea_orm::sea_query::Expr::value(workspace.status.as_str().to_owned()),
        )
        .col_expr(
            entities::code_workspace::Column::Pr,
            sea_orm::sea_query::Expr::value(match &workspace.pr {
                Some(pr) => Some(serde_json::to_value(pr)?),
                None => None,
            }),
        )
        .col_expr(
            entities::code_workspace::Column::ArchivedAt,
            sea_orm::sea_query::Expr::value(workspace.archived_at),
        )
        .filter(entities::code_workspace::Column::Id.eq(workspace.id.0))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Set a workspace's title only while it still reads `expected`.
///
/// The compare half is what lets background naming lose to a rename: a derived
/// title replaces the generated placeholder it was derived against, and nothing
/// else, no matter when the write lands. Returns whether the row changed.
pub async fn set_workspace_title_if(
    store: &DbStore,
    id: WorkspaceId,
    expected: &str,
    title: &str,
) -> Result<bool> {
    let result = entities::code_workspace::Entity::update_many()
        .col_expr(
            entities::code_workspace::Column::Title,
            sea_orm::sea_query::Expr::value(title.to_owned()),
        )
        .filter(entities::code_workspace::Column::Id.eq(id.0))
        .filter(entities::code_workspace::Column::Title.eq(expected))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Delete a workspace row. Used to roll back a failed create that left no checkout.
pub async fn delete_workspace(store: &DbStore, id: WorkspaceId) -> Result<bool> {
    let result = entities::code_workspace::Entity::delete_by_id(id.0)
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

pub(super) fn workspace_from_row(row: entities::code_workspace::Model) -> Result<CodeWorkspace> {
    let status = CodeWorkspaceStatus::from_str(&row.status).ok_or_else(|| {
        AgentError::Store(format!(
            "code_workspace {} has unknown status {}",
            row.id, row.status
        ))
    })?;
    let pr = match row.pr {
        Some(value) => Some(
            serde_json::from_value::<PullRequestDigest>(value)
                .map_err(|err| AgentError::Store(format!("code_workspace {} pr: {err}", row.id)))?,
        ),
        None => None,
    };
    Ok(CodeWorkspace {
        id: WorkspaceId(row.id),
        repo_id: RepoId(row.repo_id),
        title: row.title,
        worktree_path: row.worktree_path,
        branch_name: row.branch_name,
        base_ref: row.base_ref,
        status,
        pr,
        created_at: row.created_at,
        archived_at: row.archived_at,
    })
}
