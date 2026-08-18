use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, Set, TransactionTrait,
};

use crate::code::{
    CodeSessionId, CodeTurn, CodeTurnAttachment, CodeTurnId, CodeTurnStatus, CodeUsage, Diffstat,
};
use crate::error::{AgentError, Result};
use crate::image::ImageMediaType;
use crate::model::MAX_MESSAGE_ATTACHMENTS;

use super::super::super::{entities, store_err, DbStore};
use super::super::blob as blob_ops;

/// Insert a turn row.
pub async fn insert_turn(store: &DbStore, turn: &CodeTurn) -> Result<()> {
    validate_attachments(&turn.attachments)?;
    let txn = store.conn.begin().await.map_err(store_err)?;
    entities::code_turn::ActiveModel {
        id: Set(turn.id.0),
        session_id: Set(turn.session_id.0),
        ordinal: Set(turn.ordinal),
        status: Set(turn.status.as_str().to_owned()),
        user_input: Set(turn.user_input.clone()),
        user_input_blob_id: Set(turn.user_input_blob_id),
        checkpoint_ref: Set(turn.checkpoint_ref.clone()),
        diffstat: Set(match &turn.diffstat {
            Some(stat) => Some(serde_json::to_value(stat)?),
            None => None,
        }),
        usage: Set(match &turn.usage {
            Some(usage) => Some(serde_json::to_value(usage)?),
            None => None,
        }),
        narrative: Set(turn.narrative.clone()),
        started_at: Set(turn.started_at),
        ended_at: Set(turn.ended_at),
    }
    .insert(&txn)
    .await
    .map_err(store_err)?;
    insert_attachments_on(&txn, turn.id, &turn.attachments).await?;
    txn.commit().await.map_err(store_err)?;
    Ok(())
}

fn validate_attachments(attachments: &[CodeTurnAttachment]) -> Result<()> {
    if attachments.len() > MAX_MESSAGE_ATTACHMENTS {
        return Err(AgentError::Store(format!(
            "a code turn may carry at most {MAX_MESSAGE_ATTACHMENTS} image attachments"
        )));
    }
    for attachment in attachments {
        if attachment.blob_id.is_nil() {
            return Err(AgentError::Store(
                "code turn attachment blob id must not be nil".into(),
            ));
        }
        if attachment.byte_len == 0 {
            return Err(AgentError::Store("code turn attachment is empty".into()));
        }
        if attachment.byte_len > crate::image::MAX_IMAGE_BYTES {
            return Err(AgentError::Store(
                "code turn attachment exceeds the maximum size".into(),
            ));
        }
    }
    Ok(())
}

