//! Image publication for a code session: authority, not upload.
//!
//! The blob store is content-addressed and owner-blind, so a blob id is a
//! capability to anyone who learns one. Chat has bound that with
//! `chat_image_publication` since it shipped; this is the code-mode
//! counterpart, and without it a session could attach any blob id it knew and
//! read the pixels back through its own image route.
//!
//! Publication also carries the validated descriptor, so turn-attachment
//! resolution can check the bytes still match what was reserved rather than
//! trusting a client to restate it.

use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set, TransactionTrait};

use crate::code::{CodeSessionId, CodeSessionLifecycle};
use crate::error::Result;
use crate::image::{ImageMediaType, ImageRef};
use crate::{AgentError, OwnerId};

use super::super::super::{entities, store_err, DbStore};
use super::super::blob as blob_ops;
use super::acquire_code_session_write_lock;

/// Reserve one validated image for one session.
///
/// Idempotent for an identical descriptor; a conflicting descriptor for the
/// same `(session_id, blob_id)` is refused rather than silently replacing what
/// an earlier turn may already reference. The session fence serializes this
/// reservation with session ending. The reservation and retirement
/// cancellation commit together, including on exact retries. Returns `true`
/// when the image is published, including an exact retry, and `false` when the
/// session lifecycle fence refuses publication.
pub async fn publish_session_image(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    image: &ImageRef,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    image.validate().map_err(|reason| {
        AgentError::Store(format!("code session image is not publishable: {reason}"))
    })?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, session_id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(false);
    }
    let session_is_live = entities::code_session::Entity::find_by_id(session_id.0)
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_session::Column::Lifecycle.ne(CodeSessionLifecycle::Ended.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some();
    if !session_is_live {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(false);
    }

    entities::code_session_image::Entity::insert(entities::code_session_image::ActiveModel {
        session_id: Set(session_id.0),
        blob_id: Set(image.blob_id),
        owner: Set(owner.as_str().to_owned()),
        media_type: Set(image.media_type.as_str().to_owned()),
        width: Set(i32::try_from(image.width).unwrap_or(i32::MAX)),
        height: Set(i32::try_from(image.height).unwrap_or(i32::MAX)),
        byte_len: Set(i64::try_from(image.byte_len).unwrap_or(i64::MAX)),
        created_at: Set(created_at),
    })
    .on_conflict_do_nothing()
    .exec_without_returning(&transaction)
    .await
    .map_err(store_err)?;
    if let Some(existing) =
        get_published_session_image_on(&transaction, owner, session_id, image.blob_id).await?
    {
        if &existing != image {
            transaction.rollback().await.map_err(store_err)?;
            return Err(AgentError::Store(format!(
                "image {} is already published to session {session_id} with a different descriptor",
                image.blob_id
            )));
        }
    } else {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "published image {} disappeared from code session {session_id}",
            image.blob_id
        )));
    }
    blob_ops::cancel_on(&transaction, image.blob_id).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(true)
}

/// Resolve one image only when it was explicitly published to this session by
/// this owner.
///
/// Another owner's publication is indistinguishable from absent, so the
/// lookup cannot even confirm the blob exists elsewhere.
pub async fn get_published_session_image(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    blob_id: uuid::Uuid,
) -> Result<Option<ImageRef>> {
    get_published_session_image_on(&store.conn, owner, session_id, blob_id).await
}

async fn get_published_session_image_on<C>(
    conn: &C,
    owner: &OwnerId,
    session_id: CodeSessionId,
    blob_id: uuid::Uuid,
) -> Result<Option<ImageRef>>
where
    C: ConnectionTrait,
{
    let Some(row) = entities::code_session_image::Entity::find()
        .filter(entities::code_session_image::Column::SessionId.eq(session_id.0))
        .filter(entities::code_session_image::Column::BlobId.eq(blob_id))
        .filter(entities::code_session_image::Column::Owner.eq(owner.as_str()))
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    let media_type = ImageMediaType::parse(&row.media_type).ok_or_else(|| {
        AgentError::Store(format!(
            "code_session_image {} has unknown media type {}",
            row.blob_id, row.media_type
        ))
    })?;
    Ok(Some(ImageRef {
        blob_id: row.blob_id,
        media_type,
        width: u32::try_from(row.width).unwrap_or(0),
        height: u32::try_from(row.height).unwrap_or(0),
        byte_len: u64::try_from(row.byte_len).unwrap_or(0),
    }))
}
