use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder, Set, TransactionTrait,
};

use crate::attention::{Attention, AttentionSource, AttentionState, FenceReason};
use crate::code::{
    CodeSession, CodeSessionId, CodeSessionKind, CodeSessionLifecycle, CodeSubagentSummary,
    HarnessKind, WorkspaceId,
};
use crate::error::{AgentError, Result};
use crate::OwnerId;
use crate::PermissionMode;

use super::super::super::{entities, store_err, DbStore};
use super::acquire_code_session_write_lock;

/// Insert a session row. The row belongs to `session.owner`, denormalized
/// from the workspace it runs in.
pub async fn insert_session(store: &DbStore, session: &CodeSession) -> Result<()> {
    entities::code_session::ActiveModel {
        id: Set(session.id.0),
        owner: Set(session.owner.as_str().to_owned()),
        workspace_id: Set(session.workspace_id.0),
        kind: Set(session.kind.as_str().to_owned()),
        harness_kind: Set(session.harness_kind.as_str().to_owned()),
        harness_version: Set(session.harness_version.clone()),
        harness_resume_ref: Set(session.harness_resume_ref.clone()),
        permission_mode: Set(session.permission_mode.as_str().to_owned()),
        model: Set(session.model.clone()),
        reasoning_effort: Set(session
            .reasoning_effort
            .map(|effort| effort.as_str().to_owned())),
        fast_mode: Set(session.fast_mode),
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
        subagents: Set(if session.subagents.is_empty() {
            None
        } else {
            Some(serde_json::to_value(&session.subagents)?)
        }),
        created_at: Set(session.created_at),
    }
    .insert(&store.conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

/// Load one of the owner's sessions by id.
///
/// Another owner's session is indistinguishable from a missing one.
pub async fn get_session(
    store: &DbStore,
    owner: &OwnerId,
    id: CodeSessionId,
) -> Result<Option<CodeSession>> {
    let Some(row) = entities::code_session::Entity::find_by_id(id.0)
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    Ok(Some(session_from_row(row)?))
}

/// Load a session by id, whatever owner it belongs to.
///
/// A system path, not a request path: a session worker re-reads the row it
/// was spawned against after bumping the spawn epoch, and the id it holds
/// was authorized when the worker was created. Nothing reachable from a
/// route may call it.
pub async fn get_session_all_owners(
    store: &DbStore,
    id: CodeSessionId,
) -> Result<Option<CodeSession>> {
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

/// The owner's sessions, most recently created first.
pub async fn list_sessions(store: &DbStore, owner: &OwnerId) -> Result<Vec<CodeSession>> {
    entities::code_session::Entity::find()
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .order_by_desc(entities::code_session::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(session_from_row)
        .collect()
}

/// The owner's sessions in one workspace, most recently created first.
pub async fn list_sessions_for_workspace(
    store: &DbStore,
    owner: &OwnerId,
    workspace_id: WorkspaceId,
) -> Result<Vec<CodeSession>> {
    entities::code_session::Entity::find()
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_session::Column::WorkspaceId.eq(workspace_id.0))
        .order_by_desc(entities::code_session::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(session_from_row)
        .collect()
}

/// Every session on the machine, most recently created first.
///
/// A system path, not a request path: boot recovery re-attaches a worker to
/// each live session regardless of who owns it. Nothing reachable from a
/// route may call it.
pub async fn list_sessions_all_owners(store: &DbStore) -> Result<Vec<CodeSession>> {
    entities::code_session::Entity::find()
        .order_by_desc(entities::code_session::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(session_from_row)
        .collect()
}

/// Sessions in one lifecycle state, across every owner.
///
/// A system path, not a request path: boot recovery and the stall sweep act
/// on the whole machine's sessions, the way the chat side's unscoped store
/// handle serves turn workers and retirement scans. Nothing reachable from a
/// route may call it.
pub async fn list_sessions_by_lifecycle_all_owners(
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

/// Persist mutable session fields. `id`, `workspace_id`, `created_at`,
/// `attention`, and `subagents` stay as stored. Attention and subagents have
/// targeted writes so a full-row save from a stale worker cannot clobber a
/// concurrent structured update.
///
/// `spawn_epoch` must be non-decreasing, and `Ended` is terminal: a caller
/// that is not `Ended` cannot overwrite that lifecycle. Returns `false` when
/// nothing was written.
pub async fn save_session(store: &DbStore, session: &CodeSession) -> Result<bool> {
    let mut predicate = Condition::all()
        .add(entities::code_session::Column::Id.eq(session.id.0))
        .add(entities::code_session::Column::Owner.eq(session.owner.as_str()))
        .add(entities::code_session::Column::SpawnEpoch.lte(session.spawn_epoch));
    if session.lifecycle != CodeSessionLifecycle::Ended {
        predicate = predicate.add(
            entities::code_session::Column::Lifecycle
                .ne(CodeSessionLifecycle::Ended.as_str().to_owned()),
        );
    }
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
            entities::code_session::Column::Model,
            sea_orm::sea_query::Expr::value(session.model.clone()),
        )
        .col_expr(
            entities::code_session::Column::ReasoningEffort,
            sea_orm::sea_query::Expr::value(
                session
                    .reasoning_effort
                    .map(|effort| effort.as_str().to_owned()),
            ),
        )
        .col_expr(
            entities::code_session::Column::FastMode,
            sea_orm::sea_query::Expr::value(session.fast_mode),
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
            entities::code_session::Column::UnrecognizedEventCount,
            sea_orm::sea_query::Expr::value(session.unrecognized_event_count),
        )
        .filter(predicate)
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    if result.rows_affected != 1 {
        // Rare, and invisible without a word: distinguish a superseded writer
        // from a row that is simply gone.
        if let Some(current) = get_session(store, &session.owner, session.id).await? {
            tracing::warn!(
                session = %session.id,
                attempted_epoch = session.spawn_epoch,
                current_epoch = current.spawn_epoch,
                attempted_lifecycle = session.lifecycle.as_str(),
                current_lifecycle = current.lifecycle.as_str(),
                "dropping a session write from a superseded or ended code-session row"
            );
        }
        return Ok(false);
    }
    Ok(true)
}

/// Replace one session's attention through the shared row-locked policy.
///
/// `from_user` is the explicit pin or clear path. Automatic callers must pass
/// `false`, which preserves manual attention and reevaluates replacement
/// against the database value instead of a stale session copy.
pub async fn replace_session_attention(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    next: &Attention,
    from_user: bool,
) -> Result<Option<Attention>> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let changed =
        replace_session_attention_on(&transaction, owner, session_id, next, from_user).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(changed)
}

pub(in crate::db) async fn replace_session_attention_on<C>(
    conn: &C,
    owner: &OwnerId,
    session_id: CodeSessionId,
    next: &Attention,
    from_user: bool,
) -> Result<Option<Attention>>
where
    C: ConnectionTrait,
{
    if !acquire_code_session_write_lock(conn, session_id).await? {
        return Ok(None);
    }
    let row = entities::code_session::Entity::find_by_id(session_id.0)
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .one(conn)
        .await
        .map_err(store_err)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let current = session_from_row(row)?.attention;
    if current == *next || (!from_user && !crate::attention::should_replace(&current, next)) {
        return Ok(None);
    }
    let updated = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::AttentionState,
            sea_orm::sea_query::Expr::value(serde_json::to_value(&next.state)?),
        )
        .col_expr(
            entities::code_session::Column::AttentionSource,
            sea_orm::sea_query::Expr::value(next.source.as_str()),
        )
        .filter(entities::code_session::Column::Id.eq(session_id.0))
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "code session {session_id} disappeared while replacing attention"
        )));
    }
    Ok(Some(next.clone()))
}

