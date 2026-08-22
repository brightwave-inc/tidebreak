//! Persistence for standing trigger rules and their fire fingerprints.
//!
//! Triggers bind per repository (decision 60). The fire table is the
//! fingerprint that makes a trigger fire on an edge rather than on every sweep
//! that still finds its condition true.

use sea_orm::sea_query::{Expr, ExprTrait, OnConflict, Query};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
    TransactionTrait,
};

use crate::code::{
    CodeSessionId, CodeSessionLifecycle, CodeTrigger, CodeTriggerAction, CodeTriggerCondition,
    CodeTriggerDeliveryId, CodeTriggerDeliverySink, CodeTriggerFire, CodeTriggerFireIdentity,
    CodeTriggerFirePayload, CodeTriggerFireState, CodeTriggerId, CodeTurn, CodeTurnId, RepoId,
    WorkspaceId,
};
use crate::error::{AgentError, Result};
use crate::Attention;
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};

const EMPTY_TRIGGER_FIRE_ERROR: &str = "trigger delivery failed";

/// Whether any sink already crossed the durable acceptance boundary.
pub async fn trigger_delivery_accepted(
    store: &DbStore,
    owner: &OwnerId,
    delivery_id: CodeTriggerDeliveryId,
) -> Result<bool> {
    Ok(
        entities::code_trigger_delivery_receipt::Entity::find_by_id(delivery_id.0)
            .filter(entities::code_trigger_delivery_receipt::Column::Owner.eq(owner.as_str()))
            .one(&store.conn)
            .await
            .map_err(store_err)?
            .is_some(),
    )
}

/// Cross an at-most-once sink boundary before an external side effect.
pub async fn accept_trigger_delivery(
    store: &DbStore,
    owner: &OwnerId,
    delivery_id: CodeTriggerDeliveryId,
    lease_token: uuid::Uuid,
    sink: CodeTriggerDeliverySink,
    session_id: CodeSessionId,
    turn_id: Option<CodeTurnId>,
    accepted_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    validate_claimed_delivery_on(&transaction, owner, delivery_id, lease_token, accepted_at)
        .await?;
    let accepted = insert_delivery_receipt_on(
        &transaction,
        owner,
        delivery_id,
        sink,
        session_id,
        turn_id,
        accepted_at,
    )
    .await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(accepted)
}

/// Atomically accept a trigger turn and persist its running row.
pub async fn accept_trigger_turn_delivery(
    store: &DbStore,
    owner: &OwnerId,
    delivery_id: CodeTriggerDeliveryId,
    lease_token: uuid::Uuid,
    turn: &CodeTurn,
    accepted_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    validate_claimed_delivery_on(&transaction, owner, delivery_id, lease_token, accepted_at)
        .await?;
    if !super::acquire_code_session_write_lock(&transaction, turn.session_id).await? {
        return Err(AgentError::Store(format!(
            "code trigger turn session {} not found",
            turn.session_id
        )));
    }
    let session = entities::code_session::Entity::find_by_id(turn.session_id.0)
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "code trigger turn session {} not found",
                turn.session_id
            ))
        })?;
    let lifecycle = CodeSessionLifecycle::from_str(&session.lifecycle).ok_or_else(|| {
        AgentError::Store(format!(
            "code session {} has unknown lifecycle {}",
            session.id, session.lifecycle
        ))
    })?;
    if matches!(
        lifecycle,
        CodeSessionLifecycle::Running | CodeSessionLifecycle::Fenced | CodeSessionLifecycle::Ended
    ) {
        return Err(AgentError::Store(format!(
            "code trigger turn session {} cannot accept a turn while {}",
            turn.session_id,
            lifecycle.as_str()
        )));
    }
    let accepted = insert_delivery_receipt_on(
        &transaction,
        owner,
        delivery_id,
        CodeTriggerDeliverySink::Turn,
        turn.session_id,
        Some(turn.id),
        accepted_at,
    )
    .await?;
    if accepted {
        super::turn::insert_turn_on(&transaction, owner, turn).await?;
        let updated = entities::code_session::Entity::update_many()
            .col_expr(
                entities::code_session::Column::Lifecycle,
                Expr::value(CodeSessionLifecycle::Running.as_str()),
            )
            .filter(entities::code_session::Column::Id.eq(turn.session_id.0))
            .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
            .filter(entities::code_session::Column::Lifecycle.eq(session.lifecycle))
            .exec(&transaction)
            .await
            .map_err(store_err)?;
        if updated.rows_affected != 1 {
            return Err(AgentError::Store(format!(
                "code trigger turn session {} changed while accepting its turn",
                turn.session_id
            )));
        }
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(accepted)
}

