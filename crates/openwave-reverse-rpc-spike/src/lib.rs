//! SPIKE — reverse-RPC callback channel feasibility. **Not wired into the
//! server, and not a shipped feature.**
//!
//! This crate is the prototype behind the go/no-go in
//! `docs/spikes/reverse-rpc-findings.md`. It exists to prove (or fail to prove)
//! the hard parts of the callback channel that step 5 of the sandbox-provider
//! delivery sequence calls the highest-risk unproven element of the design:
//!
//! - a host-held bidirectional connection the sandbox supervisor multiplexes
//!   reverse requests back over, with model inference as the first capability;
//! - durable per-run **operation identity** with recorded responses, so a
//!   request re-issued after a reconnect is answered once, not executed twice;
//! - request/response **correlation** over one connection, **cancellation** of
//!   an in-flight request, and **backpressure** against a slow host;
//! - **disconnect semantics**: an in-flight request whose connection drops is
//!   failed to the sandbox side and is safe to re-issue.
//!
//! It follows the `openwave-host-broker` envelope shape on purpose — versioned
//! protocol with an exact-equality version check, deny-by-default capability
//! gating, a per-operation idempotency identity, and bounded typed results —
//! rather than inventing a parallel style. The shipped host broker is not
//! touched.
//!
//! The transport is a `tokio::io::duplex` pipe between two tasks; see
//! [`transport`]. That is the cheapest thing that still exercises real framing
//! and real concurrency, and dropping a half models a dropped connection. No
//! real E2B/Daytona backend is stood up.

pub mod host;
pub mod model;
pub mod protocol;
pub mod supervisor;
pub mod transport;

pub use host::{serve_connection, CapabilityHost};
pub use model::{Completion, ModelProvider};
pub use protocol::{
    Capability, ErrorCode, ErrorResponse, ModelInferenceParams, ModelInferenceResult, OperationId,
    RequestId, ReverseRequest, ReverseResult, PROTOCOL_VERSION,
};
pub use supervisor::{Call, ClientError, ReverseClient};
