//! Durable approval broker for Sensitive foreground tool calls.
//!
//! The database is authoritative; `Notify` is only a latency hint. Registration
//! commits before `ApprovalRequired` is emitted, decisions are exact idempotent
//! transitions, and a waiter recreated after process loss recovers the same
//! pending or terminal state.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::sync::Notify;

use openwave_core::{
    ApprovalDecision, ApprovalFuture, ApprovalGate, ApprovalJournalIdentity, ApprovalRegistration,
    ApprovalRegistrationFuture, ApprovalRequest, ApprovalRequiredPublication, CallId, ChatId,
    DecideToolApprovalOutcome, GrantScope, RequestToolApprovalOutcome, Result, StandingGrant,
    StandingGrants, Store,
};

/// Coordinates durable approval state with local low-latency waiters.
pub struct ApprovalBroker {
    store: Arc<dyn Store>,
    wake: Arc<Notify>,
    standing_grants: Arc<StandingGrants>,
}

impl ApprovalBroker {
    /// Build a broker over the application's authoritative store.
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self {
            store,
            wake: Arc::new(Notify::new()),
            standing_grants: Arc::new(StandingGrants::new()),
        }
    }

    /// Live remembered approvals shared with foreground agent loops.
    pub fn standing_grants(&self) -> Arc<StandingGrants> {
        self.standing_grants.clone()
    }

    /// Decide one exact request. The same decision is an idempotent success;
    /// an opposite decision is a conflict.
    pub async fn resolve(
        &self,
        chat_id: ChatId,
        call_id: CallId,
        decision: ApprovalDecision,
    ) -> Result<ResolveApprovalOutcome> {
        self.resolve_with_grant(chat_id, call_id, decision, None)
            .await
    }

    /// Decide one exact request and optionally remember an approval for later
    /// matching calls in the same chat.
    pub async fn resolve_with_grant(
        &self,
        chat_id: ChatId,
        call_id: CallId,
        decision: ApprovalDecision,
        rung: Option<crate::routes::ApprovalGrantRung>,
    ) -> Result<ResolveApprovalOutcome> {
        let Some(current) = self.store.get_tool_call_approval(call_id).await? else {
            return Ok(ResolveApprovalOutcome::NotPending);
        };
        if current.chat_id != chat_id {
            return Ok(ResolveApprovalOutcome::WrongChat);
        }
        if matches!(decision, ApprovalDecision::Approve) && !current.kind.is_approvable() {
            return Ok(ResolveApprovalOutcome::NotApprovable);
        }
        // Resolve the scope from durable state before deciding, so a rung the
        // call cannot support fails the request instead of silently landing a
        // broader grant after the call has already run.
        let scope = match rung {
            None => None,
            Some(rung) => {
                match grant_scope(rung, current.preview.as_ref(), current.action_is_exact) {
                    Some(scope) => Some(scope),
                    None => return Ok(ResolveApprovalOutcome::GrantNotAvailable),
                }
            }
        };
        let outcome = self
            .store
            .decide_tool_call_approval(chat_id, call_id, &decision, Utc::now())
            .await?;
        match outcome {
            DecideToolApprovalOutcome::Decided(_) | DecideToolApprovalOutcome::Existing(_) => {
                if let Some(scope) = scope {
                    if let Some(grant) = StandingGrant::scoped(
                        current.chat_id,
                        current.tool_name,
                        current.kind,
                        scope,
                        Utc::now(),
                    ) {
                        self.standing_grants.record(grant);
                    }
                }
                self.wake.notify_waiters();
                Ok(ResolveApprovalOutcome::Resolved)
            }
            DecideToolApprovalOutcome::DecisionConflict => {
                Ok(ResolveApprovalOutcome::DecisionConflict)
            }
            DecideToolApprovalOutcome::Unavailable => Ok(ResolveApprovalOutcome::NotPending),
        }
    }
}

