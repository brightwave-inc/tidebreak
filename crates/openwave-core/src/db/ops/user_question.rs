use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::model::{ToolCallExecution, ToolCallStatus, TurnRunStatus};
use crate::storage::AnswerUserQuestionsOutcome;
use crate::{
    AnswerUserQuestionsRequest, AskUserQuestionsArgs, CallId, ChatId, PendingUserQuestions, TurnId,
    UserQuestion, UserQuestionRequestStatus, ASK_USER_QUESTIONS_TOOL,
};

use super::super::{entities, store_err, DbStore};
use super::turn::{canonical_db_timestamp, recover_turn_after_client_resolution_on};
use super::{acquire_chat_write_lock, acquire_tool_call_write_lock, acquire_turn_write_lock};

/// Commit the bounded renderer projection and its journal refresh hint inside
/// the caller's already-fenced client-wait transaction.
pub(in crate::db) async fn checkpoint_on<C>(
    conn: &C,
    call: &crate::ClientToolCallRequest,
    asked_at: DateTime<Utc>,
) -> Result<Option<SequencedEvent>>
where
    C: ConnectionTrait,
{
    if call.name != ASK_USER_QUESTIONS_TOOL {
        return Ok(None);
    }
    let arguments = parse_arguments(&call.arguments)?;
    let event = AgentEvent::UserQuestionsAsked {
        call_id: call.id,
        turn_id: call.turn_id,
    };
    let seq =
        super::conversation::append_event_on(conn, call.chat_id, None, None, None, None, &event)
            .await?;
    entities::user_question_request::ActiveModel {
        call_id: Set(call.id.0),
        turn_id: Set(call.turn_id.0),
        chat_id: Set(call.chat_id.0),
        status: Set(UserQuestionRequestStatus::Pending.as_str().into()),
        event_seq: Set(seq),
        asked_at: Set(asked_at),
        resolved_at: Set(None),
        additional_user_context: Set(None),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    for (position, question) in arguments.questions.into_iter().enumerate() {
        entities::user_question::ActiveModel {
            call_id: Set(call.id.0),
            question_id: Set(question.id),
            position: Set(i32::try_from(position)
                .map_err(|_| AgentError::Store("question position overflowed".into()))?),
            header: Set(question.header),
            prompt: Set(question.question),
            options: Set(serde_json::to_value(question.options)?),
            question_type: Set(question.question_type.as_str().into()),
            allow_free_form: Set(question.allow_free_form),
            answer_selected_option_ids: Set(None),
            answer_custom_answer: Set(None),
            response_recorded_at: Set(None),
        }
        .insert(conn)
        .await
        .map_err(store_err)?;
    }
    Ok(Some(SequencedEvent { seq, event }))
}

/// Recover and validate the exact committed renderer hint for an ambiguous
/// checkpoint retry.
pub(in crate::db) async fn recover_checkpoint_on<C>(
    conn: &C,
    call: &crate::ClientToolCallRequest,
) -> Result<Option<SequencedEvent>>
where
    C: ConnectionTrait,
{
    if call.name != ASK_USER_QUESTIONS_TOOL {
        return Ok(None);
    }
    let expected = parse_arguments(&call.arguments)?;
    let request = entities::user_question_request::Entity::find_by_id(call.id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "question checkpoint {} is missing its renderer receipt",
                call.id
            ))
        })?;
    if request.chat_id != call.chat_id.0 || request.turn_id != call.turn_id.0 {
        return Err(AgentError::Store(format!(
            "question checkpoint {} has mismatched scope",
            call.id
        )));
    }
    if request.status != UserQuestionRequestStatus::Pending.as_str() {
        if request.status == UserQuestionRequestStatus::Answered.as_str()
            || request.status == UserQuestionRequestStatus::Cancelled.as_str()
        {
            return Ok(None);
        }
        return Err(AgentError::Store(format!(
            "question checkpoint {} has unknown status {}",
            call.id, request.status
        )));
    }
    let questions = question_models(conn, call.id).await?;
    if questions_from_models(&questions)? != expected.questions {
        return Err(AgentError::Store(format!(
            "question checkpoint {} has mismatched presentation data",
            call.id
        )));
    }
    let stored = entities::event::Entity::find_by_id((call.chat_id.0, request.event_seq))
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("question renderer event is missing".into()))?;
    let event = serde_json::from_value::<AgentEvent>(stored.payload)?;
    let expected_event = AgentEvent::UserQuestionsAsked {
        call_id: call.id,
        turn_id: call.turn_id,
    };
    if stored.turn_id.is_some() || stored.terminal || event != expected_event {
        return Err(AgentError::Store(
            "question renderer event does not match its checkpoint".into(),
        ));
    }
    Ok(Some(SequencedEvent {
        seq: request.event_seq,
        event,
    }))
}

