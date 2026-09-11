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

use crate::code::{
    ExternalMessageRecord, ExternalSteerAdmission, ExternalSteerQueuedReason, QueuedTurn,
    SessionId, TurnId,
};
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
    // Replay above is authoritative even when the retry changes its metadata.
    // Validate new admissions only, before creating any durable queue row.
    if steering.request_steer && steering.expected_turn_id.is_none() {
        return Err(ExternalMessageIntakeError::Context {
            kind: "steer_target_required",
            message: "Steering requires the turn that the message targets.".into(),
        });
    }
    if let Some(correlation) = steering.correlation_uuid {
        let collision = entities::code_external_event::Entity::find()
            .filter(entities::code_external_event::Column::Owner.eq(owner.as_str()))
            .filter(entities::code_external_event::Column::SessionId.eq(session_id.0))
            .filter(entities::code_external_event::Column::CorrelationUuid.eq(correlation))
            .one(&transaction)
            .await
            .map_err(store_err)?;
        if correlation.is_nil() || collision.is_some() {
            return Err(ExternalMessageIntakeError::Context {
                kind: "steer_correlation_conflict",
                message: "Each delivery needs its own nonempty steering correlation ID.".into(),
            });
        }
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
        steer_sandbox_id: Set(None),
        steer_native_turn: Set(None),
        steer_runtime_id: Set(None),
        outcome: Set(None),
        outcome_reason: Set(None),
        outcome_at: Set(None),
        recovery_action: Set(None),
        recovery_retry_turn_id: Set(None),
        recovered_at: Set(None),
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
    if (admission == ExternalSteerAdmission::Steered) != reason.is_none() {
        return Ok(false);
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, session_id).await? {
        return Ok(false);
    }
    let Some(event) = find_event_on(&transaction, owner, session_id, event_id).await? else {
        return Ok(false);
    };
    if !event.steer_requested || event.expected_turn_id != Some(expected_turn_id.0) {
        return Ok(false);
    }
    let settled =
        settle_external_steer_on(&transaction, owner, session_id, event, admission, reason).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(settled)
}

async fn settle_external_steer_on<C: ConnectionTrait>(
    conn: &C,
    owner: &OwnerId,
    session_id: SessionId,
    event: entities::code_external_event::Model,
    admission: ExternalSteerAdmission,
    reason: Option<ExternalSteerQueuedReason>,
) -> Result<bool> {
    let requested_reason = reason.map(|reason| reason.as_str().to_owned());
    if let Some(existing) = event.outcome.as_deref() {
        return Ok(existing == admission.as_str() && event.outcome_reason == requested_reason);
    }
    let now = database_now(conn).await?;
    entities::code_external_event::ActiveModel {
        id: Set(event.id),
        outcome: Set(Some(admission.as_str().to_owned())),
        outcome_reason: Set(requested_reason),
        outcome_at: Set(Some(now)),
        ..Default::default()
    }
    .update(conn)
    .await
    .map_err(store_err)?;
    if admission == ExternalSteerAdmission::Steered {
        // The native turn has consumed this input. Its receipt survives, but
        // the same input can no longer become a second turn after a restart.
        entities::code_queued_turn::Entity::delete_many()
            .filter(entities::code_queued_turn::Column::Id.eq(event.turn_id))
            .filter(entities::code_queued_turn::Column::Owner.eq(owner.as_str()))
            .filter(entities::code_queued_turn::Column::SessionId.eq(session_id.0))
            .exec(conn)
            .await
            .map_err(store_err)?;
    }
    Ok(true)
}

/// Settle a machine acknowledgment only while its original turn and worker own
/// the session. Remote dispatch claims cannot settle through a machine sink.
pub async fn settle_machine_external_steer_ack(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    worker_epoch: i64,
    expected_turn_id: TurnId,
    correlation_uuid: uuid::Uuid,
) -> Result<bool> {
    settle_machine_external_steer_admission(
        store,
        owner,
        session_id,
        worker_epoch,
        expected_turn_id,
        correlation_uuid,
        ExternalSteerAdmission::Steered,
        None,
    )
    .await
}

