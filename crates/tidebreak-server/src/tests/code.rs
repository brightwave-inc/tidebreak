//! End-to-end code-mode routes against the scripted engine.

use super::*;

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;

use axum::Router;
use tokio::net::TcpListener;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{
    Attention, AttentionSource, AttentionState, CapLevel, CodePermissionMode, CodeSessionId,
    CodeSessionLifecycle, CodeTurnStatus, CodeWorkspaceStatus, DbStore, FenceReason, HarnessKind,
    WorkspaceId,
};
use tidebreak_harness::{AdapterRegistry, ApprovalDecision, HarnessApprovalRef, HarnessEvent};

async fn code_app(
    events: Vec<HarnessEvent>,
) -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    code_app_with(ScriptedAdapter::new(events)).await
}

async fn code_app_with(
    adapter: ScriptedAdapter,
) -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    let (dir, store) = temp_db_store("code.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(adapter));
    let runtime = Arc::new(CodeRuntime::with_registry(
        db,
        dir.path().to_path_buf(),
        registry,
    ));
    let mut state = AppState::new(
        Config::desktop(dir.path()),
        store_trait,
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    state.code = Some(runtime.clone());
    let token = state.token.clone();
    (app(state), token, runtime, dir)
}

fn approval_script() -> Vec<HarnessEvent> {
    write_approval_script("toolu_scripted", "hello")
}

fn oversized_write_approval_script() -> Vec<HarnessEvent> {
    write_approval_script("toolu_oversized", &"x".repeat(20 * 1024))
}

fn write_approval_script(call_id: &str, content: &str) -> Vec<HarnessEvent> {
    vec![
        HarnessEvent::SessionStarted {
            harness_kind: HarnessKind::ClaudeCode,
            harness_version: "scripted".into(),
            resume_ref: Some("scripted-session".into()),
        },
        HarnessEvent::TurnStarted,
        HarnessEvent::ApprovalRequested {
            harness_ref: HarnessApprovalRef {
                call_id: call_id.into(),
            },
            raw: serde_json::json!({
                "tool_name": "Write",
                "input": { "file_path": "/workspace/probe.txt", "content": content },
                "tool_use_id": call_id
            }),
        },
        HarnessEvent::AssistantDelta {
            text: "after the decision".into(),
        },
        HarnessEvent::TurnCompleted {
            usage: Default::default(),
        },
    ]
}

fn init_git_repo_named(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let repo = dir.join(name);
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        ["git", "init", "-b", "main"].as_slice(),
        ["git", "config", "user.email", "dev@example.com"].as_slice(),
        ["git", "config", "user.name", "Dev"].as_slice(),
    ] {
        assert!(std::process::Command::new(args[0])
            .args(&args[1..])
            .current_dir(&repo)
            .env("GIT_TERMINAL_PROMPT", "0")
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    assert!(std::process::Command::new("git")
        .args(["add", "README.md"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    assert!(std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    repo
}

fn init_git_repo(dir: &std::path::Path) -> std::path::PathBuf {
    init_git_repo_named(dir, "origin")
}

async fn register_and_workspace(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    repo: &std::path::Path,
) -> (serde_json::Value, serde_json::Value) {
    let registered = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "path": repo }))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), reqwest::StatusCode::CREATED);
    let repo_body: serde_json::Value = registered.json().await.unwrap();
    let created = client
        .post(format!("http://{addr}/code/workspaces"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "repo_id": json_id(&repo_body),
            "title": "first change",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let workspace: serde_json::Value = created.json().await.unwrap();
    let _ = json_id(&workspace);
    (repo_body, workspace)
}

fn json_id(value: &serde_json::Value) -> &str {
    value["id"].as_str().expect("id is a string")
}

async fn serve(router: Router) -> std::net::SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}

/// Auto is gated by its own capability flag (decision 0038): an engine with
/// a live approval channel but no auto posture refuses Auto, and an engine
/// whose only honest posture is unsupervised Auto creates an Auto session.
/// A wrong implementation deriving Auto from the approval flag passes
/// neither arm.
#[tokio::test]
async fn auto_stands_on_its_own_capability_flag() {
    let adapter = ScriptedAdapter::new(plain_text_script())
        .with_approvals(CapLevel::Supported)
        .with_auto_mode(CapLevel::Unsupported);
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
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
            "permission_mode": "auto",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "permission_mode_unavailable");
    assert!(
        body["message"]
            .as_str()
            .unwrap_or("")
            .contains("auto posture"),
        "{}",
        body["message"]
    );

    let adapter = ScriptedAdapter::new(plain_text_script()).with_auto_mode(CapLevel::Supported);
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
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
            "permission_mode": "auto",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = created.json().await.unwrap();
    assert_eq!(session["permission_mode"], "auto");
}

/// Allow is gated by its own capability flag (decision 0039): an engine
/// with Auto but no allow-all posture refuses Allow.
#[tokio::test]
async fn allow_stands_on_its_own_capability_flag() {
    let adapter = ScriptedAdapter::new(plain_text_script())
        .with_auto_mode(CapLevel::Supported)
        .with_allow_mode(CapLevel::Unsupported);
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
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
            "permission_mode": "allow",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "permission_mode_unavailable");
    assert!(
        body["message"]
            .as_str()
            .unwrap_or("")
            .contains("allow-all posture"),
        "{}",
        body["message"]
    );

    let adapter = ScriptedAdapter::new(plain_text_script()).with_allow_mode(CapLevel::Supported);
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
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
            "permission_mode": "allow",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = created.json().await.unwrap();
    assert_eq!(session["permission_mode"], "allow");
}

