//! Loopback OAuth client for remote MCP servers.
//!
//! An HTTP MCP server can require OAuth 2.1 authorization instead of a static
//! bearer. This module carries the full lifecycle the MCP authorization
//! specification builds on top of the base RFCs:
//!
//! * discovery that a server needs OAuth — a `401` whose `WWW-Authenticate`
//!   challenge names protected-resource metadata (RFC 9728), which in turn
//!   names one or more authorization servers whose metadata (RFC 8414) carries
//!   the authorize, token, and registration endpoints;
//! * dynamic client registration (RFC 7591), because a desktop install has no
//!   pre-issued client id at the servers it will meet;
//! * authorization code + PKCE S256 (RFC 7636) on a loopback redirect
//!   (RFC 8252 §7.3), driven through the system browser; and
//! * a refresh-rotating access token presented as the per-call bearer.
//!
//! The shape mirrors [`super::ChatGptAuth`] and the gateway connector: PKCE on
//! an ephemeral loopback listener, tokens held only in the host
//! [`SecretProvider`], and no token material in any `Debug` output. The
//! deltas from ChatGPT are that the authorization server is *discovered* per
//! server rather than fixed, the client id is *registered* rather than
//! well-known, and the loopback port is *ephemeral* rather than a fixed 1455.
//!
//! Every network destination this module reaches — discovery, registration,
//! token exchange, and refresh — is admitted through [`admit_oauth_endpoint`]
//! first, so a malicious `resource_metadata` pointer cannot aim a token fetch
//! at a loopback or private-network address.
//!
//! [`SecretProvider`]: tidebreak_core::SecretProvider

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tidebreak_core::id::{ConnectedAppId, SessionId};
use tidebreak_core::{AgentError, Result, SecretProvider};

/// Grace applied to the stored access-token expiry so a token is refreshed
/// before it is actually rejected. Matches the ChatGPT connector.
const EXPIRY_LEEWAY_SECONDS: u64 = 60;

/// How long a pending browser authorization is awaited before the loopback
/// listener is torn down and the sign-in reported as failed.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

/// Secret-store key holding one server's registered OAuth client (RFC 7591).
///
/// A public client carries no secret, but the assigned `client_id` — and any
/// `registration_access_token` — still must survive restarts, so the whole
/// registration is persisted here. Derived from the connected-app record id,
/// never from a request, so this surface only ever reads and writes its own
/// secrets. Mirrors [`env_secret_key`](crate::mcp_config::env_secret_key).
#[must_use]
pub fn oauth_client_secret_key(id: ConnectedAppId) -> String {
    format!("mcp.{id}.oauth_client_v1")
}

/// Secret-store key holding one server's OAuth tokens (access, refresh,
/// expiry). Same derivation and isolation as [`oauth_client_secret_key`].
#[must_use]
pub fn oauth_token_secret_key(id: ConnectedAppId) -> String {
    format!("mcp.{id}.oauth_token_v1")
}

/// True when an operation failed because there is no usable OAuth session and
/// the user must run the Connect flow. Lets the save-and-verify path treat a
/// "needs Connect" server as pending rather than a terminal failure.
#[must_use]
pub fn is_oauth_sign_in_required(error: &AgentError) -> bool {
    matches!(error, AgentError::SignInRequired(_))
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn random_token() -> String {
    format!("{}-{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4())
}

// ---------------------------------------------------------------------------
// Discovery metadata (RFC 9728 / RFC 8414 / RFC 7591)
// ---------------------------------------------------------------------------

/// RFC 9728 protected-resource metadata, fetched from
/// `<resource>/.well-known/oauth-protected-resource` or from the URL named by
/// the `resource_metadata` parameter of a `401` `WWW-Authenticate` challenge.
#[derive(Debug, Clone, Deserialize)]
pub struct ProtectedResourceMetadata {
    /// The resource identifier the metadata describes. Advisory here.
    #[serde(default)]
    pub resource: Option<String>,
    /// Authorization servers that can issue tokens for this resource. The
    /// client uses the first whose metadata it can fetch and register with.
    #[serde(default)]
    pub authorization_servers: Vec<String>,
    #[serde(default)]
    pub scopes_supported: Vec<String>,
}

/// RFC 8414 authorization-server metadata.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthorizationServerMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    /// RFC 7591 dynamic-registration endpoint. Absent means the server issues
    /// client ids some other way, which the desktop cannot satisfy — the
    /// server is then reported as OAuth-unsupported.
    #[serde(default)]
    pub registration_endpoint: Option<String>,
    #[serde(default)]
    pub revocation_endpoint: Option<String>,
    #[serde(default)]
    pub scopes_supported: Vec<String>,
    #[serde(default)]
    pub code_challenge_methods_supported: Vec<String>,
}

/// RFC 7591 dynamic client registration response. Persisted under
/// [`oauth_client_secret_key`]; a public client gets no `client_secret`, but
/// the assigned `client_id` still must survive restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientRegistration {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub registration_access_token: Option<String>,
    #[serde(default)]
    pub registration_client_uri: Option<String>,
}

