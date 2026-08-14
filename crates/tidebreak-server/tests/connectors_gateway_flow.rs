//! End-to-end exercises of the gateway OAuth client against an in-process
//! fake that implements the deployment's real contract: PKCE S256 checked at
//! exchange, one-time codes, rotating refresh tokens with reuse detection,
//! resource-scoped audiences, and bearer-authenticated CLI endpoints.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tidebreak_core::{Result as CoreResult, SecretProvider};
use tidebreak_server::connectors::{
    is_sign_in_required, CredentialVault, GatewayAuth, GatewayAuthConfig, GatewayCatalogFetch,
    GatewayConnection, GatewayInvokeOutcome, RESOURCE_CONTROL, RESOURCE_LLM,
};

const INSTALLATION: &str = "11111111-2222-3333-4444-555555555555";
const USER: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const SESSION: &str = "99999999-8888-7777-6666-555555555555";

#[derive(Default)]
struct MockSecrets(Mutex<HashMap<String, String>>);

#[async_trait::async_trait]
impl SecretProvider for MockSecrets {
    async fn get_secret(&self, key: &str) -> CoreResult<Option<String>> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }

    async fn set_secret(&self, key: &str, value: &str) -> CoreResult<()> {
        self.0.lock().unwrap().insert(key.into(), value.into());
        Ok(())
    }

    async fn delete_secret(&self, key: &str) -> CoreResult<()> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}

struct PendingCode {
    challenge: String,
    redirect_uri: String,
}

#[derive(Default)]
struct FakeGateway {
    codes: Mutex<HashMap<String, PendingCode>>,
    /// refresh token -> still valid
    refresh_tokens: Mutex<HashMap<String, bool>>,
    /// access token -> resource
    access_tokens: Mutex<HashMap<String, String>>,
    revoked: Mutex<Vec<String>>,
    token_requests: AtomicUsize,
    corrupt_state: AtomicBool,
    /// Serve 404 for `/api/v1/cli/apps`, like a gateway that predates it.
    apps_unsupported: AtomicBool,
    /// Serve 404 for `/api/v1/me/catalog`, like a gateway that predates the
    /// member catalog while still serving the per-surface reads.
    member_catalog_unsupported: AtomicBool,
    /// Serve 404 for the per-app catalog read, like a gateway that predates
    /// it while still listing entitlements.
    catalogs_unsupported: AtomicBool,
    /// Serve 404 for the shared-app invoke route, like a gateway that predates
    /// shared apps entirely.
    shared_apps_unsupported: AtomicBool,
    /// Answer the next shared-app invoke with the typed
    /// `authorization_required` failure instead of executing it.
    shared_app_needs_authorization: AtomicBool,
    /// Shared-app invoke bodies observed, with the bearer's resource.
    shared_app_invokes: Mutex<Vec<(String, Option<String>, Value)>>,
    /// Attestation context ids observed on refresh grants, in order.
    contexts_seen: Mutex<Vec<String>>,
    /// Reject the next unseen attestation context with `invalid_target`,
    /// like a context row pinned to a superseded session.
    reject_next_context: AtomicBool,
}

impl FakeGateway {
    fn mint(self: &Arc<Self>, resource: &str) -> Value {
        let access = format!("mg_at_{}", uuid_like());
        let refresh = format!("mg_rt_{}", uuid_like());
        self.access_tokens
            .lock()
            .unwrap()
            .insert(access.clone(), resource.to_string());
        self.refresh_tokens
            .lock()
            .unwrap()
            .insert(refresh.clone(), true);
        json!({
            "access_token": access,
            "token_type": "Bearer",
            "expires_in": 600,
            "refresh_token": refresh,
            "scope": scope_for(resource),
            "resource": resource,
            "installation_id": INSTALLATION,
        })
    }
}

fn scope_for(resource: &str) -> &'static str {
    match resource {
        "control" => "profile models:read",
        "llm" => "models:read inference:invoke",
        _ => "mcp:invoke",
    }
}

fn uuid_like() -> String {
    use std::sync::atomic::AtomicU64;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("token{}", NEXT.fetch_add(1, Ordering::SeqCst))
}

async fn meta() -> Json<Value> {
    Json(json!({
        "api_version": "v1",
        "installation_id": INSTALLATION,
        "gateway_version": "0.1.0-test",
        "public_url": "http://fake.test/",
        "auth_mode": "development",
    }))
}

