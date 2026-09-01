//! Turn lifecycle: interrupts, queues, sibling sessions, fencing, and recovery.

use super::code::*;
use super::*;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{
    Attention, AttentionSource, AttentionState, CapLevel, CodeEvent, CodeSession, CodeSessionId,
    CodeSessionLifecycle, CodeTurnStatus, FenceReason, HarnessKind, PermissionMode, WorkspaceId,
};
use tidebreak_harness::HarnessEvent;

async fn code_app_with_browser(
    adapter: ScriptedAdapter,
    browser_runtime: Arc<RecordingBrowserRuntime>,
) -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    code_app_with_optional_browser(adapter, Some(browser_runtime)).await
}

fn browser_token_for_session(runtime: &CodeRuntime, session_id: CodeSessionId) -> String {
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

fn mark_as_exited_orphan(session: &mut CodeSession) {
    session.lifecycle = CodeSessionLifecycle::Fenced;
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

    let session_id: CodeSessionId = json_id(&session).parse().unwrap();
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
            if matches!(event.event, CodeEvent::TurnStarted { .. }) {
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
    let session_id: CodeSessionId = json_id(&session).parse().unwrap();
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

    let session_id: CodeSessionId = json_id(&session).parse().unwrap();
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
            if matches!(event.event, CodeEvent::TurnStarted { .. }) {
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
    let parsed: CodeSessionId = session_id.parse().unwrap();

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
            if row.lifecycle == CodeSessionLifecycle::Running {
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
                && turns[1].status == CodeTurnStatus::Completed
                && turns[1].user_input == "follow-up"
            {
                assert_eq!(turns[0].user_input, "first");
                assert_eq!(turns[0].status, CodeTurnStatus::Completed);
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

#[tokio::test]
async fn a_workspace_runs_several_agents_that_take_turns_on_one_worktree() {
    // Record 54: conversations are unlimited, the checkout is not. A send to a
    // session whose sibling is mid-turn has to queue rather than run, or the
    // two harnesses edit the same files at once.
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(plain_text_script()).with_delay(Duration::from_millis(120)),
    )
    .await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;

    let mut ids = Vec::new();
    for _ in 0..2 {
        let created = client
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
            .unwrap();
        assert_eq!(
            created.status(),
            reqwest::StatusCode::CREATED,
            "a second agent in one workspace must create"
        );
        let body: serde_json::Value = created.json().await.unwrap();
        ids.push(json_id(&body).to_owned());
    }
    assert_ne!(ids[0], ids[1]);

    let first_id = ids[0].clone();
    let first = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        async move {
            client
                .post(format!("http://{addr}/code/sessions/{first_id}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "message": "first" }))
                .send()
                .await
                .unwrap()
        }
    });

    let first_parsed: CodeSessionId = ids[0].parse().unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let row = tidebreak_core::db::code::get_session(
                &runtime.db,
                &tidebreak_core::OwnerId::local(),
                first_parsed,
            )
            .await
            .unwrap()
            .unwrap();
            if row.lifecycle == CodeSessionLifecycle::Running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the first agent never reached Running");

    let sibling = client
        .post(format!("http://{addr}/code/sessions/{}/turns", ids[1]))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "second" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        sibling.status(),
        reqwest::StatusCode::ACCEPTED,
        "a send while a sibling holds the worktree must queue, not run"
    );

    assert_eq!(first.await.unwrap().status(), reqwest::StatusCode::ACCEPTED);

    let second_parsed: CodeSessionId = ids[1].parse().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let first_turns = tidebreak_core::db::code::list_turns(
                &runtime.db,
                &tidebreak_core::OwnerId::local(),
                first_parsed,
            )
            .await
            .unwrap();
            let second_turns = tidebreak_core::db::code::list_turns(
                &runtime.db,
                &tidebreak_core::OwnerId::local(),
                second_parsed,
            )
            .await
            .unwrap();
            if second_turns.first().map(|turn| turn.status) == Some(CodeTurnStatus::Completed) {
                let first_end = first_turns[0].ended_at.expect("the first turn ended");
                assert!(
                    second_turns[0].started_at >= first_end,
                    "the sibling's turn must start after the worktree frees"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the queued sibling turn never ran");
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
async fn two_idle_siblings_sending_at_once_get_one_turn_and_one_queue() {
    // Both sends leave before either session is marked Running, so no
    // database read can tell them apart — the turn lock is the only thing
    // that can, and taking it is the reservation. The reader sees one turn
    // and one queued message; neither request is left open for the length of
    // the other's turn.
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(plain_text_script()).with_delay(Duration::from_millis(400)),
    )
    .await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let ids = create_sibling_sessions(&client, addr, &token, &workspace, 2).await;

    let send = |session_id: String, message: &'static str| {
        let client = client.clone();
        let token = token.clone();
        tokio::spawn(async move {
            let response = client
                .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "message": message }))
                .send()
                .await
                .unwrap();
            let status = response.status();
            let body: serde_json::Value = response.json().await.unwrap();
            (status, body)
        })
    };
    let first = send(ids[0].clone(), "first");
    let second = send(ids[1].clone(), "second");
    let (first, second) = tokio::time::timeout(Duration::from_secs(20), async {
        (first.await.unwrap(), second.await.unwrap())
    })
    .await
    .expect("a send blocked on the sibling's turn instead of queueing");

    for (status, body) in [&first, &second] {
        assert_eq!(*status, reqwest::StatusCode::ACCEPTED, "unexpected: {body}");
    }
    // A queued reply carries the parked message and its position; a turn
    // reply carries the turn. Exactly one of each is the whole contract.
    let queued = [&first, &second]
        .into_iter()
        .filter(|(_, body)| body.get("position").is_some())
        .count();
    assert_eq!(
        queued, 1,
        "one send must queue and one must run: {first:?} {second:?}"
    );

    // Both turns still land, one after the other, on the shared checkout.
    let parsed: Vec<CodeSessionId> = ids.iter().map(|id| id.parse().unwrap()).collect();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let mut windows = Vec::new();
            for id in &parsed {
                let turns = tidebreak_core::db::code::list_turns(
                    &runtime.db,
                    &tidebreak_core::OwnerId::local(),
                    *id,
                )
                .await
                .unwrap();
                match turns.first() {
                    Some(turn) if turn.status == CodeTurnStatus::Completed => {
                        windows.push((turn.started_at, turn.ended_at.expect("a completed turn")));
                    }
                    _ => break,
                }
            }
            if windows.len() == parsed.len() {
                windows.sort_by_key(|(started, _)| *started);
                assert!(
                    windows[1].0 >= windows[0].1,
                    "the turns overlapped on one checkout: {windows:?}"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("both turns never completed");
}

#[tokio::test]
async fn interrupting_a_queued_turn_stops_it_before_it_reaches_the_worktree() {
    // A queued turn waits for the sibling's turn to end, and that wait has to
    // keep answering control. Otherwise a stop pressed while the message is
    // still queued is delivered only once the turn has started, which reads
    // as the stop being ignored.
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(plain_text_script()).with_delay(Duration::from_millis(2_000)),
    )
    .await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let ids = create_sibling_sessions(&client, addr, &token, &workspace, 2).await;

    let holder_id = ids[0].clone();
    let holder = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        async move {
            client
                .post(format!("http://{addr}/code/sessions/{holder_id}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "message": "long" }))
                .send()
                .await
                .unwrap()
        }
    });
    let first_parsed: CodeSessionId = ids[0].parse().unwrap();
    wait_for_open_turn(&runtime, first_parsed).await;

    let queued = client
        .post(format!("http://{addr}/code/sessions/{}/turns", ids[1]))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "queued" }))
        .send()
        .await
        .unwrap();
    assert_eq!(queued.status(), reqwest::StatusCode::ACCEPTED);
    assert!(
        queued
            .json::<serde_json::Value>()
            .await
            .unwrap()
            .get("position")
            .is_some(),
        "the sibling send must queue while the worktree is held"
    );

    // By now the worker has taken the message out of its queue and is waiting
    // on the checkout: parking happens before the send's response is written,
    // so a whole HTTP round trip has passed since. The stop has to be
    // answered from inside that wait, not after it.
    let stopped = tokio::time::timeout(
        Duration::from_secs(1),
        client
            .post(format!("http://{addr}/code/sessions/{}/interrupt", ids[1]))
            .bearer_auth(&token)
            .send(),
    )
    .await
    .expect("the stop waited for the sibling's turn instead of being answered")
    .unwrap();
    assert_eq!(stopped.status(), reqwest::StatusCode::ACCEPTED);

    assert_eq!(
        holder.await.unwrap().status(),
        reqwest::StatusCode::ACCEPTED
    );
    let second_parsed: CodeSessionId = ids[1].parse().unwrap();
    let turns = tidebreak_core::db::code::list_turns(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        second_parsed,
    )
    .await
    .unwrap();
    assert!(
        turns.is_empty(),
        "a stopped queued turn must never reach the worktree: {turns:?}"
    );
    // Stop declines to start the turn; it does not delete the message. The
    // row stays in the durable queue, and the queue reads paused — sibling
    // turn completion releases the checkout but wakes nobody, so an unpaused
    // queue here would look live while nothing will ever run it.
    let rows = tidebreak_core::db::code::list_queued_turns(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        second_parsed,
    )
    .await
    .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "the stopped message must stay queued, not vanish"
    );
    assert_eq!(rows[0].message, "queued");
    assert!(
        tidebreak_core::db::code::queue_paused(
            &runtime.db,
            &tidebreak_core::OwnerId::local(),
            second_parsed
        )
        .await
        .unwrap(),
        "a stop aimed at queued work must pause the queue so the tray says so"
    );

    // Send-now is the release: it clears the pause and wakes the worker, and
    // the held message finally promotes now that the checkout is free. The
    // turn row appearing is the proof — this fixture's engine takes two
    // seconds per event, so waiting for completion would time the test on
    // the script, not on the queue.
    let released = client
        .post(format!(
            "http://{addr}/code/sessions/{}/queued/send-now",
            ids[1]
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(released.status(), reqwest::StatusCode::NO_CONTENT);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let turns = tidebreak_core::db::code::list_turns(
                &runtime.db,
                &tidebreak_core::OwnerId::local(),
                second_parsed,
            )
            .await
            .unwrap();
            if turns.iter().any(|turn| turn.user_input == "queued") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("send-now must revive the paused queue once the checkout is free");
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
    let fenced_id: CodeSessionId = ids[0].parse().unwrap();
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
    let fenced_id: CodeSessionId = ids[0].parse().unwrap();
    let reason = FenceReason::RepeatedTurnFailures {
        count: 3,
        detail: "the provider refused three turns in a row".into(),
    };
    let mut row = tidebreak_core::db::code::get_session(&runtime.db, &owner, fenced_id)
        .await
        .unwrap()
        .unwrap();
    row.lifecycle = CodeSessionLifecycle::Fenced;
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
            tidebreak_core::CodeSessionKind::Watch,
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

    let parsed: CodeSessionId = session_id.parse().unwrap();
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

    let stuck = client3
        .post(format!("http://{addr3}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token3)
        .json(&serde_json::json!({ "message": "while fenced" }))
        .send()
        .await
        .unwrap();
    assert_eq!(stuck.status(), reqwest::StatusCode::CONFLICT);
    let stuck_body: serde_json::Value = stuck.json().await.unwrap();
    assert_eq!(stuck_body["kind"], "session_fenced");

    let reaped = client3
        .post(format!("http://{addr3}/code/sessions/{session_id}/reap"))
        .bearer_auth(&token3)
        .send()
        .await
        .unwrap();
    let status = reaped.status();
    let body = reaped.text().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::OK, "reap failed: {body}");
    let after_reap: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(after_reap["lifecycle"], "idle");

    let after = client3
        .post(format!("http://{addr3}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token3)
        .json(&serde_json::json!({ "message": "after reap" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        after.status(),
        reqwest::StatusCode::ACCEPTED,
        "reap must attach a worker: {}",
        after.text().await.unwrap()
    );
    let after_body: serde_json::Value = after.json().await.unwrap();
    assert_eq!(after_body["status"], "completed");
    assert_eq!(after_body["user_input"], "after reap");
}

#[tokio::test]
async fn a_completed_turn_records_a_checkpoint_and_serves_bounded_review() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let worktree = std::path::Path::new(workspace["worktree_path"].as_str().unwrap());

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

    std::fs::write(worktree.join("added.txt"), "new file\n").unwrap();
    std::fs::write(worktree.join("README.md"), "hello from turn\n").unwrap();

    let turn = client
        .post(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "edit the tree" }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(turn["status"], "completed");
    let checkpoint = turn["checkpoint_ref"].as_str().expect("checkpoint_ref");
    assert!(
        checkpoint.contains("refs/tidebreak/checkpoints/"),
        "{checkpoint}"
    );
    assert!(turn["diffstat"]["files"].as_u64().unwrap() >= 1);

    let files = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/files?turn={}",
            json_id(&workspace),
            json_id(&turn)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let paths: Vec<&str> = files["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|file| file["path"].as_str())
        .collect();
    assert!(paths.contains(&"added.txt"), "{paths:?}");

    let diff = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/diff?turn={}&file=added.txt",
            json_id(&workspace),
            json_id(&turn)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(
        diff["diff"].as_str().unwrap().contains("new file"),
        "{}",
        diff["diff"]
    );
    assert_eq!(diff["truncated"], false);

    let events = journaled_events(&_runtime.db, json_id(&session).parse().unwrap()).await;
    assert!(
        events.iter().any(|framed| {
            matches!(
                framed.event,
                tidebreak_core::CodeEvent::CheckpointRecorded { .. }
            )
        }),
        "expected CheckpointRecorded in {events:?}"
    );

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
    let leftover = std::process::Command::new("git")
        .args([
            "for-each-ref",
            "--format=%(refname)",
            "refs/tidebreak/checkpoints/",
        ])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        leftover.status.success(),
        "{}",
        String::from_utf8_lossy(&leftover.stderr)
    );
    assert!(
        String::from_utf8_lossy(&leftover.stdout).trim().is_empty(),
        "archive must drop checkpoint refs: {}",
        String::from_utf8_lossy(&leftover.stdout)
    );
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
                tidebreak_core::CodeEvent::HarnessNotice {
                    level: tidebreak_core::HarnessNoticeLevel::Warning,
                    ..
                }
            )
        }),
        "expected a warning notice, got {events:?}"
    );
}

/// Decision 0064: a session-long engine child idle past the threshold is
/// parked — the engine records the call, the row's pid clears — and the next
/// turn simply runs again. Every other test runs per-turn scripted children,
/// which keep the park timer disarmed.
#[tokio::test]
async fn an_idle_engine_child_is_parked_and_the_next_turn_still_runs() {
    let adapter = ScriptedAdapter::new(plain_text_script())
        .with_child_pid(4242)
        .with_session_long_child();
    let observer = adapter.clone();
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
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let session_id = json_id(&session).to_owned();
    let parsed: CodeSessionId = session_id.parse().unwrap();

    let first = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "one" }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), reqwest::StatusCode::ACCEPTED);
    tokio::time::timeout(Duration::from_secs(5), async {
        while turn_statuses(&client, addr, &token, &session).await != ["completed"] {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the first turn never completed");

    // The park timer (150 ms under cfg(test)) fires once the worker sits
    // idle, and the row's pid clears with it.
    wait_until(|| observer.park_count() > 0).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let row = tidebreak_core::db::code::get_session(
                &runtime.db,
                &tidebreak_core::OwnerId::local(),
                parsed,
            )
            .await
            .unwrap()
            .expect("the parked session row still exists");
            if row.child_pid.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a parked child must leave no pid on the row");

    // Parking is invisible to the caller: the next turn just runs.
    let second = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "two" }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::ACCEPTED);
    tokio::time::timeout(Duration::from_secs(5), async {
        while turn_statuses(&client, addr, &token, &session).await != ["completed", "completed"] {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the wake turn never completed");
}
