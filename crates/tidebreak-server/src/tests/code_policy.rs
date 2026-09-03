//! Managed ceilings and capability flags over code-session modes.

use super::code::*;

use std::sync::Arc;

use axum::Router;

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{CapLevel, HarnessKind, PermissionMode};

/// A code app whose OS policy asserts a permission-mode ceiling.
async fn code_app_with_ceiling(
    adapter: ScriptedAdapter,
    ceiling: PermissionMode,
) -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    code_app_with_options(adapter, None, None, Some(ceiling), false).await
}

/// A managed profile's `permission_mode_ceiling` binds code sessions the way
/// it binds chats: an over-ceiling posture is refused where it is chosen —
/// session create and the mode route — while a stricter one stays the
/// reader's choice. The UI lock is legibility; this is the enforcement.
#[tokio::test]
async fn a_managed_ceiling_refuses_over_ceiling_code_session_modes() {
    let adapter = ScriptedAdapter::new(plain_text_script())
        .with_approvals(CapLevel::Supported)
        .with_auto_mode(CapLevel::Supported)
        .with_allow_mode(CapLevel::Supported);
    let (router, token, _runtime, dir) = code_app_with_ceiling(adapter, PermissionMode::Ask).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let sessions_url = format!(
        "http://{addr}/code/workspaces/{}/sessions",
        json_id(&workspace)
    );

    // Create: the engine honors Allow, the policy does not.
    let refused = client
        .post(&sessions_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "allow",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "permission_mode_locked", "{body}");

    let created = client
        .post(&sessions_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "ask",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = created.json().await.unwrap();
    assert_eq!(session["permission_mode"], "ask", "{session}");
    let session = json_id(&session).to_owned();

    // The mode route carries the same ceiling.
    let refused = client
        .post(format!("http://{addr}/code/sessions/{session}/mode"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "permission_mode": "auto" }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "permission_mode_locked", "{body}");

    // A ceiling names a maximum, so dialing down stays open.
    let allowed = client
        .post(format!("http://{addr}/code/sessions/{session}/mode"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "permission_mode": "plan" }))
        .send()
        .await
        .unwrap();
    let status = allowed.status();
    let body: serde_json::Value = allowed.json().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["permission_mode"], "plan", "{body}");
}

/// A Plan-only ceiling against an engine that only offers Auto/Allow is not
/// a missing picker: create must name the ceiling and the engine.
#[tokio::test]
async fn a_managed_ceiling_that_permits_no_engine_mode_is_named() {
    let adapter = ScriptedAdapter::new(plain_text_script())
        .with_kind(HarnessKind::Grok)
        .with_plan_mode(CapLevel::Unsupported)
        .with_auto_mode(CapLevel::Supported)
        .with_allow_mode(CapLevel::Supported);
    let (router, token, _runtime, dir) = code_app_with_ceiling(adapter, PermissionMode::Plan).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;

    let sessions_url = format!(
        "http://{addr}/code/workspaces/{}/sessions",
        json_id(&workspace)
    );
    // Posting the engine's default (Auto) or a below-ceiling Plan must both
    // name the empty intersection — not look like a lone over-ceiling pick
    // or an engine that simply cannot honor Plan.
    for mode in ["auto", "plan"] {
        let refused = client
            .post(&sessions_url)
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "harness": "grok",
                "permission_mode": mode,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
        let body: serde_json::Value = refused.json().await.unwrap();
        assert_eq!(body["kind"], "permission_mode_locked", "{body}");
        let message = body["message"].as_str().unwrap_or("");
        assert!(
            message.contains("grok") && message.contains("plan") && message.contains("ceiling"),
            "{mode}: {message}"
        );
        assert!(
            !message.contains("exceeds the maximum"),
            "{mode}: {message}"
        );
    }
}

#[tokio::test]
async fn a_managed_ceiling_can_refuse_remote_allow_sessions() {
    let (router, token, runtime, dir) = code_app_with_remote(
        ScriptedAdapter::new(plain_text_script()),
        Some(PermissionMode::Ask),
    )
    .await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo_named(dir.path(), "remote-ceiling");
    add_github_remote(&repo, "remote-ceiling");
    let registered = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "path": repo }))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), reqwest::StatusCode::CREATED);
    let registered = registered.json::<serde_json::Value>().await.unwrap();
    record_github_origin(&runtime, &registered, "remote-ceiling").await;
    let workspace = client
        .post(format!("http://{addr}/code/remote/workspaces"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "repo_id": json_id(&registered) }))
        .send()
        .await
        .unwrap();
    assert_eq!(workspace.status(), reqwest::StatusCode::CREATED);
    let workspace = workspace.json::<serde_json::Value>().await.unwrap();
    let refused = client
        .post(format!(
            "http://{addr}/code/remote/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "harness": "claude_code" }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "permission_mode_locked", "{body}");
}

/// Shipped in #2133 without its test attribute, so it never ran. It passes.
#[tokio::test]
async fn ask_is_refused_when_structured_approvals_are_unsupported() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let refused = client
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
    assert_eq!(refused.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "permission_mode_unavailable");
}
