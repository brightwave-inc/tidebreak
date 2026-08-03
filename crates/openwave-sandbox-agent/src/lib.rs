//! The in-container OpenWave sandbox agent.
//!
//! This crate is the sandbox-resident side of the sandbox-provider design's step
//! 7.1: a container image running OpenWave's agent loop with a closed,
//! sandbox-resident tool registry, behind the versioned
//! [`openwave_sandbox_protocol`] wire contract. It is the primary consumer of
//! that protocol's sandbox-side transport.
//!
//! Four components, and their separation is the point:
//!
//! - the [`supervisor`] owns the transport listener (and, as a documented stub,
//!   the credential boundary) — the non-agent component;
//! - the [`agent`] loop drives the run: model inference dialed back to the host
//!   over reverse RPC, a sandbox-resident [`tools`] registry, the event stream,
//!   and a final result;
//! - the [`model`] client turns each model step into a host-proxied reverse call,
//!   so **no model credential ever lives in the container**;
//! - the [`egress`] proxy is the binary's second face: it runs in its *own*
//!   dual-homed container — never beside the agent, whose failure domain it
//!   must not share — enforcing the run's compiled network policy and relaying
//!   the host's transport into the otherwise-unroutable sandbox.
//!
//! The sandbox-resident tool surface — [`exec`], [`fs`], and the closed
//! [`tools`] registry that pins them — runs *inside* the container. That
//! in-container execution is the containment: model output can only move the
//! sandbox. Egress is enforced from outside it: on the local Docker backend the
//! sandbox's only network is an internal bridge, and the egress-proxy container
//! is its single, policy-checked way out (see [`exec`]'s module docs for what
//! that does and does not guarantee).
//!
//! # UNSTABLE
//!
//! Like the protocol it speaks, this crate is unstable and unversioned until a
//! named release.

pub mod agent;
pub mod egress;
pub mod exec;
pub mod fs;
pub mod model;
pub mod supervisor;
pub mod tools;

pub use agent::{run_agent, AgentRunError};
pub use egress::{EgressProxy, EgressProxyConfig};
pub use exec::{ExecTool, EXEC_TOOL};
pub use fs::{
    ListDirTool, ReadFileTool, WriteFileTool, LIST_DIR_TOOL, READ_FILE_TOOL, WRITE_FILE_TOOL,
};
pub use model::{HostModel, ModelError};
pub use supervisor::{CredentialProxy, Supervisor};
pub use tools::{sandbox_tool_registry, SANDBOX_REGISTRY_TOOL_NAMES};
