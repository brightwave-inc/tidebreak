//! Server-owned native computer-use runtime integration boundary.
//!
//! The [`NativeRuntime`] trait is the contract between the server and the
//! desktop native adapter. The server owns this trait and never depends on a
//! desktop crate. The desktop implements it behind an `Arc<dyn NativeRuntime>`
//! and installs it through the bind-time constructor before code-session
//! recovery starts.
//!
//! Every method receives a [`NativeRuntimeScope`] derived from a validated
//! session capability token - never from request fields. The runtime MUST
//! validate scope before touching any native input, display, or approval
//! surface.

use async_trait::async_trait;
use tidebreak_core::{
    computer_session::{ComputerUseCall, ComputerUseResult},
    OwnerId, SessionId, WorkspaceId,
};

/// The `{owner, workspace, session}` triple resolved from a native bearer
/// token. Route handlers derive this from the token registry; the adapter
/// uses it to locate the native host's per-session ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRuntimeScope {
    pub owner: OwnerId,
    pub workspace: WorkspaceId,
    pub session: SessionId,
}

impl From<crate::code::native_channel::NativeSubject> for NativeRuntimeScope {
    fn from(subject: crate::code::native_channel::NativeSubject) -> Self {
        Self {
            owner: subject.owner,
            workspace: subject.workspace,
            session: subject.session,
        }
    }
}

/// Server-owned error taxonomy for native computer-use operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeRuntimeError {
    /// The subject's native authority has ended or been revoked.
    SessionEnded,
    /// The subject is live but did not hold the native grant for the target.
    NotAuthorized(String),
    /// The engine or platform cannot perform this operation.
    Unsupported(String),
    /// The call identity differs from a stored in-flight or terminal result.
    RequestConflict,
    /// A stored result matches the same call and arguments.
    Recovered,
    /// A previous action's effect is unknown; a new action must not replay it.
    UnknownOutcome,
    /// The runtime failed while performing the operation.
    Failed(String),
}

/// Cancellation handle for one native operation.
#[async_trait]
pub trait NativeOperationHandle: Send + Sync {
    /// Cancel the pending input and release the session's exclusive ownership.
    async fn cancel(&self) -> Result<(), String>;
}

/// The server's contract with a desktop native computer-use adapter.
///
/// Every method receives a [`NativeRuntimeScope`] derived from a validated
/// session capability token. The implementation must revalidate scope before
/// touching any native resource - the token alone is not authorization; the
/// runtime owns live per-session ownership, consent, and Stop state.
#[async_trait]
pub trait NativeRuntime: Send + Sync {
    /// Whether this host can run native computer-use operations at all.
    fn is_available(&self) -> bool {
        false
    }

    /// Execute one canonical, validated operation scoped to a session.
    ///
    /// The runtime may return [`NativeRuntimeError::Recovered`] when the exact
    /// call and arguments already have a stored result. Implementations must
    /// expose the stored result through [`Self::result_for_call`] so the route
    /// can answer it.
    async fn execute(
        &self,
        scope: &NativeRuntimeScope,
        call: &ComputerUseCall,
    ) -> Result<ComputerUseResult, NativeRuntimeError>;

    /// Fetch a stored result for an exact call identity and arguments.
    async fn result_for_call(
        &self,
        scope: &NativeRuntimeScope,
        call: &ComputerUseCall,
    ) -> Result<Option<ComputerUseResult>, NativeRuntimeError>;

    /// Cancel the operation currently owned by `scope.session`, if any.
    async fn cancel_session(&self, scope: &NativeRuntimeScope) -> Result<(), String>;

    /// Synchronously revoke all native computer-use authority for
    /// `scope.session`. Called only when the logical code session has ended.
    /// Implementations must leave an enduring tombstone so a stale or
    /// reissued HTTP token can never lazily recreate native authority for
    /// that session id. Idempotent.
    fn revoke_session(&self, scope: &NativeRuntimeScope);
}
