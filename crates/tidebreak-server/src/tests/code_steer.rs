//! Steering an active turn through the engine channel.

use super::code::*;

use std::time::Duration;

use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{CapLevel, CodeEvent, CodeSessionId, CodeTurnId, CodeTurnStatus};

#[tokio::test]
async fn unsupported_explicit_steer_is_refused_without_queueing() {
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(plain_text_script()).with_delay(Duration::from_millis(40)),
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
    let turn = tokio::spawn({
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
    let active_turn_id = wait_for_open_turn(&runtime, parsed).await;
    let refused = client
        .post(format!("http://{addr}/code/sessions/{session_id}/steer"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "expected_turn_id": active_turn_id,
            "guidance": "redirect",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "steering_unavailable");
    assert_eq!(turn.await.unwrap().status(), reqwest::StatusCode::ACCEPTED);
}

#[tokio::test]
async fn supported_steer_reaches_the_active_turn_once_without_creating_a_follow_up() {
    // Park on an approval so the active-turn window is event-driven rather
    // than a short wall-clock delay. Fixed delays race on loaded Windows CI.
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(approval_script())
            .with_approvals(CapLevel::Supported)
            .with_steering(CapLevel::Supported),
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
            "permission_mode": "ask",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let session_id = json_id(&session).to_owned();
    let parsed: CodeSessionId = session_id.parse().unwrap();
    let (mut events, _) = runtime.bus.attach(parsed);

    let turn = tokio::spawn({
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
    let active_turn_id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let CodeEvent::TurnStarted { turn_id } = events.recv().await.unwrap().event {
                break turn_id;
            }
        }
    })
    .await
    .expect("turn never started");
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

    let first_steer = client
        .post(format!("http://{addr}/code/sessions/{session_id}/steer"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "expected_turn_id": active_turn_id,
            "guidance": "try the other file",
        }))
        .send();
    let second_steer = client
        .post(format!("http://{addr}/code/sessions/{session_id}/steer"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "expected_turn_id": active_turn_id,
            "guidance": "keep the public API small",
        }))
        .send();
    let (first_steer, second_steer) = tokio::join!(first_steer, second_steer);
    assert_eq!(first_steer.unwrap().status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(
        second_steer.unwrap().status(),
        reqwest::StatusCode::ACCEPTED
    );

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
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), turn)
            .await
            .expect("turn never finished after the mid-turn decision")
            .unwrap()
            .status(),
        reqwest::StatusCode::ACCEPTED
    );

    let mut steer_events = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = events.recv().await.unwrap().event;
            if let CodeEvent::UserSteered { text, .. } = &event {
                steer_events.push(text.clone());
            }
            if matches!(event, CodeEvent::TurnCompleted { .. }) {
                break;
            }
        }
    })
    .await
    .expect("turn never completed");
    steer_events.sort();
    assert_eq!(
        steer_events,
        ["keep the public API small", "try the other file"]
    );

    let turns = tidebreak_core::db::code::list_turns(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap();
    assert_eq!(turns.len(), 1, "steering must not create a follow-up turn");
    assert_eq!(turns[0].user_input, "first");
}

