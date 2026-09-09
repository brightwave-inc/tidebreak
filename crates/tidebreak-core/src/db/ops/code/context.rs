//! Conversation provenance and direct children, independent of repository workspaces.
use super::acquire_code_session_write_lock;
use crate::db::{entities, store_err, DbStore};
use crate::error::{AgentError, Result};
use crate::{OwnerId, SessionId};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionContext {
    pub channel_id: Option<String>,
    pub parent_session_id: Option<SessionId>,
    pub request_key: Option<String>,
}

/// Read provenance only after resolving the session for its owner.
pub async fn session_context(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
) -> Result<Option<SessionContext>> {
    if super::get_session(store, owner, session).await?.is_none() {
        return Err(AgentError::InvalidTarget("session not found".into()));
    }
    Ok(
        entities::code_session_context::Entity::find_by_id(session.0)
            .one(&store.conn)
            .await
            .map_err(store_err)?
            .map(|row| SessionContext {
                channel_id: row.channel_id,
                parent_session_id: row.parent_session_id.map(SessionId),
                request_key: row.request_key,
            }),
    )
}

/// Set an immutable channel or parent. A replay must name the same scope.
pub async fn set_session_context(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    channel: Option<&str>,
    parent: Option<SessionId>,
    request_key: Option<&str>,
) -> Result<()> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, session).await? {
        return Err(AgentError::InvalidTarget("session not found".into()));
    }
    let row = entities::session::Entity::find_by_id(session.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .filter(|row| row.owner == owner.as_str())
        .ok_or_else(|| AgentError::InvalidTarget("session not found".into()))?;
    if let Some(parent) = parent {
        if parent == session
            || entities::session::Entity::find_by_id(parent.0)
                .one(&transaction)
                .await
                .map_err(store_err)?
                .is_none_or(|p| p.owner != row.owner)
        {
            return Err(AgentError::InvalidTarget("parent session not found".into()));
        }
    }
    let existing = entities::code_session_context::Entity::find_by_id(session.0)
        .one(&transaction)
        .await
        .map_err(store_err)?;
    if let Some(existing) = existing {
        if existing.channel_id.as_deref() != channel
            || existing.parent_session_id != parent.map(|id| id.0)
            || existing.request_key.as_deref() != request_key
        {
            return Err(AgentError::InvalidTarget(
                "the conversation already belongs to another channel or parent".into(),
            ));
        }
    } else {
        entities::code_session_context::ActiveModel {
            session_id: Set(session.0),
            channel_id: Set(channel.map(str::to_owned)),
            parent_session_id: Set(parent.map(|id| id.0)),
            request_key: Set(request_key.map(str::to_owned)),
        }
        .insert(&transaction)
        .await
        .map_err(store_err)?;
    }
    transaction.commit().await.map_err(store_err)
}

pub async fn child_sessions(
    store: &DbStore,
    owner: &OwnerId,
    parent: SessionId,
) -> Result<Vec<crate::Session>> {
    if super::get_session(store, owner, parent).await?.is_none() {
        return Err(AgentError::InvalidTarget("parent session not found".into()));
    }
    let rows = entities::code_session_context::Entity::find()
        .filter(entities::code_session_context::Column::ParentSessionId.eq(parent.0))
        .order_by_asc(entities::code_session_context::Column::SessionId)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let mut sessions = Vec::new();
    for row in rows {
        if let Some(session) = super::get_session(store, owner, SessionId(row.session_id)).await? {
            sessions.push(session);
        }
    }
    Ok(sessions)
}
