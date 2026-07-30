//! Durable sandbox provisioning records (issue #920).
//!
//! One row per container run, written *before* the backend's create call and
//! carrying the host-minted correlation tag, so recovery is driven by the
//! intent rather than by what the provider reports. The state machine is
//! `intended -> committed -> teardown -> done`, with `intended -> teardown`
//! for a lapsed window: a crash on either side of the create converges on the
//! same terminal state through the lapse and the tag sweep.
//!
//! The whole correctness rule is who wins the `intended` row: the driver's
//! handle commit and the sweep's lapse both predicate their update on
//! `state = 'intended'`, so exactly one of them transitions it and the loser
//! observes the outcome instead of racing it.

use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait, TryInsertResult};

use crate::error::Result;
use crate::storage::{BeginSandboxProvisionOutcome, SandboxProvision, SandboxProvisionState};

use super::super::entities::sandbox_provision;
use super::super::{entities, store_err, DbStore};

const STATE_INTENDED: &str = "intended";
const STATE_COMMITTED: &str = "committed";
const STATE_TEARDOWN: &str = "teardown";
const STATE_DONE: &str = "done";

fn state_from_str(state: &str) -> SandboxProvisionState {
    match state {
        STATE_COMMITTED => SandboxProvisionState::Committed,
        STATE_TEARDOWN => SandboxProvisionState::Teardown,
        STATE_DONE => SandboxProvisionState::Done,
        // Any other stored value is an `intended` record; the CHECK constraint
        // bounds the column to the four known states.
        _ => SandboxProvisionState::Intended,
    }
}

fn from_model(model: sandbox_provision::Model) -> SandboxProvision {
    SandboxProvision {
        run_id: model.run_id,
        tag: model.tag,
        state: state_from_str(&model.state),
        handle: model.handle,
        window_expires_at: model.window_expires_at,
    }
}

pub(in crate::db) async fn begin(
    store: &DbStore,
    run_id: uuid::Uuid,
    tag: &str,
    window_expires_at: chrono::DateTime<chrono::Utc>,
) -> Result<BeginSandboxProvisionOutcome> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let now = Utc::now();

    // The insert is the serialization point: a duplicate run id is refused by
    // the primary key, so exactly one racing driver commits the intent and the
    // rest observe the existing record.
    let inserted = entities::sandbox_provision::Entity::insert(sandbox_provision::ActiveModel {
        run_id: Set(run_id),
        tag: Set(tag.to_owned()),
        state: Set(STATE_INTENDED.to_owned()),
        handle: Set(None),
        window_expires_at: Set(window_expires_at),
        created_at: Set(now),
        updated_at: Set(now),
    })
    .on_conflict_do_nothing()
    .exec_without_returning(&transaction)
    .await
    .map_err(store_err)?;

    if matches!(inserted, TryInsertResult::Inserted(1)) {
        transaction.commit().await.map_err(store_err)?;
        return Ok(BeginSandboxProvisionOutcome::Started);
    }

    let existing = entities::sandbox_provision::Entity::find_by_id(run_id)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            crate::error::AgentError::Store(
                "sandbox provision record vanished between insert and read".into(),
            )
        })?;
    transaction.commit().await.map_err(store_err)?;
    Ok(BeginSandboxProvisionOutcome::Existing(from_model(existing)))
}

pub(in crate::db) async fn commit_handle(
    store: &DbStore,
    run_id: uuid::Uuid,
    handle: &str,
) -> Result<bool> {
    // Predicated on `intended`: if the sweep's lapse got here first the update
    // matches nothing, and the caller learns it owns a disowned sandbox.
    let updated = entities::sandbox_provision::Entity::update_many()
        .col_expr(
            sandbox_provision::Column::State,
            Expr::value(STATE_COMMITTED),
        )
        .col_expr(sandbox_provision::Column::Handle, Expr::value(handle))
        .col_expr(
            sandbox_provision::Column::UpdatedAt,
            Expr::value(Utc::now()),
        )
        .filter(sandbox_provision::Column::RunId.eq(run_id))
        .filter(sandbox_provision::Column::State.eq(STATE_INTENDED))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(updated.rows_affected == 1)
}

