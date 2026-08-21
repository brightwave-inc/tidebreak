//! `/code/browser/*` routes: capability-bearer auth, subject scoping,
//! and the runtime seam against a fake runtime.

use super::*;

use std::net::Ipv4Addr;
use std::sync::Arc;

use axum::Router;
use tokio::net::TcpListener;

use crate::code::browser_channel::BrowserSubject;
use crate::code::browser_runtime::{BrowserRuntime, BrowserRuntimeError, BrowserRuntimeScope};
use crate::code::CodeRuntime;
use tidebreak_core::{
    db, Attention, AttentionSource, AttentionState, BrowserContentTrust, BrowserControllerState,
    BrowserEngineCapabilities, BrowserEngineDescriptor, BrowserEngineName, BrowserListResult,
    BrowserLoadState, BrowserNavigateArgs, BrowserNavigateResult, BrowserPageSnapshot,
    BrowserScreenshotArgs, BrowserScreenshotResult, BrowserSessionSummary, BrowserSnapshotArgs,
    BrowserViewport, BrowserWaitArgs, BrowserWaitResult, BrowserWaitStatus, CodePermissionMode,
    CodeRepo, CodeSession, CodeSessionId, CodeSessionKind, CodeSessionLifecycle, CodeWorkspace,
    CodeWorkspaceStatus, DbStore, HarnessKind, OwnerId, RepoId, Store, WorkspaceId,
};
use tidebreak_harness::AdapterRegistry;

