//! Gateway runtime tests against a fake gateway.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::extract::{Form, State};
use axum::http::{HeaderMap, Request};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::{Extension, Router as AxumRouter};
use futures::StreamExt;
use serde_json::{json, Value};
use tidebreak_core::id::{AppId, AppRevisionId};
use tidebreak_core::local_app::{
    AppGatewayOperationsGrantBinding, AppGrant, AppGrantBinding, AppManifest, CreateApp,
    NewAppRevision,
};
use tidebreak_core::{
    AgentConfig, ChatMessage, ChatRequest, Config, DbStore, ModelProvider, OwnerId, ProviderId,
    Role, ToolRegistry,
};
use tower::ServiceExt;

use super::relay::shared_app_invoke_body;
use super::*;

/// The relay body is the gateway's `proxy_api` vocabulary: absent halves
/// must be absent keys, never null — the gateway's `serde(default)`
/// argument maps refuse an explicit null, so a regression here fails
/// every relayed call while passing any test that fakes the dispatcher.
#[test]
fn invoke_body_omits_absent_argument_halves() {
    let bare = shared_app_invoke_body(&crate::connected_apps::GatewayOperationRequest {
        gateway_app: "app-1".into(),
        operation_id: "listMonitors".into(),
        path_parameters: None,
        query: None,
        body: None,
    });
    assert_eq!(
        bare,
        json!({"connected_app_id": "app-1", "operation_id": "listMonitors"})
    );

    let full = shared_app_invoke_body(&crate::connected_apps::GatewayOperationRequest {
        gateway_app: "app-1".into(),
        operation_id: "getMonitor".into(),
        path_parameters: Some(json!({"monitor_id": "7"})),
        query: Some(json!({"verbose": true})),
        body: Some(json!({"note": "hi"})),
    });
    assert_eq!(
        full,
        json!({
            "connected_app_id": "app-1",
            "operation_id": "getMonitor",
            "path_parameters": {"monitor_id": "7"},
            "query": {"verbose": true},
            "body": {"note": "hi"},
        })
    );
}

#[derive(Default)]
struct MockSecrets(std::sync::Mutex<HashMap<String, String>>);

#[async_trait]
impl SecretProvider for MockSecrets {
    async fn get_secret(&self, key: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
        self.0.lock().unwrap().insert(key.into(), value.into());
        Ok(())
    }
    async fn delete_secret(&self, key: &str) -> Result<()> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}

#[derive(Default)]
struct FakeGateway {
    refreshes: AtomicUsize,
    /// Number of metadata reads completed. Tests can delay the machine
    /// offer until a later read to model a rolling Gateway deployment.
    meta_reads: AtomicUsize,
    machine_offer_after_reads: AtomicUsize,
    /// Every refresh token posted to `/oauth/revoke`, in order.
    revoked: std::sync::Mutex<Vec<String>>,
    /// When set, `/api/v1/cli/apps` answers 500 — the outage shape the
    /// endpoint-mount reconcile must degrade quietly on.
    apps_fail: std::sync::atomic::AtomicBool,
    /// `(resource, attestation_context_id)` per refresh grant, in order.
    minted: std::sync::Mutex<Vec<(String, Option<String>)>>,
    /// `(method, authorization)` per MCP endpoint request, in order.
    mcp_requests: std::sync::Mutex<Vec<(String, String)>>,
    /// When set, `/api/v1/me/catalog` serves this body under a fixed
    /// `ETag`; when `None` the route answers 404, the shape of a gateway
    /// that predates the member catalog.
    catalog: std::sync::Mutex<Option<Value>>,
    /// How many catalog fetches arrived with a matching `If-None-Match`
    /// and were answered 304.
    catalog_not_modified: AtomicUsize,
}

struct RepointableOs(std::sync::Mutex<String>);

impl RepointableOs {
    fn new(base_url: String) -> Self {
        Self(std::sync::Mutex::new(base_url))
    }

    fn repoint(&self, base_url: &str) {
        *self.0.lock().unwrap() = base_url.to_owned();
    }
}

impl crate::managed_policy::OsPolicySource for RepointableOs {
    fn gateway_url(&self) -> Result<Option<String>> {
        Ok(Some(self.0.lock().unwrap().clone()))
    }
}

struct DelayedInferenceGateway {
    delay_mint: usize,
    mint_arrived: tokio::sync::Notify,
    release_mint: tokio::sync::Notify,
    llm_mints: AtomicUsize,
    anthropic_requests: AtomicUsize,
    responses_requests: AtomicUsize,
    pause_first_anthropic_request: bool,
}

impl DelayedInferenceGateway {
    fn new(delay_mint: usize, pause_first_anthropic_request: bool) -> Self {
        Self {
            delay_mint,
            mint_arrived: tokio::sync::Notify::new(),
            release_mint: tokio::sync::Notify::new(),
            llm_mints: AtomicUsize::new(0),
            anthropic_requests: AtomicUsize::new(0),
            responses_requests: AtomicUsize::new(0),
            pause_first_anthropic_request,
        }
    }
}

async fn delayed_inference_token(
    State(gateway): State<Arc<DelayedInferenceGateway>>,
    Form(form): Form<HashMap<String, String>>,
) -> Json<Value> {
    assert_eq!(
        form.get("grant_type").map(String::as_str),
        Some("refresh_token")
    );
    let resource = form.get("resource").cloned().unwrap_or_default();
    let mint = gateway.llm_mints.fetch_add(1, Ordering::SeqCst);
    if mint == gateway.delay_mint {
        gateway.mint_arrived.notify_one();
        gateway.release_mint.notified().await;
    }
    Json(json!({
        "access_token": format!("mg_at_{resource}_{mint}"),
        "token_type": "Bearer",
        // Below the refresh leeway so every continuation performs the
        // network-backed mint that the race is about.
        "expires_in": 1,
        "refresh_token": format!("mg_rt_{mint}"),
        "scope": "inference:invoke",
        "resource": resource,
        "installation_id": "install-1",
    }))
}

async fn delayed_anthropic_inference(
    State(gateway): State<Arc<DelayedInferenceGateway>>,
) -> Response {
    let request = gateway.anthropic_requests.fetch_add(1, Ordering::SeqCst);
    let stop_reason = if gateway.pause_first_anthropic_request && request == 0 {
        "pause_turn"
    } else {
        "end_turn"
    };
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        format!(
            "data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"{stop_reason}\"}}}}\n\n"
        ),
    )
        .into_response()
}

async fn delayed_responses_inference(
    State(gateway): State<Arc<DelayedInferenceGateway>>,
) -> Response {
    gateway.responses_requests.fetch_add(1, Ordering::SeqCst);
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
    )
        .into_response()
}

async fn managed_router_with_delayed_inference(
    protocol: providers::GatewayModelProtocol,
    gateway: Arc<DelayedInferenceGateway>,
) -> (
    tidebreak_router::Router,
    String,
    Arc<RepointableOs>,
    tempfile::TempDir,
) {
    let app = AxumRouter::new()
        .route("/oauth/token", post(delayed_inference_token))
        .route(
            "/compat/anthropic/v1/messages",
            post(delayed_anthropic_inference),
        )
        .route(
            "/compat/openai/v1/responses",
            post(delayed_responses_inference),
        )
        .with_state(gateway);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("gateway.db").display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
    let credentials: crate::connectors::GatewayCredentials = serde_json::from_value(json!({
        "base_url": base,
        "installation_id": "install-1",
        "user_id": "user-1",
        "refresh_token": "mg_rt_seed",
        "access_tokens": {}
    }))
    .unwrap();
    CredentialVault::new(secrets.clone())
        .save(&credentials)
        .await
        .unwrap();
    let os = Arc::new(RepointableOs::new(base.clone()));
    let runtime = GatewayRuntime::new(
        store.clone(),
        secrets,
        crate::managed_policy::MemoryProvisionedPolicy::new(),
        os.clone(),
    );
    let policy = runtime.policy().unwrap();
    let (model, upstream) = match protocol {
        providers::GatewayModelProtocol::AnthropicMessages => {
            ("anthropic-us-claude-opus-5", "us.anthropic.claude-opus-5")
        }
        providers::GatewayModelProtocol::OpenaiResponses => ("gateway-gpt-5-6-sol", "gpt-5.6-sol"),
    };
    let snapshot = providers::GatewayModelSnapshot {
        gateway_url: policy.gateway_url.clone().unwrap(),
        installation_id: Some("install-1".into()),
        models: vec![CustomModelConfig {
            id: model.into(),
            upstream_id: Some(upstream.into()),
            ..Default::default()
        }],
        model_protocols: std::collections::BTreeMap::from([(model.into(), protocol)]),
        model_reasoning_efforts: Default::default(),
        member_catalog: Some("v1".into()),
        catalog_etag: None,
    };
    providers::write_gateway_snapshot(&*store, &snapshot)
        .await
        .unwrap();
    let frozen = providers::gateway_execution_policy(
        &snapshot,
        &crate::model_registry::selection_key(providers::ProviderKind::ModelGateway, model),
    )
    .unwrap()
    .route_model;
    let routes = providers::collect_routes(
        &*store,
        &*runtime.secrets,
        runtime.route_token_source().await,
        None,
        None,
        &policy,
    )
    .await;
    (
        tidebreak_router::Router::build(routes),
        frozen,
        os,
        directory,
    )
}

#[tokio::test]
async fn external_policy_repoint_during_bearer_mint_blocks_gateway_initial_egress() {
    for protocol in [
        providers::GatewayModelProtocol::AnthropicMessages,
        providers::GatewayModelProtocol::OpenaiResponses,
    ] {
        let gateway = Arc::new(DelayedInferenceGateway::new(0, false));
        let (router, frozen, os, _directory) =
            managed_router_with_delayed_inference(protocol, gateway.clone()).await;
        let request = ChatRequest {
            provider: Some(ProviderId::new("model_gateway")),
            model: frozen,
            messages: vec![ChatMessage::text(Role::User, "hi")],
            ..Default::default()
        };
        let dispatch = tokio::spawn(async move { router.stream(request).await });

        tokio::time::timeout(Duration::from_secs(2), gateway.mint_arrived.notified())
            .await
            .expect("the inference token mint reaches gateway A");
        os.repoint("https://gateway-b.example.test");
        gateway.release_mint.notify_one();

        let result = tokio::time::timeout(Duration::from_secs(2), dispatch)
            .await
            .expect("authorization finishes after the delayed mint")
            .unwrap();
        assert!(
            result.is_err(),
            "a token minted by a gateway retired during refresh must not authorize egress"
        );
        assert_eq!(
            gateway.anthropic_requests.load(Ordering::SeqCst),
            0,
            "Anthropic request data must not reach retired gateway A"
        );
        assert_eq!(
            gateway.responses_requests.load(Ordering::SeqCst),
            0,
            "Responses request data must not reach retired gateway A"
        );
    }
}

