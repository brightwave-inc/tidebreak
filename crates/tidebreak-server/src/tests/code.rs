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
    CapLevel, CodePermissionMode, CodeSessionId, CodeSessionLifecycle, CodeTurnStatus,
    CodeWorkspaceStatus, DbStore, HarnessKind, WorkspaceId,
};
use tidebreak_harness::{AdapterRegistry, HarnessEvent};

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

fn init_git_repo(dir: &std::path::Path) -> std::path::PathBuf {
    let repo = dir.join("origin");
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
    assert!(
        follow_body.get("status").is_none(),
        "queued follow-up must not mint a fake turn row: {follow_body}"
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
async fn doctor_lists_the_scripted_adapter() {
    let (router, token, _runtime, _dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let report = reqwest::Client::new()
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
