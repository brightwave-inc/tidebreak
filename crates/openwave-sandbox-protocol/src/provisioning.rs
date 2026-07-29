//! The provision / address / destroy backend decomposition.
//!
//! A sandbox backend is exactly three operations, and the self-hosted case —
//! no provisioning, just an address and a credential — is the conformance test
//! for this decomposition: if it needs a special case, the abstraction is
//! wrong. So [`SandboxBackend::provision`] may be a no-op wrapping a
//! user-supplied endpoint, and [`SandboxBackend::destroy`] may be a no-op; the
//! host drives them identically either way.
//!
//! Reachability of a self-hosted backend is the user's responsibility: the
//! endpoint must be dialable from the host by the user's own means. OpenWave
//! operates no rendezvous or relay.

use serde::{Deserialize, Serialize};

use crate::ids::{RunId, SandboxTag};

/// A backend failed a provisioning operation without widening host trust.
#[derive(Debug, Clone, thiserror::Error)]
pub enum BackendError {
    /// The backend could not provision a sandbox.
    #[error("sandbox provisioning failed: {0}")]
    Provision(String),
    /// The handle does not resolve to a reachable address.
    #[error("sandbox handle is not addressable: {0}")]
    Unaddressable(String),
    /// Teardown could not be confirmed. Teardown is idempotent, so the caller
    /// re-drives the obligation rather than abandoning it.
    #[error("sandbox teardown is unconfirmed: {0}")]
    Teardown(String),
    /// The handle names no sandbox this backend provisioned.
    #[error("no sandbox matches this handle")]
    UnknownHandle,
}

/// The host's request to provision a sandbox for one run.
///
/// The host commits a durable provisioning intent carrying `tag` before this
/// call, and the backend stamps the tag into the sandbox's metadata so an orphan
/// sweep can reclaim a sandbox whose intent later lapsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvisionRequest {
    /// The run this sandbox serves.
    pub run_id: RunId,
    /// Host-minted correlation tag to stamp into sandbox metadata.
    pub tag: SandboxTag,
    /// The outer lifetime cap, in seconds, if the backend can enforce one from
    /// outside the sandbox. `None` means the backend cannot cap lifetime, which
    /// (per the design) makes the run attached-only.
    pub lifetime_cap_secs: Option<u64>,
}

/// An opaque, backend-specific handle to a provisioned sandbox.
///
/// The host commits this onto the run row with the attempt's result idempotency
/// key. It is transport-neutral: what it means is the backend's business, and
/// the host only ever hands it back to [`SandboxBackend::address`] and
/// [`SandboxBackend::destroy`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxHandle {
    /// The backend's own identifier for the sandbox (a container id, a vendor
    /// sandbox id, or a user-supplied endpoint key for a self-hosted backend).
    pub reference: String,
    /// The correlation tag this sandbox was stamped with, echoed back for the
    /// orphan sweep.
    pub tag: SandboxTag,
}

/// A per-run bearer credential the supervisor requires on every request.
///
/// It is a bearer secret and a deduplication aid, not proof of sandbox
/// authenticity: whoever carries it can impersonate the run to the host, which
/// is a trust-model fact, not fine print. It is never logged or `Debug`-printed.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TransportSecret(String);

impl TransportSecret {
    /// Wrap a minted secret.
    #[must_use]
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    /// The raw secret, for presenting on the transport.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for TransportSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TransportSecret([redacted])")
    }
}

/// A reachable base URL plus the per-run credential the host presents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxAddress {
    /// Where the host dials the sandbox supervisor. For a local container this
    /// is a loopback address; for a self-hosted backend it is the user-supplied
    /// endpoint; for the reference backend it is an in-process locator.
    pub base_url: String,
    /// The bearer credential to present on every request.
    pub transport_secret: TransportSecret,
}

/// The provision / address / destroy contract a sandbox backend implements.
///
/// Every method is host-driven; the sandbox never dials the host. Real backends
/// perform network I/O, so the trait is async and dyn-compatible.
#[async_trait::async_trait]
pub trait SandboxBackend: Send + Sync {
    /// Provision a sandbox for a run, returning an opaque handle.
    ///
    /// May be a no-op that wraps a user-supplied endpoint (the self-hosted
    /// case), in which case the returned handle simply names that endpoint.
    ///
    /// # Errors
    /// [`BackendError::Provision`] if the backend cannot stand up a sandbox.
    async fn provision(&self, request: ProvisionRequest) -> Result<SandboxHandle, BackendError>;

    /// Resolve a handle to a reachable address.
    ///
    /// # Errors
    /// [`BackendError::Unaddressable`] or [`BackendError::UnknownHandle`] if the
    /// handle does not resolve.
    async fn address(&self, handle: &SandboxHandle) -> Result<SandboxAddress, BackendError>;

    /// Destroy a sandbox. Idempotent from the host's side, and may be a no-op
    /// for a self-hosted backend the user tears down by their own means.
    ///
    /// # Errors
    /// [`BackendError::Teardown`] if teardown could not be confirmed; the caller
    /// re-drives the obligation on the next sweep.
    async fn destroy(&self, handle: &SandboxHandle) -> Result<(), BackendError>;
}