pub(in crate::db) async fn list_pending(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<PendingUserQuestions>> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(Vec::new());
    }
    let requests = entities::user_question_request::Entity::find()
        .filter(entities::user_question_request::Column::ChatId.eq(chat_id.0))
        .filter(
            entities::user_question_request::Column::Status
                .eq(UserQuestionRequestStatus::Pending.as_str()),
        )
        .order_by_asc(entities::user_question_request::Column::AskedAt)
        .order_by_asc(entities::user_question_request::Column::CallId)
        .all(&transaction)
        .await
        .map_err(store_err)?;
    let mut pending = Vec::with_capacity(requests.len());
    for request in requests {
        let call = entities::tool_call::Entity::find_by_id(request.call_id)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store("pending question call is missing".into()))?;
        let wait = entities::turn_client_wait::Entity::find_by_id(request.call_id)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store("pending question wait is missing".into()))?;
        let turn = entities::turn_run::Entity::find_by_id(request.turn_id)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store("pending question turn is missing".into()))?;
        if call.chat_id != request.chat_id
            || call.turn_id != request.turn_id
            || call.name != ASK_USER_QUESTIONS_TOOL
            || call.execution != ToolCallExecution::Orchestration.as_str()
            || call.status != ToolCallStatus::Pending.as_str()
            || call.client_executor_id.is_some()
            || wait.chat_id != request.chat_id
            || wait.turn_id != request.turn_id
            || wait.status != crate::TurnClientWaitStatus::Waiting.as_str()
            || turn.chat_id != request.chat_id
            || turn.status != TurnRunStatus::WaitingForClient.as_str()
            || turn.attempt_count != wait.attempt_count
            || turn.claim_count != wait.claim_count
        {
            return Err(AgentError::Store(
                "pending question projection does not match its live continuation".into(),
            ));
        }
        let questions =
            questions_from_models(&question_models(&transaction, CallId(request.call_id)).await?)?;
        pending.push(PendingUserQuestions {
            call_id: CallId(request.call_id),
            turn_id: TurnId(request.turn_id),
            questions,
            asked_at: request.asked_at,
        });
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(pending)
}

