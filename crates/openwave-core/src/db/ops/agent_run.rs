use chrono::Utc;
use sea_orm::{
    sea_query::ExprTrait, ActiveModelTrait, ColumnTrait, Condition, DatabaseBackend, EntityTrait,
    NotSet, QueryFilter, QueryOrder, QuerySelect, Set, Statement, TransactionTrait,
};

use crate::agent_tools::SandboxAgentFileResource;
use crate::error::{AgentError, Result};
use crate::id::{AgentRunId, CallId, ChatId, MessageId, TurnId};
use crate::model::{
    AgentRun, AgentRunCancellationReason, AgentRunExecutionLocation, AgentRunInboxEntry,
    AgentRunInboxStatus, AgentRunResult, AgentRunResultPayload, AgentRunStatus, AgentRunTier,
    SandboxAgentAdmission, TurnRun, TurnRunStatus, TurnSteerStatus,
};
use crate::storage::{
    AcceptAgentRunOutcome, AcceptSandboxAgentRunAndParkTurnOutcome, AdmitSandboxAgentRunOutcome,
    ClaimAgentRunInboxOutcome, ConsumeAgentRunInboxAndResumeTurnOutcome,
    ConsumeAgentRunInboxOutcome, FailAgentRunOutcome, ParkTurnForAgentRunInboxOutcome,
    SubmitAgentRunResultOutcome,
};
use crate::{RequestFolderAccessArgs, RequestedFolderHint};

use super::super::{entities, store_err, DbStore};
use super::{
    acquire_chat_write_lock, acquire_turn_write_lock,
    conversation::{
        next_message_seq_on, reserve_message_identity_on, MESSAGE_IDENTITY_OWNER_MESSAGE,
    },
    turn::{
        canonical_db_timestamp, park_turn_for_agent_run_inbox_on,
        validate_agent_run_inbox_park_request,
    },
};

mod cancellation;
pub(in crate::db) use cancellation::{
    cancel_sandbox_children_for_origin_turn_on, finish_agent_run_cancellation,
    get_agent_run_cancellation_signal, request_agent_run_cancellation,
    unsettled_sandbox_children_for_origin_turn_on,
};

