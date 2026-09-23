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
//!   (RFC 8252 §7.3), with the resource indicator (RFC 8707) the MCP
//!   specification requires, driven through the person's browser; and
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

use std::future::IntoFuture;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tidebreak_core::id::{ConnectedAppId, SessionId};
use tidebreak_core::{AgentError, Result, SecretProvider};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// Grace applied to the stored access-token expiry so a token is refreshed
/// before it is actually rejected. Matches the ChatGPT connector.
const EXPIRY_LEEWAY_SECONDS: u64 = 60;

/// How long a pending browser authorization is awaited before the loopback
/// listener is torn down and the sign-in reported as failed.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

/// Access-token lifetime used when a token response omits `expires_in`.
/// Short so a missing expiry never becomes "never expires".
const DEFAULT_ACCESS_TTL_SECONDS: u64 = 300;

/// The largest discovery, registration, or token response read. These are
/// small JSON documents; discovery runs on every `401`, so a server must not
/// be able to make Tidebreak read an unbounded body.
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

/// The diagnostic for a token request the sign-in service did not answer.
/// Temporary: the session is kept and the connection retried. The runtime's
/// diagnostic classifier recognizes this text by its first words.
pub const SIGN_IN_SERVICE_UNAVAILABLE: &str = "Sign-in service unavailable. Tidebreak could \
                                               not refresh your sign-in for this server and \
                                               will try again.";

/// The most one discovery, registration, or token request may take.
/// Discovery runs inside a failed connection and refresh inside a tool call,
/// so a server that never answers must not hold either open.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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
    /// The resource identifier the metadata describes. Sent as the RFC 8707
    /// resource indicator when it shares the server's origin.
    #[serde(default)]
    pub resource: Option<String>,
    /// Authorization servers that can issue tokens for this resource. The
    /// client uses the first whose metadata it can fetch and register with.
    #[serde(default)]
    pub authorization_servers: Vec<String>,
    #[serde(default)]
    pub scopes_supported: Vec<String>,
}

/// RFC 8414 authorization-server metadata, or the same fields from an OpenID
/// Connect discovery document.
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

/// The resolved OAuth client for one server, persisted under
/// [`oauth_client_secret_key`]. It carries the RFC 7591 dynamic-registration
/// response *plus* the discovery result the runtime needs to mint and refresh
/// tokens after a restart without repeating discovery on every process start:
/// the token endpoint and the scopes the authorize request used.
///
/// A public client gets no `client_secret`, but the assigned `client_id`, the
/// token endpoint, and the scopes still must survive restarts, so the whole
/// record is persisted here. The `token_endpoint`/`scopes` fields default so an
/// older stored record (registration only) still deserializes; the runtime
/// treats a record whose `token_endpoint` is absent as needing rediscovery.
///
/// The record is bound to the one server URL it was issued for
/// ([`server_url`](Self::server_url)). A session is presented only to that
/// exact URL, so editing a server's URL can never hand its token to the new
/// address. `Debug` is hand-written to keep the secret and the registration
/// access token out of every log.
#[derive(Clone, Serialize, Deserialize)]
pub struct ClientRegistration {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub registration_access_token: Option<String>,
    #[serde(default)]
    pub registration_client_uri: Option<String>,
    /// RFC 8414 token endpoint resolved at Connect time. Absolute `https` URL,
    /// already passed through [`admit_oauth_endpoint`]. Absent in a record
    /// written before this field existed.
    #[serde(default)]
    pub token_endpoint: Option<String>,
    /// Scopes granted at registration/authorize time, carried so a refresh and
    /// a re-authorize request the same grant. Empty when the server took none.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// RFC 8707 resource indicator the tokens were requested for, sent again
    /// on every refresh. Absent in a record written before this field
    /// existed, which then refreshes without one, as it always did.
    #[serde(default)]
    pub resource: Option<String>,
    /// The exact MCP server URL this session was issued for. A session whose
    /// URL differs from the server's current one is never presented, and a
    /// record written before this field existed has none, so it is never
    /// presented either.
    #[serde(default)]
    pub server_url: Option<String>,
    /// Host of the sign-in page, shown in Settings beside the session.
    #[serde(default)]
    pub sign_in_host: Option<String>,
}

impl std::fmt::Debug for ClientRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientRegistration")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "registration_access_token",
                &self
                    .registration_access_token
                    .as_ref()
                    .map(|_| "<redacted>"),
            )
            .field("registration_client_uri", &self.registration_client_uri)
            .field("token_endpoint", &self.token_endpoint)
            .field("scopes", &self.scopes)
            .field("resource", &self.resource)
            .field("server_url", &self.server_url)
            .field("sign_in_host", &self.sign_in_host)
            .finish()
    }
}

/// The authorization endpoints resolved for one server, parsed and admitted,
/// ready to drive a sign-in. Produced by [`McpOAuthClient::discover`].
#[derive(Debug, Clone)]
pub struct DiscoveredAuthorization {
    pub authorization_endpoint: url::Url,
    pub token_endpoint: url::Url,
    pub registration_endpoint: url::Url,
    pub revocation_endpoint: Option<url::Url>,
    /// The scopes to request: the challenge's `scope` when the server named
    /// one, otherwise the protected resource's `scopes_supported`, otherwise
    /// none. This is the MCP specification's scope-selection order.
    pub scopes: Vec<String>,
    /// The RFC 8707 resource indicator for this server: the identifier its
    /// protected-resource metadata declares, which [`resource_matches`] has
    /// already tied to this server's URL, or the server URL itself.
    pub resource: String,
    /// Host of the authorization endpoint: where Connect sends the person.
    pub sign_in_host: String,
}

/// What discovery learned about signing in to one MCP server.
#[derive(Debug, Clone)]
pub enum Discovery {
    /// The server names an OAuth sign-in Tidebreak can run.
    Supported(Box<DiscoveredAuthorization>),
    /// The server asks for OAuth sign-in in a way Tidebreak cannot complete.
    Unsupported(OAuthUnsupported),
    /// The server asks for OAuth sign-in, but its metadata or its sign-in
    /// service did not answer: a timeout, a network failure, a `5xx`, or a
    /// name that did not resolve. Temporary; ask again later.
    Unavailable,
}

/// Why Tidebreak cannot complete a server's OAuth sign-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthUnsupported {
    /// The authorization server offers no dynamic client registration, and a
    /// desktop install has no client ID issued in advance.
    NoClientRegistration,
    /// No authorization-server metadata Tidebreak can read.
    UnreadableMetadata,
    /// The authorization server, or an endpoint it names, is not a public
    /// `https` address.
    RefusedEndpoint,
    /// The protected-resource metadata describes a different server
    /// (RFC 9728 §3.3).
    ResourceMismatch,
    /// The authorization-server metadata names a different issuer than the
    /// one it was fetched for (RFC 8414 §3.3).
    IssuerMismatch,
    /// The authorization server lists PKCE methods, and S256 is not one of
    /// them.
    NoS256,
}

