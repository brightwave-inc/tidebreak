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
    collections::{HashMap, VecDeque},
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
    events::EmitError,
    events::{EventPayload, SandboxEvent},
    ids::{EventCursor, OperationId, RequestId, Sequence},
    protocol::{
        handshake, AttachAccepted, HandshakeResponse, Response, MAX_BUFFERED_EVENTS,
        MAX_INFLIGHT_REQUESTS, PROTOCOL_VERSION,
    },
    provisioning::TransportSecret,
    reverse::{
        Capability, ControlFrame, RequestFrame, ReverseEnvelope, ReverseRequest, ReverseResult,
    },
    wire::{read_frame, write_frame, write_prioritized, FrameError, WireFrame, ATTACH_TIMEOUT},
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
    /// The per-run transport secret the supervisor was configured with. An attach
    /// is authenticated against it before its connection is installed. `None`
    /// means no secret was configured, and the run fails closed — every attach is
    /// refused rather than served unauthenticated.
    expected_secret: Option<TransportSecret>,
    granted: Vec<Capability>,
    request_lane_capacity: usize,
    events: Mutex<EventBuffer>,
    conn: watch::Sender<Option<ConnHandle>>,
    /// The run init the host delivers after attach. First delivery wins; a
    /// redelivery on reattach is ignored.
    init: watch::Sender<Option<crate::init::RunInit>>,
    /// Monotonic activity generation observed by the sandbox idle watchdog.
    activity: watch::Sender<u64>,
    /// Host-sent steering instructions the agent loop has not consumed yet,
    /// oldest first and bounded by [`MAX_PENDING_STEERS`].
    steering: Mutex<VecDeque<String>>,
}

/// One run-scoped, cloneable sandbox. Every clone shares one event buffer and one
/// live-connection slot.
#[derive(Clone)]
pub struct SandboxRun {
    inner: Arc<RunInner>,
}