#[tokio::test]
async fn plan_is_the_only_session_mode_and_a_turn_journals_end_to_end() {
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
    assert!(
        body["message"]
            .as_str()
            .unwrap_or("")
            .contains("structured approvals"),
        "{}",
        body["message"]
    );

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
        .unwrap();
    assert_eq!(session.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = session.json().await.unwrap();
    assert_eq!(session["permission_mode"], "plan");

    let turn = client
        .post(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(turn.status(), reqwest::StatusCode::ACCEPTED);
    let turn: serde_json::Value = turn.json().await.unwrap();
    assert_eq!(turn["status"], "completed");

    let busy = client
        .post(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "again" }))
        .send()
        .await
        .unwrap();
    // Second turn is accepted because the first already finished.
    assert_eq!(busy.status(), reqwest::StatusCode::ACCEPTED);
}

#[tokio::test]
async fn listing_workspace_sessions_returns_create_shaped_snapshots() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let missing = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            uuid::Uuid::new_v4()
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    let empty = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(empty.status(), reqwest::StatusCode::OK);
    let empty_body: Vec<serde_json::Value> = empty.json().await.unwrap();
    assert!(empty_body.is_empty());

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
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let listed = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    let listed: Vec<serde_json::Value> = listed.json().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], created["id"]);
    assert_eq!(listed[0]["workspace_id"], created["workspace_id"]);
    assert_eq!(listed[0]["harness_kind"], "claude_code");
    assert_eq!(listed[0]["permission_mode"], "plan");
    assert_eq!(listed[0]["lifecycle"], created["lifecycle"]);
}

