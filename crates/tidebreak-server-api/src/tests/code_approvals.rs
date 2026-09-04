//! Approval delivery, abandonment, and attention.

use super::code::*;
use super::*;

use std::sync::Arc;
use std::time::Duration;

use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{
    AttentionSource, AttentionState, CapLevel, DbStore, Event, HarnessKind, SessionId,
    SessionLifecycle, TurnId,
};
use tidebreak_harness::{AdapterRegistry, ApprovalDecision, HarnessApprovalRef, HarnessEvent};

fn oversized_write_approval_script() -> Vec<HarnessEvent> {
    write_approval_script("toolu_oversized", &"x".repeat(20 * 1024))
}

/// An engine that asks for an approval and then stops waiting for one. Claude
/// Code times a parked permission prompt out after 60 seconds, resolves the
/// tool call `failed`, and finishes the turn with nobody having decided.
fn timed_out_approval_script() -> Vec<HarnessEvent> {
    let mut events = undecided_approval_script("toolu_timeout");
    events.insert(
        3,
        HarnessEvent::ToolCompleted {
            call_id: "toolu_timeout".into(),
            outcome: tidebreak_core::ToolOutcome::Failed,
            preview: "Bash tool call timed out after 60s".into(),
            detail: None,
            parent_call_id: None,
        },
    );
    events
}

/// The same request, but the engine never reports the call at all: only the
/// turn boundary can settle the row.
fn dropped_approval_script() -> Vec<HarnessEvent> {
    undecided_approval_script("toolu_dropped")
}

fn undecided_approval_script(call_id: &str) -> Vec<HarnessEvent> {
    vec![
        HarnessEvent::SessionStarted {
            harness_kind: HarnessKind::ClaudeCode,
            harness_version: "scripted".into(),
            resume_ref: Some("scripted-session".into()),
        },
        HarnessEvent::TurnStarted,
        HarnessEvent::ApprovalRequested {
            harness_ref: HarnessApprovalRef::engine(call_id),
            raw: serde_json::json!({
                "tool_name": "Bash",
                "input": { "command": "rm -rf /tmp/scratch" },
                "tool_use_id": call_id
            }),
            kind: None,
        },
        HarnessEvent::TurnCompleted {
            usage: Default::default(),
        },
    ]
}

/// Register a repo and workspace, open an Ask session, and run one turn to
/// completion. Only useful for scripts that never park, which is every script
/// modelling an approval nobody decides.
async fn ran_one_ask_turn(
    adapter: ScriptedAdapter,
) -> (
    reqwest::Client,
    std::net::SocketAddr,
    Arc<str>,
    SessionId,
    Arc<CodeRuntime>,
    tempfile::TempDir,
) {
    let (router, token, runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session: serde_json::Value = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "ask",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_id: SessionId = json_id(&session).parse().unwrap();
    let turn = tokio::time::timeout(
        Duration::from_secs(10),
        client
            .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "message": "run it" }))
            .send(),
    )
    .await
    .expect("the turn must not park on an approval nobody answers")
    .unwrap();
    assert_eq!(turn.status(), reqwest::StatusCode::ACCEPTED);
    (client, addr, token, session_id, runtime, dir)
}

