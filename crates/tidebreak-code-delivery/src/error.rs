//! HTTP-neutral delivery failures.

/// How the server maps a delivery failure onto its route response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryErrorStatus {
    BadRequest,
    Conflict,
    NotFound,
    Internal,
}

/// A stable error kind and message with an HTTP-neutral status class.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct DeliveryError {
    pub status: DeliveryErrorStatus,
    pub kind: &'static str,
    pub message: String,
}

impl DeliveryError {
    pub fn kind(&self) -> &str {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::bad_request_kind("bad_request", message)
    }

    pub fn bad_request_kind(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: DeliveryErrorStatus::BadRequest,
            kind,
            message: message.into(),
        }
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::conflict_kind("conflict", message)
    }

    pub fn conflict_kind(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: DeliveryErrorStatus::Conflict,
            kind,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: DeliveryErrorStatus::NotFound,
            kind: "not_found",
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: DeliveryErrorStatus::Internal,
            kind: "internal",
            message: message.into(),
        }
    }
}

impl From<tidebreak_core::AgentError> for DeliveryError {
    fn from(error: tidebreak_core::AgentError) -> Self {
        Self::internal(error.to_string())
    }
}
