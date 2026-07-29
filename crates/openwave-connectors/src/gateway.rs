//! Loopback OAuth client for a model-gateway deployment.
//!
//! The gateway is a public-client authorization server: PKCE S256 is
//! mandatory, there is no client secret, redirects are loopback-only, and
//! tokens are opaque with rotation and reuse detection on refresh. This
//! module mirrors the contract its own CLI uses, with one addition: an
//! explicit installation pin so a swapped-out deployment behind the same URL
//! is detected instead of silently trusted.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use openwave_core::{AgentError, Result, SecretProvider};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};

/// The audience for profile and entitlement reads (`/api/v1/cli/*`).
pub const RESOURCE_CONTROL: &str = "control";
/// The audience for inference calls on the gateway's compatible routes.
pub const RESOURCE_LLM: &str = "llm";

const SCOPE: &str = "openid profile offline_access models:read inference:invoke";
const SECRET_KEY: &str = "gateway.credentials_v1";
/// Refresh an access token this close to expiry instead of using it.
const EXPIRY_LEEWAY_SECONDS: u64 = 60;
const SIGN_IN_REQUIRED_PREFIX: &str = "gateway sign-in required";

/// True when an operation failed because there is no usable gateway session —
/// never signed in, session revoked, or refresh-token reuse detected. Callers
/// should surface a reconnect affordance instead of treating this as a fault.
#[must_use]
pub fn is_sign_in_required(error: &AgentError) -> bool {
    matches!(error, AgentError::Authentication(_))
}

fn sign_in_required(detail: &str) -> AgentError {
    AgentError::Authentication(format!("{SIGN_IN_REQUIRED_PREFIX}: {detail}"))
}

fn gateway_error(context: &str, detail: impl std::fmt::Display) -> AgentError {
    AgentError::config(format!("model-gateway {context}: {detail}"))
}

/// How this client identifies itself to the gateway.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayAuthConfig {
    base_url: reqwest::Url,
    client_id: String,
    /// Attribution declared on refresh. `None` lets the gateway default to
    /// `generic`; the gateway rejects names outside its known-client set.
    client_name: Option<String>,
}

impl GatewayAuthConfig {
    /// The registered `openwave` public client: the gateway's client registry
    /// names it on the consent page and attributes its sessions to OpenWave.
    /// The name is also declared on every refresh, because usage attribution
    /// reads the access token's client name — omitting it there would leave
    /// inference calls attributed `generic` despite the registered client id.
    pub fn new(base_url: &str) -> Result<Self> {
        Self::with_client(base_url, "openwave", Some("openwave".to_string()))
    }

    fn with_client(base_url: &str, client_id: &str, client_name: Option<String>) -> Result<Self> {
        let base_url = reqwest::Url::parse(base_url)
            .map_err(|_| gateway_error("configuration", "base URL is invalid"))?;
        if !matches!(base_url.scheme(), "http" | "https") {
            return Err(gateway_error(
                "configuration",
                "base URL must use http or https",
            ));
        }
        if !base_url.username().is_empty() || base_url.password().is_some() {
            return Err(gateway_error(
                "configuration",
                "base URL must not embed credentials",
            ));
        }
        Ok(Self {
            base_url,
            client_id: client_id.to_string(),
            client_name,
        })
    }

    /// The configured gateway origin.
    #[must_use]
    pub fn base_url(&self) -> &reqwest::Url {
        &self.base_url
    }
}

/// Unauthenticated deployment identity from `/api/v1/meta`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GatewayMeta {
    pub api_version: String,
    pub installation_id: String,
    pub gateway_version: String,
    pub public_url: String,
    pub auth_mode: String,
}

/// The authenticated user, from `/api/v1/cli/me`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GatewayIdentity {
    pub user_id: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub session_id: String,
    pub installation_id: String,
}

/// One entitled model from `/api/v1/cli/models`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GatewayModel {
    pub id: String,
    pub name: String,
    pub context_window: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub supports_tools: bool,
    pub supports_vision: bool,
}

/// One entitled connected app from `/api/v1/cli/apps`: the apps the user
/// reaches through team grants, and the slugs of the MCP endpoints that
/// aggregate each one (the `mcp:<slug>` resources a client may request).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GatewayApp {
    pub id: String,
    pub name: String,
    pub app_kind: String,
    pub enabled: bool,
    pub mcp_endpoint_slugs: Vec<String>,
}