/// Every approval on the session, whatever its state.
async fn approvals_for(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    session_id: SessionId,
) -> Vec<serde_json::Value> {
    client
        .get(format!(
            "http://{addr}/code/approvals?session_id={session_id}"
        ))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// The outcome carried on every `ApprovalResolved` the session journaled.
async fn journaled_resolutions(
    db: &DbStore,
    session_id: SessionId,
) -> Vec<tidebreak_core::ApprovalDecisionKind> {
    journaled_events(db, session_id)
        .await
        .into_iter()
        .filter_map(|framed| match framed.event {
            Event::ApprovalResolved { decision, .. } => Some(decision),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn mid_turn_decision_is_delivered_while_run_turn_is_still_executing() {
    let adapter = ScriptedAdapter::new(approval_script())
        .with_approvals(CapLevel::Supported)
        .with_delay(Duration::from_millis(20));
    let observed = adapter.clone();
    let (router, token, runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "ask",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(session.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = session.json().await.unwrap();
    let session_id = json_id(&session).to_owned();

    let turn = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        let session_id = session_id.clone();
        async move {
            client
                .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "message": "write it" }))
                .send()
                .await
                .unwrap()
        }
    });

    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let listed = client
                .get(format!("http://{addr}/code/approvals?state=pending"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .json::<Vec<serde_json::Value>>()
                .await
                .unwrap();
            if let Some(row) = listed.into_iter().next() {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("pending approval never appeared");

    let parsed: SessionId = session_id.parse().unwrap();
    let row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(row.lifecycle, SessionLifecycle::Running);
    assert!(!turn.is_finished(), "run_turn must still be executing");

    let decided = tokio::time::timeout(Duration::from_secs(2), async {
        client
            .post(format!(
                "http://{addr}/code/approvals/{}/decision",
                approval["id"].as_str().unwrap()
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "decision": "approve" }))
            .send()
            .await
            .unwrap()
    })
    .await
    .expect("decision must complete while run_turn is parked; a worker that cannot multiplex deadlocks here");
    assert_eq!(decided.status(), reqwest::StatusCode::OK);
    assert_eq!(
        observed.observed_decisions().len(),
        1,
        "the harness must observe the decision before the turn ends"
    );

    let finished = tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .expect("turn must finish after the mid-turn decision")
        .unwrap();
    assert_eq!(finished.status(), reqwest::StatusCode::ACCEPTED);
    let body: serde_json::Value = finished.json().await.unwrap();
    assert_eq!(body["status"], "completed");
    let events = journaled_events(&runtime.db, parsed).await;
    let kinds: Vec<&str> = events
        .iter()
        .map(|framed| match &framed.event {
            tidebreak_core::Event::ApprovalRequested { .. } => "requested",
            tidebreak_core::Event::ApprovalResolved { .. } => "resolved",
            // Deltas stream and are never journaled; the message that states
            // the same text is what the turn leaves behind (record 57).
            tidebreak_core::Event::AssistantMessage { .. } => "message",
            tidebreak_core::Event::TurnCompleted { .. } => "completed",
            _ => "other",
        })
        .collect();
    assert!(kinds.contains(&"requested"));
    assert!(kinds.contains(&"resolved"));
    assert!(kinds.contains(&"message"));
    assert!(kinds.contains(&"completed"));
    assert!(events.iter().any(|framed| matches!(
        &framed.event,
        tidebreak_core::Event::AssistantMessage { text, .. } if text == "after the decision"
    )));
    let requested = kinds.iter().position(|k| *k == "requested").unwrap();
    let message = kinds.iter().position(|k| *k == "message").unwrap();
    let completed = kinds.iter().position(|k| *k == "completed").unwrap();
    assert!(requested < message);
    assert!(message < completed);
}

#[tokio::test]
async fn concurrent_approval_decisions_deliver_exactly_once() {
    let adapter = ScriptedAdapter::new(approval_script()).with_approvals(CapLevel::Supported);
    let observed = adapter.clone();
    let (router, token, runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "ask",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let session_id = json_id(&session).to_owned();
    let turn = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        let session_id = session_id.clone();
        async move {
            client
                .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "message": "write it" }))
                .send()
                .await
                .unwrap()
        }
    });
    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let listed = client
                .get(format!("http://{addr}/code/approvals?state=pending"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .json::<Vec<serde_json::Value>>()
                .await
                .unwrap();
            if let Some(row) = listed.into_iter().next() {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("pending approval never appeared");
    let approval_id = approval["id"].as_str().unwrap().to_owned();
    let decide = |decision: &'static str| {
        let client = client.clone();
        let token = token.clone();
        let approval_id = approval_id.clone();
        async move {
            client
                .post(format!(
                    "http://{addr}/code/approvals/{approval_id}/decision"
                ))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "decision": decision }))
                .send()
                .await
                .unwrap()
        }
    };
    let (approve, deny) = tokio::join!(decide("approve"), decide("deny"));
    let statuses = [approve.status(), deny.status()];
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == reqwest::StatusCode::OK)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == reqwest::StatusCode::CONFLICT)
            .count(),
        1
    );
    assert_eq!(observed.observed_decisions().len(), 1);

    let finished = tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .expect("the winning decision did not release the turn")
        .unwrap();
    assert_eq!(finished.status(), reqwest::StatusCode::ACCEPTED);
    let rows = approvals_for(
        &client,
        addr,
        &token,
        session_id.parse::<SessionId>().unwrap(),
    )
    .await;
    assert_eq!(rows.len(), 1);
    assert!(matches!(
        rows[0]["state"].as_str(),
        Some("approved" | "denied")
    ));
    let resolutions =
        journaled_resolutions(&runtime.db, session_id.parse::<SessionId>().unwrap()).await;
    assert_eq!(resolutions.len(), 1);
}

