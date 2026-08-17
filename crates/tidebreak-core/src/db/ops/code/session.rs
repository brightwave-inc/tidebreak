use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};

use crate::code::{
    Attention, AttentionSource, AttentionState, CodePermissionMode, CodeSession, CodeSessionId,
    CodeSessionLifecycle, FenceReason, HarnessKind, WorkspaceId,
};
use crate::error::{AgentError, Result};

use super::super::super::{entities, store_err, DbStore};
use super::acquire_code_session_write_lock;

/// Insert a session row.
pub async fn insert_session(store: &DbStore, session: &CodeSession) -> Result<()> {
    entities::code_session::ActiveModel {
        id: Set(session.id.0),
        workspace_id: Set(session.workspace_id.0),
        harness_kind: Set(session.harness_kind.as_str().to_owned()),
        harness_version: Set(session.harness_version.clone()),
        harness_resume_ref: Set(session.harness_resume_ref.clone()),
        permission_mode: Set(session.permission_mode.as_str().to_owned()),
        lifecycle: Set(session.lifecycle.as_str().to_owned()),
        fence_reason: Set(match &session.fence_reason {
            Some(reason) => Some(serde_json::to_value(reason)?),
            None => None,
        }),
        child_pid: Set(session.child_pid),
        spawn_epoch: Set(session.spawn_epoch),
        attention_state: Set(serde_json::to_value(&session.attention.state)?),
        attention_source: Set(session.attention.source.as_str().to_owned()),
        unrecognized_event_count: Set(session.unrecognized_event_count),
        created_at: Set(session.created_at),
    }
    .insert(&store.conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

/// Load a session by id.
pub async fn get_session(store: &DbStore, id: CodeSessionId) -> Result<Option<CodeSession>> {
    let Some(row) = entities::code_session::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    Ok(Some(session_from_row(row)?))
}

/// Advance the spawn epoch and record the new child pid. Returns the new epoch.
///
/// Serialized on the same session-row lock journal appends take, so a
/// superseded worker cannot keep a live epoch.
pub async fn bump_spawn_epoch(
    store: &DbStore,
    id: CodeSessionId,
    child_pid: Option<i64>,
) -> Result<i64> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, id).await? {
        return Err(AgentError::Store(format!("code session {id} not found")));
    }
    let Some(session) = entities::code_session::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        return Err(AgentError::Store(format!("code session {id} not found")));
    };
    let next = session
        .spawn_epoch
        .checked_add(1)
        .ok_or_else(|| AgentError::Store(format!("code session {id} spawn epoch overflow")))?;
    let updated = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::SpawnEpoch,
            sea_orm::sea_query::Expr::value(next),
        )
        .col_expr(
            entities::code_session::Column::ChildPid,
            sea_orm::sea_query::Expr::value(child_pid),
        )
        .filter(entities::code_session::Column::Id.eq(id.0))
        .filter(entities::code_session::Column::SpawnEpoch.eq(session.spawn_epoch))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "code session {id} spawn epoch changed under the lock"
        )));
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(next)
}