#[tokio::test]
async fn external_policy_repoint_during_continuation_mint_blocks_anthropic_egress() {
    let gateway = Arc::new(DelayedInferenceGateway::new(1, true));
    let (router, frozen, os, _directory) = managed_router_with_delayed_inference(
        providers::GatewayModelProtocol::AnthropicMessages,
        gateway.clone(),
    )
    .await;
    let stream = router
        .stream(ChatRequest {
            provider: Some(ProviderId::new("model_gateway")),
            model: frozen,
            vendor_web_search: Some(tidebreak_core::provider::VendorWebSearch { max_uses: 1 }),
            messages: vec![ChatMessage::text(Role::User, "search")],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(gateway.anthropic_requests.load(Ordering::SeqCst), 1);

    let continuation = tokio::spawn(async move { stream.collect::<Vec<_>>().await });
    tokio::time::timeout(Duration::from_secs(2), gateway.mint_arrived.notified())
        .await
        .expect("the continuation starts its fresh token mint");
    os.repoint("https://gateway-b.example.test");
    gateway.release_mint.notify_one();

    let events = tokio::time::timeout(Duration::from_secs(2), continuation)
        .await
        .expect("the continuation refuses the retired route")
        .unwrap();
    assert_eq!(
        gateway.anthropic_requests.load(Ordering::SeqCst),
        1,
        "only the leg dispatched before the MDM repoint may reach gateway A"
    );
    assert_eq!(gateway.llm_mints.load(Ordering::SeqCst), 2);
    assert!(matches!(
        events.last(),
        Some(tidebreak_core::ProviderEvent::Failed { .. })
    ));
}

const CATALOG_ETAG: &str = "W/\"catalog-rev-1\"";

async fn catalog(State(gateway): State<Arc<FakeGateway>>, headers: HeaderMap) -> Response {
    let Some(body) = gateway.catalog.lock().unwrap().clone() else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert!(bearer.starts_with("Bearer mg_at_control_"), "{bearer}");
    if headers
        .get("if-none-match")
        .and_then(|value| value.to_str().ok())
        == Some(CATALOG_ETAG)
    {
        gateway.catalog_not_modified.fetch_add(1, Ordering::SeqCst);
        return (
            axum::http::StatusCode::NOT_MODIFIED,
            [(axum::http::header::ETAG, CATALOG_ETAG)],
        )
            .into_response();
    }
    ([(axum::http::header::ETAG, CATALOG_ETAG)], Json(body)).into_response()
}

/// One entitled connected app aggregating the fixture's `tools` MCP
/// endpoint, the shape `/api/v1/cli/apps` serves.
async fn apps(State(gateway): State<Arc<FakeGateway>>, headers: HeaderMap) -> Response {
    if gateway.apps_fail.load(Ordering::SeqCst) {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "apps are down",
        )
            .into_response();
    }
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert!(bearer.starts_with("Bearer mg_at_control_"), "{bearer}");
    Json(json!({
        "apps": [{
            "id": "app-1",
            "name": "Tools",
            "app_kind": "mcp_endpoint",
            "enabled": true,
            "mcp_endpoint_slugs": ["tools"]
        }]
    }))
    .into_response()
}

async fn token(
    State(gateway): State<Arc<FakeGateway>>,
    Form(form): Form<HashMap<String, String>>,
) -> Json<Value> {
    assert_eq!(
        form.get("grant_type").map(String::as_str),
        Some("refresh_token")
    );
    let sequence = gateway.refreshes.fetch_add(1, Ordering::SeqCst);
    let resource = form.get("resource").cloned().unwrap_or_default();
    gateway.minted.lock().unwrap().push((
        resource.clone(),
        form.get("attestation_context_id").cloned(),
    ));
    Json(json!({
        "access_token": format!("mg_at_{resource}_{sequence}"),
        "token_type": "Bearer",
        "expires_in": 600,
        "refresh_token": format!("mg_rt_{sequence}"),
        "scope": "models:read inference:invoke",
        "resource": resource,
        "installation_id": "install-1",
    }))
}

async fn models(
    axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    assert!(!query.contains_key("protocol"));
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert!(bearer.starts_with("Bearer mg_at_control_"), "{bearer}");
    Json(json!({
        "models": [
            {
                "id": "sample-claude",
                "protocol": "anthropic_messages",
                "name": "Sample Claude",
                "context_window": 200000,
                "max_output_tokens": 8192,
                "supports_tools": true,
                "supports_vision": true
            },
            {
                "id": "sample-coder",
                "protocol": "openai_responses",
                "name": "Sample Coder",
                "context_window": null,
                "max_output_tokens": null,
                "supports_tools": true,
                "supports_vision": false
            }
        ]
    }))
    .into_response()
}

/// Minimal Streamable HTTP MCP endpoint that requires an `mcp:tools`
/// session bearer, exactly as the gateway's `/mcp/{slug}` route does.
async fn mcp_endpoint(
    State(gateway): State<Arc<FakeGateway>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert!(bearer.starts_with("Bearer mg_at_mcp:tools_"), "{bearer}");
    let request: Value = serde_json::from_str(&body).unwrap();
    let method = request["method"].as_str().unwrap_or_default().to_string();
    gateway
        .mcp_requests
        .lock()
        .unwrap()
        .push((method.clone(), bearer.to_string()));
    let id = request.get("id").cloned().unwrap_or_default();
    let result = match method.as_str() {
        "initialize" => json!({
            "protocolVersion": tidebreak_mcp::PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "gateway-fixture", "version": "1"}
        }),
        "tools/list" => json!({
            "tools": [{
                "name": "lookup",
                "description": "Look something up",
                "inputSchema": {"type": "object"}
            }]
        }),
        "tools/call" => json!({
            "content": [{"type": "text", "text": "ok"}]
        }),
        _ => json!({}),
    };
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

async fn revoke(
    State(gateway): State<Arc<FakeGateway>>,
    Form(form): Form<HashMap<String, String>>,
) -> Json<Value> {
    gateway
        .revoked
        .lock()
        .unwrap()
        .push(form.get("token").cloned().unwrap_or_default());
    Json(json!({}))
}

async fn meta(State(gateway): State<Arc<FakeGateway>>) -> Json<Value> {
    let read = gateway.meta_reads.fetch_add(1, Ordering::SeqCst);
    let offer_after = gateway.machine_offer_after_reads.load(Ordering::SeqCst);
    let mut metadata = json!({
        "api_version": "v1",
        "installation_id": "install-1",
        "gateway_version": "1.0.0",
        "public_url": "http://gateway.test",
        "auth_mode": "oidc",
    });
    if read >= offer_after {
        metadata["tidebreak_machine_url"] = json!("https://machine.tidebreak.test");
    }
    Json(metadata)
}

async fn serve(gateway: Arc<FakeGateway>) -> std::net::SocketAddr {
    let app = AxumRouter::new()
        .route("/api/v1/meta", get(meta))
        .route("/oauth/token", post(token))
        .route("/oauth/revoke", post(revoke))
        .route("/api/v1/cli/models", get(models))
        .route("/api/v1/cli/apps", get(apps))
        .route("/api/v1/me/catalog", get(catalog))
        .route("/mcp/{slug}", post(mcp_endpoint))
        .with_state(gateway);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    address
}

/// A runtime with a stored session for `session_base`, on a profile
/// provisioned (managed) to `provisioned` — policy being the only way a
/// profile is gateway-connected at all.
async fn signed_in_runtime_at(
    session_base: &str,
    provisioned: &str,
) -> (Arc<GatewayRuntime>, Arc<dyn Store>, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("gateway.db").display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
    // Stored credentials are private to the connectors crate by design;
    // seed the vault through its serialized form, exactly as a completed
    // sign-in would have persisted it.
    let credentials: crate::connectors::GatewayCredentials = serde_json::from_value(json!({
        "base_url": session_base,
        "installation_id": "install-1",
        "user_id": "user-1",
        "account_hint": "abaas@example.test",
        "refresh_token": "mg_rt_seed",
        "access_tokens": {}
    }))
    .unwrap();
    CredentialVault::new(secrets.clone())
        .save(&credentials)
        .await
        .unwrap();
    let provisioned_policy = crate::managed_policy::MemoryProvisionedPolicy::new();
    crate::managed_policy::provision(&*provisioned_policy, provisioned).unwrap();
    (
        GatewayRuntime::new(
            store.clone(),
            secrets,
            provisioned_policy,
            Arc::new(crate::managed_policy::NoOsPolicy),
        ),
        store,
        directory,
    )
}

async fn signed_in_runtime(
    base_url: &str,
) -> (Arc<GatewayRuntime>, Arc<dyn Store>, tempfile::TempDir) {
    signed_in_runtime_at(base_url, base_url).await
}

/// An MCP runtime resolving gateway endpoints through `runtime`, the way
/// the server wires the two together.
fn mcp_for(
    runtime: &Arc<GatewayRuntime>,
    store: &Arc<dyn Store>,
) -> Arc<crate::mcp_config::McpRuntime> {
    Arc::new(crate::mcp_config::McpRuntime::new(
        Arc::new(tidebreak_core::ToolRegistry::new()),
        store.clone(),
        Arc::new(MockSecrets::default()),
        runtime.clone(),
        runtime.provisioned_policy().clone(),
        Arc::new(crate::managed_policy::NoOsPolicy),
    ))
}

/// A gateway that hosts a machine names it on `/api/v1/meta`, and the
/// settings panel prefills the address field from it. Reading it needs
/// no session: meta is unauthenticated, so the offer is there before the
/// first sign-in.
#[tokio::test]
async fn the_offered_machine_comes_from_gateway_meta() {
    let address = serve(Arc::new(FakeGateway::default())).await;
    let base = format!("http://{address}");
    let (runtime, _store, _directory) = signed_in_runtime(&base).await;

    assert_eq!(
        runtime.offered_machine().await.url.as_deref(),
        Some("https://machine.tidebreak.test")
    );
}

#[tokio::test]
async fn a_missing_machine_offer_is_retried_during_the_same_process() {
    let gateway = Arc::new(FakeGateway {
        machine_offer_after_reads: AtomicUsize::new(1),
        ..Default::default()
    });
    let address = serve(gateway.clone()).await;
    let base = format!("http://{address}");
    let (runtime, _store, _directory) = signed_in_runtime(&base).await;

    assert!(runtime.offered_machine().await.url.is_none());
    assert_eq!(
        runtime.offered_machine().await.url.as_deref(),
        Some("https://machine.tidebreak.test")
    );
    assert_eq!(
        runtime.offered_machine().await.url.as_deref(),
        Some("https://machine.tidebreak.test")
    );
    assert_eq!(gateway.meta_reads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn syncs_entitled_models_into_the_snapshot() {
    let address = serve(Arc::new(FakeGateway::default())).await;
    let base = format!("http://{address}");
    let (runtime, store, _directory) = signed_in_runtime(&base).await;

    assert_eq!(runtime.sync_models().await.unwrap(), 2);

    let policy = runtime.policy().unwrap();
    let models = providers::gateway_models(&*store, &policy, None)
        .await
        .unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "sample-claude");
    assert_eq!(models[0].display_name.as_deref(), Some("Sample Claude"));
    assert_eq!(models[0].context_window, 200_000);
    // Absent limits fall back to the conservative custom-model defaults.
    assert_eq!(models[1].context_window, 32_768);
    assert_eq!(models[1].max_output_tokens, 4_096);
    let snapshot = providers::read_gateway_snapshot(&*store)
        .await
        .unwrap()
        .expect("the sync persisted its protocol map");
    assert_eq!(
        snapshot.model_protocols.get("sample-claude"),
        Some(&providers::GatewayModelProtocol::AnthropicMessages)
    );
    assert_eq!(
        snapshot.model_protocols.get("sample-coder"),
        Some(&providers::GatewayModelProtocol::OpenaiResponses)
    );

    // The synced snapshot resolves as a model policy under the gateway key.
    let policy =
        providers::resolve_model_policy(&*store, "model_gateway::sample-claude", false, None)
            .await
            .unwrap()
            .expect("synced model resolves");
    assert_eq!(
        policy.provider,
        crate::providers::ProviderKind::ModelGateway
    );
    assert_eq!(policy.display_name, "Sample Claude");
    // Synced through the degraded CLI reads: no catalog revision to stamp.
    let snapshot = providers::read_gateway_snapshot(&*store).await.unwrap();
    assert_eq!(snapshot.unwrap().member_catalog, None);
}

fn sample_catalog() -> Value {
    json!({
        "models": [
            {
                "id": "sample-claude",
                "name": "Sample Claude",
                // Dual-protocol: one row, both surfaces, and the sync
                // must route it through Anthropic Messages.
                "protocols": ["anthropic_messages", "openai_responses"],
                "aliases": ["us.anthropic.claude-opus-5"],
                "supports_tools": true,
                "supports_vision": true,
                "context_window": 200000,
                "max_output_tokens": 8192,
                "provider_name": "Anthropic"
            },
            {
                "id": "sample-coder",
                "name": "Sample Coder",
                "protocols": ["openai_responses"],
                "supports_tools": true,
                "supports_vision": false,
                "context_window": null,
                "max_output_tokens": null,
                "provider_name": "Acme"
            },
            {
                "id": "sample-exotic",
                "name": "Sample Exotic",
                // A protocol this client does not speak: skipped, never
                // fatal — the set is the gateway's to grow.
                "protocols": ["grpc_frames"],
                "supports_tools": false,
                "supports_vision": false,
                "context_window": null,
                "max_output_tokens": null,
                "provider_name": "Acme"
            }
        ],
        "apps": [{
            "id": "app-1",
            "name": "Tools",
            "app_kind": "mcp_endpoint",
            "enabled": true,
            "mcp_endpoint_slugs": ["tools"],
            "connection": "authorization_required"
        }]
    })
}

#[tokio::test]
async fn sync_prefers_the_member_catalog_and_stamps_its_revision() {
    let gateway = Arc::new(FakeGateway::default());
    *gateway.catalog.lock().unwrap() = Some(sample_catalog());
    let address = serve(gateway).await;
    let base = format!("http://{address}");
    let (runtime, store, _directory) = signed_in_runtime(&base).await;

    // Two models the client can route; the exotic protocol is skipped.
    assert_eq!(runtime.sync_models().await.unwrap(), 2);

    let snapshot = providers::read_gateway_snapshot(&*store)
        .await
        .unwrap()
        .expect("the sync persisted a snapshot");
    assert_eq!(snapshot.member_catalog.as_deref(), Some("v1"));
    assert_eq!(snapshot.catalog_etag.as_deref(), Some(CATALOG_ETAG));
    assert_eq!(
        snapshot.model_protocols.get("sample-claude"),
        Some(&providers::GatewayModelProtocol::AnthropicMessages),
        "a dual-protocol model routes through Anthropic Messages"
    );
    assert_eq!(
        snapshot.model_protocols.get("sample-coder"),
        Some(&providers::GatewayModelProtocol::OpenaiResponses)
    );
    assert!(!snapshot.model_protocols.contains_key("sample-exotic"));

    // The catalog's alias matches the curated registry row, so the
    // policy carries curated capabilities under the gateway's own id.
    let policy =
        providers::resolve_model_policy(&*store, "model_gateway::sample-claude", false, None)
            .await
            .unwrap()
            .expect("synced model resolves");
    assert_eq!(policy.display_name, "Claude Opus 5");

    // The status projection surfaces the catalog revision for the
    // settings panel's older-gateway note.
    let status = runtime.status().await.unwrap();
    assert_eq!(status.member_catalog.as_deref(), Some("v1"));
}

#[tokio::test]
async fn overlapping_model_syncs_serialize_before_the_second_fetch() {
    let gateway = Arc::new(FakeGateway::default());
    let requests = Arc::new(AtomicUsize::new(0));
    let (arrived_tx, mut arrived_rx) = tokio::sync::mpsc::unbounded_channel::<usize>();
    let catalog_route = {
        let requests = requests.clone();
        move |headers: HeaderMap| {
            let requests = requests.clone();
            let arrived = arrived_tx.clone();
            async move {
                let bearer = headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default();
                assert!(bearer.starts_with("Bearer mg_at_control_"), "{bearer}");
                let request = requests.fetch_add(1, Ordering::SeqCst);
                arrived.send(request).expect("the test is listening");
                (
                    [(axum::http::header::ETAG, format!("W/\"rev-{request}\""))],
                    Json(sample_catalog()),
                )
                    .into_response()
            }
        }
    };
    let app = AxumRouter::new()
        .route("/oauth/token", post(token))
        .route("/api/v1/me/catalog", get(catalog_route))
        .with_state(gateway);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (runtime, store, _directory) = signed_in_runtime(&base).await;
    let first_pause = Arc::new(MigrationPause::default());
    *runtime.sync_commit_pause.lock().await = Some(first_pause.clone());

    let first = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.sync_models().await }
    });
    assert_eq!(arrived_rx.recv().await, Some(0));
    first_pause.arrived.notified().await;

    let (second_started_tx, second_started_rx) = tokio::sync::oneshot::channel();
    let second = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            second_started_tx.send(()).unwrap();
            runtime.sync_models().await
        }
    });
    second_started_rx.await.unwrap();
    tokio::task::yield_now().await;
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "the second sync must wait before issuing its catalog fetch"
    );

    first_pause.release.notify_one();
    assert_eq!(first.await.unwrap().unwrap(), 2);
    assert_eq!(arrived_rx.recv().await, Some(1));
    assert_eq!(second.await.unwrap().unwrap(), 2);
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    assert_eq!(
        providers::read_gateway_snapshot(&*store)
            .await
            .unwrap()
            .unwrap()
            .catalog_etag
            .as_deref(),
        Some("W/\"rev-1\"")
    );
}

