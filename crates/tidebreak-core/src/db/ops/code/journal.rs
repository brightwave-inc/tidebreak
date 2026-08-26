use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};

use crate::code::{CodeEvent, CodeSessionId, SequencedCodeEvent};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};
use super::{acquire_code_session_write_lock, CodeJournalError};

/// Append one journal event under the session's spawn-epoch fence.
///
/// Sequence numbers are allocated while holding the session row lock, the
/// same discipline the chat journal uses on the chat row. An append whose
/// `spawn_epoch` does not match the session row is rejected so a superseded
/// worker cannot corrupt the stream.
pub async fn append_event(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    spawn_epoch: i64,
    event: &CodeEvent,
) -> std::result::Result<i64, CodeJournalError> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, session_id).await? {
        return Err(CodeJournalError::SessionNotFound { session_id });
    }
    let Some(session) = entities::code_session::Entity::find_by_id(session_id.0)
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        return Err(CodeJournalError::SessionNotFound { session_id });
    };
    if session.spawn_epoch != spawn_epoch {
        return Err(CodeJournalError::StaleSpawnEpoch {
            session_id,
            attempted: spawn_epoch,
            current: session.spawn_epoch,
        });
    }
    let seq = append_event_on_locked(&transaction, owner, session_id, event).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(seq)
}

/// Append after the caller has locked and fenced the session row.
pub(in crate::db) async fn append_event_on_locked<C>(
    conn: &C,
    owner: &OwnerId,
    session_id: CodeSessionId,
    event: &CodeEvent,
) -> Result<i64>
where
    C: ConnectionTrait,
{
    let last = entities::code_event::Entity::find()
        .filter(entities::code_event::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_event::Column::SessionId.eq(session_id.0))
        .order_by_desc(entities::code_event::Column::Seq)
        .one(conn)
        .await
        .map_err(store_err)?;
    let seq = last
        .map_or(Some(1), |model| model.seq.checked_add(1))
        .ok_or_else(|| {
            AgentError::Store(format!(
                "event sequence exhausted for code session {session_id}"
            ))
        })?;
    entities::code_event::ActiveModel {
        owner: Set(owner.as_str().to_owned()),
        session_id: Set(session_id.0),
        seq: Set(seq),
        event: Set(serde_json::to_value(event).map_err(AgentError::from)?),
        created_at: Set(Utc::now()),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    Ok(seq)
}

/// Created-at of the newest journal row, if the session has any.
pub async fn latest_event_created_at(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
) -> Result<Option<chrono::DateTime<Utc>>> {
    Ok(entities::code_event::Entity::find()
        .filter(entities::code_event::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_event::Column::SessionId.eq(session_id.0))
        .order_by_desc(entities::code_event::Column::Seq)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(|model| model.created_at))
}

/// Default replay cap for [`list_events`].
///
/// A session journal grows for as long as the session lives, and a client
/// that connects with `after = 0` asks for all of it. Two thousand events is
/// more than any transcript a reader scrolls through, and it bounds what one
/// reconnect can cost the server.
pub const MAX_REPLAY_EVENTS: u64 = 2_000;

/// One bounded window of a session journal.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CodeEventPage {
    /// Events in ascending sequence order.
    pub events: Vec<SequencedCodeEvent>,
    /// True when older events above the cursor were dropped to honor the cap.
    ///
    /// The window keeps the newest events, so a truncated page leaves a hole
    /// between the caller's cursor and the first event it carries. Say so
    /// rather than let a reader believe it holds the whole history.
    pub truncated: bool,
}

/// Events for one of the owner's sessions with `seq > after`, in order.
///
/// At most `limit` events come back, and the window keeps the *newest* ones:
/// a client that fell far behind resumes at the live tail instead of paying
/// for history it would scroll past. Pass [`MAX_REPLAY_EVENTS`] unless you
/// have a reason to want a different bound.
pub async fn list_events(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    after: i64,
    limit: u64,
) -> Result<CodeEventPage> {
    // Read one past the cap so a full window is distinguishable from a window
    // that happens to end exactly on it.
    let probe = limit.saturating_add(1);
    let mut rows = entities::code_event::Entity::find()
        .filter(entities::code_event::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_event::Column::SessionId.eq(session_id.0))
        .filter(entities::code_event::Column::Seq.gt(after))
        .order_by_desc(entities::code_event::Column::Seq)
        .limit(probe)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let truncated = rows.len() as u64 > limit;
    rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    rows.reverse();
    let events = rows
        .into_iter()
        .map(|model| {
            Ok(SequencedCodeEvent {
                seq: model.seq,
                event: serde_json::from_value(model.event)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(CodeEventPage { events, truncated })
}

/// Newest journal events for one session, newest first. Digests use this
/// bounded tail to identify an unresolved top-level tool without replaying a
/// long conversation on every updates-socket connection.
pub async fn list_recent_events(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    limit: u64,
) -> Result<Vec<SequencedCodeEvent>> {
    entities::code_event::Entity::find()
        .filter(entities::code_event::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_event::Column::SessionId.eq(session_id.0))
        .order_by_desc(entities::code_event::Column::Seq)
        .limit(limit)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|model| {
            Ok(SequencedCodeEvent {
                seq: model.seq,
                event: serde_json::from_value(model.event)?,
            })
        })
        .collect()
}
