//! Durable queued-message operations: messages accepted while a chat had a
//! live turn, promoted into real turns strictly FIFO once the chat is free.
//!
//! Promotion re-reads the exact FIFO head beneath the chat write lock and
//! commits admission, message, turn, and queue removal together. Queue edits,
//! reorders, and retractions use the same lock, so whichever operation wins is
//! authoritative and a stale promoter snapshot can neither execute nor delete.

use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::id::{ChatId, DocumentId, TurnId};
use crate::image::ImageRef;
use crate::model::{AgentRunStatus, QueuedTurn, TurnAdmissionLease, TurnAdmissionRequest};
use crate::storage::{AcceptTurnOutcome, PromoteQueuedTurnOutcome, ReservedQueuedTurnOutcome};

use super::super::super::{entities, store_err, DbStore};
use super::super::acquire_chat_write_lock;
use super::super::agent_run::database_now;
use super::admission;

fn queued_turn_from_model(model: entities::code_queued_turn::Model) -> Result<QueuedTurn> {
    let parse = |json: &str| -> Result<Vec<uuid::Uuid>> {
        serde_json::from_str(json)
            .map_err(|_| AgentError::Store("invalid stored queued-turn attachment list".into()))
    };
    Ok(QueuedTurn {
        id: TurnId(model.id),
        chat_id: ChatId(model.session_id),
        content: model.message,
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

fn admission_request(queued: &QueuedTurn) -> TurnAdmissionRequest {
    TurnAdmissionRequest {
        id: queued.id,
        chat_id: queued.chat_id,
        content: queued.content.clone(),
        attachments: queued.attachments.clone(),
        file_attachments: queued.file_attachments.clone(),
        invoked_skills: queued.invoked_skills.clone(),
        voice_input_used: queued.voice_input_used,
    }
}

async fn current_head_on<C>(conn: &C, chat_id: ChatId) -> Result<Option<QueuedTurn>>
where
    C: sea_orm::ConnectionTrait,
{
    entities::code_queued_turn::Entity::find()
        .filter(entities::code_queued_turn::Column::SessionId.eq(chat_id.0))
        .order_by_asc(entities::code_queued_turn::Column::Position)
        .order_by_asc(entities::code_queued_turn::Column::CreatedAt)
        .one(conn)
        .await
        .map_err(store_err)?
        .map(queued_turn_from_model)
        .transpose()
}

pub(in crate::db) async fn enqueue_turn(
    store: &DbStore,
    queued: &QueuedTurn,
) -> Result<QueuedTurn> {
    match enqueue_turn_inner(store, None, queued).await? {
        ReservedQueuedTurnOutcome::Queued(queued) => Ok(queued),
        ReservedQueuedTurnOutcome::LeaseLost => Err(AgentError::Store(format!(
            "queued turn {} has an unresolved admission owned by another process",
            queued.id
        ))),
    }
}

pub(in crate::db) async fn enqueue_reserved_turn(
    store: &DbStore,
    lease: TurnAdmissionLease,
    queued: &QueuedTurn,
) -> Result<ReservedQueuedTurnOutcome> {
    enqueue_turn_inner(store, Some(lease), queued).await
}

async fn enqueue_turn_inner(
    store: &DbStore,
    reservation: Option<TurnAdmissionLease>,
    queued: &QueuedTurn,
) -> Result<ReservedQueuedTurnOutcome> {
    if queued.id.0.is_nil() || queued.content.trim().is_empty() || queued.content.contains('\0') {
        return Err(AgentError::Store("invalid queued turn".into()));
    }
    let request = admission_request(queued);
    admission::validate_request(&request)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, queued.chat_id).await? {
        return Err(AgentError::Store(format!(
            "chat {} does not exist",
            queued.chat_id
        )));
    }
    let now = database_now(&transaction).await?;
    let _ = reservation;
    let _ = request;

    if let Some(existing) = entities::code_queued_turn::Entity::find_by_id(queued.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        // An ambiguous retry of the same enqueue is the row it made.
        let existing = queued_turn_from_model(existing)?;
        transaction.commit().await.map_err(store_err)?;
        if existing.same_request(queued) {
            return Ok(ReservedQueuedTurnOutcome::Queued(existing));
        }
        return Err(AgentError::Store(
            "queued turn id was already used with different request data".into(),
        ));
    }
    let count = entities::code_queued_turn::Entity::find()
        .filter(entities::code_queued_turn::Column::SessionId.eq(queued.chat_id.0))
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
    let position = entities::code_queued_turn::Entity::find()
        .filter(entities::code_queued_turn::Column::SessionId.eq(queued.chat_id.0))
        .order_by_desc(entities::code_queued_turn::Column::Position)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .map_or(0, |last| last.position + 1);

    let session = entities::code_session::Entity::find_by_id(queued.chat_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("session {} does not exist", queued.chat_id)))?;
    entities::code_queued_turn::ActiveModel {
        id: Set(queued.id.0),
        owner: Set(session.owner),
        session_id: Set(queued.chat_id.0),
        message: Set(queued.content.clone()),
        fingerprint: Set(Some(admission_request(queued).fingerprint().to_vec())),
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
    let inserted = entities::code_queued_turn::Entity::find_by_id(queued.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("queued turn disappeared".into()))?;
    let inserted = queued_turn_from_model(inserted)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(ReservedQueuedTurnOutcome::Queued(inserted))
}

pub(in crate::db) async fn promote_turn(
    store: &DbStore,
    expected: &QueuedTurn,
    model: &str,
    images: &[ImageRef],
) -> Result<PromoteQueuedTurnOutcome> {
    super::validate_turn_input(
        expected.id,
        model,
        &expected.content,
        &expected.invoked_skills,
    )?;
    super::message_attachment_ops::validate(images)?;
    super::message_document_attachment_ops::validate_count(
        images.len(),
        &expected.file_attachments,
    )?;
    let request = admission_request(expected);
    admission::validate_request(&request)?;

    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, expected.chat_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(PromoteQueuedTurnOutcome::Stale);
    }
    let Some(head) = current_head_on(&transaction, expected.chat_id).await? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(PromoteQueuedTurnOutcome::Stale);
    };
    if head != *expected {
        transaction.commit().await.map_err(store_err)?;
        return Ok(PromoteQueuedTurnOutcome::Stale);
    }
    let _ = request;
    let foreground = super::find_foreground_agent_run_on(&transaction, expected.chat_id)
        .await?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "chat {} has no foreground agent run",
                expected.chat_id
            ))
        })?;

    if let Some(existing) = entities::code_turn::Entity::find_by_id(expected.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let outcome = super::exact_accepted_turn_on(
            &transaction,
            existing,
            expected.chat_id,
            foreground.id,
            model,
            &expected.content,
            images,
            &expected.file_attachments,
            &expected.invoked_skills,
            expected.voice_input_used,
        )
        .await?;
        let AcceptTurnOutcome::Existing(existing) = outcome else {
            transaction.commit().await.map_err(store_err)?;
            return Ok(PromoteQueuedTurnOutcome::Stale);
        };
        delete_exact_head_on(&transaction, expected).await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(PromoteQueuedTurnOutcome::Existing(existing));
    }
    if foreground.status != AgentRunStatus::Active {
        return Err(AgentError::Store(format!(
            "chat {} foreground agent run is not active",
            expected.chat_id
        )));
    }
    if let Some(active) = super::find_active_turn_on(&transaction, expected.chat_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(PromoteQueuedTurnOutcome::ChatBusy(
            super::turn_run_from_model(active)?,
        ));
    }
    if entities::code_turn::Entity::find_by_id(expected.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some()
    {
        return Err(AgentError::Store(format!(
            "queued admission {} already has a turn",
            expected.id
        )));
    }

    let now = database_now(&transaction).await?;
    let inserted = super::insert_accepted_turn_on(
        &transaction,
        expected.id,
        expected.chat_id,
        foreground.id,
        model,
        &expected.content,
        images,
        &expected.file_attachments,
        &expected.invoked_skills,
        expected.voice_input_used,
        now,
    )
    .await?;
    delete_exact_head_on(&transaction, expected).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(PromoteQueuedTurnOutcome::Promoted(
        super::turn_run_from_model(inserted)?,
    ))
}

