//! Bearer-token authentication for the API.
//!
//! Which credentials authenticate depends on the boot [`Profile`]:
//!
//! - **Desktop** binds to loopback with a per-launch token (see [`AppState`]);
//!   whoever presents it is the one person at the machine, so it resolves to
//!   [`Principal::LocalOwner`]. It's the one thing standing between the agent
//!   and any other local process that finds the port, so the check is
//!   mandatory on every non-health route.
//! - **Self-host** authenticates with live Model Gateway identity, with a
//!   standalone OpenID Connect provider, or with static named bearer tokens
//!   from an operator-maintained file ([`TokenMap`]). Each resolves to
//!   [`Principal::User`]. The per-launch token names nobody on a shared
//!   deployment and is not accepted there — a credential that names no one is
//!   rejected at this middleware (#853).
//!
//! # Self-host token file
//!
//! `TIDEBREAK_AUTH_TOKENS_FILE` points at a plain-text file, one mapping per
//! line — `<user-id> <token> [admin]`, whitespace-separated. Blank lines and
//! lines starting with `#` are ignored. A user may hold several tokens
//! (rotation); a token may name only one user, and duplicates fail the load.
//! Tokens are opaque secrets matched exactly (no hashing scheme to
//! misconfigure): at least 32 characters from `[A-Za-z0-9._~-]`, so they stay
//! valid in both the `Authorization` header and the WebSocket subprotocol
//! below. Generate them with e.g. `openssl rand -hex 32`. Gateway-derived
//! identity (#578) later replaces this file behind the same
//! credential-to-principal seam.
//!
//! The optional third field is the user's [`Role`]: `admin` puts them on the
//! deployment plane (configuration and shared secrets), and its absence makes
//! them a member. A user's lines must agree about the role, and a file naming
//! no admin at all fails to load — a deployment nobody can configure must not
//! start, for the same reason an empty file must not. See
//! `docs/decisions/0006-self-host-deployment-plane-authorization.md`.
//!
//! ```text
//! # user-id  token                                              role
//! alice  4f9c0e9b2d5a4c1e8f7b6a5d4c3b2a1f0e9d8c7b6a5f4e3d2c1b0a99  admin
//! bob    0123456789abcdef0123456789abcdef0123456789abcdef01234567
//! ```
//!
//! # Self-host OpenID Connect
//!
//! `TIDEBREAK_AUTH_OIDC_ISSUER`, `TIDEBREAK_AUTH_OIDC_CLIENT_ID`, and
//! `TIDEBREAK_AUTH_OIDC_CLIENT_SECRET` select a third exclusive mode
//! ([`OidcAuthenticator`]) that signs a browser in without a gateway
//! (`docs/decisions/0087-standalone-browser-sign-in.md`). Combining them with
//! `TIDEBREAK_AUTH_GATEWAY_URL` is a boot error; a token file may sit beside
//! them as the bootstrap for the first administrator and for CLI access,
//! because OIDC itself only ever names a member.
//!
//! The machine runs the authorization-code flow with PKCE itself
//! ([`oidc_start`] and [`oidc_callback`]), maps the login claim named by
//! `TIDEBREAK_AUTH_OIDC_CLAIM` (`sub` by default) to a [`UserId`], and mints
//! its own bearer for an hour. Every bearer it mints starts with
//! `tb_oidc_`, so a mode confusion is a refusal rather than a coincidence.
//!
//! Browsers can't set an `Authorization` header on a WebSocket upgrade, so on
//! upgrade requests the token is also accepted via `Sec-WebSocket-Protocol` as
//! `tidebreak-token.<token>` (alongside the handshake subprotocol `tidebreak-v1`).
//! Non-browser clients keep using `Authorization: Bearer`. Subprotocol auth is
//! ignored on ordinary HTTP requests.
//!
//! Operators should know that this second channel is noisier than the first:
//! `Sec-WebSocket-Protocol` is an ordinary request header that intermediary
//! proxies and load balancers log far more readily than `Authorization`, which
//! their default redaction lists usually cover. A self-host deployment behind
//! someone else's fronting infrastructure should assume the WebSocket token
//! can end up in that infrastructure's access logs, and rotate accordingly.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use axum::extract::{FromRequestParts, Query, Request, State};
use axum::http::{
    header::{
        AUTHORIZATION, CACHE_CONTROL, HOST, LOCATION, ORIGIN, REFERRER_POLICY,
        SEC_WEBSOCKET_PROTOCOL, UPGRADE,
    },
    request::Parts,
    HeaderMap, HeaderName, StatusCode,
};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::StreamExt as _;
use tidebreak_core::{AgentError, Profile, Result};

use crate::error::ServerError;
use crate::principal::{AuthContext, ClientExecutor, Principal, Role, UserId};
use crate::state::AppState;

/// Handshake subprotocol the server selects when the client offered it.
/// Clients that pass the token via subprotocol MUST offer this value alongside
/// [`WS_TOKEN_SUBPROTOCOL_PREFIX`] so the browser accepts the handshake.
pub const WS_HANDSHAKE_SUBPROTOCOL: &str = "tidebreak-v1";

/// Prefix for the subprotocol entry that carries the bearer token.
/// The per-launch token is a UUID (no commas/spaces), so it is a valid
/// subprotocol token-char sequence.
pub const WS_TOKEN_SUBPROTOCOL_PREFIX: &str = "tidebreak-token.";

/// Native-only credential header for claim, heartbeat, and resolve mutations.
pub const CLIENT_EXECUTOR_HEADER: HeaderName =
    HeaderName::from_static("x-tidebreak-client-executor");

/// Scoped credential for publishing caller-held bytes into one chat.
pub const LOCAL_IMPORT_HEADER: HeaderName = HeaderName::from_static("x-tidebreak-local-import");

/// Comma-separated adapter bootstrap bearers accepted by the pre-grant
/// connect route. Several values allow a zero-downtime rotation: add the new
/// value, move the adapter, then remove the old one.
pub const ADAPTER_BOOTSTRAP_TOKENS_ENV: &str = "TIDEBREAK_ADAPTER_BOOTSTRAP_TOKENS";

/// The narrow service credentials allowed to start an external connect flow.
///
/// This type deliberately implements neither `Debug` nor serialization. The
/// credentials live only in process memory and authorize no owner API or
/// post-connect adapter route.
pub struct AdapterBootstrapTokens {
    tokens: Vec<std::sync::Arc<str>>,
}

impl AdapterBootstrapTokens {
    /// Read and validate the optional token set once at boot.
    pub fn from_env() -> Result<Option<Self>> {
        Self::parse(std::env::var(ADAPTER_BOOTSTRAP_TOKENS_ENV).ok())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(token: &str) -> Self {
        Self::parse(Some(token.to_owned()))
            .expect("the adapter bootstrap test token is valid")
            .expect("the adapter bootstrap test token is present")
    }

    fn parse(value: Option<String>) -> Result<Option<Self>> {
        let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
            return Ok(None);
        };
        let mut tokens: Vec<std::sync::Arc<str>> = Vec::new();
        for raw in value.split(',') {
            let token = raw.trim();
            if !(32..=512).contains(&token.len()) {
                return Err(AgentError::config(format!(
                    "{ADAPTER_BOOTSTRAP_TOKENS_ENV} entries must be 32 to 512 characters"
                )));
            }
            if !token.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'~' | b'-')
            }) {
                return Err(AgentError::config(format!(
                    "{ADAPTER_BOOTSTRAP_TOKENS_ENV} entries must use only letters, digits, `.`, `_`, `~`, or `-`"
                )));
            }
            if tokens.iter().any(|existing| existing.as_ref() == token) {
                return Err(AgentError::config(format!(
                    "{ADAPTER_BOOTSTRAP_TOKENS_ENV} contains a duplicate token"
                )));
            }
            tokens.push(token.to_owned().into());
        }
        if tokens.len() > 4 {
            return Err(AgentError::config(format!(
                "{ADAPTER_BOOTSTRAP_TOKENS_ENV} accepts at most four rotation values"
            )));
        }
        Ok(Some(Self { tokens }))
    }

    fn accepts(&self, presented: &str) -> bool {
        let mut accepted = false;
        for token in &self.tokens {
            accepted |= constant_time_eq(token.as_bytes(), presented.as_bytes());
        }
        accepted
    }
}

/// Authenticates the adapter service before it can create a connect approval.
///
/// The adapter does not hold a grant yet, so this credential is distinct from
/// both owner bearers and the grant tokens minted at completion.
pub struct AdapterBootstrapAuth;

impl FromRequestParts<AppState> for AdapterBootstrapAuth {
    type Rejection = ServerError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> std::result::Result<Self, Self::Rejection> {
        let accepted = state
            .adapter_bootstrap_tokens
            .as_deref()
            .zip(extract_token(&parts.headers))
            .is_some_and(|(configured, presented)| configured.accepts(presented));
        if !accepted {
            return Err(ServerError::unauthorized(
                "the adapter bootstrap token is invalid",
            ));
        }
        Ok(Self)
    }
}

/// Public, non-secret authentication metadata for clients attaching to a
/// hosted machine. A native client uses it to discover that it should mint a
/// Gateway `tidebreak` resource token instead of asking a person to paste a
/// long-lived static bearer; a browser tab uses it to pick which of the three
/// sign-in screens it can offer (decision 0087).
///
/// The document is public and unauthenticated by design — a page has to read
/// it before it holds any credential — so it names modes and public URLs and
/// nothing else.
#[derive(serde::Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum AuthDiscovery {
    Gateway {
        gateway_url: String,
        resource: String,
    },
    StaticToken,
    Oidc {
        /// What the sign-in button names, taken from the issuer's host.
        issuer_name: String,
        /// Where the browser starts the authorization-code flow.
        start_url: String,
    },
    Local,
}

pub async fn discovery(State(state): State<AppState>) -> Json<AuthDiscovery> {
    let discovery = match (
        state.config.profile,
        state.principal_authenticator.as_ref(),
        state.config.auth_gateway_url.as_deref(),
    ) {
        (Profile::SelfHost, PrincipalAuthenticator::Gateway(gateway), Some(gateway_url)) => {
            AuthDiscovery::Gateway {
                gateway_url: gateway_url.trim_end_matches('/').to_owned(),
                resource: gateway.resource.clone(),
            }
        }
        (Profile::SelfHost, PrincipalAuthenticator::Oidc(oidc), _) => AuthDiscovery::Oidc {
            issuer_name: oidc.issuer_name(),
            start_url: oidc.start_url.clone(),
        },
        (Profile::SelfHost, PrincipalAuthenticator::Static(_), _) => AuthDiscovery::StaticToken,
        _ => AuthDiscovery::Local,
    };
    Json(discovery)
}

#[derive(serde::Deserialize)]
pub struct HandoffQuery {
    #[serde(default)]
    code: String,
    /// Which UI route should open once the page is signed in — the hash-router
    /// path a connect card or deep link opened before the console took over.
    /// Optional; the root when absent or refused.
    #[serde(default)]
    return_to: Option<String>,
}

