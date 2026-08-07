//! Loopback OAuth client for ChatGPT subscription sign-in.
//!
//! Speaks the same public Codex-compatible contract other local tools use:
//! PKCE S256 against `auth.openai.com`, fixed loopback redirect on port
//! 1455, and a refresh-rotating access token whose bearer is presented to
//! the ChatGPT Codex Responses backend together with the account id.
//!
//! This is not a registered OpenWave client — OpenAI does not offer a
//! public registration path for subscription inference — so the client id
//! is the well-known Codex public client. The product surface is ours
//! (`originator=openwave`); the OAuth identity is shared.

use std::future::IntoFuture;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use openwave_core::{AgentError, Result, SecretProvider};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};

/// SecretProvider key for the durable ChatGPT OAuth session.
pub const SECRET_KEY: &str = "provider.openai.chatgpt_oauth_v1";

/// Codex public OAuth client id. Third-party local tools reuse this; there is
/// no self-serve registration for a distinct OpenWave client today.
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Fixed redirect the public Codex client allows. Binding anything else fails
/// at the authorize step.
pub const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";

/// Loopback port carved out by [`REDIRECT_URI`].
pub const CALLBACK_PORT: u16 = 1455;

/// Product originator sent on authorize and on Codex inference.
pub const ORIGINATOR: &str = "openwave";

/// ChatGPT / Codex Responses API root (no trailing `/v1`; callers append
/// `/responses`).
pub const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

const DEFAULT_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const DEFAULT_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const DEFAULT_REVOKE_URL: &str = "https://auth.openai.com/oauth/revoke";
const SCOPE: &str = "openid profile email offline_access";
const EXPIRY_LEEWAY_SECONDS: u64 = 60;
const SIGN_IN_REQUIRED_PREFIX: &str = "chatgpt sign-in required";

/// True when an operation failed because there is no usable ChatGPT session.
#[must_use]
pub fn is_chatgpt_sign_in_required(error: &AgentError) -> bool {
    matches!(error, AgentError::Authentication(message) if message.starts_with(SIGN_IN_REQUIRED_PREFIX))
}

fn sign_in_required(detail: &str) -> AgentError {
    AgentError::Authentication(format!("{SIGN_IN_REQUIRED_PREFIX}: {detail}"))
}

fn chatgpt_error(context: &str, detail: impl std::fmt::Display) -> AgentError {
    AgentError::config(format!("chatgpt oauth {context}: {detail}"))
}

/// How this client identifies itself to OpenAI's authorization server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatGptAuthConfig {
    authorize_url: reqwest::Url,
    token_url: reqwest::Url,
    revoke_url: reqwest::Url,
    client_id: String,
    redirect_uri: String,
    originator: String,
}

impl ChatGptAuthConfig {
    /// Production endpoints and the Codex public client.
    pub fn production() -> Self {
        Self {
            authorize_url: DEFAULT_AUTHORIZE_URL.parse().expect("static authorize URL"),
            token_url: DEFAULT_TOKEN_URL.parse().expect("static token URL"),
            revoke_url: DEFAULT_REVOKE_URL.parse().expect("static revoke URL"),
            client_id: CLIENT_ID.to_string(),
            redirect_uri: REDIRECT_URI.to_string(),
            originator: ORIGINATOR.to_string(),
        }
    }

    /// Point authorize/token/revoke at a fake origin for tests. Redirect stays
    /// on the Codex-registered loopback URI so the callback contract matches
    /// production.
    pub fn for_test(auth_base: &str) -> Result<Self> {
        let base = reqwest::Url::parse(auth_base)
            .map_err(|_| chatgpt_error("configuration", "auth base URL is invalid"))?;
        let join = |path: &str| -> Result<reqwest::Url> {
            let mut root = base.to_string();
            if !root.ends_with('/') {
                root.push('/');
            }
            reqwest::Url::parse(&root)
                .and_then(|url| url.join(path.trim_start_matches('/')))
                .map_err(|_| chatgpt_error("configuration", "could not build auth endpoint"))
        };
        Ok(Self {
            authorize_url: join("/oauth/authorize")?,
            token_url: join("/oauth/token")?,
            revoke_url: join("/oauth/revoke")?,
            client_id: CLIENT_ID.to_string(),
            redirect_uri: REDIRECT_URI.to_string(),
            originator: ORIGINATOR.to_string(),
        })
    }

    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    #[must_use]
    pub fn originator(&self) -> &str {
        &self.originator
    }
}

