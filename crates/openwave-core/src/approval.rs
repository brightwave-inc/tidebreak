//! Approval park-and-resume for Sensitive tools.
//!
//! When the agent hits a tool whose [`ApprovalClass`](crate::tool::ApprovalClass)
//! requires a human decision, it emits `ApprovalRequired` and awaits an
//! [`ApprovalGate`]. The server's gate parks the turn on a oneshot channel until
//! `POST /chats/{id}/approvals/{call_id}` decides; tests inject an auto-approve
//! or refuse gate instead.

use async_trait::async_trait;

use crate::id::{CallId, ChatId, TurnId};
use crate::tool::ApprovalClass;

/// The human's decision on a parked tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Run the tool.
    Approve,
    /// Skip the tool; feed an error result back to the model.
    Reject {
        /// Why it was rejected (shown to the model / UI).
        reason: String,
    },
}

/// What the agent asks the gate to decide.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// Correlates with `ApprovalRequired` / the HTTP path.
    pub call_id: CallId,
    /// Chat the turn belongs to.
    pub chat_id: ChatId,
    /// Turn that is parked.
    pub turn_id: TurnId,
    /// Tool name the model invoked.
    pub tool_name: String,
    /// Approval class that triggered the park.
    pub class: ApprovalClass,
    /// Short human-readable summary of what will happen.
    pub summary: String,
}

/// Decides whether a Sensitive tool call may run.
///
/// Object-safe and held behind `Arc` so the server can park on a channel while
/// tests inject an immediate approve/reject.
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    /// Await a decision for `request`. Must not return until the call is
    /// approved or rejected (or the turn is abandoned).
    async fn decide(&self, request: ApprovalRequest) -> ApprovalDecision;
}

/// Always rejects — the default when no gate is wired. Keeps fail-closed
/// behavior for Sensitive tools outside the server's park-and-resume path.
pub struct RefuseGate;

#[async_trait]
impl ApprovalGate for RefuseGate {
    async fn decide(&self, request: ApprovalRequest) -> ApprovalDecision {
        ApprovalDecision::Reject {
            reason: format!(
                "{} requires approval, which is not configured",
                request.tool_name
            ),
        }
    }
}

/// Always approves — for tests that exercise Sensitive tools without a UI.
pub struct AutoApproveGate;

#[async_trait]
impl ApprovalGate for AutoApproveGate {
    async fn decide(&self, _request: ApprovalRequest) -> ApprovalDecision {
        ApprovalDecision::Approve
    }
}
