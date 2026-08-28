//! Durable sandbox lifetimes for remote sessions (the incarnation intent
//! protocol).
//!
//! The order is the contract: [`create_incarnation_intent`] commits a row
//! *before* the environment is asked to provision, [`activate_incarnation`]
//! records what the spawn returned, and [`stop_incarnation`] is terminal. A
//! crash between spawn and activation leaves an intent row the reconcile
//! sweep can find and cancel, instead of a spending sandbox nothing
//! remembers.
//!
//! The intent insert is also the owner's concurrency reservation. It counts
//! live rows and inserts in one transaction under the `sandbox_incarnation`
//! advisory lock, so two sessions reincarnating at once cannot both read
//! `cap - 1` and both spawn — the check-then-act shape the cap exists to
//! prevent.

use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};

use crate::code::{
    CodeIncarnationId, CodeSessionId, CodeSessionIncarnation, IncarnationAdmission,
    IncarnationState,
};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};
use super::super::agent_run::database_now;
use super::super::{acquire_advisory_lock, AdvisoryLockName};

fn incarnation_from_model(
    model: entities::code_session_incarnation::Model,
) -> Result<CodeSessionIncarnation> {
    Ok(CodeSessionIncarnation {
        id: CodeIncarnationId(model.id),
        owner: OwnerId::new(&model.owner)?,
        session_id: CodeSessionId(model.session_id),
        incarnation: model.incarnation,
        state: IncarnationState::from_str(&model.state).ok_or_else(|| {
            AgentError::Store(format!(
                "invalid stored incarnation state {:?}",
                model.state
            ))
        })?,
        sandbox_id: model.sandbox_id,
        starting_turn: model.starting_turn,
        stop_reason: model.stop_reason,
        spend_microusd: model.spend_microusd,
        terminal_events_journaled: model.terminal_events_journaled,
        events_cursor: model.events_cursor,
        task_output: model.task_output,
        last_wip_ref: model.last_wip_ref,
        created_at: model.created_at,
        activated_at: model.activated_at,
        stopped_at: model.stopped_at,
        updated_at: model.updated_at,
    })
}

/// Rows that hold a slot against the owner's cap: everything not stopped.
fn live_states() -> [&'static str; 2] {
    [
        IncarnationState::Intent.as_str(),
        IncarnationState::Active.as_str(),
    ]
}

/// Reserves the owner's next incarnation for `session_id`, or refuses at
/// the cap.
///
/// On admission the returned row is in [`IncarnationState::Intent`] with the
/// next per-session incarnation number; the caller provisions against it and
/// then activates or stops it. On refusal the sessions holding live
/// incarnations come back so the refusal can name what is running.
pub async fn create_incarnation_intent(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    starting_turn: i32,
    cap: usize,
) -> Result<IncarnationAdmission> {
    let txn = store.conn.begin().await.map_err(store_err)?;
    acquire_advisory_lock(&txn, AdvisoryLockName::SandboxIncarnation).await?;

    let live = entities::code_session_incarnation::Entity::find()
        .filter(entities::code_session_incarnation::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_session_incarnation::Column::State.is_in(live_states()))
        .all(&txn)
        .await
        .map_err(store_err)?;
    if live.len() >= cap {
        let mut running: Vec<CodeSessionId> = live
            .iter()
            .map(|row| CodeSessionId(row.session_id))
            .collect();
        running.sort_by_key(|id| id.0);
        running.dedup();
        txn.rollback().await.map_err(store_err)?;
        return Ok(IncarnationAdmission::CapExhausted { running });
    }

    let last = entities::code_session_incarnation::Entity::find()
        .filter(entities::code_session_incarnation::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_session_incarnation::Column::SessionId.eq(session_id.0))
        .order_by_desc(entities::code_session_incarnation::Column::Incarnation)
        .one(&txn)
        .await
        .map_err(store_err)?;
    if let Some(last) = &last {
        // One live incarnation per session: the serialize-stop-then-resume
        // rule. A successor minted beside a live predecessor would race it
        // for the same engine session.
        if IncarnationState::from_str(&last.state) != Some(IncarnationState::Stopped) {
            txn.rollback().await.map_err(store_err)?;
            return Err(AgentError::Store(format!(
                "session {} already has a live incarnation {}",
                session_id.0, last.incarnation
            )));
        }
    }
    let incarnation = last.as_ref().map_or(1, |row| row.incarnation + 1);

    let now = database_now(&txn).await?;
    let model = entities::code_session_incarnation::ActiveModel {
        id: Set(uuid::Uuid::new_v4()),
        owner: Set(owner.as_str().to_owned()),
        session_id: Set(session_id.0),
        incarnation: Set(incarnation),
        state: Set(IncarnationState::Intent.as_str().to_owned()),
        sandbox_id: Set(None),
        starting_turn: Set(starting_turn),
        stop_reason: Set(None),
        spend_microusd: Set(None),
        terminal_events_journaled: Set(false),
        events_cursor: Set(0),
        task_output: Set(None),
        last_wip_ref: Set(None),
        created_at: Set(now),
        activated_at: Set(None),
        stopped_at: Set(None),
        updated_at: Set(now),
    };
    let inserted = model.insert(&txn).await.map_err(store_err)?;
    txn.commit().await.map_err(store_err)?;
    Ok(IncarnationAdmission::Admitted(Box::new(
        incarnation_from_model(inserted)?,
    )))
}

