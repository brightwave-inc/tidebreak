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

    /// A `401 Unauthorized` for a missing or unknown session-scoped token.
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            info: AgentErrorInfo {
                kind: "unauthorized".to_string(),
                message: message.into(),
            },
        }
    }

    /// A `403 Forbidden` for a caller whose token is real but whose authority
    /// no longer covers the request (for example, an ended session).
    pub(crate) fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            info: AgentErrorInfo {
                kind: "forbidden".to_string(),
                message: message.into(),
            },
        }
    }

    /// A `501 Not Implemented` for a capability this embedding does not
    /// provide (for example, browser routes with no runtime attached).
    pub(crate) fn not_implemented(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_IMPLEMENTED,
            info: AgentErrorInfo {
                kind: "not_implemented".to_string(),
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

    /// Keep secret-store setup errors that an operator can fix while redacting
    /// all other backend details behind a route-specific fallback message.
    pub(crate) fn credential_storage(error: AgentError, fallback: impl Into<String>) -> Self {
        if matches!(error, AgentError::Config(_)) {
            Self::from(error)
        } else {
            Self::internal(fallback)
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

/// Memory errors carry product meaning at the route boundary.
impl From<tidebreak_core::MemoryError> for ServerError {
    fn from(error: tidebreak_core::MemoryError) -> Self {
        use tidebreak_core::{MemoryCapability, MemoryError};

        match &error {
            MemoryError::Unsupported(MemoryCapability::Extraction) => {
                Self::not_implemented(error.to_string())
            }
            MemoryError::Unsupported(_) => Self::not_implemented(error.to_string()),
            MemoryError::InvalidRecord(_)
            | MemoryError::EvidenceNotFound(_)
            | MemoryError::ScopeNotFound => Self::bad_request(error.to_string()),
            MemoryError::NotFound => Self::not_found("memory record not found"),
            MemoryError::AlreadyExists => Self::conflict("memory record already exists"),
            MemoryError::RevisionConflict { .. } => {
                Self::conflict_kind("memory_revision_conflict", error.to_string())
            }
            MemoryError::InvalidStatusTransition { .. } => {
                Self::conflict_kind("memory_status_conflict", error.to_string())
            }
            MemoryError::ActiveRecordCapExceeded { .. } | MemoryError::DigestCapExceeded { .. } => {
                Self::conflict_kind("memory_cap_exceeded", error.to_string())
            }
            MemoryError::Backend(_) => Self::internal("memory storage failed"),
            _ => Self::internal("memory storage failed"),
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

    #[test]
    fn credential_storage_keeps_setup_guidance_and_redacts_backend_failures() {
        let setup = ServerError::credential_storage(
            AgentError::config("set TIDEBREAK_VAULT_ADDR"),
            "credential storage is unavailable",
        );
        assert_eq!(setup.kind(), "config");
        assert!(setup.message().contains("TIDEBREAK_VAULT_ADDR"));

        let backend = ServerError::credential_storage(
            AgentError::Secret("private keychain detail".into()),
            "credential storage is unavailable",
        );
        assert_eq!(backend.kind(), "internal");
        assert_eq!(backend.message(), "credential storage is unavailable");
    }
}