impl SandboxRun {
    /// A run speaking the current [`PROTOCOL_VERSION`], granting `capabilities`
    /// deny-by-default, authenticating attaches against `expected_secret`, with
    /// the default event-buffer and request-lane bounds.
    ///
    /// `expected_secret` is `None` only when no per-run secret was configured, in
    /// which case the run fails closed and refuses every attach.
    #[must_use]
    pub fn new(
        capabilities: impl IntoIterator<Item = Capability>,
        expected_secret: Option<TransportSecret>,
    ) -> Self {
        Self::with_config(
            PROTOCOL_VERSION,
            MAX_BUFFERED_EVENTS,
            MAX_INFLIGHT_REQUESTS,
            capabilities,
            expected_secret,
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
        expected_secret: Option<TransportSecret>,
    ) -> Self {
        let (conn, _) = watch::channel(None);
        let (init, _) = watch::channel(None);
        let (activity, _) = watch::channel(0);
        Self {
            inner: Arc::new(RunInner {
                protocol_version,
                expected_secret,
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
                init,
                activity,
                steering: Mutex::new(VecDeque::new()),
            }),
        }
    }

    /// Subscribe to authenticated host traffic and sandbox run activity.
    #[must_use]
    pub fn activity(&self) -> watch::Receiver<u64> {
        self.inner.activity.subscribe()
    }

    fn mark_activity(&self) {
        self.inner
            .activity
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    /// Wait for the host to deliver the run init — the task, policy snapshot,
    /// and (for a detached run) the scoped token. Delivered after the attach
    /// handshake, once the handle has committed on the host; the agent loop
    /// must not start before it arrives.
    pub async fn init(&self) -> crate::init::RunInit {
        let mut watcher = self.inner.init.subscribe();
        loop {
            if let Some(init) = watcher.borrow_and_update().clone() {
                return init;
            }
            if watcher.changed().await.is_err() {
                // The sender lives inside this run, so this is unreachable
                // while the run exists; park rather than fabricate an init.
                std::future::pending::<()>().await;
            }
        }
    }

    /// Record the host-delivered init. First delivery wins: a redelivery on
    /// reattach — or from a superseded connection still draining — is ignored.
    fn deliver_init(&self, init: crate::init::RunInit) {
        self.inner.init.send_if_modified(|slot| {
            if slot.is_some() {
                return false;
            }
            *slot = Some(init);
            true
        });
    }

    /// Take every host [steering](crate::steer) instruction that has arrived and
    /// not yet been consumed, oldest first, leaving the queue empty.
    ///
    /// The agent loop calls this at a step boundary and folds what it gets into
    /// the next model step, so an instruction that arrives mid-step lands on the
    /// step after it. Steering is attached-only: nothing is queued while the host
    /// is away, and a run nobody steered returns an empty vector.
    #[must_use]
    pub fn take_steering(&self) -> Vec<String> {
        self.inner
            .steering
            .lock()
            .expect("steering lock")
            .drain(..)
            .collect()
    }

    /// Queue one host steering instruction for the agent loop's next step.
    ///
    /// Bounded at [`MAX_PENDING_STEERS`](crate::steer::MAX_PENDING_STEERS): a host
    /// that steers faster than the loop steps drops its *oldest* pending
    /// instruction rather than growing sandbox memory, because the newest
    /// guidance is the guidance the user meant.
    fn deliver_steer(&self, text: String) {
        let mut steering = self.inner.steering.lock().expect("steering lock");
        while steering.len() >= crate::steer::MAX_PENDING_STEERS {
            steering.pop_front();
        }
        steering.push_back(text);
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

    /// Emit the run's terminal loop-end failure, returning its sequence.
    ///
    /// A conforming sandbox emits exactly one terminal event — this or
    /// [`emit_result`](Self::emit_result) — when its loop ends, so the host can
    /// terminalize and tear down instead of waiting on a connection the
    /// supervisor keeps serving after the agent is done.
    ///
    /// # Errors
    /// [`EmitError::TooLarge`] past the per-event bound, or [`EmitError::Overflow`]
    /// when the un-acknowledged buffer is full.
    pub async fn emit_failed(&self, text: impl Into<String>) -> Result<Sequence, EmitError> {
        self.emit(EventPayload::Failed(text.into())).await
    }

    async fn emit(&self, payload: EventPayload) -> Result<Sequence, EmitError> {
        let event = self.buffer_event(payload)?;
        self.mark_activity();
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
        self.mark_activity();

        match rx.await {
            Ok(response) => {
                let _ = conn
                    .control
                    .send(WireFrame::Control(ControlFrame::Acknowledge {
                        operation_id,
                    }))
                    .await;
                ReverseOutcome::Settled(response)
            }
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

    /// Whether `closed` belongs to the connection currently installed as the
    /// run's live peer.
    ///
    /// A serve loop keeps reading until its own socket drops, which can be after
    /// a reconnect has already installed a newer connection. Frames that mutate
    /// run-scoped state are gated on this so a superseded peer cannot speak for
    /// the run.
    fn is_live(&self, closed: &Arc<AtomicBool>) -> bool {
        matches!(&*self.inner.conn.borrow(), Some(current) if Arc::ptr_eq(&current.closed, closed))
    }

    /// Advance the un-acknowledged buffer to the host's committed cursor,
    /// pruning the events that cursor commits.
    ///
    /// Acknowledgement is the signal that the host has committed those events,
    /// so entries at or below the cursor are dropped here rather than held for
    /// the life of the run. The acknowledged cursor — not the emitted head — is
    /// the safe watermark: a reconnecting host resumes from its own committed
    /// position, which attach takes as an acknowledgement, so it can never ask
    /// to replay anything at or below what has been acknowledged.
    fn acknowledge(&self, cursor: EventCursor) {
        let mut buffer = self.inner.events.lock().expect("event buffer lock");
        buffer.acked_through = buffer.acked_through.max(cursor.get());
        // Events are buffered in ascending sequence order, so the acknowledged
        // prefix is contiguous.
        let committed = buffer
            .events
            .partition_point(|event| event.sequence.get() <= buffer.acked_through);
        buffer.events.drain(..committed);
        let unacked = (buffer.next_seq - 1).saturating_sub(buffer.acked_through);
        if unacked < buffer.buffer_cap as u64 {
            buffer.overflowed = false;
        }
    }
}

/// Serve one host connection against `run` until the host closes it.
///
/// Reads the [`AttachRequest`](crate::protocol::AttachRequest), answers the
/// handshake with the canonical [`handshake`] function (a version skew or a
/// failed transport-secret authentication yields an on-wire refusal and the
/// connection is neither installed nor served), and on acceptance runs the
/// connection: replays buffered events past the host's resume cursor,
/// carries the agent's reverse requests and events out, and correlates the host's
/// responses back. Returns when the connection drops; the `run` survives for the
/// host to reattach to.
///
/// The attach frame is read under [`ATTACH_TIMEOUT`]. A peer that connects and
/// then sends nothing, or dribbles bytes without ever completing the frame,
/// holds a serving task and a descriptor for as long as it likes otherwise —
/// [`read_frame`] bounds how *much* it will buffer, not how long it will wait.
/// The bound applies to the whole frame, not to each read, so a slow drip is
/// dropped just as an idle peer is.
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

    // The first frame must be the attach handshake, and must arrive promptly.
    let opening = tokio::time::timeout(ATTACH_TIMEOUT, read_frame(&mut reader)).await;
    let attach = match opening {
        Ok(frame) => match frame? {
            WireFrame::Attach(attach) => attach,
            // A peer that opens with anything else is not speaking the protocol.
            _ => return Ok(()),
        },
        // A peer that never finished its attach is dropped; nothing was
        // installed, so the run is untouched and stands for a real host.
        Err(_elapsed) => return Ok(()),
    };

    let response = handshake(
        &attach,
        run.inner.protocol_version,
        run.inner.expected_secret.as_ref(),
        run.inner.granted.clone(),
        run.latest_sequence(),
    );
    write_frame(&mut write_half, &WireFrame::Handshake(response.clone())).await?;
    let _accepted: AttachAccepted = match response {
        HandshakeResponse::Accepted(accepted) => accepted,
        // Refused — a version skew or a failed authentication. The connection is
        // left unusable and is NOT installed as the run's live peer: the code
        // below that publishes this connection via `send_replace` is never
        // reached, so an unauthenticated dial cannot hijack the channel from the
        // authenticated one. The run stands for a legitimate host to attach.
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

    // Spawn the writer BEFORE pushing anything onto the outbound queues. The
    // resume replay below can enqueue far more events than the request-lane queue
    // depth (up to MAX_BUFFERED_EVENTS), and if the writer that drains `data_rx`
    // is not already running, the replay loop wedges on a full channel and the
    // connection never starts serving. With the writer live, `data_rx` drains as
    // the replay fills it.
    let writer = tokio::spawn(write_prioritized(write_half, control_rx, data_rx));

    // Publish this connection as the run's live connection, then replay buffered
    // events strictly newer than the host's resume cursor. `send_replace`, not
    // `send`: the run holds no long-lived receiver, so `send` would be rejected
    // for want of one and leave the slot empty — `send_replace` updates the value
    // unconditionally, and a `call` that subscribes afterward still sees it.
    run.inner.conn.send_replace(Some(conn.clone()));
    run.mark_activity();
    // The resume cursor is this host's own committed position, so take it as an
    // acknowledgement. It is what makes ignoring a superseded peer's acks below
    // lossless: the incoming host restates its commitment on attach rather than
    // the run depending on an ack that may still be in flight on the old socket.
    run.acknowledge(attach.resume_from);
    for event in run.events_since(attach.resume_from) {
        if conn.data.send(WireFrame::Event(event)).await.is_err() {
            break;
        }
    }

    // Serve inbound frames: correlate reverse responses, answer pings, and take
    // event acknowledgements.
    loop {
        let frame = match read_frame(&mut reader).await {
            Ok(frame) => frame,
            Err(_) => break,
        };
        match frame {
            WireFrame::Request(RequestFrame::Response(envelope)) => {
                run.mark_activity();
                if let Some(tx) = pending
                    .lock()
                    .expect("pending lock")
                    .remove(&envelope.request_id)
                {
                    let _ = tx.send(envelope.response);
                }
            }
            WireFrame::Control(ControlFrame::Ping { nonce }) => {
                run.mark_activity();
                let _ = conn
                    .control
                    .send(WireFrame::Control(ControlFrame::Pong { nonce }))
                    .await;
            }
            WireFrame::Control(ControlFrame::Keepalive) => run.mark_activity(),
            // An acknowledgement is run-scoped state, so only the connection that
            // is currently installed may advance it. A superseded peer still
            // draining its socket after a reconnect speaks for a delivery the
            // live host has not made — and the live host's own commitment is
            // already taken from its resume cursor at attach.
            WireFrame::EventAck { cursor } => {
                if run.is_live(&closed) {
                    run.mark_activity();
                    run.acknowledge(cursor);
                }
            }
            // The run init: task, policy, and token, delivered only over an
            // authenticated connection (an unauthenticated dial never reaches
            // this loop). First delivery wins.
            WireFrame::Init(init) => {
                run.mark_activity();
                run.deliver_init(init);
            }
            // Mid-run steering, likewise only over an authenticated connection,
            // and only from the connection currently installed as the run's live
            // peer: a superseded socket must not inject guidance for a host that
            // has already been replaced. An over-bound instruction is dropped
            // rather than truncated — a conforming host checks the bound before
            // it writes, so this is a non-conforming peer, and half an
            // instruction is worse guidance than none.
            WireFrame::Steer(message) => {
                if run.is_live(&closed) && message.within_bounds() {
                    run.mark_activity();
                    run.deliver_steer(message.text);
                }
            }
            // The sandbox originates cancels and requests; it never receives
            // them. Pong is liveness only. Ignore rather than trust peer input.
            WireFrame::Control(
                ControlFrame::Pong { .. }
                | ControlFrame::Cancel { .. }
                | ControlFrame::Acknowledge { .. },
            )
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

#[cfg(test)]
mod tests {
    use tokio::io::{split, DuplexStream, WriteHalf};

    use super::{
        read_frame, write_frame, ControlFrame, EventCursor, SandboxRun, Sequence, TransportSecret,
        WireFrame, ATTACH_TIMEOUT, PROTOCOL_VERSION,
    };
    use crate::{
        ids::RunId,
        protocol::{AttachRequest, HandshakeResponse},
        steer::{SteerMessage, MAX_PENDING_STEERS, MAX_STEER_BYTES},
    };

    type HostReader = tokio::io::BufReader<tokio::io::ReadHalf<DuplexStream>>;

    fn run_with(secret: &TransportSecret) -> SandboxRun {
        SandboxRun::new([], Some(secret.clone()))
    }

    /// Ping over `writer` and await the matching pong on `reader`.
    ///
    /// A serve loop reads its first inbound frame only after it has installed
    /// its connection as the run's live peer, so a pong proves both that the
    /// installation has happened and that every frame written ahead of the ping
    /// has already been processed. `attach` returning proves neither: the
    /// handshake response is written before the connection is installed, so a
    /// test that orders two connections by their handshakes is ordering them by
    /// nothing at all.
    async fn ping_pong(reader: &mut HostReader, writer: &mut WriteHalf<DuplexStream>, nonce: u64) {
        write_frame(writer, &WireFrame::Control(ControlFrame::Ping { nonce }))
            .await
            .expect("send ping");
        let pong = tokio::time::timeout(std::time::Duration::from_secs(5), read_frame(reader))
            .await
            .expect("the connection is served")
            .expect("pong");
        assert!(
            matches!(pong, WireFrame::Control(ControlFrame::Pong { nonce: answered }) if answered == nonce),
            "the connection answered {pong:?} rather than the pong for {nonce}"
        );
    }

    /// Attach over `host_side`, returning the halves so the caller can keep
    /// speaking on the connection afterwards.
    async fn attach(
        host_side: DuplexStream,
        secret: &TransportSecret,
        resume_from: EventCursor,
    ) -> (HostReader, WriteHalf<DuplexStream>) {
        let (read_half, mut write_half) = split(host_side);
        let mut reader = tokio::io::BufReader::new(read_half);
        let attach = WireFrame::Attach(AttachRequest {
            protocol_version: PROTOCOL_VERSION,
            run_id: RunId::new(),
            resume_from,
            transport_secret: secret.clone(),
        });
        write_frame(&mut write_half, &attach)
            .await
            .expect("send attach");
        let answered = read_frame(&mut reader).await.expect("handshake answered");
        assert!(matches!(
            answered,
            WireFrame::Handshake(HandshakeResponse::Accepted(_))
        ));
        (reader, write_half)
    }

    /// A peer that connects and then never completes its attach is dropped rather
    /// than holding a serving task forever, and nothing is installed on the run.
    ///
    /// Time is paused, so the deadline is exercised without the test waiting on
    /// it: the runtime advances the clock once every task is idle, which a serve
    /// loop that reads without a deadline never becomes.
    #[tokio::test(start_paused = true)]
    async fn a_peer_that_never_completes_its_attach_is_dropped() {
        let secret = TransportSecret::new("slowloris-test");
        let run = run_with(&secret);
        let (host_side, sandbox_side) = tokio::io::duplex(1024);
        let served = tokio::spawn(super::serve_connection(sandbox_side, run.clone()));

        // The peer holds the connection open and sends nothing at all.
        let outcome = tokio::time::timeout(ATTACH_TIMEOUT * 3, served)
            .await
            .expect("the silent peer is dropped at the attach deadline");
        assert!(outcome.expect("serve task").is_ok());
        assert!(
            run.current_conn().is_none(),
            "a peer that never attached must not be installed"
        );
        drop(host_side);
    }

    /// Steering is bounded on the way in and keeps the newest guidance: an
    /// over-bound instruction is refused rather than truncated, and a host that
    /// steers faster than the loop steps loses its oldest pending instruction,
    /// never sandbox memory.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn steering_is_bounded_and_keeps_the_newest_instructions() {
        let secret = TransportSecret::new("steer-bounds-test");
        let run = run_with(&secret);
        let (host_side, sandbox_side) = tokio::io::duplex(128 * 1024);
        tokio::spawn(super::serve_connection(sandbox_side, run.clone()));
        let (mut reader, mut writer) = attach(host_side, &secret, EventCursor::START).await;

        let over_bound = "x".repeat(MAX_STEER_BYTES + 1);
        write_frame(
            &mut writer,
            &WireFrame::Steer(SteerMessage::new(over_bound.clone())),
        )
        .await
        .expect("send an over-bound steer");
        for index in 0..MAX_PENDING_STEERS + 2 {
            write_frame(
                &mut writer,
                &WireFrame::Steer(SteerMessage::new(format!("step {index}"))),
            )
            .await
            .expect("send a steer");
        }
        // A ping behind them, answered on the same connection, proves every
        // steer above was processed before the assertions read the queue.
        ping_pong(&mut reader, &mut writer, 7).await;

        let taken = run.take_steering();
        assert_eq!(
            taken.len(),
            MAX_PENDING_STEERS,
            "the pending queue is capped, got {taken:?}"
        );
        assert!(
            !taken.contains(&over_bound),
            "an over-bound instruction must be refused, not carried"
        );
        assert_eq!(
            taken.first().map(String::as_str),
            Some("step 2"),
            "the oldest pending instructions are what a full queue drops"
        );
        assert!(
            run.take_steering().is_empty(),
            "taking steering leaves the queue empty"
        );
    }

    /// A superseded peer — still draining its socket after the host reconnected —
    /// cannot advance the run's acknowledged cursor. Its ack speaks for a
    /// delivery the live host has not made, and taking it would let the sandbox
    /// discard backpressure the live host is entitled to.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_superseded_peer_cannot_advance_the_acknowledged_cursor() {
        let secret = TransportSecret::new("stale-ack-test");
        let run = run_with(&secret);

        let (first_host, first_sandbox) = tokio::io::duplex(4096);
        tokio::spawn(super::serve_connection(first_sandbox, run.clone()));
        let (mut first_reader, mut first_writer) =
            attach(first_host, &secret, EventCursor::START).await;
        // Supersession is ordered by installation, not by handshake, so the
        // first connection must be *installed* before the second attaches —
        // otherwise the two race and the first can install last, making it the
        // live peer and its ack a legitimate one.
        ping_pong(&mut first_reader, &mut first_writer, 0).await;

        // The host reconnects; the second connection becomes the live one while
        // the first is still readable. Its own ping-pong proves its serve loop —
        // which runs only after the connection is installed as live — is up, so
        // the first connection is now definitively superseded.
        let (second_host, second_sandbox) = tokio::io::duplex(4096);
        tokio::spawn(super::serve_connection(second_sandbox, run.clone()));
        let (mut second_reader, mut second_writer) =
            attach(second_host, &secret, EventCursor::START).await;
        ping_pong(&mut second_reader, &mut second_writer, 1).await;

        // The stale peer acknowledges. A ping behind it, answered on the same
        // connection, proves the ack was processed before we assert.
        write_frame(
            &mut first_writer,
            &WireFrame::EventAck {
                cursor: EventCursor::committed(Sequence::new(5)),
            },
        )
        .await
        .expect("send stale ack");
        ping_pong(&mut first_reader, &mut first_writer, 2).await;

        let acked = run
            .inner
            .events
            .lock()
            .expect("event buffer lock")
            .acked_through;
        assert_eq!(
            acked,
            EventCursor::START.get(),
            "a superseded peer advanced the acknowledged cursor"
        );
    }

    /// An acknowledgement prunes the events the host has committed, and the
    /// prune watermark is the acknowledged cursor — not the emitted head — so
    /// a host that reconnects from its committed position still replays every
    /// event it has not acknowledged.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn acknowledgement_prunes_committed_events_but_keeps_the_replay_tail() {
        let secret = TransportSecret::new("prune-test");
        let run = run_with(&secret);

        // Emitted unattached, so all three sit in the buffer for replay.
        for _ in 0..3 {
            run.emit_progress("line").await.expect("emit");
        }

        let (first_host, first_sandbox) = tokio::io::duplex(4096);
        tokio::spawn(super::serve_connection(first_sandbox, run.clone()));
        let (mut first_reader, mut first_writer) =
            attach(first_host, &secret, EventCursor::START).await;

        // The resume replay delivers all three; the host commits the first two.
        for expected in 1..=3 {
            let frame = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                read_frame(&mut first_reader),
            )
            .await
            .expect("the resume replay is served")
            .expect("replayed event");
            assert!(
                matches!(frame, WireFrame::Event(ref event) if event.sequence.get() == expected),
                "the resume replay delivered events out of order"
            );
        }
        write_frame(
            &mut first_writer,
            &WireFrame::EventAck {
                cursor: EventCursor::committed(Sequence::new(2)),
            },
        )
        .await
        .expect("send ack");
        // A ping answered behind the ack proves the ack was processed.
        ping_pong(&mut first_reader, &mut first_writer, 0).await;

        {
            let buffer = run.inner.events.lock().expect("event buffer lock");
            assert!(
                buffer.events.iter().all(|event| event.sequence.get() > 2),
                "acknowledged events must be pruned from the buffer"
            );
            assert_eq!(
                buffer.events.len(),
                1,
                "the un-acknowledged tail stays buffered"
            );
        }

        // The host reconnects from its committed position; the tail it never
        // acknowledged still replays.
        drop(first_reader);
        drop(first_writer);
        let (second_host, second_sandbox) = tokio::io::duplex(4096);
        tokio::spawn(super::serve_connection(second_sandbox, run.clone()));
        let (mut second_reader, _second_writer) = attach(
            second_host,
            &secret,
            EventCursor::committed(Sequence::new(2)),
        )
        .await;
        let replayed = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_frame(&mut second_reader),
        )
        .await
        .expect("the replay after resume is served")
        .expect("replayed event");
        assert!(
            matches!(replayed, WireFrame::Event(ref event) if event.sequence.get() == 3),
            "a reconnect must still replay the un-acknowledged tail"
        );
    }
}
