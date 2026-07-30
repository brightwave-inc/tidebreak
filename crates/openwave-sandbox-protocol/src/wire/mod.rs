//! The concrete byte transport: newline-delimited JSON framing plus a host-side
//! client and a sandbox-side server that carry the protocol's existing frame
//! types over a real socket.
//!
//! The crate's core (see the [crate docs](crate)) pins the wire *types and their
//! semantics* against an in-process [reference backend](crate::reference); it
//! deliberately left the *byte transport* — how a frame is delimited on a socket
//! and how the request and control lanes share one connection — to a concrete
//! backend. This module is that transport, and it is what the local container
//! backend and its host connect over.
//!
//! # Framing: newline-delimited JSON
//!
//! One frame is one line of compact JSON terminated by `\n`. This mirrors the
//! shipped `openwave-host-broker` sidecar's hand-rolled newline protocol and the
//! reverse-RPC spike, and it was chosen over a length prefix for three reasons:
//! it is trivially debuggable (a frame is a readable JSON line), `read_until`
//! over a [`BufReader`](tokio::io::BufReader) reassembles a frame that spans
//! several reads and retains any bytes past the newline for the next frame, and
//! it needs no codec dependency. `serde_json` emits compact single-line JSON, so
//! the delimiter is unambiguous; a frame is bounded by [`MAX_FRAME_BYTES`] so a
//! peer cannot force unbounded buffering with one enormous line.
//!
//! # Lanes over one connection
//!
//! Every unit on the wire is a [`WireFrame`], an adjacently-tagged envelope that
//! names which lane a frame belongs to and carries one of the protocol's
//! existing types unchanged — the reverse-RPC [`RequestFrame`], the reserved
//! [`ControlFrame`], a [`SandboxEvent`], or the [handshake](AttachRequest) pair.
//! The envelope is the only new wire type; the payloads are the same ones the
//! [wire-format spec](../tests/wire_format.rs) pins.
//!
//! The [reserved control lane](crate::reverse::ControlFrame) is realized as a
//! separate outbound queue that the writer drains with priority over the request
//! lane, so a [`ControlFrame::Cancel`] is never stuck behind a saturated request
//! backlog. On the sandbox side the request lane also applies backpressure at a
//! bounded in-flight semaphore before a request frame is even enqueued, so a
//! saturated request lane blocks new requests rather than the writer — and a
//! cancel, which acquires no permit and rides the control queue, still lands.

pub mod host;
pub mod sandbox;

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    sync::mpsc,
};

use crate::{
    events::SandboxEvent,
    ids::EventCursor,
    protocol::{AttachRequest, HandshakeResponse},
    reverse::{ControlFrame, RequestFrame},
};

pub use host::{HostConnection, WireClient};
pub use sandbox::{serve_connection, ReverseOutcome, SandboxRun};

/// How long either side waits for the handshake frame it is owed before it drops
/// the connection.
///
/// [`read_frame`] bounds how *much* a peer may make this side buffer, not how
/// long it may take: without a deadline a peer that connects and then says
/// nothing — or dribbles a frame a byte at a time — holds a serving task and a
/// descriptor indefinitely. The bound covers the whole frame rather than each
/// read, so a slow drip is dropped just as an idle peer is. It is generous
/// against the work being waited on (one write on a freshly established
/// connection) and so is not a latency budget for anything else.
pub const ATTACH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Largest single frame a conforming transport reads or writes, re-exported from
/// the protocol's [`MAX_FRAME_BYTES`](crate::protocol::MAX_FRAME_BYTES) so the
/// byte transport and the semantic bound stay one number.
pub const MAX_FRAME_BYTES: usize = crate::protocol::MAX_FRAME_BYTES;

