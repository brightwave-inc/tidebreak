//! Live session settings: modes, reasoning effort, fast mode, and model switches.

use super::code::*;
use super::*;

use std::time::Duration;

use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{
    CapLevel, CodeSessionId, CodeSessionLifecycle, FenceReason, HarnessKind, PermissionMode,
    ReasoningEffort,
};

async fn execute_code_db_unprepared(dir: &tempfile::TempDir, statement: &str) {
    let database = dir.path().join("code.db");
    let connection = sea_orm::Database::connect(format!("sqlite://{}?mode=rw", database.display()))
        .await
        .unwrap();
    connection.execute_unprepared(statement).await.unwrap();
    connection.close().await.unwrap();
}

/// Plan mode had no exit. The mode was settable only at creation, so once a
/// user approved a plan the engine kept refusing the edits and the only way
/// forward was a new session with a new conversation.
#[tokio::test]
async fn a_session_can_leave_plan_mode_and_the_engine_relaunches_under_the_new_one() {
    let (router, token, _runtime, dir) = code_app_with(
        ScriptedAdapter::new(plain_text_script()).with_auto_mode(CapLevel::Supported),
    )
    .await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = create_sibling_sessions(&client, addr, &token, &workspace, 1)
        .await
        .remove(0);

    let set_mode = |mode: &'static str| {
        let client = client.clone();
        let token = token.clone();
        let session = session.clone();
        async move {
            let response = client
                .post(format!("http://{addr}/code/sessions/{session}/mode"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "permission_mode": mode }))
                .send()
                .await
                .unwrap();
            let status = response.status();
            let body: serde_json::Value = response.json().await.unwrap();
            (status, body)
        }
    };

    let (status, body) = set_mode("auto").await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(
        body["permission_mode"], "auto",
        "the change is reported on the session it returns: {body}"
    );

    // It stuck, and the engine came back up under it rather than being left
    // stopped by the relaunch.
    let reread: serde_json::Value = client
        .get(format!("http://{addr}/code/sessions/{session}/debug"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reread["session"]["permission_mode"], "auto", "{reread}");
    assert_ne!(
        reread["session"]["lifecycle"], "ended",
        "the relaunch must leave a usable session: {reread}"
    );

    // Setting the mode it already has is a no-op, not a needless relaunch.
    let (status, body) = set_mode("auto").await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["permission_mode"], "auto");

    // A mode this engine cannot honor is refused here, not approximated at
    // the next turn: the scripted adapter declares allow unsupported.
    let (status, body) = set_mode("allow").await;
    assert_eq!(
        status,
        reqwest::StatusCode::UNPROCESSABLE_ENTITY,
        "an unhonored mode must be refused: {body}"
    );
    let still: serde_json::Value = client
        .get(format!("http://{addr}/code/sessions/{session}/debug"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        still["session"]["permission_mode"], "auto",
        "a refused change must not move the row: {still}"
    );
}

