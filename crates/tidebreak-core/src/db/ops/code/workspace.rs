use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};

use crate::code::{CodeWorkspace, CodeWorkspaceStatus, PullRequestDigest, RepoId, WorkspaceId};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};

/// Insert a workspace. The row belongs to `workspace.owner`, denormalized
/// from the repository it was created against.
pub async fn insert_workspace(store: &DbStore, workspace: &CodeWorkspace) -> Result<()> {
    insert_workspace_on(&store.conn, workspace).await
}

/// [`insert_workspace`] against any connection, so a caller can commit the
/// workspace with the rows that depend on it.
pub(in crate::db) async fn insert_workspace_on<C>(
    connection: &C,
    workspace: &CodeWorkspace,
) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    entities::code_workspace::ActiveModel {
        id: Set(workspace.id.0),
        owner: Set(workspace.owner.as_str().to_owned()),
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
        released_at: Set(workspace.released_at),
        released_tip: Set(workspace.released_tip.clone()),
        bundle_bytes: Set(workspace.bundle_bytes),
    }
    .insert(connection)
    .await
    .map_err(store_err)?;
    Ok(())
}

/// Load one of the owner's workspaces by id.
///
/// Another owner's workspace is indistinguishable from a missing one.
pub async fn get_workspace(
    store: &DbStore,
    owner: &OwnerId,
    id: WorkspaceId,
) -> Result<Option<CodeWorkspace>> {
    let Some(row) = entities::code_workspace::Entity::find_by_id(id.0)
        .filter(entities::code_workspace::Column::Owner.eq(owner.as_str()))
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    Ok(Some(workspace_from_row(row)?))
}

