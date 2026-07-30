//! Renderer-safe plan recovery and exact decision submission.

use axum::extract::State;
use chrono::Utc;
use openwave_core::storage::DecidePlanOutcome;
use openwave_core::{
    CallId, ChatId, DecidePlanRequest, PendingPlanApproval, PlanDecision, TurnRunStatus,
};
use serde::Serialize;

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::state::AppState;

/// A conservative body cap above the validated semantic limits.
pub const MAX_PLAN_DECISION_BODY_BYTES: usize = 16 * 1024;

pub async fn list_pending_plan_approvals(
    State(state): State<AppState>,
    Path(chat_id): Path<ChatId>,
) -> Result<Json<Vec<PendingPlanApproval>>, ServerError> {
    ensure_chat(&state, chat_id).await?;
    Ok(Json(
        state.store.list_pending_plan_approvals(chat_id).await?,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanDecisionDisposition {
    Decided,
    Existing,
}

#[derive(Debug, Serialize)]
pub struct DecidedPlan {
    pub disposition: PlanDecisionDisposition,
}

pub async fn decide_plan(
    State(state): State<AppState>,
    Path((chat_id, call_id)): Path<(ChatId, CallId)>,
    Json(decision): Json<PlanDecision>,
) -> Result<Json<DecidedPlan>, ServerError> {
    ensure_chat(&state, chat_id).await?;
    let outcome = state
        .store
        .decide_plan(
            &DecidePlanRequest {
                chat_id,
                call_id,
                decision,
            },
            Utc::now(),
        )
        .await?;
    let (disposition, turn) = match outcome {
        DecidePlanOutcome::Decided(turn) => (PlanDecisionDisposition::Decided, turn),
        DecidePlanOutcome::Existing(turn) => (PlanDecisionDisposition::Existing, turn),
        DecidePlanOutcome::DecisionConflict => {
            return Err(ServerError::conflict(format!(
                "plan request {call_id} already has a different decision"
            )));
        }
        DecidePlanOutcome::InvalidDecision => {
            return Err(ServerError::bad_request(
                "a plan decision must accept with a non-plan continuation mode or reject with optional feedback",
            ));
        }
        DecidePlanOutcome::Unavailable => {
            return Err(ServerError::conflict(format!(
                "plan request {call_id} is not decidable"
            )));
        }
    };
    if turn.status == TurnRunStatus::Resuming {
        state.turn_job_wake.notify_one();
    }
    Ok(Json(DecidedPlan { disposition }))
}

async fn ensure_chat(state: &AppState, chat_id: ChatId) -> Result<(), ServerError> {
    if state.store.get_chat(chat_id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {chat_id} not found")));
    }
    Ok(())
}