#[tokio::test]
async fn listing_session_turns_returns_user_input_and_usage() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let missing = client
        .get(format!(
            "http://{addr}/code/sessions/{}/turns",
            uuid::Uuid::new_v4()
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

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
    let empty = client
        .get(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(empty.status(), reqwest::StatusCode::OK);
    let empty_body: Vec<serde_json::Value> = empty.json().await.unwrap();
    assert!(empty_body.is_empty());

    let first = client
        .post(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "hello" }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let second = client
        .post(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "again" }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let listed = client
        .get(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    let listed: Vec<serde_json::Value> = listed.json().await.unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0]["id"], first["id"]);
    assert_eq!(listed[0]["ordinal"], 1);
    assert_eq!(listed[0]["status"], "completed");
    assert_eq!(listed[0]["user_input"], "hello");
    assert_eq!(listed[0]["usage"]["output_tokens"], 6);
    assert!(listed[0]["started_at"].is_string());
    assert!(listed[0]["ended_at"].is_string());
    assert_eq!(listed[1]["id"], second["id"]);
    assert_eq!(listed[1]["ordinal"], 2);
    assert_eq!(listed[1]["user_input"], "again");
}

#[tokio::test]
async fn workspace_setup_failure_preserves_the_checkout() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let registered = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "path": repo,
            "setup_script": "exit 3",
        }))
        .send()
        .await
        .unwrap();
    let repo_body: serde_json::Value = registered.json().await.unwrap();
    let created = client
        .post(format!("http://{addr}/code/workspaces"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repo_id": json_id(&repo_body),
            "title": "broken setup",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let listed = client
        .get(format!(
            "http://{addr}/code/workspaces?repo_id={}",
            json_id(&repo_body)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<Vec<serde_json::Value>>()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["status"], "setup_failed");
    let path = listed[0]["worktree_path"].as_str().unwrap();
    assert!(std::path::Path::new(path).join("README.md").is_file());
}

#[tokio::test]
async fn archive_requires_force_when_the_tree_is_dirty() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let path = workspace["worktree_path"].as_str().unwrap();
    std::fs::write(std::path::Path::new(path).join("dirty.txt"), "nope\n").unwrap();

    let refused = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);

    let forced = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "force": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(forced.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = forced.json().await.unwrap();
    assert_eq!(body["status"], "archived");
    assert!(!std::path::Path::new(path).exists());
}

#[tokio::test]
async fn no_force_archive_of_a_dirty_workspace_leaves_an_idle_session() {
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
    let path = workspace["worktree_path"].as_str().unwrap();
    std::fs::write(std::path::Path::new(path).join("dirty.txt"), "nope\n").unwrap();

    let refused = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "uncommitted");

    let parsed: CodeSessionId = json_id(&session).parse().unwrap();
    let row = tidebreak_core::db::code::get_session(&runtime.db, parsed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.lifecycle, CodeSessionLifecycle::Idle);
    assert!(std::path::Path::new(path).exists());

    let turn = client
        .post(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "still here" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        turn.status(),
        reqwest::StatusCode::ACCEPTED,
        "session must stay usable after a refused dirty archive: {}",
        turn.text().await.unwrap()
    );
}

#[tokio::test]
async fn interrupt_stops_a_running_scripted_turn() {
    let (router, token, _runtime, dir) = code_app_with(
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

    let turn_req = client
        .post(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "slow" }));
    let interrupt = async {
        tokio::time::sleep(Duration::from_millis(30)).await;
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
}

/// A killed engine reaches EOF exactly like a finished one. Reading that as
/// success journaled a Stop — and an OOM kill, or a signed-out engine — as a
/// completed turn with zero tokens.
#[tokio::test]
async fn an_engine_that_dies_without_saying_so_journals_an_interrupted_turn() {
    let (router, token, _runtime, dir) = code_app_with(
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

    let turn_req = client
        .post(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "slow" }));
    let interrupt = async {
        tokio::time::sleep(Duration::from_millis(30)).await;
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
        .with_delay(Duration::from_millis(40))
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
            let row = tidebreak_core::db::code::get_session(&runtime.db, parsed)
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
    assert_eq!(follow_body["position"], 1);
    assert!(
        follow_body.get("id").is_none() && follow_body.get("status").is_none(),
        "queued follow-up must not mint a fake turn id: {follow_body}"
    );

    let overflow = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "too many" }))
        .send()
        .await
        .unwrap();
    assert_eq!(overflow.status(), reqwest::StatusCode::CONFLICT);
    let overflow_body: serde_json::Value = overflow.json().await.unwrap();
    assert_eq!(overflow_body["kind"], "queue_full");

    assert_eq!(first.await.unwrap().status(), reqwest::StatusCode::ACCEPTED);

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let turns = tidebreak_core::db::code::list_turns(&runtime.db, parsed)
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
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("queued follow-up did not run after the current turn completed");
}

#[tokio::test]
async fn explicit_steer_is_not_yet_available() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
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
        .json(&serde_json::json!({ "message": "redirect" }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "steering_unavailable");
}

fn scripted_registry() -> AdapterRegistry {
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(ScriptedAdapter::new(plain_text_script())));
    registry
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
    let mut row = tidebreak_core::db::code::get_session(&runtime.db, parsed)
        .await
        .unwrap()
        .unwrap();
    row.lifecycle = CodeSessionLifecycle::Fenced;
    row.fence_reason = Some(FenceReason::OrphanAlive);
    row.attention = Attention::new(
        AttentionState::Fenced {
            reason: FenceReason::OrphanAlive,
        },
        AttentionSource::Lifecycle,
    );
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
    assert_eq!(reaped.status(), reqwest::StatusCode::OK);
    let after_reap: serde_json::Value = reaped.json().await.unwrap();
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