#[tokio::test]
async fn request_route_setups_share_read_leases_while_catalog_sync_waits() {
    let gateway = Arc::new(FakeGateway::default());
    *gateway.catalog.lock().unwrap() = Some(sample_catalog());
    let address = serve(gateway.clone()).await;
    let base = format!("http://{address}");
    let (runtime, store, _directory) = signed_in_runtime(&base).await;
    runtime.sync_models().await.unwrap();

    let policy = runtime.policy().unwrap();
    let snapshot = providers::gateway_snapshot_for_policy(&*store, &policy)
        .await
        .unwrap()
        .unwrap();
    let frozen = providers::gateway_execution_policy(&snapshot, "model_gateway::sample-claude")
        .unwrap()
        .route_model;
    let source = runtime.route_token_source().await.unwrap();

    let first = source
        .lease_model_route(&frozen)
        .await
        .unwrap()
        .expect("first request setup receives a route lease");
    let second = tokio::time::timeout(Duration::from_secs(1), source.lease_model_route(&frozen))
        .await
        .expect("a second request setup must not serialize behind the first")
        .unwrap()
        .expect("second request setup receives the same live route lease");

    let mut writer = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.sync_models().await }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut writer)
            .await
            .is_err(),
        "catalog mutation must wait while request legs hold read leases"
    );
    assert_eq!(gateway.catalog_not_modified.load(Ordering::SeqCst), 0);

    drop(first);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut writer)
            .await
            .is_err(),
        "every active request leg must release before the writer proceeds"
    );
    drop(second);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), writer)
            .await
            .expect("catalog sync proceeds after both dispatch leases end")
            .unwrap()
            .unwrap(),
        2
    );
    assert_eq!(gateway.catalog_not_modified.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn sign_out_waits_for_catalog_sync_then_clears_its_commit() {
    let gateway = Arc::new(FakeGateway::default());
    *gateway.catalog.lock().unwrap() = Some(sample_catalog());
    let address = serve(gateway).await;
    let base = format!("http://{address}");
    let (runtime, store, _directory) = signed_in_runtime(&base).await;
    crate::model_roles::write_selection(
        &*store,
        crate::model_roles::ModelRole::Chat,
        Some("anthropic::claude-opus-5"),
    )
    .await
    .unwrap();
    let pause = Arc::new(MigrationPause::default());
    *runtime.sync_commit_pause.lock().await = Some(pause.clone());

    let sync = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.sync_models().await }
    });
    pause.arrived.notified().await;
    let mut sign_out = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.sign_out().await }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut sign_out)
            .await
            .is_err(),
        "sign-out must wait for the in-flight catalog writer instead of deadlocking it"
    );
    pause.release.notify_one();
    assert_eq!(sync.await.unwrap().unwrap(), 2);
    tokio::time::timeout(Duration::from_secs(2), sign_out)
        .await
        .expect("sign-out proceeds after catalog sync releases the writer")
        .unwrap()
        .unwrap();

    let policy = runtime.policy().unwrap();
    assert!(providers::gateway_models(&*store, &policy, None)
        .await
        .unwrap()
        .is_empty());
    assert!(runtime
        .connection()
        .await
        .unwrap()
        .unwrap()
        .stored_credentials()
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        crate::model_roles::read_selection(&*store, crate::model_roles::ModelRole::Chat)
            .await
            .unwrap()
            .as_deref(),
        Some("anthropic::claude-opus-5")
    );
}

#[tokio::test]
async fn a_catalog_response_cannot_commit_after_same_url_session_replacement() {
    let gateway = Arc::new(FakeGateway::default());
    *gateway.catalog.lock().unwrap() = Some(sample_catalog());
    let address = serve(gateway).await;
    let base = format!("http://{address}");
    let (runtime, store, _directory) = signed_in_runtime(&base).await;
    crate::model_roles::write_selection(
        &*store,
        crate::model_roles::ModelRole::Chat,
        Some("anthropic::claude-opus-5"),
    )
    .await
    .unwrap();
    let pause = Arc::new(MigrationPause::default());
    *runtime.sync_commit_pause.lock().await = Some(pause.clone());

    let sync = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.sync_models().await }
    });
    pause.arrived.notified().await;
    let replacement: crate::connectors::GatewayCredentials = serde_json::from_value(json!({
        "base_url": base,
        "installation_id": "install-1",
        "user_id": "user-1",
        "account_hint": "abaas@example.test",
        "refresh_token": "mg_rt_replacement",
        "access_tokens": {}
    }))
    .unwrap();
    CredentialVault::new(runtime.secrets.clone())
        .save(&replacement)
        .await
        .unwrap();
    pause.release.notify_one();

    let error = sync
        .await
        .unwrap()
        .expect_err("the old session's response must not commit over its replacement");
    assert_eq!(error.kind(), "gateway_changed");
    assert!(providers::read_gateway_snapshot(&*store)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        crate::model_roles::read_selection(&*store, crate::model_roles::ModelRole::Chat)
            .await
            .unwrap()
            .as_deref(),
        Some("anthropic::claude-opus-5")
    );
}

#[tokio::test]
async fn sync_never_rewrites_durable_model_selections() {
    let gateway = Arc::new(FakeGateway::default());
    *gateway.catalog.lock().unwrap() = Some(sample_catalog());
    let address = serve(gateway).await;
    let base = format!("http://{address}");
    let (runtime, store, _directory) = signed_in_runtime(&base).await;

    crate::model_roles::write_selection(
        &*store,
        crate::model_roles::ModelRole::Chat,
        Some("anthropic::claude-opus-5"),
    )
    .await
    .unwrap();
    store
        .set_setting(
            "chat_default.model",
            &serde_json::Value::String("claude-opus-5".into()),
        )
        .await
        .unwrap();
    let pinned = tidebreak_core::Chat {
        id: tidebreak_core::ChatId::new(),
        project_id: None,
        title: None,
        model: Some("claude-opus-5".into()),
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: chrono::Utc::now(),
    };
    store.create_chat(&pinned).await.unwrap();

    runtime.sync_models().await.unwrap();

    assert_eq!(
        crate::model_roles::read_selection(&*store, crate::model_roles::ModelRole::Chat)
            .await
            .unwrap()
            .as_deref(),
        Some("anthropic::claude-opus-5")
    );
    assert_eq!(
        store.get_setting("chat_default.model").await.unwrap(),
        Some(serde_json::Value::String("claude-opus-5".into()))
    );
    assert_eq!(
        store
            .get_chat(pinned.id)
            .await
            .unwrap()
            .unwrap()
            .model
            .as_deref(),
        Some("claude-opus-5")
    );
}

#[tokio::test]
async fn an_unchanged_catalog_answers_304_and_the_snapshot_stays() {
    let gateway = Arc::new(FakeGateway::default());
    *gateway.catalog.lock().unwrap() = Some(sample_catalog());
    let address = serve(gateway.clone()).await;
    let base = format!("http://{address}");
    let (runtime, store, _directory) = signed_in_runtime(&base).await;

    assert_eq!(runtime.sync_models().await.unwrap(), 2);
    // The second tick sends the stored ETag and keeps the snapshot.
    assert_eq!(runtime.sync_models().await.unwrap(), 2);
    assert_eq!(gateway.catalog_not_modified.load(Ordering::SeqCst), 1);
    let snapshot = providers::read_gateway_snapshot(&*store).await.unwrap();
    assert_eq!(snapshot.unwrap().models.len(), 2);
}

