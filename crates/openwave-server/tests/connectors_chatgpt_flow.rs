//! End-to-end ChatGPT OAuth client against an in-process fake that checks
//! PKCE S256, the Codex public client id, and the fixed loopback redirect.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Form, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use openwave_core::{Result as CoreResult, SecretProvider};
use openwave_server::connectors::{
    has_stored_chatgpt_credentials, ChatGptAuth, ChatGptAuthConfig, ChatGptConnection,
    ChatGptCredentialVault, CHATGPT_SECRET_KEY, CLIENT_ID, REDIRECT_URI,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const ACCOUNT: &str = "313b2c23-7977-4b2c-bf87-0ffb1c3d4217";

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
struct FakeAuth {
    codes: Mutex<HashMap<String, PendingCode>>,
    refresh_tokens: Mutex<HashMap<String, bool>>,
    token_requests: AtomicUsize,
    revoked: Mutex<Vec<String>>,
}

impl FakeAuth {
    fn mint_access_jwt(&self) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(
            json!({
                "https://api.openai.com/auth": { "chatgpt_account_id": ACCOUNT }
            })
            .to_string()
            .as_bytes(),
        );
        format!("{header}.{payload}.sig")
    }

    fn mint(self: &Arc<Self>) -> Value {
        let access = self.mint_access_jwt();
        let refresh = format!("rt_{}", next_id());
        self.refresh_tokens
            .lock()
            .unwrap()
            .insert(refresh.clone(), true);
        json!({
            "access_token": access,
            "token_type": "Bearer",
            "expires_in": 600,
            "refresh_token": refresh,
            "scope": "openid profile email offline_access",
        })
    }

    fn sparse_refresh(&self) -> Value {
        json!({
            "access_token": format!("opaque-access-{}", next_id()),
            "token_type": "Bearer",
            "expires_in": 600,
            "scope": "openid profile email offline_access",
        })
    }
}

fn next_id() -> u64 {
    use std::sync::atomic::AtomicU64;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::SeqCst)
}

async fn authorize(
    State(auth): State<Arc<FakeAuth>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if query.get("client_id").map(String::as_str) != Some(CLIENT_ID)
        || query.get("code_challenge_method").map(String::as_str) != Some("S256")
        || query.get("response_type").map(String::as_str) != Some("code")
        || query.get("redirect_uri").map(String::as_str) != Some(REDIRECT_URI)
        || query.get("originator").map(String::as_str) != Some("openwave")
        || query.get("codex_cli_simplified_flow").map(String::as_str) != Some("true")
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let code = format!("code_{}", next_id());
    let redirect_uri = query.get("redirect_uri").cloned().unwrap_or_default();
    auth.codes.lock().unwrap().insert(
        code.clone(),
        PendingCode {
            challenge: query.get("code_challenge").cloned().unwrap_or_default(),
            redirect_uri: redirect_uri.clone(),
        },
    );
    let state = query.get("state").cloned().unwrap_or_default();
    // Rewrite localhost → 127.0.0.1 so the test "browser" hits the IPv4
    // listener even when `localhost` prefers ::1 on this machine.
    let callback = redirect_uri.replacen("://localhost:", "://127.0.0.1:", 1);
    Redirect::to(&format!("{callback}?code={code}&state={state}")).into_response()
}

async fn token(
    State(auth): State<Arc<FakeAuth>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    auth.token_requests.fetch_add(1, Ordering::SeqCst);
    match form.get("grant_type").map(String::as_str) {
        Some("authorization_code") => {
            let Some(code) = form.get("code") else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            let Some(pending) = auth.codes.lock().unwrap().remove(code) else {
                return invalid_grant();
            };
            let verifier = form.get("code_verifier").cloned().unwrap_or_default();
            let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
            if challenge != pending.challenge
                || form.get("redirect_uri") != Some(&pending.redirect_uri)
                || form.get("client_id").map(String::as_str) != Some(CLIENT_ID)
            {
                return invalid_grant();
            }
            Json(auth.mint()).into_response()
        }
        Some("refresh_token") => {
            let Some(refresh) = form.get("refresh_token") else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            if form.get("client_id").map(String::as_str) != Some(CLIENT_ID) {
                return invalid_grant();
            }
            if auth.refresh_tokens.lock().unwrap().get(refresh) != Some(&true) {
                return invalid_grant();
            }
            // OAuth refresh responses may keep the existing refresh token and
            // omit identity fields that were already established at sign-in.
            Json(auth.sparse_refresh()).into_response()
        }
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn revoke(
    State(auth): State<Arc<FakeAuth>>,
    Form(form): Form<HashMap<String, String>>,
) -> StatusCode {
    if let Some(token) = form.get("token") {
        auth.revoked.lock().unwrap().push(token.clone());
        auth.refresh_tokens
            .lock()
            .unwrap()
            .insert(token.clone(), false);
    }
    StatusCode::OK
}

fn invalid_grant() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": "invalid_grant", "error_description": "bad grant"})),
    )
        .into_response()
}

