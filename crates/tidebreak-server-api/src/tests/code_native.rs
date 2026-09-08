//! `/code/native/*` routes: capability-bearer auth, token-derived subject
//! scoping, request-id recovery and conflict handling, and image transport,
//! all against a fake native runtime.

use super::*;

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Arc;

use axum::Router;
use tokio::net::TcpListener;
use uuid::Uuid;

use crate::code::native_channel::NativeSubject;
use crate::code::native_runtime::{NativeRuntime, NativeRuntimeError, NativeRuntimeScope};
use crate::code::CodeRuntime;
use tidebreak_core::computer_session::{
    ComputerUseCall, ComputerUseImage, ComputerUseOutcome, ComputerUseResult,
};
use tidebreak_core::{
    db, Attention, AttentionSource, AttentionState, CodeRepo, CodeWorkspace, CodeWorkspaceStatus,
    DbStore, HarnessKind, OwnerId, PermissionMode, RepoId, Session, SessionKind, SessionLifecycle,
    Store, WorkspaceId,
};
use tidebreak_harness::AdapterRegistry;

/// In-memory native runtime with the request-id semantics the routes rely
/// on: an exact repeat recovers the stored result, a request-id reuse with
/// different arguments conflicts, and revocation is an enduring tombstone.
#[derive(Default)]
struct FakeNativeRuntime {
    executions: Mutex<Vec<(NativeRuntimeScope, ComputerUseCall)>>,
    stored: Mutex<HashMap<Uuid, (ComputerUseCall, ComputerUseResult)>>,
    revoked: Mutex<Vec<NativeRuntimeScope>>,
    unavailable: bool,
    unknown_outcome: bool,
    blocked: Option<Arc<BlockedOperation>>,
}

#[derive(Default)]
struct BlockedOperation {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    released: tokio::sync::Notify,
    returned: std::sync::atomic::AtomicBool,
    owner_released: std::sync::atomic::AtomicBool,
}

struct InFlightOwner(Arc<BlockedOperation>);

impl Drop for InFlightOwner {
    fn drop(&mut self) {
        self.0
            .owner_released
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.0.released.notify_one();
    }
}

impl FakeNativeRuntime {
    fn unavailable() -> Self {
        Self {
            unavailable: true,
            ..Self::default()
        }
    }
    fn unknown_outcome() -> Self {
        Self {
            unknown_outcome: true,
            ..Self::default()
        }
    }
    fn executions(&self) -> Vec<(NativeRuntimeScope, ComputerUseCall)> {
        self.executions.lock().unwrap().clone()
    }
    fn tombstoned(&self, scope: &NativeRuntimeScope) -> bool {
        self.revoked.lock().unwrap().contains(scope)
    }

    fn perform(&self, call: &ComputerUseCall) -> ComputerUseResult {
        let images = if call.name == "computer_capture_screen" {
            vec![ComputerUseImage {
                mime_type: "image/png".into(),
                base64: "iVBORw0KGgo=".into(),
            }]
        } else {
            vec![]
        };
        ComputerUseResult {
            request_id: call.request_id,
            outcome: ComputerUseOutcome::Completed,
            text: format!("{} completed", call.name),
            data: serde_json::json!({}),
            error_code: None,
            images,
        }
    }
}

#[async_trait]
impl NativeRuntime for FakeNativeRuntime {
    fn is_available(&self) -> bool {
        !self.unavailable
    }