/// Decision 0032: the archive script obeys the same failure-preserves rule as
/// setup. A script whose job is to back the workspace up must be able to stop
/// the archive by failing, and a refused archive must not have run it at all.
#[tokio::test]
async fn a_failing_archive_script_preserves_the_worktree() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let registered = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "path": repo,
            "archive_script": "echo ran >> .archive-ran; exit 4",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), reqwest::StatusCode::CREATED);
    let repo_body: serde_json::Value = registered.json().await.unwrap();
    let created = client
        .post(format!("http://{addr}/code/workspaces"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repo_id": json_id(&repo_body),
            "title": "backed up on archive",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let workspace: serde_json::Value = created.json().await.unwrap();
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    std::fs::write(path.join("dirty.txt"), "nope\n").unwrap();

    let refused = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    assert!(
        !path.join(".archive-ran").exists(),
        "a refused archive must not run the archive script"
    );

    let failed = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "force": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(failed.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = failed.json().await.unwrap();
    assert_eq!(body["kind"], "archive_script_failed");
    assert!(path.join(".archive-ran").is_file());
    assert!(
        path.join("dirty.txt").is_file(),
        "a failed archive script must leave the worktree on disk"
    );
    let listed = client
        .get(format!(
            "http://{addr}/code/workspaces?repo_id={}",
            json_id(&repo_body)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<Vec<serde_json::Value>>()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["status"], "active");
}

#[tokio::test]
async fn archive_ends_the_session_before_removing_the_worktree() {
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
    let path = workspace["worktree_path"].as_str().unwrap().to_owned();
    let archived = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(archived.status(), reqwest::StatusCode::OK);
    assert!(!std::path::Path::new(&path).exists());
    let parsed: CodeSessionId = json_id(&session).parse().unwrap();
    let row = tidebreak_core::db::code::get_session(&runtime.db, parsed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.lifecycle, CodeSessionLifecycle::Ended);

    let again = client
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
        again.status(),
        reqwest::StatusCode::CONFLICT,
        "archived workspace is not ready for a new session"
    );
}

#[tokio::test]
async fn archive_refuses_a_running_session_without_force() {
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
        .with_delay(Duration::from_millis(50)),
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
        async move {
            client
                .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "message": "busy" }))
                .send()
                .await
                .unwrap()
        }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let row = tidebreak_core::db::code::get_session(&runtime.db, parsed)
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
    .expect("turn never reached Running");

    let refused = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "session_running");

    let forced = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/archive",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "force": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(forced.status(), reqwest::StatusCode::OK);
    let _ = turn.await;
    let row = tidebreak_core::db::code::get_session(&runtime.db, parsed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.lifecycle, CodeSessionLifecycle::Ended);
}

