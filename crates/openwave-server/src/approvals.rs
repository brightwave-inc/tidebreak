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
    DecideToolApprovalOutcome, GrantLevel, GrantScope, RequestToolApprovalOutcome, Result,
    StandingGrant, StandingGrants, Store,
};

/// Coordinates durable approval state with local low-latency waiters.
pub struct ApprovalBroker {
    store: Arc<dyn Store>,
    wake: Arc<Notify>,
    /// Test/embedded compatibility mirror. Foreground authorization always
    /// reads the durable store inside registration, never this process-local
    /// cache.
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

    /// Compatibility mirror for callers that inspect a decision immediately.
    /// It is deliberately not wired into foreground agent authorization.
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
        // Read before the decision so the grant is written at the level the
        // chat actually has, not one inferred after the fact.
        let chat_project_id = match rung {
            None => None,
            Some(_) => self
                .store
                .get_chat(chat_id)
                .await?
                .and_then(|chat| chat.project_id),
        };
        let grant = match scope {
            Some(scope) => match StandingGrant::scoped(
                // The level follows where the chat lives rather than being
                // put to the reader: a chat in a project grants across it, a
                // loose chat has nothing wider to mean. The card's label is
                // what states which one this is.
                GrantLevel::for_chat(current.chat_id, chat_project_id),
                current.tool_name.clone(),
                current.kind,
                scope,
                Utc::now(),
            ) {
                Some(grant) => Some(grant),
                None => return Ok(ResolveApprovalOutcome::GrantNotAvailable),
            },
            None => None,
        };
        let outcome = match grant.as_ref() {
            Some(grant) => {
                self.store
                    .decide_tool_call_approval_with_grant(
                        chat_id,
                        call_id,
                        &decision,
                        grant,
                        Utc::now(),
                    )
                    .await?
            }
            None => {
                self.store
                    .decide_tool_call_approval(chat_id, call_id, &decision, Utc::now())
                    .await?
            }
        };
        match outcome {
            DecideToolApprovalOutcome::Decided(_) | DecideToolApprovalOutcome::Existing(_) => {
                if let Some(grant) = grant {
                    self.standing_grants.record(grant);
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

impl ApprovalBroker {
    /// Land the Auto-mode judge's verdict on one parked call.
    ///
    /// A pure compare-and-set against durable state: a human decision that
    /// already landed wins, and `false` reports the judge no longer owned the
    /// call. An approval wakes the parked waiter exactly like a human click.
    pub async fn resolve_from_judge(
        &self,
        chat_id: ChatId,
        call_id: CallId,
        approved: bool,
    ) -> Result<bool> {
        let landed = self
            .store
            .resolve_tool_call_approval_from_judge(chat_id, call_id, approved)
            .await?;
        if landed && approved {
            self.wake.notify_waiters();
        }
        Ok(landed)
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
    // A narrow rung names the action, and the preview it would be named from is
    // clamped for display. Naming one from a clamped preview would authorize
    // every other call that clamps to the same text, so an approximate
    // description is refused rather than turned into standing authority.
    let candidate = match (rung, action) {
        (ApprovalGrantRung::WholeTool, _) => GrantScope::WholeTool,
        (ApprovalGrantRung::ExactAction, Some(action)) if action_is_exact => {
            GrantScope::ExactAction(action.clone())
        }
        (
            ApprovalGrantRung::CommandPrefix { tokens },
            Some(action @ ToolActionPreview::Exec { .. }),
        ) if action_is_exact => command_prefix_scope(action, tokens)?,
        _ => return None,
    };
    let available = match action {
        Some(action) => GrantScope::ladder_for_action(action),
        None => vec![GrantScope::WholeTool],
    };
    available.contains(&candidate).then_some(candidate)
}

/// Rebuild a prefix rung from the parked call rather than from the request.
///
/// The renderer sends only a length. The concrete tokens come from the
/// action the call is actually parked on, and the length is honored only if
/// the analyzer offered a prefix of exactly that many tokens for this
/// command — so a client cannot name a wider prefix than the card showed, and
/// cannot name one at all for a command whose ladder has none.
fn command_prefix_scope(
    action: &openwave_core::ToolActionPreview,
    tokens: usize,
) -> Option<GrantScope> {
    openwave_core::GrantScope::ladder_for_action(action)
        .into_iter()
        .find(|scope| {
            matches!(scope, GrantScope::CommandPrefix { tokens: offered } if offered.len() == tokens)
        })
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
                                    if approval.approved_by_standing_grant {
                                        ApprovalRequiredPublication::StandingGrant
                                    } else {
                                        ApprovalRequiredPublication::None
                                    },
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
                            RequestToolApprovalOutcome::Granted(approval) => {
                                break (approval, ApprovalRequiredPublication::StandingGrant);
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
                    Ok(RequestToolApprovalOutcome::Existing(approval)) => (
                        approval.clone(),
                        if approval.approved_by_standing_grant {
                            ApprovalRequiredPublication::StandingGrant
                        } else {
                            ApprovalRequiredPublication::None
                        },
                    ),
                    Ok(RequestToolApprovalOutcome::Granted(approval)) => {
                        (approval, ApprovalRequiredPublication::StandingGrant)
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
            permission_mode: None,
            network_policy: Default::default(),
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
                    raw_arguments: None,
                    execution: ToolCallExecution::Server,
                    status: ToolCallStatus::Pending,
                    result: None,
                    result_preview: None,
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
                auto_judge: false,
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

    async fn request_for(
        store: &Arc<dyn Store>,
        chat_id: ChatId,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> ApprovalRequest {
        let request = ApprovalRequest {
            auto_judge: false,
            call_id: CallId::new(),
            chat_id,
            turn_id: TurnId::new(),
            tool_name: tool_name.into(),
            class: ApprovalClass::Sensitive,
            kind: openwave_core::ToolApprovalKind::for_tool_name(tool_name),
            preview: None,
            summary: "private summary".into(),
        };
        assert!(matches!(
            store
                .accept_tool_call(&ToolCallRecord {
                    id: request.call_id,
                    chat_id,
                    turn_id: request.turn_id,
                    provider_id: format!("provider-{}", request.call_id),
                    name: tool_name.into(),
                    arguments,
                    raw_arguments: None,
                    execution: ToolCallExecution::Server,
                    status: ToolCallStatus::Pending,
                    result: None,
                    result_preview: None,
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
        request
    }

    /// The exclusion this replaces: `exec` was kept away from the judge
    /// because nothing deterministic stood beneath it. Something does now, so
    /// the line moved from "no command, ever" to "no command the analyzer has
    /// not already cleared".
    #[tokio::test]
    async fn only_a_command_that_cleared_the_analyzer_reaches_the_judge() {
        // A routine build command parks with a judge on it.
        let (store, mut request) = setup_with_arguments(
            "exec",
            json!({ "command": "cargo", "args": ["test"], "cwd": "." }),
        )
        .await;
        request.auto_judge = true;
        assert!(store
            .request_tool_call_approval(&request, Utc::now())
            .await
            .is_ok());
        let parked = store
            .get_tool_call_approval(request.call_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            parked.auto_judge_status,
            Some(openwave_core::AutoJudgeStatus::Judging)
        );

        // Anything the analyzer refuses parks as an ordinary card instead —
        // it loses its judge, not its card, decided on the arguments the row
        // actually holds rather than on what the caller believed.
        for arguments in [
            json!({ "command": "bash", "args": ["-c", "id"], "cwd": "." }),
            json!({ "command": "rm", "args": ["-rf", "/"], "cwd": "." }),
            json!({ "command": "cat", "args": ["../../outside.txt"], "cwd": "." }),
        ] {
            let (store, mut request) = setup_with_arguments("exec", arguments.clone()).await;
            request.auto_judge = true;
            store
                .request_tool_call_approval(&request, Utc::now())
                .await
                .unwrap();
            let parked = store
                .get_tool_call_approval(request.call_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                parked.auto_judge_status, None,
                "the judge must not be offered {arguments}"
            );
            assert_eq!(parked.status, openwave_core::ToolApprovalStatus::Pending);
        }
    }

    #[tokio::test]
    async fn judge_verdicts_are_a_cas_the_human_always_wins() {
        let (store, mut request) =
            setup_with_arguments("search", json!({ "query": "quarterly filings" })).await;
        request.auto_judge = true;
        let broker = ApprovalBroker::new(store.clone());
        let pending = broker.register(request.clone(), None).await;
        drop(pending.decision);

        // The park stamped judge ownership durably.
        let parked = store
            .get_tool_call_approval(request.call_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            parked.auto_judge_status,
            Some(openwave_core::AutoJudgeStatus::Judging)
        );

        // A decline moves only the marker: the call stays pending for the
        // human, and the judge cannot re-own it.
        assert!(broker
            .resolve_from_judge(request.chat_id, request.call_id, false)
            .await
            .unwrap());
        let declined = store
            .get_tool_call_approval(request.call_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(declined.status, openwave_core::ToolApprovalStatus::Pending);
        assert_eq!(
            declined.auto_judge_status,
            Some(openwave_core::AutoJudgeStatus::Declined)
        );
        assert!(!broker
            .resolve_from_judge(request.chat_id, request.call_id, true)
            .await
            .unwrap());

        // The human decides; a late judge approval on a human-owned card
        // no-ops rather than relabeling the decision.
        assert_eq!(
            broker
                .resolve(request.chat_id, request.call_id, ApprovalDecision::Approve)
                .await
                .unwrap(),
            ResolveApprovalOutcome::Resolved
        );
        assert!(!broker
            .resolve_from_judge(request.chat_id, request.call_id, true)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn a_judge_approval_resolves_the_parked_decision() {
        let (store, mut request) =
            setup_with_arguments("search", json!({ "query": "quarterly filings" })).await;
        request.auto_judge = true;
        let broker = ApprovalBroker::new(store.clone());
        let registration = broker.register(request.clone(), None).await;

        assert!(broker
            .resolve_from_judge(request.chat_id, request.call_id, true)
            .await
            .unwrap());
        assert_eq!(registration.decision.await, ApprovalDecision::Approve);
        let approved = store
            .get_tool_call_approval(request.call_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            approved.auto_judge_status,
            Some(openwave_core::AutoJudgeStatus::Approved)
        );
    }

    /// Create a chat filed under a fresh project, plus a parked exec request.
    async fn setup_in_project(
        arguments: serde_json::Value,
    ) -> (Arc<dyn Store>, openwave_core::ProjectId, ApprovalRequest) {
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("approval.db").display()
            ))
            .await
            .unwrap(),
        );
        std::mem::forget(db);
        let project = openwave_core::Project {
            id: openwave_core::ProjectId::new(),
            title: Some("Filings".into()),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_project(&project).await.unwrap();
        let chat = Chat {
            id: ChatId::new(),
            project_id: Some(project.id),
            title: Some("First".into()),
            model: None,
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let request = request_for(&store, chat.id, "exec", arguments).await;
        (store, project.id, request)
    }

    /// The whole point of the widening: a grant made in one conversation is
    /// still in force in the next one, instead of being re-asked forever.
    #[tokio::test]
    async fn a_grant_made_in_a_project_covers_the_next_chat_in_it() {
        let exec_args = json!({ "command": "cargo", "args": ["test"], "cwd": "." });
        let (store, project_id, request) = setup_in_project(exec_args.clone()).await;
        let broker = ApprovalBroker::new(store.clone());
        let pending = broker.register(request.clone(), None).await;
        drop(pending.decision);
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
        // The grant was written at the level the chat lives at, not at the
        // chat that happened to ask.
        let listed = store.list_standing_tool_grants().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].grant.level(), GrantLevel::Project { project_id });

        // A different chat in the same project is covered without asking.
        let sibling = Chat {
            id: ChatId::new(),
            project_id: Some(project_id),
            title: Some("Second".into()),
            model: None,
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&sibling).await.unwrap();
        let covered = request_for(&store, sibling.id, "exec", exec_args.clone()).await;
        let registration = broker.register(covered, None).await;
        assert!(matches!(
            registration.publication,
            openwave_core::ApprovalRequiredPublication::StandingGrant
        ));
        assert_eq!(registration.decision.await, ApprovalDecision::Approve);

        // A chat outside the project is not. A project grant must widen to
        // its project and no further.
        let outsider = Chat {
            id: ChatId::new(),
            project_id: None,
            title: Some("Loose".into()),
            model: None,
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&outsider).await.unwrap();
        let uncovered = request_for(&store, outsider.id, "exec", exec_args).await;
        let registration = broker.register(uncovered, None).await;
        assert!(!matches!(
            registration.publication,
            openwave_core::ApprovalRequiredPublication::StandingGrant
        ));
        drop(registration.decision);
    }

    /// The rung the ladder exists for, end to end: a grant taken for
    /// `cargo test` covers the next `cargo test` without asking, and does not
    /// quietly become a grant for every `cargo`.
    #[tokio::test]
    async fn a_prefix_grant_covers_its_subcommand_and_no_more() {
        let (store, request) = setup_with_arguments(
            "exec",
            json!({ "command": "cargo", "args": ["test", "--all"], "cwd": "." }),
        )
        .await;
        let broker = ApprovalBroker::new(store.clone());
        let pending = broker.register(request.clone(), None).await;
        drop(pending.decision);
        assert_eq!(
            broker
                .resolve_with_grant(
                    request.chat_id,
                    request.call_id,
                    ApprovalDecision::Approve,
                    // Two tokens: `cargo test`.
                    Some(crate::routes::ApprovalGrantRung::CommandPrefix { tokens: 2 }),
                )
                .await
                .unwrap(),
            ResolveApprovalOutcome::Resolved
        );

        let exec = |args: &[&str]| json!({ "command": "cargo", "args": args, "cwd": "." });
        // Another `cargo test` runs without asking...
        let covered = request_for(&store, request.chat_id, "exec", exec(&["test", "--lib"])).await;
        assert!(matches!(
            broker.register(covered, None).await.publication,
            openwave_core::ApprovalRequiredPublication::StandingGrant
        ));
        // ...but a different `cargo` subcommand still asks.
        let uncovered = request_for(&store, request.chat_id, "exec", exec(&["publish"])).await;
        let registration = broker.register(uncovered, None).await;
        assert!(!matches!(
            registration.publication,
            openwave_core::ApprovalRequiredPublication::StandingGrant
        ));
        drop(registration.decision);
    }

    /// A prefix the card never offered cannot be claimed by asking for it.
    #[tokio::test]
    async fn a_prefix_longer_than_the_ladder_offered_is_refused() {
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
                    // The ladder offers 1 and 2 tokens; 9 is not on it.
                    Some(crate::routes::ApprovalGrantRung::CommandPrefix { tokens: 9 }),
                )
                .await
                .unwrap(),
            ResolveApprovalOutcome::GrantNotAvailable
        );
    }

    #[tokio::test]
    async fn a_revoked_grant_leaves_the_list_and_stops_covering() {
        let exec_args = json!({ "command": "cargo", "args": ["test"], "cwd": "." });
        let (store, request) = setup_with_arguments("exec", exec_args.clone()).await;
        let broker = ApprovalBroker::new(store.clone());
        let pending = broker.register(request.clone(), None).await;
        drop(pending.decision);
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

        // The grant is findable, carrying the identity a revocation names.
        let listed = store.list_standing_tool_grants().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].source_call_id, request.call_id);
        assert_eq!(
            listed[0].grant.level(),
            GrantLevel::Chat {
                chat_id: request.chat_id
            }
        );
        assert_eq!(listed[0].grant.tool_name(), "exec");

        // A matching later call is auto-granted while the grant stands…
        let covered = request_for(&store, request.chat_id, "exec", exec_args.clone()).await;
        let registration = broker.register(covered, None).await;
        assert!(matches!(
            registration.publication,
            openwave_core::ApprovalRequiredPublication::StandingGrant
        ));
        assert_eq!(registration.decision.await, ApprovalDecision::Approve);

        // …and parks on the gate again once it is revoked.
        assert!(store
            .revoke_standing_tool_grant(request.call_id)
            .await
            .unwrap());
        assert!(store.list_standing_tool_grants().await.unwrap().is_empty());
        assert!(!store
            .revoke_standing_tool_grant(request.call_id)
            .await
            .unwrap());
        let uncovered = request_for(&store, request.chat_id, "exec", exec_args).await;
        let registration = broker.register(uncovered, None).await;
        assert!(!matches!(
            registration.publication,
            openwave_core::ApprovalRequiredPublication::StandingGrant
        ));
        drop(registration.decision);
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
            None,
            &request.tool_name,
            request.kind,
            &json!({})
        ));
    }

    #[tokio::test]
    async fn standing_grant_survives_broker_recreation_and_stays_chat_scoped() {
        let (store, first_request) =
            setup_with_arguments("web_search", json!({"query": "quarterly filings"})).await;
        let first = ApprovalBroker::new(store.clone());
        let _pending = first.register(first_request.clone(), None).await;
        assert_eq!(
            first
                .resolve_with_grant(
                    first_request.chat_id,
                    first_request.call_id,
                    ApprovalDecision::Approve,
                    Some(crate::routes::ApprovalGrantRung::ExactAction),
                )
                .await
                .unwrap(),
            ResolveApprovalOutcome::Resolved
        );

        // This broker has no in-process grant mirror from the first decision.
        // The immediate approval proves the restarted broker read the durable
        // row while registering the new canonical call.
        let restarted = ApprovalBroker::new(store.clone());
        let matching = request_for(
            &store,
            first_request.chat_id,
            "web_search",
            json!({"query": "quarterly filings"}),
        )
        .await;
        let registration = restarted.register(matching.clone(), None).await;
        assert!(matches!(
            registration.publication,
            ApprovalRequiredPublication::StandingGrant
        ));
        assert_eq!(registration.decision.await, ApprovalDecision::Approve);
        assert!(
            store
                .get_tool_call_approval(matching.call_id)
                .await
                .unwrap()
                .unwrap()
                .approved_by_standing_grant
        );

        let other_chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: Some("Other approval test".into()),
            model: None,
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&other_chat).await.unwrap();
        let other = request_for(
            &store,
            other_chat.id,
            "web_search",
            json!({"query": "quarterly filings"}),
        )
        .await;
        let pending = restarted.register(other, None).await;
        assert!(matches!(
            pending.publication,
            ApprovalRequiredPublication::Ordinary
        ));
        drop(pending.decision);
    }

    #[tokio::test]
    async fn a_decision_retry_cannot_turn_a_one_shot_approval_into_a_grant() {
        let (store, request) = setup("search").await;
        let broker = ApprovalBroker::new(store.clone());
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
                .resolve_with_grant(
                    request.chat_id,
                    request.call_id,
                    ApprovalDecision::Approve,
                    Some(crate::routes::ApprovalGrantRung::WholeTool),
                )
                .await
                .unwrap(),
            ResolveApprovalOutcome::DecisionConflict
        );

        let next = request_for(
            &store,
            request.chat_id,
            "search",
            json!({"query": "private"}),
        )
        .await;
        let pending = ApprovalBroker::new(store).register(next, None).await;
        assert!(matches!(
            pending.publication,
            ApprovalRequiredPublication::Ordinary
        ));
        drop(pending.decision);
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
            None,
            "exec",
            request.kind,
            &exec("cargo", &["test"]),
        ));
        // The grant was built from the arguments the call was parked on, so it
        // cannot stretch to a command the human never saw.
        assert!(!grants.covers(
            request.chat_id,
            None,
            "exec",
            request.kind,
            &exec("cargo", &["publish"]),
        ));
        assert!(!grants.covers(request.chat_id, None, "exec", request.kind, &json!({})));
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
            None,
            "web_search",
            request.kind,
            &query("quarterly filings"),
        ));
        // The grant names the query the card showed, so the next search still
        // asks.
        assert!(!grants.covers(
            request.chat_id,
            None,
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
                    Some(crate::routes::ApprovalGrantRung::CommandPrefix { tokens: 1 }),
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
            None,
            "exec",
            request.kind,
            &json!({})
        ));
    }

