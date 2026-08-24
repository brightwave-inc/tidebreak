//! Durable queued follow-ups for code sessions: messages accepted while the
//! session or its workspace checkout was busy, promoted into real turns
//! strictly FIFO once the session worker is free (decision 67).
//!
//! The chat queue (decision 9) proves promotion through the turn-admission
//! table because any process may promote. A code session has exactly one
//! consumer — its worker — so promotion here is simpler: the worker snapshots
//! the FIFO head, and [`promote_queued_turn`] deletes that exact row and
//! inserts the turn in one transaction. An edit, reorder, or retraction that
//! landed in between changes the row beneath the snapshot, the delete matches
//! nothing, and the promotion reports stale instead of running old content.

use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};

use crate::code::{CodeQueuedTurn, CodeSessionId, CodeTurn, CodeTurnId};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};
use super::super::agent_run::database_now;
use super::acquire_code_session_write_lock;

fn queued_turn_from_model(model: entities::code_queued_turn::Model) -> Result<CodeQueuedTurn> {
    Ok(CodeQueuedTurn {
        id: CodeTurnId(model.id),
        session_id: CodeSessionId(model.session_id),
        message: model.message,
        attachments: serde_json::from_str(&model.attachments_json).map_err(|_| {
            AgentError::Store("invalid stored code queued-turn attachment list".into())
        })?,
        position: model.position,
        created_at: model.created_at,
        updated_at: model.updated_at,
    })
}

async fn list_on<C>(
    conn: &C,
    owner: &OwnerId,
    session_id: CodeSessionId,
) -> Result<Vec<CodeQueuedTurn>>
where
    C: ConnectionTrait,
{
    entities::code_queued_turn::Entity::find()
        .filter(entities::code_queued_turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_queued_turn::Column::SessionId.eq(session_id.0))
        .order_by_asc(entities::code_queued_turn::Column::Position)
        .order_by_asc(entities::code_queued_turn::Column::CreatedAt)
        .all(conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(queued_turn_from_model)
        .collect()
}

/// The session's queued messages, FIFO.
pub async fn list_queued_turns(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
) -> Result<Vec<CodeQueuedTurn>> {
    list_on(&store.conn, owner, session_id).await
}

/// The FIFO head, or `None` when the queue is empty.
pub async fn queued_turn_head(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
) -> Result<Option<CodeQueuedTurn>> {
    entities::code_queued_turn::Entity::find()
        .filter(entities::code_queued_turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_queued_turn::Column::SessionId.eq(session_id.0))
        .order_by_asc(entities::code_queued_turn::Column::Position)
        .order_by_asc(entities::code_queued_turn::Column::CreatedAt)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(queued_turn_from_model)
        .transpose()
}

/// Park one message at the queue tail. The caller supplies the row id, which
/// is the turn id promotion later inserts under.
pub async fn enqueue_queued_turn(
    store: &DbStore,
    owner: &OwnerId,
    queued: &CodeQueuedTurn,
) -> Result<CodeQueuedTurn> {
    if queued.id.0.is_nil() || queued.message.trim().is_empty() || queued.message.contains('\0') {
        return Err(AgentError::Store("invalid code queued turn".into()));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, queued.session_id).await? {
        return Err(AgentError::Store(format!(
            "code session {} not found",
            queued.session_id
        )));
    }
    let rows = list_on(&transaction, owner, queued.session_id).await?;
    if rows.len() >= CodeQueuedTurn::MAX_PER_SESSION {
        transaction.commit().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "a session may queue at most {} messages",
            CodeQueuedTurn::MAX_PER_SESSION
        )));
    }
    let position = rows.last().map_or(0, |last| last.position + 1);
    let now = database_now(&transaction).await?;
    entities::code_queued_turn::ActiveModel {
        id: Set(queued.id.0),
        owner: Set(owner.as_str().to_owned()),
        session_id: Set(queued.session_id.0),
        message: Set(queued.message.clone()),
        attachments_json: Set(serde_json::to_string(&queued.attachments).map_err(store_err)?),
        position: Set(position),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    let inserted = entities::code_queued_turn::Entity::find_by_id(queued.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("code queued turn disappeared".into()))?;
    let inserted = queued_turn_from_model(inserted)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(inserted)
}

/// Promote the exact head snapshot into a running turn.
///
/// Deletes the row and inserts `turn` in one transaction. Returns `false` —
/// with nothing written — when the row was edited, reordered, or retracted
/// after the snapshot was taken; the worker re-reads the head and tries again.
pub async fn promote_queued_turn(
    store: &DbStore,
    owner: &OwnerId,
    expected: &CodeQueuedTurn,
    turn: &CodeTurn,
) -> Result<bool> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, expected.session_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(false);
    }
    let deleted = entities::code_queued_turn::Entity::delete_many()
        .filter(entities::code_queued_turn::Column::Id.eq(expected.id.0))
        .filter(entities::code_queued_turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_queued_turn::Column::SessionId.eq(expected.session_id.0))
        .filter(entities::code_queued_turn::Column::Position.eq(expected.position))
        .filter(entities::code_queued_turn::Column::UpdatedAt.eq(expected.updated_at))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if deleted.rows_affected != 1 {
        transaction.commit().await.map_err(store_err)?;
        return Ok(false);
    }
    super::turn::insert_turn_on(&transaction, owner, turn).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(true)
}