/// Closed HTTP-facing resolution result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveApprovalOutcome {
    Resolved,
    NotPending,
    WrongChat,
    NotApprovable,
    /// The chosen rung does not exist for this call.
    GrantNotAvailable,
    DecisionConflict,
}

/// Build the concrete grant a rung names, from the action the call is parked
/// on. A narrow rung over an action the renderer could not describe has nothing
/// to name, so it is refused rather than widened.
fn grant_scope(
    rung: crate::routes::ApprovalGrantRung,
    action: Option<&openwave_core::ToolActionPreview>,
    action_is_exact: bool,
) -> Option<GrantScope> {
    use crate::routes::ApprovalGrantRung;
    use openwave_core::ToolActionPreview;
    if matches!(rung, ApprovalGrantRung::WholeTool) {
        return Some(GrantScope::WholeTool);
    }
    // A narrow rung names the action, and the preview it would be named from is
    // clamped for display. Naming one from a clamped preview would authorize
    // every other call that clamps to the same text, so an approximate
    // description is refused rather than turned into standing authority.
    if !action_is_exact {
        return None;
    }
    match (rung, action?) {
        (ApprovalGrantRung::WholeTool, _) => Some(GrantScope::WholeTool),
        (ApprovalGrantRung::ExactAction, action) => Some(GrantScope::ExactAction(action.clone())),
        (ApprovalGrantRung::AnyArgsForCommand, ToolActionPreview::Exec { command, .. }) => {
            Some(GrantScope::AnyArgsFor {
                command: command.clone(),
            })
        }
        // Only a command has an executable to name, so no other action can
        // reach the rung between exact and whole-tool.
        (ApprovalGrantRung::AnyArgsForCommand, _) => None,
    }
}

impl ApprovalGate for ApprovalBroker {
    fn register(
        &self,
        request: ApprovalRequest,
        journal: Option<ApprovalJournalIdentity>,
    ) -> ApprovalRegistrationFuture<'_> {
        Box::pin(async move {
            let (initial, publication) = match journal {
                Some(journal) => loop {
                    match self
                        .store
                        .request_tool_call_approval_and_append_event(
                            &request,
                            journal.lease_token,
                            journal.event_ordinal,
                            Utc::now(),
                        )
                        .await
                    {
                        Ok(journaled) => match journaled.outcome {
                            RequestToolApprovalOutcome::Requested(approval)
                            | RequestToolApprovalOutcome::Existing(approval) => {
                                let publication = journaled.required_event.map_or(
                                    ApprovalRequiredPublication::None,
                                    |event| {
                                        if approval.decision().is_some() {
                                            ApprovalRequiredPublication::Recovered {
                                                event_ordinal: journal.event_ordinal,
                                                event,
                                            }
                                        } else {
                                            ApprovalRequiredPublication::Committed {
                                                event_ordinal: journal.event_ordinal,
                                                event,
                                            }
                                        }
                                    },
                                );
                                break (approval, publication);
                            }
                            RequestToolApprovalOutcome::IdentityConflict => {
                                return ready_reject("approval request identity conflict");
                            }
                            RequestToolApprovalOutcome::Unavailable => {
                                return ready_reject("approval request is no longer available");
                            }
                        },
                        Err(_) => {
                            // Commit acknowledgement can be lost. Retry the
                            // exact receipt until the caller cancels this
                            // registration future or state is classified.
                            tokio::time::sleep(Duration::from_millis(25)).await;
                        }
                    }
                },
                None => match self
                    .store
                    .request_tool_call_approval(&request, Utc::now())
                    .await
                {
                    Ok(RequestToolApprovalOutcome::Requested(approval)) => {
                        (approval, ApprovalRequiredPublication::Ordinary)
                    }
                    Ok(RequestToolApprovalOutcome::Existing(approval)) => {
                        (approval, ApprovalRequiredPublication::None)
                    }
                    Ok(RequestToolApprovalOutcome::IdentityConflict) => {
                        return ready_reject("approval request identity conflict");
                    }
                    Ok(RequestToolApprovalOutcome::Unavailable) => {
                        return ready_reject("approval request is no longer available");
                    }
                    Err(_) => return ready_reject("durable approval storage is unavailable"),
                },
            };
            if let Some(decision) = initial.decision() {
                return ApprovalRegistration {
                    decision: Box::pin(async move { decision }) as ApprovalFuture,
                    publication,
                };
            }

            let store = self.store.clone();
            let wake = self.wake.clone();
            let call_id = request.call_id;
            let decision = Box::pin(async move {
                loop {
                    // Arm the notification before reading so a decision cannot
                    // land between the authoritative read and the wait.
                    let notified = wake.notified();
                    match store.get_tool_call_approval(call_id).await {
                        Ok(Some(approval)) => {
                            if let Some(decision) = approval.decision() {
                                return decision;
                            }
                        }
                        Ok(None) => {
                            return ApprovalDecision::Reject {
                                reason: "approval request disappeared".into(),
                            };
                        }
                        Err(_) => {
                            // A transient read failure is not a decision. Poll
                            // durably instead of converting it into consent.
                        }
                    }
                    tokio::select! {
                        () = notified => {}
                        () = tokio::time::sleep(Duration::from_millis(250)) => {}
                    }
                }
            }) as ApprovalFuture;
            ApprovalRegistration {
                decision,
                publication,
            }
        })
    }
}

