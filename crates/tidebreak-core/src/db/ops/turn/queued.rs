//! Durable queued-message operations: messages accepted while a chat had a
//! live turn, promoted into real turns strictly FIFO once the chat is free.
//!
//! Promotion itself deliberately lives with the caller: the promoter *tries*
//! the ordinary idempotent turn acceptance under the queued row's own id and
//! deletes the row only on success, so there is no fenced two-table dance
//! here — `ChatBusy` simply leaves the row for the next attempt, and a crash
//! between acceptance and deletion re-runs into `Existing`.

use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::id::{ChatId, DocumentId, TurnId};
use crate::model::QueuedTurn;

use super::super::super::{entities, store_err, DbStore};
use super::super::agent_run::database_now;

fn queued_turn_from_model(model: entities::queued_turn::Model) -> Result<QueuedTurn> {
    let parse = |json: &str| -> Result<Vec<uuid::Uuid>> {
        serde_json::from_str(json)
            .map_err(|_| AgentError::Store("invalid stored queued-turn attachment list".into()))
    };
    Ok(QueuedTurn {
        id: TurnId(model.id),
        chat_id: ChatId(model.chat_id),
        content: model.content,
        attachments: parse(&model.attachments_json)?,
        file_attachments: parse(&model.file_attachments_json)?
            .into_iter()
            .map(DocumentId)
            .collect(),
        invoked_skills: serde_json::from_str(&model.invoked_skills_json)
            .map_err(|_| AgentError::Store("invalid stored queued-turn skill list".into()))?,
        voice_input_used: model.voice_input_used,
        position: model.position,
        created_at: model.created_at,
        updated_at: model.updated_at,
    })
}

pub(in crate::db) async fn enqueue_turn(
    store: &DbStore,
    queued: &QueuedTurn,
) -> Result<QueuedTurn> {
    if queued.id.0.is_nil() || queued.content.trim().is_empty() || queued.content.contains('\0') {
        return Err(AgentError::Store("invalid queued turn".into()));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let now = database_now(&transaction).await?;
    if let Some(existing) = entities::queued_turn::Entity::find_by_id(queued.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        // An ambiguous retry of the same enqueue is the row it made.
        let existing = queued_turn_from_model(existing)?;
        transaction.commit().await.map_err(store_err)?;
        if existing.same_request(queued) {
            return Ok(existing);
        }
        return Err(AgentError::Store(
            "queued turn id was already used with different request data".into(),
        ));
    }
    let count = entities::queued_turn::Entity::find()
        .filter(entities::queued_turn::Column::ChatId.eq(queued.chat_id.0))
        .count(&transaction)
        .await
        .map_err(store_err)?;
    if count >= QueuedTurn::MAX_PER_CHAT as u64 {
        transaction.commit().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "a chat may queue at most {} messages",
            QueuedTurn::MAX_PER_CHAT
        )));
    }
    let position = entities::queued_turn::Entity::find()
        .filter(entities::queued_turn::Column::ChatId.eq(queued.chat_id.0))
        .order_by_desc(entities::queued_turn::Column::Position)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .map_or(0, |last| last.position + 1);
    entities::queued_turn::ActiveModel {
        id: Set(queued.id.0),
        chat_id: Set(queued.chat_id.0),
        content: Set(queued.content.clone()),
        attachments_json: Set(serde_json::to_string(&queued.attachments).map_err(store_err)?),
        file_attachments_json: Set(serde_json::to_string(
            &queued
                .file_attachments
                .iter()
                .map(|id| id.0)
                .collect::<Vec<_>>(),
        )
        .map_err(store_err)?),
        invoked_skills_json: Set(serde_json::to_string(&queued.invoked_skills).map_err(store_err)?),
        voice_input_used: Set(queued.voice_input_used),
        position: Set(position),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    let inserted = entities::queued_turn::Entity::find_by_id(queued.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("queued turn disappeared".into()))?;
    let inserted = queued_turn_from_model(inserted)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(inserted)
}

pub(in crate::db) async fn list_queued_turns(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<QueuedTurn>> {
    entities::queued_turn::Entity::find()
        .filter(entities::queued_turn::Column::ChatId.eq(chat_id.0))
        .order_by_asc(entities::queued_turn::Column::Position)
        .order_by_asc(entities::queued_turn::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(queued_turn_from_model)
        .collect()
}

/// Chats that currently hold at least one queued message, for the promoter's
/// scan. Bounded output: distinct chat ids only.
pub(in crate::db) async fn chats_with_queued_turns(store: &DbStore) -> Result<Vec<ChatId>> {
    use sea_orm::QuerySelect;
    Ok(entities::queued_turn::Entity::find()
        .select_only()
        .column(entities::queued_turn::Column::ChatId)
        .distinct()
        .into_tuple::<uuid::Uuid>()
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(ChatId)
        .collect())
}

pub(in crate::db) async fn delete_queued_turn(
    store: &DbStore,
    chat_id: ChatId,
    id: TurnId,
) -> Result<bool> {
    let deleted = entities::queued_turn::Entity::delete_many()
        .filter(entities::queued_turn::Column::Id.eq(id.0))
        .filter(entities::queued_turn::Column::ChatId.eq(chat_id.0))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(deleted.rows_affected == 1)
}

/// Edit a queued message's content and/or move it to a new position. Rewrites
/// every position in the chat so the order stays dense and total.
pub(in crate::db) async fn update_queued_turn(
    store: &DbStore,
    chat_id: ChatId,
    id: TurnId,
    content: Option<&str>,
    position: Option<i32>,
) -> Result<Option<QueuedTurn>> {
    if let Some(content) = content {
        if content.trim().is_empty() || content.contains('\0') {
            return Err(AgentError::Store("invalid queued-turn content".into()));
        }
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let now = database_now(&transaction).await?;
    let mut rows = entities::queued_turn::Entity::find()
        .filter(entities::queued_turn::Column::ChatId.eq(chat_id.0))
        .order_by_asc(entities::queued_turn::Column::Position)
        .order_by_asc(entities::queued_turn::Column::CreatedAt)
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
        let mut active = entities::queued_turn::ActiveModel {
            id: Set(row.id),
            position: Set(i32::try_from(ordinal).unwrap_or(i32::MAX)),
            ..Default::default()
        };
        if is_edited {
            if let Some(content) = content {
                active.content = Set(content.to_owned());
            }
            active.updated_at = Set(now);
        }
        active.update(&transaction).await.map_err(store_err)?;
    }
    let updated = entities::queued_turn::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("queued turn disappeared".into()))?;
    let updated = queued_turn_from_model(updated)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(updated))
}
