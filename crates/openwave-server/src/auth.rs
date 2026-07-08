//! Bearer-token authentication for the local API.
//!
//! The server binds to loopback with a per-launch token (see [`AppState`]); a
//! middleware layer rejects any request that doesn't present it. It's the one
//! thing standing between the agent and any other local process that finds the
//! port, so the check is mandatory on every non-health route.

use axum::extract::{Request, State};
use axum::http::{header::AUTHORIZATION, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::state::AppState;

/// Reject requests without a valid `Authorization: Bearer <token>` header.
pub async fn require_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let presented = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));

    match presented {
        Some(token) if constant_time_eq(token.as_bytes(), state.token.as_bytes()) => {
            next.run(request).await
        }
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
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
