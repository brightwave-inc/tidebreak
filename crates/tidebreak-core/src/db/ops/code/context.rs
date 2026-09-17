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

/// Stable external binding key for a delegated child request.
pub fn delegated_child_external_key(parent: SessionId, request_key: &str) -> String {
    format!("child/{parent}/{request_key}")
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

/// Children named by an active wait. Expired leases stay inert until replaced.
pub async fn parent_wait_ids(
    store: &DbStore,
    owner: &OwnerId,
    parent: SessionId,
) -> Result<Option<Vec<SessionId>>> {
    let Some(row) = active_parent_wait(store, owner, parent).await? else {
        return Ok(None);
    };
    let ids: Vec<uuid::Uuid> = serde_json::from_str(&row.child_ids)
        .map_err(|error| AgentError::Store(error.to_string()))?;
    Ok(Some(ids.into_iter().map(SessionId).collect()))
}

/// Deadline for a connected reader's wait refresh. No active lease means no timer.
pub async fn parent_wait_deadline(
    store: &DbStore,
    owner: &OwnerId,
    parent: SessionId,
) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
    Ok(active_parent_wait(store, owner, parent)
        .await?
        .map(|row| row.expires_at))
}

async fn active_parent_wait(
    store: &DbStore,
    owner: &OwnerId,
    parent: SessionId,
) -> Result<Option<entities::code_parent_wait::Model>> {
    if super::get_session(store, owner, parent).await?.is_none() {
        return Err(AgentError::InvalidTarget("parent session not found".into()));
    }
    entities::code_parent_wait::Entity::find_by_id(parent.0)
        .filter(entities::code_parent_wait::Column::ExpiresAt.gt(chrono::Utc::now()))
        .one(&store.conn)
        .await
        .map_err(store_err)
}

/// Replace the active wait under the parent's write lock.
///
/// Each invocation owns one generation. Its deadline bounds abandoned waits
/// after process loss; snapshots never change this authoritative row.
pub async fn set_parent_wait(
    store: &DbStore,
    owner: &OwnerId,
    parent: SessionId,
    generation: uuid::Uuid,
    ids: &[SessionId],
    expires_at: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    if ids.is_empty() {
        return Err(AgentError::InvalidTarget(
            "a wait must name children".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, parent).await?
        || entities::session::Entity::find_by_id(parent.0)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .is_none_or(|row| row.owner != owner.as_str())
    {
        return Err(AgentError::InvalidTarget("parent session not found".into()));
    }
    let child_ids = serde_json::to_string(&ids.iter().map(|id| id.0).collect::<Vec<uuid::Uuid>>())
        .map_err(|error| AgentError::Store(error.to_string()))?;
    let existing = entities::code_parent_wait::Entity::find_by_id(parent.0)
        .one(&transaction)
        .await
        .map_err(store_err)?;
    if let Some(existing) = existing {
        let mut active: entities::code_parent_wait::ActiveModel = existing.into();
        active.generation = Set(generation);
        active.child_ids = Set(child_ids);
        active.expires_at = Set(expires_at);
        active.update(&transaction).await.map_err(store_err)?;
    } else {
        entities::code_parent_wait::ActiveModel {
            parent_session_id: Set(parent.0),
            generation: Set(generation),
            child_ids: Set(child_ids),
            expires_at: Set(expires_at),
        }
        .insert(&transaction)
        .await
        .map_err(store_err)?;
    }
    transaction.commit().await.map_err(store_err)
}

/// Clear only the invocation that finishes; an older call cannot clear its replacement.
pub async fn clear_parent_wait(
    store: &DbStore,
    owner: &OwnerId,
    parent: SessionId,
    generation: uuid::Uuid,
) -> Result<bool> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, parent).await?
        || entities::session::Entity::find_by_id(parent.0)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .is_none_or(|row| row.owner != owner.as_str())
    {
        return Ok(false);
    }
    let deleted = entities::code_parent_wait::Entity::delete_many()
        .filter(entities::code_parent_wait::Column::ParentSessionId.eq(parent.0))
        .filter(entities::code_parent_wait::Column::Generation.eq(generation))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(deleted.rows_affected != 0)
}
