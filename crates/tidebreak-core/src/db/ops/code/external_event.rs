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
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};

use crate::code::{ExternalMessageRecord, ExternalSteerAdmission, ExternalSteerQueuedReason, QueuedTurn, SessionId, TurnId};
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
///
/// The actor rides on the queue row so promotion carries the channel identity
/// onto the turn, which is what names the person on the web (decision 0086).
pub async fn record_external_message(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    event_id: &str,
    channel_ts: &str,
    message: &str,
    actor: &crate::code::TurnActor,
) -> Result<ExternalMessageRecord> {
    record_external_message_with_context(
        store, owner, session_id, event_id, channel_ts, message, actor, None,
    )
    .await
    .map_err(|error| match error {
        ExternalMessageIntakeError::Store(error) => error,
        ExternalMessageIntakeError::Context { message, .. } => AgentError::Store(message),
    })
}

/// Admission metadata an external message may carry when it asks to steer.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExternalSteerAdmissionInput {
    /// Whether the delivery asked to steer into the active native turn.
    pub request_steer: bool,
    /// The native turn the instruction targets. Required when steering.
    pub expected_turn_id: Option<TurnId>,
    /// Caller correlation id; echoed in the admission response and carried
    /// through the supervised sandbox so an engine UUID maps back here.
    pub correlation_uuid: Option<uuid::Uuid>,
}