/// opencode takes its agent and permission ruleset on `POST /session`, and
/// resuming is a plain `GET` — nothing captured re-applies either. The runtime
/// used to relaunch anyway, which resumed the old posture while the row, the
/// picker, and the journal all said the new one had taken.
#[tokio::test]
async fn a_mode_change_a_relaunch_cannot_carry_is_refused_rather_than_recorded() {
    let (router, token, _runtime, dir) = code_app_with(
        ScriptedAdapter::new(plain_text_script())
            .with_auto_mode(CapLevel::Supported)
            .with_posture_fixed_at_session_start(),
    )
    .await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = create_sibling_sessions(&client, addr, &token, &workspace, 1)
        .await
        .remove(0);

    // A turn is what gives the engine a session to resume into.
    let accepted = client
        .post(format!("http://{addr}/code/sessions/{session}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "one" }))
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), reqwest::StatusCode::ACCEPTED);

    let response = client
        .post(format!("http://{addr}/code/sessions/{session}/mode"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "permission_mode": "auto" }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::CONFLICT, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("start a new session"),
        "the refusal says what to do instead: {body}"
    );

    let still: serde_json::Value = client
        .get(format!("http://{addr}/code/sessions/{session}/debug"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        still["session"]["permission_mode"], "plan",
        "a refused change must not move the row: {still}"
    );
}

/// opencode advertises no reasoning control, so neither creation nor a turn
/// may store a level that its prompt API drops.
#[tokio::test]
async fn opencode_reasoning_is_never_reported_as_active() {
    let adapter = ScriptedAdapter::new(plain_text_script()).with_kind(HarnessKind::Opencode);
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let create_url = format!(
        "http://{addr}/code/workspaces/{}/sessions",
        json_id(&workspace)
    );

    let refused = client
        .post(&create_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "opencode",
            "permission_mode": "plan",
            "reasoning_effort": "high",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "reasoning_effort_unsupported", "{body}");

    let created = client
        .post(&create_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "opencode",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = created.json().await.unwrap();
    let session_id = json_id(&session);

    for (path, body) in [
        (
            "turns",
            serde_json::json!({ "message": "reason", "reasoning_effort": "high" }),
        ),
        ("effort", serde_json::json!({ "reasoning_effort": "high" })),
    ] {
        let refused = client
            .post(format!("http://{addr}/code/sessions/{session_id}/{path}"))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = refused.json().await.unwrap();
        assert_eq!(body["kind"], "reasoning_effort_unsupported", "{body}");
    }

    let debug: serde_json::Value = client
        .get(format!("http://{addr}/code/sessions/{session_id}/debug"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(debug["session"]["reasoning_effort"].is_null(), "{debug}");
}

/// An explicit model outside the catalog cannot inherit the engine's broad
/// reasoning ladder. The implicit default still uses that fallback when the
/// catalog does not identify a default row.
#[tokio::test]
async fn an_unknown_explicit_model_cannot_inherit_engine_reasoning() {
    let adapter = ScriptedAdapter::new(plain_text_script())
        .with_reasoning_levels(CapLevel::Supported)
        .with_models(vec![listed_model("listed", false, &[], false)]);
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let create_url = format!(
        "http://{addr}/code/workspaces/{}/sessions",
        json_id(&workspace)
    );

    let implicit = client
        .post(&create_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
            "reasoning_effort": "high",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(implicit.status(), reqwest::StatusCode::CREATED);

    let explicit = client
        .post(&create_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
            "model": "unlisted",
            "reasoning_effort": "high",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(explicit.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = explicit.json().await.unwrap();
    assert_eq!(body["kind"], "reasoning_effort_unsupported", "{body}");
}

/// An unlisted explicit model cannot inherit fast mode from the catalog's
/// default row. Omitting the model still uses that advertised default.
#[tokio::test]
async fn an_unknown_explicit_model_cannot_inherit_default_fast_mode() {
    let adapter = ScriptedAdapter::new(plain_text_script()).with_models(vec![listed_model(
        "fast-default",
        true,
        &[],
        true,
    )]);
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let create_url = format!(
        "http://{addr}/code/workspaces/{}/sessions",
        json_id(&workspace)
    );

    let implicit = client
        .post(&create_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
            "fast_mode": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(implicit.status(), reqwest::StatusCode::CREATED);

    let explicit = client
        .post(&create_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
            "model": "unlisted",
            "fast_mode": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(explicit.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = explicit.json().await.unwrap();
    assert_eq!(body["kind"], "fast_mode_unsupported", "{body}");
}

/// A model without a fast service tier cannot make the session or the picker
/// report fast mode as active.
#[tokio::test]
async fn unsupported_fast_mode_is_refused_on_create_and_live_update() {
    let adapter = ScriptedAdapter::new(plain_text_script()).with_models(vec![listed_model(
        "steady",
        true,
        &[],
        false,
    )]);
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let create_url = format!(
        "http://{addr}/code/workspaces/{}/sessions",
        json_id(&workspace)
    );

    let refused = client
        .post(&create_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
            "model": "steady",
            "fast_mode": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "fast_mode_unsupported", "{body}");

    let created = client
        .post(&create_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
            "model": "steady",
        }))
        .send()
        .await
        .unwrap();
    let session: serde_json::Value = created.json().await.unwrap();
    let refused = client
        .post(format!(
            "http://{addr}/code/sessions/{}/fast-mode",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "fast_mode": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "fast_mode_unsupported", "{body}");
}

/// A model switch keeps the model but clears settings the new row cannot
/// honor before the turn or its snapshot sees them.
#[tokio::test]
async fn a_model_switch_deactivates_incompatible_execution_settings() {
    let adapter = ScriptedAdapter::new(plain_text_script())
        .with_reasoning_levels(CapLevel::Supported)
        .with_models(vec![
            listed_model("fast-thinker", true, &[ReasoningEffort::High], true),
            listed_model("steady", false, &[ReasoningEffort::Low], false),
        ]);
    let engine = adapter.clone();
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
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
            "permission_mode": "plan",
            "model": "fast-thinker",
            "reasoning_effort": "high",
            "fast_mode": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = created.json().await.unwrap();
    let session_id = json_id(&session);

    let turn = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "switch", "model": "steady" }))
        .send()
        .await
        .unwrap();
    assert_eq!(turn.status(), reqwest::StatusCode::ACCEPTED);
    let turn: serde_json::Value = turn.json().await.unwrap();
    assert_eq!(turn["model"], "steady", "{turn}");
    assert_eq!(turn["fast_mode"], false, "{turn}");
    assert_eq!(engine.turn_efforts(), vec![None]);
    let inputs = engine.turn_inputs();
    assert_eq!(inputs[0].model.as_deref(), Some("steady"));
    assert!(!inputs[0].fast_mode);

    let debug: serde_json::Value = client
        .get(format!("http://{addr}/code/sessions/{session_id}/debug"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(debug["session"]["model"], "steady", "{debug}");
    assert!(debug["session"]["reasoning_effort"].is_null(), "{debug}");
    assert_eq!(debug["session"]["fast_mode"], false, "{debug}");
}

/// A composer change made during one turn becomes the durable effective
/// settings for the queued turn, after unsupported inherited values clear.
#[tokio::test]
async fn a_queued_turn_receives_the_validated_effective_settings() {
    let adapter = ScriptedAdapter::new(plain_text_script())
        .with_delay(Duration::from_millis(100))
        .with_reasoning_levels(CapLevel::Supported)
        .with_models(vec![
            listed_model("fast-thinker", true, &[ReasoningEffort::High], true),
            listed_model("steady", false, &[ReasoningEffort::Low], false),
        ]);
    let engine = adapter.clone();
    let (router, token, runtime, dir) = code_app_with(adapter).await;
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
            "permission_mode": "plan",
            "model": "fast-thinker",
            "reasoning_effort": "high",
            "fast_mode": true,
        }))
        .send()
        .await
        .unwrap();
    let session: serde_json::Value = created.json().await.unwrap();
    let session_id = json_id(&session).to_owned();
    let parsed: CodeSessionId = session_id.parse().unwrap();

    let first = {
        let client = client.clone();
        let token = token.clone();
        let session_id = session_id.clone();
        tokio::spawn(async move {
            client
                .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "message": "first" }))
                .send()
                .await
                .unwrap()
        })
    };
    let _ = wait_for_open_turn(&runtime, parsed).await;
    let queued = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "second", "model": "steady" }))
        .send()
        .await
        .unwrap();
    assert_eq!(queued.status(), reqwest::StatusCode::ACCEPTED);
    let queued: serde_json::Value = queued.json().await.unwrap();
    assert_eq!(queued["message"], "second", "{queued}");
    assert_eq!(first.await.unwrap().status(), reqwest::StatusCode::ACCEPTED);

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if engine.turn_inputs().len() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the queued turn did not run");
    assert_eq!(
        engine.turn_efforts(),
        vec![Some(ReasoningEffort::High), None]
    );
    let inputs = engine.turn_inputs();
    assert_eq!(inputs[1].model.as_deref(), Some("steady"));
    assert!(!inputs[1].fast_mode);
}