async fn delete_exact_head_on<C>(conn: &C, expected: &QueuedTurn) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    let deleted = entities::code_queued_turn::Entity::delete_many()
        .filter(entities::code_queued_turn::Column::Id.eq(expected.id.0))
        .filter(entities::code_queued_turn::Column::SessionId.eq(expected.chat_id.0))
        .filter(entities::code_queued_turn::Column::Position.eq(expected.position))
        .filter(entities::code_queued_turn::Column::UpdatedAt.eq(expected.updated_at))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if deleted.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "queued turn {} changed beneath its chat lock",
            expected.id
        )));
    }
    Ok(())
}

pub(in crate::db) async fn delete_turn_if_current(
    store: &DbStore,
    expected: &QueuedTurn,
) -> Result<bool> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, expected.chat_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(false);
    }
    if current_head_on(&transaction, expected.chat_id)
        .await?
        .as_ref()
        != Some(expected)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(false);
    }
    delete_exact_head_on(&transaction, expected).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(true)
}

pub(in crate::db) async fn list_queued_turns(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<QueuedTurn>> {
    entities::code_queued_turn::Entity::find()
        .filter(entities::code_queued_turn::Column::SessionId.eq(chat_id.0))
        .order_by_asc(entities::code_queued_turn::Column::Position)
        .order_by_asc(entities::code_queued_turn::Column::CreatedAt)
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
    Ok(entities::code_queued_turn::Entity::find()
        .select_only()
        .column(entities::code_queued_turn::Column::SessionId)
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
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(false);
    }
    let deleted = entities::code_queued_turn::Entity::delete_many()
        .filter(entities::code_queued_turn::Column::Id.eq(id.0))
        .filter(entities::code_queued_turn::Column::SessionId.eq(chat_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
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
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let now = database_now(&transaction).await?;
    let mut rows = entities::code_queued_turn::Entity::find()
        .filter(entities::code_queued_turn::Column::SessionId.eq(chat_id.0))
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
            if let Some(content) = content {
                active.message = Set(content.to_owned());
            }
            active.updated_at = Set(now);
        }
        active.update(&transaction).await.map_err(store_err)?;
    }
    let updated = entities::code_queued_turn::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("queued turn disappeared".into()))?;
    let updated = queued_turn_from_model(updated)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(updated))
}
