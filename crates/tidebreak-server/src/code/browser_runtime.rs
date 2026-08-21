//! Server-owned browser runtime integration boundary.
//!
//! The [`BrowserRuntime`] trait defines the public contract between the
//! server and the desktop browser adapter. The server owns this trait and
//! never depends on a desktop crate. The desktop implements it behind an
//! `Arc<dyn BrowserRuntime>` and passes it to the bind-time constructor
//! (or, in tests, through [`CodeRuntime::set_browser_runtime`]).
//!
//! ## Trust boundary
//!
//! Every method receives a [`BrowserRuntimeScope`] derived from a validated
//! session browser token — never from request fields. The runtime MUST
//! validate scope before accessing any native browser resource.

use std::sync::Arc;

use async_trait::async_trait;

use tidebreak_core::{
    BrowserListResult, BrowserNavigateArgs, BrowserNavigateResult, BrowserPageSnapshot,
    BrowserSnapshotArgs, CodeSessionId, OwnerId, WorkspaceId,
};

// ── BrowserRuntimeScope ─────────────────────────────────────────────────────

/// The `{owner, workspace, session}` triple resolved from a browser bearer
/// token. Route handlers derive this from the token registry; the adapter
/// uses it to locate native browser resources.
///
/// Never constructed from request body fields — the route layer derives
/// scope exclusively from the trusted token registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserRuntimeScope {
    pub owner: OwnerId,
    pub workspace: WorkspaceId,
    pub session: CodeSessionId,
}

impl From<crate::code::browser_channel::BrowserSubject> for BrowserRuntimeScope {
    fn from(subject: crate::code::browser_channel::BrowserSubject) -> Self {
        Self {
            owner: subject.owner,
            workspace: subject.workspace,
            session: subject.session,
        }
    }
}

// ── BrowserRuntimeError ─────────────────────────────────────────────────────

/// Server-owned error taxonomy for browser operations.
///
/// The route layer maps every variant into a stable HTTP status and
/// `{kind, message}` JSON body. Desktop implementations return this error
/// rather than constructing raw [`ServerError`], which is crate-private.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserRuntimeError {
    /// The opaque browser id names no tab this subject may see.
    UnknownBrowserId(String),
    /// The subject's browser authority has ended.
    SessionEnded,
    /// The engine or platform cannot perform this operation.
    Unsupported(String),
    /// The page or target changed since the snapshot the caller is acting
    /// from; a fresh snapshot is required.
    StaleTarget,
    /// The engine failed while performing the operation.
    Failed(String),
}

// ── BrowserRuntime trait ────────────────────────────────────────────────────

/// The server's contract with a desktop browser adapter.
///
/// Every method receives a [`BrowserRuntimeScope`] derived from a validated
/// session browser token. The implementation must revalidate scope before
/// touching any native resource — the scope alone is not authorization; the
/// runtime owns the live grant and controller state.
///
/// All methods return [`BrowserRuntimeError`] so the adapter can express the
/// error taxonomy without constructing crate-private [`ServerError`]. The
/// route layer maps errors to stable HTTP responses centrally.
#[async_trait]
pub trait BrowserRuntime: Send + Sync {
    /// List browser tabs visible to the session identified by `scope`.
    async fn list(
        &self,
        scope: &BrowserRuntimeScope,
    ) -> Result<BrowserListResult, BrowserRuntimeError>;

    /// Navigate a tab identified by `args.browser_id` within `scope`.
    async fn navigate(
        &self,
        scope: &BrowserRuntimeScope,
        args: &BrowserNavigateArgs,
    ) -> Result<BrowserNavigateResult, BrowserRuntimeError>;

    /// Capture a semantic page snapshot for the tab in `args.browser_id`.
    async fn snapshot(
        &self,
        scope: &BrowserRuntimeScope,
        args: &BrowserSnapshotArgs,
    ) -> Result<BrowserPageSnapshot, BrowserRuntimeError>;

    /// Synchronously revoke all browser capability for `scope.session`.
    ///
    /// Called by [`CodeRuntime`] lifecycle paths — session end, stop,
    /// interrupt, reap, relaunch, and launch failure. Must be idempotent
    /// and must never block on async work: the caller holds no runtime
    /// handle after this returns and the session's browser access is dead.
    ///
    /// The default implementation is a no-op so tests and headless
    /// deployments without a browser adapter compile without changes.
    fn revoke_session(&self, _scope: &BrowserRuntimeScope) {}
}
