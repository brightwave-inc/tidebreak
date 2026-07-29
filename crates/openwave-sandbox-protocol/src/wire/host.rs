//! The host-side transport client: dial a sandbox, do the version handshake,
//! then service the connection.
//!
//! The host dials the sandbox and holds the connection; "reverse" names only
//! which side originates requests over it (the sandbox does). Once the handshake
//! is accepted, the client runs the connection: it answers reverse requests
//! against the run-scoped [`CapabilityHost`] — the same operation-log seam that
//! fences idempotent replay in-process — forwards the sandbox's event stream to
//! the caller, and answers liveness pings. Reverse-RPC exactly-once is the
//! `CapabilityHost`'s job, not the transport's; the transport only carries the
//! frames.
//!
//! This client is what the local container backend (issue #874) connects to a
//! provisioned container with: dial the container's loopback address into any
//! [`AsyncRead`]`+`[`AsyncWrite`] stream (a [`TcpStream`](tokio::net::TcpStream)
//! there), hand it the [`AttachRequest`] and the run's `CapabilityHost`, and
//! drive the returned [`HostConnection`].

use tokio::{
    io::{split, AsyncRead, AsyncWrite, BufReader},
    sync::mpsc,
    task::JoinHandle,
};

use crate::{
    events::SandboxEvent,
    host::CapabilityHost,
    ids::EventCursor,
    protocol::{AttachAccepted, AttachRefused, AttachRequest, HandshakeResponse},
    reverse::{ControlFrame, RequestFrame, ReverseResponseEnvelope},
    wire::{read_frame, write_frame, write_prioritized, FrameError, WireFrame},
};

/// Why dialing and attaching to a sandbox over the wire failed.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// The sandbox refused the version; it answered with an on-wire
    /// [`AttachRefused`] carrying its own version, and the connection is not
    /// established.
    #[error("attach refused: sandbox speaks protocol version {}", .0.protocol_version)]
    VersionRefused(AttachRefused),
    /// The handshake did not complete: the sandbox closed the connection or sent
    /// something other than a handshake frame first.
    #[error("the sandbox did not complete the attach handshake: {0}")]
    Handshake(String),
    /// The transport failed before the handshake completed.
    #[error("transport failed during attach: {0}")]
    Transport(#[from] FrameError),
}

/// The host's live connection to a sandbox.
///
/// Holds the accepted handshake, a receiver for the sandbox's event stream, and
/// a handle to send control frames. Dropping it aborts the service tasks and so
/// drops the connection — the sandbox observes a disconnect and, for an
/// attached-only run, checkpoints and waits.
pub struct HostConnection {
    accepted: AttachAccepted,
    events: mpsc::Receiver<SandboxEvent>,
    outbound: Outbound,
    tasks: Vec<JoinHandle<()>>,
}

/// Prioritized outbound queues shared with the writer task: control frames drain
/// ahead of the request/ack lane.
#[derive(Clone)]
struct Outbound {
    control: mpsc::Sender<WireFrame>,
    data: mpsc::Sender<WireFrame>,
}

/// The host-side transport client. Its one job is to dial and attach; the live
/// connection it returns is a [`HostConnection`].
pub struct WireClient;

impl WireClient {
    /// Attach to a sandbox over `stream`: send the [`AttachRequest`], read the
    /// handshake, and — on acceptance — spawn the connection's service tasks
    /// wired to `host`.
    ///
    /// # Errors
    /// [`ConnectError::VersionRefused`] on a version mismatch (the connection is
    /// not established), [`ConnectError::Handshake`] if the sandbox closes or
    /// answers with a non-handshake frame, or [`ConnectError::Transport`] on a
    /// transport failure during attach.
    pub async fn connect<S>(
        stream: S,
        attach: AttachRequest,
        host: CapabilityHost,
    ) -> Result<HostConnection, ConnectError>
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (read_half, write_half) = split(stream);
        let mut reader = BufReader::new(read_half);

        // The control queue is depth-1 and drained first; the data queue carries
        // reverse responses and event acks.
        let (control_tx, control_rx) = mpsc::channel::<WireFrame>(8);
        let (data_tx, data_rx) = mpsc::channel::<WireFrame>(crate::protocol::MAX_INFLIGHT_REQUESTS);
        let outbound = Outbound {
            control: control_tx,
            data: data_tx,
        };

