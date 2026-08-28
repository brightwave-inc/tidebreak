//! Server-owned browser runtime integration boundary.
//!
//! The [`BrowserRuntime`] trait defines the public contract between the
//! server and the desktop browser adapter. The server owns this trait and
//! never depends on a desktop crate. The desktop implements it behind an
//! `Arc<dyn BrowserRuntime>` and passes it to the bind-time constructor before
//! code-session recovery starts.
//!
//! ## Trust boundary
//!
//! Every method receives a [`BrowserRuntimeScope`] derived from a validated
//! session browser token — never from request fields. The runtime MUST
//! validate scope before accessing any native browser resource.

use async_trait::async_trait;

use tidebreak_core::{
    BrowserActArgs, BrowserActResult, BrowserListResult, BrowserNavigateArgs,
    BrowserNavigateResult, BrowserPageSnapshot, BrowserScreenshotArgs, BrowserScreenshotResult,
    BrowserSnapshotArgs, BrowserWaitArgs, BrowserWaitResult, CodeSessionId, OwnerId, WorkspaceId,
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
    /// The subject is live but does not have a native grant for the target.
    NotAuthorized(String),
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
/// error taxonomy without constructing crate-private [`crate::ServerError`]. The
/// route layer maps errors to stable HTTP responses centrally.
#[async_trait]
pub trait BrowserRuntime: Send + Sync {
    /// Whether this runtime can synthesize trusted native semantic actions.
    ///
    /// The server uses this flag only to advertise the action tool. The
    /// runtime still revalidates every action request at dispatch time.
    fn supports_semantic_actions(&self) -> bool {
        false
    }

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

    /// Poll for a deterministic page condition on the tab in `args.browser_id`.
    ///
    /// `args.snapshot_id` and `args.document_epoch` are the caller's
    /// last-known snapshot; the runtime must validate them before polling.
    /// UrlChanged conditions resolve across document boundaries; every other
    /// condition is bounded by `args.document_epoch`.
    async fn wait(
        &self,
        scope: &BrowserRuntimeScope,
        args: &BrowserWaitArgs,
    ) -> Result<BrowserWaitResult, BrowserRuntimeError>;

    /// Capture a base-64 PNG screenshot bounded by `args.snapshot_id` and
    /// `args.document_epoch`.
    ///
    /// The runtime must atomically fence, validate, capture, and record the
    /// screenshot against the live controller/instance/document/snapshot
    /// state. A changed document or replaced instance must refuse.
    async fn screenshot(
        &self,
        scope: &BrowserRuntimeScope,
        args: &BrowserScreenshotArgs,
    ) -> Result<BrowserScreenshotResult, BrowserRuntimeError>;

    /// Perform one semantic action against an exact snapshot target.
    async fn act(
        &self,
        _scope: &BrowserRuntimeScope,
        _args: &BrowserActArgs,
    ) -> Result<BrowserActResult, BrowserRuntimeError> {
        Err(BrowserRuntimeError::Unsupported(
            "trusted native semantic actions".to_owned(),
        ))
    }

    /// Synchronously revoke all browser capability for `scope.session`.
    ///
    /// Called only when the logical code session has ended. Implementations
    /// must leave an enduring tombstone so a stale or reissued HTTP token can
    /// never lazily recreate native authority for that session id. Must be
    /// idempotent and synchronous.
    ///
    fn revoke_session(&self, scope: &BrowserRuntimeScope);
}
