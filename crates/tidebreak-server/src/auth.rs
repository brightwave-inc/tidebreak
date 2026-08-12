//! Bearer-token authentication for the API.
//!
//! Which credentials authenticate depends on the boot [`Profile`]:
//!
//! - **Desktop** binds to loopback with a per-launch token (see [`AppState`]);
//!   whoever presents it is the one person at the machine, so it resolves to
//!   [`Principal::LocalOwner`]. It's the one thing standing between the agent
//!   and any other local process that finds the port, so the check is
//!   mandatory on every non-health route.
//! - **Self-host** authenticates with static named bearer tokens from an
//!   operator-maintained file ([`TokenMap`]); each token names the configured
//!   user it belongs to, resolved to [`Principal::User`]. The per-launch token
//!   names nobody on a shared deployment and is not accepted there — a
//!   credential that names no one is rejected at this middleware (#853).
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
//! `docs/decisions/0004-self-host-deployment-plane-authorization.md`.
//!
//! ```text
//! # user-id  token                                              role
//! alice  4f9c0e9b2d5a4c1e8f7b6a5d4c3b2a1f0e9d8c7b6a5f4e3d2c1b0a99  admin
//! bob    0123456789abcdef0123456789abcdef0123456789abcdef01234567
//! ```
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

use axum::extract::{Request, State};
use axum::http::{
    header::{AUTHORIZATION, HOST, ORIGIN, SEC_WEBSOCKET_PROTOCOL, UPGRADE},
    HeaderMap, HeaderName, StatusCode,
};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tidebreak_core::{AgentError, Profile, Result};

use crate::principal::{AuthContext, Principal, Role, UserId};
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
    let resolved =
        extract_token(request.headers()).and_then(|presented| resolve_principal(&state, presented));

    match resolved {
        Some(principal) => {
            request.extensions_mut().insert(AuthContext {
                principal,
                client_executor: false,
            });
            next.run(request).await
        }
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

/// Map the presented credential to WHO is asking, per the boot profile.
///
/// `None` is a 401: a token that names no one admits no one. Unknown future
/// profiles resolve nobody until they choose an authenticator — fail closed,
/// never defaulted to the local owner.
fn resolve_principal(state: &AppState, presented: &str) -> Option<Principal> {
    match state.config.profile {
        // The per-launch bearer is loopback-only and handed to one client, so
        // the verified caller *is* the local owner.
        Profile::Desktop => constant_time_eq(presented.as_bytes(), state.token.as_bytes())
            .then_some(Principal::LocalOwner),
        // Every self-host credential names a configured user. The per-launch
        // bearer is deliberately not consulted: on a shared deployment it
        // names nobody, so it authenticates nobody.
        Profile::SelfHost => state
            .principal_tokens
            .resolve(presented)
            .map(|(id, role)| Principal::User { id, role }),
        _ => None,
    }
}

/// The self-host profile's static credential-to-principal mapping.
///
/// Loaded once at boot from the operator's token file (format in the module
/// docs). This is the seam a future authenticator (gateway-derived identity,
/// #578) replaces: whatever verifies the credential, the middleware's output
/// stays "which [`UserId`] is asking".
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
/// The credential is a capability, not a principal: it marks the bit on the
/// [`AuthContext`] the bearer middleware already attached, and fails closed if
/// no context is present — the native credential alone names nobody and must
/// never admit a request by itself.
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
            let Some(auth) = request.extensions().get::<AuthContext>().cloned() else {
                return StatusCode::UNAUTHORIZED.into_response();
            };
            request.extensions_mut().insert(AuthContext {
                client_executor: true,
                ..auth
            });
            next.run(request).await
        }
        _ => StatusCode::UNAUTHORIZED.into_response(),
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
/// macOS and Linux, `http(s)://tauri.localhost` on Windows. `null` is here
/// because a document on a non-http scheme is reported as an opaque origin by
/// some webviews, and refusing it would take the packaged app's own requests
/// down; it grants nothing a page could not already reach without an `Origin`
/// header at all.
const APP_ORIGINS: &[&str] = &[
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
    "null",
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
        assert!(origin_is_this_app(&HeaderMap::new()));
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
}
