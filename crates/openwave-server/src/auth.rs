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
//! `OPENWAVE_AUTH_TOKENS_FILE` points at a plain-text file, one mapping per
//! line — `<user-id> <token>`, whitespace-separated. Blank lines and lines
//! starting with `#` are ignored. A user may hold several tokens (rotation);
//! a token may name only one user, and duplicates fail the load. Tokens are
//! opaque secrets matched exactly (no hashing scheme to misconfigure): at
//! least 16 characters from `[A-Za-z0-9._~-]`, so they stay valid in both the
//! `Authorization` header and the WebSocket subprotocol below. Generate them
//! with e.g. `openssl rand -hex 32`. Gateway-derived identity (#578) later
//! replaces this file behind the same credential-to-principal seam.
//!
//! ```text
//! # user-id  token
//! alice  4f9c0e9b2d5a4c1e8f7b6a5d4c3b2a1f
//! bob    0123456789abcdef0123456789abcdef
//! ```
//!
//! Browsers can't set an `Authorization` header on a WebSocket upgrade, so on
//! upgrade requests the token is also accepted via `Sec-WebSocket-Protocol` as
//! `openwave-token.<token>` (alongside the handshake subprotocol `openwave-v1`).
//! Non-browser clients keep using `Authorization: Bearer`. Subprotocol auth is
//! ignored on ordinary HTTP requests.

use std::path::Path;

use axum::extract::{Request, State};
use axum::http::{
    header::{AUTHORIZATION, HOST, ORIGIN, SEC_WEBSOCKET_PROTOCOL, UPGRADE},
    HeaderMap, HeaderName, StatusCode,
};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use openwave_core::{AgentError, Profile, Result};

use crate::principal::{AuthContext, Principal, UserId};
use crate::state::AppState;

/// Handshake subprotocol the server selects when the client offered it.
/// Clients that pass the token via subprotocol MUST offer this value alongside
/// [`WS_TOKEN_SUBPROTOCOL_PREFIX`] so the browser accepts the handshake.
pub const WS_HANDSHAKE_SUBPROTOCOL: &str = "openwave-v1";

/// Prefix for the subprotocol entry that carries the bearer token.
/// The per-launch token is a UUID (no commas/spaces), so it is a valid
/// subprotocol token-char sequence.
pub const WS_TOKEN_SUBPROTOCOL_PREFIX: &str = "openwave-token.";

/// Native-only credential header for claim, heartbeat, and resolve mutations.
pub const CLIENT_EXECUTOR_HEADER: HeaderName =
    HeaderName::from_static("x-openwave-client-executor");

/// Reject requests whose bearer token does not resolve to a principal — from
/// `Authorization: Bearer <token>`, or (on WebSocket upgrades only)
/// `Sec-WebSocket-Protocol: openwave-token.<token>`.
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
            .map(Principal::User),
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
    /// `(token, user)` pairs; tokens are unique, users may repeat.
    entries: Vec<(Box<str>, UserId)>,
}

