//! The versioned sandbox-agent wire protocol.
//!
//! This crate defines the boundary between the OpenWave host and a
//! sandbox-resident agent run: provisioning, run init, the resumable,
//! monotonically sequenced event stream, artifact collection, and the reverse-RPC
//! callback channel with host-proxied model inference as its first capability.
//! It is a public, versioned interface third parties implement — a self-hosted
//! backend means someone other than OpenWave runs the sandbox side of it — so
//! the wire contract, not any one backend, is the deliverable. OpenWave's own
//! backends are its first consumers; the [in-process reference](reference) here
//! is the first of them and the target the [conformance suite](conformance)
//! runs against.
//!
//! It follows the [`openwave-host-broker`](https://docs.rs/openwave-host-broker)
//! envelope discipline deliberately: a single [`PROTOCOL_VERSION`] checked for
//! *exact equality* with a handshake on attach, deny-by-default capability
//! gating, a durable per-run operation identity that fences idempotent replay,
//! and bounded typed results with explicit per-capability bounds.
//!
//! # UNSTABLE
//!
//! **This protocol is unstable and unversioned for compatibility purposes until
//! a named release.** While OpenWave is pre-1.0 nobody should build on it
//! without expecting breakage: the wire types, the [`PROTOCOL_VERSION`], and the
//! trait shapes may all change without a migration path. The exact-equality
//! version check exists to make a skew a hard, visible refusal rather than a
//! silent misinterpretation, not to promise forward compatibility.
//!
//! # What this slice ships, and what it defers
//!
//! This slice ships the protocol contract, the reference backend, and the
//! conformance suite. It backs the reverse-RPC operation log with an
//! **in-memory** [`OperationStore`](oplog::OperationStore); the crash-safe
//! durable operation log (#858) and its retention/eviction policy (#859) are
//! correctness-critical designs that get their own focused review and are
//! tracked as follow-ups (see [`oplog`]). The
//! [`OperationStore`](oplog::OperationStore) trait is the seam they plug into.

pub mod artifacts;
pub mod conformance;
pub mod events;
pub mod host;
pub mod ids;
pub mod init;
pub mod oplog;
pub mod protocol;
pub mod provisioning;
pub mod reference;
pub mod reverse;

pub use host::{CapabilityHost, ReverseWaiter};
pub use ids::{EventCursor, OperationId, RequestId, RunId, SandboxTag, Sequence};
pub use oplog::{ClaimOutcome, InMemoryOperationStore, OperationState, OperationStore, StoreError};
pub use protocol::{
    require_version, AttachAccepted, AttachRequest, ErrorCode, ErrorResponse, Response,
    MAX_ARTIFACTS, MAX_ARTIFACT_BYTES, MAX_BUFFERED_EVENTS, MAX_EVENT_PAYLOAD_BYTES,
    MAX_FRAME_BYTES, MAX_MODEL_COMPLETION_BYTES, MAX_MODEL_PROMPT_BYTES, PROTOCOL_VERSION,
};
pub use provisioning::{
    BackendError, ProvisionRequest, SandboxAddress, SandboxBackend, SandboxHandle, TransportSecret,
};
pub use reverse::{
    Capability, CapabilityResponder, ControlFrame, GrantSet, ModelInferenceParams,
    ModelInferenceResult, RequestFrame, ReverseEnvelope, ReverseRequest, ReverseResponseEnvelope,
    ReverseResult, RunProvenance,
};