#[tokio::test]
async fn a_304_sync_racing_a_policy_repoint_is_rejected() {
    let gateway = Arc::new(FakeGateway::default());
    *gateway.catalog.lock().unwrap() = Some(sample_catalog());
    let address = serve(gateway.clone()).await;
    let base = format!("http://{address}");

    struct SwappableOs(std::sync::Mutex<String>);

    impl crate::managed_policy::OsPolicySource for SwappableOs {
        fn gateway_url(&self) -> Result<Option<String>> {
            Ok(Some(self.0.lock().unwrap().clone()))
        }
    }

    let (seed_runtime, store, directory) = signed_in_runtime(&base).await;
    seed_runtime.sync_models().await.unwrap();
    drop(seed_runtime);
    let os = Arc::new(SwappableOs(std::sync::Mutex::new(base.clone())));
    let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
    let credentials: crate::connectors::GatewayCredentials = serde_json::from_value(json!({
        "base_url": base,
        "installation_id": "install-1",
        "user_id": "user-1",
        "refresh_token": "mg_rt_seed",
        "access_tokens": {}
    }))
    .unwrap();
    CredentialVault::new(secrets.clone())
        .save(&credentials)
        .await
        .unwrap();
    let runtime = GatewayRuntime::new(
        store.clone(),
        secrets,
        crate::managed_policy::MemoryProvisionedPolicy::new(),
        os.clone(),
    );
    let held = providers::GATEWAY_STATE_WRITES.lock().await;
    let sync = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.sync_models().await }
    });
    while gateway.catalog_not_modified.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    *os.0.lock().unwrap() = "https://gateway-b.test".to_owned();
    drop(held);

    let error = sync
        .await
        .unwrap()
        .expect_err("a 304 sync must recheck policy before returning");
    assert_eq!(error.kind(), "gateway_changed");
    drop(directory);
}

#[tokio::test]
async fn apps_carry_readiness_from_the_member_catalog() {
    let gateway = Arc::new(FakeGateway::default());
    *gateway.catalog.lock().unwrap() = Some(sample_catalog());
    let address = serve(gateway).await;
    let base = format!("http://{address}");
    let (runtime, _store, _directory) = signed_in_runtime(&base).await;

    let apps = runtime
        .apps(&tidebreak_core::OwnerId::local())
        .await
        .unwrap();
    assert!(apps.supported);
    assert_eq!(apps.apps.len(), 1);
    assert_eq!(
        apps.apps[0].connection.as_deref(),
        Some("authorization_required")
    );
}

#[tokio::test]
async fn gateway_apps_route_scopes_used_by_count_to_the_authenticated_principal() {
    let gateway = Arc::new(FakeGateway::default());
    *gateway.catalog.lock().unwrap() = Some(sample_catalog());
    let address = serve(gateway).await;
    let base = format!("http://{address}");
    let (runtime, store, directory) = signed_in_runtime(&base).await;
    let alice = OwnerId::new("user:alice").unwrap();
    let app_id = AppId::new();
    store
        .create_app_scoped(
            &alice,
            &CreateApp {
                id: app_id,
                revision: NewAppRevision {
                    id: AppRevisionId::new(),
                    manifest: AppManifest {
                        name: "Alice gateway app".into(),
                        bindings: Vec::new(),
                    },
                    byte_len: 1,
                    sha256: [0; 32],
                    turn_id: None,
                    producing_run_id: None,
                    chat_id: None,
                    created_at: chrono::Utc::now(),
                },
            },
        )
        .await
        .unwrap();
    store
        .put_app_grant_scoped(
            &alice,
            &AppGrant {
                app_id,
                bindings: vec![AppGrantBinding::GatewayOperations(
                    AppGatewayOperationsGrantBinding {
                        gateway_app: "app-1".into(),
                        operation_ids: vec!["list".into()],
                        fingerprint: [1; 32],
                    },
                )],
                created_at: chrono::Utc::now(),
            },
        )
        .await
        .unwrap();

    struct RouteTestResolver;

    #[async_trait]
    impl crate::resolver::ProviderResolver for RouteTestResolver {
        async fn resolve(&self) -> Arc<dyn ModelProvider> {
            Arc::new(crate::provider::UnconfiguredProvider)
        }
    }

    let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
    let chatgpt = Arc::new(
        crate::chatgpt_runtime::ChatGptRuntime::new(store.clone(), secrets.clone()).unwrap(),
    );
    let state = crate::state::AppState::with_gateway_runtime(
        Config::desktop(directory.path()),
        store,
        Arc::new(RouteTestResolver),
        secrets,
        Arc::new(ToolRegistry::new()),
        AgentConfig::default(),
        uuid::Uuid::new_v4(),
        runtime,
        chatgpt,
        crate::managed_policy::MemoryProvisionedPolicy::new(),
        Arc::new(crate::managed_policy::NoOsPolicy),
    )
    .unwrap();

    async fn count_for(
        state: crate::state::AppState,
        principal: crate::principal::Principal,
    ) -> usize {
        let response = AxumRouter::new()
            .route("/gateway/apps", get(crate::routes::get_gateway_apps))
            .with_state(state)
            .layer(Extension(crate::principal::AuthContext {
                principal,
                client_executor: false,
            }))
            .oneshot(
                Request::builder()
                    .uri("/gateway/apps")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice::<Value>(&body).unwrap()["apps"][0]["used_by_app_count"]
            .as_u64()
            .unwrap() as usize
    }

    let alice_count = count_for(
        state.clone(),
        crate::principal::Principal::User {
            id: crate::principal::UserId::new("alice").unwrap(),
            role: crate::principal::Role::Member,
        },
    )
    .await;
    let bob_count = count_for(
        state,
        crate::principal::Principal::User {
            id: crate::principal::UserId::new("bob").unwrap(),
            role: crate::principal::Role::Member,
        },
    )
    .await;

    assert_eq!(alice_count, 1);
    assert_eq!(bob_count, 0);
}

#[tokio::test]
async fn apps_from_a_catalogless_gateway_carry_no_readiness() {
    let address = serve(Arc::new(FakeGateway::default())).await;
    let base = format!("http://{address}");
    let (runtime, _store, _directory) = signed_in_runtime(&base).await;

    let apps = runtime
        .apps(&tidebreak_core::OwnerId::local())
        .await
        .unwrap();
    assert!(apps.supported);
    assert_eq!(apps.apps.len(), 1);
    assert_eq!(apps.apps[0].connection, None);
}

/// The boot-before-sign-in race: a status or sync read caches the
/// keychain's `NoEntry`, and the session then lands without the cache
/// observing the write — a read that was in flight while sign-in
/// completed stores its stale miss *after* the write's invalidation, and
/// a session written by another process never invalidates at all. The
/// gateway session key's misses are not memoized
/// (`CachingSecretProvider::with_miss_passthrough`), so the next
/// background tick sees the session with no restart and no manual sync.
#[tokio::test]
async fn a_session_written_behind_the_secret_cache_syncs_without_a_restart() {
    let address = serve(Arc::new(FakeGateway::default())).await;
    let base = format!("http://{address}");
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("gateway.db").display()
        ))
        .await
        .unwrap(),
    );
    let inner = Arc::new(MockSecrets::default());
    // The same wrap `secret_provider` in lib.rs applies at boot.
    let secrets: Arc<dyn SecretProvider> = Arc::new(
        tidebreak_core::CachingSecretProvider::new(inner.clone())
            .with_miss_passthrough([crate::connectors::GATEWAY_SECRET_KEY]),
    );
    let provisioned_policy = crate::managed_policy::MemoryProvisionedPolicy::new();
    crate::managed_policy::provision(&*provisioned_policy, &base).unwrap();
    let runtime = GatewayRuntime::new(
        store.clone(),
        secrets,
        provisioned_policy,
        Arc::new(crate::managed_policy::NoOsPolicy),
    );

    // Boot before sign-in: the tick reads the empty store. Were the miss
    // memoized, this is the read that would hide the session forever.
    assert_eq!(runtime.sync_models_if_connected().await.unwrap(), None);

    // The session lands behind the cache's back, straight into the store.
    let credentials: crate::connectors::GatewayCredentials = serde_json::from_value(json!({
        "base_url": base,
        "installation_id": "install-1",
        "user_id": "user-1",
        "account_hint": "abaas@example.test",
        "refresh_token": "mg_rt_seed",
        "access_tokens": {}
    }))
    .unwrap();
    inner
        .set_secret(
            crate::connectors::GATEWAY_SECRET_KEY,
            &serde_json::to_string(&credentials).unwrap(),
        )
        .await
        .unwrap();

    // The very next tick recovers — no restart, no manual sync click.
    assert_eq!(runtime.sync_models_if_connected().await.unwrap(), Some(2));
}

/// A gateway that serves a model under its upstream id is serving that
/// curated model over a different route, so the row inherits the curated
/// capabilities instead of being presented as an anonymous unverified
/// endpoint — whether the gateway id is itself the curated id or a
/// deployment alias whose reported upstream id resolves to one. The
/// deployment's own limits still win, and a row the registry can be matched
/// to by neither route keeps the conservative treatment.
#[tokio::test]
async fn a_gateway_model_matching_a_curated_id_inherits_its_capabilities() {
    let address = serve(Arc::new(FakeGateway::default())).await;
    let base = format!("http://{address}");
    let (_runtime, store, _directory) = signed_in_runtime(&base).await;

    providers::write_gateway_snapshot(
        &*store,
        &providers::GatewayModelSnapshot {
            gateway_url: base.clone(),
            installation_id: Some("install-1".into()),
            models: vec![
                CustomModelConfig {
                    id: "claude-opus-5".to_string(),
                    upstream_id: None,
                    display_name: Some("Opus (gateway)".to_string()),
                    context_window: 200_000,
                    max_output_tokens: 32_000,
                    ..Default::default()
                },
                CustomModelConfig {
                    id: "anthropic-us-claude-opus-5".to_string(),
                    upstream_id: Some("us.anthropic.claude-opus-5".to_string()),
                    display_name: None,
                    context_window: 200_000,
                    max_output_tokens: 32_000,
                    ..Default::default()
                },
                CustomModelConfig {
                    id: "acme-inhouse-llm".to_string(),
                    upstream_id: None,
                    display_name: None,
                    context_window: 200_000,
                    max_output_tokens: 32_000,
                    ..Default::default()
                },
            ],
            model_protocols: Default::default(),
            model_reasoning_efforts: Default::default(),
            member_catalog: None,
            catalog_etag: None,
        },
    )
    .await
    .unwrap();

    let matched =
        providers::resolve_model_policy(&*store, "model_gateway::claude-opus-5", false, None)
            .await
            .unwrap()
            .expect("the gateway row resolves");
    assert_eq!(
        matched.provider,
        crate::providers::ProviderKind::ModelGateway,
        "the gateway still serves the request"
    );
    assert_eq!(
        matched.vendor,
        Some(crate::providers::ProviderKind::Anthropic)
    );
    assert_eq!(matched.key, "model_gateway::claude-opus-5");
    assert_eq!(matched.display_name, "Claude Opus 5");
    assert_eq!(
        matched.verification,
        crate::model_registry::VerificationTier::Verified
    );
    assert!(matched
        .input_modalities
        .contains(&crate::model_registry::InputModality::Image));
    assert!(matched.supports_reasoning);
    assert!(matched.supports_structured_output);
    assert!(!matched.reasoning_efforts.is_empty());
    // The deployment's reported limits, not the curated model's.
    assert_eq!(matched.context_window, 200_000);
    assert_eq!(matched.max_output_tokens, 32_000);

    let aliased = providers::resolve_model_policy(
        &*store,
        "model_gateway::anthropic-us-claude-opus-5",
        false,
        None,
    )
    .await
    .unwrap()
    .expect("the aliased gateway row resolves");
    assert_eq!(
        aliased.provider,
        crate::providers::ProviderKind::ModelGateway,
        "matching by upstream id must not re-route the request"
    );
    assert_eq!(
        aliased.id, "anthropic-us-claude-opus-5",
        "the gateway's own id is what the request carries"
    );
    assert_eq!(
        aliased.request_shaping_model, "claude-opus-5",
        "the unambiguous canonical upstream identity shapes the request"
    );
    assert_eq!(
        aliased.vendor,
        Some(crate::providers::ProviderKind::Anthropic)
    );
    assert_eq!(aliased.display_name, "Claude Opus 5");
    assert_eq!(
        aliased.verification,
        crate::model_registry::VerificationTier::Verified
    );
    assert!(aliased
        .input_modalities
        .contains(&crate::model_registry::InputModality::Image));
    assert!(aliased.supports_reasoning);
    assert!(aliased.supports_structured_output);
    assert_eq!(aliased.context_window, 200_000);

    let unmatched =
        providers::resolve_model_policy(&*store, "model_gateway::acme-inhouse-llm", false, None)
            .await
            .unwrap()
            .expect("the unmatched gateway row still resolves");
    assert_eq!(unmatched.vendor, None);
    assert_eq!(
        unmatched.verification,
        crate::model_registry::VerificationTier::Unverified
    );
    assert_eq!(
        unmatched.input_modalities,
        vec![crate::model_registry::InputModality::Text]
    );
    assert!(!unmatched.supports_reasoning);
    assert!(!unmatched.supports_structured_output);
}

