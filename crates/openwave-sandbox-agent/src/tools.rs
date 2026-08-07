//! The sandbox-resident tool registry — a closed, test-pinned set.
//!
//! Running the agent loop inside a sandbox is "the same code with a different
//! tool registry and transport" (see
//! [sandbox-providers.md](../../docs/sandbox-providers.md)). This module builds
//! that registry from OpenWave's own [`Tool`](openwave_core::Tool) trait, so the
//! tools the sandbox loop invokes are ordinary [`openwave_core`] tools — not a
//! parallel abstraction.
//!
//! # The closed set is the design gate
//!
//! The design makes widening the sandbox-resident surface a deliberate, reviewed
//! change: "a sandbox-resident run gets its own pinned tool registry, and
//! widening that registry is a separate design gated on this document." The gate
//! is mechanical — [`SANDBOX_REGISTRY_TOOL_NAMES`] names the exact set and a
//! test asserts the built registry matches it. Adding a tool that is not a
//! drive-by change means editing that named invariant, which review sees.
//!
//! For this slice the surface is:
//!
//! - **model inference** — dialed back to the host over reverse RPC (not a
//!   registered [`Tool`]; the run's granted reverse capability, so no model
//!   credential lives in the container);
//! - **[`exec`](crate::exec)** — a bounded, model-authored command run *inside*
//!   the container (the container is the containment);
//! - **[`read_file`](crate::fs), [`write_file`](crate::fs),
//!   [`list_dir`](crate::fs)** — path-validated filesystem access within the
//!   agent's workspace directory.
//!
//! # Host-side tools are not registry entries
//!
//! This registry names the tools the loop runs *locally*. A background agent's
//! host-side surface — submission, public web search, the task plan — is not
//! here and must not be added here: those calls write to the host's database or
//! leave the host's network, and executing them in the container would either
//! be impossible or would hand container-resident model output the authority
//! the container exists to withhold. They reach the host over durable
//! checkpoints instead. Routing one into the container would need a new reverse
//! capability, which is the separate design this module's gate is about.
//!
//! # NOT YET FOR CREDENTIAL-BEARING WORK
//!
//! `exec` can make network calls. Egress is meant to be routed through the
//! sandbox supervisor (credential separation + an egress proxy), which is a
//! **stub** in this crate. In-container execution keeps model output from
//! reaching host authority, but egress *from the container* is not yet
//! externally enforced, so this surface must not be routed to production
//! credential-bearing work until externally-enforced egress and the
//! transport-auth gate land.

use std::path::PathBuf;

use openwave_core::ToolRegistry;

use crate::exec::{ExecTool, DEFAULT_EXEC_TIMEOUT, EXEC_TOOL};
use crate::fs::{
    ListDirTool, ReadFileTool, WriteFileTool, LIST_DIR_TOOL, READ_FILE_TOOL, WRITE_FILE_TOOL,
};

/// The exact, closed set of tool names the sandbox-resident registry registers.
///
/// This is the named invariant the design gate rides on: the reverse-RPC model
/// inference capability is granted separately (it is not a registered tool), and
/// every *local* tool the sandbox loop can invoke is one of these. Widening it is
/// a deliberate edit here, seen in review.
pub const SANDBOX_REGISTRY_TOOL_NAMES: [&str; 4] =
    [EXEC_TOOL, READ_FILE_TOOL, WRITE_FILE_TOOL, LIST_DIR_TOOL];

/// Build the closed sandbox-resident tool registry rooted at `workspace`.
///
/// `workspace` is the agent's in-container workspace directory; the filesystem
/// tools are scoped to it and `exec` runs commands inside it. The exec tool is
/// bounded by [`DEFAULT_EXEC_TIMEOUT`].
#[must_use]
pub fn sandbox_tool_registry(workspace: impl Into<PathBuf>) -> ToolRegistry {
    let workspace = workspace.into();
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ExecTool::new(
        workspace.clone(),
        DEFAULT_EXEC_TIMEOUT,
    )));
    registry.register(Box::new(ReadFileTool::new(workspace.clone())));
    registry.register(Box::new(WriteFileTool::new(workspace.clone())));
    registry.register(Box::new(ListDirTool::new(workspace)));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The design gate: the built registry advertises exactly the pinned closed
    /// set — no more, no fewer. A new tool must edit
    /// [`SANDBOX_REGISTRY_TOOL_NAMES`] to pass, which is the reviewed change the
    /// design requires.
    #[test]
    fn the_registry_is_the_pinned_closed_set() {
        let registry = sandbox_tool_registry(std::env::temp_dir());
        let mut built: Vec<String> = registry.specs().into_iter().map(|spec| spec.name).collect();
        built.sort();
        let mut pinned: Vec<String> = SANDBOX_REGISTRY_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        pinned.sort();
        assert_eq!(
            built, pinned,
            "the sandbox-resident registry must match its pinned closed set; \
             widening it is a deliberate edit to SANDBOX_REGISTRY_TOOL_NAMES"
        );
    }
}
