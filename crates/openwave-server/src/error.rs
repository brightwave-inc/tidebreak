//! The HTTP error type.
//!
//! Handlers return `Result<_, ServerError>`; a [`ServerError`] carries an HTTP
//! status plus the serializable [`AgentErrorInfo`] (the same error projection the
//! event stream uses), so a failed request answers with a stable
//! `{ "kind", "message" }` body a client can branch on.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use openwave_core::{AgentError, AgentErrorInfo};

/// An HTTP-facing error: a status code and a serializable description.
pub struct ServerError {
    status: StatusCode,
    info: AgentErrorInfo,
}

impl ServerError {
    /// A `404 Not Found` for a missing resource.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            info: AgentErrorInfo {
                kind: "not_found".to_string(),
                message: message.into(),
            },
        }
    }

    /// A `400 Bad Request` for a malformed request (bad path segment, or an
    /// unparseable / wrong-typed / wrong-content-type body). Used to map axum's
    /// built-in extractor rejections into the same `{ kind, message }` shape as
    /// every other error, so a client can always parse the body as JSON.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            info: AgentErrorInfo {
                kind: "bad_request".to_string(),
                message: message.into(),
            },
        }
    }

    /// A `413 Payload Too Large` for a request that exceeds an endpoint's
    /// explicit body-size limit.
    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            info: AgentErrorInfo {
                kind: "payload_too_large".to_string(),
                message: message.into(),
            },
        }
    }

    /// A `409 Conflict` for a request that clashes with current state (e.g. a
    /// turn is already running for the chat).
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            info: AgentErrorInfo {
                kind: "conflict".to_string(),
                message: message.into(),
            },
        }
    }

    /// A `409 Conflict` with a route-specific stable machine-readable kind.
    pub(crate) fn conflict_kind(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            info: AgentErrorInfo {
                kind: kind.to_owned(),
                message: message.into(),
            },
        }
    }

    /// A `500 Internal Server Error` for an unexpected server-side failure that
    /// isn't an [`AgentError`] (e.g. a retrieval backend fault). Carries a stable
    /// `kind` so a client sees the same `{ kind, message }` shape as any error.
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            info: AgentErrorInfo {
                kind: "internal".to_string(),
                message: message.into(),
            },
        }
    }
}

/// Core errors surfacing from a handler are internal failures (a store write
/// failed, a provider errored); map them to `500` with their info preserved.
impl From<AgentError> for ServerError {
    fn from(err: AgentError) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            info: (&err).into(),
        }
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        (self.status, Json(self.info)).into_response()
    }
}
