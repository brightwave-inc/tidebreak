use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::state::AppState;
use tidebreak_core::CodeApprovalId;
use tidebreak_harness::ApprovalDecision;

use super::require_code;
use super::types::{
    CodeApprovalDecision, CodeApprovalDecisionBody, CodeApprovalSnapshot, ListApprovalsQuery,
};

pub async fn list_approvals(
    State(state): State<AppState>,
    Query(query): Query<ListApprovalsQuery>,
) -> Result<Json<Vec<CodeApprovalSnapshot>>, ServerError> {
    let approvals = require_code(&state)?
        .list_approvals(query.state, query.session_id)
        .await?;
    Ok(Json(
        approvals
            .into_iter()
            .map(CodeApprovalSnapshot::from)
            .collect(),
    ))
}

pub async fn decide_approval(
    State(state): State<AppState>,
    Path(id): Path<CodeApprovalId>,
    Json(body): Json<CodeApprovalDecisionBody>,
) -> Result<impl IntoResponse, ServerError> {
    let decision = match body.decision {
        CodeApprovalDecision::Approve => ApprovalDecision::Approve,
        CodeApprovalDecision::Deny => ApprovalDecision::Deny {
            feedback: body.feedback,
        },
    };
    let approval = require_code(&state)?.decide_approval(id, decision).await?;
    Ok((StatusCode::OK, Json(CodeApprovalSnapshot::from(approval))))
}