#[tokio::test]
async fn two_repos_with_the_same_name_get_distinct_worktrees() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let left = init_git_repo_named(&dir.path().join("left"), "origin");
    let right = init_git_repo_named(&dir.path().join("right"), "origin");
    let (repo_a, ws_a) = register_and_workspace(&client, addr, &token, &left).await;
    let (repo_b, ws_b) = register_and_workspace(&client, addr, &token, &right).await;
    assert_eq!(repo_a["display_name"], "origin");
    assert_eq!(repo_b["display_name"], "origin");
    let path_a = ws_a["worktree_path"].as_str().unwrap();
    let path_b = ws_b["worktree_path"].as_str().unwrap();
    assert_ne!(path_a, path_b);
    assert!(path_a.contains(json_id(&repo_a)));
    assert!(path_b.contains(json_id(&repo_b)));
    assert!(std::path::Path::new(path_a).join("README.md").is_file());
    assert!(std::path::Path::new(path_b).join("README.md").is_file());
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_replays_then_lives_without_gaps_or_duplicates() {
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(vec![
            HarnessEvent::TurnStarted,
            HarnessEvent::AssistantDelta { text: "one".into() },
            HarnessEvent::AssistantDelta { text: "two".into() },
            HarnessEvent::TurnCompleted {
                usage: Default::default(),
            },
        ])
        .with_delay(Duration::from_millis(20)),
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

    let mut request = format!("ws://{addr}/code/sessions/{session_id}/events?after=0")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();

    let _ = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "hi" }))
        .send()
        .await
        .unwrap();

    let mut seqs = Vec::new();
    let read = async {
        while let Some(frame) = socket.next().await {
            let WsMessage::Text(text) = frame.unwrap() else {
                continue;
            };
            let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
            let seq = value["seq"].as_i64().unwrap();
            seqs.push(seq);
            if value["event"]["type"] == "turn_completed" {
                break;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .expect("turn did not complete over the socket");
    assert!(seqs.windows(2).all(|pair| pair[0] < pair[1]), "{seqs:?}");
    assert_eq!(
        seqs.iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        seqs.len(),
        "duplicate seq on the live socket: {seqs:?}"
    );

    // Concurrent write after connect: journal a notice and publish a later seq
    // first so the socket must fill the gap from the journal.
    let parsed: CodeSessionId = session_id.parse().unwrap();
    let current = *seqs.last().unwrap();
    let _ = tidebreak_core::db::code::append_event(
        &runtime.db,
        parsed,
        1,
        &tidebreak_core::CodeEvent::HarnessNotice {
            level: tidebreak_core::HarnessNoticeLevel::Info,
            message: "gap-a".into(),
        },
    )
    .await
    .unwrap();
    let seq_b = tidebreak_core::db::code::append_event(
        &runtime.db,
        parsed,
        1,
        &tidebreak_core::CodeEvent::HarnessNotice {
            level: tidebreak_core::HarnessNoticeLevel::Info,
            message: "gap-b".into(),
        },
    )
    .await
    .unwrap();
    runtime.bus.publish(
        parsed,
        tidebreak_core::SequencedCodeEvent {
            seq: seq_b,
            event: tidebreak_core::CodeEvent::HarnessNotice {
                level: tidebreak_core::HarnessNoticeLevel::Info,
                message: "gap-b".into(),
            },
        },
    );
    let mut recovered = Vec::new();
    for _ in 0..2 {
        let frame = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("gap recovery timed out")
            .expect("socket closed")
            .unwrap();
        let WsMessage::Text(text) = frame else {
            continue;
        };
        let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
        recovered.push(value["seq"].as_i64().unwrap());
    }
    assert_eq!(recovered, vec![current + 1, current + 2]);
}

#[tokio::test]
async fn superseded_worker_cannot_append_to_the_journal() {
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
    let session_id: CodeSessionId = json_id(&session).parse().unwrap();
    let current = session["lifecycle"].as_str().unwrap().to_owned();
    assert_eq!(current, "idle");
    let bumped = tidebreak_core::db::code::bump_spawn_epoch(&runtime.db, session_id, None)
        .await
        .unwrap();
    let err = tidebreak_core::db::code::append_event(
        &runtime.db,
        session_id,
        bumped - 1,
        &tidebreak_core::CodeEvent::TurnInterrupted,
    )
    .await
    .unwrap_err();
    match err {
        tidebreak_core::db::code::CodeJournalError::StaleSpawnEpoch {
            attempted, current, ..
        } => {
            assert_eq!(attempted, bumped - 1);
            assert_eq!(current, bumped);
        }
        other => panic!("expected stale epoch, got {other:?}"),
    }
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

    let events =
        tidebreak_core::db::code::list_events(&_runtime.db, json_id(&session).parse().unwrap(), 0)
            .await
            .unwrap();
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

    let events =
        tidebreak_core::db::code::list_events(&runtime.db, json_id(&session).parse().unwrap(), 0)
            .await
            .unwrap();
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

    let parsed: CodeSessionId = session_id.parse().unwrap();
    let row = tidebreak_core::db::code::get_session(&runtime.db, parsed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.lifecycle, CodeSessionLifecycle::Running);
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
    let events = tidebreak_core::db::code::list_events(&runtime.db, parsed, 0)
        .await
        .unwrap();
    let kinds: Vec<&str> = events
        .iter()
        .map(|framed| match &framed.event {
            tidebreak_core::CodeEvent::ApprovalRequested { .. } => "requested",
            tidebreak_core::CodeEvent::ApprovalResolved { .. } => "resolved",
            tidebreak_core::CodeEvent::AssistantDelta { .. } => "delta",
            tidebreak_core::CodeEvent::TurnCompleted { .. } => "completed",
            _ => "other",
        })
        .collect();
    assert!(kinds.contains(&"requested"));
    assert!(kinds.contains(&"resolved"));
    assert!(kinds.contains(&"delta"));
    assert!(kinds.contains(&"completed"));
    let requested = kinds.iter().position(|k| *k == "requested").unwrap();
    let delta = kinds.iter().position(|k| *k == "delta").unwrap();
    let completed = kinds.iter().position(|k| *k == "completed").unwrap();
    assert!(requested < delta);
    assert!(delta < completed);
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
                return row;
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

    let parsed: CodeSessionId = json_id(&session).parse().unwrap();
    let row = tidebreak_core::db::code::get_session(&runtime.db, parsed)
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
async fn pending_approval_survives_restart_and_is_decidable() {
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
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["id"], approval["id"]);

    let parsed: CodeSessionId = session_id.parse().unwrap();
    let row = tidebreak_core::db::code::get_session(&runtime.db, parsed)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        row.attention.state,
        AttentionState::NeedsYou { .. }
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
    assert_eq!(decided.status(), reqwest::StatusCode::OK);
    let seen = observed.observed_decisions();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].1, ApprovalDecision::Approve);
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
async fn updates_channel_restates_the_full_digest_on_reconnect() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
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

    let mut request = format!("ws://{addr}/code/updates")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    let first = next_json(&mut socket).await;
    assert_eq!(first["type"], "snapshot");
    let sessions = first["sessions"].as_array().expect("snapshot sessions");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["session"], session_id);
    assert_eq!(sessions[0]["workspace"], json_id(&workspace));
    assert_eq!(sessions[0]["title"], "first change");
    assert_eq!(sessions[0]["turn_count"], 0);

    let _ = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "hi" }))
        .send()
        .await
        .unwrap();

    let mut saw_turn = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        let notice = tokio::time::timeout(Duration::from_millis(500), next_json(&mut socket))
            .await
            .ok();
        let Some(notice) = notice else {
            continue;
        };
        if notice["type"] == "digest" && notice["turn_count"] == 1 {
            saw_turn = true;
            break;
        }
    }
    assert!(saw_turn, "live digest must carry the new turn count");
    drop(socket);

    let mut request = format!("ws://{addr}/code/updates")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    let restated = next_json(&mut socket).await;
    assert_eq!(restated["type"], "snapshot");
    let sessions = restated["sessions"].as_array().expect("restated sessions");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["session"], session_id);
    assert_eq!(sessions[0]["turn_count"], 1);
    assert_eq!(sessions[0]["attention"]["state"]["type"], "done_unreviewed");
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

    let parsed: CodeSessionId = session_id.parse().unwrap();
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
                let session = tidebreak_core::db::code::get_session(&runtime.db, parsed)
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

    let after_decision = tidebreak_core::db::code::get_session(&runtime.db, parsed)
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
    let row = tidebreak_core::db::code::get_session(&runtime.db, parsed)
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
            let row = tidebreak_core::db::code::get_session(&runtime.db, parsed)
                .await
                .unwrap()
                .unwrap();
            if row.attention.state == AttentionState::Working {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("viewing the session must clear DoneUnreviewed");
}

