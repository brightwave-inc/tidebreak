//! Correlate supervisor turns with delivered user input, never counter offsets.
use crate::code::CodeIncarnationId;
use crate::db::{entities, store_err, DbStore};
use crate::{AgentError, OwnerId, Result, SessionId, TurnId};
use entities::{code_native_turn_input as input, code_native_turn_observation as observation};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set, TransactionTrait,
};

fn invalid(message: &str) -> AgentError {
    AgentError::InvalidTarget(message.into())
}
async fn check_scope<C: ConnectionTrait>(
    conn: &C,
    owner: &OwnerId,
    session: SessionId,
    incarnation: CodeIncarnationId,
) -> Result<()> {
    super::acquire_code_session_write_lock(conn, session).await?;
    let row = entities::code_session_incarnation::Entity::find_by_id(incarnation.0)
        .one(conn)
        .await
        .map_err(store_err)?;
    if row.is_none_or(|row| row.owner != owner.as_str() || row.session_id != session.0) {
        return Err(invalid(
            "native turn incarnation is not owned by this session",
        ));
    }
    Ok(())
}
async fn resolve_on<C: ConnectionTrait>(
    conn: &C,
    row: &observation::Model,
) -> Result<Option<TurnId>> {
    let mut candidates =
        input::Entity::find().filter(input::Column::IncarnationId.eq(row.incarnation_id));
    match row.source.as_str() {
        "spawn_task" => candidates = candidates.filter(input::Column::MessageSeq.is_null()),
        "inbox" => {
            let sequences: Vec<i64> = serde_json::from_value(row.input_sequences.clone())
                .map_err(|e| AgentError::Store(e.to_string()))?;
            candidates = candidates.filter(input::Column::MessageSeq.is_in(sequences));
        }
        "goal_resume" => return Ok(None),
        _ => return Err(invalid("unknown native turn source")),
    }
    let found = candidates.all(conn).await.map_err(store_err)?;
    if found.len() > 1 {
        return Err(invalid(
            "native turn combines more than one hosted user turn",
        ));
    }
    let candidate = found.first().map(|row| row.turn_id);
    if row.turn_id.is_some() && row.turn_id != candidate {
        return Err(invalid("native turn input changed its hosted turn"));
    }
    if row.turn_id.is_none() {
        if let Some(turn) = candidate {
            let already = observation::Entity::find()
                .filter(observation::Column::IncarnationId.eq(row.incarnation_id))
                .filter(observation::Column::RuntimeId.eq(row.runtime_id))
                .filter(observation::Column::TurnId.eq(turn))
                .one(conn)
                .await
                .map_err(store_err)?;
            if already.is_some_and(|other| other.native_turn != row.native_turn) {
                return Err(invalid(
                    "one hosted turn cannot be consumed by two native turns",
                ));
            }
            observation::Entity::update_many()
                .col_expr(
                    observation::Column::TurnId,
                    sea_orm::sea_query::Expr::value(Some(turn)),
                )
                .filter(observation::Column::IncarnationId.eq(row.incarnation_id))
                .filter(observation::Column::RuntimeId.eq(row.runtime_id))
                .filter(observation::Column::NativeTurn.eq(row.native_turn))
                .exec(conn)
                .await
                .map_err(store_err)?;
        }
    }
    Ok(candidate.map(TurnId))
}

/// Save the inbox receipt before publishing a hosted turn as running. `None` identifies its spawn task.
pub async fn record_native_turn_input(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    incarnation: CodeIncarnationId,
    turn: TurnId,
    message_seq: Option<i64>,
) -> Result<()> {
    let tx = store.conn.begin().await.map_err(store_err)?;
    check_scope(&tx, owner, session, incarnation).await?;
    record_input_on(&tx, owner, session, incarnation, turn, message_seq).await?;
    tx.commit().await.map_err(store_err)
}