/// One minted access/refresh pair, resource-bound.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_unix: u64,
    pub scope: String,
    pub resource: String,
    pub installation_id: String,
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TokenSet")
            .field("resource", &self.resource)
            .field("scope", &self.scope)
            .field("expires_at_unix", &self.expires_at_unix)
            .field("installation_id", &self.installation_id)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CachedAccessToken {
    token: String,
    expires_at_unix: u64,
    scope: String,
}

impl CachedAccessToken {
    fn is_fresh(&self) -> bool {
        self.expires_at_unix > unix_time().saturating_add(EXPIRY_LEEWAY_SECONDS)
    }
}

/// The durable signed-in state, serialized as one keychain secret.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayCredentials {
    pub base_url: String,
    pub installation_id: String,
    pub user_id: String,
    pub account_hint: Option<String>,
    refresh_token: String,
    access_tokens: BTreeMap<String, CachedAccessToken>,
}

impl GatewayCredentials {
    /// Whether these credentials were minted against `base_url`. Trailing
    /// slashes are normalized the same way [`GatewayAuth::endpoint`] does, so
    /// a subpath deployment retyped with or without one never reads as a
    /// different gateway. A base URL that does not parse matches nothing.
    #[must_use]
    pub fn matches_base_url(&self, base_url: &str) -> bool {
        fn normalized(url: &reqwest::Url) -> String {
            let mut base = url.to_string();
            if !base.ends_with('/') {
                base.push('/');
            }
            base
        }
        match (
            reqwest::Url::parse(&self.base_url),
            reqwest::Url::parse(base_url),
        ) {
            (Ok(stored), Ok(expected)) => normalized(&stored) == normalized(&expected),
            _ => false,
        }
    }
}