/// Drop one queued message that can no longer run (a dead attachment, a
/// failed start). Deletes by id alone, so it also cleans up after a promotion
/// that already removed the row. Returns whether a row was deleted.
pub async fn delete_queued_turn(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    id: CodeTurnId,
) -> Result<bool> {
    let deleted = entities::code_queued_turn::Entity::delete_many()
        .filter(entities::code_queued_turn::Column::Id.eq(id.0))
        .filter(entities::code_queued_turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_queued_turn::Column::SessionId.eq(session_id.0))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(deleted.rows_affected == 1)
}

/// Clear every queued message on a session. Ending a session retracts its
/// queue: nothing will ever promote the rows, and the tray is gone.
pub async fn delete_session_queued_turns(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
) -> Result<u64> {
    let deleted = entities::code_queued_turn::Entity::delete_many()
        .filter(entities::code_queued_turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_queued_turn::Column::SessionId.eq(session_id.0))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(deleted.rows_affected)
}

/// Edit a queued message's content and/or move it to a new position. Rewrites
/// every position in the session so the order stays dense and total.
pub async fn update_queued_turn(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    id: CodeTurnId,
    message: Option<&str>,
    position: Option<i32>,
) -> Result<Option<CodeQueuedTurn>> {
    if let Some(message) = message {
        if message.trim().is_empty() || message.contains('\0') {
            return Err(AgentError::Store("invalid code queued-turn message".into()));
        }
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, session_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let now = database_now(&transaction).await?;
    let mut rows = entities::code_queued_turn::Entity::find()
        .filter(entities::code_queued_turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_queued_turn::Column::SessionId.eq(session_id.0))
        .order_by_asc(entities::code_queued_turn::Column::Position)
        .order_by_asc(entities::code_queued_turn::Column::CreatedAt)
        .all(&transaction)
        .await
        .map_err(store_err)?;
    let Some(index) = rows.iter().position(|row| row.id == id.0) else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if let Some(target) = position {
        let target = usize::try_from(target.max(0))
            .unwrap_or(0)
            .min(rows.len() - 1);
        let row = rows.remove(index);
        rows.insert(target, row);
    }
    for (ordinal, row) in rows.iter().enumerate() {
        let is_edited = row.id == id.0;
        let mut active = entities::code_queued_turn::ActiveModel {
            id: Set(row.id),
            position: Set(i32::try_from(ordinal).unwrap_or(i32::MAX)),
            ..Default::default()
        };
        if is_edited {
            if let Some(message) = message {
                active.message = Set(message.to_owned());
            }
            active.updated_at = Set(now);
        }
        active.update(&transaction).await.map_err(store_err)?;
    }
    let updated = entities::code_queued_turn::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("code queued turn disappeared".into()))?;
    let updated = queued_turn_from_model(updated)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(updated))
}

fn queue_paused_setting(session_id: CodeSessionId) -> String {
    format!("code.sessions.{session_id}.queue_paused")
}

/// Whether promotion is paused for this session. Absent reads as running,
/// exactly like the chat queue's per-chat pause key.
///
/// The key lives in the settings table, which carries no owner column, so
/// the owner check anchors on the session row: a foreign owner reads the
/// default and observes nothing.
pub async fn queue_paused(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
) -> Result<bool> {
    if super::session::get_session(store, owner, session_id)
        .await?
        .is_none()
    {
        return Ok(false);
    }
    Ok(
        entities::setting::Entity::find_by_id(queue_paused_setting(session_id))
            .one(&store.conn)
            .await
            .map_err(store_err)?
            .map(|model| model.value_json == serde_json::json!(true))
            .unwrap_or(false),
    )
}

/// Pause or release promotion for this session; queued rows stay put while
/// paused. Refuses a session the owner does not hold, for the same reason
/// [`queue_paused`] anchors on the session row.
pub async fn set_queue_paused(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    paused: bool,
) -> Result<()> {
    if super::session::get_session(store, owner, session_id)
        .await?
        .is_none()
    {
        return Err(AgentError::Store(format!(
            "code session {session_id} not found"
        )));
    }
    let model = entities::setting::ActiveModel {
        key: Set(queue_paused_setting(session_id)),
        value_json: Set(serde_json::json!(paused)),
    };
    entities::setting::Entity::insert(model)
        .on_conflict(
            sea_orm::sea_query::OnConflict::column(entities::setting::Column::Key)
                .update_column(entities::setting::Column::ValueJson)
                .to_owned(),
        )
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(())
}
