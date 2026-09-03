//! Black-box conformance for a self-hosted sandbox backend at a user-supplied
//! endpoint.
//!
//! The in-crate suites construct both sides themselves: the reference suite
//! pins semantics in-process and `wire_conformance` pins them over a loopback
//! socket, but neither can be pointed at somebody else's implementation. This
//! harness closes that gap for the one topology where the sandbox side is not
//! Tidebreak's code — a self-hosted backend, which per the provisioning
//! decomposition is nothing but an address and a credential. Given those two
//! facts and nothing else, it dials the endpoint as a real host and verifies
//! the attach gates every conforming backend must hold, in the exact shape
//! [`handshake`](tidebreak_sandbox_protocol::protocol::handshake) documents:
//!
//! - a version skew is refused with [`ErrorCode::ProtocolVersion`] and the
//!   backend's own version in the refusal, before authentication is considered
//!   (a skewed dial with a bad secret must still refuse on version, so neither
//!   gate leaks signal about the other);
//! - a wrong or absent transport secret is refused with
//!   [`ErrorCode::Unauthenticated`] and no connection is installed;
//! - the correct secret attaches, and the accepted frame echoes the exact
//!   protocol version.
//!
//! The harness is deliberately read-only toward the run: it never sends
//! [`RunInit`](tidebreak_sandbox_protocol::init::RunInit), so the backend's agent
//! takes no step and the endpoint is reusable across invocations. It attaches
//! with a fresh [`RunId`] each dial — the canonical handshake gates on version
//! and secret only, so a conforming backend must not refuse an attach for the
//! run id alone.
//!
//! Configuration is by environment, and the harness skips (passes without
//! asserting) when unconfigured so ordinary CI is unaffected:
//!
//! - `TIDEBREAK_SELFHOST_CONFORMANCE_ENDPOINT` — the `host:port` the backend
//!   listens on, dialable from this machine by the operator's own means.
//! - `TIDEBREAK_SELFHOST_CONFORMANCE_SECRET` — the per-run transport secret the
//!   backend was configured with.
//!
//! Run it against the reference implementation (the `tidebreak-sandbox-agent`
//! binary with `TIDEBREAK_TRANSPORT_SECRET` set) or any third-party backend:
//!
//! ```sh
//! TIDEBREAK_SELFHOST_CONFORMANCE_ENDPOINT=127.0.0.1:8080 \
//! TIDEBREAK_SELFHOST_CONFORMANCE_SECRET=the-per-run-secret \
//! cargo test -p tidebreak-sandbox-protocol --test selfhost_conformance -- --nocapture
//! ```

use std::{env, sync::Arc, time::Duration};

use tokio::{net::TcpStream, time::timeout};

use tidebreak_sandbox_protocol::{
    ids::{EventCursor, RunId},
    oplog::InMemoryOperationStore,
    protocol::{AttachRequest, ErrorCode, ErrorResponse, Response, PROTOCOL_VERSION},
    reverse::{
        Capability, CapabilityResponder, GrantSet, ReverseRequest, ReverseResult, RunProvenance,
    },
    CapabilityHost, ConnectError, HostConnection, TransportSecret, WireClient,
};

/// Endpoint and secret environment variables the harness is configured by.
const ENDPOINT_ENV: &str = "TIDEBREAK_SELFHOST_CONFORMANCE_ENDPOINT";
const SECRET_ENV: &str = "TIDEBREAK_SELFHOST_CONFORMANCE_SECRET";

/// Wall-clock bound on each dial, so an endpoint that accepts TCP and then
/// says nothing fails the harness rather than hanging it.
const DIAL_TIMEOUT: Duration = Duration::from_secs(15);

/// A version the backend cannot speak, for the skew refusals.
const SKEWED_VERSION: u32 = PROTOCOL_VERSION + 1;

/// The harness never grants a capability, so a backend that issues a reverse
/// request right after attach is answered `Denied` by the host's own
/// deny-by-default gate; this responder is unreachable behind an empty grant
/// set and answers `Internal` if a host regression ever routes past it.
struct NoCapabilities;

#[async_trait::async_trait]
impl CapabilityResponder for NoCapabilities {
    async fn respond(&self, _request: ReverseRequest) -> Response<ReverseResult> {
        Response::Error(ErrorResponse::new(
            ErrorCode::Internal,
            "the conformance harness serves no capabilities",
            false,
        ))
    }
}

/// A host with no granted capabilities and a fresh operation store per dial.
fn harness_host() -> CapabilityHost {
    let provenance = RunProvenance {
        run_id: RunId::new(),
        provider: "selfhost-conformance".to_owned(),
    };
    CapabilityHost::new(
        GrantSet::new(provenance, Vec::<Capability>::new()),
        Arc::new(NoCapabilities),
        Arc::new(InMemoryOperationStore::new()),
    )
}