pub(in crate::db) async fn insert_foreground_agent_run_on<C>(
    conn: &C,
    chat_id: ChatId,
    created_at: chrono::DateTime<Utc>,
) -> Result<AgentRunId>
where
    C: sea_orm::ConnectionTrait,
{
    let id = AgentRunId::foreground_for_chat(chat_id);
    let created_at = canonical_db_timestamp(created_at)?;
    entities::agent_run::ActiveModel {
        id: Set(id.0),
        chat_id: Set(chat_id.0),
        parent_id: Set(None),
        parent_depth: Set(None),
        spawn_call_id: Set(None),
        tier: Set(AgentRunTier::Foreground.as_str().into()),
        execution_location: Set(AgentRunExecutionLocation::InProcess.as_str().into()),
        depth: Set(0),
        status: Set(AgentRunStatus::Active.as_str().into()),
        input: Set(None),
        // A foreground coordinator carries no model of its own; each of its
        // turns records the selection it ran.
        model: NotSet,
        attempt_count: Set(0),
        max_attempts: Set(0),
        claim_count: Set(0),
        available_at: Set(created_at),
        deadline_at: Set(None),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        started_at: Set(None),
        finished_at: Set(None),
        last_error_code: Set(None),
        last_error_detail: Set(None),
        created_at: Set(created_at),
        updated_at: Set(created_at),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    Ok(id)
}

pub(in crate::db) async fn find_foreground_agent_run_on<C>(
    conn: &C,
    chat_id: ChatId,
) -> Result<Option<AgentRun>>
where
    C: sea_orm::ConnectionTrait,
{
    find_foreground_on(conn, chat_id)
        .await?
        .map(agent_run_from_model)
        .transpose()
}

pub(in crate::db) async fn accept_agent_run(
    store: &DbStore,
    id: AgentRunId,
    chat_id: ChatId,
    parent_id: Option<AgentRunId>,
    spawn_call_id: Option<CallId>,
    tier: AgentRunTier,
    input: Option<&str>,
) -> Result<AcceptAgentRunOutcome> {
    if tier == AgentRunTier::Background {
        return Err(AgentError::Store(
            "sandbox agent runs require exact turn-bound admission".into(),
        ));
    }
    validate_request(id, parent_id, spawn_call_id, tier, input)?;

    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        return Err(AgentError::Store(format!("chat {chat_id} does not exist")));
    }

    if let Some(existing) = find_by_id_on(&transaction, id).await? {
        let outcome =
            existing_request_outcome(existing, chat_id, parent_id, spawn_call_id, tier, input)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(outcome);
    }
    if let Some(spawn_call_id) = spawn_call_id {
        if let Some(existing) = find_by_spawn_call_on(&transaction, spawn_call_id).await? {
            let outcome = existing_request_outcome(
                existing,
                chat_id,
                parent_id,
                Some(spawn_call_id),
                tier,
                input,
            )?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(outcome);
        }
    }

    let (depth, status) = match tier {
        AgentRunTier::Foreground => {
            if let Some(existing) = entities::agent_run::Entity::find()
                .filter(entities::agent_run::Column::ChatId.eq(chat_id.0))
                .filter(entities::agent_run::Column::Tier.eq(AgentRunTier::Foreground.as_str()))
                .one(&transaction)
                .await
                .map_err(store_err)?
            {
                let existing = agent_run_from_model(existing)?;
                transaction.commit().await.map_err(store_err)?;
                return Ok(AcceptAgentRunOutcome::ForegroundExists(existing));
            }
            (0_i16, AgentRunStatus::Active)
        }
        AgentRunTier::Background => {
            let Some(parent_id) = parent_id else {
                unreachable!("validated sandbox parent")
            };
            let parent = find_by_id_on(&transaction, parent_id).await?;
            let available = parent.is_some_and(|parent| {
                parent.chat_id == chat_id.0
                    && parent.parent_id.is_none()
                    && parent.depth == 0
                    && parent.tier == AgentRunTier::Foreground.as_str()
                    && parent.status == AgentRunStatus::Active.as_str()
            });
            if !available {
                transaction.commit().await.map_err(store_err)?;
                return Ok(AcceptAgentRunOutcome::ParentUnavailable);
            }
            (i16::from(AgentRun::MAX_DEPTH), AgentRunStatus::Queued)
        }
    };

    let now = database_now(&transaction).await?;
    let model = entities::agent_run::ActiveModel {
        id: Set(id.0),
        chat_id: Set(chat_id.0),
        parent_id: Set(parent_id.map(|parent| parent.0)),
        parent_depth: Set(parent_id.map(|_| 0)),
        spawn_call_id: Set(spawn_call_id.map(|call| call.0)),
        tier: Set(tier.as_str().into()),
        execution_location: Set(AgentRunExecutionLocation::InProcess.as_str().into()),
        depth: Set(depth),
        status: Set(status.as_str().into()),
        input: Set(input.map(ToOwned::to_owned)),
        // Only turn-bound sandbox admission records a model, and this path
        // rejects sandbox execution outright.
        model: NotSet,
        attempt_count: Set(0),
        max_attempts: Set(match tier {
            AgentRunTier::Foreground => 0,
            AgentRunTier::Background => AgentRun::DEFAULT_MAX_ATTEMPTS,
        }),
        claim_count: Set(0),
        available_at: Set(now),
        deadline_at: Set(match tier {
            AgentRunTier::Foreground => None,
            AgentRunTier::Background => Some(now + AgentRun::DEFAULT_MAX_DURATION),
        }),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        started_at: Set(None),
        finished_at: Set(None),
        last_error_code: Set(None),
        last_error_detail: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    let model = match model.insert(&transaction).await {
        Ok(model) => model,
        Err(error) => {
            transaction.rollback().await.map_err(store_err)?;
            if let Some(existing) = find_by_id_on(&store.conn, id).await? {
                return existing_request_outcome(
                    existing,
                    chat_id,
                    parent_id,
                    spawn_call_id,
                    tier,
                    input,
                );
            }
            if let Some(spawn_call_id) = spawn_call_id {
                if let Some(existing) = find_by_spawn_call_on(&store.conn, spawn_call_id).await? {
                    return existing_request_outcome(
                        existing,
                        chat_id,
                        parent_id,
                        Some(spawn_call_id),
                        tier,
                        input,
                    );
                }
            }
            if tier == AgentRunTier::Foreground {
                if let Some(existing) = find_foreground_on(&store.conn, chat_id).await? {
                    return Ok(AcceptAgentRunOutcome::ForegroundExists(
                        agent_run_from_model(existing)?,
                    ));
                }
            }
            return Err(store_err(error));
        }
    };
    let run = agent_run_from_model(model)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(AcceptAgentRunOutcome::Accepted(run))
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn admit_sandbox_agent_run(
    store: &DbStore,
    origin_turn_id: TurnId,
    spawn_call_id: CallId,
    input: &str,
    lease_token: uuid::Uuid,
    expected_steer_revision: i64,
    max_outstanding_children: u32,
    now: chrono::DateTime<Utc>,
) -> Result<Option<AdmitSandboxAgentRunOutcome>> {
    admit_sandbox_agent_run_at(
        store,
        origin_turn_id,
        spawn_call_id,
        input,
        AgentRunExecutionLocation::InProcess,
        lease_token,
        expected_steer_revision,
        max_outstanding_children,
        now,
    )
    .await
}

/// Admit a depth-one sandbox child that runs inside a sandbox-resident
/// container, host-driven over the wire protocol. Identical to
/// [`admit_sandbox_agent_run`] except the child's execution location is
/// [`AgentRunExecutionLocation::Container`], so the in-process scheduler leaves
/// it for the sandbox-resident driver to claim, provision, attach, and drive.
#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn admit_sandbox_container_agent_run(
    store: &DbStore,
    origin_turn_id: TurnId,
    spawn_call_id: CallId,
    input: &str,
    lease_token: uuid::Uuid,
    expected_steer_revision: i64,
    max_outstanding_children: u32,
    now: chrono::DateTime<Utc>,
) -> Result<Option<AdmitSandboxAgentRunOutcome>> {
    admit_sandbox_agent_run_at(
        store,
        origin_turn_id,
        spawn_call_id,
        input,
        AgentRunExecutionLocation::Container,
        lease_token,
        expected_steer_revision,
        max_outstanding_children,
        now,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn admit_sandbox_agent_run_at(
    store: &DbStore,
    origin_turn_id: TurnId,
    spawn_call_id: CallId,
    input: &str,
    execution_location: AgentRunExecutionLocation,
    lease_token: uuid::Uuid,
    expected_steer_revision: i64,
    max_outstanding_children: u32,
    now: chrono::DateTime<Utc>,
) -> Result<Option<AdmitSandboxAgentRunOutcome>> {
    validate_admission_request(
        origin_turn_id,
        spawn_call_id,
        input,
        lease_token,
        max_outstanding_children,
    )?;
    // Validate the caller timestamp at the boundary, but never use a value
    // captured before lock acquisition to fence a live lease.
    canonical_db_timestamp(now)?;
    let Some(scope) = entities::turn_run::Entity::find_by_id(origin_turn_id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    let transaction = store.conn.begin().await.map_err(store_err)?;
    // Admission and terminal child delivery share the scheduler lock before
    // chat and turn ownership. This gives the unsettled-child classifier one
    // stable snapshot and preserves the global scheduler -> chat -> turn order.
    acquire_agent_run_claim_lock(&transaction).await?;
    if !acquire_chat_write_lock(&transaction, ChatId(scope.chat_id)).await?
        || !acquire_turn_write_lock(&transaction, origin_turn_id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let turn = entities::turn_run::Entity::find_by_id(origin_turn_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked turn exists");
    let now = database_now(&transaction).await?;
    let outcome = admit_sandbox_agent_run_on(
        &transaction,
        &turn,
        spawn_call_id,
        input,
        None,
        execution_location,
        lease_token,
        expected_steer_revision,
        max_outstanding_children,
        now,
    )
    .await;
    match outcome {
        Ok(outcome) => {
            transaction.commit().await.map_err(store_err)?;
            Ok(Some(outcome))
        }
        Err(error) => {
            // PostgreSQL aborts the transaction after a unique-key race. Roll
            // it back before resolving the immutable identity on a fresh
            // connection; this also covers an ambiguous insert result.
            transaction.rollback().await.map_err(store_err)?;
            if let Some(outcome) = resolve_existing_sandbox_admission_on(
                &store.conn,
                origin_turn_id,
                ChatId(turn.chat_id),
                AgentRunId(turn.agent_run_id),
                spawn_call_id,
                input,
                None,
            )
            .await?
            {
                Ok(Some(outcome))
            } else {
                Err(error)
            }
        }
    }
}

pub(in crate::db) async fn get_sandbox_agent_admission(
    store: &DbStore,
    child_run_id: AgentRunId,
) -> Result<Option<SandboxAgentAdmission>> {
    entities::sandbox_agent_admission::Entity::find_by_id(child_run_id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(sandbox_agent_admission_from_model)
        .transpose()
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn admit_sandbox_agent_run_on<C>(
    conn: &C,
    turn: &entities::turn_run::Model,
    spawn_call_id: CallId,
    input: &str,
    resource: Option<&SandboxAgentFileResource>,
    execution_location: AgentRunExecutionLocation,
    lease_token: uuid::Uuid,
    expected_steer_revision: i64,
    max_outstanding_children: u32,
    now: chrono::DateTime<Utc>,
) -> Result<AdmitSandboxAgentRunOutcome>
where
    C: sea_orm::ConnectionTrait,
{
    let origin_turn_id = TurnId(turn.id);
    let chat_id = ChatId(turn.chat_id);
    let parent_id = AgentRunId(turn.agent_run_id);
    let child_run_id = AgentRunId::sandbox_for_spawn_call(spawn_call_id);

    if let Some(outcome) = resolve_existing_sandbox_admission_on(
        conn,
        origin_turn_id,
        chat_id,
        parent_id,
        spawn_call_id,
        input,
        resource,
    )
    .await?
    {
        return Ok(outcome);
    }

    if find_by_id_on(conn, child_run_id).await?.is_some()
        || find_by_spawn_call_on(conn, spawn_call_id).await?.is_some()
    {
        return Ok(AdmitSandboxAgentRunOutcome::IdentityConflict);
    }

    if turn.status != TurnRunStatus::Running.as_str()
        || turn.lease_token != Some(lease_token)
        || turn
            .lease_expires_at
            .is_none_or(|lease_expires_at| lease_expires_at <= now)
        || turn.updated_at > now
    {
        return Ok(AdmitSandboxAgentRunOutcome::LeaseLost);
    }
    let steer_pending = entities::turn_steer::Entity::find()
        .filter(entities::turn_steer::Column::TurnId.eq(origin_turn_id.0))
        .filter(entities::turn_steer::Column::Status.eq(TurnSteerStatus::Pending.as_str()))
        .one(conn)
        .await
        .map_err(store_err)?
        .is_some();
    if steer_pending || turn.steer_revision != expected_steer_revision {
        let turn = super::turn::turn_run_from_model(turn.clone())?;
        return Ok(if steer_pending {
            AdmitSandboxAgentRunOutcome::SteerPending(turn)
        } else {
            AdmitSandboxAgentRunOutcome::OutputSuperseded(turn)
        });
    }
    let parent = find_by_id_on(conn, parent_id).await?;
    let parent_available = parent.is_some_and(|parent| {
        parent.chat_id == chat_id.0
            && parent.parent_id.is_none()
            && parent.depth == 0
            && parent.tier == AgentRunTier::Foreground.as_str()
            && parent.status == AgentRunStatus::Active.as_str()
    });
    if !parent_available {
        return Ok(AdmitSandboxAgentRunOutcome::ParentUnavailable);
    }
    if let Some(resource) = resource {
        let attached = entities::chat_root_attachment::Entity::find_by_id((
            chat_id.0,
            *resource.root_id.as_uuid(),
        ))
        .one(conn)
        .await
        .map_err(store_err)?
        .is_some();
        if !attached {
            return Ok(AdmitSandboxAgentRunOutcome::DelegatedResourceUnavailable);
        }
    }

    // A terminal child remains outstanding until its immutable delivery has
    // been consumed or explicitly retired. Counting only live run rows would
    // let fast children fall out of this bound before the foreground model can
    // wait on them, eventually producing more IDs than one ordered wait can
    // consume. The shared classifier also validates admission and terminal
    // receipt provenance before capacity can be released.
    let outstanding = unsettled_sandbox_children_for_origin_turn_on(conn, turn).await?;
    if outstanding.len() >= max_outstanding_children as usize {
        return Ok(AdmitSandboxAgentRunOutcome::AtCapacity);
    }

    let created_at = database_now(conn).await?;
    let child = entities::agent_run::ActiveModel {
        id: Set(child_run_id.0),
        chat_id: Set(chat_id.0),
        parent_id: Set(Some(parent_id.0)),
        parent_depth: Set(Some(0)),
        spawn_call_id: Set(Some(spawn_call_id.0)),
        tier: Set(AgentRunTier::Background.as_str().into()),
        execution_location: Set(execution_location.as_str().into()),
        depth: Set(i16::from(AgentRun::MAX_DEPTH)),
        status: Set(AgentRunStatus::Queued.as_str().into()),
        input: Set(Some(input.into())),
        // Inherit the origin turn's frozen selection inside the same
        // transaction that admits the child. A sandbox executor can then run
        // the conversation's model without re-resolving mutable settings, and
        // the row records what it ran against.
        model: Set(Some(turn.model.clone())),
        attempt_count: Set(0),
        // A sandbox-resident (container) run has exactly one execution attempt:
        // an external effect (model spend) cannot be proven unexecuted after a
        // loss, so a lost run fails terminally and is never re-claimed into a
        // second attempt. Only in-process runs use the multi-attempt retry
        // machinery.
        max_attempts: Set(match execution_location {
            AgentRunExecutionLocation::Container => 1,
            _ => AgentRun::DEFAULT_MAX_ATTEMPTS,
        }),
        claim_count: Set(0),
        available_at: Set(created_at),
        deadline_at: Set(Some(created_at + AgentRun::DEFAULT_MAX_DURATION)),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        started_at: Set(None),
        finished_at: Set(None),
        last_error_code: Set(None),
        last_error_detail: Set(None),
        created_at: Set(created_at),
        updated_at: Set(created_at),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    let admission = entities::sandbox_agent_admission::ActiveModel {
        child_run_id: Set(child_run_id.0),
        parent_run_id: Set(parent_id.0),
        origin_turn_id: Set(origin_turn_id.0),
        chat_id: Set(chat_id.0),
        spawn_call_id: Set(spawn_call_id.0),
        delegated_root_id: Set(resource.map(|resource| *resource.root_id.as_uuid())),
        delegated_relative_path: Set(resource.map(|resource| resource.relative_path.clone())),
        admitted_at: Set(created_at),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    Ok(AdmitSandboxAgentRunOutcome::Accepted {
        child: agent_run_from_model(child)?,
        admission: sandbox_agent_admission_from_model(admission)?,
    })
}

async fn resolve_existing_sandbox_admission_on<C>(
    conn: &C,
    origin_turn_id: TurnId,
    chat_id: ChatId,
    parent_id: AgentRunId,
    spawn_call_id: CallId,
    input: &str,
    resource: Option<&SandboxAgentFileResource>,
) -> Result<Option<AdmitSandboxAgentRunOutcome>>
where
    C: sea_orm::ConnectionTrait,
{
    let child_run_id = AgentRunId::sandbox_for_spawn_call(spawn_call_id);
    let existing_admission = entities::sandbox_agent_admission::Entity::find()
        .filter(
            Condition::any()
                .add(entities::sandbox_agent_admission::Column::ChildRunId.eq(child_run_id.0))
                .add(entities::sandbox_agent_admission::Column::SpawnCallId.eq(spawn_call_id.0)),
        )
        .one(conn)
        .await
        .map_err(store_err)?;
    if let Some(admission) = existing_admission {
        let child = find_by_id_on(conn, AgentRunId(admission.child_run_id)).await?;
        let exact = admission.child_run_id == child_run_id.0
            && admission.parent_run_id == parent_id.0
            && admission.origin_turn_id == origin_turn_id.0
            && admission.chat_id == chat_id.0
            && admission.spawn_call_id == spawn_call_id.0
            && admission.delegated_root_id == resource.map(|resource| *resource.root_id.as_uuid())
            && admission.delegated_relative_path.as_deref()
                == resource.map(|resource| resource.relative_path.as_str())
            && child.as_ref().is_some_and(|child| {
                child.chat_id == chat_id.0
                    && child.parent_id == Some(parent_id.0)
                    && child.spawn_call_id == Some(spawn_call_id.0)
                    && child.tier == AgentRunTier::Background.as_str()
                    && child.input.as_deref() == Some(input)
            });
        return Ok(Some(if exact {
            AdmitSandboxAgentRunOutcome::Existing {
                child: agent_run_from_model(child.expect("exact admission has child"))?,
                admission: sandbox_agent_admission_from_model(admission)?,
            }
        } else {
            AdmitSandboxAgentRunOutcome::IdentityConflict
        }));
    }
    Ok(None)
}

/// Accept one sandbox child and park its exact foreground turn in the same
/// transaction. The sandbox parent comes from the locked turn, which binds
/// child hierarchy and checkpoint scope without trusting a caller-supplied
/// parent id.
#[allow(clippy::too_many_arguments)] // Atomic checkpoint inputs form the durable receipt identity.
pub(in crate::db) async fn accept_sandbox_agent_run_and_park_turn(
    store: &DbStore,
    child_run_id: AgentRunId,
    turn_id: TurnId,
    spawn_call_id: CallId,
    input: &str,
    lease_token: uuid::Uuid,
    expected_steer_revision: i64,
    progress: crate::TurnCheckpointProgress,
    now: chrono::DateTime<Utc>,
) -> Result<Option<AcceptSandboxAgentRunAndParkTurnOutcome>> {
    validate_agent_run_inbox_park_request(turn_id, child_run_id, lease_token, progress)?;
    // Validate the API value, then take authoritative time after acquiring the
    // same locks that protect the lease transition.
    canonical_db_timestamp(now)?;
    let Some(scope) = entities::turn_run::Entity::find_by_id(turn_id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };

    let transaction = store.conn.begin().await.map_err(store_err)?;
    // Legacy atomic spawn uses the same scheduler -> chat -> turn order as the
    // nonblocking checkpoint so the shared unsettled classifier is stable.
    acquire_agent_run_claim_lock(&transaction).await?;
    if !acquire_chat_write_lock(&transaction, ChatId(scope.chat_id)).await?
        || !acquire_turn_write_lock(&transaction, turn_id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let turn = entities::turn_run::Entity::find_by_id(turn_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked turn exists");
    let now = database_now(&transaction).await?;
    if child_run_id != AgentRunId::sandbox_for_spawn_call(spawn_call_id) {
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(
            AcceptSandboxAgentRunAndParkTurnOutcome::IdentityConflict,
        ));
    }
    validate_admission_request(
        turn_id,
        spawn_call_id,
        input,
        lease_token,
        AgentRun::DEFAULT_MAX_OUTSTANDING_CHILDREN,
    )?;
    let admission_outcome = admit_sandbox_agent_run_on(
        &transaction,
        &turn,
        spawn_call_id,
        input,
        None,
        AgentRunExecutionLocation::InProcess,
        lease_token,
        expected_steer_revision,
        AgentRun::DEFAULT_MAX_OUTSTANDING_CHILDREN,
        now,
    )
    .await;
    let admission_outcome = match admission_outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            let chat_id = ChatId(turn.chat_id);
            let parent_id = AgentRunId(turn.agent_run_id);
            transaction.rollback().await.map_err(store_err)?;
            if resolve_existing_sandbox_admission_on(
                &store.conn,
                turn_id,
                chat_id,
                parent_id,
                spawn_call_id,
                input,
                None,
            )
            .await?
            .is_some()
            {
                return Ok(Some(
                    AcceptSandboxAgentRunAndParkTurnOutcome::IdentityConflict,
                ));
            }
            return Err(error);
        }
    };
    let (child, existing) = match admission_outcome {
        AdmitSandboxAgentRunOutcome::Accepted { child, .. } => (child, false),
        AdmitSandboxAgentRunOutcome::Existing { child, .. } => {
            let atomic_wait = entities::turn_agent_run_wait::Entity::find_by_id(child.id.0)
                .one(&transaction)
                .await
                .map_err(store_err)?;
            if !atomic_wait.is_some_and(|wait| wait.atomic_admission) {
                transaction.commit().await.map_err(store_err)?;
                return Ok(Some(
                    AcceptSandboxAgentRunAndParkTurnOutcome::IdentityConflict,
                ));
            }
            (child, true)
        }
        AdmitSandboxAgentRunOutcome::IdentityConflict => {
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(
                AcceptSandboxAgentRunAndParkTurnOutcome::IdentityConflict,
            ));
        }
        AdmitSandboxAgentRunOutcome::ParentUnavailable => {
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(
                AcceptSandboxAgentRunAndParkTurnOutcome::ParentUnavailable,
            ));
        }
        AdmitSandboxAgentRunOutcome::DelegatedResourceUnavailable => {
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(
                AcceptSandboxAgentRunAndParkTurnOutcome::IdentityConflict,
            ));
        }
        AdmitSandboxAgentRunOutcome::LeaseLost => {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(None);
        }
        AdmitSandboxAgentRunOutcome::AtCapacity => {
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(AcceptSandboxAgentRunAndParkTurnOutcome::AtCapacity));
        }
        AdmitSandboxAgentRunOutcome::SteerPending(turn) => {
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(AcceptSandboxAgentRunAndParkTurnOutcome::SteerPending(
                turn,
            )));
        }
        AdmitSandboxAgentRunOutcome::OutputSuperseded(turn) => {
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(
                AcceptSandboxAgentRunAndParkTurnOutcome::OutputSuperseded(turn),
            ));
        }
    };

    let outcome = park_turn_for_agent_run_inbox_on(
        &transaction,
        turn_id,
        child_run_id,
        lease_token,
        expected_steer_revision,
        progress,
        now,
        true,
    )
    .await?;
    let Some(outcome) = outcome else {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    };
    let outcome = match outcome {
        ParkTurnForAgentRunInboxOutcome::Parked { turn, wait } => {
            AcceptSandboxAgentRunAndParkTurnOutcome::Parked { child, turn, wait }
        }
        ParkTurnForAgentRunInboxOutcome::Existing { turn, wait } if existing => {
            AcceptSandboxAgentRunAndParkTurnOutcome::Existing { child, turn, wait }
        }
        ParkTurnForAgentRunInboxOutcome::SteerPending(turn) => {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(Some(AcceptSandboxAgentRunAndParkTurnOutcome::SteerPending(
                turn,
            )));
        }
        ParkTurnForAgentRunInboxOutcome::OutputSuperseded(turn) => {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(Some(
                AcceptSandboxAgentRunAndParkTurnOutcome::OutputSuperseded(turn),
            ));
        }
        ParkTurnForAgentRunInboxOutcome::Existing { .. }
        | ParkTurnForAgentRunInboxOutcome::IdentityConflict => {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(Some(
                AcceptSandboxAgentRunAndParkTurnOutcome::IdentityConflict,
            ));
        }
    };
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(outcome))
}

pub(in crate::db) async fn get_agent_run(
    store: &DbStore,
    id: AgentRunId,
) -> Result<Option<AgentRun>> {
    find_by_id_on(&store.conn, id)
        .await?
        .map(agent_run_from_model)
        .transpose()
}

pub(in crate::db) async fn claim_agent_run(
    store: &DbStore,
    lease_token: uuid::Uuid,
    lease_duration: chrono::Duration,
    max_running_global: u32,
    max_running_per_chat: u32,
) -> Result<Option<AgentRun>> {
    validate_claim_request(
        lease_token,
        lease_duration,
        max_running_global,
        max_running_per_chat,
    )?;

    loop {
        let transaction = store.conn.begin().await.map_err(store_err)?;
        acquire_agent_run_claim_lock(&transaction).await?;
        let now = database_now(&transaction).await?;
        let lease_expires_at = now.checked_add_signed(lease_duration).ok_or_else(|| {
            AgentError::Store(
                "agent-run lease duration overflows the database timestamp range".into(),
            )
        })?;

        if let Some(receipt) = entities::agent_run_claim::Entity::find_by_id(lease_token)
            .one(&transaction)
            .await
            .map_err(store_err)?
        {
            let Some(agent_run_id) = receipt.agent_run_id else {
                transaction.commit().await.map_err(store_err)?;
                return Ok(None);
            };
            let admitted = entities::sandbox_agent_admission::Entity::find_by_id(agent_run_id)
                .one(&transaction)
                .await
                .map_err(store_err)?
                .is_some();
            let existing = find_by_id_on(&transaction, AgentRunId(agent_run_id))
                .await?
                .filter(|run| {
                    admitted
                        && run.tier == AgentRunTier::Background.as_str()
                        && matches!(run.status.as_str(), "running" | "cancelling")
                        && Some(run.attempt_count) == receipt.attempt_count
                        && Some(run.claim_count) == receipt.claim_count
                        && run.lease_token == Some(lease_token)
                        && run.lease_expires_at.is_some_and(|expiry| expiry > now)
                        && run.deadline_at.is_some_and(|deadline| deadline > now)
                })
                .map(agent_run_from_model)
                .transpose()?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(existing);
        }

        if let Some(expired) = find_expired_deadline_on(&transaction, now).await? {
            let updated = fail_candidate_on(
                &transaction,
                &expired,
                now,
                "deadline_exceeded",
                "sandbox agent exceeded its wall-clock deadline",
            )
            .await?;
            if !updated {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            }
            transaction.commit().await.map_err(store_err)?;
            continue;
        }

        // Reaping does not consume a scheduler slot. Do it before capacity
        // admission so an expired cancellation or exhausted worker cannot be
        // stranded indefinitely behind unrelated live work.
        if let Some(expired) = find_expired_lease_reaper_on(&transaction, now).await? {
            let (error_code, error_detail) =
                if expired.status == AgentRunStatus::Cancelling.as_str() {
                    (
                        "cancelled",
                        "sandbox agent lease expired while cancellation was pending",
                    )
                } else {
                    (
                        "lease_expired",
                        "sandbox agent exhausted its attempt budget after lease expiry",
                    )
                };
            let updated =
                fail_candidate_on(&transaction, &expired, now, error_code, error_detail).await?;
            if !updated {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            }
            transaction.commit().await.map_err(store_err)?;
            continue;
        }

        let live = entities::agent_run::Entity::find()
            .filter(entities::agent_run::Column::Tier.eq(AgentRunTier::Background.as_str()))
            .filter(entities::agent_run::Column::Id.in_subquery(admitted_child_id_subquery()))
            .filter(entities::agent_run::Column::Status.is_in([
                AgentRunStatus::Running.as_str(),
                AgentRunStatus::Cancelling.as_str(),
            ]))
            .filter(entities::agent_run::Column::LeaseExpiresAt.gt(now))
            .all(&transaction)
            .await
            .map_err(store_err)?;
        if live.len() >= max_running_global as usize {
            record_empty_claim_scan_on(&transaction, lease_token, now).await?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(None);
        }
        let mut live_by_chat = std::collections::HashMap::<uuid::Uuid, usize>::new();
        for run in live {
            *live_by_chat.entry(run.chat_id).or_default() += 1;
        }

        let Some(candidate) =
            find_claim_candidate_on(&transaction, now, max_running_per_chat, &live_by_chat).await?
        else {
            record_empty_claim_scan_on(&transaction, lease_token, now).await?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(None);
        };

        let continuing_from_tool = candidate.status == AgentRunStatus::RetryWait.as_str()
            && candidate.last_error_code.as_deref() == Some("tool_checkpoint_resolved");
        let attempt_count = if continuing_from_tool {
            candidate.attempt_count
        } else {
            candidate.attempt_count.checked_add(1).ok_or_else(|| {
                AgentError::Store(format!("agent run {} attempt count overflow", candidate.id))
            })?
        };
        let claim_count = candidate.claim_count.checked_add(1).ok_or_else(|| {
            AgentError::Store(format!("agent run {} claim count overflow", candidate.id))
        })?;
        if claim_count == i32::MAX {
            return Err(AgentError::Store(format!(
                "agent run {} claim count exhausted",
                candidate.id
            )));
        }
        let deadline_at = candidate
            .deadline_at
            .ok_or_else(|| AgentError::Store("sandbox agent run is missing its deadline".into()))?;
        // `ExprTrait` (in scope for query builders) also defines `min`, so name
        // the `Ord` comparison explicitly.
        let effective_lease_expires_at = Ord::min(deadline_at, lease_expires_at);
        entities::agent_run_claim::ActiveModel {
            token: Set(lease_token),
            agent_run_id: Set(Some(candidate.id)),
            attempt_count: Set(Some(attempt_count)),
            claim_count: Set(Some(claim_count)),
            claimed_at: Set(now),
            lease_expires_at: Set(Some(effective_lease_expires_at)),
        }
        .insert(&transaction)
        .await
        .map_err(store_err)?;

        let reclaiming = candidate.status == AgentRunStatus::Running.as_str();
        let claim = entities::agent_run::Entity::update_many()
            .col_expr(
                entities::agent_run::Column::Status,
                sea_orm::sea_query::Expr::value(AgentRunStatus::Running.as_str()),
            )
            .col_expr(
                entities::agent_run::Column::AttemptCount,
                sea_orm::sea_query::Expr::value(attempt_count),
            )
            .col_expr(
                entities::agent_run::Column::ClaimCount,
                sea_orm::sea_query::Expr::value(claim_count),
            )
            .col_expr(
                entities::agent_run::Column::LeaseToken,
                sea_orm::sea_query::Expr::value(Some(lease_token)),
            )
            .col_expr(
                entities::agent_run::Column::LeaseExpiresAt,
                sea_orm::sea_query::Expr::value(Some(effective_lease_expires_at)),
            )
            .col_expr(
                entities::agent_run::Column::StartedAt,
                sea_orm::sea_query::Expr::value(Some(candidate.started_at.unwrap_or(now))),
            )
            .col_expr(
                entities::agent_run::Column::LastErrorCode,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
            .col_expr(
                entities::agent_run::Column::LastErrorDetail,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
            .col_expr(
                entities::agent_run::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .filter(entities::agent_run::Column::Id.eq(candidate.id))
            .filter(entities::agent_run::Column::Status.eq(&candidate.status))
            .filter(entities::agent_run::Column::AttemptCount.eq(candidate.attempt_count))
            .filter(entities::agent_run::Column::ClaimCount.eq(candidate.claim_count))
            .filter(entities::agent_run::Column::UpdatedAt.eq(candidate.updated_at))
            .filter(entities::agent_run::Column::UpdatedAt.lte(now))
            .filter(entities::agent_run::Column::DeadlineAt.gt(now));
        let claim = if reclaiming {
            claim
                .filter(entities::agent_run::Column::LeaseToken.eq(candidate.lease_token))
                .filter(entities::agent_run::Column::LeaseExpiresAt.eq(candidate.lease_expires_at))
                .filter(entities::agent_run::Column::LeaseExpiresAt.lte(now))
        } else {
            claim
                .filter(entities::agent_run::Column::LeaseToken.is_null())
                .filter(entities::agent_run::Column::LeaseExpiresAt.is_null())
                .filter(entities::agent_run::Column::AvailableAt.lte(now))
        };
        let claimed = claim.exec(&transaction).await.map_err(store_err)?;
        if claimed.rows_affected != 1 {
            transaction.rollback().await.map_err(store_err)?;
            continue;
        }
        let claimed = find_by_id_on(&transaction, AgentRunId(candidate.id))
            .await?
            .ok_or_else(|| AgentError::Store("claimed agent run disappeared".into()))?;
        let claimed = agent_run_from_model(claimed)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(claimed));
    }
}

/// Claim one specific queued sandbox-resident container run by id under an exact
/// bounded lease, so the sandbox-resident driver can drive it and commit its
/// result through the same fenced result path as an in-process run.
///
/// Unlike [`claim_agent_run`] — which the in-process scheduler uses to select
/// the oldest due `in_process` run under global and per-chat concurrency limits
/// — this claims exactly the named `container` run, because the driver has
/// already decided which run it is provisioning a container for. Reusing
/// `lease_token` recovers only its original still-live claim (the same
/// ambiguous-commit recovery `claim_agent_run` gives) and never claims different
/// work. A container run has exactly one execution attempt, so this only
/// transitions a fresh `queued` run to `running`; it never reclaims an expired
/// lease into a second attempt.
pub(in crate::db) async fn claim_container_agent_run(
    store: &DbStore,
    id: AgentRunId,
    lease_token: uuid::Uuid,
    lease_duration: chrono::Duration,
) -> Result<Option<AgentRun>> {
    if lease_token.is_nil() || lease_duration <= chrono::Duration::zero() {
        return Err(AgentError::Store(
            "container agent-run claim requires a non-nil token and positive duration".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    let lease_expires_at = now.checked_add_signed(lease_duration).ok_or_else(|| {
        AgentError::Store("agent-run lease duration overflows the database timestamp range".into())
    })?;

    // Idempotent re-claim: a prior commit may have been lost after it wrote the
    // claim receipt. Recover only the exact still-live claim this token owns.
    if let Some(receipt) = entities::agent_run_claim::Entity::find_by_id(lease_token)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let Some(agent_run_id) = receipt.agent_run_id else {
            transaction.commit().await.map_err(store_err)?;
            return Ok(None);
        };
        let existing = find_by_id_on(&transaction, AgentRunId(agent_run_id))
            .await?
            .filter(|run| {
                run.tier == AgentRunTier::Background.as_str()
                    && run.execution_location == AgentRunExecutionLocation::Container.as_str()
                    && matches!(run.status.as_str(), "running" | "cancelling")
                    && Some(run.attempt_count) == receipt.attempt_count
                    && Some(run.claim_count) == receipt.claim_count
                    && run.lease_token == Some(lease_token)
                    && run.lease_expires_at.is_some_and(|expiry| expiry > now)
                    && run.deadline_at.is_some_and(|deadline| deadline > now)
            })
            .map(agent_run_from_model)
            .transpose()?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(existing);
    }

    let Some(candidate) = find_by_id_on(&transaction, id).await? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let admitted = entities::sandbox_agent_admission::Entity::find_by_id(candidate.id)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some();
    let claimable = admitted
        && candidate.tier == AgentRunTier::Background.as_str()
        && candidate.execution_location == AgentRunExecutionLocation::Container.as_str()
        && candidate.status == AgentRunStatus::Queued.as_str()
        && candidate.available_at <= now
        && candidate.deadline_at.is_some_and(|deadline| deadline > now)
        && candidate.updated_at <= now;
    if !claimable {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }

    let attempt_count = candidate.attempt_count.checked_add(1).ok_or_else(|| {
        AgentError::Store(format!("agent run {} attempt count overflow", candidate.id))
    })?;
    let claim_count = candidate.claim_count.checked_add(1).ok_or_else(|| {
        AgentError::Store(format!("agent run {} claim count overflow", candidate.id))
    })?;
    let deadline_at = candidate
        .deadline_at
        .ok_or_else(|| AgentError::Store("container agent run is missing its deadline".into()))?;
    let effective_lease_expires_at = Ord::min(deadline_at, lease_expires_at);
    entities::agent_run_claim::ActiveModel {
        token: Set(lease_token),
        agent_run_id: Set(Some(candidate.id)),
        attempt_count: Set(Some(attempt_count)),
        claim_count: Set(Some(claim_count)),
        claimed_at: Set(now),
        lease_expires_at: Set(Some(effective_lease_expires_at)),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;

    let claimed = entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(AgentRunStatus::Running.as_str()),
        )
        .col_expr(
            entities::agent_run::Column::AttemptCount,
            sea_orm::sea_query::Expr::value(attempt_count),
        )
        .col_expr(
            entities::agent_run::Column::ClaimCount,
            sea_orm::sea_query::Expr::value(claim_count),
        )
        .col_expr(
            entities::agent_run::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Some(lease_token)),
        )
        .col_expr(
            entities::agent_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Some(effective_lease_expires_at)),
        )
        .col_expr(
            entities::agent_run::Column::StartedAt,
            sea_orm::sea_query::Expr::value(Some(candidate.started_at.unwrap_or(now))),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorCode,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorDetail,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::agent_run::Column::Id.eq(candidate.id))
        .filter(entities::agent_run::Column::Status.eq(AgentRunStatus::Queued.as_str()))
        .filter(entities::agent_run::Column::AttemptCount.eq(candidate.attempt_count))
        .filter(entities::agent_run::Column::ClaimCount.eq(candidate.claim_count))
        .filter(entities::agent_run::Column::UpdatedAt.eq(candidate.updated_at))
        .filter(entities::agent_run::Column::LeaseToken.is_null())
        .filter(entities::agent_run::Column::LeaseExpiresAt.is_null())
        .filter(entities::agent_run::Column::AvailableAt.lte(now))
        .filter(entities::agent_run::Column::DeadlineAt.gt(now))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if claimed.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let claimed = find_by_id_on(&transaction, id)
        .await?
        .ok_or_else(|| AgentError::Store("claimed container agent run disappeared".into()))?;
    let claimed = agent_run_from_model(claimed)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(claimed))
}

/// List container-located runs whose driver died: `running` under an expired
/// lease with the deadline still open. The in-process lease reaper deliberately
/// exempts container runs (lease expiry there means "the driving host died",
/// not "the work died"), so this scan feeds the recovery pass that replaces it.
pub(in crate::db) async fn list_reclaimable_container_agent_runs(
    store: &DbStore,
    now: chrono::DateTime<Utc>,
) -> Result<Vec<AgentRun>> {
    entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::Tier.eq(AgentRunTier::Background.as_str()))
        .filter(
            entities::agent_run::Column::ExecutionLocation
                .eq(AgentRunExecutionLocation::Container.as_str()),
        )
        .filter(entities::agent_run::Column::Status.eq(AgentRunStatus::Running.as_str()))
        .filter(entities::agent_run::Column::LeaseExpiresAt.lte(now))
        .filter(entities::agent_run::Column::DeadlineAt.gt(now))
        .order_by_asc(entities::agent_run::Column::CreatedAt)
        .order_by_asc(entities::agent_run::Column::Id)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(agent_run_from_model)
        .collect()
}

/// Reclaim one expired-lease container run under a fresh bounded lease,
/// **without** a second execution attempt.
///
/// A container run has exactly one attempt — exactly one container was ever
/// asked to run it — so recovery re-drives that same attempt: the claim count
/// advances, the attempt count does not, and the drive that follows reconciles
/// the container through the durable provisioning record and the operation
/// log rather than executing anything twice. Refuses a live lease (another
/// driver still owns the run) and a crossed deadline (the claim scan fails
/// those, enqueueing the container's teardown).
pub(in crate::db) async fn reclaim_container_agent_run(
    store: &DbStore,
    id: AgentRunId,
    lease_token: uuid::Uuid,
    lease_duration: chrono::Duration,
) -> Result<Option<AgentRun>> {
    if lease_token.is_nil() || lease_duration <= chrono::Duration::zero() {
        return Err(AgentError::Store(
            "container agent-run reclaim requires a non-nil token and positive duration".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    let lease_expires_at = now.checked_add_signed(lease_duration).ok_or_else(|| {
        AgentError::Store("agent-run lease duration overflows the database timestamp range".into())
    })?;

    // Idempotent re-claim: recover only the exact still-live claim this token
    // owns, exactly as the fresh-claim path does.
    if let Some(receipt) = entities::agent_run_claim::Entity::find_by_id(lease_token)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let Some(agent_run_id) = receipt.agent_run_id else {
            transaction.commit().await.map_err(store_err)?;
            return Ok(None);
        };
        let existing = find_by_id_on(&transaction, AgentRunId(agent_run_id))
            .await?
            .filter(|run| {
                run.tier == AgentRunTier::Background.as_str()
                    && run.execution_location == AgentRunExecutionLocation::Container.as_str()
                    && matches!(run.status.as_str(), "running" | "cancelling")
                    && Some(run.attempt_count) == receipt.attempt_count
                    && Some(run.claim_count) == receipt.claim_count
                    && run.lease_token == Some(lease_token)
                    && run.lease_expires_at.is_some_and(|expiry| expiry > now)
                    && run.deadline_at.is_some_and(|deadline| deadline > now)
            })
            .map(agent_run_from_model)
            .transpose()?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(existing);
    }

    let Some(candidate) = find_by_id_on(&transaction, id).await? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let reclaimable = candidate.tier == AgentRunTier::Background.as_str()
        && candidate.execution_location == AgentRunExecutionLocation::Container.as_str()
        && candidate.status == AgentRunStatus::Running.as_str()
        && candidate
            .lease_expires_at
            .is_some_and(|expiry| expiry <= now)
        && candidate.deadline_at.is_some_and(|deadline| deadline > now)
        && candidate.updated_at <= now;
    if !reclaimable {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let claim_count = candidate.claim_count.checked_add(1).ok_or_else(|| {
        AgentError::Store(format!("agent run {} claim count overflow", candidate.id))
    })?;
    let deadline_at = candidate
        .deadline_at
        .ok_or_else(|| AgentError::Store("container agent run is missing its deadline".into()))?;
    let effective_lease_expires_at = Ord::min(deadline_at, lease_expires_at);
    entities::agent_run_claim::ActiveModel {
        token: Set(lease_token),
        agent_run_id: Set(Some(candidate.id)),
        attempt_count: Set(Some(candidate.attempt_count)),
        claim_count: Set(Some(claim_count)),
        claimed_at: Set(now),
        lease_expires_at: Set(Some(effective_lease_expires_at)),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;

    let reclaimed = entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::ClaimCount,
            sea_orm::sea_query::Expr::value(claim_count),
        )
        .col_expr(
            entities::agent_run::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Some(lease_token)),
        )
        .col_expr(
            entities::agent_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Some(effective_lease_expires_at)),
        )
        .col_expr(
            entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::agent_run::Column::Id.eq(candidate.id))
        .filter(entities::agent_run::Column::Status.eq(AgentRunStatus::Running.as_str()))
        .filter(entities::agent_run::Column::AttemptCount.eq(candidate.attempt_count))
        .filter(entities::agent_run::Column::ClaimCount.eq(candidate.claim_count))
        .filter(entities::agent_run::Column::UpdatedAt.eq(candidate.updated_at))
        .filter(entities::agent_run::Column::LeaseToken.eq(candidate.lease_token))
        .filter(entities::agent_run::Column::LeaseExpiresAt.lte(now))
        .filter(entities::agent_run::Column::DeadlineAt.gt(now))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if reclaimed.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let reclaimed = find_by_id_on(&transaction, id)
        .await?
        .ok_or_else(|| AgentError::Store("reclaimed container agent run disappeared".into()))?;
    let reclaimed = agent_run_from_model(reclaimed)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(reclaimed))
}

async fn record_empty_claim_scan_on<C>(
    conn: &C,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    entities::agent_run_claim::ActiveModel {
        token: Set(lease_token),
        agent_run_id: Set(None),
        attempt_count: Set(None),
        claim_count: Set(None),
        claimed_at: Set(now),
        lease_expires_at: Set(None),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

pub(in crate::db) async fn heartbeat_agent_run(
    store: &DbStore,
    id: AgentRunId,
    lease_token: uuid::Uuid,
    lease_duration: chrono::Duration,
) -> Result<bool> {
    if lease_token.is_nil() || lease_duration <= chrono::Duration::zero() {
        return Err(AgentError::Store(
            "agent-run heartbeat requires a non-nil token and positive duration".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    // Serialize lease extension with claim, cancellation, and terminal
    // resolution. Otherwise a racing heartbeat could make a one-shot durable
    // cancellation request lose its compare-and-swap.
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    let lease_expires_at = now.checked_add_signed(lease_duration).ok_or_else(|| {
        AgentError::Store("agent-run lease duration overflows the database timestamp range".into())
    })?;
    let heartbeat = entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Some(lease_expires_at)),
        )
        .col_expr(
            entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::agent_run::Column::Id.eq(id.0))
        .filter(entities::agent_run::Column::Tier.eq(AgentRunTier::Background.as_str()))
        .filter(entities::agent_run::Column::Status.eq(AgentRunStatus::Running.as_str()))
        .filter(entities::agent_run::Column::LeaseToken.eq(lease_token))
        .filter(entities::agent_run::Column::LeaseExpiresAt.gt(now))
        .filter(entities::agent_run::Column::LeaseExpiresAt.lt(lease_expires_at))
        .filter(entities::agent_run::Column::DeadlineAt.gt(now))
        .filter(entities::agent_run::Column::DeadlineAt.gte(lease_expires_at))
        .filter(entities::agent_run::Column::UpdatedAt.lte(now))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(heartbeat.rows_affected == 1)
}

/// Resolve an exact live sandbox lease after a provider/executor failure.
///
/// Intermediate failures release only the current lease and remain replay-safe.
/// The final attempt also writes the same immutable parent inbox receipt used
/// by successful completion, but transitions the child to `failed`.
pub(in crate::db) async fn fail_agent_run(
    store: &DbStore,
    id: AgentRunId,
    lease_token: uuid::Uuid,
    error_code: &str,
    error_detail: &str,
    retry_delay: chrono::Duration,
) -> Result<Option<FailAgentRunOutcome>> {
    if lease_token.is_nil()
        || error_code.is_empty()
        || error_code.chars().count() > AgentRun::MAX_ERROR_CODE_LEN
        || error_detail.chars().count() > AgentRun::MAX_ERROR_DETAIL_LEN
        || retry_delay <= chrono::Duration::zero()
    {
        return Err(AgentError::Store(
            "invalid sandbox agent failure resolution".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    let Some(run) = find_by_id_on(&transaction, id).await? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    // A completed final receipt lets an ambiguous retry recover exactly; a
    // later different terminal outcome never overwrites it.
    if let Some(result) = entities::agent_run_result::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let result = agent_run_result_from_model(result)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok((result.lease_token == lease_token
            && run.status == AgentRunStatus::Failed.as_str())
        .then_some(FailAgentRunOutcome::ExistingFailed(result)));
    }
    let valid = run.tier == AgentRunTier::Background.as_str()
        && run.status == AgentRunStatus::Running.as_str()
        && run.lease_token == Some(lease_token)
        && run.lease_expires_at.is_some_and(|expiry| expiry > now)
        && run.deadline_at.is_some_and(|deadline| deadline > now)
        && run.updated_at <= now;
    if !valid {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let terminal = run.attempt_count >= run.max_attempts;
    let available_at = now.checked_add_signed(retry_delay).ok_or_else(|| {
        AgentError::Store("agent-run retry delay overflows timestamp range".into())
    })?;
    let status = if terminal {
        AgentRunStatus::Failed
    } else {
        AgentRunStatus::RetryWait
    };
    let mut update = entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(status.as_str()),
        )
        .col_expr(
            entities::agent_run::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::agent_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorCode,
            sea_orm::sea_query::Expr::value(Some(error_code.to_owned())),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorDetail,
            sea_orm::sea_query::Expr::value(Some(error_detail.to_owned())),
        )
        .col_expr(
            entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::agent_run::Column::Id.eq(id.0))
        .filter(entities::agent_run::Column::Status.eq(AgentRunStatus::Running.as_str()))
        .filter(entities::agent_run::Column::LeaseToken.eq(lease_token))
        .filter(entities::agent_run::Column::LeaseExpiresAt.eq(run.lease_expires_at))
        .filter(entities::agent_run::Column::LeaseExpiresAt.gt(now))
        .filter(entities::agent_run::Column::DeadlineAt.gt(now))
        .filter(entities::agent_run::Column::UpdatedAt.eq(run.updated_at));
    if terminal {
        update = update.col_expr(
            entities::agent_run::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        );
    } else {
        update = update.col_expr(
            entities::agent_run::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(available_at),
        );
    }
    if update
        .exec(&transaction)
        .await
        .map_err(store_err)?
        .rows_affected
        != 1
    {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let updated = find_by_id_on(&transaction, id)
        .await?
        .expect("updated agent run exists");
    if !terminal {
        let updated = agent_run_from_model(updated)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(FailAgentRunOutcome::RetryScheduled(updated)));
    }
    let Some(parent_id) = run.parent_id.map(AgentRunId) else {
        return Err(AgentError::Store("sandbox run missing parent".into()));
    };
    let text = format!("Sandbox task failed ({error_code}): {error_detail}");
    let payload = AgentRunResultPayload::FinalText { text: text.clone() };
    entities::agent_run_result::ActiveModel {
        agent_run_id: Set(id.0),
        lease_token: Set(lease_token),
        attempt_count: Set(run.attempt_count),
        claim_count: Set(run.claim_count),
        payload_kind: Set(agent_run_result_payload_kind(&payload).into()),
        payload_json: Set(agent_run_result_payload_json(&payload)?),
        text: Set(text),
        submitted_at: Set(now),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    entities::agent_run_inbox::ActiveModel {
        child_run_id: Set(id.0),
        parent_run_id: Set(parent_id.0),
        chat_id: Set(run.chat_id),
        parent_depth: Set(0),
        result_lease_token: Set(lease_token),
        result_attempt_count: Set(run.attempt_count),
        result_claim_count: Set(run.claim_count),
        status: Set(AgentRunInboxStatus::Pending.as_str().into()),
        claim_count: Set(0),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        consumed_lease_token: Set(None),
        consumed_at: Set(None),
        delivered_at: Set(now),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    let result = entities::agent_run_result::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("inserted result exists");
    // A container run that just failed terminally owes its container a
    // teardown in the same transaction, so the sweep can never observe a
    // failed run whose provisioning record still calls the container live.
    if run.execution_location == AgentRunExecutionLocation::Container.as_str() {
        super::sandbox_provision::enqueue_teardown_on(&transaction, id.0).await?;
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(FailAgentRunOutcome::Failed(
        agent_run_result_from_model(result)?,
    )))
}

pub(in crate::db) async fn submit_agent_run_result(
    store: &DbStore,
    id: AgentRunId,
    lease_token: uuid::Uuid,
    text: &str,
) -> Result<Option<SubmitAgentRunResultOutcome>> {
    submit_agent_run_result_payload(
        store,
        id,
        lease_token,
        AgentRunResultPayload::FinalText {
            text: text.to_owned(),
        },
    )
    .await
}

/// Submit the one typed folder-consent proposal a sandbox may return to its
/// foreground parent. This is a terminal child result, never a client call.
pub(in crate::db) async fn submit_agent_run_folder_access_proposal(
    store: &DbStore,
    id: AgentRunId,
    lease_token: uuid::Uuid,
    request: &RequestFolderAccessArgs,
) -> Result<Option<SubmitAgentRunResultOutcome>> {
    submit_agent_run_result_payload(
        store,
        id,
        lease_token,
        AgentRunResultPayload::FolderAccessProposal {
            request: request.clone(),
        },
    )
    .await
}

async fn submit_agent_run_result_payload(
    store: &DbStore,
    id: AgentRunId,
    lease_token: uuid::Uuid,
    payload: AgentRunResultPayload,
) -> Result<Option<SubmitAgentRunResultOutcome>> {
    validate_agent_run_result_payload(&payload)?;
    let text = agent_run_result_display_text(&payload);
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    if let Some(result) = entities::agent_run_result::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let result = agent_run_result_from_model(result)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(
            (result.lease_token == lease_token && result.payload == payload)
                .then_some(SubmitAgentRunResultOutcome::Existing(result)),
        );
    }
    let Some(run) = find_by_id_on(&transaction, id).await? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if run.tier != AgentRunTier::Background.as_str()
        || run.depth != 1
        || run.status != AgentRunStatus::Running.as_str()
        || run.lease_token != Some(lease_token)
        || run.lease_expires_at.is_none_or(|expiry| expiry <= now)
        || run.deadline_at.is_none_or(|deadline| deadline <= now)
        || run.updated_at > now
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(parent_id) = run.parent_id.map(AgentRunId) else {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "sandbox agent run {id} is missing its parent"
        )));
    };
    let Some(parent) = find_by_id_on(&transaction, parent_id).await? else {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "sandbox agent run {id} references missing parent {parent_id}"
        )));
    };
    if parent.chat_id != run.chat_id
        || parent.depth != 0
        || parent.tier != AgentRunTier::Foreground.as_str()
        || parent.status != AgentRunStatus::Active.as_str()
    {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    entities::agent_run_result::ActiveModel {
        agent_run_id: Set(id.0),
        lease_token: Set(lease_token),
        attempt_count: Set(run.attempt_count),
        claim_count: Set(run.claim_count),
        payload_kind: Set(agent_run_result_payload_kind(&payload).into()),
        payload_json: Set(agent_run_result_payload_json(&payload)?),
        text: Set(text),
        submitted_at: Set(now),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    entities::agent_run_inbox::ActiveModel {
        child_run_id: Set(id.0),
        parent_run_id: Set(parent_id.0),
        chat_id: Set(run.chat_id),
        parent_depth: Set(0),
        result_lease_token: Set(lease_token),
        result_attempt_count: Set(run.attempt_count),
        result_claim_count: Set(run.claim_count),
        status: Set(AgentRunInboxStatus::Pending.as_str().into()),
        claim_count: Set(0),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        consumed_lease_token: Set(None),
        consumed_at: Set(None),
        delivered_at: Set(now),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    let updated = entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(AgentRunStatus::Completed.as_str()),
        )
        .col_expr(
            entities::agent_run::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::agent_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::agent_run::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorCode,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorDetail,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::agent_run::Column::Id.eq(id.0))
        .filter(entities::agent_run::Column::Status.eq(AgentRunStatus::Running.as_str()))
        .filter(entities::agent_run::Column::AttemptCount.eq(run.attempt_count))
        .filter(entities::agent_run::Column::ClaimCount.eq(run.claim_count))
        .filter(entities::agent_run::Column::LeaseToken.eq(lease_token))
        .filter(entities::agent_run::Column::LeaseExpiresAt.eq(run.lease_expires_at))
        .filter(entities::agent_run::Column::LeaseExpiresAt.gt(now))
        .filter(entities::agent_run::Column::DeadlineAt.gt(now))
        .filter(entities::agent_run::Column::UpdatedAt.eq(run.updated_at))
        .filter(entities::agent_run::Column::UpdatedAt.lte(now))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let result = entities::agent_run_result::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("completed agent run {id} is missing its result")))
        .and_then(agent_run_result_from_model)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(SubmitAgentRunResultOutcome::Completed(result)))
}

pub(in crate::db) async fn list_agent_run_inbox(
    store: &DbStore,
    parent_run_id: AgentRunId,
) -> Result<Vec<AgentRunInboxEntry>> {
    let entries = entities::agent_run_inbox::Entity::find()
        .filter(entities::agent_run_inbox::Column::ParentRunId.eq(parent_run_id.0))
        .order_by_asc(entities::agent_run_inbox::Column::DeliveredAt)
        .order_by_asc(entities::agent_run_inbox::Column::ChildRunId)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let mut inbox = Vec::with_capacity(entries.len());
    for entry in entries {
        inbox.push(load_agent_run_inbox_entry_on(&store.conn, entry).await?);
    }
    Ok(inbox)
}

/// Find deliveries that need a foreground continuation attempt.
///
/// Selection is deliberately only a bounded recovery hint. The exact claim
/// below serializes ownership against every other worker and rechecks the
/// parent checkpoint with the database clock.
pub(in crate::db) async fn list_agent_run_inbox_candidates(
    store: &DbStore,
    limit: u64,
) -> Result<Vec<AgentRunInboxEntry>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let now = database_now(&store.conn).await?;
    // Do not let an old result for a completed or cancelled parent consume the
    // bounded recovery scan.  The worker still rechecks this relationship when
    // claiming, but filtering it here gives a live parked turn a fair chance to
    // run even if historical inbox receipts remain in the database.
    let waiting_checkpoint = sea_orm::sea_query::Query::select()
        .expr(sea_orm::sea_query::Expr::value(1))
        .from(entities::turn_agent_run_wait::Entity)
        .inner_join(
            entities::turn_run::Entity,
            sea_orm::sea_query::Expr::col((
                entities::turn_agent_run_wait::Entity,
                entities::turn_agent_run_wait::Column::TurnId,
            ))
            .equals((entities::turn_run::Entity, entities::turn_run::Column::Id)),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_agent_run_wait::Entity,
                entities::turn_agent_run_wait::Column::Status,
            ))
            .eq(crate::model::TurnAgentRunWaitStatus::Waiting.as_str()),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_agent_run_wait::Entity,
                entities::turn_agent_run_wait::Column::ClosedAt,
            ))
            .is_null(),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_run::Entity,
                entities::turn_run::Column::Status,
            ))
            .eq(crate::model::TurnRunStatus::WaitingForAgentRun.as_str()),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_agent_run_wait::Entity,
                entities::turn_agent_run_wait::Column::ChildRunId,
            ))
            .equals((
                entities::agent_run_inbox::Entity,
                entities::agent_run_inbox::Column::ChildRunId,
            )),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_agent_run_wait::Entity,
                entities::turn_agent_run_wait::Column::ParentRunId,
            ))
            .equals((
                entities::agent_run_inbox::Entity,
                entities::agent_run_inbox::Column::ParentRunId,
            )),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_agent_run_wait::Entity,
                entities::turn_agent_run_wait::Column::ChatId,
            ))
            .equals((
                entities::agent_run_inbox::Entity,
                entities::agent_run_inbox::Column::ChatId,
            )),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_run::Entity,
                entities::turn_run::Column::ChatId,
            ))
            .equals((
                entities::turn_agent_run_wait::Entity,
                entities::turn_agent_run_wait::Column::ChatId,
            )),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_run::Entity,
                entities::turn_run::Column::AgentRunId,
            ))
            .equals((
                entities::turn_agent_run_wait::Entity,
                entities::turn_agent_run_wait::Column::ParentRunId,
            )),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_run::Entity,
                entities::turn_run::Column::AttemptCount,
            ))
            .equals((
                entities::turn_agent_run_wait::Entity,
                entities::turn_agent_run_wait::Column::AttemptCount,
            )),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_run::Entity,
                entities::turn_run::Column::ClaimCount,
            ))
            .equals((
                entities::turn_agent_run_wait::Entity,
                entities::turn_agent_run_wait::Column::ClaimCount,
            )),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_run::Entity,
                entities::turn_run::Column::LeaseToken,
            ))
            .is_null(),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_run::Entity,
                entities::turn_run::Column::LeaseExpiresAt,
            ))
            .is_null(),
        )
        .to_owned();
    let active_parents = sea_orm::sea_query::Query::select()
        .column((entities::agent_run::Entity, entities::agent_run::Column::Id))
        .from(entities::agent_run::Entity)
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::agent_run::Entity,
                entities::agent_run::Column::Status,
            ))
            .eq(AgentRunStatus::Active.as_str()),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::agent_run::Entity,
                entities::agent_run::Column::Tier,
            ))
            .eq(AgentRunTier::Foreground.as_str()),
        )
        .to_owned();
    let entries = entities::agent_run_inbox::Entity::find()
        .filter(
            Condition::any()
                .add(
                    entities::agent_run_inbox::Column::Status
                        .eq(AgentRunInboxStatus::Pending.as_str()),
                )
                .add(
                    Condition::all()
                        .add(
                            entities::agent_run_inbox::Column::Status
                                .eq(AgentRunInboxStatus::Claimed.as_str()),
                        )
                        .add(entities::agent_run_inbox::Column::LeaseExpiresAt.lte(now)),
                ),
        )
        .filter(sea_orm::sea_query::Expr::exists(waiting_checkpoint))
        .filter(entities::agent_run_inbox::Column::ParentRunId.in_subquery(active_parents))
        .order_by_asc(entities::agent_run_inbox::Column::DeliveredAt)
        .order_by_asc(entities::agent_run_inbox::Column::ChildRunId)
        .limit(limit)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let mut inbox = Vec::with_capacity(entries.len());
    for entry in entries {
        inbox.push(load_agent_run_inbox_entry_on(&store.conn, entry).await?);
    }
    Ok(inbox)
}

