//! The HTTP error type.
//!
//! Handlers return `Result<_, ServerError>`; a [`ServerError`] carries an HTTP
//! status plus the serializable [`AgentErrorInfo`] (the same error projection the
//! event stream uses), so a failed request answers with a stable
//! `{ "kind", "message" }` body a client can branch on.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use tidebreak_core::{AgentError, AgentErrorInfo};

/// An HTTP-facing error: a status code and a serializable description.
#[derive(Debug)]
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

    /// A `400 Bad Request` with a route-specific stable machine-readable kind.
    pub(crate) fn bad_request_kind(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            info: AgentErrorInfo {
                kind: kind.to_owned(),
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

    /// The stable machine-readable `kind` clients branch on.
    #[cfg(test)]
    pub(crate) fn kind(&self) -> &str {
        &self.info.kind
    }

    /// The human-readable message, for surfaces that log rather than respond.
    pub(crate) fn message(&self) -> &str {
        &self.info.message
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

    /// A `415 Unsupported Media Type` with a route-specific stable kind.
    pub(crate) fn unsupported_media_type_kind(
        kind: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
            info: AgentErrorInfo {
                kind: kind.to_owned(),
                message: message.into(),
            },
        }
    }

    /// A `422 Unprocessable Entity` with a route-specific stable kind.
    pub(crate) fn unprocessable_kind(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            info: AgentErrorInfo {
                kind: kind.to_owned(),
                message: message.into(),
            },
        }
    }

    /// A `429 Too Many Requests` with a route-specific stable kind.
    pub(crate) fn too_many_requests_kind(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            info: AgentErrorInfo {
                kind: kind.to_owned(),
                message: message.into(),
            },
        }
    }

    /// A `500 Internal Server Error` for an unexpected server-side failure that
    /// isn't an [`AgentError`] (for example, another subsystem fault). Carries a stable
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

/// Core errors surfacing from a handler are normally internal failures. The
/// typed missing-project result is the deliberate exception: a project-scoped
/// write may lose a concurrent deletion race after an adapter's preflight, and
/// that remains a product-facing `404` rather than a database failure.
impl From<AgentError> for ServerError {
    fn from(err: AgentError) -> Self {
        let status = if matches!(err, AgentError::ProjectNotFound(_)) {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        Self {
            status,
            info: (&err).into(),
        }
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        (self.status, Json(self.info)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_write_that_loses_deletion_maps_to_not_found() {
        let project_id = tidebreak_core::ProjectId::new();
        let error = ServerError::from(AgentError::ProjectNotFound(project_id));
        assert_eq!(error.status, StatusCode::NOT_FOUND);
        assert_eq!(error.info.kind, "not_found");
        assert!(error.info.message.contains(&project_id.to_string()));
    }
}