impl OAuthUnsupported {
    /// The reason as a sentence for the person. Never names a URL.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::NoClientRegistration => {
                "Its sign-in service does not let new apps register (no dynamic client \
                 registration), and Tidebreak has no client ID for it."
            }
            Self::UnreadableMetadata => {
                "Tidebreak could not read the settings of its sign-in service."
            }
            Self::RefusedEndpoint => "Its sign-in service is not at a public https address.",
            Self::ResourceMismatch => {
                "Its sign-in settings describe a different server, so Tidebreak does not use \
                 them."
            }
            Self::IssuerMismatch => {
                "Its sign-in service's settings name a different service, so Tidebreak does \
                 not use them."
            }
            Self::NoS256 => {
                "Its sign-in service does not offer the PKCE method Tidebreak requires (S256)."
            }
        }
    }
}

/// Why a browser sign-in did not store a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInFailure {
    /// The authorization server refused to register Tidebreak as a client.
    RegistrationRefused,
    /// The person declined, or the authorization server refused the request.
    Denied,
    /// Nobody finished the sign-in in the browser in time.
    TimedOut,
    /// The authorization server did not accept the authorization code.
    ExchangeRefused,
    /// A network failure or an answer Tidebreak could not read.
    Failed,
}

impl SignInFailure {
    /// The failure as a sentence for the person. Never names a URL or echoes
    /// anything the server sent.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::RegistrationRefused => {
                "The server refused to register Tidebreak for sign-in. It may allow only \
                 apps it has approved."
            }
            Self::Denied => "The sign-in was canceled or denied. Select Try again to start over.",
            Self::TimedOut => {
                "The sign-in timed out before you finished it. Select Connect to try again."
            }
            Self::ExchangeRefused => {
                "The server did not accept the sign-in. Select Connect to try again."
            }
            Self::Failed => {
                "Tidebreak could not finish the sign-in. Check your connection, then select \
                 Connect to try again."
            }
        }
    }
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
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
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
        serde_json::from_str(&raw).map(Some).map_err(|error| {
            AgentError::config(format!("stored MCP OAuth token is unreadable: {error}"))
        })
    }

    pub async fn save(&self, credentials: &McpOAuthCredentials) -> Result<()> {
        let raw = serde_json::to_string(credentials).map_err(|error| {
            AgentError::config(format!("could not serialize MCP OAuth token: {error}"))
        })?;
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
            AgentError::config(format!(
                "stored MCP OAuth client registration is unreadable: {error}"
            ))
        })
    }

    pub async fn save_registration(&self, registration: &ClientRegistration) -> Result<()> {
        let raw = serde_json::to_string(registration).map_err(|error| {
            AgentError::config(format!(
                "could not serialize MCP OAuth client registration: {error}"
            ))
        })?;
        self.secrets.set_secret(&self.client_key, &raw).await
    }

    pub async fn clear_registration(&self) -> Result<()> {
        self.secrets.delete_secret(&self.client_key).await
    }

    /// The stored registration and tokens, but only when they were issued
    /// for exactly `server_url`. A session bound to another URL, or to none
    /// (a record from before sessions were bound), reads as no session.
    pub async fn load_for(
        &self,
        server_url: &str,
    ) -> Result<Option<(ClientRegistration, McpOAuthCredentials)>> {
        let Some(registration) = self
            .load_registration()
            .await?
            .filter(|registration| registration.server_url.as_deref() == Some(server_url))
        else {
            return Ok(None);
        };
        Ok(self
            .load()
            .await?
            .map(|credentials| (registration, credentials)))
    }

    /// Remove the tokens and the registration.
    pub async fn clear_all(&self) -> Result<()> {
        self.clear().await?;
        self.clear_registration().await
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
    Pkce {
        verifier,
        challenge,
    }
}

/// Build the RFC 6749 authorization-code request URL with a PKCE S256
/// challenge. `scopes` are space-joined per RFC 6749 §3.3 and omitted when
/// empty so a server that rejects an empty `scope` is not sent one. `resource`
/// is the RFC 8707 resource indicator the MCP specification requires; an
/// authorization server that does not know the parameter ignores it.
#[must_use]
pub fn build_authorize_url(
    authorization_endpoint: &url::Url,
    client_id: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
    scopes: &[String],
    resource: Option<&str>,
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
        if let Some(resource) = resource {
            query.append_pair("resource", resource);
        }
    }
    url
}

/// Extract the `resource_metadata` URL from a `401` `WWW-Authenticate` Bearer
/// challenge (RFC 9728 §5.1). Returns `None` when the header has no Bearer
/// challenge or it names no resource metadata, in which case the client falls
/// back to the well-known paths under the resource origin.
#[must_use]
pub fn resource_metadata_from_challenge(header: &str) -> Option<String> {
    bearer_challenge_param(header, "resource_metadata")
}

/// Extract the `scope` a `401` `WWW-Authenticate` Bearer challenge asks for
/// (RFC 6750 §3), split on whitespace. `None` when it names none.
#[must_use]
pub fn scope_from_challenge(header: &str) -> Option<Vec<String>> {
    let scope = bearer_challenge_param(header, "scope")?;
    let scopes: Vec<String> = scope.split_whitespace().map(str::to_string).collect();
    (!scopes.is_empty()).then_some(scopes)
}

/// One auth-param of the first Bearer challenge in a `WWW-Authenticate`
/// value (RFC 9110 §11.6.1).
///
/// The value can hold several challenges (`Basic realm="a", Bearer
/// resource_metadata="…"`), and a quoted value can hold commas and escaped
/// quotes, so this splits on commas outside quotes. An item that starts with
/// a scheme and a space opens a new challenge; any other item is a parameter
/// of the challenge before it.
fn bearer_challenge_param(header: &str, name: &str) -> Option<String> {
    let mut in_bearer = false;
    for item in split_outside_quotes(header) {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let param = match item.split_once(char::is_whitespace) {
            Some((scheme, rest))
                if !scheme.contains('=') && !rest.trim_start().starts_with('=') =>
            {
                in_bearer = scheme.eq_ignore_ascii_case("bearer");
                rest.trim()
            }
            _ if !item.contains('=') => {
                // A bare scheme with no parameters.
                in_bearer = item.eq_ignore_ascii_case("bearer");
                continue;
            }
            _ => item,
        };
        if !in_bearer {
            continue;
        }
        let Some((key, value)) = param.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case(name) {
            return Some(unquote(value.trim()));
        }
    }
    None
}

