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

use std::{io, time::Duration};

use openwave_sandbox_protocol::{serve_connection, SandboxRun};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpListener, TcpStream},
};

/// The first pause after an accept failure that is not a per-connection error.
const ACCEPT_BACKOFF_MIN: Duration = Duration::from_millis(5);

/// The ceiling the accept backoff grows to, so a listener that is failing for a
/// reason that never clears retries at a steady, cheap rate instead of spinning.
const ACCEPT_BACKOFF_MAX: Duration = Duration::from_secs(1);

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
    /// The host dials this listener and authenticates the dial with the per-run
    /// transport secret the backend injected; the run behind `run` holds the
    /// expected secret and [`serve_connection`] refuses an attach that does not
    /// present it, before installing the connection.
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
    ///
    /// The loop never ends on an accept failure. The listener is the run's only
    /// reachability: a supervisor that stopped accepting would leave a container
    /// that is alive and looks healthy but can never be attached to again, which
    /// is strictly worse than a process that exits and is observed to have died.
    /// So every `accept` error is retried — see [`serve_accepted`] for how the
    /// two classes of error are paced.
    pub async fn serve(self) {
        serve_accepted(self.listener, self.run).await;
    }
}

/// The accept loop, over anything that can accept connections, so the retry
/// behavior can be driven against a listener that fails on demand.
///
/// Errors split into two classes:
///
/// - A per-connection error (the peer went away between the SYN and the accept,
///   or the syscall was interrupted) says nothing about the listener. The failed
///   connection is dropped and the next one accepted immediately; pausing here
///   would let one aborted dial delay a real host's attach.
/// - Anything else — descriptor exhaustion above all — is a condition of the
///   process or the host, not of one connection, and retrying it in a tight loop
///   would spin a core. Those back off exponentially to
///   [`ACCEPT_BACKOFF_MAX`], and the delay resets once a connection is accepted.
async fn serve_accepted<L>(listener: L, run: SandboxRun)
where
    L: Accept,
{
    let mut backoff = ACCEPT_BACKOFF_MIN;
    loop {
        match listener.accept().await {
            Ok(stream) => {
                backoff = ACCEPT_BACKOFF_MIN;
                let run = run.clone();
                tokio::spawn(async move {
                    let _ = serve_connection(stream, run).await;
                });
            }
            Err(error) if is_per_connection(&error) => {}
            Err(_) => {
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(ACCEPT_BACKOFF_MAX);
            }
        }
    }
}

/// Whether an accept failure concerns only the connection being accepted, and so
/// carries no signal about the listener's health.
fn is_per_connection(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::Interrupted
    )
}

/// A source of inbound connections. Implemented for [`TcpListener`] in
/// production; the tests implement it over a scripted sequence of failures.
trait Accept {
    /// The stream one accepted connection is served over.
    type Stream: AsyncRead + AsyncWrite + Send + 'static;

    /// Accept the next connection, or report why none could be taken.
    async fn accept(&self) -> io::Result<Self::Stream>;
}

impl Accept for TcpListener {
    type Stream = TcpStream;

    async fn accept(&self) -> io::Result<TcpStream> {
        TcpListener::accept(self)
            .await
            .map(|(stream, _peer)| stream)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use openwave_sandbox_protocol::{
        protocol::{AttachRequest, HandshakeResponse, PROTOCOL_VERSION},
        read_frame, write_frame, EventCursor, RunId, TransportSecret, WireFrame,
    };
    use tokio::io::{split, BufReader, DuplexStream};

    use super::{io, serve_accepted, Accept, Duration, SandboxRun};

    /// A listener that hands out a scripted sequence of accept outcomes and then
    /// stops producing connections, as an idle listener does.
    struct ScriptedListener {
        steps: Mutex<VecDeque<io::Result<DuplexStream>>>,
    }

    impl Accept for ScriptedListener {
        type Stream = DuplexStream;

        async fn accept(&self) -> io::Result<DuplexStream> {
            let step = self.steps.lock().expect("script lock").pop_front();
            match step {
                Some(step) => step,
                // The script is spent: park, like a listener with nothing pending.
                None => std::future::pending().await,
            }
        }
    }

    /// A transient accept failure must not end the accept loop — a sandbox that
    /// stopped accepting is alive but unreachable forever. Both error classes are
    /// exercised, the per-connection one retried immediately and the
    /// listener-wide one backed off, and the connection behind them is still
    /// accepted and served.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn accept_failures_do_not_end_the_accept_loop() {
        let (host_side, sandbox_side) = tokio::io::duplex(64 * 1024);
        let listener = ScriptedListener {
            steps: Mutex::new(VecDeque::from([
                Err(io::Error::from(io::ErrorKind::ConnectionAborted)),
                // Descriptor exhaustion: the class that backs off.
                Err(io::Error::other("too many open files")),
                Ok(sandbox_side),
            ])),
        };
        let secret = TransportSecret::new("accept-loop-test");
        let run = SandboxRun::new([], Some(secret.clone()));
        tokio::spawn(serve_accepted(listener, run));

        let (read_half, mut write_half) = split(host_side);
        let mut reader = BufReader::new(read_half);
        let attach = WireFrame::Attach(AttachRequest {
            protocol_version: PROTOCOL_VERSION,
            run_id: RunId::new(),
            resume_from: EventCursor::START,
            transport_secret: secret,
        });
        write_frame(&mut write_half, &attach)
            .await
            .expect("send attach");

        let answered = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut reader))
            .await
            .expect("the connection behind the failures is accepted and served")
            .expect("handshake frame");
        assert!(
            matches!(
                answered,
                WireFrame::Handshake(HandshakeResponse::Accepted(_))
            ),
            "expected the attach to be accepted, got {answered:?}"
        );
    }
}
