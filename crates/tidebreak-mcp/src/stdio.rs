//! The stdio transport: newline-delimited JSON-RPC over stdin/stdout.
//!
//! This is the transport MCP clients launch a server with — one JSON message per
//! line in, one per line out. [`serve_stdio`] wires an [`McpServer`] to the process
//! stdin/stdout; [`serve`] does the same over any async reader/writer, which is
//! what makes the loop testable without touching the real console.

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::client::MAX_JSON_RPC_FRAME_BYTES;
use crate::protocol::{error_code, Request, Response, RpcError};
use crate::server::McpServer;
use serde_json::Value;

/// Serve `server` over the process stdin/stdout until stdin reaches EOF.
pub async fn serve_stdio(server: McpServer) -> std::io::Result<()> {
    serve(
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
        server,
    )
    .await
}

/// Serve `server` over an arbitrary reader/writer.
///
/// Each non-blank input line is parsed as a JSON-RPC request and dispatched;
/// responses (and parse errors) are written as one JSON line each. Notifications
/// and blank lines produce no output. Returns when the reader reaches EOF.
///
/// A frame longer than [`MAX_JSON_RPC_FRAME_BYTES`] — the same bound this crate's
/// client applies to the identical framing — is refused rather than buffered, so
/// a hostile or broken client cannot drive unbounded memory growth. The oversized
/// frame is discarded up to its newline so the stream stays in sync, and the
/// client gets a protocol error instead of a silently truncated request.
pub async fn serve<R, W>(mut reader: R, mut writer: W, server: McpServer) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        let Some(frame) = read_bounded_frame(&mut reader).await? else {
            return Ok(());
        };
        let response = match frame {
            Frame::Line(line) => {
                if line.trim().is_empty() {
                    continue;
                }
                match parse_request(&line) {
                    Ok(request) => server.handle(request).await,
                    Err(error) => Some(Response::error(Value::Null, error)),
                }
            }
            Frame::TooLarge => Some(Response::error(
                Value::Null,
                RpcError::new(
                    error_code::INVALID_REQUEST,
                    format!("request exceeds the {MAX_JSON_RPC_FRAME_BYTES}-byte frame limit"),
                ),
            )),
        };
        if let Some(response) = response {
            write_line(&mut writer, &response).await?;
        }
    }
}

/// One input frame: a complete line, or the marker that an oversized one was
/// discarded.
enum Frame {
    Line(String),
    TooLarge,
}

/// Read one newline-delimited frame, holding at most [`MAX_JSON_RPC_FRAME_BYTES`]
/// in memory. Returns `None` at EOF with nothing buffered.
async fn read_bounded_frame<R>(reader: &mut R) -> std::io::Result<Option<Frame>>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    let mut over_limit = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if over_limit {
                return Ok(Some(Frame::TooLarge));
            }
            return Ok((!line.is_empty()).then(|| Frame::Line(lossy_line(line))));
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        if over_limit || line.len().saturating_add(consumed) > MAX_JSON_RPC_FRAME_BYTES {
            // Keep draining to the newline so the next frame starts on a request
            // boundary, but stop growing the buffer.
            over_limit = true;
            line = Vec::new();
        } else {
            line.extend_from_slice(&available[..consumed]);
        }
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(Some(if over_limit {
                Frame::TooLarge
            } else {
                Frame::Line(lossy_line(line))
            }));
        }
    }
}

/// Decode a frame's bytes, trimming the delimiter. Invalid UTF-8 falls through to
/// the parser, which reports it as a JSON parse error.
fn lossy_line(bytes: Vec<u8>) -> String {
    let mut line = String::from_utf8_lossy(&bytes).into_owned();
    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }
    line
}

/// Parse one line into a [`Request`], distinguishing a syntax error (`-32700
/// Parse error`) from valid JSON that isn't a well-formed request (`-32600 Invalid
/// Request`, e.g. a missing `method`).
fn parse_request(line: &str) -> Result<Request, RpcError> {
    let value: Value = serde_json::from_str(line)
        .map_err(|e| RpcError::new(error_code::PARSE_ERROR, format!("invalid JSON: {e}")))?;
    serde_json::from_value(value)
        .map_err(|e| RpcError::new(error_code::INVALID_REQUEST, format!("invalid request: {e}")))
}

/// Write a response as one JSON line, then flush.
async fn write_line<W: AsyncWrite + Unpin>(
    out: &mut W,
    response: &Response,
) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(response).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    out.write_all(&bytes).await?;
    out.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    use tidebreak_core::{SessionId, ToolCtx, ToolRegistry};

    fn empty_server() -> McpServer {
        let ctx = ToolCtx::new_legacy_workspace(SessionId::new(), None, PathBuf::from("/tmp/ws"));
        McpServer::new(Arc::new(ToolRegistry::new()), ctx)
    }

    #[tokio::test]
    async fn serve_replies_to_requests_and_skips_notifications_and_blanks() {
        // Initialize, acknowledge the lifecycle notification, then list tools.
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
            "\n\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            "\n",
        );
        let mut output = Vec::new();
        serve(input.as_bytes(), &mut output, empty_server())
            .await
            .unwrap();

        // The two requests get replies; the blank and notification do not.
        let out = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        let response: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(response["id"], 2);
        assert!(response["result"]["tools"].is_array());
    }

    #[tokio::test]
    async fn serve_reports_a_parse_error_for_invalid_json() {
        let mut output = Vec::new();
        serve(&b"not json\n"[..], &mut output, empty_server())
            .await
            .unwrap();
        let response: serde_json::Value =
            serde_json::from_str(String::from_utf8(output).unwrap().trim()).unwrap();
        assert_eq!(response["error"]["code"], error_code::PARSE_ERROR);
        assert_eq!(response["id"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn serve_reports_invalid_request_for_valid_json_without_method() {
        // Valid JSON, but not a well-formed request (no `method`) => -32600, not
        // the -32700 reserved for JSON syntax errors.
        let mut output = Vec::new();
        serve(
            &br#"{"jsonrpc":"2.0","id":1}"#[..],
            &mut output,
            empty_server(),
        )
        .await
        .unwrap();
        let response: serde_json::Value =
            serde_json::from_str(String::from_utf8(output).unwrap().trim()).unwrap();
        assert_eq!(response["error"]["code"], error_code::INVALID_REQUEST);
    }

    #[tokio::test]
    async fn an_oversized_frame_is_refused_without_desynchronizing_the_stream() {
        let mut input = vec![b'x'; MAX_JSON_RPC_FRAME_BYTES + 1];
        input.push(b'\n');
        input.extend_from_slice(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
        );
        input.push(b'\n');

        let mut output = Vec::new();
        serve(&input[..], &mut output, empty_server())
            .await
            .unwrap();

        let out = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        let refused: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(refused["error"]["code"], error_code::INVALID_REQUEST);
        // The request after the oversized frame is still read as a request, so
        // the discarded bytes did not shift the framing.
        let accepted: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(accepted["id"], 1);
        assert!(accepted["result"]["protocolVersion"].is_string());
    }
}