pub(in crate::db) async fn answer(
    store: &DbStore,
    request: &AnswerUserQuestionsRequest,
    answered_at: DateTime<Utc>,
) -> Result<AnswerUserQuestionsOutcome> {
    if request.chat_id.0.is_nil()
        || request.call_id.0.is_nil()
        || !request.answers.shape_is_well_formed()
    {
        return Ok(AnswerUserQuestionsOutcome::InvalidAnswer);
    }
    let requested_at = canonical_db_timestamp(answered_at)?;
    let Some(scope) = entities::user_question_request::Entity::find_by_id(request.call_id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(AnswerUserQuestionsOutcome::Unavailable);
    };
    if scope.chat_id != request.chat_id.0 {
        return Ok(AnswerUserQuestionsOutcome::Unavailable);
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, request.chat_id).await?
        || !acquire_turn_write_lock(&transaction, TurnId(scope.turn_id)).await?
        || !acquire_tool_call_write_lock(&transaction, request.call_id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(AnswerUserQuestionsOutcome::Unavailable);
    }
    let call = entities::tool_call::Entity::find_by_id(request.call_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked question call exists");
    if call.chat_id != request.chat_id.0
        || call.name != ASK_USER_QUESTIONS_TOOL
        || call.execution != ToolCallExecution::Orchestration.as_str()
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(AnswerUserQuestionsOutcome::Unavailable);
    }
    let question_request = entities::user_question_request::Entity::find_by_id(request.call_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "question call {} is missing its request",
                request.call_id
            ))
        })?;
    if question_request.chat_id != request.chat_id.0 || question_request.turn_id != call.turn_id {
        transaction.commit().await.map_err(store_err)?;
        return Ok(AnswerUserQuestionsOutcome::Unavailable);
    }
    let questions = question_models(&transaction, request.call_id).await?;
    let Some(canonical) = canonical_answers(&questions, &request.answers)? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(AnswerUserQuestionsOutcome::InvalidAnswer);
    };
    let result = serde_json::to_string(&canonical)?;

    if question_request.status == UserQuestionRequestStatus::Answered.as_str() {
        let result_matches = if questions
            .iter()
            .all(|question| question.response_recorded_at.is_some())
        {
            call.result.as_deref() == Some(result.as_str())
        } else {
            // Before the extension, the result used scalar `option_id` /
            // `free_form` fields. The compatibility columns below still prove
            // the semantic answer exactly; requiring byte-identical new JSON
            // would turn an equivalent retry after upgrade into a conflict.
            call.result.is_some()
        };
        let exact = stored_answers_match(&question_request, &questions, &canonical)?
            && call.status == ToolCallStatus::Completed.as_str()
            && result_matches;
        if !exact {
            transaction.commit().await.map_err(store_err)?;
            return Ok(AnswerUserQuestionsOutcome::AnswerConflict);
        }
        let transition = recover_turn_after_client_resolution_on(&transaction, &call)
            .await?
            .ok_or_else(|| {
                AgentError::Store(format!(
                    "answered question {} is missing its client wait",
                    request.call_id
                ))
            })?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(AnswerUserQuestionsOutcome::Existing(transition.turn));
    }
    if question_request.status != UserQuestionRequestStatus::Pending.as_str()
        || call.status != ToolCallStatus::Pending.as_str()
        || call.client_executor_id.is_some()
        || call.client_lease_token.is_some()
        || call.client_lease_expires_at.is_some()
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(AnswerUserQuestionsOutcome::Unavailable);
    }
    let turn = entities::turn_run::Entity::find_by_id(call.turn_id)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked question turn exists");
    if turn.chat_id != request.chat_id.0
        || turn.status != TurnRunStatus::WaitingForClient.as_str()
        || question_request.turn_id != turn.id
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(AnswerUserQuestionsOutcome::Unavailable);
    }
    let database_now = super::agent_run::database_now(&transaction).await?;
    let answered_at = requested_at
        .max(database_now)
        .max(question_request.asked_at)
        .max(call.created_at)
        .max(turn.updated_at);

    let answers_by_id: HashMap<_, _> = canonical
        .answers
        .iter()
        .map(|answer| (answer.question_id.as_str(), answer))
        .collect();
    for model in &questions {
        let answer = answers_by_id.get(model.question_id.as_str());
        let mut active: entities::user_question::ActiveModel = model.clone().into();
        active.answer_selected_option_ids = Set(Some(serde_json::to_value(
            answer
                .map(|answer| answer.selected_option_ids.as_slice())
                .unwrap_or_default(),
        )?));
        active.answer_custom_answer = Set(answer.and_then(|answer| answer.custom_answer.clone()));
        active.response_recorded_at = Set(Some(answered_at));
        active.update(&transaction).await.map_err(store_err)?;
    }
    let mut active_request: entities::user_question_request::ActiveModel = question_request.into();
    active_request.status = Set(UserQuestionRequestStatus::Answered.as_str().into());
    active_request.resolved_at = Set(Some(answered_at));
    active_request.additional_user_context = Set(canonical.additional_user_context.clone());
    active_request
        .update(&transaction)
        .await
        .map_err(store_err)?;

    let mut active_call: entities::tool_call::ActiveModel = call.into();
    active_call.status = Set(ToolCallStatus::Completed.as_str().into());
    active_call.result = Set(Some(result));
    // The recap card reads this column on history rehydration, the same way
    // every other settled card does. Without it, reopening the chat showed the
    // answered question as a bare rail line.
    let preview = answers_preview(&questions, &canonical)?;
    active_call.result_preview = Set(Some(serde_json::to_value(&preview)?));
    active_call.error_code = Set(None);
    active_call.error_detail = Set(None);
    active_call.resolved_at = Set(Some(answered_at));
    let resolved = active_call.update(&transaction).await.map_err(store_err)?;
    // The question call resolves outside the agent loop, so nothing else ever
    // announces that it finished: the resumed worker reads the committed
    // result straight into the model transcript and never revisits the call.
    // Without this event the renderer showed the card waiting from
    // `ToolCallStarted` until the turn's terminal hydration finally settled
    // it — the answer looked lost exactly when it had committed.
    //
    // Journaled here, in the transaction that makes the row terminal, so the
    // event cannot disagree with the row it describes. An exact retry returns
    // above as `Existing` without reaching this, so the call announces itself
    // once. Chat-scoped like its `UserQuestionsAsked`: the turn is parked
    // with no lease, so no attempt owns this event.
    let completion_event = AgentEvent::ToolCallCompleted {
        call_id: request.call_id,
        output: crate::ToolOutput::text(resolved.result.clone().ok_or_else(|| {
            AgentError::Store(format!(
                "answered question {} committed no result",
                request.call_id
            ))
        })?),
        action: crate::ToolActionPreview::build(&resolved.name, &resolved.arguments),
        result: Some(preview),
    };
    let seq = super::conversation::append_event_on(
        &transaction,
        ChatId(resolved.chat_id),
        None,
        None,
        None,
        None,
        &completion_event,
    )
    .await?;
    let transition =
        super::turn::advance_turn_after_client_resolution_on(&transaction, &resolved, answered_at)
            .await?
            .ok_or_else(|| {
                AgentError::Store(format!(
                    "answered question {} is missing its client wait",
                    request.call_id
                ))
            })?;
    transaction.commit().await.map_err(store_err)?;
    Ok(AnswerUserQuestionsOutcome::Answered {
        turn: transition.turn,
        completion_event: Box::new(SequencedEvent {
            seq,
            event: completion_event,
        }),
    })
}

