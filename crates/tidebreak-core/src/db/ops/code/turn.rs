use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, TransactionTrait,
};

use crate::code::{Diffstat, SessionId, Turn, TurnId, TurnParkWait, TurnStatus, TurnUsage};
use crate::error::{AgentError, Result};
use crate::image::ImageMediaType;
use crate::image::ImageRef;
use crate::model::{TurnAgentRunWaitStatus, TurnClientWaitStatus, MAX_MESSAGE_ATTACHMENTS};
use crate::{AgentRunId, CallId, OwnerId};

use super::super::super::{entities, store_err, DbStore};
use super::super::blob as blob_ops;
use super::super::{
    acquire_advisory_lock, acquire_session_write_lock, acquire_turn_write_lock, AdvisoryLockName,
};

/// The fields analytics needs from one turn.
///
/// Keeping this projection separate from [`Turn`] avoids loading image
/// attachment rows for a report that never reads them.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnMetric {
    pub session_id: SessionId,
    pub status: TurnStatus,
    pub model: Option<String>,
    pub fast_mode: bool,
    pub usage: Option<TurnUsage>,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

/// Insert a turn row under its session's owner.
pub async fn insert_turn(store: &DbStore, owner: &OwnerId, turn: &Turn) -> Result<()> {
    validate_attachments(&turn.attachments)?;
    let txn = store.conn.begin().await.map_err(store_err)?;
    insert_turn_on(&txn, owner, turn).await?;
    txn.commit().await.map_err(store_err)?;
    Ok(())
}

