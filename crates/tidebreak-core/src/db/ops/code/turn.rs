use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, TransactionTrait,
};

use crate::code::{CodeSessionId, CodeTurn, CodeTurnId, CodeTurnStatus, CodeUsage, Diffstat};
use crate::error::{AgentError, Result};
use crate::image::ImageMediaType;
use crate::image::ImageRef;
use crate::model::MAX_MESSAGE_ATTACHMENTS;
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};
use super::super::blob as blob_ops;

/// The fields analytics needs from one turn.
///
/// Keeping this projection separate from [`CodeTurn`] avoids loading image
/// attachment rows for a report that never reads them.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeTurnMetric {
    pub session_id: CodeSessionId,
    pub status: CodeTurnStatus,
    pub model: Option<String>,
    pub fast_mode: bool,
    pub usage: Option<CodeUsage>,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

/// Insert a turn row under its session's owner.
pub async fn insert_turn(store: &DbStore, owner: &OwnerId, turn: &CodeTurn) -> Result<()> {
    validate_attachments(&turn.attachments)?;
    let txn = store.conn.begin().await.map_err(store_err)?;
    insert_turn_on(&txn, owner, turn).await?;
    txn.commit().await.map_err(store_err)?;
    Ok(())
}

pub(in crate::db) async fn insert_turn_on<C>(
    conn: &C,
    owner: &OwnerId,
    turn: &CodeTurn,
) -> Result<()>
where
    C: ConnectionTrait,
{
    validate_attachments(&turn.attachments)?;
    entities::code_turn::ActiveModel {
        id: Set(turn.id.0),
        owner: Set(owner.as_str().to_owned()),
        session_id: Set(turn.session_id.0),
        ordinal: Set(turn.ordinal),
        status: Set(turn.status.as_str().to_owned()),
        model: Set(turn.model.clone()),
        fast_mode: Set(turn.fast_mode),
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
        rewrite: Set(turn.rewrite.clone()),
        started_at: Set(turn.started_at),
        ended_at: Set(turn.ended_at),
        park_ref: Set(turn.park_ref.clone()),
        park_wait: Set(match &turn.park_wait {
            Some(wait) => Some(serde_json::to_value(wait)?),
            None => None,
        }),
        attempt_count: Set(0),
        max_attempts: Set(crate::model::TurnRun::DEFAULT_MAX_ATTEMPTS),
        claim_count: Set(0),
        model_steps: Set(0),
        input_tokens: Set(0),
        output_tokens: Set(0),
        cache_read_input_tokens: Set(0),
        cache_creation_input_tokens: Set(0),
        available_at: Set(Some(turn.started_at)),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        last_error_code: Set(None),
        last_error_detail: Set(None),
        steer_revision: Set(0),
        last_steer_applied_at: Set(None),
        invoked_skills: Set(serde_json::json!([])),
        voice_input_used: Set(false),
        input_message_id: Set(None),
        output_message_id: Set(None),
        updated_at: Set(Some(turn.started_at)),
        fingerprint: Set(None),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    insert_attachments_on(conn, owner, turn.id, &turn.attachments).await?;
    Ok(())
}

fn validate_attachments(attachments: &[ImageRef]) -> Result<()> {
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
    owner: &OwnerId,
    turn_id: CodeTurnId,
    attachments: &[ImageRef],
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
                owner: Set(owner.as_str().to_owned()),
                ordinal: Set(ordinal),
                blob_id: Set(attachment.blob_id),
                media_type: Set(attachment.media_type.as_str().to_owned()),
                width: Set(i32::try_from(attachment.width).unwrap_or(i32::MAX)),
                height: Set(i32::try_from(attachment.height).unwrap_or(i32::MAX)),
                byte_len: Set(byte_len),
                message_id: Set(None),
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

async fn load_attachments_on<C>(
    conn: &C,
    owner: &OwnerId,
    turn_id: CodeTurnId,
) -> Result<Vec<ImageRef>>
where
    C: ConnectionTrait,
{
    let rows = entities::code_turn_attachment::Entity::find()
        .filter(entities::code_turn_attachment::Column::Owner.eq(owner.as_str()))
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
            Ok(ImageRef {
                blob_id: row.blob_id,
                media_type,
                width: u32::try_from(row.width).unwrap_or(0),
                height: u32::try_from(row.height).unwrap_or(0),
                byte_len,
            })
        })
        .collect()
}