fn ready_reject(reason: &'static str) -> ApprovalRegistration {
    ApprovalRegistration {
        decision: Box::pin(async move {
            ApprovalDecision::Reject {
                reason: reason.into(),
            }
        }),
        publication: ApprovalRequiredPublication::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwave_core::{
        AcceptToolCallOutcome, AcceptTurnOutcome, ApprovalClass, Chat, DbStore, ToolCallExecution,
        ToolCallRecord, ToolCallStatus, TurnId,
    };
    use serde_json::json;

    async fn setup(tool_name: &str) -> (Arc<dyn Store>, ApprovalRequest) {
        setup_with_arguments(tool_name, json!({"query": "private"})).await
    }

    async fn setup_with_arguments(
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> (Arc<dyn Store>, ApprovalRequest) {
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("approval.db").display()
            ))
            .await
            .unwrap(),
        );
        // Keep the directory alive for the test process; SQLite already owns
        // its open connection after this setup.
        std::mem::forget(db);
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: Some("Approval test".into()),
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let turn_id = TurnId::new();
        let call_id = CallId::new();
        assert!(matches!(
            store
                .accept_tool_call(&ToolCallRecord {
                    id: call_id,
                    chat_id: chat.id,
                    turn_id,
                    provider_id: "provider-call".into(),
                    name: tool_name.into(),
                    arguments,
                    execution: ToolCallExecution::Server,
                    status: ToolCallStatus::Pending,
                    result: None,
                    error_code: None,
                    error_detail: None,
                    client_executor_id: None,
                    client_lease_expires_at: None,
                    created_at: Utc::now(),
                    resolved_at: None,
                })
                .await
                .unwrap(),
            AcceptToolCallOutcome::Accepted(_)
        ));
        (
            store,
            ApprovalRequest {
                call_id,
                chat_id: chat.id,
                turn_id,
                tool_name: tool_name.into(),
                class: ApprovalClass::Sensitive,
                kind: openwave_core::ToolApprovalKind::for_tool_name(tool_name),
                preview: None,
                summary: "private summary".into(),
            },
        )
    }

    #[tokio::test]
    async fn decision_survives_broker_recreation() {
        let (store, request) = setup("search").await;
        let first = ApprovalBroker::new(store.clone());
        let pending = first.register(request.clone(), None).await;
        drop(pending.decision);

        assert_eq!(
            first
                .resolve(request.chat_id, request.call_id, ApprovalDecision::Approve)
                .await
                .unwrap(),
            ResolveApprovalOutcome::Resolved
        );
        let restarted = ApprovalBroker::new(store);
        assert_eq!(
            restarted.register(request, None).await.decision.await,
            ApprovalDecision::Approve
        );
    }

    #[tokio::test]
    async fn web_search_consent_kind_survives_durable_recovery() {
        let (store, request) = setup("web_search").await;
        assert_eq!(
            request.kind,
            openwave_core::ToolApprovalKind::WebSearchMayShareQuery
        );
        let first = ApprovalBroker::new(store.clone());
        let pending = first.register(request.clone(), None).await;
        drop(pending.decision);
        let persisted = store
            .get_tool_call_approval(request.call_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            persisted.kind,
            openwave_core::ToolApprovalKind::WebSearchMayShareQuery
        );

        assert_eq!(
            first
                .resolve(request.chat_id, request.call_id, ApprovalDecision::Approve)
                .await
                .unwrap(),
            ResolveApprovalOutcome::Resolved
        );
        assert_eq!(
            ApprovalBroker::new(store)
                .register(request, None)
                .await
                .decision
                .await,
            ApprovalDecision::Approve
        );
    }

    #[tokio::test]
    async fn decisions_are_exact_and_idempotent() {
        let (store, request) = setup("search").await;
        let broker = ApprovalBroker::new(store);
        let _pending = broker.register(request.clone(), None).await;
        assert_eq!(
            broker
                .resolve(request.chat_id, request.call_id, ApprovalDecision::Approve)
                .await
                .unwrap(),
            ResolveApprovalOutcome::Resolved
        );
        assert_eq!(
            broker
                .resolve(request.chat_id, request.call_id, ApprovalDecision::Approve)
                .await
                .unwrap(),
            ResolveApprovalOutcome::Resolved
        );
        assert_eq!(
            broker
                .resolve(
                    request.chat_id,
                    request.call_id,
                    ApprovalDecision::Reject {
                        reason: "changed".into(),
                    },
                )
                .await
                .unwrap(),
            ResolveApprovalOutcome::DecisionConflict
        );
    }

    #[tokio::test]
    async fn remembered_approval_grants_matching_calls_in_the_chat() {
        let (store, request) = setup("search").await;
        let broker = ApprovalBroker::new(store);
        let _pending = broker.register(request.clone(), None).await;

        assert_eq!(
            broker
                .resolve_with_grant(
                    request.chat_id,
                    request.call_id,
                    ApprovalDecision::Approve,
                    Some(crate::routes::ApprovalGrantRung::WholeTool),
                )
                .await
                .unwrap(),
            ResolveApprovalOutcome::Resolved
        );
        assert!(broker.standing_grants().covers(
            request.chat_id,
            &request.tool_name,
            request.kind,
            &json!({})
        ));
    }

    #[tokio::test]
    async fn a_narrow_grant_covers_the_command_it_named_and_nothing_else() {
        // The grant is derived from the arguments the call is durably parked
        // on, never from what the request carried in memory.
        let (store, request) = setup_with_arguments(
            "exec",
            json!({ "command": "cargo", "args": ["test"], "cwd": "." }),
        )
        .await;
        let broker = ApprovalBroker::new(store);
        let _pending = broker.register(request.clone(), None).await;

        assert_eq!(
            broker
                .resolve_with_grant(
                    request.chat_id,
                    request.call_id,
                    ApprovalDecision::Approve,
                    Some(crate::routes::ApprovalGrantRung::ExactAction),
                )
                .await
                .unwrap(),
            ResolveApprovalOutcome::Resolved
        );

        let grants = broker.standing_grants();
        let exec =
            |command: &str, args: &[&str]| json!({ "command": command, "args": args, "cwd": "." });
        assert!(grants.covers(
            request.chat_id,
            "exec",
            request.kind,
            &exec("cargo", &["test"]),
        ));
        // The grant was built from the arguments the call was parked on, so it
        // cannot stretch to a command the human never saw.
        assert!(!grants.covers(
            request.chat_id,
            "exec",
            request.kind,
            &exec("cargo", &["publish"]),
        ));
        assert!(!grants.covers(request.chat_id, "exec", request.kind, &json!({})));
    }

    #[tokio::test]
    async fn a_search_can_be_remembered_for_its_query_rather_than_for_every_search() {
        // The narrow rung used to exist only for `exec`, so approving a
        // Sensitive search offered nothing between "this once" and "every
        // search in this chat".
        let (store, request) =
            setup_with_arguments("web_search", json!({ "query": "quarterly filings" })).await;
        let broker = ApprovalBroker::new(store);
        let _pending = broker.register(request.clone(), None).await;

        assert_eq!(
            broker
                .resolve_with_grant(
                    request.chat_id,
                    request.call_id,
                    ApprovalDecision::Approve,
                    Some(crate::routes::ApprovalGrantRung::ExactAction),
                )
                .await
                .unwrap(),
            ResolveApprovalOutcome::Resolved
        );

        let grants = broker.standing_grants();
        let query = |query: &str| json!({ "query": query });
        assert!(grants.covers(
            request.chat_id,
            "web_search",
            request.kind,
            &query("quarterly filings"),
        ));
        // The grant names the query the card showed, so the next search still
        // asks.
        assert!(!grants.covers(
            request.chat_id,
            "web_search",
            request.kind,
            &query("payroll")
        ));
        // Only a command has an executable to name, so the middle rung has
        // nothing to build from and is refused rather than widened.
        let (store, request) =
            setup_with_arguments("web_search", json!({ "query": "payroll" })).await;
        let broker = ApprovalBroker::new(store);
        let _pending = broker.register(request.clone(), None).await;
        assert_eq!(
            broker
                .resolve_with_grant(
                    request.chat_id,
                    request.call_id,
                    ApprovalDecision::Approve,
                    Some(crate::routes::ApprovalGrantRung::AnyArgsForCommand),
                )
                .await
                .unwrap(),
            ResolveApprovalOutcome::GrantNotAvailable
        );
    }

    #[tokio::test]
    async fn a_rung_the_call_cannot_describe_is_refused_rather_than_widened() {
        // The approval carries no action, so "always allow exactly this" has
        // nothing to name. Falling back to a broader grant would hand out more
        // authority than the human chose.
        let (store, request) = setup("exec").await;
        let broker = ApprovalBroker::new(store);
        let _pending = broker.register(request.clone(), None).await;

        assert_eq!(
            broker
                .resolve_with_grant(
                    request.chat_id,
                    request.call_id,
                    ApprovalDecision::Approve,
                    Some(crate::routes::ApprovalGrantRung::ExactAction),
                )
                .await
                .unwrap(),
            ResolveApprovalOutcome::GrantNotAvailable
        );
        assert!(!broker.standing_grants().covers(
            request.chat_id,
            "exec",
            request.kind,
            &json!({})
        ));
    }

    #[tokio::test]
    async fn escaping_exec_can_be_approved_and_remembered() {
        // Deny-by-default previously blocked every non-`search` Sensitive action
        // from being approved (409 `NotApprovable`). An escaping `exec` is now a
        // presentable, grantable action.
        let (store, request) = setup("exec").await;
        let broker = ApprovalBroker::new(store);
        let _pending = broker.register(request.clone(), None).await;

        assert_eq!(
            broker
                .resolve_with_grant(
                    request.chat_id,
                    request.call_id,
                    ApprovalDecision::Approve,
                    Some(crate::routes::ApprovalGrantRung::WholeTool),
                )
                .await
                .unwrap(),
            ResolveApprovalOutcome::Resolved
        );
        assert!(broker.standing_grants().covers(
            request.chat_id,
            &request.tool_name,
            request.kind,
            &json!({})
        ));
    }

    #[tokio::test]
    async fn unknown_action_cannot_be_approved_but_can_be_rejected() {
        let (store, request) = setup("third_party_sensitive").await;
        let broker = ApprovalBroker::new(store);
        let pending = broker.register(request.clone(), None).await;
        assert_eq!(
            broker
                .resolve(request.chat_id, request.call_id, ApprovalDecision::Approve)
                .await
                .unwrap(),
            ResolveApprovalOutcome::NotApprovable
        );
        assert_eq!(
            broker
                .resolve(
                    request.chat_id,
                    request.call_id,
                    ApprovalDecision::Reject {
                        reason: "not allowed".into(),
                    },
                )
                .await
                .unwrap(),
            ResolveApprovalOutcome::Resolved
        );
        assert_eq!(
            pending.decision.await,
            ApprovalDecision::Reject {
                reason: "not allowed".into()
            }
        );
    }

    #[tokio::test]
    async fn terminal_exact_retry_preserves_the_committed_event_receipt() {
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("approval-journal.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: Some("Approval journal test".into()),
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let turn_id = TurnId::new();
        assert!(matches!(
            store
                .accept_turn(turn_id, chat.id, "test-model", "test request")
                .await
                .unwrap(),
            AcceptTurnOutcome::Accepted(_)
        ));
        let lease_token = uuid::Uuid::new_v4();
        let claimed_at = Utc::now();
        assert!(store
            .claim_turn_run(
                lease_token,
                claimed_at,
                claimed_at + chrono::Duration::minutes(5),
            )
            .await
            .unwrap()
            .turn
            .is_some());
        let request = ApprovalRequest {
            call_id: CallId::new(),
            chat_id: chat.id,
            turn_id,
            tool_name: "search".into(),
            class: ApprovalClass::Sensitive,
            kind: openwave_core::ToolApprovalKind::for_tool_name("search"),
            preview: None,
            summary: "search requires approval".into(),
        };
        assert!(matches!(
            store
                .accept_tool_call(&ToolCallRecord {
                    id: request.call_id,
                    chat_id: chat.id,
                    turn_id,
                    provider_id: "provider-call".into(),
                    name: request.tool_name.clone(),
                    arguments: json!({"query": "private"}),
                    execution: ToolCallExecution::Server,
                    status: ToolCallStatus::Pending,
                    result: None,
                    error_code: None,
                    error_detail: None,
                    client_executor_id: None,
                    client_lease_expires_at: None,
                    created_at: claimed_at,
                    resolved_at: None,
                })
                .await
                .unwrap(),
            AcceptToolCallOutcome::Accepted(_)
        ));

        // Model an ambiguous response: the request/event commits, but the
        // producer never consumes the receipt before a decision lands.
        let committed = store
            .request_tool_call_approval_and_append_event(&request, lease_token, 1, Utc::now())
            .await
            .unwrap();
        assert_eq!(
            committed.required_event.as_ref().map(|event| event.seq),
            Some(1)
        );
        assert!(matches!(
            store
                .decide_tool_call_approval(
                    chat.id,
                    request.call_id,
                    &ApprovalDecision::Reject {
                        reason: "cancelled while response was ambiguous".into(),
                    },
                    Utc::now(),
                )
                .await
                .unwrap(),
            DecideToolApprovalOutcome::Decided(_)
        ));

        let registration = ApprovalBroker::new(store)
            .register(
                request,
                Some(ApprovalJournalIdentity {
                    lease_token,
                    event_ordinal: 1,
                }),
            )
            .await;
        assert!(matches!(
            registration.publication,
            ApprovalRequiredPublication::Recovered {
                event_ordinal: 1,
                ref event,
            } if event.seq == 1
        ));
        assert_eq!(
            registration.decision.await,
            ApprovalDecision::Reject {
                reason: "cancelled while response was ambiguous".into()
            }
        );
    }
}