async fn authorize(
    State(gateway): State<Arc<FakeGateway>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if query.get("client_id").map(String::as_str) != Some("tidebreak")
        || query.get("code_challenge_method").map(String::as_str) != Some("S256")
        || query.get("response_type").map(String::as_str) != Some("code")
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let code = format!("code_{}", uuid_like());
    let redirect_uri = query.get("redirect_uri").cloned().unwrap_or_default();
    gateway.codes.lock().unwrap().insert(
        code.clone(),
        PendingCode {
            challenge: query.get("code_challenge").cloned().unwrap_or_default(),
            redirect_uri: redirect_uri.clone(),
        },
    );
    let state = if gateway.corrupt_state.load(Ordering::SeqCst) {
        "wrong-state".to_string()
    } else {
        query.get("state").cloned().unwrap_or_default()
    };
    Redirect::to(&format!("{redirect_uri}?code={code}&state={state}")).into_response()
}

async fn token(
    State(gateway): State<Arc<FakeGateway>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    gateway.token_requests.fetch_add(1, Ordering::SeqCst);
    match form.get("grant_type").map(String::as_str) {
        Some("authorization_code") => {
            let Some(code) = form.get("code") else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            let Some(pending) = gateway.codes.lock().unwrap().remove(code) else {
                return invalid_grant();
            };
            let verifier = form.get("code_verifier").cloned().unwrap_or_default();
            let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
            if challenge != pending.challenge
                || form.get("redirect_uri") != Some(&pending.redirect_uri)
                || form.get("client_id").map(String::as_str) != Some("tidebreak")
            {
                return invalid_grant();
            }
            Json(gateway.mint("control")).into_response()
        }
        Some("refresh_token") => {
            // Attribution rides the refresh grant: every minted access token
            // must carry the client name, or usage reads as `generic`.
            if form.get("client_name").map(String::as_str) != Some("tidebreak") {
                return invalid_grant();
            }
            // Context validation precedes refresh consumption, as on the real
            // gateway: a rejected context aborts the grant without rotating.
            if let Some(context) = form.get("attestation_context_id") {
                if uuid::Uuid::parse_str(context).is_err() {
                    return invalid_target();
                }
                let unseen = !gateway.contexts_seen.lock().unwrap().contains(context);
                if unseen && gateway.reject_next_context.swap(false, Ordering::SeqCst) {
                    return invalid_target();
                }
                gateway.contexts_seen.lock().unwrap().push(context.clone());
            }
            let Some(refresh) = form.get("refresh_token") else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            let mut refresh_tokens = gateway.refresh_tokens.lock().unwrap();
            if refresh_tokens.get(refresh) != Some(&true) {
                drop(refresh_tokens);
                return invalid_grant();
            }
            refresh_tokens.insert(refresh.clone(), false);
            drop(refresh_tokens);
            let resource = form.get("resource").cloned().unwrap_or_default();
            Json(gateway.mint(&resource)).into_response()
        }
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

fn invalid_grant() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": "invalid_grant"})),
    )
        .into_response()
}

fn invalid_target() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "error": "invalid_target",
            "error_description": "The requested attestation context is unavailable.",
        })),
    )
        .into_response()
}

async fn revoke(
    State(gateway): State<Arc<FakeGateway>>,
    Form(form): Form<HashMap<String, String>>,
) -> StatusCode {
    if let Some(token) = form.get("token") {
        gateway
            .refresh_tokens
            .lock()
            .unwrap()
            .insert(token.clone(), false);
        gateway.revoked.lock().unwrap().push(token.clone());
    }
    StatusCode::OK
}

fn bearer_resource(gateway: &FakeGateway, headers: &HeaderMap) -> Option<String> {
    let token = headers
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    gateway.access_tokens.lock().unwrap().get(token).cloned()
}

async fn me(State(gateway): State<Arc<FakeGateway>>, headers: HeaderMap) -> Response {
    if bearer_resource(&gateway, &headers).as_deref() != Some("control") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({
        "user_id": USER,
        "email": "abaas@example.test",
        "display_name": "Abaas",
        "client_name": "tidebreak",
        "session_id": SESSION,
        "installation_id": INSTALLATION,
    }))
    .into_response()
}

