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
//! [`0079`]: ../../../../../docs/decisions/0079-supervised-agent-declines-the-sandbox-protocol.md

// The session runtime consumes this module in a later slice (#2873); until
// then only the tests construct it. Same stance as `scripted_harness`.
#![cfg_attr(not(test), allow(dead_code))]

pub(crate) mod driver;
#[cfg(test)]
pub(crate) mod fixtures;
pub(crate) mod gateway;
pub(crate) mod ingest;
pub(crate) mod wire;

use async_trait::async_trait;
use tidebreak_core::OwnerId;

use wire::{
    EventCursor, MessageReceipt, SandboxEvents, SandboxLease, SandboxMessage, SandboxStatus,
    SpawnArguments,
};

/// Why one runtime call did not produce its result.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RemoteSandboxError {
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
    pub(crate) fn is_retryable(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }
}

/// A short-lived bearer for the runtime surface.
///
/// The secret is the whole value: no `Clone`, and `Debug` redacts it.
pub(crate) struct RuntimeToken {
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
pub(crate) trait RuntimeTokenSource: Send + Sync {
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
pub(crate) trait SandboxProvisioner: Send + Sync {
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