/// One framed unit on the wire.
///
/// The variants name the four lanes the connection multiplexes plus the two
/// handshake frames that open it. Each carries one of the protocol's existing
/// types verbatim; this envelope does not fork their shapes, it only tags which
/// lane the payload belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "lane",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum WireFrame {
    /// Host -> sandbox: the attach handshake request. Always the first frame the
    /// host sends on a fresh connection.
    Attach(AttachRequest),
    /// Sandbox -> host: the handshake answer. Always the first frame the sandbox
    /// sends, before any event or reverse response.
    Handshake(HandshakeResponse),
    /// The reverse-RPC request lane: a request sandbox -> host, or the host's
    /// correlated response host -> sandbox.
    Request(RequestFrame),
    /// The reserved control lane: cancel (sandbox -> host) and liveness (either
    /// direction). Never subject to request backpressure.
    Control(ControlFrame),
    /// Sandbox -> host: one event on the resumable, monotonically sequenced
    /// event stream.
    Event(SandboxEvent),
    /// Host -> sandbox: acknowledge the event stream through a committed cursor,
    /// so the sandbox may advance its un-acknowledged buffer.
    EventAck {
        /// The host's committed cursor.
        cursor: EventCursor,
    },
}

/// Why a framed read or write stopped.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The peer closed the connection (EOF at a frame boundary).
    #[error("the connection closed")]
    Closed,
    /// A frame exceeded [`MAX_FRAME_BYTES`]; the peer is refused rather than
    /// buffered without bound.
    #[error("a frame exceeded {MAX_FRAME_BYTES} bytes")]
    TooLarge,
    /// A frame was not valid protocol JSON.
    #[error("a frame was not valid protocol JSON")]
    Malformed,
    /// The underlying transport failed.
    #[error("transport i/o failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Read the next [`WireFrame`] from a buffered reader, or report why none came.
///
/// The read is bounded to [`MAX_FRAME_BYTES`] `+ 1` bytes by an
/// [`AsyncReadExt::take`] before it looks for the delimiter, so the *buffering
/// itself* is capped, not just checked after the fact: a peer that streams bytes
/// without ever sending `\n` cannot grow the frame buffer without limit and OOM
/// this side. Within that budget, `read_until` reassembles a frame that spans
/// several underlying reads and the [`BufReader`] retains any bytes past the
/// newline for the next call — the framing behavior a real socket forces.
///
/// # Errors
/// [`FrameError::Closed`] at EOF on a frame boundary, [`FrameError::TooLarge`]
/// once the byte budget is exhausted before a delimiter, [`FrameError::Malformed`]
/// on invalid JSON, or [`FrameError::Io`] on a transport failure.
pub async fn read_frame<R>(reader: &mut BufReader<R>) -> Result<WireFrame, FrameError>
where
    R: AsyncRead + Unpin,
{
    // One byte past the cap: a maximal frame (MAX_FRAME_BYTES of content plus its
    // delimiter) still fits, and anything larger trips the budget before a
    // delimiter is seen.
    let budget = MAX_FRAME_BYTES as u64 + 1;
    let mut line = Vec::new();
    let read = (&mut *reader)
        .take(budget)
        .read_until(b'\n', &mut line)
        .await?;
    if read == 0 {
        return Err(FrameError::Closed);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    } else {
        // No delimiter within the budget. Either the peer exhausted the byte cap
        // without a newline (a flood, or an over-bound frame) or the stream ended
        // mid-frame; the buffer never exceeded the cap either way.
        return if line.len() as u64 >= budget {
            Err(FrameError::TooLarge)
        } else {
            Err(FrameError::Closed)
        };
    }
    if line.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    serde_json::from_slice(&line).map_err(|_| FrameError::Malformed)
}

/// Write one [`WireFrame`] followed by its newline delimiter and flush it.
///
/// # Errors
/// [`FrameError::TooLarge`] if the encoded frame exceeds [`MAX_FRAME_BYTES`],
/// [`FrameError::Malformed`] if it cannot serialize, or [`FrameError::Io`] on a
/// transport failure.
pub async fn write_frame<W>(writer: &mut W, frame: &WireFrame) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
{
    let mut encoded = serde_json::to_vec(frame).map_err(|_| FrameError::Malformed)?;
    if encoded.len() >= MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    writer.flush().await?;
    Ok(())
}