/// Load one of the owner's turns by id.
pub async fn get_turn(
    store: &DbStore,
    owner: &OwnerId,
    id: CodeTurnId,
) -> Result<Option<CodeTurn>> {
    let Some(row) = entities::code_turn::Entity::find_by_id(id.0)
        .filter(entities::code_turn::Column::Owner.eq(owner.as_str()))
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    turn_from_stored(store, owner, row).await.map(Some)
}

/// Turns of one of the owner's sessions, oldest first.
pub async fn list_turns(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
) -> Result<Vec<CodeTurn>> {
    let rows = entities::code_turn::Entity::find()
        .filter(entities::code_turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_turn::Column::SessionId.eq(session_id.0))
        .order_by_asc(entities::code_turn::Column::Ordinal)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let mut turns = Vec::with_capacity(rows.len());
    for row in rows {
        turns.push(turn_from_stored(store, owner, row).await?);
    }
    Ok(turns)
}

/// Every turn that belongs to one owner, newest first, projected for reports.
pub async fn list_turn_metrics(store: &DbStore, owner: &OwnerId) -> Result<Vec<CodeTurnMetric>> {
    let rows = entities::code_turn::Entity::find()
        .select_only()
        .column(entities::code_turn::Column::Id)
        .column(entities::code_turn::Column::SessionId)
        .column(entities::code_turn::Column::Status)
        .column(entities::code_turn::Column::Model)
        .column(entities::code_turn::Column::FastMode)
        .column(entities::code_turn::Column::Usage)
        .column(entities::code_turn::Column::StartedAt)
        .filter(entities::code_turn::Column::Owner.eq(owner.as_str()))
        .order_by_desc(entities::code_turn::Column::StartedAt)
        .into_tuple::<(
            uuid::Uuid,
            uuid::Uuid,
            String,
            Option<String>,
            bool,
            Option<serde_json::Value>,
            chrono::DateTime<chrono::Utc>,
        )>()
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    rows.into_iter()
        .map(
            |(id, session_id, status, model, fast_mode, usage, started_at)| {
                Ok(CodeTurnMetric {
                    session_id: CodeSessionId(session_id),
                    status: turn_status_from_stored(id, &status)?,
                    model,
                    fast_mode,
                    usage: code_usage_from_stored(id, usage)?,
                    started_at,
                })
            },
        )
        .collect()
}

/// How many turns one of the owner's sessions has recorded.
pub async fn count_turns(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
) -> Result<i64> {
    let count = entities::code_turn::Entity::find()
        .filter(entities::code_turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_turn::Column::SessionId.eq(session_id.0))
        .count(&store.conn)
        .await
        .map_err(store_err)?;
    i64::try_from(count)
        .map_err(|_| AgentError::Store(format!("turn count overflow for session {session_id}")))
}

/// Most recently created turn for one of the owner's sessions, if any.
pub async fn latest_turn(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
) -> Result<Option<CodeTurn>> {
    let Some(row) = entities::code_turn::Entity::find()
        .filter(entities::code_turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_turn::Column::SessionId.eq(session_id.0))
        .order_by_desc(entities::code_turn::Column::Ordinal)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    turn_from_stored(store, owner, row).await.map(Some)
}

/// The open (non-terminal) turn for one of the owner's sessions, if any.
pub async fn get_open_turn(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
) -> Result<Option<CodeTurn>> {
    let turns = list_turns(store, owner, session_id).await?;
    Ok(turns.into_iter().rev().find(|turn| turn.status.is_open()))
}

