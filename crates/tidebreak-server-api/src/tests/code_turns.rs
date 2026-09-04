//! Turn lifecycle: interrupts, queues, sibling sessions, fencing, and recovery.

use super::code::*;
use super::*;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{
    Attention, AttentionSource, AttentionState, CapLevel, Event, FenceReason, HarnessKind,
    PermissionMode, Session, SessionId, SessionLifecycle, TurnStatus, WorkspaceId,
};
use tidebreak_harness::HarnessEvent;

async fn code_app_with_browser(
    adapter: ScriptedAdapter,
    browser_runtime: Arc<RecordingBrowserRuntime>,
) -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    code_app_with_optional_browser(adapter, Some(browser_runtime)).await
}

fn browser_token_for_session(runtime: &CodeRuntime, session_id: SessionId) -> String {
    for entry in std::fs::read_dir(runtime.browser_tokens.capfile_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let body = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        let token = value["token"].as_str().unwrap();
        if runtime
            .browser_tokens
            .subject_for_token(token)
            .is_some_and(|subject| subject.session == session_id)
        {
            return token.to_owned();
        }
    }
    panic!("browser token for session {session_id} was not found")
}

fn mark_as_exited_orphan(session: &mut Session) {
    session.lifecycle = SessionLifecycle::Fenced;
    session.fence_reason = Some(FenceReason::OrphanAlive);
    // A stale identity on a live PID models PID reuse. Reap treats that as
    // proof that the recorded process exited and never signals the new owner.
    session.child_pid = Some(i64::from(std::process::id()));
    session.child_process_identity = Some("test:exited-orphan".into());
    session.attention = Attention::new(
        AttentionState::Fenced {
            reason: FenceReason::OrphanAlive,
        },
        AttentionSource::Lifecycle,
    );
}

#[tokio::test]
async fn interrupt_stops_a_running_turn_without_ending_its_browser_channel() {
    let browser_runtime = Arc::new(RecordingBrowserRuntime::default());
    let (router, token, runtime, dir) = code_app_with_browser(
        ScriptedAdapter::new(vec![
            HarnessEvent::TurnStarted,
            HarnessEvent::AssistantDelta {
                text: "working".into(),
            },
            HarnessEvent::TurnCompleted {
                usage: Default::default(),
            },
        ])
        .with_delay(Duration::from_millis(80)),
        browser_runtime.clone(),
    )
    .await;
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

    let session_id: SessionId = json_id(&session).parse().unwrap();
    let browser_token = browser_token_for_session(&runtime, session_id);
    let (mut events, _) = runtime.bus.attach(session_id);

    let turn_req = client
        .post(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "slow" }));
    let interrupt = async {
        // Wait for the turn to actually start before sending the interrupt.
        // A fixed sleep is flaky because the turn request may still be
        // queuing on a slow CI runner.
        loop {
            let event = events.recv().await.unwrap();
            if matches!(event.event, Event::TurnStarted { .. }) {
                break;
            }
        }
        client
            .post(format!(
                "http://{addr}/code/sessions/{}/interrupt",
                json_id(&session)
            ))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
    };
    let (turn, interrupted) = tokio::join!(turn_req.send(), interrupt);
    assert_eq!(interrupted.status(), reqwest::StatusCode::ACCEPTED);
    let turn = turn.unwrap();
    assert_eq!(turn.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(
        turn_statuses(&client, addr, &token, &session).await,
        ["interrupted"]
    );

    let browser_list = client
        .get(format!("http://{addr}/code/browser/list"))
        .bearer_auth(&browser_token)
        .send()
        .await
        .unwrap();
    assert_eq!(browser_list.status(), reqwest::StatusCode::OK);
    assert_eq!(browser_runtime.listed.lock().unwrap().len(), 1);
    assert!(browser_runtime.revoked.lock().unwrap().is_empty());
}

