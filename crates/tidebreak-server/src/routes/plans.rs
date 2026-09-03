//! Renderer-safe plan recovery and exact decision submission.

use axum::extract::State;
use chrono::Utc;
use serde::Serialize;
use tidebreak_core::storage::DecidePlanOutcome;
use tidebreak_core::{
    ApprovalId, CallId, DecidePlanRequest, PendingPlanApproval, PlanDecision, PlanDecisionChoice,
    SessionId, TurnRunStatus, DEFAULT_ACCEPTED_PLAN_MODE,
};

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::scoped_store::ScopedStore;
use crate::state::AppState;

/// A conservative body cap above the validated semantic limits.
pub const MAX_PLAN_DECISION_BODY_BYTES: usize = 16 * 1024;

pub async fn list_pending_plan_approvals(
    store: ScopedStore,
    Path(chat_id): Path<SessionId>,
) -> Result<Json<Vec<PendingPlanApproval>>, ServerError> {
    store.require_chat(chat_id).await?;
    Ok(Json(store.list_pending_plan_approvals(chat_id).await?))
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
    store: ScopedStore,
    Path((chat_id, call_id)): Path<(SessionId, CallId)>,
    Json(decision): Json<PlanDecision>,
) -> Result<Json<DecidedPlan>, ServerError> {
    store.require_chat(chat_id).await?;
    // A session a worker drives takes its decisions through the session
    // decision route, so the worker hears the decision and resumes the park
    // (decision 0048: the chat routes are aliases). The engine contract
    // carries the mode the approval proposed; a decision naming another
    // mode settles the row directly, which the worker resumes from as well.
    if let Some(code) = state.code.as_ref() {
        let proposed_mode = decision
            .permission_mode
            .is_none_or(|mode| mode == DEFAULT_ACCEPTED_PLAN_MODE);
        if proposed_mode && code.has_worker(SessionId(chat_id.0)) {
            match code
                .decide_approval(
                    &store.owner_id(),
                    ApprovalId(call_id.0),
                    crate::code::runtime::ApprovalDecisionRequest::PlanDecision {
                        approve: matches!(decision.decision, PlanDecisionChoice::Accept),
                        feedback: decision.feedback.clone(),
                    },
                )
                .await
            {
                Ok(_) => {
                    state.turn_job_wake.notify_one();
                    return Ok(Json(DecidedPlan {
                        disposition: PlanDecisionDisposition::Decided,
                    }));
                }
                Err(error) if super::user_questions::worker_cannot_take(&error) => {}
                Err(error) => return Err(error),
            }
        }
    }
    let outcome = store
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
        DecidePlanOutcome::Decided {
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
            (PlanDecisionDisposition::Decided, turn)
        }
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
