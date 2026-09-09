//! Per-session pull-request delivery state and outbox (issue 3203).
//!
//! The journal is replayable and per session. This module gives the
//! publisher a durable per-session memory: what state token was last planned
//! for a (session, PR, family), and which journal payloads are still queued
//! for delivery. The journal append and the outbox `delivered` write commit
//! in the same transaction, so an event is appended exactly once per
//! session. A failed append leaves the row queued; a delivery sweep retries
//! it, and one session's failure never marks another session's delivery as
//! done.

use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};

use crate::code::{Event, SessionId};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};

/// One durable pull-request delivery family. State tokens are compared
/// lexicographically; the publisher assigns monotone occurrences to
/// re-entrant transitions of the same family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrDeliveryFamily {
    Opened,
    ChecksPending,
    ChecksFailed,
    ReviewRequested,
    ChangesRequested,
    Approved,
    Mergeable,
    Merged,
    Watch,
    Review,
}

impl PrDeliveryFamily {
    /// Stable wire/storage token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Opened => "opened",
            Self::ChecksPending => "checks_pending",
            Self::ChecksFailed => "checks_failed",
            Self::ReviewRequested => "review_requested",
            Self::ChangesRequested => "changes_requested",
            Self::Approved => "approved",
            Self::Mergeable => "mergeable",
            Self::Merged => "merged",
            Self::Watch => "watch",
            Self::Review => "review",
        }
    }
}

/// Whether the same state token was already planned for this session+PR.
pub async fn event_already_planned(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    host: &str,
    repo_owner: &str,
    repo_name: &str,
    number: u64,
    family: PrDeliveryFamily,
    state_token: &str,
) -> Result<bool> {
    let number = i64::try_from(number)
        .map_err(|_| AgentError::Store(format!("pull request number {number} overflows")))?;
    Ok(entities::code_pr_delivery_state::Entity::find()
        .filter(entities::code_pr_delivery_state::Column::SessionId.eq(session_id.0))
        .filter(entities::code_pr_delivery_state::Column::Family.eq(family.as_str()))
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .is_some_and(|row| {
            row.owner == owner.as_str()
                && row.host == host
                && row.repo_owner == repo_owner
                && row.repo_name == repo_name
                && row.number == number
                && row.last_state_token == state_token
        }))
}

/// Record that state token `next` will be delivered as the next occurrence
/// of `family` on this session, returning the occurrence to journal.
///
/// Idempotent: a replayed or racing call for the same planned token returns
/// the already-planned occurrence. A token that differs from the stored one
/// advances the occurrence counter, so a re-entrant transition is never
/// suppressed by an older equal string (e.g. pending after failed uses its
/// own token built from the head SHA and check bucket).
pub async fn plan_delivery(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    host: &str,
    repo_owner: &str,
    repo_name: &str,
    number: u64,
    family: PrDeliveryFamily,
    state_token: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<i64>> {
    let number = i64::try_from(number)
        .map_err(|_| AgentError::Store(format!("pull request number {number} overflows")))?;
    let existing = entities::code_pr_delivery_state::Entity::find()
        .filter(entities::code_pr_delivery_state::Column::SessionId.eq(session_id.0))
        .filter(entities::code_pr_delivery_state::Column::Family.eq(family.as_str()))
        .one(&store.conn)
        .await
        .map_err(store_err)?;
    let occurrence = match existing {
        Some(row) if row.last_state_token == state_token => return Ok(None),
        Some(row) => row.next_occurrence,
        None => 1,
    };
    entities::code_pr_delivery_state::Entity::insert(
        entities::code_pr_delivery_state::ActiveModel {
            session_id: Set(session_id.0),
            family: Set(family.as_str().to_owned()),
            owner: Set(owner.as_str().to_owned()),
            host: Set(host.to_owned()),
            repo_owner: Set(repo_owner.to_owned()),
            repo_name: Set(repo_name.to_owned()),
            number: Set(number),
            last_state_token: Set(state_token.to_owned()),
            next_occurrence: Set(occurrence.saturating_add(1)),
            updated_at: Set(now),
        },
    )
    .on_conflict(
        OnConflict::columns([
            entities::code_pr_delivery_state::Column::SessionId,
            entities::code_pr_delivery_state::Column::Family,
        ])
        .update_columns([
            entities::code_pr_delivery_state::Column::LastStateToken,
            entities::code_pr_delivery_state::Column::NextOccurrence,
            entities::code_pr_delivery_state::Column::UpdatedAt,
        ])
        .to_owned(),
    )
    .exec_without_returning(&store.conn)
    .await
    .map_err(store_err)?;
    Ok(Some(occurrence))
}

/// Enqueue one journal payload for delivery, idempotently.
pub async fn enqueue_delivery(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    host: &str,
    repo_owner: &str,
    repo_name: &str,
    number: u64,
    family: PrDeliveryFamily,
    occurrence: i64,
    event: &Event,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    let number = i64::try_from(number)
        .map_err(|_| AgentError::Store(format!("pull request number {number} overflows")))?;
    let json = serde_json::to_string(event).map_err(AgentError::from)?;
    let result = entities::code_pr_delivery_outbox::Entity::insert(
        entities::code_pr_delivery_outbox::ActiveModel {
            session_id: Set(session_id.0),
            family: Set(family.as_str().to_owned()),
            occurrence: Set(occurrence),
            owner: Set(owner.as_str().to_owned()),
            host: Set(host.to_owned()),
            repo_owner: Set(repo_owner.to_owned()),
            repo_name: Set(repo_name.to_owned()),
            number: Set(number),
            event_json: Set(json),
            queued_at: Set(now),
            delivered_at: Set(None),
            delivered_seq: Set(None),
        },
    )
    .on_conflict(
        OnConflict::columns([
            entities::code_pr_delivery_outbox::Column::SessionId,
            entities::code_pr_delivery_outbox::Column::Family,
            entities::code_pr_delivery_outbox::Column::Occurrence,
        ])
        .do_nothing()
        .to_owned(),
    )
    .exec_without_returning(&store.conn)
    .await
    .map_err(store_err)?;
    Ok(result != 0)
}

/// Pending outbox rows for one session, in occurrence order.
pub async fn pending_deliveries_for_session(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
) -> Result<Vec<(PrDeliveryFamily, i64, Event)>> {
    let rows = entities::code_pr_delivery_outbox::Entity::find()
        .filter(entities::code_pr_delivery_outbox::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_pr_delivery_outbox::Column::SessionId.eq(session_id.0))
        .filter(entities::code_pr_delivery_outbox::Column::DeliveredAt.is_null())
        .order_by_asc(entities::code_pr_delivery_outbox::Column::QueuedAt)
        .order_by_asc(entities::code_pr_delivery_outbox::Column::Family)
        .order_by_asc(entities::code_pr_delivery_outbox::Column::Occurrence)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push((
            parse_family(&row.family)?,
            row.occurrence,
            serde_json::from_str(&row.event_json).map_err(AgentError::from)?,
        ));
    }
    Ok(out)
}