/// Split on commas that are not inside a quoted string.
fn split_outside_quotes(header: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in header.char_indices() {
        match character {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ',' if !quoted => {
                items.push(&header[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    items.push(&header[start..]);
    items
}

/// A token or quoted-string value, with the quotes and escapes removed.
fn unquote(value: &str) -> String {
    let Some(inner) = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    else {
        return value.to_string();
    };
    let mut out = String::with_capacity(inner.len());
    let mut escaped = false;
    for character in inner.chars() {
        if escaped || character != '\\' {
            out.push(character);
            escaped = false;
        } else {
            escaped = true;
        }
    }
    out
}

/// Why an OAuth endpoint was not admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// Not `https`, or an address on the denied-network list: never allowed.
    Refused,
    /// The name did not resolve. Temporary: it may resolve on a later try.
    Unresolved,
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
/// The production client also resolves every name through
/// [`AdmittedResolver`], which applies the same check at connect time, so a
/// name that resolves differently after this check still cannot reach a
/// denied address.
///
/// [`admit_plugin_endpoint`]: crate::mcp_config
pub async fn admit_oauth_endpoint(url: &url::Url) -> Result<()> {
    check_endpoint(url).await.map_err(|refusal| match refusal {
        Refusal::Refused => AgentError::config("MCP OAuth endpoint is not an allowed destination"),
        Refusal::Unresolved => AgentError::config("MCP OAuth endpoint host could not be resolved"),
    })
}

async fn check_endpoint(url: &url::Url) -> std::result::Result<(), Refusal> {
    if url.scheme() != "https" {
        // Token-bearing traffic must be TLS; a loopback dev server is not a
        // valid OAuth authorization server for a remote resource.
        return Err(Refusal::Refused);
    }
    let host = url.host_str().ok_or(Refusal::Refused)?;
    let port = url.port_or_known_default().ok_or(Refusal::Refused)?;
    let addresses = admitted_addresses(host, port).await?;
    if addresses.is_empty() {
        return Err(Refusal::Unresolved);
    }
    Ok(())
}

/// Resolve `host` and return its addresses, refusing the whole answer when
/// any address is on the denied-network list.
async fn admitted_addresses(
    host: &str,
    port: u16,
) -> std::result::Result<Vec<std::net::SocketAddr>, Refusal> {
    use crate::web_search::admit_fetch_address;

    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let addresses: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| Refusal::Unresolved)?
        .collect();
    if addresses
        .iter()
        .any(|address| admit_fetch_address(address.ip()).is_err())
    {
        return Err(Refusal::Refused);
    }
    Ok(addresses)
}

/// The DNS resolver of the production OAuth client: it resolves each name and
/// hands the connector only addresses that clear the denied-network list, so
/// the address a request connects to is the one that was checked.
struct AdmittedResolver;

impl reqwest::dns::Resolve for AdmittedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addresses = admitted_addresses(&host, 0).await.map_err(|refusal| {
                Box::new(std::io::Error::other(match refusal {
                    Refusal::Refused => "the OAuth endpoint is not an allowed destination",
                    Refusal::Unresolved => "the OAuth endpoint host did not resolve",
                })) as Box<dyn std::error::Error + Send + Sync>
            })?;
            if addresses.is_empty() {
                return Err(Box::new(std::io::Error::other(
                    "the OAuth endpoint host resolved to no addresses",
                ))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
        })
    }
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
    /// RFC 8707 resource indicator, the same one the authorize request sent.
    pub resource: Option<&'a str>,
}

/// What one discovery document request came back with.
enum Fetched<T> {
    Found(T),
    /// A definite answer that there is no usable document: a `4xx`, a body
    /// that is not the JSON expected, or one larger than Tidebreak reads.
    Missing,
    /// No answer to go on: a timeout, a network failure, a `5xx`, `408`, or
    /// `429`. Temporary.
    Unavailable,
}

/// Whether an HTTP status means "try again later" rather than "no".
fn is_temporary(status: reqwest::StatusCode) -> bool {
    status.is_server_error()
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
}

/// Read a response body, refusing one larger than [`MAX_RESPONSE_BYTES`]
/// instead of reading it to the end. `None` for a body that is too large or
/// that failed mid-read.
async fn read_capped(mut response: reqwest::Response) -> Option<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return None;
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some(body)
}

/// The network client for the OAuth flows. Holds a redirect-refusing reqwest
/// client so a token endpoint cannot bounce the exchange to a third party.
#[derive(Clone)]
pub struct McpOAuthClient {
    http: reqwest::Client,
    /// Test builds can admit plain-`http` loopback endpoints, so a local fake
    /// authorization server can stand in for a real one. No production build
    /// has this switch.
    #[cfg(test)]
    admit_loopback: bool,
}

impl McpOAuthClient {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .dns_resolver(AdmittedResolver)
            .build()
            .map_err(|error| {
                AgentError::config(format!(
                    "could not build the MCP OAuth HTTP client: {error}"
                ))
            })?;
        Ok(Self {
            http,
            #[cfg(test)]
            admit_loopback: false,
        })
    }

    /// A client that also admits `http` endpoints on a literal IPv4 loopback
    /// address, for tests that stand up a fake authorization server.
    #[cfg(test)]
    pub fn admitting_loopback_for_tests() -> Result<Self> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .no_proxy()
            .build()
            .map_err(|error| {
                AgentError::config(format!(
                    "could not build the MCP OAuth HTTP client: {error}"
                ))
            })?;
        Ok(Self {
            http,
            admit_loopback: true,
        })
    }

    /// [`check_endpoint`], plus the test-only loopback allowance.
    async fn admit(&self, url: &url::Url) -> std::result::Result<(), Refusal> {
        #[cfg(test)]
        if self.admit_loopback
            && url.scheme() == "http"
            && matches!(url.host(), Some(url::Host::Ipv4(address)) if address.is_loopback())
        {
            return Ok(());
        }
        check_endpoint(url).await
    }

    /// Parse and admit one URL a server's metadata named.
    async fn endpoint(&self, value: &str) -> std::result::Result<url::Url, AuthorizationProblem> {
        let url = parse_oauth_url(value)
            .map_err(|_| AuthorizationProblem::Unsupported(OAuthUnsupported::UnreadableMetadata))?;
        self.admit(&url).await.map_err(AuthorizationProblem::from)?;
        Ok(url)
    }

    /// Learn how to sign in to the MCP server at `resource`, following the
    /// MCP authorization specification.
    ///
    /// The protected-resource metadata (RFC 9728) comes from the URL a `401`
    /// challenge names, then from the well-known paths under the server's
    /// origin. It must describe this server (RFC 9728 §3.3). It names
    /// authorization servers; the first one's metadata comes from its RFC 8414
    /// or OpenID Connect discovery document, and must name the same issuer
    /// (RFC 8414 §3.3). Every URL, including the ones the server names, clears
    /// [`admit_oauth_endpoint`] before it is fetched.
    ///
    /// Returns `None` when the server publishes no protected-resource metadata
    /// that names an authorization server: then it is not asking for an OAuth
    /// sign-in Tidebreak can recognize. A server whose metadata did not answer
    /// is `None` too, unless its challenge named metadata, which makes it
    /// [`Discovery::Unavailable`].
    pub async fn discover(
        &self,
        resource: &url::Url,
        challenge: Option<&str>,
    ) -> Option<Discovery> {
        let hint = challenge.and_then(resource_metadata_from_challenge);
        let mut unavailable = false;
        let mut resource_metadata = None;
        for candidate in protected_resource_metadata_urls(resource, hint.as_deref()) {
            match self.admit(&candidate).await {
                Ok(()) => {}
                Err(Refusal::Refused) => continue,
                Err(Refusal::Unresolved) => {
                    unavailable = true;
                    continue;
                }
            }
            match self.get_json::<ProtectedResourceMetadata>(&candidate).await {
                Fetched::Found(metadata) if !metadata.authorization_servers.is_empty() => {
                    resource_metadata = Some(metadata);
                    break;
                }
                Fetched::Found(_) | Fetched::Missing => {}
                Fetched::Unavailable => unavailable = true,
            }
        }
        let Some(resource_metadata) = resource_metadata else {
            return (unavailable && hint.is_some()).then_some(Discovery::Unavailable);
        };
        if let Some(declared) = resource_metadata.resource.as_deref() {
            if !resource_matches(resource, declared) {
                return Some(Discovery::Unsupported(OAuthUnsupported::ResourceMismatch));
            }
        }
        Some(
            match self
                .discover_authorization(resource, &resource_metadata, challenge)
                .await
            {
                Ok(discovered) => Discovery::Supported(Box::new(discovered)),
                Err(AuthorizationProblem::Unsupported(reason)) => Discovery::Unsupported(reason),
                Err(AuthorizationProblem::Unavailable) => Discovery::Unavailable,
            },
        )
    }

    async fn discover_authorization(
        &self,
        resource: &url::Url,
        resource_metadata: &ProtectedResourceMetadata,
        challenge: Option<&str>,
    ) -> std::result::Result<DiscoveredAuthorization, AuthorizationProblem> {
        let issuer = resource_metadata.authorization_servers.first().ok_or(
            AuthorizationProblem::Unsupported(OAuthUnsupported::UnreadableMetadata),
        )?;
        let issuer = self.endpoint(issuer).await?;
        let mut unavailable = false;
        let mut mismatched = false;
        let mut metadata = None;
        for candidate in authorization_server_metadata_urls(&issuer) {
            match self
                .get_json::<AuthorizationServerMetadata>(&candidate)
                .await
            {
                Fetched::Found(found) if issuer_matches(&issuer, &found.issuer) => {
                    metadata = Some(found);
                    break;
                }
                // RFC 8414 §3.3: metadata naming another issuer must not be
                // used. Keep looking; another location may be right.
                Fetched::Found(_) => mismatched = true,
                Fetched::Missing => {}
                Fetched::Unavailable => unavailable = true,
            }
        }
        let metadata = match metadata {
            Some(metadata) => metadata,
            None if unavailable => return Err(AuthorizationProblem::Unavailable),
            None if mismatched => {
                return Err(AuthorizationProblem::Unsupported(
                    OAuthUnsupported::IssuerMismatch,
                ))
            }
            None => {
                return Err(AuthorizationProblem::Unsupported(
                    OAuthUnsupported::UnreadableMetadata,
                ))
            }
        };
        // Tidebreak signs in with PKCE S256 only. A server that lists its
        // methods must list that one; one that lists none is asked anyway.
        if !metadata.code_challenge_methods_supported.is_empty()
            && !metadata
                .code_challenge_methods_supported
                .iter()
                .any(|method| method == "S256")
        {
            return Err(AuthorizationProblem::Unsupported(OAuthUnsupported::NoS256));
        }
        let authorization_endpoint = self.endpoint(&metadata.authorization_endpoint).await?;
        let token_endpoint = self.endpoint(&metadata.token_endpoint).await?;
        let registration_endpoint =
            metadata
                .registration_endpoint
                .as_deref()
                .ok_or(AuthorizationProblem::Unsupported(
                    OAuthUnsupported::NoClientRegistration,
                ))?;
        let registration_endpoint = self.endpoint(registration_endpoint).await?;
        // Revocation is optional: an endpoint Tidebreak cannot use is left out
        // rather than failing the sign-in.
        let revocation_endpoint = match metadata.revocation_endpoint.as_deref() {
            Some(value) => self.endpoint(value).await.ok(),
            None => None,
        };
        let scopes = challenge
            .and_then(scope_from_challenge)
            .unwrap_or_else(|| resource_metadata.scopes_supported.clone());
        let sign_in_host = authorization_endpoint
            .host_str()
            .unwrap_or_default()
            .to_string();
        Ok(DiscoveredAuthorization {
            authorization_endpoint,
            token_endpoint,
            registration_endpoint,
            revocation_endpoint,
            scopes,
            resource: resource_indicator(resource, resource_metadata.resource.as_deref()),
            sign_in_host,
        })
    }

    /// RFC 7591 dynamic client registration. Register a public client for the
    /// loopback `redirect_uri` and return the assigned id. The endpoint is
    /// admitted before the request.
    pub async fn register(
        &self,
        registration_endpoint: &url::Url,
        redirect_uri: &str,
    ) -> std::result::Result<ClientRegistration, SignInFailure> {
        self.admit(registration_endpoint)
            .await
            .map_err(|_| SignInFailure::Failed)?;
        let body = serde_json::json!({
            "redirect_uris": [redirect_uri],
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "client_name": "Tidebreak",
        });
        let response = self
            .http
            .post(registration_endpoint.clone())
            .json(&body)
            .send()
            .await
            .map_err(|_| SignInFailure::Failed)?;
        let status = response.status();
        if status.is_client_error() && !is_temporary(status) {
            return Err(SignInFailure::RegistrationRefused);
        }
        if !status.is_success() {
            return Err(SignInFailure::Failed);
        }
        let body = read_capped(response).await.ok_or(SignInFailure::Failed)?;
        serde_json::from_slice(&body).map_err(|_| SignInFailure::Failed)
    }

    /// Exchange an authorization code for tokens (RFC 6749 §4.1.3 + PKCE
    /// verifier). The endpoint is admitted before the request.
    pub async fn exchange_code(
        &self,
        token_endpoint: &url::Url,
        exchange: CodeExchange<'_>,
    ) -> Result<McpOAuthCredentials> {
        self.admit_token_endpoint(token_endpoint).await?;
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", exchange.code),
            ("redirect_uri", exchange.redirect_uri),
            ("code_verifier", exchange.verifier),
            ("client_id", exchange.client_id),
        ];
        if let Some(secret) = exchange.client_secret {
            form.push(("client_secret", secret));
        }
        if let Some(resource) = exchange.resource {
            form.push(("resource", resource));
        }
        let token = self.post_token(token_endpoint, &form).await?;
        credentials_from_token(token, None)
    }

    /// Refresh an access token (RFC 6749 §6). A rotated refresh token in the
    /// response replaces the stored one; its absence keeps the old one. The
    /// endpoint is admitted before the request.
    ///
    /// Fails with [`AgentError::SignInRequired`] only when the token endpoint
    /// refuses the refresh token. Any other failure is temporary
    /// ([`SIGN_IN_SERVICE_UNAVAILABLE`]): the session stays and the caller
    /// tries again later.
    pub async fn refresh(
        &self,
        token_endpoint: &url::Url,
        client_id: &str,
        client_secret: Option<&str>,
        refresh_token: &str,
        resource: Option<&str>,
    ) -> Result<McpOAuthCredentials> {
        self.admit_token_endpoint(token_endpoint).await?;
        let mut form = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ];
        if let Some(secret) = client_secret {
            form.push(("client_secret", secret));
        }
        if let Some(resource) = resource {
            form.push(("resource", resource));
        }
        let token = self.post_token(token_endpoint, &form).await?;
        credentials_from_token(token, Some(refresh_token))
    }

    /// Admit a token endpoint. A refusal is permanent; a name that did not
    /// resolve is temporary, like a token service that did not answer.
    async fn admit_token_endpoint(&self, token_endpoint: &url::Url) -> Result<()> {
        self.admit(token_endpoint)
            .await
            .map_err(|refusal| match refusal {
                Refusal::Refused => {
                    AgentError::config("MCP OAuth endpoint is not an allowed destination")
                }
                Refusal::Unresolved => AgentError::config(SIGN_IN_SERVICE_UNAVAILABLE),
            })
    }

    /// One discovery document.
    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &url::Url) -> Fetched<T> {
        let Ok(response) = self
            .http
            .get(url.clone())
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
        else {
            return Fetched::Unavailable;
        };
        let status = response.status();
        if is_temporary(status) {
            return Fetched::Unavailable;
        }
        if !status.is_success() {
            return Fetched::Missing;
        }
        match read_capped(response).await {
            Some(body) => serde_json::from_slice(&body).map_or(Fetched::Missing, Fetched::Found),
            None => Fetched::Missing,
        }
    }

    /// Post to the token endpoint. A refused grant (`400`, `401`, and the
    /// other permanent `4xx`) is [`AgentError::SignInRequired`]; anything
    /// else that is not a readable token is [`SIGN_IN_SERVICE_UNAVAILABLE`].
    async fn post_token(
        &self,
        token_endpoint: &url::Url,
        form: &[(&str, &str)],
    ) -> Result<TokenResponse> {
        let unavailable = || AgentError::config(SIGN_IN_SERVICE_UNAVAILABLE);
        let response = self
            .http
            .post(token_endpoint.clone())
            .form(form)
            .send()
            .await
            .map_err(|_| unavailable())?;
        let status = response.status();
        if status.is_client_error() && !is_temporary(status) {
            return Err(AgentError::SignInRequired(
                "the MCP OAuth session is no longer valid".to_string(),
            ));
        }
        if !status.is_success() {
            return Err(unavailable());
        }
        let body = read_capped(response).await.ok_or_else(unavailable)?;
        serde_json::from_slice(&body).map_err(|_| unavailable())
    }
}