/// Claim one exact immutable delivery as the next parent continuation boundary.
///
/// The shared scheduler lock deliberately serializes this with sandbox claims:
/// a continuation must observe one database-clock ordering, and an expired
/// continuation owner must never race a replacement owner into consumption.
pub(in crate::db) async fn claim_agent_run_inbox_entry(
    store: &DbStore,
    parent_run_id: AgentRunId,
    child_run_id: AgentRunId,
    lease_token: uuid::Uuid,
    lease_duration: chrono::Duration,
) -> Result<Option<ClaimAgentRunInboxOutcome>> {
    validate_inbox_claim_request(lease_token, lease_duration)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    let lease_expires_at = now.checked_add_signed(lease_duration).ok_or_else(|| {
        AgentError::Store(
            "agent-run inbox lease duration overflows the database timestamp range".into(),
        )
    })?;
    if !parent_is_active_on(&transaction, parent_run_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(model) = find_agent_run_inbox_on(&transaction, parent_run_id, child_run_id).await?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    // A completed child may arrive before its foreground worker reaches the
    // checkpoint boundary. Keep that delivery pending until the exact waiting
    // turn exists: otherwise a continuation lease could expire or be consumed
    // before there is a durable parent state to wake.
    if !parent_turn_checkpoint_is_waiting_on(&transaction, parent_run_id, child_run_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let entry = load_agent_run_inbox_entry_on(&transaction, model.clone()).await?;
    match entry.status {
        AgentRunInboxStatus::Pending => {
            let updated = entities::agent_run_inbox::Entity::update_many()
                .col_expr(
                    entities::agent_run_inbox::Column::Status,
                    sea_orm::sea_query::Expr::value(AgentRunInboxStatus::Claimed.as_str()),
                )
                .col_expr(
                    entities::agent_run_inbox::Column::ClaimCount,
                    sea_orm::sea_query::Expr::value(1),
                )
                .col_expr(
                    entities::agent_run_inbox::Column::LeaseToken,
                    sea_orm::sea_query::Expr::value(Some(lease_token)),
                )
                .col_expr(
                    entities::agent_run_inbox::Column::LeaseExpiresAt,
                    sea_orm::sea_query::Expr::value(Some(lease_expires_at)),
                )
                .filter(entities::agent_run_inbox::Column::ChildRunId.eq(child_run_id.0))
                .filter(entities::agent_run_inbox::Column::ParentRunId.eq(parent_run_id.0))
                .filter(
                    entities::agent_run_inbox::Column::Status
                        .eq(AgentRunInboxStatus::Pending.as_str()),
                )
                .filter(entities::agent_run_inbox::Column::ClaimCount.eq(0))
                .exec(&transaction)
                .await
                .map_err(store_err)?;
            if updated.rows_affected != 1 {
                transaction.rollback().await.map_err(store_err)?;
                return Ok(None);
            }
            let claimed = load_agent_run_inbox_by_ids_on(&transaction, parent_run_id, child_run_id)
                .await?
                .ok_or_else(|| {
                    AgentError::Store("claimed agent-run inbox entry disappeared".into())
                })?;
            transaction.commit().await.map_err(store_err)?;
            Ok(Some(ClaimAgentRunInboxOutcome::Claimed(claimed)))
        }
        AgentRunInboxStatus::Claimed
            if entry.lease_token == Some(lease_token)
                && entry
                    .lease_expires_at
                    .is_some_and(|expires_at| expires_at > now) =>
        {
            transaction.commit().await.map_err(store_err)?;
            Ok(Some(ClaimAgentRunInboxOutcome::Existing(entry)))
        }
        AgentRunInboxStatus::Claimed
            if entry
                .lease_expires_at
                .is_some_and(|expires_at| expires_at <= now) =>
        {
            // A lease token is a capability for one continuation attempt, not
            // a retry identity. Once it expires, its former owner must not
            // revive itself into a new attempt; only a fresh token may reclaim
            // the immutable delivery.
            if entry.lease_token == Some(lease_token) {
                transaction.commit().await.map_err(store_err)?;
                return Ok(None);
            }
            let next_claim_count = entry
                .claim_count
                .checked_add(1)
                .ok_or_else(|| AgentError::Store("agent-run inbox claim count exhausted".into()))?;
            let updated = entities::agent_run_inbox::Entity::update_many()
                .col_expr(
                    entities::agent_run_inbox::Column::ClaimCount,
                    sea_orm::sea_query::Expr::value(next_claim_count),
                )
                .col_expr(
                    entities::agent_run_inbox::Column::LeaseToken,
                    sea_orm::sea_query::Expr::value(Some(lease_token)),
                )
                .col_expr(
                    entities::agent_run_inbox::Column::LeaseExpiresAt,
                    sea_orm::sea_query::Expr::value(Some(lease_expires_at)),
                )
                .filter(entities::agent_run_inbox::Column::ChildRunId.eq(child_run_id.0))
                .filter(entities::agent_run_inbox::Column::ParentRunId.eq(parent_run_id.0))
                .filter(
                    entities::agent_run_inbox::Column::Status
                        .eq(AgentRunInboxStatus::Claimed.as_str()),
                )
                .filter(entities::agent_run_inbox::Column::ClaimCount.eq(entry.claim_count))
                .filter(entities::agent_run_inbox::Column::LeaseToken.eq(entry.lease_token))
                .filter(
                    entities::agent_run_inbox::Column::LeaseExpiresAt.eq(entry.lease_expires_at),
                )
                .filter(entities::agent_run_inbox::Column::LeaseExpiresAt.lte(now))
                .exec(&transaction)
                .await
                .map_err(store_err)?;
            if updated.rows_affected != 1 {
                transaction.rollback().await.map_err(store_err)?;
                return Ok(None);
            }
            let claimed = load_agent_run_inbox_by_ids_on(&transaction, parent_run_id, child_run_id)
                .await?
                .ok_or_else(|| {
                    AgentError::Store("reclaimed agent-run inbox entry disappeared".into())
                })?;
            transaction.commit().await.map_err(store_err)?;
            Ok(Some(ClaimAgentRunInboxOutcome::Claimed(claimed)))
        }
        AgentRunInboxStatus::Claimed
        | AgentRunInboxStatus::Consumed
        | AgentRunInboxStatus::Cancelled => {
            transaction.commit().await.map_err(store_err)?;
            Ok(None)
        }
    }
}

/// Mark one exact inbox delivery consumed under its active continuation lease.
///
/// Consumption is the commit point for a future parent resume transition. The
/// resulting receipt is retained so a worker can recover an ambiguous commit
/// without re-consuming the child result.
pub(in crate::db) async fn consume_agent_run_inbox_entry(
    store: &DbStore,
    parent_run_id: AgentRunId,
    child_run_id: AgentRunId,
    lease_token: uuid::Uuid,
) -> Result<Option<ConsumeAgentRunInboxOutcome>> {
    Ok(consume_agent_run_inbox_entry_and_resume_turn(
        store,
        parent_run_id,
        child_run_id,
        lease_token,
    )
    .await?
    .map(|outcome| match outcome {
        ConsumeAgentRunInboxAndResumeTurnOutcome::Resumed { inbox, .. } => {
            ConsumeAgentRunInboxOutcome::Consumed(inbox)
        }
        ConsumeAgentRunInboxAndResumeTurnOutcome::Existing { inbox, .. } => {
            ConsumeAgentRunInboxOutcome::Existing(inbox)
        }
    }))
}

/// Consume one exact child result and queue its foreground turn to resume in
/// the same transaction. The turn's durable `resuming` state is the wake
/// signal; callers can restart after commit and use ordinary turn claiming.
pub(in crate::db) async fn consume_agent_run_inbox_entry_and_resume_turn(
    store: &DbStore,
    parent_run_id: AgentRunId,
    child_run_id: AgentRunId,
    lease_token: uuid::Uuid,
) -> Result<Option<ConsumeAgentRunInboxAndResumeTurnOutcome>> {
    if lease_token.is_nil() {
        return Err(AgentError::Store(
            "agent-run inbox consumption requires a non-nil lease token".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    if !parent_is_active_on(&transaction, parent_run_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(model) = find_agent_run_inbox_on(&transaction, parent_run_id, child_run_id).await?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let entry = load_agent_run_inbox_entry_on(&transaction, model).await?;
    if entry.status == AgentRunInboxStatus::Consumed {
        let turn = if entry.consumed_lease_token == Some(lease_token) {
            super::turn::resume_turn_after_agent_run_inbox_consumption_on(
                &transaction,
                parent_run_id,
                child_run_id,
                now,
            )
            .await?
        } else {
            None
        };
        if let Some(turn) = turn.as_ref() {
            ensure_sandbox_result_message_on(&transaction, &entry, turn, now, false).await?;
        }
        transaction.commit().await.map_err(store_err)?;
        return Ok(
            turn.map(|turn| ConsumeAgentRunInboxAndResumeTurnOutcome::Existing {
                inbox: entry,
                turn,
            }),
        );
    }
    if entry.status != AgentRunInboxStatus::Claimed
        || entry.lease_token != Some(lease_token)
        || entry
            .lease_expires_at
            .is_none_or(|expires_at| expires_at <= now)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(turn) = super::turn::resume_turn_after_agent_run_inbox_consumption_on(
        &transaction,
        parent_run_id,
        child_run_id,
        now,
    )
    .await?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    ensure_sandbox_result_message_on(&transaction, &entry, &turn, now, true).await?;
    let updated = entities::agent_run_inbox::Entity::update_many()
        .col_expr(
            entities::agent_run_inbox::Column::Status,
            sea_orm::sea_query::Expr::value(AgentRunInboxStatus::Consumed.as_str()),
        )
        .col_expr(
            entities::agent_run_inbox::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::agent_run_inbox::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::agent_run_inbox::Column::ConsumedLeaseToken,
            sea_orm::sea_query::Expr::value(Some(lease_token)),
        )
        .col_expr(
            entities::agent_run_inbox::Column::ConsumedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .filter(entities::agent_run_inbox::Column::ChildRunId.eq(child_run_id.0))
        .filter(entities::agent_run_inbox::Column::ParentRunId.eq(parent_run_id.0))
        .filter(entities::agent_run_inbox::Column::Status.eq(AgentRunInboxStatus::Claimed.as_str()))
        .filter(entities::agent_run_inbox::Column::LeaseToken.eq(lease_token))
        .filter(entities::agent_run_inbox::Column::LeaseExpiresAt.eq(entry.lease_expires_at))
        .filter(entities::agent_run_inbox::Column::LeaseExpiresAt.gt(now))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let inbox = load_agent_run_inbox_by_ids_on(&transaction, parent_run_id, child_run_id)
        .await?
        .ok_or_else(|| AgentError::Store("consumed agent-run inbox entry disappeared".into()))?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(ConsumeAgentRunInboxAndResumeTurnOutcome::Resumed {
        inbox,
        turn,
    }))
}

fn validate_claim_request(
    lease_token: uuid::Uuid,
    lease_duration: chrono::Duration,
    max_running_global: u32,
    max_running_per_chat: u32,
) -> Result<()> {
    if lease_token.is_nil() || lease_duration <= chrono::Duration::zero() {
        return Err(AgentError::Store(
            "agent-run claim requires a non-nil token and positive duration".into(),
        ));
    }
    if !(1..=AgentRun::MAX_CONCURRENCY_LIMIT).contains(&max_running_global)
        || !(1..=AgentRun::MAX_CONCURRENCY_LIMIT).contains(&max_running_per_chat)
        || max_running_per_chat > max_running_global
    {
        return Err(AgentError::Store(format!(
            "agent-run concurrency limits must satisfy 1 <= per-chat <= global <= {}",
            AgentRun::MAX_CONCURRENCY_LIMIT
        )));
    }
    Ok(())
}

fn validate_inbox_claim_request(
    lease_token: uuid::Uuid,
    lease_duration: chrono::Duration,
) -> Result<()> {
    if lease_token.is_nil() || lease_duration <= chrono::Duration::zero() {
        return Err(AgentError::Store(
            "agent-run inbox claim requires a non-nil token and positive duration".into(),
        ));
    }
    Ok(())
}

pub(in crate::db) async fn database_now<C>(conn: &C) -> Result<chrono::DateTime<Utc>>
where
    C: sea_orm::ConnectionTrait,
{
    let backend = conn.get_database_backend();
    // PostgreSQL's CURRENT_TIMESTAMP is fixed at transaction start. Claimers
    // can wait on a lock, so use statement-time clocks. SQLite's bare
    // CURRENT_TIMESTAMP loses fractional seconds; use the end of its current
    // clock millisecond so application-authored microsecond timestamps from
    // that same millisecond never appear to come from the future.
    let clock_sql = match backend {
        DatabaseBackend::Postgres => "SELECT clock_timestamp() AS now",
        DatabaseBackend::Sqlite => "SELECT (strftime('%Y-%m-%dT%H:%M:%f', 'now') || '999Z') AS now",
        _ => "SELECT CURRENT_TIMESTAMP AS now",
    };
    let statement = Statement::from_string(backend, clock_sql);
    // sea-orm 2.0: `query_one` takes a `StatementBuilder`; a prepared raw
    // `Statement` goes through `query_one_raw`.
    let row = conn
        .query_one_raw(statement)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("database clock query returned no row".into()))?;
    let now = row
        .try_get::<chrono::DateTime<Utc>>("", "now")
        .map_err(store_err)?;
    canonical_db_timestamp(now)
}

pub(in crate::db) async fn acquire_agent_run_claim_lock<C>(conn: &C) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    let locked = entities::agent_run_claim_lock::Entity::update_many()
        .col_expr(
            entities::agent_run_claim_lock::Column::Id,
            sea_orm::sea_query::Expr::col(entities::agent_run_claim_lock::Column::Id),
        )
        .filter(entities::agent_run_claim_lock::Column::Id.eq(1))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if locked.rows_affected != 1 {
        return Err(AgentError::Store(
            "durable agent-run claim lock is missing".into(),
        ));
    }
    Ok(())
}

async fn find_expired_deadline_on<C>(
    conn: &C,
    now: chrono::DateTime<Utc>,
) -> Result<Option<entities::agent_run::Model>>
where
    C: sea_orm::ConnectionTrait,
{
    entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::Tier.eq(AgentRunTier::Background.as_str()))
        .filter(entities::agent_run::Column::Id.in_subquery(admitted_child_id_subquery()))
        .filter(entities::agent_run::Column::Status.is_in([
            AgentRunStatus::Queued.as_str(),
            AgentRunStatus::Running.as_str(),
            AgentRunStatus::Cancelling.as_str(),
            AgentRunStatus::Waiting.as_str(),
            AgentRunStatus::RetryWait.as_str(),
        ]))
        .filter(entities::agent_run::Column::DeadlineAt.lte(now))
        .filter(entities::agent_run::Column::UpdatedAt.lte(now))
        .order_by_asc(entities::agent_run::Column::DeadlineAt)
        .order_by_asc(entities::agent_run::Column::CreatedAt)
        .order_by_asc(entities::agent_run::Column::Id)
        .one(conn)
        .await
        .map_err(store_err)
}

async fn find_claim_candidate_on<C>(
    conn: &C,
    now: chrono::DateTime<Utc>,
    max_running_per_chat: u32,
    live_by_chat: &std::collections::HashMap<uuid::Uuid, usize>,
) -> Result<Option<entities::agent_run::Model>>
where
    C: sea_orm::ConnectionTrait,
{
    const PAGE_SIZE: u64 = 64;
    let mut offset = 0;
    loop {
        let page = entities::agent_run::Entity::find()
            .filter(entities::agent_run::Column::Tier.eq(AgentRunTier::Background.as_str()))
            // The in-process scheduler only advances runs that execute in
            // process; a run resident in a provider boundary has its own
            // lifecycle machinery.
            .filter(
                entities::agent_run::Column::ExecutionLocation
                    .eq(AgentRunExecutionLocation::InProcess.as_str()),
            )
            .filter(entities::agent_run::Column::Id.in_subquery(admitted_child_id_subquery()))
            .filter(
                sea_orm::Condition::any()
                    .add(
                        sea_orm::Condition::all()
                            .add(entities::agent_run::Column::Status.is_in([
                                AgentRunStatus::Queued.as_str(),
                                AgentRunStatus::RetryWait.as_str(),
                            ]))
                            .add(entities::agent_run::Column::AvailableAt.lte(now)),
                    )
                    .add(
                        sea_orm::Condition::all()
                            .add(
                                entities::agent_run::Column::Status
                                    .is_in([AgentRunStatus::Running.as_str()]),
                            )
                            .add(entities::agent_run::Column::LeaseExpiresAt.lte(now)),
                    ),
            )
            .filter(entities::agent_run::Column::UpdatedAt.lte(now))
            .filter(entities::agent_run::Column::DeadlineAt.gt(now))
            .order_by_asc(entities::agent_run::Column::AvailableAt)
            .order_by_asc(entities::agent_run::Column::CreatedAt)
            .order_by_asc(entities::agent_run::Column::Id)
            .offset(offset)
            .limit(PAGE_SIZE)
            .all(conn)
            .await
            .map_err(store_err)?;
        if page.is_empty() {
            return Ok(None);
        }
        let page_len = page.len();
        if let Some(candidate) = page.into_iter().find(|candidate| {
            live_by_chat.get(&candidate.chat_id).copied().unwrap_or(0)
                < max_running_per_chat as usize
        }) {
            return Ok(Some(candidate));
        }
        if page_len < PAGE_SIZE as usize {
            return Ok(None);
        }
        offset += PAGE_SIZE;
    }
}

async fn find_expired_lease_reaper_on<C>(
    conn: &C,
    now: chrono::DateTime<Utc>,
) -> Result<Option<entities::agent_run::Model>>
where
    C: sea_orm::ConnectionTrait,
{
    entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::Tier.eq(AgentRunTier::Background.as_str()))
        // Lease expiry means "the in-process worker that held this run died".
        // A sandbox-resident run holds no in-process worker: its driver keeps
        // the lease live by heartbeat while the container works, and a driver
        // that dies is recovered by reconciling the existing container, not by
        // reaping the run out from under a container that is still spending.
        // Its absolute deadline (checked by the deadline scan above, which has
        // no such exemption) remains the backstop.
        .filter(
            entities::agent_run::Column::ExecutionLocation
                .eq(AgentRunExecutionLocation::InProcess.as_str()),
        )
        .filter(entities::agent_run::Column::Id.in_subquery(admitted_child_id_subquery()))
        .filter(
            sea_orm::Condition::any()
                .add(entities::agent_run::Column::Status.eq(AgentRunStatus::Cancelling.as_str()))
                .add(
                    sea_orm::Condition::all()
                        .add(
                            entities::agent_run::Column::Status
                                .eq(AgentRunStatus::Running.as_str()),
                        )
                        .add(
                            sea_orm::sea_query::Expr::col(
                                entities::agent_run::Column::AttemptCount,
                            )
                            .gte(sea_orm::sea_query::Expr::col(
                                entities::agent_run::Column::MaxAttempts,
                            )),
                        ),
                ),
        )
        .filter(entities::agent_run::Column::LeaseExpiresAt.lte(now))
        .filter(entities::agent_run::Column::UpdatedAt.lte(now))
        .order_by_asc(entities::agent_run::Column::LeaseExpiresAt)
        .order_by_asc(entities::agent_run::Column::CreatedAt)
        .order_by_asc(entities::agent_run::Column::Id)
        .one(conn)
        .await
        .map_err(store_err)
}

fn admitted_child_id_subquery() -> sea_orm::sea_query::SelectStatement {
    sea_orm::sea_query::Query::select()
        .column(entities::sandbox_agent_admission::Column::ChildRunId)
        .from(entities::sandbox_agent_admission::Entity)
        .to_owned()
}

async fn fail_candidate_on<C>(
    conn: &C,
    candidate: &entities::agent_run::Model,
    now: chrono::DateTime<Utc>,
    error_code: &str,
    error_detail: &str,
) -> Result<bool>
where
    C: sea_orm::ConnectionTrait,
{
    if candidate.status == AgentRunStatus::Cancelling.as_str() {
        return cancellation::reap_expired_cancelling_on(conn, candidate, now).await;
    }
    if candidate.status == AgentRunStatus::Waiting.as_str() {
        super::sandbox_tool::terminalize_sandbox_tool_call_for_run_on(
            conn,
            AgentRunId(candidate.id),
            error_code,
            now,
        )
        .await?;
    }
    let terminal_status = AgentRunStatus::Failed;
    let update = entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(terminal_status.as_str()),
        )
        .col_expr(
            entities::agent_run::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::agent_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::agent_run::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorCode,
            sea_orm::sea_query::Expr::value(Some(error_code.to_owned())),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorDetail,
            sea_orm::sea_query::Expr::value(Some(error_detail.to_owned())),
        )
        .col_expr(
            entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::agent_run::Column::Id.eq(candidate.id))
        .filter(entities::agent_run::Column::Status.eq(candidate.status.clone()))
        .filter(entities::agent_run::Column::AttemptCount.eq(candidate.attempt_count))
        .filter(entities::agent_run::Column::ClaimCount.eq(candidate.claim_count))
        .filter(entities::agent_run::Column::UpdatedAt.eq(candidate.updated_at))
        .filter(entities::agent_run::Column::UpdatedAt.lte(now));
    let update = if candidate.lease_token.is_some() {
        update
            .filter(entities::agent_run::Column::LeaseToken.eq(candidate.lease_token))
            .filter(entities::agent_run::Column::LeaseExpiresAt.eq(candidate.lease_expires_at))
    } else {
        update
            .filter(entities::agent_run::Column::LeaseToken.is_null())
            .filter(entities::agent_run::Column::LeaseExpiresAt.is_null())
    };
    let updated = update.exec(conn).await.map_err(store_err)?;
    if updated.rows_affected == 1 {
        deliver_terminal_candidate_failure_on(conn, candidate, now, error_code, error_detail)
            .await?;
        // A container run that went terminal owes its container a teardown in
        // the same transition — deadline expiry against a live unattached
        // container is exactly the leak this closes.
        if candidate.execution_location == AgentRunExecutionLocation::Container.as_str() {
            super::sandbox_provision::enqueue_teardown_on(conn, candidate.id).await?;
        }
    }
    Ok(updated.rows_affected == 1)
}

async fn deliver_terminal_candidate_failure_on<C>(
    conn: &C,
    candidate: &entities::agent_run::Model,
    now: chrono::DateTime<Utc>,
    error_code: &str,
    error_detail: &str,
) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    if entities::agent_run_result::Entity::find_by_id(candidate.id)
        .one(conn)
        .await
        .map_err(store_err)?
        .is_some()
    {
        return Ok(());
    }
    let Some(parent_id) = candidate.parent_id else {
        return Err(AgentError::Store(
            "terminal sandbox run is missing its parent".into(),
        ));
    };
    let (lease_token, attempt_count, claim_count) = if let Some(lease_token) = candidate.lease_token
    {
        (lease_token, candidate.attempt_count, candidate.claim_count)
    } else {
        let call = entities::sandbox_tool_call::Entity::find()
            .filter(entities::sandbox_tool_call::Column::AgentRunId.eq(candidate.id))
            .order_by_desc(entities::sandbox_tool_call::Column::ParkClaimCount)
            .order_by_desc(entities::sandbox_tool_call::Column::ParkAttemptCount)
            .one(conn)
            .await
            .map_err(store_err)?;
        if let Some(call) = call {
            (
                call.park_lease_token,
                call.park_attempt_count,
                call.park_claim_count,
            )
        } else if let Some(claim) = entities::agent_run_claim::Entity::find()
            .filter(entities::agent_run_claim::Column::AgentRunId.eq(Some(candidate.id)))
            .filter(
                entities::agent_run_claim::Column::AttemptCount.eq(Some(candidate.attempt_count)),
            )
            .filter(entities::agent_run_claim::Column::ClaimCount.eq(Some(candidate.claim_count)))
            .one(conn)
            .await
            .map_err(store_err)?
        {
            (
                claim.token,
                claim.attempt_count.expect("run claim has attempt"),
                claim.claim_count.expect("run claim has count"),
            )
        } else {
            // A queued child can expire before its first worker claim. Mint a
            // scheduler-origin receipt segment solely to preserve the same
            // immutable result/inbox contract for its parked parent.
            let lease_token = uuid::Uuid::new_v4();
            let attempt_count = Ord::max(candidate.attempt_count, 1);
            let claim_count = Ord::max(candidate.claim_count, attempt_count);
            entities::agent_run_claim::ActiveModel {
                token: Set(lease_token),
                agent_run_id: Set(Some(candidate.id)),
                attempt_count: Set(Some(attempt_count)),
                claim_count: Set(Some(claim_count)),
                claimed_at: Set(now),
                lease_expires_at: Set(Some(now + chrono::Duration::seconds(1))),
            }
            .insert(conn)
            .await
            .map_err(store_err)?;
            (lease_token, attempt_count, claim_count)
        }
    };
    let text = format!("Sandbox task failed ({error_code}): {error_detail}");
    let payload = AgentRunResultPayload::FinalText { text: text.clone() };
    entities::agent_run_result::ActiveModel {
        agent_run_id: Set(candidate.id),
        lease_token: Set(lease_token),
        attempt_count: Set(attempt_count),
        claim_count: Set(claim_count),
        payload_kind: Set(agent_run_result_payload_kind(&payload).into()),
        payload_json: Set(agent_run_result_payload_json(&payload)?),
        text: Set(text),
        submitted_at: Set(now),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    entities::agent_run_inbox::ActiveModel {
        child_run_id: Set(candidate.id),
        parent_run_id: Set(parent_id),
        chat_id: Set(candidate.chat_id),
        parent_depth: Set(0),
        result_lease_token: Set(lease_token),
        result_attempt_count: Set(attempt_count),
        result_claim_count: Set(claim_count),
        status: Set(AgentRunInboxStatus::Pending.as_str().into()),
        claim_count: Set(0),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        consumed_lease_token: Set(None),
        consumed_at: Set(None),
        delivered_at: Set(now),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

pub(in crate::db) async fn list_agent_runs(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<AgentRun>> {
    entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::ChatId.eq(chat_id.0))
        .order_by_asc(entities::agent_run::Column::CreatedAt)
        .order_by_asc(entities::agent_run::Column::Id)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(agent_run_from_model)
        .collect()
}

fn validate_request(
    id: AgentRunId,
    parent_id: Option<AgentRunId>,
    spawn_call_id: Option<CallId>,
    tier: AgentRunTier,
    input: Option<&str>,
) -> Result<()> {
    if id.0.is_nil() {
        return Err(AgentError::Store("agent-run id must not be nil".into()));
    }
    match tier {
        AgentRunTier::Foreground
            if parent_id.is_some() || spawn_call_id.is_some() || input.is_some() =>
        {
            Err(AgentError::Store(
                "foreground agent runs cannot have a parent, spawn call, or delegated task".into(),
            ))
        }
        AgentRunTier::Background if parent_id.is_none() || spawn_call_id.is_none() => {
            Err(AgentError::Store(
                "sandbox agent runs require a foreground parent and spawn-call identity".into(),
            ))
        }
        AgentRunTier::Background => {
            if parent_id.is_some_and(|parent| parent.0.is_nil())
                || spawn_call_id.is_some_and(|call| call.0.is_nil())
            {
                return Err(AgentError::Store(
                    "sandbox parent and spawn-call identities must not be nil".into(),
                ));
            }
            let Some(input) = input else {
                return Err(AgentError::Store(
                    "sandbox agent runs require a delegated task".into(),
                ));
            };
            let input_len = input.chars().count();
            if input_len == 0 || input_len > AgentRun::MAX_INPUT_LEN {
                return Err(AgentError::Store(format!(
                    "sandbox agent-run task must contain 1..={} characters",
                    AgentRun::MAX_INPUT_LEN
                )));
            }
            Ok(())
        }
        AgentRunTier::Foreground => Ok(()),
    }
}

fn validate_admission_request(
    origin_turn_id: TurnId,
    spawn_call_id: CallId,
    input: &str,
    lease_token: uuid::Uuid,
    max_outstanding_children: u32,
) -> Result<()> {
    if origin_turn_id.0.is_nil() || spawn_call_id.0.is_nil() || lease_token.is_nil() {
        return Err(AgentError::Store(
            "sandbox admission identities must not be nil".into(),
        ));
    }
    if max_outstanding_children == 0 || max_outstanding_children > AgentRun::MAX_CONCURRENCY_LIMIT {
        return Err(AgentError::Store(format!(
            "sandbox outstanding-child limit must be in 1..={}",
            AgentRun::MAX_CONCURRENCY_LIMIT
        )));
    }
    let input_len = input.chars().count();
    if input_len == 0 || input_len > AgentRun::MAX_INPUT_LEN {
        return Err(AgentError::Store(format!(
            "sandbox agent-run task must contain 1..={} characters",
            AgentRun::MAX_INPUT_LEN
        )));
    }
    Ok(())
}

pub(in crate::db) fn agent_run_from_model(model: entities::agent_run::Model) -> Result<AgentRun> {
    let tier = match model.tier.as_str() {
        "foreground" => AgentRunTier::Foreground,
        "background" => AgentRunTier::Background,
        value => return Err(AgentError::Store(format!("invalid agent-run tier {value}"))),
    };
    let execution_location = match model.execution_location.as_str() {
        "in_process" => AgentRunExecutionLocation::InProcess,
        "container" => AgentRunExecutionLocation::Container,
        value => {
            return Err(AgentError::Store(format!(
                "invalid agent-run execution location {value}"
            )))
        }
    };
    let status = agent_run_status_from_db(&model.status)?;
    validate_stored_shape(&model, tier, status)?;
    Ok(AgentRun {
        id: AgentRunId(model.id),
        chat_id: ChatId(model.chat_id),
        parent_id: model.parent_id.map(AgentRunId),
        spawn_call_id: model.spawn_call_id.map(CallId),
        tier,
        execution_location,
        depth: u8::try_from(model.depth)
            .map_err(|_| AgentError::Store("invalid negative agent-run depth".into()))?,
        status,
        input: model.input,
        model: model.model,
        attempt_count: model.attempt_count,
        max_attempts: model.max_attempts,
        claim_count: model.claim_count,
        available_at: model.available_at,
        deadline_at: model.deadline_at,
        lease_token: model.lease_token,
        lease_expires_at: model.lease_expires_at,
        started_at: model.started_at,
        finished_at: model.finished_at,
        last_error_code: model.last_error_code,
        last_error_detail: model.last_error_detail,
        created_at: model.created_at,
        updated_at: model.updated_at,
    })
}

fn sandbox_agent_admission_from_model(
    model: entities::sandbox_agent_admission::Model,
) -> Result<SandboxAgentAdmission> {
    let resource = match (model.delegated_root_id, model.delegated_relative_path) {
        (Some(root_id), Some(relative_path)) => Some(SandboxAgentFileResource {
            root_id: crate::HostRootId::from_uuid(root_id).map_err(|error| {
                AgentError::Store(format!("invalid delegated root id: {error}"))
            })?,
            relative_path,
        }),
        (None, None) => None,
        _ => {
            return Err(AgentError::Store(
                "stored sandbox delegation has a partial file identity".into(),
            ));
        }
    };
    let admission = SandboxAgentAdmission {
        child_run_id: AgentRunId(model.child_run_id),
        parent_run_id: AgentRunId(model.parent_run_id),
        origin_turn_id: TurnId(model.origin_turn_id),
        chat_id: ChatId(model.chat_id),
        spawn_call_id: CallId(model.spawn_call_id),
        resource,
        admitted_at: model.admitted_at,
    };
    if admission.child_run_id != AgentRunId::sandbox_for_spawn_call(admission.spawn_call_id)
        || admission.parent_run_id.0.is_nil()
        || admission.origin_turn_id.0.is_nil()
        || admission.chat_id.0.is_nil()
        || admission
            .resource
            .as_ref()
            .is_some_and(|resource| !resource.is_well_formed())
    {
        return Err(AgentError::Store(
            "invalid stored sandbox agent admission".into(),
        ));
    }
    Ok(admission)
}

fn agent_run_status_from_db(value: &str) -> Result<AgentRunStatus> {
    let status = match value {
        "active" => AgentRunStatus::Active,
        "queued" => AgentRunStatus::Queued,
        "running" => AgentRunStatus::Running,
        "cancelling" => AgentRunStatus::Cancelling,
        "waiting" => AgentRunStatus::Waiting,
        "retry_wait" => AgentRunStatus::RetryWait,
        "completed" => AgentRunStatus::Completed,
        "failed" => AgentRunStatus::Failed,
        "cancelled" => AgentRunStatus::Cancelled,
        value => {
            return Err(AgentError::Store(format!(
                "invalid agent-run status {value}"
            )))
        }
    };
    Ok(status)
}

fn agent_run_result_from_model(model: entities::agent_run_result::Model) -> Result<AgentRunResult> {
    if model.lease_token.is_nil()
        || model.attempt_count < 1
        || model.claim_count < model.attempt_count
        || model.text.is_empty()
        || model.text.chars().count() > AgentRun::MAX_RESULT_LEN
    {
        return Err(AgentError::Store("invalid stored agent-run result".into()));
    }
    let payload = agent_run_result_payload_from_columns(&model.payload_kind, &model.payload_json)?;
    if model.text != agent_run_result_display_text(&payload) {
        return Err(AgentError::Store(
            "agent-run result display text does not match its typed payload".into(),
        ));
    }
    Ok(AgentRunResult {
        agent_run_id: AgentRunId(model.agent_run_id),
        lease_token: model.lease_token,
        attempt_count: model.attempt_count,
        claim_count: model.claim_count,
        payload,
        text: model.text,
        submitted_at: model.submitted_at,
    })
}

fn validate_agent_run_result_payload(payload: &AgentRunResultPayload) -> Result<()> {
    match payload {
        AgentRunResultPayload::FinalText { text }
            if !text.is_empty() && text.chars().count() <= AgentRun::MAX_RESULT_LEN =>
        {
            Ok(())
        }
        AgentRunResultPayload::FinalText { .. } => Err(AgentError::Store(format!(
            "agent-run result requires 1..={} characters",
            AgentRun::MAX_RESULT_LEN
        ))),
        AgentRunResultPayload::FolderAccessProposal { request } if request.is_well_formed() => {
            Ok(())
        }
        AgentRunResultPayload::FolderAccessProposal { .. } => Err(AgentError::Store(
            "invalid sandbox folder-access proposal".into(),
        )),
        AgentRunResultPayload::Cancelled { .. } => Ok(()),
    }
}

fn agent_run_result_payload_kind(payload: &AgentRunResultPayload) -> &'static str {
    match payload {
        AgentRunResultPayload::FinalText { .. } => "final_text",
        AgentRunResultPayload::FolderAccessProposal { .. } => "folder_access_proposal",
        AgentRunResultPayload::Cancelled { .. } => "cancelled",
    }
}

fn agent_run_result_payload_json(payload: &AgentRunResultPayload) -> Result<String> {
    match payload {
        AgentRunResultPayload::FinalText { text } => serde_json::to_string(&serde_json::json!({
            "text": text,
        })),
        AgentRunResultPayload::FolderAccessProposal { request } => serde_json::to_string(request),
        AgentRunResultPayload::Cancelled { reason } => serde_json::to_string(&serde_json::json!({
            "reason": reason.as_str(),
        })),
    }
    .map_err(|error| AgentError::Store(format!("serialize agent-run result payload: {error}")))
}

fn agent_run_result_payload_from_columns(kind: &str, json: &str) -> Result<AgentRunResultPayload> {
    let payload = match kind {
        "final_text" => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct FinalTextPayload {
                text: String,
            }
            let payload = serde_json::from_str::<FinalTextPayload>(json).map_err(|_| {
                AgentError::Store("invalid stored final-text agent-run result payload".into())
            })?;
            AgentRunResultPayload::FinalText { text: payload.text }
        }
        "folder_access_proposal" => {
            let request = serde_json::from_str::<RequestFolderAccessArgs>(json).map_err(|_| {
                AgentError::Store("invalid stored folder-access proposal payload".into())
            })?;
            AgentRunResultPayload::FolderAccessProposal { request }
        }
        "cancelled" => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct CancelledPayload {
                reason: String,
            }
            let payload = serde_json::from_str::<CancelledPayload>(json).map_err(|_| {
                AgentError::Store("invalid stored cancellation result payload".into())
            })?;
            let reason = AgentRunCancellationReason::parse(&payload.reason).ok_or_else(|| {
                AgentError::Store("invalid stored cancellation result reason".into())
            })?;
            AgentRunResultPayload::Cancelled { reason }
        }
        _ => {
            return Err(AgentError::Store(
                "invalid stored agent-run result payload kind".into(),
            ))
        }
    };
    validate_agent_run_result_payload(&payload)?;
    Ok(payload)
}

