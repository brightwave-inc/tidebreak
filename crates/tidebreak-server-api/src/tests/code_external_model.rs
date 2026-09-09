//! Repository-free Slack sessions freeze the connected grant's model before inference.

use super::*;

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Json};
use tidebreak_core::{CodeExternalGrant, OwnerId};
use tidebreak_harness::AdapterRegistry;

use crate::code::harness_llm::HarnessLlmRelay;
use crate::code::CodeRuntime;
use crate::engine::internal::InternalAdapter;
use crate::obo_gateway::OboGateway;
use crate::resolver::ConfiguredResolver;

const USER: &str = "26fecc98-0998-4f3b-a302-cafce9b8dd68";
const RESOURCE: &str = "tidebreak:external-model-test";
const BROWSER: &str = "browser-session-for-external-model-test";
const DELEGATED: &str = "delegated-session-for-external-model-test";

#[derive(Clone, Default)]
struct GatewayCalls {
    empty_catalog: Arc<AtomicBool>,
    exchanges: Arc<Mutex<Vec<String>>>,
    inference: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
}

async fn model_gateway(empty_catalog: bool) -> (String, GatewayCalls) {
    let calls = GatewayCalls::default();
    calls.empty_catalog.store(empty_catalog, Ordering::SeqCst);
    let router = Router::new()
        .route(
            "/api/v1/tidebreak/external-delegations",
            post(|headers: HeaderMap, Json(body): Json<serde_json::Value>| async move {
                assert_eq!(headers[header::AUTHORIZATION], format!("Bearer {BROWSER}"));
                assert_eq!(body["resource"], RESOURCE);
                Json(serde_json::json!({
                    "delegation_id": body["delegation_id"], "resource": RESOURCE, "user_id": USER,
                }))
            }),
        )
        .route(
            "/api/v1/tidebreak/external-delegations/{id}/token",
            post(|Path(_id): Path<String>, Json(body): Json<serde_json::Value>| async move {
                assert_eq!(body["client_id"], "tidebreak-test");
                assert_eq!(body["client_secret"], "external-model-fixture-secret");
                assert_eq!(body["resource"], RESOURCE);
                Json(serde_json::json!({
                    "access_token": DELEGATED, "token_type": "Bearer", "expires_in": 600,
                    "user_id": USER, "resource": RESOURCE,
                }))
            }),
        )
        .route(
            "/api/v1/tidebreak/external-delegations/{id}/revoke",
            post(|| async { StatusCode::NO_CONTENT }),
        )
        .route(
            "/oauth/token",
            post(|State(calls): State<GatewayCalls>, Form(body): Form<HashMap<String, String>>| async move {
                let subject = body.get("subject_token").unwrap();
                calls.exchanges.lock().unwrap().push(subject.clone());
                Json(serde_json::json!({
                    "access_token": format!("{}:{subject}", body["audience"]),
                    "token_type": "Bearer", "expires_in": 600,
                }))
            }),
        )
        .route(
            "/api/v1/me/catalog",
            get(|State(calls): State<GatewayCalls>, headers: HeaderMap| async move {
                let bearer = headers[header::AUTHORIZATION].to_str().unwrap();
                let models = if calls.empty_catalog.load(Ordering::SeqCst) {
                    Vec::new()
                } else if bearer.ends_with(DELEGATED) {
                    vec!["grant-default", "grant-selected"]
                } else {
                    assert!(bearer.ends_with(BROWSER));
                    vec!["browser-only"]
                };
                Json(serde_json::json!({
                    "models": models.into_iter().map(|id| serde_json::json!({
                        "id": id, "name": id, "protocols": ["anthropic_messages"],
                        "aliases": [], "supports_tools": true, "supports_vision": false,
                        "context_window": 200_000, "max_output_tokens": 8_000,
                        "provider_name": "Anthropic",
                    })).collect::<Vec<_>>(),
                    "apps": [],
                }))
            }),
        )
        .route(
            "/compat/anthropic/v1/messages",
            post(|State(calls): State<GatewayCalls>, headers: HeaderMap, Json(body): Json<serde_json::Value>| async move {
                let bearer = headers.get(header::AUTHORIZATION)
                    .or_else(|| headers.get("x-api-key")).unwrap().to_str().unwrap().to_owned();
                calls.inference.lock().unwrap().push((bearer, body.clone()));
                let events = [
                    ("message_start", serde_json::json!({"type":"message_start","message":{"id":"msg-external-model","type":"message","role":"assistant","model":body["model"],"content":[],"usage":{"input_tokens":10,"output_tokens":0}}})),
                    ("content_block_start", serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}})),
                    ("content_block_delta", serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"The grant model answered."}})),
                    ("content_block_stop", serde_json::json!({"type":"content_block_stop","index":0})),
                    ("message_delta", serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":6}})),
                    ("message_stop", serde_json::json!({"type":"message_stop"})),
                ];
                let body = events.into_iter().map(|(event, data)| format!("event: {event}
data: {data}

")).collect::<String>();
                ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
            }),
        )
        .with_state(calls.clone());
    let address = super::code::serve(router).await;
    (format!("http://{address}"), calls)
}

