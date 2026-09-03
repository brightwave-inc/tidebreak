use super::*;

pub(in crate::db) async fn request_turn_cancellation(
    store: &DbStore,
    id: TurnId,
    now: chrono::DateTime<Utc>,
) -> Result<Option<RequestTurnCancellationOutcome>> {
    Ok(request_turn_cancellation_inner(store, id, now, false)
        .await?
        .map(|resolution| resolution.outcome))
}

pub(in crate::db) async fn request_turn_cancellation_and_append_event(
    store: &DbStore,
    id: TurnId,
    now: chrono::DateTime<Utc>,
) -> Result<Option<JournaledTurnOutcome<RequestTurnCancellationOutcome>>> {
    request_turn_cancellation_inner(store, id, now, true).await
}

async fn request_turn_cancellation_inner(
    store: &DbStore,
    id: TurnId,
    now: chrono::DateTime<Utc>,
    journal_terminal_event: bool,
) -> Result<Option<JournaledTurnOutcome<RequestTurnCancellationOutcome>>> {
    let requested_at = canonical_db_timestamp(now)?;
    // Client claiming/resolution also takes this chat lock before its call and
    // turn locks. Take it even for the non-journaling Store API so cancellation
    // cannot invert that order while inspecting a parked client call.
    let journal_chat_id = journal_chat_id(store, id, true).await?;
    if journal_chat_id.is_none() {
        return Ok(None);
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    // Sandbox scheduling, direct child cancellation, inbox continuation, and
    // foreground cancellation share this outer lock order: scheduler first,
    // then chat and turn. This lets parent cancellation mint terminal child
    // provenance without racing a first worker claim or a direct cancellation
    // request, while avoiding the scheduler/turn inversion that would arise
    // from taking this lock after the turn lock.
    super::super::super::agent_run::acquire_agent_run_claim_lock(&transaction).await?;
    super::super::multi_agent_run_wait::acquire_wait_set_lock(&transaction).await?;
    if let Some(chat_id) = journal_chat_id {
        if !acquire_chat_write_lock(&transaction, chat_id).await? {
            return Err(AgentError::Store(format!(
                "turn {id} references missing chat {chat_id}"
            )));
        }
    }
    if !acquire_turn_write_lock(&transaction, id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    // The caller timestamp is request intent, not an operational clock. Lock
    // acquisition may wait behind admission, heartbeat, or tool resolution;
    // take authoritative statement time after every cancellation lock so no
    // child or turn timestamp can move backwards.
    let now = std::cmp::max(
        requested_at,
        super::super::super::agent_run::database_now(&transaction).await?,
    );
    let Some(turn) = entities::turn::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let adapter_wait = super::super::adapter_park_wait(&turn)?;
    let adapter_client_call = super::super::adapter_client_park_call_id(&turn)?;
    let status = match adapter_wait {
        Some(crate::TurnParkWait::Approval { .. })
        | Some(crate::TurnParkWait::ClientToolCall { .. }) => TurnRunStatus::WaitingForClient,
        Some(crate::TurnParkWait::AgentRuns { .. }) => TurnRunStatus::WaitingForAgentRun,
        None => turn_run_status_from_db(&turn.status)?,
    };
    match status {
        TurnRunStatus::Cancelling | TurnRunStatus::CancellingClient | TurnRunStatus::Cancelled => {
            let sequenced_event = if status == TurnRunStatus::Cancelled {
                existing_cancellation_event_on(&transaction, id, journal_terminal_event).await?
            } else {
                None
            };
            let turn = turn_run_from_model(turn)?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(JournaledTurnOutcome {
                outcome: RequestTurnCancellationOutcome::Existing(turn),
                terminal_event: sequenced_event,
            }));
        }
        TurnRunStatus::Completed | TurnRunStatus::Failed => {
            let turn = turn_run_from_model(turn)?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(JournaledTurnOutcome {
                outcome: RequestTurnCancellationOutcome::AlreadyTerminal(turn),
                terminal_event: None,
            }));
        }
        TurnRunStatus::Queued
        | TurnRunStatus::Running
        | TurnRunStatus::WaitingForClient
        | TurnRunStatus::WaitingForAgentRun
        | TurnRunStatus::Resuming
        | TurnRunStatus::RetryWait => {}
    }
    if turn
        .updated_at
        .is_some_and(|updated_at| updated_at > requested_at)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }

    // Every sandbox admitted by this exact turn is part of its cancellation
    // boundary, including children that were spawned before the turn parked
    // (or that the turn never chose to await). The outer scheduler lock makes
    // this enumeration serial with claims, direct cancellation, and terminal
    // worker acknowledgement.
    if !super::super::super::agent_run::cancel_sandbox_children_for_origin_turn_on(
        &transaction,
        &turn,
        now,
        crate::model::AgentRunCancellationReason::ParentTurnCancelled,
    )
    .await?
    {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }

    let mut cancel_unclaimed_call = None;
    if status == TurnRunStatus::WaitingForClient {
        let wait = entities::turn_client_wait::Entity::find()
            .filter(entities::turn_client_wait::Column::TurnId.eq(id.0))
            .filter(
                entities::turn_client_wait::Column::Status
                    .eq(crate::model::TurnClientWaitStatus::Waiting.as_str()),
            )
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| {
                AgentError::Store(format!("waiting turn {id} is missing its client receipt"))
            })?;
        if wait.session_id != turn.session_id
            || wait.attempt_count != turn.attempt_count
            || wait.claim_count != turn.claim_count
            || adapter_client_call.is_some_and(|call_id| call_id.0 != wait.call_id)
        {
            return Err(AgentError::Store(format!(
                "waiting turn {id} has a mismatched client receipt"
            )));
        }
        let call_id = crate::CallId(wait.call_id);
        if !super::super::super::acquire_tool_call_write_lock(&transaction, call_id).await? {
            return Err(AgentError::Store(format!(
                "waiting turn {id} references missing client call {call_id}"
            )));
        }
        let call = entities::tool_call::Entity::find_by_id(wait.call_id)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .expect("locked waiting client call exists");
        let is_foreground_question = call.name == crate::ASK_USER_QUESTIONS_TOOL
            && call.execution == crate::model::ToolCallExecution::Orchestration.as_str();
        if call.chat_id != turn.session_id
            || call.turn_id != turn.id
            || (call.execution != crate::model::ToolCallExecution::Client.as_str()
                && !is_foreground_question)
            || call.status != crate::model::ToolCallStatus::Pending.as_str()
        {
            return Err(AgentError::Store(format!(
                "waiting turn {id} has an invalid client call"
            )));
        }
        let executor_lease_expired = call
            .client_lease_expires_at
            .is_some_and(|expires_at| expires_at <= now);
        if call.client_executor_id.is_none() || executor_lease_expired {
            cancel_unclaimed_call = Some((wait, call));
        }
    }

    if status == TurnRunStatus::WaitingForAgentRun
        && !super::super::multi_agent_run_wait::cancel_wait_set_for_turn_on(
            &transaction,
            id,
            &turn,
            now,
        )
        .await?
    {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }

    let next_status = match status {
        TurnRunStatus::Running => TurnRunStatus::Cancelling,
        TurnRunStatus::WaitingForClient if cancel_unclaimed_call.is_none() => {
            TurnRunStatus::CancellingClient
        }
        _ => TurnRunStatus::Cancelled,
    };
    match next_status {
        TurnRunStatus::Cancelling => {
            super::super::super::approval::reject_pending_for_cancelling_turn_on(
                &transaction,
                id,
                now,
            )
            .await?;
        }
        TurnRunStatus::Cancelled => {
            super::super::super::approval::close_pending_for_terminal_turn_on(
                &transaction,
                id,
                now,
            )
            .await?;
        }
        _ => {}
    }
    if let Some((wait, call)) = cancel_unclaimed_call {
        let cancelled_call = entities::tool_call::Entity::update_many()
            .col_expr(
                entities::tool_call::Column::Status,
                sea_orm::sea_query::Expr::value(crate::model::ToolCallStatus::Cancelled.as_str()),
            )
            .col_expr(
                entities::tool_call::Column::Result,
                sea_orm::sea_query::Expr::value(Some(
                    "cancelled before client execution".to_owned(),
                )),
            )
            .col_expr(
                entities::tool_call::Column::ResolvedAt,
                sea_orm::sea_query::Expr::value(Some(now)),
            )
            .col_expr(
                entities::tool_call::Column::ClientLeaseExpiresAt,
                sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
            )
            .filter(entities::tool_call::Column::Id.eq(call.id))
            .filter(
                entities::tool_call::Column::Status
                    .eq(crate::model::ToolCallStatus::Pending.as_str()),
            )
            .exec(&transaction)
            .await
            .map_err(store_err)?;
        let closed_wait = entities::turn_client_wait::Entity::update_many()
            .col_expr(
                entities::turn_client_wait::Column::Status,
                sea_orm::sea_query::Expr::value(
                    crate::model::TurnClientWaitStatus::Cancelled.as_str(),
                ),
            )
            .col_expr(
                entities::turn_client_wait::Column::ClosedAt,
                sea_orm::sea_query::Expr::value(Some(now)),
            )
            .filter(entities::turn_client_wait::Column::CallId.eq(wait.call_id))
            .filter(
                entities::turn_client_wait::Column::Status
                    .eq(crate::model::TurnClientWaitStatus::Waiting.as_str()),
            )
            .exec(&transaction)
            .await
            .map_err(store_err)?;
        if cancelled_call.rows_affected != 1 || closed_wait.rows_affected != 1 {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(None);
        }
        // The park's approval row closes with the call it waited on.
        super::super::super::approval::abandon_pending_for_call_on(
            &transaction,
            crate::CallId(call.id),
            now,
        )
        .await?;
    }
    let update = entities::turn::Entity::update_many()
        .col_expr(
            entities::turn::Column::Status,
            sea_orm::sea_query::Expr::value(next_status.as_str()),
        )
        .col_expr(
            entities::turn::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        );
    let update = if next_status == TurnRunStatus::Cancelled {
        update
            .col_expr(
                entities::turn::Column::EndedAt,
                sea_orm::sea_query::Expr::value(Some(now)),
            )
            .col_expr(
                entities::turn::Column::LastErrorCode,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
            .col_expr(
                entities::turn::Column::LastErrorDetail,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
    } else {
        update
    };
    let update = update
        .filter(entities::turn::Column::Id.eq(id.0))
        .filter(entities::turn::Column::Status.eq(&turn.status))
        .filter(entities::turn::Column::AttemptCount.eq(turn.attempt_count))
        .filter(entities::turn::Column::UpdatedAt.eq(turn.updated_at))
        .filter(entities::turn::Column::UpdatedAt.lte(now));
    let update = if status == TurnRunStatus::Running {
        update
            .filter(entities::turn::Column::LeaseToken.eq(turn.lease_token))
            .filter(entities::turn::Column::LeaseExpiresAt.eq(turn.lease_expires_at))
    } else {
        update
    };
    let cancelled = update.exec(&transaction).await.map_err(store_err)?;
    if cancelled.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    super::super::steer::reject_pending_turn_steers_on(&transaction, id, now).await?;
    let updated = entities::turn::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("cancelled turn {id} disappeared")))
        .and_then(turn_run_from_model)?;
    let sequenced_event = if next_status == TurnRunStatus::Cancelled {
        let event = if journal_terminal_event {
            Some(AgentEvent::TurnCancelled {
                usage: super::super::usage_from_turn_model(&turn)?,
            })
        } else {
            None
        };
        append_terminal_event_on(
            &transaction,
            id,
            SessionId(turn.session_id),
            None,
            event.as_ref(),
        )
        .await?
    } else {
        None
    };
    transaction.commit().await.map_err(store_err)?;
    let outcome = if matches!(
        next_status,
        TurnRunStatus::Cancelling | TurnRunStatus::CancellingClient
    ) {
        RequestTurnCancellationOutcome::Requested(updated)
    } else {
        RequestTurnCancellationOutcome::Cancelled(updated)
    };
    Ok(Some(JournaledTurnOutcome {
        outcome,
        terminal_event: sequenced_event,
    }))
}

