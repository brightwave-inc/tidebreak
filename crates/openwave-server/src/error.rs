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
