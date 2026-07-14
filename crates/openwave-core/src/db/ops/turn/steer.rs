use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::id::{ChatId, MessageId, TurnId, TurnSteerId};
use crate::model::{TurnRunStatus, TurnSteer, TurnSteerStatus};
use crate::storage::{AcceptTurnSteerOutcome, ApplyTurnSteerOutcome};

use super::super::super::{entities, store_err, DbStore};
use super::super::{
    acquire_chat_write_lock, acquire_turn_write_lock,
    conversation::{
        next_message_seq_on, reserve_message_identity_on, transfer_steer_message_identity_on,
        MESSAGE_IDENTITY_OWNER_MESSAGE, MESSAGE_IDENTITY_OWNER_STEER,
    },
};
use super::{canonical_db_timestamp, turn_run_status_from_db};

pub(in crate::db) async fn accept_turn_steer(
    store: &DbStore,
    id: TurnSteerId,
    turn_id: TurnId,
    chat_id: ChatId,
    content: &str,
    interrupt: bool,
) -> Result<AcceptTurnSteerOutcome> {
    validate_steer_input(id, turn_id, chat_id, content)?;
    if let Some(existing) = entities::turn_steer::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    {
        return exact_accepted_steer(existing, turn_id, chat_id, content, interrupt);
    }
    if entities::message::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .is_some()
    {
        return Ok(AcceptTurnSteerOutcome::IdentityConflict);
    }
    let now = canonical_db_timestamp(Utc::now())?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        return Err(AgentError::Store(format!("chat {chat_id} does not exist")));
    }
    if !acquire_turn_write_lock(&transaction, turn_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(AcceptTurnSteerOutcome::TurnUnavailable);
    }

    if let Some(existing) = entities::turn_steer::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let outcome = exact_accepted_steer(existing, turn_id, chat_id, content, interrupt)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(outcome);
    }
    if entities::message::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some()
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(AcceptTurnSteerOutcome::IdentityConflict);
    }

    let turn = entities::turn_run::Entity::find_by_id(turn_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("turn {turn_id} disappeared while locked")))?;
    let status = turn_run_status_from_db(&turn.status)?;
    let accepts_steer = turn.chat_id == chat_id.0
        && turn.updated_at <= now
        && matches!(
            status,
            TurnRunStatus::Queued | TurnRunStatus::Running | TurnRunStatus::RetryWait
        )
        && (status != TurnRunStatus::Running
            || turn
                .lease_expires_at
                .is_some_and(|lease_expires_at| lease_expires_at > now));
    if !accepts_steer {
        transaction.commit().await.map_err(store_err)?;
        return Ok(AcceptTurnSteerOutcome::TurnUnavailable);
    }

    if !reserve_message_identity_on(
        &transaction,
        MessageId(id.0),
        chat_id,
        turn_id,
        MESSAGE_IDENTITY_OWNER_STEER,
    )
    .await?
    {
        transaction.rollback().await.map_err(store_err)?;
        if let Some(existing) = entities::turn_steer::Entity::find_by_id(id.0)
            .one(&store.conn)
            .await
            .map_err(store_err)?
        {
            return exact_accepted_steer(existing, turn_id, chat_id, content, interrupt);
        }
        return Ok(AcceptTurnSteerOutcome::IdentityConflict);
    }

    let steer = entities::turn_steer::ActiveModel {
        id: Set(id.0),
        turn_id: Set(turn_id.0),
        chat_id: Set(chat_id.0),
        content: Set(content.to_owned()),
        interrupt: Set(interrupt),
        status: Set(TurnSteerStatus::Pending.as_str().into()),
        applied_lease_token: Set(None),
        message_id: Set(None),
        created_at: Set(now),
        resolved_at: Set(None),
    }
    .insert(&transaction)
    .await;
    let steer = match steer {
        Ok(steer) => steer,
        Err(error) => {
            transaction.rollback().await.map_err(store_err)?;
            if let Some(existing) = entities::turn_steer::Entity::find_by_id(id.0)
                .one(&store.conn)
                .await
                .map_err(store_err)?
            {
                return exact_accepted_steer(existing, turn_id, chat_id, content, interrupt);
            }
            if entities::message::Entity::find_by_id(id.0)
                .one(&store.conn)
                .await
                .map_err(store_err)?
                .is_some()
            {
                return Ok(AcceptTurnSteerOutcome::IdentityConflict);
            }
            return Err(store_err(error));
        }
    };

    let touched = entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::turn_run::Column::Id.eq(turn_id.0))
        .filter(entities::turn_run::Column::Status.eq(&turn.status))
        .filter(entities::turn_run::Column::AttemptCount.eq(turn.attempt_count))
        .filter(entities::turn_run::Column::UpdatedAt.eq(turn.updated_at))
        .filter(entities::turn_run::Column::UpdatedAt.lte(now));
    let touched = if status == TurnRunStatus::Running {
        touched
            .filter(entities::turn_run::Column::LeaseToken.eq(turn.lease_token))
            .filter(entities::turn_run::Column::LeaseExpiresAt.eq(turn.lease_expires_at))
    } else {
        touched
    }
    .exec(&transaction)
    .await
    .map_err(store_err)?;
    if touched.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "turn {turn_id} changed while accepting steer {id}"
        )));
    }

    transaction.commit().await.map_err(store_err)?;
    Ok(AcceptTurnSteerOutcome::Accepted(turn_steer_from_model(
        steer,
    )?))
}

