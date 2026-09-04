use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::code::runtime::ApprovalDecisionRequest;
use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::{Json, Path};
use tidebreak_core::ApprovalId;

use super::types::{ApprovalDecision, ApprovalDecisionBody, ApprovalSnapshot, ListApprovalsQuery};

pub async fn list_approvals(
    code: ScopedCode,
    Query(query): Query<ListApprovalsQuery>,
) -> Result<Json<Vec<ApprovalSnapshot>>, ServerError> {
    let approvals = code.list_approvals(query.state, query.session_id).await?;
    Ok(Json(
        approvals.into_iter().map(ApprovalSnapshot::from).collect(),
    ))
}

pub async fn decide_approval(
    code: ScopedCode,
    Path(id): Path<ApprovalId>,
    Json(body): Json<ApprovalDecisionBody>,
) -> Result<impl IntoResponse, ServerError> {
    let decision = match body.decision {
        ApprovalDecision::Approve => ApprovalDecisionRequest::Approve,
        ApprovalDecision::Deny => ApprovalDecisionRequest::Deny {
            feedback: body.feedback,
        },
        ApprovalDecision::ApproveWithGrant { grant_index } => {
            ApprovalDecisionRequest::ApproveWithGrant { grant_index }
        }
        ApprovalDecision::Answers { answers } => ApprovalDecisionRequest::Answers { answers },
        ApprovalDecision::PlanDecision { approve } => ApprovalDecisionRequest::PlanDecision {
            approve,
            feedback: body.feedback,
        },
    };
    let approval = code.decide_approval(id, decision).await?;
    Ok((StatusCode::OK, Json(ApprovalSnapshot::from(approval))))
}