/// A return route is a hash-router path: it starts with one slash, carries no
/// scheme or authority (so no `//host`), no nested fragment, and no control
/// bytes. Anything else lands on the root instead of refusing the sign-in —
/// the route is a convenience, and the bearer is the point.
fn handoff_return_route(raw: Option<&str>) -> &str {
    raw.filter(|route| {
        route.starts_with('/')
            && !route.starts_with("//")
            && !route.starts_with("/\\")
            && route.len() <= 4096
            && !route.contains('#')
            && !route.bytes().any(|byte| byte.is_ascii_control())
    })
    .unwrap_or("/")
}

/// The URL-safe alphabet both a handoff code and a bearer are drawn from.
fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'~')
}

/// A handoff code is the gateway's, prefixed so a stray bearer or session id
/// pasted here is refused before it reaches the network.
fn handoff_code_is_well_formed(code: &str) -> bool {
    code.starts_with("mg_ho_") && code.len() <= 256 && code.bytes().all(is_token_byte)
}

/// Where the landing page lives: the canonical public URL, so an ingress
/// path prefix survives. Absent (it never is on a gateway-authenticated
/// machine), the page's own root.
fn handoff_landing(state: &AppState) -> String {
    state
        .config
        .public_url
        .as_deref()
        .and_then(|raw| canonical_public_url(raw).ok())
        .unwrap_or_default()
}

/// A `302` to the landing page carrying a handoff envelope after `#`.
///
/// The envelope carries the bearer and optional hash-router route. A browser
/// never sends the fragment to a server, so it is in no access log, and the
/// page clears the bearer before the router sees it. `no-store` keeps the
/// redirect itself out of a cache that could replay it.
fn handoff_redirect(
    landing: &str,
    handoff_key: &str,
    handoff_value: &str,
    return_route: Option<&str>,
) -> Response {
    let mut fragment = url::form_urlencoded::Serializer::new(String::new());
    fragment.append_pair(handoff_key, handoff_value);
    if let Some(route) = return_route.filter(|route| *route != "/") {
        fragment.append_pair("return_to", route);
    }
    (
        StatusCode::FOUND,
        [
            (LOCATION, format!("{landing}/#{}", fragment.finish())),
            (CACHE_CONTROL, "no-store".to_owned()),
            (REFERRER_POLICY, "no-referrer".to_owned()),
        ],
    )
        .into_response()
}

/// Land a browser on this machine signed in.
///
/// The gateway console mints a one-time code and sends the reader here with
/// it. This route exchanges the code server to server for a bearer bound to
/// this machine and hands the bearer to the page in the URL fragment; the
/// page keeps it in memory and presents it like any other bearer. Nothing
/// is stored, no cookie is set, and the code is never logged.
///
/// A machine that does not authenticate through a gateway has no console to
/// take codes from, so the route does not exist there. A refused or
/// unusable exchange still lands on the page, with a reason the page can
/// word, rather than on a bare error a reader cannot act on.
pub async fn handoff(State(state): State<AppState>, Query(query): Query<HandoffQuery>) -> Response {
    let PrincipalAuthenticator::Gateway(gateway) = state.principal_authenticator.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let landing = handoff_landing(&state);
    let code = query.code.trim();
    if !handoff_code_is_well_formed(code) {
        return handoff_redirect(&landing, "handoff-failed", "invalid", None);
    }
    // A granted bearer lands where the reader was headed; a failure lands
    // on the root, where the sign-in screen words it.
    let return_route = handoff_return_route(query.return_to.as_deref());
    match gateway.redeem_handoff(code).await {
        HandoffOutcome::Granted(bearer) => {
            handoff_redirect(&landing, "handoff", &bearer, Some(return_route))
        }
        HandoffOutcome::Refused => handoff_redirect(&landing, "handoff-failed", "expired", None),
        HandoffOutcome::Unavailable => {
            handoff_redirect(&landing, "handoff-failed", "unavailable", None)
        }
    }
}