/// The owner's workspaces in one repo, or all of them when `repo_id` is
/// `None`. Most recently created first.
pub async fn list_workspaces(
    store: &DbStore,
    owner: &OwnerId,
    repo_id: Option<RepoId>,
) -> Result<Vec<CodeWorkspace>> {
    let mut query = entities::code_workspace::Entity::find()
        .filter(entities::code_workspace::Column::Owner.eq(owner.as_str()));
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

/// Every workspace in one lifecycle state, across owners.
///
/// Boot recovery uses this only for the transient `Archiving` state. Owner
/// scoping resumes before any caller-facing operation.
pub async fn list_workspaces_by_status_all_owners(
    store: &DbStore,
    status: CodeWorkspaceStatus,
) -> Result<Vec<CodeWorkspace>> {
    entities::code_workspace::Entity::find()
        .filter(entities::code_workspace::Column::Status.eq(status.as_str()))
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(workspace_from_row)
        .collect()
}

/// Change one workspace lifecycle only when it still has the expected state.
pub async fn compare_and_set_workspace_status(
    store: &DbStore,
    owner: &OwnerId,
    id: WorkspaceId,
    expected: CodeWorkspaceStatus,
    next: CodeWorkspaceStatus,
) -> Result<bool> {
    let result = entities::code_workspace::Entity::update_many()
        .col_expr(
            entities::code_workspace::Column::Status,
            sea_orm::sea_query::Expr::value(next.as_str()),
        )
        .filter(entities::code_workspace::Column::Id.eq(id.0))
        .filter(entities::code_workspace::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_workspace::Column::Status.eq(expected.as_str()))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Finish an archive only while the workspace still owns the transient state.
pub async fn complete_workspace_archive(
    store: &DbStore,
    owner: &OwnerId,
    id: WorkspaceId,
    archived_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    let result = entities::code_workspace::Entity::update_many()
        .col_expr(
            entities::code_workspace::Column::Status,
            sea_orm::sea_query::Expr::value(CodeWorkspaceStatus::Archived.as_str()),
        )
        .col_expr(
            entities::code_workspace::Column::ArchivedAt,
            sea_orm::sea_query::Expr::value(archived_at),
        )
        .filter(entities::code_workspace::Column::Id.eq(id.0))
        .filter(entities::code_workspace::Column::Owner.eq(owner.as_str()))
        .filter(
            entities::code_workspace::Column::Status.eq(CodeWorkspaceStatus::Archiving.as_str()),
        )
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Finish a local archive with its recovery metadata in one durable write.
pub async fn complete_workspace_release(
    store: &DbStore,
    owner: &OwnerId,
    id: WorkspaceId,
    archived_at: chrono::DateTime<chrono::Utc>,
    released_tip: Option<String>,
    bundle_bytes: Option<i64>,
) -> Result<bool> {
    let result = entities::code_workspace::Entity::update_many()
        .col_expr(
            entities::code_workspace::Column::Status,
            sea_orm::sea_query::Expr::value(CodeWorkspaceStatus::Released.as_str()),
        )
        .col_expr(
            entities::code_workspace::Column::ArchivedAt,
            sea_orm::sea_query::Expr::value(archived_at),
        )
        .col_expr(
            entities::code_workspace::Column::ReleasedAt,
            sea_orm::sea_query::Expr::value(archived_at),
        )
        .col_expr(
            entities::code_workspace::Column::ReleasedTip,
            sea_orm::sea_query::Expr::value(released_tip),
        )
        .col_expr(
            entities::code_workspace::Column::BundleBytes,
            sea_orm::sea_query::Expr::value(bundle_bytes),
        )
        .filter(entities::code_workspace::Column::Id.eq(id.0))
        .filter(entities::code_workspace::Column::Owner.eq(owner.as_str()))
        .filter(
            entities::code_workspace::Column::Status.eq(CodeWorkspaceStatus::Archiving.as_str()),
        )
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
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
        .col_expr(
            entities::code_workspace::Column::ReleasedAt,
            sea_orm::sea_query::Expr::value(workspace.released_at),
        )
        .col_expr(
            entities::code_workspace::Column::ReleasedTip,
            sea_orm::sea_query::Expr::value(workspace.released_tip.clone()),
        )
        .col_expr(
            entities::code_workspace::Column::BundleBytes,
            sea_orm::sea_query::Expr::value(workspace.bundle_bytes),
        )
        .filter(entities::code_workspace::Column::Id.eq(workspace.id.0))
        .filter(entities::code_workspace::Column::Owner.eq(workspace.owner.as_str()))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Write only the pull-request compatibility column of an active workspace.
///
/// Background refreshes hold their workspace snapshot across host I/O. A
/// full-row save from that snapshot could erase a title or other field a
/// concurrent request changed; this targeted write leaves every unrelated
/// column untouched and loses cleanly if the workspace left the active tier.
pub async fn set_active_workspace_pull_request(
    store: &DbStore,
    owner: &OwnerId,
    id: WorkspaceId,
    pull_request: &PullRequestDigest,
) -> Result<bool> {
    let encoded = serde_json::to_value(pull_request)?;
    let result = entities::code_workspace::Entity::update_many()
        .col_expr(
            entities::code_workspace::Column::Pr,
            sea_orm::sea_query::Expr::value(Some(encoded)),
        )
        .filter(entities::code_workspace::Column::Id.eq(id.0))
        .filter(entities::code_workspace::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_workspace::Column::Status.eq(CodeWorkspaceStatus::Active.as_str()))
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
    owner: &OwnerId,
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
        .filter(entities::code_workspace::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_workspace::Column::Title.eq(expected))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Set a workspace branch only while its generated title and branch still
/// match the values the background naming task observed.
pub async fn set_workspace_branch_if(
    store: &DbStore,
    owner: &OwnerId,
    id: WorkspaceId,
    title: &str,
    expected_branch: &str,
    branch: &str,
) -> Result<bool> {
    let result = entities::code_workspace::Entity::update_many()
        .col_expr(
            entities::code_workspace::Column::BranchName,
            sea_orm::sea_query::Expr::value(branch.to_owned()),
        )
        .filter(entities::code_workspace::Column::Id.eq(id.0))
        .filter(entities::code_workspace::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_workspace::Column::Title.eq(title))
        .filter(entities::code_workspace::Column::BranchName.eq(expected_branch))
        .filter(entities::code_workspace::Column::Status.eq(CodeWorkspaceStatus::Active.as_str()))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Delete a workspace row. Used to roll back a failed create that left no checkout.
pub async fn delete_workspace(store: &DbStore, owner: &OwnerId, id: WorkspaceId) -> Result<bool> {
    let result = entities::code_workspace::Entity::delete_many()
        .filter(entities::code_workspace::Column::Id.eq(id.0))
        .filter(entities::code_workspace::Column::Owner.eq(owner.as_str()))
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
        owner: OwnerId::new(&row.owner)?,
        repo_id: RepoId(row.repo_id),
        title: row.title,
        worktree_path: row.worktree_path,
        branch_name: row.branch_name,
        base_ref: row.base_ref,
        status,
        pr,
        created_at: row.created_at,
        archived_at: row.archived_at,
        released_at: row.released_at,
        released_tip: row.released_tip,
        bundle_bytes: row.bundle_bytes,
    })
}