struct CapturingResolver {
    configured: ConfiguredResolver,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
}

struct CapturingProvider {
    inner: Arc<dyn ModelProvider>,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
}

#[async_trait]
impl ModelProvider for CapturingProvider {
    fn id(&self) -> ProviderId {
        self.inner.id()
    }

    async fn stream(
        &self,
        request: ChatRequest,
    ) -> tidebreak_core::Result<BoxStream<'static, ProviderEvent>> {
        self.requests.lock().unwrap().push(request.clone());
        self.inner.stream(request).await
    }
}

#[async_trait]
impl ProviderResolver for CapturingResolver {
    async fn resolve(&self) -> Arc<dyn ModelProvider> {
        self.configured.resolve().await
    }

    async fn resolve_for(&self, owner: Option<&OwnerId>) -> Arc<dyn ModelProvider> {
        self.configured.resolve_for(owner).await
    }

    async fn resolve_for_session(
        &self,
        owner: Option<&OwnerId>,
        session: SessionId,
    ) -> Arc<dyn ModelProvider> {
        Arc::new(CapturingProvider {
            inner: self.configured.resolve_for_session(owner, session).await,
            requests: self.requests.clone(),
        })
    }

    async fn session_gateway_snapshot(
        &self,
        owner: Option<&OwnerId>,
        session: SessionId,
    ) -> tidebreak_core::Result<Option<providers::GatewayModelSnapshot>> {
        self.configured
            .session_gateway_snapshot(owner, session)
            .await
    }

