//! Shared fixtures for the code-mode route tests, plus the end-to-end
//! plan-mode smoke test. Topic files live beside this one as `code_*.rs`.

use super::*;

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use futures::StreamExt;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::code::browser_runtime::{BrowserRuntime, BrowserRuntimeError, BrowserRuntimeScope};
use crate::code::remote::driver::RemoteSpawnSettings;
use crate::code::remote::service::RemoteSessions;
use crate::code::remote::wire::{
    EventCursor, MessageReceipt, SandboxEvents, SandboxLease, SandboxMessage, SandboxStatus,
    SpawnArguments,
};
use crate::code::remote::{RemoteSandboxError, SandboxProvisioner};
use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{
    BlobStore, BrowserListResult, BrowserNavigateArgs, BrowserNavigateResult, BrowserPageSnapshot,
    BrowserScreenshotArgs, BrowserScreenshotResult, BrowserSnapshotArgs, BrowserWaitArgs,
    BrowserWaitResult, CodeSessionId, CodeTurnId, CodeTurnStatus, CodeWorkspaceStatus, DbStore,
    HarnessKind, PermissionMode, ReasoningEffort, RepoId, Store, WorkspaceId,
};
use tidebreak_harness::{AdapterRegistry, HarnessApprovalRef, HarnessEvent, ListedHarnessModel};

pub(super) struct PutGate {
    pub(super) started: tokio::sync::Notify,
    pub(super) release: tokio::sync::Notify,
}

struct GatedPutBlobStore {
    inner: Arc<dyn BlobStore>,
    gate: Arc<PutGate>,
}

#[async_trait]
impl BlobStore for GatedPutBlobStore {
    async fn put(&self, id: uuid::Uuid, bytes: Vec<u8>) -> tidebreak_core::Result<()> {
        self.gate.started.notify_one();
        self.gate.release.notified().await;
        self.inner.put(id, bytes).await
    }

    async fn get(&self, id: uuid::Uuid) -> tidebreak_core::Result<Option<Vec<u8>>> {
        self.inner.get(id).await
    }

    async fn delete(&self, id: uuid::Uuid) -> tidebreak_core::Result<()> {
        self.inner.delete(id).await
    }
}

#[derive(Default)]
pub(super) struct RecordingBrowserRuntime {
    pub(super) listed: Mutex<Vec<BrowserRuntimeScope>>,
    pub(super) revoked: Mutex<Vec<BrowserRuntimeScope>>,
}

#[async_trait]
impl BrowserRuntime for RecordingBrowserRuntime {
    async fn list(
        &self,
        scope: &BrowserRuntimeScope,
    ) -> Result<BrowserListResult, BrowserRuntimeError> {
        self.listed.lock().unwrap().push(scope.clone());
        Ok(BrowserListResult { sessions: vec![] })
    }

    async fn navigate(
        &self,
        _scope: &BrowserRuntimeScope,
        _args: &BrowserNavigateArgs,
    ) -> Result<BrowserNavigateResult, BrowserRuntimeError> {
        Err(BrowserRuntimeError::Unsupported("test navigate".into()))
    }

    async fn snapshot(
        &self,
        _scope: &BrowserRuntimeScope,
        _args: &BrowserSnapshotArgs,
    ) -> Result<BrowserPageSnapshot, BrowserRuntimeError> {
        Err(BrowserRuntimeError::Unsupported("test snapshot".into()))
    }

    async fn wait(
        &self,
        _scope: &BrowserRuntimeScope,
        _args: &BrowserWaitArgs,
    ) -> Result<BrowserWaitResult, BrowserRuntimeError> {
        Err(BrowserRuntimeError::Unsupported("test wait".into()))
    }

    async fn screenshot(
        &self,
        _scope: &BrowserRuntimeScope,
        _args: &BrowserScreenshotArgs,
    ) -> Result<BrowserScreenshotResult, BrowserRuntimeError> {
        Err(BrowserRuntimeError::Unsupported("test screenshot".into()))
    }

    fn revoke_session(&self, scope: &BrowserRuntimeScope) {
        self.revoked.lock().unwrap().push(scope.clone());
    }
}

struct UnusedRemoteProvisioner;

#[async_trait]
impl SandboxProvisioner for UnusedRemoteProvisioner {
    async fn spawn(
        &self,
        _owner: &tidebreak_core::OwnerId,
        _arguments: &SpawnArguments,
    ) -> Result<SandboxLease, RemoteSandboxError> {
        panic!("remote create-route tests must not provision a sandbox")
    }

