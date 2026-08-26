//! End-to-end code-mode routes against the scripted engine.

use super::*;

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;

use axum::Router;
use tokio::net::TcpListener;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::code::browser_runtime::{BrowserRuntime, BrowserRuntimeError, BrowserRuntimeScope};
use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{
    Attention, AttentionSource, AttentionState, BlobStore, BrowserListResult, BrowserNavigateArgs,
    BrowserNavigateResult, BrowserPageSnapshot, BrowserScreenshotArgs, BrowserScreenshotResult,
    BrowserSnapshotArgs, BrowserWaitArgs, BrowserWaitResult, CapLevel, CodeEvent, CodeRepo,
    CodeSession, CodeSessionId, CodeSessionLifecycle, CodeTurnId, CodeTurnStatus, CodeWorkspace,
    CodeWorkspaceStatus, DbStore, FenceReason, HarnessKind, PermissionMode, ReasoningEffort,
    RepoId, Store, WorkspaceId,
};
use tidebreak_harness::{AdapterRegistry, ApprovalDecision, HarnessApprovalRef, HarnessEvent};

struct PutGate {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
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

    fn delete(&self, id: uuid::Uuid) -> tidebreak_core::Result<()> {
        self.inner.delete(id)
    }
}

#[derive(Default)]
struct RecordingBrowserRuntime {
    listed: Mutex<Vec<BrowserRuntimeScope>>,
    revoked: Mutex<Vec<BrowserRuntimeScope>>,
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

async fn code_app(
    events: Vec<HarnessEvent>,
) -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    code_app_with(ScriptedAdapter::new(events)).await
}

async fn code_app_with(
    adapter: ScriptedAdapter,
) -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    code_app_with_optional_browser(adapter, None).await
}

async fn code_app_with_browser(
    adapter: ScriptedAdapter,
    browser_runtime: Arc<RecordingBrowserRuntime>,
) -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    code_app_with_optional_browser(adapter, Some(browser_runtime)).await
}

async fn code_app_with_optional_browser(
    adapter: ScriptedAdapter,
    browser_runtime: Option<Arc<RecordingBrowserRuntime>>,
) -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    code_app_with_options(adapter, browser_runtime, None).await
}

async fn code_app_with_put_gate(
    adapter: ScriptedAdapter,
    gate: Arc<PutGate>,
) -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    code_app_with_options(adapter, None, Some(gate)).await
}

async fn code_app_with_options(
    adapter: ScriptedAdapter,
    browser_runtime: Option<Arc<RecordingBrowserRuntime>>,
    put_gate: Option<Arc<PutGate>>,
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
    let runtime = Arc::new(CodeRuntime::with_registry_and_browser_runtime(
        db,
        dir.path().to_path_buf(),
        registry,
        installed_browser_runtime,
        browser_bridge_command,
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
    if let Some(gate) = put_gate {
        state.blobs = Arc::new(GatedPutBlobStore {
            inner: runtime.blobs.clone(),
            gate,
        });
    }
    state.code = Some(runtime.clone());
    let token = state.token.clone();
    (app(state), token, runtime, dir)
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
            harness_ref: HarnessApprovalRef::engine(call_id),
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

/// An engine that asks for an approval and then stops waiting for one. Claude
/// Code times a parked permission prompt out after 60 seconds, resolves the
/// tool call `failed`, and finishes the turn with nobody having decided.
fn timed_out_approval_script() -> Vec<HarnessEvent> {
    let mut events = undecided_approval_script("toolu_timeout");
    events.insert(
        3,
        HarnessEvent::ToolCompleted {
            call_id: "toolu_timeout".into(),
            outcome: tidebreak_core::ToolOutcome::Failed,
            preview: "Bash tool call timed out after 60s".into(),
            detail: None,
            parent_call_id: None,
        },
    );
    events
}

/// The same request, but the engine never reports the call at all: only the
/// turn boundary can settle the row.
fn dropped_approval_script() -> Vec<HarnessEvent> {
    undecided_approval_script("toolu_dropped")
}

fn undecided_approval_script(call_id: &str) -> Vec<HarnessEvent> {
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
                "tool_name": "Bash",
                "input": { "command": "rm -rf /tmp/scratch" },
                "tool_use_id": call_id
            }),
        },
        HarnessEvent::TurnCompleted {
            usage: Default::default(),
        },
    ]
}

/// Register a repo and workspace, open an Ask session, and run one turn to
/// completion. Only useful for scripts that never park, which is every script
/// modelling an approval nobody decides.
async fn ran_one_ask_turn(
    adapter: ScriptedAdapter,
) -> (
    reqwest::Client,
    std::net::SocketAddr,
    Arc<str>,
    CodeSessionId,
    Arc<CodeRuntime>,
    tempfile::TempDir,
) {
    let (router, token, runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session: serde_json::Value = client
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
        .json()
        .await
        .unwrap();
    let session_id: CodeSessionId = json_id(&session).parse().unwrap();
    let turn = tokio::time::timeout(
        Duration::from_secs(10),
        client
            .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "message": "run it" }))
            .send(),
    )
    .await
    .expect("the turn must not park on an approval nobody answers")
    .unwrap();
    assert_eq!(turn.status(), reqwest::StatusCode::ACCEPTED);
    (client, addr, token, session_id, runtime, dir)
}

/// Every approval on the session, whatever its state.
async fn approvals_for(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    session_id: CodeSessionId,
) -> Vec<serde_json::Value> {
    client
        .get(format!(
            "http://{addr}/code/approvals?session_id={session_id}"
        ))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// Every event a session journaled, in order.
async fn journaled_events(
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

/// The outcome carried on every `ApprovalResolved` the session journaled.
async fn journaled_resolutions(
    db: &DbStore,
    session_id: CodeSessionId,
) -> Vec<tidebreak_core::ApprovalDecisionKind> {
    journaled_events(db, session_id)
        .await
        .into_iter()
        .filter_map(|framed| match framed.event {
            CodeEvent::ApprovalResolved { decision, .. } => Some(decision),
            _ => None,
        })
        .collect()
}

fn init_git_repo_named(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
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

/// The smallest valid PNG: a 1x1 RGBA pixel.
///
/// Attachment paths run the real ingest, which reads dimensions out of the
/// header, so a signature followed by filler is not an image and never was —
/// it only used to reach the turn because resolution trusted the blob store
/// rather than a publication.
fn one_pixel_png() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ]
}

/// Publish one image to a session and return its blob id.
///
/// Publication is the authority a turn attachment is checked against, so a
/// test that attaches an image has to reserve it the way a client does.
async fn publish_one_pixel_png(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    session_id: &str,
) -> String {
    let response = client
        .post(format!(
            "http://{addr}/code/sessions/{session_id}/attachments/images"
        ))
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_TYPE, "image/png")
        .body(one_pixel_png())
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        reqwest::StatusCode::CREATED,
        "publishing the fixture image failed"
    );
    let body: serde_json::Value = response.json().await.unwrap();
    body["attachment_id"]
        .as_str()
        .or_else(|| body["blob_id"].as_str())
        .expect("the publication names the blob")
        .to_owned()
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
    let row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
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

/// Poll until `ready` holds, or fail the test.
///
/// A turn is accepted before it runs, so everything the worker does with it —
/// writing an attachment, handing the engine its input — lands after the
/// response the caller already has.
async fn wait_until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("condition never held");
}

async fn wait_for_open_turn(runtime: &CodeRuntime, session_id: CodeSessionId) -> CodeTurnId {
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

/// Only Claude Code's adapter puts image bytes on the wire. On every other
/// engine an attached image used to be dropped between the composer and the
/// child, because the field it rides is one those adapters never read. The
/// bytes go to disk instead and the prompt carries the path, which is the
/// route a fork's transcript already takes.
#[tokio::test]
async fn an_engine_with_no_image_protocol_is_handed_the_file_and_its_path() {
    let adapter = ScriptedAdapter::new(plain_text_script())
        .with_image_input(CapLevel::Unsupported)
        .with_delay(std::time::Duration::from_millis(100));
    let engine = adapter.clone();
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let worktree =
        std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap().to_owned());
    let session = create_sibling_sessions(&client, addr, &token, &workspace, 1)
        .await
        .remove(0);

    let pixels = crate::routes::image_attachment::png_header(4, 4);
    let published = client
        .post(format!(
            "http://{addr}/code/sessions/{session}/attachments/images"
        ))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, "image/png")
        .body(pixels.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), reqwest::StatusCode::CREATED);
    let attachment: serde_json::Value = published.json().await.unwrap();
    let blob_id = attachment["attachment_id"].as_str().unwrap().to_owned();

    let turn_request = {
        let client = client.clone();
        let token = token.clone();
        let blob_id = blob_id.clone();
        let turn_url = format!("http://{addr}/code/sessions/{session}/turns");
        tokio::spawn(async move {
            client
                .post(turn_url)
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "message": "what is in this",
                    "attachments": [{ "blob_id": blob_id, "media_type": "image/png" }],
                }))
                .send()
                .await
                .unwrap()
        })
    };

    wait_until(|| !engine.turn_inputs().is_empty()).await;
    let handed = engine.turn_inputs().remove(0);
    assert_eq!(
        handed.images, 0,
        "an engine with no image protocol is sent no image bytes"
    );
    assert!(
        handed.text.starts_with("what is in this"),
        "the message the person wrote leads: {:?}",
        handed.text
    );
    let path = handed
        .text
        .lines()
        .find_map(|line| line.strip_prefix("- `")?.strip_suffix('`'))
        .map(std::path::PathBuf::from)
        .expect("the prompt names the attachment path");
    assert!(path.is_absolute(), "the engine receives an absolute path");
    assert!(
        !path.starts_with(&worktree),
        "private attachment storage must stay outside the Git worktree: {}",
        path.display()
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        pixels,
        "the engine reads the bytes while the turn is active"
    );

    let accepted = turn_request.await.unwrap();
    let accepted_status = accepted.status();
    let accepted_body: serde_json::Value = accepted.json().await.unwrap();
    assert_eq!(
        accepted_status,
        reqwest::StatusCode::ACCEPTED,
        "{accepted_body}"
    );
    assert!(
        !path.exists(),
        "the worker removes the private attachment after the turn"
    );

    // The transcript keeps what was typed, not what the engine was handed.
    let turns: Vec<serde_json::Value> = client
        .get(format!("http://{addr}/code/sessions/{session}/turns"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(turns[0]["user_input"], "what is in this");

    assert!(
        !worktree.join(".tidebreak").exists(),
        "fallback delivery does not write private bytes into the worktree"
    );
}

/// The engine that has its own image path keeps it. Bytes in the protocol are
/// lossless and already captured; writing a file for Claude Code would cost a
/// tool call to read back something it can already see.
#[tokio::test]
async fn an_engine_that_states_image_input_is_still_handed_the_bytes() {
    let adapter = ScriptedAdapter::new(plain_text_script()).with_image_input(CapLevel::Supported);
    let engine = adapter.clone();
    let (router, token, runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = create_sibling_sessions(&client, addr, &token, &workspace, 1)
        .await
        .remove(0);

    let published = client
        .post(format!(
            "http://{addr}/code/sessions/{session}/attachments/images"
        ))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, "image/png")
        .body(crate::routes::image_attachment::png_header(4, 4))
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), reqwest::StatusCode::CREATED);
    let attachment: serde_json::Value = published.json().await.unwrap();

    let accepted = client
        .post(format!("http://{addr}/code/sessions/{session}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "message": "what is in this",
            "attachments": [{
                "blob_id": attachment["attachment_id"],
                "media_type": "image/png",
            }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), reqwest::StatusCode::ACCEPTED);

    wait_until(|| !engine.turn_inputs().is_empty()).await;
    let handed = engine.turn_inputs().remove(0);
    assert_eq!(handed.images, 1, "the bytes ride the protocol");
    assert_eq!(
        handed.text, "what is in this",
        "nothing is appended to a prompt that carries the image itself"
    );
    assert!(
        !runtime
            .data_dir
            .join("code")
            .join("private")
            .join(workspace["id"].as_str().unwrap())
            .join("attachments")
            .exists(),
        "no file is written for an engine that never needs to read one"
    );
}