/// Durable ChatGPT OAuth session stored in the keychain.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatGptCredentials {
    access_token: String,
    refresh_token: String,
    /// ChatGPT account id required as `ChatGPT-Account-ID` on inference.
    pub account_id: String,
    expires_at_unix: u64,
}

impl ChatGptCredentials {
    #[must_use]
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    fn access_is_fresh(&self) -> bool {
        self.expires_at_unix > unix_time().saturating_add(EXPIRY_LEEWAY_SECONDS)
    }
}

impl std::fmt::Debug for ChatGptCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatGptCredentials")
            .field("account_id", &self.account_id)
            .field("expires_at_unix", &self.expires_at_unix)
            .finish_non_exhaustive()
    }
}

/// Whether a ChatGPT OAuth session is stored.
pub async fn has_stored_chatgpt_credentials(secrets: &dyn SecretProvider) -> bool {
    matches!(
        secrets.get_secret(SECRET_KEY).await,
        Ok(Some(raw)) if serde_json::from_str::<ChatGptCredentials>(&raw).is_ok()
    )
}

/// Keychain-backed storage for [`ChatGptCredentials`].
#[derive(Clone)]
pub struct ChatGptCredentialVault {
    secrets: Arc<dyn SecretProvider>,
}

impl ChatGptCredentialVault {
    #[must_use]
    pub fn new(secrets: Arc<dyn SecretProvider>) -> Self {
        Self { secrets }
    }

    pub async fn load(&self) -> Result<Option<ChatGptCredentials>> {
        let Some(raw) = self.secrets.get_secret(SECRET_KEY).await? else {
            return Ok(None);
        };
        serde_json::from_str(&raw)
            .map(Some)
            .map_err(|_| chatgpt_error("credentials", "stored credentials are unreadable"))
    }

    pub async fn save(&self, credentials: &ChatGptCredentials) -> Result<()> {
        let raw = serde_json::to_string(credentials)?;
        self.secrets.set_secret(SECRET_KEY, &raw).await
    }

    pub async fn clear(&self) -> Result<()> {
        self.secrets.delete_secret(SECRET_KEY).await
    }
}

/// Stateless HTTP client for ChatGPT OAuth endpoints.
#[derive(Clone)]
pub struct ChatGptAuth {
    config: ChatGptAuthConfig,
    http: reqwest::Client,
}