/// The authorization endpoints resolved for one server, parsed and admitted,
/// ready to drive a sign-in. Produced by [`McpOAuthClient::discover`].
#[derive(Debug, Clone)]
pub struct DiscoveredAuthorization {
    pub authorization_endpoint: url::Url,
    pub token_endpoint: url::Url,
    pub registration_endpoint: Option<url::Url>,
    pub revocation_endpoint: Option<url::Url>,
    pub scopes_supported: Vec<String>,
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

/// A server's OAuth tokens as persisted in the secret store. `Debug` is
/// hand-written so no token material is ever formatted into a log or error.
#[derive(Clone, Serialize, Deserialize)]
pub struct McpOAuthCredentials {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Absolute Unix expiry of `access_token`.
    pub expires_at_unix: u64,
    #[serde(default)]
    pub scope: Option<String>,
}

impl McpOAuthCredentials {
    /// True while the access token is comfortably in date (expiry minus the
    /// refresh leeway is still in the future).
    #[must_use]
    pub fn access_is_fresh(&self) -> bool {
        self.expires_at_unix > unix_time().saturating_add(EXPIRY_LEEWAY_SECONDS)
    }
}

impl std::fmt::Debug for McpOAuthCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpOAuthCredentials")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "<redacted>"))
            .field("expires_at_unix", &self.expires_at_unix)
            .field("scope", &self.scope)
            .finish()
    }
}

/// Secret-store-backed store for one server's OAuth client registration and
/// tokens, keyed by its connected-app record id. Fully mechanical: it is the
/// storage contract the runtime and the sign-in flow both write through.
#[derive(Clone)]
pub struct McpOAuthCredentialVault {
    secrets: Arc<dyn SecretProvider>,
    token_key: String,
    client_key: String,
}

impl McpOAuthCredentialVault {
    #[must_use]
    pub fn new(secrets: Arc<dyn SecretProvider>, id: ConnectedAppId) -> Self {
        Self {
            secrets,
            token_key: oauth_token_secret_key(id),
            client_key: oauth_client_secret_key(id),
        }
    }

    pub async fn load(&self) -> Result<Option<McpOAuthCredentials>> {
        let Some(raw) = self.secrets.get_secret(&self.token_key).await? else {
            return Ok(None);
        };
        serde_json::from_str(&raw)
            .map(Some)
            .map_err(|error| AgentError::config(format!("stored MCP OAuth token is unreadable: {error}")))
    }

    pub async fn save(&self, credentials: &McpOAuthCredentials) -> Result<()> {
        let raw = serde_json::to_string(credentials)
            .map_err(|error| AgentError::config(format!("could not serialize MCP OAuth token: {error}")))?;
        self.secrets.set_secret(&self.token_key, &raw).await
    }

    pub async fn clear(&self) -> Result<()> {
        self.secrets.delete_secret(&self.token_key).await
    }

    pub async fn load_registration(&self) -> Result<Option<ClientRegistration>> {
        let Some(raw) = self.secrets.get_secret(&self.client_key).await? else {
            return Ok(None);
        };
        serde_json::from_str(&raw).map(Some).map_err(|error| {
            AgentError::config(format!("stored MCP OAuth client registration is unreadable: {error}"))
        })
    }

    pub async fn save_registration(&self, registration: &ClientRegistration) -> Result<()> {
        let raw = serde_json::to_string(registration).map_err(|error| {
            AgentError::config(format!("could not serialize MCP OAuth client registration: {error}"))
        })?;
        self.secrets.set_secret(&self.client_key, &raw).await
    }

    pub async fn clear_registration(&self) -> Result<()> {
        self.secrets.delete_secret(&self.client_key).await
    }
}

// ---------------------------------------------------------------------------
// PKCE + authorize-URL construction (mechanical, pinned by tests)
// ---------------------------------------------------------------------------

