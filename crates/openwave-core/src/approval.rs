//! Approval park-and-resume for Sensitive tools.
//!
//! When the agent hits a tool whose [`ApprovalClass`](crate::tool::ApprovalClass)
//! requires a human decision, it arms an [`ApprovalGate`] (so the call is
//! resolvable), emits `ApprovalRequired`, then awaits the decision. The
//! server's gate parks the turn on a oneshot channel until
//! `POST /chats/{id}/approvals/{call_id}` decides; tests inject an auto-approve
//! or refuse gate instead.
//!
//! **Ordering invariant:** the gate must make the call resolvable *before*
//! returning from [`ApprovalGate::arm`], so a client that sees
//! `ApprovalRequired` can never race a 404 against a not-yet-parked call.

use std::future::Future;
use std::pin::Pin;

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

/// A future that resolves to an [`ApprovalDecision`].
pub type ApprovalFuture<'a> = Pin<Box<dyn Future<Output = ApprovalDecision> + Send + 'a>>;

/// Decides whether a Sensitive tool call may run.
///
/// Object-safe and held behind `Arc` so the server can park on a channel while
/// tests inject an immediate approve/reject.
pub trait ApprovalGate: Send + Sync {
    /// Synchronously arm a pending approval, returning a future that resolves
    /// when decided.
    ///
    /// Parking implementations MUST register the call (so HTTP resolve can find
    /// it) before returning. The agent emits `ApprovalRequired` only after
    /// `arm` returns, then awaits the future — closing the race where a client
    /// sees the event before the call is parked.
    fn arm(&self, request: ApprovalRequest) -> ApprovalFuture<'_>;
}

/// Always rejects — the default when no gate is wired. Keeps fail-closed
/// behavior for Sensitive tools outside the server's park-and-resume path.
pub struct RefuseGate;

impl ApprovalGate for RefuseGate {
    fn arm(&self, request: ApprovalRequest) -> ApprovalFuture<'_> {
        Box::pin(async move {
            ApprovalDecision::Reject {
                reason: format!(
                    "{} requires approval, which is not configured",
                    request.tool_name
                ),
            }
        })
    }
}

/// Always approves — for tests that exercise Sensitive tools without a UI.
pub struct AutoApproveGate;

impl ApprovalGate for AutoApproveGate {
    fn arm(&self, _request: ApprovalRequest) -> ApprovalFuture<'_> {
        Box::pin(async { ApprovalDecision::Approve })
    }
}
