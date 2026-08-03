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
    /// The egress the sandbox is granted, in the compiled deny-by-default form.
    /// Absent on the wire means [`SandboxNetworkPolicy::deny_all`], so a caller
    /// that predates the field provisions a sandbox with no egress rather than
    /// an unrestricted one.
    #[serde(default)]
    pub network_policy: SandboxNetworkPolicy,
}

/// The egress a provisioned sandbox may perform, compiled by the host into a
/// closed allowlist before it reaches a backend.
///
/// This is deliberately *not* the host's own policy vocabulary: destination
/// classes (for example the package-manager registry set) are expanded by the
/// host into exact hosts, so an enforcement point — a backend, or the egress
/// proxy a backend stands up — needs no class knowledge and no dependency
/// beyond this crate. The default is deny-everything.
///
/// A destination is permitted when [`permits`](Self::permits) says so; private,
/// loopback, and link-local address space is the enforcement point's own
/// obligation to refuse regardless of this policy.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxNetworkPolicy {
    /// Permit every public destination. Enforcement points must still refuse
    /// private, loopback, and link-local address space.
    #[serde(default)]
    pub allow_all_public: bool,
    /// Exact lowercase DNS hosts permitted on any port. Never wildcards: the
    /// host refuses wildcard entries at compile time rather than shipping them.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// Exact lowercase DNS hosts permitted on port 443 only — the compiled
    /// expansion of a registry destination class.
    #[serde(default)]
    pub https_only_hosts: Vec<String>,
}

impl SandboxNetworkPolicy {
    /// The deny-everything policy (also `Default`).
    #[must_use]
    pub fn deny_all() -> Self {
        Self::default()
    }

    /// Permit every public destination.
    #[must_use]
    pub fn open() -> Self {
        Self {
            allow_all_public: true,
            allowed_hosts: Vec::new(),
            https_only_hosts: Vec::new(),
        }
    }

    /// Whether this policy denies every destination.
    #[must_use]
    pub fn denies_everything(&self) -> bool {
        !self.allow_all_public && self.allowed_hosts.is_empty() && self.https_only_hosts.is_empty()
    }

    /// Whether `host:port` is permitted. `host` is matched case-insensitively
    /// against the exact entries; no wildcard or suffix matching exists here.
    #[must_use]
    pub fn permits(&self, host: &str, port: u16) -> bool {
        if self.allow_all_public {
            return true;
        }
        let matches = |entry: &String| entry.eq_ignore_ascii_case(host);
        self.allowed_hosts.iter().any(matches)
            || (port == 443 && self.https_only_hosts.iter().any(matches))
    }
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
///
/// The sandbox supervisor holds the expected secret and checks a presented token
/// against it with [`verify`](Self::verify) — a constant-time comparison — before
/// it installs a connection or serves any capability.
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

    /// Whether a `presented` token equals this expected secret.
    ///
    /// The comparison runs in time independent of *where* the two tokens diverge:
    /// it never returns on the first differing byte, so a caller cannot learn a
    /// correct prefix from how long the check took. A length difference — the
    /// length of a bearer token is not itself the secret — is the only early exit.
    /// Neither operand is logged or `Debug`-printed.
    #[must_use]
    pub fn verify(&self, presented: &TransportSecret) -> bool {
        constant_time_eq(self.0.as_bytes(), presented.0.as_bytes())
    }
}

impl Default for TransportSecret {
    /// The empty token: it authenticates against no configured secret. It stands
    /// for "no secret presented" so an attach that omits one deserializes to this
    /// and is refused, rather than being silently accepted.
    fn default() -> Self {
        Self(String::new())
    }
}

impl std::fmt::Debug for TransportSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TransportSecret([redacted])")
    }
}

/// Byte equality in time independent of where the inputs differ.
///
/// The difference is accumulated across every byte rather than returned on the
/// first mismatch, so two equal-length inputs are always compared in full; only a
/// length difference short-circuits. [`std::hint::black_box`] keeps the optimizer
/// from reintroducing an early exit over the accumulator.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut difference: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        difference |= x ^ y;
    }
    std::hint::black_box(difference) == 0
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

    /// Destroy every sandbox this backend holds whose correlation tag is *not*
    /// in `live_tags`, returning the handles it reclaimed.
    ///
    /// The host builds `live_tags` from its durable provisioning records, so a
    /// tagged sandbox the host does not recognize belongs to a lapsed intent —
    /// a run that never committed a handle, whether the host crashed before or
    /// after the create returned. `Ok` means the backend now holds **no**
    /// sandbox outside `live_tags`; that guarantee is what lets the sweep mark
    /// a handle-less teardown obligation done. The default is for backends
    /// that never hold provider-created sandboxes (the self-hosted case), for
    /// which the guarantee is vacuously true.
    ///
    /// # Errors
    /// [`BackendError::Teardown`] if the backend could not list its sandboxes
    /// or could not confirm every removal; the sweep retries.
    async fn reclaim_orphans(
        &self,
        live_tags: &std::collections::HashSet<SandboxTag>,
    ) -> Result<Vec<SandboxHandle>, BackendError> {
        let _ = live_tags;
        Ok(Vec::new())
    }

    /// Whether this backend enforces a sandbox lifetime cap from **outside**
    /// the sandbox, set at provisioning through
    /// [`ProvisionRequest::lifetime_cap_secs`] to no more than the run's
    /// absolute deadline.
    ///
    /// Detached admission requires this capability: without it, nothing bounds
    /// an orphaned sandbox whose host never returns. The default is `false`
    /// (fail closed) — a backend that does not declare the enforcement cannot
    /// host a detached run. A local container runtime never overrides this: no
    /// mechanism outside the container bounds its lifetime.
    fn enforces_external_lifetime_cap(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_the_secret() {
        let secret = TransportSecret::new("super-secret-token");
        let rendered = format!("{secret:?}");
        assert!(
            !rendered.contains("super-secret-token"),
            "Debug leaked the secret"
        );
        assert!(rendered.contains("redacted"));
    }

    #[test]
    fn verify_accepts_the_matching_secret_and_rejects_others() {
        let expected = TransportSecret::new("correct-horse-battery-staple");
        assert!(expected.verify(&TransportSecret::new("correct-horse-battery-staple")));
        // A wrong token of the SAME length is refused: the comparison examines
        // every byte, so a shared prefix does not pass.
        assert!(!expected.verify(&TransportSecret::new("Xorrect-horse-battery-staple")));
        assert!(!expected.verify(&TransportSecret::new("correct-horse-battery-staplX")));
        // A different length, and the empty (absent) token, are refused too.
        assert!(!expected.verify(&TransportSecret::new("short")));
        assert!(!expected.verify(&TransportSecret::default()));
    }

    #[test]
    fn constant_time_eq_examines_every_byte() {
        assert!(constant_time_eq(b"abcdef", b"abcdef"));
        // A difference at the first byte is caught.
        assert!(!constant_time_eq(b"Xbcdef", b"abcdef"));
        // A difference only at the LAST byte is the short-circuit trap: a compare
        // that returned `true` after the matching prefix would wrongly accept it.
        assert!(!constant_time_eq(b"abcdeX", b"abcdef"));
        // A length mismatch is refused.
        assert!(!constant_time_eq(b"abc", b"abcdef"));
    }
}
