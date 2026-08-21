//! The seam between the `/code/browser/*` routes and the desktop's engine
//! adapter.
//!
//! The server owns authentication and scoping: a route resolves the caller's
//! bearer token to a [`BrowserSubject`] through the
//! [`super::browser_channel::BrowserTokenRegistry`] and validates the
//! subject's session and workspace before any call lands here. An
//! implementation therefore trusts the subject it receives and only decides
//! what the named browser can do — it never re-derives authority from model
//! arguments.
//!
//! The trait is deliberately server-private. The desktop adapter that
//! implements it is injected at bind time; a build without one answers
//! `501 Not Implemented` from every browser route.

use tidebreak_core::{
    BrowserActArgs, BrowserActResult, BrowserListArgs, BrowserListResult, BrowserNavigateArgs,
    BrowserNavigateResult, BrowserPageSnapshot, BrowserScreenshotArgs, BrowserScreenshotResult,
    BrowserSnapshotArgs, BrowserWaitArgs, BrowserWaitResult, CodeSessionId,
};

use super::browser_channel::BrowserSubject;

/// Why a browser operation could not be performed.
///
/// Variants map onto stable HTTP statuses in the route layer:
/// `UnknownBrowserId` → 404, `SessionEnded` → 403, `Unsupported` → 501,
/// `StaleTarget` → 409, `Failed` → 500. Messages must never carry bearer
/// tokens or another workspace's identifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BrowserRuntimeError {
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

/// The trusted half of the agent-driveable in-app browser.
///
/// One implementation per embedding: the desktop app adapts these calls onto
/// its native webview engine. Every method receives the [`BrowserSubject`]
/// the route layer derived from the caller's capability token, plus the
/// already-validated operation arguments.
#[async_trait::async_trait]
pub(crate) trait BrowserRuntime: Send + Sync {
    /// List the tabs `subject` is authorized to observe.
    async fn list(
        &self,
        subject: &BrowserSubject,
        args: BrowserListArgs,
    ) -> Result<BrowserListResult, BrowserRuntimeError>;

    /// Navigate one authorized tab to an HTTP(S) address.
    async fn navigate(
        &self,
        subject: &BrowserSubject,
        args: BrowserNavigateArgs,
    ) -> Result<BrowserNavigateResult, BrowserRuntimeError>;

    /// Read one authorized tab as a bounded semantic snapshot.
    async fn snapshot(
        &self,
        subject: &BrowserSubject,
        args: BrowserSnapshotArgs,
    ) -> Result<BrowserPageSnapshot, BrowserRuntimeError>;

    /// Poll for a deterministic page condition with a hard timeout.
    async fn wait_for(
        &self,
        subject: &BrowserSubject,
        args: BrowserWaitArgs,
    ) -> Result<BrowserWaitResult, BrowserRuntimeError>;

    /// Capture an epoch-bound screenshot of one authorized tab.
    async fn screenshot(
        &self,
        subject: &BrowserSubject,
        args: BrowserScreenshotArgs,
    ) -> Result<BrowserScreenshotResult, BrowserRuntimeError>;

    /// Perform one semantic action on a re-resolved target.
    async fn act(
        &self,
        subject: &BrowserSubject,
        args: BrowserActArgs,
    ) -> Result<BrowserActResult, BrowserRuntimeError>;

    /// Drop every capability held for `session`, synchronously.
    ///
    /// Called on the session-end path — the same moment the token registry
    /// invalidates the session's bearer — so the native side revokes its
    /// grant before the end call returns rather than on some later poll.
    /// Must be cheap and non-blocking; idempotent for unknown sessions.
    fn revoke_session(&self, session: CodeSessionId);
}