impl ChatGptAuth {
    pub fn new(config: ChatGptAuthConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| chatgpt_error("client", error))?;
        Ok(Self { config, http })
    }

    #[must_use]
    pub fn config(&self) -> &ChatGptAuthConfig {
        &self.config
    }

    /// Bind port 1455 and produce the URL the user's browser must visit.
    pub async fn start_sign_in(&self) -> Result<ChatGptPendingSignIn> {
        // The redirect URI the client allows names host `localhost`, so the
        // browser may arrive over either loopback family depending on how it
        // resolves the name. Bind both rather than betting on IPv4.
        let v4 = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, CALLBACK_PORT))
            .await
            .map_err(|error| {
                chatgpt_error(
                    "sign-in",
                    format!(
                        "could not bind localhost:{CALLBACK_PORT} ({error}); \
                         close any Codex login or free the port and try again"
                    ),
                )
            })?;
        // Prefer dual-stack: browsers that resolve `localhost` to `::1` never
        // reach the IPv4 listener. Skip only when the host has no IPv6; a
        // port conflict on `::1` is the same class of failure as on IPv4.
        let v6 = match TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, CALLBACK_PORT)).await {
            Ok(listener) => Some(listener),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::Unsupported
                ) =>
            {
                None
            }
            Err(error) => {
                return Err(chatgpt_error(
                    "sign-in",
                    format!(
                        "could not bind [::1]:{CALLBACK_PORT} ({error}); \
                         close any Codex login or free the port and try again"
                    ),
                ));
            }
        };
        let listeners = LoopbackListeners { v4, v6 };
        let verifier = random_token();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = random_token();

        let mut authorization_url = self.config.authorize_url.clone();
        authorization_url
            .query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.client_id)
            .append_pair("redirect_uri", &self.config.redirect_uri)
            .append_pair("scope", SCOPE)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("id_token_add_organizations", "true")
            .append_pair("codex_cli_simplified_flow", "true")
            .append_pair("originator", &self.config.originator);

        Ok(ChatGptPendingSignIn {
            auth: self.clone(),
            listeners,
            authorization_url: authorization_url.to_string(),
            verifier,
            state,
        })
    }

    /// Exchange a refresh token for a new access/refresh pair.
    pub async fn refresh(&self, refresh_token: &str) -> Result<ChatGptCredentials> {
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", self.config.client_id.as_str()),
        ];
        self.request_token(&form).await
    }

    /// Best-effort revoke. Failures are ignored by callers that are clearing
    /// local state either way.
    pub async fn revoke(&self, refresh_token: &str) -> Result<()> {
        let response = self
            .http
            .post(self.config.revoke_url.clone())
            .form(&[
                ("token", refresh_token),
                ("client_id", self.config.client_id.as_str()),
            ])
            .send()
            .await
            .map_err(|error| chatgpt_error("revocation request", error.without_url()))?;
        if !response.status().is_success() {
            return Err(chatgpt_error(
                "revocation request",
                format!("HTTP status {}", response.status().as_u16()),
            ));
        }
        Ok(())
    }

    async fn exchange_code(&self, code: &str, verifier: &str) -> Result<ChatGptCredentials> {
        let form = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", self.config.redirect_uri.as_str()),
            ("client_id", self.config.client_id.as_str()),
            ("code_verifier", verifier),
        ];
        self.request_token(&form).await
    }

    async fn request_token(&self, form: &[(&str, &str)]) -> Result<ChatGptCredentials> {
        let response = self
            .http
            .post(self.config.token_url.clone())
            .form(form)
            .send()
            .await
            .map_err(|error| chatgpt_error("token request", error.without_url()))?;
        if !response.status().is_success() {
            let status = response.status();
            let error: Option<OAuthErrorResponse> = response.json().await.ok();
            if error
                .as_ref()
                .is_some_and(|error| error.error == "invalid_grant")
            {
                return Err(sign_in_required("the ChatGPT session is no longer valid"));
            }
            let detail = error
                .and_then(|error| error.error_description.or(Some(error.error)))
                .unwrap_or_else(|| format!("HTTP status {}", status.as_u16()));
            return Err(chatgpt_error("token request", detail));
        }
        let token: TokenResponse = response
            .json()
            .await
            .map_err(|_| chatgpt_error("token request", "invalid token response"))?;
        let account_id = token
            .account_id
            .filter(|id| !id.is_empty())
            .or_else(|| extract_account_id(&token.access_token))
            .or_else(|| token.id_token.as_deref().and_then(extract_account_id))
            .ok_or_else(|| {
                chatgpt_error(
                    "token request",
                    "response did not include a ChatGPT account id",
                )
            })?;
        Ok(ChatGptCredentials {
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            account_id,
            expires_at_unix: unix_time()
                .saturating_add(u64::try_from(token.expires_in).unwrap_or(3600)),
        })
    }
}

/// A sign-in in flight. Dropping this cancels the listener.
pub struct ChatGptPendingSignIn {
    auth: ChatGptAuth,
    listeners: LoopbackListeners,
    authorization_url: String,
    verifier: String,
    state: String,
}

/// The loopback callback port, held on both families the browser might use.
struct LoopbackListeners {
    v4: TcpListener,
    v6: Option<TcpListener>,
}

impl LoopbackListeners {
    /// Accept callbacks on every bound address until the caller stops polling.
    async fn serve(self, app: Router) {
        match self.v6 {
            Some(v6) => {
                let v6_app = app.clone();
                tokio::select! {
                    _ = axum::serve(self.v4, app).into_future() => {}
                    _ = axum::serve(v6, v6_app).into_future() => {}
                }
            }
            None => {
                let _ = axum::serve(self.v4, app).into_future().await;
            }
        }
    }
}

/// Completed ChatGPT sign-in ready to persist.
#[derive(Debug, Clone)]
pub struct ChatGptAuthorizedSession {
    pub credentials: ChatGptCredentials,
}