#[tokio::test]
async fn reap_replaces_browser_authority_without_tombstoning_the_session() {
    let browser_runtime = Arc::new(RecordingBrowserRuntime::default());
    let (router, token, runtime, dir) = code_app_with_browser(
        ScriptedAdapter::new(plain_text_script()),
        browser_runtime.clone(),
    )
    .await;
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
    let session_id: SessionId = json_id(&session).parse().unwrap();
    let old_browser_token = browser_token_for_session(&runtime, session_id);

    let initial_list = client
        .get(format!("http://{addr}/code/browser/list"))
        .bearer_auth(&old_browser_token)
        .send()
        .await
        .unwrap();
    assert_eq!(initial_list.status(), reqwest::StatusCode::OK);

    let owner = tidebreak_core::OwnerId::local();
    let mut row = tidebreak_core::db::code::get_session(&runtime.db, &owner, session_id)
        .await
        .unwrap()
        .unwrap();
    mark_as_exited_orphan(&mut row);
    tidebreak_core::db::code::save_session(&runtime.db, &row)
        .await
        .unwrap();

    let reaped = client
        .post(format!("http://{addr}/code/sessions/{session_id}/reap"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let status = reaped.status();
    let body = reaped.text().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::OK, "reap failed: {body}");

    let old_list = client
        .get(format!("http://{addr}/code/browser/list"))
        .bearer_auth(&old_browser_token)
        .send()
        .await
        .unwrap();
    assert_eq!(old_list.status(), reqwest::StatusCode::UNAUTHORIZED);

    let new_browser_token = browser_token_for_session(&runtime, session_id);
    assert_ne!(new_browser_token, old_browser_token);
    let new_list = client
        .get(format!("http://{addr}/code/browser/list"))
        .bearer_auth(&new_browser_token)
        .send()
        .await
        .unwrap();
    assert_eq!(new_list.status(), reqwest::StatusCode::OK);

    assert_eq!(browser_runtime.listed.lock().unwrap().len(), 2);
    assert!(browser_runtime.revoked.lock().unwrap().is_empty());

    // Model a launch failure that has already removed the transient channel.
    // A later terminal archive must still tombstone the database-backed
    // session scope in the native adapter.
    runtime.browser_tokens.revoke(session_id);
    let archived = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "force": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(archived.status(), reqwest::StatusCode::OK);
    let revoked = browser_runtime.revoked.lock().unwrap();
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0].session, session_id);
}