/// A PKCE verifier and its S256 challenge (RFC 7636).
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// Mint a fresh PKCE pair: a high-entropy verifier and the base64url-unpadded
/// SHA-256 of it. Identical construction to the ChatGPT and gateway
/// connectors.
#[must_use]
pub fn pkce_pair() -> Pkce {
    let verifier = random_token();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    Pkce { verifier, challenge }
}

/// Build the RFC 6749 authorization-code request URL with a PKCE S256
/// challenge. `scopes` are space-joined per RFC 6749 §3.3 and omitted when
/// empty so a server that rejects an empty `scope` is not sent one.
#[must_use]
pub fn build_authorize_url(
    authorization_endpoint: &url::Url,
    client_id: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
    scopes: &[String],
) -> url::Url {
    let mut url = authorization_endpoint.clone();
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", state);
        if !scopes.is_empty() {
            query.append_pair("scope", &scopes.join(" "));
        }
    }
    url
}

/// Extract the `resource_metadata` URL from a `401` `WWW-Authenticate` Bearer
/// challenge (RFC 9728 §5.1). Returns `None` when the header is not a Bearer
/// challenge or names no resource metadata, in which case the client falls
/// back to the well-known path under the resource origin.
#[must_use]
pub fn resource_metadata_from_challenge(header: &str) -> Option<String> {
    let rest = header.trim().strip_prefix("Bearer ").or_else(|| header.trim().strip_prefix("bearer "))?;
    for param in rest.split(',') {
        let param = param.trim();
        let Some((key, value)) = param.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("resource_metadata") {
            return Some(value.trim().trim_matches('"').to_string());
        }
    }
    None
}

/// Admit an OAuth discovery, registration, or token endpoint before any request
/// is sent to it.
///
/// A `resource_metadata` pointer and the `authorization_servers` it names are
/// attacker-influenced: a malicious MCP server could aim them at a loopback or
/// private-network address to make the desktop fetch — and hand a bearer to —
/// something behind the user's firewall. Every resolved address must clear the
/// same denied-network list the native web-fetch path uses (loopback, RFC 1918,
/// link-local and cloud-metadata, CGNAT). Unlike [`admit_plugin_endpoint`],
/// there is **no loopback exception**: an OAuth token must never leave for a
/// loopback address discovered from server-controlled metadata.
///
/// [`admit_plugin_endpoint`]: crate::mcp_config
pub async fn admit_oauth_endpoint(url: &url::Url) -> Result<()> {
    use crate::web_search::admit_fetch_address;

    let refused = || AgentError::config("MCP OAuth endpoint is not an allowed destination");
    if url.scheme() != "https" {
        // Token-bearing traffic must be TLS; a loopback dev server is not a
        // valid OAuth authorization server for a remote resource.
        return Err(refused());
    }
    let host = url.host_str().ok_or_else(refused)?;
    let port = url.port_or_known_default().ok_or_else(refused)?;
    let addresses: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| AgentError::config("MCP OAuth endpoint host could not be resolved"))?
        .collect();
    if addresses.is_empty() {
        return Err(AgentError::config("MCP OAuth endpoint host resolved to no addresses"));
    }
    for address in addresses {
        admit_fetch_address(address.ip()).map_err(|_| refused())?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// OAuth HTTP engine
// ---------------------------------------------------------------------------

/// Parameters for an authorization-code token exchange.
pub struct CodeExchange<'a> {
    pub client_id: &'a str,
    pub client_secret: Option<&'a str>,
    pub code: &'a str,
    pub redirect_uri: &'a str,
    pub verifier: &'a str,
}

/// The network client for the OAuth flows. Holds a redirect-refusing reqwest
/// client so a token endpoint cannot bounce the exchange to a third party.
#[derive(Clone)]
pub struct McpOAuthClient {
    http: reqwest::Client,
}