/// Next 1-based ordinal for a new turn on one of the owner's sessions.
pub async fn next_turn_ordinal(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
) -> Result<i64> {
    let last = entities::code_turn::Entity::find()
        .filter(entities::code_turn::Column::Owner.eq(owner.as_str()))
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
///
/// `narrative` and `rewrite` are deliberately not among them. Both are derived
/// asynchronously after a turn ends and can land at any point, while the
/// callers here hold a [`CodeTurn`] read before that — `checkpoint::after_turn_ended`
/// takes its snapshot before the turn is even terminal. Writing the whole row
/// from a stale snapshot would blank a recap or rewrite that had already been
/// stored, so each column has exactly one writer: [`set_turn_narrative`] and
/// [`set_turn_rewrite`].
pub async fn save_turn(store: &DbStore, owner: &OwnerId, turn: &CodeTurn) -> Result<bool> {
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
            entities::code_turn::Column::EndedAt,
            sea_orm::sea_query::Expr::value(turn.ended_at),
        )
        .col_expr(
            entities::code_turn::Column::ParkRef,
            sea_orm::sea_query::Expr::value(turn.park_ref.clone()),
        )
        .col_expr(
            entities::code_turn::Column::ParkWait,
            sea_orm::sea_query::Expr::value(match &turn.park_wait {
                Some(wait) => Some(serde_json::to_value(wait)?),
                None => None,
            }),
        )
        .filter(entities::code_turn::Column::Id.eq(turn.id.0))
        .filter(entities::code_turn::Column::Owner.eq(owner.as_str()))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Store one turn's derived narrative, touching no other column.
///
/// Targeted for the reason [`save_turn`] documents: this lands while other
/// writers hold a `CodeTurn` read before the narrative existed, so it must not
/// be carried on a whole-row write in either direction.
pub async fn set_turn_narrative(
    store: &DbStore,
    owner: &OwnerId,
    id: CodeTurnId,
    narrative: &str,
) -> Result<bool> {
    let result = entities::code_turn::Entity::update_many()
        .col_expr(
            entities::code_turn::Column::Narrative,
            sea_orm::sea_query::Expr::value(Some(narrative.to_owned())),
        )
        .filter(entities::code_turn::Column::Id.eq(id.0))
        .filter(entities::code_turn::Column::Owner.eq(owner.as_str()))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Store one turn's derived rewrite, touching no other column.
///
/// Targeted for the reason [`save_turn`] documents: this lands while other
/// writers hold a `CodeTurn` read before the rewrite existed, so it must not
/// be carried on a whole-row write in either direction.
pub async fn set_turn_rewrite(
    store: &DbStore,
    owner: &OwnerId,
    id: CodeTurnId,
    rewrite: &str,
) -> Result<bool> {
    let result = entities::code_turn::Entity::update_many()
        .col_expr(
            entities::code_turn::Column::Rewrite,
            sea_orm::sea_query::Expr::value(Some(rewrite.to_owned())),
        )
        .filter(entities::code_turn::Column::Id.eq(id.0))
        .filter(entities::code_turn::Column::Owner.eq(owner.as_str()))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

async fn turn_from_stored(
    store: &DbStore,
    owner: &OwnerId,
    row: entities::code_turn::Model,
) -> Result<CodeTurn> {
    let mut turn = turn_from_row(row)?;
    turn.attachments = load_attachments_on(&store.conn, owner, turn.id).await?;
    Ok(turn)
}

pub(super) fn turn_from_row(row: entities::code_turn::Model) -> Result<CodeTurn> {
    let status = turn_status_from_stored(row.id, &row.status)?;
    let diffstat =
        match row.diffstat {
            Some(value) => Some(serde_json::from_value::<Diffstat>(value).map_err(|err| {
                AgentError::Store(format!("code_turn {} diffstat: {err}", row.id))
            })?),
            None => None,
        };
    let usage = code_usage_from_stored(row.id, row.usage)?;
    Ok(CodeTurn {
        id: CodeTurnId(row.id),
        session_id: CodeSessionId(row.session_id),
        ordinal: row.ordinal,
        status,
        model: row.model,
        fast_mode: row.fast_mode,
        user_input: row.user_input,
        user_input_blob_id: row.user_input_blob_id,
        attachments: Vec::new(),
        checkpoint_ref: row.checkpoint_ref,
        diffstat,
        usage,
        narrative: row.narrative,
        rewrite: row.rewrite,
        started_at: row.started_at,
        ended_at: row.ended_at,
        park_ref: row.park_ref,
        park_wait: match row.park_wait {
            Some(value) => Some(serde_json::from_value(value).map_err(|err| {
                AgentError::Store(format!("code_turn {} park_wait: {err}", row.id))
            })?),
            None => None,
        },
    })
}

fn turn_status_from_stored(id: uuid::Uuid, status: &str) -> Result<CodeTurnStatus> {
    CodeTurnStatus::from_str(status)
        .ok_or_else(|| AgentError::Store(format!("code_turn {id} has unknown status {status}")))
}

fn code_usage_from_stored(
    id: uuid::Uuid,
    usage: Option<serde_json::Value>,
) -> Result<Option<CodeUsage>> {
    usage
        .map(|value| {
            serde_json::from_value::<CodeUsage>(value)
                .map_err(|err| AgentError::Store(format!("code_turn {id} usage: {err}")))
        })
        .transpose()
}