impl ChatGptPendingSignIn {
    #[must_use]
    pub fn authorization_url(&self) -> &str {
        &self.authorization_url
    }

    /// Wait for the browser callback, then exchange the code.
    pub async fn finish(self, timeout: Duration) -> Result<ChatGptAuthorizedSession> {
        let Self {
            auth,
            listeners,
            verifier,
            state,
            ..
        } = self;

        let (sender, mut receiver) = mpsc::channel::<CallbackResult>(1);
        let callback_app = Router::new()
            .route("/auth/callback", get(callback))
            .with_state(CallbackState {
                expected_state: state,
                sender,
            });
        // The callback server runs inside this future rather than a detached
        // task so that cancelling the waiter releases the callback port; a
        // leaked listener would block every later sign-in.
        let outcome = tokio::select! {
            outcome = tokio::time::timeout(timeout, receiver.recv()) => outcome,
            () = listeners.serve(callback_app) => Ok(None),
        };

        let callback = outcome
            .map_err(|_| chatgpt_error("sign-in", "browser authorization timed out"))?
            .ok_or_else(|| chatgpt_error("sign-in", "the authorization callback closed"))?;
        if let Some(error) = callback.error {
            return Err(chatgpt_error(
                "sign-in",
                format!("authorization failed: {error}"),
            ));
        }
        let code = callback
            .code
            .ok_or_else(|| chatgpt_error("sign-in", "no authorization code was returned"))?;

        let credentials = auth.exchange_code(&code, &verifier).await?;
        Ok(ChatGptAuthorizedSession { credentials })
    }
}

/// Vault-backed connection that refreshes access tokens under a lock.
pub struct ChatGptConnection {
    auth: ChatGptAuth,
    vault: ChatGptCredentialVault,
    token_motion: Mutex<()>,
}

impl ChatGptConnection {
    #[must_use]
    pub fn new(auth: ChatGptAuth, vault: ChatGptCredentialVault) -> Self {
        Self {
            auth,
            vault,
            token_motion: Mutex::new(()),
        }
    }

    #[must_use]
    pub fn auth(&self) -> &ChatGptAuth {
        &self.auth
    }

    pub async fn store_session(&self, session: &ChatGptAuthorizedSession) -> Result<()> {
        self.vault.save(&session.credentials).await
    }

    pub async fn stored_credentials(&self) -> Result<Option<ChatGptCredentials>> {
        self.vault.load().await
    }

    /// A currently valid access token, refreshing near expiry.
    pub async fn access_token(&self) -> Result<String> {
        let _guard = self.token_motion.lock().await;
        let Some(mut credentials) = self.vault.load().await? else {
            return Err(sign_in_required("no ChatGPT session is stored"));
        };
        if credentials.access_is_fresh() {
            return Ok(credentials.access_token);
        }
        let refreshed = self.auth.refresh(&credentials.refresh_token).await?;
        // Preserve account id if refresh omits it (should not, but be safe).
        if refreshed.account_id.is_empty() {
            credentials.access_token = refreshed.access_token;
            credentials.refresh_token = refreshed.refresh_token;
            credentials.expires_at_unix = refreshed.expires_at_unix;
            self.vault.save(&credentials).await?;
            return Ok(credentials.access_token);
        }
        self.vault.save(&refreshed).await?;
        Ok(refreshed.access_token)
    }

    /// Account id for the stored session, if any.
    pub async fn account_id(&self) -> Result<Option<String>> {
        Ok(self.vault.load().await?.map(|c| c.account_id))
    }

    /// Revoke at OpenAI (best-effort) and clear local credentials.
    pub async fn sign_out(&self) -> Result<()> {
        let _guard = self.token_motion.lock().await;
        if let Some(credentials) = self.vault.load().await? {
            let _ = self.auth.revoke(&credentials.refresh_token).await;
        }
        self.vault.clear().await
    }
}

/// Pull `chatgpt_account_id` from a JWT payload without verifying the
/// signature — the token is only used as a bearer against OpenAI, and the
/// account id is an opaque routing hint.
fn extract_account_id(jwt: &str) -> Option<String> {
    let payload = jwt.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| STANDARD.decode(payload))
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    value
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(|id| id.as_str())
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            value
                .get("chatgpt_account_id")
                .and_then(|id| id.as_str())
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
        })
}