/// Records the spawned sandbox on an intent row and marks it active.
///
/// Guarded on the current state: activating a row that is not an intent —
/// stopped by the sweep while the spawn was in flight, say — is reported,
/// and the caller must treat the sandbox it spawned as orphaned and cancel
/// it rather than run against a row the protocol already closed.
pub async fn activate_incarnation(
    store: &DbStore,
    owner: &OwnerId,
    id: CodeIncarnationId,
    sandbox_id: &str,
) -> Result<()> {
    let now = database_now(&store.conn).await?;
    let updated = entities::code_session_incarnation::Entity::update_many()
        .col_expr(
            entities::code_session_incarnation::Column::State,
            sea_orm::sea_query::Expr::value(IncarnationState::Active.as_str()),
        )
        .col_expr(
            entities::code_session_incarnation::Column::SandboxId,
            sea_orm::sea_query::Expr::value(sandbox_id),
        )
        .col_expr(
            entities::code_session_incarnation::Column::ActivatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .col_expr(
            entities::code_session_incarnation::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::code_session_incarnation::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_session_incarnation::Column::Id.eq(id.0))
        .filter(
            entities::code_session_incarnation::Column::State.eq(IncarnationState::Intent.as_str()),
        )
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "incarnation {} is not an open intent",
            id.0
        )));
    }
    Ok(())
}