async fn spawn_fake() -> (String, Arc<FakeAuth>) {
    let fake = Arc::new(FakeAuth::default());
    let app = Router::new()
        .route("/oauth/authorize", get(authorize))
        .route("/oauth/token", post(token))
        .route("/oauth/revoke", post(revoke))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), fake)
}

fn browser() -> reqwest::Client {
    // Follows the authorize redirect out to the loopback listener.
    reqwest::Client::builder().build().unwrap()
}

async fn expire_stored_access_token(secrets: &dyn SecretProvider) -> Value {
    let raw = secrets
        .get_secret(CHATGPT_SECRET_KEY)
        .await
        .unwrap()
        .unwrap();
    let mut value: Value = serde_json::from_str(&raw).unwrap();
    value["expires_at_unix"] = json!(0);
    secrets
        .set_secret(CHATGPT_SECRET_KEY, &serde_json::to_string(&value).unwrap())
        .await
        .unwrap();
    value
}

#[tokio::test]
async fn sign_in_refresh_and_sign_out_round_trip() {
    let (base, fake) = spawn_fake().await;
    let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
    let auth = ChatGptAuth::new(ChatGptAuthConfig::for_test(&base).unwrap()).unwrap();
    let connection = ChatGptConnection::new(auth, ChatGptCredentialVault::new(secrets.clone()));

    assert!(!has_stored_chatgpt_credentials(secrets.as_ref()).await);

    let pending = connection.auth().start_sign_in().await.unwrap();
    let url = pending.authorization_url().to_string();
    let finish = tokio::spawn(pending.finish(Duration::from_secs(5)));
    browser().get(&url).send().await.unwrap();
    let session = finish.await.unwrap().unwrap();
    assert_eq!(session.credentials.account_id(), ACCOUNT);
    connection.store_session(&session).await.unwrap();
    assert!(has_stored_chatgpt_credentials(secrets.as_ref()).await);

    let token = connection.access_token().await.unwrap();
    assert!(!token.is_empty());
    assert_eq!(fake.token_requests.load(Ordering::SeqCst), 1);

    // Expire the cached access token so the next read refreshes. The fake's
    // refresh response deliberately omits account_id, id_token, and
    // refresh_token; the connection must retain those durable fields.
    let before_refresh = expire_stored_access_token(secrets.as_ref()).await;
    let original_refresh = before_refresh["refresh_token"]
        .as_str()
        .unwrap()
        .to_string();

    let token_after = connection.access_token().await.unwrap();
    assert!(token_after.starts_with("opaque-access-"));
    assert_eq!(fake.token_requests.load(Ordering::SeqCst), 2);

    let persisted: Value = serde_json::from_str(
        &secrets
            .get_secret(CHATGPT_SECRET_KEY)
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(persisted["account_id"], ACCOUNT);
    assert_eq!(persisted["refresh_token"], original_refresh);

    // The retained token must remain usable for another refresh, not merely
    // survive serialization once.
    expire_stored_access_token(secrets.as_ref()).await;
    let token_again = connection.access_token().await.unwrap();
    assert!(token_again.starts_with("opaque-access-"));
    assert_eq!(fake.token_requests.load(Ordering::SeqCst), 3);

    connection.sign_out().await.unwrap();
    assert!(!has_stored_chatgpt_credentials(secrets.as_ref()).await);
    let revoked = fake.revoked.lock().unwrap();
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0], original_refresh);
}