async fn models(State(gateway): State<Arc<FakeGateway>>, headers: HeaderMap) -> Response {
    if bearer_resource(&gateway, &headers).as_deref() != Some("control") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({
        "models": [
            {
                "id": "anthropic/claude-fable-5",
                "name": "Claude Fable 5",
                "context_window": 500000,
                "max_output_tokens": 64000,
                "supports_tools": true,
                "supports_vision": true,
            },
            {
                "id": "openai/gpt-fable-5",
                "protocol": "openai_chat_completions",
                "name": "GPT Fable 5",
                "context_window": 256000,
                "max_output_tokens": 32000,
                "supports_tools": true,
                "supports_vision": true,
            }
        ]
    }))
    .into_response()
}

async fn apps(State(gateway): State<Arc<FakeGateway>>, headers: HeaderMap) -> Response {
    if gateway.apps_unsupported.load(Ordering::SeqCst) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if bearer_resource(&gateway, &headers).as_deref() != Some("control") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({
        "apps": [{
            "id": "app-incident",
            "name": "Incident API",
            "app_kind": "rest_api",
            "enabled": true,
            "mcp_endpoint_slugs": ["example-security-tools"],
        }]
    }))
    .into_response()
}

const CATALOG_ETAG: &str = "\"catalog-rev-1\"";

async fn member_catalog(State(gateway): State<Arc<FakeGateway>>, headers: HeaderMap) -> Response {
    if gateway.member_catalog_unsupported.load(Ordering::SeqCst) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if bearer_resource(&gateway, &headers).as_deref() != Some("control") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if headers
        .get(axum::http::header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        == Some(CATALOG_ETAG)
    {
        return (
            StatusCode::NOT_MODIFIED,
            [(axum::http::header::ETAG, CATALOG_ETAG)],
        )
            .into_response();
    }
    (
        [(axum::http::header::ETAG, CATALOG_ETAG)],
        Json(json!({
            "models": [{
                "id": "claude-opus-5",
                "name": "Claude Opus 5",
                "protocols": ["anthropic_messages", "openai_responses"],
                "supports_tools": true,
                "supports_vision": true,
                "context_window": 200000,
                "max_output_tokens": 64000,
                "provider_name": "Anthropic",
            }],
            "apps": [{
                "id": "app-incident",
                "name": "Incident API",
                "app_kind": "rest_api",
                "enabled": true,
                "mcp_endpoint_slugs": ["example-security-tools"],
                "connection": "authorization_required",
            }],
        })),
    )
        .into_response()
}

async fn app_operations(
    State(gateway): State<Arc<FakeGateway>>,
    axum::extract::Path(app_id): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Response {
    if gateway.catalogs_unsupported.load(Ordering::SeqCst) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if bearer_resource(&gateway, &headers).as_deref() != Some("control") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if app_id != "app-incident" {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(json!({
        "operations": [
            { "operation_id": "listIncidents", "method": "GET", "summary": "List incidents" },
            // No summary, and a field this client does not know: the catalog
            // read is additive at the gateway and must stay parseable here.
            { "operation_id": "createIncident", "method": "POST", "document_sha256": "abc" },
        ]
    }))
    .into_response()
}

/// The shared-app invoke route: the SPA-tier data plane, reached here on a
/// harness's `control`-audience PKCE session.
async fn shared_app_invoke(
    State(gateway): State<Arc<FakeGateway>>,
    axum::extract::Path(shared_app_id): axum::extract::Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if gateway.shared_apps_unsupported.load(Ordering::SeqCst) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let resource = bearer_resource(&gateway, &headers);
    gateway
        .shared_app_invokes
        .lock()
        .unwrap()
        .push((shared_app_id, resource.clone(), body));
    if resource.as_deref() != Some("control") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if gateway
        .shared_app_needs_authorization
        .load(Ordering::SeqCst)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": {
                    "code": "authorization_required",
                    "message": "Connect Incident API to continue",
                }
            })),
        )
            .into_response();
    }
    Json(json!({
        "operation_id": "listIncidents",
        "status": 200,
        "content_type": "application/json",
        "headers": { "x-request-id": "abc" },
        "body": { "incidents": [] },
    }))
    .into_response()
}

