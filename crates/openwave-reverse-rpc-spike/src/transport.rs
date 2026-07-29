//! Newline-delimited JSON framing over an async byte duplex.
//!
//! The spike runs the host and the sandbox supervisor as two tasks joined by a
//! `tokio::io::duplex` pipe. That is the cheapest transport that still exercises
//! the two hard properties a real socket has: genuine framing (a frame can span
//! reads, and multiple frames can share one read) and genuine concurrency (both
//! ends make progress on the runtime at once). Dropping either half models a
//! dropped connection. The framing here mirrors the broker sidecar's
//! hand-rolled newline protocol rather than pulling in a codec dependency.

use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::protocol::Frame;

/// Largest single frame accepted or emitted, so a peer cannot force unbounded
/// buffering with one enormous line. Model prompts and completions in the spike
/// are tiny; this only needs to bound abuse.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Why a framed read stopped yielding frames.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("the connection closed")]
    Closed,
    #[error("a frame exceeded {MAX_FRAME_BYTES} bytes")]
    TooLarge,
    #[error("a frame was not valid JSON")]
    Malformed,
    #[error("transport i/o failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Read the next frame from a buffered reader, or signal why none came.
pub async fn read_frame<R>(reader: &mut BufReader<R>) -> Result<Frame, FrameError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    // `read_until` internally loops until the delimiter or EOF, so a frame that
    // spans several underlying reads is reassembled here, and `BufReader`
    // retains any bytes past the newline for the next frame — exactly the
    // framing behavior a real socket forces on us.
    let mut line = Vec::new();
    let read = reader.read_until(b'\n', &mut line).await?;
    if read == 0 {
        return Err(FrameError::Closed);
    }
    if line.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    serde_json::from_slice(&line).map_err(|_| FrameError::Malformed)
}

/// Write one frame followed by its newline delimiter and flush it.
pub async fn write_frame<W>(writer: &mut W, frame: &Frame) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
{
    let mut encoded = serde_json::to_vec(frame).map_err(|_| FrameError::Malformed)?;
    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    writer.flush().await?;
    Ok(())
}
