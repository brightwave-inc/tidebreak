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
use openwave_connectors::{
    is_sign_in_required, CredentialVault, GatewayAuth, GatewayAuthConfig, GatewayConnection,
    RESOURCE_CONTROL, RESOURCE_LLM,
};
use openwave_core::{Result as CoreResult, SecretProvider};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

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
    if query.get("client_id").map(String::as_str) != Some("modelctl")
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
                || form.get("client_id").map(String::as_str) != Some("modelctl")
            {
                return invalid_grant();
            }
            Json(gateway.mint("control")).into_response()
        }
        Some("refresh_token") => {
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
        "client_name": "modelctl",
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
        "models": [{
            "id": "anthropic/claude-fable-5",
            "name": "Claude Fable 5",
            "context_window": 500000,
            "max_output_tokens": 64000,
            "supports_tools": true,
            "supports_vision": true,
        }]
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
        GatewayAuth::new(GatewayAuthConfig::modelctl_compat(&format!("http://{address}")).unwrap())
            .unwrap();

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
        GatewayAuth::new(GatewayAuthConfig::modelctl_compat(&format!("http://{address}")).unwrap())
            .unwrap();

    let pending = auth.start_sign_in().await.unwrap();
    assert_eq!(pending.meta().installation_id, INSTALLATION);
    let url = pending.authorization_url().to_string();
    assert!(url.contains("client_id=modelctl"));
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
    assert!(models[0].supports_tools);
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
    let auth =
        GatewayAuth::new(GatewayAuthConfig::modelctl_compat(&stale.base_url).unwrap()).unwrap();
    let stale_connection = GatewayConnection::new(auth, vault);

    let error = stale_connection
        .access_token(RESOURCE_LLM)
        .await
        .expect_err("superseded refresh token must fail");
    assert!(is_sign_in_required(&error), "{error}");
}

#[tokio::test]
async fn a_state_mismatch_never_reaches_the_token_endpoint() {
    let gateway = Arc::new(FakeGateway::default());
    gateway.corrupt_state.store(true, Ordering::SeqCst);
    let address = serve_fake_gateway(gateway.clone()).await;
    let auth =
        GatewayAuth::new(GatewayAuthConfig::modelctl_compat(&format!("http://{address}")).unwrap())
            .unwrap();

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