/// A killed engine reaches EOF exactly like a finished one. Reading that as
/// success journaled a Stop — and an OOM kill, or a signed-out engine — as a
/// completed turn with zero tokens.
#[tokio::test]
async fn an_engine_that_dies_without_saying_so_journals_an_interrupted_turn() {
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(vec![
            HarnessEvent::TurnStarted,
            HarnessEvent::AssistantDelta {
                text: "working".into(),
            },
            HarnessEvent::TurnCompleted {
                usage: Default::default(),
            },
        ])
        .with_delay(Duration::from_millis(80))
        .with_silent_interrupt(),
    )
    .await;
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

    let session_id: SessionId = json_id(&session).parse().unwrap();
    let (mut events, _) = runtime.bus.attach(session_id);

    let turn_req = client
        .post(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "slow" }));
    let interrupt = async {
        // Wait for the turn to actually start before sending the interrupt.
        // A fixed sleep is flaky because the turn request may still be
        // queuing on a slow CI runner.
        loop {
            let event = events.recv().await.unwrap();
            if matches!(event.event, Event::TurnStarted { .. }) {
                break;
            }
        }
        client
            .post(format!(
                "http://{addr}/code/sessions/{}/interrupt",
                json_id(&session)
            ))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
    };
    let (turn, interrupted) = tokio::join!(turn_req.send(), interrupt);
    assert_eq!(interrupted.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(turn.unwrap().status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(
        turn_statuses(&client, addr, &token, &session).await,
        ["interrupted"]
    );
}

/// Turn statuses for a session, oldest first.
async fn turn_statuses(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    session: &serde_json::Value,
) -> Vec<String> {
    let listed: Vec<serde_json::Value> = client
        .get(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(session)
        ))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    listed
        .iter()
        .map(|turn| turn["status"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[tokio::test]
async fn a_mid_turn_send_queues_and_runs_after_the_current_turn() {
    // Claude Code advertises mid_turn_steering: Unknown. Queue-default must
    // still accept the follow-up; it must not 409 as if this were a steer.
    // The delay keeps the first turn open across the queue CRUD below.
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(vec![
            HarnessEvent::TurnStarted,
            HarnessEvent::AssistantDelta {
                text: "working".into(),
            },
            HarnessEvent::TurnCompleted {
                usage: Default::default(),
            },
        ])
        .with_delay(Duration::from_millis(80))
        .with_steering(CapLevel::Unknown),
    )
    .await;
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
    let session_id = json_id(&session).to_owned();
    let parsed: SessionId = session_id.parse().unwrap();

    let first = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        let session_id = session_id.clone();
        async move {
            client
                .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "message": "first" }))
                .send()
                .await
                .unwrap()
        }
    });

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
            if row.lifecycle == SessionLifecycle::Running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("first turn never reached Running");

    let follow = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "follow-up" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        follow.status(),
        reqwest::StatusCode::ACCEPTED,
        "mid-turn submit must queue, not 409, even when steering is Unknown"
    );
    let follow_body: serde_json::Value = follow.json().await.unwrap();
    assert_eq!(follow_body["message"], "follow-up");
    assert_eq!(follow_body["position"], 0);
    assert!(
        follow_body.get("status").is_none(),
        "a queue receipt is a row, not a turn: {follow_body}"
    );
    let follow_id = follow_body["id"]
        .as_str()
        .expect("a queued row is addressable")
        .to_owned();

    // Depth is no longer one (decision 69): a second mid-turn send parks
    // behind the first instead of refusing with queue_full.
    let second = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "second follow-up" }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::ACCEPTED);
    let second_body: serde_json::Value = second.json().await.unwrap();
    assert_eq!(second_body["position"], 1);
    let second_id = second_body["id"].as_str().unwrap().to_owned();

    // The queue is durable and addressable while the live turn runs: list,
    // edit, and retract are real routes, exactly as on a chat.
    let listed: serde_json::Value = client
        .get(format!("http://{addr}/code/sessions/{session_id}/queued"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["paused"], false);
    let rows = listed["queued"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"].as_str().unwrap(), follow_id);

    let edited: serde_json::Value = client
        .patch(format!(
            "http://{addr}/code/sessions/{session_id}/queued/{second_id}"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "second follow-up, edited" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(edited["message"], "second follow-up, edited");

    let removed = client
        .delete(format!(
            "http://{addr}/code/sessions/{session_id}/queued/{second_id}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), reqwest::StatusCode::NO_CONTENT);

    assert_eq!(first.await.unwrap().status(), reqwest::StatusCode::ACCEPTED);

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let turns = tidebreak_core::db::code::list_turns(
                &runtime.db,
                &tidebreak_core::OwnerId::local(),
                parsed,
            )
            .await
            .unwrap();
            if turns.len() >= 2
                && turns[1].status == TurnStatus::Completed
                && turns[1].user_input == "follow-up"
            {
                assert_eq!(turns[0].user_input, "first");
                assert_eq!(turns[0].status, TurnStatus::Completed);
                let first_end = turns[0].ended_at.expect("first turn ended");
                assert!(
                    turns[1].started_at >= first_end,
                    "queued turn must start after the live turn ends"
                );
                assert_eq!(
                    turns[1].id.to_string(),
                    follow_id,
                    "the queue row's id is the promoted turn's id"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("queued follow-up did not run after the current turn completed");

    // The retracted second row must never have become a turn.
    let turns = tidebreak_core::db::code::list_turns(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap();
    assert_eq!(turns.len(), 2, "a retracted queued message must not run");
}

/// A turn script that always fails, the way an expired credential does.
fn always_failing_script() -> Vec<HarnessEvent> {
    vec![
        HarnessEvent::SessionStarted {
            harness_kind: tidebreak_core::HarnessKind::ClaudeCode,
            harness_version: "scripted".into(),
            resume_ref: Some("scripted-session".into()),
        },
        HarnessEvent::TurnStarted,
        HarnessEvent::TurnFailed {
            error: tidebreak_core::BoundedError {
                message: "Auth recovery succeeded but 4 authenticated inference requests \
                          were still rejected (401); giving up after 3 retries."
                    .into(),
            },
        },
    ]
}

/// A credential that stops working fails every turn identically, and the
/// session went on reporting `idle` — so the next turn, and the one after
/// that, were invited to fail the same way with no remedy offered.
#[tokio::test]
async fn a_session_whose_turns_keep_failing_is_fenced_rather_than_left_idle() {
    let (router, token, _runtime, dir) =
        code_app_with(ScriptedAdapter::new(always_failing_script())).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = create_sibling_sessions(&client, addr, &token, &workspace, 1)
        .await
        .remove(0);

    let lifecycle = |session: String| {
        let client = client.clone();
        let token = token.clone();
        async move {
            let body: serde_json::Value = client
                .get(format!("http://{addr}/code/sessions/{session}/debug"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            body["session"].clone()
        }
    };

    for attempt in 1..=3 {
        let response = client
            .post(format!("http://{addr}/code/sessions/{session}/turns"))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "message": format!("attempt {attempt}") }))
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "the turn is accepted even though the engine fails it"
        );

        let row = lifecycle(session.clone()).await;
        if attempt < 3 {
            assert_eq!(
                row["lifecycle"], "idle",
                "one or two failures are ordinary; do not fence early: {row}"
            );
            assert!(row["fence_reason"].is_null(), "{row}");
        }
    }

    let row = lifecycle(session.clone()).await;
    assert_eq!(
        row["lifecycle"], "fenced",
        "three failures in a row is a property of the session: {row}"
    );
    assert_eq!(
        row["fence_reason"]["type"], "repeated_turn_failures",
        "{row}"
    );
    assert_eq!(row["fence_reason"]["count"], 3, "{row}");
    assert!(
        row["fence_reason"]["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("401"),
        "the fence carries why, which the turn row never stored: {row}"
    );
}

#[tokio::test]
async fn a_fenced_session_closes_its_whole_workspace_to_turns() {
    // A fence means an engine may still be alive in this checkout from before
    // a restart, outside every lock this process holds. The turn lock cannot
    // order a process it does not own, so no sibling writes until the reap.
    let (router, token, runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let ids = create_sibling_sessions(&client, addr, &token, &workspace, 2).await;

    let owner = tidebreak_core::OwnerId::local();
    let fenced_id: SessionId = ids[0].parse().unwrap();
    let mut row = tidebreak_core::db::code::get_session(&runtime.db, &owner, fenced_id)
        .await
        .unwrap()
        .unwrap();
    mark_as_exited_orphan(&mut row);
    tidebreak_core::db::code::save_session(&runtime.db, &row)
        .await
        .unwrap();

    let refused = client
        .post(format!("http://{addr}/code/sessions/{}/turns", ids[1]))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "while a sibling is fenced" }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "workspace_fenced");

    // Reaping the fenced session reopens the workspace.
    let reaped = client
        .post(format!("http://{addr}/code/sessions/{}/reap", ids[0]))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let status = reaped.status();
    let body = reaped.text().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::OK, "reap failed: {body}");
    let accepted = client
        .post(format!("http://{addr}/code/sessions/{}/turns", ids[1]))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "after the reap" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        accepted.status(),
        reqwest::StatusCode::ACCEPTED,
        "the reap must reopen the workspace: {}",
        accepted.text().await.unwrap()
    );
}

#[tokio::test]
async fn a_sibling_fenced_for_repeated_failures_does_not_close_the_workspace() {
    // Repeated turn failures fence the session that hit them, but its engine
    // answered every time — an expired credential, a refused prompt, a
    // provider outage. No process is unaccounted for and the worktree is not
    // at risk, so a healthy sibling keeps working. Only a fence that implies
    // an engine outside our locks closes the workspace.
    let (router, token, runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let ids = create_sibling_sessions(&client, addr, &token, &workspace, 2).await;

    let owner = tidebreak_core::OwnerId::local();
    let fenced_id: SessionId = ids[0].parse().unwrap();
    let reason = FenceReason::RepeatedTurnFailures {
        count: 3,
        detail: "the provider refused three turns in a row".into(),
    };
    let mut row = tidebreak_core::db::code::get_session(&runtime.db, &owner, fenced_id)
        .await
        .unwrap()
        .unwrap();
    row.lifecycle = SessionLifecycle::Fenced;
    row.fence_reason = Some(reason.clone());
    row.attention = Attention::new(
        AttentionState::Fenced { reason },
        AttentionSource::Lifecycle,
    );
    tidebreak_core::db::code::save_session(&runtime.db, &row)
        .await
        .unwrap();

    let accepted = client
        .post(format!("http://{addr}/code/sessions/{}/turns", ids[1]))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "while a sibling is fenced" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        accepted.status(),
        reqwest::StatusCode::ACCEPTED,
        "a sibling fenced for repeated failures must not close the workspace: {}",
        accepted.text().await.unwrap()
    );
}

#[tokio::test]
async fn a_workspace_still_holds_only_one_watch_session() {
    // Watch keeps its cap: the fix loop belongs to the workspace, not to one
    // agent, so a second one would double every push. Record 54 lifted the cap
    // on interactive sessions only.
    let (router, token, runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let workspace_id: WorkspaceId = json_id(&workspace).parse().unwrap();
    let owner = tidebreak_core::OwnerId::local();

    let watch = |()| {
        runtime.create_session_of_kind(
            &owner,
            workspace_id,
            tidebreak_core::SessionKind::Watch,
            HarnessKind::ClaudeCode,
            crate::code::runtime::NewSessionSettings {
                permission_mode: PermissionMode::Plan,
                ..Default::default()
            },
        )
    };
    watch(()).await.expect("the first watch session creates");
    let second = watch(()).await.expect_err("a second watch must be refused");
    assert!(
        format!("{second:?}").contains("session_exists"),
        "unexpected error: {second:?}"
    );
}

#[tokio::test]
async fn a_recovered_session_accepts_a_turn() {
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
    let session_id = json_id(&session).to_owned();

    let restarted = Arc::new(CodeRuntime::with_registry(
        runtime.db.clone(),
        dir.path().to_path_buf(),
        scripted_registry(),
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

    let turn = reqwest::Client::new()
        .post(format!("http://{addr2}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token2)
        .json(&serde_json::json!({ "message": "after restart" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        turn.status(),
        reqwest::StatusCode::ACCEPTED,
        "recovered session must accept a turn: {}",
        turn.text().await.unwrap()
    );
    let body: serde_json::Value = turn.json().await.unwrap();
    assert_eq!(body["status"], "completed");
    assert_eq!(body["user_input"], "after restart");

    let parsed: SessionId = session_id.parse().unwrap();
    let mut row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap()
    .unwrap();
    mark_as_exited_orphan(&mut row);
    tidebreak_core::db::code::save_session(&runtime.db, &row)
        .await
        .unwrap();

    let fenced = Arc::new(CodeRuntime::with_registry(
        runtime.db.clone(),
        dir.path().to_path_buf(),
        scripted_registry(),
    ));
    fenced.recover().await.unwrap();
    let mut fenced_state = AppState::new(
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
    fenced_state.code = Some(fenced);
    let token3 = fenced_state.token.clone();
    let addr3 = serve(app(fenced_state)).await;
    let client3 = reqwest::Client::new();

    let after_orphan_exit = client3
        .post(format!("http://{addr3}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token3)
        .json(&serde_json::json!({ "message": "after orphan exit" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        after_orphan_exit.status(),
        reqwest::StatusCode::ACCEPTED,
        "boot recovery must attach a worker after the orphan exits: {}",
        after_orphan_exit.text().await.unwrap()
    );
    let after_body: serde_json::Value = after_orphan_exit.json().await.unwrap();
    assert_eq!(after_body["status"], "completed");
    assert_eq!(after_body["user_input"], "after orphan exit");
}

#[tokio::test]
async fn a_failed_checkpoint_does_not_fail_the_turn() {
    let (router, token, runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let worktree = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());

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

    // Replace the checkout with a non-repo so the snapshot fails after the
    // engine turn has already succeeded.
    std::fs::remove_dir_all(&worktree).unwrap();
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(worktree.join("orphan.txt"), "still here\n").unwrap();

    let turn = client
        .post(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "keep going" }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(turn["status"], "completed");
    assert!(turn["checkpoint_ref"].is_null());

    let events = journaled_events(&runtime.db, json_id(&session).parse().unwrap()).await;
    assert!(
        events.iter().any(|framed| {
            matches!(
                framed.event,
                tidebreak_core::Event::HarnessNotice {
                    level: tidebreak_core::HarnessNoticeLevel::Warning,
                    ..
                }
            )
        }),
        "expected a warning notice, got {events:?}"
    );
}