impl std::fmt::Debug for GatewayCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GatewayCredentials")
            .field("base_url", &self.base_url)
            .field("installation_id", &self.installation_id)
            .field("user_id", &self.user_id)
            .field("account_hint", &self.account_hint)
            .field(
                "cached_resources",
                &self.access_tokens.keys().collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

/// Whether a gateway session is stored, without constructing a vault.
///
/// For readiness surfaces (`has_credential`-style projections) that hold only
/// a borrowed [`SecretProvider`].
pub async fn has_stored_credentials(secrets: &dyn SecretProvider) -> bool {
    matches!(
        secrets.get_secret(SECRET_KEY).await,
        Ok(Some(raw)) if serde_json::from_str::<GatewayCredentials>(&raw).is_ok()
    )
}

/// Whether a gateway session minted against `base_url` is stored.
///
/// The deployment-matched form of [`has_stored_credentials`], for readiness
/// surfaces on a managed profile: a session left by a superseded deployment
/// must not read as a credential for the policy's current one — every route
/// and token path already refuses it.
pub async fn has_stored_credentials_for(secrets: &dyn SecretProvider, base_url: &str) -> bool {
    matches!(
        secrets.get_secret(SECRET_KEY).await,
        Ok(Some(raw)) if serde_json::from_str::<GatewayCredentials>(&raw)
            .is_ok_and(|credentials| credentials.matches_base_url(base_url))
    )
}

/// Keychain-backed storage for [`GatewayCredentials`].
#[derive(Clone)]
pub struct CredentialVault {
    secrets: Arc<dyn SecretProvider>,
}

impl CredentialVault {
    #[must_use]
    pub fn new(secrets: Arc<dyn SecretProvider>) -> Self {
        Self { secrets }
    }

    pub async fn load(&self) -> Result<Option<GatewayCredentials>> {
        let Some(raw) = self.secrets.get_secret(SECRET_KEY).await? else {
            return Ok(None);
        };
        serde_json::from_str(&raw)
            .map(Some)
            .map_err(|_| gateway_error("credentials", "stored credentials are unreadable"))
    }

    pub async fn save(&self, credentials: &GatewayCredentials) -> Result<()> {
        let raw = serde_json::to_string(credentials)?;
        self.secrets.set_secret(SECRET_KEY, &raw).await
    }

    pub async fn clear(&self) -> Result<()> {
        self.secrets.delete_secret(SECRET_KEY).await
    }
}

/// Stateless HTTP client for the gateway's OAuth and CLI endpoints.
#[derive(Clone)]
pub struct GatewayAuth {
    config: GatewayAuthConfig,
    http: reqwest::Client,
}

impl GatewayAuth {
    pub fn new(config: GatewayAuthConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| gateway_error("client", error))?;
        Ok(Self { config, http })
    }

    /// Fetch the deployment identity used for installation pinning.
    pub async fn meta(&self) -> Result<GatewayMeta> {
        let response = self
            .http
            .get(self.endpoint("/api/v1/meta")?)
            .send()
            .await
            .map_err(|error| gateway_error("metadata request", error.without_url()))?;
        decode_json(response, "metadata request").await
    }

    /// Bind the loopback listener and produce the URL the user's browser
    /// must visit. The listener stays alive inside the returned
    /// [`PendingSignIn`] until [`PendingSignIn::finish`] resolves.
    pub async fn start_sign_in(&self) -> Result<PendingSignIn> {
        let meta = self.meta().await?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|error| gateway_error("sign-in", error))?;
        let port = listener
            .local_addr()
            .map_err(|error| gateway_error("sign-in", error))?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");
        let verifier = random_token();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = random_token();

        let mut authorization_url = self.endpoint("/oauth/authorize")?;
        authorization_url
            .query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("scope", SCOPE);

        Ok(PendingSignIn {
            auth: self.clone(),
            meta,
            listener,
            authorization_url: authorization_url.to_string(),
            redirect_uri,
            verifier,
            state,
        })
    }

    /// Exchange the refresh token for a token bound to `resource`, rotating
    /// the refresh token. `invalid_grant` — an expired, revoked, or reused
    /// token — maps to a sign-in-required error.
    pub async fn refresh(
        &self,
        refresh_token: &str,
        resource: &str,
        expected_installation: &str,
    ) -> Result<TokenSet> {
        let mut form: Vec<(&str, &str)> = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("resource", resource),
        ];
        if let Some(client_name) = self.config.client_name.as_deref() {
            form.push(("client_name", client_name));
        }
        let token = self.request_token(&form).await?;
        if token.resource != resource || token.installation_id != expected_installation {
            return Err(gateway_error(
                "token request",
                "token was minted for an unexpected installation or resource",
            ));
        }
        Ok(token)
    }

    /// Revoke a refresh token and its session family.
    pub async fn revoke(&self, refresh_token: &str) -> Result<()> {
        let response = self
            .http
            .post(self.endpoint("/oauth/revoke")?)
            .form(&[("token", refresh_token)])
            .send()
            .await
            .map_err(|error| gateway_error("revocation request", error.without_url()))?;
        if !response.status().is_success() {
            return Err(gateway_error(
                "revocation request",
                format!("HTTP status {}", response.status().as_u16()),
            ));
        }
        Ok(())
    }

    /// The authenticated identity behind a `control` token.
    pub async fn whoami(&self, access_token: &str) -> Result<GatewayIdentity> {
        let response = self
            .http
            .get(self.endpoint("/api/v1/cli/me")?)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|error| gateway_error("identity request", error.without_url()))?;
        decode_json(response, "identity request").await
    }

    /// The models the authenticated user may invoke, optionally filtered to
    /// one inference protocol (`anthropic` / `openai`).
    pub async fn models(
        &self,
        access_token: &str,
        protocol: Option<&str>,
    ) -> Result<Vec<GatewayModel>> {
        let mut url = self.endpoint("/api/v1/cli/models")?;
        if let Some(protocol) = protocol {
            url.query_pairs_mut().append_pair("protocol", protocol);
        }
        let response = self
            .http
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|error| gateway_error("model request", error.without_url()))?;
        let list: ModelListResponse = decode_json(response, "model request").await?;
        Ok(list.models)
    }

    /// The connected apps the authenticated user is entitled to, or `None`
    /// against a gateway that predates the JSON apps surface.
    pub async fn apps(&self, access_token: &str) -> Result<Option<Vec<GatewayApp>>> {
        let response = self
            .http
            .get(self.endpoint("/api/v1/cli/apps")?)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|error| gateway_error("app request", error.without_url()))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let list: AppListResponse = decode_json(response, "app request").await?;
        Ok(Some(list.apps))
    }

    async fn exchange_code(
        &self,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
    ) -> Result<TokenSet> {
        self.request_token(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", &self.config.client_id),
            ("code_verifier", verifier),
        ])
        .await
    }

    async fn request_token(&self, form: &[(&str, &str)]) -> Result<TokenSet> {
        let response = self
            .http
            .post(self.endpoint("/oauth/token")?)
            .form(form)
            .send()
            .await
            .map_err(|error| gateway_error("token request", error.without_url()))?;
        if !response.status().is_success() {
            let status = response.status();
            let error: Option<OAuthErrorResponse> = response.json().await.ok();
            if error
                .as_ref()
                .is_some_and(|error| error.error == "invalid_grant")
            {
                return Err(sign_in_required("the gateway session is no longer valid"));
            }
            let detail = error
                .and_then(|error| error.error_description.or(Some(error.error)))
                .unwrap_or_else(|| format!("HTTP status {}", status.as_u16()));
            return Err(gateway_error("token request", detail));
        }
        let token: TokenResponse = response
            .json()
            .await
            .map_err(|_| gateway_error("token request", "invalid token response"))?;
        Ok(TokenSet {
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            expires_at_unix: unix_time()
                .saturating_add(u64::try_from(token.expires_in).unwrap_or_default()),
            scope: token.scope,
            resource: token.resource,
            installation_id: token.installation_id,
        })
    }

    fn endpoint(&self, path: &str) -> Result<reqwest::Url> {
        // Join below the configured base rather than at its origin, so a
        // gateway deployed under a subpath keeps its prefix.
        let mut base = self.config.base_url.to_string();
        if !base.ends_with('/') {
            base.push('/');
        }
        reqwest::Url::parse(&base)
            .and_then(|base| base.join(path.trim_start_matches('/')))
            .map_err(|_| gateway_error("configuration", "could not build endpoint URL"))
    }
}