fn agent_run_result_display_text(payload: &AgentRunResultPayload) -> String {
    match payload {
        AgentRunResultPayload::FinalText { text } => text.clone(),
        AgentRunResultPayload::FolderAccessProposal { request } => {
            let hint = match request.folder_hint {
                Some(RequestedFolderHint::Documents) => "\nPicker hint: Documents",
                Some(RequestedFolderHint::Downloads) => "\nPicker hint: Downloads",
                None => "",
            };
            format!(
                "Sandbox agent requested that you decide whether to ask the user to connect a folder. This grants no access. If appropriate, issue the normal request_folder_access tool; otherwise continue without host access.\nReason: {}\nRequested capability: read_files{hint}",
                request.reason
            )
        }
        AgentRunResultPayload::Cancelled { reason } => match reason {
            AgentRunCancellationReason::Requested => "Sandbox task was cancelled.".to_owned(),
            AgentRunCancellationReason::ParentTurnCancelled => {
                "Sandbox task was cancelled because its parent turn was cancelled.".to_owned()
            }
            AgentRunCancellationReason::ParentTurnFailed => {
                "Sandbox task was cancelled because its parent turn failed.".to_owned()
            }
        },
    }
}

fn agent_run_inbox_from_models(
    model: entities::agent_run_inbox::Model,
    result: entities::agent_run_result::Model,
) -> Result<AgentRunInboxEntry> {
    let status = AgentRunInboxStatus::parse(&model.status).ok_or_else(|| {
        AgentError::Store("invalid stored agent-run inbox continuation status".into())
    })?;
    let valid_continuation = match status {
        AgentRunInboxStatus::Pending => {
            model.claim_count == 0
                && model.lease_token.is_none()
                && model.lease_expires_at.is_none()
                && model.consumed_lease_token.is_none()
                && model.consumed_at.is_none()
        }
        AgentRunInboxStatus::Claimed => {
            model.claim_count >= 1
                && model.lease_token.is_some_and(|token| !token.is_nil())
                && model.lease_expires_at.is_some()
                && model.consumed_lease_token.is_none()
                && model.consumed_at.is_none()
        }
        AgentRunInboxStatus::Consumed => {
            model.claim_count >= 1
                && model.lease_token.is_none()
                && model.lease_expires_at.is_none()
                && model
                    .consumed_lease_token
                    .is_some_and(|token| !token.is_nil())
                && model.consumed_at.is_some()
        }
        AgentRunInboxStatus::Cancelled => {
            model.lease_token.is_none()
                && model.lease_expires_at.is_none()
                && model.consumed_lease_token.is_none()
                && model.consumed_at.is_none()
        }
    };
    if model.parent_depth != 0
        || model.result_lease_token.is_nil()
        || model.result_attempt_count < 1
        || model.result_claim_count < model.result_attempt_count
        || !valid_continuation
    {
        return Err(AgentError::Store(
            "invalid stored agent-run inbox entry".into(),
        ));
    }
    let result = agent_run_result_from_model(result)?;
    if result.agent_run_id != AgentRunId(model.child_run_id)
        || result.lease_token != model.result_lease_token
        || result.attempt_count != model.result_attempt_count
        || result.claim_count != model.result_claim_count
    {
        return Err(AgentError::Store(
            "agent-run inbox does not match its result receipt".into(),
        ));
    }
    Ok(AgentRunInboxEntry {
        parent_run_id: AgentRunId(model.parent_run_id),
        child_run_id: AgentRunId(model.child_run_id),
        chat_id: ChatId(model.chat_id),
        result,
        status,
        claim_count: model.claim_count,
        lease_token: model.lease_token,
        lease_expires_at: model.lease_expires_at,
        consumed_lease_token: model.consumed_lease_token,
        consumed_at: model.consumed_at,
        delivered_at: model.delivered_at,
    })
}