/// The background loop's first attempt is immediate — the boot case it
/// exists for: a signed-in profile gets a fresh snapshot without anyone
/// pressing Refresh.
#[tokio::test]
async fn the_background_sync_populates_the_snapshot_without_a_manual_refresh() {
    let address = serve(Arc::new(FakeGateway::default())).await;
    let base = format!("http://{address}");
    let (runtime, store, _directory) = signed_in_runtime(&base).await;

    let task = tokio::spawn(
        runtime
            .clone()
            .sync_models_periodically(mcp_for(&runtime, &store)),
    );
    let synced = async {
        loop {
            if let Some(snapshot) = providers::read_gateway_snapshot(&*store).await.unwrap() {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let snapshot = tokio::time::timeout(Duration::from_secs(5), synced)
        .await
        .expect("the boot-time sync lands without a manual refresh");
    task.abort();
    assert_eq!(snapshot.models.len(), 2);
}

#[tokio::test]
async fn the_route_token_source_mints_llm_tokens_per_rotation() {
    let gateway = Arc::new(FakeGateway::default());
    let address = serve(gateway.clone()).await;
    let base = format!("http://{address}");
    let (runtime, store, _directory) = signed_in_runtime(&base).await;

    let source = runtime
        .route_token_source()
        .await
        .expect("signed-in runtime offers a token source");
    let token = source.bearer_token().await.unwrap();
    assert!(token.starts_with("mg_at_llm_"), "{token}");
    // A fresh token is cached: no second refresh inside the expiry leeway.
    assert_eq!(source.bearer_token().await.unwrap(), token);
    assert_eq!(gateway.refreshes.load(Ordering::SeqCst), 1);

    // A conversation mints inside that chat's attestation context: cached
    // per chat, distinct across chats, and never the shared token.
    let chat = tidebreak_core::id::ChatId::new();
    let attested = source.bearer_token_for(Some(chat)).await.unwrap();
    assert_ne!(attested, token);
    assert_eq!(source.bearer_token_for(Some(chat)).await.unwrap(), attested);
    let other = source
        .bearer_token_for(Some(tidebreak_core::id::ChatId::new()))
        .await
        .unwrap();
    assert_ne!(other, attested);
    // No conversation — titling, judging, maintenance — keeps the shared
    // token and records nothing.
    assert_eq!(source.bearer_token_for(None).await.unwrap(), token);

    // The route set includes the gateway with its synced models claimed.
    // A legacy provider row — even one pointing at a different deployment
    // — is never read, so it changes nothing about the composite route.
    runtime.sync_models().await.unwrap();
    providers::write_config(
        &*store,
        crate::providers::ProviderKind::ModelGateway,
        &providers::ProviderConfig {
            enabled: true,
            base_url: Some("http://127.0.0.1:9".to_string()),
            models: Vec::new(),
        },
    )
    .await
    .unwrap();
    let policy = runtime.policy().unwrap();
    let routes = providers::collect_routes(
        &*store,
        &*runtime.secrets,
        runtime.route_token_source().await,
        None,
        None,
        &policy,
    )
    .await;
    let anthropic_route = routes
        .iter()
        .find(|route| route.kind == tidebreak_router::RouteKind::ModelGateway)
        .expect("Anthropic gateway route present");
    assert_eq!(
        anthropic_route.base_url.as_deref(),
        Some(format!("{base}/compat/anthropic").as_str())
    );
    assert!(anthropic_route.api_key.is_empty());
    assert!(anthropic_route
        .curated_models
        .contains(&"sample-claude".to_string()));
    assert!(!anthropic_route
        .curated_models
        .contains(&"sample-coder".to_string()));
    let anthropic_frozen: Vec<_> = anthropic_route
        .curated_models
        .iter()
        .filter(|model| model.starts_with("__tidebreak_gateway_v1."))
        .collect();
    assert_eq!(anthropic_frozen.len(), 1);
    assert_eq!(
        anthropic_route.model_rewrites.get(anthropic_frozen[0]),
        Some(&"sample-claude".to_string())
    );
    let openai_route = routes
        .iter()
        .find(|route| route.kind == tidebreak_router::RouteKind::ModelGatewayOpenai)
        .expect("OpenAI gateway route present");
    assert_eq!(
        openai_route.base_url.as_deref(),
        Some(format!("{base}/compat/openai/v1").as_str())
    );
    assert!(openai_route.api_key.is_empty());
    assert!(openai_route
        .curated_models
        .contains(&"sample-coder".to_string()));
    let openai_frozen: Vec<_> = openai_route
        .curated_models
        .iter()
        .filter(|model| model.starts_with("__tidebreak_gateway_v1."))
        .collect();
    assert_eq!(openai_frozen.len(), 1);
    assert_eq!(
        openai_route.model_rewrites.get(openai_frozen[0]),
        Some(&"sample-coder".to_string())
    );
    assert!(providers::provider_is_usable(
        &*store,
        &*runtime.secrets,
        crate::providers::ProviderKind::ModelGateway,
        &policy,
        None
    )
    .await
    .unwrap());

    let frozen = anthropic_frozen[0].as_str();
    let lease = source
        .lease_model_route(frozen)
        .await
        .unwrap()
        .expect("the current frozen selector receives a live route lease");
    assert_eq!(lease.wire_model(), "sample-claude");
    assert_eq!(lease.request_shaping_model(), "sample-claude");
    drop(lease);

    let mut retargeted = providers::gateway_snapshot_for_policy(&*store, &policy)
        .await
        .unwrap()
        .unwrap();
    retargeted
        .models
        .iter_mut()
        .find(|model| model.id == "sample-claude")
        .unwrap()
        .upstream_id = Some("gpt-5.6-sol".into());
    providers::write_gateway_snapshot(&*store, &retargeted)
        .await
        .unwrap();
    assert!(source.lease_model_route(frozen).await.unwrap().is_none());
}

/// The race #896 named, in its surviving form: with policy the only
/// gateway source, the one authority that can re-point a deployment
/// mid-sync is an OS (MDM) push. A sync whose entitlement fetch is still
/// in flight when that happens must not stamp the old gateway's model
/// list into the snapshot — what this pins is the under-lock policy
/// recheck refusing the stale write.
#[tokio::test]
async fn a_sync_racing_a_policy_repoint_cannot_stamp_the_old_gateways_models() {
    // Gateway A: answers token refreshes normally but parks the model
    // list until released — the window the pairing lands in.
    let (arrived_tx, mut arrived_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let release = Arc::new(tokio::sync::Notify::new());
    let parked_models = {
        let release = release.clone();
        move || {
            let release = release.clone();
            let arrived = arrived_tx.clone();
            async move {
                arrived.send(()).expect("the test is listening");
                release.notified().await;
                Json(json!({
                    "models": [{
                        "id": "stale-model",
                        "name": "Stale Model",
                        "context_window": 200000,
                        "max_output_tokens": 8192,
                        "supports_tools": true,
                        "supports_vision": false
                    }]
                }))
            }
        }
    };
    let app = AxumRouter::new()
        .route("/oauth/token", post(token))
        .route("/api/v1/cli/models", get(parked_models))
        .with_state(Arc::new(FakeGateway::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_a = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // An OS authority whose asserted deployment can change mid-test, as
    // an MDM push would.
    struct SwappableOs(std::sync::Mutex<String>);

    impl crate::managed_policy::OsPolicySource for SwappableOs {
        fn gateway_url(&self) -> Result<Option<String>> {
            Ok(Some(self.0.lock().unwrap().clone()))
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("gateway.db").display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
    let credentials: crate::connectors::GatewayCredentials = serde_json::from_value(json!({
        "base_url": base_a,
        "installation_id": "install-1",
        "user_id": "user-1",
        "refresh_token": "mg_rt_seed",
        "access_tokens": {}
    }))
    .unwrap();
    CredentialVault::new(secrets.clone())
        .save(&credentials)
        .await
        .unwrap();
    let os = Arc::new(SwappableOs(std::sync::Mutex::new(base_a.clone())));
    let runtime = GatewayRuntime::new(
        store.clone(),
        secrets,
        crate::managed_policy::MemoryProvisionedPolicy::new(),
        os.clone(),
    );

    let sync = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.sync_models().await }
    });
    arrived_rx
        .recv()
        .await
        .expect("the fetch reaches gateway A");

    // While A's model list is in flight, the MDM authority re-points the
    // profile at gateway B.
    *os.0.lock().unwrap() = "https://gateway-b.test".to_string();

    release.notify_one();
    let error = sync
        .await
        .unwrap()
        .expect_err("a sync whose deployment changed mid-fetch must refuse to write");
    assert_eq!(
        error.kind(),
        "gateway_changed",
        "the refusal is the stable retryable conflict, not a fault: {error:?}"
    );

    assert!(
        providers::read_gateway_snapshot(&*store)
            .await
            .unwrap()
            .is_none(),
        "gateway A's models must not be stamped into the snapshot"
    );
}

#[tokio::test]
async fn status_reflects_the_session_and_sign_out_clears_the_snapshot() {
    let address = serve(Arc::new(FakeGateway::default())).await;
    let base = format!("http://{address}");
    let (runtime, store, _directory) = signed_in_runtime(&base).await;
    runtime.sync_models().await.unwrap();

    let status = runtime.status().await.unwrap();
    assert!(status.base_url.is_some() && status.signed_in);
    assert_eq!(status.account_hint.as_deref(), Some("abaas@example.test"));
    assert_eq!(status.installation_id.as_deref(), Some("install-1"));
    assert_eq!(status.model_count, 2);
    assert_eq!(status.sign_in, SignInProgress::Idle);
    // The projection carries no token-shaped material.
    let json = serde_json::to_string(&status).unwrap();
    assert!(!json.contains("mg_at_"));
    assert!(!json.contains("mg_rt_"));

    runtime.sign_out().await.unwrap();
    let status = runtime.status().await.unwrap();
    assert!(!status.signed_in);
    assert_eq!(status.model_count, 0);
    assert!(status.account_hint.is_none());
    let policy = runtime.policy().unwrap();
    assert!(providers::gateway_models(&*store, &policy, None)
        .await
        .unwrap()
        .is_empty());
    // Managed policy is a separate layer with a separate lifecycle:
    // disconnecting the session must never deprovision the profile.
    assert!(policy.managed, "sign-out must not deprovision the profile");
}

#[tokio::test]
async fn mounts_a_gateway_endpoint_from_the_signed_in_session() {
    let gateway = Arc::new(FakeGateway::default());
    let address = serve(gateway.clone()).await;
    let base = format!("http://{address}");
    let (runtime, store, _directory) = signed_in_runtime(&base).await;

    let mcp = mcp_for(&runtime, &store);
    let definition = crate::mcp_config::McpServerDefinition {
        name: "tools".to_string(),
        command: None,
        args: Vec::new(),
        env: std::collections::BTreeSet::new(),
        env_values: std::collections::BTreeMap::new(),
        env_from: Vec::new(),
        cwd: None,
        url: None,
        bearer_token_env: None,
        gateway_endpoint: Some("tools".to_string()),
        request_timeout_ms: 60_000,
        enabled: true,
        plugin: None,
        launch: None,
    };
    // No environment variable anywhere: the URL and bearer come from the
    // stored session via a resource-scoped refresh (asserted inside the
    // fixture endpoint).
    let info = mcp
        .replace(crate::mcp_config::McpServersConfig {
            servers: vec![definition],
        })
        .await
        .unwrap();
    assert_eq!(
        info.servers[0].health,
        crate::mcp_config::McpHealth::Healthy
    );
    assert!(mcp.snapshot().get("mcp__tools__lookup").is_some());

    // The handshake rode a context-bearing token: an attested endpoint
    // refuses any request minted without one, so the connect mint is what
    // makes attested mounts able to list their tools at all.
    let connect_context = {
        let minted = gateway.minted.lock().unwrap();
        let (_, context) = minted
            .iter()
            .find(|(resource, _)| resource == "mcp:tools")
            .expect("the mount minted an mcp:tools token")
            .clone();
        context.expect("the connect mint carries an attestation context")
    };

    // A tool call from a chat presents that chat's bearer, minted inside
    // the same attestation context the chat's inference tokens carry —
    // the invariant that lets an attested endpoint match the call against
    // the observation the inference recorded.
    let chat = tidebreak_core::id::ChatId::new();
    let snapshot = mcp.snapshot();
    let tool = snapshot.get("mcp__tools__lookup").unwrap();
    let ctx = tidebreak_core::ToolCtx::without_private_scratch(chat, None);
    tool.execute(&ctx, json!({})).await.unwrap();

    let source = runtime
        .route_token_source()
        .await
        .expect("signed-in runtime offers a token source");
    source.bearer_token_for(Some(chat)).await.unwrap();
    {
        let minted = gateway.minted.lock().unwrap();
        let chat_mcp = minted
            .iter()
            .filter(|(resource, _)| resource == "mcp:tools")
            .nth(1)
            .map(|(_, context)| context.clone().unwrap())
            .expect("the tool call minted a second mcp:tools token");
        let chat_llm = minted
            .iter()
            .find(|(resource, _)| resource == "llm")
            .map(|(_, context)| context.clone().unwrap())
            .expect("the chat's inference minted an llm token");
        assert_ne!(chat_mcp, connect_context);
        assert_eq!(chat_mcp, chat_llm);
    }
    {
        let requests = gateway.mcp_requests.lock().unwrap();
        let handshake: Vec<&str> = requests
            .iter()
            .filter(|(method, _)| method == "initialize" || method == "tools/list")
            .map(|(_, bearer)| bearer.as_str())
            .collect();
        assert!(!handshake.is_empty());
        let call = requests
            .iter()
            .find(|(method, _)| method == "tools/call")
            .map(|(_, bearer)| bearer.as_str())
            .expect("the tool call reached the endpoint");
        assert!(handshake.iter().all(|bearer| *bearer == handshake[0]));
        assert_ne!(call, handshake[0]);
    }

    // Sign-out degrades the mount to a secret-free sign-in diagnostic and
    // keeps the definition; the tool leaves the registry.
    runtime.sign_out().await.unwrap();
    let error = mcp.reconnect("tools").await.err().unwrap();
    assert!(!error.to_string().contains("mg_at_"), "{error}");
    let info = mcp.info().await;
    assert_eq!(
        info.servers[0].health,
        crate::mcp_config::McpHealth::Degraded
    );
    assert_eq!(
        info.servers[0].diagnostic.as_deref(),
        Some("Sign in to the model gateway to reconnect this server.")
    );
    assert_eq!(
        info.servers[0].definition.gateway_endpoint.as_deref(),
        Some("tools")
    );
    assert!(mcp.snapshot().get("mcp__tools__lookup").is_none());
}

/// #1441: the assembled `AppState` must give the resolver, the /gateway
/// routes, and MCP dispatch ONE gateway runtime. Attestation contexts
/// live in a per-instance registry, so when `state.mcp` held its own
/// runtime a chat's inference token and its MCP call bearer minted in
/// two different contexts and the gateway's observation-consume refused
/// every attested `tools/call`. Assembled the way `bind_inner` does —
/// one runtime injected into the resolver and the state — and driven
/// across the boundary that used to be two instances: the token source
/// from `state.gateway`, the dispatch bearer through `state.mcp`.
#[tokio::test]
async fn assembled_state_mints_inference_and_mcp_dispatch_in_one_attestation_context() {
    let gateway = Arc::new(FakeGateway::default());
    let address = serve(gateway.clone()).await;
    let base = format!("http://{address}");
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("gateway.db").display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
    let credentials: crate::connectors::GatewayCredentials = serde_json::from_value(json!({
        "base_url": base,
        "installation_id": "install-1",
        "user_id": "user-1",
        "refresh_token": "mg_rt_seed",
        "access_tokens": {}
    }))
    .unwrap();
    CredentialVault::new(secrets.clone())
        .save(&credentials)
        .await
        .unwrap();
    let provisioned_policy = crate::managed_policy::MemoryProvisionedPolicy::new();
    crate::managed_policy::provision(&*provisioned_policy, &base).unwrap();

    let os_policy: Arc<dyn crate::managed_policy::OsPolicySource> =
        Arc::new(crate::managed_policy::NoOsPolicy);
    let runtime = GatewayRuntime::new(
        store.clone(),
        secrets.clone(),
        provisioned_policy.clone(),
        os_policy.clone(),
    );
    let chatgpt = Arc::new(
        crate::chatgpt_runtime::ChatGptRuntime::new(store.clone(), secrets.clone()).unwrap(),
    );
    let resolver = Arc::new(crate::resolver::ConfiguredResolver::new(
        store.clone(),
        secrets.clone(),
        runtime.clone(),
        chatgpt.clone(),
        provisioned_policy.clone(),
        os_policy.clone(),
    ));
    let state = crate::state::AppState::with_gateway_runtime(
        tidebreak_core::Config::desktop(directory.path()),
        store.clone(),
        resolver,
        secrets,
        Arc::new(tidebreak_core::ToolRegistry::new()),
        tidebreak_core::AgentConfig::default(),
        uuid::Uuid::new_v4(),
        runtime,
        chatgpt,
        provisioned_policy,
        os_policy,
    )
    .unwrap();

    // Identity first: dispatch resolves through the very runtime the
    // routes and resolver hold — the cheap pin on the bug class.
    let dispatch = state.mcp.gateway_endpoints();
    assert!(
        std::ptr::eq(
            Arc::as_ptr(&dispatch) as *const (),
            Arc::as_ptr(&state.gateway) as *const (),
        ),
        "MCP dispatch must share the state's gateway runtime"
    );

    // And behaviorally: mount an endpoint through the state's MCP
    // runtime, dispatch one chat's tools/call, mint that chat's
    // router-facing inference token — one attestation context.
    let info = state
        .mcp
        .replace(crate::mcp_config::McpServersConfig {
            servers: vec![crate::mcp_config::McpServerDefinition {
                name: "tools".to_string(),
                command: None,
                args: Vec::new(),
                env: std::collections::BTreeSet::new(),
                env_values: std::collections::BTreeMap::new(),
                env_from: Vec::new(),
                cwd: None,
                url: None,
                bearer_token_env: None,
                gateway_endpoint: Some("tools".to_string()),
                request_timeout_ms: 60_000,
                enabled: true,
                plugin: None,
                launch: None,
            }],
        })
        .await
        .unwrap();
    assert_eq!(
        info.servers[0].health,
        crate::mcp_config::McpHealth::Healthy
    );

    let chat = tidebreak_core::id::ChatId::new();
    let snapshot = state.mcp.snapshot();
    let tool = snapshot.get("mcp__tools__lookup").unwrap();
    let ctx = tidebreak_core::ToolCtx::without_private_scratch(chat, None);
    tool.execute(&ctx, json!({})).await.unwrap();
    let source = state
        .gateway
        .route_token_source()
        .await
        .expect("signed-in runtime offers a token source");
    source.bearer_token_for(Some(chat)).await.unwrap();

    let minted = gateway.minted.lock().unwrap();
    let chat_mcp = minted
        .iter()
        .filter(|(resource, _)| resource == "mcp:tools")
        .nth(1)
        .map(|(_, context)| context.clone().unwrap())
        .expect("the tool call minted a chat-scoped mcp:tools token");
    let chat_llm = minted
        .iter()
        .find(|(resource, _)| resource == "llm")
        .map(|(_, context)| context.clone().unwrap())
        .expect("the chat's inference minted an llm token");
    assert_eq!(
        chat_mcp, chat_llm,
        "inference and MCP dispatch must mint in the same attestation context"
    );
}

/// Mount-by-default, end to end: a reconcile against the entitled apps
/// list mounts the endpoint enabled and connected, and a second
/// reconcile with unchanged entitlements is a strict no-op — the
/// persisted records are untouched, not rewritten to the same shape.
#[tokio::test]
async fn reconcile_auto_mounts_a_newly_entitled_endpoint_exactly_once() {
    let address = serve(Arc::new(FakeGateway::default())).await;
    let base = format!("http://{address}");
    let (runtime, store, _directory) = signed_in_runtime(&base).await;
    let mcp = mcp_for(&runtime, &store);

    runtime.reconcile_endpoint_mounts(&mcp).await.unwrap();
    let info = mcp.info().await;
    assert_eq!(info.servers.len(), 1);
    assert_eq!(info.servers[0].definition.name, "tools");
    assert_eq!(
        info.servers[0].definition.gateway_endpoint.as_deref(),
        Some("tools")
    );
    assert!(info.servers[0].definition.enabled);
    assert_eq!(
        info.servers[0].health,
        crate::mcp_config::McpHealth::Healthy
    );
    assert!(mcp.snapshot().get("mcp__tools__lookup").is_some());

    let records = store.list_connected_apps().await.unwrap();
    runtime.reconcile_endpoint_mounts(&mcp).await.unwrap();
    assert_eq!(
        store.list_connected_apps().await.unwrap(),
        records,
        "a reconcile with no new entitlements must not rewrite the records"
    );
}

/// A failing entitlements fetch degrades to "no reconcile this tick":
/// the error surfaces to the caller (which logs and retries) and the
/// configuration is untouched.
#[tokio::test]
async fn a_failing_entitlements_fetch_leaves_the_configuration_untouched() {
    let gateway = Arc::new(FakeGateway::default());
    gateway.apps_fail.store(true, Ordering::SeqCst);
    let address = serve(gateway).await;
    let base = format!("http://{address}");
    let (runtime, store, _directory) = signed_in_runtime(&base).await;
    let mcp = mcp_for(&runtime, &store);

    assert!(runtime.reconcile_endpoint_mounts(&mcp).await.is_err());
    assert!(mcp.info().await.servers.is_empty());
    assert!(store.list_connected_apps().await.unwrap().is_empty());
}

/// The sign-in surface on an unmanaged profile exists exactly while a
/// pending pairing is parked: it targets the pairing's gateway, commits
/// nothing by merely starting, and a dismissal restores the refusal —
/// the write-path guard the retired confirmation dialog used to be.
#[tokio::test]
async fn a_pending_pairing_is_what_sign_in_targets_until_dismissed() {
    let address = serve(Arc::new(FakeGateway::default())).await;
    let base = format!("http://{address}/");
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("gateway.db").display()
        ))
        .await
        .unwrap(),
    );
    let runtime = GatewayRuntime::new(
        store.clone(),
        Arc::new(MockSecrets::default()),
        crate::managed_policy::MemoryProvisionedPolicy::new(),
        Arc::new(crate::managed_policy::NoOsPolicy),
    );
    let mcp = mcp_for(&runtime, &store);

    // Unmanaged with nothing pending: the legible refusal.
    assert!(runtime.begin_sign_in(mcp.clone()).await.is_err());

    runtime
        .register_pending_pairing(base.clone(), mcp.clone(), None)
        .await;
    let url = runtime.begin_sign_in(mcp.clone()).await.unwrap();
    assert!(
        url.starts_with(&format!("{base}oauth/authorize")),
        "sign-in must target the pending gateway: {url}"
    );
    // Starting the flow wrote nothing durable.
    let policy = runtime.policy().unwrap();
    assert!(!policy.managed);

    runtime.dismiss_pending_pairing().await;
    assert_eq!(runtime.pending_pairing_url().await, None);
    assert_eq!(
        runtime.status().await.unwrap().sign_in,
        SignInProgress::Idle
    );
    assert!(runtime.begin_sign_in(mcp).await.is_err());
}

#[tokio::test]
async fn a_session_for_a_different_gateway_reads_signed_out() {
    let address = serve(Arc::new(FakeGateway::default())).await;
    let base = format!("http://{address}");
    // The profile is managed to one deployment while the stored session
    // (minted against `base`) belongs to another.
    let (runtime, _store, _directory) = signed_in_runtime_at(&base, "http://127.0.0.1:9").await;

    let status = runtime.status().await.unwrap();
    assert!(status.base_url.is_some());
    assert!(
        !status.signed_in,
        "a foreign session must not read signed-in"
    );
    assert!(status.account_hint.is_none());
    assert!(status.installation_id.is_none());
    assert!(runtime.route_token_source().await.is_none());
}

/// An OS (MDM) authority asserting one gateway, as a device-managed
/// profile has.
struct OsManaged(String);

impl crate::managed_policy::OsPolicySource for OsManaged {
    fn gateway_url(&self) -> Result<Option<String>> {
        Ok(Some(self.0.clone()))
    }
}

/// A pure-MDM profile has no stored provider row at all — nothing ever
/// wrote one — so reading the status from that row rendered "not
/// configured" while sign-in, routing, and mounts all worked. Policy is
/// the authority for a managed profile here, exactly as it is for the
/// connection itself.
#[tokio::test]
async fn a_pure_mdm_profile_reads_configured_from_policy_without_a_stored_row() {
    let address = serve(Arc::new(FakeGateway::default())).await;
    let base = format!("http://{address}");
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("gateway.db").display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
    let credentials: crate::connectors::GatewayCredentials = serde_json::from_value(json!({
        "base_url": base,
        "installation_id": "install-1",
        "user_id": "user-1",
        "account_hint": "abaas@example.test",
        "refresh_token": "mg_rt_seed",
        "access_tokens": {}
    }))
    .unwrap();
    CredentialVault::new(secrets.clone())
        .save(&credentials)
        .await
        .unwrap();
    let runtime = GatewayRuntime::new(
        store.clone(),
        secrets,
        crate::managed_policy::MemoryProvisionedPolicy::new(),
        Arc::new(OsManaged(base.clone())),
    );

    assert!(
        providers::read_gateway_snapshot(&*store)
            .await
            .unwrap()
            .is_none(),
        "the fixture must have no stored gateway state for the assertion to mean anything"
    );
    let status = runtime.status().await.unwrap();
    assert!(status.signed_in);
    assert_eq!(
        status.base_url.as_deref(),
        Some(format!("{base}/").as_str())
    );
    assert_eq!(status.account_hint.as_deref(), Some("abaas@example.test"));
}

#[tokio::test]
async fn a_signed_out_runtime_offers_no_route() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("gateway.db").display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
    let provisioned = crate::managed_policy::MemoryProvisionedPolicy::new();
    crate::managed_policy::provision(&*provisioned, "http://127.0.0.1:1").unwrap();
    let runtime = GatewayRuntime::new(
        store.clone(),
        secrets,
        provisioned,
        Arc::new(crate::managed_policy::NoOsPolicy),
    );

    assert!(runtime.route_token_source().await.is_none());
    let policy = runtime.policy().unwrap();
    let routes =
        providers::collect_routes(&*store, &*runtime.secrets, None, None, None, &policy).await;
    assert!(routes
        .iter()
        .all(|route| route.kind != tidebreak_router::RouteKind::ModelGateway));
}

/// The legacy hard cut: an unmanaged profile with a stored additive
/// gateway row — and even a leftover signed-in session — has zero
/// gateway surface. The row is ignored, never auto-converted to
/// managed: lockdown must not be imposed without the pairing consent
/// flow, so the remedy is pairing, and the sign-in surface says so.
#[tokio::test]
async fn an_unmanaged_profile_with_a_legacy_row_has_no_gateway_surface() {
    struct StaticTokens;

    #[async_trait]
    impl BearerTokenSource for StaticTokens {
        async fn bearer_token(&self) -> Result<String> {
            Ok("mg_at_test".into())
        }
    }

    let address = serve(Arc::new(FakeGateway::default())).await;
    let base = format!("http://{address}");
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("gateway.db").display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
    let credentials: crate::connectors::GatewayCredentials = serde_json::from_value(json!({
        "base_url": base,
        "installation_id": "install-1",
        "user_id": "user-1",
        "refresh_token": "mg_rt_seed",
        "access_tokens": {}
    }))
    .unwrap();
    CredentialVault::new(secrets.clone())
        .save(&credentials)
        .await
        .unwrap();
    providers::write_config(
        &*store,
        crate::providers::ProviderKind::ModelGateway,
        &providers::ProviderConfig {
            enabled: true,
            base_url: Some(format!("{base}/")),
            models: vec![CustomModelConfig {
                id: "legacy-model".to_string(),
                upstream_id: None,
                display_name: None,
                context_window: 32_768,
                max_output_tokens: 4_096,
                ..Default::default()
            }],
        },
    )
    .await
    .unwrap();
    let runtime = GatewayRuntime::new(
        store.clone(),
        secrets.clone(),
        crate::managed_policy::MemoryProvisionedPolicy::new(),
        Arc::new(crate::managed_policy::NoOsPolicy),
    );

    // The boot cutover warns and ignores the row: no snapshot appears
    // and the profile stays unmanaged.
    let policy = runtime.policy().unwrap();
    providers::retire_legacy_gateway_row(&*store, &policy)
        .await
        .unwrap();
    assert!(providers::read_gateway_snapshot(&*store)
        .await
        .unwrap()
        .is_none());
    assert!(
        !policy.managed,
        "a legacy row must never auto-convert the profile to managed"
    );

    // Including the machine offer: with no gateway of its own to ask,
    // an unmanaged profile reads none whatever this one advertises.
    assert!(runtime.offered_machine().await.url.is_none());

    // The status surface reads no gateway and signed out.
    let status = runtime.status().await.unwrap();
    assert!(!status.signed_in);
    assert!(status.base_url.is_none());
    assert_eq!(status.model_count, 0);

    // Routing, the picker, and enumeration offer nothing — even with a
    // token source in hand, the gateway route is not built.
    assert!(runtime.route_token_source().await.is_none());
    let tokens: Arc<dyn BearerTokenSource> = Arc::new(StaticTokens);
    let routes =
        providers::collect_routes(&*store, &*secrets, Some(tokens), None, None, &policy).await;
    assert!(routes
        .iter()
        .all(|route| route.kind != tidebreak_router::RouteKind::ModelGateway));
    assert!(providers::catalog_models(&*store, &*secrets, &policy, None)
        .await
        .unwrap()
        .iter()
        .all(|model| model.policy.provider != crate::providers::ProviderKind::ModelGateway));
    assert!(providers::list_providers(&*store, &*secrets, &policy, None)
        .await
        .unwrap()
        .iter()
        .all(|provider| provider.kind != crate::providers::ProviderKind::ModelGateway));
    assert!(!providers::provider_is_usable(
        &*store,
        &*secrets,
        crate::providers::ProviderKind::ModelGateway,
        &policy,
        None
    )
    .await
    .unwrap());

    // The sign-in surface is managed-only, and the refusal names the
    // remedy.
    let error = runtime
        .begin_sign_in(mcp_for(&runtime, &store))
        .await
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("pair via your gateway"),
        "{error}"
    );
    assert!(runtime.sync_models().await.is_err());
    assert!(runtime
        .apps(&tidebreak_core::OwnerId::local())
        .await
        .is_err());
    assert!(runtime.sign_out().await.is_err());
}

