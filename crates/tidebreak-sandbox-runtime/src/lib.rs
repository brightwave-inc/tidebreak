//! Sandboxed background-agent execution.
//!
//! The runtime owns in-process sandbox runs, container-hosted runs, Docker
//! lifecycle, detached-admission checks, exact-attempt cancellation, and the
//! durable reverse-operation log. The embedding server supplies model routing,
//! live settings, event publication, and tool catalogs through narrow traits.

pub mod admission;
pub mod agent_worker;
pub mod container_run;
pub mod container_worker;
pub mod docker;
pub mod durable_oplog;
pub mod guards;
pub mod host;
pub mod scoped_model_token;

#[cfg(test)]
mod test_support;
#[cfg(test)]
pub(crate) use test_support::{bus, resolver, TestSandboxHost};

pub use durable_oplog::DurableOperationStore;
pub use guards::{SandboxAttemptGuard, SandboxSteerGuard, SandboxSteerRefusal};
pub use host::{ResolvedSandboxModel, SandboxHost, SandboxModelUse};