async fn record_input_on<C: ConnectionTrait>(
    conn: &C,
    owner: &OwnerId,
    session: SessionId,
    incarnation: CodeIncarnationId,
    turn: TurnId,
    message_seq: Option<i64>,
) -> Result<()> {
    if message_seq.is_some_and(|seq| seq <= 0) {
        return Err(invalid("native input has an invalid inbox sequence"));
    }
    if entities::turn::Entity::find_by_id(turn.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .is_none_or(|row| row.owner != owner.as_str() || row.session_id != session.0)
    {
        return Err(invalid("native input turn is not owned by this session"));
    }
    if let Some(existing) = input::Entity::find_by_id(turn.0)
        .one(conn)
        .await
        .map_err(store_err)?
    {
        if existing.incarnation_id != incarnation.0 || existing.message_seq != message_seq {
            return Err(invalid("a hosted turn cannot change its native input"));
        }
    } else {
        if message_seq.is_none()
            && input::Entity::find()
                .filter(input::Column::IncarnationId.eq(incarnation.0))
                .filter(input::Column::MessageSeq.is_null())
                .one(conn)
                .await
                .map_err(store_err)?
                .is_some()
        {
            return Err(invalid("an incarnation has only one spawn input"));
        }
        input::ActiveModel {
            turn_id: Set(turn.0),
            incarnation_id: Set(incarnation.0),
            message_seq: Set(message_seq),
        }
        .insert(conn)
        .await
        .map_err(store_err)?;
    }
    // The supervisor can report its start before the sender records the receipt.
    for row in observation::Entity::find()
        .filter(observation::Column::IncarnationId.eq(incarnation.0))
        .filter(observation::Column::TurnId.is_null())
        .all(conn)
        .await
        .map_err(store_err)?
    {
        resolve_on(conn, &row).await?;
    }
    Ok(())
}

/// Retain starts even when they race the hosted receipt. Unmatched control turns stay unbound.
#[allow(clippy::too_many_arguments)]
pub async fn observe_native_turn(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    incarnation: CodeIncarnationId,
    runtime: uuid::Uuid,
    native_turn: u32,
    source: &str,
    sequences: &[i64],
) -> Result<Option<TurnId>> {
    if runtime.is_nil()
        || native_turn == 0
        || sequences.len() > 128
        || sequences.iter().any(|seq| *seq <= 0)
        || !matches!(source, "spawn_task" | "inbox" | "goal_resume")
        || (source != "inbox" && !sequences.is_empty())
    {
        return Err(invalid("invalid native turn input identity"));
    }
    let tx = store.conn.begin().await.map_err(store_err)?;
    check_scope(&tx, owner, session, incarnation).await?;
    let key = (incarnation.0, runtime, i64::from(native_turn));
    let value = serde_json::json!(sequences);
    let row = match observation::Entity::find_by_id(key)
        .one(&tx)
        .await
        .map_err(store_err)?
    {
        Some(row) => {
            if row.source != source || row.input_sequences != value {
                return Err(invalid("native turn replay changed its input"));
            }
            row
        }
        None => observation::ActiveModel {
            incarnation_id: Set(incarnation.0),
            runtime_id: Set(runtime),
            native_turn: Set(i64::from(native_turn)),
            source: Set(source.into()),
            input_sequences: Set(value),
            turn_id: Set(None),
            terminal_status: Set(None),
            assistant_record: Set(None),
            start_journaled: Set(false),
            output_journaled: Set(false),
        }
        .insert(&tx)
        .await
        .map_err(store_err)?,
    };
    let mapped = resolve_on(&tx, &row).await?;
    tx.commit().await.map_err(store_err)?;
    Ok(mapped)
}

pub(super) async fn hosted_turn_for_native_on<C: ConnectionTrait>(
    conn: &C,
    incarnation: CodeIncarnationId,
    runtime: uuid::Uuid,
    native_turn: u32,
) -> Result<Option<TurnId>> {
    Ok(
        observation::Entity::find_by_id((incarnation.0, runtime, i64::from(native_turn)))
            .one(conn)
            .await
            .map_err(store_err)?
            .and_then(|row| row.turn_id)
            .map(TurnId),
    )
}
pub async fn hosted_turn_for_native(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    incarnation: CodeIncarnationId,
    runtime: uuid::Uuid,
    native_turn: u32,
) -> Result<Option<TurnId>> {
    let tx = store.conn.begin().await.map_err(store_err)?;
    check_scope(&tx, owner, session, incarnation).await?;
    let result = hosted_turn_for_native_on(&tx, incarnation, runtime, native_turn).await?;
    tx.commit().await.map_err(store_err)?;
    Ok(result)
}
pub async fn native_turn_for_host(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    incarnation: CodeIncarnationId,
    runtime: uuid::Uuid,
    turn: TurnId,
) -> Result<Option<u32>> {
    let tx = store.conn.begin().await.map_err(store_err)?;
    check_scope(&tx, owner, session, incarnation).await?;
    let result = observation::Entity::find()
        .filter(observation::Column::IncarnationId.eq(incarnation.0))
        .filter(observation::Column::RuntimeId.eq(runtime))
        .filter(observation::Column::TurnId.eq(turn.0))
        .one(&tx)
        .await
        .map_err(store_err)?
        .map(|row| {
            u32::try_from(row.native_turn).map_err(|_| invalid("invalid native turn counter"))
        })
        .transpose()?;
    tx.commit().await.map_err(store_err)?;
    Ok(result)
}

/// A host receipt may still be in flight while the supervisor requests a decision.
pub const NATIVE_TURN_INPUT_PENDING: &str = "native turn is waiting for its hosted input receipt";

pub(super) async fn native_turn_input_pending_on<C: ConnectionTrait>(
    conn: &C,
    incarnation: CodeIncarnationId,
    runtime: uuid::Uuid,
    native_turn: u32,
) -> Result<bool> {
    Ok(
        observation::Entity::find_by_id((incarnation.0, runtime, i64::from(native_turn)))
            .one(conn)
            .await
            .map_err(store_err)?
            .is_some_and(|row| {
                row.turn_id.is_none()
                    && row.terminal_status.is_none()
                    && matches!(row.source.as_str(), "spawn_task" | "inbox")
            }),
    )
}

/// Retain completion when the supervisor finishes before its hosted input is recorded.
pub async fn record_native_turn_terminal(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    incarnation: CodeIncarnationId,
    runtime: uuid::Uuid,
    native_turn: u32,
    status: crate::code::TurnStatus,
) -> Result<()> {
    let status = match status {
        crate::code::TurnStatus::Completed => "completed",
        crate::code::TurnStatus::Failed => "failed",
        crate::code::TurnStatus::Interrupted => "interrupted",
        _ => return Err(invalid("invalid native terminal status")),
    };
    let tx = store.conn.begin().await.map_err(store_err)?;
    check_scope(&tx, owner, session, incarnation).await?;
    let row = observation::Entity::find_by_id((incarnation.0, runtime, i64::from(native_turn)))
        .one(&tx)
        .await
        .map_err(store_err)?
        .ok_or_else(|| invalid("native turn completion has no start identity"))?;
    if row
        .terminal_status
        .as_deref()
        .is_some_and(|existing| existing != status)
    {
        return Err(invalid("native turn replay changed its terminal status"));
    }
    let mut active: observation::ActiveModel = row.into();
    active.terminal_status = Set(Some(status.into()));
    active.update(&tx).await.map_err(store_err)?;
    tx.commit().await.map_err(store_err)
}

/// Return the authenticated runtime only when it advertises exact input identities.
pub async fn native_turn_identity_runtime(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    incarnation: CodeIncarnationId,
) -> Result<Option<uuid::Uuid>> {
    let tx = store.conn.begin().await.map_err(store_err)?;
    check_scope(&tx, owner, session, incarnation).await?;
    let scope = entities::code_session_incarnation::Entity::find_by_id(incarnation.0)
        .one(&tx)
        .await
        .map_err(store_err)?
        .expect("scope was checked");
    let setting = entities::setting::Entity::find_by_id(format!(
        "code.incarnations.{incarnation}.steering_protocol"
    ))
    .one(&tx)
    .await
    .map_err(store_err)?;
    let value = setting.filter(|row| {
        row.value_json
            .get("turn_identity_protocol")
            .and_then(serde_json::Value::as_u64)
            == Some(1)
            && row
                .value_json
                .get("sandbox_id")
                .and_then(serde_json::Value::as_str)
                == scope.sandbox_id.as_deref()
    });
    let result = value
        .and_then(|row| {
            row.value_json
                .get("runtime_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
        })
        .filter(|id| !id.is_nil());
    tx.commit().await.map_err(store_err)?;
    Ok(result)
}

/// Save bounded assistant output until its input receipt identifies the hosted turn.
pub async fn record_native_turn_output(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    incarnation: CodeIncarnationId,
    runtime: uuid::Uuid,
    native_turn: u32,
    payload: &serde_json::Value,
) -> Result<()> {
    let tx = store.conn.begin().await.map_err(store_err)?;
    check_scope(&tx, owner, session, incarnation).await?;
    let row = observation::Entity::find_by_id((incarnation.0, runtime, i64::from(native_turn)))
        .one(&tx)
        .await
        .map_err(store_err)?
        .ok_or_else(|| invalid("native output has no start identity"))?;
    let body = payload
        .get("body")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let bounded = serde_json::json!({"body":body.chars().take(crate::MAX_EVENT_TEXT_CHARS).collect::<String>(),"truncated":body.chars().count()>crate::MAX_EVENT_TEXT_CHARS || payload.get("truncated").and_then(serde_json::Value::as_bool)==Some(true)});
    if row
        .assistant_record
        .as_ref()
        .is_some_and(|old| old != &bounded)
    {
        return Err(invalid("native turn replay changed its output"));
    }
    let mut active: observation::ActiveModel = row.into();
    active.assistant_record = Set(Some(bounded));
    active.update(&tx).await.map_err(store_err)?;
    tx.commit().await.map_err(store_err)
}

/// Journal a bound turn and its completion in the same transaction as its status.
/// An unbound background turn cannot update the running hosted turn.
pub async fn project_native_turn(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    incarnation: CodeIncarnationId,
    runtime: uuid::Uuid,
    turn: TurnId,
) -> Result<(Vec<crate::code::SequencedEvent>, bool)> {
    use crate::code::{Event, HarnessNoticeLevel, TurnUsage};
    use crate::{Attention, AttentionSource, AttentionState};
    let tx = store.conn.begin().await.map_err(store_err)?;
    check_scope(&tx, owner, session, incarnation).await?;
    let running = entities::session::Entity::find_by_id(session.0)
        .filter(entities::session::Column::Owner.eq(owner.as_str()))
        .filter(entities::session::Column::Lifecycle.eq("running"))
        .one(&tx)
        .await
        .map_err(store_err)?
        .is_some();
    if !running {
        tx.commit().await.map_err(store_err)?;
        return Ok((vec![], false));
    }
    let Some(row) = observation::Entity::find()
        .filter(observation::Column::IncarnationId.eq(incarnation.0))
        .filter(observation::Column::RuntimeId.eq(runtime))
        .filter(observation::Column::TurnId.eq(turn.0))
        .one(&tx)
        .await
        .map_err(store_err)?
    else {
        tx.commit().await.map_err(store_err)?;
        return Ok((vec![], false));
    };
    let Some(host) = entities::turn::Entity::find_by_id(turn.0)
        .filter(entities::turn::Column::Owner.eq(owner.as_str()))
        .filter(entities::turn::Column::SessionId.eq(session.0))
        .filter(entities::turn::Column::Status.eq("running"))
        .one(&tx)
        .await
        .map_err(store_err)?
    else {
        tx.commit().await.map_err(store_err)?;
        return Ok((vec![], false));
    };
    let mut events = vec![];
    if !row.start_journaled {
        events.push(Event::TurnStarted { turn_id: turn });
    }
    if !row.output_journaled {
        if let Some(record) = &row.assistant_record {
            let body = record
                .get("body")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if !body.is_empty() {
                events.push(Event::AssistantMessage {
                    text: body.to_owned(),
                    parent_call_id: None,
                });
            }
            if record.get("truncated").and_then(serde_json::Value::as_bool) == Some(true) {
                events.push(Event::HarnessNotice {
                    level: HarnessNoticeLevel::Warning,
                    message:
                        "The engine's answer was truncated; the full text stayed in the sandbox."
                            .into(),
                });
            }
        }
    }
    let settled = row.terminal_status.is_some();
    if let Some(status) = &row.terminal_status {
        let (event, attention) = match status.as_str() {
            "completed" => (
                Event::TurnCompleted {
                    usage: TurnUsage::default(),
                    checkpoint: None,
                    stop_reason: None,
                },
                Attention::new(AttentionState::DoneUnreviewed, AttentionSource::Lifecycle),
            ),
            "interrupted" => (
                Event::TurnInterrupted { usage: None },
                Attention::needs_you("the turn was interrupted", AttentionSource::Lifecycle),
            ),
            _ => (
                Event::TurnFailed {
                    error: crate::BoundedError {
                        message: "the engine turn failed".into(),
                    },
                    detail: None,
                },
                Attention::needs_you("the engine turn failed", AttentionSource::Lifecycle),
            ),
        };
        let mut active: entities::turn::ActiveModel = host.into();
        active.status = Set(status.clone());
        active.ended_at = Set(Some(chrono::Utc::now()));
        active.update(&tx).await.map_err(store_err)?;
        events.push(event);
        super::session::replace_session_attention_on(&tx, owner, session, &attention, false)
            .await?;
        entities::session::Entity::update_many()
            .col_expr(
                entities::session::Column::Lifecycle,
                sea_orm::sea_query::Expr::value("idle"),
            )
            .filter(entities::session::Column::Lifecycle.eq("running"))
            .filter(entities::session::Column::Id.eq(session.0))
            .filter(entities::session::Column::Owner.eq(owner.as_str()))
            .exec(&tx)
            .await
            .map_err(store_err)?;
    }
    let mut projected = vec![];
    for event in events {
        let seq = super::append_event_on_locked(&tx, owner, session, &event).await?;
        projected.push(crate::code::SequencedEvent { seq, event });
    }
    let has_output = row.assistant_record.is_some();
    let mut active: observation::ActiveModel = row.into();
    active.start_journaled = Set(true);
    active.output_journaled = Set(has_output);
    active.update(&tx).await.map_err(store_err)?;
    tx.commit().await.map_err(store_err)?;
    Ok((projected, settled))
}

/// Commit the delivered turn, its receipt, and its running state together.
/// A moved queue row keeps its identity; an edited row remains queued.
pub async fn insert_remote_turn_with_input(
    store: &DbStore,
    owner: &OwnerId,
    incarnation: CodeIncarnationId,
    turn: &crate::code::Turn,
    expected: Option<&crate::code::QueuedTurn>,
    message_seq: Option<i64>,
) -> Result<crate::code::Turn> {
    use sea_orm::sea_query::Expr;
    let tx = store.conn.begin().await.map_err(store_err)?;
    check_scope(&tx, owner, turn.session_id, incarnation).await?;
    let mut turn = turn.clone();
    if let Some(expected) = expected {
        if expected.session_id != turn.session_id || expected.id != turn.id {
            return Err(invalid(
                "remote promotion does not match the delivered turn",
            ));
        }
        if !super::queued::promote_moved_queued_turn_on(&tx, owner, expected, &turn).await? {
            turn.id = TurnId::new();
            super::turn::insert_turn_on(&tx, owner, &turn).await?;
        }
    } else {
        super::turn::insert_turn_on(&tx, owner, &turn).await?;
    }
    record_input_on(
        &tx,
        owner,
        turn.session_id,
        incarnation,
        turn.id,
        message_seq,
    )
    .await?;
    entities::session::Entity::update_many()
        .col_expr(entities::session::Column::Lifecycle, Expr::value("running"))
        .filter(entities::session::Column::Id.eq(turn.session_id.0))
        .filter(entities::session::Column::Owner.eq(owner.as_str()))
        .exec(&tx)
        .await
        .map_err(store_err)?;
    super::session::replace_session_attention_on(
        &tx,
        owner,
        turn.session_id,
        &crate::Attention::working(crate::AttentionSource::Lifecycle),
        false,
    )
    .await?;
    tx.commit().await.map_err(store_err)?;
    Ok(turn)
}
