//! Idempotent external message intake (docs/slack-sessions.md, stage 2).
//!
//! A channel delivers at-least-once, so every message carries an event id.
//! The first delivery commits the event row and the queue row it causes in
//! one transaction; a replay finds the event row and answers with the id the
//! first delivery minted, writing nothing. The caller derives the replayed
//! outcome from that row's current state — still queued, promoted into a
//! turn, or retracted — so there is no outcome snapshot to go stale.
//!
//! Ordering: a channel can also deliver out of order. Each event carries the
//! channel's own ordering token (`channel_ts`, compared lexicographically —
//! Slack's fixed-width epoch format sorts correctly this way). While a
//! message is still queued it can move, so the insert reorders the session's
//! still-queued external rows by that token. A row already promoted into a
//! turn is outside the window and never moves, so "A then B" cannot become
//! "B steered by A".

use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};

use crate::code::{ExternalMessageRecord, QueuedTurn, SessionId, TurnId};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};
use super::super::agent_run::database_now;
use super::acquire_code_session_write_lock;
use super::queued::queued_turn_from_model;

async fn find_event_on<C>(
    conn: &C,
    owner: &OwnerId,
    session_id: SessionId,
    event_id: &str,
) -> Result<Option<entities::code_external_event::Model>>
where
    C: ConnectionTrait,
{
    entities::code_external_event::Entity::find()
        .filter(entities::code_external_event::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_external_event::Column::SessionId.eq(session_id.0))
        .filter(entities::code_external_event::Column::EventId.eq(event_id))
        .one(conn)
        .await
        .map_err(store_err)
}

/// Reorder the session's still-queued external rows by their channel
/// ordering token. Rows without an event row (desktop follow-ups) keep
/// their positions; external rows permute within their own position slots,
/// so the two populations never leapfrog each other as a group.
async fn order_queued_by_channel_ts<C>(
    conn: &C,
    owner: &OwnerId,
    session_id: SessionId,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let rows = entities::code_queued_turn::Entity::find()
        .filter(entities::code_queued_turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_queued_turn::Column::SessionId.eq(session_id.0))
        .order_by_asc(entities::code_queued_turn::Column::Position)
        .order_by_asc(entities::code_queued_turn::Column::CreatedAt)
        .all(conn)
        .await
        .map_err(store_err)?;
    let events = entities::code_external_event::Entity::find()
        .filter(entities::code_external_event::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_external_event::Column::SessionId.eq(session_id.0))
        .all(conn)
        .await
        .map_err(store_err)?;
    let ts_of = |row_id: uuid::Uuid| {
        events
            .iter()
            .find(|event| event.turn_id == row_id)
            .map(|event| event.channel_ts.clone())
    };
    let mut external: Vec<(usize, String)> = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| ts_of(row.id).map(|ts| (index, ts)))
        .collect();
    let slots: Vec<i32> = external
        .iter()
        .map(|(index, _)| rows[*index].position)
        .collect();
    external.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    for ((index, _), slot) in external.into_iter().zip(slots) {
        let row = &rows[index];
        if row.position == slot {
            continue;
        }
        entities::code_queued_turn::ActiveModel {
            id: Set(row.id),
            position: Set(slot),
            ..Default::default()
        }
        .update(conn)
        .await
        .map_err(store_err)?;
    }
    Ok(())
}

/// Record one external message delivery.
///
/// First delivery: parks the message as a queue row, records the event
/// against the row's id, and reorders still-queued external rows by
/// `channel_ts`, all in one transaction. Replay: answers with the recorded
/// id and writes nothing. The queue row's id becomes the promoted turn's
/// id (decision 69), so one id follows the message through its whole life.
pub async fn record_external_message(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    event_id: &str,
    channel_ts: &str,
    message: &str,
) -> Result<ExternalMessageRecord> {
    if event_id.trim().is_empty() || channel_ts.trim().is_empty() {
        return Err(AgentError::Store(
            "an external message needs an event id and an ordering token".into(),
        ));
    }
    if message.trim().is_empty() || message.contains('\0') {
        return Err(AgentError::Store("invalid external message".into()));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, session_id).await? {
        return Err(AgentError::Store(format!(
            "code session {session_id} not found"
        )));
    }
    if let Some(event) = find_event_on(&transaction, owner, session_id, event_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ExternalMessageRecord::Replay {
            turn_id: TurnId(event.turn_id),
        });
    }
    let existing = entities::code_queued_turn::Entity::find()
        .filter(entities::code_queued_turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_queued_turn::Column::SessionId.eq(session_id.0))
        .order_by_asc(entities::code_queued_turn::Column::Position)
        .order_by_asc(entities::code_queued_turn::Column::CreatedAt)
        .all(&transaction)
        .await
        .map_err(store_err)?;
    if existing.len() >= QueuedTurn::MAX_PER_SESSION {
        transaction.commit().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "a session may queue at most {} messages",
            QueuedTurn::MAX_PER_SESSION
        )));
    }
    let position = existing.last().map_or(0, |last| last.position + 1);
    let now = database_now(&transaction).await?;
    let queued_id = TurnId::new();
    entities::code_queued_turn::ActiveModel {
        id: Set(queued_id.0),
        owner: Set(owner.as_str().to_owned()),
        session_id: Set(session_id.0),
        message: Set(message.to_owned()),
        attachments_json: Set("[]".to_owned()),
        file_attachments_json: Set("[]".to_owned()),
        invoked_skills_json: Set("[]".to_owned()),
        voice_input_used: Set(false),
        fingerprint: Set(None),
        actor: Set(None),
        position: Set(position),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    let inserted = entities::code_external_event::ActiveModel {
        id: Set(uuid::Uuid::new_v4()),
        owner: Set(owner.as_str().to_owned()),
        session_id: Set(session_id.0),
        event_id: Set(event_id.to_owned()),
        channel_ts: Set(channel_ts.to_owned()),
        turn_id: Set(queued_id.0),
        created_at: Set(now),
    }
    .insert(&transaction)
    .await;
    if let Err(error) = inserted {
        // The unique event key refused: a concurrent delivery of the same
        // event committed between our read and this insert. Drop our rows
        // and answer with the winner's id.
        transaction.rollback().await.map_err(store_err)?;
        let Some(event) = find_event_on(&store.conn, owner, session_id, event_id).await? else {
            return Err(store_err(error));
        };
        return Ok(ExternalMessageRecord::Replay {
            turn_id: TurnId(event.turn_id),
        });
    }
    order_queued_by_channel_ts(&transaction, owner, session_id).await?;
    let row = entities::code_queued_turn::Entity::find_by_id(queued_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("code queued turn disappeared".into()))?;
    let row = queued_turn_from_model(row)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(ExternalMessageRecord::Recorded(Box::new(row)))
}