/// Atomically accept a trigger attention delivery and write its session state.
///
/// The session row is locked before the replacement rule is evaluated. A
/// concurrent manual or structured attention write therefore wins or loses by
/// database order instead of being overwritten from a stale server copy.
pub async fn accept_trigger_attention_delivery(
    store: &DbStore,
    owner: &OwnerId,
    delivery_id: CodeTriggerDeliveryId,
    lease_token: uuid::Uuid,
    session_id: CodeSessionId,
    attention: &Attention,
    accepted_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    validate_claimed_delivery_on(&transaction, owner, delivery_id, lease_token, accepted_at)
        .await?;
    if !super::acquire_code_session_write_lock(&transaction, session_id).await? {
        return Err(AgentError::Store(format!(
            "code trigger attention session {session_id} not found"
        )));
    }
    let session_exists = entities::code_session::Entity::find_by_id(session_id.0)
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some();
    if !session_exists {
        return Err(AgentError::Store(format!(
            "code trigger attention session {session_id} not found"
        )));
    }
    let accepted = insert_delivery_receipt_on(
        &transaction,
        owner,
        delivery_id,
        CodeTriggerDeliverySink::Attention,
        session_id,
        None,
        accepted_at,
    )
    .await?;
    if !accepted {
        transaction.commit().await.map_err(store_err)?;
        return Ok(false);
    }

    let changed = super::session::replace_session_attention_on(
        &transaction,
        owner,
        session_id,
        attention,
        false,
    )
    .await?
    .is_some();
    transaction.commit().await.map_err(store_err)?;
    Ok(changed)
}

async fn insert_delivery_receipt_on<C>(
    conn: &C,
    owner: &OwnerId,
    delivery_id: CodeTriggerDeliveryId,
    sink: CodeTriggerDeliverySink,
    session_id: CodeSessionId,
    turn_id: Option<CodeTurnId>,
    accepted_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    if delivery_id.0.is_nil() {
        return Err(AgentError::Store(
            "code trigger delivery id must not be nil".into(),
        ));
    }
    let acceptance_token = uuid::Uuid::new_v4();
    entities::code_trigger_delivery_receipt::Entity::insert(
        entities::code_trigger_delivery_receipt::ActiveModel {
            delivery_id: Set(delivery_id.0),
            owner: Set(owner.as_str().to_owned()),
            sink: Set(sink.as_str().to_owned()),
            session_id: Set(session_id.0),
            turn_id: Set(turn_id.map(|id| id.0)),
            acceptance_token: Set(acceptance_token),
            accepted_at: Set(accepted_at),
        },
    )
    .on_conflict(
        OnConflict::column(entities::code_trigger_delivery_receipt::Column::DeliveryId)
            .do_nothing()
            .to_owned(),
    )
    .exec_without_returning(conn)
    .await
    .map_err(store_err)?;
    let receipt = entities::code_trigger_delivery_receipt::Entity::find_by_id(delivery_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("trigger delivery {delivery_id} disappeared")))?;
    if receipt.owner != owner.as_str() {
        return Err(AgentError::Store(format!(
            "trigger delivery {delivery_id} belongs to another owner"
        )));
    }
    Ok(receipt.acceptance_token == acceptance_token)
}

