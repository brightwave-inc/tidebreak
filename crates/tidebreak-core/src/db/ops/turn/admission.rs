//! Durable ownership of client-supplied turn ids before mutable admission.

use sea_orm::sea_query::{Expr, SimpleExpr};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, ExprTrait, QueryFilter, Set,
    TransactionTrait, TryInsertResult,
};

use crate::error::{AgentError, Result};
use crate::model::{TurnAdmissionLease, TurnAdmissionRequest, TurnRun};
use crate::storage::BeginTurnAdmissionOutcome;

use super::super::super::{entities, store_err, DbStore};
use super::super::agent_run::database_now;

pub(super) const STATE_PENDING: &str = "pending";
pub(super) const STATE_QUEUED: &str = "queued";
pub(super) const STATE_ACCEPTED: &str = "accepted";
const MAX_LEASE_TTL: chrono::Duration = chrono::Duration::minutes(5);

fn statement_now_expr(backend: DatabaseBackend) -> SimpleExpr {
    match backend {
        // CURRENT_TIMESTAMP is fixed at transaction start on PostgreSQL, which
        // is precisely the stale authorization boundary this fence must avoid.
        DatabaseBackend::Postgres => Expr::cust("clock_timestamp()"),
        // Match database_now's fractional, end-of-millisecond timestamp so a
        // lease expiring during a long SQLite transaction cannot still commit.
        DatabaseBackend::Sqlite => Expr::cust("(strftime('%Y-%m-%dT%H:%M:%f', 'now') || '999Z')"),
        _ => Expr::cust("CURRENT_TIMESTAMP"),
    }
}

pub(super) fn validate_request(request: &TurnAdmissionRequest) -> Result<()> {
    if request.id.0.is_nil()
        || request.content.trim().is_empty()
        || request.content.contains('\0')
        || request
            .attachments
            .len()
            .saturating_add(request.file_attachments.len())
            > crate::MAX_MESSAGE_ATTACHMENTS
        || request.invoked_skills.len() > TurnRun::MAX_INVOKED_SKILLS
    {
        return Err(AgentError::Store("invalid turn admission request".into()));
    }
    let mut distinct = std::collections::HashSet::with_capacity(request.invoked_skills.len());
    if request.invoked_skills.iter().any(|skill| {
        skill.is_empty()
            || skill.len() > TurnRun::MAX_INVOKED_SKILL_NAME_LEN
            || !distinct.insert(skill.as_str())
    }) {
        return Err(AgentError::Store(
            "invalid invoked skill identity in turn admission".into(),
        ));
    }
    Ok(())
}

pub(super) fn request_matches(
    row: &entities::turn_admission::Model,
    request: &TurnAdmissionRequest,
) -> bool {
    row.chat_id == request.chat_id.0 && row.fingerprint == request.fingerprint()
}