/// The engines set an effort per turn, so a level chosen mid-conversation has
/// to reach the next turn without a new session. Before this the picker was a
/// local map that forgot on reload and never left the renderer.
#[tokio::test]
async fn reasoning_effort_is_chosen_mid_conversation_and_reaches_the_next_turn() {
    let adapter = ScriptedAdapter::new(plain_text_script())
        .with_reasoning_levels(CapLevel::Supported)
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

    let turn = |body: serde_json::Value| {
        let client = client.clone();
        let token = token.clone();
        let session = session.clone();
        async move {
            let response = client
                .post(format!("http://{addr}/code/sessions/{session}/turns"))
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
        }
    };
    let set_effort = |effort: serde_json::Value| {
        let client = client.clone();
        let token = token.clone();
        let session = session.clone();
        async move {
            let response = client
                .post(format!("http://{addr}/code/sessions/{session}/effort"))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "reasoning_effort": effort }))
                .send()
                .await
                .unwrap();
            let status = response.status();
            let body: serde_json::Value = response.json().await.unwrap();
            (status, body)
        }
    };

    // Nothing chosen: the engine's own default is left in force.
    turn(serde_json::json!({ "message": "one" })).await;

    let (status, body) = set_effort(serde_json::json!("ultra")).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["reasoning_effort"], "ultra", "{body}");
    turn(serde_json::json!({ "message": "two" })).await;

    // A turn may carry its own level, and that choice sticks the way a model
    // chosen in the composer does.
    turn(serde_json::json!({ "message": "three", "reasoning_effort": "low" })).await;
    turn(serde_json::json!({ "message": "four" })).await;

    // Back to the engine's default: `null` is a choice, not an omission, and
    // the turn body must read it the same way the route does.
    turn(serde_json::json!({ "message": "five", "reasoning_effort": null })).await;
    let (status, body) = set_effort(serde_json::Value::Null).await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "the turn already cleared it, so this is a no-op: {body}"
    );
    assert!(body["reasoning_effort"].is_null(), "{body}");

    assert_eq!(
        engine.turn_efforts(),
        vec![
            None,
            Some(ReasoningEffort::Ultra),
            Some(ReasoningEffort::Low),
            Some(ReasoningEffort::Low),
            None,
        ],
        "each turn runs at the level in force when it was sent",
    );

    // It survives a reload: the level is on the session row, not in a renderer.
    let reread: serde_json::Value = client
        .get(format!("http://{addr}/code/sessions/{session}/debug"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(reread["session"]["reasoning_effort"].is_null(), "{reread}");
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

/// Create `count` interactive sessions in one workspace, returning their ids.
async fn create_sibling_sessions(
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
            if let CodeEvent::UserSteered { text } = &event {
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
    let row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
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
    let row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
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
    // Same repo name, so the same repo folder — the workspace id suffix is
    // what keeps the two checkouts apart.
    assert_ne!(path_a, path_b);
    assert!(path_a.contains(&json_id(&ws_a)[..8]));
    assert!(path_b.contains(&json_id(&ws_b)[..8]));
    assert!(std::path::Path::new(path_a).join("README.md").is_file());
    assert!(std::path::Path::new(path_b).join("README.md").is_file());
}

/// The worktree root is a setting, and moving it moves only what comes next.
///
/// The two halves are one test because the second is meaningless without the
/// first: a root that new workspaces honour but old ones silently follow would
/// leave every existing checkout pointing at nothing.
#[tokio::test]
async fn the_worktree_root_moves_new_workspaces_and_leaves_existing_ones() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (repo_body, before) = register_and_workspace(&client, addr, &token, &repo).await;
    let before_path = before["worktree_path"].as_str().unwrap().to_owned();
    // The default is the data directory until a root is set.
    let defaults = client
        .get(format!("http://{addr}/code/worktree-root"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(defaults["root"].is_null());
    assert_eq!(defaults["effective_root"], defaults["default_root"]);
    assert!(before_path.starts_with(defaults["default_root"].as_str().unwrap()));

    // A root that does not exist yet is created rather than refused.
    let chosen = dir.path().join("visible").join("workspaces");
    let moved = client
        .put(format!("http://{addr}/code/worktree-root"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "root": chosen }))
        .send()
        .await
        .unwrap();
    assert_eq!(moved.status(), reqwest::StatusCode::OK);
    let moved: serde_json::Value = moved.json().await.unwrap();
    assert_eq!(moved["root"], moved["effective_root"]);
    assert!(chosen.is_dir());

    let created = client
        .post(format!("http://{addr}/code/workspaces"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repo_id": json_id(&repo_body),
            "title": "second change",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let after: serde_json::Value = created.json().await.unwrap();
    let after_path = after["worktree_path"].as_str().unwrap();
    assert!(
        after_path.starts_with(chosen.to_str().unwrap()),
        "{after_path}"
    );
    // Readable name first, id last.
    assert!(after_path.ends_with(&format!("second-change-{}", &json_id(&after)[..8])));
    assert!(std::path::Path::new(after_path).join("README.md").is_file());

    // The workspace created before the move keeps the path on its row, and the
    // checkout is still there.
    let reread = client
        .get(format!(
            "http://{addr}/code/workspaces/{}",
            json_id(&before)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(reread["worktree_path"], before_path);
    assert!(std::path::Path::new(&before_path)
        .join("README.md")
        .is_file());

    // Clearing the setting returns the deployment to its default and, again,
    // touches nothing on disk.
    let cleared = client
        .put(format!("http://{addr}/code/worktree-root"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "root": null }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(cleared["root"].is_null());
    assert_eq!(cleared["effective_root"], cleared["default_root"]);
}

/// A root the deployment cannot write worktrees under is refused when it is
/// set, not at the first workspace that fails.
#[tokio::test]
async fn the_worktree_root_refuses_a_relative_path_and_a_file() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();

    let relative = client
        .put(format!("http://{addr}/code/worktree-root"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "root": "workspaces" }))
        .send()
        .await
        .unwrap();
    assert_eq!(relative.status(), reqwest::StatusCode::BAD_REQUEST);

    let file = dir.path().join("not-a-directory");
    std::fs::write(&file, b"x").unwrap();
    let refused = client
        .put(format!("http://{addr}/code/worktree-root"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "root": file }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::BAD_REQUEST);
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
    let mut streamed = String::new();
    let read = async {
        while let Some(frame) = socket.next().await {
            let WsMessage::Text(text) = frame.unwrap() else {
                continue;
            };
            let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
            // Deltas ride the live stream without a row, so they repeat the
            // cursor rather than advancing it. The ordering contract is about
            // the journal.
            if value["transient"] == true {
                streamed.push_str(value["event"]["text"].as_str().unwrap());
                continue;
            }
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
    assert_eq!(streamed, "onetwo", "deltas must still stream live");
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
        &tidebreak_core::OwnerId::local(),
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
        &tidebreak_core::OwnerId::local(),
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

/// Delta rows are no longer written, but journals that already hold them
/// must still read back. This appends them the way a pre-record-57 session
/// did and replays the result.
#[tokio::test(flavor = "multi_thread")]
async fn ws_replay_emits_every_durable_sequence_after_the_cursor_in_order() {
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
    let parsed: CodeSessionId = session_id.parse().unwrap();
    let replay_after_seq = journaled_events(&runtime.db, parsed)
        .await
        .last()
        .map_or(0, |event| event.seq);

    let first_delta_seq = tidebreak_core::db::code::append_event(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
        1,
        &tidebreak_core::CodeEvent::AssistantDelta { text: "hel".into() },
    )
    .await
    .unwrap();
    let second_delta_seq = tidebreak_core::db::code::append_event(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
        1,
        &tidebreak_core::CodeEvent::AssistantDelta { text: "lo".into() },
    )
    .await
    .unwrap();

    let mut replay_request =
        format!("ws://{addr}/code/sessions/{session_id}/events?after={replay_after_seq}")
            .into_client_request()
            .unwrap();
    replay_request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut replay_socket, _) = connect_async(replay_request).await.unwrap();

    let mut replayed = Vec::new();
    for _ in 0..2 {
        let frame = tokio::time::timeout(Duration::from_secs(2), replay_socket.next())
            .await
            .expect("durable replay timed out")
            .expect("replay socket closed")
            .unwrap();
        let WsMessage::Text(text) = frame else {
            continue;
        };
        replayed.push(serde_json::from_str::<serde_json::Value>(text.as_str()).unwrap());
    }
    assert_eq!(
        replayed
            .iter()
            .map(|frame| frame["seq"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![first_delta_seq, second_delta_seq]
    );
    assert_eq!(replayed[0]["event"]["text"], "hel");
    assert_eq!(replayed[1]["event"]["text"], "lo");
    assert_eq!(replayed[0]["replayed"], true);
    assert_eq!(replayed[1]["replayed"], true);
}

/// Record 57: the message states the whole answer, so the deltas that built
/// it are streamed and dropped. A plausible wrong implementation stops
/// publishing them too and passes every durable assertion here.
#[tokio::test(flavor = "multi_thread")]
async fn a_turn_journals_its_message_and_none_of_the_deltas_that_built_it() {
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(vec![
            HarnessEvent::TurnStarted,
            HarnessEvent::AssistantDelta {
                text: "half a ".into(),
            },
            HarnessEvent::AssistantDelta {
                text: "sentence".into(),
            },
            HarnessEvent::AssistantMessage {
                text: "half a sentence".into(),
                parent_call_id: None,
            },
            HarnessEvent::TurnCompleted {
                usage: Default::default(),
            },
        ])
        .with_delay(Duration::from_millis(10)),
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

    let mut request = format!("ws://{addr}/code/sessions/{session_id}/events?after=0")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();

    let turn = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "hi" }))
        .send()
        .await
        .unwrap();
    assert_eq!(turn.status(), reqwest::StatusCode::ACCEPTED);

    let mut streamed = Vec::new();
    let read = async {
        while let Some(frame) = socket.next().await {
            let WsMessage::Text(text) = frame.unwrap() else {
                continue;
            };
            let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
            if value["event"]["type"] == "assistant_delta" {
                assert_eq!(value["transient"], true, "a delta must not claim a row");
                streamed.push(value["event"]["text"].as_str().unwrap().to_owned());
            }
            if value["event"]["type"] == "turn_completed" {
                break;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .expect("turn did not complete over the socket");
    assert_eq!(streamed, vec!["half a ", "sentence"]);

    let journaled = journaled_events(&runtime.db, parsed).await;
    assert!(
        !journaled
            .iter()
            .any(|framed| matches!(framed.event, CodeEvent::AssistantDelta { .. })),
        "a delta reached the journal: {journaled:?}"
    );
    let messages: Vec<&str> = journaled
        .iter()
        .filter_map(|framed| match &framed.event {
            CodeEvent::AssistantMessage { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(messages, vec!["half a sentence"]);
}

/// Interrupt an answer mid-sentence and the words the reader watched arrive
/// must survive a reload. The engine never sent a message for them, so the
/// server writes the one it owed.
#[tokio::test(flavor = "multi_thread")]
async fn text_streamed_before_an_interrupt_is_written_down() {
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(vec![
            HarnessEvent::TurnStarted,
            HarnessEvent::AssistantDelta {
                text: "thinking out ".into(),
            },
            HarnessEvent::AssistantDelta {
                text: "loud".into(),
            },
            HarnessEvent::AssistantDelta {
                text: " and on".into(),
            },
            HarnessEvent::TurnCompleted {
                usage: Default::default(),
            },
        ])
        .with_delay(Duration::from_millis(150)),
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

    let turn_req = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "go" }));
    let interrupt = async {
        // Wait for the first delta so there is streamed text to lose.
        loop {
            let event = events.recv().await.unwrap();
            if matches!(event.event, CodeEvent::AssistantDelta { .. }) {
                break;
            }
        }
        client
            .post(format!(
                "http://{addr}/code/sessions/{session_id}/interrupt"
            ))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
    };
    let (turn, interrupted) = tokio::join!(turn_req.send(), interrupt);
    assert_eq!(interrupted.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(turn.unwrap().status(), reqwest::StatusCode::ACCEPTED);

    let recovered = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let journaled = journaled_events(&runtime.db, parsed).await;
            let text = journaled.iter().find_map(|framed| match &framed.event {
                CodeEvent::AssistantMessage { text, .. } => Some(text.clone()),
                _ => None,
            });
            if let Some(text) = text {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("streamed text must survive the interrupt");
    assert!(
        recovered.starts_with("thinking out"),
        "expected the streamed text, got {recovered:?}"
    );
}

/// Replay is capped, and the client is told when the cap bit. Silently
/// dropping the head would let a long session open on its middle and read as
/// if that were where it began.
#[tokio::test(flavor = "multi_thread")]
async fn a_capped_replay_tells_the_socket_that_history_was_dropped() {
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
    let parsed: CodeSessionId = session_id.parse().unwrap();

    let overflow = tidebreak_core::db::code::MAX_REPLAY_EVENTS + 2;
    let epoch = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap()
    .unwrap()
    .spawn_epoch;
    for index in 0..overflow {
        tidebreak_core::db::code::append_event(
            &runtime.db,
            &tidebreak_core::OwnerId::local(),
            parsed,
            epoch,
            &tidebreak_core::CodeEvent::HarnessNotice {
                level: tidebreak_core::HarnessNoticeLevel::Info,
                message: format!("notice {index}"),
            },
        )
        .await
        .unwrap();
    }

    let mut request = format!("ws://{addr}/code/sessions/{session_id}/events?after=0")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .expect("replay timed out")
        .expect("socket closed")
        .unwrap();
    let WsMessage::Text(text) = frame else {
        panic!("expected a text frame");
    };
    let first: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
    assert_eq!(first["truncated"], true, "{first}");
    // The window keeps the newest events, so it starts a cap's worth below
    // the head of the journal rather than at the cursor the client asked
    // from.
    let newest = journaled_events(&runtime.db, parsed)
        .await
        .last()
        .expect("the session journaled something")
        .seq;
    assert_eq!(
        first["seq"].as_i64().unwrap(),
        newest - tidebreak_core::db::code::MAX_REPLAY_EVENTS as i64 + 1
    );

    let second = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .expect("second frame timed out")
        .expect("socket closed")
        .unwrap();
    let WsMessage::Text(text) = second else {
        panic!("expected a text frame");
    };
    let second: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
    assert!(
        second.get("truncated").is_none(),
        "only the first frame of the window carries the flag: {second}"
    );
}

/// A reader who opens the pane mid-answer must see the sentence from its
/// start, not from wherever they happened to arrive. Nothing durable holds
/// that text yet, so the live tail is the only source.
#[tokio::test(flavor = "multi_thread")]
async fn connecting_mid_answer_replays_the_text_that_already_streamed() {
    let (router, token, runtime, dir) = code_app_with(
        ScriptedAdapter::new(vec![
            HarnessEvent::TurnStarted,
            HarnessEvent::AssistantDelta {
                text: "first ".into(),
            },
            HarnessEvent::AssistantDelta {
                text: "second ".into(),
            },
            HarnessEvent::AssistantDelta {
                text: "third".into(),
            },
            HarnessEvent::AssistantMessage {
                text: "first second third".into(),
                parent_call_id: None,
            },
            HarnessEvent::TurnCompleted {
                usage: Default::default(),
            },
        ])
        .with_delay(Duration::from_millis(200)),
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
                .json(&serde_json::json!({ "message": "go" }))
                .send()
                .await
                .unwrap()
        }
    });

    // Let two deltas go by, then connect as a second reader would.
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut seen = 0;
        while seen < 2 {
            if matches!(
                events.recv().await.unwrap().event,
                CodeEvent::AssistantDelta { .. }
            ) {
                seen += 1;
            }
        }
    })
    .await
    .expect("no deltas streamed");

    let mut request = format!("ws://{addr}/code/sessions/{session_id}/events?after=0")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();

    let mut assembled = String::new();
    let mut finalized = None;
    let read = async {
        while let Some(frame) = socket.next().await {
            let WsMessage::Text(text) = frame.unwrap() else {
                continue;
            };
            let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
            match value["event"]["type"].as_str() {
                Some("assistant_delta") => {
                    assembled.push_str(value["event"]["text"].as_str().unwrap());
                }
                Some("assistant_message") => {
                    finalized = Some(value["event"]["text"].as_str().unwrap().to_owned());
                    break;
                }
                _ => {}
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .expect("the late reader never saw the message");
    assert_eq!(
        assembled, "first second third",
        "a mid-answer reader must be caught up before the message lands"
    );
    assert_eq!(finalized.as_deref(), Some("first second third"));
    let _ = turn.await;
}

/// A reconnect may keep a prefix and miss later deltas while the socket is
/// down. The server sends the complete tail as a replacement.
#[tokio::test(flavor = "multi_thread")]
async fn reconnecting_mid_answer_replaces_with_the_complete_live_tail() {
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
    let parsed: CodeSessionId = session_id.parse().unwrap();

    let mut request = format!("ws://{addr}/code/sessions/{session_id}/events?after=0")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();

    let epoch = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap()
    .unwrap()
    .spawn_epoch;
    let marker = CodeEvent::HarnessNotice {
        level: tidebreak_core::HarnessNoticeLevel::Info,
        message: "reconnect cursor".into(),
    };
    let cursor = tidebreak_core::db::code::append_event(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
        epoch,
        &marker,
    )
    .await
    .unwrap();
    runtime.bus.publish(
        parsed,
        tidebreak_core::SequencedCodeEvent {
            seq: cursor,
            event: marker,
        },
    );

    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(frame) = socket.next().await {
            let WsMessage::Text(text) = frame.unwrap() else {
                continue;
            };
            let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
            if value["seq"] == cursor {
                break;
            }
        }
    })
    .await
    .expect("the socket did not reach the reconnect cursor");

    runtime.bus.publish_transient(
        parsed,
        CodeEvent::AssistantDelta {
            text: "first ".into(),
        },
    );
    runtime.bus.publish_transient(
        parsed,
        CodeEvent::AssistantDelta {
            text: "second ".into(),
        },
    );

    let mut assembled = String::new();
    for _ in 0..2 {
        let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("a live delta timed out")
            .expect("the live socket closed")
            .unwrap();
        let WsMessage::Text(text) = frame else {
            panic!("expected a text frame");
        };
        let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
        assert_eq!(value["seq"], cursor);
        assert_eq!(value["transient"], true);
        assembled.push_str(value["event"]["text"].as_str().unwrap());
    }
    assert_eq!(assembled, "first second ");
    drop(socket);

    runtime.bus.publish_transient(
        parsed,
        CodeEvent::AssistantDelta {
            text: "third".into(),
        },
    );

    let mut request = format!("ws://{addr}/code/sessions/{session_id}/events?after={cursor}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut resumed, _) = connect_async(request).await.unwrap();

    let frame = tokio::time::timeout(Duration::from_secs(5), resumed.next())
        .await
        .expect("the replacement tail timed out")
        .expect("the resumed socket closed")
        .unwrap();
    let WsMessage::Text(text) = frame else {
        panic!("expected a text frame");
    };
    let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
    assert_eq!(value["seq"], cursor);
    assert_eq!(value["event"]["type"], "assistant_delta");
    assert_eq!(value["event"]["text"], "first second third");
    assert_eq!(value["transient"], true);
    assert_eq!(value["replacement"], true);
    assembled = value["event"]["text"].as_str().unwrap().to_owned();

    runtime.bus.publish_transient(
        parsed,
        CodeEvent::AssistantDelta {
            text: " fourth".into(),
        },
    );
    let frame = tokio::time::timeout(Duration::from_secs(5), resumed.next())
        .await
        .expect("the resumed delta timed out")
        .expect("the resumed socket closed")
        .unwrap();
    let WsMessage::Text(text) = frame else {
        panic!("expected a text frame");
    };
    let value: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
    assert_eq!(value["event"]["type"], "assistant_delta");
    assert_eq!(value["event"]["text"], " fourth");
    assert!(value.get("replacement").is_none());
    assembled.push_str(value["event"]["text"].as_str().unwrap());
    assert_eq!(assembled, "first second third fourth");
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
        &tidebreak_core::OwnerId::local(),
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
    let row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
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
    let events = journaled_events(&runtime.db, parsed).await;
    let kinds: Vec<&str> = events
        .iter()
        .map(|framed| match &framed.event {
            tidebreak_core::CodeEvent::ApprovalRequested { .. } => "requested",
            tidebreak_core::CodeEvent::ApprovalResolved { .. } => "resolved",
            // Deltas stream and are never journaled; the message that states
            // the same text is what the turn leaves behind (record 57).
            tidebreak_core::CodeEvent::AssistantMessage { .. } => "message",
            tidebreak_core::CodeEvent::TurnCompleted { .. } => "completed",
            _ => "other",
        })
        .collect();
    assert!(kinds.contains(&"requested"));
    assert!(kinds.contains(&"resolved"));
    assert!(kinds.contains(&"message"));
    assert!(kinds.contains(&"completed"));
    assert!(events.iter().any(|framed| matches!(
        &framed.event,
        tidebreak_core::CodeEvent::AssistantMessage { text, .. } if text == "after the decision"
    )));
    let requested = kinds.iter().position(|k| *k == "requested").unwrap();
    let message = kinds.iter().position(|k| *k == "message").unwrap();
    let completed = kinds.iter().position(|k| *k == "completed").unwrap();
    assert!(requested < message);
    assert!(message < completed);
}

#[tokio::test]
async fn concurrent_approval_decisions_deliver_exactly_once() {
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
    let approval_id = approval["id"].as_str().unwrap().to_owned();
    let decide = |decision: &'static str| {
        let client = client.clone();
        let token = token.clone();
        let approval_id = approval_id.clone();
        async move {
            client
                .post(format!(
                    "http://{addr}/code/approvals/{approval_id}/decision"
                ))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "decision": decision }))
                .send()
                .await
                .unwrap()
        }
    };
    let (approve, deny) = tokio::join!(decide("approve"), decide("deny"));
    let statuses = [approve.status(), deny.status()];
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == reqwest::StatusCode::OK)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == reqwest::StatusCode::CONFLICT)
            .count(),
        1
    );
    assert_eq!(observed.observed_decisions().len(), 1);

    let finished = tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .expect("the winning decision did not release the turn")
        .unwrap();
    assert_eq!(finished.status(), reqwest::StatusCode::ACCEPTED);
    let rows = approvals_for(
        &client,
        addr,
        &token,
        session_id.parse::<CodeSessionId>().unwrap(),
    )
    .await;
    assert_eq!(rows.len(), 1);
    assert!(matches!(
        rows[0]["state"].as_str(),
        Some("approved" | "denied")
    ));
    let resolutions =
        journaled_resolutions(&runtime.db, session_id.parse::<CodeSessionId>().unwrap()).await;
    assert_eq!(resolutions.len(), 1);
}

#[tokio::test]
async fn a_definite_native_approval_delivery_failure_is_abandoned() {
    let adapter = ScriptedAdapter::new(approval_script())
        .with_approvals(CapLevel::Supported)
        .with_approval_delivery_error("the native waiter closed");
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

    let refused = client
        .post(format!(
            "http://{addr}/code/approvals/{}/decision",
            approval["id"].as_str().unwrap()
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "decision": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "approval_delivery_failed");
    assert!(observed.observed_decisions().is_empty());

    let parsed: CodeSessionId = session_id.parse().unwrap();
    let rows = approvals_for(&client, addr, &token, parsed).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["state"], "abandoned");
    assert_eq!(
        journaled_resolutions(&runtime.db, parsed).await,
        vec![tidebreak_core::ApprovalDecisionKind::Abandoned]
    );

    let interrupted = client
        .post(format!(
            "http://{addr}/code/sessions/{session_id}/interrupt"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(interrupted.status(), reqwest::StatusCode::ACCEPTED);
    let _ = tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .expect("interrupt did not release the failed approval turn");
}

#[tokio::test]
async fn shutdown_waits_for_an_accepted_approval_to_finish_durably() {
    let adapter = ScriptedAdapter::new(approval_script())
        .with_approvals(CapLevel::Supported)
        .with_approval_ack_delay(Duration::from_millis(500));
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
    let approval_id = approval["id"].as_str().unwrap().to_owned();

    let decision = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        let approval_id = approval_id.clone();
        async move {
            client
                .post(format!(
                    "http://{addr}/code/approvals/{approval_id}/decision"
                ))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "decision": "approve" }))
                .send()
                .await
                .unwrap()
        }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while observed.observed_decisions().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the scripted engine never accepted the approval");

    let archive = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        let workspace_id = json_id(&workspace).to_owned();
        async move {
            client
                .post(format!(
                    "http://{addr}/code/workspaces/{workspace_id}/archive"
                ))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "force": true }))
                .send()
                .await
                .unwrap()
        }
    });

    let decided = tokio::time::timeout(Duration::from_secs(3), decision)
        .await
        .expect("the delayed approval acknowledgement never completed")
        .unwrap();
    assert_eq!(decided.status(), reqwest::StatusCode::OK);
    let archived = tokio::time::timeout(Duration::from_secs(5), archive)
        .await
        .expect("archive did not resume after approval finalization")
        .unwrap();
    assert_eq!(archived.status(), reqwest::StatusCode::OK);

    let parsed: CodeSessionId = session_id.parse().unwrap();
    let rows = approvals_for(&client, addr, &token, parsed).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["state"], "approved");
    assert_eq!(
        journaled_resolutions(&runtime.db, parsed).await,
        vec![tidebreak_core::ApprovalDecisionKind::Approve]
    );
    let _ = tokio::time::timeout(Duration::from_secs(5), turn).await;
}

