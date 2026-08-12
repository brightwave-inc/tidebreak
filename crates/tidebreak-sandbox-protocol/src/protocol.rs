//! Versioning, the attach handshake, transport-stable errors, and the explicit
//! result bounds every capability is measured against.
//!
//! The shape follows `tidebreak-host-broker`'s protocol module: a single
//! [`PROTOCOL_VERSION`] checked for *exact equality*, a [`Hello`](AttachRequest)
//! style handshake that is answered even across a version skew so a peer can
//! discover the mismatch, and a [`Response`] that carries either a typed success
//! or a safe, transport-stable [`ErrorResponse`].

use serde::{Deserialize, Serialize};

use crate::ids::{EventCursor, RunId};
use crate::provisioning::TransportSecret;

/// Current sandbox-agent wire protocol.
///
/// Bump this for any incompatible wire change. The host checks it for exact
/// equality and refuses a differing peer, exactly as the host broker does; the
/// protocol is [UNSTABLE](crate) until a named release and offers no
/// negotiation window across versions.
///
/// Version 2 added authenticated attach, version 3 added run initialization,
/// version 4 added sandbox acknowledgement of consumed reverse responses so the
/// host can evict replay bodies safely, and version 5 adds host -> sandbox
/// [steering](crate::steer) of a live run.
///
/// A new frame is an incompatible change here even though it is additive: the
/// wire envelope names a closed set of lanes and refuses an unknown one rather
/// than skipping it, so a peer that has never heard of a lane must be turned
/// away at the handshake — where the refusal is legible — instead of dropping a
/// frame it cannot parse mid-run.
pub const PROTOCOL_VERSION: u32 = 5;

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

/// Largest number of reverse requests one connection keeps in flight before the
/// request lane applies backpressure. The reserved control lane
/// ([`ControlFrame`](crate::reverse::ControlFrame)) is not subject to this bound,
/// so a cancel or a heartbeat preempts a saturated request backlog.
pub const MAX_INFLIGHT_REQUESTS: usize = 16;

/// The host's request to attach to a provisioned sandbox.
///
/// `resume_from` is the host's last committed [`EventCursor`]; a fresh run
/// attaches at [`EventCursor::START`]. The handshake is the first thing that
/// crosses a new connection, before any event or reverse request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachRequest {
    /// The version the host speaks. The sandbox refuses a value it does not
    /// speak exactly.
    pub protocol_version: u32,
    /// The run this connection carries.
    pub run_id: RunId,
    /// Where to resume the event stream from.
    pub resume_from: EventCursor,
    /// The per-run transport secret the host presents to authenticate the dial.
    ///
    /// The sandbox verifies it against the secret it was configured with —
    /// [after](handshake) the version check and *before* it installs the
    /// connection or serves any capability — and refuses a mismatch with
    /// [`ErrorCode::Unauthenticated`]. It is an opaque bearer token carried within
    /// the frame bound; it is never logged (its [`Debug`] is redacting). An
    /// omitted token decodes, via `#[serde(default)]`, to the empty secret, which
    /// authenticates against nothing and is refused — a naive or older peer that
    /// sends no token gets a clean auth refusal rather than a frame-parse drop.
    #[serde(default)]
    pub transport_secret: TransportSecret,
}

/// The sandbox supervisor's answer to an [`AttachRequest`], as an on-wire frame.
///
/// The sandbox always answers a handshake: it [`Accepted`](HandshakeResponse::Accepted)
/// the connection, or it [`Refused`](HandshakeResponse::Refused) it and returns
/// the sandbox's own version so the host learns the mismatch rather than getting
/// a blank refusal — the same courtesy the broker's `Hello` extends. A refusal
/// still leaves the connection unusable. Backends compute this with
/// [`handshake`]; the reference backend surfaces a refusal as
/// [`ConnectError::VersionRefused`](crate::reference::ConnectError::VersionRefused).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandshakeResponse {
    /// The versions matched exactly; the connection is established.
    Accepted(AttachAccepted),
    /// The versions did not match; the connection is refused.
    Refused(AttachRefused),
}

/// The accepted-attach frame the sandbox returns when versions match exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachAccepted {
    /// The version the sandbox speaks; equal to the host's on an accepted attach.
    pub protocol_version: u32,
    /// The reverse-RPC capabilities the sandbox may request over this
    /// connection, resolved deny-by-default from the run's grants.
    pub granted_capabilities: Vec<crate::reverse::Capability>,
    /// The highest sequence the sandbox currently holds, so the host can bound
    /// how far a resume will carry it.
    pub latest_sequence: Option<crate::ids::Sequence>,
}

/// The refused-attach frame the sandbox returns on a version mismatch.
///
/// It carries the sandbox's own version so the host — or a third-party backend's
/// host — learns what the peer speaks. The connection is not established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachRefused {
    /// The version the sandbox speaks (equal to the host's on an authentication
    /// refusal, different on a version refusal).
    pub protocol_version: u32,
    /// Why the attach was refused: [`ErrorCode::ProtocolVersion`] on a version
    /// skew, or [`ErrorCode::Unauthenticated`] when the presented transport secret
    /// did not match.
    pub code: ErrorCode,
}

