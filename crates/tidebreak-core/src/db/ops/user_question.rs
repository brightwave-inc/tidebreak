//! The questions card as an approval row (decision 0048 step 5).
//!
//! `ask_user_questions` parks its turn on a `code_approval` row whose kind is
//! [`CodeApprovalKind::Questions`] and whose id is the call id. The answers
//! settle the row as [`ApprovalDecisionKind::Answered`] and complete the
//! call, so the chat's answer route and the session decision route land on
//! the same row and the same journal rows.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sea_orm::{ActiveModelTrait, ConnectionTrait, EntityTrait, Set, TransactionTrait};

use crate::code::{
    ApprovalDecisionKind, CodeApproval, CodeApprovalId, CodeApprovalKind, CodeApprovalState,
    CodeSessionId, CodeTurnId,
};
use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::model::{OwnerId, ToolCallExecution, ToolCallStatus, TurnRunStatus};
use crate::storage::AnswerUserQuestionsOutcome;
use crate::{
    AnswerUserQuestionsRequest, AskUserQuestionsArgs, CallId, ChatId, PendingUserQuestions, TurnId,
    UserQuestion, ASK_USER_QUESTIONS_TOOL,
};

use super::super::{entities, store_err, DbStore};
use super::approval::{claim_of, session_row, settle_row_on};
use super::code::approval::{find_approval_row_on, insert_approval_on};
use super::turn::{canonical_db_timestamp, recover_turn_after_client_resolution_on};
use super::{acquire_chat_write_lock, acquire_tool_call_write_lock, acquire_turn_write_lock};

/// Commit the park's approval row and its journal row inside the caller's
/// already-fenced client-wait transaction.
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
    let session = session_row(conn, call.chat_id).await?;
    let owner = OwnerId::new(&session.owner)?;
    insert_approval_on(
        conn,
        &owner,
        &CodeApproval {
            id: CodeApprovalId(call.id.0),
            session_id: CodeSessionId(call.chat_id.0),
            turn_id: CodeTurnId(call.turn_id.0),
            kind: CodeApprovalKind::Questions {
                questions: arguments.questions,
            },
            harness_raw: serde_json::Value::Null,
            native_call_id: Some(call.id.to_string()),
            server_capability: None,
            request_sha256: None,
            worker_epoch: Some(session.spawn_epoch),
            decision_claim: None,
            claimed_at: None,
            state: CodeApprovalState::Pending,
            feedback: None,
            requested_at: asked_at,
            decided_at: None,
            auto_judge_status: None,
        },
    )
    .await?;
    Ok(Some(SequencedEvent { seq, event }))
}

/// Recover and validate the exact committed park for an ambiguous checkpoint
/// retry: the row must carry the same questions, and the journal row that
/// announced it is returned when the row is still pending.
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
    let row = find_approval_row_on(conn, CodeApprovalId(call.id.0))
        .await?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "question checkpoint {} is missing its approval row",
                call.id
            ))
        })?;
    if row.session_id != call.chat_id.0 || row.turn_id != call.turn_id.0 {
        return Err(AgentError::Store(format!(
            "question checkpoint {} has mismatched scope",
            call.id
        )));
    }
    if questions_of(&row)? != expected.questions {
        return Err(AgentError::Store(format!(
            "question checkpoint {} has mismatched presentation data",
            call.id
        )));
    }
    if row.state != CodeApprovalState::Pending.as_str() {
        return Ok(None);
    }
    let expected_event = AgentEvent::UserQuestionsAsked {
        call_id: call.id,
        turn_id: call.turn_id,
    };
    super::plan::park_request_receipt_on(conn, call.chat_id, &expected_event).await
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
    let rows = super::plan::pending_park_rows_on(&transaction, chat_id).await?;
    let mut pending = Vec::with_capacity(rows.len());
    for row in rows {
        let Ok(questions) = questions_of(&row) else {
            continue;
        };
        let call = entities::tool_call::Entity::find_by_id(row.id)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store("pending question call is missing".into()))?;
        let wait = entities::turn_client_wait::Entity::find_by_id(row.id)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store("pending question wait is missing".into()))?;
        let turn = entities::code_turn::Entity::find_by_id(row.turn_id)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store("pending question turn is missing".into()))?;
        if call.chat_id != row.session_id
            || call.turn_id != row.turn_id
            || call.name != ASK_USER_QUESTIONS_TOOL
            || call.execution != ToolCallExecution::Orchestration.as_str()
            || call.status != ToolCallStatus::Pending.as_str()
            || call.client_executor_id.is_some()
            || wait.session_id != row.session_id
            || wait.turn_id != row.turn_id
            || wait.status != crate::TurnClientWaitStatus::Waiting.as_str()
            || turn.session_id != row.session_id
            || turn.status != TurnRunStatus::WaitingForClient.as_str()
            || turn.attempt_count != wait.attempt_count
            || turn.claim_count != wait.claim_count
        {
            return Err(AgentError::Store(
                "pending question projection does not match its live continuation".into(),
            ));
        }
        pending.push(PendingUserQuestions {
            call_id: CallId(row.id),
            turn_id: TurnId(row.turn_id),
            questions,
            asked_at: row.requested_at,
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
    let Some(scope) = find_approval_row_on(&store.conn, CodeApprovalId(request.call_id.0)).await?
    else {
        return Ok(AnswerUserQuestionsOutcome::Unavailable);
    };
    if scope.session_id != request.chat_id.0 {
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
    let row = find_approval_row_on(&transaction, CodeApprovalId(request.call_id.0))
        .await?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "question call {} is missing its approval row",
                request.call_id
            ))
        })?;
    if row.session_id != request.chat_id.0 || row.turn_id != call.turn_id {
        transaction.commit().await.map_err(store_err)?;
        return Ok(AnswerUserQuestionsOutcome::Unavailable);
    }
    let Ok(questions) = questions_of(&row) else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(AnswerUserQuestionsOutcome::Unavailable);
    };
    let Some(canonical) = canonical_answers(&questions, &request.answers)? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(AnswerUserQuestionsOutcome::InvalidAnswer);
    };
    let result = serde_json::to_string(&canonical)?;

    if row.state == CodeApprovalState::Approved.as_str() {
        let exact = call.status == ToolCallStatus::Completed.as_str()
            && call.result.as_deref() == Some(result.as_str());
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
    if row.state != CodeApprovalState::Pending.as_str()
        || call.status != ToolCallStatus::Pending.as_str()
        || call.client_executor_id.is_some()
        || call.client_lease_token.is_some()
        || call.client_lease_expires_at.is_some()
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(AnswerUserQuestionsOutcome::Unavailable);
    }
    let turn = entities::code_turn::Entity::find_by_id(call.turn_id)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked question turn exists");
    if turn.session_id != request.chat_id.0
        || turn.status != TurnRunStatus::WaitingForClient.as_str()
        || row.turn_id != turn.id
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(AnswerUserQuestionsOutcome::Unavailable);
    }
    let database_now = super::agent_run::database_now(&transaction).await?;
    let answered_at = requested_at
        .max(database_now)
        .max(row.requested_at)
        .max(call.created_at)
        .max(turn.updated_at.unwrap_or(database_now));

    // The row settles first, in the same transaction as the call: the
    // decision route and this one both land here, so the card is decided
    // exactly once whichever surface answered it.
    let settlement = settle_row_on(
        &transaction,
        &row,
        claim_of(&row),
        ApprovalDecisionKind::Answered {
            answers: canonical.answers.clone(),
        },
        answered_at,
    )
    .await?
    .ok_or_else(|| {
        AgentError::Store(format!("question {} could not be settled", request.call_id))
    })?;

    let mut active_call: entities::tool_call::ActiveModel = call.into();
    active_call.status = Set(ToolCallStatus::Completed.as_str().into());
    active_call.result = Set(Some(result));
    // The recap card reads this column on history rehydration, the same way
    // every other settled card does. Without it, reopening the chat showed the
    // answered question as a bare rail line.
    let preview = answers_preview(&questions, &canonical);
    active_call.result_preview = Set(Some(serde_json::to_value(&preview)?));
    active_call.error_code = Set(None);
    active_call.error_detail = Set(None);
    active_call.resolved_at = Set(Some(answered_at));
    let resolved = active_call.update(&transaction).await.map_err(store_err)?;
    // The question call resolves outside the agent loop, so nothing else ever
    // announces that it finished: the resumed worker reads the committed
    // result straight into the model transcript and never revisits the call.
    // Journaled here, in the transaction that makes the row terminal, so the
    // event cannot disagree with the row it describes. Chat-scoped like its
    // request: the turn is parked with no lease, so no attempt owns it.
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
        resolution: Box::new(settlement.event),
    })
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

