//! In-process approval broker: parks Sensitive tool calls until a decision.
//!
//! The agent arms this gate (registering a oneshot keyed by `call_id`) *before*
//! emitting `ApprovalRequired`, then awaits the future. The HTTP handler
//! (`POST /chats/{id}/approvals/{call_id}`) resolves the oneshot. Pending
//! entries are dropped when the turn finishes without a decision (channel
//! closed → reject).

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::oneshot;

use openwave_core::{
    ApprovalDecision, ApprovalFuture, ApprovalGate, ApprovalRequest, CallId, ChatId,
};

/// Parks Sensitive tool calls until `resolve` is called from the HTTP surface.
#[derive(Default)]
pub struct ApprovalBroker {
    pending: Mutex<HashMap<CallId, Pending>>,
}

struct Pending {
    chat_id: ChatId,
    registration_id: uuid::Uuid,
    tx: oneshot::Sender<ApprovalDecision>,
}

struct PendingRegistration<'a> {
    broker: &'a ApprovalBroker,
    call_id: CallId,
    registration_id: uuid::Uuid,
}

impl Drop for PendingRegistration<'_> {
    fn drop(&mut self) {
        let mut pending = self.broker.pending.lock().unwrap();
        if pending
            .get(&self.call_id)
            .is_some_and(|entry| entry.registration_id == self.registration_id)
        {
            pending.remove(&self.call_id);
        }
    }
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

/// Why a resolve call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecideError {
    /// No parked call with that id (already decided, never parked, or turn ended).
    NotPending,
    /// The call is parked under a different chat.
    WrongChat,
}

impl ApprovalGate for ApprovalBroker {
    fn arm(&self, request: ApprovalRequest) -> ApprovalFuture<'_> {
        let (tx, rx) = oneshot::channel();
        let registration_id = uuid::Uuid::new_v4();
        {
            let mut pending = self.pending.lock().unwrap();
            // A duplicate park for the same call_id shouldn't happen; if it
            // does, the newer oneshot wins and the old waiter gets a closed rx.
            pending.insert(
                request.call_id,
                Pending {
                    chat_id: request.chat_id,
                    registration_id,
                    tx,
                },
            );
        }
        // Entry is registered *before* this future is returned, so the agent
        // can safely emit ApprovalRequired and a client can resolve immediately.
        let registration = PendingRegistration {
            broker: self,
            call_id: request.call_id,
            registration_id,
        };
        Box::pin(async move {
            let _registration = registration;
            match rx.await {
                Ok(decision) => decision,
                // Turn dropped / broker cleared without a decision → reject.
                Err(_) => ApprovalDecision::Reject {
                    reason: "approval cancelled".into(),
                },
            }
        })
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
        let pending = gate.arm(ApprovalRequest {
            call_id,
            chat_id,
            turn_id: TurnId::new(),
            tool_name: "shell".into(),
            class: ApprovalClass::Sensitive,
            summary: "run shell".into(),
        });
        // Resolvable immediately after arm — before the waiter is polled.
        broker
            .resolve(chat_id, call_id, ApprovalDecision::Approve)
            .unwrap();
        assert_eq!(pending.await, ApprovalDecision::Approve);
    }

    #[tokio::test]
    async fn wrong_chat_is_refused() {
        let broker = Arc::new(ApprovalBroker::new());
        let chat_id = ChatId::new();
        let other = ChatId::new();
        let call_id = CallId::new();
        let gate = broker.clone() as Arc<dyn ApprovalGate>;
        let _pending = gate.arm(ApprovalRequest {
            call_id,
            chat_id,
            turn_id: TurnId::new(),
            tool_name: "shell".into(),
            class: ApprovalClass::Sensitive,
            summary: "run".into(),
        });
        assert_eq!(
            broker.resolve(other, call_id, ApprovalDecision::Approve),
            Err(DecideError::WrongChat)
        );
    }

    #[tokio::test]
    async fn dropping_a_parked_turn_removes_its_pending_approval() {
        let broker = ApprovalBroker::new();
        let chat_id = ChatId::new();
        let call_id = CallId::new();
        let pending = broker.arm(ApprovalRequest {
            call_id,
            chat_id,
            turn_id: TurnId::new(),
            tool_name: "shell".into(),
            class: ApprovalClass::Sensitive,
            summary: "run shell".into(),
        });

        drop(pending);
        assert_eq!(
            broker.resolve(chat_id, call_id, ApprovalDecision::Approve),
            Err(DecideError::NotPending)
        );
    }

    #[tokio::test]
    async fn arm_makes_call_resolvable_before_await() {
        // Regression for the emit-before-park race: after arm returns, resolve
        // must succeed even if the waiter hasn't been polled yet.
        let broker = ApprovalBroker::new();
        let chat_id = ChatId::new();
        let call_id = CallId::new();
        let _pending = broker.arm(ApprovalRequest {
            call_id,
            chat_id,
            turn_id: TurnId::new(),
            tool_name: "shell".into(),
            class: ApprovalClass::Sensitive,
            summary: "run".into(),
        });
        assert!(broker
            .resolve(chat_id, call_id, ApprovalDecision::Approve)
            .is_ok());
    }
}
