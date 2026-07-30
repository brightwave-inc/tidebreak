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
    protocol::{AttachAccepted, AttachRefused, AttachRequest, ErrorCode, HandshakeResponse},
    reverse::{ControlFrame, RequestFrame, ReverseResponseEnvelope},
    wire::{read_frame, write_frame, write_prioritized, FrameError, WireFrame, ATTACH_TIMEOUT},
};

/// Why dialing and attaching to a sandbox over the wire failed.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// The sandbox refused the version; it answered with an on-wire
    /// [`AttachRefused`] carrying its own version, and the connection is not
    /// established.
    #[error("attach refused: sandbox speaks protocol version {}", .0.protocol_version)]
    VersionRefused(AttachRefused),
    /// The sandbox refused the presented transport secret; the connection is not
    /// established and no capability was served. The error text carries no secret.
    #[error("attach refused: the sandbox rejected the transport secret")]
    Unauthenticated(AttachRefused),
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
    /// [`ConnectError::VersionRefused`] on a version mismatch or
    /// [`ConnectError::Unauthenticated`] on a rejected transport secret (in either
    /// case the connection is not established), [`ConnectError::Handshake`] if the
    /// sandbox closes or answers with a non-handshake frame, or
    /// [`ConnectError::Transport`] on a transport failure during attach.
    pub async fn connect<S>(
        stream: S,
        attach: AttachRequest,
        host: CapabilityHost,
    ) -> Result<HostConnection, ConnectError>
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        // The host caps its inbound event buffer at MAX_BUFFERED_EVENTS — the same
        // ceiling a conforming sandbox self-limits its un-acknowledged events to,
        // so a conforming peer never fills it, but a misbehaving one is torn down
        // rather than allowed to grow host memory without bound.
        Self::connect_with(stream, attach, host, crate::protocol::MAX_BUFFERED_EVENTS).await
    }

    /// [`connect`](Self::connect) with an explicit inbound event-buffer bound,
    /// for exercising the host's ceiling at a small, cheap scale.
    pub(crate) async fn connect_with<S>(
        stream: S,
        attach: AttachRequest,
        host: CapabilityHost,
        event_capacity: usize,
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

            // Bounded the same way the sandbox bounds its read of the attach: a
            // peer that accepts the connection and then says nothing would
            // otherwise park this dial forever.
            let answer = match tokio::time::timeout(ATTACH_TIMEOUT, read_frame(&mut reader)).await {
                Ok(answer) => answer,
                Err(_elapsed) => {
                    return Err(ConnectError::Handshake(
                        "the sandbox did not answer the handshake in time".to_owned(),
                    ));
                }
            };
            let accepted = match answer {
                Ok(WireFrame::Handshake(HandshakeResponse::Accepted(accepted))) => accepted,
                Ok(WireFrame::Handshake(HandshakeResponse::Refused(refused))) => {
                    return Err(match refused.code {
                        ErrorCode::Unauthenticated => ConnectError::Unauthenticated(refused),
                        _ => ConnectError::VersionRefused(refused),
                    });
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

            // The event channel is BOUNDED at the host's ceiling. The read loop
            // forwards events with a non-blocking `try_send`, so a slow
            // `next_event` consumer never stalls reverse-request dispatch or ping
            // handling (the decoupling this design needs); but the bound caps host
            // memory, and a peer that overruns it is torn down as a protocol
            // violation rather than buffered without limit.
            let (event_tx, event_rx) = mpsc::channel::<SandboxEvent>(event_capacity.max(1));
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
    ///
    /// Forwarded events are buffered off the read loop's path (a non-blocking
    /// hand-off) so a slow consumer cannot stall reverse-request dispatch or
    /// liveness. That buffer is bounded at the host's own ceiling
    /// (MAX_BUFFERED_EVENTS): a conforming sandbox self-limits below it and never
    /// trips it, but a peer that overruns it — buggy or hostile — has its
    /// connection torn down (this stream then returns `None`) rather than being
    /// allowed to grow host memory without bound. Draining, and
    /// [`acknowledge`](Self::acknowledge)ing, keeps the backlog from parking the
    /// run.
    pub async fn next_event(&mut self) -> Option<SandboxEvent> {
        self.events.recv().await
    }

    /// Deliver the run init — task, policy snapshot, and (for a detached run)
    /// the scoped token — to the sandbox. Sent after the handle has committed
    /// on the host, and on every attach: the sandbox keeps the first delivery
    /// and ignores the rest. Best-effort: a closed connection drops it, and the
    /// reattach redelivers.
    pub async fn send_init(&self, init: crate::init::RunInit) {
        let _ = self.outbound.data.send(WireFrame::Init(init)).await;
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
    use tokio::sync::mpsc::error::TrySendError;
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
                // An inbound event is untrusted input. `read_frame` bounds a frame
                // at MAX_FRAME_BYTES; re-enforce the smaller per-event payload cap
                // here, and refuse an over-bound event as a protocol violation
                // rather than forward it.
                if !event.payload.within_bounds() {
                    break;
                }
                // Non-blocking: forwarding an event must never stall this loop's
                // reverse-request dispatch or ping handling. A `Full` channel means
                // the peer overran the host's event ceiling — impossible for a
                // conforming sandbox, which self-limits below it — so tear the
                // connection down instead of blocking or buffering without bound.
                // `Closed` means the owner dropped the connection.
                match events.try_send(event) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_) | TrySendError::Closed(_)) => break,
                }
            }
            // The host answers requests; it never receives responses or another
            // handshake mid-connection. Ignore rather than trust peer input.
            WireFrame::Request(RequestFrame::Response(_))
            | WireFrame::Attach(_)
            | WireFrame::Handshake(_)
            | WireFrame::EventAck { .. }
            | WireFrame::Init(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use super::*;
    use crate::{
        events::EventPayload,
        ids::{RunId, Sequence},
        oplog::InMemoryOperationStore,
        protocol::{Response, MAX_EVENT_PAYLOAD_BYTES, PROTOCOL_VERSION},
        reverse::{CapabilityResponder, GrantSet, ReverseRequest, ReverseResult, RunProvenance},
    };

    /// A responder that is never invoked: these tests drive only the event lane.
    struct NoopResponder;

    #[async_trait::async_trait]
    impl CapabilityResponder for NoopResponder {
        async fn respond(&self, _request: ReverseRequest) -> Response<ReverseResult> {
            unreachable!("the ceiling tests never issue a reverse request")
        }
    }

    fn events_only_host() -> CapabilityHost {
        CapabilityHost::new(
            GrantSet::none(RunProvenance {
                run_id: RunId::new(),
                provider: "wire-host-test".to_owned(),
            }),
            Arc::new(NoopResponder),
            Arc::new(InMemoryOperationStore::new()),
        )
    }

    fn attach() -> AttachRequest {
        AttachRequest {
            protocol_version: PROTOCOL_VERSION,
            run_id: RunId::new(),
            resume_from: EventCursor::START,
            transport_secret: crate::provisioning::TransportSecret::new("event-lane-test"),
        }
    }

    /// Answer the attach handshake on a raw sandbox side, then hand back the write
    /// half so the caller can flood frames directly.
    async fn accept_handshake<S>(
        sandbox: S,
    ) -> (BufReader<tokio::io::ReadHalf<S>>, tokio::io::WriteHalf<S>)
    where
        S: AsyncRead + AsyncWrite,
    {
        let (read_half, mut write_half) = split(sandbox);
        let mut reader = BufReader::new(read_half);
        let _ = read_frame(&mut reader).await;
        let accepted = WireFrame::Handshake(HandshakeResponse::Accepted(AttachAccepted {
            protocol_version: PROTOCOL_VERSION,
            granted_capabilities: Vec::new(),
            latest_sequence: None,
        }));
        let _ = write_frame(&mut write_half, &accepted).await;
        (reader, write_half)
    }

    /// A peer that floods more un-acknowledged events than the host's ceiling is
    /// torn down — the host buffers at most its ceiling and then closes the
    /// stream, rather than growing memory without bound or stalling.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_peer_that_overruns_the_event_ceiling_is_torn_down() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let capacity = 4usize;
            let (host_side, sandbox_side) = tokio::io::duplex(64 * 1024);

            let flood = tokio::spawn(async move {
                let (_reader, mut write_half) = accept_handshake(sandbox_side).await;
                // Flood well past the host's ceiling, never reading an ack.
                for seq in 1..=(capacity as u64 + 50) {
                    let event = WireFrame::Event(SandboxEvent {
                        sequence: Sequence::new(seq),
                        payload: EventPayload::Progress(format!("e{seq}")),
                    });
                    if write_frame(&mut write_half, &event).await.is_err() {
                        break;
                    }
                }
            });

            let mut conn =
                WireClient::connect_with(host_side, attach(), events_only_host(), capacity)
                    .await
                    .expect("attach accepted");

            // Let the read loop fill the bounded channel and tear down on overrun.
            tokio::time::sleep(Duration::from_millis(100)).await;

            // The stream yields at most the ceiling, then closes (teardown) — it
            // does not hang (the outer timeout would catch that) or buffer without
            // bound (the count would exceed the ceiling).
            let mut count = 0usize;
            while conn.next_event().await.is_some() {
                count += 1;
                assert!(count <= capacity, "host buffered past its ceiling");
            }
            assert!(
                count <= capacity,
                "host tore down after buffering at most its ceiling, got {count}"
            );

            let _ = flood.await;
        })
        .await
        .expect("test completed within its time bound");
    }

    /// A single over-bound event (within the 1 MiB frame cap but past the 64 KiB
    /// per-event payload cap) is refused at the host as a protocol violation.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_over_bound_event_payload_is_refused_at_the_host() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let (host_side, sandbox_side) = tokio::io::duplex(4 * 1024 * 1024);

            let peer = tokio::spawn(async move {
                let (_reader, mut write_half) = accept_handshake(sandbox_side).await;
                let event = WireFrame::Event(SandboxEvent {
                    sequence: Sequence::new(1),
                    payload: EventPayload::Progress("x".repeat(MAX_EVENT_PAYLOAD_BYTES + 1)),
                });
                let _ = write_frame(&mut write_half, &event).await;
            });

            let mut conn = WireClient::connect_with(host_side, attach(), events_only_host(), 16)
                .await
                .expect("attach accepted");

            // The over-bound event is dropped and the connection torn down, so the
            // stream closes without ever yielding it.
            assert!(
                conn.next_event().await.is_none(),
                "an over-bound event must be refused, not forwarded"
            );

            let _ = peer.await;
        })
        .await
        .expect("test completed within its time bound");
    }
}