/// Settle a machine control response while its worker still owns the session.
/// A successful steer also requires its exact target turn to remain running.
#[allow(clippy::too_many_arguments)]
pub async fn settle_machine_external_steer_admission(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    worker_epoch: i64,
    expected_turn_id: TurnId,
    correlation_uuid: uuid::Uuid,
    admission: ExternalSteerAdmission,
    reason: Option<ExternalSteerQueuedReason>,
) -> Result<bool> {
    use crate::code::{ExecutionLocation, SessionLifecycle, TurnStatus};

    if (admission == ExternalSteerAdmission::Steered) != reason.is_none() {
        return Ok(false);
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, session_id).await? {
        return Ok(false);
    }
    let Some(session) = entities::session::Entity::find_by_id(session_id.0)
        .filter(entities::session::Column::Owner.eq(owner.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        return Ok(false);
    };
    if session.spawn_epoch != worker_epoch
        || session.execution_location != ExecutionLocation::Machine.as_str()
        || (admission == ExternalSteerAdmission::Steered
            && session.lifecycle != SessionLifecycle::Running.as_str())
    {
        return Ok(false);
    }
    let running = entities::turn::Entity::find_by_id(expected_turn_id.0)
        .filter(entities::turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::turn::Column::SessionId.eq(session_id.0))
        .filter(entities::turn::Column::Status.eq(TurnStatus::Running.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?;
    if admission == ExternalSteerAdmission::Steered && running.is_none() {
        return Ok(false);
    }
    let Some(event) = entities::code_external_event::Entity::find()
        .filter(entities::code_external_event::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_external_event::Column::SessionId.eq(session_id.0))
        .filter(entities::code_external_event::Column::CorrelationUuid.eq(correlation_uuid))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        return Ok(false);
    };
    if !event.steer_requested
        || event.expected_turn_id != Some(expected_turn_id.0)
        || event.steer_sandbox_id.is_some()
        || event.steer_native_turn.is_some()
        || event.steer_runtime_id.is_some()
    {
        return Ok(false);
    }
    let settled =
        settle_external_steer_on(&transaction, owner, session_id, event, admission, reason).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(settled)
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
        .into_tuple::<String>()
        .one(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(row)
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

/// The immutable target of one sandbox steering dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSteerTarget {
    /// Channel delivery key.
    pub event_id: String,
    /// Queue-row identity retained by the receipt after consumption.
    pub turn_id: TurnId,
    /// Tidebreak turn that may consume the instruction.
    pub expected_turn_id: TurnId,
    /// Correlation carried by the transport frame and acknowledgment.
    pub correlation_uuid: uuid::Uuid,
    /// Sandbox that owns this native turn.
    pub sandbox_id: String,
    /// One-based native turn within that sandbox.
    pub native_turn: u32,
    /// Supervisor process that owns the native turn.
    pub runtime_id: uuid::Uuid,
}

/// Settle the frozen sandbox delivery and preserve its admitted text together.
/// Replays return the same outcome without adding another transcript event.
pub async fn settle_sandbox_external_steer_admission(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    target: &ExternalSteerTarget,
    admission: ExternalSteerAdmission,
    reason: Option<ExternalSteerQueuedReason>,
) -> Result<(bool, Option<crate::code::SequencedEvent>)> {
    use crate::code::{Event, ExecutionLocation, SequencedEvent};

    if (admission == ExternalSteerAdmission::Steered) != reason.is_none() {
        return Ok((false, None));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, session_id).await? {
        return Ok((false, None));
    }
    let session = entities::session::Entity::find_by_id(session_id.0)
        .filter(entities::session::Column::Owner.eq(owner.as_str()))
        .filter(
            entities::session::Column::ExecutionLocation.eq(ExecutionLocation::Sandbox.as_str()),
        )
        .one(&transaction)
        .await
        .map_err(store_err)?;
    if session.is_none() {
        return Ok((false, None));
    }
    let Some(receipt) = find_event_on(&transaction, owner, session_id, &target.event_id).await?
    else {
        return Ok((false, None));
    };
    if !receipt.steer_requested
        || receipt.turn_id != target.turn_id.0
        || receipt.expected_turn_id != Some(target.expected_turn_id.0)
        || receipt.correlation_uuid != Some(target.correlation_uuid)
        || receipt.steer_sandbox_id.as_deref() != Some(target.sandbox_id.as_str())
        || receipt.steer_native_turn != Some(i64::from(target.native_turn))
        || receipt.steer_runtime_id != Some(target.runtime_id)
    {
        return Ok((false, None));
    }
    let original = if receipt.outcome.is_none() && admission == ExternalSteerAdmission::Steered {
        entities::code_queued_turn::Entity::find_by_id(target.turn_id.0)
            .filter(entities::code_queued_turn::Column::Owner.eq(owner.as_str()))
            .filter(entities::code_queued_turn::Column::SessionId.eq(session_id.0))
            .one(&transaction)
            .await
            .map_err(store_err)?
    } else {
        None
    };
    let settled =
        settle_external_steer_on(&transaction, owner, session_id, receipt, admission, reason)
            .await?;
    let journaled = if settled {
        if let Some(original) = original {
            let event = Event::UserSteered {
                text: original.message,
                message_id: Some(target.turn_id.0),
            };
            let seq =
                super::journal::append_event_on_locked(&transaction, owner, session_id, &event)
                    .await?;
            Some(SequencedEvent { seq, event })
        } else {
            None
        }
    } else {
        None
    };
    transaction.commit().await.map_err(store_err)?;
    Ok((settled, journaled))
}

/// Claim one sandbox dispatch before sending its frame.
///
/// Only the first claim returns true. A retry never sends again after an
/// ambiguous response or a restart; its unresolved admission stays held.
#[allow(clippy::too_many_arguments)]
pub async fn claim_external_steer_target(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    event_id: &str,
    expected_turn_id: TurnId,
    correlation_uuid: uuid::Uuid,
    sandbox_id: &str,
    native_turn: u32,
    runtime_id: uuid::Uuid,
) -> Result<bool> {
    if sandbox_id.trim().is_empty()
        || native_turn == 0
        || correlation_uuid.is_nil()
        || runtime_id.is_nil()
    {
        return Ok(false);
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, session_id).await? {
        return Ok(false);
    }
    let Some(event) = find_event_on(&transaction, owner, session_id, event_id).await? else {
        return Ok(false);
    };
    if !event.steer_requested
        || event.expected_turn_id != Some(expected_turn_id.0)
        || event.correlation_uuid != Some(correlation_uuid)
        || event.outcome.is_some()
        || event.steer_sandbox_id.is_some()
        || event.steer_native_turn.is_some()
        || event.steer_runtime_id.is_some()
    {
        return Ok(false);
    }
    let queued = entities::code_queued_turn::Entity::find_by_id(event.turn_id)
        .filter(entities::code_queued_turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_queued_turn::Column::SessionId.eq(session_id.0))
        .one(&transaction)
        .await
        .map_err(store_err)?;
    if queued.is_none() {
        return Ok(false);
    }
    entities::code_external_event::ActiveModel {
        id: Set(event.id),
        steer_sandbox_id: Set(Some(sandbox_id.to_owned())),
        steer_native_turn: Set(Some(i64::from(native_turn))),
        steer_runtime_id: Set(Some(runtime_id)),
        ..Default::default()
    }
    .update(&transaction)
    .await
    .map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(true)
}

/// Resolve an acknowledgment to the exact persisted sandbox target.
pub async fn external_steer_target_by_correlation(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    correlation_uuid: uuid::Uuid,
) -> Result<Option<ExternalSteerTarget>> {
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
    if !event.steer_requested {
        return Ok(None);
    }
    let (Some(expected), Some(sandbox_id), Some(native_turn), Some(runtime_id)) = (
        event.expected_turn_id,
        event.steer_sandbox_id,
        event.steer_native_turn,
        event.steer_runtime_id,
    ) else {
        return Ok(None);
    };
    let native_turn = u32::try_from(native_turn)
        .ok()
        .filter(|turn| *turn > 0)
        .ok_or_else(|| AgentError::Store("invalid persisted sandbox steering turn".into()))?;
    Ok(Some(ExternalSteerTarget {
        event_id: event.event_id,
        turn_id: TurnId(event.turn_id),
        expected_turn_id: TurnId(expected),
        correlation_uuid,
        sandbox_id,
        native_turn,
        runtime_id,
    }))
}

/// A recovery request could not safely change an unconfirmed admission.
#[derive(Debug, thiserror::Error)]
pub enum ExternalSteerRecoveryError {
    #[error("steering admission not found")]
    NotFound,
    #[error("The native admission already resolved. Refresh its status before recovering.")]
    AdmissionResolved,
    #[error("This admission already has a different recovery decision.")]
    ConflictingAction,
    #[error(
        "Accept that the original instruction may already have run before queuing another copy."
    )]
    DuplicateRiskNotAccepted,
    #[error("The original held message is no longer available to retry.")]
    OriginalMissing,
    #[error("The session ended and cannot accept another copy.")]
    SessionEnded,
    #[error(transparent)]
    Store(#[from] AgentError),
}

fn recovery_from_event(
    event: &entities::code_external_event::Model,
) -> Result<Option<crate::code::ExternalSteerRecovery>> {
    use crate::code::{ExternalSteerRecovery, ExternalSteerRecoveryAction};
    let Some(action) = event.recovery_action.as_deref() else {
        return Ok(None);
    };
    let action = ExternalSteerRecoveryAction::from_str(action)
        .ok_or_else(|| AgentError::Store("invalid stored steering recovery action".into()))?;
    let recovered_at = event
        .recovered_at
        .ok_or_else(|| AgentError::Store("steering recovery has no timestamp".into()))?;
    if (action == ExternalSteerRecoveryAction::Retry) != event.recovery_retry_turn_id.is_some() {
        return Err(AgentError::Store(
            "steering recovery has an invalid retry identity".into(),
        ));
    }
    Ok(Some(ExternalSteerRecovery {
        action,
        retry_turn_id: event.recovery_retry_turn_id.map(TurnId),
        recovered_at,
    }))
}

/// Read the immutable recovery decision under the event's owner and session.
pub async fn external_steer_recovery(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    event_id: &str,
) -> Result<Option<crate::code::ExternalSteerRecovery>> {
    let Some(event) = find_event_on(&store.conn, owner, session_id, event_id).await? else {
        return Ok(None);
    };
    recovery_from_event(&event)
}

/// Recover one unresolved admission without claiming anything about native execution.
/// The original receipt is the idempotency key; its decision never changes.
pub async fn recover_external_steer(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    event_id: &str,
    action: crate::code::ExternalSteerRecoveryAction,
    accept_duplicate_risk: bool,
) -> std::result::Result<crate::code::ExternalSteerRecovery, ExternalSteerRecoveryError> {
    use crate::code::{ExternalSteerRecovery, ExternalSteerRecoveryAction, SessionLifecycle};
    if action == ExternalSteerRecoveryAction::Retry && !accept_duplicate_risk {
        return Err(ExternalSteerRecoveryError::DuplicateRiskNotAccepted);
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    // The same owner-scoped session lock serializes recovery with native settlement.
    let locked = entities::session::Entity::update_many()
        .col_expr(
            entities::session::Column::UnrecognizedEventCount,
            sea_orm::sea_query::Expr::col(entities::session::Column::UnrecognizedEventCount),
        )
        .filter(entities::session::Column::Id.eq(session_id.0))
        .filter(entities::session::Column::Owner.eq(owner.as_str()))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if locked.rows_affected != 1 {
        return Err(ExternalSteerRecoveryError::NotFound);
    }
    let event = find_event_on(&transaction, owner, session_id, event_id)
        .await?
        .filter(|event| event.steer_requested)
        .ok_or(ExternalSteerRecoveryError::NotFound)?;
    if let Some(recovery) = recovery_from_event(&event)? {
        if recovery.action != action {
            return Err(ExternalSteerRecoveryError::ConflictingAction);
        }
        transaction.commit().await.map_err(store_err)?;
        return Ok(recovery);
    }
    if event.outcome.is_some() {
        return Err(ExternalSteerRecoveryError::AdmissionResolved);
    }
    let now = database_now(&transaction).await?;
    let original = if action == ExternalSteerRecoveryAction::Retry {
        let session = entities::session::Entity::find_by_id(session_id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or(ExternalSteerRecoveryError::NotFound)?;
        if session.lifecycle == SessionLifecycle::Ended.as_str() {
            return Err(ExternalSteerRecoveryError::SessionEnded);
        }
        let original = entities::code_queued_turn::Entity::find_by_id(event.turn_id)
            .filter(entities::code_queued_turn::Column::Owner.eq(owner.as_str()))
            .filter(entities::code_queued_turn::Column::SessionId.eq(session_id.0))
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or(ExternalSteerRecoveryError::OriginalMissing)?;
        Some(original)
    } else {
        None
    };
    entities::code_queued_turn::Entity::delete_many()
        .filter(entities::code_queued_turn::Column::Id.eq(event.turn_id))
        .filter(entities::code_queued_turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_queued_turn::Column::SessionId.eq(session_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    let retry_turn_id = if let Some(original) = original {
        let tail = entities::code_queued_turn::Entity::find()
            .filter(entities::code_queued_turn::Column::Owner.eq(owner.as_str()))
            .filter(entities::code_queued_turn::Column::SessionId.eq(session_id.0))
            .order_by_desc(entities::code_queued_turn::Column::Position)
            .one(&transaction)
            .await
            .map_err(store_err)?;
        let position = tail
            .map_or(Some(0), |row| row.position.checked_add(1))
            .ok_or_else(|| AgentError::Store("the queue position limit was reached".into()))?;
        let id = TurnId::new();
        entities::code_queued_turn::ActiveModel {
            id: Set(id.0),
            owner: Set(owner.as_str().to_owned()),
            session_id: Set(session_id.0),
            message: Set(original.message),
            attachments_json: Set(original.attachments_json),
            file_attachments_json: Set(original.file_attachments_json),
            invoked_skills_json: Set(original.invoked_skills_json),
            voice_input_used: Set(original.voice_input_used),
            fingerprint: Set(None),
            actor: Set(original.actor),
            position: Set(position),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&transaction)
        .await
        .map_err(store_err)?;
        Some(id)
    } else {
        None
    };
    entities::code_external_event::ActiveModel {
        id: Set(event.id),
        recovery_action: Set(Some(action.as_str().into())),
        recovery_retry_turn_id: Set(retry_turn_id.map(|id| id.0)),
        recovered_at: Set(Some(now)),
        ..Default::default()
    }
    .update(&transaction)
    .await
    .map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(ExternalSteerRecovery {
        action,
        retry_turn_id,
        recovered_at: now,
    })
}
