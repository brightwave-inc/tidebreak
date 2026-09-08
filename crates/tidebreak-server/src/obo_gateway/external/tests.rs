use super::*;
use crate::code::{harness_llm::HarnessLlmRelay, runtime::CodeRuntime};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const USER: &str = "26fecc98-0998-4f3b-a302-cafce9b8dd68";
const RESOURCE: &str = "tidebreak:test-machine";

#[derive(Clone, Default)]
struct Gateway {
    approvals: Arc<AtomicUsize>,
    exchanges: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    mints: Arc<AtomicUsize>,
    revokes: Arc<AtomicUsize>,
    failed: Arc<AtomicBool>,
    wrong_owner: Arc<AtomicBool>,
    wrong_machine: Arc<AtomicBool>,
    enrolled: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

async fn gateway() -> (String, Gateway) {
    let state = Gateway::default();
    let router = Router::new()
        .route("/api/v1/tidebreak/external-delegations", post(|State(state): State<Gateway>, headers: HeaderMap, Json(body): Json<serde_json::Value>| async move {
            assert_eq!(headers.get("authorization").unwrap(), "Bearer browser-owner");
            assert_eq!(body["resource"], RESOURCE);
            state.approvals.fetch_add(1, Ordering::SeqCst);
            state.enrolled.lock().unwrap().insert(body["delegation_id"].as_str().unwrap().to_owned());
            Json(serde_json::json!({"delegation_id": body["delegation_id"], "resource": RESOURCE, "user_id": USER}))
        }))
        .route("/api/v1/tidebreak/external-delegations/{id}/token", post(|State(state): State<Gateway>, Path(id): Path<String>, Json(body): Json<serde_json::Value>| async move {
            assert_eq!(body["client_id"], "tidebreak");
            assert_eq!(body["client_secret"], "test-machine-secret");
            assert_eq!(body["resource"], RESOURCE);
            if !state.enrolled.lock().unwrap().contains(&id) { return StatusCode::NOT_FOUND.into_response(); }
            let minted = state.mints.fetch_add(1, Ordering::SeqCst) + 1;
            Json(serde_json::json!({
                "access_token": format!("delegated-{minted}"), "token_type": "Bearer", "expires_in": 600,
                "user_id": if state.wrong_owner.load(Ordering::SeqCst) { "someone-else" } else { USER },
                "resource": if state.wrong_machine.load(Ordering::SeqCst) { "tidebreak:another-machine" } else { RESOURCE },
            })).into_response()
        }))
        .route("/api/v1/tidebreak/external-delegations/{id}/revoke", post(|State(state): State<Gateway>, Path(id): Path<String>, Json(body): Json<serde_json::Value>| async move {
            assert_eq!(body["client_secret"], "test-machine-secret");
            if state.failed.load(Ordering::SeqCst) { return StatusCode::SERVICE_UNAVAILABLE; }
            state.revokes.fetch_add(1, Ordering::SeqCst);
            state.enrolled.lock().unwrap().remove(&id);
            StatusCode::NO_CONTENT
        }))
        .route("/oauth/token", post(|State(state): State<Gateway>, axum::Form(body): axum::Form<HashMap<String, String>>| async move {
            assert!(body["subject_token"].starts_with("delegated-"), "a browser token must never supply external inference");
            state.exchanges.lock().unwrap().push((body["subject_token"].clone(), body["audience"].clone()));
            let prefix = if body["audience"].starts_with("runtime:") { "runtime" } else { "llm" };
            Json(serde_json::json!({"access_token": format!("{prefix}-{}", body["subject_token"]), "expires_in": 600}))
        }))
        .route("/compat/openai/v1/responses", post(|headers: HeaderMap| async move {
            headers.get("authorization").unwrap().to_str().unwrap().to_owned()
        }))
        .route("/api/v1/tidebreak/git-forge", axum::routing::get(|headers: HeaderMap| async move {
            assert!(headers.get("authorization").unwrap().to_str().unwrap().starts_with("Bearer delegated-"));
            Json(serde_json::json!({"app_name":"forge", "attribution":"person", "acts_as":"slack-owner", "display_name":"Slack Owner", "commit_email":"owner@example.com"}))
        }))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (base, state)
}

fn obo(base: &str) -> Arc<OboGateway> {
    let mut gateway = OboGateway::new(base, RESOURCE.into()).unwrap();
    gateway.machine_credentials = Some(("tidebreak".into(), "test-machine-secret".into()));
    Arc::new(gateway)
}

async fn setup() -> (
    tempfile::TempDir,
    Arc<DbStore>,
    CodeRuntime,
    Arc<OboGateway>,
    String,
    Gateway,
    OwnerId,
) {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("code.db").display()
        ))
        .await
        .unwrap(),
    );
    let (base, state) = gateway().await;
    let gateway = obo(&base);
    let owner = OwnerId::new(&format!("user:{USER}")).unwrap();
    gateway.record_caller(&owner, "browser-owner".into());
    let relay =
        Arc::new(HarnessLlmRelay::new(gateway.clone()).with_external_delegations(db.clone()));
    let runtime = CodeRuntime::new(
        db.clone(),
        dir.path().to_path_buf(),
        None,
        None,
        None,
        None,
        None,
        Some(relay),
    );
    (dir, db, runtime, gateway, base, state, owner)
}