pub(in crate::db) async fn list_pending_turn_steers(
    store: &DbStore,
    turn_id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
) -> Result<Option<Vec<TurnSteer>>> {
    if turn_id.0.is_nil() || lease_token.is_nil() {
        return Err(AgentError::Store(
            "turn and lease identities must not be nil".into(),
        ));
    }
    let now = canonical_db_timestamp(now)?;
    let Some(claim) = entities::turn_claim::Entity::find_by_id(lease_token)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .filter(|claim| claim.turn_id == turn_id.0)
    else {
        return Ok(None);
    };
    let live = entities::turn_run::Entity::find_by_id(turn_id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .is_some_and(|turn| {
            turn.status == TurnRunStatus::Running.as_str()
                && turn.attempt_count == claim.attempt_count
                && turn.lease_token == Some(lease_token)
                && turn
                    .lease_expires_at
                    .is_some_and(|lease_expires_at| lease_expires_at > now)
                && turn.updated_at <= now
        });
    if !live {
        return Ok(None);
    }
    entities::turn_steer::Entity::find()
        .filter(entities::turn_steer::Column::TurnId.eq(turn_id.0))
        .filter(entities::turn_steer::Column::Status.eq(TurnSteerStatus::Pending.as_str()))
        .order_by_asc(entities::turn_steer::Column::CreatedAt)
        .order_by_asc(entities::turn_steer::Column::Id)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(turn_steer_from_model)
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

pub(in crate::db) async fn apply_turn_steer(
    store: &DbStore,
    turn_id: TurnId,
    lease_token: uuid::Uuid,
    steer_id: TurnSteerId,
    now: chrono::DateTime<Utc>,
) -> Result<Option<ApplyTurnSteerOutcome>> {
    if turn_id.0.is_nil() || lease_token.is_nil() || steer_id.0.is_nil() {
        return Err(AgentError::Store(
            "turn, lease, and steer identities must not be nil".into(),
        ));
    }
    let now = canonical_db_timestamp(now)?;
    let Some(chat_id) = entities::turn_run::Entity::find_by_id(turn_id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(|turn| ChatId(turn.chat_id))
    else {
        return Ok(None);
    };
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        return Err(AgentError::Store(format!(
            "turn {turn_id} references missing chat {chat_id}"
        )));
    }
    if !acquire_turn_write_lock(&transaction, turn_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(steer) = entities::turn_steer::Entity::find_by_id(steer_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let status = turn_steer_status_from_db(&steer.status)?;
    if status == TurnSteerStatus::Applied {
        let exact = steer.turn_id == turn_id.0 && steer.applied_lease_token == Some(lease_token);
        let outcome = if exact {
            ensure_exact_applied_message_on(&transaction, &steer).await?;
            Some(ApplyTurnSteerOutcome::Existing(turn_steer_from_model(
                steer,
            )?))
        } else {
            None
        };
        transaction.commit().await.map_err(store_err)?;
        return Ok(outcome);
    }
    if status == TurnSteerStatus::Rejected || steer.turn_id != turn_id.0 {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let earliest_pending = entities::turn_steer::Entity::find()
        .filter(entities::turn_steer::Column::TurnId.eq(turn_id.0))
        .filter(entities::turn_steer::Column::Status.eq(TurnSteerStatus::Pending.as_str()))
        .order_by_asc(entities::turn_steer::Column::CreatedAt)
        .order_by_asc(entities::turn_steer::Column::Id)
        .one(&transaction)
        .await
        .map_err(store_err)?;
    if earliest_pending.as_ref().map(|pending| pending.id) != Some(steer.id) {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }

    let Some(claim) = entities::turn_claim::Entity::find_by_id(lease_token)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .filter(|claim| claim.turn_id == turn_id.0)
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let Some(turn) = entities::turn_run::Entity::find_by_id(turn_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if turn.status != TurnRunStatus::Running.as_str()
        || turn.attempt_count != claim.attempt_count
        || turn.lease_token != Some(lease_token)
        || turn
            .lease_expires_at
            .is_none_or(|lease_expires_at| lease_expires_at <= now)
        || turn.updated_at > now
        || steer.chat_id != turn.chat_id
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let next_steer_revision = turn
        .steer_revision
        .checked_add(1)
        .ok_or_else(|| AgentError::Store(format!("turn {turn_id} steer revision overflow")))?;

    if !transfer_steer_message_identity_on(
        &transaction,
        MessageId(steer.id),
        ChatId(steer.chat_id),
        TurnId(steer.turn_id),
    )
    .await?
    {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "turn steer {steer_id} has a missing or conflicting message reservation"
        )));
    }
    let message = entities::message::ActiveModel {
        id: Set(steer.id),
        chat_id: Set(steer.chat_id),
        turn_id: Set(steer.turn_id),
        seq: Set(next_message_seq_on(&transaction, ChatId(steer.chat_id)).await?),
        role: Set("user".into()),
        content: Set(steer.content.clone()),
        created_at: Set(now),
    };
    if let Err(error) = message.insert(&transaction).await {
        transaction.rollback().await.map_err(store_err)?;
        return Err(store_err(error));
    }

    let applied = entities::turn_steer::Entity::update_many()
        .col_expr(
            entities::turn_steer::Column::Status,
            sea_orm::sea_query::Expr::value(TurnSteerStatus::Applied.as_str()),
        )
        .col_expr(
            entities::turn_steer::Column::AppliedLeaseToken,
            sea_orm::sea_query::Expr::value(Some(lease_token)),
        )
        .col_expr(
            entities::turn_steer::Column::MessageId,
            sea_orm::sea_query::Expr::value(Some(steer.id)),
        )
        .col_expr(
            entities::turn_steer::Column::ResolvedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .filter(entities::turn_steer::Column::Id.eq(steer.id))
        .filter(entities::turn_steer::Column::Status.eq(TurnSteerStatus::Pending.as_str()))
        .filter(entities::turn_steer::Column::AppliedLeaseToken.is_null())
        .filter(entities::turn_steer::Column::MessageId.is_null())
        .filter(entities::turn_steer::Column::ResolvedAt.is_null())
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if applied.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }

    let touched = entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .col_expr(
            entities::turn_run::Column::SteerRevision,
            sea_orm::sea_query::Expr::value(next_steer_revision),
        )
        .col_expr(
            entities::turn_run::Column::LastSteerAppliedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .filter(entities::turn_run::Column::Id.eq(turn_id.0))
        .filter(entities::turn_run::Column::Status.eq(TurnRunStatus::Running.as_str()))
        .filter(entities::turn_run::Column::AttemptCount.eq(claim.attempt_count))
        .filter(entities::turn_run::Column::LeaseToken.eq(lease_token))
        .filter(entities::turn_run::Column::LeaseExpiresAt.eq(turn.lease_expires_at))
        .filter(entities::turn_run::Column::LeaseExpiresAt.gt(now))
        .filter(entities::turn_run::Column::SteerRevision.eq(turn.steer_revision))
        .filter(entities::turn_run::Column::UpdatedAt.eq(turn.updated_at))
        .filter(entities::turn_run::Column::UpdatedAt.lte(now))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if touched.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }

    let applied = entities::turn_steer::Entity::find_by_id(steer.id)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("applied steer {steer_id} disappeared")))?;
    ensure_exact_applied_message_on(&transaction, &applied).await?;
    let applied = turn_steer_from_model(applied)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(ApplyTurnSteerOutcome::Applied(applied)))
}

