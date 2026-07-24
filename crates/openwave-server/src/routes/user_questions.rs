//! Renderer-safe foreground question recovery and exact answer submission.

use axum::extract::State;
use chrono::Utc;
use openwave_core::{
    AnswerUserQuestions, AnswerUserQuestionsOutcome, AnswerUserQuestionsRequest, CallId, ChatId,
    PendingUserQuestions, TurnRunStatus,
};
use serde::Serialize;

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::state::AppState;

/// A conservative body cap above the validated semantic limits.
pub const MAX_USER_QUESTION_ANSWER_BODY_BYTES: usize = 8 * 1024;

pub async fn list_pending_user_questions(
    State(state): State<AppState>,
    Path(chat_id): Path<ChatId>,
) -> Result<Json<Vec<PendingUserQuestions>>, ServerError> {
    ensure_chat(&state, chat_id).await?;
    Ok(Json(
        state.store.list_pending_user_questions(chat_id).await?,
    ))
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
    Path((chat_id, call_id)): Path<(ChatId, CallId)>,
    Json(answers): Json<AnswerUserQuestions>,
) -> Result<Json<AnsweredUserQuestions>, ServerError> {
    ensure_chat(&state, chat_id).await?;
    let outcome = state
        .store
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
        AnswerUserQuestionsOutcome::Answered(turn) => (AnswerDisposition::Answered, turn),
        AnswerUserQuestionsOutcome::Existing(turn) => (AnswerDisposition::Existing, turn),
        AnswerUserQuestionsOutcome::AnswerConflict => {
            return Err(ServerError::conflict(format!(
                "question request {call_id} already has different answers"
            )));
        }
        AnswerUserQuestionsOutcome::InvalidAnswer => {
            return Err(ServerError::bad_request(
                "answers must cover every question exactly once and select a valid option or allowed free-form answer",
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

async fn ensure_chat(state: &AppState, chat_id: ChatId) -> Result<(), ServerError> {
    if state.store.get_chat(chat_id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {chat_id} not found")));
    }
    Ok(())
}
