use sea_orm::{ActiveModelTrait, EntityTrait, Set};

use crate::code::{CodeRepo, QuickAction, RepoId};
use crate::error::{AgentError, Result};

use super::super::super::{entities, store_err, DbStore};

/// Insert a registered repository.
pub async fn insert_repo(store: &DbStore, repo: &CodeRepo) -> Result<()> {
    entities::code_repo::ActiveModel {
        id: Set(repo.id.0),
        root_path: Set(repo.root_path.clone()),
        display_name: Set(repo.display_name.clone()),
        default_base_ref: Set(repo.default_base_ref.clone()),
        branch_prefix: Set(repo.branch_prefix.clone()),
        setup_script: Set(repo.setup_script.clone()),
        archive_script: Set(repo.archive_script.clone()),
        quick_actions: Set(serde_json::to_value(&repo.quick_actions)?),
        created_at: Set(repo.created_at),
    }
    .insert(&store.conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

/// Load a repository by id.
pub async fn get_repo(store: &DbStore, id: RepoId) -> Result<Option<CodeRepo>> {
    let Some(row) = entities::code_repo::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    Ok(Some(repo_from_row(row)?))
}

pub(super) fn repo_from_row(row: entities::code_repo::Model) -> Result<CodeRepo> {
    let quick_actions = serde_json::from_value::<Vec<QuickAction>>(row.quick_actions)
        .map_err(|err| AgentError::Store(format!("code_repo {} quick_actions: {err}", row.id)))?;
    Ok(CodeRepo {
        id: RepoId(row.id),
        root_path: row.root_path,
        display_name: row.display_name,
        default_base_ref: row.default_base_ref,
        branch_prefix: row.branch_prefix,
        setup_script: row.setup_script,
        archive_script: row.archive_script,
        quick_actions,
        created_at: row.created_at,
    })
}