/// The boot cutover's carry-forward: a managed profile — the shape a
/// gateway-page pairing produces — keeps the models its row had synced,
/// once, and only from the deployment policy actually names.
///
/// Nothing re-syncs the entitled set for a reader who is already signed
/// in, so dropping this leaves their picker empty until they find the
/// refresh button.
///
/// The row URL is the VERBATIM form here: the old provider write path
/// did not normalize (#935), so a profile that became managed by MDM
/// over such a row holds "https://corp.gateway" beside a policy's
/// "https://corp.gateway/". Compare deployments as strings instead of
/// URLs and that profile silently reaches the picker empty.
#[tokio::test]
async fn boot_carries_a_managed_rows_snapshot_forward_once() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("gateway.db").display()
        ))
        .await
        .unwrap(),
    );
    let provisioned = crate::managed_policy::MemoryProvisionedPolicy::new();
    crate::managed_policy::provision(&*provisioned, "https://corp.gateway").unwrap();
    let legacy_row = |id: &str| providers::ProviderConfig {
        enabled: true,
        base_url: Some("https://corp.gateway".to_string()),
        models: vec![CustomModelConfig {
            id: id.to_string(),
            upstream_id: None,
            display_name: None,
            context_window: 32_768,
            max_output_tokens: 4_096,
            ..Default::default()
        }],
    };
    providers::write_config(
        &*store,
        crate::providers::ProviderKind::ModelGateway,
        &legacy_row("carried-model"),
    )
    .await
    .unwrap();

    let policy =
        crate::managed_policy::resolve(&*provisioned, &crate::managed_policy::NoOsPolicy).unwrap();
    providers::retire_legacy_gateway_row(&*store, &policy)
        .await
        .unwrap();
    let models = providers::gateway_models(&*store, &policy, None)
        .await
        .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "carried-model");

    // The row is gone once it has been dealt with, so "retired entirely"
    // is true of the store and not only of the read paths.
    assert!(
        providers::read_config(&*store, crate::providers::ProviderKind::ModelGateway)
            .await
            .unwrap()
            .base_url
            .is_none()
    );

    // One-shot: once a snapshot exists, a row that reappears never
    // overwrites it.
    providers::write_config(
        &*store,
        crate::providers::ProviderKind::ModelGateway,
        &legacy_row("other-model"),
    )
    .await
    .unwrap();
    providers::retire_legacy_gateway_row(&*store, &policy)
        .await
        .unwrap();
    let models = providers::gateway_models(&*store, &policy, None)
        .await
        .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "carried-model");
}

