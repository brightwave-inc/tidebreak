//! Durable per-chat authority for published image blobs.
//!
//! Blob identity is global and content-addressed, but publication authority is
//! not: each chat that may attach a blob owns its own reservation row. The row
//! also keeps the blob live until chat deletion and records the validated image
//! descriptor that later resolution must match.

use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QuerySelect, Set, TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::id::ChatId;
use crate::image::{ImageMediaType, ImageRef};
use crate::model::OwnerId;

use super::super::{entities, store_err, DbStore};
use super::acquire_chat_write_lock;
use super::blob as blob_ops;

/// Establish `chat_id`'s authority to attach `image`.
///
/// The chat fence serializes publication with chat deletion. Exact retries use
/// the composite primary key and recover the original row, while a descriptor
/// mismatch for the same content id fails closed. Cancelling blob retirement in
/// the same transaction makes the reservation a durable live reference.
pub(in crate::db) async fn publish(
    store: &DbStore,
    chat_id: ChatId,
    image: &ImageRef,
    owner: Option<&OwnerId>,
) -> Result<bool> {
    if image.blob_id.is_nil() {
        return Err(AgentError::Store(
            "published image blob id must not be nil".into(),
        ));
    }
    image
        .validate()
        .map_err(|reason| AgentError::Store(reason.into()))?;

    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(false);
    }
    if let Some(owner) = owner {
        let owned = entities::chat::Entity::find_by_id(chat_id.0)
            .filter(entities::chat::Column::Owner.eq(owner.as_str()))
            .one(&transaction)
            .await
            .map_err(store_err)?
            .is_some();
        if !owned {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(false);
        }
    }

    entities::chat_image_publication::Entity::insert(
        entities::chat_image_publication::ActiveModel {
            chat_id: Set(chat_id.0),
            blob_id: Set(image.blob_id),
            media_type: Set(image.media_type.as_str().to_owned()),
            width: Set(i32::try_from(image.width)
                .map_err(|_| AgentError::Store("published image width overflow".into()))?),
            height: Set(i32::try_from(image.height)
                .map_err(|_| AgentError::Store("published image height overflow".into()))?),
            byte_len: Set(i64::try_from(image.byte_len)
                .map_err(|_| AgentError::Store("published image byte length overflow".into()))?),
            created_at: Set(chrono::Utc::now()),
        },
    )
    .on_conflict_do_nothing()
    .exec_without_returning(&transaction)
    .await
    .map_err(store_err)?;

    let stored = entities::chat_image_publication::Entity::find_by_id((chat_id.0, image.blob_id))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "published image reservation disappeared for chat {chat_id} and blob {}",
                image.blob_id
            ))
        })?;
    if image_from_model(&stored)? != *image {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "published image {} has conflicting metadata in chat {chat_id}",
            image.blob_id
        )));
    }

    blob_ops::cancel_on(&transaction, image.blob_id).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(true)
}

/// Resolve one exact publication reservation for `chat_id`.
pub(in crate::db) async fn get(
    store: &DbStore,
    chat_id: ChatId,
    blob_id: uuid::Uuid,
) -> Result<Option<ImageRef>> {
    entities::chat_image_publication::Entity::find_by_id((chat_id.0, blob_id))
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .as_ref()
        .map(image_from_model)
        .transpose()
}

/// Require every submitted image to have an exact reservation in `chat_id`.
///
/// Callers hold the chat write lock while this runs, so publication and chat
/// deletion cannot change the authority being checked before the caller's
/// transaction commits its new references.
pub(in crate::db) async fn require_exact_on<C>(
    conn: &C,
    chat_id: ChatId,
    images: &[ImageRef],
) -> Result<()>
where
    C: ConnectionTrait,
{
    for image in images {
        let Some(stored) =
            entities::chat_image_publication::Entity::find_by_id((chat_id.0, image.blob_id))
                .one(conn)
                .await
                .map_err(store_err)?
        else {
            return Err(AgentError::Store(format!(
                "image {} is not published for chat {chat_id}",
                image.blob_id
            )));
        };
        if image_from_model(&stored)? != *image {
            return Err(AgentError::Store(format!(
                "published image {} has conflicting metadata in chat {chat_id}",
                image.blob_id
            )));
        }
    }
    Ok(())
}

/// Distinct publication blobs reserved by `chat_id`, ascending.
pub(in crate::db) async fn list_chat_blob_ids_on<C>(
    conn: &C,
    chat_id: ChatId,
) -> Result<Vec<uuid::Uuid>>
where
    C: ConnectionTrait,
{
    let mut blob_ids = entities::chat_image_publication::Entity::find()
        .select_only()
        .column(entities::chat_image_publication::Column::BlobId)
        .distinct()
        .filter(entities::chat_image_publication::Column::ChatId.eq(chat_id.0))
        .into_tuple::<uuid::Uuid>()
        .all(conn)
        .await
        .map_err(store_err)?;
    blob_ids.sort_unstable();
    blob_ids.dedup();
    Ok(blob_ids)
}

/// Drop every publication reservation owned by `chat_id`.
///
/// The chat deletion transaction owns retirement of the returned/freed blobs;
/// this helper only removes references.
pub(in crate::db) async fn delete_for_chat_on<C>(conn: &C, chat_id: ChatId) -> Result<()>
where
    C: ConnectionTrait,
{
    entities::chat_image_publication::Entity::delete_many()
        .filter(entities::chat_image_publication::Column::ChatId.eq(chat_id.0))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

fn image_from_model(model: &entities::chat_image_publication::Model) -> Result<ImageRef> {
    let media_type = ImageMediaType::parse(&model.media_type).ok_or_else(|| {
        AgentError::Store(format!(
            "unknown published image media type: {}",
            model.media_type
        ))
    })?;
    Ok(ImageRef {
        blob_id: model.blob_id,
        media_type,
        width: u32::try_from(model.width)
            .map_err(|_| AgentError::Store("published image width is negative".into()))?,
        height: u32::try_from(model.height)
            .map_err(|_| AgentError::Store("published image height is negative".into()))?,
        byte_len: u64::try_from(model.byte_len)
            .map_err(|_| AgentError::Store("published image byte length is negative".into()))?,
    })
}