/// Drain two outbound queues onto one write half, control frames first.
///
/// Both the host and sandbox sides split their outbound frames into a reserved
/// control queue and a request/data queue; this `biased` select drains any
/// pending control frame before the data lane, realizing the reserved control
/// lane at the writer so a cancel or a pong is never stuck behind a data
/// backlog. Returns when both queues close or a write fails.
pub(crate) async fn write_prioritized<W>(
    mut write_half: W,
    mut control_rx: mpsc::Receiver<WireFrame>,
    mut data_rx: mpsc::Receiver<WireFrame>,
) where
    W: AsyncWrite + Unpin + Send + 'static,
{
    loop {
        // `Some(..) =` disables a closed queue rather than treating its `None` as
        // a frame, so a closed control lane does not starve a still-open data
        // lane; `else` fires only once both queues have closed.
        let frame = tokio::select! {
            biased;
            Some(frame) = control_rx.recv() => frame,
            Some(frame) = data_rx.recv() => frame,
            else => break,
        };
        if write_frame(&mut write_half, &frame).await.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ids::{OperationId, RequestId, RunId},
        protocol::PROTOCOL_VERSION,
        reverse::{ModelInferenceParams, ReverseEnvelope, ReverseRequest},
    };

    #[test]
    fn wire_frame_tags_its_lane() {
        let frame = WireFrame::Control(ControlFrame::Cancel {
            operation_id: OperationId::new(),
        });
        let encoded = serde_json::to_value(&frame).unwrap();
        assert_eq!(encoded["lane"], "control");
        assert_eq!(encoded["body"]["control"], "cancel");
        assert_eq!(serde_json::from_value::<WireFrame>(encoded).unwrap(), frame);
    }

    #[tokio::test]
    async fn frames_survive_being_split_and_batched_across_reads() {
        // Encode two frames back to back, then feed them through a duplex a few
        // bytes at a time so a frame spans reads and two frames share a read —
        // the two properties newline framing must handle.
        let attach = WireFrame::Attach(AttachRequest {
            protocol_version: PROTOCOL_VERSION,
            run_id: RunId::new(),
            resume_from: EventCursor::START,
            transport_secret: crate::provisioning::TransportSecret::new("frame-split-test"),
        });
        let request = WireFrame::Request(RequestFrame::Request(ReverseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(),
            operation_id: OperationId::new(),
            request: ReverseRequest::ModelInference(ModelInferenceParams {
                prompt: "hello".to_owned(),
            }),
        }));

        let mut bytes = serde_json::to_vec(&attach).unwrap();
        bytes.push(b'\n');
        bytes.extend(serde_json::to_vec(&request).unwrap());
        bytes.push(b'\n');

        let (mut writer, reader) = tokio::io::duplex(8);
        let feed = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt as _;
            for chunk in bytes.chunks(3) {
                writer.write_all(chunk).await.unwrap();
                writer.flush().await.unwrap();
            }
            // Drop closes the write half so the reader eventually sees EOF.
        });

        let mut reader = BufReader::new(reader);
        assert_eq!(read_frame(&mut reader).await.unwrap(), attach);
        assert_eq!(read_frame(&mut reader).await.unwrap(), request);
        assert!(matches!(
            read_frame(&mut reader).await,
            Err(FrameError::Closed)
        ));
        feed.await.unwrap();
    }

    #[tokio::test]
    async fn a_no_newline_flood_is_refused_as_too_large_without_unbounded_buffering() {
        // An endless stream of non-delimiter bytes must be refused once the byte
        // budget is spent, not buffered without bound. `repeat` never yields a
        // newline and never ends, so a correct `read_frame` returns after reading
        // the bounded budget; a regression that buffers unboundedly would run the
        // machine out of memory instead of returning here.
        let flood = tokio::io::repeat(b'x');
        let mut reader = BufReader::new(flood);
        match read_frame(&mut reader).await {
            Err(FrameError::TooLarge) => {}
            other => panic!("a no-newline flood must be refused as TooLarge, got {other:?}"),
        }
    }
}