/// A sign-in in flight: the loopback listener is bound and the browser URL
/// is ready. Dropping this cancels the sign-in.
pub struct PendingSignIn {
    auth: GatewayAuth,
    meta: GatewayMeta,
    listener: TcpListener,
    authorization_url: String,
    redirect_uri: String,
    verifier: String,
    state: String,
}

/// A completed sign-in: identity plus the initial `control`-resource tokens.
#[derive(Debug, Clone)]
pub struct AuthorizedSession {
    pub meta: GatewayMeta,
    pub identity: GatewayIdentity,
    pub tokens: TokenSet,
}

impl PendingSignIn {
    /// The URL to open in the user's browser.
    #[must_use]
    pub fn authorization_url(&self) -> &str {
        &self.authorization_url
    }

    /// The deployment identity pinned when the sign-in started.
    #[must_use]
    pub fn meta(&self) -> &GatewayMeta {
        &self.meta
    }

    /// Wait for the browser callback, then exchange and validate the code.
    pub async fn finish(self, timeout: Duration) -> Result<AuthorizedSession> {
        let Self {
            auth,
            meta,
            listener,
            redirect_uri,
            verifier,
            state,
            ..
        } = self;

        let (sender, mut receiver) = mpsc::channel::<CallbackResult>(1);
        let callback_app =
            Router::new()
                .route("/callback", get(callback))
                .with_state(CallbackState {
                    expected_state: state,
                    sender,
                });
        let server = tokio::spawn(async move { axum::serve(listener, callback_app).await });
        let outcome = tokio::time::timeout(timeout, receiver.recv()).await;
        server.abort();
        let _ = server.await;

        let callback = outcome
            .map_err(|_| gateway_error("sign-in", "browser authorization timed out"))?
            .ok_or_else(|| gateway_error("sign-in", "the authorization callback closed"))?;
        if let Some(error) = callback.error {
            return Err(gateway_error(
                "sign-in",
                format!("authorization failed: {error}"),
            ));
        }
        let code = callback
            .code
            .ok_or_else(|| gateway_error("sign-in", "no authorization code was returned"))?;

        let tokens = auth.exchange_code(&code, &redirect_uri, &verifier).await?;
        if tokens.resource != RESOURCE_CONTROL || tokens.installation_id != meta.installation_id {
            return Err(gateway_error(
                "sign-in",
                "token was minted for an unexpected installation or resource",
            ));
        }
        let identity = auth.whoami(&tokens.access_token).await?;
        if identity.installation_id != meta.installation_id {
            return Err(gateway_error(
                "sign-in",
                "gateway identity changed during authorization",
            ));
        }
        Ok(AuthorizedSession {
            meta,
            identity,
            tokens,
        })
    }
}

