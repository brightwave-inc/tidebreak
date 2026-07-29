//! The sandbox-side transport server: accept a host connection, answer the
//! version handshake, and serve the run over the wire.
//!
//! [`SandboxRun`] is the run-scoped state that outlives any single connection:
//! the resumable event buffer and the reverse-RPC lane the in-container agent
//! calls into. A connection is transient plumbing. When the host reconnects, it
//! reattaches to the *same* `SandboxRun`, so buffered events replay from the
//! host's committed cursor and reverse calls resume with the same
//! [`OperationId`](crate::ids::OperationId).
//!
//! The agent drives the run through the cloneable [`SandboxRun`] handle: it
//! [`emit_progress`](SandboxRun::emit_progress)es events, submits a final result
//! with [`emit_result`](SandboxRun::emit_result), and dials the host for a model
//! completion with [`call`](SandboxRun::call). Reverse-RPC availability is keyed
//! to attachment: a `call` with no attached host waits for the host to attach.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use tokio::{
    io::{split, AsyncRead, AsyncWrite, BufReader},
    sync::{mpsc, oneshot, watch, Semaphore},
};

use crate::{
    events::{EventPayload, SandboxEvent},
    ids::{EventCursor, OperationId, RequestId, Sequence},
    protocol::{
        handshake, AttachAccepted, HandshakeResponse, Response, MAX_BUFFERED_EVENTS,
        MAX_INFLIGHT_REQUESTS, PROTOCOL_VERSION,
    },
    reference::EmitError,
    reverse::{
        Capability, ControlFrame, RequestFrame, ReverseEnvelope, ReverseRequest, ReverseResult,
    },
    wire::{read_frame, write_frame, write_prioritized, FrameError, WireFrame},
};

/// The outcome of one sandbox-originated reverse call over the current
/// connection.
#[derive(Debug, Clone)]
pub enum ReverseOutcome {
    /// The host settled the call (a success or a transport-stable error).
    Settled(Response<ReverseResult>),
    /// The connection dropped before a response arrived. The host's execution
    /// keeps running and records, so re-issuing the same `OperationId` after the
    /// host reattaches returns the recorded outcome.
    Disconnected,
}

/// In-flight reverse requests awaiting their correlated response, keyed by the
/// per-attempt [`RequestId`].
type PendingResponses = Arc<Mutex<HashMap<RequestId, oneshot::Sender<Response<ReverseResult>>>>>;

/// The live connection's outbound handle, shared between the agent (which sends
/// reverse requests and events) and the serve loop (which completes responses).
#[derive(Clone)]
struct ConnHandle {
    /// Request and event frames; subject to the request-lane bound.
    data: mpsc::Sender<WireFrame>,
    /// The reserved control lane; drained ahead of `data`.
    control: mpsc::Sender<WireFrame>,
    /// In-flight reverse requests awaiting a correlated response.
    pending: PendingResponses,
    /// The request lane's in-flight bound. A reverse call acquires a permit
    /// before enqueuing; a cancel on the control lane acquires none.
    permits: Arc<Semaphore>,
    /// Set once this connection has dropped, so a late `call` fails fast.
    closed: Arc<AtomicBool>,
}

struct EventBuffer {
    events: Vec<SandboxEvent>,
    next_seq: u64,
    acked_through: u64,
    buffer_cap: usize,
    overflowed: bool,
}

struct RunInner {
    protocol_version: u32,
    granted: Vec<Capability>,
    request_lane_capacity: usize,
    events: Mutex<EventBuffer>,
    conn: watch::Sender<Option<ConnHandle>>,
}

/// One run-scoped, cloneable sandbox. Every clone shares one event buffer and one
/// live-connection slot.
#[derive(Clone)]
pub struct SandboxRun {
    inner: Arc<RunInner>,
}

impl SandboxRun {
    /// A run speaking the current [`PROTOCOL_VERSION`], granting `capabilities`
    /// deny-by-default, with the default event-buffer and request-lane bounds.
    #[must_use]
    pub fn new(capabilities: impl IntoIterator<Item = Capability>) -> Self {
        Self::with_config(
            PROTOCOL_VERSION,
            MAX_BUFFERED_EVENTS,
            MAX_INFLIGHT_REQUESTS,
            capabilities,
        )
    }

    /// A run with an explicit protocol version, event-buffer bound, and
    /// request-lane capacity, for exercising version refusal and backpressure at
    /// a small, cheap scale.
    #[must_use]
    pub fn with_config(
        protocol_version: u32,
        buffer_cap: usize,
        request_lane_capacity: usize,
        capabilities: impl IntoIterator<Item = Capability>,
    ) -> Self {
        let (conn, _) = watch::channel(None);
        Self {
            inner: Arc::new(RunInner {
                protocol_version,
                granted: capabilities.into_iter().collect(),
                request_lane_capacity: request_lane_capacity.max(1),
                events: Mutex::new(EventBuffer {
                    events: Vec::new(),
                    next_seq: Sequence::FIRST.get(),
                    acked_through: EventCursor::START.get(),
                    buffer_cap: buffer_cap.max(1),
                    overflowed: false,
                }),
                conn,
            }),
        }
    }

