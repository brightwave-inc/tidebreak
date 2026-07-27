//! Durable, monotonic semantic checkpoints for one conversation.
//!
//! Checkpoints are a bounded cache of meaning, not transcript messages. The
//! chat write lock makes their source-boundary comparison serializable across
//! workers, so a resumed older worker cannot replace a newer checkpoint.

use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set, TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::id::{ChatId, MessageId};
use crate::semantic_checkpoint::{ContextCheckpoint, SaveContextCheckpointOutcome};

use super::super::{entities, store_err, DbStore};
use super::{acquire_chat_write_lock, turn::canonical_db_timestamp};

pub(in crate::db) async fn save_context_checkpoint(
    store: &DbStore,
    checkpoint: &ContextCheckpoint,
) -> Result<SaveContextCheckpointOutcome> {
    checkpoint.validate()?;
    let created_at = canonical_db_timestamp(checkpoint.created_at)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, checkpoint.chat_id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "chat {} does not exist",
            checkpoint.chat_id
        )));
    }
    let Some(source) = entities::message::Entity::find_by_id(checkpoint.source_message_id.0)
        .filter(entities::message::Column::ChatId.eq(checkpoint.chat_id.0))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "context checkpoint source message {} does not belong to chat {}",
            checkpoint.source_message_id, checkpoint.chat_id
        )));
    };

    if let Some(existing) =
        find_context_checkpoint_model_on(&transaction, checkpoint.chat_id).await?
    {
        let current = context_checkpoint_from_model(existing.clone())?;
        if existing.source_message_seq > source.seq {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(SaveContextCheckpointOutcome::Stale(current));
        }
        if existing.source_message_seq == source.seq {
            transaction.rollback().await.map_err(store_err)?;
            return if existing.source_message_id == checkpoint.source_message_id.0
                && existing.format_version == i32::from(checkpoint.format_version)
                && existing.content == checkpoint.content
            {
                Ok(SaveContextCheckpointOutcome::Existing(current))
            } else {
                Ok(SaveContextCheckpointOutcome::Conflict(current))
            };
        }
    }

    let stored = entities::context_checkpoint::ActiveModel {
        chat_id: Set(checkpoint.chat_id.0),
        source_message_id: Set(checkpoint.source_message_id.0),
        source_message_seq: Set(source.seq),
        format_version: Set(i32::from(checkpoint.format_version)),
        content: Set(checkpoint.content.clone()),
        created_at: Set(created_at),
    };
    if find_context_checkpoint_model_on(&transaction, checkpoint.chat_id)
        .await?
        .is_some()
    {
        stored.update(&transaction).await.map_err(store_err)?;
    } else {
        stored.insert(&transaction).await.map_err(store_err)?;
    }
    let saved = require_context_checkpoint_on(&transaction, checkpoint.chat_id).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(SaveContextCheckpointOutcome::Saved(saved))
}

pub(in crate::db) async fn get_context_checkpoint(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Option<ContextCheckpoint>> {
    find_context_checkpoint_on(&store.conn, chat_id).await
}

async fn find_context_checkpoint_model_on<C>(
    conn: &C,
    chat_id: ChatId,
) -> Result<Option<entities::context_checkpoint::Model>>
where
    C: ConnectionTrait,
{
    entities::context_checkpoint::Entity::find_by_id(chat_id.0)
        .one(conn)
        .await
        .map_err(store_err)
}

async fn find_context_checkpoint_on<C>(
    conn: &C,
    chat_id: ChatId,
) -> Result<Option<ContextCheckpoint>>
where
    C: ConnectionTrait,
{
    find_context_checkpoint_model_on(conn, chat_id)
        .await?
        .map(context_checkpoint_from_model)
        .transpose()
}

async fn require_context_checkpoint_on<C>(conn: &C, chat_id: ChatId) -> Result<ContextCheckpoint>
where
    C: ConnectionTrait,
{
    find_context_checkpoint_on(conn, chat_id)
        .await?
        .ok_or_else(|| {
            AgentError::Store(format!("context checkpoint for chat {chat_id} is missing"))
        })
}

fn context_checkpoint_from_model(
    model: entities::context_checkpoint::Model,
) -> Result<ContextCheckpoint> {
    let checkpoint = ContextCheckpoint {
        chat_id: ChatId(model.chat_id),
        source_message_id: MessageId(model.source_message_id),
        format_version: u16::try_from(model.format_version).map_err(|_| {
            AgentError::Store("stored context checkpoint format version is outside u16".into())
        })?,
        content: model.content,
        created_at: model.created_at,
    };
    checkpoint.validate()?;
    Ok(checkpoint)
}
