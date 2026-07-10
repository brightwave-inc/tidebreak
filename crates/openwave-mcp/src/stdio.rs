//! The stdio transport: newline-delimited JSON-RPC over stdin/stdout.
//!
//! This is the transport MCP clients launch a server with — one JSON message per
//! line in, one per line out. [`serve_stdio`] wires an [`McpServer`] to the process
//! stdin/stdout; [`serve`] does the same over any async reader/writer, which is
//! what makes the loop testable without touching the real console.

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

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
pub async fn serve<R, W>(reader: R, mut writer: W, server: McpServer) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut lines = reader.lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => server.handle(request).await,
            Err(err) => Some(Response::error(
                Value::Null,
                RpcError::new(error_code::PARSE_ERROR, format!("invalid JSON-RPC: {err}")),
            )),
        };
        if let Some(response) = response {
            write_line(&mut writer, &response).await?;
        }
    }
    Ok(())
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

    use openwave_core::{ChatId, ToolCtx, ToolRegistry};

    fn empty_server() -> McpServer {
        let ctx = ToolCtx {
            chat_id: ChatId::new(),
            workspace_dir: PathBuf::from("/tmp/ws"),
        };
        McpServer::new(Arc::new(ToolRegistry::new()), ctx)
    }

    #[tokio::test]
    async fn serve_replies_to_requests_and_skips_notifications_and_blanks() {
        // A request, then a blank line, then a notification (no id).
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            "\n\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
        );
        let mut output = Vec::new();
        serve(input.as_bytes(), &mut output, empty_server())
            .await
            .unwrap();

        // Exactly one response line (the request); the blank and notification
        // produced nothing.
        let out = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 1);
        let response: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(response["id"], 1);
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
}
