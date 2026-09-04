//! Renderer-safe foreground question recovery and exact answer submission.

use axum::extract::State;
use chrono::Utc;
use serde::Serialize;
use tidebreak_core::{
    AnswerUserQuestions, AnswerUserQuestionsOutcome, AnswerUserQuestionsRequest, ApprovalId,
    CallId, PendingChatPrompt, PendingUserQuestions, SessionId, TurnRunStatus,
};

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::scoped_store::ScopedStore;
use crate::state::AppState;

/// A conservative body cap above the validated semantic limits.
pub const MAX_USER_QUESTION_ANSWER_BODY_BYTES: usize = 8 * 1024;

pub async fn list_pending_user_questions(
    store: ScopedStore,
    Path(chat_id): Path<SessionId>,
) -> Result<Json<Vec<PendingUserQuestions>>, ServerError> {
    store.require_chat(chat_id).await?;
    Ok(Json(store.list_pending_user_questions(chat_id).await?))
}

/// `GET /chats/pending-prompts` — opaque cross-conversation attention state.
///
/// The shell uses this single summary read to find parked chats. Detail stays
/// behind each chat's dedicated prompt-recovery route, so this endpoint never
/// carries question content, folder-access arguments, or executor metadata.
/// A cross-chat root read, so it answers only for the requesting principal's
/// own conversations.
pub async fn list_pending_chat_prompts(
    store: ScopedStore,
) -> Result<Json<Vec<PendingChatPrompt>>, ServerError> {
    Ok(Json(store.list_pending_chat_prompts().await?))
}

/// Whether a session-route refusal means the worker cannot take the
/// decision at all, rather than that the decision itself is wrong.
pub(crate) fn worker_cannot_take(error: &ServerError) -> bool {
    matches!(
        error.kind(),
        "session_worker_missing"
            | "approval_worker_inactive"
            | "approval_worker_replaced"
            | "not_found"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerDisposition {
    Answered,
    Existing,
}

#[derive(Debug, Serialize)]
pub struct AnsweredUserQuestions {
    pub disposition: AnswerDisposition,
}

pub async fn answer_user_questions(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, call_id)): Path<(SessionId, CallId)>,
    Json(answers): Json<AnswerUserQuestions>,
) -> Result<Json<AnsweredUserQuestions>, ServerError> {
    store.require_chat(chat_id).await?;
    // A session a worker drives takes its decisions through the session
    // decision route, so the worker hears the decision and resumes the park
    // (decision 0048: the chat routes are aliases). The answers form is what
    // the engine contract carries; additional context has no field there
    // and settles the row directly, which the worker resumes from as well.
    if let Some(code) = state.code.as_ref() {
        if answers.additional_user_context.is_none() && code.has_worker(SessionId(chat_id.0)) {
            match code
                .decide_approval(
                    &store.owner_id(),
                    ApprovalId(call_id.0),
                    crate::code::runtime::ApprovalDecisionRequest::Answers {
                        answers: answers.answers.clone(),
                    },
                    Some(tidebreak_core::TurnActor {
                        principal: Some(store.owner_id().to_string()),
                        display: None,
                        channel_kind: None,
                        external_identity: None,
                    }),
                )
                .await
            {
                Ok(_) => {
                    state.turn_job_wake.notify_one();
                    return Ok(Json(AnsweredUserQuestions {
                        disposition: AnswerDisposition::Answered,
                    }));
                }
                // The worker that would take this decision is not the one
                // the card belongs to; the row settles directly below.
                Err(error) if worker_cannot_take(&error) => {}
                Err(error) => return Err(error),
            }
        }
    }
    let outcome = store
        .answer_user_questions(
            &AnswerUserQuestionsRequest {
                chat_id,
                call_id,
                answers,
            },
            Utc::now(),
        )
        .await?;
    let (disposition, turn) = match outcome {
        AnswerUserQuestionsOutcome::Answered {
            turn,
            completion_event,
            resolution,
        } => {
            // Live delivery of the journaled decision and completion; replay
            // covers anyone not connected, so a missed send is not a
            // correctness gap.
            let sender = state.events.sender(chat_id);
            let _ = sender.send_row(*resolution);
            let _ = sender.send(*completion_event);
            (AnswerDisposition::Answered, turn)
        }
        AnswerUserQuestionsOutcome::Existing(turn) => (AnswerDisposition::Existing, turn),
        AnswerUserQuestionsOutcome::AnswerConflict => {
            return Err(ServerError::conflict(format!(
                "question request {call_id} already has different answers"
            )));
        }
        AnswerUserQuestionsOutcome::InvalidAnswer => {
            return Err(ServerError::bad_request(
                "answers must name known questions and contain valid selections or allowed custom answers",
            ));
        }
        AnswerUserQuestionsOutcome::Unavailable => {
            return Err(ServerError::conflict(format!(
                "question request {call_id} is not answerable"
            )));
        }
    };
    if turn.status == TurnRunStatus::Resuming {
        state.turn_job_wake.notify_one();
    }
    Ok(Json(AnsweredUserQuestions { disposition }))
}
