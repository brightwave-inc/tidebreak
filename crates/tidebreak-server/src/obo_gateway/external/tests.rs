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
        .route("/oauth/token", post(|axum::Form(body): axum::Form<HashMap<String, String>>| async move {
            assert!(body["subject_token"].starts_with("delegated-"), "a browser token must never supply external inference");
            Json(serde_json::json!({"access_token": format!("llm-{}", body["subject_token"]), "expires_in": 600}))
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