#[derive(Default)]
struct FakeBrowserRuntime {
    calls: Mutex<Vec<(&'static str, BrowserRuntimeScope)>>,
    revoked: Mutex<Vec<BrowserRuntimeScope>>,
    stale: bool,
    not_authorized: bool,
}

impl FakeBrowserRuntime {
    fn stale() -> Self {
        Self {
            stale: true,
            ..Self::default()
        }
    }
    fn not_authorized() -> Self {
        Self {
            not_authorized: true,
            ..Self::default()
        }
    }
    fn record(&self, m: &'static str, s: &BrowserRuntimeScope) {
        self.calls.lock().unwrap().push((m, s.clone()));
    }
    fn subjects(&self) -> Vec<(&'static str, BrowserRuntimeScope)> {
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
            semantic_actions: false,
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
        scope: &BrowserRuntimeScope,
    ) -> Result<BrowserListResult, BrowserRuntimeError> {
        self.record("list", scope);
        Ok(BrowserListResult {
            sessions: vec![BrowserSessionSummary {
                browser_id: "browser-1".into(),
                url: Some("https://example.com".into()),
                title: Some("Example".into()),
                load_state: BrowserLoadState::Ready,
                visible: true,
                engine: engine(),
                controller: BrowserControllerState::default(),
            }],
        })
    }
    async fn navigate(
        &self,
        scope: &BrowserRuntimeScope,
        args: &BrowserNavigateArgs,
    ) -> Result<BrowserNavigateResult, BrowserRuntimeError> {
        self.record("navigate", scope);
        if args.browser_id != "browser-1" {
            return Err(BrowserRuntimeError::UnknownBrowserId(
                args.browser_id.clone(),
            ));
        }
        Ok(BrowserNavigateResult {
            browser_id: args.browser_id.clone(),
            url: args.url.clone(),
            load_state: BrowserLoadState::Loading,
            document_epoch: 2,
        })
    }
    async fn snapshot(
        &self,
        scope: &BrowserRuntimeScope,
        _args: &BrowserSnapshotArgs,
    ) -> Result<BrowserPageSnapshot, BrowserRuntimeError> {
        self.record("snapshot", scope);
        if self.stale {
            return Err(BrowserRuntimeError::StaleTarget);
        }
        if self.not_authorized {
            return Err(BrowserRuntimeError::NotAuthorized(
                "browser origin is not shared with this agent".into(),
            ));
        }
        Ok(BrowserPageSnapshot {
            browser_id: "browser-1".into(),
            snapshot_id: "snapshot-1".into(),
            document_epoch: 2,
            content_trust: BrowserContentTrust::UntrustedPage,
            url: "https://example.com".into(),
            title: "Example".into(),
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
    async fn wait(
        &self,
        scope: &BrowserRuntimeScope,
        args: &BrowserWaitArgs,
    ) -> Result<BrowserWaitResult, BrowserRuntimeError> {
        self.record("wait", scope);
        if args.browser_id != "browser-1" {
            return Err(BrowserRuntimeError::UnknownBrowserId(
                args.browser_id.clone(),
            ));
        }
        if self.stale {
            return Err(BrowserRuntimeError::StaleTarget);
        }
        Ok(BrowserWaitResult {
            browser_id: args.browser_id.clone(),
            status: BrowserWaitStatus::Resolved,
            message: "Wait condition satisfied.".into(),
            document_epoch: 2,
            url: Some("https://example.com".into()),
            title: Some("Example".into()),
        })
    }
    async fn screenshot(
        &self,
        scope: &BrowserRuntimeScope,
        args: &BrowserScreenshotArgs,
    ) -> Result<BrowserScreenshotResult, BrowserRuntimeError> {
        self.record("screenshot", scope);
        if args.browser_id != "browser-1" {
            return Err(BrowserRuntimeError::UnknownBrowserId(
                args.browser_id.clone(),
            ));
        }
        if self.stale {
            return Err(BrowserRuntimeError::StaleTarget);
        }
        Ok(BrowserScreenshotResult {
            browser_id: args.browser_id.clone(),
            snapshot_id: args.snapshot_id.clone(),
            document_epoch: 2,
            image_base64: "AAAA".into(),
            mime_type: "image/png".into(),
        })
    }
    fn revoke_session(&self, scope: &BrowserRuntimeScope) {
        self.revoked.lock().unwrap().push(scope.clone());
    }
}

struct BrowserApp {
    addr: std::net::SocketAddr,
    code: Arc<CodeRuntime>,
    fake: Option<Arc<FakeBrowserRuntime>>,
    _dir: tempfile::TempDir,
}

async fn browser_app(fake: Option<Arc<FakeBrowserRuntime>>) -> BrowserApp {
    let (dir, store) = temp_db_store("code-browser.db").await;
    let db = Arc::new(store);
    let st: Arc<dyn Store> = db.clone();
    let browser_runtime = fake
        .as_ref()
        .map(|runtime| -> Arc<dyn BrowserRuntime> { runtime.clone() });
    let bridge = crate::code::browser_channel::test_bridge_command();
    let code = Arc::new(CodeRuntime::with_registry_and_browser_runtime(
        db,
        dir.path().into(),
        AdapterRegistry::new(),
        browser_runtime,
        Some(bridge),
    ));
    let mut state = AppState::new(
        Config::desktop(dir.path()),
        st,
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    state.code = Some(code.clone());
    let addr = serve(app(state)).await;
    BrowserApp {
        addr,
        code,
        fake,
        _dir: dir,
    }
}

async fn serve(router: Router) -> std::net::SocketAddr {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let a = l.local_addr().unwrap();
    tokio::spawn(async {
        let _ = axum::serve(l, router).await;
    });
    a
}

async fn seed_session(db: &DbStore, lc: CodeSessionLifecycle) -> (WorkspaceId, CodeSessionId) {
    let repo_id = RepoId::new();
    db::code::insert_repo(
        db,
        &CodeRepo {
            id: repo_id,
            owner: OwnerId::local(),
            root_path: "/nonexistent-repo".into(),
            display_name: "browser-test".into(),
            default_base_ref: "main".into(),
            branch_prefix: "tidebreak/".into(),
            setup_script: None,
            archive_script: None,
            quick_actions: vec![],
            created_at: chrono::Utc::now(),
            removed_at: None,
            cloned_from: None,
        },
    )
    .await
    .unwrap();
    let ws = CodeWorkspace {
        id: WorkspaceId::new(),
        owner: OwnerId::local(),
        repo_id,
        title: "browser".into(),
        worktree_path: "/nonexistent".into(),
        branch_name: "tidebreak/browser".into(),
        base_ref: "main".into(),
        status: CodeWorkspaceStatus::Active,
        pr: None,
        created_at: chrono::Utc::now(),
        archived_at: None,
        released_at: None,
        released_tip: None,
        bundle_bytes: None,
    };
    db::code::insert_workspace(db, &ws).await.unwrap();
    let s = CodeSession {
        id: CodeSessionId::new(),
        owner: OwnerId::local(),
        workspace_id: ws.id,
        kind: CodeSessionKind::Interactive,
        harness_kind: HarnessKind::ClaudeCode,
        harness_version: None,
        harness_resume_ref: None,
        permission_mode: CodePermissionMode::Ask,
        model: None,
        lifecycle: lc,
        fence_reason: None,
        child_pid: None,
        spawn_epoch: 0,
        attention: Attention::new(AttentionState::Working, AttentionSource::Lifecycle),
        unrecognized_event_count: 0,
        subagents: vec![],
        created_at: chrono::Utc::now(),
    };
    db::code::insert_session(db, &s).await.unwrap();
    (ws.id, s.id)
}

fn mint_token(code: &CodeRuntime, ws: WorkspaceId, s: CodeSessionId) -> String {
    let bridge = crate::code::browser_channel::test_bridge_command();
    let sp = code
        .browser_tokens
        .issue(
            BrowserSubject {
                owner: OwnerId::local(),
                workspace: ws,
                session: s,
            },
            &bridge,
        )
        .unwrap();
    let c = std::fs::read_to_string(&sp.capability_file).unwrap();
    serde_json::from_str::<serde_json::Value>(&c).unwrap()["token"]
        .as_str()
        .unwrap()
        .into()
}

async fn post(
    addr: std::net::SocketAddr,
    rt: &str,
    tok: Option<&str>,
    body: serde_json::Value,
) -> reqwest::Response {
    let mut r = reqwest::Client::new()
        .post(format!("http://{addr}/code/browser/{rt}"))
        .json(&body);
    if let Some(t) = tok {
        r = r.bearer_auth(t);
    }
    r.send().await.unwrap()
}
async fn get(addr: std::net::SocketAddr, rt: &str, tok: Option<&str>) -> reqwest::Response {
    let mut r = reqwest::Client::new().get(format!("http://{addr}/code/browser/{rt}"));
    if let Some(t) = tok {
        r = r.bearer_auth(t);
    }
    r.send().await.unwrap()
}

#[tokio::test]
async fn valid_token_lists() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    let r = get(a.addr, "list", Some(&t)).await;
    assert_eq!(r.status(), reqwest::StatusCode::OK);
    let b: serde_json::Value = r.json().await.unwrap();
    assert_eq!(b["sessions"][0]["browserId"], "browser-1");
    let c = a.fake.as_ref().unwrap().subjects();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].0, "list");
    assert_eq!(c[0].1.owner, OwnerId::local());
    assert_eq!(c[0].1.workspace, ws);
    assert_eq!(c[0].1.session, s);
}

#[tokio::test]
async fn missing_and_unknown_401() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let _ = mint_token(&a.code, ws, s);
    assert_eq!(
        get(a.addr, "list", None).await.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let g = "tbreak_bt_00000000-0000-4000-8000-000000000000";
    let r = get(a.addr, "list", Some(g)).await;
    assert_eq!(r.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(!r.text().await.unwrap().contains(g));
    assert!(a.fake.as_ref().unwrap().subjects().is_empty());
}

#[tokio::test]
async fn app_token_not_browser() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let _ = mint_token(&a.code, ws, s);
    assert_eq!(
        get(
            a.addr,
            "list",
            Some(crate::state::TEST_CLIENT_EXECUTOR_TOKEN)
        )
        .await
        .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn revoked_then_401() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    a.code.browser_tokens.revoke(s);
    let r = get(a.addr, "list", Some(&t)).await;
    assert_eq!(r.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(!r.text().await.unwrap().contains(&t));
}

#[tokio::test]
async fn ended_session_403() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Ended).await;
    let t = mint_token(&a.code, ws, s);
    assert_eq!(
        get(a.addr, "list", Some(&t)).await.status(),
        reqwest::StatusCode::FORBIDDEN
    );
    assert!(a.fake.as_ref().unwrap().subjects().is_empty());
}

#[tokio::test]
async fn cross_workspace_404() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (_, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, WorkspaceId::new(), s);
    assert_eq!(
        get(a.addr, "list", Some(&t)).await.status(),
        reqwest::StatusCode::NOT_FOUND
    );
    assert!(a.fake.as_ref().unwrap().subjects().is_empty());
}

#[tokio::test]
async fn missing_runtime_501() {
    let a = browser_app(None).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    let r = get(a.addr, "list", Some(&t)).await;
    assert_eq!(r.status(), reqwest::StatusCode::NOT_IMPLEMENTED);
    assert!(!r.text().await.unwrap().contains(&t));
    assert_eq!(
        get(a.addr, "list", Some("nope")).await.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn stale_snapshot_409() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::stale()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    let r = post(
        a.addr,
        "snapshot",
        Some(&t),
        serde_json::json!({"browser_id":"browser-1"}),
    )
    .await;
    assert_eq!(r.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        r.json::<serde_json::Value>().await.unwrap()["kind"],
        "stale_browser_target"
    );
}

#[tokio::test]
async fn unshared_origin_403() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::not_authorized()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    let r = post(
        a.addr,
        "snapshot",
        Some(&t),
        serde_json::json!({"browser_id":"browser-1"}),
    )
    .await;
    assert_eq!(r.status(), reqwest::StatusCode::FORBIDDEN);
    let body = r.text().await.unwrap();
    assert!(body.contains("not shared"));
    assert!(!body.contains(&t));
}

#[tokio::test]
async fn navigate_roundtrip() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    let r = post(
        a.addr,
        "navigate",
        Some(&t),
        serde_json::json!({"browser_id":"browser-1","url":"https://example.com/docs"}),
    )
    .await;
    assert_eq!(r.status(), reqwest::StatusCode::OK);
    let b: serde_json::Value = r.json().await.unwrap();
    assert_eq!(b["url"], "https://example.com/docs");
    assert_eq!(b["loadState"], "loading");
    let u = post(
        a.addr,
        "navigate",
        Some(&t),
        serde_json::json!({"browser_id":"browser-9","url":"https://example.com"}),
    )
    .await;
    assert_eq!(u.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn snapshot_roundtrip() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    let r = post(
        a.addr,
        "snapshot",
        Some(&t),
        serde_json::json!({"browser_id":"browser-1"}),
    )
    .await;
    assert_eq!(r.status(), reqwest::StatusCode::OK);
    assert_eq!(
        r.json::<serde_json::Value>().await.unwrap()["url"],
        "https://example.com"
    );
}

#[tokio::test]
async fn navigate_refuses_non_http() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    for u in ["file:///etc/passwd", "https://user:secret@example.com"] {
        assert_eq!(
            post(
                a.addr,
                "navigate",
                Some(&t),
                serde_json::json!({"browser_id":"browser-1","url":u})
            )
            .await
            .status(),
            reqwest::StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert!(a.fake.as_ref().unwrap().subjects().is_empty());
}

#[tokio::test]
async fn wait_roundtrip() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    let r = post(
        a.addr,
        "wait",
        Some(&t),
        serde_json::json!({
            "browser_id": "browser-1",
            "snapshot_id": "snapshot-1",
            "document_epoch": 2,
            "condition": {"kind": "load_state", "state": "ready"},
            "timeout_ms": 5000
        }),
    )
    .await;
    assert_eq!(r.status(), reqwest::StatusCode::OK);
    let b: serde_json::Value = r.json().await.unwrap();
    assert_eq!(b["status"], "resolved");
    assert_eq!(b["browserId"], "browser-1");
}

#[tokio::test]
async fn wait_refuses_missing_browser_id() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    assert_eq!(
        post(
            a.addr,
            "wait",
            Some(&t),
            serde_json::json!({
                "browser_id": "browser-9",
                "snapshot_id": "snapshot-1",
                "document_epoch": 2,
                "condition": {"kind": "load_state", "state": "ready"}
            })
        )
        .await
        .status(),
        reqwest::StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn wait_refuses_non_well_formed() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    assert_eq!(
        post(
            a.addr,
            "wait",
            Some(&t),
            serde_json::json!({"browser_id":"b-1","snapshot_id":"s","document_epoch":0,"condition":{"kind":"text_present","text":""},"timeout_ms":99999})
        )
        .await
        .status(),
        reqwest::StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[tokio::test]
async fn wait_stale_409() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::stale()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    let r = post(
        a.addr,
        "wait",
        Some(&t),
        serde_json::json!({
            "browser_id": "browser-1",
            "snapshot_id": "snapshot-1",
            "document_epoch": 2,
            "condition": {"kind": "load_state", "state": "ready"}
        }),
    )
    .await;
    assert_eq!(r.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test]
async fn wait_missing_runtime_501() {
    let a = browser_app(None).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    let r = post(
        a.addr,
        "wait",
        Some(&t),
        serde_json::json!({
            "browser_id": "browser-1",
            "snapshot_id": "snapshot-1",
            "document_epoch": 2,
            "condition": {"kind": "load_state", "state": "ready"}
        }),
    )
    .await;
    assert_eq!(r.status(), reqwest::StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn screenshot_roundtrip() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    let r = post(
        a.addr,
        "screenshot",
        Some(&t),
        serde_json::json!({
            "browser_id": "browser-1",
            "snapshot_id": "snapshot-1",
            "document_epoch": 2
        }),
    )
    .await;
    assert_eq!(r.status(), reqwest::StatusCode::OK);
    let b: serde_json::Value = r.json().await.unwrap();
    assert_eq!(b["mimeType"], "image/png");
    assert_eq!(b["browserId"], "browser-1");
}

#[tokio::test]
async fn screenshot_refuses_non_well_formed() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    assert_eq!(
        post(
            a.addr,
            "screenshot",
            Some(&t),
            serde_json::json!({"browser_id":"b-1","snapshot_id":"s","document_epoch":0,"max_width":999999})
        )
        .await
        .status(),
        reqwest::StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[tokio::test]
async fn screenshot_stale_409() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::stale()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    let r = post(
        a.addr,
        "screenshot",
        Some(&t),
        serde_json::json!({
            "browser_id": "browser-1",
            "snapshot_id": "snapshot-1",
            "document_epoch": 2
        }),
    )
    .await;
    assert_eq!(r.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test]
async fn screenshot_missing_runtime_501() {
    let a = browser_app(None).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    let r = post(
        a.addr,
        "screenshot",
        Some(&t),
        serde_json::json!({
            "browser_id": "browser-1",
            "snapshot_id": "snapshot-1",
            "document_epoch": 2
        }),
    )
    .await;
    assert_eq!(r.status(), reqwest::StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn new_routes_require_capability_token() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let _t = mint_token(&a.code, ws, s);
    for (rt, body) in [
        (
            "wait",
            serde_json::json!({
                "browser_id": "browser-1",
                "snapshot_id": "snapshot-1",
                "document_epoch": 2,
                "condition": {"kind": "load_state", "state": "ready"}
            }),
        ),
        (
            "screenshot",
            serde_json::json!({
                "browser_id": "browser-1",
                "snapshot_id": "snapshot-1",
                "document_epoch": 2
            }),
        ),
    ] {
        assert_eq!(
            post(a.addr, rt, None, body).await.status(),
            reqwest::StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn new_routes_refuse_wrong_workspace_scope() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (_, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, WorkspaceId::new(), s);
    for (rt, body) in [
        (
            "wait",
            serde_json::json!({"browser_id":"browser-1","snapshot_id":"s","document_epoch":0,"condition":{"kind":"load_state","state":"ready"}}),
        ),
        (
            "screenshot",
            serde_json::json!({"browser_id":"browser-1","snapshot_id":"s","document_epoch":0}),
        ),
    ] {
        assert_eq!(
            post(a.addr, rt, Some(&t), body).await.status(),
            reqwest::StatusCode::NOT_FOUND
        );
    }
    assert!(a.fake.as_ref().unwrap().subjects().is_empty());
}

#[tokio::test]
async fn new_routes_refuse_ended_session() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Ended).await;
    let t = mint_token(&a.code, ws, s);
    for (rt, body) in [
        (
            "wait",
            serde_json::json!({"browser_id":"browser-1","snapshot_id":"s","document_epoch":0,"condition":{"kind":"load_state","state":"ready"}}),
        ),
        (
            "screenshot",
            serde_json::json!({"browser_id":"browser-1","snapshot_id":"s","document_epoch":0}),
        ),
    ] {
        assert_eq!(
            post(a.addr, rt, Some(&t), body).await.status(),
            reqwest::StatusCode::FORBIDDEN
        );
    }
    assert!(a.fake.as_ref().unwrap().subjects().is_empty());
}

#[tokio::test]
async fn snapshot_needs_browser_id() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    assert_eq!(
        post(a.addr, "snapshot", Some(&t), serde_json::json!({}))
            .await
            .status(),
        reqwest::StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn bodies_reject_subject_ids() {
    let a = browser_app(Some(Arc::new(FakeBrowserRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, CodeSessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    for (rt, body) in [
        (
            "navigate",
            serde_json::json!({"browser_id":"b-1","url":"https://x.com","session_id":"g"}),
        ),
        (
            "snapshot",
            serde_json::json!({"browser_id":"b-1","owner_id":"g"}),
        ),
        (
            "wait",
            serde_json::json!({
                "browser_id":"b-1",
                "snapshot_id":"snapshot-1",
                "document_epoch":0,
                "condition":{"kind":"url_changed"},
                "session_id":"g"
            }),
        ),
        (
            "screenshot",
            serde_json::json!({
                "browser_id":"b-1",
                "snapshot_id":"snapshot-1",
                "document_epoch":0,
                "owner_id":"g"
            }),
        ),
    ] {
        assert_eq!(
            post(a.addr, rt, Some(&t), body).await.status(),
            reqwest::StatusCode::BAD_REQUEST
        );
    }
    assert!(a.fake.as_ref().unwrap().subjects().is_empty());
}