/// Close presentation state when cancellation terminalizes the unclaimed
/// question call through the shared client-wait state machine.
pub(in crate::db) async fn cancel_for_call_on<C>(
    conn: &C,
    call_id: CallId,
    cancelled_at: DateTime<Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let Some(request) = entities::user_question_request::Entity::find_by_id(call_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(());
    };
    if request.status != UserQuestionRequestStatus::Pending.as_str() {
        return Ok(());
    }
    let mut active: entities::user_question_request::ActiveModel = request.into();
    active.status = Set(UserQuestionRequestStatus::Cancelled.as_str().into());
    active.resolved_at = Set(Some(cancelled_at));
    active.update(conn).await.map_err(store_err)?;
    Ok(())
}

pub(in crate::db) async fn close_pending_for_terminal_turn_on<C>(
    conn: &C,
    turn_id: TurnId,
    terminal_at: DateTime<Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let requests = entities::user_question_request::Entity::find()
        .filter(entities::user_question_request::Column::TurnId.eq(turn_id.0))
        .filter(
            entities::user_question_request::Column::Status
                .eq(UserQuestionRequestStatus::Pending.as_str()),
        )
        .order_by_asc(entities::user_question_request::Column::CallId)
        .all(conn)
        .await
        .map_err(store_err)?;
    for request in requests {
        let resolved_at = terminal_at.max(request.asked_at);
        let mut active: entities::user_question_request::ActiveModel = request.into();
        active.status = Set(UserQuestionRequestStatus::Cancelled.as_str().into());
        active.resolved_at = Set(Some(resolved_at));
        active.update(conn).await.map_err(store_err)?;
    }
    Ok(())
}

fn parse_arguments(value: &serde_json::Value) -> Result<AskUserQuestionsArgs> {
    let arguments: AskUserQuestionsArgs = serde_json::from_value(value.clone())?;
    if !arguments.is_well_formed() {
        return Err(AgentError::Store(
            "invalid durable user-question arguments".into(),
        ));
    }
    Ok(arguments)
}

async fn question_models<C>(
    conn: &C,
    call_id: CallId,
) -> Result<Vec<entities::user_question::Model>>
where
    C: ConnectionTrait,
{
    entities::user_question::Entity::find()
        .filter(entities::user_question::Column::CallId.eq(call_id.0))
        .order_by_asc(entities::user_question::Column::Position)
        .all(conn)
        .await
        .map_err(store_err)
}

fn questions_from_models(models: &[entities::user_question::Model]) -> Result<Vec<UserQuestion>> {
    if models.is_empty() || models.len() > crate::MAX_USER_QUESTIONS {
        return Err(AgentError::Store(
            "durable question request has an invalid question count".into(),
        ));
    }
    models
        .iter()
        .enumerate()
        .map(|(position, model)| {
            if model.position
                != i32::try_from(position)
                    .map_err(|_| AgentError::Store("question position overflowed".into()))?
            {
                return Err(AgentError::Store(
                    "durable question request has non-canonical ordering".into(),
                ));
            }
            let question = UserQuestion {
                id: model.question_id.clone(),
                header: model.header.clone(),
                question: model.prompt.clone(),
                options: serde_json::from_value(model.options.clone())?,
                question_type: match model.question_type.as_str() {
                    "single_select" => crate::UserQuestionType::SingleSelect,
                    "multi_select" => crate::UserQuestionType::MultiSelect,
                    _ => {
                        return Err(AgentError::Store(
                            "durable question has an unknown question type".into(),
                        ));
                    }
                },
                allow_free_form: model.allow_free_form,
            };
            if !question.is_well_formed() {
                return Err(AgentError::Store(
                    "durable question presentation is malformed".into(),
                ));
            }
            Ok(question)
        })
        .collect()
}

