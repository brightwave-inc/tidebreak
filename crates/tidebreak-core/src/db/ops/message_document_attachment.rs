//! Durable links between user messages and the source documents they introduced.

use chrono::Utc;
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};

use crate::error::{AgentError, Result};
use crate::id::{ChatId, DocumentId, MessageId, TurnId};
use crate::model::{MessageDocumentAttachment, MAX_MESSAGE_ATTACHMENTS};

use super::super::{entities, source_blob_from_model, store_err, DbStore};

pub(in crate::db) fn validate_count(images: usize, documents: &[DocumentId]) -> Result<()> {
    if images.saturating_add(documents.len()) > MAX_MESSAGE_ATTACHMENTS {
        return Err(AgentError::Store(format!(
            "a message may carry at most {MAX_MESSAGE_ATTACHMENTS} attachments"
        )));
    }
    if documents.iter().any(|id| id.0.is_nil()) {
        return Err(AgentError::Store(
            "document attachment id must not be nil".into(),
        ));
    }
    let mut distinct = documents.to_vec();
    distinct.sort_unstable_by_key(|id| id.0);
    distinct.dedup();
    if distinct.len() != documents.len() {
        return Err(AgentError::Store(
            "a document may be attached to a message only once".into(),
        ));
    }
    Ok(())
}

pub(in crate::db) async fn insert_on<C>(
    conn: &C,
    chat_id: ChatId,
    turn_id: TurnId,
    message_id: MessageId,
    documents: &[DocumentId],
    now: chrono::DateTime<Utc>,
) -> Result<Vec<MessageDocumentAttachment>>
where
    C: ConnectionTrait,
{
    if documents.is_empty() {
        return Ok(Vec::new());
    }

    let owner = entities::code_session::Entity::find_by_id(chat_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .map(|session| session.owner)
        .ok_or_else(|| AgentError::Store(format!("session {chat_id} does not exist")))?;
    let next_ordinal = entities::code_turn_document_attachment::Entity::find()
        .filter(entities::code_turn_document_attachment::Column::TurnId.eq(turn_id.0))
        .order_by_desc(entities::code_turn_document_attachment::Column::Ordinal)
        .one(conn)
        .await
        .map_err(store_err)?
        .map_or(0, |row| row.ordinal.saturating_add(1));

    let mut attachments = Vec::with_capacity(documents.len());
    for (offset, &document_id) in documents.iter().enumerate() {
        let document = entities::document::Entity::find_by_id(document_id.0)
            .one(conn)
            .await
            .map_err(store_err)?
            .ok_or_else(|| {
                AgentError::Store(format!("document attachment {document_id} does not exist"))
            })?;
        if document.chat_id != Some(chat_id.0) || document.project_id.is_some() {
            return Err(AgentError::Store(format!(
                "document attachment {document_id} does not belong to chat {chat_id}"
            )));
        }
        let source_blob = source_blob_from_model(
            document.source_blob_id,
            document.source_sha256,
            document.source_byte_len,
        )?;
        let ordinal = next_ordinal.saturating_add(i32::try_from(offset).expect("bounded"));
        let attachment = MessageDocumentAttachment {
            message_id,
            chat_id,
            ordinal,
            document_id,
            title: document.title,
            media_type: document.media_type,
            source_blob,
            readable: !document.canonical_text.is_empty(),
            created_at: now,
        };
        attachment
            .validate()
            .map_err(|reason| AgentError::Store(reason.into()))?;
        attachments.push(attachment);
    }

    let rows = attachments
        .iter()
        .map(
            |attachment| entities::code_turn_document_attachment::ActiveModel {
                turn_id: Set(turn_id.0),
                ordinal: Set(attachment.ordinal),
                owner: Set(owner.clone()),
                message_id: Set(Some(attachment.message_id.0)),
                document_id: Set(attachment.document_id.0),
                created_at: Set(attachment.created_at),
            },
        )
        .collect::<Vec<_>>();
    entities::code_turn_document_attachment::Entity::insert_many(rows)
        .exec_without_returning(conn)
        .await
        .map_err(store_err)?;
    Ok(attachments)
}

pub(in crate::db) async fn list_for_chat(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<MessageDocumentAttachment>> {
    list_for_chat_on(&store.conn, chat_id).await
}

pub(in crate::db) async fn list_for_chat_on<C>(
    conn: &C,
    chat_id: ChatId,
) -> Result<Vec<MessageDocumentAttachment>>
where
    C: ConnectionTrait,
{
    let turn_ids = entities::code_turn::Entity::find()
        .select_only()
        .column(entities::code_turn::Column::Id)
        .filter(entities::code_turn::Column::SessionId.eq(chat_id.0))
        .into_tuple::<uuid::Uuid>()
        .all(conn)
        .await
        .map_err(store_err)?;
    if turn_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = entities::code_turn_document_attachment::Entity::find()
        .filter(entities::code_turn_document_attachment::Column::TurnId.is_in(turn_ids))
        .filter(entities::code_turn_document_attachment::Column::MessageId.is_not_null())
        .order_by_asc(entities::code_turn_document_attachment::Column::CreatedAt)
        .order_by_asc(entities::code_turn_document_attachment::Column::MessageId)
        .order_by_asc(entities::code_turn_document_attachment::Column::Ordinal)
        .all(conn)
        .await
        .map_err(store_err)?;
    let mut attachments = Vec::with_capacity(rows.len());
    for row in rows {
        let document = entities::document::Entity::find_by_id(row.document_id)
            .one(conn)
            .await
            .map_err(store_err)?
            .ok_or_else(|| {
                AgentError::Store(format!(
                    "document attachment {} has no document",
                    row.document_id
                ))
            })?;
        attachments.push(from_models(row, document, chat_id)?);
    }
    Ok(attachments)
}

pub(in crate::db) async fn list_ids_for_message_on<C>(
    conn: &C,
    message_id: MessageId,
) -> Result<Vec<DocumentId>>
where
    C: ConnectionTrait,
{
    entities::code_turn_document_attachment::Entity::find()
        .filter(entities::code_turn_document_attachment::Column::MessageId.eq(message_id.0))
        .order_by_asc(entities::code_turn_document_attachment::Column::Ordinal)
        .all(conn)
        .await
        .map_err(store_err)
        .map(|rows| {
            rows.into_iter()
                .map(|row| DocumentId(row.document_id))
                .collect()
        })
}

fn from_models(
    row: entities::code_turn_document_attachment::Model,
    document: entities::document::Model,
    chat_id: ChatId,
) -> Result<MessageDocumentAttachment> {
    let source_blob = source_blob_from_model(
        document.source_blob_id,
        document.source_sha256,
        document.source_byte_len,
    )?;
    let readable = !document.canonical_text.is_empty();
    let message_id = row.message_id.ok_or_else(|| {
        AgentError::Store("code turn document attachment is missing a transcript message".into())
    })?;
    let attachment = MessageDocumentAttachment {
        message_id: MessageId(message_id),
        chat_id,
        ordinal: row.ordinal,
        document_id: DocumentId(row.document_id),
        title: document.title,
        media_type: document.media_type,
        source_blob,
        readable,
        created_at: row.created_at,
    };
    attachment
        .validate()
        .map_err(|reason| AgentError::Store(reason.into()))?;
    Ok(attachment)
}
