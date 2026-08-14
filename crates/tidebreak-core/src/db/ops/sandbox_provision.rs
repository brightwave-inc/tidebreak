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
use sea_orm::{
    ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter, Set, TransactionTrait,
    TryInsertResult,
};

use crate::error::{AgentError, Result};
use crate::id::AgentRunId;
use crate::model::{AgentRunExecutionLocation, AgentRunStatus, AgentRunTier};
use crate::storage::{
    BeginSandboxProvisionOutcome, SandboxAdmissionMode, SandboxProvision, SandboxProvisionState,
};

use super::super::entities::sandbox_provision;
use super::super::{entities, store_err, DbStore};
use super::agent_run::{acquire_agent_run_claim_lock, database_now};

const STATE_INTENDED: &str = "intended";
const STATE_COMMITTED: &str = "committed";
const STATE_TEARDOWN: &str = "teardown";
const STATE_DONE: &str = "done";

const ADMISSION_ATTACHED_ONLY: &str = "attached_only";
const ADMISSION_DETACHED: &str = "detached";

fn admission_to_str(admission: SandboxAdmissionMode) -> &'static str {
    match admission {
        SandboxAdmissionMode::AttachedOnly => ADMISSION_ATTACHED_ONLY,
        SandboxAdmissionMode::Detached => ADMISSION_DETACHED,
    }
}

fn admission_from_str(admission: &str) -> SandboxAdmissionMode {
    match admission {
        ADMISSION_DETACHED => SandboxAdmissionMode::Detached,
        // Fail closed: a record written before the column existed, or any
        // unrecognized value, is an attached-only admission.
        _ => SandboxAdmissionMode::AttachedOnly,
    }
}

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
        admission: admission_from_str(&model.admission),
        handle: model.handle,
        late_result_evidence: model.late_result_evidence,
        window_expires_at: model.window_expires_at,
    }
}

pub(in crate::db) async fn begin(
    store: &DbStore,
    run_id: uuid::Uuid,
    tag: &str,
    window_expires_at: chrono::DateTime<chrono::Utc>,
    admission: SandboxAdmissionMode,
) -> Result<BeginSandboxProvisionOutcome> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let now = Utc::now();

    let outcome = begin_on(&transaction, run_id, tag, window_expires_at, admission, now).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(outcome)
}