fn canonical_answers(
    questions: &[entities::user_question::Model],
    supplied: &crate::AnswerUserQuestions,
) -> Result<Option<crate::AnswerUserQuestions>> {
    let by_id: HashMap<_, _> = supplied
        .answers
        .iter()
        .map(|answer| (answer.question_id.as_str(), answer))
        .collect();
    let mut answers = Vec::with_capacity(supplied.answers.len());
    for question in questions {
        let Some(answer) = by_id.get(question.question_id.as_str()) else {
            continue;
        };
        let options: Vec<crate::UserQuestionOption> =
            serde_json::from_value(question.options.clone())?;
        let selections_are_valid = answer
            .selected_option_ids
            .iter()
            .all(|option_id| options.iter().any(|option| option.id == *option_id));
        let custom_is_valid = answer.custom_answer.is_none() || question.allow_free_form;
        let selection_shape_is_valid = match question.question_type.as_str() {
            "single_select" => {
                (answer.selected_option_ids.len() == 1 && answer.custom_answer.is_none())
                    || (answer.selected_option_ids.is_empty() && answer.custom_answer.is_some())
            }
            "multi_select" => true,
            _ => false,
        };
        if !selections_are_valid || !custom_is_valid || !selection_shape_is_valid {
            return Ok(None);
        }
        answers.push((*answer).clone());
    }
    if answers.len() != supplied.answers.len() {
        return Ok(None);
    }
    Ok(Some(crate::AnswerUserQuestions {
        answers,
        additional_user_context: supplied.additional_user_context.clone(),
    }))
}

/// Project the settled recap card from the questions and the exact answers.
///
/// Option *labels*, not ids: the recap is read by a person, and the ids are an
/// internal handle they never saw. Every question is listed, answered or not,
/// so the card can say which ones were skipped rather than quietly omitting
/// them.
fn answers_preview(
    questions: &[entities::user_question::Model],
    canonical: &crate::AnswerUserQuestions,
) -> Result<crate::ToolResultPreview> {
    let by_id: HashMap<_, _> = canonical
        .answers
        .iter()
        .map(|answer| (answer.question_id.as_str(), answer))
        .collect();
    let mut answers = Vec::with_capacity(questions.len());
    for question in questions {
        let answer = by_id.get(question.question_id.as_str());
        let options: Vec<crate::UserQuestionOption> =
            serde_json::from_value(question.options.clone())?;
        let selected = options
            .into_iter()
            .filter(|option| {
                answer.is_some_and(|answer| answer.selected_option_ids.contains(&option.id))
            })
            .map(|option| option.label)
            .collect();
        answers.push(crate::AnsweredUserQuestion {
            question: question.prompt.clone(),
            selected,
            custom_answer: answer.and_then(|answer| answer.custom_answer.clone()),
        });
    }
    Ok(crate::ToolResultPreview::UserQuestions {
        answers,
        additional_context: canonical.additional_user_context.clone(),
    })
}

fn stored_answers_match(
    request: &entities::user_question_request::Model,
    questions: &[entities::user_question::Model],
    expected: &crate::AnswerUserQuestions,
) -> Result<bool> {
    if request.additional_user_context != expected.additional_user_context {
        return Ok(false);
    }
    let expected_by_id: HashMap<_, _> = expected
        .answers
        .iter()
        .map(|answer| (answer.question_id.as_str(), answer))
        .collect();
    for question in questions {
        let expected_answer = expected_by_id.get(question.question_id.as_str());
        if question.response_recorded_at.is_some() {
            let stored_option_ids: Vec<String> = question
                .answer_selected_option_ids
                .clone()
                .map(serde_json::from_value)
                .transpose()?
                .unwrap_or_default();
            if stored_option_ids
                != expected_answer
                    .map(|answer| answer.selected_option_ids.clone())
                    .unwrap_or_default()
                || question.answer_custom_answer
                    != expected_answer.and_then(|answer| answer.custom_answer.clone())
            {
                return Ok(false);
            }
            continue;
        }

        // Answering stamps every question row, so an unstamped row means this
        // request is not the one already recorded.
        return Ok(false);
    }
    Ok(true)
}