pub(in crate::db) async fn begin(
    store: &DbStore,
    request: &TurnAdmissionRequest,
    lease_token: uuid::Uuid,
    lease_ttl: chrono::Duration,
) -> Result<BeginTurnAdmissionOutcome> {
    validate_request(request)?;
    if lease_token.is_nil() || lease_ttl <= chrono::Duration::zero() || lease_ttl > MAX_LEASE_TTL {
        return Err(AgentError::Store(
            "turn admission requires a non-nil token and a lease duration of at most five minutes"
                .into(),
        ));
    }

    loop {
        let transaction = store.conn.begin().await.map_err(store_err)?;
        let now = database_now(&transaction).await?;
        let lease_expires_at = now
            .checked_add_signed(lease_ttl)
            .ok_or_else(|| AgentError::Store("turn admission lease duration overflowed".into()))?;
        let fingerprint = request.fingerprint().to_vec();
        let inserted =
            entities::turn_admission::Entity::insert(entities::turn_admission::ActiveModel {
                id: Set(request.id.0),
                chat_id: Set(request.chat_id.0),
                fingerprint: Set(fingerprint.clone()),
                state: Set(STATE_PENDING.into()),
                lease_token: Set(Some(lease_token)),
                lease_expires_at: Set(Some(lease_expires_at)),
                created_at: Set(now),
                updated_at: Set(now),
            })
            .on_conflict_do_nothing()
            .exec_without_returning(&transaction)
            .await
            .map_err(store_err)?;
        if matches!(inserted, TryInsertResult::Inserted(1)) {
            transaction.commit().await.map_err(store_err)?;
            return Ok(BeginTurnAdmissionOutcome::Acquired(TurnAdmissionLease {
                id: request.id,
                lease_token,
                lease_expires_at,
            }));
        }

        let Some(existing) = entities::turn_admission::Entity::find_by_id(request.id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?
        else {
            transaction.rollback().await.map_err(store_err)?;
            continue;
        };
        if !request_matches(&existing, request) {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(BeginTurnAdmissionOutcome::IdentityConflict);
        }
        match existing.state.as_str() {
            STATE_ACCEPTED => {
                transaction.commit().await.map_err(store_err)?;
                return Ok(BeginTurnAdmissionOutcome::Accepted);
            }
            STATE_QUEUED => {
                transaction.commit().await.map_err(store_err)?;
                return Ok(BeginTurnAdmissionOutcome::Queued);
            }
            STATE_PENDING => {
                let Some(existing_token) = existing.lease_token else {
                    transaction.rollback().await.map_err(store_err)?;
                    return Err(AgentError::Store(format!(
                        "pending turn admission {} is missing its lease token",
                        request.id
                    )));
                };
                let Some(existing_expiry) = existing.lease_expires_at else {
                    transaction.rollback().await.map_err(store_err)?;
                    return Err(AgentError::Store(format!(
                        "pending turn admission {} is missing its lease expiry",
                        request.id
                    )));
                };
                if existing_token == lease_token && existing_expiry > now {
                    transaction.commit().await.map_err(store_err)?;
                    return Ok(BeginTurnAdmissionOutcome::Acquired(TurnAdmissionLease {
                        id: request.id,
                        lease_token,
                        lease_expires_at: existing_expiry,
                    }));
                }
                if existing_expiry > now {
                    transaction.commit().await.map_err(store_err)?;
                    return Ok(BeginTurnAdmissionOutcome::Pending {
                        lease_expires_at: existing_expiry,
                    });
                }

                let taken = entities::turn_admission::Entity::update_many()
                    .col_expr(
                        entities::turn_admission::Column::LeaseToken,
                        Expr::value(Some(lease_token)),
                    )
                    .col_expr(
                        entities::turn_admission::Column::LeaseExpiresAt,
                        Expr::value(Some(lease_expires_at)),
                    )
                    .col_expr(
                        entities::turn_admission::Column::UpdatedAt,
                        Expr::value(now),
                    )
                    .filter(entities::turn_admission::Column::Id.eq(request.id.0))
                    .filter(entities::turn_admission::Column::State.eq(STATE_PENDING))
                    .filter(entities::turn_admission::Column::LeaseToken.eq(Some(existing_token)))
                    .filter(
                        entities::turn_admission::Column::LeaseExpiresAt.eq(Some(existing_expiry)),
                    )
                    .exec(&transaction)
                    .await
                    .map_err(store_err)?;
                if taken.rows_affected == 1 {
                    transaction.commit().await.map_err(store_err)?;
                    return Ok(BeginTurnAdmissionOutcome::Acquired(TurnAdmissionLease {
                        id: request.id,
                        lease_token,
                        lease_expires_at,
                    }));
                }
                transaction.rollback().await.map_err(store_err)?;
            }
            _ => {
                transaction.rollback().await.map_err(store_err)?;
                return Err(AgentError::Store(format!(
                    "turn admission {} has invalid state {}",
                    request.id, existing.state
                )));
            }
        }
    }
}

pub(in crate::db) async fn release(store: &DbStore, lease: TurnAdmissionLease) -> Result<bool> {
    if lease.id.0.is_nil() || lease.lease_token.is_nil() {
        return Ok(false);
    }
    let statement_now = statement_now_expr(store.conn.get_database_backend());
    let deleted = entities::turn_admission::Entity::delete_many()
        .filter(entities::turn_admission::Column::Id.eq(lease.id.0))
        .filter(entities::turn_admission::Column::State.eq(STATE_PENDING))
        .filter(entities::turn_admission::Column::LeaseToken.eq(Some(lease.lease_token)))
        .filter(entities::turn_admission::Column::LeaseExpiresAt.eq(Some(lease.lease_expires_at)))
        .filter(Expr::col(entities::turn_admission::Column::LeaseExpiresAt).gt(statement_now))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(deleted.rows_affected == 1)
}

pub(super) async fn lease_is_current_on<C>(
    conn: &C,
    lease: TurnAdmissionLease,
    chat_id: crate::ChatId,
    fingerprint: [u8; 32],
) -> Result<bool>
where
    C: ConnectionTrait,
{
    let now = database_now(conn).await?;
    Ok(entities::turn_admission::Entity::find_by_id(lease.id.0)
        .filter(entities::turn_admission::Column::ChatId.eq(chat_id.0))
        .filter(entities::turn_admission::Column::Fingerprint.eq(fingerprint.to_vec()))
        .filter(entities::turn_admission::Column::State.eq(STATE_PENDING))
        .filter(entities::turn_admission::Column::LeaseToken.eq(Some(lease.lease_token)))
        .filter(entities::turn_admission::Column::LeaseExpiresAt.eq(Some(lease.lease_expires_at)))
        .filter(entities::turn_admission::Column::LeaseExpiresAt.gt(now))
        .one(conn)
        .await
        .map_err(store_err)?
        .is_some())
}

pub(super) async fn transition_pending_on<C>(
    conn: &C,
    lease: TurnAdmissionLease,
    state: &str,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    let statement_now = statement_now_expr(conn.get_database_backend());
    let updated = entities::turn_admission::Entity::update_many()
        .col_expr(entities::turn_admission::Column::State, Expr::value(state))
        .col_expr(
            entities::turn_admission::Column::LeaseToken,
            Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::turn_admission::Column::LeaseExpiresAt,
            Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
        )
        .col_expr(
            entities::turn_admission::Column::UpdatedAt,
            statement_now.clone(),
        )
        .filter(entities::turn_admission::Column::Id.eq(lease.id.0))
        .filter(entities::turn_admission::Column::State.eq(STATE_PENDING))
        .filter(entities::turn_admission::Column::LeaseToken.eq(Some(lease.lease_token)))
        .filter(entities::turn_admission::Column::LeaseExpiresAt.eq(Some(lease.lease_expires_at)))
        .filter(Expr::col(entities::turn_admission::Column::LeaseExpiresAt).gt(statement_now))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(updated.rows_affected == 1)
}

pub(super) async fn update_queued_fingerprint_on<C>(
    conn: &C,
    request: &TurnAdmissionRequest,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    let updated = entities::turn_admission::Entity::update_many()
        .col_expr(
            entities::turn_admission::Column::Fingerprint,
            Expr::value(request.fingerprint().to_vec()),
        )
        .col_expr(
            entities::turn_admission::Column::UpdatedAt,
            Expr::value(now),
        )
        .filter(entities::turn_admission::Column::Id.eq(request.id.0))
        .filter(entities::turn_admission::Column::ChatId.eq(request.chat_id.0))
        .filter(entities::turn_admission::Column::State.eq(STATE_QUEUED))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(updated.rows_affected == 1)
}