/// Why the authorization server half of discovery stopped.
enum AuthorizationProblem {
    Unsupported(OAuthUnsupported),
    Unavailable,
}

impl From<Refusal> for AuthorizationProblem {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Refused => Self::Unsupported(OAuthUnsupported::RefusedEndpoint),
            Refusal::Unresolved => Self::Unavailable,
        }
    }
}

fn parse_oauth_url(value: &str) -> Result<url::Url> {
    url::Url::parse(value).map_err(|_| AgentError::config("MCP OAuth endpoint is not a valid URL"))
}

/// `url` with its path replaced and its query and fragment removed.
fn with_path(url: &url::Url, path: &str) -> url::Url {
    let mut url = url.clone();
    url.set_path(path);
    url.set_query(None);
    url.set_fragment(None);
    url
}

/// Where to look for a server's protected-resource metadata, in order: the
/// URL its challenge named, the well-known path with the server's own path
/// inserted after it (RFC 9728 §3.1), and the well-known path at the root.
fn protected_resource_metadata_urls(resource: &url::Url, hint: Option<&str>) -> Vec<url::Url> {
    let mut urls = Vec::new();
    if let Some(hint) = hint.and_then(|hint| url::Url::parse(hint).ok()) {
        urls.push(hint);
    }
    let path = resource.path().trim_end_matches('/');
    if !path.is_empty() {
        urls.push(with_path(
            resource,
            &format!("/.well-known/oauth-protected-resource{path}"),
        ));
    }
    let root = with_path(resource, "/.well-known/oauth-protected-resource");
    if !urls.contains(&root) {
        urls.push(root);
    }
    urls
}

