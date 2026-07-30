//! Durable staged writes that did not reach a granted folder.

use chrono::Utc;
use sea_orm::{
    ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::id::{ChatId, TurnId};
use crate::model::{
    ExecFileRejection, ExecFileRejectionReason, ExecFileRejectionRecord,
    EXEC_SNAPSHOT_RETAINED_TURNS,
};

use super::super::{entities, store_err, DbStore};

pub(in crate::db) async fn record(
    store: &DbStore,
    chat_id: ChatId,
    turn_id: TurnId,
    files: &[ExecFileRejectionRecord],
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
        .map(|file| entities::exec_file_rejection::ActiveModel {
            id: Set(uuid::Uuid::new_v4()),
            chat_id: Set(chat_id.0),
            turn_id: Set(turn_id.0),
            folder_path: Set(file.folder_path.clone()),
            relative_path: Set(file.relative_path.clone()),
            reason: Set(file.reason.as_str().to_owned()),
            recorded_at: Set(now),
        });
    entities::exec_file_rejection::Entity::insert_many(rows)
        .exec_without_returning(&transaction)
        .await
        .map_err(store_err)?;

    let retained = entities::exec_file_rejection::Entity::find()
        .select_only()
        .column(entities::exec_file_rejection::Column::TurnId)
        .column_as(
            entities::exec_file_rejection::Column::RecordedAt.max(),
            "newest",
        )
        .filter(entities::exec_file_rejection::Column::ChatId.eq(chat_id.0))
        .group_by(entities::exec_file_rejection::Column::TurnId)
        .order_by_desc(entities::exec_file_rejection::Column::RecordedAt.max())
        .limit(u64::try_from(EXEC_SNAPSHOT_RETAINED_TURNS).unwrap_or(u64::MAX))
        .into_tuple::<(uuid::Uuid, chrono::DateTime<Utc>)>()
        .all(&transaction)
        .await
        .map_err(store_err)?;
    if retained.len() == EXEC_SNAPSHOT_RETAINED_TURNS {
        if let Some((_, cutoff)) = retained.last().copied() {
            entities::exec_file_rejection::Entity::delete_many()
                .filter(entities::exec_file_rejection::Column::ChatId.eq(chat_id.0))
                .filter(entities::exec_file_rejection::Column::RecordedAt.lt(cutoff))
                .exec(&transaction)
                .await
                .map_err(store_err)?;
        }
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(())
}

pub(in crate::db) async fn list_for_chat(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<ExecFileRejection>> {
    entities::exec_file_rejection::Entity::find()
        .filter(entities::exec_file_rejection::Column::ChatId.eq(chat_id.0))
        .order_by_desc(entities::exec_file_rejection::Column::RecordedAt)
        .order_by_asc(entities::exec_file_rejection::Column::TurnId)
        .order_by_asc(entities::exec_file_rejection::Column::RelativePath)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|model| {
            let reason = ExecFileRejectionReason::parse(&model.reason).ok_or_else(|| {
                AgentError::Store(format!("unknown exec file rejection: {}", model.reason))
            })?;
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
        })
        .collect()
}