#[tokio::test]
async fn a_definite_native_approval_delivery_failure_is_abandoned() {
    let adapter = ScriptedAdapter::new(approval_script())
        .with_approvals(CapLevel::Supported)
        .with_approval_delivery_error("the native waiter closed");
    let observed = adapter.clone();
    let (router, token, runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "ask",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let session_id = json_id(&session).to_owned();
    let turn = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        let session_id = session_id.clone();
        async move {
            client
                .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "message": "write it" }))
                .send()
                .await
                .unwrap()
        }
    });
    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let listed = client
                .get(format!("http://{addr}/code/approvals?state=pending"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .json::<Vec<serde_json::Value>>()
                .await
                .unwrap();
            if let Some(row) = listed.into_iter().next() {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("pending approval never appeared");

    let refused = client
        .post(format!(
            "http://{addr}/code/approvals/{}/decision",
            approval["id"].as_str().unwrap()
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "decision": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "approval_delivery_failed");
    assert!(observed.observed_decisions().is_empty());

    let parsed: SessionId = session_id.parse().unwrap();
    let rows = approvals_for(&client, addr, &token, parsed).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["state"], "abandoned");
    assert_eq!(
        journaled_resolutions(&runtime.db, parsed).await,
        vec![tidebreak_core::ApprovalDecisionKind::Abandoned]
    );

    let interrupted = client
        .post(format!(
            "http://{addr}/code/sessions/{session_id}/interrupt"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(interrupted.status(), reqwest::StatusCode::ACCEPTED);
    let _ = tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .expect("interrupt did not release the failed approval turn");
}

#[tokio::test]
async fn shutdown_waits_for_an_accepted_approval_to_finish_durably() {
    let adapter = ScriptedAdapter::new(approval_script())
        .with_approvals(CapLevel::Supported)
        .with_approval_ack_delay(Duration::from_millis(500));
    let observed = adapter.clone();
    let (router, token, runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "ask",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let session_id = json_id(&session).to_owned();
    let turn = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        let session_id = session_id.clone();
        async move {
            client
                .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "message": "write it" }))
                .send()
                .await
                .unwrap()
        }
    });
    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let listed = client
                .get(format!("http://{addr}/code/approvals?state=pending"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .json::<Vec<serde_json::Value>>()
                .await
                .unwrap();
            if let Some(row) = listed.into_iter().next() {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("pending approval never appeared");
    let approval_id = approval["id"].as_str().unwrap().to_owned();

    let decision = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        let approval_id = approval_id.clone();
        async move {
            client
                .post(format!(
                    "http://{addr}/code/approvals/{approval_id}/decision"
                ))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "decision": "approve" }))
                .send()
                .await
                .unwrap()
        }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while observed.observed_decisions().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the scripted engine never accepted the approval");

    let archive = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        let workspace_id = json_id(&workspace).to_owned();
        async move {
            client
                .post(format!(
                    "http://{addr}/code/workspaces/{workspace_id}/archive"
                ))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "force": true }))
                .send()
                .await
                .unwrap()
        }
    });

    let decided = tokio::time::timeout(Duration::from_secs(3), decision)
        .await
        .expect("the delayed approval acknowledgement never completed")
        .unwrap();
    assert_eq!(decided.status(), reqwest::StatusCode::OK);
    let archived = tokio::time::timeout(Duration::from_secs(5), archive)
        .await
        .expect("archive did not resume after approval finalization")
        .unwrap();
    assert_eq!(archived.status(), reqwest::StatusCode::OK);

    let parsed: SessionId = session_id.parse().unwrap();
    let rows = approvals_for(&client, addr, &token, parsed).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["state"], "approved");
    assert_eq!(
        journaled_resolutions(&runtime.db, parsed).await,
        vec![tidebreak_core::ApprovalDecisionKind::Approve]
    );
    let _ = tokio::time::timeout(Duration::from_secs(5), turn).await;
}