/// Every session, most recently created first.
pub async fn list_sessions(store: &DbStore) -> Result<Vec<CodeSession>> {
    entities::code_session::Entity::find()
        .order_by_desc(entities::code_session::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(session_from_row)
        .collect()
}

/// Sessions belonging to one workspace, most recently created first.
pub async fn list_sessions_for_workspace(
    store: &DbStore,
    workspace_id: WorkspaceId,
) -> Result<Vec<CodeSession>> {
    entities::code_session::Entity::find()
        .filter(entities::code_session::Column::WorkspaceId.eq(workspace_id.0))
        .order_by_desc(entities::code_session::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(session_from_row)
        .collect()
}

/// Sessions in one lifecycle state.
pub async fn list_sessions_by_lifecycle(
    store: &DbStore,
    lifecycle: CodeSessionLifecycle,
) -> Result<Vec<CodeSession>> {
    entities::code_session::Entity::find()
        .filter(entities::code_session::Column::Lifecycle.eq(lifecycle.as_str().to_owned()))
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(session_from_row)
        .collect()
}

/// Persist mutable session fields. `id`, `workspace_id`, and `created_at` stay as stored.
///
/// The write is fenced on `spawn_epoch`, the same way journal appends are: a
/// caller holding an epoch older than the stored row is a superseded worker
/// unwinding, and its write is dropped rather than allowed to regress the row.
/// Without the fence such a worker writes its own epoch back over a newer
/// spawn's — un-fencing itself, making the live worker the stale one, and
/// silently dropping everything the live worker appends afterwards. The same
/// fence keeps a superseded worker from regressing lifecycle, attention, or
/// the recorded child pid.
///
/// Returns `false` when nothing was written: either the row is gone or the
/// caller has been superseded.
pub async fn save_session(store: &DbStore, session: &CodeSession) -> Result<bool> {
    let result = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::HarnessKind,
            sea_orm::sea_query::Expr::value(session.harness_kind.as_str().to_owned()),
        )
        .col_expr(
            entities::code_session::Column::HarnessVersion,
            sea_orm::sea_query::Expr::value(session.harness_version.clone()),
        )
        .col_expr(
            entities::code_session::Column::HarnessResumeRef,
            sea_orm::sea_query::Expr::value(session.harness_resume_ref.clone()),
        )
        .col_expr(
            entities::code_session::Column::PermissionMode,
            sea_orm::sea_query::Expr::value(session.permission_mode.as_str().to_owned()),
        )
        .col_expr(
            entities::code_session::Column::Lifecycle,
            sea_orm::sea_query::Expr::value(session.lifecycle.as_str().to_owned()),
        )
        .col_expr(
            entities::code_session::Column::FenceReason,
            sea_orm::sea_query::Expr::value(match &session.fence_reason {
                Some(reason) => Some(serde_json::to_value(reason)?),
                None => None,
            }),
        )
        .col_expr(
            entities::code_session::Column::ChildPid,
            sea_orm::sea_query::Expr::value(session.child_pid),
        )
        .col_expr(
            entities::code_session::Column::SpawnEpoch,
            sea_orm::sea_query::Expr::value(session.spawn_epoch),
        )
        .col_expr(
            entities::code_session::Column::AttentionState,
            sea_orm::sea_query::Expr::value(serde_json::to_value(&session.attention.state)?),
        )
        .col_expr(
            entities::code_session::Column::AttentionSource,
            sea_orm::sea_query::Expr::value(session.attention.source.as_str().to_owned()),
        )
        .col_expr(
            entities::code_session::Column::UnrecognizedEventCount,
            sea_orm::sea_query::Expr::value(session.unrecognized_event_count),
        )
        .filter(entities::code_session::Column::Id.eq(session.id.0))
        .filter(entities::code_session::Column::SpawnEpoch.lte(session.spawn_epoch))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    if result.rows_affected != 1 {
        // Rare, and invisible without a word: distinguish a superseded writer
        // from a row that is simply gone.
        if let Some(current) = get_session(store, session.id).await? {
            tracing::warn!(
                session = %session.id,
                attempted = session.spawn_epoch,
                current = current.spawn_epoch,
                "dropping a session write from a superseded code-session worker"
            );
        }
        return Ok(false);
    }
    Ok(true)
}

pub(super) fn session_from_row(row: entities::code_session::Model) -> Result<CodeSession> {
    let harness_kind = HarnessKind::from_str(&row.harness_kind).ok_or_else(|| {
        AgentError::Store(format!(
            "code_session {} has unknown harness_kind {}",
            row.id, row.harness_kind
        ))
    })?;
    let permission_mode = CodePermissionMode::from_str(&row.permission_mode).ok_or_else(|| {
        AgentError::Store(format!(
            "code_session {} has unknown permission_mode {}",
            row.id, row.permission_mode
        ))
    })?;
    let lifecycle = CodeSessionLifecycle::from_str(&row.lifecycle).ok_or_else(|| {
        AgentError::Store(format!(
            "code_session {} has unknown lifecycle {}",
            row.id, row.lifecycle
        ))
    })?;
    let source = AttentionSource::from_str(&row.attention_source).ok_or_else(|| {
        AgentError::Store(format!(
            "code_session {} has unknown attention_source {}",
            row.id, row.attention_source
        ))
    })?;
    let state = serde_json::from_value::<AttentionState>(row.attention_state).map_err(|err| {
        AgentError::Store(format!("code_session {} attention_state: {err}", row.id))
    })?;
    let fence_reason = match row.fence_reason {
        Some(value) => Some(serde_json::from_value::<FenceReason>(value).map_err(|err| {
            AgentError::Store(format!("code_session {} fence_reason: {err}", row.id))
        })?),
        None => None,
    };
    Ok(CodeSession {
        id: CodeSessionId(row.id),
        workspace_id: WorkspaceId(row.workspace_id),
        harness_kind,
        harness_version: row.harness_version,
        harness_resume_ref: row.harness_resume_ref,
        permission_mode,
        lifecycle,
        fence_reason,
        child_pid: row.child_pid,
        spawn_epoch: row.spawn_epoch,
        attention: Attention::new(state, source),
        unrecognized_event_count: row.unrecognized_event_count,
        created_at: row.created_at,
    })
}