/// A context refusal is client input; persistence failures remain server faults.
#[derive(Debug, thiserror::Error)]
pub enum ExternalMessageIntakeError {
    /// Invalid or late context, with a stable API classification.
    #[error("{message}")]
    Context {
        /// Machine-readable refusal.
        kind: &'static str,
        /// Client-safe explanation.
        message: String,
    },
    /// Persistence or serialization failed.
    #[error(transparent)]
    Store(#[from] AgentError),
}

/// Record quoted first-turn context and its binding consent with the message.
#[allow(clippy::too_many_arguments)]
pub async fn record_external_message_with_context(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    event_id: &str,
    channel_ts: &str,
    message: &str,
    actor: &crate::code::TurnActor,
    context: Option<&crate::code::ExternalThreadContext>,
) -> std::result::Result<ExternalMessageRecord, ExternalMessageIntakeError> {
    record_external_message_with_steer(
        store,
        owner,
        session_id,
        event_id,
        channel_ts,
        message,
        actor,
        context,
        ExternalSteerAdmissionInput::default(),
    )
    .await
}

/// Record one external delivery with its steering admission metadata.
#[allow(clippy::too_many_arguments)]
pub async fn record_external_message_with_steer(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    event_id: &str,
    channel_ts: &str,
    message: &str,
    actor: &crate::code::TurnActor,
    context: Option<&crate::code::ExternalThreadContext>,
    steering: ExternalSteerAdmissionInput,
) -> std::result::Result<ExternalMessageRecord, ExternalMessageIntakeError> {
    if event_id.trim().is_empty() || channel_ts.trim().is_empty() {
        return Err(AgentError::Store(
            "an external message needs an event id and an ordering token".into(),
        )
        .into());
    }
    if message.trim().is_empty() || message.contains('\0') {
        return Err(AgentError::Store("invalid external message".into()).into());
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, session_id).await? {
        return Err(AgentError::Store(format!("code session {session_id} not found")).into());
    }
    if let Some(event) = find_event_on(&transaction, owner, session_id, event_id).await? {
        let record = replay_from_event(event);
        transaction.commit().await.map_err(store_err)?;
        return Ok(record);
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
        ))
        .into());
    }
    let rendered;
    let message = if let Some(context) = context {
        context
            .validate()
            .map_err(|message| ExternalMessageIntakeError::Context {
                kind: "invalid_thread_context",
                message,
            })?;
        let has_turn = entities::turn::Entity::find()
            .filter(entities::turn::Column::Owner.eq(owner.as_str()))
            .filter(entities::turn::Column::SessionId.eq(session_id.0))
            .one(&transaction)
            .await
            .map_err(store_err)?
            .is_some();
        let has_event = entities::code_external_event::Entity::find()
            .filter(entities::code_external_event::Column::Owner.eq(owner.as_str()))
            .filter(entities::code_external_event::Column::SessionId.eq(session_id.0))
            .one(&transaction)
            .await
            .map_err(store_err)?
            .is_some();
        if has_turn || has_event || !existing.is_empty() {
            return Err(ExternalMessageIntakeError::Context {
                kind: "context_first_turn_only",
                message: "Thread context is accepted only on the first message of a session."
                    .into(),
            });
        }
        let binding = entities::code_external_binding::Entity::find_by_id(context.binding_id.0)
            .filter(entities::code_external_binding::Column::Owner.eq(owner.as_str()))
            .filter(entities::code_external_binding::Column::SessionId.eq(session_id.0))
            .filter(entities::code_external_binding::Column::GrantId.eq(context.grant_id.0))
            .one(&transaction)
            .await
            .map_err(store_err)?;
        let Some(binding) = binding else {
            return Err(ExternalMessageIntakeError::Context {
                kind: "context_binding_mismatch",
                message: "The context binding must belong to this session and grant.".into(),
            });
        };
        entities::code_external_binding::ActiveModel {
            id: Set(binding.id),
            context_opt_in: Set(true),
            ..Default::default()
        }
        .update(&transaction)
        .await
        .map_err(store_err)?;
        rendered = context.render(message).map_err(AgentError::from)?;
        rendered.as_str()
    } else {
        message
    };
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
        actor: Set(Some(serde_json::to_value(actor).map_err(AgentError::from)?)),
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
        steer_requested: Set(steering.request_steer),
        expected_turn_id: Set(steering.expected_turn_id.map(|id| id.0)),
        correlation_uuid: Set(steering.correlation_uuid),
        outcome: Set(None),
        outcome_reason: Set(None),
        outcome_at: Set(None),
    }
    .insert(&transaction)
    .await;
    if let Err(error) = inserted {
        // The unique event key refused: a concurrent delivery of the same
        // event committed between our read and this insert. Drop our rows
        // and answer with the winner's id.
        transaction.rollback().await.map_err(store_err)?;
        let Some(event) = find_event_on(&store.conn, owner, session_id, event_id).await? else {
            return Err(store_err(error).into());
        };
        return Ok(replay_from_event(event));
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

fn replay_from_event(event: entities::code_external_event::Model) -> ExternalMessageRecord {
    let admission = event
        .outcome
        .as_deref()
        .and_then(ExternalSteerAdmission::from_str);
    ExternalMessageRecord::Replay {
        turn_id: TurnId(event.turn_id),
        steer_requested: event.steer_requested.then_some(true),
        expected_turn_id: event.expected_turn_id.map(TurnId),
        correlation_uuid: event.correlation_uuid,
        admission,
        // The queued reason is part of the durable admission; a replay must
        // reproduce it so the adapter renders the same honest substitute.
        queued_reason: event
            .outcome_reason
            .as_deref()
            .and_then(ExternalSteerQueuedReason::from_str),
    }
}

/// Read the durable admission state of one external event.
pub async fn external_steer_admission(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    event_id: &str,
) -> Result<Option<ExternalMessageRecord>> {
    let Some(event) = find_event_on(&store.conn, owner, session_id, event_id).await? else {
        return Ok(None);
    };
    Ok(Some(replay_from_event(event)))
}

/// Settle one steering admission durably.
///
/// Idempotent for the exact resolution and expected turn. `false` means the
/// event is missing or its steering metadata cannot be reconciled with the
/// caller's expected turn, so no outcome is invented.
pub async fn settle_external_steer_admission(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    event_id: &str,
    expected_turn_id: TurnId,
    admission: ExternalSteerAdmission,
    reason: Option<ExternalSteerQueuedReason>,
) -> Result<bool> {
    let Some(event) = find_event_on(&store.conn, owner, session_id, event_id).await? else {
        return Ok(false);
    };
    if event.expected_turn_id != Some(expected_turn_id.0) {
        return Ok(false);
    }
    let requested_reason = reason.map(|reason| reason.as_str().to_owned());
    let now = database_now(&store.conn).await?;
    let (outcome, outcome_reason, outcome_at) = match event.outcome.as_deref() {
        // An already settled admission is the same outcome, reason included.
        // A queued admission carries a reason; a steered one carries none.
        Some(existing)
            if existing == admission.as_str()
                && event.outcome_reason.as_deref() == requested_reason.as_deref() =>
        {
            return Ok(true);
        }
        Some(_) => return Ok(false),
        None => (
            admission.as_str().to_owned(),
            requested_reason.clone(),
            Some(now),
        ),
    };
    let updated = entities::code_external_event::Entity::update_many()
        .col_expr(
            entities::code_external_event::Column::Outcome,
            sea_orm::sea_query::Expr::value(Some(outcome)),
        )
        .col_expr(
            entities::code_external_event::Column::OutcomeReason,
            sea_orm::sea_query::Expr::value(outcome_reason),
        )
        .col_expr(
            entities::code_external_event::Column::OutcomeAt,
            sea_orm::sea_query::Expr::value(outcome_at),
        )
        .filter(entities::code_external_event::Column::Id.eq(event.id))
        .filter(entities::code_external_event::Column::Outcome.is_null())
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    if updated.rows_affected == 1 {
        return Ok(true);
    }
    // A concurrent settler may have won the write between our read and this
    // CAS. That is the same admission only when every field matches, reason
    // included; otherwise nobody can claim this resolution.
    let Some(settled) = find_event_on(&store.conn, owner, session_id, event_id).await? else {
        return Ok(false);
    };
    Ok(settled.expected_turn_id == Some(expected_turn_id.0)
        && settled.outcome.as_deref() == Some(admission.as_str())
        && settled.outcome_reason.as_deref()
            == reason.map(|reason| reason.as_str().to_owned()).as_deref())
}

/// Find one external event by its admission correlation id, scoped to a
/// session. Correlation ids are minted per delivery by the adapter, so a
/// match is the durable handle a supervised sandbox ack carries back.
pub async fn external_event_by_correlation(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    correlation_uuid: uuid::Uuid,
) -> Result<Option<ExternalMessageRecord>> {
    let Some(event) = entities::code_external_event::Entity::find()
        .filter(entities::code_external_event::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_external_event::Column::SessionId.eq(session_id.0))
        .filter(entities::code_external_event::Column::CorrelationUuid.eq(correlation_uuid))
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    Ok(Some(replay_from_event(event)))
}

/// The channel event id behind one admission correlation id.
pub async fn external_event_key_by_correlation(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    correlation_uuid: uuid::Uuid,
) -> Result<Option<String>> {
    let row = entities::code_external_event::Entity::find()
        .select_only()
        .column(entities::code_external_event::Column::EventId)
        .filter(entities::code_external_event::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_external_event::Column::SessionId.eq(session_id.0))
        .filter(entities::code_external_event::Column::CorrelationUuid.eq(correlation_uuid))
        .one(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(row.map(|row| row.event_id))
}

/// Correlation lookup by its wire string form; invalid strings match nothing.
pub async fn external_event_key_by_correlation_str(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    correlation_uuid: &str,
) -> Result<Option<String>> {
    let Ok(correlation_uuid) = uuid::Uuid::parse_str(correlation_uuid) else {
        return Ok(None);
    };
    external_event_key_by_correlation(store, owner, session_id, correlation_uuid).await
}

/// Correlation record lookup by wire string; invalid strings match nothing.
pub async fn external_event_by_correlation_str(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    correlation_uuid: &str,
) -> Result<Option<ExternalMessageRecord>> {
    let Ok(correlation_uuid) = uuid::Uuid::parse_str(correlation_uuid) else {
        return Ok(None);
    };
    external_event_by_correlation(store, owner, session_id, correlation_uuid).await
}
