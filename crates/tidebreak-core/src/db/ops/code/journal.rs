use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};

use crate::code::{CodeEvent, CodeSessionId, SequencedCodeEvent};
use crate::error::{AgentError, Result};

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
    session_id: CodeSessionId,
    spawn_epoch: i64,
    event: &CodeEvent,
) -> std::result::Result<i64, CodeJournalError> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, session_id).await? {
        return Err(CodeJournalError::SessionNotFound { session_id });
    }
    let Some(session) = entities::code_session::Entity::find_by_id(session_id.0)
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
    let last = entities::code_event::Entity::find()
        .filter(entities::code_event::Column::SessionId.eq(session_id.0))
        .order_by_desc(entities::code_event::Column::Seq)
        .one(&transaction)
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
        session_id: Set(session_id.0),
        seq: Set(seq),
        event: Set(serde_json::to_value(event).map_err(AgentError::from)?),
        created_at: Set(Utc::now()),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(seq)
}

/// Events for a session with `seq > after`, in order.
pub async fn list_events(
    store: &DbStore,
    session_id: CodeSessionId,
    after: i64,
) -> Result<Vec<SequencedCodeEvent>> {
    entities::code_event::Entity::find()
        .filter(entities::code_event::Column::SessionId.eq(session_id.0))
        .filter(entities::code_event::Column::Seq.gt(after))
        .order_by_asc(entities::code_event::Column::Seq)
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
