use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::code::runtime::ApprovalDecisionRequest;
use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::{Json, Path};
use tidebreak_core::CodeApprovalId;

use super::types::{
    CodeApprovalDecision, CodeApprovalDecisionBody, CodeApprovalSnapshot, ListApprovalsQuery,
};

pub async fn list_approvals(
    code: ScopedCode,
    Query(query): Query<ListApprovalsQuery>,
) -> Result<Json<Vec<CodeApprovalSnapshot>>, ServerError> {
    let approvals = code.list_approvals(query.state, query.session_id).await?;
    Ok(Json(
        approvals
            .into_iter()
            .map(CodeApprovalSnapshot::from)
            .collect(),
    ))
}

pub async fn decide_approval(
    code: ScopedCode,
    Path(id): Path<CodeApprovalId>,
    Json(body): Json<CodeApprovalDecisionBody>,
) -> Result<impl IntoResponse, ServerError> {
    let decision = match body.decision {
        CodeApprovalDecision::Approve => ApprovalDecisionRequest::Approve,
        CodeApprovalDecision::Deny => ApprovalDecisionRequest::Deny {
            feedback: body.feedback,
        },
        CodeApprovalDecision::ApproveWithGrant { grant_index } => {
            ApprovalDecisionRequest::ApproveWithGrant { grant_index }
        }
        CodeApprovalDecision::Answers { answers } => ApprovalDecisionRequest::Answers { answers },
        CodeApprovalDecision::PlanDecision { approve } => ApprovalDecisionRequest::PlanDecision {
            approve,
            feedback: body.feedback,
        },
    };
    let approval = code.decide_approval(id, decision).await?;
    Ok((StatusCode::OK, Json(CodeApprovalSnapshot::from(approval))))
}
