//! Versioning, the attach handshake, transport-stable errors, and the explicit
//! result bounds every capability is measured against.
//!
//! The shape follows `openwave-host-broker`'s protocol module: a single
//! [`PROTOCOL_VERSION`] checked for *exact equality*, a [`Hello`](AttachRequest)
//! style handshake that is answered even across a version skew so a peer can
//! discover the mismatch, and a [`Response`] that carries either a typed success
//! or a safe, transport-stable [`ErrorResponse`].

use serde::{Deserialize, Serialize};

use crate::ids::{EventCursor, RunId};

/// Current sandbox-agent wire protocol.
///
/// Bump this for any incompatible wire change. The host checks it for exact
/// equality and refuses a differing peer, exactly as the host broker does; the
/// protocol is [UNSTABLE](crate) until a named release and offers no
/// negotiation window across versions.
pub const PROTOCOL_VERSION: u32 = 1;

/// Largest single reverse-RPC or event frame a conforming transport accepts,
/// so a peer cannot force unbounded buffering with one enormous frame.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Largest prompt one host-proxied model inference request may carry.
pub const MAX_MODEL_PROMPT_BYTES: usize = 512 * 1024;

/// Largest completion one host-proxied model inference result may carry.
pub const MAX_MODEL_COMPLETION_BYTES: usize = 512 * 1024;

/// Largest UTF-8 payload one sandbox event may carry.
pub const MAX_EVENT_PAYLOAD_BYTES: usize = 64 * 1024;

/// Largest number of un-acknowledged events the sandbox buffers before it
/// checkpoints and stops producing rather than dropping events.
pub const MAX_BUFFERED_EVENTS: usize = 4_096;

/// Largest number of artifacts one run may expose for collection.
pub const MAX_ARTIFACTS: usize = 256;

/// Largest single artifact the protocol returns as opaque bytes.
pub const MAX_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;

/// The host's request to attach to a provisioned sandbox.
///
/// `resume_from` is the host's last committed [`EventCursor`]; a fresh run
/// attaches at [`EventCursor::START`]. The handshake is the first thing that
/// crosses a new connection, before any event or reverse request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachRequest {
    /// The version the host speaks. The sandbox refuses a value it does not
    /// speak exactly.
    pub protocol_version: u32,
    /// The run this connection carries.
    pub run_id: RunId,
    /// Where to resume the event stream from.
    pub resume_from: EventCursor,
}

/// The sandbox supervisor's answer to an [`AttachRequest`].
///
/// It is returned even on a version mismatch so the host learns the sandbox's
/// version rather than a blank refusal — the same courtesy the broker's `Hello`
/// extends. A mismatch still denies the connection; the fields describe why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachAccepted {
    /// The version the sandbox speaks. Equal to the host's on an accepted
    /// attach; different on a refusal.
    pub protocol_version: u32,
    /// The reverse-RPC capabilities the sandbox may request over this
    /// connection, resolved deny-by-default from the run's grants.
    pub granted_capabilities: Vec<crate::reverse::Capability>,
    /// The highest sequence the sandbox currently holds, so the host can bound
    /// how far a resume will carry it.
    pub latest_sequence: Option<crate::ids::Sequence>,
}

/// Stable failure classes carried across the sandbox-agent boundary.
///
/// Every arm is transport-safe: it names a class, never a host-internal path,
/// credential, or raw OS error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorCode {
    /// The request did not match the host's exact [`PROTOCOL_VERSION`].
    ProtocolVersion,
    /// The requested capability was not granted to this run.
    Denied,
    /// The operation identity was reused for a different request.
    OperationIdConflict,
    /// An operation identity was found `Claimed` with no live execution — a
    /// crash-ambiguous external effect that must not be replayed.
    OperationAmbiguous,
    /// The in-flight request was cancelled over the control lane.
    Cancelled,
    /// The connection dropped before the response arrived.
    Disconnected,
    /// The request was structurally invalid.
    InvalidRequest,
    /// A result exceeded its declared per-capability bound.
    TooLarge,
    /// The named artifact does not exist.
    NotFound,
    /// The host could not settle the operation.
    Internal,
}

/// Safe error payload; it carries no host-internal detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorResponse {
    pub code: ErrorCode,
    pub message: String,
    /// Whether re-issuing the same operation identity may succeed later.
    pub retryable: bool,
}

impl ErrorResponse {
    /// Build a transport-stable error.
    #[must_use]
    pub fn new(code: ErrorCode, message: &str, retryable: bool) -> Self {
        Self {
            code,
            message: message.to_owned(),
            retryable,
        }
    }

    /// The canonical deny-by-default refusal.
    #[must_use]
    pub fn denied() -> Self {
        Self::new(
            ErrorCode::Denied,
            "capability is not granted to this run",
            false,
        )
    }
}

/// Typed success or a transport-stable error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum Response<T> {
    Ok(T),
    Error(ErrorResponse),
}

impl<T> Response<T> {
    /// Whether this response is a success.
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        matches!(self, Response::Ok(_))
    }
}

/// Whether `received` matches the exact protocol this build speaks.
///
/// # Errors
/// Returns a [`ErrorCode::ProtocolVersion`] response when the versions differ.
pub fn require_version(received: u32) -> Result<(), ErrorResponse> {
    if received == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ErrorResponse::new(
            ErrorCode::ProtocolVersion,
            "sandbox protocol version mismatch",
            false,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::Sequence;
    use crate::reverse::Capability;

    #[test]
    fn attach_handshake_roundtrips() {
        let request = AttachRequest {
            protocol_version: PROTOCOL_VERSION,
            run_id: RunId::new(),
            resume_from: EventCursor::committed(Sequence::new(7)),
        };
        let encoded = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<AttachRequest>(&encoded).unwrap(),
            request
        );

        let accepted = AttachAccepted {
            protocol_version: PROTOCOL_VERSION,
            granted_capabilities: vec![Capability::ModelInference],
            latest_sequence: Some(Sequence::new(7)),
        };
        let encoded = serde_json::to_string(&accepted).unwrap();
        assert_eq!(
            serde_json::from_str::<AttachAccepted>(&encoded).unwrap(),
            accepted
        );
    }

    #[test]
    fn version_check_is_exact_equality() {
        assert!(require_version(PROTOCOL_VERSION).is_ok());
        let error = require_version(PROTOCOL_VERSION + 1).unwrap_err();
        assert_eq!(error.code, ErrorCode::ProtocolVersion);
        assert!(!error.retryable);
    }

    #[test]
    fn response_tags_success_and_error_distinctly() {
        let ok: Response<u32> = Response::Ok(1);
        let encoded = serde_json::to_value(&ok).unwrap();
        assert_eq!(encoded["status"], "ok");

        let err: Response<u32> = Response::Error(ErrorResponse::denied());
        let encoded = serde_json::to_value(&err).unwrap();
        assert_eq!(encoded["status"], "error");
        assert_eq!(encoded["payload"]["code"], "denied");
    }
}