impl McpOAuthClient {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| AgentError::config(format!("could not build the MCP OAuth HTTP client: {error}")))?;
        Ok(Self { http })
    }

    /// RFC 9728 → RFC 8414 discovery. Fetch protected-resource metadata (from
    /// the challenge hint or the well-known path), pick an authorization
    /// server, fetch its metadata, and parse the endpoints. Every URL —
    /// including the ones the server names — is passed through
    /// [`admit_oauth_endpoint`] before it is fetched.
    pub async fn discover(
        &self,
        resource: &url::Url,
        resource_metadata_hint: Option<&str>,
    ) -> Result<DiscoveredAuthorization> {
        let _ = (&self.http, resource, resource_metadata_hint);
        todo!(
            "RFC 9728/8414: admit + GET protected-resource metadata (hint or \
             /.well-known/oauth-protected-resource), select an authorization \
             server, admit + GET its RFC 8414 metadata, and parse+admit the \
             authorize/token/registration/revocation endpoints"
        )
    }

    /// RFC 7591 dynamic client registration. Register a public client for the
    /// loopback `redirect_uri` and return the assigned id. The endpoint is
    /// admitted before the request.
    pub async fn register(
        &self,
        registration_endpoint: &url::Url,
        redirect_uri: &str,
    ) -> Result<ClientRegistration> {
        let _ = (&self.http, registration_endpoint, redirect_uri);
        todo!(
            "RFC 7591: admit + POST client metadata (redirect_uris, \
             token_endpoint_auth_method=none, grant_types=authorization_code + \
             refresh_token, response_types=code) and parse the registration"
        )
    }

    /// Exchange an authorization code for tokens (RFC 6749 §4.1.3 + PKCE
    /// verifier). The endpoint is admitted before the request.
    pub async fn exchange_code(
        &self,
        token_endpoint: &url::Url,
        exchange: CodeExchange<'_>,
    ) -> Result<McpOAuthCredentials> {
        let _ = (&self.http, token_endpoint, exchange.client_id, exchange.code);
        todo!("RFC 6749 §4.1.3 + RFC 7636: admit + POST grant_type=authorization_code with code_verifier")
    }

    /// Refresh an access token (RFC 6749 §6). A rotated refresh token in the
    /// response replaces the stored one; its absence keeps the old one. The
    /// endpoint is admitted before the request.
    pub async fn refresh(
        &self,
        token_endpoint: &url::Url,
        client_id: &str,
        client_secret: Option<&str>,
        refresh_token: &str,
    ) -> Result<McpOAuthCredentials> {
        let _ = (&self.http, token_endpoint, client_id, client_secret, refresh_token);
        todo!("RFC 6749 §6: admit + POST grant_type=refresh_token, carry a rotated refresh token forward")
    }
}

// ---------------------------------------------------------------------------
// Pending sign-in (loopback listener) + live connection
// ---------------------------------------------------------------------------

/// A browser authorization in flight: an ephemeral loopback listener is bound
/// (RFC 8252 §7.3), the authorize URL is open in the system browser, and
/// [`finish`](Self::finish) awaits the redirect and completes the exchange.
pub struct PendingMcpSignIn {
    /// The system-browser URL the desktop opens. Never contains a token.
    pub authorization_url: url::Url,
    /// The loopback redirect the listener is bound to, echoed into the
    /// authorize request and the token exchange so they match.
    pub redirect_uri: String,
    // Retained: the bound listener, PKCE verifier, CSRF `state`, discovered
    // token endpoint, and client id. These carry no `Debug`-safe token
    // material, so the struct intentionally derives none.
    #[allow(dead_code)]
    pub(crate) verifier: String,
    #[allow(dead_code)]
    pub(crate) state: String,
}

impl PendingMcpSignIn {
    /// Await the loopback redirect, validate `state`, and exchange the code for
    /// tokens. Times out after [`SIGN_IN_TIMEOUT`]. On success the tokens are
    /// the caller's to persist through [`McpOAuthCredentialVault::save`].
    pub async fn finish(self, _client: &McpOAuthClient) -> Result<McpOAuthCredentials> {
        todo!(
            "RFC 8252: accept one loopback GET on the bound listener, reject a \
             mismatched state, serve the shared callback page, then \
             McpOAuthClient::exchange_code with the retained verifier"
        )
    }
}

/// A live, refreshing OAuth session for one connected server. The runtime holds
/// one and hands its [`access_token`](Self::access_token) to the per-call
/// bearer. Refresh is serialized under `token_motion` so concurrent calls make
/// at most one refresh.
pub struct McpOAuthConnection {
    #[allow(dead_code)]
    client: McpOAuthClient,
    #[allow(dead_code)]
    vault: McpOAuthCredentialVault,
    #[allow(dead_code)]
    token_endpoint: url::Url,
    #[allow(dead_code)]
    client_id: String,
    #[allow(dead_code)]
    client_secret: Option<String>,
    #[allow(dead_code)]
    token_motion: tokio::sync::Mutex<()>,
}