pub(super) async fn reject_pending_turn_steers_on<C>(
    conn: &C,
    turn_id: TurnId,
    resolved_at: chrono::DateTime<Utc>,
) -> Result<u64>
where
    C: ConnectionTrait,
{
    let rejected = entities::turn_steer::Entity::update_many()
        .col_expr(
            entities::turn_steer::Column::Status,
            sea_orm::sea_query::Expr::value(TurnSteerStatus::Rejected.as_str()),
        )
        .col_expr(
            entities::turn_steer::Column::ResolvedAt,
            sea_orm::sea_query::Expr::value(Some(resolved_at)),
        )
        .filter(entities::turn_steer::Column::TurnId.eq(turn_id.0))
        .filter(entities::turn_steer::Column::Status.eq(TurnSteerStatus::Pending.as_str()))
        .filter(entities::turn_steer::Column::AppliedLeaseToken.is_null())
        .filter(entities::turn_steer::Column::MessageId.is_null())
        .filter(entities::turn_steer::Column::ResolvedAt.is_null())
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(rejected.rows_affected)
}

fn validate_steer_input(
    id: TurnSteerId,
    turn_id: TurnId,
    chat_id: ChatId,
    content: &str,
) -> Result<()> {
    let content_len = content.chars().count();
    if id.0.is_nil() || turn_id.0.is_nil() || chat_id.0.is_nil() {
        return Err(AgentError::Store(
            "steer, turn, and chat identities must not be nil".into(),
        ));
    }
    if content.trim().is_empty()
        || content.contains('\0')
        || !(1..=TurnSteer::MAX_CONTENT_LEN).contains(&content_len)
    {
        return Err(AgentError::Store(format!(
            "turn steer content must contain 1 to {} non-NUL characters",
            TurnSteer::MAX_CONTENT_LEN
        )));
    }
    Ok(())
}