    /// Emit a bounded progress line, returning its assigned sequence.
    ///
    /// The event is buffered for resume and, if a host is attached, delivered
    /// live over the connection. Emitting does not require attachment: an event
    /// emitted while unattached is buffered and replays when the host attaches.
    ///
    /// # Errors
    /// [`EmitError::TooLarge`] past the per-event bound, or [`EmitError::Overflow`]
    /// when the un-acknowledged buffer is full.
    pub async fn emit_progress(&self, text: impl Into<String>) -> Result<Sequence, EmitError> {
        self.emit(EventPayload::Progress(text.into())).await
    }

    /// Emit the run's terminal result submission, returning its sequence.
    ///
    /// # Errors
    /// [`EmitError::TooLarge`] past the per-event bound, or [`EmitError::Overflow`]
    /// when the un-acknowledged buffer is full.
    pub async fn emit_result(&self, text: impl Into<String>) -> Result<Sequence, EmitError> {
        self.emit(EventPayload::Result(text.into())).await
    }

    async fn emit(&self, payload: EventPayload) -> Result<Sequence, EmitError> {
        let event = self.buffer_event(payload)?;
        let sequence = event.sequence;
        if let Some(conn) = self.current_conn() {
            // Live delivery on the current connection. A send failure only means
            // the connection dropped; the event stays buffered for replay.
            let _ = conn.data.send(WireFrame::Event(event)).await;
        }
        Ok(sequence)
    }

    fn buffer_event(&self, payload: EventPayload) -> Result<SandboxEvent, EmitError> {
        if !payload.within_bounds() {
            return Err(EmitError::TooLarge);
        }
        let mut buffer = self.inner.events.lock().expect("event buffer lock");
        let unacked = (buffer.next_seq - 1).saturating_sub(buffer.acked_through);
        if unacked >= buffer.buffer_cap as u64 {
            buffer.overflowed = true;
            return Err(EmitError::Overflow);
        }
        let sequence = Sequence::new(buffer.next_seq);
        let event = SandboxEvent { sequence, payload };
        buffer.events.push(event.clone());
        buffer.next_seq += 1;
        Ok(event)
    }

    /// Dial the host for one host-proxied capability under the durable
    /// `operation_id`, awaiting the host's answer.
    ///
    /// Waits for a host to be attached (reverse RPC is keyed to attachment), then
    /// acquires a request-lane permit — the backpressure point — before enqueuing
    /// the request frame. A disconnect mid-flight yields
    /// [`ReverseOutcome::Disconnected`]; re-issue the same `operation_id` after
    /// the host reattaches to receive the recorded outcome.
    pub async fn call(&self, operation_id: OperationId, request: ReverseRequest) -> ReverseOutcome {
        let Some(conn) = self.attached().await else {
            return ReverseOutcome::Disconnected;
        };
        let Ok(_permit) = Arc::clone(&conn.permits).acquire_owned().await else {
            return ReverseOutcome::Disconnected;
        };
        if conn.closed.load(Ordering::SeqCst) {
            return ReverseOutcome::Disconnected;
        }

        let request_id = RequestId::new();
        let (tx, rx) = oneshot::channel();
        conn.pending
            .lock()
            .expect("pending lock")
            .insert(request_id, tx);

        let envelope = ReverseEnvelope {
            protocol_version: self.inner.protocol_version,
            request_id,
            operation_id,
            request,
        };
        if conn
            .data
            .send(WireFrame::Request(RequestFrame::Request(envelope)))
            .await
            .is_err()
        {
            conn.pending
                .lock()
                .expect("pending lock")
                .remove(&request_id);
            return ReverseOutcome::Disconnected;
        }

        match rx.await {
            Ok(response) => ReverseOutcome::Settled(response),
            Err(_) => ReverseOutcome::Disconnected,
        }
    }

    /// Cancel an in-flight reverse operation over the reserved control lane.
    ///
    /// The cancel acquires no request-lane permit and rides the control queue,
    /// which the writer drains ahead of the request lane, so it lands even while
    /// the request lane is saturated.
    pub fn cancel(&self, operation_id: OperationId) {
        if let Some(conn) = self.current_conn() {
            let _ = conn
                .control
                .try_send(WireFrame::Control(ControlFrame::Cancel { operation_id }));
        }
    }

    fn current_conn(&self) -> Option<ConnHandle> {
        self.inner.conn.borrow().clone()
    }

    /// Wait until a host is attached, returning that connection.
    async fn attached(&self) -> Option<ConnHandle> {
        let mut rx = self.inner.conn.subscribe();
        loop {
            if let Some(conn) = rx.borrow_and_update().clone() {
                if !conn.closed.load(Ordering::SeqCst) {
                    return Some(conn);
                }
            }
            if rx.changed().await.is_err() {
                return None;
            }
        }
    }

    fn latest_sequence(&self) -> Option<Sequence> {
        let buffer = self.inner.events.lock().expect("event buffer lock");
        (buffer.next_seq > Sequence::FIRST.get()).then(|| Sequence::new(buffer.next_seq - 1))
    }