/// The questions a row carries, or an error for a row of another kind.
fn questions_of(row: &entities::code_approval::Model) -> Result<Vec<UserQuestion>> {
    match serde_json::from_value::<CodeApprovalKind>(row.kind.clone())? {
        CodeApprovalKind::Questions { questions } => {
            if questions.is_empty()
                || questions.len() > crate::MAX_USER_QUESTIONS
                || !questions.iter().all(UserQuestion::is_well_formed)
            {
                return Err(AgentError::Store(
                    "durable question request is malformed".into(),
                ));
            }
            Ok(questions)
        }
        _ => Err(AgentError::Store(format!(
            "approval {} is not a questions card",
            row.id
        ))),
    }
}

fn canonical_answers(
    questions: &[UserQuestion],
    supplied: &crate::AnswerUserQuestions,
) -> Result<Option<crate::AnswerUserQuestions>> {
    let by_id: HashMap<_, _> = supplied
        .answers
        .iter()
        .map(|answer| (answer.question_id.as_str(), answer))
        .collect();
    let mut answers = Vec::with_capacity(supplied.answers.len());
    for question in questions {
        let Some(answer) = by_id.get(question.id.as_str()) else {
            continue;
        };
        let selections_are_valid = answer.selected_option_ids.iter().all(|option_id| {
            question
                .options
                .iter()
                .any(|option| option.id == *option_id)
        });
        let custom_is_valid = answer.custom_answer.is_none() || question.allow_free_form;
        let selection_shape_is_valid = match question.question_type {
            crate::UserQuestionType::SingleSelect => {
                (answer.selected_option_ids.len() == 1 && answer.custom_answer.is_none())
                    || (answer.selected_option_ids.is_empty() && answer.custom_answer.is_some())
            }
            crate::UserQuestionType::MultiSelect => true,
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
    questions: &[UserQuestion],
    canonical: &crate::AnswerUserQuestions,
) -> crate::ToolResultPreview {
    let by_id: HashMap<_, _> = canonical
        .answers
        .iter()
        .map(|answer| (answer.question_id.as_str(), answer))
        .collect();
    let answers = questions
        .iter()
        .map(|question| {
            let answer = by_id.get(question.id.as_str());
            let selected = question
                .options
                .iter()
                .filter(|option| {
                    answer.is_some_and(|answer| answer.selected_option_ids.contains(&option.id))
                })
                .map(|option| option.label.clone())
                .collect();
            crate::AnsweredUserQuestion {
                question: question.question.clone(),
                selected,
                custom_answer: answer.and_then(|answer| answer.custom_answer.clone()),
            }
        })
        .collect();
    crate::ToolResultPreview::UserQuestions {
        answers,
        additional_context: canonical.additional_user_context.clone(),
    }
}