/// A row from a deployment the profile is no longer managed by is not
/// carried forward: its models describe another gateway's entitlements.
/// The row still goes.
#[tokio::test]
async fn boot_discards_a_snapshot_from_a_foreign_deployment() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("gateway.db").display()
        ))
        .await
        .unwrap(),
    );
    let provisioned = crate::managed_policy::MemoryProvisionedPolicy::new();
    crate::managed_policy::provision(&*provisioned, "https://corp.gateway").unwrap();
    providers::write_config(
        &*store,
        crate::providers::ProviderKind::ModelGateway,
        &providers::ProviderConfig {
            enabled: true,
            base_url: Some("https://other.gateway".to_string()),
            models: vec![CustomModelConfig {
                id: "foreign-model".to_string(),
                upstream_id: None,
                display_name: None,
                context_window: 32_768,
                max_output_tokens: 4_096,
                ..Default::default()
            }],
        },
    )
    .await
    .unwrap();

    let policy =
        crate::managed_policy::resolve(&*provisioned, &crate::managed_policy::NoOsPolicy).unwrap();
    providers::retire_legacy_gateway_row(&*store, &policy)
        .await
        .unwrap();
    assert!(providers::read_gateway_snapshot(&*store)
        .await
        .unwrap()
        .is_none());
    assert!(
        providers::read_config(&*store, crate::providers::ProviderKind::ModelGateway)
            .await
            .unwrap()
            .base_url
            .is_none()
    );
}

/// The unmanaged half of the cutover: the row is ignored (never
/// converted to managed) and dropped, so the warning naming the remedy
/// is a one-time upgrade notice rather than a line on every boot.
#[tokio::test]
async fn boot_drops_an_unmanaged_legacy_row_without_making_it_managed() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("gateway.db").display()
        ))
        .await
        .unwrap(),
    );
    providers::write_config(
        &*store,
        crate::providers::ProviderKind::ModelGateway,
        &providers::ProviderConfig {
            enabled: true,
            base_url: Some("https://corp.gateway".to_string()),
            models: vec![CustomModelConfig {
                id: "legacy-model".to_string(),
                upstream_id: None,
                display_name: None,
                context_window: 32_768,
                max_output_tokens: 4_096,
                ..Default::default()
            }],
        },
    )
    .await
    .unwrap();

    let provisioned = crate::managed_policy::MemoryProvisionedPolicy::new();
    let policy =
        crate::managed_policy::resolve(&*provisioned, &crate::managed_policy::NoOsPolicy).unwrap();
    providers::retire_legacy_gateway_row(&*store, &policy)
        .await
        .unwrap();

    assert!(
        !policy.managed,
        "a legacy row must never auto-convert the profile to managed"
    );
    assert!(providers::read_gateway_snapshot(&*store)
        .await
        .unwrap()
        .is_none());
    assert!(
        providers::read_config(&*store, crate::providers::ProviderKind::ModelGateway)
            .await
            .unwrap()
            .base_url
            .is_none()
    );
}