/// Compute the sandbox's handshake answer for one attach — the canonical answer
/// a backend returns, so a third-party implementation has an exact example.
///
/// Two gates, in this order:
///
/// 1. **Version**, exact-equality, mirroring the host broker: a skew refuses with
///    [`ErrorCode::ProtocolVersion`] and returns the sandbox's version, before
///    authentication is even considered — so neither gate leaks signal about the
///    other.
/// 2. **Authentication**: the presented [`AttachRequest::transport_secret`] is
///    compared, in constant time, against `expected_secret`. A mismatch — or a
///    sandbox with no configured secret (`expected_secret` is `None`), which
///    authenticates nothing and so **fails closed** — refuses with
///    [`ErrorCode::Unauthenticated`]. The refusal carries no secret.
///
/// Only when both gates pass is the connection accepted. A caller installs the
/// connection or serves a capability *only* on [`HandshakeResponse::Accepted`].
#[must_use]
pub fn handshake(
    request: &AttachRequest,
    sandbox_version: u32,
    expected_secret: Option<&TransportSecret>,
    granted_capabilities: Vec<crate::reverse::Capability>,
    latest_sequence: Option<crate::ids::Sequence>,
) -> HandshakeResponse {
    if request.protocol_version != sandbox_version {
        return HandshakeResponse::Refused(AttachRefused {
            protocol_version: sandbox_version,
            code: ErrorCode::ProtocolVersion,
        });
    }
    let authenticated =
        expected_secret.is_some_and(|secret| secret.verify(&request.transport_secret));
    if !authenticated {
        return HandshakeResponse::Refused(AttachRefused {
            protocol_version: sandbox_version,
            code: ErrorCode::Unauthenticated,
        });
    }
    HandshakeResponse::Accepted(AttachAccepted {
        protocol_version: sandbox_version,
        granted_capabilities,
        latest_sequence,
    })
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
    /// The presented per-run transport secret did not match the sandbox's, or no
    /// token was presented. The attach is refused before the connection is
    /// installed or any capability is served.
    Unauthenticated,
    /// The requested capability was not granted to this run.
    Denied,
    /// The operation identity was reused for a different request.
    OperationIdConflict,
    /// An operation identity was found `Claimed` with no live execution — a
    /// crash-ambiguous external effect that must not be replayed.
    OperationAmbiguous,
    /// The operation completed terminally, but its recorded result was evicted
    /// by retention (#859). It ran exactly once and must not be re-executed;
    /// unlike [`OperationAmbiguous`](ErrorCode::OperationAmbiguous) the outcome
    /// is known, only the body is gone.
    OperationEvicted,
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
            transport_secret: TransportSecret::new("per-run-secret"),
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
    fn handshake_accepts_on_match_and_refuses_on_skew() {
        let secret = TransportSecret::new("the-run-secret");
        let request = AttachRequest {
            protocol_version: PROTOCOL_VERSION,
            run_id: RunId::new(),
            resume_from: EventCursor::START,
            transport_secret: secret.clone(),
        };
        match handshake(
            &request,
            PROTOCOL_VERSION,
            Some(&secret),
            vec![Capability::ModelInference],
            None,
        ) {
            HandshakeResponse::Accepted(accepted) => {
                assert_eq!(accepted.protocol_version, PROTOCOL_VERSION);
                assert_eq!(
                    accepted.granted_capabilities,
                    vec![Capability::ModelInference]
                );
            }
            HandshakeResponse::Refused(_) => panic!("a matching version and secret must accept"),
        }

        // A skewed attach is answered with an on-wire refusal carrying the
        // sandbox's own version, not a blank error.
        let skewed = handshake(
            &request,
            PROTOCOL_VERSION + 1,
            Some(&secret),
            Vec::new(),
            None,
        );
        match skewed {
            HandshakeResponse::Refused(refused) => {
                assert_eq!(refused.protocol_version, PROTOCOL_VERSION + 1);
                assert_eq!(refused.code, ErrorCode::ProtocolVersion);
            }
            HandshakeResponse::Accepted(_) => panic!("skewed versions must refuse"),
        }

        // The refusal frame round-trips and is externally tagged.
        let encoded = serde_json::to_value(&skewed).unwrap();
        assert!(encoded.get("refused").is_some());
        assert_eq!(
            serde_json::from_value::<HandshakeResponse>(encoded).unwrap(),
            skewed
        );
    }

    #[test]
    fn handshake_refuses_a_bad_secret_after_the_version_gate() {
        let expected = TransportSecret::new("the-run-secret");
        let attach_with = |secret: TransportSecret| AttachRequest {
            protocol_version: PROTOCOL_VERSION,
            run_id: RunId::new(),
            resume_from: EventCursor::START,
            transport_secret: secret,
        };

        // Right version, wrong secret: refused as Unauthenticated (not accepted,
        // and not a version refusal).
        let wrong = handshake(
            &attach_with(TransportSecret::new("not-the-secret")),
            PROTOCOL_VERSION,
            Some(&expected),
            vec![Capability::ModelInference],
            None,
        );
        match wrong {
            HandshakeResponse::Refused(refused) => {
                assert_eq!(refused.code, ErrorCode::Unauthenticated)
            }
            HandshakeResponse::Accepted(_) => panic!("a wrong secret must be refused"),
        }

        // The version gate comes first: a skew with a *correct* secret is still a
        // version refusal, so auth never bypasses the version check.
        let skewed_but_authed = handshake(
            &attach_with(expected.clone()),
            PROTOCOL_VERSION + 1,
            Some(&expected),
            Vec::new(),
            None,
        );
        match skewed_but_authed {
            HandshakeResponse::Refused(refused) => {
                assert_eq!(refused.code, ErrorCode::ProtocolVersion)
            }
            HandshakeResponse::Accepted(_) => panic!("a version skew must refuse even when authed"),
        }

        // No configured secret fails closed: every attach is Unauthenticated even
        // with a matching version.
        let unconfigured = handshake(
            &attach_with(expected.clone()),
            PROTOCOL_VERSION,
            None,
            Vec::new(),
            None,
        );
        assert!(matches!(
            unconfigured,
            HandshakeResponse::Refused(refused) if refused.code == ErrorCode::Unauthenticated
        ));
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
