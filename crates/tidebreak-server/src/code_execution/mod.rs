//! Host-owned code-execution provider selection and policy.
//!
//! The model cannot select a provider or timeout. The foreground `exec` tool
//! calls [`ConfiguredCodeExecutionProvider`], which reads the current host
//! setting at the last possible boundary and delegates to the selected adapter.
//! Local and managed adapters implement the same provider contract without
//! changing the tool schema or persisted tool-call arguments.

mod config;
mod provider;
mod staging;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub use config::*;
pub use provider::*;
pub use staging::StagedFolders;