/// Replace one session's subagent list (decision 52). A targeted write so
/// the event sink never read-modify-writes the whole row against the
/// worker's concurrent full-row saves. Returns `false` when the row is gone
/// or belongs to another owner.
pub async fn set_session_subagents(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    subagents: &[CodeSubagentSummary],
) -> Result<bool> {
    let result = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::Subagents,
            sea_orm::sea_query::Expr::value(if subagents.is_empty() {
                None
            } else {
                Some(serde_json::to_value(subagents)?)
            }),
        )
        .filter(entities::code_session::Column::Id.eq(session_id.0))
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

pub(super) fn session_from_row(row: entities::code_session::Model) -> Result<CodeSession> {
    let harness_kind = HarnessKind::from_str(&row.harness_kind).ok_or_else(|| {
        AgentError::Store(format!(
            "code_session {} has unknown harness_kind {}",
            row.id, row.harness_kind
        ))
    })?;
    let permission_mode = PermissionMode::from_str(&row.permission_mode).ok_or_else(|| {
        AgentError::Store(format!(
            "code_session {} has unknown permission_mode {}",
            row.id, row.permission_mode
        ))
    })?;
    let kind = CodeSessionKind::from_str(&row.kind).ok_or_else(|| {
        AgentError::Store(format!(
            "code_session {} has unknown kind {}",
            row.id, row.kind
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
    let reasoning_effort = match row.reasoning_effort.as_deref() {
        Some(token) => Some(
            crate::model::ReasoningEffort::from_str(token).ok_or_else(|| {
                AgentError::Store(format!(
                    "code_session {} has unknown reasoning_effort {token}",
                    row.id
                ))
            })?,
        ),
        None => None,
    };
    let subagents = match row.subagents {
        Some(value) => {
            serde_json::from_value::<Vec<CodeSubagentSummary>>(value).map_err(|err| {
                AgentError::Store(format!("code_session {} subagents: {err}", row.id))
            })?
        }
        None => Vec::new(),
    };
    Ok(CodeSession {
        id: CodeSessionId(row.id),
        owner: OwnerId::new(&row.owner)?,
        workspace_id: WorkspaceId(row.workspace_id),
        kind,
        harness_kind,
        harness_version: row.harness_version,
        harness_resume_ref: row.harness_resume_ref,
        permission_mode,
        model: row.model,
        reasoning_effort,
        fast_mode: row.fast_mode,
        lifecycle,
        fence_reason,
        child_pid: row.child_pid,
        spawn_epoch: row.spawn_epoch,
        attention: Attention::new(state, source),
        unrecognized_event_count: row.unrecognized_event_count,
        subagents,
        created_at: row.created_at,
    })
}
