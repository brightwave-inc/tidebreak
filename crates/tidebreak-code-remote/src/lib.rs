//! Remote session execution: the client side of the confining environment's
//! sandbox runtime API.
//!
//! Decision [`0079`] split remote execution in two: the workload half is
//! `tidebreak-supervised-agent`, polling outward to a control endpoint the
//! environment owns; this module is the other half — the Tidebreak machine
//! asking that environment to provision, watch, steer, and stop the sandbox
//! a remote session's engine runs in. Tidebreak never dials into the pod.
//!
//! The pinned contract (`/api/v1/runtime/...` on the gateway):
//!
//! - `POST /api/v1/runtime/endpoints/{endpoint_slug}/sandboxes` — spawn.
//!   Preflight refuses loudly (unknown profile, unreachable repository, a
//!   ref the remote does not advertise, an unresolvable credential) before
//!   anything is provisioned.
//! - `GET /api/v1/runtime/sandboxes/{id}` — status. Cheap to poll.
//! - `GET /api/v1/runtime/sandboxes/{id}/events` — the durable, sequenced,
//!   gap-free event stream; `after_seq` resumes without loss, `wait_seconds`
//!   (at most 25) holds until an event lands.
//! - `POST /api/v1/runtime/sandboxes/{id}/messages` — append to the inbox
//!   the agent drains. At-least-once and asynchronous: a receipt means
//!   durable, not read.
//! - `POST /api/v1/runtime/sandboxes/{id}/cancel` — stop, with a short
//!   grace so the terminal WIP checkpoint can land.
//!
//! Every call authenticates with a short-lived `mg_at_` bearer minted for
//! the resource `runtime:{endpoint_slug}` carrying the `runtime:execute`
//! scope. Minting is behind [`RuntimeTokenSource`] because whose credential
//! backs it is an owner-scoped policy question (the caller's machine-bound
//! session, decisions 0049/0051), not a transport one.
//!
//! [`0079`]: ../../../docs/decisions/0079-supervised-agent-declines-the-sandbox-protocol.md

pub mod driver;
#[cfg(test)]
mod fixtures;
pub mod gateway;
mod ingest;
pub mod wire;

use async_trait::async_trait;
use tidebreak_core::code::SequencedEvent;
use tidebreak_core::{Attention, DbStore, Event, FenceReason, OwnerId, Session, SessionId};

use wire::{
    EventCursor, MessageReceipt, SandboxEvents, SandboxLease, SandboxMessage, SandboxStatus,
    SpawnArguments,
};

/// Why one runtime call did not produce its result.
#[derive(Debug, thiserror::Error)]
pub enum RemoteSandboxError {
    /// The environment named a refusal; retrying the same request cannot
    /// fix it. Carries the environment's own code and description so the
    /// session surfaces the real reason, not a paraphrase.
    #[error("the sandbox environment refused {operation} ({code}): {message}")]
    Refused {
        /// Which call was refused.
        operation: &'static str,
        /// Machine-readable refusal code.
        code: String,
        /// The environment's own description.
        message: String,
    },
    /// The runtime token was rejected or cannot be minted; the owner signs
    /// in again. Never retried onto another credential.
    #[error("the sandbox environment rejected the runtime credential: {0}")]
    SignInRequired(String),
    /// A transport or server fault; retryable on the caller's cadence.
    #[error("{operation} did not reach the sandbox environment: {detail}")]
    Unavailable {
        /// Which call failed.
        operation: &'static str,
        /// What went wrong.
        detail: String,
    },
    /// This side refused the request before sending it.
    #[error("invalid sandbox request: {0}")]
    InvalidRequest(String),
}

impl RemoteSandboxError {
    /// Whether retrying the same call later can succeed.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }
}

/// A short-lived bearer for the runtime surface.
///
/// The secret is the whole value: no `Clone`, and `Debug` redacts it.
pub struct RuntimeToken {
    /// The `mg_at_` access token presented as the bearer.
    pub secret: String,
}

impl std::fmt::Debug for RuntimeToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeToken")
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// Mints the runtime bearer one owner's calls present.
///
/// Owner-scoped so spend, repository access, and policy follow the person
/// (decision 0065's stance, applied to sandboxes): a session's sandbox is
/// provisioned as its owner, never as a shared machine identity.
#[async_trait]
pub trait RuntimeTokenSource: Send + Sync {
    /// A live token for `owner`, minted or refreshed as needed.
    ///
    /// # Errors
    ///
    /// [`RemoteSandboxError::SignInRequired`] when no credential for this
    /// owner can mint one.
    async fn runtime_token(&self, owner: &OwnerId) -> Result<RuntimeToken, RemoteSandboxError>;
}

/// The provisioning client a remote session drives its sandbox through.
///
/// One method per pinned route. Implementations are transport only: no
/// lifecycle decisions, no retries past what a single call needs, no
/// interpretation of event payloads — the session runtime owns all of that.
#[async_trait]
pub trait SandboxProvisioner: Send + Sync {
    /// Asks the environment to provision one sandbox.
    ///
    /// Preflight refusals surface as [`RemoteSandboxError::Refused`] with
    /// the environment's code; nothing was provisioned in that case.
    async fn spawn(
        &self,
        owner: &OwnerId,
        arguments: &SpawnArguments,
    ) -> Result<SandboxLease, RemoteSandboxError>;