fn approval_lease() -> crate::auth::GatewayAuthLease {
    crate::auth::GatewayAuthLease::for_test(
        crate::principal::Principal::User {
            id: crate::principal::UserId::new(USER).unwrap(),
            kind: crate::principal::PrincipalKind::Person,
            role: crate::principal::Role::Member,
        },
        "browser-owner".into(),
    )
}

async fn connect(runtime: &CodeRuntime, owner: &OwnerId) -> tidebreak_core::CodeExternalGrant {
    let (_, nonce, confirm) = runtime
        .start_connect_handshake("slack", "U1", "T1", "Thet", "Workspace", None)
        .await
        .unwrap();
    let (_, csrf) = runtime
        .view_connect_handshake(owner, &nonce)
        .await
        .unwrap()
        .unwrap();
    runtime
        .approve_connect_handshake(owner, &nonce, &csrf, Some(&approval_lease()))
        .await
        .unwrap()
        .unwrap();
    runtime
        .complete_connect_handshake(&nonce, &confirm)
        .await
        .unwrap()
        .unwrap()
        .0
}

#[tokio::test]
async fn completed_connection_survives_restart_and_refreshes_without_browser_credentials() {
    let (_dir, db, runtime, _browser, base, state, owner) = setup().await;
    _browser.record_caller(&owner, "second-browser-session".into());
    let grant = connect(&runtime, &owner).await;
    let restarted = ExternalDelegations::new(obo(&base), db);
    let first = restarted.for_grant(&owner, grant.id).await.unwrap();
    assert_eq!(first.bearer_for(&owner).await.unwrap(), "llm-delegated-1");
    assert_eq!(
        restarted
            .for_grant(&owner, grant.id)
            .await
            .unwrap()
            .bearer_for(&owner)
            .await
            .unwrap(),
        "llm-delegated-1"
    );
    assert_eq!(state.mints.load(Ordering::SeqCst), 1);
    let slot = restarted
        .slots
        .lock()
        .unwrap()
        .get(&grant.id)
        .unwrap()
        .clone();
    slot.lock().await.as_mut().unwrap().expires_at = 0;
    assert_eq!(
        restarted
            .for_grant(&owner, grant.id)
            .await
            .unwrap()
            .bearer_for(&owner)
            .await
            .unwrap(),
        "llm-delegated-2"
    );
}

