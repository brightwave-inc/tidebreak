//! The journal of file changes a turn applied to a granted folder.
//!
//! Rows here are the third class of live blob reference in the schema, after
//! `document.source_blob_id` and `message_attachment.blob_id`. What they retain
//! is not a copy of something the user still has: it is the *only* remaining
//! copy of bytes the agent overwrote. Everything in this module exists to keep
//! that copy referenced for exactly as long as undo is offered, and to let it go
//! promptly afterwards.

use chrono::Utc;
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
    TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::id::{ChatId, TurnId};
use crate::model::{
    ExecFileChange, ExecFileSnapshot, ExecFileSnapshotRecord, ExecUndoState,
    EXEC_SNAPSHOT_RETAINED_TURNS,
};

use super::super::{entities, store_err, DbStore};
use super::blob as blob_ops;

/// Journal one turn's changes and prune the chat back to its undo window.
///
/// The blob bytes are already published when this runs, so the insert is what
/// makes them live. Any retirement queued for them in the meantime is cancelled
/// in the same transaction, for the same reason message attachments do it: an
/// orphan sweep that ran moments before the agent's write must not delete the
/// prior copy this row now depends on.
pub(in crate::db) async fn record(
    store: &DbStore,
    chat_id: ChatId,
    turn_id: TurnId,
    files: &[ExecFileSnapshotRecord],
) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let now = Utc::now();
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !super::acquire_chat_write_lock(&transaction, chat_id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!("chat {chat_id} not found")));
    }

    let rows = files
        .iter()
        .map(|file| {
            Ok(entities::exec_file_snapshot::ActiveModel {
                id: Set(uuid::Uuid::new_v4()),
                chat_id: Set(chat_id.0),
                turn_id: Set(turn_id.0),
                folder_path: Set(file.folder_path.clone()),
                relative_path: Set(file.relative_path.clone()),
                change_kind: Set(file.change.as_str().to_owned()),
                prior_blob_id: Set(file.prior_blob_id),
                prior_byte_len: Set(file
                    .prior_byte_len
                    .map(|len| {
                        i64::try_from(len).map_err(|_| {
                            AgentError::Store("prior file length exceeds i64".to_owned())
                        })
                    })
                    .transpose()?),
                new_sha256: Set(file.new_sha256.clone()),
                undo_state: Set(file.undo.as_str().to_owned()),
                recorded_at: Set(now),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    entities::exec_file_snapshot::Entity::insert_many(rows)
        .exec_without_returning(&transaction)
        .await
        .map_err(store_err)?;

    // Ascending blob-id order, the same global lock order the rest of the
    // retirement path uses, so concurrent writers cannot build a lock cycle.
    let mut blob_ids: Vec<_> = files.iter().filter_map(|file| file.prior_blob_id).collect();
    blob_ids.sort_unstable();
    blob_ids.dedup();
    for blob_id in blob_ids {
        blob_ops::cancel_on(&transaction, blob_id).await?;
    }

    prune_on(&transaction, chat_id).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(())
}

/// Drop everything outside this chat's newest [`EXEC_SNAPSHOT_RETAINED_TURNS`]
/// turns, enqueueing the blobs that just lost their last reference.
///
/// The cutoff is a timestamp rather than a turn-id list because every row one
/// turn writes shares a single `recorded_at`, so "older than the oldest retained
/// turn" and "belongs to a turn outside the window" are the same set. Two turns
/// that somehow share a timestamp are retained together, which errs toward
/// keeping an undo rather than retracting one.
async fn prune_on<C>(conn: &C, chat_id: ChatId) -> Result<()>
where
    C: ConnectionTrait,
{
    let retained = entities::exec_file_snapshot::Entity::find()
        .select_only()
        .column(entities::exec_file_snapshot::Column::TurnId)
        .column_as(
            entities::exec_file_snapshot::Column::RecordedAt.max(),
            "newest",
        )
        .filter(entities::exec_file_snapshot::Column::ChatId.eq(chat_id.0))
        .group_by(entities::exec_file_snapshot::Column::TurnId)
        .order_by_desc(entities::exec_file_snapshot::Column::RecordedAt.max())
        .limit(u64::try_from(EXEC_SNAPSHOT_RETAINED_TURNS).unwrap_or(u64::MAX))
        .into_tuple::<(uuid::Uuid, chrono::DateTime<Utc>)>()
        .all(conn)
        .await
        .map_err(store_err)?;
    let Some((_, cutoff)) = retained.last().copied() else {
        return Ok(());
    };
    if retained.len() < EXEC_SNAPSHOT_RETAINED_TURNS {
        return Ok(());
    }

    let mut freed = entities::exec_file_snapshot::Entity::find()
        .select_only()
        .column(entities::exec_file_snapshot::Column::PriorBlobId)
        .distinct()
        .filter(entities::exec_file_snapshot::Column::ChatId.eq(chat_id.0))
        .filter(entities::exec_file_snapshot::Column::RecordedAt.lt(cutoff))
        .filter(entities::exec_file_snapshot::Column::PriorBlobId.is_not_null())
        .into_tuple::<Option<uuid::Uuid>>()
        .all(conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    entities::exec_file_snapshot::Entity::delete_many()
        .filter(entities::exec_file_snapshot::Column::ChatId.eq(chat_id.0))
        .filter(entities::exec_file_snapshot::Column::RecordedAt.lt(cutoff))
        .exec(conn)
        .await
        .map_err(store_err)?;

    freed.sort_unstable();
    freed.dedup();
    for blob_id in freed {
        blob_ops::enqueue_on(conn, blob_id).await?;
    }
    Ok(())
}

/// Whether any journaled change still holds `blob_id` as its prior bytes.
pub(in crate::db) async fn references_blob_on<C>(conn: &C, blob_id: uuid::Uuid) -> Result<bool>
where
    C: ConnectionTrait,
{
    Ok(entities::exec_file_snapshot::Entity::find()
        .select_only()
        .column(entities::exec_file_snapshot::Column::Id)
        .filter(entities::exec_file_snapshot::Column::PriorBlobId.eq(blob_id))
        .into_tuple::<uuid::Uuid>()
        .one(conn)
        .await
        .map_err(store_err)?
        .is_some())
}

/// This chat's journal, newest change first.
pub(in crate::db) async fn list_for_chat(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<ExecFileSnapshot>> {
    entities::exec_file_snapshot::Entity::find()
        .filter(entities::exec_file_snapshot::Column::ChatId.eq(chat_id.0))
        .order_by_desc(entities::exec_file_snapshot::Column::RecordedAt)
        .order_by_asc(entities::exec_file_snapshot::Column::TurnId)
        .order_by_asc(entities::exec_file_snapshot::Column::RelativePath)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(from_model)
        .collect()
}

/// Distinct prior blobs referenced by this chat's journal, ascending.
pub(in crate::db) async fn list_chat_blob_ids_on<C>(
    conn: &C,
    chat_id: ChatId,
) -> Result<Vec<uuid::Uuid>>
where
    C: ConnectionTrait,
{
    let mut blob_ids = entities::exec_file_snapshot::Entity::find()
        .select_only()
        .column(entities::exec_file_snapshot::Column::PriorBlobId)
        .distinct()
        .filter(entities::exec_file_snapshot::Column::ChatId.eq(chat_id.0))
        .filter(entities::exec_file_snapshot::Column::PriorBlobId.is_not_null())
        .into_tuple::<Option<uuid::Uuid>>()
        .all(conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    blob_ids.sort_unstable();
    blob_ids.dedup();
    Ok(blob_ids)
}

/// Remove every journaled change in `chat_id`. Callers own blob retirement.
pub(in crate::db) async fn delete_for_chat_on<C>(conn: &C, chat_id: ChatId) -> Result<()>
where
    C: ConnectionTrait,
{
    entities::exec_file_snapshot::Entity::delete_many()
        .filter(entities::exec_file_snapshot::Column::ChatId.eq(chat_id.0))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

fn from_model(model: entities::exec_file_snapshot::Model) -> Result<ExecFileSnapshot> {
    let change = ExecFileChange::parse(&model.change_kind).ok_or_else(|| {
        AgentError::Store(format!("unknown exec file change: {}", model.change_kind))
    })?;
    let undo = ExecUndoState::parse(&model.undo_state).ok_or_else(|| {
        AgentError::Store(format!("unknown exec undo state: {}", model.undo_state))
    })?;
    Ok(ExecFileSnapshot {
        id: model.id,
        chat_id: ChatId(model.chat_id),
        turn_id: TurnId(model.turn_id),
        recorded_at: model.recorded_at,
        file: ExecFileSnapshotRecord {
            folder_path: model.folder_path,
            relative_path: model.relative_path,
            change,
            prior_blob_id: model.prior_blob_id,
            prior_byte_len: model
                .prior_byte_len
                .map(|len| {
                    u64::try_from(len)
                        .map_err(|_| AgentError::Store("prior file length is negative".to_owned()))
                })
                .transpose()?,
            new_sha256: model.new_sha256,
            undo,
        },
    })
}