pub(in crate::db) async fn finish_turn_cancellation(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
) -> Result<Option<FinishTurnCancellationOutcome>> {
    Ok(
        finish_turn_cancellation_inner(store, id, lease_token, now, None, None, None, &[])
            .await?
            .map(|resolution| resolution.outcome),
    )
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn finish_turn_cancellation_and_append_event(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
    model_steps: i32,
    usage: Usage,
    output: Option<&Message>,
    citations: &[crate::AssistantCitationInput],
) -> Result<Option<JournaledTurnOutcome<FinishTurnCancellationOutcome>>> {
    let event = AgentEvent::TurnCancelled { usage };
    finish_turn_cancellation_inner(
        store,
        id,
        lease_token,
        now,
        Some(model_steps),
        Some(&event),
        output,
        citations,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn finish_turn_cancellation_inner(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
    terminal_model_steps: Option<i32>,
    terminal_event: Option<&AgentEvent>,
    output: Option<&Message>,
    citations: &[crate::AssistantCitationInput],
) -> Result<Option<JournaledTurnOutcome<FinishTurnCancellationOutcome>>> {
    if lease_token.is_nil() {
        return Err(AgentError::Store("turn lease token must not be nil".into()));
    }
    // The prose a cancelled turn already streamed commits with the same
    // transition (#1182). An empty output is not a message.
    let output = output.filter(|output| !output.content.is_empty());
    if let Some(output) = output {
        validate_turn_output(id, lease_token, output)?;
        super::super::super::citation::validate_assistant_message(output, citations)?;
    }
    let requested_at = canonical_db_timestamp(now)?;
    let journal_chat_id = journal_chat_id(store, id, terminal_event.is_some()).await?;
    if terminal_event.is_some() && journal_chat_id.is_none() {
        return Ok(None);
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if let Some(chat_id) = journal_chat_id {
        if !acquire_chat_write_lock(&transaction, chat_id).await? {
            return Err(AgentError::Store(format!(
                "turn {id} references missing chat {chat_id}"
            )));
        }
    }
    if !acquire_turn_write_lock(&transaction, id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    // The cancellation request may have stamped `updated_at` from the
    // database clock. In SQLite that clock is deliberately rounded to the end
    // of the current millisecond, so a caller's `Utc::now()` from the same
    // millisecond can compare earlier even though this acknowledgement happens
    // afterwards. Resolve operational time after the locks, just as the
    // request path does, so the exact owning worker cannot lose to clock-source
    // granularity.
    let now = std::cmp::max(
        requested_at,
        super::super::super::agent_run::database_now(&transaction).await?,
    );
    let Some(claim) = entities::code_turn_claim::Entity::find_by_id(lease_token)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .filter(|claim| claim.turn_id == id.0)
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let Some(turn) = entities::turn::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if turn.status == TurnRunStatus::Cancelled.as_str()
        && turn.attempt_count == claim.attempt_count
        && turn.claim_count == claim.claim_count
    {
        if terminal_model_steps.is_some_and(|model_steps| model_steps != turn.model_steps) {
            return Err(AgentError::Store(
                "exact cancellation retry changed terminal model steps".into(),
            ));
        }
        if !exact_cancellation_output_on(&transaction, &turn, output, citations).await? {
            return Err(AgentError::Store(format!(
                "turn {id} was already cancelled with different output"
            )));
        }
        let sequenced_event =
            exact_terminal_event_on(&transaction, id, Some(lease_token), terminal_event).await?;
        let turn = turn_run_from_model(turn)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(JournaledTurnOutcome {
            outcome: FinishTurnCancellationOutcome::Existing(turn),
            terminal_event: sequenced_event,
        }));
    }
    if turn.status != TurnRunStatus::Cancelling.as_str()
        || turn.attempt_count != claim.attempt_count
        || turn.claim_count != claim.claim_count
        || turn.lease_token != Some(lease_token)
        || turn.updated_at.is_some_and(|updated_at| updated_at > now)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    if let Some(AgentEvent::TurnCancelled { usage }) = terminal_event {
        validate_terminal_usage(*usage, super::super::usage_from_turn_model(&turn)?)?;
    }
    if let Some(model_steps) = terminal_model_steps {
        validate_terminal_model_steps(model_steps, turn.model_steps)?;
    }

    super::super::super::approval::close_pending_for_terminal_turn_on(&transaction, id, now)
        .await?;

    // Commit the partial output under the cancelled turn before the status
    // flip so the message row and the terminal transition land atomically —
    // the same shape as a completed turn's output.
    let output_message_id = match output {
        Some(output) => {
            if output.chat_id.0 != turn.session_id {
                return Err(AgentError::Store(format!(
                    "turn {id} output belongs to a different chat"
                )));
            }
            if !reserve_message_identity_on(
                &transaction,
                output.id,
                output.chat_id,
                output.turn_id,
                MESSAGE_IDENTITY_OWNER_MESSAGE,
            )
            .await?
            {
                return Err(AgentError::Store(format!(
                    "turn output message identity {} is already reserved",
                    output.id
                )));
            }
            let message = entities::message::ActiveModel {
                id: Set(output.id.0),
                chat_id: Set(output.chat_id.0),
                turn_id: Set(output.turn_id.0),
                seq: Set(next_message_seq_on(&transaction, output.chat_id).await?),
                role: Set("assistant".into()),
                content: Set(output.content.clone()),
                llm_content: Set(output.llm_content.clone()),
                reasoning: Set(reasoning_to_db(&output.reasoning)),
                turn_lease_token: Set(Some(lease_token)),
                created_at: Set(canonical_db_timestamp(output.created_at)?),
            };
            message.insert(&transaction).await.map_err(store_err)?;
            super::super::super::citation::insert_for_message_on(&transaction, output, citations)
                .await?;
            Some(output.id.0)
        }
        None => None,
    };

    let mut cancelled = entities::turn::Entity::update_many()
        .col_expr(
            entities::turn::Column::Status,
            sea_orm::sea_query::Expr::value(TurnRunStatus::Cancelled.as_str()),
        )
        .col_expr(
            entities::turn::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::turn::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::turn::Column::EndedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .col_expr(
            entities::turn::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        );
    if let Some(message_id) = output_message_id {
        cancelled = cancelled.col_expr(
            entities::turn::Column::OutputMessageId,
            sea_orm::sea_query::Expr::value(Some(message_id)),
        );
    }
    // A cancelled turn spent tokens before it stopped, and until now they
    // reached the journal without ever reaching the row. That left the turn's
    // durable accounting reading zero for a stopped turn, so anything rebuilt
    // from storage — the transcript's context usage among them — understated
    // it. `validate_terminal_usage` above has already established that this
    // total covers the row's checkpoint, so the write stays monotonic.
    if let Some(AgentEvent::TurnCancelled { usage }) = terminal_event {
        cancelled = cancelled
            .col_expr(
                entities::turn::Column::InputTokens,
                sea_orm::sea_query::Expr::value(i64::from(usage.input_tokens)),
            )
            .col_expr(
                entities::turn::Column::OutputTokens,
                sea_orm::sea_query::Expr::value(i64::from(usage.output_tokens)),
            )
            .col_expr(
                entities::turn::Column::CacheReadInputTokens,
                sea_orm::sea_query::Expr::value(i64::from(usage.cache_read_input_tokens)),
            )
            .col_expr(
                entities::turn::Column::CacheCreationInputTokens,
                sea_orm::sea_query::Expr::value(i64::from(usage.cache_creation_input_tokens)),
            );
    }
    if let Some(model_steps) = terminal_model_steps {
        cancelled = cancelled.col_expr(
            entities::turn::Column::ModelSteps,
            sea_orm::sea_query::Expr::value(model_steps),
        );
    }
    let cancelled = cancelled
        .filter(entities::turn::Column::Id.eq(id.0))
        .filter(entities::turn::Column::Status.eq(TurnRunStatus::Cancelling.as_str()))
        .filter(entities::turn::Column::AttemptCount.eq(claim.attempt_count))
        .filter(entities::turn::Column::ClaimCount.eq(claim.claim_count))
        .filter(entities::turn::Column::LeaseToken.eq(lease_token))
        .filter(entities::turn::Column::LeaseExpiresAt.eq(turn.lease_expires_at))
        .filter(entities::turn::Column::UpdatedAt.eq(turn.updated_at))
        .filter(entities::turn::Column::UpdatedAt.lte(now))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if cancelled.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    super::super::steer::reject_pending_turn_steers_on(&transaction, id, now).await?;
    let cancelled = entities::turn::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("cancelled turn {id} disappeared")))
        .and_then(turn_run_from_model)?;
    let sequenced_event = append_terminal_event_on(
        &transaction,
        id,
        SessionId(turn.session_id),
        Some(lease_token),
        terminal_event,
    )
    .await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(JournaledTurnOutcome {
        outcome: FinishTurnCancellationOutcome::Cancelled(cancelled),
        terminal_event: sequenced_event,
    }))
}

async fn exact_cancellation_output_on<C>(
    conn: &C,
    turn: &entities::turn::Model,
    output: Option<&Message>,
    citations: &[crate::AssistantCitationInput],
) -> Result<bool>
where
    C: ConnectionTrait,
{
    match (turn.output_message_id, output) {
        (None, None) => Ok(citations.is_empty()),
        (Some(message_id), Some(output)) if message_id == output.id.0 => {
            Ok(exact_completed_output_on(conn, output).await?
                && exact_completed_citations_on(conn, output, citations).await?)
        }
        _ => Ok(false),
    }
}

async fn existing_cancellation_event_on<C>(
    conn: &C,
    id: TurnId,
    expected: bool,
) -> Result<Option<SequencedAgentEvent>>
where
    C: ConnectionTrait,
{
    if !expected {
        return Ok(None);
    }
    let stored = entities::event::Entity::find()
        .filter(entities::event::Column::TurnId.eq(id.0))
        .filter(entities::event::Column::Terminal.eq(true))
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("cancelled turn {id} is missing its event")))?;
    let event = crate::chat_journal::decode_chat_event_required(stored.event)?;
    if !matches!(event, AgentEvent::TurnCancelled { .. }) {
        return Err(AgentError::Store(format!(
            "cancelled turn {id} has a different terminal event"
        )));
    }
    Ok(Some(SequencedAgentEvent {
        seq: stored.seq,
        event,
    }))
}