/// All pending outbox rows, oldest first, for the delivery sweep.
pub async fn pending_deliveries_all_sessions(
    store: &DbStore,
    limit: u64,
) -> Result<Vec<(OwnerId, SessionId, PrDeliveryFamily, i64, Event)>> {
    use sea_orm::QueryOrder;
    let rows = entities::code_pr_delivery_outbox::Entity::find()
        .filter(entities::code_pr_delivery_outbox::Column::DeliveredAt.is_null())
        .order_by_asc(entities::code_pr_delivery_outbox::Column::QueuedAt)
        .limit(limit)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push((
            OwnerId::new(&row.owner)?,
            SessionId(row.session_id),
            parse_family(&row.family)?,
            row.occurrence,
            serde_json::from_str(&row.event_json).map_err(AgentError::from)?,
        ));
    }
    Ok(out)
}

/// Append a journal event and mark one outbox row delivered in one
/// transaction. Returns the journal sequence, or `None` if the row was
/// already delivered (a replayed sweep).
pub async fn deliver_outbox_row(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    spawn_epoch: i64,
    family: PrDeliveryFamily,
    occurrence: i64,
    event: Event,
) -> Result<Option<i64>> {
    let txn = store.conn.begin().await.map_err(store_err)?;
    if !super::acquire_code_session_write_lock(&txn, session_id).await? {
        return Err(AgentError::Store(format!(
            "code session {session_id} not found"
        )));
    }
    let session = entities::session::Entity::find_by_id(session_id.0)
        .filter(entities::session::Column::Owner.eq(owner.as_str()))
        .one(&txn)
        .await
        .map_err(store_err)?;
    if session
        .as_ref()
        .is_none_or(|row| row.spawn_epoch != spawn_epoch)
    {
        txn.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "stale spawn epoch {spawn_epoch} for code session {session_id}"
        )));
    }
    let row = entities::code_pr_delivery_outbox::Entity::find()
        .filter(entities::code_pr_delivery_outbox::Column::SessionId.eq(session_id.0))
        .filter(entities::code_pr_delivery_outbox::Column::Family.eq(family.as_str()))
        .filter(entities::code_pr_delivery_outbox::Column::Occurrence.eq(occurrence))
        .one(&txn)
        .await
        .map_err(store_err)?;
    let Some(row) = row else {
        txn.rollback().await.map_err(store_err)?;
        return Ok(None);
    };
    if row.delivered_at.is_some() {
        txn.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    if row.owner != owner.as_str() || row.session_id != session_id.0 {
        txn.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let seq = super::journal::append_event_on_locked(&txn, owner, session_id, &event).await?;
    entities::code_pr_delivery_outbox::Entity::update_many()
        .col_expr(
            entities::code_pr_delivery_outbox::Column::DeliveredAt,
            Expr::value(Some(chrono::Utc::now())),
        )
        .col_expr(
            entities::code_pr_delivery_outbox::Column::DeliveredSeq,
            Expr::value(Some(seq)),
        )
        .filter(entities::code_pr_delivery_outbox::Column::SessionId.eq(session_id.0))
        .filter(entities::code_pr_delivery_outbox::Column::Family.eq(family.as_str()))
        .filter(entities::code_pr_delivery_outbox::Column::Occurrence.eq(occurrence))
        .filter(entities::code_pr_delivery_outbox::Column::DeliveredAt.is_null())
        .exec(&txn)
        .await
        .map_err(store_err)?;
    txn.commit().await.map_err(store_err)?;
    Ok(Some(seq))
}

fn parse_family(value: &str) -> Result<PrDeliveryFamily> {
    match value {
        "opened" => Ok(PrDeliveryFamily::Opened),
        "checks_pending" => Ok(PrDeliveryFamily::ChecksPending),
        "checks_failed" => Ok(PrDeliveryFamily::ChecksFailed),
        "review_requested" => Ok(PrDeliveryFamily::ReviewRequested),
        "changes_requested" => Ok(PrDeliveryFamily::ChangesRequested),
        "approved" => Ok(PrDeliveryFamily::Approved),
        "mergeable" => Ok(PrDeliveryFamily::Mergeable),
        "merged" => Ok(PrDeliveryFamily::Merged),
        "watch" => Ok(PrDeliveryFamily::Watch),
        "review" => Ok(PrDeliveryFamily::Review),
        _ => Err(AgentError::Store(format!(
            "unknown pull-request delivery family {value}"
        ))),
    }
}