#[tokio::test]
async fn supported_steer_requires_an_active_turn() {
    let (router, token, _runtime, dir) =
        code_app_with(ScriptedAdapter::new(plain_text_script()).with_steering(CapLevel::Supported))
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

    let refused = client
        .post(format!(
            "http://{addr}/code/sessions/{}/steer",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "expected_turn_id": CodeTurnId::new(),
            "guidance": "too late",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "no_active_turn");
}

#[tokio::test]
async fn steer_rejects_blank_nul_and_oversized_guidance() {
    let (router, token, _runtime, dir) =
        code_app_with(ScriptedAdapter::new(plain_text_script()).with_steering(CapLevel::Supported))
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

    for guidance in [
        "   ".to_owned(),
        "contains\0nul".to_owned(),
        "x".repeat(tidebreak_core::TurnSteer::MAX_CONTENT_LEN + 1),
    ] {
        let refused = client
            .post(format!(
                "http://{addr}/code/sessions/{}/steer",
                json_id(&session)
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "expected_turn_id": CodeTurnId::new(),
                "guidance": guidance,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), reqwest::StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn stale_turn_steering_is_rejected_before_reaching_the_adapter() {
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(plain_text_script())
            .with_delay(Duration::from_millis(80))
            .with_steering(CapLevel::Supported),
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
    let turn = tokio::spawn({
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
    let active_turn_id = wait_for_open_turn(&runtime, parsed).await;
    let stale_turn_id = loop {
        let candidate = CodeTurnId::new();
        if candidate != active_turn_id {
            break candidate;
        }
    };

    let refused = client
        .post(format!("http://{addr}/code/sessions/{session_id}/steer"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "expected_turn_id": stale_turn_id,
            "guidance": "wrong turn",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "stale_turn");
    assert_eq!(turn.await.unwrap().status(), reqwest::StatusCode::ACCEPTED);

    let events = journaled_events(&runtime.db, parsed).await;
    assert!(
        events
            .iter()
            .all(|event| !matches!(event.event, CodeEvent::UserSteered { .. })),
        "stale steering reached the adapter: {events:?}"
    );
}

#[tokio::test]
async fn stalled_native_steering_times_out_without_wedging_turn_completion() {
    // Hold the turn open on an approval so a slow runner cannot finish the
    // script before the stalled steer is admitted. The worker must still
    // bound the control and let the parked turn complete after a decision.
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(approval_script())
            .with_approvals(CapLevel::Supported)
            .with_steering_delay(Duration::from_secs(1)),
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
            "permission_mode": "ask",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let session_id = json_id(&session).to_owned();
    let parsed: CodeSessionId = session_id.parse().unwrap();
    let turn = tokio::spawn({
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
    let active_turn_id = wait_for_open_turn(&runtime, parsed).await;
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

    let started = tokio::time::Instant::now();
    let refused = client
        .post(format!("http://{addr}/code/sessions/{session_id}/steer"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "expected_turn_id": active_turn_id,
            "guidance": "this adapter never answers",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    assert!(
        started.elapsed() < Duration::from_millis(750),
        "worker-level timeout did not bound the stalled control"
    );
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "steering_rejected");

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
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), turn)
            .await
            .expect("turn completion was wedged")
            .unwrap()
            .status(),
        reqwest::StatusCode::ACCEPTED
    );
}

#[tokio::test]
async fn terminal_turn_event_closes_steering_before_a_late_command_is_admitted() {
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(plain_text_script())
            .with_delay(Duration::from_millis(20))
            .with_steering(CapLevel::Supported),
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
    let (mut events, _) = runtime.bus.attach(parsed);
    let turn = tokio::spawn({
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
    let mut active_turn_id = None;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match events.recv().await.unwrap().event {
                CodeEvent::TurnStarted { turn_id } => active_turn_id = Some(turn_id),
                CodeEvent::TurnCompleted { .. } => break,
                _ => {}
            }
        }
    })
    .await
    .expect("turn never completed");

    let refused = client
        .post(format!("http://{addr}/code/sessions/{session_id}/steer"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "expected_turn_id": active_turn_id.expect("turn id"),
            "guidance": "too late",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "no_active_turn");
    assert_eq!(turn.await.unwrap().status(), reqwest::StatusCode::ACCEPTED);
}

#[tokio::test]
async fn a_native_steer_rejection_does_not_fail_or_redirect_the_turn() {
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(plain_text_script())
            .with_delay(Duration::from_millis(40))
            .with_steering_rejection("turn is no longer steerable"),
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
    let (mut events, _) = runtime.bus.attach(parsed);
    let turn = tokio::spawn({
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
    let active_turn_id = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let CodeEvent::TurnStarted { turn_id } = events.recv().await.unwrap().event {
                break turn_id;
            }
        }
    })
    .await
    .expect("turn never started");

    let refused = client
        .post(format!("http://{addr}/code/sessions/{session_id}/steer"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "expected_turn_id": active_turn_id,
            "guidance": "redirect",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "steering_rejected");
    assert_eq!(turn.await.unwrap().status(), reqwest::StatusCode::ACCEPTED);

    let turns = tidebreak_core::db::code::list_turns(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].status, CodeTurnStatus::Completed);
}
