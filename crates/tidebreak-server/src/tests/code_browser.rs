//! `/code/browser/*` routes: capability-bearer auth, subject scoping, and
//! the runtime seam's error mapping — all against a fake runtime.

use super::*;

use std::net::Ipv4Addr;
use std::sync::Arc;

use axum::Router;
use tokio::net::TcpListener;

use crate::code::browser_channel::BrowserSubject;
use crate::code::browser_runtime::{BrowserRuntime, BrowserRuntimeError};
use crate::code::CodeRuntime;
use tidebreak_core::{
    db, Attention, AttentionSource, AttentionState, BrowserActArgs, BrowserActResult,
    BrowserActStatus, BrowserContentTrust, BrowserControllerState, BrowserEngineCapabilities,
    BrowserEngineDescriptor, BrowserEngineName, BrowserListArgs, BrowserListResult,
    BrowserLoadState, BrowserNavigateArgs, BrowserNavigateResult, BrowserPageSnapshot,
    BrowserScreenshotArgs, BrowserScreenshotResult, BrowserSessionSummary, BrowserSnapshotArgs,
    BrowserViewport, BrowserWaitArgs, BrowserWaitResult, BrowserWaitStatus, CodePermissionMode,
    CodeSession, CodeSessionId, CodeSessionKind, CodeSessionLifecycle, CodeWorkspace,
    CodeWorkspaceStatus, DbStore, HarnessKind, OwnerId, RepoId, Store, WorkspaceId,
};
use tidebreak_harness::AdapterRegistry;

// ── fake runtime ─────────────────────────────────────────────────────────

/// Records every call's subject and answers canned results; `stale: true`
/// answers `StaleTarget` from snapshot instead.
#[derive(Default)]
struct FakeBrowserRuntime {
    calls: Mutex<Vec<(&'static str, BrowserSubject)>>,
    revoked: Mutex<Vec<CodeSessionId>>,
    stale: bool,
}

impl FakeBrowserRuntime {
    fn stale() -> Self {
        Self {
            stale: true,
            ..Self::default()
        }
    }

    fn record(&self, method: &'static str, subject: &BrowserSubject) {
        self.calls.lock().unwrap().push((method, subject.clone()));
    }

    fn subjects(&self) -> Vec<(&'static str, BrowserSubject)> {
        self.calls.lock().unwrap().clone()
    }
}

fn engine() -> BrowserEngineDescriptor {
    BrowserEngineDescriptor {
        name: BrowserEngineName::WkWebView,
        capabilities: BrowserEngineCapabilities {
            lifecycle: true,
            persistent_profile: true,
            semantic_snapshot: true,
            semantic_actions: true,
            screenshot: true,
            cross_origin_frames: false,
            profile_reset: true,
        },
    }
}

#[async_trait]
impl BrowserRuntime for FakeBrowserRuntime {
    async fn list(
        &self,
        subject: &BrowserSubject,
        _args: BrowserListArgs,
    ) -> Result<BrowserListResult, BrowserRuntimeError> {
        self.record("list", subject);
        Ok(BrowserListResult {
            sessions: vec![BrowserSessionSummary {
                browser_id: "browser-1".to_owned(),
                url: Some("https://example.com".to_owned()),
                title: Some("Example".to_owned()),
                load_state: BrowserLoadState::Ready,
                visible: true,
                engine: engine(),
                controller: BrowserControllerState::default(),
            }],
        })
    }

    async fn navigate(
        &self,
        subject: &BrowserSubject,
        args: BrowserNavigateArgs,
    ) -> Result<BrowserNavigateResult, BrowserRuntimeError> {
        self.record("navigate", subject);
        if args.browser_id != "browser-1" {
            return Err(BrowserRuntimeError::UnknownBrowserId(args.browser_id));
        }
        Ok(BrowserNavigateResult {
            browser_id: args.browser_id,
            url: args.url,
            load_state: BrowserLoadState::Loading,
            document_epoch: 2,
        })
    }

    async fn snapshot(
        &self,
        subject: &BrowserSubject,
        args: BrowserSnapshotArgs,
    ) -> Result<BrowserPageSnapshot, BrowserRuntimeError> {
        self.record("snapshot", subject);
        if self.stale {
            return Err(BrowserRuntimeError::StaleTarget);
        }
        Ok(BrowserPageSnapshot {
            browser_id: args.browser_id,
            snapshot_id: "snapshot-1".to_owned(),
            document_epoch: 2,
            content_trust: BrowserContentTrust::UntrustedPage,
            url: "https://example.com".to_owned(),
            title: "Example".to_owned(),
            viewport: BrowserViewport {
                width: 800.0,
                height: 600.0,
                scroll_x: 0.0,
                scroll_y: 0.0,
            },
            nodes: vec![],
            frames: vec![],
            truncated: false,
        })
    }