#[tokio::test]
async fn stall_sweep_marks_a_silent_running_session() {
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
    let parsed: CodeSessionId = json_id(&session).parse().unwrap();
    let mut row = tidebreak_core::db::code::get_session(&runtime.db, parsed)
        .await
        .unwrap()
        .unwrap();
    row.lifecycle = CodeSessionLifecycle::Running;
    tidebreak_core::db::code::save_session(&runtime.db, &row)
        .await
        .unwrap();

    crate::code::attention::sweep_stalled(&runtime.db, &runtime.bus, 0)
        .await
        .unwrap();
    let row = tidebreak_core::db::code::get_session(&runtime.db, parsed)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(row.attention.state, AttentionState::Stalled { .. }),
        "{:?}",
        row.attention
    );
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

    let parsed: CodeSessionId = session_id.parse().unwrap();
    let mut row = tidebreak_core::db::code::get_session(&runtime.db, parsed)
        .await
        .unwrap()
        .unwrap();
    row.lifecycle = CodeSessionLifecycle::Running;
    tidebreak_core::db::code::save_session(&runtime.db, &row)
        .await
        .unwrap();
    crate::code::attention::sweep_stalled(&runtime.db, &runtime.bus, 0)
        .await
        .unwrap();
    let row = tidebreak_core::db::code::get_session(&runtime.db, parsed)
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