/// One dial: connect to `endpoint` and attach with `version` and `secret`.
async fn dial(endpoint: &str, version: u32, secret: &str) -> Result<HostConnection, ConnectError> {
    let stream = timeout(DIAL_TIMEOUT, TcpStream::connect(endpoint))
        .await
        .unwrap_or_else(|_| panic!("dialing {endpoint} timed out"))
        .unwrap_or_else(|error| panic!("dialing {endpoint} failed: {error}"));
    let attach = AttachRequest {
        protocol_version: version,
        run_id: RunId::new(),
        resume_from: EventCursor::START,
        transport_secret: TransportSecret::new(secret),
    };
    timeout(
        DIAL_TIMEOUT,
        WireClient::connect(stream, attach, harness_host()),
    )
    .await
    .unwrap_or_else(|_| {
        panic!("the endpoint accepted the connection but never answered the handshake")
    })
}

/// The attach gates every conforming self-hosted backend must hold, driven
/// against the configured endpoint. Refusal cases run first — a refused attach
/// must install nothing — and the authenticated attach runs last and is
/// dropped without delivering a run init.
#[tokio::test]
async fn attach_gates_hold_at_the_configured_endpoint() {
    let (Ok(endpoint), Ok(secret)) = (env::var(ENDPOINT_ENV), env::var(SECRET_ENV)) else {
        eprintln!(
            "selfhost conformance skipped: set {ENDPOINT_ENV} and {SECRET_ENV} \
             to run it against a backend"
        );
        return;
    };

    // Version skew with the correct secret: refused on version, and the
    // refusal teaches the host what the backend speaks.
    match dial(&endpoint, SKEWED_VERSION, &secret).await {
        Err(ConnectError::VersionRefused(refused)) => {
            assert_eq!(
                refused.code,
                ErrorCode::ProtocolVersion,
                "a version skew refuses with the ProtocolVersion code"
            );
            assert_ne!(
                refused.protocol_version, SKEWED_VERSION,
                "the refusal carries the backend's own version, not an echo of the host's"
            );
        }
        Err(other) => panic!("a skewed version must refuse on version; got {other:?}"),
        Ok(_) => panic!("a skewed version must not attach"),
    }

    // Version skew with a wrong secret: still refused on version — the gates
    // run in a fixed order and neither leaks signal about the other.
    match dial(&endpoint, SKEWED_VERSION, "not-the-secret").await {
        Err(ConnectError::VersionRefused(refused)) => {
            assert_eq!(
                refused.code,
                ErrorCode::ProtocolVersion,
                "the version gate answers before authentication is considered"
            );
        }
        Err(other) => {
            panic!("a skewed version must refuse on version even with a bad secret; got {other:?}")
        }
        Ok(_) => panic!("a skewed version must not attach"),
    }

    // Wrong secret at the exact version: refused as unauthenticated, with the
    // versions in agreement so the refusal is unambiguous.
    match dial(&endpoint, PROTOCOL_VERSION, "not-the-secret").await {
        Err(ConnectError::Unauthenticated(refused)) => {
            assert_eq!(
                refused.code,
                ErrorCode::Unauthenticated,
                "a wrong transport secret refuses as unauthenticated"
            );
            assert_eq!(
                refused.protocol_version, PROTOCOL_VERSION,
                "an authentication refusal reports the matching version"
            );
        }
        Err(other) => panic!("a wrong secret must refuse as unauthenticated; got {other:?}"),
        Ok(_) => panic!("a wrong secret must not attach"),
    }

    // Absent secret (the empty token authenticates against nothing): refused
    // as unauthenticated, never served open.
    match dial(&endpoint, PROTOCOL_VERSION, "").await {
        Err(ConnectError::Unauthenticated(refused)) => {
            assert_eq!(refused.code, ErrorCode::Unauthenticated);
        }
        Err(other) => panic!("an absent secret must refuse as unauthenticated; got {other:?}"),
        Ok(_) => panic!("an absent secret must not attach"),
    }

    // The correct secret at the exact version attaches, and the accepted frame
    // echoes the version exactly. Dropped without a run init, so the backend's
    // agent takes no step on the harness's account.
    match dial(&endpoint, PROTOCOL_VERSION, &secret).await {
        Ok(connection) => {
            let accepted = connection.accepted();
            assert_eq!(
                accepted.protocol_version, PROTOCOL_VERSION,
                "an accepted attach echoes the exact protocol version"
            );
            eprintln!(
                "selfhost conformance: endpoint {endpoint} conforms \
                 (granted capabilities: {:?}, latest sequence: {:?})",
                accepted.granted_capabilities, accepted.latest_sequence
            );
        }
        Err(error) => panic!("the correct secret must attach; got {error:?}"),
    }
}