#[tokio::test]
async fn an_unknown_approval_delivery_stays_claimed_until_restart_recovery() {
    let adapter = ScriptedAdapter::new(approval_script())
        .with_approvals(CapLevel::Supported)
        .with_approval_ack_delay(Duration::from_secs(2));
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
    let approval_id = approval["id"]
        .as_str()
        .unwrap()
        .parse::<tidebreak_core::CodeApprovalId>()
        .unwrap();

    let unknown = client
        .post(format!(
            "http://{addr}/code/approvals/{approval_id}/decision"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "decision": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = unknown.json().await.unwrap();
    assert_eq!(body["kind"], "approval_delivery_unknown");
    assert_eq!(
        observed.observed_decisions().len(),
        1,
        "the timeout is ambiguous because the native engine accepted the decision"
    );

    let claimed = tidebreak_core::db::code::get_approval(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        approval_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(claimed.state, tidebreak_core::CodeApprovalState::Pending);
    assert!(claimed.decision_claim.is_some());
    assert!(claimed.decided_at.is_none());

    let finished = tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .expect("the turn did not resume after the decision timeout")
        .unwrap();
    assert_eq!(finished.status(), reqwest::StatusCode::ACCEPTED);

    let restarted = Arc::new(CodeRuntime::with_registry(
        runtime.db.clone(),
        dir.path().to_path_buf(),
        {
            let mut registry = AdapterRegistry::new();
            registry.register(Arc::new(
                ScriptedAdapter::new(plain_text_script()).with_approvals(CapLevel::Supported),
            ));
            registry
        },
    ));
    restarted.recover().await.unwrap();

    let recovered = tidebreak_core::db::code::get_approval(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        approval_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        recovered.state,
        tidebreak_core::CodeApprovalState::Abandoned
    );
    assert!(recovered.decision_claim.is_none());
    assert!(recovered.decided_at.is_some());
    assert_eq!(
        journaled_resolutions(&runtime.db, session_id.parse::<CodeSessionId>().unwrap()).await,
        vec![tidebreak_core::ApprovalDecisionKind::Abandoned]
    );
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
                let parsed: CodeSessionId = json_id(&session).parse().unwrap();
                let session = tidebreak_core::db::code::get_session(
                    &runtime.db,
                    &tidebreak_core::OwnerId::local(),
                    parsed,
                )
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
    .expect("pending approval never appeared");
    assert!(approval["harness_raw_json"]
        .as_str()
        .unwrap_or("")
        .contains("Write"));

    let parsed: CodeSessionId = json_id(&session).parse().unwrap();
    let row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
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
async fn restart_abandons_an_approval_whose_native_waiter_was_lost() {
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
    assert!(pending.is_empty());

    let rows = reqwest::Client::new()
        .get(format!(
            "http://{addr2}/code/approvals?session_id={session_id}"
        ))
        .bearer_auth(&token2)
        .send()
        .await
        .unwrap()
        .json::<Vec<serde_json::Value>>()
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], approval["id"]);
    assert_eq!(rows[0]["state"], "abandoned");

    let parsed: CodeSessionId = session_id.parse().unwrap();
    let row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!matches!(
        row.attention.state,
        AttentionState::NeedsYou {
            source: AttentionSource::Structured,
            ..
        }
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
    assert_eq!(decided.status(), reqwest::StatusCode::CONFLICT);
    assert!(observed.observed_decisions().is_empty());
}

/// The bug this fixes: an engine that times its own tool call out leaves the
/// approval row `pending` forever, so `code approvals` lists a request nobody
/// can act on and `code approve` reports success for a command that never ran.
/// The completion carries the `tool_use_id` the approval is parked on, which
/// is the join.
#[tokio::test]
async fn an_approval_is_abandoned_when_its_tool_call_resolves_undecided() {
    let adapter = ScriptedAdapter::new(timed_out_approval_script())
        .with_approvals(CapLevel::Supported)
        .with_unattended_approvals();
    let (client, addr, token, session_id, runtime, _dir) = ran_one_ask_turn(adapter).await;

    let rows = approvals_for(&client, addr, &token, session_id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["state"], "abandoned");
    assert!(
        rows[0]["decided_at"].is_string(),
        "an abandoned approval is settled, so it carries a decided_at"
    );

    let pending: Vec<serde_json::Value> = client
        .get(format!("http://{addr}/code/approvals?state=pending"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        pending.is_empty(),
        "a settled approval must leave the pending list"
    );

    assert_eq!(
        journaled_resolutions(&runtime.db, session_id).await,
        vec![tidebreak_core::ApprovalDecisionKind::Abandoned],
        "the journal is how the CLI and the UI learn the request went undecided"
    );
}

#[tokio::test]
async fn a_stale_worker_completion_cannot_abandon_a_reused_call_id() {
    let adapter = ScriptedAdapter::new(plain_text_script()).with_approvals(CapLevel::Supported);
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
    let session = session.json::<serde_json::Value>().await.unwrap();
    let session_id = json_id(&session).parse::<CodeSessionId>().unwrap();
    let turn = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "finish" }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let turn_id = json_id(&turn).parse::<CodeTurnId>().unwrap();
    let row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        session_id,
    )
    .await
    .unwrap()
    .unwrap();
    let stale_epoch = row.spawn_epoch - 1;
    let stale_id = tidebreak_core::CodeApprovalId::new();
    let current_id = tidebreak_core::CodeApprovalId::new();
    for (id, worker_epoch) in [(stale_id, stale_epoch), (current_id, row.spawn_epoch)] {
        tidebreak_core::db::code::insert_approval(
            &runtime.db,
            &row.owner,
            &tidebreak_core::CodeApproval {
                id,
                session_id,
                turn_id,
                kind: tidebreak_core::CodeApprovalKind::Other {
                    summary: "run command".into(),
                },
                harness_raw: serde_json::json!({"call_id":"toolu_reused"}),
                native_call_id: Some("toolu_reused".into()),
                server_capability: None,
                request_sha256: None,
                worker_epoch: Some(worker_epoch),
                decision_claim: None,
                claimed_at: None,
                state: tidebreak_core::CodeApprovalState::Pending,
                feedback: None,
                requested_at: chrono::Utc::now(),
                decided_at: None,
            },
        )
        .await
        .unwrap();
    }

    crate::code::approval_sweep::abandon_for_call(
        &runtime.db,
        &runtime.bus,
        &row.owner,
        session_id,
        stale_epoch,
        "toolu_reused",
    )
    .await;

    assert_eq!(
        tidebreak_core::db::code::get_approval(&runtime.db, &row.owner, stale_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        tidebreak_core::CodeApprovalState::Abandoned
    );
    assert_eq!(
        tidebreak_core::db::code::get_approval(&runtime.db, &row.owner, current_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        tidebreak_core::CodeApprovalState::Pending,
        "a stale completion must not settle the replacement worker's approval"
    );
}

/// Deciding a settled approval must fail out loud. A missing native waiter is
/// a delivery failure, so the durable row prevents a later false success.
#[tokio::test]
async fn deciding_an_abandoned_approval_is_refused() {
    let adapter = ScriptedAdapter::new(timed_out_approval_script())
        .with_approvals(CapLevel::Supported)
        .with_unattended_approvals();
    let observed = adapter.clone();
    let (client, addr, token, session_id, runtime, _dir) = ran_one_ask_turn(adapter).await;

    let rows = approvals_for(&client, addr, &token, session_id).await;
    let approval_id = rows[0]["id"].as_str().unwrap().to_owned();
    let refused = client
        .post(format!(
            "http://{addr}/code/approvals/{approval_id}/decision"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "decision": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert!(
        body.to_string().contains("no longer awaiting a decision"),
        "the refusal must say why, got {body}"
    );

    let rows = approvals_for(&client, addr, &token, session_id).await;
    assert_eq!(rows[0]["state"], "abandoned");
    assert_eq!(
        journaled_resolutions(&runtime.db, session_id).await,
        vec![tidebreak_core::ApprovalDecisionKind::Abandoned],
        "a refused decision journals nothing"
    );
    assert!(
        observed.observed_decisions().is_empty(),
        "a refused decision never reaches the engine"
    );
}

/// A tool call the engine drops without reporting must not leave a row pending
/// forever either. The turn boundary is the backstop.
#[tokio::test]
async fn a_turn_that_ends_without_a_tool_result_abandons_its_approval() {
    let adapter = ScriptedAdapter::new(dropped_approval_script())
        .with_approvals(CapLevel::Supported)
        .with_unattended_approvals();
    let (client, addr, token, session_id, runtime, _dir) = ran_one_ask_turn(adapter).await;

    let rows = approvals_for(&client, addr, &token, session_id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["state"], "abandoned");
    assert!(rows[0]["decided_at"].is_string());
    assert_eq!(
        journaled_resolutions(&runtime.db, session_id).await,
        vec![tidebreak_core::ApprovalDecisionKind::Abandoned]
    );

    let session = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        session_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        !matches!(session.attention.state, AttentionState::NeedsYou { .. }),
        "nothing is waiting on the user once the request is settled"
    );
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
                let session = tidebreak_core::db::code::get_session(
                    &runtime.db,
                    &tidebreak_core::OwnerId::local(),
                    parsed,
                )
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

    let after_decision = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
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
    let row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
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
            let row = tidebreak_core::db::code::get_session(
                &runtime.db,
                &tidebreak_core::OwnerId::local(),
                parsed,
            )
            .await
            .unwrap()
            .unwrap();
            // Idle, not Working. Viewing settles the session; it does not
            // start an engine. This asserted `Working` while `Idle` did not
            // exist, which is how the derivation came to claim a finished
            // session was busy.
            if row.attention.state == AttentionState::Idle {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("viewing the session must settle it to Idle, not report it Working");
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
    let mut row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
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
    let row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
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
    let mut row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
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
    let row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
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
    let mut row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
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

#[tokio::test]
async fn attachments_are_accepted_and_journaled_when_the_adapter_declares_support() {
    let adapter = ScriptedAdapter::new(plain_text_script()).with_image_input(CapLevel::Supported);
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
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(session.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = session.json().await.unwrap();

    let pixels = one_pixel_png();
    let blob_id = publish_one_pixel_png(&client, addr, &token, json_id(&session)).await;

    let accepted = client
        .post(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "message": "look at this",
            "attachments": [{
                "blob_id": blob_id,
                "media_type": "image/png",
            }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), reqwest::StatusCode::ACCEPTED);
    let turn: serde_json::Value = accepted.json().await.unwrap();
    assert_eq!(turn["attachments"][0]["blob_id"], blob_id);
    assert_eq!(turn["attachments"][0]["media_type"], "png");
    assert_eq!(turn["attachments"][0]["byte_len"], pixels.len() as u64);
    assert!(
        turn["attachments"][0].get("bytes").is_none(),
        "journaled attachment must stay a bounded reference: {turn}"
    );

    let listed = client
        .get(format!(
            "http://{addr}/code/sessions/{}/turns",
            json_id(&session)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(listed[0]["attachments"][0]["blob_id"], blob_id);
    assert_eq!(listed[0]["attachments"][0]["byte_len"], pixels.len() as u64);
}

#[tokio::test]
async fn workspace_tree_is_bounded_ignores_and_never_returns_contents() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    std::fs::write(repo.join(".gitignore"), "secret.bin\n").unwrap();
    std::fs::write(repo.join("src.rs"), "fn main() {}\n").unwrap();
    std::fs::write(repo.join("secret.bin"), "UNIQUE_PAYLOAD_xyz\n").unwrap();
    std::fs::write(repo.join("notes.md"), "UNIQUE_PAYLOAD_xyz\n").unwrap();
    assert!(std::process::Command::new("git")
        .args(["add", ".gitignore", "src.rs"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    assert!(std::process::Command::new("git")
        .args(["commit", "-m", "more"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let worktree = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    std::fs::write(worktree.join(".gitignore"), "secret.bin\n").unwrap();
    std::fs::write(worktree.join("src.rs"), "fn main() {}\n").unwrap();
    std::fs::write(worktree.join("secret.bin"), "UNIQUE_PAYLOAD_xyz\n").unwrap();
    std::fs::write(worktree.join("notes.md"), "UNIQUE_PAYLOAD_xyz\n").unwrap();
    for index in 0..80 {
        std::fs::write(worktree.join(format!("bulk-{index:03}.txt")), "x\n").unwrap();
    }

    let listed = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/tree?query=bulk-&limit=50",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = listed.json().await.unwrap();
    assert_eq!(body["paths"].as_array().unwrap().len(), 50);
    assert_eq!(body["truncated"], true);
    let rendered = body.to_string();
    assert!(
        !rendered.contains("UNIQUE_PAYLOAD_xyz"),
        "tree route leaked file contents: {rendered}"
    );
    assert!(!rendered.contains("secret.bin"));

    let named = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/tree?query=notes",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(named["paths"][0], "notes.md");

    let searched = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/search",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .query(&[
            ("query", "unique_payload_XYZ"),
            ("include", "*.md"),
            ("limit", "50"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(searched.status(), reqwest::StatusCode::OK);
    let searched: serde_json::Value = searched.json().await.unwrap();
    assert_eq!(searched["truncated"], false);
    assert_eq!(searched["matches"].as_array().unwrap().len(), 1);
    assert_eq!(searched["matches"][0]["path"], "notes.md");
    assert_eq!(searched["matches"][0]["line_number"], 1);
    assert_eq!(searched["matches"][0]["line"], "UNIQUE_PAYLOAD_xyz");
    assert!(searched.to_string().find("secret.bin").is_none());

    let bounded_search = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/search",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .query(&[("query", "x"), ("include", "bulk-*.txt"), ("limit", "1")])
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(bounded_search["matches"].as_array().unwrap().len(), 1);
    assert_eq!(bounded_search["truncated"], true);
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

/// A self-host code app with two principals: alice is an admin, bob a member.
async fn two_user_code_app() -> (Router, tempfile::TempDir, std::path::PathBuf) {
    let (dir, store) = temp_db_store("code-two-user.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let tokens_file = dir.path().join("tokens");
    std::fs::write(
        &tokens_file,
        format!("alice {ALICE_TOKEN} admin\nbob {BOB_TOKEN}\n"),
    )
    .unwrap();
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(ScriptedAdapter::new(plain_text_script())));
    let runtime = Arc::new(CodeRuntime::with_registry(
        db,
        dir.path().to_path_buf(),
        registry,
    ));
    let mut config = Config::desktop(dir.path());
    config.profile = tidebreak_core::Profile::SelfHost;
    config.auth_tokens_file = Some(tokens_file);
    let mut state = AppState::new(
        config,
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
    let repo = init_git_repo(dir.path());
    (app(state), dir, repo)
}

/// Decision 47's validation for decision point 4, through real routes: a
/// second user on a shared machine sees neither another owner's `code_*` rows
/// nor their session events on the updates channel.
///
/// The plausible wrong implementation scopes the reads and leaves the live
/// channel install-wide, so the digest half of this test is the load-bearing
/// half.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_user_sees_neither_code_rows_nor_updates_of_another_owner() {
    let (router, _dir, repo) = two_user_code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();

    let (alice_repo, alice_workspace) =
        register_and_workspace(&client, addr, ALICE_TOKEN, &repo).await;
    let alice_session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&alice_workspace)
        ))
        .bearer_auth(ALICE_TOKEN)
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
    let alice_session_id = json_id(&alice_session).to_owned();

    // Bob's listings are empty, not filtered copies of Alice's.
    for path in ["/code/repos", "/code/workspaces", "/code/approvals"] {
        let listed = client
            .get(format!("http://{addr}{path}"))
            .bearer_auth(BOB_TOKEN)
            .send()
            .await
            .unwrap()
            .json::<Vec<serde_json::Value>>()
            .await
            .unwrap();
        assert!(
            listed.is_empty(),
            "{path} carried another owner's rows to a second user"
        );
    }

    // Every by-id read answers as if the row does not exist, which is what
    // keeps the reply from confirming that it does.
    for path in [
        format!("/code/repos/{}", json_id(&alice_repo)),
        format!("/code/workspaces/{}", json_id(&alice_workspace)),
        format!("/code/workspaces/{}/sessions", json_id(&alice_workspace)),
        format!("/code/workspaces/{}/files", json_id(&alice_workspace)),
        format!("/code/workspaces/{}/diff", json_id(&alice_workspace)),
        format!("/code/workspaces/{}/tree", json_id(&alice_workspace)),
        format!("/code/workspaces/{}/terminals", json_id(&alice_workspace)),
        format!("/code/sessions/{alice_session_id}/turns"),
        format!("/code/sessions/{alice_session_id}/debug"),
    ] {
        let response = client
            .get(format!("http://{addr}{path}"))
            .bearer_auth(BOB_TOKEN)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::NOT_FOUND,
            "{path} answered a second user about another owner's row"
        );
    }

    // Writes are refused the same way, so a guessed id is not a way in.
    let submitted = client
        .post(format!(
            "http://{addr}/code/sessions/{alice_session_id}/turns"
        ))
        .bearer_auth(BOB_TOKEN)
        .json(&serde_json::json!({ "message": "whose session is this" }))
        .send()
        .await
        .unwrap();
    assert_eq!(submitted.status(), reqwest::StatusCode::NOT_FOUND);
    let interrupted = client
        .post(format!(
            "http://{addr}/code/sessions/{alice_session_id}/interrupt"
        ))
        .bearer_auth(BOB_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(interrupted.status(), reqwest::StatusCode::NOT_FOUND);
    let steered = client
        .post(format!(
            "http://{addr}/code/sessions/{alice_session_id}/steer"
        ))
        .bearer_auth(BOB_TOKEN)
        .json(&serde_json::json!({
            "expected_turn_id": CodeTurnId::new(),
            "guidance": "whose session is this",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(steered.status(), reqwest::StatusCode::NOT_FOUND);

    // The updates channel. Bob's connect snapshot is empty even though Alice
    // has a live session, and Alice's turn produces no notice on his socket.
    let mut request = format!("ws://{addr}/code/updates")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {BOB_TOKEN}").parse().unwrap(),
    );
    let (mut bob_socket, _) = connect_async(request).await.unwrap();
    let snapshot = next_json(&mut bob_socket).await;
    assert_eq!(snapshot["type"], "snapshot");
    assert!(
        snapshot["sessions"]
            .as_array()
            .expect("snapshot sessions")
            .is_empty(),
        "the connect snapshot restated another owner's sessions"
    );

    let mut request = format!("ws://{addr}/code/updates")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {ALICE_TOKEN}").parse().unwrap(),
    );
    let (mut alice_socket, _) = connect_async(request).await.unwrap();
    let alice_snapshot = next_json(&mut alice_socket).await;
    assert_eq!(
        alice_snapshot["sessions"]
            .as_array()
            .expect("alice sessions")
            .len(),
        1,
        "the owner still sees her own session"
    );

    let _ = client
        .post(format!(
            "http://{addr}/code/sessions/{alice_session_id}/turns"
        ))
        .bearer_auth(ALICE_TOKEN)
        .json(&serde_json::json!({ "message": "hi" }))
        .send()
        .await
        .unwrap();

    // Alice's socket carries the turn; Bob's stays silent for as long again.
    let mut saw_turn = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        let Some(notice) =
            tokio::time::timeout(Duration::from_millis(500), next_json(&mut alice_socket))
                .await
                .ok()
        else {
            continue;
        };
        if notice["type"] == "digest" && notice["turn_count"] == 1 {
            saw_turn = true;
            break;
        }
    }
    assert!(saw_turn, "the owner's own digest must still arrive");
    assert!(
        tokio::time::timeout(Duration::from_secs(1), next_json(&mut bob_socket))
            .await
            .is_err(),
        "a second user received a notice for another owner's session"
    );
}

/// Deployment-plane code routes are admin-gated by where they are registered
/// (decision 6). A member is refused; an admin is not.
#[tokio::test(flavor = "multi_thread")]
async fn code_deployment_plane_routes_refuse_a_member() {
    let (router, _dir, _repo) = two_user_code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();

    let member = client
        .get(format!("http://{addr}/code/repos/clone-defaults"))
        .bearer_auth(BOB_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(member.status(), reqwest::StatusCode::FORBIDDEN);
    let admin = client
        .get(format!("http://{addr}/code/repos/clone-defaults"))
        .bearer_auth(ALICE_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(admin.status(), reqwest::StatusCode::OK);

    let member_root = client
        .get(format!("http://{addr}/code/worktree-root"))
        .bearer_auth(BOB_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(member_root.status(), reqwest::StatusCode::FORBIDDEN);
    let admin_root = client
        .get(format!("http://{addr}/code/worktree-root"))
        .bearer_auth(ALICE_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(admin_root.status(), reqwest::StatusCode::OK);

    let member_refresh = client
        .post(format!("http://{addr}/code/harnesses/refresh"))
        .bearer_auth(BOB_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(member_refresh.status(), reqwest::StatusCode::FORBIDDEN);

    // The member plane is untouched: the doctor read still answers a member.
    let doctor = client
        .get(format!("http://{addr}/code/harnesses"))
        .bearer_auth(BOB_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(doctor.status(), reqwest::StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_self_host_member_cannot_use_ambient_github_credentials_for_unowned_targets() {
    let (router, _dir, _repo) = two_user_code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repository = serde_json::json!({
        "host": "github.com",
        "owner": "private-org",
        "name": "deployment-only",
    });
    let requests = [
        (
            "/code/delivery/repositories/resolve",
            serde_json::json!({"repositories": ["private-org/deployment-only"]}),
        ),
        (
            "/code/delivery/pull-requests/query",
            serde_json::json!({"repositories": [repository.clone()]}),
        ),
        (
            "/code/delivery/pull-requests/detail",
            serde_json::json!({"repository": repository.clone(), "number": 1}),
        ),
        (
            "/code/delivery/pull-requests/action",
            serde_json::json!({
                "target": {"repository": repository.clone(), "number": 1},
                "action": {"type": "close"},
            }),
        ),
        (
            "/code/delivery/runs/query",
            serde_json::json!({"repositories": [repository.clone()]}),
        ),
        (
            "/code/delivery/runs/detail",
            serde_json::json!({
                "repository": repository.clone(),
                "kind": "workflow_run",
                "id": 1,
            }),
        ),
        (
            "/code/delivery/runs/action",
            serde_json::json!({
                "target": {
                    "repository": repository.clone(),
                    "kind": "workflow_run",
                    "id": 1,
                },
                "action": {"type": "rerun_failed"},
            }),
        ),
    ];

    for (path, body) in requests {
        let response = client
            .post(format!("http://{addr}{path}"))
            .bearer_auth(BOB_TOKEN)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::NOT_FOUND,
            "{path} let a member target a repository outside their registered catalog"
        );
    }
}

/// Two users cloning the same remote must not collide on disk. The clone
/// parent directory is one shared setting, so the owner segment is what keeps
/// the second clone from landing on the first one's checkout.
#[test]
fn clone_targets_are_keyed_by_owner() {
    use crate::code::clone::{legacy_owner_dir, owner_dir};
    let parent = std::path::Path::new("/srv/checkouts");
    // The local profile is single-user and keeps the paths people already have.
    assert_eq!(
        owner_dir(parent, &tidebreak_core::OwnerId::local()),
        parent.to_path_buf()
    );
    let alice = owner_dir(parent, &tidebreak_core::OwnerId::new("alice").unwrap());
    let bob = owner_dir(parent, &tidebreak_core::OwnerId::new("bob").unwrap());
    assert_ne!(alice, bob);
    assert_eq!(alice.parent(), Some(parent));
    assert!(alice.starts_with(parent));
    // The compatibility path remains available for identifying existing rows,
    // but new paths never use it.
    assert_eq!(
        legacy_owner_dir(parent, &tidebreak_core::OwnerId::new("alice").unwrap()),
        parent.join("alice")
    );
    let hostile = owner_dir(
        parent,
        &tidebreak_core::OwnerId::new("../../etc/passwd").unwrap(),
    );
    assert_eq!(hostile.parent(), Some(parent));
    assert!(hostile.starts_with(parent));
    let dots = owner_dir(parent, &tidebreak_core::OwnerId::new("..").unwrap());
    assert_eq!(dots.parent(), Some(parent));
    assert_ne!(dots, parent);
}

/// Rows created with the legacy owner directory keep their exact absolute
/// paths. The new namespace applies only to later clones and worktrees.
#[tokio::test]
async fn legacy_managed_repo_and_worktree_paths_remain_accessible() {
    use crate::code::clone::{legacy_owner_dir, owner_dir, registered_legacy_clone_target};
    use crate::code::worktree::create_worktree;

    let (dir, store) = temp_db_store("code-owner-path-compat.db").await;
    let db = Arc::new(store);
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(ScriptedAdapter::new(plain_text_script())));
    let runtime = CodeRuntime::with_registry(db.clone(), dir.path().to_path_buf(), registry);
    let owner = tidebreak_core::OwnerId::new("user:alice@example").unwrap();

    let clone_parent = dir.path().join("clones");
    let legacy_repo_root = legacy_owner_dir(&clone_parent, &owner).join("demo");
    let repo_root = init_git_repo_named(legacy_repo_root.parent().unwrap(), "demo");
    let repo = CodeRepo {
        id: RepoId::new(),
        owner: owner.clone(),
        root_path: repo_root.canonicalize().unwrap().display().to_string(),
        display_name: "demo".to_owned(),
        default_base_ref: "main".to_owned(),
        branch_prefix: "thet/".to_owned(),
        setup_script: None,
        archive_script: None,
        quick_actions: Vec::new(),
        created_at: chrono::Utc::now(),
        removed_at: None,
        cloned_from: Some("https://example.com/acme/demo.git".to_owned()),
        origin_host: None,
        origin_owner: None,
        origin_name: None,
    };
    tidebreak_core::db::code::insert_repo(&db, &repo)
        .await
        .unwrap();
    assert!(registered_legacy_clone_target(&db, &owner, &repo_root)
        .await
        .unwrap());
    let colliding_owner = tidebreak_core::OwnerId::new("user:alice_example").unwrap();
    assert_eq!(
        legacy_owner_dir(&clone_parent, &colliding_owner),
        legacy_repo_root.parent().unwrap()
    );
    assert!(
        !registered_legacy_clone_target(&db, &colliding_owner, &repo_root)
            .await
            .unwrap()
    );

    let legacy_worktree_root = legacy_owner_dir(&runtime.default_worktree_root(), &owner);
    let workspace_id = WorkspaceId::new();
    let worktree_path =
        legacy_worktree_root.join(format!("demo-{}", &workspace_id.to_string()[..8]));
    create_worktree(&repo_root, &worktree_path, "thet/legacy-owner-path", "main")
        .await
        .unwrap();
    let workspace = CodeWorkspace {
        id: workspace_id,
        owner: owner.clone(),
        repo_id: repo.id,
        title: "Legacy owner path".to_owned(),
        worktree_path: worktree_path.display().to_string(),
        branch_name: "thet/legacy-owner-path".to_owned(),
        base_ref: "main".to_owned(),
        status: CodeWorkspaceStatus::Active,
        pr: None,
        created_at: chrono::Utc::now(),
        archived_at: None,
        released_at: None,
        released_tip: None,
        bundle_bytes: None,
    };
    tidebreak_core::db::code::insert_workspace(&db, &workspace)
        .await
        .unwrap();

    assert_ne!(
        owner_dir(&clone_parent, &owner),
        legacy_repo_root.parent().unwrap()
    );
    let new_worktree_root = runtime.owner_worktree_root(&owner).await.unwrap();
    assert_ne!(new_worktree_root, legacy_worktree_root);
    let reread_repo = runtime.get_repo(&owner, repo.id).await.unwrap();
    assert_eq!(reread_repo.root_path, repo.root_path);
    let next_workspace = runtime
        .create_workspace(
            &owner,
            repo.id,
            Some("New owner namespace".to_owned()),
            None,
        )
        .await
        .unwrap();
    assert!(std::path::Path::new(&next_workspace.worktree_path).starts_with(new_worktree_root));
    let reread_workspace = runtime.get_workspace(&owner, workspace_id).await.unwrap();
    assert_eq!(reread_workspace.worktree_path, workspace.worktree_path);
    let (paths, truncated) = runtime
        .workspace_tree(&owner, workspace_id, "README", Some(10))
        .await
        .unwrap();
    assert_eq!(paths, vec!["README.md"]);
    assert!(!truncated);
}

/// The `/code/*` routes reach the store only through the owner-scoped view.
///
/// Decision 6 puts enforcement in the router rather than in handler habits,
/// and decision 48 step 1 applies that to data scoping: a new code route is
/// owner-scoped because of what it extracts, not because its author
/// remembered to filter. This check is the tripwire for the two ways back
/// out — an unscoped runtime gate, or a system-path store function called
/// from a request path.
#[test]
fn code_routes_go_through_the_owner_scoped_view() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/routes/code");
    let mut findings = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("code routes directory") {
        let path = entry.expect("code routes entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        let text = std::fs::read_to_string(&path).expect("code route file");
        // A system-path store function is never a request path.
        if text.contains("_all_owners") {
            findings.push(format!(
                "{name} calls an `_all_owners` store function; those are system \
                 paths (boot recovery, the stall sweep), not request paths"
            ));
        }
        // The pre-scoping gate answered "is code mode configured", never
        // "whose row is this". `ScopedCode` answers both.
        if text.contains("require_code(") {
            findings.push(format!(
                "{name} uses an unscoped code gate; extract `ScopedCode` instead"
            ));
        }
        // Files that serve code data extract the scoped view. `mod.rs` and
        // `types.rs` declare and shape, and serve nothing.
        let serves_data = text.contains("pub async fn") && name != "mod.rs";
        if serves_data && !text.contains("ScopedCode") {
            // Browser and harness inference are capability-bearer routes.
            // Each resolves a session-private token to the owning session
            // instead of accepting the app-token `ScopedCode` extractor.
            // Require each route's own authorization path here.
            if name == "browser.rs" {
                if !text.contains("fn authorize(")
                    || !text.contains("BrowserSubject")
                    || !text.contains("bearer_token")
                {
                    findings.push(format!(
                        "{name} is the capability-bearer browser route but is \
                         missing its `authorize` / `BrowserSubject` / \
                        `bearer_token` authorization path"
                    ));
                }
            } else if name == "llm.rs" {
                if !text.contains("HarnessLlmRelay")
                    || !text.contains("HeaderMap")
                    || !text.contains("relay.forward(")
                {
                    findings.push(format!(
                        "{name} is the capability-bearer harness inference \
                         route but is missing its `HarnessLlmRelay` / \
                         `HeaderMap` / `relay.forward` authorization path"
                    ));
                }
            } else {
                findings.push(format!(
                    "{name} defines route handlers but never extracts `ScopedCode`"
                ));
            }
        }
    }
    assert!(
        findings.is_empty(),
        "code routes must query through the owner-scoped view: {findings:?}"
    );
}

/// Archive keeps the branch; restore puts a checkout back under the same
/// workspace row. Committed work returns, force-discarded work stays gone,
/// and restoring an already-active workspace is a no-op.
#[tokio::test]
async fn restore_reactivates_an_archived_workspace_on_its_own_branch() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let id = json_id(&workspace);
    let path = workspace["worktree_path"].as_str().unwrap().to_owned();
    let branch = workspace["branch_name"].as_str().unwrap().to_owned();

    // One committed file (should survive on the branch) and one uncommitted
    // (force-archive discards it).
    std::fs::write(std::path::Path::new(&path).join("kept.txt"), "kept\n").unwrap();
    for args in [
        ["add", "kept.txt"].as_slice(),
        ["commit", "-m", "keep this"].as_slice(),
    ] {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(&path)
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(std::path::Path::new(&path).join("scratch.txt"), "gone\n").unwrap();

    let archived = client
        .post(format!("http://{addr}/code/workspaces/{id}/archive"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "force": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(archived.status(), reqwest::StatusCode::OK);
    assert!(!std::path::Path::new(&path).exists());

    let restored = client
        .post(format!("http://{addr}/code/workspaces/{id}/restore"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(restored.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = restored.json().await.unwrap();
    assert_eq!(body["status"], "active");
    assert_eq!(body["worktree_path"], path.as_str());
    assert_eq!(body["branch_name"], branch.as_str());
    assert!(body.get("archived_at").is_none() || body["archived_at"].is_null());
    let kept = std::fs::read_to_string(std::path::Path::new(&path).join("kept.txt")).unwrap();
    assert_eq!(
        kept.replace("\r\n", "\n").replace('\r', "\n"),
        "kept\n",
        "restored committed content must match regardless of platform newlines"
    );
    assert!(!std::path::Path::new(&path).join("scratch.txt").exists());

    // Idempotent on an active workspace.
    let again = client
        .post(format!("http://{addr}/code/workspaces/{id}/restore"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), reqwest::StatusCode::OK);
}

/// The two ways a restore can be impossible, each with its own kind so the
/// UI can offer the right fallback: the branch was deleted since archive, or
/// something has claimed the worktree path.
#[tokio::test]
async fn restore_refuses_a_missing_branch_and_an_occupied_path() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let id = json_id(&workspace);
    let path = workspace["worktree_path"].as_str().unwrap().to_owned();
    let branch = workspace["branch_name"].as_str().unwrap().to_owned();

    let archived = client
        .post(format!("http://{addr}/code/workspaces/{id}/archive"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(archived.status(), reqwest::StatusCode::OK);

    // Occupy the path first: the branch still exists, so this is the
    // path-specific refusal.
    std::fs::create_dir_all(&path).unwrap();
    let occupied = client
        .post(format!("http://{addr}/code/workspaces/{id}/restore"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(occupied.status(), reqwest::StatusCode::CONFLICT);
    let occupied_body: serde_json::Value = occupied.json().await.unwrap();
    assert_eq!(occupied_body["kind"], "worktree_path_occupied");
    std::fs::remove_dir_all(&path).unwrap();

    assert!(std::process::Command::new("git")
        .args(["branch", "-D", &branch])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    let missing = client
        .post(format!("http://{addr}/code/workspaces/{id}/restore"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::CONFLICT);
    let missing_body: serde_json::Value = missing.json().await.unwrap();
    assert_eq!(missing_body["kind"], "branch_missing");
}

/// The reclaim ladder end to end over HTTP: archive frees the checkout,
/// release frees the branch, restore rebuilds both from the bundle.
///
/// The assertion that matters is the last one — the file the workspace's
/// commit added is back on disk after a round trip through a branch that no
/// longer existed. Without that, release is deletion with extra steps.
#[tokio::test]
async fn release_frees_the_branch_and_restore_rebuilds_it_from_the_bundle() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let registered = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "path": repo }))
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
            "title": "released later",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let workspace: serde_json::Value = created.json().await.unwrap();
    let workspace_id = json_id(&workspace);
    let branch = workspace["branch_name"].as_str().unwrap().to_owned();
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    let repo_root = std::path::PathBuf::from(&repo);

    // Commit real work, so the bundle has something to carry.
    std::fs::write(path.join("kept.txt"), "survives release\n").unwrap();
    for args in [
        vec!["add", "kept.txt"],
        vec!["commit", "-m", "work worth keeping"],
    ] {
        let ok = std::process::Command::new("git")
            .args(&args)
            .current_dir(&path)
            .status()
            .unwrap();
        assert!(ok.success(), "git {args:?} failed");
    }

    // Release requires an archived workspace.
    let too_early = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/release"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(too_early.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = too_early.json().await.unwrap();
    assert_eq!(body["kind"], "workspace_not_archived");

    let archived = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/archive"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "force": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(archived.status(), reqwest::StatusCode::OK);
    assert!(!path.exists(), "archive removes the checkout");
    assert!(
        branch_exists_in(&repo_root, &branch),
        "archive keeps the branch"
    );

    // Unmerged commits are the case release confirms rather than assumes.
    let refused = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/release"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "branch_unmerged");
    assert!(
        branch_exists_in(&repo_root, &branch),
        "a refused release must not drop the branch"
    );

    let released = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/release"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "force": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(released.status(), reqwest::StatusCode::OK);
    let released_body: serde_json::Value = released.json().await.unwrap();
    assert_eq!(released_body["status"], "released");
    assert!(released_body["bundle_bytes"].as_i64().unwrap() > 0);
    assert!(released_body["released_tip"].as_str().is_some());
    assert!(
        !branch_exists_in(&repo_root, &branch),
        "release drops the branch"
    );

    let restored = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/restore"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        restored.status(),
        reqwest::StatusCode::OK,
        "restore from bundle failed: {}",
        restored.text().await.unwrap()
    );
    let restored_body: serde_json::Value = restored.json().await.unwrap();
    assert_eq!(restored_body["status"], "active");
    assert!(restored_body["released_at"].is_null());
    // Normalize: git checks out with CRLF under Windows' default
    // `core.autocrlf`.
    assert_eq!(
        std::fs::read_to_string(path.join("kept.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "survives release\n",
        "the released commit did not come back"
    );
}

/// Whether a branch ref exists in a repository.
fn branch_exists_in(repo_root: &std::path::Path, branch: &str) -> bool {
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

/// Reclaim deletes only a checkout Tidebreak made.
///
/// A registered repository is a directory the user already had, and the clone
/// parent is a setting that moves, so there is no path test that stays
/// honest. The recorded origin is the whole guard: without it this route is a
/// recursive delete pointed at someone's own work.
#[tokio::test]
async fn reclaim_refuses_a_checkout_tidebreak_did_not_clone() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let registered = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "path": repo }))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), reqwest::StatusCode::CREATED);
    let repo_body: serde_json::Value = registered.json().await.unwrap();
    let repo_id = json_id(&repo_body);
    let root = std::path::PathBuf::from(&repo);

    let refused = client
        .delete(format!(
            "http://{addr}/code/repos/{repo_id}?reclaim_checkout=true"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["kind"], "checkout_not_reclaimable");
    assert!(
        root.join(".git").exists(),
        "a registered checkout must survive a refused reclaim"
    );

    // The registration still goes away; only the directory is spared.
    let removed = client
        .delete(format!("http://{addr}/code/repos/{repo_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), reqwest::StatusCode::NO_CONTENT);
    assert!(
        root.join(".git").exists(),
        "removal must not delete the user's checkout"
    );
    let listed = client
        .get(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<Vec<serde_json::Value>>()
        .await
        .unwrap();
    assert!(listed.is_empty(), "a removed registration leaves the list");
}

/// A blob id is not a capability. Publication is.
///
/// The blob store is content-addressed and owner-blind, so before this an
/// attachment resolved on the strength of the bytes existing anywhere. That
/// let a session bind an id it had merely learned, and then read the pixels
/// back through its own image route. Chat has bound this with
/// `chat_image_publication` since it shipped; this is the code-mode
/// equivalent, and the assertion is that publishing to one session does not
/// authorize another.
#[tokio::test]
async fn a_live_session_image_upload_is_idempotently_published() {
    let (router, token, runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = create_sibling_sessions(&client, addr, &token, &workspace, 1)
        .await
        .remove(0);
    let pixels = one_pixel_png();
    let expected = crate::routes::image_attachment::inspect_image_bytes(&pixels).unwrap();

    for _ in 0..2 {
        let response = client
            .post(format!(
                "http://{addr}/code/sessions/{session}/attachments/images"
            ))
            .bearer_auth(&token)
            .header(reqwest::header::CONTENT_TYPE, "image/png")
            .body(pixels.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::CREATED,
            "an exact upload retry must keep its successful publication"
        );
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["attachment_id"], expected.blob_id.to_string());
    }

    let session_id: CodeSessionId = session.parse().unwrap();
    assert_eq!(
        runtime
            .db
            .get_published_code_session_image(
                &tidebreak_core::OwnerId::local(),
                session_id,
                expected.blob_id,
            )
            .await
            .unwrap(),
        Some(expected)
    );
}

#[tokio::test]
async fn an_image_upload_losing_the_session_end_race_conflicts_and_queues_retirement() {
    let gate = Arc::new(PutGate {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let (router, token, runtime, dir) =
        code_app_with_put_gate(ScriptedAdapter::new(plain_text_script()), gate.clone()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = create_sibling_sessions(&client, addr, &token, &workspace, 1)
        .await
        .remove(0);
    let session_id: CodeSessionId = session.parse().unwrap();
    let pixels = one_pixel_png();
    let image = crate::routes::image_attachment::inspect_image_bytes(&pixels).unwrap();
    let upload = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        let session = session.clone();
        async move {
            client
                .post(format!(
                    "http://{addr}/code/sessions/{session}/attachments/images"
                ))
                .bearer_auth(&token)
                .header(reqwest::header::CONTENT_TYPE, "image/png")
                .body(pixels)
                .send()
                .await
                .unwrap()
        }
    });
    gate.started.notified().await;

    let mut row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        session_id,
    )
    .await
    .unwrap()
    .unwrap();
    row.lifecycle = CodeSessionLifecycle::Ended;
    row.child_pid = None;
    assert!(tidebreak_core::db::code::save_session(&runtime.db, &row)
        .await
        .unwrap());
    gate.release.notify_one();

    let response = upload.await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["kind"], "session_ended");
    assert!(runtime
        .db
        .get_published_code_session_image(
            &tidebreak_core::OwnerId::local(),
            session_id,
            image.blob_id,
        )
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        runtime
            .db
            .get_blob_retirement(image.blob_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        tidebreak_core::BlobRetirementStatus::Queued
    );
}

#[tokio::test]
async fn a_session_cannot_attach_an_image_published_to_another_session() {
    // The capability gate refuses attachments before authority is consulted,
    // so an engine that declares image input is what puts this test on the
    // path it means to exercise.
    let adapter = ScriptedAdapter::new(plain_text_script()).with_image_input(CapLevel::Supported);
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;

    let start = |workspace_id: String| {
        let client = client.clone();
        let token = token.clone();
        async move {
            let response = client
                .post(format!(
                    "http://{addr}/code/workspaces/{workspace_id}/sessions"
                ))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "harness": "claude_code",
                    "permission_mode": "plan",
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::CREATED);
            let body: serde_json::Value = response.json().await.unwrap();
            json_id(&body).to_owned()
        }
    };
    let owning = start(json_id(&workspace).to_owned()).await;
    let other = start(json_id(&workspace).to_owned()).await;

    // A 1x1 PNG, published to the first session only.
    let png: Vec<u8> = vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    let published = client
        .post(format!(
            "http://{addr}/code/sessions/{owning}/attachments/images"
        ))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, "image/png")
        .body(png)
        .send()
        .await
        .unwrap();
    assert_eq!(
        published.status(),
        reqwest::StatusCode::CREATED,
        "publish failed: {}",
        published.text().await.unwrap()
    );
    let published: serde_json::Value = published.json().await.unwrap();
    let blob_id = published["attachment_id"]
        .as_str()
        .or_else(|| published["blob_id"].as_str())
        .expect("the publication names the blob")
        .to_owned();

    let submit = |session: String| {
        let client = client.clone();
        let token = token.clone();
        let blob_id = blob_id.clone();
        async move {
            client
                .post(format!("http://{addr}/code/sessions/{session}/turns"))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "message": "look at this",
                    "attachments": [{ "blob_id": blob_id, "media_type": "image/png" }],
                }))
                .send()
                .await
                .unwrap()
        }
    };

    // The session it was published to may attach it.
    let owned = submit(owning).await;
    assert!(
        owned.status().is_success(),
        "the publishing session must be able to attach its own image: {}",
        owned.text().await.unwrap()
    );

    // A sibling session that merely knows the id may not — even though the
    // bytes are plainly present in the shared blob store.
    // Without the publication check this returns 202 with the image bound, so
    // the assertion is load-bearing rather than incidentally true.
    let stolen = submit(other).await;
    let status = stolen.status();
    let body = stolen.text().await.unwrap();
    assert_eq!(
        status,
        reqwest::StatusCode::BAD_REQUEST,
        "knowing a blob id must not authorize attaching it: {body}"
    );
    assert!(
        body.contains("was not published to session"),
        "the refusal must name authority, not blob absence: {body}"
    );
}

/// Arming one condition twice must answer with the row that exists.
///
/// `arm_trigger` upserts on `(owner, repo, condition)` and updates action and
/// enabled without touching the stored id. Returning the freshly minted id
/// instead would answer 201 with a trigger that `GET`, `PATCH` and `DELETE`
/// cannot find.
#[tokio::test]
async fn re_arming_a_condition_keeps_the_stored_trigger_id() {
    let adapter = ScriptedAdapter::new(plain_text_script());
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo_path = init_git_repo(dir.path());
    let (repo, _workspace) = register_and_workspace(&client, addr, &token, &repo_path).await;
    let repo_id = json_id(&repo).to_owned();

    let armed = client
        .post(format!("http://{addr}/code/repos/{repo_id}/triggers"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "condition": "checks_failed", "action": "deliver" }))
        .send()
        .await
        .unwrap();
    assert_eq!(armed.status(), reqwest::StatusCode::CREATED);
    let first: serde_json::Value = armed.json().await.unwrap();

    // Same condition, different action: the store's unique key collides.
    let rearmed = client
        .post(format!("http://{addr}/code/repos/{repo_id}/triggers"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "condition": "checks_failed", "action": "notify" }))
        .send()
        .await
        .unwrap();
    assert_eq!(rearmed.status(), reqwest::StatusCode::CREATED);
    let second: serde_json::Value = rearmed.json().await.unwrap();

    assert_eq!(
        json_id(&second),
        json_id(&first),
        "re-arming a condition must keep the stored id"
    );
    assert_eq!(
        second["action"], "notify",
        "the action is the one just armed"
    );

    let listed = client
        .get(format!("http://{addr}/code/repos/{repo_id}/triggers"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let rows: Vec<serde_json::Value> = listed.json().await.unwrap();
    assert_eq!(rows.len(), 1, "one row per condition, not one per action");
    assert_eq!(json_id(&rows[0]), json_id(&first));

    // The id it answered with has to be one the other routes can reach.
    let patched = client
        .patch(format!(
            "http://{addr}/code/repos/{repo_id}/triggers/{}",
            json_id(&second)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "enabled": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), reqwest::StatusCode::OK);
    let patched_body: serde_json::Value = patched.json().await.unwrap();
    assert_eq!(patched_body["enabled"], false);
    assert_eq!(
        patched_body["action"], "notify",
        "an enabled toggle must not overwrite the action"
    );

    // POST means arm: when it serializes after a disable, it deliberately
    // chooses the requested action and enables the existing row.
    let armed_again = client
        .post(format!("http://{addr}/code/repos/{repo_id}/triggers"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "condition": "checks_failed", "action": "deliver" }))
        .send()
        .await
        .unwrap();
    assert_eq!(armed_again.status(), reqwest::StatusCode::CREATED);
    let armed_again: serde_json::Value = armed_again.json().await.unwrap();
    assert_eq!(json_id(&armed_again), json_id(&first));
    assert_eq!(armed_again["action"], "deliver");
    assert_eq!(armed_again["enabled"], true);
}

/// A trigger id is not authority to mutate it through another repository's
/// route. Both writes must return not found and leave the owning row intact.
#[tokio::test]
async fn trigger_mutations_require_the_repository_in_the_path() {
    let adapter = ScriptedAdapter::new(plain_text_script());
    let (router, token, _runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let left = init_git_repo_named(dir.path(), "left-trigger-repo");
    let right = init_git_repo_named(dir.path(), "right-trigger-repo");
    let (left_repo, _left_workspace) = register_and_workspace(&client, addr, &token, &left).await;
    let (right_repo, _right_workspace) =
        register_and_workspace(&client, addr, &token, &right).await;
    let left_id = json_id(&left_repo).to_owned();
    let right_id = json_id(&right_repo).to_owned();

    let armed = client
        .post(format!("http://{addr}/code/repos/{right_id}/triggers"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "condition": "conflicts", "action": "notify" }))
        .send()
        .await
        .unwrap();
    assert_eq!(armed.status(), reqwest::StatusCode::CREATED);
    let trigger: serde_json::Value = armed.json().await.unwrap();
    let trigger_id = json_id(&trigger).to_owned();

    let patched = client
        .patch(format!(
            "http://{addr}/code/repos/{left_id}/triggers/{trigger_id}"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "enabled": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), reqwest::StatusCode::NOT_FOUND);

    let deleted = client
        .delete(format!(
            "http://{addr}/code/repos/{left_id}/triggers/{trigger_id}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), reqwest::StatusCode::NOT_FOUND);

    let listed = client
        .get(format!("http://{addr}/code/repos/{right_id}/triggers"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    let rows: Vec<serde_json::Value> = listed.json().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(json_id(&rows[0]), trigger_id);
    assert_eq!(rows[0]["enabled"], true);
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
