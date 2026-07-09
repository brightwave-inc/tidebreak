//! Bearer-token authentication for the local API.
//!
//! The server binds to loopback with a per-launch token (see [`AppState`]); a
//! middleware layer rejects any request that doesn't present it. It's the one
//! thing standing between the agent and any other local process that finds the
//! port, so the check is mandatory on every non-health route.
//!
//! Browsers can't set an `Authorization` header on a WebSocket upgrade, so the
//! token is also accepted via `Sec-WebSocket-Protocol` as
//! `openwave-token.<token>` (alongside the handshake subprotocol `openwave-v1`).
//! Non-browser clients keep using `Authorization: Bearer`.

use axum::extract::{Request, State};
use axum::http::{
    header::{AUTHORIZATION, SEC_WEBSOCKET_PROTOCOL},
    HeaderMap, StatusCode,
};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::state::AppState;

/// Handshake subprotocol the server selects when auth came via subprotocol.
/// Clients that pass the token this way MUST offer this value alongside
/// [`WS_TOKEN_SUBPROTOCOL_PREFIX`] so the browser accepts the handshake.
pub const WS_HANDSHAKE_SUBPROTOCOL: &str = "openwave-v1";

/// Prefix for the subprotocol entry that carries the bearer token.
pub const WS_TOKEN_SUBPROTOCOL_PREFIX: &str = "openwave-token.";

/// Reject requests without a valid bearer token — from
/// `Authorization: Bearer <token>` or `Sec-WebSocket-Protocol: openwave-token.<token>`.
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

/// Resolve the presented token: `Authorization` wins, then the WS subprotocol.
pub fn extract_token(headers: &HeaderMap) -> Option<&str> {
    if let Some(token) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|token| !token.is_empty())
    {
        return Some(token);
    }
    extract_token_from_subprotocols(headers)
}

/// Extract the access token from `Sec-WebSocket-Protocol`.
///
/// Clients offer `openwave-token.<token>` alongside `openwave-v1` so the token
/// never appears in the request URL.
pub fn extract_token_from_subprotocols(headers: &HeaderMap) -> Option<&str> {
    let header_value = headers.get(SEC_WEBSOCKET_PROTOCOL)?.to_str().ok()?;
    for entry in header_value.split(',') {
        let trimmed = entry.trim();
        if let Some(token) = trimmed.strip_prefix(WS_TOKEN_SUBPROTOCOL_PREFIX) {
            if !token.is_empty() {
                return Some(token);
            }
        }
    }
    None
}

/// Whether the client offered the handshake subprotocol (so the upgrade should
/// select it in the response).
pub fn offered_handshake_subprotocol(headers: &HeaderMap) -> bool {
    let Some(header_value) = headers
        .get(SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    header_value
        .split(',')
        .any(|entry| entry.trim() == WS_HANDSHAKE_SUBPROTOCOL)
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
    fn extracts_token_from_subprotocol() {
        let mut headers = HeaderMap::new();
        headers.insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("openwave-v1, openwave-token.abc.def"),
        );
        assert_eq!(extract_token_from_subprotocols(&headers), Some("abc.def"));
        assert_eq!(extract_token(&headers), Some("abc.def"));
        assert!(offered_handshake_subprotocol(&headers));
    }

    #[test]
    fn authorization_wins_over_subprotocol() {
        let mut headers = HeaderMap::new();
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
}
