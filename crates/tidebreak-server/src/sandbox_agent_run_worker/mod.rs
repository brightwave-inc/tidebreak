//! Execution for durably claimed sandboxed agent runs.
//!
//! A sandbox run intentionally is not a second foreground turn. It receives
//! only its immutable delegated task and a small fixed tool surface — commands
//! in its own private workspace, a host-owned web search, the one file
//! explicitly delegated to it — and returns bounded text through the fenced
//! agent-run result transition. Every tool call is a durable checkpoint
//! executed by its own lane, so the run holds no lease while it waits.
//!
//! Its real product is files: a command that writes under `output/` publishes
//! that file to the parent conversation as an output named by its own
//! filename. The run never names an output identity, and neither does the
//! host.

mod config;
mod model_step;
mod worker;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub(crate) use config::{SandboxAgentRunWorker, SandboxAgentRunWorkerConfig};
