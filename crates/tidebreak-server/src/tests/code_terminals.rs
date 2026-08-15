//! Auxiliary terminal routes: cursor-pull, restart reap, no durable bytes.

use super::*;

use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use tokio::net::TcpListener;

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{DbStore, Store};
use tidebreak_harness::AdapterRegistry;

async fn terminal_app() -> (
    Router,
    Arc<str>,
    Arc<CodeRuntime>,
    tempfile::TempDir,
    AppState,
) {
    let (dir, store) = temp_db_store("code-terminals.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(ScriptedAdapter::new(plain_text_script())));
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
    (app(state.clone()), token, runtime, dir, state)
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

async fn register_workspace(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    repo: &std::path::Path,
) -> String {
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
            "repo_id": repo_body["id"].as_str().unwrap(),
            "title": "shell",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let workspace: serde_json::Value = created.json().await.unwrap();
    workspace["id"].as_str().unwrap().to_owned()
}

async fn serve(router: Router) -> std::net::SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn decode_b64(value: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .unwrap()
}

#[tokio::test]
async fn create_write_read_and_two_cursors() {
    let (router, token, _runtime, dir, _state) = terminal_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let workspace_id = register_workspace(&client, addr, &token, &repo).await;

    let created = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/terminals"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "cols": 80, "rows": 24 }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let terminal: serde_json::Value = created.json().await.unwrap();
    let tid = terminal["id"].as_str().unwrap();
    assert_eq!(terminal["ended"], false);

    let listed = client
        .get(format!(
            "http://{addr}/code/workspaces/{workspace_id}/terminals"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    let list: Vec<serde_json::Value> = listed.json().await.unwrap();
    assert_eq!(list.len(), 1);

    let payload = b"echo TERM_TWO_READERS_ok\r";
    let written = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/terminals/{tid}/write"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "bytes": b64(payload) }))
        .send()
        .await
        .unwrap();
    assert_eq!(written.status(), reqwest::StatusCode::NO_CONTENT);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut body = Vec::new();
    let mut cursor = 0u64;
    while tokio::time::Instant::now() < deadline {
        let read = client
            .get(format!(
                "http://{addr}/code/workspaces/{workspace_id}/terminals/{tid}/read?cursor={cursor}"
            ))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(read.status(), reqwest::StatusCode::OK);
        let page: serde_json::Value = read.json().await.unwrap();
        body.extend(decode_b64(page["bytes"].as_str().unwrap()));
        cursor = page["cursor"].as_u64().unwrap();
        if body
            .windows(b"TERM_TWO_READERS_ok".len())
            .any(|w| w == b"TERM_TWO_READERS_ok")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(
        body.windows(b"TERM_TWO_READERS_ok".len())
            .any(|w| w == b"TERM_TWO_READERS_ok"),
        "shell did not echo the written command: {:?}",
        String::from_utf8_lossy(&body)
    );

    let late = client
        .get(format!(
            "http://{addr}/code/workspaces/{workspace_id}/terminals/{tid}/read?cursor={cursor}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let late_page: serde_json::Value = late.json().await.unwrap();
    assert_eq!(late_page["cursor"].as_u64().unwrap(), cursor);

    let from_zero = client
        .get(format!(
            "http://{addr}/code/workspaces/{workspace_id}/terminals/{tid}/read?cursor=0"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let zero: serde_json::Value = from_zero.json().await.unwrap();
    let zero_bytes = decode_b64(zero["bytes"].as_str().unwrap());
    assert!(zero_bytes
        .windows(b"TERM_TWO_READERS_ok".len())
        .any(|w| w == b"TERM_TWO_READERS_ok"));
}

#[tokio::test]
async fn restart_reports_ended_and_store_holds_no_terminal_bytes() {
    let marker = b"TERM_MARKER_no_durable_9f3a7c1e";
    let (router, token, _runtime, dir, state) = terminal_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let workspace_id = register_workspace(&client, addr, &token, &repo).await;

    let created = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/terminals"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let terminal: serde_json::Value = created.json().await.unwrap();
    let tid = terminal["id"].as_str().unwrap().to_owned();
    let parsed: tidebreak_core::CodeTerminalId = tid.parse().unwrap();
    state.terminals.push_output(parsed, marker);
    assert!(state
        .terminals
        .read(workspace_id.parse().unwrap(), parsed, 0)
        .data
        .windows(marker.len())
        .any(|window| window == marker));

    let (router2, token2, _runtime2, _dir2, state2) = {
        let db = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rw",
                dir.path().join("code-terminals.db").display()
            ))
            .await
            .unwrap(),
        );
        let store_trait: Arc<dyn Store> = db.clone();
        let mut registry = AdapterRegistry::new();
        registry.register(Arc::new(ScriptedAdapter::new(plain_text_script())));
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
        (
            app(state.clone()),
            token,
            runtime,
            dir.path().to_path_buf(),
            state,
        )
    };
    let addr2 = serve(router2).await;

    let listed = client
        .get(format!(
            "http://{addr2}/code/workspaces/{workspace_id}/terminals"
        ))
        .bearer_auth(&token2)
        .send()
        .await
        .unwrap();
    let list: Vec<serde_json::Value> = listed.json().await.unwrap();
    assert!(list.is_empty(), "restart must reap live terminals");

    let read = client
        .get(format!(
            "http://{addr2}/code/workspaces/{workspace_id}/terminals/{tid}/read?cursor=0"
        ))
        .bearer_auth(&token2)
        .send()
        .await
        .unwrap();
    assert_eq!(read.status(), reqwest::StatusCode::OK);
    let page: serde_json::Value = read.json().await.unwrap();
    assert_eq!(page["ended"], true);
    assert_eq!(page["bytes"].as_str().unwrap(), "");

    assert_store_has_no_bytes(dir.path(), marker);
    drop(state2);
}

fn assert_store_has_no_bytes(root: &Path, needle: &[u8]) {
    fn walk(path: &Path, needle: &[u8]) {
        let Ok(meta) = std::fs::metadata(path) else {
            return;
        };
        if meta.is_file() {
            let Ok(bytes) = std::fs::read(path) else {
                return;
            };
            assert!(
                !bytes.windows(needle.len()).any(|window| window == needle),
                "terminal bytes persisted at {}",
                path.display()
            );
        } else if meta.is_dir() {
            let Ok(entries) = std::fs::read_dir(path) else {
                return;
            };
            for entry in entries.flatten() {
                walk(&entry.path(), needle);
            }
        }
    }
    walk(root, needle);
}
