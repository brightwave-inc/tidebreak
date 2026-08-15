use sea_orm::{ActiveModelTrait, EntityTrait, Set};

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
