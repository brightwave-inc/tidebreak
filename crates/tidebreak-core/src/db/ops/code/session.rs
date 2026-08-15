use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

use crate::code::{
    Attention, AttentionSource, AttentionState, CodePermissionMode, CodeSession, CodeSessionId,
    CodeSessionLifecycle, FenceReason, HarnessKind, WorkspaceId,
};
use crate::error::{AgentError, Result};

use super::super::super::{entities, store_err, DbStore};

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
pub async fn bump_spawn_epoch(
    store: &DbStore,
    id: CodeSessionId,
    child_pid: Option<i64>,
) -> Result<i64> {
    let Some(mut session) = get_session(store, id).await? else {
        return Err(AgentError::Store(format!("code session {id} not found")));
    };
    session.spawn_epoch = session
        .spawn_epoch
        .checked_add(1)
        .ok_or_else(|| AgentError::Store(format!("code session {id} spawn epoch overflow")))?;
    session.child_pid = child_pid;
    entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::SpawnEpoch,
            sea_orm::sea_query::Expr::value(session.spawn_epoch),
        )
        .col_expr(
            entities::code_session::Column::ChildPid,
            sea_orm::sea_query::Expr::value(child_pid),
        )
        .filter(entities::code_session::Column::Id.eq(id.0))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(session.spawn_epoch)
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