async fn find_agent_run_inbox_on<C>(
    conn: &C,
    parent_run_id: AgentRunId,
    child_run_id: AgentRunId,
) -> Result<Option<entities::agent_run_inbox::Model>>
where
    C: sea_orm::ConnectionTrait,
{
    entities::agent_run_inbox::Entity::find_by_id(child_run_id.0)
        .filter(entities::agent_run_inbox::Column::ParentRunId.eq(parent_run_id.0))
        .one(conn)
        .await
        .map_err(store_err)
}

pub(in crate::db) async fn load_agent_run_inbox_by_ids_on<C>(
    conn: &C,
    parent_run_id: AgentRunId,
    child_run_id: AgentRunId,
) -> Result<Option<AgentRunInboxEntry>>
where
    C: sea_orm::ConnectionTrait,
{
    let Some(model) = find_agent_run_inbox_on(conn, parent_run_id, child_run_id).await? else {
        return Ok(None);
    };
    load_agent_run_inbox_entry_on(conn, model).await.map(Some)
}

async fn load_agent_run_inbox_entry_on<C>(
    conn: &C,
    model: entities::agent_run_inbox::Model,
) -> Result<AgentRunInboxEntry>
where
    C: sea_orm::ConnectionTrait,
{
    let result = entities::agent_run_result::Entity::find_by_id(model.child_run_id)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "agent-run inbox child {} is missing its result receipt",
                model.child_run_id
            ))
        })?;
    agent_run_inbox_from_models(model, result)
}