    async fn execute(
        &self,
        scope: &NativeRuntimeScope,
        call: &ComputerUseCall,
    ) -> Result<ComputerUseResult, NativeRuntimeError> {
        if self.tombstoned(scope) {
            return Err(NativeRuntimeError::SessionEnded);
        }
        {
            let stored = self.stored.lock().unwrap();
            if let Some((prior, _)) = stored.get(&call.request_id) {
                if prior == call {
                    return Err(NativeRuntimeError::Recovered);
                }
                return Err(NativeRuntimeError::RequestConflict);
            }
        }
        self.executions
            .lock()
            .unwrap()
            .push((scope.clone(), call.clone()));
        let _owner = if let Some(blocked) = &self.blocked {
            let owner = InFlightOwner(blocked.clone());
            blocked.entered.notify_one();
            blocked.release.notified().await;
            Some(owner)
        } else {
            None
        };
        if self.unknown_outcome {
            let unknown = ComputerUseResult {
                request_id: call.request_id,
                outcome: ComputerUseOutcome::Unknown,
                text: "the host lost track of this action's effect".into(),
                data: serde_json::json!({}),
                error_code: Some("unknown_outcome".into()),
                images: vec![],
            };
            self.stored
                .lock()
                .unwrap()
                .insert(call.request_id, (call.clone(), unknown));
            return Err(NativeRuntimeError::UnknownOutcome);
        }
        let result = self.perform(call);
        self.stored
            .lock()
            .unwrap()
            .insert(call.request_id, (call.clone(), result.clone()));
        if let Some(blocked) = &self.blocked {
            blocked
                .returned
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(result)
    }

    async fn result_for_call(
        &self,
        scope: &NativeRuntimeScope,
        call: &ComputerUseCall,
    ) -> Result<Option<ComputerUseResult>, NativeRuntimeError> {
        if self.tombstoned(scope) {
            return Err(NativeRuntimeError::SessionEnded);
        }
        match self.stored.lock().unwrap().get(&call.request_id) {
            Some((prior, result)) if prior == call => Ok(Some(result.clone())),
            Some(_) => Err(NativeRuntimeError::RequestConflict),
            None => Ok(None),
        }
    }

    async fn cancel_session(&self, _scope: &NativeRuntimeScope) -> Result<(), String> {
        Ok(())
    }

    fn revoke_session(&self, scope: &NativeRuntimeScope) {
        self.revoked.lock().unwrap().push(scope.clone());
    }
}

struct NativeApp {
    addr: std::net::SocketAddr,
    code: Arc<CodeRuntime>,
    fake: Option<Arc<FakeNativeRuntime>>,
    _dir: tempfile::TempDir,
}

async fn native_app(fake: Option<Arc<FakeNativeRuntime>>) -> NativeApp {
    let (dir, store) = temp_db_store("code-native.db").await;
    let db = Arc::new(store);
    let st: Arc<dyn Store> = db.clone();
    let native_runtime = fake
        .as_ref()
        .map(|runtime| -> Arc<dyn NativeRuntime> { runtime.clone() });
    let code = Arc::new(CodeRuntime::with_registry_and_native_runtime(
        db,
        dir.path().into(),
        AdapterRegistry::new(),
        native_runtime,
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
    NativeApp {
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

async fn seed_session(db: &DbStore, lc: SessionLifecycle) -> (WorkspaceId, SessionId) {
    let repo_id = RepoId::new();
    db::code::insert_repo(
        db,
        &CodeRepo {
            id: repo_id,
            owner: OwnerId::local(),
            root_path: "/nonexistent-repo".into(),
            display_name: "native-test".into(),
            default_base_ref: "main".into(),
            branch_prefix: "tidebreak/".into(),
            setup_script: None,
            archive_script: None,
            quick_actions: vec![],
            created_at: chrono::Utc::now(),
            removed_at: None,
            cloned_from: None,
            origin_host: None,
            origin_owner: None,
            origin_name: None,
        },
    )
    .await
    .unwrap();
    let ws = CodeWorkspace {
        id: WorkspaceId::new(),
        owner: OwnerId::local(),
        repo_id,
        title: "native".into(),
        worktree_path: "/nonexistent".into(),
        branch_name: "tidebreak/native".into(),
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
    let s = Session {
        id: SessionId::new(),
        owner: OwnerId::local(),
        owner_kind: None,
        workspace_id: Some(ws.id),
        kind: SessionKind::Interactive,
        harness_kind: HarnessKind::ClaudeCode,
        harness_version: None,
        harness_resume_ref: None,
        permission_mode: PermissionMode::Ask,
        model: None,
        reasoning_effort: None,
        fast_mode: false,
        lifecycle: lc,
        fence_reason: None,
        child_pid: None,
        child_process_identity: None,
        spawn_epoch: 0,
        attention: Attention::new(AttentionState::Working, AttentionSource::Lifecycle),
        unrecognized_event_count: 0,
        subagents: vec![],
        visibility: tidebreak_core::SessionVisibility::Private,
        created_at: chrono::Utc::now(),
        execution_location: tidebreak_core::ExecutionLocation::Machine,
        acts_as: None,
    };
    db::code::insert_session(db, &s).await.unwrap();
    (ws.id, s.id)
}

fn mint_token(code: &CodeRuntime, ws: WorkspaceId, s: SessionId) -> String {
    let capfile = code
        .native_tokens
        .issue(NativeSubject {
            owner: OwnerId::local(),
            workspace: ws,
            session: s,
        })
        .unwrap();
    let c = std::fs::read_to_string(&capfile).unwrap();
    serde_json::from_str::<serde_json::Value>(&c).unwrap()["token"]
        .as_str()
        .unwrap()
        .into()
}

fn wait_call(request_id: Uuid) -> serde_json::Value {
    serde_json::json!({
        "request_id": request_id,
        "name": "computer_wait",
        "arguments": {"seconds": 1.0},
    })
}

async fn post(
    addr: std::net::SocketAddr,
    rt: &str,
    tok: Option<&str>,
    body: serde_json::Value,
) -> reqwest::Response {
    let mut r = reqwest::Client::new()
        .post(format!("http://{addr}/code/native/{rt}"))
        .json(&body);
    if let Some(t) = tok {
        r = r.bearer_auth(t);
    }
    r.send().await.unwrap()
}

#[tokio::test]
async fn client_disconnect_keeps_native_execution_alive_until_its_result_is_stored() {
    use std::sync::atomic::Ordering;
    use tokio::io::AsyncWriteExt;

    let blocked = Arc::new(BlockedOperation::default());
    let runtime = Arc::new(FakeNativeRuntime {
        blocked: Some(blocked.clone()),
        ..FakeNativeRuntime::default()
    });
    let a = native_app(Some(runtime.clone())).await;
    let (workspace, session) = seed_session(&a.code.db, SessionLifecycle::Idle).await;
    let token = mint_token(&a.code, workspace, session);
    let request_id = Uuid::new_v4();
    let call = wait_call(request_id);
    let body = serde_json::to_string(&call).unwrap();
    let mut connection = tokio::net::TcpStream::connect(a.addr).await.unwrap();
    let request = format!(
        "POST /code/native/execute HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len(),
    );
    connection.write_all(request.as_bytes()).await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        blocked.entered.notified(),
    )
    .await
    .unwrap();
    // Close the actual HTTP connection after the runtime takes ownership.
    // Its pending host operation cannot be recalled by dropping this socket.
    connection.shutdown().await.unwrap();
    drop(connection);
    let released_early = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        blocked.released.notified(),
    )
    .await
    .is_ok();
    assert!(
        !released_early,
        "HTTP disconnect dropped the native runtime future before the host operation completed"
    );
    assert!(!blocked.owner_released.load(Ordering::SeqCst));
    blocked.release.notify_one();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        blocked.released.notified(),
    )
    .await
    .unwrap();
    assert!(blocked.returned.load(Ordering::SeqCst));
    let result = post(a.addr, "result", Some(&token), call.clone()).await;
    assert_eq!(result.status(), reqwest::StatusCode::OK);
    assert_eq!(
        result.json::<serde_json::Value>().await.unwrap()["outcome"],
        "completed"
    );
    let repeated = post(a.addr, "execute", Some(&token), call).await;
    assert_eq!(repeated.status(), reqwest::StatusCode::OK);
    assert_eq!(runtime.executions().len(), 1);
}

#[tokio::test]
async fn valid_token_executes_with_the_token_derived_scope() {
    let a = native_app(Some(Arc::new(FakeNativeRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, SessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    let request_id = Uuid::new_v4();
    let r = post(a.addr, "execute", Some(&t), wait_call(request_id)).await;
    assert_eq!(r.status(), reqwest::StatusCode::OK);
    let b: serde_json::Value = r.json().await.unwrap();
    assert_eq!(b["request_id"], request_id.to_string());
    assert_eq!(b["outcome"], "completed");
    let executions = a.fake.as_ref().unwrap().executions();
    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0].0.owner, OwnerId::local());
    assert_eq!(executions[0].0.workspace, ws);
    assert_eq!(executions[0].0.session, s);
}

#[tokio::test]
async fn missing_and_unknown_tokens_401() {
    let a = native_app(Some(Arc::new(FakeNativeRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, SessionLifecycle::Idle).await;
    let _ = mint_token(&a.code, ws, s);
    assert_eq!(
        post(a.addr, "execute", None, wait_call(Uuid::new_v4()))
            .await
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let guess = "tbreak_nt_00000000-0000-4000-8000-000000000000";
    let r = post(a.addr, "execute", Some(guess), wait_call(Uuid::new_v4())).await;
    assert_eq!(r.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(!r.text().await.unwrap().contains(guess));
    assert!(a.fake.as_ref().unwrap().executions().is_empty());
}

#[tokio::test]
async fn revoked_token_401_and_the_adapter_holds_a_tombstone() {
    let a = native_app(Some(Arc::new(FakeNativeRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, SessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    a.code.native_tokens.revoke(s);
    let r = post(a.addr, "execute", Some(&t), wait_call(Uuid::new_v4())).await;
    assert_eq!(r.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(!r.text().await.unwrap().contains(&t));
    // A reissued token for a session the adapter has tombstoned still cannot
    // execute: the adapter answers SessionEnded, mapped to 403.
    let scope = NativeRuntimeScope {
        owner: OwnerId::local(),
        workspace: ws,
        session: s,
    };
    a.fake.as_ref().unwrap().revoke_session(&scope);
    let t2 = mint_token(&a.code, ws, s);
    let r = post(a.addr, "execute", Some(&t2), wait_call(Uuid::new_v4())).await;
    assert_eq!(r.status(), reqwest::StatusCode::FORBIDDEN);
    assert!(a.fake.as_ref().unwrap().executions().is_empty());
}

#[tokio::test]
async fn wrong_owner_is_an_unknown_token() {
    let a = native_app(Some(Arc::new(FakeNativeRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, SessionLifecycle::Idle).await;
    // A token minted for another owner resolves to a subject whose session
    // read misses: the session belongs to `local`, not `intruder`.
    let capfile = a
        .code
        .native_tokens
        .issue(NativeSubject {
            owner: OwnerId::new("intruder").unwrap(),
            workspace: ws,
            session: s,
        })
        .unwrap();
    let c = std::fs::read_to_string(&capfile).unwrap();
    let t: String = serde_json::from_str::<serde_json::Value>(&c).unwrap()["token"]
        .as_str()
        .unwrap()
        .into();
    let r = post(a.addr, "execute", Some(&t), wait_call(Uuid::new_v4())).await;
    assert_eq!(r.status(), reqwest::StatusCode::FORBIDDEN);
    assert!(a.fake.as_ref().unwrap().executions().is_empty());
}

#[tokio::test]
async fn ended_session_403() {
    let a = native_app(Some(Arc::new(FakeNativeRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, SessionLifecycle::Ended).await;
    let t = mint_token(&a.code, ws, s);
    assert_eq!(
        post(a.addr, "execute", Some(&t), wait_call(Uuid::new_v4()))
            .await
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    assert!(a.fake.as_ref().unwrap().executions().is_empty());
}

#[tokio::test]
async fn cross_workspace_404() {
    let a = native_app(Some(Arc::new(FakeNativeRuntime::default()))).await;
    let (_, s) = seed_session(&a.code.db, SessionLifecycle::Idle).await;
    let t = mint_token(&a.code, WorkspaceId::new(), s);
    assert_eq!(
        post(a.addr, "execute", Some(&t), wait_call(Uuid::new_v4()))
            .await
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    assert!(a.fake.as_ref().unwrap().executions().is_empty());
}

#[tokio::test]
async fn missing_and_unavailable_runtime_501() {
    let a = native_app(None).await;
    let (ws, s) = seed_session(&a.code.db, SessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    let r = post(a.addr, "execute", Some(&t), wait_call(Uuid::new_v4())).await;
    assert_eq!(r.status(), reqwest::StatusCode::NOT_IMPLEMENTED);
    assert!(!r.text().await.unwrap().contains(&t));

    let b = native_app(Some(Arc::new(FakeNativeRuntime::unavailable()))).await;
    let (ws, s) = seed_session(&b.code.db, SessionLifecycle::Idle).await;
    let t = mint_token(&b.code, ws, s);
    let r = post(b.addr, "execute", Some(&t), wait_call(Uuid::new_v4())).await;
    assert_eq!(r.status(), reqwest::StatusCode::NOT_IMPLEMENTED);
    assert!(b.fake.as_ref().unwrap().executions().is_empty());
}

#[tokio::test]
async fn unknown_tool_and_malformed_arguments_422() {
    let a = native_app(Some(Arc::new(FakeNativeRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, SessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    let r = post(
        a.addr,
        "execute",
        Some(&t),
        serde_json::json!({
            "request_id": Uuid::new_v4(),
            "name": "computer_not_a_tool",
            "arguments": {},
        }),
    )
    .await;
    assert_eq!(r.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        r.json::<serde_json::Value>().await.unwrap()["kind"],
        "invalid_native_arguments"
    );
    let r = post(
        a.addr,
        "execute",
        Some(&t),
        serde_json::json!({
            "request_id": Uuid::new_v4(),
            "name": "computer_wait",
            "arguments": {"seconds": 1e9},
        }),
    )
    .await;
    assert_eq!(r.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    assert!(a.fake.as_ref().unwrap().executions().is_empty());
}

#[tokio::test]
async fn bodies_reject_subject_identifiers() {
    let a = native_app(Some(Arc::new(FakeNativeRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, SessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    let mut body = wait_call(Uuid::new_v4());
    body["owner"] = serde_json::json!("local");
    let r = post(a.addr, "execute", Some(&t), body).await;
    assert!(
        r.status().is_client_error(),
        "a body naming a subject must be refused, got {}",
        r.status()
    );
    assert!(a.fake.as_ref().unwrap().executions().is_empty());
}

#[tokio::test]
async fn an_exact_duplicate_recovers_the_stored_result_without_replaying() {
    let a = native_app(Some(Arc::new(FakeNativeRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, SessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    let request_id = Uuid::new_v4();
    let first = post(a.addr, "execute", Some(&t), wait_call(request_id)).await;
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    let first: serde_json::Value = first.json().await.unwrap();
    let second = post(a.addr, "execute", Some(&t), wait_call(request_id)).await;
    assert_eq!(second.status(), reqwest::StatusCode::OK);
    let second: serde_json::Value = second.json().await.unwrap();
    assert_eq!(first, second);
    // The host performed the action once; the second answer came from the
    // stored result.
    assert_eq!(a.fake.as_ref().unwrap().executions().len(), 1);
}

#[tokio::test]
async fn a_request_id_reuse_with_different_arguments_409s() {
    let a = native_app(Some(Arc::new(FakeNativeRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, SessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    let request_id = Uuid::new_v4();
    assert_eq!(
        post(a.addr, "execute", Some(&t), wait_call(request_id))
            .await
            .status(),
        reqwest::StatusCode::OK
    );
    let r = post(
        a.addr,
        "execute",
        Some(&t),
        serde_json::json!({
            "request_id": request_id,
            "name": "computer_wait",
            "arguments": {"seconds": 2.0},
        }),
    )
    .await;
    assert_eq!(r.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        r.json::<serde_json::Value>().await.unwrap()["kind"],
        "native_request_conflict"
    );
    assert_eq!(a.fake.as_ref().unwrap().executions().len(), 1);
}

#[tokio::test]
async fn an_unknown_outcome_409s_and_the_result_route_serves_the_record() {
    let a = native_app(Some(Arc::new(FakeNativeRuntime::unknown_outcome()))).await;
    let (ws, s) = seed_session(&a.code.db, SessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    let request_id = Uuid::new_v4();
    let r = post(a.addr, "execute", Some(&t), wait_call(request_id)).await;
    assert_eq!(r.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        r.json::<serde_json::Value>().await.unwrap()["kind"],
        "native_unknown_outcome"
    );
    // The bridge inspects what the host recorded before acting again.
    let r = post(a.addr, "result", Some(&t), wait_call(request_id)).await;
    assert_eq!(r.status(), reqwest::StatusCode::OK);
    let b: serde_json::Value = r.json().await.unwrap();
    assert_eq!(b["outcome"], "unknown");
    assert_eq!(b["error_code"], "unknown_outcome");
}

#[tokio::test]
async fn the_result_route_404s_with_nothing_stored() {
    let a = native_app(Some(Arc::new(FakeNativeRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, SessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    assert_eq!(
        post(a.addr, "result", Some(&t), wait_call(Uuid::new_v4()))
            .await
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn screenshots_carry_real_image_blocks() {
    let a = native_app(Some(Arc::new(FakeNativeRuntime::default()))).await;
    let (ws, s) = seed_session(&a.code.db, SessionLifecycle::Idle).await;
    let t = mint_token(&a.code, ws, s);
    let r = post(
        a.addr,
        "execute",
        Some(&t),
        serde_json::json!({
            "request_id": Uuid::new_v4(),
            "name": "computer_capture_screen",
            "arguments": {},
        }),
    )
    .await;
    assert_eq!(r.status(), reqwest::StatusCode::OK);
    let b: serde_json::Value = r.json().await.unwrap();
    assert_eq!(b["images"][0]["mime_type"], "image/png");
    assert_eq!(b["images"][0]["base64"], "iVBORw0KGgo=");
    assert!(
        !b["text"].as_str().unwrap().contains("iVBORw0KGgo="),
        "image bytes must not leak into model-facing text"
    );
}