/// Reject requests whose bearer token does not resolve to a principal — from
/// `Authorization: Bearer <token>`, or (on WebSocket upgrades only)
/// `Sec-WebSocket-Protocol: tidebreak-token.<token>`.
///
/// This is the only place a principal is minted; handlers learn it through
/// the fail-closed `AuthContext` extractor.
pub async fn require_token(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let presented = extract_token(request.headers()).map(str::to_owned);
    let resolved = match presented.as_deref() {
        Some(presented) => resolve_principal(&state, presented).await,
        None => None,
    };

    match resolved {
        Some(principal) => {
            if matches!(
                state.principal_authenticator.as_ref(),
                PrincipalAuthenticator::Gateway(_)
            ) {
                let bearer: std::sync::Arc<str> = presented
                    .expect("a resolved principal had a presented token")
                    .into();
                // Remember the live caller token so this caller's turns and
                // catalog reads run on their own gateway authority (decisions
                // 51 and 62). The token stays in process memory and is never
                // persisted.
                if let Some(gateway) = state.on_behalf_of_gateway.as_ref() {
                    gateway.record_caller(&principal.owner_id(), bearer.clone());
                }
                request.extensions_mut().insert(GatewayAuthLease {
                    bearer,
                    principal: principal.clone(),
                });
            }
            request.extensions_mut().insert(AuthContext {
                principal,
                client_executor: false,
            });
            next.run(request).await
        }
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

/// Process-memory lease retained by Gateway-authenticated WebSocket tasks.
///
/// The bearer is intentionally private and this type deliberately implements
/// neither `Debug` nor serialization. Socket loops use it only to re-check the
/// live Gateway principal; static-token and local sockets receive no lease.
#[derive(Clone)]
pub struct GatewayAuthLease {
    bearer: std::sync::Arc<str>,
    principal: Principal,
}

impl GatewayAuthLease {
    /// Revalidate the original credential and require identity and role to be
    /// unchanged. Refusal, expiry, deactivation, demotion/promotion, and
    /// verifier outages all fail closed.
    pub async fn revalidate(&self, state: &AppState) -> bool {
        state
            .principal_authenticator
            .resolve(&self.bearer)
            .await
            .is_some_and(|principal| principal == self.principal)
    }
}

/// Map the presented credential to WHO is asking, per the boot profile.
///
/// `None` is a 401: a token that names no one admits no one. Unknown future
/// profiles resolve nobody until they choose an authenticator — fail closed,
/// never defaulted to the local owner.
async fn resolve_principal(state: &AppState, presented: &str) -> Option<Principal> {
    match state.config.profile {
        // The per-launch bearer is loopback-only and handed to one client, so
        // the verified caller *is* the local owner.
        Profile::Desktop => constant_time_eq(presented.as_bytes(), state.token.as_bytes())
            .then_some(Principal::LocalOwner),
        // Every self-host credential names a configured user. The per-launch
        // bearer is deliberately not consulted: on a shared deployment it
        // names nobody, so it authenticates nobody.
        Profile::SelfHost => state.principal_authenticator.resolve(presented).await,
        _ => None,
    }
}

/// The self-host credential-to-principal mechanism selected at boot.
///
/// Gateway mode checks every request against the Gateway's live account and
/// session state. Standalone machines use static tokens or OpenID Connect;
/// OIDC may also load a token file for administrator bootstrap and CLI access.
pub enum PrincipalAuthenticator {
    None,
    Static(TokenMap),
    Gateway(GatewayAuthenticator),
    Oidc(OidcAuthenticator),
}

impl PrincipalAuthenticator {
    pub fn from_config(config: &tidebreak_core::Config) -> Result<Self> {
        // OIDC is checked first because it is the one mode that may carry a
        // token file alongside it, so the exclusivity table below would read
        // an OIDC deployment's bootstrap file as static-token mode.
        if let Some(issuer) = config.auth_oidc_issuer.as_deref() {
            if config.auth_gateway_url.is_some() {
                return Err(AgentError::config(
                    "self-host authentication is ambiguous: TIDEBREAK_AUTH_OIDC_ISSUER \
                     cannot be combined with TIDEBREAK_AUTH_GATEWAY_URL, because a machine \
                     signs browsers in through one identity provider",
                ));
            }
            let client_id = config.auth_oidc_client_id.as_deref().ok_or_else(|| {
                AgentError::config(
                    "TIDEBREAK_AUTH_OIDC_CLIENT_ID is required with TIDEBREAK_AUTH_OIDC_ISSUER",
                )
            })?;
            let client_secret = config.auth_oidc_client_secret.as_deref().ok_or_else(|| {
                AgentError::config(
                    "TIDEBREAK_AUTH_OIDC_CLIENT_SECRET is required with TIDEBREAK_AUTH_OIDC_ISSUER",
                )
            })?;
            let public_url = config.public_url.as_deref().ok_or_else(|| {
                AgentError::config(
                    "TIDEBREAK_PUBLIC_URL is required with TIDEBREAK_AUTH_OIDC_ISSUER so the \
                     provider has a callback to return to",
                )
            })?;
            // The token file stays the bootstrap for the first administrator
            // and for CLI access; OIDC never mints an admin (decision 0087).
            let bootstrap = config
                .auth_tokens_file
                .as_deref()
                .map(TokenMap::load)
                .transpose()?;
            return Ok(Self::Oidc(OidcAuthenticator::new(
                issuer,
                client_id,
                client_secret,
                config
                    .auth_oidc_claim
                    .as_deref()
                    .unwrap_or(DEFAULT_OIDC_CLAIM),
                public_url,
                bootstrap,
            )?));
        }
        match (
            config.auth_tokens_file.as_deref(),
            config.auth_gateway_url.as_deref(),
            config.auth_gateway_verifier_url.as_deref(),
            config.public_url.as_deref(),
        ) {
            (Some(path), None, None, _) => Ok(Self::Static(TokenMap::load(path)?)),
            (None, Some(gateway_url), verifier_url, Some(public_url)) => {
                let public_url = canonical_public_url(public_url)?;
                Ok(Self::Gateway(GatewayAuthenticator::new(
                    verifier_url.unwrap_or(gateway_url),
                    tidebreak_core::config::tidebreak_machine_resource(&public_url),
                )?))
            }
            (None, Some(_), _, None) => Err(AgentError::config(
                "TIDEBREAK_PUBLIC_URL is required with TIDEBREAK_AUTH_GATEWAY_URL so user credentials can be bound to this exact machine",
            )),
            (Some(_), Some(_), _, _) | (Some(_), None, Some(_), _) => Err(AgentError::config(
                "self-host authentication is ambiguous: set exactly one of \
                 TIDEBREAK_AUTH_TOKENS_FILE or TIDEBREAK_AUTH_GATEWAY_URL",
            )),
            (None, None, Some(_), _) => Err(AgentError::config(
                "TIDEBREAK_AUTH_GATEWAY_VERIFIER_URL requires \
                 TIDEBREAK_AUTH_GATEWAY_URL to name the public identity authority",
            )),
            (None, None, None, _) => Err(AgentError::config(
                "self-host requires a principal-naming authenticator: set \
                 TIDEBREAK_AUTH_GATEWAY_URL for Model Gateway identity, \
                 TIDEBREAK_AUTH_OIDC_ISSUER for an OpenID Connect provider, or \
                 TIDEBREAK_AUTH_TOKENS_FILE for standalone static tokens",
            )),
        }
    }

    async fn resolve(&self, presented: &str) -> Option<Principal> {
        match self {
            Self::None => None,
            Self::Static(tokens) => tokens
                .resolve(presented)
                .map(|(id, role)| Principal::User { id, role }),
            Self::Oidc(oidc) => oidc.resolve(presented),
            Self::Gateway(gateway) => match gateway.resolve(presented).await {
                Ok(principal) => principal,
                Err(error) => {
                    tracing::warn!(%error, "model-gateway could not validate Tidebreak caller");
                    None
                }
            },
        }
    }
}

/// The ID-token claim whose string value names the Tidebreak user when the
/// operator sets no override.
const DEFAULT_OIDC_CLAIM: &str = "sub";

/// Prefix on every bearer this machine mints for an OIDC sign-in. It is what
/// makes a mode confusion visible: a static token never carries it, and a
/// token-file machine holds no store that could ever answer for one.
const OIDC_BEARER_PREFIX: &str = "tb_oidc_";

/// How long a minted bearer names its principal. A browser tab holds no
/// refreshable session, so this is the length of a hosted sign-in (decision
/// 0082 defers the refresh path).
const OIDC_BEARER_LIFETIME: Duration = Duration::from_secs(3600);

/// How long a started authorization flow waits for its callback. Long enough
/// for a password, a second factor, and a consent screen; short enough that an
/// abandoned flow is not a lasting entry.
const OIDC_FLOW_LIFETIME: Duration = Duration::from_secs(600);

/// Ceilings on the two in-memory maps. `/auth/oidc/start` is public, so
/// without a cap anyone could grow the pending map by asking; a start that
/// would exceed the cap is refused rather than served, which is the
/// fail-closed direction. Both are far above any real standalone deployment.
const MAX_PENDING_OIDC_FLOWS: usize = 512;
const MAX_OIDC_BEARERS: usize = 4096;

/// Nothing an issuer legitimately answers with is this large. Discovery
/// documents and key sets run to a few kilobytes.
const OIDC_RESPONSE_LIMIT: usize = 64 * 1024;

/// The subset of the issuer's discovery document this machine uses.
#[derive(serde::Deserialize)]
struct OidcMetadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
}

/// One authorization flow between `/auth/oidc/start` and its callback.
///
/// The verifier and the nonce never leave this process: the verifier proves
/// the callback belongs to the browser that started the flow, and the nonce
/// proves the ID token was minted for it.
struct PendingOidc {
    verifier: String,
    nonce: String,
    return_to: String,
    created: Instant,
}

/// Standalone OpenID Connect authenticator (decision 0087).
///
/// The machine runs the authorization-code flow itself and mints its own
/// bearer once the ID token verifies, so the issuer is on the path at sign-in
/// and nowhere else. Bearers live in process memory: a restart signs everyone
/// out, which is the same bound a hosted tab already has.
pub struct OidcAuthenticator {
    issuer: reqwest::Url,
    client_id: String,
    client_secret: String,
    /// The ID-token claim mapped to the Tidebreak user id.
    claim: String,
    /// What the authorization request asks the issuer to release.
    scope: &'static str,
    callback_url: String,
    /// Where the sign-in page sends the browser; published by discovery.
    start_url: String,
    client: reqwest::Client,
    pending: std::sync::Mutex<HashMap<String, PendingOidc>>,
    bearers: std::sync::Mutex<HashMap<String, (Principal, Instant)>>,
    /// The token file an OIDC deployment may keep beside the provider, for the
    /// first administrator and for CLI access. OIDC itself mints no admin.
    bootstrap: Option<TokenMap>,
}

/// Claims an issuer releases only for the `profile` scope. Asking for the
/// scope a login claim needs is the difference between a working mapping and
/// a sign-in that fails closed on a claim the provider was never told to send.
const OIDC_PROFILE_CLAIMS: &[&str] = &[
    "name",
    "preferred_username",
    "nickname",
    "given_name",
    "family_name",
    "profile",
];

impl OidcAuthenticator {
    fn new(
        issuer: &str,
        client_id: &str,
        client_secret: &str,
        claim: &str,
        public_url: &str,
        bootstrap: Option<TokenMap>,
    ) -> Result<Self> {
        let issuer = reqwest::Url::parse(issuer.trim())
            .map_err(|error| AgentError::config(format!("invalid OIDC issuer: {error}")))?;
        require_https_or_loopback(&issuer, "TIDEBREAK_AUTH_OIDC_ISSUER")?;
        if issuer.query().is_some()
            || issuer.fragment().is_some()
            || !issuer.username().is_empty()
            || issuer.password().is_some()
        {
            return Err(AgentError::config(
                "TIDEBREAK_AUTH_OIDC_ISSUER must not contain credentials, a query, or a fragment",
            ));
        }
        if claim.is_empty()
            || claim.len() > 128
            || !claim
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(AgentError::config(
                "TIDEBREAK_AUTH_OIDC_CLAIM must be a plain claim name of letters, digits, \
                 `.`, `_`, or `-`",
            ));
        }
        let scope = match claim {
            "email" | "email_verified" => "openid email",
            claim if OIDC_PROFILE_CLAIMS.contains(&claim) => "openid profile",
            _ => "openid",
        };
        let public_url = canonical_public_url(public_url)?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| AgentError::config(format!("OIDC client: {error}")))?;
        Ok(Self {
            issuer,
            client_id: client_id.to_owned(),
            client_secret: client_secret.to_owned(),
            claim: claim.to_owned(),
            scope,
            callback_url: format!("{public_url}/auth/oidc/callback"),
            start_url: format!("{public_url}/auth/oidc/start"),
            client,
            pending: std::sync::Mutex::new(HashMap::new()),
            bearers: std::sync::Mutex::new(HashMap::new()),
            bootstrap,
        })
    }

    /// What the sign-in button names. The host alone, because an operator
    /// reads "login.example.com", not a URL with a scheme and a path.
    fn issuer_name(&self) -> String {
        self.issuer
            .host_str()
            .unwrap_or(self.issuer.as_str())
            .to_owned()
    }

    /// The bootstrap token file first, then this machine's own bearers.
    ///
    /// Anything else names nobody, which is how an OIDC machine refuses a
    /// static token it was never given: there is no roster to find it in.
    fn resolve(&self, presented: &str) -> Option<Principal> {
        if let Some((id, role)) = self
            .bootstrap
            .as_ref()
            .and_then(|tokens| tokens.resolve(presented))
        {
            return Some(Principal::User { id, role });
        }
        if !presented.starts_with(OIDC_BEARER_PREFIX) {
            return None;
        }
        let mut bearers = self.bearers.lock().ok()?;
        let now = Instant::now();
        bearers.retain(|_, (_, expires)| *expires > now);
        bearers
            .get(presented)
            .map(|(principal, _)| principal.clone())
    }

    /// Start a flow: remember what the callback must prove, and answer with
    /// what the authorization request has to carry.
    ///
    /// `None` when the pending map is at its ceiling. A machine that cannot
    /// remember a flow must not start one, because a callback it cannot check
    /// is a callback it would have to trust.
    fn begin(&self, return_to: &str) -> Option<(String, String, oauth2::PkceCodeChallenge)> {
        let (challenge, verifier) = oauth2::PkceCodeChallenge::new_random_sha256();
        // 256 bits from the OS generator, base64url — so both values are
        // header- and fragment-safe as well as unguessable.
        let state = oauth2::CsrfToken::new_random().secret().clone();
        let nonce = oauth2::CsrfToken::new_random().secret().clone();
        let mut pending = self.pending.lock().ok()?;
        pending.retain(|_, flow| flow.created.elapsed() < OIDC_FLOW_LIFETIME);
        if pending.len() >= MAX_PENDING_OIDC_FLOWS {
            return None;
        }
        pending.insert(
            state.clone(),
            PendingOidc {
                verifier: verifier.secret().clone(),
                nonce: nonce.clone(),
                return_to: return_to.to_owned(),
                created: Instant::now(),
            },
        );
        Some((state, nonce, challenge))
    }

    /// Take the flow this callback claims, if it is one this machine started
    /// and has not already answered. Removing it is what makes a `state`
    /// single-use.
    fn claim_flow(&self, state: &str) -> Option<PendingOidc> {
        self.pending
            .lock()
            .ok()?
            .remove(state)
            .filter(|flow| flow.created.elapsed() < OIDC_FLOW_LIFETIME)
    }

    /// The URL the browser is sent to, carrying everything the callback will
    /// be checked against. Separate from [`OidcAuthenticator::begin`] so the
    /// request this machine actually makes is readable on its own.
    fn authorization_url(
        &self,
        endpoint: &str,
        state: &str,
        nonce: &str,
        challenge: &oauth2::PkceCodeChallenge,
    ) -> Option<reqwest::Url> {
        let mut url = reqwest::Url::parse(endpoint).ok()?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", &self.callback_url)
            .append_pair("scope", self.scope)
            .append_pair("state", state)
            .append_pair("nonce", nonce)
            .append_pair("code_challenge", challenge.as_str())
            .append_pair("code_challenge_method", challenge.method().as_str());
        Some(url)
    }

    /// Mint a bearer for a verified principal and remember it for `lifetime`.
    fn mint_bearer(&self, principal: Principal, lifetime: Duration) -> Option<String> {
        let bearer = format!(
            "{OIDC_BEARER_PREFIX}{}",
            oauth2::CsrfToken::new_random().secret()
        );
        let mut bearers = self.bearers.lock().ok()?;
        let now = Instant::now();
        bearers.retain(|_, (_, expires)| *expires > now);
        if bearers.len() >= MAX_OIDC_BEARERS {
            return None;
        }
        bearers.insert(bearer.clone(), (principal, now + lifetime));
        Some(bearer)
    }

    /// Read the issuer's discovery document and check it describes the issuer
    /// this machine was configured with.
    ///
    /// Every endpoint it names is used to reach the provider, so each one is
    /// held to the same transport rule as the issuer itself: an issuer that
    /// answers with an `http://` token endpoint does not get the client
    /// secret.
    async fn metadata(&self) -> Result<OidcMetadata> {
        let mut url = self.issuer.clone();
        let base = url.path().trim_end_matches('/');
        url.set_path(&format!("{base}/.well-known/openid-configuration"));
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|error| AgentError::msg(format!("OIDC discovery failed: {error}")))?
            .error_for_status()
            .map_err(|error| AgentError::msg(format!("OIDC discovery was refused: {error}")))?;
        let metadata: OidcMetadata = bounded_oidc_json(response).await?;
        if metadata.issuer.trim_end_matches('/') != self.issuer.as_str().trim_end_matches('/') {
            return Err(AgentError::msg(
                "OIDC discovery named a different issuer than the configured one",
            ));
        }
        for (endpoint, what) in [
            (&metadata.authorization_endpoint, "authorization_endpoint"),
            (&metadata.token_endpoint, "token_endpoint"),
            (&metadata.jwks_uri, "jwks_uri"),
        ] {
            let url = reqwest::Url::parse(endpoint).map_err(|error| {
                AgentError::msg(format!("OIDC discovery {what} is not a URL: {error}"))
            })?;
            require_https_or_loopback(&url, &format!("the OIDC {what}"))?;
        }
        Ok(metadata)
    }
}