/// Atomically validate the exact live container execution claim and commit its
/// provisioning intent under the same claim lock used by cancellation.
///
/// If cancellation or another terminal transition got the lock first, no
/// intent is written and the caller is forbidden from invoking the external
/// backend. If this transaction gets the lock first, the intent is durable
/// before the backend call; a later cancellation is detected by the driver's
/// durable watcher and leaves the tagged side effect teardown-only.
pub(in crate::db) async fn begin_for_agent_run(
    store: &DbStore,
    run_id: AgentRunId,
    lease_token: uuid::Uuid,
    tag: &str,
    window_expires_at: chrono::DateTime<chrono::Utc>,
    admission: SandboxAdmissionMode,
) -> Result<Option<BeginSandboxProvisionOutcome>> {
    if lease_token.is_nil() {
        return Err(AgentError::Store(
            "sandbox provisioning requires a non-nil agent-run lease token".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    if !exact_live_agent_run_claim_on(
        &transaction,
        run_id,
        lease_token,
        AgentRunExecutionLocation::Container,
        now,
    )
    .await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }

    let outcome = begin_on(
        &transaction,
        run_id.0,
        tag,
        window_expires_at,
        admission,
        now,
    )
    .await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(outcome))
}

pub(in crate::db) async fn validate_agent_run_execution(
    store: &DbStore,
    run_id: AgentRunId,
    lease_token: uuid::Uuid,
    execution_location: AgentRunExecutionLocation,
) -> Result<bool> {
    if lease_token.is_nil() {
        return Err(AgentError::Store(
            "sandbox execution validation requires a non-nil lease token".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    let valid =
        exact_live_agent_run_claim_on(&transaction, run_id, lease_token, execution_location, now)
            .await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(valid)
}

async fn exact_live_agent_run_claim_on(
    transaction: &DatabaseTransaction,
    run_id: AgentRunId,
    lease_token: uuid::Uuid,
    execution_location: AgentRunExecutionLocation,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    let run = entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::Id.eq(run_id.0))
        .one(transaction)
        .await
        .map_err(store_err)?;
    let claim = entities::agent_run_claim::Entity::find_by_id(lease_token)
        .one(transaction)
        .await
        .map_err(store_err)?;
    Ok(run
        .as_ref()
        .zip(claim.as_ref())
        .is_some_and(|(run, claim)| {
            run.tier == AgentRunTier::Background.as_str()
                && run.execution_location == execution_location.as_str()
                && run.status == AgentRunStatus::Running.as_str()
                && run.lease_token == Some(lease_token)
                && run.lease_expires_at.is_some_and(|expiry| expiry > now)
                && run.deadline_at.is_some_and(|deadline| deadline > now)
                && run.updated_at <= now
                && claim.agent_run_id == Some(run.id)
                && claim.attempt_count == Some(run.attempt_count)
                && claim.claim_count == Some(run.claim_count)
        }))
}

async fn begin_on(
    transaction: &DatabaseTransaction,
    run_id: uuid::Uuid,
    tag: &str,
    window_expires_at: chrono::DateTime<chrono::Utc>,
    admission: SandboxAdmissionMode,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<BeginSandboxProvisionOutcome> {
    // The insert is the serialization point: a duplicate run id is refused by
    // the primary key, so exactly one racing driver commits the intent and the
    // rest observe the existing record.
    let inserted = entities::sandbox_provision::Entity::insert(sandbox_provision::ActiveModel {
        run_id: Set(run_id),
        tag: Set(tag.to_owned()),
        state: Set(STATE_INTENDED.to_owned()),
        admission: Set(admission_to_str(admission).to_owned()),
        handle: Set(None),
        late_result_evidence: Set(None),
        window_expires_at: Set(window_expires_at),
        created_at: Set(now),
        updated_at: Set(now),
    })
    .on_conflict_do_nothing()
    .exec_without_returning(transaction)
    .await
    .map_err(store_err)?;

    if matches!(inserted, TryInsertResult::Inserted(1)) {
        return Ok(BeginSandboxProvisionOutcome::Started);
    }

    let existing = entities::sandbox_provision::Entity::find_by_id(run_id)
        .one(transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            crate::error::AgentError::Store(
                "sandbox provision record vanished between insert and read".into(),
            )
        })?;
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

/// Transaction-scoped teardown enqueue for a container run that just went
/// terminal: whatever provisioning record exists — an open intent or a
/// committed handle — now owes a teardown, in the same transaction as the
/// run's terminal transition, so the sweep can never observe a failed run
/// whose container the records still call live.
pub(in crate::db) async fn enqueue_teardown_on<C>(conn: &C, run_id: uuid::Uuid) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    entities::sandbox_provision::Entity::update_many()
        .col_expr(
            sandbox_provision::Column::State,
            Expr::value(STATE_TEARDOWN),
        )
        .col_expr(
            sandbox_provision::Column::UpdatedAt,
            Expr::value(Utc::now()),
        )
        .filter(sandbox_provision::Column::RunId.eq(run_id))
        .filter(sandbox_provision::Column::State.is_in([STATE_INTENDED, STATE_COMMITTED]))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(())
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

pub(in crate::db) async fn get(
    store: &DbStore,
    run_id: uuid::Uuid,
) -> Result<Option<SandboxProvision>> {
    Ok(entities::sandbox_provision::Entity::find_by_id(run_id)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(from_model))
}

pub(in crate::db) async fn record_late_result_evidence(
    store: &DbStore,
    run_id: uuid::Uuid,
    text: &str,
) -> Result<bool> {
    // First writer wins: the NULL predicate makes a redelivered late result a
    // no-op rather than an overwrite.
    let updated = entities::sandbox_provision::Entity::update_many()
        .col_expr(
            sandbox_provision::Column::LateResultEvidence,
            Expr::value(text),
        )
        .col_expr(
            sandbox_provision::Column::UpdatedAt,
            Expr::value(Utc::now()),
        )
        .filter(sandbox_provision::Column::RunId.eq(run_id))
        .filter(sandbox_provision::Column::LateResultEvidence.is_null())
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(updated.rows_affected == 1)
}
