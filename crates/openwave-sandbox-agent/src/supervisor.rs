//! The sandbox supervisor: the non-agent component that owns the transport
//! listener and (eventually) holds credentials and fronts egress.
//!
//! The supervisor is deliberately **distinct from the agent process**. Per the
//! [credential separation](../../docs/sandbox-providers.md) invariant, the
//! non-agent component owns the transport listener, holds credentials, and
//! substitutes them at its own egress boundary so a third-party credential never
//! enters the agent's address space. This slice ships the listener half of that
//! separation and leaves the credential/egress boundary as a **documented stub**
//! ([`CredentialProxy`]) marking exactly where it plugs in — the full egress
//! proxy is its own step and is not built here.
//!
//! In this crate the split is structural: [`Supervisor`] binds the listener and
//! serves each host connection against the run's transport ([`SandboxRun`]),
//! while the agent loop ([`crate::agent::run_agent`]) is a separate future that
//! only ever touches the same `SandboxRun` handle — never the listener, never a
//! credential. A tool that shells out would run under the supervisor's egress
//! boundary with a cleared environment and, where the platform allows, a
//! different UID; this slice has no subprocess tools, so that obligation is
//! recorded here rather than exercised.

use std::io;

use openwave_sandbox_protocol::{serve_connection, SandboxRun};
use tokio::net::TcpListener;

/// The credential and egress boundary the supervisor owns — a documented stub.
///
/// In the full design the agent addresses a connected service with an opaque
/// placeholder and this boundary substitutes the real credential and strips
/// agent-supplied authentication, terminating TLS at the supervisor. None of
/// that is built in this slice: an attached-only run carries no credential (the
/// host is the model proxy, reached over reverse RPC), so there is nothing to
/// substitute yet. This type exists to name the seam the egress step wires in.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct CredentialProxy;

impl CredentialProxy {
    /// A credential/egress boundary that holds and substitutes nothing.
    ///
    /// The real proxy replaces this: it holds the run's scoped credentials, is
    /// the only component that ever sees them, and applies the egress policy
    /// snapshot the host delivered at admission.
    #[must_use]
    pub fn stub() -> Self {
        Self
    }
}

/// The sandbox supervisor. Owns the transport listener; the agent loop does not.
pub struct Supervisor {
    listener: TcpListener,
    run: SandboxRun,
    #[allow(dead_code)]
    credentials: CredentialProxy,
}

impl Supervisor {
    /// Bind the transport listener on `addr` for the run behind `run`.
    ///
    /// The host dials this listener; the per-run transport secret authenticating
    /// the dial is delivered out of band through the provider control plane and
    /// is a follow-up on this listener (see the crate docs).
    ///
    /// # Errors
    /// Propagates the bind failure if the address cannot be listened on.
    pub async fn bind(addr: &str, run: SandboxRun) -> io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        Ok(Self {
            listener,
            run,
            credentials: CredentialProxy::stub(),
        })
    }

    /// The address the listener actually bound, so a caller that passed port 0
    /// can learn the assigned port.
    ///
    /// # Errors
    /// Propagates the failure if the local address cannot be read.
    pub fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }

    /// Accept host connections forever, serving each against the run's transport.
    ///
    /// Each connection is served on its own task, so a reconnect after a
    /// disconnect reattaches to the same [`SandboxRun`] — its event buffer and
    /// operation identities outlive any single connection.
    pub async fn serve(self) {
        while let Ok((stream, _peer)) = self.listener.accept().await {
            let run = self.run.clone();
            tokio::spawn(async move {
                let _ = serve_connection(stream, run).await;
            });
        }
    }
}