async fn next_json(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> serde_json::Value {
    loop {
        let Some(frame) = socket.next().await else {
            panic!("updates socket closed");
        };
        let WsMessage::Text(text) = frame.expect("ws frame") else {
            continue;
        };
        return serde_json::from_str(text.as_str()).unwrap();
    }
}

/// The doctor serves memoized probes, and refresh is the on-demand re-probe
/// (decision 0034). A cold probe spends an interactive login shell plus a
/// version and an authentication subprocess per harness, and the code-mode
/// surface reads this route on every navigation.
#[tokio::test]
async fn the_doctor_caches_probes_and_refresh_re_probes() {
    let adapter = ScriptedAdapter::new(plain_text_script());
    let (router, token, _runtime, _dir) = code_app_with(adapter.clone()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let report = client
        .get(format!("http://{addr}/code/harnesses"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(report["harnesses"][0]["kind"], "claude_code");
    assert_eq!(report["harnesses"][0]["found"], true);

    let again = client
        .get(format!("http://{addr}/code/harnesses"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(again["harnesses"][0]["found"], true);
    assert_eq!(
        adapter.probe_count(),
        1,
        "a second doctor read must be served from the cache"
    );

    let refreshed = client
        .post(format!("http://{addr}/code/harnesses/refresh"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(refreshed["harnesses"][0]["found"], true);
    assert_eq!(
        adapter.probe_count(),
        2,
        "refresh must re-probe rather than repeat the cached answer"
    );
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

/// Decision 0031's honesty mechanism, end to end: a parser that could not read
/// part of a stream must leave a durable, readable count behind. A build that
/// counts drops but never persists them is indistinguishable from one that
/// drops silently, which is the failure the record exists to prevent.
#[tokio::test]
async fn unread_engine_events_accumulate_on_the_session_row_and_reach_the_doctor() {
    let (router, token, _runtime, dir) =
        code_app_with(ScriptedAdapter::new(plain_text_script()).with_unrecognized_per_turn(2))
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
    assert_eq!(session["unrecognized_event_count"], 0);

    for message in ["hello", "again"] {
        let turn = client
            .post(format!(
                "http://{addr}/code/sessions/{}/turns",
                json_id(&session)
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "message": message }))
            .send()
            .await
            .unwrap();
        assert_eq!(turn.status(), reqwest::StatusCode::ACCEPTED);
    }

    // Both turns, not just the last: the row accumulates rather than being
    // overwritten with whatever the newest turn happened to see.
    let listed = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<Vec<serde_json::Value>>()
        .await
        .unwrap();
    assert_eq!(listed[0]["unrecognized_event_count"], 4);

    let report = client
        .get(format!("http://{addr}/code/harnesses"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(report["harnesses"][0]["unrecognized_event_count"], 4);
}

/// A resume ref the engine has lost wedges the session otherwise: every turn
/// fails identically, the session stays idle, and nothing offers a reap.
#[tokio::test]
async fn a_lost_resume_fences_the_session_instead_of_failing_every_turn() {
    let adapter =
        ScriptedAdapter::new(plain_text_script()).with_lost_resume("thread not found: dead-thread");
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

    // The session carries a ref from an earlier engine process.
    let mut row = tidebreak_core::db::code::get_session(&runtime.db, parsed)
        .await
        .unwrap()
        .unwrap();
    row.harness_resume_ref = Some("dead-thread".into());
    assert!(tidebreak_core::db::code::save_session(&runtime.db, &row)
        .await
        .unwrap());

    let failed = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "carry on" }))
        .send()
        .await
        .unwrap();
    assert_eq!(failed.status(), reqwest::StatusCode::INTERNAL_SERVER_ERROR);

    let listed = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let after = listed[0].clone();
    assert_eq!(after["lifecycle"], "fenced");
    assert_eq!(after["fence_reason"]["type"], "resume_lost");
    assert_eq!(
        after["fence_reason"]["detail"],
        "thread not found: dead-thread"
    );
    assert_eq!(after["attention"]["state"]["type"], "fenced");
    assert!(
        after["harness_resume_ref"].is_null(),
        "the fence must drop the dead ref so a reap starts a fresh session: {after}"
    );

    // Fenced, so the next turn is refused with the reap the UI offers rather
    // than another identical failure.
    let refused = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "again" }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let refused_body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(refused_body["kind"], "session_fenced");
}

#[allow(dead_code)]
fn _types(
    _id: WorkspaceId,
    _status: CodeWorkspaceStatus,
    _turn: CodeTurnStatus,
    _db: Option<DbStore>,
    _mode: CodePermissionMode,
    _kind: HarnessKind,
) {
}