pub(in crate::db) async fn insert_turn_on<C>(conn: &C, owner: &OwnerId, turn: &Turn) -> Result<()>
where
    C: ConnectionTrait,
{
    validate_attachments(&turn.attachments)?;
    entities::turn::ActiveModel {
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
        actor: Set(turn.actor.as_ref().map(serde_json::to_value).transpose()?),
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
    turn_id: TurnId,
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
            Ok(entities::turn_attachment::ActiveModel {
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
    entities::turn_attachment::Entity::insert_many(rows)
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

async fn load_attachments_on<C>(conn: &C, owner: &OwnerId, turn_id: TurnId) -> Result<Vec<ImageRef>>
where
    C: ConnectionTrait,
{
    let rows = entities::turn_attachment::Entity::find()
        .filter(entities::turn_attachment::Column::Owner.eq(owner.as_str()))
        .filter(entities::turn_attachment::Column::TurnId.eq(turn_id.0))
        .order_by_asc(entities::turn_attachment::Column::Ordinal)
        .all(conn)
        .await
        .map_err(store_err)?;
    rows.into_iter()
        .map(|row| {
            let media_type = ImageMediaType::parse(&row.media_type).ok_or_else(|| {
                AgentError::Store(format!(
                    "turn_attachment {turn_id} has unknown media type {}",
                    row.media_type
                ))
            })?;
            let byte_len = u64::try_from(row.byte_len).map_err(|_| {
                AgentError::Store(format!(
                    "turn_attachment {turn_id} has a negative byte length"
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
pub async fn get_turn(store: &DbStore, owner: &OwnerId, id: TurnId) -> Result<Option<Turn>> {
    let Some(row) = entities::turn::Entity::find_by_id(id.0)
        .filter(entities::turn::Column::Owner.eq(owner.as_str()))
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
    session_id: SessionId,
) -> Result<Vec<Turn>> {
    let rows = entities::turn::Entity::find()
        .filter(entities::turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::turn::Column::SessionId.eq(session_id.0))
        .order_by_asc(entities::turn::Column::Ordinal)
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
pub async fn list_turn_metrics(store: &DbStore, owner: &OwnerId) -> Result<Vec<TurnMetric>> {
    let rows = entities::turn::Entity::find()
        .select_only()
        .column(entities::turn::Column::Id)
        .column(entities::turn::Column::SessionId)
        .column(entities::turn::Column::Status)
        .column(entities::turn::Column::Model)
        .column(entities::turn::Column::FastMode)
        .column(entities::turn::Column::Usage)
        .column(entities::turn::Column::StartedAt)
        .filter(entities::turn::Column::Owner.eq(owner.as_str()))
        .order_by_desc(entities::turn::Column::StartedAt)
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
                Ok(TurnMetric {
                    session_id: SessionId(session_id),
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
pub async fn count_turns(store: &DbStore, owner: &OwnerId, session_id: SessionId) -> Result<i64> {
    let count = entities::turn::Entity::find()
        .filter(entities::turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::turn::Column::SessionId.eq(session_id.0))
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
    session_id: SessionId,
) -> Result<Option<Turn>> {
    let Some(row) = entities::turn::Entity::find()
        .filter(entities::turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::turn::Column::SessionId.eq(session_id.0))
        .order_by_desc(entities::turn::Column::Ordinal)
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
    session_id: SessionId,
) -> Result<Option<Turn>> {
    let turns = list_turns(store, owner, session_id).await?;
    Ok(turns.into_iter().rev().find(|turn| turn.status.is_open()))
}

/// Next 1-based ordinal for a new turn on one of the owner's sessions.
pub async fn next_turn_ordinal(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
) -> Result<i64> {
    let last = entities::turn::Entity::find()
        .filter(entities::turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::turn::Column::SessionId.eq(session_id.0))
        .order_by_desc(entities::turn::Column::Ordinal)
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
/// callers here hold a [`Turn`] read before that — `checkpoint::after_turn_ended`
/// takes its snapshot before the turn is even terminal. Writing the whole row
/// from a stale snapshot would blank a recap or rewrite that had already been
/// stored, so each column has exactly one writer: [`set_turn_narrative`] and
/// [`set_turn_rewrite`].
pub async fn save_turn(store: &DbStore, owner: &OwnerId, turn: &Turn) -> Result<bool> {
    let result = entities::turn::Entity::update_many()
        .col_expr(
            entities::turn::Column::Status,
            sea_orm::sea_query::Expr::value(turn.status.as_str().to_owned()),
        )
        .col_expr(
            entities::turn::Column::UserInput,
            sea_orm::sea_query::Expr::value(turn.user_input.clone()),
        )
        .col_expr(
            entities::turn::Column::UserInputBlobId,
            sea_orm::sea_query::Expr::value(turn.user_input_blob_id),
        )
        .col_expr(
            entities::turn::Column::CheckpointRef,
            sea_orm::sea_query::Expr::value(turn.checkpoint_ref.clone()),
        )
        .col_expr(
            entities::turn::Column::Diffstat,
            sea_orm::sea_query::Expr::value(match &turn.diffstat {
                Some(stat) => Some(serde_json::to_value(stat)?),
                None => None,
            }),
        )
        .col_expr(
            entities::turn::Column::Usage,
            sea_orm::sea_query::Expr::value(match &turn.usage {
                Some(usage) => Some(serde_json::to_value(usage)?),
                None => None,
            }),
        )
        .col_expr(
            entities::turn::Column::EndedAt,
            sea_orm::sea_query::Expr::value(turn.ended_at),
        )
        .col_expr(
            entities::turn::Column::ParkRef,
            sea_orm::sea_query::Expr::value(turn.park_ref.clone()),
        )
        .col_expr(
            entities::turn::Column::ParkWait,
            sea_orm::sea_query::Expr::value(match &turn.park_wait {
                Some(wait) => Some(serde_json::to_value(wait)?),
                None => None,
            }),
        )
        .filter(entities::turn::Column::Id.eq(turn.id.0))
        .filter(entities::turn::Column::Owner.eq(owner.as_str()))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Attach one durable adapter park without overwriting a resolution that won
/// after the engine released its turn lease.
///
/// Internal-engine client and agent-run waits first checkpoint through the
/// turn store, which leaves the row in a legacy wait status. The adapter then
/// records its opaque resume ref and changes that status to `waiting`. If the
/// dependency resolves between those two writes, the row is already
/// `resuming`; this operation keeps that status and only attaches the park.
pub async fn store_turn_park(
    store: &DbStore,
    owner: &OwnerId,
    id: TurnId,
    park_ref: &str,
    wait: &TurnParkWait,
) -> Result<Option<TurnStatus>> {
    let Some(scope) = entities::turn::Entity::find_by_id(id.0)
        .filter(entities::turn::Column::Owner.eq(owner.as_str()))
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if matches!(wait, TurnParkWait::AgentRuns { .. }) {
        acquire_advisory_lock(&transaction, AdvisoryLockName::TurnAgentRunWait).await?;
    }
    if !acquire_session_write_lock(&transaction, scope.session_id).await?
        || !acquire_turn_write_lock(&transaction, id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(turn) = entities::turn::Entity::find_by_id(id.0)
        .filter(entities::turn::Column::Owner.eq(owner.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let status = TurnStatus::from_str(&turn.status).ok_or_else(|| {
        AgentError::Store(format!("turn {} has unknown status {}", id, turn.status))
    })?;
    let raw_wait = serde_json::to_value(wait)?;
    match (turn.park_ref.as_deref(), turn.park_wait.as_ref()) {
        (Some(stored_ref), Some(stored_wait))
            if stored_ref == park_ref && stored_wait == &raw_wait =>
        {
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(status));
        }
        (None, None) => {}
        _ => {
            return Err(AgentError::Store(format!(
                "turn {id} already carries a different durable park"
            )));
        }
    }

    let next_status = match status {
        TurnStatus::Running => TurnStatus::Waiting,
        TurnStatus::WaitingForClient
            if matches!(
                wait,
                TurnParkWait::Approval { .. } | TurnParkWait::ClientToolCall { .. }
            ) =>
        {
            require_client_park_receipt(&transaction, &turn, wait, TurnClientWaitStatus::Waiting)
                .await?;
            TurnStatus::Waiting
        }
        TurnStatus::WaitingForAgentRun if matches!(wait, TurnParkWait::AgentRuns { .. }) => {
            require_agent_run_park_receipt(
                &transaction,
                &turn,
                park_ref,
                wait,
                TurnAgentRunWaitStatus::Waiting,
            )
            .await?;
            TurnStatus::Waiting
        }
        TurnStatus::Resuming => {
            require_resolved_park_receipt(&transaction, &turn, park_ref, wait).await?;
            TurnStatus::Resuming
        }
        TurnStatus::CancellingClient
            if matches!(
                wait,
                TurnParkWait::Approval { .. } | TurnParkWait::ClientToolCall { .. }
            ) =>
        {
            require_client_park_receipt(&transaction, &turn, wait, TurnClientWaitStatus::Waiting)
                .await?;
            TurnStatus::CancellingClient
        }
        TurnStatus::Interrupted => {
            require_cancelled_park_receipt(&transaction, &turn, park_ref, wait).await?;
            TurnStatus::Interrupted
        }
        _ => {
            return Err(AgentError::Store(format!(
                "turn {id} cannot store a durable park from status {}",
                status.as_str()
            )));
        }
    };
    let updated = entities::turn::Entity::update_many()
        .col_expr(
            entities::turn::Column::Status,
            sea_orm::sea_query::Expr::value(next_status.as_str()),
        )
        .col_expr(
            entities::turn::Column::ParkRef,
            sea_orm::sea_query::Expr::value(Some(park_ref.to_owned())),
        )
        .col_expr(
            entities::turn::Column::ParkWait,
            sea_orm::sea_query::Expr::value(Some(raw_wait)),
        )
        .filter(entities::turn::Column::Id.eq(turn.id))
        .filter(entities::turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::turn::Column::Status.eq(&turn.status))
        .filter(entities::turn::Column::AttemptCount.eq(turn.attempt_count))
        .filter(entities::turn::Column::ClaimCount.eq(turn.claim_count))
        .filter(entities::turn::Column::ParkRef.is_null())
        .filter(entities::turn::Column::ParkWait.is_null())
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(next_status))
}

/// Clear one exact durable adapter park without writing any other turn field.
pub async fn clear_turn_park(
    store: &DbStore,
    owner: &OwnerId,
    id: TurnId,
    park_ref: &str,
    wait: &TurnParkWait,
) -> Result<Option<TurnStatus>> {
    let Some(scope) = entities::turn::Entity::find_by_id(id.0)
        .filter(entities::turn::Column::Owner.eq(owner.as_str()))
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_session_write_lock(&transaction, scope.session_id).await?
        || !acquire_turn_write_lock(&transaction, id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(turn) = entities::turn::Entity::find_by_id(id.0)
        .filter(entities::turn::Column::Owner.eq(owner.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let status = TurnStatus::from_str(&turn.status).ok_or_else(|| {
        AgentError::Store(format!("turn {} has unknown status {}", id, turn.status))
    })?;
    let raw_wait = serde_json::to_value(wait)?;
    if turn.park_ref.is_none() && turn.park_wait.is_none() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(status));
    }
    if turn.park_ref.as_deref() != Some(park_ref) || turn.park_wait.as_ref() != Some(&raw_wait) {
        return Err(AgentError::Store(format!(
            "turn {id} carries a different durable park"
        )));
    }
    let updated = entities::turn::Entity::update_many()
        .col_expr(
            entities::turn::Column::ParkRef,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::turn::Column::ParkWait,
            sea_orm::sea_query::Expr::value(Option::<serde_json::Value>::None),
        )
        .filter(entities::turn::Column::Id.eq(turn.id))
        .filter(entities::turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::turn::Column::ParkRef.eq(park_ref))
        .filter(entities::turn::Column::ParkWait.eq(raw_wait))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(status))
}

async fn require_resolved_park_receipt<C>(
    conn: &C,
    turn: &entities::turn::Model,
    park_ref: &str,
    wait: &TurnParkWait,
) -> Result<()>
where
    C: ConnectionTrait,
{
    match wait {
        TurnParkWait::Approval { .. } | TurnParkWait::ClientToolCall { .. } => {
            require_client_park_receipt(conn, turn, wait, TurnClientWaitStatus::Resumed).await
        }
        TurnParkWait::AgentRuns { .. } => {
            require_agent_run_park_receipt(
                conn,
                turn,
                park_ref,
                wait,
                TurnAgentRunWaitStatus::Resumed,
            )
            .await
        }
    }
}

async fn require_cancelled_park_receipt<C>(
    conn: &C,
    turn: &entities::turn::Model,
    park_ref: &str,
    wait: &TurnParkWait,
) -> Result<()>
where
    C: ConnectionTrait,
{
    match wait {
        TurnParkWait::Approval { .. } | TurnParkWait::ClientToolCall { .. } => {
            require_client_park_receipt(conn, turn, wait, TurnClientWaitStatus::Cancelled).await
        }
        TurnParkWait::AgentRuns { .. } => {
            require_agent_run_park_receipt(
                conn,
                turn,
                park_ref,
                wait,
                TurnAgentRunWaitStatus::Cancelled,
            )
            .await
        }
    }
}

async fn require_client_park_receipt<C>(
    conn: &C,
    turn: &entities::turn::Model,
    wait: &TurnParkWait,
    expected_status: TurnClientWaitStatus,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let call_id = match wait {
        TurnParkWait::Approval { call_id } | TurnParkWait::ClientToolCall { call_id } => call_id,
        TurnParkWait::AgentRuns { .. } => {
            return Err(AgentError::Store(format!(
                "turn {} client park has an agent-run wait",
                TurnId(turn.id)
            )));
        }
    };
    let call_id = call_id.parse::<CallId>().map_err(|_| {
        AgentError::Store(format!(
            "turn {} client park has an invalid call id",
            TurnId(turn.id)
        ))
    })?;
    let receipt = entities::turn_client_wait::Entity::find_by_id(call_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "turn {} client park is missing wait {call_id}",
                TurnId(turn.id)
            ))
        })?;
    if receipt.turn_id != turn.id
        || receipt.session_id != turn.session_id
        || receipt.attempt_count != turn.attempt_count
        || receipt.claim_count != turn.claim_count
        || receipt.status != expected_status.as_str()
    {
        return Err(AgentError::Store(format!(
            "turn {} client park has a mismatched wait {call_id}",
            TurnId(turn.id)
        )));
    }
    Ok(())
}

async fn require_agent_run_park_receipt<C>(
    conn: &C,
    turn: &entities::turn::Model,
    park_ref: &str,
    wait: &TurnParkWait,
    expected_status: TurnAgentRunWaitStatus,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let TurnParkWait::AgentRuns { run_ids } = wait else {
        return Err(AgentError::Store(format!(
            "turn {} agent-run park has a different wait kind",
            TurnId(turn.id)
        )));
    };
    let wait_id = park_ref.parse::<CallId>().map_err(|_| {
        AgentError::Store(format!(
            "turn {} agent-run park has an invalid wait id",
            TurnId(turn.id)
        ))
    })?;
    let expected_runs = run_ids
        .iter()
        .map(|id| {
            id.parse::<AgentRunId>().map_err(|_| {
                AgentError::Store(format!(
                    "turn {} agent-run park has an invalid run id",
                    TurnId(turn.id)
                ))
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let receipt = entities::turn_agent_run_wait_set::Entity::find_by_id(wait_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "turn {} agent-run park is missing wait {wait_id}",
                TurnId(turn.id)
            ))
        })?;
    let members = entities::turn_agent_run_wait_member::Entity::find()
        .filter(entities::turn_agent_run_wait_member::Column::WaitId.eq(wait_id.0))
        .order_by_asc(entities::turn_agent_run_wait_member::Column::Position)
        .all(conn)
        .await
        .map_err(store_err)?;
    let stored_runs = members
        .iter()
        .map(|member| AgentRunId(member.child_run_id))
        .collect::<Vec<_>>();
    if receipt.turn_id != turn.id
        || receipt.session_id != turn.session_id
        || receipt.attempt_count != turn.attempt_count
        || receipt.claim_count != turn.claim_count
        || receipt.status != expected_status.as_str()
        || stored_runs != expected_runs
    {
        return Err(AgentError::Store(format!(
            "turn {} agent-run park has a mismatched wait {wait_id}",
            TurnId(turn.id)
        )));
    }
    Ok(())
}

/// Store one turn's derived narrative, touching no other column.
///
/// Targeted for the reason [`save_turn`] documents: this lands while other
/// writers hold a `Turn` read before the narrative existed, so it must not
/// be carried on a whole-row write in either direction.
pub async fn set_turn_narrative(
    store: &DbStore,
    owner: &OwnerId,
    id: TurnId,
    narrative: &str,
) -> Result<bool> {
    let result = entities::turn::Entity::update_many()
        .col_expr(
            entities::turn::Column::Narrative,
            sea_orm::sea_query::Expr::value(Some(narrative.to_owned())),
        )
        .filter(entities::turn::Column::Id.eq(id.0))
        .filter(entities::turn::Column::Owner.eq(owner.as_str()))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Store one turn's derived rewrite, touching no other column.
///
/// Targeted for the reason [`save_turn`] documents: this lands while other
/// writers hold a `Turn` read before the rewrite existed, so it must not
/// be carried on a whole-row write in either direction.
pub async fn set_turn_rewrite(
    store: &DbStore,
    owner: &OwnerId,
    id: TurnId,
    rewrite: &str,
) -> Result<bool> {
    let result = entities::turn::Entity::update_many()
        .col_expr(
            entities::turn::Column::Rewrite,
            sea_orm::sea_query::Expr::value(Some(rewrite.to_owned())),
        )
        .filter(entities::turn::Column::Id.eq(id.0))
        .filter(entities::turn::Column::Owner.eq(owner.as_str()))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

async fn turn_from_stored(
    store: &DbStore,
    owner: &OwnerId,
    row: entities::turn::Model,
) -> Result<Turn> {
    let mut turn = turn_from_row(row)?;
    turn.attachments = load_attachments_on(&store.conn, owner, turn.id).await?;
    Ok(turn)
}

pub(super) fn turn_from_row(row: entities::turn::Model) -> Result<Turn> {
    let status = turn_status_from_stored(row.id, &row.status)?;
    let diffstat = match row.diffstat {
        Some(value) => Some(
            serde_json::from_value::<Diffstat>(value)
                .map_err(|err| AgentError::Store(format!("turn {} diffstat: {err}", row.id)))?,
        ),
        None => None,
    };
    let usage = code_usage_from_stored(row.id, row.usage)?;
    Ok(Turn {
        actor: row
            .actor
            .map(serde_json::from_value)
            .transpose()
            .map_err(|err| AgentError::Store(format!("turn {} actor: {err}", row.id)))?,
        id: TurnId(row.id),
        session_id: SessionId(row.session_id),
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
            Some(value) => {
                Some(serde_json::from_value(value).map_err(|err| {
                    AgentError::Store(format!("turn {} park_wait: {err}", row.id))
                })?)
            }
            None => None,
        },
    })
}

fn turn_status_from_stored(id: uuid::Uuid, status: &str) -> Result<TurnStatus> {
    TurnStatus::from_str(status)
        .ok_or_else(|| AgentError::Store(format!("turn {id} has unknown status {status}")))
}

fn code_usage_from_stored(
    id: uuid::Uuid,
    usage: Option<serde_json::Value>,
) -> Result<Option<TurnUsage>> {
    usage
        .map(|value| {
            serde_json::from_value::<TurnUsage>(value)
                .map_err(|err| AgentError::Store(format!("turn {id} usage: {err}")))
        })
        .transpose()
}
