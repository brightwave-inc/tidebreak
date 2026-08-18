//! The host-tool broker: one seam between skill-declared host dependencies
//! and whatever machinery provides them.
//!
//! Skills declare host tools in their manifests (`deps: { host: [...] }`,
//! the closed [`HostDep`] vocabulary). Something has to make those tools
//! real: on the desktop that is the managed LibreOffice installer; headless
//! embeddings have nothing. The broker is the one interface both sides meet
//! at, so every caller — turn staging warming a declared dependency ahead of
//! use, the operating prompt stating whether office rendering is real, an
//! explicit user retry — reads and drives the same state instead of growing
//! its own install path.
//!
//! `ensure` is deliberately fire-and-forget: provisioning can be a 300 MB
//! download, and no caller is allowed to block a turn on it. The provider
//! behind the broker keeps its own discipline (serialized installs, a
//! remembered failure that only an explicit retry clears), and `status`
//! reports the current truth those rules produce.

use std::path::PathBuf;

use async_trait::async_trait;

use crate::skills::HostDep;

/// The current truth about one host tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostToolStatus {
    /// The tool resolves right now; a caller that needs it can have it.
    Available,
    /// Provisioning is under way; the tool is expected to resolve soon.
    Installing,
    /// The tool does not resolve and is not being provisioned, with the
    /// reason (unsupported platform, a remembered install failure, or simply
    /// not installed).
    Unavailable(String),
}

/// Provides host tools on demand and reports their state.
#[async_trait]
pub trait HostToolBroker: Send + Sync {
    /// Begin providing `tool` if it is absent and provisioning is possible;
    /// returns immediately. Repeat calls are cheap: an available tool, an
    /// install already under way, and a remembered failure are all no-ops —
    /// nothing re-downloads without an explicit user retry.
    fn ensure(&self, tool: HostDep);

    /// Explicitly retry provisioning `tool`, clearing a remembered failure
    /// when the embedding supports one. The default preserves older brokers'
    /// idempotent ensure behavior; interactive surfaces such as the harness
    /// doctor's Refresh button use this hook to make "try again" real.
    fn retry(&self, tool: HostDep) {
        self.ensure(tool);
    }

    /// The current truth about `tool`.
    async fn status(&self, tool: HostDep) -> HostToolStatus;

    /// The host directory a provisioned `tool` is rooted at, for the tools an
    /// execution backend has to be handed a path to rather than ones the host
    /// drives itself.
    ///
    /// `None` for a tool that does not resolve right now, and for a tool with
    /// no such root — LibreOffice conversion runs on the host, so nothing
    /// downstream ever needs its location. A returned path is a host-verified
    /// managed install; a backend may expose it read-only and nothing more.
    async fn managed_root(&self, tool: HostDep) -> Option<PathBuf>;
}