    async fn wait_for(
        &self,
        subject: &BrowserSubject,
        args: BrowserWaitArgs,
    ) -> Result<BrowserWaitResult, BrowserRuntimeError> {
        self.record("wait_for", subject);
        Ok(BrowserWaitResult {
            browser_id: args.browser_id,
            status: BrowserWaitStatus::Resolved,
            message: "condition met".to_owned(),
            document_epoch: args.document_epoch,
            url: None,
            title: None,
        })
    }

    async fn screenshot(
        &self,
        subject: &BrowserSubject,
        args: BrowserScreenshotArgs,
    ) -> Result<BrowserScreenshotResult, BrowserRuntimeError> {
        self.record("screenshot", subject);
        Ok(BrowserScreenshotResult {
            browser_id: args.browser_id,
            snapshot_id: args.snapshot_id,
            document_epoch: args.document_epoch,
            image_base64: "aGVsbG8=".to_owned(),
            mime_type: "image/png".to_owned(),
        })
    }

    async fn act(
        &self,
        subject: &BrowserSubject,
        args: BrowserActArgs,
    ) -> Result<BrowserActResult, BrowserRuntimeError> {
        self.record("act", subject);
        Ok(BrowserActResult {
            browser_id: args.browser_id,
            snapshot_id: args.snapshot_id,
            document_epoch: args.document_epoch,
            target_ref: args.target_ref,
            action: args.action.kind().to_owned(),
            status: BrowserActStatus::Ok,
            message: "acted".to_owned(),
            requires_resnapshot: true,
            url: None,
            title: None,
        })
    }

    fn revoke_session(&self, session: CodeSessionId) {
        self.revoked.lock().unwrap().push(session);
    }
}

// ── harness ──────────────────────────────────────────────────────────────

struct BrowserApp {
    addr: std::net::SocketAddr,
    code: Arc<CodeRuntime>,
    fake: Option<Arc<FakeBrowserRuntime>>,
    _dir: tempfile::TempDir,
}

async fn browser_app(fake: Option<Arc<FakeBrowserRuntime>>) -> BrowserApp {
    let (dir, store) = temp_db_store("code-browser.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let code = Arc::new(CodeRuntime::with_registry(
        db,
        dir.path().to_path_buf(),
        AdapterRegistry::new(),
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
    state.code = Some(code.clone());
    if let Some(fake) = &fake {
        // The same wiring `bind_inner` performs: runtime installed on state,
        // and revocation observed synchronously through the token registry.
        let revoked_runtime = fake.clone();
        code.browser_tokens.set_revocation_hook(Arc::new(move |session| {
            revoked_runtime.revoke_session(session);
        }));
        state.set_browser_runtime(fake.clone());
    }
    let addr = serve(app(state)).await;
    BrowserApp {
        addr,
        code,
        fake,
        _dir: dir,
    }
}

async fn serve(router: Router) -> std::net::SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}

/// Insert an Active workspace plus one session in `lifecycle` directly; the
/// routes read rows, not worktrees, so no git checkout is needed.
async fn seed_session(
    db: &DbStore,
    lifecycle: CodeSessionLifecycle,
) -> (WorkspaceId, CodeSessionId) {
    let workspace = CodeWorkspace {
        id: WorkspaceId::new(),
        owner: OwnerId::local(),
        repo_id: RepoId::new(),
        title: "browser".to_owned(),
        worktree_path: "/nonexistent".to_owned(),
        branch_name: "tidebreak/browser".to_owned(),
        base_ref: "main".to_owned(),
        status: CodeWorkspaceStatus::Active,
        pr: None,
        created_at: chrono::Utc::now(),
        archived_at: None,
    };
    db::code::insert_workspace(db, &workspace).await.unwrap();
    let session = CodeSession {
        id: CodeSessionId::new(),
        owner: OwnerId::local(),
        workspace_id: workspace.id,
        kind: CodeSessionKind::Interactive,
        harness_kind: HarnessKind::ClaudeCode,
        harness_version: None,
        harness_resume_ref: None,
        permission_mode: CodePermissionMode::Ask,
        model: None,
        lifecycle,
        fence_reason: None,
        child_pid: None,
        spawn_epoch: 0,
        attention: Attention::new(AttentionState::Working, AttentionSource::Lifecycle),
        unrecognized_event_count: 0,
        subagents: vec![],
        created_at: chrono::Utc::now(),
    };
    db::code::insert_session(db, &session).await.unwrap();
    (workspace.id, session.id)
}

/// Mint a capability token for `{local owner, workspace, session}` and read
/// the bearer back the way the engine child would: from the capfile.
fn mint_token(code: &CodeRuntime, workspace: WorkspaceId, session: CodeSessionId) -> String {
    let spec = code
        .browser_tokens
        .issue(BrowserSubject {
            owner: OwnerId::local(),
            workspace,
            session,
        })
        .unwrap();
    let contents = std::fs::read_to_string(&spec.capability_file).unwrap();
    let value: serde_json::Value = serde_json::from_str(&contents).unwrap();
    value["token"].as_str().unwrap().to_owned()
}

async fn post_browser(
    addr: std::net::SocketAddr,
    route: &str,
    token: Option<&str>,
    body: serde_json::Value,
) -> reqwest::Response {
    let mut request = reqwest::Client::new()
        .post(format!("http://{addr}/code/browser/{route}"))
        .json(&body);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    request.send().await.unwrap()
}

// ── authentication ───────────────────────────────────────────────────────

#[tokio::test]
async fn valid_token_lists_sessions_for_its_exact_subject() {
    let app = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (workspace, session) = seed_session(&app.code.db, CodeSessionLifecycle::Idle).await;
    let token = mint_token(&app.code, workspace, session);

    let response = post_browser(app.addr, "list", Some(&token), serde_json::json!({})).await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["sessions"][0]["browserId"], "browser-1");