async fn parent_is_active_on<C>(conn: &C, parent_run_id: AgentRunId) -> Result<bool>
where
    C: sea_orm::ConnectionTrait,
{
    let parent = find_by_id_on(conn, parent_run_id).await?;
    Ok(parent.is_some_and(|parent| {
        parent.depth == 0
            && parent.tier == AgentRunTier::Foreground.as_str()
            && parent.status == AgentRunStatus::Active.as_str()
    }))
}

/// Persist the exact terminal child result into the foreground transcript at
/// the same durable boundary that consumes its inbox receipt.  A deterministic
/// message id makes an ambiguous commit retry safe without relying on worker
/// memory: the next foreground provider request rebuilds this message from the
/// ordinary transcript after any restart.
pub(in crate::db) async fn ensure_sandbox_result_message_on<C>(
    conn: &C,
    entry: &AgentRunInboxEntry,
    turn: &TurnRun,
    now: chrono::DateTime<Utc>,
    insert_if_missing: bool,
) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    let child = find_by_id_on(conn, entry.child_run_id)
        .await?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "agent-run inbox child {} disappeared before transcript delivery",
                entry.child_run_id
            ))
        })?;
    if child.chat_id != entry.chat_id.0
        || child.parent_id != Some(entry.parent_run_id.0)
        || child.tier != AgentRunTier::Background.as_str()
        || child.depth != 1
    {
        return Err(AgentError::Store(format!(
            "agent-run inbox child {} does not match its durable parent delivery",
            entry.child_run_id
        )));
    }
    let disposition = match agent_run_status_from_db(&child.status)? {
        AgentRunStatus::Completed => "completed",
        AgentRunStatus::Failed => "failed",
        // Cancellation delivery is introduced by a later coordinated slice.
        // Accepting its eventual immutable receipt here keeps ordered multi-
        // child transcript assembly additive without synthesizing context.
        AgentRunStatus::Cancelled => "cancelled",
        status => {
            return Err(AgentError::Store(format!(
                "agent-run inbox child {} is not terminal ({})",
                entry.child_run_id,
                status.as_str()
            )));
        }
    };
    let message_id = MessageId::sandbox_result_for_child(entry.child_run_id);
    let content = match &entry.result.payload {
        AgentRunResultPayload::FinalText { .. } => format!(
            "Sandbox agent {disposition}. Its exact final result follows:\n{}",
            entry.result.text
        ),
        AgentRunResultPayload::FolderAccessProposal { .. } => entry.result.text.clone(),
        AgentRunResultPayload::Cancelled { .. } => entry.result.text.clone(),
    };
    let existing = entities::message::Entity::find_by_id(message_id.0)
        .one(conn)
        .await
        .map_err(store_err)?;
    if let Some(existing) = existing {
        if existing.chat_id == entry.chat_id.0
            && existing.turn_id == turn.id.0
            && existing.role == "system"
            && existing.content == content
        {
            return Ok(());
        }
        return Err(AgentError::Store(format!(
            "sandbox result message identity {message_id} is already bound to different content"
        )));
    }
    if !insert_if_missing {
        return Err(AgentError::Store(format!(
            "consumed agent-run inbox entry for child {} is missing its transcript result",
            entry.child_run_id
        )));
    }
    if !reserve_message_identity_on(
        conn,
        message_id,
        entry.chat_id,
        turn.id,
        MESSAGE_IDENTITY_OWNER_MESSAGE,
    )
    .await?
    {
        return Err(AgentError::Store(format!(
            "sandbox result message identity {message_id} was reserved without a message"
        )));
    }
    entities::message::ActiveModel {
        id: Set(message_id.0),
        chat_id: Set(entry.chat_id.0),
        turn_id: Set(turn.id.0),
        seq: Set(next_message_seq_on(conn, entry.chat_id).await?),
        role: Set("system".to_owned()),
        content: Set(content),
        turn_lease_token: Set(None),
        created_at: Set(now),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

/// Confirm the parent has durably yielded the foreground turn that this exact
/// child result will wake. Holding the turn lock through the inbox claim keeps
/// cancellation or another wake from leaving a live continuation lease with no
/// checkpoint to consume.
async fn parent_turn_checkpoint_is_waiting_on<C>(
    conn: &C,
    parent_run_id: AgentRunId,
    child_run_id: AgentRunId,
) -> Result<bool>
where
    C: sea_orm::ConnectionTrait,
{
    let Some(wait) = entities::turn_agent_run_wait::Entity::find_by_id(child_run_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(false);
    };
    if wait.parent_run_id != parent_run_id.0
        || wait.status != crate::model::TurnAgentRunWaitStatus::Waiting.as_str()
        || wait.closed_at.is_some()
    {
        return Ok(false);
    }
    let turn_id = crate::TurnId(wait.turn_id);
    if !acquire_turn_write_lock(conn, turn_id).await? {
        return Err(AgentError::Store(format!(
            "agent-run checkpoint for child {child_run_id} references missing turn {turn_id}"
        )));
    }
    let turn = entities::turn_run::Entity::find_by_id(wait.turn_id)
        .one(conn)
        .await
        .map_err(store_err)?
        .expect("locked agent-run checkpoint turn exists");
    Ok(turn.chat_id == wait.chat_id
        && turn.agent_run_id == parent_run_id.0
        && turn.status == crate::TurnRunStatus::WaitingForAgentRun.as_str()
        && turn.attempt_count == wait.attempt_count
        && turn.claim_count == wait.claim_count
        && turn.lease_token.is_none()
        && turn.lease_expires_at.is_none())
}

pub(in crate::db) async fn find_by_id_on<C>(
    conn: &C,
    id: AgentRunId,
) -> Result<Option<entities::agent_run::Model>>
where
    C: sea_orm::ConnectionTrait,
{
    entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::Id.eq(id.0))
        .one(conn)
        .await
        .map_err(store_err)
}