/// Serialize sink acceptance with trigger disable or deletion, then verify that
/// this worker still owns a live pending lease.
async fn validate_claimed_delivery_on<C>(
    conn: &C,
    owner: &OwnerId,
    delivery_id: CodeTriggerDeliveryId,
    lease_token: uuid::Uuid,
    accepted_at: chrono::DateTime<chrono::Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    validate_trigger_fire_ids(delivery_id, lease_token)?;
    let fire = entities::code_trigger_fire::Entity::find()
        .filter(entities::code_trigger_fire::Column::DeliveryId.eq(delivery_id.0))
        .filter(entities::code_trigger_fire::Column::Owner.eq(owner.as_str()))
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("trigger delivery {delivery_id} not found")))?;

    // A no-op write takes the same trigger-row lock as toggle and delete. The
    // re-read after it therefore sees cancellation or deletion that won first.
    let locked = entities::code_trigger::Entity::update_many()
        .col_expr(
            entities::code_trigger::Column::UpdatedAt,
            Expr::col(entities::code_trigger::Column::UpdatedAt),
        )
        .filter(entities::code_trigger::Column::Id.eq(fire.trigger_id))
        .filter(entities::code_trigger::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_trigger::Column::Enabled.eq(true))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if locked.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "trigger delivery {delivery_id} was cancelled before acceptance"
        )));
    }

    let claimed = entities::code_trigger_fire::Entity::find()
        .filter(entities::code_trigger_fire::Column::DeliveryId.eq(delivery_id.0))
        .filter(entities::code_trigger_fire::Column::Owner.eq(owner.as_str()))
        .filter(
            entities::code_trigger_fire::Column::State.eq(CodeTriggerFireState::Pending.as_str()),
        )
        .filter(entities::code_trigger_fire::Column::LeaseToken.eq(lease_token))
        .filter(entities::code_trigger_fire::Column::LeaseExpiresAt.gt(accepted_at))
        .one(conn)
        .await
        .map_err(store_err)?;
    if claimed.is_none() {
        return Err(AgentError::Store(format!(
            "trigger delivery {delivery_id} no longer has an active lease"
        )));
    }
    Ok(())
}