async fn insert_attachments_on<C>(
    conn: &C,
    turn_id: CodeTurnId,
    attachments: &[CodeTurnAttachment],
) -> Result<()>
where
    C: ConnectionTrait,
{
    if attachments.is_empty() {
        return Ok(());
    }
    let rows = attachments
        .iter()
        .enumerate()
        .map(|(ordinal, attachment)| {
            let ordinal = i32::try_from(ordinal).map_err(|_| {
                AgentError::Store("code turn attachment ordinal overflow".to_owned())
            })?;
            let byte_len = i64::try_from(attachment.byte_len).map_err(|_| {
                AgentError::Store("code turn attachment byte length overflow".to_owned())
            })?;
            Ok(entities::code_turn_attachment::ActiveModel {
                turn_id: Set(turn_id.0),
                ordinal: Set(ordinal),
                blob_id: Set(attachment.blob_id),
                media_type: Set(attachment.media_type.as_str().to_owned()),
                byte_len: Set(byte_len),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    entities::code_turn_attachment::Entity::insert_many(rows)
        .exec_without_returning(conn)
        .await
        .map_err(store_err)?;
    let mut blob_ids: Vec<_> = attachments.iter().map(|item| item.blob_id).collect();
    blob_ids.sort_unstable();
    blob_ids.dedup();
    for blob_id in blob_ids {
        blob_ops::cancel_on(conn, blob_id).await?;
    }
    Ok(())
}

async fn load_attachments_on<C>(conn: &C, turn_id: CodeTurnId) -> Result<Vec<CodeTurnAttachment>>
where
    C: ConnectionTrait,
{
    let rows = entities::code_turn_attachment::Entity::find()
        .filter(entities::code_turn_attachment::Column::TurnId.eq(turn_id.0))
        .order_by_asc(entities::code_turn_attachment::Column::Ordinal)
        .all(conn)
        .await
        .map_err(store_err)?;
    rows.into_iter()
        .map(|row| {
            let media_type = ImageMediaType::parse(&row.media_type).ok_or_else(|| {
                AgentError::Store(format!(
                    "code_turn_attachment {turn_id} has unknown media type {}",
                    row.media_type
                ))
            })?;
            let byte_len = u64::try_from(row.byte_len).map_err(|_| {
                AgentError::Store(format!(
                    "code_turn_attachment {turn_id} has a negative byte length"
                ))
            })?;
            Ok(CodeTurnAttachment {
                blob_id: row.blob_id,
                media_type,
                byte_len,
            })
        })
        .collect()
}

/// Load a turn by id.
pub async fn get_turn(store: &DbStore, id: CodeTurnId) -> Result<Option<CodeTurn>> {
    let Some(row) = entities::code_turn::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    turn_from_stored(store, row).await.map(Some)
}

/// Turns of one session, oldest first.
pub async fn list_turns(store: &DbStore, session_id: CodeSessionId) -> Result<Vec<CodeTurn>> {
    let rows = entities::code_turn::Entity::find()
        .filter(entities::code_turn::Column::SessionId.eq(session_id.0))
        .order_by_asc(entities::code_turn::Column::Ordinal)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let mut turns = Vec::with_capacity(rows.len());
    for row in rows {
        turns.push(turn_from_stored(store, row).await?);
    }
    Ok(turns)
}

/// How many turns a session has recorded.
pub async fn count_turns(store: &DbStore, session_id: CodeSessionId) -> Result<i64> {
    let count = entities::code_turn::Entity::find()
        .filter(entities::code_turn::Column::SessionId.eq(session_id.0))
        .count(&store.conn)
        .await
        .map_err(store_err)?;
    i64::try_from(count)
        .map_err(|_| AgentError::Store(format!("turn count overflow for session {session_id}")))
}

/// Most recently created turn for a session, if any.
pub async fn latest_turn(store: &DbStore, session_id: CodeSessionId) -> Result<Option<CodeTurn>> {
    let Some(row) = entities::code_turn::Entity::find()
        .filter(entities::code_turn::Column::SessionId.eq(session_id.0))
        .order_by_desc(entities::code_turn::Column::Ordinal)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    turn_from_stored(store, row).await.map(Some)
}

/// The open (non-terminal) turn for a session, if any.
pub async fn get_open_turn(store: &DbStore, session_id: CodeSessionId) -> Result<Option<CodeTurn>> {
    let turns = list_turns(store, session_id).await?;
    Ok(turns
        .into_iter()
        .rev()
        .find(|turn| turn.status == CodeTurnStatus::Running))
}

/// Next 1-based ordinal for a new turn on this session.
pub async fn next_turn_ordinal(store: &DbStore, session_id: CodeSessionId) -> Result<i64> {
    let last = entities::code_turn::Entity::find()
        .filter(entities::code_turn::Column::SessionId.eq(session_id.0))
        .order_by_desc(entities::code_turn::Column::Ordinal)
        .one(&store.conn)
        .await
        .map_err(store_err)?;
    last.map_or(Some(1), |row| row.ordinal.checked_add(1))
        .ok_or_else(|| {
            AgentError::Store(format!("turn ordinal exhausted for session {session_id}"))
        })
}

/// Persist mutable turn fields. `id`, `session_id`, `ordinal`, and `started_at` stay as stored.
pub async fn save_turn(store: &DbStore, turn: &CodeTurn) -> Result<bool> {
    let result = entities::code_turn::Entity::update_many()
        .col_expr(
            entities::code_turn::Column::Status,
            sea_orm::sea_query::Expr::value(turn.status.as_str().to_owned()),
        )
        .col_expr(
            entities::code_turn::Column::UserInput,
            sea_orm::sea_query::Expr::value(turn.user_input.clone()),
        )
        .col_expr(
            entities::code_turn::Column::UserInputBlobId,
            sea_orm::sea_query::Expr::value(turn.user_input_blob_id),
        )
        .col_expr(
            entities::code_turn::Column::CheckpointRef,
            sea_orm::sea_query::Expr::value(turn.checkpoint_ref.clone()),
        )
        .col_expr(
            entities::code_turn::Column::Diffstat,
            sea_orm::sea_query::Expr::value(match &turn.diffstat {
                Some(stat) => Some(serde_json::to_value(stat)?),
                None => None,
            }),
        )
        .col_expr(
            entities::code_turn::Column::Usage,
            sea_orm::sea_query::Expr::value(match &turn.usage {
                Some(usage) => Some(serde_json::to_value(usage)?),
                None => None,
            }),
        )
        .col_expr(
            entities::code_turn::Column::Narrative,
            sea_orm::sea_query::Expr::value(turn.narrative.clone()),
        )
        .col_expr(
            entities::code_turn::Column::EndedAt,
            sea_orm::sea_query::Expr::value(turn.ended_at),
        )
        .filter(entities::code_turn::Column::Id.eq(turn.id.0))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

async fn turn_from_stored(store: &DbStore, row: entities::code_turn::Model) -> Result<CodeTurn> {
    let mut turn = turn_from_row(row)?;
    turn.attachments = load_attachments_on(&store.conn, turn.id).await?;
    Ok(turn)
}

pub(super) fn turn_from_row(row: entities::code_turn::Model) -> Result<CodeTurn> {
    let status = CodeTurnStatus::from_str(&row.status).ok_or_else(|| {
        AgentError::Store(format!(
            "code_turn {} has unknown status {}",
            row.id, row.status
        ))
    })?;
    let diffstat =
        match row.diffstat {
            Some(value) => Some(serde_json::from_value::<Diffstat>(value).map_err(|err| {
                AgentError::Store(format!("code_turn {} diffstat: {err}", row.id))
            })?),
            None => None,
        };
    let usage = match row.usage {
        Some(value) => Some(
            serde_json::from_value::<CodeUsage>(value)
                .map_err(|err| AgentError::Store(format!("code_turn {} usage: {err}", row.id)))?,
        ),
        None => None,
    };
    Ok(CodeTurn {
        id: CodeTurnId(row.id),
        session_id: CodeSessionId(row.session_id),
        ordinal: row.ordinal,
        status,
        user_input: row.user_input,
        user_input_blob_id: row.user_input_blob_id,
        attachments: Vec::new(),
        checkpoint_ref: row.checkpoint_ref,
        diffstat,
        usage,
        narrative: row.narrative,
        started_at: row.started_at,
        ended_at: row.ended_at,
    })
}
