//! Durable links between user messages and the source documents they introduced.

use chrono::Utc;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set};

use crate::error::{AgentError, Result};
use crate::id::{ChatId, DocumentId, MessageId};
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

    let mut attachments = Vec::with_capacity(documents.len());
    for (ordinal, &document_id) in documents.iter().enumerate() {
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
        let attachment = MessageDocumentAttachment {
            message_id,
            chat_id,
            ordinal: i32::try_from(ordinal).expect("attachment count is bounded"),
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
            |attachment| entities::message_document_attachment::ActiveModel {
                message_id: Set(attachment.message_id.0),
                ordinal: Set(attachment.ordinal),
                chat_id: Set(attachment.chat_id.0),
                document_id: Set(attachment.document_id.0),
                created_at: Set(attachment.created_at),
            },
        )
        .collect::<Vec<_>>();
    entities::message_document_attachment::Entity::insert_many(rows)
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
    entities::message_document_attachment::Entity::find()
        .find_also_related(entities::document::Entity)
        .filter(entities::message_document_attachment::Column::ChatId.eq(chat_id.0))
        .order_by_asc(entities::message_document_attachment::Column::CreatedAt)
        .order_by_asc(entities::message_document_attachment::Column::MessageId)
        .order_by_asc(entities::message_document_attachment::Column::Ordinal)
        .all(conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|(attachment, document)| {
            let document = document.ok_or_else(|| {
                AgentError::Store(format!(
                    "document attachment {} has no document",
                    attachment.document_id
                ))
            })?;
            from_models(attachment, document)
        })
        .collect()
}

pub(in crate::db) async fn list_ids_for_message_on<C>(
    conn: &C,
    message_id: MessageId,
) -> Result<Vec<DocumentId>>
where
    C: ConnectionTrait,
{
    entities::message_document_attachment::Entity::find()
        .filter(entities::message_document_attachment::Column::MessageId.eq(message_id.0))
        .order_by_asc(entities::message_document_attachment::Column::Ordinal)
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
    row: entities::message_document_attachment::Model,
    document: entities::document::Model,
) -> Result<MessageDocumentAttachment> {
    let source_blob = source_blob_from_model(
        document.source_blob_id,
        document.source_sha256,
        document.source_byte_len,
    )?;
    let readable = !document.canonical_text.is_empty();
    let attachment = MessageDocumentAttachment {
        message_id: MessageId(row.message_id),
        chat_id: ChatId(row.chat_id),
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