fn exact_accepted_steer(
    existing: entities::turn_steer::Model,
    turn_id: TurnId,
    chat_id: ChatId,
    content: &str,
    interrupt: bool,
) -> Result<AcceptTurnSteerOutcome> {
    if existing.turn_id != turn_id.0
        || existing.chat_id != chat_id.0
        || existing.content != content
        || existing.interrupt != interrupt
    {
        return Ok(AcceptTurnSteerOutcome::IdentityConflict);
    }
    Ok(AcceptTurnSteerOutcome::Existing(turn_steer_from_model(
        existing,
    )?))
}

async fn ensure_exact_applied_message_on<C>(
    conn: &C,
    steer: &entities::turn_steer::Model,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let exact = entities::message::Entity::find_by_id(steer.id)
        .one(conn)
        .await
        .map_err(store_err)?
        .is_some_and(|message| {
            message.chat_id == steer.chat_id
                && message.turn_id == steer.turn_id
                && message.role == "user"
                && message.content == steer.content
                && steer
                    .resolved_at
                    .is_some_and(|resolved_at| message.created_at == resolved_at)
        });
    let exact_identity = entities::message_identity::Entity::find_by_id(steer.id)
        .one(conn)
        .await
        .map_err(store_err)?
        .is_some_and(|identity| {
            identity.chat_id == steer.chat_id
                && identity.turn_id == steer.turn_id
                && identity.owner == MESSAGE_IDENTITY_OWNER_MESSAGE
        });
    if !exact || !exact_identity {
        return Err(AgentError::Store(format!(
            "applied turn steer {} has a different or missing message",
            TurnSteerId(steer.id)
        )));
    }
    Ok(())
}

fn turn_steer_from_model(model: entities::turn_steer::Model) -> Result<TurnSteer> {
    Ok(TurnSteer {
        id: TurnSteerId(model.id),
        turn_id: TurnId(model.turn_id),
        chat_id: ChatId(model.chat_id),
        content: model.content,
        interrupt: model.interrupt,
        status: turn_steer_status_from_db(&model.status)?,
        applied_lease_token: model.applied_lease_token,
        message_id: model.message_id.map(MessageId),
        created_at: model.created_at,
        resolved_at: model.resolved_at,
    })
}

fn turn_steer_status_from_db(value: &str) -> Result<TurnSteerStatus> {
    match value {
        "pending" => Ok(TurnSteerStatus::Pending),
        "applied" => Ok(TurnSteerStatus::Applied),
        "rejected" => Ok(TurnSteerStatus::Rejected),
        other => Err(AgentError::Store(format!(
            "invalid durable turn steer status: {other}"
        ))),
    }
}