    /// Buffered events strictly newer than `cursor`, for a resume replay.
    fn events_since(&self, cursor: EventCursor) -> Vec<SandboxEvent> {
        self.inner
            .events
            .lock()
            .expect("event buffer lock")
            .events
            .iter()
            .filter(|event| cursor.precedes(event.sequence))
            .cloned()
            .collect()
    }

    /// Advance the un-acknowledged buffer to the host's committed cursor.
    fn acknowledge(&self, cursor: EventCursor) {
        let mut buffer = self.inner.events.lock().expect("event buffer lock");
        buffer.acked_through = buffer.acked_through.max(cursor.get());
        let unacked = (buffer.next_seq - 1).saturating_sub(buffer.acked_through);
        if unacked < buffer.buffer_cap as u64 {
            buffer.overflowed = false;
        }
    }
}

/// Serve one host connection against `run` until the host closes it.
///
/// Reads the [`AttachRequest`](crate::protocol::AttachRequest), answers the
/// handshake with the canonical [`handshake`] function (a version skew yields an
/// on-wire refusal and the connection is not established), and on acceptance
/// runs the connection: replays buffered events past the host's resume cursor,
/// carries the agent's reverse requests and events out, and correlates the host's
/// responses back. Returns when the connection drops; the `run` survives for the
/// host to reattach to.
///
/// # Errors
/// [`FrameError`] if the transport fails before or during the handshake.
pub async fn serve_connection<S>(stream: S, run: SandboxRun) -> Result<(), FrameError>
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (read_half, write_half) = split(stream);
    let mut reader = BufReader::new(read_half);
    let mut write_half = write_half;

    // The first frame must be the attach handshake.
    let attach = match read_frame(&mut reader).await? {
        WireFrame::Attach(attach) => attach,
        // A peer that opens with anything else is not speaking the protocol.
        _ => return Ok(()),
    };

    let response = handshake(
        &attach,
        run.inner.protocol_version,
        run.inner.granted.clone(),
        run.latest_sequence(),
    );
    write_frame(&mut write_half, &WireFrame::Handshake(response.clone())).await?;
    let _accepted: AttachAccepted = match response {
        HandshakeResponse::Accepted(accepted) => accepted,
        // The connection is refused and left unusable; the run stands.
        HandshakeResponse::Refused(_) => return Ok(()),
    };

    let (control_tx, control_rx) = mpsc::channel::<WireFrame>(8);
    let (data_tx, data_rx) = mpsc::channel::<WireFrame>(MAX_INFLIGHT_REQUESTS);
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let closed = Arc::new(AtomicBool::new(false));
    let conn = ConnHandle {
        data: data_tx,
        control: control_tx,
        pending: Arc::clone(&pending),
        permits: Arc::new(Semaphore::new(run.inner.request_lane_capacity)),
        closed: Arc::clone(&closed),
    };

    // Publish this connection as the run's live connection, then replay buffered
    // events strictly newer than the host's resume cursor. `send_replace`, not
    // `send`: the run holds no long-lived receiver, so `send` would be rejected
    // for want of one and leave the slot empty — `send_replace` updates the value
    // unconditionally, and a `call` that subscribes afterward still sees it.
    run.inner.conn.send_replace(Some(conn.clone()));
    for event in run.events_since(attach.resume_from) {
        if conn.data.send(WireFrame::Event(event)).await.is_err() {
            break;
        }
    }

    let writer = tokio::spawn(write_prioritized(write_half, control_rx, data_rx));

    // Serve inbound frames: correlate reverse responses, answer pings, and take
    // event acknowledgements.
    loop {
        let frame = match read_frame(&mut reader).await {
            Ok(frame) => frame,
            Err(_) => break,
        };
        match frame {
            WireFrame::Request(RequestFrame::Response(envelope)) => {
                if let Some(tx) = pending
                    .lock()
                    .expect("pending lock")
                    .remove(&envelope.request_id)
                {
                    let _ = tx.send(envelope.response);
                }
            }
            WireFrame::Control(ControlFrame::Ping { nonce }) => {
                let _ = conn
                    .control
                    .send(WireFrame::Control(ControlFrame::Pong { nonce }))
                    .await;
            }
            WireFrame::EventAck { cursor } => run.acknowledge(cursor),
            // The sandbox originates cancels and requests; it never receives
            // them. Pong is liveness only. Ignore rather than trust peer input.
            WireFrame::Control(ControlFrame::Pong { .. } | ControlFrame::Cancel { .. })
            | WireFrame::Request(RequestFrame::Request(_))
            | WireFrame::Attach(_)
            | WireFrame::Handshake(_)
            | WireFrame::Event(_) => {}
        }
    }

    // Disconnect: fail every in-flight reverse call and retire this connection.
    closed.store(true, Ordering::SeqCst);
    pending.lock().expect("pending lock").clear();
    // Clear the live slot only if it is still this connection — a reconnect may
    // already have installed a newer one.
    run.inner.conn.send_if_modified(|slot| match slot {
        Some(current) if Arc::ptr_eq(&current.closed, &closed) => {
            *slot = None;
            true
        }
        _ => false,
    });
    writer.abort();
    Ok(())
}