/// Tokens shorter than this are refused at load: a guessable credential names
/// someone without authenticating them.
const MIN_TOKEN_LEN: usize = 16;

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
    /// weak or header-unsafe tokens, duplicate tokens, and a file that names
    /// nobody — an empty authenticator must fail loudly at boot, not admit
    /// nobody silently.
    pub fn parse(text: &str) -> Result<Self> {
        let mut entries: Vec<(Box<str>, UserId)> = Vec::new();
        for (index, raw) in text.lines().enumerate() {
            let line_no = index + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split_whitespace();
            let (Some(user), Some(token), None) = (fields.next(), fields.next(), fields.next())
            else {
                return Err(AgentError::config(format!(
                    "auth tokens file line {line_no}: expected `<user-id> <token>`"
                )));
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
                .any(|(existing, _)| existing.as_ref() == token)
            {
                return Err(AgentError::config(format!(
                    "auth tokens file line {line_no}: duplicate token"
                )));
            }
            entries.push((token.into(), user));
        }
        if entries.is_empty() {
            return Err(AgentError::config(
                "auth tokens file names no principals; a self-host server nobody can \
                 authenticate to must not start",
            ));
        }
        Ok(Self { entries })
    }

    /// The user the presented credential names, if any. Exact match; every
    /// entry is compared in constant time regardless of where a match lands.
    pub fn resolve(&self, presented: &str) -> Option<UserId> {
        let mut resolved = None;
        for (token, user) in &self.entries {
            if constant_time_eq(token.as_bytes(), presented.as_bytes()) && resolved.is_none() {
                resolved = Some(user.clone());
            }
        }
        resolved
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
/// The desktop profile is also what a bare `openwave serve` boots, so an
/// integrator driving that daemon from a browser page of their own must run it
/// as `OPENWAVE_PROFILE=self_host`. A parent process reading the daemon's
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
/// fall back to the `openwave-token.` subprotocol entry.
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
/// Clients offer `openwave-token.<token>` alongside `openwave-v1` so the token
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
/// if they offered `openwave-v1`, RFC 6455 expects a selection.
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

    #[test]
    fn token_map_parses_the_documented_format_and_resolves_exactly() {
        let map = TokenMap::parse(
            "# staff\n\nalice aaaaaaaaaaaaaaaa\nbob\tbbbbbbbbbbbbbbbb\nalice cccccccccccccccc\n",
        )
        .unwrap();
        let alice = UserId::new("alice").unwrap();
        assert_eq!(map.resolve("aaaaaaaaaaaaaaaa"), Some(alice.clone()));
        assert_eq!(
            map.resolve("cccccccccccccccc"),
            Some(alice),
            "a user may hold several tokens"
        );
        assert_eq!(
            map.resolve("bbbbbbbbbbbbbbbb"),
            Some(UserId::new("bob").unwrap())
        );
        assert_eq!(map.resolve("dddddddddddddddd"), None);
        assert_eq!(
            map.resolve("aaaaaaaaaaaaaaa"),
            None,
            "prefixes do not match"
        );
    }

    #[test]
    fn token_map_rejects_files_that_cannot_name_someone_safely() {
        for (text, why) in [
            ("", "names nobody"),
            ("# only comments\n", "names nobody"),
            ("alice\n", "missing token"),
            ("alice aaaaaaaaaaaaaaaa extra\n", "trailing field"),
            ("alice short\n", "guessably short token"),
            ("alice aaaaaaaa,aaaaaaaa\n", "header-unsafe token character"),
            ("al!ce aaaaaaaaaaaaaaaa\n", "invalid user id"),
            (
                "alice aaaaaaaaaaaaaaaa\nbob aaaaaaaaaaaaaaaa\n",
                "one token must name one user",
            ),
        ] {
            assert!(TokenMap::parse(text).is_err(), "{why}: {text:?}");
        }
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
            HeaderValue::from_static("openwave-v1, openwave-token.abc.def"),
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
            HeaderValue::from_static("openwave-v1, openwave-token.abc.def"),
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
            HeaderValue::from_static("openwave-v1, openwave-token.from-proto"),
        );
        assert_eq!(extract_token(&headers), Some("from-header"));
    }

    #[test]
    fn ignores_empty_token_subprotocol() {
        let mut headers = HeaderMap::new();
        headers.insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("openwave-v1, openwave-token."),
        );
        assert!(extract_token_from_subprotocols(&headers).is_none());
    }

    #[test]
    fn ignores_subprotocols_without_token_entry() {
        let mut headers = HeaderMap::new();
        headers.insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("openwave-v1"),
        );
        assert!(extract_token_from_subprotocols(&headers).is_none());
        assert!(offered_handshake_subprotocol(&headers));
    }

    #[test]
    fn reads_token_across_multiple_protocol_headers() {
        let mut headers = HeaderMap::new();
        headers.append(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("openwave-v1"),
        );
        headers.append(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("openwave-token.split-token"),
        );
        assert_eq!(
            extract_token_from_subprotocols(&headers),
            Some("split-token")
        );
        assert!(offered_handshake_subprotocol(&headers));
    }
}