async fn find_by_spawn_call_on<C>(
    conn: &C,
    id: CallId,
) -> Result<Option<entities::agent_run::Model>>
where
    C: sea_orm::ConnectionTrait,
{
    entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::SpawnCallId.eq(id.0))
        .one(conn)
        .await
        .map_err(store_err)
}

async fn find_foreground_on<C>(
    conn: &C,
    chat_id: ChatId,
) -> Result<Option<entities::agent_run::Model>>
where
    C: sea_orm::ConnectionTrait,
{
    entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::ChatId.eq(chat_id.0))
        .filter(entities::agent_run::Column::Tier.eq(AgentRunTier::Foreground.as_str()))
        .one(conn)
        .await
        .map_err(store_err)
}

fn existing_request_outcome(
    existing: entities::agent_run::Model,
    chat_id: ChatId,
    parent_id: Option<AgentRunId>,
    spawn_call_id: Option<CallId>,
    tier: AgentRunTier,
    input: Option<&str>,
) -> Result<AcceptAgentRunOutcome> {
    let exact = existing.chat_id == chat_id.0
        && existing.parent_id == parent_id.map(|parent| parent.0)
        && existing.spawn_call_id == spawn_call_id.map(|call| call.0)
        && existing.tier == tier.as_str()
        && existing.input.as_deref() == input;
    Ok(if exact {
        AcceptAgentRunOutcome::Existing(agent_run_from_model(existing)?)
    } else {
        AcceptAgentRunOutcome::IdentityConflict
    })
}

