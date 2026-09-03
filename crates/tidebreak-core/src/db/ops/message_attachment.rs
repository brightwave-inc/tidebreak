//! Durable identity for the images a message was submitted with.
//!
//! Rows live on `turn_attachment` with a nullable `message_id` so the
//! worker can read by turn and the transcript can read by message.

use chrono::{DateTime, Utc};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};

use crate::error::{AgentError, Result};
use crate::id::{MessageId, SessionId, TurnId};
use crate::image::{ImageMediaType, ImageRef};
use crate::model::{MessageAttachment, MAX_MESSAGE_ATTACHMENTS};

use super::super::{entities, store_err, DbStore};
use super::blob as blob_ops;

/// Reject an attachment list before any of it reaches the database.
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
pub(in crate::db) async fn insert_on<C>(
    conn: &C,
    chat_id: SessionId,
    turn_id: TurnId,
    message_id: MessageId,
    images: &[ImageRef],
    _now: chrono::DateTime<Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    if images.is_empty() {
        return Ok(());
    }
    validate(images)?;
    let owner = session_owner_on(conn, chat_id).await?;
    let next_ordinal = next_ordinal_on(conn, turn_id).await?;

    let rows = images
        .iter()
        .enumerate()
        .map(|(offset, image)| {
            let ordinal =
                next_ordinal.saturating_add(i32::try_from(offset).map_err(|_| {
                    AgentError::Store("image attachment ordinal overflow".to_owned())
                })?);
            Ok(entities::turn_attachment::ActiveModel {
                turn_id: Set(turn_id.0),
                ordinal: Set(ordinal),
                owner: Set(owner.clone()),
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
                message_id: Set(Some(message_id.0)),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    entities::turn_attachment::Entity::insert_many(rows)
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

/// Bind a turn's existing image rows to its user transcript message.
pub(in crate::db) async fn bind_turn_to_message_on<C>(
    conn: &C,
    turn_id: TurnId,
    message_id: MessageId,
) -> Result<Vec<ImageRef>>
where
    C: ConnectionTrait,
{
    let rows = entities::turn_attachment::Entity::find()
        .filter(entities::turn_attachment::Column::TurnId.eq(turn_id.0))
        .order_by_asc(entities::turn_attachment::Column::Ordinal)
        .all(conn)
        .await
        .map_err(store_err)?;
    let mut unbound = 0_u64;
    let mut images = Vec::with_capacity(rows.len());
    for row in &rows {
        match row.message_id {
            None => unbound = unbound.saturating_add(1),
            Some(bound) if bound == message_id.0 => {}
            Some(bound) => {
                return Err(AgentError::Store(format!(
                    "turn {turn_id} attachment is already bound to message {bound}"
                )))
            }
        }
        images.push(image_from_model(row)?);
    }
    if unbound > 0 {
        let updated = entities::turn_attachment::Entity::update_many()
            .col_expr(
                entities::turn_attachment::Column::MessageId,
                sea_orm::sea_query::Expr::value(Some(message_id.0)),
            )
            .filter(entities::turn_attachment::Column::TurnId.eq(turn_id.0))
            .filter(entities::turn_attachment::Column::MessageId.is_null())
            .exec(conn)
            .await
            .map_err(store_err)?;
        if updated.rows_affected != unbound {
            return Err(AgentError::Store(format!(
                "turn {turn_id} attachments changed while binding the transcript message"
            )));
        }
    }
    Ok(images)
}

/// Every attachment in `chat_id`, ordered by message then submission position.
pub(in crate::db) async fn list_for_chat(
    store: &DbStore,
    chat_id: SessionId,
) -> Result<Vec<MessageAttachment>> {
    list_for_chat_on(&store.conn, chat_id).await
}

pub(in crate::db) async fn list_for_chat_on<C>(
    conn: &C,
    chat_id: SessionId,
) -> Result<Vec<MessageAttachment>>
where
    C: ConnectionTrait,
{
    let turn_ids = turn_ids_for_session_on(conn, chat_id).await?;
    if turn_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = entities::turn_attachment::Entity::find()
        .filter(entities::turn_attachment::Column::TurnId.is_in(turn_ids))
        .filter(entities::turn_attachment::Column::MessageId.is_not_null())
        .order_by_asc(entities::turn_attachment::Column::MessageId)
        .order_by_asc(entities::turn_attachment::Column::Ordinal)
        .all(conn)
        .await
        .map_err(store_err)?;
    rows.into_iter()
        .map(|row| from_model(row, chat_id))
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
    entities::turn_attachment::Entity::find()
        .filter(entities::turn_attachment::Column::MessageId.eq(message_id.0))
        .order_by_asc(entities::turn_attachment::Column::Ordinal)
        .all(conn)
        .await
        .map_err(store_err)?
        .iter()
        .map(image_from_model)
        .collect()
}

/// Distinct blobs referenced by any attachment in `chat_id`, ascending.
pub(in crate::db) async fn list_chat_blob_ids_on<C>(
    conn: &C,
    chat_id: SessionId,
) -> Result<Vec<uuid::Uuid>>
where
    C: ConnectionTrait,
{
    let turn_ids = turn_ids_for_session_on(conn, chat_id).await?;
    if turn_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut blob_ids = entities::turn_attachment::Entity::find()
        .select_only()
        .column(entities::turn_attachment::Column::BlobId)
        .distinct()
        .filter(entities::turn_attachment::Column::TurnId.is_in(turn_ids))
        .into_tuple::<uuid::Uuid>()
        .all(conn)
        .await
        .map_err(store_err)?;
    blob_ids.sort_unstable();
    blob_ids.dedup();
    Ok(blob_ids)
}

/// Remove every attachment in `chat_id`, returning nothing.
pub(in crate::db) async fn delete_for_chat_on<C>(conn: &C, chat_id: SessionId) -> Result<()>
where
    C: ConnectionTrait,
{
    let turn_ids = turn_ids_for_session_on(conn, chat_id).await?;
    if turn_ids.is_empty() {
        return Ok(());
    }
    entities::turn_attachment::Entity::delete_many()
        .filter(entities::turn_attachment::Column::TurnId.is_in(turn_ids))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

async fn session_owner_on<C>(conn: &C, chat_id: SessionId) -> Result<String>
where
    C: ConnectionTrait,
{
    entities::session::Entity::find_by_id(chat_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .map(|session| session.owner)
        .ok_or_else(|| AgentError::Store(format!("session {chat_id} does not exist")))
}

async fn turn_ids_for_session_on<C>(conn: &C, chat_id: SessionId) -> Result<Vec<uuid::Uuid>>
where
    C: ConnectionTrait,
{
    entities::turn::Entity::find()
        .select_only()
        .column(entities::turn::Column::Id)
        .filter(entities::turn::Column::SessionId.eq(chat_id.0))
        .into_tuple::<uuid::Uuid>()
        .all(conn)
        .await
        .map_err(store_err)
}

async fn next_ordinal_on<C>(conn: &C, turn_id: TurnId) -> Result<i32>
where
    C: ConnectionTrait,
{
    let last = entities::turn_attachment::Entity::find()
        .filter(entities::turn_attachment::Column::TurnId.eq(turn_id.0))
        .order_by_desc(entities::turn_attachment::Column::Ordinal)
        .one(conn)
        .await
        .map_err(store_err)?;
    Ok(last.map_or(0, |row| row.ordinal.saturating_add(1)))
}

fn from_model(
    model: entities::turn_attachment::Model,
    chat_id: SessionId,
) -> Result<MessageAttachment> {
    let message_id = model.message_id.ok_or_else(|| {
        AgentError::Store("turn attachment is missing a transcript message".into())
    })?;
    Ok(MessageAttachment {
        message_id: MessageId(message_id),
        chat_id,
        ordinal: model.ordinal,
        image: image_from_model(&model)?,
        created_at: DateTime::<Utc>::UNIX_EPOCH,
    })
}

fn image_from_model(model: &entities::turn_attachment::Model) -> Result<ImageRef> {
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