pub(in crate::db) async fn enqueue_teardown(
    store: &DbStore,
    run_id: uuid::Uuid,
) -> Result<Option<SandboxProvision>> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let updated = entities::sandbox_provision::Entity::update_many()
        .col_expr(
            sandbox_provision::Column::State,
            Expr::value(STATE_TEARDOWN),
        )
        .col_expr(
            sandbox_provision::Column::UpdatedAt,
            Expr::value(Utc::now()),
        )
        .filter(sandbox_provision::Column::RunId.eq(run_id))
        .filter(sandbox_provision::Column::State.is_in([
            STATE_INTENDED,
            STATE_COMMITTED,
            STATE_TEARDOWN,
        ]))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let row = entities::sandbox_provision::Entity::find_by_id(run_id)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .map(from_model);
    transaction.commit().await.map_err(store_err)?;
    Ok(row)
}

pub(in crate::db) async fn complete_teardown(store: &DbStore, run_id: uuid::Uuid) -> Result<()> {
    entities::sandbox_provision::Entity::update_many()
        .col_expr(sandbox_provision::Column::State, Expr::value(STATE_DONE))
        .col_expr(
            sandbox_provision::Column::UpdatedAt,
            Expr::value(Utc::now()),
        )
        .filter(sandbox_provision::Column::RunId.eq(run_id))
        .filter(sandbox_provision::Column::State.eq(STATE_TEARDOWN))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

pub(in crate::db) async fn lapse(
    store: &DbStore,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<SandboxProvision>> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let lapsed = entities::sandbox_provision::Entity::find()
        .filter(sandbox_provision::Column::State.eq(STATE_INTENDED))
        .filter(sandbox_provision::Column::WindowExpiresAt.lt(now))
        .all(&transaction)
        .await
        .map_err(store_err)?;
    if lapsed.is_empty() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(Vec::new());
    }
    let ids: Vec<uuid::Uuid> = lapsed.iter().map(|row| row.run_id).collect();
    // Predicated on `intended` again inside the transaction, so a handle commit
    // that landed between the read and this write keeps its win.
    let updated = entities::sandbox_provision::Entity::update_many()
        .col_expr(
            sandbox_provision::Column::State,
            Expr::value(STATE_TEARDOWN),
        )
        .col_expr(
            sandbox_provision::Column::UpdatedAt,
            Expr::value(Utc::now()),
        )
        .filter(sandbox_provision::Column::RunId.is_in(ids))
        .filter(sandbox_provision::Column::State.eq(STATE_INTENDED))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    // Report only what this call actually transitioned.
    if updated.rows_affected as usize != lapsed.len() {
        let refreshed = entities::sandbox_provision::Entity::find()
            .filter(sandbox_provision::Column::State.eq(STATE_TEARDOWN))
            .filter(
                sandbox_provision::Column::RunId
                    .is_in(lapsed.iter().map(|row| row.run_id).collect::<Vec<_>>()),
            )
            .all(&store.conn)
            .await
            .map_err(store_err)?;
        return Ok(refreshed
            .into_iter()
            .map(|row| SandboxProvision {
                state: SandboxProvisionState::Teardown,
                ..from_model(row)
            })
            .collect());
    }
    Ok(lapsed
        .into_iter()
        .map(|row| SandboxProvision {
            state: SandboxProvisionState::Teardown,
            ..from_model(row)
        })
        .collect())
}

pub(in crate::db) async fn list_teardowns(store: &DbStore) -> Result<Vec<SandboxProvision>> {
    Ok(entities::sandbox_provision::Entity::find()
        .filter(sandbox_provision::Column::State.eq(STATE_TEARDOWN))
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(from_model)
        .collect())
}

pub(in crate::db) async fn live_tags(store: &DbStore) -> Result<Vec<String>> {
    Ok(entities::sandbox_provision::Entity::find()
        .filter(sandbox_provision::Column::State.is_in([STATE_INTENDED, STATE_COMMITTED]))
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|row| row.tag)
        .collect())
}