        // Send the handshake request, then await the answer, before spawning the
        // writer task — the handshake is a strict request/response prelude.
        {
            let mut write_half = write_half;
            write_frame(&mut write_half, &WireFrame::Attach(attach))
                .await
                .map_err(ConnectError::Transport)?;

            let accepted = match read_frame(&mut reader).await {
                Ok(WireFrame::Handshake(HandshakeResponse::Accepted(accepted))) => accepted,
                Ok(WireFrame::Handshake(HandshakeResponse::Refused(refused))) => {
                    return Err(ConnectError::VersionRefused(refused));
                }
                Ok(_) => {
                    return Err(ConnectError::Handshake(
                        "sandbox sent a non-handshake frame first".to_owned(),
                    ));
                }
                Err(FrameError::Closed) => {
                    return Err(ConnectError::Handshake(
                        "sandbox closed before answering the handshake".to_owned(),
                    ));
                }
                Err(error) => return Err(ConnectError::Transport(error)),
            };

            let (event_tx, event_rx) =
                mpsc::channel::<SandboxEvent>(crate::protocol::MAX_INFLIGHT_REQUESTS);
            let writer = tokio::spawn(write_prioritized(write_half, control_rx, data_rx));
            let reader_task = tokio::spawn(read_loop(reader, host, outbound.clone(), event_tx));

            Ok(HostConnection {
                accepted,
                events: event_rx,
                outbound,
                tasks: vec![writer, reader_task],
            })
        }
    }
}

impl HostConnection {
    /// The accepted handshake: the sandbox's version, the granted capabilities,
    /// and the highest sequence it held at attach.
    #[must_use]
    pub fn accepted(&self) -> &AttachAccepted {
        &self.accepted
    }

    /// Await the next event on the sandbox's stream, or `None` once the
    /// connection has drained and closed.
    pub async fn next_event(&mut self) -> Option<SandboxEvent> {
        self.events.recv().await
    }

    /// Acknowledge the event stream through `cursor`, letting the sandbox advance
    /// its un-acknowledged buffer. Best-effort: a closed connection drops it.
    pub async fn acknowledge(&self, cursor: EventCursor) {
        let _ = self
            .outbound
            .data
            .send(WireFrame::EventAck { cursor })
            .await;
    }

    /// Send a liveness ping over the reserved control lane. Best-effort.
    pub async fn ping(&self, nonce: u64) {
        let _ = self
            .outbound
            .control
            .send(WireFrame::Control(ControlFrame::Ping { nonce }))
            .await;
    }
}

impl Drop for HostConnection {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Service inbound frames: answer reverse requests against `host`, forward events
/// to `events`, and answer pings.
async fn read_loop<R>(
    mut reader: BufReader<R>,
    host: CapabilityHost,
    outbound: Outbound,
    events: mpsc::Sender<SandboxEvent>,
) where
    R: AsyncRead + Unpin + Send + 'static,
{
    // Per-request forwarders are detached: each awaits its operation's outcome
    // and writes the response frame. The execution behind an operation lives in
    // the CapabilityHost, decoupled from this connection, so a disconnect tears
    // down the forwarders but not the executions — the disconnect asymmetry the
    // reverse-RPC design requires.
    loop {
        let frame = match read_frame(&mut reader).await {
            Ok(frame) => frame,
            Err(_) => break,
        };
        match frame {
            WireFrame::Request(RequestFrame::Request(envelope)) => {
                let request_id = envelope.request_id;
                let operation_id = envelope.operation_id;
                let protocol_version = envelope.protocol_version;
                let waiter = host.dispatch(envelope);
                let data = outbound.data.clone();
                tokio::spawn(async move {
                    let response = waiter.wait().await;
                    let frame =
                        WireFrame::Request(RequestFrame::Response(ReverseResponseEnvelope {
                            protocol_version,
                            request_id,
                            operation_id,
                            response,
                        }));
                    let _ = data.send(frame).await;
                });
            }
            WireFrame::Control(ControlFrame::Cancel { operation_id }) => host.cancel(operation_id),
            WireFrame::Control(ControlFrame::Ping { nonce }) => {
                let _ = outbound
                    .control
                    .send(WireFrame::Control(ControlFrame::Pong { nonce }))
                    .await;
            }
            WireFrame::Control(ControlFrame::Pong { .. }) => {}
            WireFrame::Event(event) => {
                if events.send(event).await.is_err() {
                    // The caller dropped the connection; stop forwarding.
                    break;
                }
            }
            // The host answers requests; it never receives responses or another
            // handshake mid-connection. Ignore rather than trust peer input.
            WireFrame::Request(RequestFrame::Response(_))
            | WireFrame::Attach(_)
            | WireFrame::Handshake(_)
            | WireFrame::EventAck { .. } => {}
        }
    }
}
