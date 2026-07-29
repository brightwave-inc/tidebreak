//! Versioned wire types for the reverse-RPC callback channel.
//!
//! This mirrors the `openwave-host-broker` envelope shape deliberately: a
//! versioned protocol with an exact-equality version check, a deny-by-default
//! capability gate, a durable per-run operation identity, and a bounded typed
//! result or a transport-stable error. The identity newtypes are re-derived
//! locally rather than imported so the spike stays standalone; the real
//! protocol step (#822) should share the broker's identity and envelope
//! conventions rather than fork them.

use std::{fmt, str::FromStr};

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Current reverse-RPC protocol. Bumped for any incompatible wire change and
/// checked for exact equality, exactly as the host broker checks its own.
pub const PROTOCOL_VERSION: u32 = 1;

/// A broker-style opaque identity refused a nil sentinel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("reverse-rpc identities must not be nil")]
pub struct NilIdentity;

macro_rules! uuid_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Allocate a fresh random identity.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Return the underlying UUID.
            #[must_use]
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(value)?))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let id = Uuid::deserialize(deserializer)?;
                if id.is_nil() {
                    Err(D::Error::custom(NilIdentity))
                } else {
                    Ok(Self(id))
                }
            }
        }
    };
}

uuid_id!(
    OperationId,
    "Durable per-run identity for one idempotent reverse operation.\n\nStable across reconnects: a request re-issued after a disconnect carries the\nsame `OperationId` and is answered from the recorded outcome, never executed\ntwice."
);
uuid_id!(
    RequestId,
    "Per-attempt correlation identity for one request/response pair.\n\nA re-issue of the same operation over a fresh connection uses a new\n`RequestId` but the same `OperationId`."
);

/// Host-mediated capabilities the sandbox may request back over the channel.
///
/// Model inference is the first and, for the spike, only carried capability:
/// a sandbox-resident loop cannot take a single step without it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Capability {
    /// Run one model completion through the host, which is the model proxy.
    ModelInference,
}

/// Parameters for one host-proxied model completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelInferenceParams {
    pub prompt: String,
}

/// Bounded result of one host-proxied model completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelInferenceResult {
    pub completion: String,
}

/// Capability-checked reverse request the sandbox issues to the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "capability",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[non_exhaustive]
pub enum ReverseRequest {
    ModelInference(ModelInferenceParams),
}

impl ReverseRequest {
    /// Capability this request is gated on, for deny-by-default authorization.
    #[must_use]
    pub const fn capability(&self) -> Capability {
        match self {
            ReverseRequest::ModelInference(_) => Capability::ModelInference,
        }
    }
}

/// Bounded typed success for one reverse request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "capability", content = "payload", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReverseResult {
    ModelInference(ModelInferenceResult),
}

/// Stable failure classes carried over the reverse channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorCode {
    /// The request envelope did not match the host's exact protocol version.
    ProtocolVersion,
    /// The requested capability was not granted to this run.
    Denied,
    /// The operation identity was reused for a different request.
    OperationIdConflict,
    /// The in-flight request was cancelled by the sandbox.
    Cancelled,
    /// The host could not settle the operation.
    Internal,
}

/// Transport-stable error payload; carries no host-internal detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorResponse {
    pub code: ErrorCode,
    pub message: String,
    /// Whether re-issuing the same operation identity may succeed later.
    pub retryable: bool,
}

impl ErrorResponse {
    pub(crate) fn new(code: ErrorCode, message: &str, retryable: bool) -> Self {
        Self {
            code,
            message: message.to_owned(),
            retryable,
        }
    }
}

/// Typed success or a transport-stable error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum Response<T> {
    Ok(T),
    Error(ErrorResponse),
}

/// A reverse request the sandbox supervisor multiplexes toward the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReverseEnvelope {
    pub protocol_version: u32,
    pub request_id: RequestId,
    pub operation_id: OperationId,
    pub request: ReverseRequest,
}

/// The host's correlated answer to one reverse request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReverseResponseEnvelope {
    pub protocol_version: u32,
    pub request_id: RequestId,
    pub operation_id: OperationId,
    pub response: Response<ReverseResult>,
}

/// A cancellation of an in-flight reverse request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelFrame {
    pub request_id: RequestId,
    pub operation_id: OperationId,
}

/// One unit multiplexed over the single bidirectional connection.
///
/// Requests and cancellations flow sandbox -> host; responses flow host ->
/// sandbox. All three share one framed byte stream, so correlation is by
/// `request_id` rather than by a dedicated socket per call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "frame",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Frame {
    Request(ReverseEnvelope),
    Response(ReverseResponseEnvelope),
    Cancel(CancelFrame),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_reject_nil() {
        let encoded = format!("\"{}\"", Uuid::nil());
        assert!(serde_json::from_str::<OperationId>(&encoded).is_err());
        assert!(serde_json::from_str::<RequestId>(&encoded).is_err());
    }

    #[test]
    fn frames_roundtrip_over_json() {
        let frame = Frame::Request(ReverseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(),
            operation_id: OperationId::new(),
            request: ReverseRequest::ModelInference(ModelInferenceParams {
                prompt: "hello".to_owned(),
            }),
        });
        let encoded = serde_json::to_string(&frame).unwrap();
        assert_eq!(serde_json::from_str::<Frame>(&encoded).unwrap(), frame);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let encoded = format!(
            r#"{{"protocol_version":1,"request_id":"{}","operation_id":"{}","request":{{"capability":"model_inference","payload":{{"prompt":"hi","extra":true}}}}}}"#,
            RequestId::new(),
            OperationId::new(),
        );
        assert!(serde_json::from_str::<ReverseEnvelope>(&encoded).is_err());
    }
}