    async fn status(
        &self,
        _owner: &tidebreak_core::OwnerId,
        _sandbox_id: &str,
    ) -> Result<SandboxStatus, RemoteSandboxError> {
        panic!("remote create-route tests must not read sandbox status")
    }

    async fn events(
        &self,
        _owner: &tidebreak_core::OwnerId,
        _sandbox_id: &str,
        _cursor: EventCursor,
    ) -> Result<SandboxEvents, RemoteSandboxError> {
        panic!("remote create-route tests must not read sandbox events")
    }

    async fn send(
        &self,
        _owner: &tidebreak_core::OwnerId,
        _sandbox_id: &str,
        _message: &SandboxMessage,
    ) -> Result<MessageReceipt, RemoteSandboxError> {
        panic!("remote create-route tests must not send sandbox messages")
    }

    async fn cancel(
        &self,
        _owner: &tidebreak_core::OwnerId,
        _sandbox_id: &str,
    ) -> Result<(), RemoteSandboxError> {
        panic!("remote create-route tests must not cancel a sandbox")
    }
}

pub(super) async fn code_app(
    events: Vec<HarnessEvent>,
) -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    code_app_with(ScriptedAdapter::new(events)).await
}

/// An OS policy that asserts one permission-mode ceiling and nothing else.
struct CappedOsPolicy(PermissionMode);

impl crate::managed_policy::OsPolicySource for CappedOsPolicy {
    fn gateway_url(&self) -> tidebreak_core::Result<Option<String>> {
        Ok(None)
    }

    fn permission_mode_ceiling(&self) -> tidebreak_core::Result<Option<PermissionMode>> {
        Ok(Some(self.0))
    }
}

pub(super) async fn code_app_with(
    adapter: ScriptedAdapter,
) -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    code_app_with_optional_browser(adapter, None).await
}

pub(super) async fn code_app_with_optional_browser(
    adapter: ScriptedAdapter,
    browser_runtime: Option<Arc<RecordingBrowserRuntime>>,
) -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    code_app_with_options(adapter, browser_runtime, None, None, false).await
}

pub(super) async fn code_app_with_remote(
    adapter: ScriptedAdapter,
    permission_mode_ceiling: Option<PermissionMode>,
) -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    code_app_with_options(adapter, None, None, permission_mode_ceiling, true).await
}

pub(super) async fn code_app_with_options(
    adapter: ScriptedAdapter,
    browser_runtime: Option<Arc<RecordingBrowserRuntime>>,
    put_gate: Option<Arc<PutGate>>,
    permission_mode_ceiling: Option<PermissionMode>,
    remote: bool,
) -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    let (dir, store) = temp_db_store("code.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(adapter));
    let installed_browser_runtime =
        browser_runtime.map(|runtime| -> Arc<dyn BrowserRuntime> { runtime });
    let browser_bridge_command = installed_browser_runtime
        .as_ref()
        .map(|_| crate::code::browser_channel::test_bridge_command());
    let runtime = CodeRuntime::with_registry_and_browser_runtime(
        db,
        dir.path().to_path_buf(),
        registry,
        installed_browser_runtime,
        browser_bridge_command,
    );
    let runtime = if remote {
        runtime.with_remote_sessions(RemoteSessions::new(
            Arc::new(UnusedRemoteProvisioner),
            RemoteSpawnSettings {
                profile: "test-remote".to_owned(),
                incarnation_cap: 2,
                spend_ceiling_microusd: None,
                session_spend_ceiling_microusd: None,
            },
        ))
    } else {
        runtime
    };
    let runtime = Arc::new(runtime);
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
    if let Some(gate) = put_gate {
        state.blobs = Arc::new(GatedPutBlobStore {
            inner: runtime.blobs.clone(),
            gate,
        });
    }
    if let Some(ceiling) = permission_mode_ceiling {
        state.os_policy = Arc::new(CappedOsPolicy(ceiling));
    }
    state.code = Some(runtime.clone());
    let token = state.token.clone();
    (app(state), token, runtime, dir)
}

pub(super) fn approval_script() -> Vec<HarnessEvent> {
    write_approval_script("toolu_scripted", "hello")
}