/// Where to look for an authorization server's metadata, in order. For an
/// issuer with a path: RFC 8414 and OpenID Connect with the path inserted
/// after the well-known segment, OpenID Connect with it appended, and the
/// RFC 8414 document appended, which older servers publish. For an issuer
/// without a path: the RFC 8414 and OpenID Connect documents at the root.
fn authorization_server_metadata_urls(issuer: &url::Url) -> Vec<url::Url> {
    let path = issuer.path().trim_end_matches('/');
    if path.is_empty() {
        return vec![
            with_path(issuer, "/.well-known/oauth-authorization-server"),
            with_path(issuer, "/.well-known/openid-configuration"),
        ];
    }
    vec![
        with_path(
            issuer,
            &format!("/.well-known/oauth-authorization-server{path}"),
        ),
        with_path(issuer, &format!("/.well-known/openid-configuration{path}")),
        with_path(issuer, &format!("{path}/.well-known/openid-configuration")),
        with_path(
            issuer,
            &format!("{path}/.well-known/oauth-authorization-server"),
        ),
    ]
}

/// A URL's path with one trailing slash, for prefix comparisons at a segment
/// boundary.
fn slashed_path(url: &url::Url) -> String {
    let path = url.path();
    if path.ends_with('/') {
        path.to_string()
    } else {
        format!("{path}/")
    }
}

/// Whether protected-resource metadata that declares `declared` describes the
/// server at `server` (RFC 9728 §3.3). It must share the server's origin, and
/// its path must be the server's path or a parent of it — the same rule the
/// MCP reference client applies, which accepts `https://mcp.example.com/` for
/// a server at `https://mcp.example.com/mcp`.
fn resource_matches(server: &url::Url, declared: &str) -> bool {
    let Ok(declared) = url::Url::parse(declared) else {
        return false;
    };
    declared.origin() == server.origin()
        && slashed_path(server).starts_with(&slashed_path(&declared))
}

/// Whether authorization-server metadata that names `declared` as its issuer
/// is the metadata for `issuer`, the identifier it was fetched for (RFC 8414
/// §3.3). The two must be the same URL; a trailing slash is not a difference.
fn issuer_matches(issuer: &url::Url, declared: &str) -> bool {
    let Ok(declared) = url::Url::parse(declared) else {
        return false;
    };
    declared.origin() == issuer.origin()
        && declared.path().trim_end_matches('/') == issuer.path().trim_end_matches('/')
        && declared.query() == issuer.query()
}

