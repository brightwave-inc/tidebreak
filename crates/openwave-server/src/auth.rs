//! Bearer-token authentication for the local API.
//!
//! The server binds to loopback with a per-launch token (see [`AppState`]); a
//! middleware layer rejects any request that doesn't present it. It's the one
//! thing standing between the agent and any other local process that finds the
//! port, so the check is mandatory on every non-health route.
//!
//! Browsers can't set an `Authorization` header on a WebSocket upgrade, so on
//! upgrade requests the token is also accepted via `Sec-WebSocket-Protocol` as
//! `openwave-token.<token>` (alongside the handshake subprotocol `openwave-v1`).
//! Non-browser clients keep using `Authorization: Bearer`. Subprotocol auth is
//! ignored on ordinary HTTP requests.

use axum::extract::{Request, State};
use axum::http::{
    header::{AUTHORIZATION, SEC_WEBSOCKET_PROTOCOL, UPGRADE},
    HeaderMap, HeaderName, StatusCode,
};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

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

/// Reject requests without a valid bearer token — from
/// `Authorization: Bearer <token>`, or (on WebSocket upgrades only)
/// `Sec-WebSocket-Protocol: openwave-token.<token>`.
pub async fn require_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let presented = extract_token(request.headers());

    match presented {
        Some(token) if constant_time_eq(token.as_bytes(), state.token.as_bytes()) => {
            next.run(request).await
        }
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
}

/// Require the second credential held only by the trusted native executor.
pub async fn require_client_executor_token(
    State(state): State<AppState>,
    request: Request,
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
            next.run(request).await
        }
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
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