pub(super) fn write_approval_script(call_id: &str, content: &str) -> Vec<HarnessEvent> {
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
                "tool_name": "Write",
                "input": { "file_path": "/workspace/probe.txt", "content": content },
                "tool_use_id": call_id
            }),
            kind: None,
        },
        HarnessEvent::AssistantDelta {
            text: "after the decision".into(),
        },
        HarnessEvent::TurnCompleted {
            usage: Default::default(),
        },
    ]
}

/// Every event a session journaled, in order.
pub(super) async fn journaled_events(
    db: &DbStore,
    session_id: CodeSessionId,
) -> Vec<tidebreak_core::SequencedCodeEvent> {
    tidebreak_core::db::code::list_events(
        db,
        &tidebreak_core::OwnerId::local(),
        session_id,
        0,
        tidebreak_core::db::code::MAX_REPLAY_EVENTS,
    )
    .await
    .unwrap()
    .events
}

pub(super) fn init_git_repo_named(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let repo = dir.join(name);
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        ["git", "init", "-b", "main"].as_slice(),
        ["git", "config", "user.email", "dev@example.com"].as_slice(),
        ["git", "config", "user.name", "Dev"].as_slice(),
        // Windows runners default to autocrlf=true; pin LF so file-content
        // assertions stay identical across platforms.
        ["git", "config", "core.autocrlf", "false"].as_slice(),
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

pub(super) fn init_git_repo(dir: &std::path::Path) -> std::path::PathBuf {
    init_git_repo_named(dir, "origin")
}

pub(super) fn add_github_remote(repo: &std::path::Path, name: &str) {
    assert!(std::process::Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            &format!("https://github.com/acme/{name}.git"),
        ])
        .current_dir(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .status()
        .unwrap()
        .success());
}

pub(super) async fn record_github_origin(
    runtime: &CodeRuntime,
    repo: &serde_json::Value,
    name: &str,
) {
    let repo_id: RepoId = json_id(repo).parse().unwrap();
    assert!(tidebreak_core::db::code::set_repo_origin(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        repo_id,
        "github.com",
        "acme",
        name,
    )
    .await
    .unwrap());
}

pub(super) async fn register_and_workspace(
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

pub(super) fn json_id(value: &serde_json::Value) -> &str {
    value["id"].as_str().expect("id is a string")
}

pub(super) async fn serve(router: Router) -> std::net::SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}

pub(super) fn listed_model(
    id: &str,
    default: bool,
    reasoning_efforts: &[ReasoningEffort],
    fast_mode: bool,
) -> ListedHarnessModel {
    ListedHarnessModel {
        id: id.into(),
        label: id.into(),
        default,
        reasoning_efforts: reasoning_efforts.to_vec(),
        fast_mode,
    }
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

/// Poll until `ready` holds, or fail the test.
///
/// A turn is accepted before it runs, so everything the worker does with it —
/// writing an attachment, handing the engine its input — lands after the
/// response the caller already has.
pub(super) async fn wait_until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("condition never held");
}

pub(super) async fn wait_for_open_turn(
    runtime: &CodeRuntime,
    session_id: CodeSessionId,
) -> CodeTurnId {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(turn) = tidebreak_core::db::code::get_open_turn(
                &runtime.db,
                &tidebreak_core::OwnerId::local(),
                session_id,
            )
            .await
            .unwrap()
            {
                break turn.id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("turn never became active")
}

/// Create `count` interactive sessions in one workspace, returning their ids.
pub(super) async fn create_sibling_sessions(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    workspace: &serde_json::Value,
    count: usize,
) -> Vec<String> {
    let mut ids = Vec::new();
    for _ in 0..count {
        let created = client
            .post(format!(
                "http://{addr}/code/workspaces/{}/sessions",
                json_id(workspace)
            ))
            .bearer_auth(token)
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
            "a sibling agent in one workspace must create"
        );
        let body: serde_json::Value = created.json().await.unwrap();
        ids.push(json_id(&body).to_owned());
    }
    ids
}

pub(super) fn scripted_registry() -> AdapterRegistry {
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(ScriptedAdapter::new(plain_text_script())));
    registry
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

pub(super) async fn next_json(
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

#[allow(dead_code)]
fn _types(
    _id: WorkspaceId,
    _status: CodeWorkspaceStatus,
    _turn: CodeTurnStatus,
    _db: Option<DbStore>,
    _mode: PermissionMode,
    _kind: HarnessKind,
) {
}

/// Whether a branch ref exists in a repository.
pub(super) fn branch_exists_in(repo_root: &std::path::Path, branch: &str) -> bool {
    std::process::Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .current_dir(repo_root)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}