/// A failed targeted write cannot make a live setting appear active or reach
/// the next turn.
#[tokio::test]
async fn a_live_setting_update_becomes_active_only_after_its_exact_write_commits() {
    let adapter = ScriptedAdapter::new(plain_text_script())
        .with_reasoning_levels(CapLevel::Supported)
        .with_models(vec![listed_model(
            "balanced",
            true,
            &[ReasoningEffort::Low],
            false,
        )]);
    let engine = adapter.clone();
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
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
            "permission_mode": "plan",
            "model": "balanced",
        }))
        .send()
        .await
        .unwrap();
    let session: serde_json::Value = created.json().await.unwrap();
    let session_id = json_id(&session);

    execute_code_db_unprepared(
        &dir,
        "CREATE TRIGGER ignore_reasoning_effort_update
         BEFORE UPDATE OF reasoning_effort ON session
         WHEN NEW.reasoning_effort IS NOT OLD.reasoning_effort
         BEGIN
           SELECT RAISE(IGNORE);
         END",
    )
    .await;

    let response = client
        .post(format!("http://{addr}/code/sessions/{session_id}/effort"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "reasoning_effort": "low" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["kind"], "session_settings_changed", "{body}");

    let turn = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "still default" }))
        .send()
        .await
        .unwrap();
    assert_eq!(turn.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(engine.turn_efforts(), vec![None]);
}