async fn serve_fake_gateway(gateway: Arc<FakeGateway>) -> SocketAddr {
    let app = Router::new()
        .route("/api/v1/meta", get(meta))
        .route("/oauth/authorize", get(authorize))
        .route("/oauth/token", post(token))
        .route("/oauth/revoke", post(revoke))
        .route("/api/v1/cli/me", get(me))
        .route("/api/v1/cli/models", get(models))
        .route("/api/v1/cli/apps", get(apps))
        .route("/api/v1/me/catalog", get(member_catalog))
        .route("/api/v1/cli/apps/{app_id}/operations", get(app_operations))
        .route(
            "/api/apps/shared/{shared_app_id}/invoke",
            post(shared_app_invoke),
        )
        .with_state(gateway);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    address
}

fn browser() -> reqwest::Client {
    // Unlike the auth client, the "browser" follows the authorize redirect
    // out to the loopback listener.
    reqwest::Client::builder().build().unwrap()
}

async fn signed_in_connection() -> (Arc<FakeGateway>, GatewayConnection) {
    let gateway = Arc::new(FakeGateway::default());
    let address = serve_fake_gateway(gateway.clone()).await;
    let auth =
        GatewayAuth::new(GatewayAuthConfig::new(&format!("http://{address}")).unwrap()).unwrap();

    let pending = auth.start_sign_in().await.unwrap();
    let url = pending.authorization_url().to_string();
    let finish = tokio::spawn(pending.finish(Duration::from_secs(5)));
    browser().get(&url).send().await.unwrap();
    let session = finish.await.unwrap().unwrap();

    let connection =
        GatewayConnection::new(auth, CredentialVault::new(Arc::new(MockSecrets::default())));
    connection.store_session(&session).await.unwrap();
    (gateway, connection)
}

