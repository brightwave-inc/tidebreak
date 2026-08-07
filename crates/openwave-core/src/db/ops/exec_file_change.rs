//! The journal of what a turn's staged write-back did to a granted folder —
//! both the changes it applied and the ones it deliberately left out.
//!
//! Applied rows are the third class of live blob reference in the schema, after
//! `document.source_blob_id` and `message_attachment.blob_id`. What they retain
//! is not a copy of something the user still has: it is the *only* remaining
//! copy of bytes the agent overwrote. Much of this module exists to keep that
//! copy referenced for exactly as long as undo is offered, and to let it go
//! promptly afterwards.
//!
//! Rejected rows carry report metadata only. They share the table because they
//! share everything else: identity, turn, path, retention, and the per-turn
//! report a reader assembles from both.

use chrono::Utc;
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
    TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::id::{ChatId, TurnId};
use crate::model::{
    ExecFileChange, ExecFileChangeClassification, ExecFileRejection, ExecFileRejectionReason,
    ExecFileRejectionRecord, ExecFileSnapshot, ExecFileSnapshotRecord, ExecUndoState,
    EXEC_SNAPSHOT_RETAINED_TURNS,
};

use super::super::{entities, store_err, DbStore};
use super::blob as blob_ops;

/// Journal one turn's applied changes and prune the chat back to its undo
/// window.
///
/// The blob bytes are already published when this runs, so the insert is what
/// makes them live. Any retirement queued for them in the meantime is cancelled
/// in the same transaction, for the same reason message attachments do it: an
/// orphan sweep that ran moments before the agent's write must not delete the
/// prior copy this row now depends on.
pub(in crate::db) async fn record_snapshots(
    store: &DbStore,
    chat_id: ChatId,
    turn_id: TurnId,
    files: &[ExecFileSnapshotRecord],
) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let now = Utc::now();
    let transaction = begin_locked(store, chat_id).await?;

    let rows = files
        .iter()
        .map(|file| {
            Ok(entities::exec_file_change::ActiveModel {
                id: Set(uuid::Uuid::new_v4()),
                chat_id: Set(chat_id.0),
                turn_id: Set(turn_id.0),
                classification: Set(ExecFileChangeClassification::Applied.as_str().to_owned()),
                folder_path: Set(file.folder_path.clone()),
                relative_path: Set(file.relative_path.clone()),
                change_kind: Set(Some(file.change.as_str().to_owned())),
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
                undo_state: Set(Some(file.undo.as_str().to_owned())),
                reason: Set(None),
                recorded_at: Set(now),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    entities::exec_file_change::Entity::insert_many(rows)
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

/// Journal one turn's rejected writes and prune the chat back to its undo
/// window. These rows hold no blob, so nothing is retained or freed by them.
pub(in crate::db) async fn record_rejections(
    store: &DbStore,
    chat_id: ChatId,
    turn_id: TurnId,
    files: &[ExecFileRejectionRecord],
) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let now = Utc::now();
    let transaction = begin_locked(store, chat_id).await?;

    let rows = files
        .iter()
        .map(|file| entities::exec_file_change::ActiveModel {
            id: Set(uuid::Uuid::new_v4()),
            chat_id: Set(chat_id.0),
            turn_id: Set(turn_id.0),
            classification: Set(ExecFileChangeClassification::Rejected.as_str().to_owned()),
            folder_path: Set(file.folder_path.clone()),
            relative_path: Set(file.relative_path.clone()),
            change_kind: Set(None),
            prior_blob_id: Set(None),
            prior_byte_len: Set(None),
            new_sha256: Set(None),
            undo_state: Set(None),
            reason: Set(Some(file.reason.as_str().to_owned())),
            recorded_at: Set(now),
        });
    entities::exec_file_change::Entity::insert_many(rows)
        .exec_without_returning(&transaction)
        .await
        .map_err(store_err)?;

    prune_on(&transaction, chat_id).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(())
}

async fn begin_locked(store: &DbStore, chat_id: ChatId) -> Result<sea_orm::DatabaseTransaction> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !super::acquire_chat_write_lock(&transaction, chat_id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!("chat {chat_id} not found")));
    }
    Ok(transaction)
}