    fn enforces_model_registry(&self) -> bool {
        true
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    state: AppState,
    runtime: Arc<CodeRuntime>,
    router: Router,
    gateway: Arc<OboGateway>,
    calls: GatewayCalls,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    grant: CodeExternalGrant,
    bearer: String,
    worker: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

async fn fixture(empty_catalog: bool) -> Fixture {
    let (directory, db) = temp_db_store("external-model.db").await;
    let db = Arc::new(db);
    let store: Arc<dyn Store> = db.clone();
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    // Explicit provider configuration prevents ambient credentials from adding routes.
    for kind in providers::ProviderKind::ALL {
        if *kind != providers::ProviderKind::ModelGateway {
            providers::write_config(&*store, *kind, &providers::ProviderConfig::disabled())
                .await
                .unwrap();
        }
    }
    let (base, calls) = model_gateway(empty_catalog).await;
    let gateway = Arc::new(
        OboGateway::new(&base, RESOURCE.into())
            .unwrap()
            .with_machine_credentials_for_test("tidebreak-test", "external-model-fixture-secret"),
    );
    let owner = OwnerId::new(&format!("user:{USER}")).unwrap();
    gateway.record_caller(&owner, BROWSER.into());
    let provisioned = crate::managed_policy::MemoryProvisionedPolicy::new();
    let os: Arc<dyn crate::managed_policy::OsPolicySource> =
        Arc::new(crate::managed_policy::NoOsPolicy);
    let gateway_runtime = crate::gateway_runtime::GatewayRuntime::new(
        store.clone(),
        secrets.clone(),
        provisioned.clone(),
        os.clone(),
    );
    let chatgpt = Arc::new(
        crate::chatgpt_runtime::ChatGptRuntime::new(store.clone(), secrets.clone()).unwrap(),
    );
    let configured = ConfiguredResolver::new(
        store.clone(),
        secrets.clone(),
        gateway_runtime,
        chatgpt,
        provisioned,
        os,
    )
    .with_on_behalf_of_gateway(Some(gateway.clone()))
    .with_external_delegations(db.clone());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut state = AppState::new(
        Config::desktop(directory.path()),
        store,
        Arc::new(CapturingResolver {
            configured,
            requests: requests.clone(),
        }),
        secrets,
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "model_gateway::grant-default".into(),
            ..AgentConfig::default()
        },
    )
    .with_on_behalf_of_gateway(Some(gateway.clone()));
    let relay =
        Arc::new(HarnessLlmRelay::new(gateway.clone()).with_external_delegations(db.clone()));
    let mut runtime =
        CodeRuntime::with_registry(db, directory.path().to_path_buf(), AdapterRegistry::new())
            .with_harness_llm(relay);
    runtime.adapters.register(Arc::new(InternalAdapter::new(
        state.clone(),
        runtime.db.clone(),
        runtime.bus.clone(),
        tidebreak_core::AgentRunExecutionLocation::InProcess,
    )));
    state.events.mirror_into(runtime.bus.clone());
    let runtime = Arc::new(runtime);
    state.code = Some(runtime.clone());
    let (_, nonce, confirm) = runtime
        .start_connect_handshake("slack", "U1", "T1", "Test member", "Test workspace", None)
        .await
        .unwrap();
    let (_, csrf) = runtime
        .view_connect_handshake(&owner, &nonce)
        .await
        .unwrap()
        .unwrap();
    let lease = crate::auth::GatewayAuthLease::for_test(
        crate::principal::Principal::User {
            id: crate::principal::UserId::new(USER).unwrap(),
            kind: crate::principal::PrincipalKind::Person,
            role: crate::principal::Role::Member,
        },
        BROWSER.into(),
    );
    runtime
        .approve_connect_handshake(&owner, &nonce, &csrf, Some(&lease))
        .await
        .unwrap()
        .unwrap();
    let (grant, tokens) = runtime
        .complete_connect_handshake(&nonce, &confirm)
        .await
        .unwrap()
        .unwrap();
    let worker = crate::engine::internal::leg::LegDriver::new(
        state.store.clone(),
        state.resolver.clone(),
        state.secrets.clone(),
        state.provisioned_policy.clone(),
        state.os_policy.clone(),
        state.tools.clone(),
        state.approvals.clone(),
        state.events.clone(),
        state.active_turns.clone(),
        state.turn_job_wake.clone(),
        state.agent_run_wake.clone(),
        state.queued_turn_wake.clone(),
        state.agent_config.clone(),
        None,
        crate::engine::internal::leg::LegDriverConfig::default(),
    )
    .with_on_behalf_of_gateway(Some(gateway.clone()));
    Fixture {
        router: app(state.clone()),
        state,
        runtime,
        gateway,
        calls,
        requests,
        grant,
        bearer: tokens.token,
        _directory: directory,
        worker: tokio::spawn(worker.run()),
    }
}

async fn post_external(
    fixture: &Fixture,
    path: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = fixture
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {}", fixture.bearer))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn repository_free_external_sessions_run_the_default_grant_model_and_refuse_revocation() {
    let fixture = fixture(false).await;
    let mut last_session = None;
    for (key, selection, expected_wire_model) in [
        ("T1/D1/default", None, "grant-default"),
        (
            "T1/D1/selected",
            Some("model_gateway::grant-selected"),
            "grant-selected",
        ),
    ] {
        crate::model_roles::write_selection(
            &*fixture.state.store,
            crate::model_roles::ModelRole::Chat,
            selection,
        )
        .await
        .unwrap();
        let (status, body) = post_external(
            &fixture,
            "/external/code/sessions",
            serde_json::json!({"external_key":key}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let session_id: SessionId = serde_json::from_value(body["session_id"].clone()).unwrap();
        let session = fixture
            .runtime
            .get_session(&fixture.grant.owner, session_id)
            .await
            .unwrap();
        assert!(session.workspace_id.is_none());
        assert_eq!(session.harness_kind, tidebreak_core::HarnessKind::Internal);
        let model = session
            .model
            .as_deref()
            .expect("repository-free creation records its model");
        assert!(
            model.starts_with("model_gateway::__tidebreak_gateway_v1."),
            "{model}"
        );
        let snapshot = fixture
            .state
            .resolver
            .session_gateway_snapshot(Some(&fixture.grant.owner), session_id)
            .await
            .unwrap()
            .unwrap();
        let policy =
            providers::resolve_model_policy(&*fixture.state.store, model, false, Some(&snapshot))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(policy.id, expected_wire_model);
        let (status, accepted) = post_external(
            &fixture,
            &format!("/external/code/sessions/{session_id}/messages"),
            serde_json::json!({
                "text":"Reply with a short acknowledgment.", "event_id":key, "channel_ts":"1.1",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{accepted}");
        let turn_id: TurnId = serde_json::from_value(accepted["turn_id"].clone()).unwrap();
        let turn = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let turn = fixture
                    .state
                    .store
                    .get_turn(turn_id)
                    .await
                    .unwrap()
                    .unwrap();
                match turn.status {
                    TurnRunStatus::Completed => break turn,
                    TurnRunStatus::Failed => panic!("the default model turn failed: {turn:?}"),
                    _ => tokio::time::sleep(Duration::from_millis(10)).await,
                }
            }
        })
        .await
        .expect("the delegated model completes the external turn");
        assert_eq!(turn.model, model);
        let requests = fixture.requests.lock().unwrap();
        let request = requests
            .iter()
            .find(|request| request.conversation == Some(session_id))
            .expect("the internal engine reached the configured resolver");
        assert_eq!(request.model, policy.route_model);
        assert!(!request.model.is_empty());
        assert_eq!(
            request
                .provider
                .as_ref()
                .map(|provider| provider.0.as_str()),
            Some("model_gateway")
        );
        let inference = fixture.calls.inference.lock().unwrap();
        assert!(inference
            .iter()
            .any(|(bearer, body)| bearer.ends_with(DELEGATED)
                && body["model"] == expected_wire_model));
        last_session = Some(session_id);
    }
    let session_id = last_session.unwrap();
    let cached_provider = fixture
        .state
        .resolver
        .resolve_for_session(Some(&fixture.grant.owner), session_id)
        .await;
    fixture
        .gateway
        .record_caller(&fixture.grant.owner, BROWSER.into());
    fixture
        .runtime
        .revoke_adapter_grant(&fixture.grant.owner, fixture.grant.id, "test disconnect")
        .await
        .unwrap();
    let exchanges_before = fixture.calls.exchanges.lock().unwrap().len();
    let inference_before = fixture.calls.inference.lock().unwrap().len();
    assert!(
        crate::routes::code::resolve_external_model(&fixture.state, &fixture.grant)
            .await
            .is_err(),
        "a revoked grant cannot resolve through the browser catalog"
    );
    let request = fixture.requests.lock().unwrap().last().unwrap().clone();
    assert!(
        cached_provider.stream(request).await.is_err(),
        "a previously resolved provider must recheck the original grant"
    );
    assert_eq!(
        fixture.calls.exchanges.lock().unwrap().len(),
        exchanges_before
    );
    assert_eq!(
        fixture.calls.inference.lock().unwrap().len(),
        inference_before
    );
    assert!(
        fixture
            .calls
            .exchanges
            .lock()
            .unwrap()
            .iter()
            .all(|subject| subject == DELEGATED),
        "external inference and catalog reads never borrow the browser session"
    );
}

#[tokio::test]
async fn repository_free_external_creation_refuses_an_empty_grant_catalog() {
    let fixture = fixture(true).await;
    let (status, body) = post_external(
        &fixture,
        "/external/code/sessions",
        serde_json::json!({"external_key":"T1/D1/no-models"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["kind"], "model_provider_unavailable");
    assert!(fixture.calls.inference.lock().unwrap().is_empty());
    assert!(fixture
        .calls
        .exchanges
        .lock()
        .unwrap()
        .iter()
        .all(|subject| subject == DELEGATED));
    let sessions =
        tidebreak_core::db::code::list_sessions(&fixture.runtime.db, &fixture.grant.owner)
            .await
            .unwrap();
    assert!(
        sessions.is_empty(),
        "refused model admission must not create a conversation"
    );
}