    /// One sandbox's current lifecycle state.
    async fn status(
        &self,
        owner: &OwnerId,
        sandbox_id: &str,
    ) -> Result<SandboxStatus, RemoteSandboxError>;

    /// Reads the durable event stream after a cursor, oldest first.
    ///
    /// Holding `wait_seconds` under [`wire::EVENTS_MAX_WAIT_SECONDS`] keeps
    /// the environment's clamp out of the picture.
    async fn events(
        &self,
        owner: &OwnerId,
        sandbox_id: &str,
        cursor: EventCursor,
    ) -> Result<SandboxEvents, RemoteSandboxError>;

    /// Appends one message to the sandbox's inbox.
    async fn send(
        &self,
        owner: &OwnerId,
        sandbox_id: &str,
        message: &SandboxMessage,
    ) -> Result<MessageReceipt, RemoteSandboxError>;

    /// Asks the environment to stop the sandbox. Idempotent server-side; a
    /// running sandbox keeps a short grace so its terminal checkpoint lands.
    async fn cancel(&self, owner: &OwnerId, sandbox_id: &str) -> Result<(), RemoteSandboxError>;
}

/// Supplies the session operations that stay owned by the embedding server.
///
/// The remote driver owns sandbox lifecycle decisions. The server still owns
/// the shared session journal, attention, recovery, and live event bus, so the
/// driver reaches those operations through this boundary without depending
/// on the server crate.
#[async_trait]
pub trait RemoteSessionHost: Send + Sync {
    /// Publish one event after its journal row commits.
    fn publish(&self, session: SessionId, event: SequencedEvent);

    /// Persist the session and publish its derived state.
    async fn persist_session(
        &self,
        store: &DbStore,
        session: &Session,
    ) -> Result<bool, tidebreak_core::AgentError>;

    /// Apply one attention transition and publish its derived state.
    async fn apply_attention(
        &self,
        store: &DbStore,
        owner: &OwnerId,
        session_id: SessionId,
        next: Attention,
    ) -> Result<(), tidebreak_core::AgentError>;

    /// Append a session event. Failures are best effort for this call site.
    async fn journal_event(
        &self,
        store: &DbStore,
        owner: &OwnerId,
        session_id: SessionId,
        spawn_epoch: i64,
        event: Event,
    );

    /// Fence one live session and publish the resulting state.
    async fn fence_session(
        &self,
        store: &DbStore,
        session: &mut Session,
        reason: FenceReason,
    ) -> Result<(), tidebreak_core::AgentError>;

    /// Settle a turn left open by a stopped sandbox.
    async fn recover_dead_worker(
        &self,
        store: &DbStore,
        session: &Session,
    ) -> Result<Option<Session>, tidebreak_core::AgentError>;

    /// Resolve a fenced remote session after its sandbox stops.
    async fn reap_session(
        &self,
        store: &DbStore,
        session: Session,
    ) -> Result<Session, RemoteReapError>;
}

/// Why a fenced remote session could not be reaped.
#[derive(Debug, thiserror::Error)]
pub enum RemoteReapError {
    /// A store operation failed.
    #[error(transparent)]
    Store(#[from] tidebreak_core::AgentError),
    /// The host refused to resolve the fence.
    #[error("{0}")]
    Host(String),
}

pub(crate) fn replace_attention(session: &mut Session, next: Attention, from_user: bool) -> bool {
    if !from_user && !tidebreak_core::should_replace(&session.attention, &next) {
        return false;
    }
    if session.attention == next {
        return false;
    }
    session.attention = next;
    true
}

pub(crate) async fn persist_session(
    store: &DbStore,
    host: &dyn RemoteSessionHost,
    session: &Session,
) -> Result<bool, tidebreak_core::AgentError> {
    host.persist_session(store, session).await
}

pub(crate) async fn apply_attention(
    store: &DbStore,
    host: &dyn RemoteSessionHost,
    owner: &OwnerId,
    session_id: SessionId,
    next: Attention,
) -> Result<(), tidebreak_core::AgentError> {
    host.apply_attention(store, owner, session_id, next).await
}

pub(crate) async fn journal_event(
    store: &DbStore,
    host: &dyn RemoteSessionHost,
    owner: &OwnerId,
    session_id: SessionId,
    spawn_epoch: i64,
    event: Event,
) {
    host.journal_event(store, owner, session_id, spawn_epoch, event)
        .await;
}

pub(crate) async fn fence_session(
    store: &DbStore,
    host: &dyn RemoteSessionHost,
    session: &mut Session,
    reason: FenceReason,
) -> Result<(), tidebreak_core::AgentError> {
    host.fence_session(store, session, reason).await
}

pub(crate) async fn recover_dead_worker(
    store: &DbStore,
    host: &dyn RemoteSessionHost,
    session: &Session,
) -> Result<Option<Session>, tidebreak_core::AgentError> {
    host.recover_dead_worker(store, session).await
}

pub(crate) async fn reap_session(
    store: &DbStore,
    host: &dyn RemoteSessionHost,
    session: Session,
) -> Result<Session, RemoteReapError> {
    host.reap_session(store, session).await
}