#[tokio::test]
async fn browser_credentials_cannot_mask_missing_or_unconfirmed_delegation() {
    let (_dir, db, runtime, browser, _base, state, owner) = setup().await;
    let (unconfirmed, nonce, _) = runtime
        .start_connect_handshake("slack", "U1", "T1", "Thet", "Workspace", None)
        .await
        .unwrap();
    let (_, csrf) = runtime
        .view_connect_handshake(&owner, &nonce)
        .await
        .unwrap()
        .unwrap();
    assert!(runtime
        .approve_connect_handshake(&owner, &nonce, "wrong csrf", Some(&approval_lease()))
        .await
        .unwrap()
        .is_none());
    assert_eq!(state.approvals.load(Ordering::SeqCst), 0);
    assert!(runtime
        .approve_connect_handshake(&owner, &nonce, &csrf, None)
        .await
        .is_err());
    let wrong_owner = crate::auth::GatewayAuthLease::for_test(
        crate::principal::Principal::LocalOwner,
        "browser-owner".into(),
    );
    assert!(runtime
        .approve_connect_handshake(&owner, &nonce, &csrf, Some(&wrong_owner))
        .await
        .is_err());
    assert_eq!(state.approvals.load(Ordering::SeqCst), 0);
    runtime
        .approve_connect_handshake(&owner, &nonce, &csrf, Some(&approval_lease()))
        .await
        .unwrap()
        .unwrap();
    let (grant, _) = runtime
        .mint_adapter_grant(&owner, "slack", "U2", "T1")
        .await
        .unwrap();
    let error = runtime
        .external_get_or_create(
            &owner,
            grant.id,
            "slack",
            "T1/C1/legacy",
            tidebreak_core::RepoId::new(),
            None,
            tidebreak_core::HarnessKind::ClaudeCode,
            crate::code::runtime::NewSessionSettings::default(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "external_reconnect_required");
    assert!(error.message().contains("reconnect"));
    assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    let delegated = ExternalDelegations::new(browser, db);
    assert!(matches!(
        delegated.for_grant(&owner, grant.id).await,
        Err(AgentError::SignInRequired(_))
    ));
    assert_eq!(state.mints.load(Ordering::SeqCst), 0);
    assert!(unconfirmed.grant_id.is_none());
    let completed = connect(&runtime, &owner).await;
    state.enrolled.lock().unwrap().clear();
    assert!(matches!(
        delegated.for_grant(&owner, completed.id).await,
        Err(AgentError::SignInRequired(_))
    ));
}

#[tokio::test]
async fn revocation_denies_cached_authority_and_retries_after_restart() {
    let (_dir, db, runtime, browser, base, state, owner) = setup().await;
    let grant = connect(&runtime, &owner).await;
    let delegated = ExternalDelegations::new(browser, db.clone());
    delegated
        .for_grant(&owner, grant.id)
        .await
        .unwrap()
        .bearer_for(&owner)
        .await
        .unwrap();
    state.failed.store(true, Ordering::SeqCst);
    runtime
        .revoke_adapter_grant(&owner, grant.id, "owner disconnected Slack")
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        delegated.for_grant(&owner, grant.id).await,
        Err(AgentError::SignInRequired(_))
    ));
    assert_eq!(state.revokes.load(Ordering::SeqCst), 0);
    state.failed.store(false, Ordering::SeqCst);
    let restarted = ExternalDelegations::new(obo(&base), db);
    restarted.reconcile_revocations().await.unwrap();
    assert_eq!(state.revokes.load(Ordering::SeqCst), 1);
    restarted.reconcile_revocations().await.unwrap();
    assert_eq!(state.revokes.load(Ordering::SeqCst), 1);
    assert!(matches!(
        restarted.for_grant(&owner, grant.id).await,
        Err(AgentError::SignInRequired(_))
    ));
}

#[tokio::test]
async fn grant_and_gateway_responses_cannot_cross_owner_or_machine() {
    let (_dir, db, runtime, browser, _base, state, owner) = setup().await;
    let grant = connect(&runtime, &owner).await;
    let delegated = ExternalDelegations::new(browser, db);
    assert!(matches!(
        delegated.for_grant(&OwnerId::local(), grant.id).await,
        Err(AgentError::SignInRequired(_))
    ));
    assert_eq!(state.mints.load(Ordering::SeqCst), 0);
    state.wrong_owner.store(true, Ordering::SeqCst);
    assert!(matches!(
        delegated.for_grant(&owner, grant.id).await,
        Err(AgentError::InvalidTarget(_))
    ));
    state.wrong_owner.store(false, Ordering::SeqCst);
    state.wrong_machine.store(true, Ordering::SeqCst);
    assert!(matches!(
        delegated.for_grant(&owner, grant.id).await,
        Err(AgentError::InvalidTarget(_))
    ));
}