#[tokio::test]
async fn an_unknown_approval_delivery_stays_claimed_until_restart_recovery() {
    let adapter = ScriptedAdapter::new(approval_script())
        .with_approvals(CapLevel::Supported)
        .with_approval_ack_delay(Duration::from_secs(2));
    let observed = adapter.clone();
    let (router, token, runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "ask",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let session_id = json_id(&session).to_owned();
    let turn = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        let session_id = session_id.clone();
        async move {
            client
                .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "message": "write it" }))
                .send()
                .await
                .unwrap()
        }
    });
    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let listed = client
                .get(format!("http://{addr}/code/approvals?state=pending"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .json::<Vec<serde_json::Value>>()
                .await
                .unwrap();
            if let Some(row) = listed.into_iter().next() {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("pending approval never appeared");
    let approval_id = approval["id"]
        .as_str()
        .unwrap()
        .parse::<tidebreak_core::ApprovalId>()
        .unwrap();

    let unknown = client
        .post(format!(
            "http://{addr}/code/approvals/{approval_id}/decision"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "decision": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = unknown.json().await.unwrap();
    assert_eq!(body["kind"], "approval_delivery_unknown");
    assert_eq!(
        observed.observed_decisions().len(),
        1,
        "the timeout is ambiguous because the native engine accepted the decision"
    );

    let claimed = tidebreak_core::db::code::get_approval(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        approval_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(claimed.state, tidebreak_core::ApprovalState::Pending);
    assert!(claimed.decision_claim.is_some());
    assert!(claimed.decided_at.is_none());

    let finished = tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .expect("the turn did not resume after the decision timeout")
        .unwrap();
    assert_eq!(finished.status(), reqwest::StatusCode::ACCEPTED);

    let restarted = Arc::new(CodeRuntime::with_registry(
        runtime.db.clone(),
        dir.path().to_path_buf(),
        {
            let mut registry = AdapterRegistry::new();
            registry.register(Arc::new(
                ScriptedAdapter::new(plain_text_script()).with_approvals(CapLevel::Supported),
            ));
            registry
        },
    ));
    restarted.recover().await.unwrap();

    let recovered = tidebreak_core::db::code::get_approval(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        approval_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(recovered.state, tidebreak_core::ApprovalState::Abandoned);
    assert!(recovered.decision_claim.is_none());
    assert!(recovered.decided_at.is_some());
    assert_eq!(
        journaled_resolutions(&runtime.db, session_id.parse::<SessionId>().unwrap()).await,
        vec![tidebreak_core::ApprovalDecisionKind::Abandoned]
    );
}

#[tokio::test]
async fn deny_feedback_reaches_the_scripted_engine() {
    let adapter = ScriptedAdapter::new(approval_script()).with_approvals(CapLevel::Supported);
    let observed = adapter.clone();
    let (router, token, runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "ask",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(session.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = session.json().await.unwrap();

    let turn = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        let session_id = json_id(&session).to_owned();
        async move {
            client
                .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "message": "write it" }))
                .send()
                .await
                .unwrap()
        }
    });

    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let listed = client
                .get(format!("http://{addr}/code/approvals?state=pending"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap();
            assert_eq!(listed.status(), reqwest::StatusCode::OK);
            let body: Vec<serde_json::Value> = listed.json().await.unwrap();
            if let Some(row) = body.into_iter().next() {
                let parsed: SessionId = json_id(&session).parse().unwrap();
                let session = tidebreak_core::db::code::get_session(
                    &runtime.db,
                    &tidebreak_core::OwnerId::local(),
                    parsed,
                )
                .await
                .unwrap()
                .unwrap();
                if matches!(
                    session.attention.state,
                    AttentionState::NeedsYou {
                        source: AttentionSource::Structured,
                        ..
                    }
                ) {
                    return row;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("pending approval never appeared");
    assert!(approval["harness_raw_json"]
        .as_str()
        .unwrap_or("")
        .contains("Write"));

    let parsed: SessionId = json_id(&session).parse().unwrap();
    let row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        row.attention.state,
        AttentionState::NeedsYou { .. }
    ));
    assert_eq!(row.attention.source, AttentionSource::Structured);

    let decided = client
        .post(format!(
            "http://{addr}/code/approvals/{}/decision",
            approval["id"].as_str().unwrap()
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "decision": "deny",
            "feedback": "no — use the fixtures directory instead",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(decided.status(), reqwest::StatusCode::OK);
    let decided: serde_json::Value = decided.json().await.unwrap();
    assert_eq!(decided["state"], "denied");
    assert_eq!(
        decided["feedback"],
        "no — use the fixtures directory instead"
    );

    let finished = turn.await.unwrap();
    assert_eq!(finished.status(), reqwest::StatusCode::ACCEPTED);
    let seen = observed.observed_decisions();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "toolu_scripted");
    assert_eq!(
        seen[0].1,
        ApprovalDecision::Deny {
            feedback: Some("no — use the fixtures directory instead".into()),
        }
    );
}

#[tokio::test]
async fn restart_abandons_an_approval_whose_native_waiter_was_lost() {
    let first = ScriptedAdapter::new(approval_script()).with_approvals(CapLevel::Supported);
    let (router, token, runtime, dir) = code_app_with(first).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "ask",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let session_id = json_id(&session).to_owned();
    tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        let session_id = session_id.clone();
        async move {
            let _ = client
                .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "message": "write it" }))
                .send()
                .await;
        }
    });
    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let listed = client
                .get(format!("http://{addr}/code/approvals?state=pending"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .json::<Vec<serde_json::Value>>()
                .await
                .unwrap();
            if let Some(row) = listed.into_iter().next() {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("pending approval never appeared");

    let second = ScriptedAdapter::new(plain_text_script()).with_approvals(CapLevel::Supported);
    let observed = second.clone();
    let restarted = Arc::new(CodeRuntime::with_registry(
        runtime.db.clone(),
        dir.path().to_path_buf(),
        {
            let mut registry = AdapterRegistry::new();
            registry.register(Arc::new(second));
            registry
        },
    ));
    restarted.recover().await.unwrap();
    let mut state = AppState::new(
        Config::desktop(dir.path()),
        runtime.db.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    state.code = Some(restarted);
    let token2 = state.token.clone();
    let addr2 = serve(app(state)).await;

    let pending = reqwest::Client::new()
        .get(format!("http://{addr2}/code/approvals?state=pending"))
        .bearer_auth(&token2)
        .send()
        .await
        .unwrap()
        .json::<Vec<serde_json::Value>>()
        .await
        .unwrap();
    assert!(pending.is_empty());

    let rows = reqwest::Client::new()
        .get(format!(
            "http://{addr2}/code/approvals?session_id={session_id}"
        ))
        .bearer_auth(&token2)
        .send()
        .await
        .unwrap()
        .json::<Vec<serde_json::Value>>()
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], approval["id"]);
    assert_eq!(rows[0]["state"], "abandoned");

    let parsed: SessionId = session_id.parse().unwrap();
    let row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!matches!(
        row.attention.state,
        AttentionState::NeedsYou {
            source: AttentionSource::Structured,
            ..
        }
    ));

    let decided = reqwest::Client::new()
        .post(format!(
            "http://{addr2}/code/approvals/{}/decision",
            approval["id"].as_str().unwrap()
        ))
        .bearer_auth(&token2)
        .json(&serde_json::json!({ "decision": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(decided.status(), reqwest::StatusCode::CONFLICT);
    assert!(observed.observed_decisions().is_empty());
}

/// The bug this fixes: an engine that times its own tool call out leaves the
/// approval row `pending` forever, so `code approvals` lists a request nobody
/// can act on and `code approve` reports success for a command that never ran.
/// The completion carries the `tool_use_id` the approval is parked on, which
/// is the join.
#[tokio::test]
async fn an_approval_is_abandoned_when_its_tool_call_resolves_undecided() {
    let adapter = ScriptedAdapter::new(timed_out_approval_script())
        .with_approvals(CapLevel::Supported)
        .with_unattended_approvals();
    let (client, addr, token, session_id, runtime, _dir) = ran_one_ask_turn(adapter).await;

    let rows = approvals_for(&client, addr, &token, session_id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["state"], "abandoned");
    assert!(
        rows[0]["decided_at"].is_string(),
        "an abandoned approval is settled, so it carries a decided_at"
    );

    let pending: Vec<serde_json::Value> = client
        .get(format!("http://{addr}/code/approvals?state=pending"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        pending.is_empty(),
        "a settled approval must leave the pending list"
    );

    assert_eq!(
        journaled_resolutions(&runtime.db, session_id).await,
        vec![tidebreak_core::ApprovalDecisionKind::Abandoned],
        "the journal is how the CLI and the UI learn the request went undecided"
    );
}

#[tokio::test]
async fn a_stale_worker_completion_cannot_abandon_a_reused_call_id() {
    let adapter = ScriptedAdapter::new(plain_text_script()).with_approvals(CapLevel::Supported);
    let (router, token, runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "ask",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(session.status(), reqwest::StatusCode::CREATED);
    let session = session.json::<serde_json::Value>().await.unwrap();
    let session_id = json_id(&session).parse::<SessionId>().unwrap();
    let turn = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "finish" }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let turn_id = json_id(&turn).parse::<TurnId>().unwrap();
    let row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        session_id,
    )
    .await
    .unwrap()
    .unwrap();
    let stale_epoch = row.spawn_epoch - 1;
    let stale_id = tidebreak_core::ApprovalId::new();
    let current_id = tidebreak_core::ApprovalId::new();
    for (id, worker_epoch) in [(stale_id, stale_epoch), (current_id, row.spawn_epoch)] {
        tidebreak_core::db::code::insert_approval(
            &runtime.db,
            &row.owner,
            &tidebreak_core::Approval {
                actor: None,
                id,
                session_id,
                turn_id,
                kind: tidebreak_core::ApprovalKind::Other {
                    summary: "run command".into(),
                },
                harness_raw: serde_json::json!({"call_id":"toolu_reused"}),
                native_call_id: Some("toolu_reused".into()),
                server_capability: None,
                request_sha256: None,
                worker_epoch: Some(worker_epoch),
                decision_claim: None,
                claimed_at: None,
                state: tidebreak_core::ApprovalState::Pending,
                feedback: None,
                requested_at: chrono::Utc::now(),
                decided_at: None,
                auto_judge_status: None,
            },
        )
        .await
        .unwrap();
    }

    crate::code::approval_sweep::abandon_for_call(
        &runtime.db,
        &runtime.bus,
        &row.owner,
        session_id,
        stale_epoch,
        "toolu_reused",
    )
    .await;

    assert_eq!(
        tidebreak_core::db::code::get_approval(&runtime.db, &row.owner, stale_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        tidebreak_core::ApprovalState::Abandoned
    );
    assert_eq!(
        tidebreak_core::db::code::get_approval(&runtime.db, &row.owner, current_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        tidebreak_core::ApprovalState::Pending,
        "a stale completion must not settle the replacement worker's approval"
    );
}

/// Deciding a settled approval must fail out loud. A missing native waiter is
/// a delivery failure, so the durable row prevents a later false success.
#[tokio::test]
async fn deciding_an_abandoned_approval_is_refused() {
    let adapter = ScriptedAdapter::new(timed_out_approval_script())
        .with_approvals(CapLevel::Supported)
        .with_unattended_approvals();
    let observed = adapter.clone();
    let (client, addr, token, session_id, runtime, _dir) = ran_one_ask_turn(adapter).await;

    let rows = approvals_for(&client, addr, &token, session_id).await;
    let approval_id = rows[0]["id"].as_str().unwrap().to_owned();
    let refused = client
        .post(format!(
            "http://{addr}/code/approvals/{approval_id}/decision"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "decision": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert!(
        body.to_string().contains("no longer awaiting a decision"),
        "the refusal must say why, got {body}"
    );

    let rows = approvals_for(&client, addr, &token, session_id).await;
    assert_eq!(rows[0]["state"], "abandoned");
    assert_eq!(
        journaled_resolutions(&runtime.db, session_id).await,
        vec![tidebreak_core::ApprovalDecisionKind::Abandoned],
        "a refused decision journals nothing"
    );
    assert!(
        observed.observed_decisions().is_empty(),
        "a refused decision never reaches the engine"
    );
}

/// A tool call the engine drops without reporting must not leave a row pending
/// forever either. The turn boundary is the backstop.
#[tokio::test]
async fn a_turn_that_ends_without_a_tool_result_abandons_its_approval() {
    let adapter = ScriptedAdapter::new(dropped_approval_script())
        .with_approvals(CapLevel::Supported)
        .with_unattended_approvals();
    let (client, addr, token, session_id, runtime, _dir) = ran_one_ask_turn(adapter).await;

    let rows = approvals_for(&client, addr, &token, session_id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["state"], "abandoned");
    assert!(rows[0]["decided_at"].is_string());
    assert_eq!(
        journaled_resolutions(&runtime.db, session_id).await,
        vec![tidebreak_core::ApprovalDecisionKind::Abandoned]
    );

    let session = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        session_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        !matches!(session.attention.state, AttentionState::NeedsYou { .. }),
        "nothing is waiting on the user once the request is settled"
    );
}

#[tokio::test]
async fn oversized_write_approval_is_still_decidable() {
    let adapter =
        ScriptedAdapter::new(oversized_write_approval_script()).with_approvals(CapLevel::Supported);
    let observed = adapter.clone();
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "ask",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(session.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = session.json().await.unwrap();

    let turn = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        let session_id = json_id(&session).to_owned();
        async move {
            client
                .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "message": "write it" }))
                .send()
                .await
                .unwrap()
        }
    });

    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let listed = client
                .get(format!("http://{addr}/code/approvals?state=pending"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .json::<Vec<serde_json::Value>>()
                .await
                .unwrap();
            if let Some(row) = listed.into_iter().next() {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("pending approval never appeared");

    let raw = approval["harness_raw_json"].as_str().unwrap_or("");
    let stored: serde_json::Value = serde_json::from_str(raw).unwrap();
    assert_eq!(stored["truncated"], true);
    assert_eq!(stored["call_id"], "toolu_oversized");
    assert!(stored.get("tool_use_id").is_none());

    let decided = client
        .post(format!(
            "http://{addr}/code/approvals/{}/decision",
            approval["id"].as_str().unwrap()
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "decision": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(decided.status(), reqwest::StatusCode::OK);

    let finished = turn.await.unwrap();
    assert_eq!(finished.status(), reqwest::StatusCode::ACCEPTED);
    let seen = observed.observed_decisions();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "toolu_oversized");
    assert_eq!(seen[0].1, ApprovalDecision::Approve);
}

#[tokio::test(flavor = "multi_thread")]
async fn attention_follows_approval_completion_and_view() {
    let adapter = ScriptedAdapter::new(approval_script())
        .with_approvals(CapLevel::Supported)
        .with_delay(Duration::from_millis(20));
    let (router, token, runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "ask",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let session_id = json_id(&session).to_owned();

    let turn = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        let session_id = session_id.clone();
        async move {
            client
                .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "message": "write it" }))
                .send()
                .await
                .unwrap()
        }
    });

    let parsed: SessionId = session_id.parse().unwrap();
    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let listed = client
                .get(format!("http://{addr}/code/approvals?state=pending"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap();
            let body: Vec<serde_json::Value> = listed.json().await.unwrap();
            if let Some(row) = body.into_iter().next() {
                let session = tidebreak_core::db::code::get_session(
                    &runtime.db,
                    &tidebreak_core::OwnerId::local(),
                    parsed,
                )
                .await
                .unwrap()
                .unwrap();
                if matches!(
                    session.attention.state,
                    AttentionState::NeedsYou {
                        source: AttentionSource::Structured,
                        ..
                    }
                ) {
                    return row;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("pending structured NeedsYou never appeared");

    let decided = client
        .post(format!(
            "http://{addr}/code/approvals/{}/decision",
            approval["id"].as_str().unwrap()
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "decision": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(decided.status(), reqwest::StatusCode::OK);

    let after_decision = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        !matches!(
            after_decision.attention.state,
            AttentionState::NeedsYou {
                source: AttentionSource::Structured,
                ..
            }
        ),
        "decision must lift structured NeedsYou, got {:?}",
        after_decision.attention
    );

    let finished = turn.await.unwrap();
    assert_eq!(finished.status(), reqwest::StatusCode::ACCEPTED);
    let row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(row.attention.state, AttentionState::DoneUnreviewed);

    let mut request = format!("ws://{addr}/code/sessions/{session_id}/events?after=0")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (_socket, _) = connect_async(request).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let row = tidebreak_core::db::code::get_session(
                &runtime.db,
                &tidebreak_core::OwnerId::local(),
                parsed,
            )
            .await
            .unwrap()
            .unwrap();
            // Idle, not Working. Viewing settles the session; it does not
            // start an engine. This asserted `Working` while `Idle` did not
            // exist, which is how the derivation came to claim a finished
            // session was busy.
            if row.attention.state == AttentionState::Idle {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("viewing the session must settle it to Idle, not report it Working");
}

#[tokio::test]
async fn user_can_pin_and_clear_attention() {
    let (router, token, runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let session_id = json_id(&session);

    let pinned = client
        .post(format!(
            "http://{addr}/code/sessions/{session_id}/attention"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "note": "look at this later" }))
        .send()
        .await
        .unwrap();
    assert_eq!(pinned.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = pinned.json().await.unwrap();
    assert_eq!(body["attention"]["state"]["type"], "manual");
    assert_eq!(body["attention"]["state"]["note"], "look at this later");
    assert_eq!(body["attention"]["source"], "user");

    let parsed: SessionId = session_id.parse().unwrap();
    let mut row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap()
    .unwrap();
    row.lifecycle = SessionLifecycle::Running;
    tidebreak_core::db::code::save_session(&runtime.db, &row)
        .await
        .unwrap();
    crate::code::attention::sweep_stalled(&runtime.db, &runtime.bus, 0)
        .await
        .unwrap();
    let row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(row.attention.state, AttentionState::Manual { .. }),
        "Manual must survive the stall sweep"
    );

    let cleared = client
        .post(format!(
            "http://{addr}/code/sessions/{session_id}/attention"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "clear": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(cleared.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = cleared.json().await.unwrap();
    assert_ne!(body["attention"]["state"]["type"], "manual");
}

/// A session restored at boot comes back supervised. Recovery re-attaches its
/// worker, and that worker's approval endpoint is minted from the bound
/// loopback address — so recovery has to run after the address is published.
/// The other order silently restores Ask- and Auto-mode sessions with no
/// approval channel: an engine that can never ask.
#[tokio::test]
async fn a_recovered_session_keeps_its_approval_channel() {
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(plain_text_script()).with_approvals(CapLevel::Supported),
    )
    .await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let created = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "ask",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);

    let adapter = ScriptedAdapter::new(plain_text_script()).with_approvals(CapLevel::Supported);
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(adapter.clone()));
    let restarted = Arc::new(CodeRuntime::with_registry(
        runtime.db.clone(),
        dir.path().to_path_buf(),
        registry,
    ));
    restarted
        .start("http://127.0.0.1:4242".into())
        .await
        .unwrap();

    assert_eq!(
        adapter.launched_approvals(),
        vec![Some(
            "http://127.0.0.1:4242/code/mcp/approval-prompt".to_owned()
        )],
        "recovery must re-attach the session with a live approval endpoint"
    );
}