/// Atomically arm a trigger for one `(owner, repository, condition)`.
///
/// A later arm sets the requested action and enables the rule. Creation fields
/// and the stored id stay unchanged. A concurrent enabled toggle writes only
/// that bit, so database serialization decides the shared enabled value while
/// neither operation can overwrite the other's unrelated columns.
pub async fn arm_trigger(store: &DbStore, owner: &OwnerId, trigger: &CodeTrigger) -> Result<()> {
    if trigger.owner != *owner {
        return Err(AgentError::Store(
            "code trigger owner does not match the requested owner".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let repo_exists = entities::code_repo::Entity::find_by_id(trigger.repo_id.0)
        .filter(entities::code_repo::Column::Owner.eq(owner.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some();
    if !repo_exists {
        return Err(AgentError::Store(format!(
            "code trigger repository {} not found",
            trigger.repo_id
        )));
    }
    entities::code_trigger::Entity::insert(entities::code_trigger::ActiveModel {
        id: Set(trigger.id.0),
        owner: Set(owner.as_str().to_owned()),
        repo_id: Set(trigger.repo_id.0),
        condition: Set(trigger.condition.as_str().to_owned()),
        action: Set(trigger.action.as_str().to_owned()),
        enabled: Set(trigger.enabled),
        created_at: Set(trigger.created_at),
        updated_at: Set(trigger.updated_at),
    })
    .on_conflict(
        OnConflict::columns([
            entities::code_trigger::Column::Owner,
            entities::code_trigger::Column::RepoId,
            entities::code_trigger::Column::Condition,
        ])
        .update_columns([
            entities::code_trigger::Column::Action,
            entities::code_trigger::Column::Enabled,
            entities::code_trigger::Column::UpdatedAt,
        ])
        .to_owned(),
    )
    .exec(&transaction)
    .await
    .map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(())
}

/// Every trigger the owner armed on one repository, enabled or not.
pub async fn list_triggers_for_repo(
    store: &DbStore,
    owner: &OwnerId,
    repo_id: RepoId,
) -> Result<Vec<CodeTrigger>> {
    entities::code_trigger::Entity::find()
        .filter(entities::code_trigger::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_trigger::Column::RepoId.eq(repo_id.0))
        .order_by_asc(entities::code_trigger::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(trigger_from_row)
        .collect()
}

/// Every enabled trigger on the machine.
///
/// A system path, not a request path: the trigger sweep drives every owner's
/// triggers, the way the watch sweep drives every owner's watches. Nothing
/// reachable from a route may call it.
pub async fn list_enabled_triggers_all_owners(store: &DbStore) -> Result<Vec<CodeTrigger>> {
    entities::code_trigger::Entity::find()
        .filter(entities::code_trigger::Column::Enabled.eq(true))
        .order_by_asc(entities::code_trigger::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(trigger_from_row)
        .collect()
}

/// Change only one trigger's enabled bit.
///
/// The repository predicate is part of the write rather than a preceding read,
/// so a stale request cannot update another repository or reinsert a trigger
/// that a concurrent delete removed.
pub async fn update_trigger_enabled(
    store: &DbStore,
    owner: &OwnerId,
    repo_id: RepoId,
    id: CodeTriggerId,
    enabled: bool,
    updated_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let result = entities::code_trigger::Entity::update_many()
        .col_expr(
            entities::code_trigger::Column::Enabled,
            Expr::value(enabled),
        )
        .col_expr(
            entities::code_trigger::Column::UpdatedAt,
            Expr::value(updated_at),
        )
        .filter(entities::code_trigger::Column::Id.eq(id.0))
        .filter(entities::code_trigger::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_trigger::Column::RepoId.eq(repo_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if result.rows_affected == 1 && !enabled {
        cancel_unaccepted_trigger_fires_on(&transaction, owner, id, updated_at).await?;
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Delete one trigger. Its fire rows go with it through the cascade.
///
/// Returns `false` when the owner and repository had no such trigger.
pub async fn delete_trigger(
    store: &DbStore,
    owner: &OwnerId,
    repo_id: RepoId,
    id: CodeTriggerId,
) -> Result<bool> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let result = entities::code_trigger::Entity::delete_many()
        .filter(entities::code_trigger::Column::Id.eq(id.0))
        .filter(entities::code_trigger::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_trigger::Column::RepoId.eq(repo_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Cancel every unaccepted pending fire while the trigger-row lock is held.
async fn cancel_unaccepted_trigger_fires_on<C>(
    conn: &C,
    owner: &OwnerId,
    trigger_id: CodeTriggerId,
    cancelled_at: chrono::DateTime<chrono::Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let accepted = Query::select()
        .column(entities::code_trigger_delivery_receipt::Column::DeliveryId)
        .from(entities::code_trigger_delivery_receipt::Entity)
        .to_owned();
    entities::code_trigger_fire::Entity::update_many()
        .col_expr(
            entities::code_trigger_fire::Column::State,
            Expr::value(CodeTriggerFireState::Cancelled.as_str()),
        )
        .col_expr(
            entities::code_trigger_fire::Column::LeaseToken,
            Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::code_trigger_fire::Column::LeaseExpiresAt,
            Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
        )
        .col_expr(
            entities::code_trigger_fire::Column::NextAttemptAt,
            Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
        )
        .col_expr(
            entities::code_trigger_fire::Column::LastError,
            Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::code_trigger_fire::Column::CancelledAt,
            Expr::value(Some(cancelled_at)),
        )
        .filter(entities::code_trigger_fire::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_trigger_fire::Column::TriggerId.eq(trigger_id.0))
        .filter(
            entities::code_trigger_fire::Column::State.eq(CodeTriggerFireState::Pending.as_str()),
        )
        .filter(entities::code_trigger_fire::Column::DeliveryId.not_in_subquery(accepted))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

/// Insert one pending delivery, or load the existing row for the exact edge.
///
/// The four-column conflict target is the suppression identity. The generated
/// delivery id belongs to the first insert and remains stable through retries.
/// The trigger row is locked before insertion so a completed disable cannot be
/// followed by a late pending fire from an older sweep snapshot.
pub async fn insert_or_load_trigger_fire(
    store: &DbStore,
    identity: &CodeTriggerFireIdentity,
    payload: &CodeTriggerFirePayload,
    fired_at: chrono::DateTime<chrono::Utc>,
) -> Result<Option<CodeTriggerFire>> {
    let pr_number = trigger_fire_pr_number(identity)?;
    let delivery_id = CodeTriggerDeliveryId::new();
    let transaction = store.conn.begin().await.map_err(store_err)?;

    let locked = entities::code_trigger::Entity::update_many()
        .col_expr(
            entities::code_trigger::Column::UpdatedAt,
            Expr::col(entities::code_trigger::Column::UpdatedAt),
        )
        .filter(entities::code_trigger::Column::Id.eq(identity.trigger_id.0))
        .filter(entities::code_trigger::Column::Owner.eq(identity.owner.as_str()))
        .filter(entities::code_trigger::Column::Enabled.eq(true))
        .filter(entities::code_trigger::Column::Condition.eq(payload.condition.as_str()))
        .filter(entities::code_trigger::Column::Action.eq(payload.action.as_str()))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if locked.rows_affected == 0 {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }

    entities::code_trigger_fire::Entity::insert(entities::code_trigger_fire::ActiveModel {
        trigger_id: Set(identity.trigger_id.0),
        owner: Set(identity.owner.as_str().to_owned()),
        workspace_id: Set(identity.workspace_id.0),
        pr_number: Set(pr_number),
        head_sha: Set(identity.head_sha.clone()),
        fired_at: Set(fired_at),
        delivery_id: Set(delivery_id.0),
        delivery_condition: Set(Some(payload.condition.as_str().to_owned())),
        delivery_action: Set(Some(payload.action.as_str().to_owned())),
        delivery_message: Set(Some(payload.message.clone())),
        state: Set(CodeTriggerFireState::Pending.as_str().to_owned()),
        attempt_count: Set(0),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        next_attempt_at: Set(Some(fired_at)),
        last_error: Set(None),
        delivered_at: Set(None),
        cancelled_at: Set(None),
    })
    .on_conflict(
        OnConflict::columns([
            entities::code_trigger_fire::Column::TriggerId,
            entities::code_trigger_fire::Column::WorkspaceId,
            entities::code_trigger_fire::Column::PrNumber,
            entities::code_trigger_fire::Column::HeadSha,
        ])
        .do_nothing()
        .to_owned(),
    )
    .exec_without_returning(&transaction)
    .await
    .map_err(store_err)?;

    let fire = find_trigger_fire_by_identity_on(&transaction, identity, pr_number)
        .await?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "code trigger fire disappeared for trigger {}, workspace {}, pull request {}, head {}",
                identity.trigger_id, identity.workspace_id, identity.pr_number, identity.head_sha
            ))
        })?;
    transaction.commit().await.map_err(store_err)?;
    fire_from_row(fire).map(Some)
}

/// Claim one due pending delivery with a fenced lease.
///
/// The attempt count increments in the same conditional update that installs
/// the lease. A live lease or a row that is not due returns `None`.
pub async fn lease_trigger_fire_delivery(
    store: &DbStore,
    owner: &OwnerId,
    delivery_id: CodeTriggerDeliveryId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<chrono::Utc>,
    lease_expires_at: chrono::DateTime<chrono::Utc>,
) -> Result<Option<CodeTriggerFire>> {
    validate_trigger_fire_lease(delivery_id, lease_token, now, lease_expires_at)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let leased = entities::code_trigger_fire::Entity::update_many()
        .col_expr(
            entities::code_trigger_fire::Column::LeaseToken,
            Expr::value(Some(lease_token)),
        )
        .col_expr(
            entities::code_trigger_fire::Column::LeaseExpiresAt,
            Expr::value(Some(lease_expires_at)),
        )
        .col_expr(
            entities::code_trigger_fire::Column::AttemptCount,
            Expr::col(entities::code_trigger_fire::Column::AttemptCount).add(1),
        )
        .filter(entities::code_trigger_fire::Column::DeliveryId.eq(delivery_id.0))
        .filter(entities::code_trigger_fire::Column::Owner.eq(owner.as_str()))
        .filter(
            entities::code_trigger_fire::Column::State.eq(CodeTriggerFireState::Pending.as_str()),
        )
        .filter(entities::code_trigger_fire::Column::NextAttemptAt.lte(now))
        .filter(
            sea_orm::Condition::any()
                .add(
                    entities::code_trigger_fire::Column::LeaseToken
                        .is_null()
                        .and(entities::code_trigger_fire::Column::LeaseExpiresAt.is_null()),
                )
                .add(entities::code_trigger_fire::Column::LeaseExpiresAt.lte(now)),
        )
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if leased.rows_affected == 0 {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }

    let fire = entities::code_trigger_fire::Entity::find()
        .filter(entities::code_trigger_fire::Column::DeliveryId.eq(delivery_id.0))
        .filter(entities::code_trigger_fire::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_trigger_fire::Column::LeaseToken.eq(lease_token))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "leased code trigger delivery {delivery_id} disappeared"
            ))
        })?;
    transaction.commit().await.map_err(store_err)?;
    fire_from_row(fire).map(Some)
}

/// A bounded page of due pending deliveries across every owner.
///
/// A system path, not a request path: the trigger sweep uses this queue even
/// when GitHub no longer reports the condition that created the row.
pub async fn list_due_trigger_fire_deliveries_all_owners(
    store: &DbStore,
    now: chrono::DateTime<chrono::Utc>,
    limit: u64,
) -> Result<Vec<CodeTriggerFire>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    entities::code_trigger_fire::Entity::find()
        .filter(
            entities::code_trigger_fire::Column::State.eq(CodeTriggerFireState::Pending.as_str()),
        )
        .filter(entities::code_trigger_fire::Column::NextAttemptAt.lte(now))
        .filter(
            sea_orm::Condition::any()
                .add(
                    entities::code_trigger_fire::Column::LeaseToken
                        .is_null()
                        .and(entities::code_trigger_fire::Column::LeaseExpiresAt.is_null()),
                )
                .add(entities::code_trigger_fire::Column::LeaseExpiresAt.lte(now)),
        )
        .order_by_asc(entities::code_trigger_fire::Column::NextAttemptAt)
        .order_by_asc(entities::code_trigger_fire::Column::FiredAt)
        .limit(limit)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(fire_from_row)
        .collect()
}

/// Mark a pending delivery as delivered when the caller still owns its lease.
pub async fn acknowledge_trigger_fire_delivery(
    store: &DbStore,
    owner: &OwnerId,
    delivery_id: CodeTriggerDeliveryId,
    lease_token: uuid::Uuid,
    delivered_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    validate_trigger_fire_ids(delivery_id, lease_token)?;
    let acknowledged = entities::code_trigger_fire::Entity::update_many()
        .col_expr(
            entities::code_trigger_fire::Column::State,
            Expr::value(CodeTriggerFireState::Delivered.as_str()),
        )
        .col_expr(
            entities::code_trigger_fire::Column::DeliveredAt,
            Expr::value(Some(delivered_at)),
        )
        .col_expr(
            entities::code_trigger_fire::Column::LeaseToken,
            Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::code_trigger_fire::Column::LeaseExpiresAt,
            Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
        )
        .col_expr(
            entities::code_trigger_fire::Column::NextAttemptAt,
            Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
        )
        .col_expr(
            entities::code_trigger_fire::Column::LastError,
            Expr::value(Option::<String>::None),
        )
        .filter(entities::code_trigger_fire::Column::DeliveryId.eq(delivery_id.0))
        .filter(entities::code_trigger_fire::Column::Owner.eq(owner.as_str()))
        .filter(
            entities::code_trigger_fire::Column::State.eq(CodeTriggerFireState::Pending.as_str()),
        )
        .filter(entities::code_trigger_fire::Column::LeaseToken.eq(lease_token))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(acknowledged.rows_affected == 1)
}

/// Keep a failed delivery pending and schedule its next bounded retry.
pub async fn reschedule_trigger_fire_delivery_failure(
    store: &DbStore,
    owner: &OwnerId,
    delivery_id: CodeTriggerDeliveryId,
    lease_token: uuid::Uuid,
    failed_at: chrono::DateTime<chrono::Utc>,
    error: &str,
) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
    validate_trigger_fire_ids(delivery_id, lease_token)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let Some(current) = entities::code_trigger_fire::Entity::find()
        .filter(entities::code_trigger_fire::Column::DeliveryId.eq(delivery_id.0))
        .filter(entities::code_trigger_fire::Column::Owner.eq(owner.as_str()))
        .filter(
            entities::code_trigger_fire::Column::State.eq(CodeTriggerFireState::Pending.as_str()),
        )
        .filter(entities::code_trigger_fire::Column::LeaseToken.eq(lease_token))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let next_attempt_at = failed_at
        .checked_add_signed(CodeTriggerFire::retry_delay(current.attempt_count))
        .ok_or_else(|| AgentError::Store("code trigger retry timestamp overflow".into()))?;
    let bounded_error = if error.is_empty() {
        EMPTY_TRIGGER_FIRE_ERROR.to_owned()
    } else {
        error
            .chars()
            .take(CodeTriggerFire::MAX_LAST_ERROR_CHARS)
            .collect::<String>()
    };
    let rescheduled = entities::code_trigger_fire::Entity::update_many()
        .col_expr(
            entities::code_trigger_fire::Column::LeaseToken,
            Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::code_trigger_fire::Column::LeaseExpiresAt,
            Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
        )
        .col_expr(
            entities::code_trigger_fire::Column::NextAttemptAt,
            Expr::value(Some(next_attempt_at)),
        )
        .col_expr(
            entities::code_trigger_fire::Column::LastError,
            Expr::value(Some(bounded_error)),
        )
        .filter(entities::code_trigger_fire::Column::DeliveryId.eq(delivery_id.0))
        .filter(entities::code_trigger_fire::Column::Owner.eq(owner.as_str()))
        .filter(
            entities::code_trigger_fire::Column::State.eq(CodeTriggerFireState::Pending.as_str()),
        )
        .filter(entities::code_trigger_fire::Column::AttemptCount.eq(current.attempt_count))
        .filter(entities::code_trigger_fire::Column::LeaseToken.eq(lease_token))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok((rescheduled.rows_affected == 1).then_some(next_attempt_at))
}

/// Fires already recorded for one workspace, newest first.
pub async fn list_fires_for_workspace(
    store: &DbStore,
    owner: &OwnerId,
    workspace_id: WorkspaceId,
) -> Result<Vec<CodeTriggerFire>> {
    entities::code_trigger_fire::Entity::find()
        .filter(entities::code_trigger_fire::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_trigger_fire::Column::WorkspaceId.eq(workspace_id.0))
        .order_by_desc(entities::code_trigger_fire::Column::FiredAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(fire_from_row)
        .collect()
}

fn trigger_from_row(row: entities::code_trigger::Model) -> Result<CodeTrigger> {
    let condition = CodeTriggerCondition::from_str(&row.condition).ok_or_else(|| {
        AgentError::Store(format!(
            "code_trigger {} has unknown condition {}",
            row.id, row.condition
        ))
    })?;
    let action = CodeTriggerAction::from_str(&row.action).ok_or_else(|| {
        AgentError::Store(format!(
            "code_trigger {} has unknown action {}",
            row.id, row.action
        ))
    })?;
    Ok(CodeTrigger {
        id: CodeTriggerId(row.id),
        owner: OwnerId::new(&row.owner)?,
        repo_id: RepoId(row.repo_id),
        condition,
        action,
        enabled: row.enabled,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn fire_from_row(row: entities::code_trigger_fire::Model) -> Result<CodeTriggerFire> {
    let pr_number = u64::try_from(row.pr_number).map_err(|_| {
        AgentError::Store(format!("code_trigger_fire {} pr number", row.trigger_id))
    })?;
    let state = CodeTriggerFireState::from_str(&row.state).ok_or_else(|| {
        AgentError::Store(format!(
            "code_trigger_fire {} has unknown state {}",
            row.delivery_id, row.state
        ))
    })?;
    let payload = match (
        row.delivery_condition,
        row.delivery_action,
        row.delivery_message,
    ) {
        (None, None, None) => None,
        (Some(condition), Some(action), Some(message)) => {
            let condition = CodeTriggerCondition::from_str(&condition).ok_or_else(|| {
                AgentError::Store(format!(
                    "code_trigger_fire {} has unknown delivery condition {condition}",
                    row.delivery_id
                ))
            })?;
            let action = CodeTriggerAction::from_str(&action).ok_or_else(|| {
                AgentError::Store(format!(
                    "code_trigger_fire {} has unknown delivery action {action}",
                    row.delivery_id
                ))
            })?;
            Some(CodeTriggerFirePayload {
                action,
                condition,
                message,
            })
        }
        _ => {
            return Err(AgentError::Store(format!(
                "code_trigger_fire {} has a partial delivery payload",
                row.delivery_id
            )))
        }
    };
    if state == CodeTriggerFireState::Pending && payload.is_none() {
        return Err(AgentError::Store(format!(
            "pending code_trigger_fire {} has no delivery payload",
            row.delivery_id
        )));
    }
    Ok(CodeTriggerFire {
        identity: CodeTriggerFireIdentity {
            trigger_id: CodeTriggerId(row.trigger_id),
            owner: OwnerId::new(&row.owner)?,
            workspace_id: WorkspaceId(row.workspace_id),
            pr_number,
            head_sha: row.head_sha,
        },
        delivery_id: CodeTriggerDeliveryId(row.delivery_id),
        payload,
        state,
        attempt_count: row.attempt_count,
        lease_token: row.lease_token,
        lease_expires_at: row.lease_expires_at,
        next_attempt_at: row.next_attempt_at,
        last_error: row.last_error,
        fired_at: row.fired_at,
        delivered_at: row.delivered_at,
        cancelled_at: row.cancelled_at,
    })
}

fn trigger_fire_pr_number(identity: &CodeTriggerFireIdentity) -> Result<i64> {
    i64::try_from(identity.pr_number).map_err(|_| {
        AgentError::Store(format!(
            "code_trigger_fire {} pr number",
            identity.trigger_id
        ))
    })
}

async fn find_trigger_fire_by_identity_on<C>(
    conn: &C,
    identity: &CodeTriggerFireIdentity,
    pr_number: i64,
) -> Result<Option<entities::code_trigger_fire::Model>>
where
    C: ConnectionTrait,
{
    entities::code_trigger_fire::Entity::find_by_id((
        identity.trigger_id.0,
        identity.workspace_id.0,
        pr_number,
        identity.head_sha.clone(),
    ))
    .one(conn)
    .await
    .map_err(store_err)
}

fn validate_trigger_fire_ids(
    delivery_id: CodeTriggerDeliveryId,
    lease_token: uuid::Uuid,
) -> Result<()> {
    if delivery_id.0.is_nil() {
        return Err(AgentError::Store(
            "code trigger delivery id must not be nil".into(),
        ));
    }
    if lease_token.is_nil() {
        return Err(AgentError::Store(
            "code trigger delivery lease token must not be nil".into(),
        ));
    }
    Ok(())
}

fn validate_trigger_fire_lease(
    delivery_id: CodeTriggerDeliveryId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<chrono::Utc>,
    lease_expires_at: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    validate_trigger_fire_ids(delivery_id, lease_token)?;
    if lease_expires_at <= now {
        return Err(AgentError::Store(
            "code trigger delivery lease expiry must be after claim time".into(),
        ));
    }
    Ok(())
}
