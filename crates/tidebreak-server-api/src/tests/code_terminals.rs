//! Auxiliary terminal routes: cursor-pull, restart reap, no durable bytes.

use super::*;

use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::Arc;

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

/// An engine whose probe reports a real binary: a script standing in for the
/// pinned `claude`, so the sign-in terminal has something to run.
struct SignInEngine {
    inner: ScriptedAdapter,
    binary: Option<std::path::PathBuf>,
    home: std::path::PathBuf,
}

#[async_trait::async_trait]
impl tidebreak_harness::HarnessAdapter for SignInEngine {
    fn kind(&self) -> tidebreak_core::HarnessKind {
        self.inner.kind()
    }

    async fn probe(&self, _host: &tidebreak_harness::HostEnv) -> tidebreak_harness::HarnessProbe {
        tidebreak_harness::HarnessProbe {
            found: self.binary.is_some(),
            binary_path: self.binary.clone(),
            version: Some("2.1.259".into()),
            authenticated: Some(false),
            stderr: String::new(),
            env: vec![
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("HOME".into(), self.home.clone().into_os_string()),
                ("GITHUB_TOKEN".into(), "ghp_not_for_the_engine".into()),
            ],
            commands: Vec::new(),
            reported_efforts: None,
        }
    }

    fn capabilities(&self, probe: &tidebreak_harness::HarnessProbe) -> tidebreak_core::HarnessCaps {
        self.inner.capabilities(probe)
    }

    async fn launch(
        &self,
        spec: tidebreak_harness::SessionSpec,
    ) -> Result<Box<dyn tidebreak_harness::HarnessSession>, tidebreak_harness::HarnessError> {
        self.inner.launch(spec).await
    }
}

async fn sign_in_app(
    binary: Option<std::path::PathBuf>,
    home: &Path,
) -> (Router, Arc<str>, tempfile::TempDir) {
    let (dir, store) = temp_db_store("code-sign-in.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(SignInEngine {
        inner: ScriptedAdapter::new(plain_text_script()),
        binary,
        home: home.to_path_buf(),
    }));
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
    state.code = Some(runtime);
    let token = state.token.clone();
    (app(state), token, dir)
}

#[cfg(unix)]
async fn read_sign_in_until_ended(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    tid: &str,
) -> String {
    use base64::Engine;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut cursor = 0u64;
    let mut output = Vec::new();
    loop {
        let page: serde_json::Value = client
            .get(format!(
                "http://{addr}/code/harnesses/claude_code/sign-in/{tid}/read?cursor={cursor}"
            ))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(page["kind"], "claude_code");
        output.extend(
            base64::engine::general_purpose::STANDARD
                .decode(page["bytes"].as_str().unwrap())
                .unwrap(),
        );
        cursor = page["cursor"].as_u64().unwrap();
        if page["ended"] == true {
            return String::from_utf8_lossy(&output).into_owned();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the sign-in never ended: {}",
            String::from_utf8_lossy(&output)
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Sign in runs the binary the engine's sessions use, with the pinned
/// sign-in arguments, in a terminal the reader types into, and in the
/// environment a session of that engine gets.
#[cfg(unix)]
#[tokio::test]
async fn sign_in_runs_the_pinned_command_in_a_terminal_the_reader_types_into() {
    let root = tempfile::tempdir().unwrap();
    let engine = root.path().join("claude");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(
            &engine,
            "#!/bin/sh\necho \"args: $*\"\necho \"token: ${GITHUB_TOKEN:-none}\"\n\
             printf 'code? '\nread answer\necho \"got $answer in $(pwd)\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let home = root.path().canonicalize().unwrap();
    let (router, token, _dir) = sign_in_app(Some(engine), &home).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();

    let started = client
        .post(format!("http://{addr}/code/harnesses/claude_code/sign-in"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "cols": 100, "rows": 30 }))
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), reqwest::StatusCode::CREATED);
    let terminal: serde_json::Value = started.json().await.unwrap();
    assert_eq!(terminal["kind"], "claude_code");
    assert_eq!(terminal["command"], "claude auth login");
    let tid = terminal["id"].as_str().unwrap().to_owned();

    // A second press lands on the sign-in already running.
    let again: serde_json::Value = client
        .post(format!("http://{addr}/code/harnesses/claude_code/sign-in"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(again["id"], terminal["id"]);

    // "second\n", typed into the prompt.
    let typed = client
        .post(format!(
            "http://{addr}/code/harnesses/claude_code/sign-in/{tid}/write"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "bytes": "c2Vjb25kCg==" }))
        .send()
        .await
        .unwrap();
    assert_eq!(typed.status(), reqwest::StatusCode::NO_CONTENT);

    let output = read_sign_in_until_ended(&client, addr, &token, &tid).await;
    assert!(output.contains("args: auth login"), "{output}");
    // Only this engine's session environment: no unrelated secret.
    assert!(output.contains("token: none"), "{output}");
    assert!(
        output.contains(&format!("got second in {}", home.display())),
        "{output}"
    );

    // Once it has finished, Sign in runs the command again.
    let rerun: serde_json::Value = client
        .post(format!("http://{addr}/code/harnesses/claude_code/sign-in"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_ne!(rerun["id"], terminal["id"]);
    let closed = client
        .delete(format!(
            "http://{addr}/code/harnesses/claude_code/sign-in/{}",
            rerun["id"].as_str().unwrap()
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(closed.status(), reqwest::StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn sign_in_needs_the_engine_on_disk() {
    let home = tempfile::tempdir().unwrap();
    let (router, token, _dir) = sign_in_app(None, home.path()).await;
    let addr = serve(router).await;
    let refused = reqwest::Client::new()
        .post(format!("http://{addr}/code/harnesses/claude_code/sign-in"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "harness_not_found", "{body}");
}