    #[tokio::test]
    async fn an_interpreter_call_cannot_resolve_with_a_grant_the_floor_refuses() {
        let (store, request) = setup_with_arguments(
            "exec",
            json!({
                "command": "python3",
                "args": ["-c", "import pptx"],
                "cwd": ".",
            }),
        )
        .await;
        let broker = ApprovalBroker::new(store.clone());
        let _pending = broker.register(request.clone(), None).await;

        for rung in [
            crate::routes::ApprovalGrantRung::ExactAction,
            crate::routes::ApprovalGrantRung::WholeTool,
        ] {
            assert_eq!(
                broker
                    .resolve_with_grant(
                        request.chat_id,
                        request.call_id,
                        ApprovalDecision::Approve,
                        Some(rung),
                    )
                    .await
                    .unwrap(),
                ResolveApprovalOutcome::GrantNotAvailable
            );
            assert_eq!(
                store
                    .get_tool_call_approval(request.call_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                openwave_core::ToolApprovalStatus::Pending
            );
        }

        // A human may still approve this exact invocation once.
        assert_eq!(
            broker
                .resolve(request.chat_id, request.call_id, ApprovalDecision::Approve)
                .await
                .unwrap(),
            ResolveApprovalOutcome::Resolved
        );
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
            None,
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
            permission_mode: None,
            network_policy: Default::default(),
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
            auto_judge: false,
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
                    raw_arguments: None,
                    execution: ToolCallExecution::Server,
                    status: ToolCallStatus::Pending,
                    result: None,
                    result_preview: None,
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