/// A secret store whose reads fail while `fail_reads` is set — the
/// transient shape of a keychain read whose ACL no longer matches the
/// running binary (denied or pending prompt), as opposed to a confirmed
/// absence.
#[derive(Default)]
struct FlakySecrets {
    values: MockSecrets,
    fail_reads: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl SecretProvider for FlakySecrets {
    async fn get_secret(&self, key: &str) -> Result<Option<String>> {
        if self.fail_reads.load(Ordering::SeqCst) {
            return Err(AgentError::Secret("keychain read denied".into()));
        }
        self.values.get_secret(key).await
    }
    async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
        self.values.set_secret(key, value).await
    }
    async fn delete_secret(&self, key: &str) -> Result<()> {
        self.values.delete_secret(key).await
    }
}

/// Retire is a one-way door — the session is gone afterwards — so it
/// must not open on a transient read error: a keychain read that fails
/// (or an unparsable blob, which also errors the load) proves nothing
/// about whether the session is superseded. Only a successful read that
/// shows a definitive policy mismatch may retire it.
#[tokio::test]
async fn boot_keeps_the_session_when_the_credential_read_errors() {
    let secrets = Arc::new(FlakySecrets::default());
    let credentials: crate::connectors::GatewayCredentials = serde_json::from_value(json!({
        "base_url": "http://127.0.0.1:1",
        "installation_id": "install-1",
        "user_id": "user-1",
        "refresh_token": "mg_rt_survivor",
        "access_tokens": {}
    }))
    .unwrap();
    CredentialVault::new(secrets.clone())
        .save(&credentials)
        .await
        .unwrap();

    // Unmanaged policy: a readable session WOULD be retired. With the
    // read erroring, retire must leave it exactly where it is.
    let provisioned = crate::managed_policy::MemoryProvisionedPolicy::new();
    let policy =
        crate::managed_policy::resolve(&*provisioned, &crate::managed_policy::NoOsPolicy).unwrap();
    assert!(!policy.managed);
    secrets.fail_reads.store(true, Ordering::SeqCst);
    retire_superseded_gateway_session(secrets.clone(), &policy)
        .await
        .unwrap();
    secrets.fail_reads.store(false, Ordering::SeqCst);
    assert!(
        crate::connectors::has_stored_credentials(&*secrets).await,
        "a transient read error must not retire the session"
    );
}

/// The session half of the legacy hard cut: an unmanaged profile with a
/// session left over from the retired additive mode has no surface that
/// could ever revoke it, so boot clears it. Without this the refresh
/// token lives in the keychain forever.
#[tokio::test]
async fn boot_clears_a_gateway_session_left_on_an_unmanaged_profile() {
    let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
    let seed = |secrets: Arc<dyn SecretProvider>, base_url: &'static str| async move {
        let credentials: crate::connectors::GatewayCredentials = serde_json::from_value(json!({
            "base_url": base_url,
            "installation_id": "install-1",
            "user_id": "user-1",
            "refresh_token": "mg_rt_zombie",
            "access_tokens": {}
        }))
        .unwrap();
        CredentialVault::new(secrets)
            .save(&credentials)
            .await
            .unwrap();
    };
    // Nothing listens here: the revoke fails fast and the clear happens
    // anyway, which is the contract.
    seed(secrets.clone(), "http://127.0.0.1:1").await;

    let provisioned = crate::managed_policy::MemoryProvisionedPolicy::new();
    let policy =
        crate::managed_policy::resolve(&*provisioned, &crate::managed_policy::NoOsPolicy).unwrap();
    assert!(!policy.managed);
    retire_superseded_gateway_session(secrets.clone(), &policy)
        .await
        .unwrap();
    assert!(
        !crate::connectors::has_stored_credentials(&*secrets).await,
        "the retired session must not survive boot on an unmanaged profile"
    );

    // A stored base URL that no longer passes the gateway contract has
    // no connection to revoke through, so only the unconditional clear
    // can retire it. Drop that clear and the credential survives here.
    seed(secrets.clone(), "http://user:pw@stale.example").await;
    retire_superseded_gateway_session(secrets.clone(), &policy)
        .await
        .unwrap();
    assert!(
        !crate::connectors::has_stored_credentials(&*secrets).await,
        "a session whose stored URL cannot be parsed must still be cleared"
    );

    // A managed profile's session is untouched: it is the credential the
    // profile actually runs on.
    seed(secrets.clone(), "https://corp.gateway/").await;
    crate::managed_policy::provision(&*provisioned, "https://corp.gateway").unwrap();
    let policy =
        crate::managed_policy::resolve(&*provisioned, &crate::managed_policy::NoOsPolicy).unwrap();
    retire_superseded_gateway_session(secrets.clone(), &policy)
        .await
        .unwrap();
    assert!(crate::connectors::has_stored_credentials(&*secrets).await);
}

/// The managed analogue of the legacy hard cut: an MDM re-point
/// supersedes the stored session, and boot is the only surface that will
/// ever revoke it at the old deployment.
#[tokio::test]
async fn boot_retires_the_session_an_mdm_repoint_superseded() {
    let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
    let old_gateway = Arc::new(FakeGateway::default());
    let old_base = format!("http://{}", serve(old_gateway.clone()).await);
    let credentials: crate::connectors::GatewayCredentials = serde_json::from_value(json!({
        "base_url": old_base,
        "installation_id": "install-1",
        "user_id": "user-1",
        "refresh_token": "mg_rt_zombie",
        "access_tokens": {}
    }))
    .unwrap();
    CredentialVault::new(secrets.clone())
        .save(&credentials)
        .await
        .unwrap();
    let provisioned = crate::managed_policy::MemoryProvisionedPolicy::new();
    crate::managed_policy::provision(&*provisioned, "https://corp-new.gateway").unwrap();
    let mut policy =
        crate::managed_policy::resolve(&*provisioned, &crate::managed_policy::NoOsPolicy).unwrap();
    assert!(policy.managed);

    // A managed policy with no usable URL is misconfiguration, not a
    // re-point: the session is left for the repaired policy to judge.
    policy.gateway_url = None;
    retire_superseded_gateway_session(secrets.clone(), &policy)
        .await
        .unwrap();
    assert!(crate::connectors::has_stored_credentials(&*secrets).await);

    policy.gateway_url = Some("https://corp-new.gateway".into());
    retire_superseded_gateway_session(secrets.clone(), &policy)
        .await
        .unwrap();
    assert!(
        !crate::connectors::has_stored_credentials(&*secrets).await,
        "the superseded session must not survive the re-point"
    );
    // Revoked at the old deployment, with the superseded refresh token.
    assert_eq!(
        old_gateway.revoked.lock().unwrap().as_slice(),
        ["mg_rt_zombie"]
    );
}

/// The durability the policy file exists for: a profile below the
/// migration pin is deleted and rebuilt on first boot, and the
/// provisioned policy — a sidecar file in the data directory — must
/// survive that. The session it authorizes is then retained: before the
/// policy lived in the file, the reset resolved the profile unmanaged and
/// boot retired the session, forcing a full re-pair.
#[tokio::test]
async fn a_schema_epoch_reset_keeps_the_policy_and_its_session() {
    let directory = tempfile::tempdir().unwrap();
    let data_dir = directory.path();
    let provisioned = crate::managed_policy::ProvisionedPolicyFile::in_data_dir(data_dir);
    crate::managed_policy::provision(&provisioned, "https://corp.gateway").unwrap();

    // The profile's database, holding a chat so its loss is observable,
    // and the gateway session the policy authorizes.
    let store = DbStore::connect(&format!(
        "sqlite://{}?mode=rwc",
        data_dir.join("tidebreak.db").display()
    ))
    .await
    .unwrap();
    store
        .create_chat(&tidebreak_core::Chat {
            id: tidebreak_core::ChatId::new(),
            project_id: None,
            title: Some("lost to the reset".to_owned()),
            model: None,
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            memory_incognito: false,
            created_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    // An explicit close, not a drop: the reset below deletes the SQLite
    // files, which Windows refuses while a handle is open.
    store.close().await.unwrap();
    let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
    let credentials: crate::connectors::GatewayCredentials = serde_json::from_value(json!({
        "base_url": "https://corp.gateway/",
        "installation_id": "install-1",
        "user_id": "user-1",
        "refresh_token": "mg_rt_seed",
        "access_tokens": {}
    }))
    .unwrap();
    CredentialVault::new(secrets.clone())
        .save(&credentials)
        .await
        .unwrap();

    // The epoch reset, as desktop_schema performs it: the database files
    // are deleted; everything else in the data directory stays.
    std::fs::remove_file(data_dir.join("tidebreak.db")).unwrap();
    assert!(!data_dir.join("tidebreak.db").exists());

    // Resolution needs no store at all now: the policy survives, still
    // managed to the same gateway, and the retire step — boot's session
    // cleanup — keeps the session it stands behind.
    let policy =
        crate::managed_policy::resolve(&provisioned, &crate::managed_policy::NoOsPolicy).unwrap();
    assert!(policy.managed && !policy.misconfigured);
    assert_eq!(policy.gateway_url.as_deref(), Some("https://corp.gateway/"));
    retire_superseded_gateway_session(secrets.clone(), &policy)
        .await
        .unwrap();
    assert!(
        crate::connectors::has_stored_credentials(&*secrets).await,
        "a policy that survives the reset must keep the session it authorizes"
    );
}

/// A completed sign-in supersedes whatever session is stored — possibly
/// one minted at a different deployment, after a re-point. Its refresh
/// token is revoked at its own gateway before the overwrite orphans it.
#[tokio::test]
async fn signing_in_revokes_the_superseded_session_at_its_own_gateway() {
    let old_gateway = Arc::new(FakeGateway::default());
    let old_base = format!("http://{}", serve(old_gateway.clone()).await);
    let new_gateway = Arc::new(FakeGateway::default());
    let new_base = format!("http://{}", serve(new_gateway.clone()).await);
    let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
    let superseded: crate::connectors::GatewayCredentials = serde_json::from_value(json!({
        "base_url": old_base,
        "installation_id": "install-1",
        "user_id": "user-1",
        "refresh_token": "mg_rt_zombie",
        "access_tokens": {}
    }))
    .unwrap();
    CredentialVault::new(secrets.clone())
        .save(&superseded)
        .await
        .unwrap();

    let connection = GatewayConnection::new(
        GatewayAuth::new(GatewayAuthConfig::new(&new_base).unwrap()).unwrap(),
        CredentialVault::new(secrets.clone()),
    );
    connection
        .store_session(&crate::connectors::AuthorizedSession {
            meta: crate::connectors::GatewayMeta {
                api_version: "1".into(),
                installation_id: "install-2".into(),
                gateway_version: "1".into(),
                public_url: new_base.clone(),
                auth_mode: "oauth".into(),
                tidebreak_machine_url: None,
                surfaces: None,
            },
            identity: crate::connectors::GatewayIdentity {
                user_id: "user-2".into(),
                email: Some("user@example.test".into()),
                display_name: None,
                session_id: "session-2".into(),
                installation_id: "install-2".into(),
            },
            tokens: crate::connectors::TokenSet {
                access_token: "mg_at_fresh".into(),
                refresh_token: "mg_rt_fresh".into(),
                expires_at_unix: u64::MAX,
                scope: "models:read inference:invoke".into(),
                resource: "control".into(),
                installation_id: "install-2".into(),
            },
        })
        .await
        .unwrap();

    // The revoke went to the superseded session's own deployment — not
    // the connection's — and the new session is stored for the new one.
    assert_eq!(
        old_gateway.revoked.lock().unwrap().as_slice(),
        ["mg_rt_zombie"]
    );
    assert!(new_gateway.revoked.lock().unwrap().is_empty());
    assert!(crate::connectors::has_stored_credentials_for(&*secrets, &new_base).await);
}
