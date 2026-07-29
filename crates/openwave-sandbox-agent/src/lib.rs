//! The in-container OpenWave sandbox agent.
//!
//! This crate is the sandbox-resident side of the sandbox-provider design's step
//! 7.1: a container image running OpenWave's agent loop with a closed,
//! sandbox-resident tool registry, behind the versioned
//! [`openwave_sandbox_protocol`] wire contract. It is the primary consumer of
//! that protocol's sandbox-side transport.
//!
//! Three components, and their separation is the point:
//!
//! - the [`supervisor`] owns the transport listener (and, as a documented stub,
//!   the credential/egress boundary) — the non-agent component;
//! - the [`agent`] loop drives the run: model inference dialed back to the host
//!   over reverse RPC, a sandbox-resident [`tools`] registry, the event stream,
//!   and a final result;
//! - the [`model`] client turns each model step into a host-proxied reverse call,
//!   so **no model credential ever lives in the container**.
//!
//! # UNSTABLE
//!
//! Like the protocol it speaks, this crate is unstable and unversioned until a
//! named release.

pub mod agent;
pub mod model;
pub mod supervisor;
pub mod tools;

pub use agent::{run_agent, AgentRunError};
pub use model::{HostModel, ModelError};
pub use supervisor::{CredentialProxy, Supervisor};
pub use tools::{sandbox_tool_registry, WordCount, WORD_COUNT_TOOL};