/// An engine with no effort control says so, rather than storing a level that
/// would silently do nothing.
#[tokio::test]
async fn an_engine_without_an_effort_ladder_refuses_the_route() {
    let (router, token, _runtime, dir) =
        code_app_with(ScriptedAdapter::new(plain_text_script())).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = create_sibling_sessions(&client, addr, &token, &workspace, 1)
        .await
        .remove(0);

    let response = client
        .post(format!("http://{addr}/code/sessions/{session}/effort"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "reasoning_effort": "high" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["kind"], "reasoning_effort_unsupported", "{body}");
}

/// An engine with its own re-posture channel keeps its child and its context.
/// The relaunch is the fallback, not the mechanism.
#[tokio::test]
async fn a_mode_switch_uses_the_engine_channel_before_it_relaunches() {
    let adapter = ScriptedAdapter::new(plain_text_script())
        .with_auto_mode(CapLevel::Supported)
        .with_live_mode_switch();
    let engine = adapter.clone();
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = create_sibling_sessions(&client, addr, &token, &workspace, 1)
        .await
        .remove(0);

    let response = client
        .post(format!("http://{addr}/code/sessions/{session}/mode"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "permission_mode": "auto" }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["permission_mode"], "auto", "{body}");
    assert_eq!(
        engine.live_modes(),
        vec![PermissionMode::Auto],
        "the engine was told, not replaced",
    );

    // And the turn after it still runs on the same session, without writing
    // the old mode back: the worker holds its own copy of the row, and every
    // turn persists the whole thing.
    let response = client
        .post(format!("http://{addr}/code/sessions/{session}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "after the switch" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);

    let reread: serde_json::Value = client
        .get(format!("http://{addr}/code/sessions/{session}/debug"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reread["session"]["permission_mode"], "auto", "{reread}");
    assert_eq!(
        engine.live_modes(),
        vec![PermissionMode::Auto],
        "one switch, and no relaunch behind it: {reread}",
    );
}

#[tokio::test]
async fn a_permission_mode_intent_persistence_failure_never_reaches_the_engine() {
    let adapter = ScriptedAdapter::new(plain_text_script())
        .with_auto_mode(CapLevel::Supported)
        .with_live_mode_switch();
    let engine = adapter.clone();
    let (router, token, runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = create_sibling_sessions(&client, addr, &token, &workspace, 1)
        .await
        .remove(0);

    execute_code_db_unprepared(
        &dir,
        "CREATE TRIGGER fail_permission_mode_intent
         BEFORE UPDATE OF permission_mode_intent ON session
         WHEN NEW.permission_mode_intent IS NOT NULL
         BEGIN
           SELECT RAISE(FAIL, 'permission-mode intent write failed');
         END",
    )
    .await;

    let response = client
        .post(format!("http://{addr}/code/sessions/{session}/mode"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "permission_mode": "auto" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(
        engine.live_modes().is_empty(),
        "the engine must remain untouched when the intent does not persist"
    );
    let stored = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        session.parse().unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stored.permission_mode, PermissionMode::Plan);
}

#[tokio::test]
async fn a_permission_mode_confirmation_failure_terminates_and_fences_the_engine() {
    let adapter = ScriptedAdapter::new(plain_text_script())
        .with_auto_mode(CapLevel::Supported)
        .with_live_mode_switch();
    let engine = adapter.clone();
    let (router, token, runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = create_sibling_sessions(&client, addr, &token, &workspace, 1)
        .await
        .remove(0);

    execute_code_db_unprepared(
        &dir,
        "CREATE TRIGGER ignore_permission_mode_confirmation
         BEFORE UPDATE OF permission_mode ON session
         WHEN NEW.permission_mode <> OLD.permission_mode
         BEGIN
           SELECT RAISE(IGNORE);
         END",
    )
    .await;

    let response = client
        .post(format!("http://{addr}/code/sessions/{session}/mode"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "permission_mode": "auto" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(engine.live_modes(), vec![PermissionMode::Auto]);
    assert_eq!(
        engine.shutdown_count(),
        1,
        "the acknowledged engine must stop before the failed request returns"
    );
    let stored = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        session.parse().unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stored.permission_mode, PermissionMode::Plan);
    assert_eq!(stored.lifecycle, CodeSessionLifecycle::Fenced);
    assert!(matches!(
        stored.fence_reason,
        Some(FenceReason::ProbeAmbiguous { .. })
    ));

    let blocked = client
        .post(format!("http://{addr}/code/sessions/{session}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "must not run" }))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = blocked.json().await.unwrap();
    assert_eq!(body["kind"], "session_fenced");
}
