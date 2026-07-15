use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::id::{AgentRunId, CallId, ChatId};
use crate::model::{AgentRun, AgentRunExecution, AgentRunStatus};
use crate::storage::AcceptAgentRunOutcome;

use super::super::{entities, store_err, DbStore};
use super::{acquire_chat_write_lock, turn::canonical_db_timestamp};

pub(in crate::db) async fn accept_agent_run(
    store: &DbStore,
    id: AgentRunId,
    chat_id: ChatId,
    parent_id: Option<AgentRunId>,
    spawn_call_id: Option<CallId>,
    execution: AgentRunExecution,
    input: Option<&str>,
) -> Result<AcceptAgentRunOutcome> {
    validate_request(id, parent_id, spawn_call_id, execution, input)?;

    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        return Err(AgentError::Store(format!("chat {chat_id} does not exist")));
    }

    if let Some(existing) = find_by_id_on(&transaction, id).await? {
        let outcome = existing_request_outcome(
            existing,
            chat_id,
            parent_id,
            spawn_call_id,
            execution,
            input,
        )?;
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
                execution,
                input,
            )?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(outcome);
        }
    }

    let (depth, status) = match execution {
        AgentRunExecution::Foreground => {
            if let Some(existing) = entities::agent_run::Entity::find()
                .filter(entities::agent_run::Column::ChatId.eq(chat_id.0))
                .filter(
                    entities::agent_run::Column::Execution
                        .eq(AgentRunExecution::Foreground.as_str()),
                )
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
        AgentRunExecution::Sandbox => {
            let Some(parent_id) = parent_id else {
                unreachable!("validated sandbox parent")
            };
            let parent = find_by_id_on(&transaction, parent_id).await?;
            let available = parent.is_some_and(|parent| {
                parent.chat_id == chat_id.0
                    && parent.parent_id.is_none()
                    && parent.depth == 0
                    && parent.execution == AgentRunExecution::Foreground.as_str()
                    && parent.status == AgentRunStatus::Active.as_str()
            });
            if !available {
                transaction.commit().await.map_err(store_err)?;
                return Ok(AcceptAgentRunOutcome::ParentUnavailable);
            }
            (i16::from(AgentRun::MAX_DEPTH), AgentRunStatus::Queued)
        }
    };

    let now = canonical_db_timestamp(Utc::now())?;
    let model = entities::agent_run::ActiveModel {
        id: Set(id.0),
        chat_id: Set(chat_id.0),
        parent_id: Set(parent_id.map(|parent| parent.0)),
        parent_depth: Set(parent_id.map(|_| 0)),
        spawn_call_id: Set(spawn_call_id.map(|call| call.0)),
        execution: Set(execution.as_str().into()),
        depth: Set(depth),
        status: Set(status.as_str().into()),
        input: Set(input.map(ToOwned::to_owned)),
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
                    execution,
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
                        execution,
                        input,
                    );
                }
            }
            if execution == AgentRunExecution::Foreground {
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

pub(in crate::db) async fn get_agent_run(
    store: &DbStore,
    id: AgentRunId,
) -> Result<Option<AgentRun>> {
    find_by_id_on(&store.conn, id)
        .await?
        .map(agent_run_from_model)
        .transpose()
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
    execution: AgentRunExecution,
    input: Option<&str>,
) -> Result<()> {
    if id.0.is_nil() {
        return Err(AgentError::Store("agent-run id must not be nil".into()));
    }
    match execution {
        AgentRunExecution::Foreground
            if parent_id.is_some() || spawn_call_id.is_some() || input.is_some() =>
        {
            Err(AgentError::Store(
                "foreground agent runs cannot have a parent, spawn call, or delegated task".into(),
            ))
        }
        AgentRunExecution::Sandbox if parent_id.is_none() || spawn_call_id.is_none() => {
            Err(AgentError::Store(
                "sandbox agent runs require a foreground parent and spawn-call identity".into(),
            ))
        }
        AgentRunExecution::Sandbox => {
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
        AgentRunExecution::Foreground => Ok(()),
    }
}

fn agent_run_from_model(model: entities::agent_run::Model) -> Result<AgentRun> {
    let execution = match model.execution.as_str() {
        "foreground" => AgentRunExecution::Foreground,
        "sandbox" => AgentRunExecution::Sandbox,
        value => {
            return Err(AgentError::Store(format!(
                "invalid agent-run execution {value}"
            )))
        }
    };
    let status = match model.status.as_str() {
        "active" => AgentRunStatus::Active,
        "queued" => AgentRunStatus::Queued,
        "running" => AgentRunStatus::Running,
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
    validate_stored_shape(&model, execution, status)?;
    Ok(AgentRun {
        id: AgentRunId(model.id),
        chat_id: ChatId(model.chat_id),
        parent_id: model.parent_id.map(AgentRunId),
        spawn_call_id: model.spawn_call_id.map(CallId),
        execution,
        depth: u8::try_from(model.depth)
            .map_err(|_| AgentError::Store("invalid negative agent-run depth".into()))?,
        status,
        input: model.input,
        created_at: model.created_at,
        updated_at: model.updated_at,
    })
}

async fn find_by_id_on<C>(conn: &C, id: AgentRunId) -> Result<Option<entities::agent_run::Model>>
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
        .filter(entities::agent_run::Column::Execution.eq(AgentRunExecution::Foreground.as_str()))
        .one(conn)
        .await
        .map_err(store_err)
}

fn existing_request_outcome(
    existing: entities::agent_run::Model,
    chat_id: ChatId,
    parent_id: Option<AgentRunId>,
    spawn_call_id: Option<CallId>,
    execution: AgentRunExecution,
    input: Option<&str>,
) -> Result<AcceptAgentRunOutcome> {
    let exact = existing.chat_id == chat_id.0
        && existing.parent_id == parent_id.map(|parent| parent.0)
        && existing.spawn_call_id == spawn_call_id.map(|call| call.0)
        && existing.execution == execution.as_str()
        && existing.input.as_deref() == input;
    Ok(if exact {
        AcceptAgentRunOutcome::Existing(agent_run_from_model(existing)?)
    } else {
        AcceptAgentRunOutcome::IdentityConflict
    })
}

fn validate_stored_shape(
    model: &entities::agent_run::Model,
    execution: AgentRunExecution,
    status: AgentRunStatus,
) -> Result<()> {
    if model.id.is_nil() || model.updated_at < model.created_at {
        return Err(AgentError::Store(
            "invalid persisted agent-run identity or timestamp".into(),
        ));
    }
    let valid = match execution {
        AgentRunExecution::Foreground => {
            model.depth == 0
                && model.parent_id.is_none()
                && model.parent_depth.is_none()
                && model.spawn_call_id.is_none()
                && model.input.is_none()
                && matches!(
                    status,
                    AgentRunStatus::Active
                        | AgentRunStatus::Completed
                        | AgentRunStatus::Failed
                        | AgentRunStatus::Cancelled
                )
        }
        AgentRunExecution::Sandbox => {
            model.depth == i16::from(AgentRun::MAX_DEPTH)
                && model.parent_id.is_some()
                && model.parent_depth == Some(0)
                && model.spawn_call_id.is_some_and(|call| !call.is_nil())
                && model.input.as_ref().is_some_and(|input| {
                    let len = input.chars().count();
                    len > 0 && len <= AgentRun::MAX_INPUT_LEN
                })
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
