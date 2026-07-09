//! In-process approval broker: parks Sensitive tool calls until a decision.
//!
//! The agent awaits [`ApprovalGate::decide`](openwave_core::ApprovalGate); this
//! broker fulfills that by parking on a oneshot keyed by `call_id`. The HTTP
//! handler (`POST /chats/{id}/approvals/{call_id}`) resolves the oneshot.
//! Pending entries are dropped when the turn finishes without a decision
//! (channel closed → reject).

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::oneshot;

use openwave_core::{ApprovalDecision, ApprovalGate, ApprovalRequest, CallId, ChatId};

/// Parks Sensitive tool calls until `decide` is called from the HTTP surface.
#[derive(Default)]
pub struct ApprovalBroker {
    pending: Mutex<HashMap<CallId, Pending>>,
}

struct Pending {
    chat_id: ChatId,
    tx: oneshot::Sender<ApprovalDecision>,
}

impl ApprovalBroker {
    /// A fresh broker with no pending approvals.
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve a parked call. Returns `Ok(())` on success, `Err` if the call
    /// isn't pending or belongs to a different chat.
    pub fn resolve(
        &self,
        chat_id: ChatId,
        call_id: CallId,
        decision: ApprovalDecision,
    ) -> Result<(), DecideError> {
        let mut pending = self.pending.lock().unwrap();
        let entry = pending.remove(&call_id).ok_or(DecideError::NotPending)?;
        if entry.chat_id != chat_id {
            // Put it back — wrong chat shouldn't steal another chat's park.
            pending.insert(call_id, entry);
            return Err(DecideError::WrongChat);
        }
        // If the turn already dropped, the send fails; treat as not pending.
        entry
            .tx
            .send(decision)
            .map_err(|_| DecideError::NotPending)?;
        Ok(())
    }
}

/// Why a decide call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecideError {
    /// No parked call with that id (already decided, never parked, or turn ended).
    NotPending,
    /// The call is parked under a different chat.
    WrongChat,
}

#[async_trait]
impl ApprovalGate for ApprovalBroker {
    async fn decide(&self, request: ApprovalRequest) -> ApprovalDecision {
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            // A duplicate park for the same call_id shouldn't happen; if it
            // does, the newer oneshot wins and the old waiter gets a closed rx.
            pending.insert(
                request.call_id,
                Pending {
                    chat_id: request.chat_id,
                    tx,
                },
            );
        }
        match rx.await {
            Ok(decision) => decision,
            // Turn dropped / broker cleared without a decision → reject.
            Err(_) => ApprovalDecision::Reject {
                reason: "approval cancelled".into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwave_core::{ApprovalClass, ApprovalGate, TurnId};
    use std::sync::Arc;

    #[tokio::test]
    async fn approve_unparks_the_waiter() {
        let broker = Arc::new(ApprovalBroker::new());
        let chat_id = ChatId::new();
        let call_id = CallId::new();
        let gate = broker.clone() as Arc<dyn ApprovalGate>;
        let waiter = tokio::spawn(async move {
            gate.decide(ApprovalRequest {
                call_id,
                chat_id,
                turn_id: TurnId::new(),
                tool_name: "shell".into(),
                class: ApprovalClass::Sensitive,
                summary: "run shell".into(),
            })
            .await
        });
        // Yield so the waiter parks.
        tokio::task::yield_now().await;
        broker
            .resolve(chat_id, call_id, ApprovalDecision::Approve)
            .unwrap();
        assert_eq!(waiter.await.unwrap(), ApprovalDecision::Approve);
    }

    #[tokio::test]
    async fn wrong_chat_is_refused() {
        let broker = Arc::new(ApprovalBroker::new());
        let chat_id = ChatId::new();
        let other = ChatId::new();
        let call_id = CallId::new();
        let gate = broker.clone() as Arc<dyn ApprovalGate>;
        let _waiter = tokio::spawn(async move {
            gate.decide(ApprovalRequest {
                call_id,
                chat_id,
                turn_id: TurnId::new(),
                tool_name: "shell".into(),
                class: ApprovalClass::Sensitive,
                summary: "run".into(),
            })
            .await
        });
        tokio::task::yield_now().await;
        assert_eq!(
            broker.resolve(other, call_id, ApprovalDecision::Approve),
            Err(DecideError::WrongChat)
        );
    }
}