    // The runtime saw exactly the token's subject, not anything the caller
    // could have named.
    let calls = app.fake.as_ref().unwrap().subjects();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "list");
    assert_eq!(
        calls[0].1,
        BrowserSubject {
            owner: OwnerId::local(),
            workspace,
            session,
        }
    );
}

#[tokio::test]
async fn missing_and_unknown_tokens_answer_401() {
    let app = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (workspace, session) = seed_session(&app.code.db, CodeSessionLifecycle::Idle).await;
    let _live = mint_token(&app.code, workspace, session);

    let missing = post_browser(app.addr, "list", None, serde_json::json!({})).await;
    assert_eq!(missing.status(), reqwest::StatusCode::UNAUTHORIZED);

    let guessed = "tbreak_bt_00000000-0000-4000-8000-000000000000";
    let unknown = post_browser(app.addr, "list", Some(guessed), serde_json::json!({})).await;
    assert_eq!(unknown.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body = unknown.text().await.unwrap();
    assert!(
        !body.contains(guessed),
        "an error body must not echo the presented token: {body}"
    );
    assert!(app.fake.as_ref().unwrap().subjects().is_empty());
}

#[tokio::test]
async fn app_launch_token_is_not_a_browser_token() {
    // The routes sit outside `require_token` on purpose; the per-launch app
    // bearer must not open the browser channel.
    let app = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (workspace, session) = seed_session(&app.code.db, CodeSessionLifecycle::Idle).await;
    let _live = mint_token(&app.code, workspace, session);

    let response = post_browser(
        app.addr,
        "list",
        Some(crate::state::TEST_CLIENT_EXECUTOR_TOKEN),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn revoked_token_answers_401_and_revocation_reaches_the_runtime() {
    let app = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (workspace, session) = seed_session(&app.code.db, CodeSessionLifecycle::Idle).await;
    let token = mint_token(&app.code, workspace, session);

    app.code.browser_tokens.revoke(session);
    // The hook fired synchronously inside `revoke`, before any poll.
    assert_eq!(
        app.fake.as_ref().unwrap().revoked.lock().unwrap().clone(),
        vec![session]
    );

    let response = post_browser(app.addr, "list", Some(&token), serde_json::json!({})).await;
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body = response.text().await.unwrap();
    assert!(
        !body.contains(&token),
        "revoked-token body leaked the token"
    );
}

// ── subject scoping ──────────────────────────────────────────────────────

#[tokio::test]
async fn ended_session_answers_403() {
    let app = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (workspace, session) = seed_session(&app.code.db, CodeSessionLifecycle::Ended).await;
    let token = mint_token(&app.code, workspace, session);

    let response = post_browser(app.addr, "list", Some(&token), serde_json::json!({})).await;
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    assert!(app.fake.as_ref().unwrap().subjects().is_empty());
}

#[tokio::test]
async fn cross_workspace_token_answers_404() {
    let app = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (_workspace, session) = seed_session(&app.code.db, CodeSessionLifecycle::Idle).await;
    // A subject naming a workspace the session does not belong to must read
    // exactly like a missing target.
    let token = mint_token(&app.code, WorkspaceId::new(), session);

    let response = post_browser(app.addr, "list", Some(&token), serde_json::json!({})).await;
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    assert!(app.fake.as_ref().unwrap().subjects().is_empty());
}

// ── runtime seam ─────────────────────────────────────────────────────────

#[tokio::test]
async fn missing_runtime_answers_501_after_authentication() {
    let app = browser_app(None).await;
    let (workspace, session) = seed_session(&app.code.db, CodeSessionLifecycle::Idle).await;
    let token = mint_token(&app.code, workspace, session);

    let response = post_browser(app.addr, "list", Some(&token), serde_json::json!({})).await;
    assert_eq!(response.status(), reqwest::StatusCode::NOT_IMPLEMENTED);
    let body = response.text().await.unwrap();
    assert!(!body.contains(&token), "501 body leaked the token");

    // Still 401 for a bad token — auth is checked before the runtime.
    let unknown = post_browser(app.addr, "list", Some("nope"), serde_json::json!({})).await;
    assert_eq!(unknown.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn stale_snapshot_answers_409() {
    let app = browser_app(Some(Arc::new(FakeBrowserRuntime::stale()))).await;
    let (workspace, session) = seed_session(&app.code.db, CodeSessionLifecycle::Idle).await;
    let token = mint_token(&app.code, workspace, session);

    let response = post_browser(
        app.addr,
        "snapshot",
        Some(&token),
        serde_json::json!({ "browser_id": "browser-1" }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["kind"], "stale_browser_target");
}

#[tokio::test]
async fn navigate_round_trips_through_the_runtime() {
    let app = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (workspace, session) = seed_session(&app.code.db, CodeSessionLifecycle::Idle).await;
    let token = mint_token(&app.code, workspace, session);

    let response = post_browser(
        app.addr,
        "navigate",
        Some(&token),
        serde_json::json!({ "browser_id": "browser-1", "url": "https://example.com/docs" }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["url"], "https://example.com/docs");
    assert_eq!(body["loadState"], "loading");

    let unknown = post_browser(
        app.addr,
        "navigate",
        Some(&token),
        serde_json::json!({ "browser_id": "browser-9", "url": "https://example.com" }),
    )
    .await;
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);
}

// ── argument validation ──────────────────────────────────────────────────

#[tokio::test]
async fn navigate_refuses_non_http_and_credentialed_urls() {
    let app = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (workspace, session) = seed_session(&app.code.db, CodeSessionLifecycle::Idle).await;
    let token = mint_token(&app.code, workspace, session);

    for url in ["file:///etc/passwd", "https://user:secret@example.com"] {
        let response = post_browser(
            app.addr,
            "navigate",
            Some(&token),
            serde_json::json!({ "browser_id": "browser-1", "url": url }),
        )
        .await;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            "url {url} must be refused before the runtime"
        );
    }
    assert!(
        app.fake.as_ref().unwrap().subjects().is_empty(),
        "an ill-formed proposal must never reach the runtime"
    );
}

#[tokio::test]
async fn bodies_never_accept_subject_identifiers() {
    // Owner, workspace, and session identity come from the token alone; a
    // body that tries to name them is malformed, not consulted.
    let app = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (workspace, session) = seed_session(&app.code.db, CodeSessionLifecycle::Idle).await;
    let token = mint_token(&app.code, workspace, session);

    for (route, body) in [
        ("list", serde_json::json!({ "workspace_id": "guess" })),
        (
            "navigate",
            serde_json::json!({
                "browser_id": "browser-1",
                "url": "https://example.com",
                "session_id": "guess",
            }),
        ),
        (
            "snapshot",
            serde_json::json!({ "browser_id": "browser-1", "owner_id": "guess" }),
        ),
    ] {
        let response = post_browser(app.addr, route, Some(&token), body).await;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "route {route} must refuse subject identifiers in the body"
        );
    }
    assert!(app.fake.as_ref().unwrap().subjects().is_empty());
}