/// A signed-in gateway connection: vault-backed and refresh-rotating.
///
/// Share the *instance*: token motion is serialized per connection, so one
/// shared `GatewayConnection` can never race itself into the gateway's
/// reuse detection — but two instances (or two processes) over the same
/// keychain entry can. A crash between a successful refresh and the vault
/// write loses the rotated token, which reads as signed-out on the next
/// call; that window is inherent to rotation and recovers via reconnect.
pub struct GatewayConnection {
    auth: GatewayAuth,
    vault: CredentialVault,
    token_motion: Mutex<()>,
}

impl GatewayConnection {
    #[must_use]
    pub fn new(auth: GatewayAuth, vault: CredentialVault) -> Self {
        Self {
            auth,
            vault,
            token_motion: Mutex::new(()),
        }
    }

    /// The underlying auth client, for flows that operate before a session
    /// exists (starting a browser sign-in).
    #[must_use]
    pub fn auth(&self) -> &GatewayAuth {
        &self.auth
    }

    /// Persist a completed sign-in as the connection's stored credentials.
    ///
    /// The vault holds one session, so whatever was stored is superseded the
    /// moment this saves: its refresh token is revoked first (best-effort, at
    /// its own gateway), or it would stay live server-side with no local
    /// state left to ever revoke it.
    pub async fn store_session(&self, session: &AuthorizedSession) -> Result<()> {
        let _guard = self.token_motion.lock().await;
        // An unreadable stored blob is skipped, as in sign-out: it carries no
        // usable refresh token to revoke.
        if let Ok(Some(superseded)) = self.vault.load().await {
            self.revoke_stored(&superseded).await;
        }
        let mut access_tokens = BTreeMap::new();
        access_tokens.insert(
            session.tokens.resource.clone(),
            CachedAccessToken {
                token: session.tokens.access_token.clone(),
                expires_at_unix: session.tokens.expires_at_unix,
                scope: session.tokens.scope.clone(),
            },
        );
        self.vault
            .save(&GatewayCredentials {
                base_url: self.auth.config.base_url.to_string(),
                installation_id: session.meta.installation_id.clone(),
                user_id: session.identity.user_id.clone(),
                account_hint: session
                    .identity
                    .email
                    .clone()
                    .or_else(|| session.identity.display_name.clone()),
                refresh_token: session.tokens.refresh_token.clone(),
                access_tokens,
            })
            .await
    }

    /// Whether stored credentials were minted against this connection's
    /// configured gateway. Trailing slashes are normalized the same way
    /// [`GatewayAuth::endpoint`] does, so a subpath deployment retyped with
    /// or without one never reads as a different gateway.
    fn matches_deployment(&self, credentials: &GatewayCredentials) -> bool {
        credentials.matches_base_url(self.auth.config.base_url.as_str())
    }

    /// Best-effort, bounded revocation of a stored session at its own
    /// gateway.
    ///
    /// The stored credentials name the deployment they were minted against,
    /// which need not be this connection's own (an MDM re-point, a re-pair):
    /// the revoke goes where the token is valid. A gateway that is
    /// unreachable, gone, or whose stored URL no longer parses cannot hold
    /// the caller hostage — the server-side session still dies at
    /// refresh-token expiry.
    async fn revoke_stored(&self, credentials: &GatewayCredentials) {
        const REVOKE_TIMEOUT: Duration = Duration::from_secs(5);
        let auth = if self.matches_deployment(credentials) {
            Some(self.auth.clone())
        } else {
            GatewayAuthConfig::new(&credentials.base_url)
                .and_then(GatewayAuth::new)
                .ok()
        };
        let Some(auth) = auth else { return };
        let _ = tokio::time::timeout(REVOKE_TIMEOUT, auth.revoke(&credentials.refresh_token)).await;
    }