/// Drop everything outside this chat's newest [`EXEC_SNAPSHOT_RETAINED_TURNS`]
/// turns, enqueueing the blobs that just lost their last reference.
///
/// The window is over turns rather than rows, and the delete names the retained
/// turn ids rather than cutting on a timestamp: one turn writes its applied and
/// its rejected rows in two transactions, so its rows carry two `recorded_at`
/// values and a cutoff drawn from the newer one would take out the older half of
/// a turn that is still inside the window.
async fn prune_on<C>(conn: &C, chat_id: ChatId) -> Result<()>
where
    C: ConnectionTrait,
{
    let retained = entities::exec_file_change::Entity::find()
        .select_only()
        .column(entities::exec_file_change::Column::TurnId)
        .column_as(
            entities::exec_file_change::Column::RecordedAt.max(),
            "newest",
        )
        .filter(entities::exec_file_change::Column::ChatId.eq(chat_id.0))
        .group_by(entities::exec_file_change::Column::TurnId)
        .order_by_desc(entities::exec_file_change::Column::RecordedAt.max())
        .limit(u64::try_from(EXEC_SNAPSHOT_RETAINED_TURNS).unwrap_or(u64::MAX))
        .into_tuple::<(uuid::Uuid, chrono::DateTime<Utc>)>()
        .all(conn)
        .await
        .map_err(store_err)?;
    if retained.len() < EXEC_SNAPSHOT_RETAINED_TURNS {
        return Ok(());
    }
    let retained: Vec<uuid::Uuid> = retained.into_iter().map(|(turn_id, _)| turn_id).collect();

    let mut freed = entities::exec_file_change::Entity::find()
        .select_only()
        .column(entities::exec_file_change::Column::PriorBlobId)
        .distinct()
        .filter(entities::exec_file_change::Column::ChatId.eq(chat_id.0))
        .filter(entities::exec_file_change::Column::TurnId.is_not_in(retained.clone()))
        .filter(entities::exec_file_change::Column::PriorBlobId.is_not_null())
        .into_tuple::<Option<uuid::Uuid>>()
        .all(conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    entities::exec_file_change::Entity::delete_many()
        .filter(entities::exec_file_change::Column::ChatId.eq(chat_id.0))
        .filter(entities::exec_file_change::Column::TurnId.is_not_in(retained))
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
    Ok(entities::exec_file_change::Entity::find()
        .select_only()
        .column(entities::exec_file_change::Column::Id)
        .filter(entities::exec_file_change::Column::PriorBlobId.eq(blob_id))
        .into_tuple::<uuid::Uuid>()
        .one(conn)
        .await
        .map_err(store_err)?
        .is_some())
}

/// This chat's applied changes, newest first.
pub(in crate::db) async fn list_snapshots_for_chat(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<ExecFileSnapshot>> {
    list_classified(store, chat_id, ExecFileChangeClassification::Applied)
        .await?
        .into_iter()
        .map(snapshot_from_model)
        .collect()
}

/// This chat's rejected writes, newest first.
pub(in crate::db) async fn list_rejections_for_chat(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<ExecFileRejection>> {
    list_classified(store, chat_id, ExecFileChangeClassification::Rejected)
        .await?
        .into_iter()
        .map(rejection_from_model)
        .collect()
}

async fn list_classified(
    store: &DbStore,
    chat_id: ChatId,
    classification: ExecFileChangeClassification,
) -> Result<Vec<entities::exec_file_change::Model>> {
    entities::exec_file_change::Entity::find()
        .filter(entities::exec_file_change::Column::ChatId.eq(chat_id.0))
        .filter(entities::exec_file_change::Column::Classification.eq(classification.as_str()))
        .order_by_desc(entities::exec_file_change::Column::RecordedAt)
        .order_by_asc(entities::exec_file_change::Column::TurnId)
        .order_by_asc(entities::exec_file_change::Column::RelativePath)
        .all(&store.conn)
        .await
        .map_err(store_err)
}

/// Distinct prior blobs referenced by this chat's journal, ascending.
pub(in crate::db) async fn list_chat_blob_ids_on<C>(
    conn: &C,
    chat_id: ChatId,
) -> Result<Vec<uuid::Uuid>>
where
    C: ConnectionTrait,
{
    let mut blob_ids = entities::exec_file_change::Entity::find()
        .select_only()
        .column(entities::exec_file_change::Column::PriorBlobId)
        .distinct()
        .filter(entities::exec_file_change::Column::ChatId.eq(chat_id.0))
        .filter(entities::exec_file_change::Column::PriorBlobId.is_not_null())
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

/// Remove every journaled change in `chat_id`, of either classification.
/// Callers own blob retirement.
///
/// The chat foreign key is `Restrict` rather than `Cascade` precisely so this
/// runs: the rows hold the last reference to their prior-content blobs, and a
/// cascade would drop them without anything retiring what they freed.
pub(in crate::db) async fn delete_for_chat_on<C>(conn: &C, chat_id: ChatId) -> Result<()>
where
    C: ConnectionTrait,
{
    entities::exec_file_change::Entity::delete_many()
        .filter(entities::exec_file_change::Column::ChatId.eq(chat_id.0))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

fn snapshot_from_model(model: entities::exec_file_change::Model) -> Result<ExecFileSnapshot> {
    let change_kind = model
        .change_kind
        .ok_or_else(|| AgentError::Store("applied exec file change has no kind".to_owned()))?;
    let change = ExecFileChange::parse(&change_kind)
        .ok_or_else(|| AgentError::Store(format!("unknown exec file change: {change_kind}")))?;
    let undo_state = model.undo_state.ok_or_else(|| {
        AgentError::Store("applied exec file change has no undo state".to_owned())
    })?;
    let undo = ExecUndoState::parse(&undo_state)
        .ok_or_else(|| AgentError::Store(format!("unknown exec undo state: {undo_state}")))?;
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

fn rejection_from_model(model: entities::exec_file_change::Model) -> Result<ExecFileRejection> {
    let reason = model
        .reason
        .ok_or_else(|| AgentError::Store("rejected exec file change has no reason".to_owned()))?;
    let reason = ExecFileRejectionReason::parse(&reason)
        .ok_or_else(|| AgentError::Store(format!("unknown exec file rejection: {reason}")))?;
    Ok(ExecFileRejection {
        id: model.id,
        chat_id: ChatId(model.chat_id),
        turn_id: TurnId(model.turn_id),
        recorded_at: model.recorded_at,
        file: ExecFileRejectionRecord {
            folder_path: model.folder_path,
            relative_path: model.relative_path,
            reason,
        },
    })
}