/// The RFC 8707 resource indicator to send for the server at `resource`: the
/// identifier its protected-resource metadata declares, which
/// [`resource_matches`] has tied to this server, or the server URL without a
/// fragment when the metadata declares none.
fn resource_indicator(resource: &url::Url, declared: Option<&str>) -> String {
    if let Some(declared) = declared.and_then(|value| url::Url::parse(value).ok()) {
        return declared.to_string();
    }
    let mut resource = resource.clone();
    resource.set_fragment(None);
    resource.to_string()
}

fn credentials_from_token(
    token: TokenResponse,
    previous_refresh: Option<&str>,
) -> Result<McpOAuthCredentials> {
    if token.access_token.is_empty() {
        return Err(AgentError::config(
            "MCP OAuth token response did not include an access token",
        ));
    }
    let refresh_token = token
        .refresh_token
        .filter(|value| !value.is_empty())
        .or_else(|| previous_refresh.map(str::to_string));
    let ttl = token
        .expires_in
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_ACCESS_TTL_SECONDS);
    Ok(McpOAuthCredentials {
        access_token: token.access_token,
        refresh_token,
        expires_at_unix: unix_time().saturating_add(ttl),
        scope: token.scope.filter(|value| !value.is_empty()),
    })
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
}

// ---------------------------------------------------------------------------
// Pending sign-in (loopback listener) + live connection
// ---------------------------------------------------------------------------

/// Bind an ephemeral loopback listener (IPv4, plus IPv6 on the same port when
/// available) and return it with the `redirect_uri` that must be registered
/// and sent on the authorize request.
pub async fn bind_mcp_loopback() -> Result<(LoopbackListeners, String)> {
    let v4 = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|_| AgentError::config("could not bind a loopback port for MCP OAuth"))?;
    let port = v4
        .local_addr()
        .map_err(|_| AgentError::config("could not read the MCP OAuth loopback port"))?
        .port();
    let v6 = match TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, port)).await {
        Ok(listener) => Some(listener),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::Unsupported
            ) =>
        {
            None
        }
        Err(_) => None,
    };
    let redirect_uri = format!("http://127.0.0.1:{port}/auth/callback");
    Ok((LoopbackListeners { v4, v6 }, redirect_uri))
}

/// The loopback callback port, held on both families the browser might use.
pub struct LoopbackListeners {
    v4: TcpListener,
    v6: Option<TcpListener>,
}

impl LoopbackListeners {
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

/// A browser authorization in flight: an ephemeral loopback listener is bound
/// (RFC 8252 §7.3), the person's browser has the authorize URL, and
/// [`finish`](Self::finish) awaits the redirect and completes the exchange.
pub struct PendingMcpSignIn {
    /// The browser URL the desktop opens. Never contains a token.
    pub authorization_url: url::Url,
    /// The loopback redirect the listener is bound to, echoed into the
    /// authorize request and the token exchange so they match.
    pub redirect_uri: String,
    pub(crate) listeners: LoopbackListeners,
    pub(crate) verifier: String,
    pub(crate) state: String,
    pub(crate) token_endpoint: url::Url,
    /// The client registered for this sign-in, with the token endpoint,
    /// scopes, and resource indicator it uses. It is stored only once the
    /// sign-in succeeds, so an abandoned sign-in never replaces the
    /// registration a working session refreshes with.
    pub(crate) registration: ClientRegistration,
}

impl PendingMcpSignIn {
    /// Await the loopback redirect, validate `state`, and exchange the code for
    /// tokens. Times out after [`SIGN_IN_TIMEOUT`]. On success the
    /// registration and tokens are the caller's to persist through
    /// [`McpOAuthCredentialVault`].
    pub async fn finish(
        self,
        client: &McpOAuthClient,
    ) -> std::result::Result<(ClientRegistration, McpOAuthCredentials), SignInFailure> {
        let Self {
            listeners,
            verifier,
            state,
            token_endpoint,
            registration,
            redirect_uri,
            ..
        } = self;

        let (sender, mut receiver) = mpsc::channel::<CallbackResult>(1);
        let callback_app = Router::new()
            .route("/auth/callback", get(callback))
            .with_state(CallbackState {
                expected_state: state,
                sender,
            });
        let outcome = tokio::select! {
            outcome = tokio::time::timeout(SIGN_IN_TIMEOUT, receiver.recv()) => outcome,
            () = listeners.serve(callback_app) => Ok(None),
        };

        let callback = match outcome {
            Ok(Some(callback)) => callback,
            Ok(None) => return Err(SignInFailure::Failed),
            Err(_) => return Err(SignInFailure::TimedOut),
        };
        if callback.error.is_some() {
            return Err(SignInFailure::Denied);
        }
        let code = callback.code.ok_or(SignInFailure::Failed)?;

        let credentials = client
            .exchange_code(
                &token_endpoint,
                CodeExchange {
                    client_id: &registration.client_id,
                    client_secret: registration.client_secret.as_deref(),
                    code: &code,
                    redirect_uri: &redirect_uri,
                    verifier: &verifier,
                    resource: registration.resource.as_deref(),
                },
            )
            .await
            .map_err(|error| {
                if is_oauth_sign_in_required(&error) {
                    SignInFailure::ExchangeRefused
                } else {
                    SignInFailure::Failed
                }
            })?;
        Ok((registration, credentials))
    }
}

async fn callback(
    State(state): State<CallbackState>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    if query.state.as_deref() != Some(state.expected_state.as_str()) {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Html(super::callback_page::callback_page(
                super::callback_page::CallbackOutcome::Failed,
                "Sign-in failed",
                "The authorization state did not match. Return to Tidebreak and try again.",
            )),
        )
            .into_response();
    }
    let success = query.code.is_some() && query.error.is_none();
    let _ = state.sender.try_send(CallbackResult {
        code: query.code,
        error: query.error,
    });
    let (outcome, heading, message) = if success {
        (
            super::callback_page::CallbackOutcome::Success,
            "You're signed in",
            "",
        )
    } else {
        (
            super::callback_page::CallbackOutcome::Denied,
            "Sign-in was denied",
            "Nothing was connected. Return to Tidebreak for details, or try again.",
        )
    };
    (
        axum::http::StatusCode::OK,
        Html(super::callback_page::callback_page(
            outcome, heading, message,
        )),
    )
        .into_response()
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

/// A live, refreshing OAuth session for one connected server. The runtime holds
/// one and hands its [`access_token`](Self::access_token) to the per-call
/// bearer. Refresh is serialized under `token_motion` so concurrent calls make
/// at most one refresh.
pub struct McpOAuthConnection {
    client: McpOAuthClient,
    vault: McpOAuthCredentialVault,
    token_endpoint: url::Url,
    client_id: String,
    client_secret: Option<String>,
    resource: Option<String>,
    token_motion: tokio::sync::Mutex<()>,
}

impl McpOAuthConnection {
    /// A refreshing session for the client `registration` describes, whose
    /// token endpoint the caller has already parsed.
    #[must_use]
    pub fn new(
        client: McpOAuthClient,
        vault: McpOAuthCredentialVault,
        token_endpoint: url::Url,
        registration: ClientRegistration,
    ) -> Self {
        Self {
            client,
            vault,
            token_endpoint,
            client_id: registration.client_id,
            client_secret: registration.client_secret,
            resource: registration.resource,
            token_motion: tokio::sync::Mutex::new(()),
        }
    }