#[tokio::test]
async fn first_relay_request_after_restart_uses_the_persisted_session_grant() {
    use crate::code::harness_llm::{HarnessLlmSubject, RelayEndpoint};
    use tidebreak_core::{
        Attention, AttentionSource, ExecutionLocation, HarnessKind, PermissionMode, Session,
        SessionKind, SessionLifecycle, SessionVisibility,
    };
    let (_dir, db, runtime, _browser, base, state, owner) = setup().await;
    let grant = connect(&runtime, &owner).await;
    let session = Session {
        id: SessionId::new(),
        owner: owner.clone(),
        owner_kind: None,
        workspace_id: None,
        kind: SessionKind::Interactive,
        harness_kind: HarnessKind::ClaudeCode,
        harness_version: None,
        harness_resume_ref: None,
        permission_mode: PermissionMode::default(),
        model: None,
        reasoning_effort: None,
        fast_mode: false,
        lifecycle: SessionLifecycle::Idle,
        fence_reason: None,
        child_pid: None,
        child_process_identity: None,
        spawn_epoch: 1,
        attention: Attention::working(AttentionSource::Lifecycle),
        unrecognized_event_count: 0,
        subagents: Vec::new(),
        created_at: chrono::Utc::now(),
        visibility: SessionVisibility::Private,
        execution_location: ExecutionLocation::Machine,
        acts_as: None,
    };
    let resolution = tidebreak_core::db::code::resolve_external_machine_session(
        &db,
        &owner,
        grant.id,
        "slack",
        "T1/C1/1.1",
        &session,
    )
    .await
    .unwrap();
    assert!(matches!(
        resolution,
        tidebreak_core::ExternalSessionResolution::Created(_)
    ));
    let loser = Session {
        id: SessionId::new(),
        ..session.clone()
    };
    let resolution = tidebreak_core::db::code::resolve_external_machine_session(
        &db,
        &owner,
        grant.id,
        "slack",
        "T1/C1/1.1",
        &loser,
    )
    .await
    .unwrap();
    assert!(matches!(
        resolution,
        tidebreak_core::ExternalSessionResolution::Existing(_)
    ));
    assert!(tidebreak_core::db::code::get_session(&db, &owner, loser.id)
        .await
        .unwrap()
        .is_none());
    let restarted = HarnessLlmRelay::new(obo(&base)).with_external_delegations(db.clone());
    let key = restarted.issue(HarnessLlmSubject {
        owner: owner.clone(),
        session: session.id,
    });
    let mut headers = HeaderMap::new();
    headers.insert("authorization", format!("Bearer {key}").parse().unwrap());
    let response = restarted
        .forward(
            RelayEndpoint::OpenAiResponses,
            &headers,
            None,
            axum::body::Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    assert_eq!(body.as_ref(), b"Bearer llm-delegated-1");
    assert_eq!(state.mints.load(Ordering::SeqCst), 1);
    runtime
        .revoke_adapter_grant(&owner, grant.id, "disconnect")
        .await
        .unwrap();
    let response = restarted
        .forward(
            RelayEndpoint::OpenAiResponses,
            &headers,
            None,
            axum::body::Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn first_external_worker_after_restart_names_the_owner_before_workspace_setup() {
    use tidebreak_core::{CodeRepo, HarnessKind, RepoId};
    use tidebreak_harness::AdapterRegistry;
    let (dir, db, runtime, _browser, base, _state, owner) = setup().await;
    let grant = connect(&runtime, &owner).await;
    let repo_root = dir.path().join("source");
    std::fs::create_dir_all(&repo_root).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo_root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let repo = CodeRepo {
        id: RepoId::new(),
        owner: owner.clone(),
        root_path: repo_root.display().to_string(),
        display_name: "Source".into(),
        default_base_ref: "main".into(),
        branch_prefix: "test/".into(),
        setup_script: Some("git var GIT_AUTHOR_IDENT > setup-author.txt".into()),
        archive_script: None,
        quick_actions: Vec::new(),
        created_at: chrono::Utc::now(),
        removed_at: None,
        cloned_from: None,
        origin_host: Some("github.com".into()),
        origin_owner: Some("example".into()),
        origin_name: Some("source".into()),
    };
    tidebreak_core::db::code::insert_repo(&db, &repo)
        .await
        .unwrap();
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(
        crate::scripted_harness::ScriptedAdapter::new(crate::scripted_harness::plain_text_script())
            .with_approvals(tidebreak_core::CapLevel::Supported),
    ));
    let relay = Arc::new(HarnessLlmRelay::new(obo(&base)).with_external_delegations(db.clone()));
    let restarted = Arc::new(
        CodeRuntime::with_registry(db, dir.path().to_path_buf(), registry).with_harness_llm(relay),
    );
    restarted.start("http://127.0.0.1:1".into()).await.unwrap();
    let result = restarted
        .external_get_or_create(
            &owner,
            grant.id,
            "slack",
            "T1/C1/restarted",
            repo.id,
            Some("From Slack".into()),
            HarnessKind::ClaudeCode,
            crate::code::runtime::NewSessionSettings::default(),
            None,
        )
        .await
        .unwrap();
    let tidebreak_core::ExternalSessionResolution::Created(binding) = result else {
        panic!("expected a new external session");
    };
    let session = restarted
        .get_session(&owner, binding.session_id)
        .await
        .unwrap();
    assert!(
        session.spawn_epoch > 0,
        "the worker must attach after the binding commits"
    );
    let workspace = restarted
        .get_workspace(&owner, session.workspace_id.unwrap())
        .await
        .unwrap();
    let author = std::fs::read_to_string(
        std::path::Path::new(&workspace.worktree_path).join("setup-author.txt"),
    )
    .unwrap();
    assert!(
        author.starts_with("Slack Owner <owner@example.com>"),
        "the setup script must run with delegated Git identity: {author}"
    );
}

async fn bind_runtime_session(
    db: &DbStore,
    owner: &OwnerId,
    grant: CodeGrantId,
    workspace: Option<tidebreak_core::WorkspaceId>,
) -> SessionId {
    use tidebreak_core::{
        Attention, AttentionSource, ExecutionLocation, HarnessKind, PermissionMode, Session,
        SessionKind, SessionLifecycle, SessionVisibility,
    };
    let session = Session {
        id: SessionId::new(),
        owner: owner.clone(),
        owner_kind: None,
        workspace_id: workspace,
        kind: SessionKind::Interactive,
        harness_kind: HarnessKind::ClaudeCode,
        harness_version: None,
        harness_resume_ref: None,
        permission_mode: PermissionMode::Ask,
        model: None,
        reasoning_effort: None,
        fast_mode: false,
        lifecycle: SessionLifecycle::Idle,
        fence_reason: None,
        child_pid: None,
        child_process_identity: None,
        spawn_epoch: 1,
        attention: Attention::working(AttentionSource::Lifecycle),
        unrecognized_event_count: 0,
        subagents: Vec::new(),
        created_at: chrono::Utc::now(),
        visibility: SessionVisibility::Private,
        execution_location: if workspace.is_some() {
            ExecutionLocation::Sandbox
        } else {
            ExecutionLocation::Machine
        },
        acts_as: None,
    };
    tidebreak_core::db::code::resolve_external_machine_session(
        db,
        owner,
        grant,
        "slack",
        "T1/C1/runtime",
        &session,
    )
    .await
    .unwrap();
    session.id
}

#[tokio::test]
async fn runtime_tokens_follow_the_session_grant_after_restart_refresh_and_revocation() {
    use crate::code::remote::{RemoteSandboxError, RuntimeTokenSource};
    let (_dir, db, runtime, _browser, base, state, owner) = setup().await;
    let grant = connect(&runtime, &owner).await;
    let session = bind_runtime_session(&db, &owner, grant.id, None).await;
    let restarted_gateway = obo(&base);
    let tokens = restarted_gateway
        .runtime_tokens("tidebreak")
        .with_external_delegations(db.clone());
    let first = tokens.runtime_token(&owner, session).await.unwrap();
    assert_eq!(first.secret, "runtime-delegated-1");
    assert_eq!(
        tokens.runtime_token(&owner, session).await.unwrap().secret,
        first.secret
    );
    assert_eq!(
        state.exchanges.lock().unwrap().as_slice(),
        &[("delegated-1".into(), "runtime:tidebreak".into())]
    );
    // A refreshed delegation invalidates the cached runtime token even while
    // that runtime token is fresh.
    let slot = tokens
        .external
        .as_ref()
        .unwrap()
        .slots
        .lock()
        .unwrap()
        .get(&grant.id)
        .unwrap()
        .clone();
    slot.lock().await.as_mut().unwrap().expires_at = 0;
    assert_eq!(
        tokens.runtime_token(&owner, session).await.unwrap().secret,
        "runtime-delegated-2"
    );
    // A browser sign-in cannot mask a revoked connection or replace its grant.
    restarted_gateway.record_caller(&owner, "replacement-browser-session".into());
    runtime
        .revoke_adapter_grant(&owner, grant.id, "disconnect")
        .await
        .unwrap();
    let _replacement = connect(&runtime, &owner).await;
    assert!(matches!(
        tokens.runtime_token(&owner, session).await,
        Err(RemoteSandboxError::SignInRequired(_))
    ));
    assert_eq!(state.exchanges.lock().unwrap().len(), 2);
}

// The map and this test own two references. A third means the request has
// cloned the slot and is waiting for the lock that the test holds.
async fn wait_for_slot_waiter<T>(slot: &Arc<T>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while Arc::strong_count(slot) < 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the request must reach the held credential slot");
}

#[tokio::test]
async fn runtime_tokens_refuse_missing_and_wrong_owner_sessions_before_browser_fallback() {
    use crate::code::remote::{RemoteSandboxError, RuntimeTokenSource};
    let (_dir, db, runtime, browser, _base, state, owner) = setup().await;
    let grant = connect(&runtime, &owner).await;
    let session = bind_runtime_session(&db, &owner, grant.id, None).await;
    let other = OwnerId::new("user:another-owner").unwrap();
    browser.record_caller(&other, "another-browser-session".into());
    let tokens = browser
        .runtime_tokens("tidebreak")
        .with_external_delegations(db);
    for (caller, target) in [(&owner, SessionId::new()), (&other, session)] {
        assert!(matches!(
            tokens.runtime_token(caller, target).await,
            Err(RemoteSandboxError::Refused {
                operation: "token",
                ..
            })
        ));
    }
    assert_eq!(state.mints.load(Ordering::SeqCst), 0);
    assert!(state.exchanges.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cached_delegation_refuses_revocation_while_waiting_for_its_slot() {
    let (_dir, db, runtime, _browser, base, state, owner) = setup().await;
    let grant = connect(&runtime, &owner).await;
    let delegations = Arc::new(ExternalDelegations::new(obo(&base), db));
    delegations.for_grant(&owner, grant.id).await.unwrap();
    let slot = delegations
        .slots
        .lock()
        .unwrap()
        .get(&grant.id)
        .unwrap()
        .clone();
    let held = slot.lock().await;
    let caller = owner.clone();
    let waiting = {
        let delegations = delegations.clone();
        tokio::spawn(async move { delegations.for_grant(&caller, grant.id).await })
    };
    wait_for_slot_waiter(&slot).await;
    runtime
        .revoke_adapter_grant(&owner, grant.id, "disconnect")
        .await
        .unwrap();
    drop(held);
    assert!(matches!(
        waiting.await.unwrap(),
        Err(AgentError::SignInRequired(_))
    ));
    assert_eq!(state.mints.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cached_runtime_token_refuses_revocation_while_waiting_for_its_slot() {
    use crate::code::remote::{RemoteSandboxError, RuntimeTokenSource};
    let (_dir, db, runtime, _browser, base, state, owner) = setup().await;
    let grant = connect(&runtime, &owner).await;
    let session = bind_runtime_session(&db, &owner, grant.id, None).await;
    let tokens = obo(&base)
        .runtime_tokens("tidebreak")
        .with_external_delegations(db);
    tokens.runtime_token(&owner, session).await.unwrap();
    let slot = tokens
        .slots
        .lock()
        .unwrap()
        .get(&(owner.clone(), session))
        .unwrap()
        .clone();
    let held = slot.lock().await;
    let caller = owner.clone();
    let waiting = {
        let tokens = tokens.clone();
        tokio::spawn(async move { tokens.runtime_token(&caller, session).await })
    };
    wait_for_slot_waiter(&slot).await;
    runtime
        .revoke_adapter_grant(&owner, grant.id, "disconnect")
        .await
        .unwrap();
    drop(held);
    assert!(matches!(
        waiting.await.unwrap(),
        Err(RemoteSandboxError::SignInRequired(_))
    ));
    assert_eq!(state.exchanges.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn remote_workspace_status_keeps_the_original_grant_after_restart_and_revocation() {
    use tidebreak_core::{CodeRepo, CodeWorkspace, CodeWorkspaceStatus, RepoId, WorkspaceId};
    let (dir, db, runtime, browser, base, state, owner) = setup().await;
    let grant = connect(&runtime, &owner).await;
    let repo = CodeRepo {
        id: RepoId::new(),
        owner: owner.clone(),
        root_path: String::new(),
        display_name: "Tools".into(),
        default_base_ref: "main".into(),
        branch_prefix: "thet/".into(),
        setup_script: None,
        archive_script: None,
        quick_actions: Vec::new(),
        created_at: chrono::Utc::now(),
        removed_at: None,
        cloned_from: Some("https://github.com/acme/tools".into()),
        origin_host: Some("github.com".into()),
        origin_owner: Some("acme".into()),
        origin_name: Some("tools".into()),
    };
    tidebreak_core::db::code::insert_repo(&db, &repo)
        .await
        .unwrap();
    let id = WorkspaceId::new();
    let workspace = CodeWorkspace {
        id,
        owner: owner.clone(),
        repo_id: repo.id,
        title: "Slack work".into(),
        worktree_path: CodeWorkspace::remote_worktree_marker(id),
        branch_name: "thet/slack-work".into(),
        base_ref: "main".into(),
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
    bind_runtime_session(&db, &owner, grant.id, Some(id)).await;
    let relay = Arc::new(HarnessLlmRelay::new(obo(&base)).with_external_delegations(db.clone()));
    let restarted = CodeRuntime::new(
        db,
        dir.path().to_path_buf(),
        None,
        None,
        None,
        None,
        None,
        Some(relay),
    )
    .with_git_credentials(browser.clone());
    let status = restarted.workspace_pr(&owner, id).await.unwrap();
    assert_eq!(status.pushes_as.as_deref(), Some("slack-owner"));
    assert_eq!(status.pushes_as_self, Some(true));
    assert_eq!(state.mints.load(Ordering::SeqCst), 1);
    browser.record_caller(&owner, "replacement-browser-session".into());
    runtime
        .revoke_adapter_grant(&owner, grant.id, "disconnect")
        .await
        .unwrap();
    let _replacement = connect(&runtime, &owner).await;
    assert!(
        restarted.workspace_pr(&owner, id).await.is_err(),
        "status must refuse the original revoked grant"
    );
    assert!(
        restarted.refresh_workspace_pr(&owner, id).await.is_err(),
        "refresh must refuse before borrowing browser credentials"
    );
    assert!(
        restarted.workspace_pr_comments(&owner, id).await.is_err(),
        "comments must refuse before borrowing browser credentials"
    );
    assert_eq!(state.mints.load(Ordering::SeqCst), 1);
}