async fn callback(
    State(state): State<CallbackState>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    if query.state.as_deref() != Some(state.expected_state.as_str()) {
        // Report the mismatch so the waiter fails now instead of holding the
        // callback port until the sign-in timeout expires.
        let _ = state.sender.try_send(CallbackResult {
            code: None,
            error: Some("authorization state did not match".to_owned()),
        });
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Html(callback_page(
                "Sign-in failed",
                "The authorization state did not match. Return to OpenWave and try again.",
            )),
        )
            .into_response();
    }
    let success = query.code.is_some() && query.error.is_none();
    let _ = state.sender.try_send(CallbackResult {
        code: query.code,
        error: query.error,
    });
    let (heading, message) = if success {
        (
            "Signed in",
            "You can close this window and return to OpenWave.",
        )
    } else {
        ("Sign-in denied", "Return to OpenWave for details.")
    };
    (
        axum::http::StatusCode::OK,
        Html(callback_page(heading, message)),
    )
        .into_response()
}

fn callback_page(heading: &str, message: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{heading}</title>
<style>
  :root {{ color-scheme: light dark; --bg:#f4f4f5; --card:#fff; --border:#e4e4e7; --text:#18181b; --muted:#52525b; }}
  @media (prefers-color-scheme: dark) {{
    :root {{ --bg:#18181b; --card:#232326; --border:#3f3f46; --text:#fafafa; --muted:#a1a1aa; }}
  }}
  body {{ margin:0; min-height:100vh; display:grid; place-items:center; background:var(--bg); color:var(--text);
    font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,"Helvetica Neue",Arial,sans-serif; }}
  main {{ background:var(--card); border:1px solid var(--border); border-radius:12px; padding:2.5rem 3rem;
    margin:1rem; max-width:24rem; text-align:center; }}
  .product {{ font-size:0.8125rem; font-weight:600; letter-spacing:0.08em; text-transform:uppercase;
    color:var(--muted); margin:0 0 1rem; }}
  h1 {{ font-size:1.375rem; font-weight:600; margin:0 0 0.5rem; }}
  p {{ color:var(--muted); font-size:0.9375rem; line-height:1.5; margin:0; }}
</style></head><body><main>
<p class="product">OpenWave</p>
<h1>{heading}</h1>
<p>{message}</p>
</main></body></html>"#
    )
}

#[derive(Clone)]
struct CallbackState {
    expected_state: String,
    sender: mpsc::Sender<CallbackResult>,
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

struct CallbackResult {
    code: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
    refresh_token: String,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
}

#[derive(Deserialize)]
struct OAuthErrorResponse {
    error: String,
    error_description: Option<String>,
}

fn random_token() -> String {
    format!("{}-{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4())
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt_with_account(account_id: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::json!({
                "https://api.openai.com/auth": {
                    "chatgpt_account_id": account_id
                }
            })
            .to_string()
            .as_bytes(),
        );
        format!("{header}.{payload}.sig")
    }

    #[test]
    fn account_id_is_read_from_access_token_claims() {
        let token = jwt_with_account("acct-123");
        assert_eq!(extract_account_id(&token).as_deref(), Some("acct-123"));
    }

    #[test]
    fn credentials_debug_redacts_tokens() {
        let credentials = ChatGptCredentials {
            access_token: "secret-access".into(),
            refresh_token: "secret-refresh".into(),
            account_id: "acct".into(),
            expires_at_unix: 1,
        };
        let debug = format!("{credentials:?}");
        assert!(!debug.contains("secret-access"));
        assert!(!debug.contains("secret-refresh"));
        assert!(debug.contains("acct"));
    }

    #[test]
    fn sign_in_required_errors_are_recognizable() {
        assert!(is_chatgpt_sign_in_required(&sign_in_required("test")));
        assert!(!is_chatgpt_sign_in_required(&chatgpt_error("x", "y")));
    }

    #[test]
    fn production_config_pins_codex_redirect() {
        let config = ChatGptAuthConfig::production();
        assert_eq!(config.redirect_uri(), REDIRECT_URI);
        assert_eq!(config.originator(), ORIGINATOR);
    }
}