#[tokio::test]
async fn full_sign_in_flow_verifies_pkce_installation_and_identity() {
    let gateway = Arc::new(FakeGateway::default());
    let address = serve_fake_gateway(gateway.clone()).await;
    let auth =
        GatewayAuth::new(GatewayAuthConfig::new(&format!("http://{address}")).unwrap()).unwrap();

    let pending = auth.start_sign_in().await.unwrap();
    assert_eq!(pending.meta().installation_id, INSTALLATION);
    let url = pending.authorization_url().to_string();
    assert!(url.contains("client_id=tidebreak"));
    assert!(url.contains("code_challenge_method=S256"));
    assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A"));

    let finish = tokio::spawn(pending.finish(Duration::from_secs(5)));
    let page = browser().get(&url).send().await.unwrap();
    assert!(page.status().is_success());
    let session = finish.await.unwrap().unwrap();

    assert_eq!(session.tokens.resource, RESOURCE_CONTROL);
    assert_eq!(session.tokens.installation_id, INSTALLATION);
    assert_eq!(session.identity.user_id, USER);
    assert_eq!(
        session.identity.email.as_deref(),
        Some("abaas@example.test")
    );
    // Exactly one token request: the code exchange.
    assert_eq!(gateway.token_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn access_tokens_cache_per_resource_and_rotate_on_refresh() {
    let (gateway, connection) = signed_in_connection().await;
    let exchanges = gateway.token_requests.load(Ordering::SeqCst);

    // The control token stored at sign-in is still fresh: no new request.
    let control = connection.access_token(RESOURCE_CONTROL).await.unwrap();
    assert_eq!(gateway.token_requests.load(Ordering::SeqCst), exchanges);

    // A new resource forces one rotating refresh; the result is then cached.
    let llm = connection.access_token(RESOURCE_LLM).await.unwrap();
    assert_ne!(control, llm);
    assert_eq!(gateway.token_requests.load(Ordering::SeqCst), exchanges + 1);
    assert_eq!(connection.access_token(RESOURCE_LLM).await.unwrap(), llm);
    assert_eq!(gateway.token_requests.load(Ordering::SeqCst), exchanges + 1);

    let models = connection.models(None).await.unwrap();
    assert_eq!(models[0].id, "anthropic/claude-fable-5");
    assert_eq!(models[0].protocol, "anthropic_messages");
    assert!(models[0].supports_tools);
    assert_eq!(models[1].protocol, "openai_chat_completions");
    let identity = connection.identity().await.unwrap();
    assert_eq!(identity.user_id, USER);
}

#[tokio::test]
async fn a_stale_refresh_token_reads_as_signed_out() {
    let (_gateway, connection) = signed_in_connection().await;
    let stale = connection.stored_credentials().await.unwrap().unwrap();

    // Rotate once so the stored refresh token in `stale` is superseded…
    connection.access_token(RESOURCE_LLM).await.unwrap();
    // …then simulate a vault that missed the rotation.
    let vault = CredentialVault::new(Arc::new(MockSecrets::default()));
    vault.save(&stale).await.unwrap();
    let auth = GatewayAuth::new(GatewayAuthConfig::new(&stale.base_url).unwrap()).unwrap();
    let stale_connection = GatewayConnection::new(auth, vault);

    let error = stale_connection
        .access_token(RESOURCE_LLM)
        .await
        .expect_err("superseded refresh token must fail");
    assert!(is_sign_in_required(&error), "{error}");

    // The refusal retires the stored session: the status surface must read
    // signed-out and offer reconnect, not report a session whose every call
    // fails.
    assert!(stale_connection
        .stored_credentials()
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn a_session_for_a_different_deployment_reads_as_signed_out() {
    let (_gateway, connection) = signed_in_connection().await;
    let stored = connection.stored_credentials().await.unwrap().unwrap();

    // The same vault contents viewed by a connection configured for a
    // different deployment — the settings URL was edited after sign-in.
    let vault = CredentialVault::new(Arc::new(MockSecrets::default()));
    vault.save(&stored).await.unwrap();
    let other = GatewayConnection::new(
        GatewayAuth::new(GatewayAuthConfig::new("http://127.0.0.1:9").unwrap()).unwrap(),
        vault,
    );

    assert!(other.stored_credentials().await.unwrap().is_none());
    // Refused before any refresh attempt: the failure must read as
    // "sign-in required", not as a provider error downstream.
    let error = other
        .access_token(RESOURCE_CONTROL)
        .await
        .expect_err("a mismatched session must not mint tokens");
    assert!(is_sign_in_required(&error), "{error}");
}

#[tokio::test]
async fn a_state_mismatch_never_reaches_the_token_endpoint() {
    let gateway = Arc::new(FakeGateway::default());
    gateway.corrupt_state.store(true, Ordering::SeqCst);
    let address = serve_fake_gateway(gateway.clone()).await;
    let auth =
        GatewayAuth::new(GatewayAuthConfig::new(&format!("http://{address}")).unwrap()).unwrap();

    let pending = auth.start_sign_in().await.unwrap();
    let url = pending.authorization_url().to_string();
    let finish = tokio::spawn(pending.finish(Duration::from_secs(2)));
    let page = browser().get(&url).send().await.unwrap();
    assert_eq!(page.status(), reqwest::StatusCode::BAD_REQUEST);

    let error = finish
        .await
        .unwrap()
        .expect_err("state mismatch must fail the sign-in");
    assert!(error.to_string().contains("timed out"), "{error}");
    assert_eq!(gateway.token_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn sign_out_revokes_remotely_and_clears_the_vault() {
    let (gateway, connection) = signed_in_connection().await;
    connection.sign_out().await.unwrap();

    assert_eq!(gateway.revoked.lock().unwrap().len(), 1);
    assert!(connection.stored_credentials().await.unwrap().is_none());
    let error = connection
        .access_token(RESOURCE_CONTROL)
        .await
        .expect_err("signed-out connection must fail");
    assert!(is_sign_in_required(&error), "{error}");
}

#[tokio::test]
async fn attested_tokens_share_a_context_per_key_and_cache_per_resource() {
    let (gateway, connection) = signed_in_connection().await;
    let exchanges = gateway.token_requests.load(Ordering::SeqCst);

    // Two resources under one key ride the same client-minted context —
    // the property that lets an attested endpoint match the tool call
    // against the observation the same chat's inference recorded.
    let llm = connection
        .attested_access_token(RESOURCE_LLM, "chat-a")
        .await
        .unwrap();
    let mcp = connection
        .attested_mcp_access_token("primary", "chat-a")
        .await
        .unwrap();
    assert_ne!(llm, mcp);
    assert_eq!(gateway.token_requests.load(Ordering::SeqCst), exchanges + 2);

    // Same key + resource is served from the in-memory cache.
    assert_eq!(
        connection
            .attested_access_token(RESOURCE_LLM, "chat-a")
            .await
            .unwrap(),
        llm
    );
    assert_eq!(gateway.token_requests.load(Ordering::SeqCst), exchanges + 2);

    // A different key mints a different context.
    connection
        .attested_access_token(RESOURCE_LLM, "chat-b")
        .await
        .unwrap();
    let contexts = gateway.contexts_seen.lock().unwrap().clone();
    assert_eq!(contexts.len(), 3);
    assert_eq!(contexts[0], contexts[1]);
    assert_ne!(contexts[0], contexts[2]);

    // Attested mints never landed in the persisted per-resource cache: a
    // plain llm token still needs its own refresh, and that refresh works —
    // proof the rotated refresh token was persisted by the attested path.
    let count = gateway.token_requests.load(Ordering::SeqCst);
    let plain = connection.access_token(RESOURCE_LLM).await.unwrap();
    assert_ne!(plain, llm);
    assert_eq!(gateway.token_requests.load(Ordering::SeqCst), count + 1);
}

#[tokio::test]
async fn a_rejected_attestation_context_remints_under_a_fresh_id() {
    let (gateway, connection) = signed_in_connection().await;
    gateway.reject_next_context.store(true, Ordering::SeqCst);

    // The first mint's context is refused (as a context pinned to a
    // superseded session would be); the connection remints under a fresh
    // id without surfacing an error or consuming the refresh token.
    connection
        .attested_access_token(RESOURCE_LLM, "chat-a")
        .await
        .unwrap();
    assert_eq!(gateway.contexts_seen.lock().unwrap().len(), 1);

    // The session survived the rejected grant.
    connection.access_token(RESOURCE_CONTROL).await.unwrap();
}

#[tokio::test]
async fn attested_state_resets_with_the_stored_session() {
    let (gateway, connection) = signed_in_connection().await;
    connection
        .attested_access_token(RESOURCE_LLM, "chat-a")
        .await
        .unwrap();

    // Sign in again: same user, same gateway, new session. The same chat
    // must not reuse the old context id — the gateway pins contexts to the
    // session they were first used with.
    let pending = connection.auth().start_sign_in().await.unwrap();
    let url = pending.authorization_url().to_string();
    let finish = tokio::spawn(pending.finish(Duration::from_secs(5)));
    browser().get(&url).send().await.unwrap();
    let session = finish.await.unwrap().unwrap();
    connection.store_session(&session).await.unwrap();

    connection
        .attested_access_token(RESOURCE_LLM, "chat-a")
        .await
        .unwrap();
    let contexts = gateway.contexts_seen.lock().unwrap().clone();
    assert_eq!(contexts.len(), 2);
    assert_ne!(contexts[0], contexts[1]);
}

#[tokio::test]
async fn apps_lists_entitlements_and_degrades_when_the_surface_is_missing() {
    let (gateway, connection) = signed_in_connection().await;

    let apps = connection.apps().await.unwrap().unwrap();
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].name, "Incident API");
    assert_eq!(apps[0].app_kind, "rest_api");
    assert!(apps[0].enabled);
    assert_eq!(apps[0].mcp_endpoint_slugs, ["example-security-tools"]);

    // An older gateway without the JSON apps surface is "unsupported", not an
    // error and not an empty entitlement list.
    gateway.apps_unsupported.store(true, Ordering::SeqCst);
    assert_eq!(connection.apps().await.unwrap(), None);
}

/// The member catalog is the envelope both the settings panel and the MCP
/// endpoint mounts read entitlements from, so its wire shape — one fetch
/// carrying models, apps, per-app readiness, and an `ETag` — is a contract.
/// It has to degrade to `Unsupported` against a gateway that predates it,
/// which is what sends callers back to the per-surface reads.
#[tokio::test]
async fn member_catalog_reads_one_envelope_and_degrades_when_missing() {
    let (gateway, connection) = signed_in_connection().await;

    let GatewayCatalogFetch::Fresh { catalog, etag } = connection.catalog(None).await.unwrap()
    else {
        panic!("expected a fresh catalog");
    };
    assert_eq!(catalog.models.len(), 1);
    assert_eq!(
        catalog.models[0].protocols,
        ["anthropic_messages", "openai_responses"]
    );
    assert_eq!(catalog.apps.len(), 1);
    assert_eq!(
        catalog.apps[0].mcp_endpoint_slugs,
        ["example-security-tools"]
    );
    assert_eq!(catalog.apps[0].connection, "authorization_required");

    // A conditional refetch with the held revision keeps the snapshot.
    let etag = etag.expect("catalog carried an ETag");
    assert_eq!(
        connection.catalog(Some(&etag)).await.unwrap(),
        GatewayCatalogFetch::NotModified
    );

    // A gateway predating the route reads as unsupported, not an error —
    // that answer is what degrades callers to the per-surface reads.
    gateway
        .member_catalog_unsupported
        .store(true, Ordering::SeqCst);
    assert_eq!(
        connection.catalog(None).await.unwrap(),
        GatewayCatalogFetch::Unsupported
    );
}

/// The per-app catalog read is what a gateway binding's fingerprint is taken
/// over, so it has to answer with the ids a manifest pins — under a `control`
/// bearer — and it has to degrade rather than fault against a deployment that
/// does not serve it yet, since the gateway half is still shipping.
#[tokio::test]
async fn app_catalogs_read_declared_operations_and_degrade_when_the_surface_is_missing() {
    let (gateway, connection) = signed_in_connection().await;

    let operations = connection
        .app_operations("app-incident")
        .await
        .unwrap()
        .unwrap();
    let ids: Vec<&str> = operations
        .iter()
        .map(|operation| operation.operation_id.as_str())
        .collect();
    assert_eq!(ids, ["listIncidents", "createIncident"]);
    assert_eq!(operations[0].method, "GET");
    assert_eq!(operations[0].summary.as_deref(), Some("List incidents"));
    assert_eq!(operations[1].summary, None);

    // An id nothing is entitled to reads as "no catalog" rather than an
    // error, so a grant naming it fails closed to re-consent.
    assert_eq!(
        connection.app_operations("app-unknown").await.unwrap(),
        None
    );
    // An id outside the binding grammar never reaches the wire at all, and a
    // traversal-shaped one is a single escaped path segment — it can only
    // ever address a missing app, never another CLI route.
    assert!(connection.app_operations("").await.is_err());
    assert_eq!(connection.app_operations("../models").await.unwrap(), None);

    // A gateway predating the catalog read degrades the same way.
    gateway.catalogs_unsupported.store(true, Ordering::SeqCst);
    assert_eq!(
        connection.app_operations("app-incident").await.unwrap(),
        None
    );
}

/// The shared-app invoke relay: the data-plane half of a gateway app binding.
/// It has to reach the route with the ordinary `control` session bearer and
/// the gateway's own five-field body, surface the typed
/// `authorization_required` as its own outcome rather than as prose, and read
/// a deployment that does not serve the route as unavailable rather than a
/// fault — the gateway half is still shipping.
#[tokio::test]
async fn shared_app_invokes_relay_on_the_control_session_and_degrade_when_unserved() {
    let (gateway, connection) = signed_in_connection().await;
    // The production relay omits absent argument halves entirely (the
    // gateway's serde defaults refuse an explicit null); the fixture body
    // mirrors the real wire shape.
    let request = json!({
        "connected_app_id": "app-incident",
        "operation_id": "listIncidents",
        "query": { "state": "open" },
    });

    let outcome = connection
        .invoke_shared_app("shared-1", &request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        outcome,
        GatewayInvokeOutcome::Executed {
            status: 200,
            content_type: Some("application/json".into()),
            body_base64: base64::engine::general_purpose::STANDARD.encode(br#"{"incidents":[]}"#),
        }
    );
    let seen = gateway.shared_app_invokes.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "shared-1");
    assert_eq!(
        seen[0].1.as_deref(),
        Some("control"),
        "the relay carries the ordinary control session bearer, never an \
         attested or endpoint-scoped one"
    );
    assert_eq!(seen[0].2, request);

    // The one failure the frame must branch on keeps its own outcome, with
    // the gateway's own message.
    gateway
        .shared_app_needs_authorization
        .store(true, Ordering::SeqCst);
    assert_eq!(
        connection
            .invoke_shared_app("shared-1", &request)
            .await
            .unwrap()
            .unwrap(),
        GatewayInvokeOutcome::AuthorizationRequired {
            message: "Connect Incident API to continue".into(),
        }
    );

    // A deployment that does not serve the route reads as "nothing answers
    // this pin", the same degradation the entitlement and catalog reads use.
    gateway
        .shared_apps_unsupported
        .store(true, Ordering::SeqCst);
    assert_eq!(
        connection
            .invoke_shared_app("shared-1", &request)
            .await
            .unwrap(),
        None
    );
    // An id outside the binding grammar never reaches the wire at all.
    assert!(connection.invoke_shared_app("", &request).await.is_err());
}