impl McpOAuthConnection {
    #[must_use]
    pub fn new(
        client: McpOAuthClient,
        vault: McpOAuthCredentialVault,
        token_endpoint: url::Url,
        client_id: String,
        client_secret: Option<String>,
    ) -> Self {
        Self {
            client,
            vault,
            token_endpoint,
            client_id,
            client_secret,
            token_motion: tokio::sync::Mutex::new(()),
        }
    }

    /// Return a currently-valid access token, refreshing under `token_motion`
    /// when the stored one is within the expiry leeway. Fails with
    /// [`AgentError::SignInRequired`] when there is no stored session or the
    /// refresh token is rejected, which is the signal the Connect flow must run
    /// again.
    pub async fn access_token(&self) -> Result<String> {
        todo!(
            "load from vault; if access_is_fresh return it; else take \
             token_motion, re-check, McpOAuthClient::refresh, save the rotated \
             credentials, and map a rejected refresh to AgentError::SignInRequired"
        )
    }
}

/// Per-`tools/call` bearer that presents the server's refreshing OAuth access
/// token. Attached to the HTTP MCP client the same way [`GatewayCallBearer`]
/// attaches the gateway's per-chat token.
///
/// [`GatewayCallBearer`]: crate::mcp_config
pub struct McpOAuthCallBearer {
    connection: Arc<McpOAuthConnection>,
}

impl McpOAuthCallBearer {
    #[must_use]
    pub fn new(connection: Arc<McpOAuthConnection>) -> Self {
        Self { connection }
    }
}

#[async_trait::async_trait]
impl tidebreak_mcp::CallBearerSource for McpOAuthCallBearer {
    async fn call_bearer(&self, _chat: SessionId) -> Result<Option<String>> {
        self.connection.access_token().await.map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_s256_of_verifier() {
        let pkce = pkce_pair();
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.verifier.as_bytes()));
        assert_eq!(pkce.challenge, expected);
        // base64url is unpadded and URL-safe.
        assert!(!pkce.challenge.contains('='));
        assert!(!pkce.challenge.contains('+'));
        assert!(!pkce.challenge.contains('/'));
    }

    #[test]
    fn authorize_url_carries_pkce_s256_and_omits_empty_scope() {
        let endpoint = url::Url::parse("https://issuer.example/authorize").unwrap();
        let url = build_authorize_url(
            &endpoint,
            "client-123",
            "http://127.0.0.1:52345/callback",
            "challenge-abc",
            "state-xyz",
            &[],
        );
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs.get("response_type").map(String::as_str), Some("code"));
        assert_eq!(pairs.get("code_challenge_method").map(String::as_str), Some("S256"));
        assert_eq!(pairs.get("client_id").map(String::as_str), Some("client-123"));
        assert_eq!(pairs.get("code_challenge").map(String::as_str), Some("challenge-abc"));
        assert_eq!(pairs.get("state").map(String::as_str), Some("state-xyz"));
        assert!(!pairs.contains_key("scope"), "empty scope must be omitted");

        let scoped = build_authorize_url(
            &endpoint,
            "client-123",
            "http://127.0.0.1:52345/callback",
            "challenge-abc",
            "state-xyz",
            &["mcp".to_string(), "profile".to_string()],
        );
        let scoped_pairs: std::collections::HashMap<_, _> =
            scoped.query_pairs().into_owned().collect();
        assert_eq!(scoped_pairs.get("scope").map(String::as_str), Some("mcp profile"));
    }

    #[test]
    fn resource_metadata_parsed_from_www_authenticate() {
        let header = r#"Bearer realm="mcp", resource_metadata="https://api.example/.well-known/oauth-protected-resource""#;
        assert_eq!(
            resource_metadata_from_challenge(header).as_deref(),
            Some("https://api.example/.well-known/oauth-protected-resource")
        );
        assert_eq!(resource_metadata_from_challenge("Basic realm=x"), None);
        assert_eq!(resource_metadata_from_challenge("Bearer realm=\"mcp\""), None);
    }

    #[test]
    fn credentials_debug_redacts_token_material() {
        let credentials = McpOAuthCredentials {
            access_token: "super-secret-access".to_string(),
            refresh_token: Some("super-secret-refresh".to_string()),
            expires_at_unix: 42,
            scope: Some("mcp".to_string()),
        };
        let rendered = format!("{credentials:?}");
        assert!(!rendered.contains("super-secret-access"), "{rendered}");
        assert!(!rendered.contains("super-secret-refresh"), "{rendered}");
        assert!(rendered.contains("expires_at_unix: 42"), "{rendered}");
    }
}