    /// The stored (offline) identity, if signed in to this connection's
    /// configured gateway. A session minted against a different deployment
    /// reads as absent: reporting it would assert an identity that is simply
    /// wrong for the configured base URL.
    pub async fn stored_credentials(&self) -> Result<Option<GatewayCredentials>> {
        Ok(self
            .vault
            .load()
            .await?
            .filter(|credentials| self.matches_deployment(credentials)))
    }

    /// A fresh access token for `resource`, from cache when possible and via
    /// rotating refresh otherwise.
    pub async fn access_token(&self, resource: &str) -> Result<String> {
        let _guard = self.token_motion.lock().await;
        let Some(mut credentials) = self.vault.load().await? else {
            return Err(sign_in_required("no gateway session is stored"));
        };
        // Refuse early rather than let installation pinning fail downstream:
        // "sign-in required" is actionable, a provider 500 is not.
        if !self.matches_deployment(&credentials) {
            return Err(sign_in_required(
                "the stored gateway session belongs to a different gateway deployment",
            ));
        }
        if let Some(cached) = credentials.access_tokens.get(resource) {
            if cached.is_fresh() {
                return Ok(cached.token.clone());
            }
        }
        let token = self
            .auth
            .refresh(
                &credentials.refresh_token,
                resource,
                &credentials.installation_id,
            )
            .await?;
        credentials.refresh_token = token.refresh_token.clone();
        credentials.access_tokens.insert(
            resource.to_string(),
            CachedAccessToken {
                token: token.access_token.clone(),
                expires_at_unix: token.expires_at_unix,
                scope: token.scope.clone(),
            },
        );
        self.vault.save(&credentials).await?;
        Ok(token.access_token)
    }

    /// Live identity check with a `control` token.
    pub async fn identity(&self) -> Result<GatewayIdentity> {
        let token = self.access_token(RESOURCE_CONTROL).await?;
        self.auth.whoami(&token).await
    }

    /// Entitled models with a `control` token.
    pub async fn models(&self, protocol: Option<&str>) -> Result<Vec<GatewayModel>> {
        let token = self.access_token(RESOURCE_CONTROL).await?;
        self.auth.models(&token, protocol).await
    }

    /// Entitled connected apps with a `control` token; `None` when the
    /// gateway predates the apps surface.
    pub async fn apps(&self) -> Result<Option<Vec<GatewayApp>>> {
        let token = self.access_token(RESOURCE_CONTROL).await?;
        self.auth.apps(&token).await
    }

    /// The gateway's MCP endpoint URL for `slug`, under the configured base.
    pub fn mcp_endpoint_url(&self, slug: &str) -> Result<String> {
        validate_mcp_endpoint_slug(slug)?;
        self.auth
            .endpoint(&format!("mcp/{slug}"))
            .map(|url| url.to_string())
    }

    /// A fresh access token bound to the `mcp:<slug>` resource, from cache
    /// when possible and via rotating refresh otherwise.
    pub async fn mcp_access_token(&self, slug: &str) -> Result<String> {
        validate_mcp_endpoint_slug(slug)?;
        self.access_token(&format!("mcp:{slug}")).await
    }

    /// Revoke the session at its own gateway (best-effort, bounded) and
    /// always clear the local vault. A gateway that is unreachable cannot
    /// hold local sign-out hostage; its server-side session still dies at
    /// refresh-token expiry.
    pub async fn sign_out(&self) -> Result<()> {
        let _guard = self.token_motion.lock().await;
        if let Some(credentials) = self.vault.load().await? {
            self.revoke_stored(&credentials).await;
        }
        self.vault.clear().await
    }
}

