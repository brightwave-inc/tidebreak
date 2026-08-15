use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};

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

/// Load a repository by its canonical toplevel path.
pub async fn get_repo_by_root_path(store: &DbStore, root_path: &str) -> Result<Option<CodeRepo>> {
    let Some(row) = entities::code_repo::Entity::find()
        .filter(entities::code_repo::Column::RootPath.eq(root_path))
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    Ok(Some(repo_from_row(row)?))
}

/// Every registered repository, most recently created first.
pub async fn list_repos(store: &DbStore) -> Result<Vec<CodeRepo>> {
    entities::code_repo::Entity::find()
        .order_by_desc(entities::code_repo::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(repo_from_row)
        .collect()
}

/// Persist mutable repository fields. `id`, `root_path`, and `created_at` stay as stored.
pub async fn save_repo(store: &DbStore, repo: &CodeRepo) -> Result<bool> {
    let result = entities::code_repo::Entity::update_many()
        .col_expr(
            entities::code_repo::Column::DisplayName,
            sea_orm::sea_query::Expr::value(repo.display_name.clone()),
        )
        .col_expr(
            entities::code_repo::Column::DefaultBaseRef,
            sea_orm::sea_query::Expr::value(repo.default_base_ref.clone()),
        )
        .col_expr(
            entities::code_repo::Column::BranchPrefix,
            sea_orm::sea_query::Expr::value(repo.branch_prefix.clone()),
        )
        .col_expr(
            entities::code_repo::Column::SetupScript,
            sea_orm::sea_query::Expr::value(repo.setup_script.clone()),
        )
        .col_expr(
            entities::code_repo::Column::ArchiveScript,
            sea_orm::sea_query::Expr::value(repo.archive_script.clone()),
        )
        .col_expr(
            entities::code_repo::Column::QuickActions,
            sea_orm::sea_query::Expr::value(serde_json::to_value(&repo.quick_actions)?),
        )
        .filter(entities::code_repo::Column::Id.eq(repo.id.0))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Delete a repository. Callers must have removed its workspaces first.
pub async fn delete_repo(store: &DbStore, id: RepoId) -> Result<bool> {
    let result = entities::code_repo::Entity::delete_by_id(id.0)
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
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