/// Read a bounded JSON body. An issuer that answers with more than a key set
/// is not one this machine reads into memory.
async fn bounded_oidc_json<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T> {
    if response
        .content_length()
        .is_some_and(|length| length > OIDC_RESPONSE_LIMIT as u64)
    {
        return Err(AgentError::msg("OIDC response exceeded the size limit"));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| AgentError::msg(format!("OIDC response failed: {error}")))?;
    if bytes.len() > OIDC_RESPONSE_LIMIT {
        return Err(AgentError::msg("OIDC response exceeded the size limit"));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| AgentError::msg(format!("OIDC response was invalid: {error}")))
}

/// Require a URL this machine is willing to send a credential to. Plain HTTP
/// is allowed only against loopback, where there is no network to read it off.
fn require_https_or_loopback(url: &reqwest::Url, what: &str) -> Result<()> {
    let loopback = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if url.scheme() == "https" || (url.scheme() == "http" && loopback) {
        return Ok(());
    }
    Err(AgentError::config(format!(
        "{what} must use https (http is allowed only for loopback development)"
    )))
}

#[derive(serde::Deserialize)]
pub struct OidcStartQuery {
    /// Which UI route to open once the page is signed in, exactly as the
    /// console hand-off carries it.
    #[serde(default)]
    return_to: Option<String>,
}

/// Start an OpenID Connect sign-in on this machine.
///
/// The browser arrives from the sign-in screen with no credential — the
/// route is public for the same reason the discovery document is. It answers
/// with a redirect to the issuer's authorization endpoint carrying a fresh
/// `state`, `nonce`, and PKCE challenge; the secrets behind all three stay
/// here. A machine that does not authenticate through OIDC has no such flow,
/// so the route does not exist there.
pub async fn oidc_start(
    State(state): State<AppState>,
    Query(query): Query<OidcStartQuery>,
) -> Response {
    let PrincipalAuthenticator::Oidc(oidc) = state.principal_authenticator.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let landing = handoff_landing(&state);
    let metadata = match oidc.metadata().await {
        Ok(metadata) => metadata,
        Err(error) => {
            tracing::warn!(%error, "OIDC discovery could not be read");
            return handoff_redirect(&landing, "handoff-failed", "unavailable", None);
        }
    };
    let return_to = handoff_return_route(query.return_to.as_deref());
    let Some((flow_state, nonce, challenge)) = oidc.begin(return_to) else {
        tracing::warn!("refused an OIDC sign-in: too many flows are already waiting");
        return handoff_redirect(&landing, "handoff-failed", "unavailable", None);
    };
    let Some(authorization) = oidc.authorization_url(
        &metadata.authorization_endpoint,
        &flow_state,
        &nonce,
        &challenge,
    ) else {
        return handoff_redirect(&landing, "handoff-failed", "unavailable", None);
    };
    (
        StatusCode::FOUND,
        [
            (LOCATION, authorization.to_string()),
            (CACHE_CONTROL, "no-store".to_owned()),
            (REFERRER_POLICY, "no-referrer".to_owned()),
        ],
    )
        .into_response()
}

#[derive(serde::Deserialize)]
pub struct OidcCallbackQuery {
    #[serde(default)]
    code: String,
    #[serde(default)]
    state: String,
    /// What the issuer sends instead of a code when it refused. A refusal
    /// admits nobody, so it is only a reason for the page to word.
    #[serde(default)]
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct OidcTokenResponse {
    id_token: String,
}

/// Finish an OpenID Connect sign-in and land the page holding a bearer.
///
/// Everything the issuer says is checked before anything is minted: the
/// `state` names a flow this machine started and has not answered, the code
/// is exchanged with that flow's PKCE verifier, and the ID token has to carry
/// this client's audience, the configured issuer, a live expiry, and the
/// flow's nonce. Only then does the machine mint a bearer of its own and hand
/// it to the page through the fragment envelope the console hand-off uses
/// (decision 0082), so the page needs no second way to receive one.
///
/// Nothing here is logged: not the code, not the ID token, not the bearer.
pub async fn oidc_callback(
    State(state): State<AppState>,
    Query(query): Query<OidcCallbackQuery>,
) -> Response {
    let PrincipalAuthenticator::Oidc(oidc) = state.principal_authenticator.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let landing = handoff_landing(&state);
    let refused = |reason| handoff_redirect(&landing, "handoff-failed", reason, None);
    if query.error.is_some() || query.code.is_empty() || query.state.is_empty() {
        return refused("invalid");
    }
    let Some(flow) = oidc.claim_flow(&query.state) else {
        return refused("invalid");
    };
    let metadata = match oidc.metadata().await {
        Ok(metadata) => metadata,
        Err(error) => {
            tracing::warn!(%error, "OIDC discovery could not be read");
            return refused("unavailable");
        }
    };
    let exchange = oidc
        .client
        .post(&metadata.token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", query.code.as_str()),
            ("redirect_uri", oidc.callback_url.as_str()),
            ("client_id", oidc.client_id.as_str()),
            ("client_secret", oidc.client_secret.as_str()),
            ("code_verifier", flow.verifier.as_str()),
        ])
        .send()
        .await;
    let token: OidcTokenResponse = match exchange {
        // A refused exchange is the provider saying no, which admits nobody.
        Ok(response) if response.status().is_client_error() => return refused("invalid"),
        Ok(response) if !response.status().is_success() => {
            tracing::warn!(status = %response.status(), "OIDC token exchange returned an error status");
            return refused("unavailable");
        }
        Ok(response) => match bounded_oidc_json(response).await {
            Ok(token) => token,
            Err(error) => {
                tracing::warn!(%error, "OIDC token response was unusable");
                return refused("unavailable");
            }
        },
        Err(error) => {
            tracing::warn!(%error, "OIDC token exchange failed");
            return refused("unavailable");
        }
    };
    let jwks: jsonwebtoken::jwk::JwkSet = match oidc.client.get(&metadata.jwks_uri).send().await {
        Ok(response) if response.status().is_success() => match bounded_oidc_json(response).await {
            Ok(jwks) => jwks,
            Err(error) => {
                tracing::warn!(%error, "OIDC key set was unusable");
                return refused("unavailable");
            }
        },
        _ => {
            tracing::warn!("OIDC key set could not be read");
            return refused("unavailable");
        }
    };
    let Some(principal) =
        validate_oidc_id_token(oidc, &metadata, &jwks, &token.id_token, &flow.nonce)
    else {
        return refused("invalid");
    };
    let Some(bearer) = oidc.mint_bearer(principal, OIDC_BEARER_LIFETIME) else {
        tracing::warn!("refused an OIDC sign-in: too many bearers are already live");
        return refused("unavailable");
    };
    handoff_redirect(&landing, "handoff", &bearer, Some(&flow.return_to))
}

/// Verify an ID token against the issuer's keys and map it to a principal.
///
/// `None` for every failure, deliberately without distinguishing them: the
/// caller lands the page on one refusal either way, and a reason told apart
/// here would only be told apart by whoever forged the token.
///
/// The role is always [`Role::Member`]. Decision 6 requires an external
/// identity provider to supply a role or default everyone to member, and an
/// ID token carries no Tidebreak role — the token file is where a standalone
/// deployment names its administrators.
fn validate_oidc_id_token(
    oidc: &OidcAuthenticator,
    metadata: &OidcMetadata,
    jwks: &jsonwebtoken::jwk::JwkSet,
    id_token: &str,
    nonce: &str,
) -> Option<Principal> {
    let header = jsonwebtoken::decode_header(id_token).ok()?;
    // The algorithm comes from the token, so the set it may name is fixed
    // here: signing algorithms only. `none` and the HMAC family are excluded,
    // the latter because their key would be the client secret rather than
    // anything the issuer's key set publishes.
    if !matches!(
        header.alg,
        jsonwebtoken::Algorithm::RS256
            | jsonwebtoken::Algorithm::RS384
            | jsonwebtoken::Algorithm::RS512
            | jsonwebtoken::Algorithm::PS256
            | jsonwebtoken::Algorithm::PS384
            | jsonwebtoken::Algorithm::PS512
            | jsonwebtoken::Algorithm::ES256
            | jsonwebtoken::Algorithm::ES384
            | jsonwebtoken::Algorithm::EdDSA
    ) {
        return None;
    }
    let key = jsonwebtoken::DecodingKey::from_jwk(jwks.find(header.kid.as_deref()?)?).ok()?;
    let mut validation = jsonwebtoken::Validation::new(header.alg);
    validation.set_audience(&[&oidc.client_id]);
    validation.set_issuer(&[metadata.issuer.as_str()]);
    validation.set_required_spec_claims(&["exp", "aud", "iss", "sub"]);
    let claims = jsonwebtoken::decode::<serde_json::Value>(id_token, &key, &validation)
        .ok()?
        .claims;
    // The nonce binds the token to the flow this browser started, which is
    // what stops one replayed here from another.
    if claims.get("nonce").and_then(serde_json::Value::as_str) != Some(nonce) {
        return None;
    }
    let login = claims
        .get(&oidc.claim)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())?;
    Some(Principal::User {
        id: UserId::new(login).ok()?,
        role: Role::Member,
    })
}

/// Live verifier for Gateway-issued `tidebreak` resource tokens.
pub struct GatewayAuthenticator {
    principal_url: reqwest::Url,
    /// Where a one-time handoff code becomes a bearer; see [`handoff`].
    handoff_url: reqwest::Url,
    resource: String,
    client: reqwest::Client,
}

/// What redeeming a handoff code came to.
pub enum HandoffOutcome {
    /// A bearer for this machine, bound by the gateway to this resource.
    Granted(String),
    /// The gateway would not exchange the code: unknown, consumed, or past
    /// its minute. The reader re-enters from the console.
    Refused,
    /// The gateway did not answer usably. Nothing about the code is known.
    Unavailable,
}

#[derive(serde::Deserialize)]
struct HandoffGrant {
    access_token: String,
}

#[derive(serde::Deserialize)]
struct GatewayPrincipal {
    user_id: uuid::Uuid,
    is_admin: bool,
}

impl GatewayAuthenticator {
    const RESPONSE_LIMIT: usize = 16 * 1024;

