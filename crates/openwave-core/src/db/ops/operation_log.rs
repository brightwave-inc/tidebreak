//! Durable reverse-RPC operation log (issue #858).
//!
//! The store persists an idempotency-keyed operation record and enforces the
//! commit predicate transactionally, mirroring the run tier's fenced result
//! commit. Bodies are opaque blobs: the protocol tier
//! (`openwave-sandbox-protocol::oplog`) owns their typed shape and the mapping
//! onto `ClaimOutcome`, so this tier carries no reverse-RPC wire types.
//!
//! The whole correctness rule is `claim`: an unseen identity is inserted
//! `Claimed`; a terminal one replays its recorded body; a re-issue with a
//! different fingerprint conflicts; and a `Claimed` entry is either a concurrent
//! duplicate of *this* process lifetime (same `owner_epoch`) or the after-crash
//! ambiguity of a *prior* lifetime (a different `owner_epoch`) that must not
//! re-execute an external effect.

use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, EntityTrait, IntoActiveModel, PaginatorTrait, QueryFilter, Set, TransactionTrait,
    TryInsertResult,
};

use crate::error::Result;
use crate::storage::{
    OperationClaimOutcome, OperationLogEntry, OperationLogState, OperationLogWrite,
};

use super::super::entities::operation_log;
use super::super::{entities, store_err, DbStore};

const STATE_CLAIMED: &str = "claimed";
const STATE_RECORDED: &str = "recorded";
const STATE_FAILED: &str = "failed";

fn state_from_str(state: &str) -> OperationLogState {
    match state {
        STATE_RECORDED => OperationLogState::Recorded,
        STATE_FAILED => OperationLogState::Failed,
        // Any other stored value is a `claimed` entry; the CHECK constraint
        // bounds the column to the three known states.
        _ => OperationLogState::Claimed,
    }
}

/// Atomically claim `operation_id` under `run_id`, or observe its existing
/// state. See the module docs for the predicate.
pub(in crate::db) async fn claim(
    store: &DbStore,
    run_id: uuid::Uuid,
    operation_id: uuid::Uuid,
    fingerprint: &[u8],
    external_effect: bool,
    owner_epoch: uuid::Uuid,
) -> Result<OperationClaimOutcome> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let now = Utc::now();

    // The insert is the serialization point: on both backends a duplicate
    // `(run_id, operation_id)` is refused by the primary key, so exactly one
    // racing claim inserts and the rest observe the existing row.
    let inserted = entities::operation_log::Entity::insert(operation_log::ActiveModel {
        run_id: Set(run_id),
        operation_id: Set(operation_id),
        state: Set(STATE_CLAIMED.to_owned()),
        fingerprint: Set(fingerprint.to_vec()),
        external_effect: Set(external_effect),
        owner_epoch: Set(owner_epoch),
        body: Set(None),
        retained: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
    })
    .on_conflict_do_nothing()
    .exec_without_returning(&transaction)
    .await
    .map_err(store_err)?;

    // A do-nothing conflict reports zero rows affected; only a real insert of
    // one row is a fresh claim.
    if matches!(inserted, TryInsertResult::Inserted(1)) {
        transaction.commit().await.map_err(store_err)?;
        return Ok(OperationClaimOutcome::Fresh);
    }

    let Some(existing) = entities::operation_log::Entity::find_by_id((run_id, operation_id))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        // The row was evicted between the refused insert and this read. An
        // evicted identity can never be re-issued, so refuse rather than
        // silently re-claiming it.
        transaction.rollback().await.map_err(store_err)?;
        return Ok(OperationClaimOutcome::Conflict);
    };

    if existing.fingerprint != fingerprint {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(OperationClaimOutcome::Conflict);
    }

    let outcome = match state_from_str(&existing.state) {
        OperationLogState::Recorded => {
            OperationClaimOutcome::Recorded(existing.body.unwrap_or_default())
        }
        OperationLogState::Failed => {
            OperationClaimOutcome::Failed(existing.body.unwrap_or_default())
        }
        OperationLogState::Claimed => {
            if existing.owner_epoch == owner_epoch {
                OperationClaimOutcome::OwnedClaim
            } else if existing.external_effect {
                OperationClaimOutcome::ForeignClaim
            } else {
                // A foreign, no-external-effect claim is safe to re-drive: take
                // over the claim for this lifetime and report it fresh.
                let mut active = existing.into_active_model();
                active.owner_epoch = Set(owner_epoch);
                active.updated_at = Set(now);
                sea_orm::ActiveModelTrait::update(active, &transaction)
                    .await
                    .map_err(store_err)?;
                OperationClaimOutcome::Fresh
            }
        }
    };
    transaction.commit().await.map_err(store_err)?;
    Ok(outcome)
}

