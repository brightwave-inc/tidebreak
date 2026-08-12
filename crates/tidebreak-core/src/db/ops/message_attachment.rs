//! Durable identity for the images a message was submitted with.
//!
//! Rows here are the second class of live blob reference in the schema, after
//! `document.source_blob_id`. They store identity only — a content-addressed
//! blob id plus bounded metadata — so nothing on this path can leak pixels or a
//! filesystem location.

use chrono::Utc;
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};

use crate::error::{AgentError, Result};
use crate::id::{ChatId, MessageId};
use crate::image::{ImageMediaType, ImageRef};
use crate::model::{MessageAttachment, MAX_MESSAGE_ATTACHMENTS};

use super::super::{entities, store_err, DbStore};
use super::blob as blob_ops;

/// Reject an attachment list before any of it reaches the database.
///
/// The schema range-checks the same bounds, but failing here keeps the error a
/// typed store error the product can surface, rather than a backend-specific
/// constraint violation.
pub(in crate::db) fn validate(images: &[ImageRef]) -> Result<()> {
    if images.len() > MAX_MESSAGE_ATTACHMENTS {
        return Err(AgentError::Store(format!(
            "a message may carry at most {MAX_MESSAGE_ATTACHMENTS} image attachments"
        )));
    }
    for image in images {
        if image.blob_id.is_nil() {
            return Err(AgentError::Store(
                "image attachment blob id must not be nil".into(),
            ));
        }
        image
            .validate()
            .map_err(|reason| AgentError::Store(reason.into()))?;
    }
    Ok(())
}

/// Record `images` against `message_id` in submission order.
///
/// Runs inside the caller's transaction so attachments commit atomically with
/// the message and turn that introduced them. Each blob gains a live reference
/// as of this commit, so any queued retirement for it is cancelled in the same
/// transaction — otherwise a retirement enqueued moments earlier could delete
/// bytes this message now depends on.
///
/// Blobs are visited in ascending id order for the same reason
/// [`blob_ops::replace_reference_on`] orders its mutations: the retirement
/// row locks are per blob id, and a consistent global order keeps concurrent
/// writers from deadlocking on PostgreSQL.
pub(in crate::db) async fn insert_on<C>(
    conn: &C,
    chat_id: ChatId,
    message_id: MessageId,
    images: &[ImageRef],
    now: chrono::DateTime<Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    if images.is_empty() {
        return Ok(());
    }
    validate(images)?;

    let rows = images
        .iter()
        .enumerate()
        .map(|(ordinal, image)| {
            let ordinal = i32::try_from(ordinal)
                .map_err(|_| AgentError::Store("image attachment ordinal overflow".to_owned()))?;
            Ok(entities::message_attachment::ActiveModel {
                message_id: Set(message_id.0),
                ordinal: Set(ordinal),
                chat_id: Set(chat_id.0),
                blob_id: Set(image.blob_id),
                media_type: Set(image.media_type.as_str().to_owned()),
                width: Set(i32::try_from(image.width).map_err(|_| {
                    AgentError::Store("image attachment width overflow".to_owned())
                })?),
                height: Set(i32::try_from(image.height).map_err(|_| {
                    AgentError::Store("image attachment height overflow".to_owned())
                })?),
                byte_len: Set(i64::try_from(image.byte_len).map_err(|_| {
                    AgentError::Store("image attachment byte length overflow".to_owned())
                })?),
                created_at: Set(now),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    entities::message_attachment::Entity::insert_many(rows)
        .exec_without_returning(conn)
        .await
        .map_err(store_err)?;

    let mut blob_ids: Vec<_> = images.iter().map(|image| image.blob_id).collect();
    blob_ids.sort_unstable();
    blob_ids.dedup();
    for blob_id in blob_ids {
        blob_ops::cancel_on(conn, blob_id).await?;
    }
    Ok(())
}

/// Every attachment in `chat_id`, ordered by message then submission position.
pub(in crate::db) async fn list_for_chat(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<MessageAttachment>> {
    list_for_chat_on(&store.conn, chat_id).await
}

pub(in crate::db) async fn list_for_chat_on<C>(
    conn: &C,
    chat_id: ChatId,
) -> Result<Vec<MessageAttachment>>
where
    C: ConnectionTrait,
{
    entities::message_attachment::Entity::find()
        .filter(entities::message_attachment::Column::ChatId.eq(chat_id.0))
        .order_by_asc(entities::message_attachment::Column::CreatedAt)
        .order_by_asc(entities::message_attachment::Column::MessageId)
        .order_by_asc(entities::message_attachment::Column::Ordinal)
        .all(conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(from_model)
        .collect()
}

/// The images attached to one message, in submission order.
pub(in crate::db) async fn list_for_message_on<C>(
    conn: &C,
    message_id: MessageId,
) -> Result<Vec<ImageRef>>
where
    C: ConnectionTrait,
{
    entities::message_attachment::Entity::find()
        .filter(entities::message_attachment::Column::MessageId.eq(message_id.0))
        .order_by_asc(entities::message_attachment::Column::Ordinal)
        .all(conn)
        .await
        .map_err(store_err)?
        .iter()
        .map(image_from_model)
        .collect()
}

/// Distinct blobs referenced by any attachment in `chat_id`, ascending.
///
/// Ascending order is the same global blob-id lock order the rest of the
/// retirement path uses, so a caller can enqueue retirements straight from this
/// list without risking a lock cycle.
pub(in crate::db) async fn list_chat_blob_ids_on<C>(
    conn: &C,
    chat_id: ChatId,
) -> Result<Vec<uuid::Uuid>>
where
    C: ConnectionTrait,
{
    let mut blob_ids = entities::message_attachment::Entity::find()
        .select_only()
        .column(entities::message_attachment::Column::BlobId)
        .distinct()
        .filter(entities::message_attachment::Column::ChatId.eq(chat_id.0))
        .into_tuple::<uuid::Uuid>()
        .all(conn)
        .await
        .map_err(store_err)?;
    blob_ids.sort_unstable();
    blob_ids.dedup();
    Ok(blob_ids)
}

/// Remove every attachment in `chat_id`, returning nothing.
///
/// Callers own retirement of the freed blobs; this only drops the references.
pub(in crate::db) async fn delete_for_chat_on<C>(conn: &C, chat_id: ChatId) -> Result<()>
where
    C: ConnectionTrait,
{
    entities::message_attachment::Entity::delete_many()
        .filter(entities::message_attachment::Column::ChatId.eq(chat_id.0))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

fn from_model(model: entities::message_attachment::Model) -> Result<MessageAttachment> {
    Ok(MessageAttachment {
        message_id: MessageId(model.message_id),
        chat_id: ChatId(model.chat_id),
        ordinal: model.ordinal,
        image: image_from_model(&model)?,
        created_at: model.created_at,
    })
}

fn image_from_model(model: &entities::message_attachment::Model) -> Result<ImageRef> {
    let media_type = ImageMediaType::parse(&model.media_type).ok_or_else(|| {
        AgentError::Store(format!(
            "unknown image attachment media type: {}",
            model.media_type
        ))
    })?;
    Ok(ImageRef {
        blob_id: model.blob_id,
        media_type,
        width: u32::try_from(model.width)
            .map_err(|_| AgentError::Store("image attachment width is negative".to_owned()))?,
        height: u32::try_from(model.height)
            .map_err(|_| AgentError::Store("image attachment height is negative".to_owned()))?,
        byte_len: u64::try_from(model.byte_len).map_err(|_| {
            AgentError::Store("image attachment byte length is negative".to_owned())
        })?,
    })
}