    fn new(base_url: &str, resource: String) -> Result<Self> {
        let mut base = reqwest::Url::parse(base_url.trim()).map_err(|error| {
            AgentError::config(format!("invalid Model Gateway verifier URL: {error}"))
        })?;
        if !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(AgentError::config(
                "the Model Gateway verifier URL must not contain credentials, a query, or a fragment",
            ));
        }
        let loopback = base.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        });
        if base.scheme() != "https" && !(base.scheme() == "http" && loopback) {
            return Err(AgentError::config(
                "the Model Gateway verifier URL must use https (http is allowed only for loopback development)",
            ));
        }
        base.set_query(None);
        base.set_fragment(None);
        let mut principal_url = base.clone();
        principal_url.set_path("/api/v1/tidebreak/principal");
        let mut handoff_url = base;
        handoff_url.set_path("/oauth/handoff/token");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| AgentError::config(format!("gateway auth client: {error}")))?;
        Ok(Self {
            principal_url,
            handoff_url,
            resource,
            client,
        })
    }

    /// Exchange a console-minted one-time code for a bearer.
    ///
    /// The machine only relays. The gateway binds the bearer to this
    /// machine's resource itself, and every later request re-presents that
    /// bearer to [`GatewayAuthenticator::resolve`], so nothing the code
    /// claims is trusted here — not even the shape of what comes back beyond
    /// it being a bearer this verifier would accept.
    pub async fn redeem_handoff(&self, code: &str) -> HandoffOutcome {
        let response = match self
            .client
            .post(self.handoff_url.clone())
            .json(&serde_json::json!({ "code": code }))
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(error = %error, "gateway handoff request failed");
                return HandoffOutcome::Unavailable;
            }
        };
        let status = response.status();
        if status.is_client_error() {
            return HandoffOutcome::Refused;
        }
        if !status.is_success() {
            tracing::warn!(%status, "gateway handoff returned an error status");
            return HandoffOutcome::Unavailable;
        }
        if response
            .content_length()
            .is_some_and(|length| length > Self::RESPONSE_LIMIT as u64)
        {
            tracing::warn!("gateway handoff response exceeded size limit");
            return HandoffOutcome::Unavailable;
        }
        let bytes = match response.bytes().await {
            Ok(bytes) if bytes.len() <= Self::RESPONSE_LIMIT => bytes,
            Ok(_) => {
                tracing::warn!("gateway handoff response exceeded size limit");
                return HandoffOutcome::Unavailable;
            }
            Err(error) => {
                tracing::warn!(error = %error, "gateway handoff response failed");
                return HandoffOutcome::Unavailable;
            }
        };
        let grant: HandoffGrant = match serde_json::from_slice(&bytes) {
            Ok(grant) => grant,
            Err(error) => {
                tracing::warn!(error = %error, "gateway handoff response was invalid");
                return HandoffOutcome::Unavailable;
            }
        };
        if !grant.access_token.starts_with("mg_at_")
            || grant.access_token.len() > 512
            || !grant.access_token.bytes().all(is_token_byte)
        {
            tracing::warn!("gateway handoff granted something other than a bearer");
            return HandoffOutcome::Unavailable;
        }
        HandoffOutcome::Granted(grant.access_token)
    }

    async fn resolve(&self, presented: &str) -> Result<Option<Principal>> {
        if !presented.starts_with("mg_at_") || presented.len() > 512 {
            return Ok(None);
        }
        let response = self
            .client
            .get(self.principal_url.clone())
            .query(&[("resource", &self.resource)])
            .bearer_auth(presented)
            .send()
            .await
            .map_err(|error| AgentError::msg(format!("gateway auth request failed: {error}")))?;
        if matches!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ) {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(AgentError::msg(format!(
                "gateway auth returned status {}",
                response.status()
            )));
        }
        if response
            .content_length()
            .is_some_and(|length| length > Self::RESPONSE_LIMIT as u64)
        {
            return Err(AgentError::msg("gateway auth response exceeded size limit"));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                AgentError::msg(format!("gateway auth response failed: {error}"))
            })?;
            if bytes.len().saturating_add(chunk.len()) > Self::RESPONSE_LIMIT {
                return Err(AgentError::msg("gateway auth response exceeded size limit"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let principal: GatewayPrincipal = serde_json::from_slice(&bytes).map_err(|error| {
            AgentError::msg(format!("gateway auth response was invalid: {error}"))
        })?;
        let id = UserId::new(&principal.user_id.to_string())?;
        let role = if principal.is_admin {
            Role::Admin
        } else {
            Role::Member
        };
        Ok(Some(Principal::User { id, role }))
    }
}

/// Normalize the public URL whose digest names this machine's `tidebreak:`
/// resource. Shared with the on-behalf-of gateway, which names the same
/// resource in git-credential requests (decision 63).
pub fn canonical_public_url(raw: &str) -> Result<String> {
    let url = reqwest::Url::parse(raw.trim())
        .map_err(|error| AgentError::config(format!("invalid Tidebreak public URL: {error}")))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AgentError::config(
            "the Tidebreak public URL must be an HTTP(S) base URL without credentials, a query, or a fragment",
        ));
    }
    let loopback = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(AgentError::config(
            "the Tidebreak public URL must use https (http is allowed only for loopback development)",
        ));
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// The self-host profile's static credential-to-principal mapping.
///
/// Loaded once at boot from the operator's token file (format in the module
/// docs). This remains the standalone compatibility implementation behind the
/// same principal-authenticator seam used by live Gateway identity.
#[derive(Debug, Default)]
pub struct TokenMap {
    /// `(token, user, role)` triples; tokens are unique, users may repeat —
    /// and every line a user holds agrees about their role.
    entries: Vec<(Box<str>, UserId, Role)>,
}

/// Tokens shorter than this are refused at load: a guessable credential names
/// someone without authenticating them. Tokens are operator-generated, so the
/// floor costs nothing but a longer `openssl rand -hex 32`.
const MIN_TOKEN_LEN: usize = 32;

/// The third field that marks a line's user as an administrator of the
/// deployment. Any other value is a parse error rather than a silent member.
const ADMIN_FIELD: &str = "admin";

impl TokenMap {
    /// Load and validate the token file at `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            AgentError::config(format!(
                "failed to read auth tokens file {}: {e}",
                path.display()
            ))
        })?;
        Self::parse(&text)
    }

    /// Parse the token-file format. Rejects malformed lines, invalid ids,
    /// weak or header-unsafe tokens, duplicate tokens, a user whose lines
    /// disagree about their role, a file that names nobody, and a file that
    /// names no administrator — an authenticator that admits nobody, and a
    /// deployment nobody can configure, must both fail loudly at boot.
    pub fn parse(text: &str) -> Result<Self> {
        let mut entries: Vec<(Box<str>, UserId, Role)> = Vec::new();
        let mut roles: HashMap<UserId, Role> = HashMap::new();
        for (index, raw) in text.lines().enumerate() {
            let line_no = index + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split_whitespace();
            let (Some(user), Some(token), marker, None) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                return Err(AgentError::config(format!(
                    "auth tokens file line {line_no}: expected `<user-id> <token> [admin]`"
                )));
            };
            let role = match marker {
                None => Role::Member,
                Some(ADMIN_FIELD) => Role::Admin,
                Some(other) => {
                    return Err(AgentError::config(format!(
                        "auth tokens file line {line_no}: unknown role field {other:?}: the \
                         optional third field is `admin` or nothing"
                    )))
                }
            };
            let user = UserId::new(user)
                .map_err(|e| AgentError::config(format!("auth tokens file line {line_no}: {e}")))?;
            // Never echo the token itself into an error message.
            let token_ok = token.len() >= MIN_TOKEN_LEN
                && token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'-'));
            if !token_ok {
                return Err(AgentError::config(format!(
                    "auth tokens file line {line_no}: token must be at least {MIN_TOKEN_LEN} \
                     characters from [A-Za-z0-9._~-]"
                )));
            }
            if entries
                .iter()
                .any(|(existing, _, _)| existing.as_ref() == token)
            {
                return Err(AgentError::config(format!(
                    "auth tokens file line {line_no}: duplicate token"
                )));
            }
            // One user, one role. Rotation means several lines per user, and
            // a rotation that silently changed someone's authority — in
            // whichever direction the last line happened to win — is exactly
            // the failure this file must not have.
            match roles.get(&user) {
                Some(known) if *known != role => {
                    return Err(AgentError::config(format!(
                        "auth tokens file line {line_no}: user {user} is listed with conflicting \
                         roles; every line for a user must agree"
                    )))
                }
                _ => {
                    roles.insert(user.clone(), role);
                }
            }
            entries.push((token.into(), user, role));
        }
        if entries.is_empty() {
            return Err(AgentError::config(
                "auth tokens file names no principals; a self-host server nobody can \
                 authenticate to must not start",
            ));
        }
        if !entries.iter().any(|(_, _, role)| *role == Role::Admin) {
            return Err(AgentError::config(
                "auth tokens file names no administrator; a self-host server nobody can \
                 configure must not start — mark at least one user's lines `admin` \
                 (`<user-id> <token> admin`)",
            ));
        }
        Ok(Self { entries })
    }

    /// The user the presented credential names and the role they hold, if
    /// any. Exact match; every entry is compared in constant time regardless
    /// of where a match lands.
    pub fn resolve(&self, presented: &str) -> Option<(UserId, Role)> {
        let mut resolved = None;
        for (token, user, role) in &self.entries {
            if constant_time_eq(token.as_bytes(), presented.as_bytes()) && resolved.is_none() {
                resolved = Some((user.clone(), *role));
            }
        }
        resolved
    }
}

/// Require the deployment plane's role over the identity already resolved.
///
/// Like [`require_client_executor_token`], this reads the [`AuthContext`] the
/// bearer middleware attached and never mints one: no context means the
/// request reached an admin route without ever being authenticated, which is a
/// `401`, not a defaulted identity. A member's request is a `403` — the route
/// exists, they may not use it.
///
/// This is a router property, not a handler habit: everything assembled into
/// the deployment-plane sub-router is gated by construction, so a new config
/// route is admin-gated by where it is registered rather than by whether its
/// author remembered a check.
pub async fn require_admin(request: Request, next: Next) -> Response {
    let Some(auth) = request.extensions().get::<AuthContext>() else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if auth.principal.is_admin() {
        next.run(request).await
    } else {
        StatusCode::FORBIDDEN.into_response()
    }
}