async fn callback(
    State(state): State<CallbackState>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    if query.state.as_deref() != Some(state.expected_state.as_str()) {
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

/// One-shot page served by the ephemeral loopback listener; it must stay
/// fully self-contained (inline CSS, no external assets).
fn callback_page(heading: &str, message: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{heading}</title>
<style>
  :root {{
    color-scheme: light dark;
    --bg: #f4f4f5;
    --card: #ffffff;
    --border: #e4e4e7;
    --text: #18181b;
    --muted: #52525b;
  }}
  @media (prefers-color-scheme: dark) {{
    :root {{
      --bg: #18181b;
      --card: #232326;
      --border: #3f3f46;
      --text: #fafafa;
      --muted: #a1a1aa;
    }}
  }}
  body {{
    margin: 0;
    min-height: 100vh;
    display: grid;
    place-items: center;
    background: var(--bg);
    color: var(--text);
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto,
      "Helvetica Neue", Arial, sans-serif;
  }}
  main {{
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 2.5rem 3rem;
    margin: 1rem;
    max-width: 24rem;
    text-align: center;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.08);
  }}
  .product {{
    font-size: 0.8125rem;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    margin: 0 0 1rem;
  }}
  h1 {{
    font-size: 1.375rem;
    font-weight: 600;
    margin: 0 0 0.5rem;
  }}
  p {{
    color: var(--muted);
    font-size: 0.9375rem;
    line-height: 1.5;
    margin: 0;
  }}
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
    scope: String,
    resource: String,
    installation_id: String,
}

#[derive(Deserialize)]
struct OAuthErrorResponse {
    error: String,
    error_description: Option<String>,
}

#[derive(Deserialize)]
struct ModelListResponse {
    models: Vec<GatewayModel>,
}

#[derive(Deserialize)]
struct AppListResponse {
    apps: Vec<GatewayApp>,
}

/// The gateway's endpoint-slug contract: 1–127 bytes of ASCII alphanumerics,
/// `-`, or `_`. Enforced here so a slug can never rewrite the request path
/// (`/`, `..`) or the token resource string it is embedded into. Public so
/// configuration layers validate against the same contract instead of a
/// second copy of these literals.
pub fn validate_mcp_endpoint_slug(slug: &str) -> Result<()> {
    if slug.is_empty()
        || slug.len() > 127
        || !slug
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(gateway_error("configuration", "invalid MCP endpoint slug"));
    }
    Ok(())
}

async fn decode_json<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
    operation: &str,
) -> Result<T> {
    if !response.status().is_success() {
        return Err(gateway_error(
            operation,
            format!("HTTP status {}", response.status().as_u16()),
        ));
    }
    response
        .json()
        .await
        .map_err(|_| gateway_error(operation, "invalid response body"))
}

/// URL-safe random material from the OS CSPRNG. Two UUIDv4 values give 244
/// bits of entropy and a 73-byte token, inside PKCE's 43–128 char contract.
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

    #[test]
    fn config_rejects_invalid_base_urls() {
        assert!(GatewayAuthConfig::new("http://127.0.0.1:28081").is_ok());
        assert!(GatewayAuthConfig::new("https://gateway.example").is_ok());
        for url in ["ftp://x", "not a url", "http://user:pw@host"] {
            assert!(GatewayAuthConfig::new(url).is_err(), "{url}");
        }
    }

    #[test]
    fn token_types_redact_secret_material() {
        let tokens = TokenSet {
            access_token: "mg_at_secret".into(),
            refresh_token: "mg_rt_secret".into(),
            expires_at_unix: 0,
            scope: "profile".into(),
            resource: RESOURCE_CONTROL.into(),
            installation_id: "install".into(),
        };
        let debug = format!("{tokens:?}");
        assert!(!debug.contains("mg_at_secret"));
        assert!(!debug.contains("mg_rt_secret"));

        let credentials = GatewayCredentials {
            base_url: "http://127.0.0.1/".into(),
            installation_id: "install".into(),
            user_id: "user".into(),
            account_hint: Some("a@example.com".into()),
            refresh_token: "mg_rt_secret".into(),
            access_tokens: BTreeMap::from([(
                RESOURCE_CONTROL.to_string(),
                CachedAccessToken {
                    token: "mg_at_secret".into(),
                    expires_at_unix: 0,
                    scope: "profile".into(),
                },
            )]),
        };
        let debug = format!("{credentials:?}");
        assert!(!debug.contains("mg_at_secret"));
        assert!(!debug.contains("mg_rt_secret"));
        assert!(debug.contains("control"));
    }

    #[test]
    fn random_tokens_fit_the_pkce_contract() {
        let token = random_token();
        assert!((43..=128).contains(&token.len()), "{}", token.len());
        assert!(token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'));
        assert_ne!(random_token(), random_token());
    }

    #[test]
    fn sign_in_required_errors_are_recognizable() {
        assert!(is_sign_in_required(&sign_in_required("test")));
        assert!(!is_sign_in_required(&gateway_error("x", "y")));
    }
}