    /// Return a currently-valid access token, refreshing under `token_motion`
    /// when the stored one is within the expiry leeway. Fails with
    /// [`AgentError::SignInRequired`] when there is no stored session or the
    /// refresh token is rejected, which is the signal the Connect flow must run
    /// again.
    pub async fn access_token(&self) -> Result<String> {
        let loaded = self.vault.load().await?;
        let Some(credentials) = loaded else {
            return Err(AgentError::SignInRequired(
                "no MCP OAuth session is stored".to_string(),
            ));
        };
        if credentials.access_is_fresh() {
            return Ok(credentials.access_token);
        }
        let _guard = self.token_motion.lock().await;
        let Some(credentials) = self.vault.load().await? else {
            return Err(AgentError::SignInRequired(
                "no MCP OAuth session is stored".to_string(),
            ));
        };
        if credentials.access_is_fresh() {
            return Ok(credentials.access_token);
        }
        let Some(refresh_token) = credentials.refresh_token.as_deref() else {
            return Err(AgentError::SignInRequired(
                "the MCP OAuth session has expired".to_string(),
            ));
        };
        let refreshed = self
            .client
            .refresh(
                &self.token_endpoint,
                &self.client_id,
                self.client_secret.as_deref(),
                refresh_token,
                self.resource.as_deref(),
            )
            .await?;
        self.vault.save(&refreshed).await?;
        Ok(refreshed.access_token)
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
            None,
        );
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs.get("response_type").map(String::as_str), Some("code"));
        assert_eq!(
            pairs.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(
            pairs.get("client_id").map(String::as_str),
            Some("client-123")
        );
        assert_eq!(
            pairs.get("code_challenge").map(String::as_str),
            Some("challenge-abc")
        );
        assert_eq!(pairs.get("state").map(String::as_str), Some("state-xyz"));
        assert!(!pairs.contains_key("scope"), "empty scope must be omitted");

        let scoped = build_authorize_url(
            &endpoint,
            "client-123",
            "http://127.0.0.1:52345/callback",
            "challenge-abc",
            "state-xyz",
            &["mcp".to_string(), "profile".to_string()],
            Some("https://mcp.example.test/"),
        );
        let scoped_pairs: std::collections::HashMap<_, _> =
            scoped.query_pairs().into_owned().collect();
        assert_eq!(
            scoped_pairs.get("scope").map(String::as_str),
            Some("mcp profile")
        );
        assert_eq!(
            scoped_pairs.get("resource").map(String::as_str),
            Some("https://mcp.example.test/")
        );
        assert!(!pairs.contains_key("resource"));
    }

    #[test]
    fn resource_metadata_parsed_from_www_authenticate() {
        let header = r#"Bearer realm="mcp", resource_metadata="https://api.example/.well-known/oauth-protected-resource""#;
        assert_eq!(
            resource_metadata_from_challenge(header).as_deref(),
            Some("https://api.example/.well-known/oauth-protected-resource")
        );
        assert_eq!(resource_metadata_from_challenge("Basic realm=x"), None);
        assert_eq!(
            resource_metadata_from_challenge("Bearer realm=\"mcp\""),
            None
        );
    }

    /// The challenge Vercel's MCP server sends, verbatim, and the shapes
    /// RFC 9110 allows around it: several challenges in one value, commas and
    /// escaped quotes inside quoted strings, spaces around `=`, and a bare
    /// scheme with no parameters.
    #[test]
    fn challenge_parameters_survive_real_header_shapes() {
        let vercel = r#"Bearer error="invalid_token", error_description="No authorization provided", resource_metadata="https://mcp.vercel.com/.well-known/oauth-protected-resource""#;
        assert_eq!(
            resource_metadata_from_challenge(vercel).as_deref(),
            Some("https://mcp.vercel.com/.well-known/oauth-protected-resource")
        );

        let several = r#"Basic realm="a, b", Negotiate, Bearer error_description="say \"hi\", then go", resource_metadata = "https://mcp.example.test/meta", scope="files:read files:write""#;
        assert_eq!(
            resource_metadata_from_challenge(several).as_deref(),
            Some("https://mcp.example.test/meta")
        );
        assert_eq!(
            scope_from_challenge(several),
            Some(vec!["files:read".to_string(), "files:write".to_string()])
        );

        // A parameter of another scheme is never read as the Bearer one.
        let other = r#"Basic resource_metadata="https://evil.example/meta", Bearer realm="x""#;
        assert_eq!(resource_metadata_from_challenge(other), None);
        assert_eq!(scope_from_challenge(vercel), None);
    }

    #[test]
    fn metadata_is_looked_up_where_the_specification_says_in_order() {
        let resource = url::Url::parse("https://mcp.example.test/v1/mcp?x=1").unwrap();
        let urls: Vec<String> =
            protected_resource_metadata_urls(&resource, Some("https://mcp.example.test/meta"))
                .iter()
                .map(url::Url::to_string)
                .collect();
        assert_eq!(
            urls,
            [
                "https://mcp.example.test/meta",
                "https://mcp.example.test/.well-known/oauth-protected-resource/v1/mcp",
                "https://mcp.example.test/.well-known/oauth-protected-resource",
            ]
        );
        let root = url::Url::parse("https://mcp.vercel.com").unwrap();
        assert_eq!(
            protected_resource_metadata_urls(&root, None)
                .iter()
                .map(url::Url::to_string)
                .collect::<Vec<_>>(),
            ["https://mcp.vercel.com/.well-known/oauth-protected-resource"]
        );

        let issuer = url::Url::parse("https://auth.example.test/tenant1").unwrap();
        assert_eq!(
            authorization_server_metadata_urls(&issuer)
                .iter()
                .map(url::Url::to_string)
                .collect::<Vec<_>>(),
            [
                "https://auth.example.test/.well-known/oauth-authorization-server/tenant1",
                "https://auth.example.test/.well-known/openid-configuration/tenant1",
                "https://auth.example.test/tenant1/.well-known/openid-configuration",
                "https://auth.example.test/tenant1/.well-known/oauth-authorization-server",
            ]
        );
        let issuer = url::Url::parse("https://vercel.com").unwrap();
        assert_eq!(
            authorization_server_metadata_urls(&issuer)
                .iter()
                .map(url::Url::to_string)
                .collect::<Vec<_>>(),
            [
                "https://vercel.com/.well-known/oauth-authorization-server",
                "https://vercel.com/.well-known/openid-configuration",
            ]
        );
    }