fn validate_stored_shape(
    model: &entities::agent_run::Model,
    tier: AgentRunTier,
    status: AgentRunStatus,
) -> Result<()> {
    if model.id.is_nil() || model.updated_at < model.created_at {
        return Err(AgentError::Store(
            "invalid persisted agent-run identity or timestamp".into(),
        ));
    }
    // A recorded model is optional — rows admitted before it was persisted have
    // none — but a stored one must be a usable selection.
    if model.model.as_ref().is_some_and(|selection| {
        let len = selection.chars().count();
        len == 0 || len > AgentRun::MAX_MODEL_LEN
    }) {
        return Err(AgentError::Store(
            "invalid persisted agent-run model".into(),
        ));
    }
    let valid = match tier {
        AgentRunTier::Foreground => {
            model.depth == 0
                && model.parent_id.is_none()
                && model.parent_depth.is_none()
                && model.spawn_call_id.is_none()
                && model.input.is_none()
                && model.attempt_count == 0
                && model.max_attempts == 0
                && model.claim_count == 0
                && model.available_at == model.created_at
                && model.deadline_at.is_none()
                && model.lease_token.is_none()
                && model.lease_expires_at.is_none()
                && model.started_at.is_none()
                && matches!(
                    status,
                    AgentRunStatus::Active
                        | AgentRunStatus::Completed
                        | AgentRunStatus::Failed
                        | AgentRunStatus::Cancelled
                )
        }
        AgentRunTier::Background => {
            model.depth == i16::from(AgentRun::MAX_DEPTH)
                && model.parent_id.is_some()
                && model.parent_depth == Some(0)
                && model.spawn_call_id.is_some_and(|call| !call.is_nil())
                && model.input.as_ref().is_some_and(|input| {
                    let len = input.chars().count();
                    len > 0 && len <= AgentRun::MAX_INPUT_LEN
                })
                && model.max_attempts >= 1
                && model.attempt_count >= 0
                && model.attempt_count <= model.max_attempts
                && model.claim_count >= model.attempt_count
                && model.available_at >= model.created_at
                && model
                    .deadline_at
                    .is_some_and(|deadline| deadline > model.created_at)
                && status != AgentRunStatus::Active
        }
    };
    if valid {
        Ok(())
    } else {
        Err(AgentError::Store(
            "invalid persisted agent-run shape".into(),
        ))
    }
}
