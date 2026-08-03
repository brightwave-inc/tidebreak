//! Run-scoped model tokens for detached-admitted sandbox runs (issue #824,
//! slice 2).
//!
//! The invariant this seam carries: **no detached run ever holds a long-lived
//! credential.** A detached-admitted run receives exactly one model credential
//! — a token minted for that run, expiring no later than the run's absolute
//! deadline, and revoked when the run terminalizes. Everything else in the
//! container path proxies inference through the host and carries none.
//!
//! The trait is the host-side seam; who mints is a per-deployment fact. The
//! model gateway's OAuth surface today mints only session-level, resource-bound
//! access tokens through refresh-token rotation and can revoke only the whole
//! session's refresh token — it has no API to mint a token scoped to one run
//! with a caller-capped lifetime, nor to revoke one issued token without
//! killing the session. Until it grows one, [`GatewayScopedTokenIssuer`]
//! reports honestly unavailable and refuses to mint, and the detached-admission
//! gate reads that refusal as its `scoped_model_token_available` input — so the
//! gate stays closed for exactly the reason the doc names.

use async_trait::async_trait;
use openwave_core::{AgentError, Result};
use openwave_sandbox_protocol::init::ScopedModelToken;
use uuid::Uuid;

/// A minted run-scoped token with the absolute expiry the issuer bound it to.
///
/// The expiry is a claim the caller verifies against the run deadline before
/// delivering the token — an issuer that overruns the cap is refused, not
/// trusted.
pub(crate) struct MintedScopedToken {
    pub(crate) token: ScopedModelToken,
    /// When the token stops working, in Unix seconds. Must not exceed the
    /// deadline the mint was capped by.
    pub(crate) expires_at_unix_secs: u64,
}

/// An issuer of run-scoped, short-lived, revocable model tokens.
///
/// A long-lived provider API key never satisfies this trait: `mint` must
/// return a credential that dies with the run whether or not `revoke` is ever
/// reached.
#[async_trait]
pub(crate) trait ScopedModelTokenIssuer: Send + Sync {
    /// Whether this issuer can mint at all. This is the truthful input to the
    /// detached-admission gate's `scoped_model_token_available` precondition —
    /// it must never report `true` unless [`mint`](Self::mint) can succeed.
    fn available(&self) -> bool;

    /// Mint a token for `run_id` expiring no later than `deadline_unix_secs`.
    ///
    /// # Errors
    /// Fails when no token can be minted; the caller fails the run closed
    /// rather than delivering a detached init without one.
    async fn mint(&self, run_id: Uuid, deadline_unix_secs: u64) -> Result<MintedScopedToken>;

    /// Revoke whatever tokens were minted for `run_id`. Idempotent and safe to
    /// call on every terminal path, including for runs that never minted.
    ///
    /// # Errors
    /// Best-effort at the issuer: a failure is logged by callers, and the
    /// lifetime cap from [`mint`](Self::mint) still bounds the credential.
    async fn revoke(&self, run_id: Uuid) -> Result<()>;
}

/// The gateway-backed issuer position today: unavailable, fail-closed.
///
/// The gateway cannot yet mint run-scoped tokens (see the module doc), so this
/// issuer refuses to mint and reports unavailable — which keeps the
/// detached-admission gate closed on the `NoScopedModelToken` denial rather
/// than smuggling a session-level bearer into a container. When the gateway
/// grows a run-scoped mint/revoke surface, this type is where it plugs in.
pub(crate) struct GatewayScopedTokenIssuer;

#[async_trait]
impl ScopedModelTokenIssuer for GatewayScopedTokenIssuer {
    fn available(&self) -> bool {
        false
    }

    async fn mint(&self, _run_id: Uuid, _deadline_unix_secs: u64) -> Result<MintedScopedToken> {
        Err(AgentError::config(
            "the model gateway cannot mint run-scoped tokens; detached admission is unavailable",
        ))
    }

    async fn revoke(&self, _run_id: Uuid) -> Result<()> {
        // Nothing was ever minted; revocation is a no-op.
        Ok(())
    }
}