    #[test]
    fn the_resource_indicator_is_the_servers_own_identifier() {
        let resource = url::Url::parse("https://mcp.vercel.com").unwrap();
        assert_eq!(
            resource_indicator(&resource, Some("https://mcp.vercel.com/")),
            "https://mcp.vercel.com/"
        );
        let with_fragment = url::Url::parse("https://mcp.example.test/mcp#top").unwrap();
        assert_eq!(
            resource_indicator(&with_fragment, None),
            "https://mcp.example.test/mcp"
        );
    }

    /// RFC 9728 §3.3: metadata that describes another server is not used.
    /// The server's own URL or a parent path on its origin matches, as the
    /// MCP reference client accepts.
    #[test]
    fn protected_resource_metadata_must_describe_this_server() {
        let server = url::Url::parse("https://mcp.example.test/v1/mcp").unwrap();
        for declared in [
            "https://mcp.example.test/v1/mcp",
            "https://mcp.example.test/v1/mcp/",
            "https://mcp.example.test/v1",
            "https://mcp.example.test/",
            "https://mcp.example.test",
        ] {
            assert!(resource_matches(&server, declared), "{declared}");
        }
        for declared in [
            "https://elsewhere.example/v1/mcp",
            "http://mcp.example.test/v1/mcp",
            "https://mcp.example.test:8443/v1/mcp",
            "https://mcp.example.test/v1/mcp/tools",
            "https://mcp.example.test/v1/mc",
            "https://mcp.example.test/v2",
            "not a url",
        ] {
            assert!(!resource_matches(&server, declared), "{declared}");
        }
        let vercel = url::Url::parse("https://mcp.vercel.com").unwrap();
        assert!(resource_matches(&vercel, "https://mcp.vercel.com/"));
    }

    /// RFC 8414 §3.3: metadata naming another issuer is not used. A trailing
    /// slash is not a difference.
    #[test]
    fn authorization_server_metadata_must_name_its_issuer() {
        let root = url::Url::parse("https://vercel.com").unwrap();
        assert!(issuer_matches(&root, "https://vercel.com"));
        assert!(issuer_matches(&root, "https://vercel.com/"));
        assert!(!issuer_matches(&root, "https://evil.example"));
        assert!(!issuer_matches(&root, "https://vercel.com/tenant"));
        let tenant = url::Url::parse("https://auth.example.test/tenant1").unwrap();
        assert!(issuer_matches(
            &tenant,
            "https://auth.example.test/tenant1/"
        ));
        assert!(!issuer_matches(
            &tenant,
            "https://auth.example.test/tenant2"
        ));
        assert!(!issuer_matches(&tenant, "https://auth.example.test"));
    }

    /// Discovery runs on every `401`, so a body larger than Tidebreak reads
    /// is refused rather than read to the end, whether or not the server
    /// declares its length.
    #[tokio::test]
    async fn response_bodies_are_capped() {
        use axum::body::Body;

        async fn serve(body: Body) -> String {
            let body = std::sync::Arc::new(std::sync::Mutex::new(Some(body)));
            let app = Router::new().route(
                "/meta",
                get(move || {
                    let body = body.clone();
                    async move { body.lock().unwrap().take().unwrap_or_else(Body::empty) }
                }),
            );
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            format!("http://{address}/meta")
        }

        let client = McpOAuthClient::admitting_loopback_for_tests().unwrap();
        let large = vec![b' '; MAX_RESPONSE_BYTES + 1];
        let declared = url::Url::parse(&serve(Body::from(large.clone())).await).unwrap();
        assert!(matches!(
            client.get_json::<serde_json::Value>(&declared).await,
            Fetched::Missing
        ));
        let chunks = futures::stream::iter(
            large
                .chunks(8 * 1024)
                .map(|chunk| Ok::<_, std::io::Error>(chunk.to_vec()))
                .collect::<Vec<_>>(),
        );
        let streamed = url::Url::parse(&serve(Body::from_stream(chunks)).await).unwrap();
        assert!(matches!(
            client.get_json::<serde_json::Value>(&streamed).await,
            Fetched::Missing
        ));
        let small = url::Url::parse(&serve(Body::from(r#"{"ok":true}"#)).await).unwrap();
        assert!(matches!(
            client.get_json::<serde_json::Value>(&small).await,
            Fetched::Found(_)
        ));
    }

    /// The production client resolves names through the admitting resolver,
    /// so a name that resolves to a denied address is refused at connect
    /// time too, not only when it was first checked.
    #[tokio::test]
    async fn the_resolver_refuses_names_that_resolve_to_denied_addresses() {
        use reqwest::dns::Resolve;

        let refused = AdmittedResolver
            .resolve("localhost".parse().unwrap())
            .await
            .err()
            .expect("localhost resolves to loopback, which is denied");
        assert!(refused.to_string().contains("not an allowed destination"));
    }

    /// The production client admits no loopback endpoint, so metadata a
    /// server controls can never aim a token request at this machine.
    #[tokio::test]
    async fn the_production_client_refuses_loopback_endpoints() {
        let client = McpOAuthClient::new().unwrap();
        let loopback = url::Url::parse("http://127.0.0.1:8080/token").unwrap();
        assert!(client.admit(&loopback).await.is_err());
        let tls_loopback = url::Url::parse("https://127.0.0.1:8443/token").unwrap();
        assert!(client.admit(&tls_loopback).await.is_err());
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

    #[test]
    fn missing_expires_in_uses_a_conservative_ttl() {
        let before = unix_time();
        let credentials = credentials_from_token(
            TokenResponse {
                access_token: "access".to_string(),
                refresh_token: Some("refresh".to_string()),
                expires_in: None,
                scope: None,
            },
            None,
        )
        .unwrap();
        let after = unix_time();
        assert!(
            credentials.expires_at_unix >= before.saturating_add(DEFAULT_ACCESS_TTL_SECONDS)
                && credentials.expires_at_unix <= after.saturating_add(DEFAULT_ACCESS_TTL_SECONDS)
        );
        assert_ne!(credentials.expires_at_unix, 0);
        assert!(credentials.expires_at_unix <= after.saturating_add(DEFAULT_ACCESS_TTL_SECONDS));
    }

    #[test]
    fn refresh_rotation_carries_a_new_refresh_token_forward() {
        let rotated = credentials_from_token(
            TokenResponse {
                access_token: "new-access".to_string(),
                refresh_token: Some("rotated-refresh".to_string()),
                expires_in: Some(3600),
                scope: None,
            },
            Some("old-refresh"),
        )
        .unwrap();
        assert_eq!(rotated.refresh_token.as_deref(), Some("rotated-refresh"));

        let kept = credentials_from_token(
            TokenResponse {
                access_token: "new-access".to_string(),
                refresh_token: None,
                expires_in: Some(3600),
                scope: None,
            },
            Some("old-refresh"),
        )
        .unwrap();
        assert_eq!(kept.refresh_token.as_deref(), Some("old-refresh"));
    }
}
