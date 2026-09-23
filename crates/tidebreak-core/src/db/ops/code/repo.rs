use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, Set, TransactionTrait,
};

use crate::code::{CodeRepo, QuickAction, RepoId};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};

/// Insert a registered repository. The row belongs to `repo.owner`.
pub async fn insert_repo(store: &DbStore, repo: &CodeRepo) -> Result<()> {
    insert_repo_on(&store.conn, repo).await
}

async fn insert_repo_on<C: ConnectionTrait>(conn: &C, repo: &CodeRepo) -> Result<()> {
    entities::code_repo::ActiveModel {
        id: Set(repo.id.0),
        owner: Set(repo.owner.as_str().to_owned()),
        root_path: Set(repo.root_path.clone()),
        display_name: Set(repo.display_name.clone()),
        default_base_ref: Set(repo.default_base_ref.clone()),
        branch_prefix: Set(repo.branch_prefix.clone()),
        setup_script: Set(repo.setup_script.clone()),
        archive_script: Set(repo.archive_script.clone()),
        quick_actions: Set(serde_json::to_value(&repo.quick_actions)?),
        created_at: Set(repo.created_at),
        removed_at: Set(repo.removed_at),
        cloned_from: Set(repo.cloned_from.clone()),
        origin_host: Set(repo.origin_host.clone()),
        origin_owner: Set(repo.origin_owner.clone()),
        origin_name: Set(repo.origin_name.clone()),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

/// Write the repositories one configuration import brings in, in one
/// transaction: insert every row in `added`, and save the settings of every
/// row in `replaced`. Either all of them land or none do.
///
/// An import is one person's request, so every row must be `owner`'s: one
/// that is not refuses the whole import before anything is written, and a
/// replaced row is matched on `owner` as well as its id, so another owner's
/// registration reads as missing.
pub async fn import_repos(
    store: &DbStore,
    owner: &OwnerId,
    added: &[CodeRepo],
    replaced: &[CodeRepo],
) -> Result<()> {
    refuse_other_owners(owner, added.iter().chain(replaced))?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    for repo in added {
        insert_repo_on(&transaction, repo).await?;
    }
    for repo in replaced {
        if !save_repo_on(&transaction, owner, repo).await? {
            return Err(AgentError::Store(format!(
                "repository {} is no longer registered",
                repo.display_name
            )));
        }
    }
    transaction.commit().await.map_err(store_err)
}

/// Refuse a batch that holds a row someone other than `owner` owns.
fn refuse_other_owners<'a>(
    owner: &OwnerId,
    mut repos: impl Iterator<Item = &'a CodeRepo>,
) -> Result<()> {
    match repos.find(|repo| repo.owner != *owner) {
        Some(repo) => Err(AgentError::Store(format!(
            "repository {} belongs to another owner",
            repo.display_name
        ))),
        None => Ok(()),
    }
}

/// Undo [`import_repos`] in one transaction: remove the rows it added and
/// put back the settings `previous` holds for the rows it replaced.
///
/// A row the import added is deleted outright, since nothing hangs off a
/// registration that is seconds old. One that already has a workspace is
/// marked removed instead, the way removing a repository always leaves it.
pub async fn revert_repo_import(
    store: &DbStore,
    owner: &OwnerId,
    added: &[RepoId],
    previous: &[CodeRepo],
) -> Result<()> {
    refuse_other_owners(owner, previous.iter())?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    for id in added {
        let in_use = entities::code_workspace::Entity::find()
            .filter(entities::code_workspace::Column::RepoId.eq(id.0))
            .filter(entities::code_workspace::Column::Owner.eq(owner.as_str()))
            .count(&transaction)
            .await
            .map_err(store_err)?
            > 0;
        if in_use {
            entities::code_repo::Entity::update_many()
                .col_expr(
                    entities::code_repo::Column::RemovedAt,
                    sea_orm::sea_query::Expr::value(chrono::Utc::now()),
                )
                .filter(entities::code_repo::Column::Id.eq(id.0))
                .filter(entities::code_repo::Column::Owner.eq(owner.as_str()))
                .exec(&transaction)
                .await
                .map_err(store_err)?;
        } else {
            entities::code_repo::Entity::delete_many()
                .filter(entities::code_repo::Column::Id.eq(id.0))
                .filter(entities::code_repo::Column::Owner.eq(owner.as_str()))
                .exec(&transaction)
                .await
                .map_err(store_err)?;
        }
    }
    for repo in previous {
        save_repo_on(&transaction, owner, repo).await?;
    }
    transaction.commit().await.map_err(store_err)
}

/// Load one of the owner's repositories by id.
///
/// Another owner's repository is indistinguishable from a missing one.
pub async fn get_repo(store: &DbStore, owner: &OwnerId, id: RepoId) -> Result<Option<CodeRepo>> {
    let Some(row) = entities::code_repo::Entity::find_by_id(id.0)
        .filter(entities::code_repo::Column::Owner.eq(owner.as_str()))
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    Ok(Some(repo_from_row(row)?))
}

/// Load one of the owner's live repositories by its canonical toplevel path.
pub async fn get_repo_by_root_path(
    store: &DbStore,
    owner: &OwnerId,
    root_path: &str,
) -> Result<Option<CodeRepo>> {
    let Some(row) = entities::code_repo::Entity::find()
        .filter(entities::code_repo::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_repo::Column::RootPath.eq(root_path))
        .filter(entities::code_repo::Column::RemovedAt.is_null())
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    Ok(Some(repo_from_row(row)?))
}

/// The owner's registered repositories, most recently created first.
pub async fn list_repos(store: &DbStore, owner: &OwnerId) -> Result<Vec<CodeRepo>> {
    entities::code_repo::Entity::find()
        .filter(entities::code_repo::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_repo::Column::RemovedAt.is_null())
        .order_by_desc(entities::code_repo::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(repo_from_row)
        .collect()
}

/// Every registered repository on the machine, most recently created first.
///
/// A system path, not a request path: background reconciliation reads every
/// repository regardless of who owns it. Nothing reachable from a route may
/// call it.
pub async fn list_repos_all_owners(store: &DbStore) -> Result<Vec<CodeRepo>> {
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
    save_repo_on(&store.conn, &repo.owner, repo).await
}

/// Save `repo`'s settings onto the row with its id that `owner` owns.
async fn save_repo_on<C: ConnectionTrait>(
    conn: &C,
    owner: &OwnerId,
    repo: &CodeRepo,
) -> Result<bool> {
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
        .filter(entities::code_repo::Column::Owner.eq(owner.as_str()))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Mark one of the owner's repositories removed, keeping the row.
///
/// The row is what archived workspaces and their transcripts hang off, so a
/// hard delete is not an option: SQLite does not enforce the workspace foreign
/// key and would strand that history unreachable, and PostgreSQL does enforce
/// it and would refuse the delete outright. Removal hides the registration;
/// reclaiming the bytes on disk is a separate, explicit act.
pub async fn mark_repo_removed(
    store: &DbStore,
    owner: &OwnerId,
    id: RepoId,
    removed_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    let result = entities::code_repo::Entity::update_many()
        .col_expr(
            entities::code_repo::Column::RemovedAt,
            sea_orm::sea_query::Expr::value(removed_at),
        )
        .filter(entities::code_repo::Column::Id.eq(id.0))
        .filter(entities::code_repo::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_repo::Column::RemovedAt.is_null())
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Record the GitHub identity parsed from a repository's origin remote.
///
/// Written by the reconcile sweep whenever the resolved identity differs from
/// what is stored, so a retargeted origin refreshes on its next resolve
/// (decision 77).
pub async fn set_repo_origin(
    store: &DbStore,
    owner: &OwnerId,
    id: RepoId,
    host: &str,
    repo_owner: &str,
    repo_name: &str,
) -> Result<bool> {
    let result = entities::code_repo::Entity::update_many()
        .col_expr(
            entities::code_repo::Column::OriginHost,
            sea_orm::sea_query::Expr::value(Some(host.to_owned())),
        )
        .col_expr(
            entities::code_repo::Column::OriginOwner,
            sea_orm::sea_query::Expr::value(Some(repo_owner.to_owned())),
        )
        .col_expr(
            entities::code_repo::Column::OriginName,
            sea_orm::sea_query::Expr::value(Some(repo_name.to_owned())),
        )
        .filter(entities::code_repo::Column::Id.eq(id.0))
        .filter(entities::code_repo::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_repo::Column::RemovedAt.is_null())
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
        owner: OwnerId::new(&row.owner)?,
        root_path: row.root_path,
        display_name: row.display_name,
        default_base_ref: row.default_base_ref,
        branch_prefix: row.branch_prefix,
        setup_script: row.setup_script,
        archive_script: row.archive_script,
        quick_actions,
        created_at: row.created_at,
        removed_at: row.removed_at,
        cloned_from: row.cloned_from,
        origin_host: row.origin_host,
        origin_owner: row.origin_owner,
        origin_name: row.origin_name,
    })
}