/// Require the second credential held only by the trusted native host.
///
/// The credential is a machine capability, not a principal. It admits only
/// routes mounted on the native executor surface. When bearer authentication
/// already attached an [`AuthContext`], it also marks that context so native
/// configuration routes can distinguish the host from the renderer.
pub async fn require_client_executor_token(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let presented = request
        .headers()
        .get(&CLIENT_EXECUTOR_HEADER)
        .and_then(|value| value.to_str().ok());
    match presented {
        Some(token)
            if constant_time_eq(token.as_bytes(), state.client_executor_token.as_bytes()) =>
        {
            request.extensions_mut().insert(ClientExecutor);
            if let Some(auth) = request.extensions().get::<AuthContext>().cloned() {
                request.extensions_mut().insert(AuthContext {
                    client_executor: true,
                    ..auth
                });
            }
            next.run(request).await
        }
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
}

/// Require either the native executor credential or the narrower local-import
/// capability over an already authenticated principal.
///
/// This middleware gates only raw chat-document and image publication. The
/// scoped token never marks the request as a client executor and therefore
/// cannot satisfy native claim/resolve routes or extractors.
pub async fn require_local_import_capability(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if request.extensions().get::<AuthContext>().is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let local_import = request
        .headers()
        .get(&LOCAL_IMPORT_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|token| {
            constant_time_eq(token.as_bytes(), state.local_import_token.as_bytes())
        });
    let client_executor = request
        .headers()
        .get(&CLIENT_EXECUTOR_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|token| {
            constant_time_eq(token.as_bytes(), state.client_executor_token.as_bytes())
        });
    if local_import || client_executor {
        next.run(request).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

/// Reject a request whose `Origin` names a site that is not this app, or
/// whose `Host` is not the loopback address the desktop server binds to.
///
/// The bearer stays the gate; this is the second condition that makes a
/// *leaked* bearer insufficient rather than sufficient. Two attacks it closes,
/// neither of which CORS covers:
///
/// - **WebSocket upgrades.** CORS does not apply to them at all, and a browser
///   can set `Sec-WebSocket-Protocol` freely — so a page that learned the
///   token could open the event stream. It cannot forge `Origin`.
/// - **DNS rebinding.** A name the attacker controls, re-resolved to
///   `127.0.0.1`, reaches the server as a same-origin request from the
///   attacker's page. The `Host` header still carries their name.
///
/// Deliberately permissive in two places, because the alternative is breaking
/// legitimate callers to close nothing:
///
/// - **A request with no `Origin` passes.** Only browsers attach one; a `curl`
///   or an SDK never does, and rejecting them would gate a header the threat
///   model does not depend on.
/// - **Only the desktop profile is checked.** A self-host deployment is
///   reached from an operator-chosen origin over an operator-chosen name, and
///   neither is knowable here. It keeps the bearer and the operator's own
///   fronting.
///
/// The desktop profile is also what a bare `tidebreak serve` boots, so an
/// integrator driving that daemon from a browser page of their own must run it
/// as `TIDEBREAK_PROFILE=self_host`. A parent process reading the daemon's
/// stdout — the documented integration — sends no `Origin` and is unaffected.
pub async fn require_app_origin(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if state.config.profile == Profile::Desktop
        && !(origin_is_this_app(request.headers()) && host_is_loopback(request.headers()))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

/// Whether the request's `Origin`, if it declared one, is this app's.
#[must_use]
pub fn origin_is_this_app(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(ORIGIN) else {
        // Not a browser request, or a same-origin navigation (an iframe
        // loading a view frame). Nothing to check.
        return true;
    };
    origin_value_is_this_app(origin)
}

/// The same judgement on a bare header value, for the CORS layer's predicate.
#[must_use]
pub fn origin_value_is_this_app(origin: &axum::http::HeaderValue) -> bool {
    origin
        .to_str()
        .is_ok_and(|origin| APP_ORIGINS.contains(&origin) || dev_server_origin(origin))
}

/// The origins the packaged webview loads its frontend from.
///
/// Tauri serves `frontendDist` over its own protocol: `tauri://localhost` on
/// macOS and Linux, `http(s)://tauri.localhost` on Windows. A packaged webview
/// that sends no `Origin` is admitted by [`origin_is_this_app`]'s absent-header
/// path. `null` is not listed: sandboxed iframes report `Origin: null` while
/// still running script.
const APP_ORIGINS: &[&str] = &[
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
];

/// The Vite dev server, in debug builds only — `build.devUrl` in
/// `tauri.conf.json`, and the same port a browser tab uses during UI work.
fn dev_server_origin(origin: &str) -> bool {
    cfg!(debug_assertions) && matches!(origin, "http://localhost:1420" | "http://127.0.0.1:1420")
}

/// Whether the request addressed the server by a loopback name.
///
/// The desktop server binds loopback, so a `Host` naming anything else is a
/// request that arrived through a name resolving there — the rebinding case.
/// An absent `Host` passes, for the same reason an absent `Origin` does: HTTP/2
/// carries the authority in the request line instead, and every browser sends
/// one, so the header's absence never describes the attack.
#[must_use]
pub fn host_is_loopback(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(HOST) else {
        return true;
    };
    let Ok(host) = host.to_str() else {
        return false;
    };
    // Strip the port, keeping an IPv6 literal's brackets intact.
    let name = match host.strip_prefix('[') {
        Some(rest) => match rest.split_once(']') {
            Some((inner, _)) => inner,
            None => return false,
        },
        None => host.split(':').next().unwrap_or(host),
    };
    // `localhost` is resolved locally rather than through the DNS an attacker
    // could answer, so it is as loopback-bound as the literals.
    name.eq_ignore_ascii_case("localhost") || name == "127.0.0.1" || name == "::1"
}

/// Resolve the presented token: `Authorization` wins; on WebSocket upgrades,
/// fall back to the `tidebreak-token.` subprotocol entry.
pub fn extract_token(headers: &HeaderMap) -> Option<&str> {
    if let Some(token) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|token| !token.is_empty())
    {
        return Some(token);
    }
    if is_websocket_upgrade(headers) {
        return extract_token_from_subprotocols(headers);
    }
    None
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

/// Extract the access token from `Sec-WebSocket-Protocol`.
///
/// Clients offer `tidebreak-token.<token>` alongside `tidebreak-v1` so the token
/// never appears in the request URL. Reads every header value (clients may
/// split protocols across multiple headers).
pub fn extract_token_from_subprotocols(headers: &HeaderMap) -> Option<&str> {
    for value in headers.get_all(SEC_WEBSOCKET_PROTOCOL) {
        let Ok(header_value) = value.to_str() else {
            continue;
        };
        for entry in header_value.split(',') {
            let trimmed = entry.trim();
            if let Some(token) = trimmed.strip_prefix(WS_TOKEN_SUBPROTOCOL_PREFIX) {
                if !token.is_empty() {
                    return Some(token);
                }
            }
        }
    }
    None
}

/// Whether the client offered the handshake subprotocol (so the upgrade should
/// select it in the response). Independent of how the client authenticated —
/// if they offered `tidebreak-v1`, RFC 6455 expects a selection.
pub fn offered_handshake_subprotocol(headers: &HeaderMap) -> bool {
    for value in headers.get_all(SEC_WEBSOCKET_PROTOCOL) {
        let Ok(header_value) = value.to_str() else {
            continue;
        };
        if header_value
            .split(',')
            .any(|entry| entry.trim() == WS_HANDSHAKE_SUBPROTOCOL)
        {
            return true;
        }
    }
    false
}

/// Compare two byte strings without an early-out, so a caller can't learn the
/// token prefix-by-prefix from response timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use axum::routing::get;
    use axum::Router;

    fn headers(pairs: &[(HeaderName, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(name.clone(), HeaderValue::from_str(value).unwrap());
        }
        map
    }

    /// The point of the check: a page on the public web that has somehow
    /// learned the bearer still cannot open the event stream, because CORS
    /// never covered WebSocket upgrades and `Origin` is the one header its
    /// script cannot forge.
    #[test]
    fn a_foreign_origin_is_refused_and_the_app_and_non_browsers_are_not() {
        assert!(!origin_is_this_app(&headers(&[(
            axum::http::header::ORIGIN,
            "https://evil.example"
        )])));
        // A loopback port that is not the dev server is a foreign site too —
        // the embedded API itself serves untrusted app HTML on one.
        assert!(!origin_is_this_app(&headers(&[(
            axum::http::header::ORIGIN,
            "http://127.0.0.1:53219"
        )])));
        // The packaged webview's own origin.
        assert!(origin_is_this_app(&headers(&[(
            axum::http::header::ORIGIN,
            "tauri://localhost"
        )])));
        // A CLI or SDK attaches no Origin at all; only browsers do.
        // The packaged webview that omits Origin is admitted on this path.
        assert!(origin_is_this_app(&HeaderMap::new()));
        // Sandboxed iframes report Origin: null but still run script.
        assert!(!origin_is_this_app(&headers(&[(
            axum::http::header::ORIGIN,
            "null"
        )])));
    }

    /// DNS rebinding: the request reaches loopback, but the name the browser
    /// was pointed at travels with it.
    #[test]
    fn a_host_that_is_not_loopback_is_refused() {
        assert!(!host_is_loopback(&headers(&[(
            HOST,
            "rebind.example:7777"
        )])));
        assert!(host_is_loopback(&headers(&[(HOST, "127.0.0.1:7777")])));
        assert!(host_is_loopback(&headers(&[(HOST, "localhost:7777")])));
        assert!(host_is_loopback(&headers(&[(HOST, "[::1]:7777")])));
        // HTTP/2 carries the authority in the request line instead.
        assert!(host_is_loopback(&HeaderMap::new()));
    }

    /// Tokens are 32 characters at minimum, so the fixtures are too; the
    /// literals are named rather than inline so the secret scanner has
    /// nothing high-entropy to trip over.
    const ALICE_FIRST: &str = "alice-token-one-padded-to-thirty-two";
    const ALICE_SECOND: &str = "alice-token-two-padded-to-thirty-two";
    const BOB_TOKEN: &str = "bob-token-padded-out-to-thirty-two-x";
    const ADAPTER_FIRST: &str = "adapter-token-one-padded-to-thirty-two";
    const ADAPTER_SECOND: &str = "adapter-token-two-padded-to-thirty-two";

    #[test]
    fn adapter_bootstrap_tokens_support_bounded_rotation() {
        let tokens =
            AdapterBootstrapTokens::parse(Some(format!("{ADAPTER_FIRST}, {ADAPTER_SECOND}")))
                .unwrap()
                .unwrap();
        assert!(tokens.accepts(ADAPTER_FIRST));
        assert!(tokens.accepts(ADAPTER_SECOND));
        assert!(!tokens.accepts("adapter-token-wrong-padded-to-thirty-two"));

        for invalid in [
            "short".to_owned(),
            format!("{ADAPTER_FIRST},{ADAPTER_FIRST}"),
            format!("{}!", "a".repeat(32)),
            [ADAPTER_FIRST; 5].join(","),
        ] {
            assert!(
                AdapterBootstrapTokens::parse(Some(invalid)).is_err(),
                "an unsafe bootstrap token set must fail boot"
            );
        }
        assert!(AdapterBootstrapTokens::parse(None).unwrap().is_none());
        assert!(AdapterBootstrapTokens::parse(Some("  ".to_owned()))
            .unwrap()
            .is_none());
    }

    #[test]
    fn token_map_parses_the_documented_format_and_resolves_exactly() {
        let map = TokenMap::parse(&format!(
            "# staff\n\nalice {ALICE_FIRST} admin\nbob\t{BOB_TOKEN}\nalice {ALICE_SECOND} admin\n"
        ))
        .unwrap();
        let alice = UserId::new("alice").unwrap();
        assert_eq!(
            map.resolve(ALICE_FIRST),
            Some((alice.clone(), Role::Admin)),
            "the third field puts the user on the deployment plane"
        );
        assert_eq!(
            map.resolve(ALICE_SECOND),
            Some((alice, Role::Admin)),
            "a user may hold several tokens"
        );
        assert_eq!(
            map.resolve(BOB_TOKEN),
            Some((UserId::new("bob").unwrap(), Role::Member)),
            "no third field is a member"
        );
        assert_eq!(map.resolve("d".repeat(36).as_str()), None);
        assert_eq!(
            map.resolve(&ALICE_FIRST[..ALICE_FIRST.len() - 1]),
            None,
            "prefixes do not match"
        );
    }

    #[test]
    fn token_map_rejects_files_that_cannot_name_someone_safely() {
        for (text, why) in [
            (String::new(), "names nobody"),
            ("# only comments\n".into(), "names nobody"),
            ("alice\n".into(), "missing token"),
            (
                format!("alice {ALICE_FIRST} admin extra\n"),
                "trailing field",
            ),
            ("alice short\n".into(), "guessably short token"),
            (
                format!("alice {}\n", "a".repeat(31)),
                "token below the 32-character floor",
            ),
            (
                format!("alice {},{}\n", "a".repeat(16), "b".repeat(16)),
                "header-unsafe token character",
            ),
            (format!("al!ce {ALICE_FIRST} admin\n"), "invalid user id"),
            (
                format!("alice {ALICE_FIRST} admin\nbob {ALICE_FIRST} admin\n"),
                "one token must name one user",
            ),
            (
                format!("alice {ALICE_FIRST} owner\n"),
                "the only role field is `admin`",
            ),
            (
                format!("alice {ALICE_FIRST} admin\nalice {ALICE_SECOND}\n"),
                "a user's lines must agree about their role",
            ),
            (
                format!("alice {ALICE_FIRST}\nbob {BOB_TOKEN}\n"),
                "a deployment nobody can configure must not start",
            ),
        ] {
            assert!(TokenMap::parse(&text).is_err(), "{why}: {text:?}");
        }
    }

    /// The two refusals an operator has to act on name the remedy, and the
    /// zero-admin one is distinguishable from the empty-file one.
    #[test]
    fn refusals_that_need_an_operator_edit_say_what_to_add() {
        let refusal = TokenMap::parse(&format!("alice {ALICE_FIRST}\n"))
            .unwrap_err()
            .to_string();
        assert!(
            refusal.contains("names no administrator") && refusal.contains("admin"),
            "the zero-admin refusal must name the fix: {refusal}"
        );
    }

    #[test]
    fn extracts_bearer_authorization() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
        assert_eq!(extract_token(&headers), Some("secret"));
    }

    #[test]
    fn extracts_token_from_subprotocol_on_ws_upgrade() {
        let mut headers = HeaderMap::new();
        headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
        headers.insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("tidebreak-v1, tidebreak-token.abc.def"),
        );
        assert_eq!(extract_token_from_subprotocols(&headers), Some("abc.def"));
        assert_eq!(extract_token(&headers), Some("abc.def"));
        assert!(offered_handshake_subprotocol(&headers));
    }

    #[test]
    fn subprotocol_token_ignored_on_ordinary_http() {
        let mut headers = HeaderMap::new();
        headers.insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("tidebreak-v1, tidebreak-token.abc.def"),
        );
        assert!(extract_token(&headers).is_none());
    }

    #[test]
    fn authorization_wins_over_subprotocol() {
        let mut headers = HeaderMap::new();
        headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer from-header"),
        );
        headers.insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("tidebreak-v1, tidebreak-token.from-proto"),
        );
        assert_eq!(extract_token(&headers), Some("from-header"));
    }

    #[test]
    fn ignores_empty_token_subprotocol() {
        let mut headers = HeaderMap::new();
        headers.insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("tidebreak-v1, tidebreak-token."),
        );
        assert!(extract_token_from_subprotocols(&headers).is_none());
    }

    #[test]
    fn ignores_subprotocols_without_token_entry() {
        let mut headers = HeaderMap::new();
        headers.insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("tidebreak-v1"),
        );
        assert!(extract_token_from_subprotocols(&headers).is_none());
        assert!(offered_handshake_subprotocol(&headers));
    }

    #[test]
    fn reads_token_across_multiple_protocol_headers() {
        let mut headers = HeaderMap::new();
        headers.append(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("tidebreak-v1"),
        );
        headers.append(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("tidebreak-token.split-token"),
        );
        assert_eq!(
            extract_token_from_subprotocols(&headers),
            Some("split-token")
        );
        assert!(offered_handshake_subprotocol(&headers));
    }

    #[tokio::test]
    async fn gateway_auth_maps_live_identity_and_fails_closed() {
        let user_id = uuid::Uuid::new_v4();
        let resource =
            tidebreak_core::config::tidebreak_machine_resource("https://tidebreak.example.test");
        let expected_resource = resource.clone();
        let app = Router::new().route(
            "/api/v1/tidebreak/principal",
            get(
                move |query: axum::extract::Query<HashMap<String, String>>, headers: HeaderMap| {
                    let expected_resource = expected_resource.clone();
                    async move {
                        if query.get("resource") != Some(&expected_resource) {
                            return StatusCode::UNAUTHORIZED.into_response();
                        }
                        match headers
                            .get(AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                        {
                            Some("Bearer mg_at_admin") => Json(serde_json::json!({
                                "user_id": user_id,
                                "is_admin": true,
                            }))
                            .into_response(),
                            Some("Bearer mg_at_member") => Json(serde_json::json!({
                                "user_id": user_id,
                                "is_admin": false,
                            }))
                            .into_response(),
                            Some("Bearer mg_at_broken") => StatusCode::BAD_GATEWAY.into_response(),
                            _ => StatusCode::UNAUTHORIZED.into_response(),
                        }
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let verifier = GatewayAuthenticator::new(&format!("http://{address}"), resource).unwrap();

        let admin = verifier.resolve("mg_at_admin").await.unwrap().unwrap();
        assert!(admin.is_admin());
        assert_eq!(admin.owner_id().as_str(), format!("user:{user_id}"));

        let member = verifier.resolve("mg_at_member").await.unwrap().unwrap();
        assert!(!member.is_admin());
        assert_eq!(member.owner_id().as_str(), format!("user:{user_id}"));

        assert!(verifier.resolve("mg_at_revoked").await.unwrap().is_none());
        assert!(verifier
            .resolve("not-a-gateway-token")
            .await
            .unwrap()
            .is_none());
        assert!(verifier.resolve("mg_at_broken").await.is_err());
        server.abort();
    }

    #[test]
    fn self_host_requires_exactly_one_principal_authenticator() {
        let data_dir = tempfile::tempdir().unwrap();
        let mut config = tidebreak_core::Config::desktop(data_dir.path());
        config.profile = Profile::SelfHost;
        assert!(PrincipalAuthenticator::from_config(&config).is_err());

        config.auth_gateway_url = Some("https://gateway.example.test".to_owned());
        config.auth_gateway_verifier_url =
            Some("https://gateway.model-gateway.svc.cluster.local".to_owned());
        assert!(PrincipalAuthenticator::from_config(&config).is_err());
        config.public_url = Some("https://tidebreak.example.test".to_owned());
        assert!(matches!(
            PrincipalAuthenticator::from_config(&config),
            Ok(PrincipalAuthenticator::Gateway(_))
        ));

        let tokens = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tokens.path(),
            "admin 0123456789abcdef0123456789abcdef admin\n",
        )
        .unwrap();
        config.auth_tokens_file = Some(tokens.path().to_owned());
        assert!(PrincipalAuthenticator::from_config(&config).is_err());

        config.auth_gateway_url = None;
        assert!(PrincipalAuthenticator::from_config(&config).is_err());
        config.auth_gateway_verifier_url = None;
        assert!(matches!(
            PrincipalAuthenticator::from_config(&config),
            Ok(PrincipalAuthenticator::Static(_))
        ));
    }

    /// A throwaway Ed25519 key pair for the ID tokens below. The private half
    /// is spelled as DER bytes rather than a PEM block so the secret scanner
    /// has nothing key-shaped to trip over; it signs nothing outside this
    /// module.
    const TEST_OIDC_PRIVATE_KEY: &[u8] = &[
        48, 46, 2, 1, 0, 48, 5, 6, 3, 43, 101, 112, 4, 34, 4, 32, 77, 131, 23, 2, 149, 50, 243,
        210, 168, 16, 155, 75, 190, 114, 52, 174, 24, 147, 187, 163, 47, 207, 192, 156, 53, 223,
        168, 73, 209, 213, 223, 139,
    ];
    const TEST_OIDC_PUBLIC_KEY: &str = "iCQjb0jIV9LwVk60OpxoyoYEtpKuMou948yRVjp6UEU";
    const TEST_OIDC_ISSUER: &str = "http://127.0.0.1:12345";

    fn unix_now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// Sign an ID token the configured client would accept, with `overrides`
    /// applied over the well-formed claims so each case says exactly what it
    /// changed. A `null` override drops the claim entirely.
    fn oidc_test_token(overrides: serde_json::Value) -> String {
        let mut claims = serde_json::json!({
            "iss": TEST_OIDC_ISSUER,
            "sub": "oidc-user",
            "aud": "client-id",
            "exp": unix_now() + 300,
            "nonce": "expected-nonce",
            "email": "person@example.test",
        });
        let object = claims.as_object_mut().expect("the claims are an object");
        for (name, value) in overrides.as_object().expect("overrides are an object") {
            if value.is_null() {
                object.remove(name);
            } else {
                object.insert(name.clone(), value.clone());
            }
        }
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::EdDSA);
        header.kid = Some("test-key".to_owned());
        jsonwebtoken::encode(
            &header,
            &claims,
            &jsonwebtoken::EncodingKey::from_ed_der(TEST_OIDC_PRIVATE_KEY),
        )
        .unwrap()
    }

    fn oidc_test_jwks() -> jsonwebtoken::jwk::JwkSet {
        serde_json::from_value(serde_json::json!({
            "keys": [{
                "kty": "OKP",
                "crv": "Ed25519",
                "kid": "test-key",
                "use": "sig",
                "alg": "EdDSA",
                "x": TEST_OIDC_PUBLIC_KEY,
            }]
        }))
        .unwrap()
    }

    fn oidc_test_metadata() -> OidcMetadata {
        OidcMetadata {
            issuer: TEST_OIDC_ISSUER.to_owned(),
            authorization_endpoint: format!("{TEST_OIDC_ISSUER}/authorize"),
            token_endpoint: format!("{TEST_OIDC_ISSUER}/token"),
            jwks_uri: format!("{TEST_OIDC_ISSUER}/jwks"),
        }
    }

    fn oidc_test_authenticator(claim: &str, bootstrap: Option<TokenMap>) -> OidcAuthenticator {
        OidcAuthenticator::new(
            TEST_OIDC_ISSUER,
            "client-id",
            "client-secret",
            claim,
            "http://127.0.0.1:8080",
            bootstrap,
        )
        .unwrap()
    }

    /// The authorization request carries everything the callback is checked
    /// against, and the flow behind it is single-use: a `state` answers once,
    /// so a callback replayed after the browser landed finds nothing.
    #[test]
    fn a_started_flow_is_single_use_and_its_request_carries_the_whole_check() {
        let oidc = oidc_test_authenticator(DEFAULT_OIDC_CLAIM, None);
        let (state, nonce, challenge) = oidc.begin("/c/session-1").unwrap();
        let url = oidc
            .authorization_url(
                &oidc_test_metadata().authorization_endpoint,
                &state,
                &nonce,
                &challenge,
            )
            .unwrap();
        let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(url.path(), "/authorize");
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["client_id"], "client-id");
        assert_eq!(
            query["redirect_uri"],
            "http://127.0.0.1:8080/auth/oidc/callback"
        );
        assert_eq!(query["scope"], "openid");
        assert_eq!(query["state"], state);
        assert_eq!(query["nonce"], nonce);
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["code_challenge"], challenge.as_str());
        assert_ne!(
            query["code_challenge"], "",
            "the verifier never leaves this process, so the challenge must"
        );
        assert!(
            !query.contains_key("code_verifier"),
            "the verifier is the secret half and must not travel with the browser"
        );

        // The flow answers exactly one callback, and only its own state.
        assert!(oidc.claim_flow("some-other-state").is_none());
        let flow = oidc.claim_flow(&state).unwrap();
        assert_eq!(flow.nonce, nonce);
        assert_eq!(flow.return_to, "/c/session-1");
        assert!(
            oidc.claim_flow(&state).is_none(),
            "a state that already landed a browser must not land another"
        );
    }

    /// `/auth/oidc/start` is public, so the map behind it is bounded. A
    /// machine that cannot remember a flow refuses to start one rather than
    /// accepting a callback it could not check.
    #[test]
    fn the_number_of_waiting_sign_ins_is_bounded() {
        let oidc = oidc_test_authenticator(DEFAULT_OIDC_CLAIM, None);
        let started: Vec<_> = (0..MAX_PENDING_OIDC_FLOWS)
            .filter_map(|_| oidc.begin("/"))
            .collect();
        assert_eq!(started.len(), MAX_PENDING_OIDC_FLOWS);
        assert!(oidc.begin("/").is_none());
        // Answering one makes room for the next reader.
        oidc.claim_flow(&started[0].0).unwrap();
        assert!(oidc.begin("/").is_some());
    }

    /// Every reason an ID token can fail is a refusal, and each one is a
    /// separate way a forged or replayed token would otherwise get in: a
    /// token minted for another client, one from another issuer, one from a
    /// flow this machine did not start, one whose hour is up, and one signed
    /// by a key the issuer does not publish.
    #[test]
    fn an_id_token_this_machine_did_not_ask_for_names_nobody() {
        let oidc = oidc_test_authenticator(DEFAULT_OIDC_CLAIM, None);
        let metadata = oidc_test_metadata();
        let jwks = oidc_test_jwks();
        let verify =
            |token: &str| validate_oidc_id_token(&oidc, &metadata, &jwks, token, "expected-nonce");

        assert_eq!(
            verify(&oidc_test_token(serde_json::json!({}))),
            Some(Principal::User {
                id: UserId::new("oidc-user").unwrap(),
                // OIDC names a member; the token file names administrators.
                role: Role::Member,
            }),
            "a token for this client, from this issuer, in this flow, signs the person in"
        );

        for (token, why) in [
            (
                oidc_test_token(serde_json::json!({ "aud": "another-client" })),
                "an audience that is not this client id",
            ),
            (
                oidc_test_token(serde_json::json!({ "iss": "https://other.example.test" })),
                "an issuer that is not the configured one",
            ),
            (
                oidc_test_token(serde_json::json!({ "nonce": "some-other-flow" })),
                "a nonce from a flow this machine did not start",
            ),
            (
                oidc_test_token(serde_json::json!({ "nonce": null })),
                "no nonce at all",
            ),
            (
                oidc_test_token(serde_json::json!({ "exp": unix_now() - 3600 })),
                "an expiry in the past",
            ),
            (
                oidc_test_token(serde_json::json!({ "sub": null })),
                "no subject",
            ),
        ] {
            assert!(verify(&token).is_none(), "{why} must admit nobody");
        }

        // A key set that does not publish the signing key admits nobody
        // either, however well-formed the token is.
        let other_keys: jsonwebtoken::jwk::JwkSet = serde_json::from_value(serde_json::json!({
            "keys": [{
                "kty": "OKP",
                "crv": "Ed25519",
                "kid": "another-key",
                "use": "sig",
                "alg": "EdDSA",
                "x": TEST_OIDC_PUBLIC_KEY,
            }]
        }))
        .unwrap();
        assert!(validate_oidc_id_token(
            &oidc,
            &metadata,
            &other_keys,
            &oidc_test_token(serde_json::json!({})),
            "expected-nonce",
        )
        .is_none());
    }

    /// The claim override picks which string names the user. A claim the
    /// provider did not send, or sent as something other than a string, signs
    /// nobody in — decision 0087's fail-closed outcome for a stale mapping.
    #[test]
    fn the_login_claim_override_names_the_user_or_nobody() {
        let oidc = oidc_test_authenticator("email", None);
        let metadata = oidc_test_metadata();
        let jwks = oidc_test_jwks();
        assert_eq!(
            validate_oidc_id_token(
                &oidc,
                &metadata,
                &jwks,
                &oidc_test_token(serde_json::json!({})),
                "expected-nonce",
            ),
            Some(Principal::User {
                id: UserId::new("person@example.test").unwrap(),
                role: Role::Member,
            })
        );
        for overrides in [
            serde_json::json!({ "email": null }),
            serde_json::json!({ "email": "" }),
            serde_json::json!({ "email": 42 }),
            serde_json::json!({ "email": ["person@example.test"] }),
        ] {
            assert!(validate_oidc_id_token(
                &oidc,
                &metadata,
                &jwks,
                &oidc_test_token(overrides.clone()),
                "expected-nonce",
            )
            .is_none());
        }
        // Asking for a claim means asking for the scope that releases it.
        assert_eq!(oidc.scope, "openid email");
        assert_eq!(oidc_test_authenticator("sub", None).scope, "openid");
        assert_eq!(
            oidc_test_authenticator("preferred_username", None).scope,
            "openid profile"
        );
    }

    /// A bearer this machine minted names its principal until its hour is up,
    /// and never a moment longer. Nothing else is a bearer.
    #[test]
    fn a_minted_bearer_lasts_its_hour_and_then_names_nobody() {
        let oidc = oidc_test_authenticator(DEFAULT_OIDC_CLAIM, None);
        let principal = Principal::User {
            id: UserId::new("oidc-user").unwrap(),
            role: Role::Member,
        };
        let bearer = oidc
            .mint_bearer(principal.clone(), OIDC_BEARER_LIFETIME)
            .unwrap();
        assert!(bearer.starts_with(OIDC_BEARER_PREFIX));
        assert!(
            bearer.bytes().all(is_token_byte),
            "a bearer travels in a header and a subprotocol, so it stays in their alphabet"
        );
        assert_eq!(oidc.resolve(&bearer), Some(principal.clone()));
        assert_eq!(oidc.resolve(&format!("{bearer}x")), None);

        let expired = oidc.mint_bearer(principal, Duration::ZERO).unwrap();
        assert_eq!(oidc.resolve(&expired), None);
    }

    /// Decision 0087's cross-mode rule, both directions. An OIDC machine has
    /// no roster to find a static token in — except the bootstrap file it may
    /// be given, which is exactly the tokens in that file and no others — and
    /// a token-file machine holds nothing that could answer for a bearer only
    /// an OIDC machine mints.
    #[tokio::test]
    async fn an_oidc_machine_refuses_a_static_token_and_a_token_file_machine_refuses_a_bearer() {
        let bootstrap = TokenMap::parse(&format!("alice {ALICE_FIRST} admin\n")).unwrap();
        let oidc = PrincipalAuthenticator::Oidc(oidc_test_authenticator(
            DEFAULT_OIDC_CLAIM,
            Some(bootstrap),
        ));
        // The bootstrap file is the first administrator and the CLI, and it
        // is the only static credential an OIDC machine accepts.
        assert_eq!(
            oidc.resolve(ALICE_FIRST).await,
            Some(Principal::User {
                id: UserId::new("alice").unwrap(),
                role: Role::Admin,
            })
        );
        assert_eq!(oidc.resolve(BOB_TOKEN).await, None);
        // Nor does a bearer shaped like one it mints but never minted.
        assert_eq!(
            oidc.resolve(&format!("{OIDC_BEARER_PREFIX}{}", "a".repeat(43)))
                .await,
            None
        );

        let tokens = PrincipalAuthenticator::Static(
            TokenMap::parse(&format!("alice {ALICE_FIRST} admin\n")).unwrap(),
        );
        let bearer = oidc_test_authenticator(DEFAULT_OIDC_CLAIM, None)
            .mint_bearer(
                Principal::User {
                    id: UserId::new("oidc-user").unwrap(),
                    role: Role::Member,
                },
                OIDC_BEARER_LIFETIME,
            )
            .unwrap();
        assert_eq!(tokens.resolve(&bearer).await, None);
    }

    /// The third mode's boot rules: it needs its client credentials and a
    /// public URL to be returned to, it refuses to stand beside the gateway,
    /// and it does stand beside a token file.
    #[test]
    fn oidc_is_a_third_exclusive_mode_that_keeps_its_bootstrap_token_file() {
        let data_dir = tempfile::tempdir().unwrap();
        let tokens = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tokens.path(), format!("alice {ALICE_FIRST} admin\n")).unwrap();

        let mut config = tidebreak_core::Config::desktop(data_dir.path());
        config.profile = Profile::SelfHost;
        config.auth_oidc_issuer = Some("https://login.example.test".to_owned());
        // The issuer alone is not a configuration.
        assert!(PrincipalAuthenticator::from_config(&config).is_err());
        config.auth_oidc_client_id = Some("client-id".to_owned());
        config.auth_oidc_client_secret = Some("client-secret".to_owned());
        // Nowhere for the provider to return to.
        assert!(PrincipalAuthenticator::from_config(&config).is_err());
        config.public_url = Some("https://tidebreak.example.test".to_owned());
        assert!(matches!(
            PrincipalAuthenticator::from_config(&config),
            Ok(PrincipalAuthenticator::Oidc(_))
        ));

        // The token file stays the bootstrap: still OIDC mode, not static.
        config.auth_tokens_file = Some(tokens.path().to_owned());
        assert!(matches!(
            PrincipalAuthenticator::from_config(&config),
            Ok(PrincipalAuthenticator::Oidc(_))
        ));

        // The gateway is the one thing it cannot share a machine with, and
        // the refusal says which two variables to choose between.
        config.auth_gateway_url = Some("https://gateway.example.test".to_owned());
        let Err(refusal) = PrincipalAuthenticator::from_config(&config) else {
            panic!("a machine cannot be both an OIDC and a gateway machine");
        };
        let refusal = refusal.to_string();
        assert!(
            refusal.contains("TIDEBREAK_AUTH_OIDC_ISSUER")
                && refusal.contains("TIDEBREAK_AUTH_GATEWAY_URL"),
            "the refusal must name both: {refusal}"
        );
    }

    /// The issuer and every endpoint it names carry a credential, so plain
    /// HTTP is refused off loopback. The login claim is a claim name, not a
    /// path into one.
    #[test]
    fn an_oidc_provider_this_machine_would_not_trust_fails_boot() {
        for (issuer, claim, why) in [
            (
                "http://login.example.test",
                DEFAULT_OIDC_CLAIM,
                "plain http",
            ),
            (
                "ftp://login.example.test",
                DEFAULT_OIDC_CLAIM,
                "no scheme to speak",
            ),
            ("not a url", DEFAULT_OIDC_CLAIM, "not a URL"),
            (
                "https://user:pass@login.example.test",
                DEFAULT_OIDC_CLAIM,
                "credentials in the issuer",
            ),
            (
                "https://login.example.test?tenant=a",
                DEFAULT_OIDC_CLAIM,
                "a query in the issuer",
            ),
            ("https://login.example.test", "", "an empty claim name"),
            (
                "https://login.example.test",
                "profile/email",
                "a path rather than a claim name",
            ),
        ] {
            assert!(
                OidcAuthenticator::new(
                    issuer,
                    "client-id",
                    "client-secret",
                    claim,
                    "https://tidebreak.example.test",
                    None,
                )
                .is_err(),
                "{why} must fail boot"
            );
        }
        // Loopback is the development exception, on both sides.
        assert!(OidcAuthenticator::new(
            "http://localhost:12345",
            "client-id",
            "client-secret",
            DEFAULT_OIDC_CLAIM,
            "http://127.0.0.1:8080",
            None,
        )
        .is_ok());
        assert!(require_https_or_loopback(
            &reqwest::Url::parse("http://keys.example.test/jwks").unwrap(),
            "the OIDC jwks_uri",
        )
        .is_err());
    }
}