/// Marks an incarnation stopped, from intent or active. Idempotent: a row
/// already stopped keeps its first reason.
pub async fn stop_incarnation(
    store: &DbStore,
    owner: &OwnerId,
    id: CodeIncarnationId,
    reason: Option<&str>,
) -> Result<()> {
    let now = database_now(&store.conn).await?;
    entities::code_session_incarnation::Entity::update_many()
        .col_expr(
            entities::code_session_incarnation::Column::State,
            sea_orm::sea_query::Expr::value(IncarnationState::Stopped.as_str()),
        )
        .col_expr(
            entities::code_session_incarnation::Column::StopReason,
            sea_orm::sea_query::Expr::value(reason),
        )
        .col_expr(
            entities::code_session_incarnation::Column::StoppedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .col_expr(
            entities::code_session_incarnation::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::code_session_incarnation::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_session_incarnation::Column::Id.eq(id.0))
        .filter(entities::code_session_incarnation::Column::State.is_in(live_states()))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

/// Records that the incarnation's terminal events reached the journal.
///
/// The gate reincarnation waits on: a successor built before this is set
/// would resume without its predecessor's last output.
pub async fn mark_incarnation_terminal_events_journaled(
    store: &DbStore,
    owner: &OwnerId,
    id: CodeIncarnationId,
) -> Result<()> {
    let now = database_now(&store.conn).await?;
    entities::code_session_incarnation::Entity::update_many()
        .col_expr(
            entities::code_session_incarnation::Column::TerminalEventsJournaled,
            sea_orm::sea_query::Expr::value(true),
        )
        .col_expr(
            entities::code_session_incarnation::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::code_session_incarnation::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_session_incarnation::Column::Id.eq(id.0))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

/// Updates the last observed spend on an incarnation, for the session
/// ledger. Per-spawn ceilings multiply by reincarnation; the ledger is what
/// a cumulative per-session ceiling reads.
pub async fn record_incarnation_spend(
    store: &DbStore,
    owner: &OwnerId,
    id: CodeIncarnationId,
    spend_microusd: i64,
) -> Result<()> {
    let now = database_now(&store.conn).await?;
    entities::code_session_incarnation::Entity::update_many()
        .col_expr(
            entities::code_session_incarnation::Column::SpendMicrousd,
            sea_orm::sea_query::Expr::value(spend_microusd),
        )
        .col_expr(
            entities::code_session_incarnation::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::code_session_incarnation::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_session_incarnation::Column::Id.eq(id.0))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

/// What one sandbox event writes besides its journal rows.
///
/// Everything here commits in the same transaction as the cursor advance,
/// so a replayed sequence — which the cursor guard skips wholesale — can
/// never have half of its side effects.
#[derive(Debug, Default)]
pub struct IncarnationSideEffects<'a> {
    /// Journal rows to append, in order.
    pub journal: &'a [crate::CodeEvent],
    /// The supervisor's terminal deliverable to retain.
    pub task_output: Option<&'a str>,
    /// The WIP checkpoint ref to retain, for resume.
    pub wip_ref: Option<&'a str>,
    /// Whether this event is the supervisor's goodbye, raising the gate
    /// reincarnation waits on.
    pub terminal_events_journaled: bool,
}

/// Journals one sandbox event's projection, applies its incarnation-row
/// side effects, and advances the ingest cursor — all in one transaction.
///
/// Exactly-once per sandbox event: the write is guarded on
/// `events_cursor < seq`, so a restart that replays an already-ingested
/// event returns `None` and writes nothing — and loses nothing, because
/// the first ingest committed every side effect with the cursor.
/// `spawn_epoch` fences the write the way every journal append is fenced:
/// a superseded worker cannot journal into a session that moved on.
///
/// Returns the journal sequence numbers assigned, in `events` order.
pub async fn ingest_incarnation_event(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    spawn_epoch: i64,
    id: CodeIncarnationId,
    seq: i64,
    side_effects: IncarnationSideEffects<'_>,
) -> Result<Option<Vec<i64>>> {
    let txn = store.conn.begin().await.map_err(store_err)?;
    if !super::acquire_code_session_write_lock(&txn, session_id).await? {
        return Err(AgentError::Store(format!(
            "code session {session_id} not found"
        )));
    }
    let Some(session) = entities::code_session::Entity::find_by_id(session_id.0)
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .one(&txn)
        .await
        .map_err(store_err)?
    else {
        return Err(AgentError::Store(format!(
            "code session {session_id} not found"
        )));
    };
    if session.spawn_epoch != spawn_epoch {
        return Err(AgentError::Store(format!(
            "stale spawn epoch {spawn_epoch} for code session {session_id}"
        )));
    }
    let Some(incarnation) = entities::code_session_incarnation::Entity::find_by_id(id.0)
        .filter(entities::code_session_incarnation::Column::Owner.eq(owner.as_str()))
        .one(&txn)
        .await
        .map_err(store_err)?
    else {
        return Err(AgentError::Store(format!("incarnation {} not found", id.0)));
    };
    if incarnation.events_cursor >= seq {
        txn.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let mut seqs = Vec::with_capacity(side_effects.journal.len());
    for event in side_effects.journal {
        seqs.push(super::journal::append_event_on_locked(&txn, owner, session_id, event).await?);
    }
    let now = database_now(&txn).await?;
    let mut update = entities::code_session_incarnation::Entity::update_many()
        .col_expr(
            entities::code_session_incarnation::Column::EventsCursor,
            sea_orm::sea_query::Expr::value(seq),
        )
        .col_expr(
            entities::code_session_incarnation::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        );
    if let Some(body) = side_effects.task_output {
        update = update.col_expr(
            entities::code_session_incarnation::Column::TaskOutput,
            sea_orm::sea_query::Expr::value(body),
        );
    }
    if let Some(reference) = side_effects.wip_ref {
        update = update.col_expr(
            entities::code_session_incarnation::Column::LastWipRef,
            sea_orm::sea_query::Expr::value(reference),
        );
    }
    if side_effects.terminal_events_journaled {
        update = update.col_expr(
            entities::code_session_incarnation::Column::TerminalEventsJournaled,
            sea_orm::sea_query::Expr::value(true),
        );
    }
    update
        .filter(entities::code_session_incarnation::Column::Id.eq(id.0))
        .exec(&txn)
        .await
        .map_err(store_err)?;
    txn.commit().await.map_err(store_err)?;
    Ok(Some(seqs))
}

/// The session's newest incarnation, in any state.
pub async fn latest_incarnation(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
) -> Result<Option<CodeSessionIncarnation>> {
    entities::code_session_incarnation::Entity::find()
        .filter(entities::code_session_incarnation::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_session_incarnation::Column::SessionId.eq(session_id.0))
        .order_by_desc(entities::code_session_incarnation::Column::Incarnation)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(incarnation_from_model)
        .transpose()
}

/// The session's cumulative spend across every incarnation, in micro-USD.
pub async fn session_spend_microusd(
    store: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
) -> Result<i64> {
    let rows = entities::code_session_incarnation::Entity::find()
        .filter(entities::code_session_incarnation::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_session_incarnation::Column::SessionId.eq(session_id.0))
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(rows.iter().filter_map(|row| row.spend_microusd).sum())
}

/// Intent rows older than `cutoff` that never activated, every owner.
///
/// `_all_owners` because this is the reconcile sweep's system path, not a
/// request path: the sweep runs machine-wide, and each row it finds is a
/// spawn whose outcome nothing recorded. The sweep stops the row and, when the environment knows a
/// matching sandbox, cancels it.
pub async fn stale_incarnation_intents_all_owners(
    store: &DbStore,
    cutoff: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<CodeSessionIncarnation>> {
    entities::code_session_incarnation::Entity::find()
        .filter(
            entities::code_session_incarnation::Column::State.eq(IncarnationState::Intent.as_str()),
        )
        .filter(entities::code_session_incarnation::Column::CreatedAt.lt(cutoff))
        .order_by_asc(entities::code_session_incarnation::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(incarnation_from_model)
        .collect()
}