/// Settle a `Claimed` entry to `Recorded`. Idempotent for an already-`Recorded`
/// entry (first write wins).
pub(in crate::db) async fn record(
    store: &DbStore,
    run_id: uuid::Uuid,
    operation_id: uuid::Uuid,
    body: &[u8],
) -> Result<OperationLogWrite> {
    settle(store, run_id, operation_id, STATE_RECORDED, body).await
}

/// Settle a `Claimed` entry to `Failed`. Idempotent for an already-`Failed`
/// entry.
pub(in crate::db) async fn fail(
    store: &DbStore,
    run_id: uuid::Uuid,
    operation_id: uuid::Uuid,
    body: &[u8],
) -> Result<OperationLogWrite> {
    settle(store, run_id, operation_id, STATE_FAILED, body).await
}

async fn settle(
    store: &DbStore,
    run_id: uuid::Uuid,
    operation_id: uuid::Uuid,
    terminal: &str,
    body: &[u8],
) -> Result<OperationLogWrite> {
    let now = Utc::now();
    let transaction = store.conn.begin().await.map_err(store_err)?;

    // The `state = claimed` guard makes the transition atomic and first-writer-
    // wins: only the write that finds the entry still claimed settles it, so a
    // racing or re-delivered terminal write never overwrites the first body.
    let updated = entities::operation_log::Entity::update_many()
        .col_expr(operation_log::Column::State, Expr::value(terminal))
        .col_expr(operation_log::Column::Body, Expr::value(body.to_vec()))
        .col_expr(operation_log::Column::Retained, Expr::value(true))
        .col_expr(operation_log::Column::UpdatedAt, Expr::value(now))
        .filter(operation_log::Column::RunId.eq(run_id))
        .filter(operation_log::Column::OperationId.eq(operation_id))
        .filter(operation_log::Column::State.eq(STATE_CLAIMED))
        .exec(&transaction)
        .await
        .map_err(store_err)?;

    if updated.rows_affected == 1 {
        transaction.commit().await.map_err(store_err)?;
        return Ok(OperationLogWrite::Committed);
    }

    let existing = entities::operation_log::Entity::find_by_id((run_id, operation_id))
        .one(&transaction)
        .await
        .map_err(store_err)?;
    transaction.rollback().await.map_err(store_err)?;
    Ok(match existing {
        Some(entry) if entry.state == terminal => OperationLogWrite::AlreadyTerminal,
        _ => OperationLogWrite::NotClaimed,
    })
}

/// Read one entry's current state, if the log knows it.
pub(in crate::db) async fn state(
    store: &DbStore,
    run_id: uuid::Uuid,
    operation_id: uuid::Uuid,
) -> Result<Option<OperationLogEntry>> {
    let entry = entities::operation_log::Entity::find_by_id((run_id, operation_id))
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(|row| OperationLogEntry {
            state: state_from_str(&row.state),
            body: row.body,
            external_effect: row.external_effect,
            retained: row.retained,
        });
    Ok(entry)
}

/// Drop one entry. #859 owns the eviction policy; this removes the row.
pub(in crate::db) async fn evict(
    store: &DbStore,
    run_id: uuid::Uuid,
    operation_id: uuid::Uuid,
) -> Result<()> {
    entities::operation_log::Entity::delete_by_id((run_id, operation_id))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

/// How many entries a run currently retains.
pub(in crate::db) async fn len(store: &DbStore, run_id: uuid::Uuid) -> Result<usize> {
    let count = entities::operation_log::Entity::find()
        .filter(operation_log::Column::RunId.eq(run_id))
        .count(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(count as usize)
}
