//! Streamable HTTP client transport for external MCP servers.
//!
//! Each JSON-RPC request is one authenticated `POST`. A server may answer with
//! a single `application/json` message or with a `text/event-stream` carrying
//! interim notifications before the final response. Responses to
//! client notifications are expected to be empty (HTTP 202).
//!
//! The bearer token lives only inside the prebuilt `Authorization` header value
//! and is never echoed into errors or logs.

use openwave_core::Result;
use serde_json::Value;

use crate::client::{mcp_error, mcp_message, MAX_JSON_RPC_FRAME_BYTES};
use crate::protocol::PROTOCOL_VERSION;

const SESSION_ID_HEADER: &str = "mcp-session-id";
const PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";
const MAX_SESSION_ID_BYTES: usize = 4 * 1024;

/// One Streamable HTTP connection to an external MCP server.
pub(crate) struct HttpWire {
    client: reqwest::Client,
    url: reqwest::Url,
    /// Prebuilt `Bearer …` header value; kept whole so the raw token never
    /// travels through formatting code that could end up in an error.
    authorization: Option<String>,
    /// Static headers the connection was configured with, already validated
    /// and stripped of every name this transport generates itself. Sent on
    /// every request, and — like the bearer — never logged or echoed into an
    /// error: a configured header may carry a credential even though the
    /// source that declared it is not treated as a secret store.
    configured: reqwest::header::HeaderMap,
    /// Server-assigned session, echoed on every request once issued.
    session_id: Option<String>,
}

/// The header names this transport generates for itself. A configured header
/// naming one is dropped rather than merged: the client-generated value wins
/// on conflict, and `host`/`content-*` belong to the request the transport
/// builds, not to configuration.
const RESERVED_HEADERS: [&str; 7] = [
    "accept",
    "authorization",
    "content-length",
    "content-type",
    "host",
    PROTOCOL_VERSION_HEADER,
    SESSION_ID_HEADER,
];

/// Turn configured `name: value` pairs into a header map, dropping the names
/// the transport generates itself.
///
/// Errors name only the offending *header name* — never its value.
pub(crate) fn static_headers(
    headers: &std::collections::BTreeMap<String, String>,
) -> Result<reqwest::header::HeaderMap> {
    let mut map = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
        let lowercase = name.to_ascii_lowercase();
        if RESERVED_HEADERS.contains(&lowercase.as_str()) {
            continue;
        }
        let name = reqwest::header::HeaderName::from_bytes(lowercase.as_bytes())
            .map_err(|_| mcp_message("configured MCP header name is not a valid HTTP field"))?;
        let value = reqwest::header::HeaderValue::from_str(value)
            .map_err(|_| mcp_message("configured MCP header value is not a valid field value"))?;
        map.insert(name, value);
    }
    Ok(map)
}

/// Validate a candidate MCP server URL without connecting.
///
/// Exposed so configuration layers can reject an invalid URL before accepting
/// a definition, with the same rules the transport applies.
pub fn validate_http_url(url: &str) -> Result<()> {
    let parsed =
        reqwest::Url::parse(url).map_err(|_| mcp_message("external server URL is invalid"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(mcp_message("external server URL must use http or https"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(mcp_message(
            "external server URL must not embed credentials",
        ));
    }
    if parsed.host_str().is_none() {
        return Err(mcp_message("external server URL must name a host"));
    }
    Ok(())
}

impl HttpWire {
    pub(crate) fn with_headers(
        url: &str,
        bearer_token: Option<&str>,
        configured: reqwest::header::HeaderMap,
    ) -> Result<Self> {
        validate_http_url(url)?;
        let url =
            reqwest::Url::parse(url).map_err(|_| mcp_message("external server URL is invalid"))?;
        if bearer_token.is_some_and(|token| {
            token.is_empty() || token.bytes().any(|byte| byte.is_ascii_control())
        }) {
            return Err(mcp_message(
                "external server bearer token must be a non-empty header-safe value",
            ));
        }
        Ok(Self {
            client: reqwest::Client::builder()
                // A credentialed client must not follow redirects: reqwest
                // strips Authorization cross-host, but a same-host https→http
                // downgrade would resend the bearer in cleartext. This is also
                // what keeps configured static headers — which reqwest knows
                // nothing about and would carry across a hop — from ever
                // reaching a different origin.
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|error| mcp_error("could not build HTTP client", error))?,
            url,
            authorization: bearer_token.map(|token| format!("Bearer {token}")),
            configured,
            session_id: None,
        })
    }

    /// Send one request and read messages until the matching response arrives.
    ///
    /// Interim server notifications set `tools_list_changed` exactly like the
    /// stdio transport. A server request embedded in the stream is answered
    /// with a follow-up POST because the response channel is one-way.
    pub(crate) async fn request(
        &mut self,
        expected_id: u64,
        message: &Value,
        tools_list_changed: &mut bool,
        bearer: Option<&str>,
    ) -> Result<Value> {
        let response = self.post(message, bearer).await?;
        let status = response.status();
        if status == reqwest::StatusCode::ACCEPTED {
            return Err(mcp_message(
                "external server accepted a request without replying",
            ));
        }
        check_status(status)?;
        // Only a successful exchange may (re)bind the session.
        self.absorb_session_id(&response)?;

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if content_type.starts_with("text/event-stream") {
            self.read_event_stream(expected_id, response, tools_list_changed)
                .await
        } else if content_type.starts_with("application/json") {
            let body = read_bounded_body(response).await?;
            let value: Value = serde_json::from_slice(&body)
                .map_err(|error| mcp_error("external server returned malformed JSON-RPC", error))?;
            match crate::client::classify_incoming(value, expected_id, tools_list_changed)? {
                crate::client::Incoming::FinalResult(result) => Ok(result),
                crate::client::Incoming::ServerRequest { .. }
                | crate::client::Incoming::Ignored => Err(mcp_message(
                    "external server JSON response did not answer the request",
                )),
            }
        } else {
            Err(mcp_message(
                "external server replied with an unsupported content type",
            ))
        }
    }

    /// Send one notification. Streamable HTTP servers acknowledge with an
    /// empty 202; any success status is accepted and the body is ignored.
    pub(crate) async fn notify(&mut self, message: &Value) -> Result<()> {
        let response = self.post(message, None).await?;
        check_status(response.status())?;
        self.absorb_session_id(&response)
    }

    /// POST one message. A `bearer` override replaces the connection's own
    /// Authorization for this request only — a gateway-attested `tools/call`
    /// must present the calling chat's token — and is validated exactly like
    /// the connect-time token so it can never smuggle header syntax.
    async fn post(&self, message: &Value, bearer: Option<&str>) -> Result<reqwest::Response> {
        let authorization = match bearer {
            Some(token) => {
                if token.is_empty() || token.bytes().any(|byte| byte.is_ascii_control()) {
                    return Err(mcp_message(
                        "external server bearer token must be a non-empty header-safe value",
                    ));
                }
                Some(format!("Bearer {token}"))
            }
            None => self.authorization.clone(),
        };
        // One map, built configured-first and then overwritten by every
        // client-generated header, so a configured entry can never displace
        // what this transport says about itself — whatever the builder's
        // append-vs-replace semantics happen to be.
        let mut headers = self.configured.clone();
        headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/json, text/event-stream"),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static(PROTOCOL_VERSION_HEADER),
            reqwest::header::HeaderValue::from_static(PROTOCOL_VERSION),
        );
        if let Some(authorization) = &authorization {
            let mut value = reqwest::header::HeaderValue::from_str(authorization)
                .map_err(|_| mcp_message("external server bearer token is not header-safe"))?;
            value.set_sensitive(true);
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
        if let Some(session_id) = &self.session_id {
            let value = reqwest::header::HeaderValue::from_str(session_id)
                .map_err(|_| mcp_message("external server session id is not header-safe"))?;
            headers.insert(
                reqwest::header::HeaderName::from_static(SESSION_ID_HEADER),
                value,
            );
        }
        self.client
            .post(self.url.clone())
            .headers(headers)
            .json(message)
            .send()
            .await
            .map_err(|error| mcp_error("could not reach external server", without_url(error)))
    }

    async fn read_event_stream(
        &mut self,
        expected_id: u64,
        response: reqwest::Response,
        tools_list_changed: &mut bool,
    ) -> Result<Value> {
        use futures::StreamExt;

        let mut stream = response.bytes_stream();
        let mut parser = SseParser::default();
        let mut server_requests: Vec<(Value, String)> = Vec::new();
        let mut outcome = None;
        'read: while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                mcp_error("could not read external server stream", without_url(error))
            })?;
            for data in parser.push(&chunk)? {
                let value: Value = serde_json::from_slice(data.as_bytes()).map_err(|error| {
                    mcp_error("external server returned malformed JSON-RPC", error)
                })?;
                match crate::client::classify_incoming(value, expected_id, tools_list_changed)? {
                    crate::client::Incoming::FinalResult(result) => {
                        outcome = Some(result);
                        break 'read;
                    }
                    crate::client::Incoming::ServerRequest { id, method } => {
                        server_requests.push((id, method));
                    }
                    crate::client::Incoming::Ignored => {}
                }
            }
        }
        drop(stream);
        // Answer embedded server requests after releasing the stream so the
        // reply POSTs cannot deadlock against an unread response body.
        for (id, method) in server_requests {
            let response = crate::client::server_request_response(id, &method)?;
            let _ = self.notify(&response).await;
        }
        outcome.ok_or_else(|| mcp_message("external server closed the stream before replying"))
    }

    fn absorb_session_id(&mut self, response: &reqwest::Response) -> Result<()> {
        let Some(session_id) = response.headers().get(SESSION_ID_HEADER) else {
            return Ok(());
        };
        let session_id = session_id
            .to_str()
            .map_err(|_| mcp_message("external server session id is not visible ASCII"))?;
        if session_id.is_empty() || session_id.len() > MAX_SESSION_ID_BYTES {
            return Err(mcp_message("external server session id exceeds the limit"));
        }
        self.session_id = Some(session_id.to_string());
        Ok(())
    }
}

fn check_status(status: reqwest::StatusCode) -> Result<()> {
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(mcp_message(
            "external server rejected the configured credentials",
        ));
    }
    if !status.is_success() {
        return Err(mcp_message(format!(
            "external server replied with HTTP status {}",
            status.as_u16()
        )));
    }
    Ok(())
}

/// Strip the request URL from a reqwest error display chain. The URL is
/// non-secret configuration, but diagnostics stay stable and minimal.
fn without_url(error: reqwest::Error) -> impl std::fmt::Display {
    error.without_url()
}

async fn read_bounded_body(response: reqwest::Response) -> Result<Vec<u8>> {
    use futures::StreamExt;

    if response
        .content_length()
        .is_some_and(|length| length > MAX_JSON_RPC_FRAME_BYTES as u64)
    {
        return Err(mcp_message(
            "external server JSON-RPC frame exceeds the limit",
        ));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            mcp_error(
                "could not read external server response",
                without_url(error),
            )
        })?;
        if body.len().saturating_add(chunk.len()) > MAX_JSON_RPC_FRAME_BYTES {
            return Err(mcp_message(
                "external server JSON-RPC frame exceeds the limit",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Incremental `text/event-stream` parser that yields complete event payloads.
///
/// Only `data:` fields matter for MCP; other fields and comments are ignored.
/// The unconsumed buffer and any accumulating event share one frame bound.
#[derive(Default)]
struct SseParser {
    buffer: Vec<u8>,
    event_data: Vec<String>,
    event_bytes: usize,
}

impl SseParser {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>> {
        if self
            .buffer
            .len()
            .saturating_add(chunk.len())
            .saturating_add(self.event_bytes)
            > MAX_JSON_RPC_FRAME_BYTES
        {
            return Err(mcp_message(
                "external server JSON-RPC frame exceeds the limit",
            ));
        }
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = self.buffer.drain(..=newline).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                if !self.event_data.is_empty() {
                    events.push(self.event_data.join("\n"));
                    self.event_data.clear();
                    self.event_bytes = 0;
                }
                continue;
            }
            let Some(value) = line.strip_prefix(b"data:") else {
                // event/id/retry fields and ":" comments are irrelevant here.
                continue;
            };
            let value = value.strip_prefix(b" ").unwrap_or(value);
            let value = std::str::from_utf8(value)
                .map_err(|_| mcp_message("external server stream is not valid UTF-8"))?;
            self.event_bytes = self.event_bytes.saturating_add(value.len());
            self.event_data.push(value.to_string());
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderName};

    #[test]
    fn sse_parser_joins_multi_line_data_and_ignores_other_fields() {
        let mut parser = SseParser::default();
        let events = parser
            .push(b": comment\nevent: message\ndata: {\"a\":\ndata: 1}\nid: 7\n\n")
            .unwrap();
        assert_eq!(events, vec!["{\"a\":\n1}".to_string()]);
    }

    #[test]
    fn sse_parser_handles_crlf_and_split_chunks() {
        let mut parser = SseParser::default();
        assert!(parser.push(b"data: {\"x\"").unwrap().is_empty());
        assert!(parser.push(b": 2}\r\n").unwrap().is_empty());
        let events = parser.push(b"\r\n").unwrap();
        assert_eq!(events, vec!["{\"x\": 2}".to_string()]);
    }

    #[test]
    fn sse_parser_enforces_the_frame_bound() {
        let mut parser = SseParser::default();
        let error = parser
            .push(&vec![b'x'; MAX_JSON_RPC_FRAME_BYTES + 1])
            .expect_err("oversized stream must fail");
        assert!(error.to_string().contains("frame exceeds"));
    }

    #[test]
    fn url_validation_rejects_unsupported_shapes() {
        assert!(validate_http_url("http://127.0.0.1:9000/mcp").is_ok());
        assert!(validate_http_url("https://gateway.example/mcp/tools").is_ok());
        for url in [
            "ftp://host/mcp",
            "http://user:secret@host/mcp",
            "not a url",
            "unix:/tmp/socket",
        ] {
            assert!(validate_http_url(url).is_err(), "{url} must be rejected");
        }
    }

    #[test]
    fn bearer_tokens_must_be_header_safe() {
        let wire = |token| HttpWire::with_headers("http://127.0.0.1/mcp", token, HeaderMap::new());
        assert!(wire(Some("token-1")).is_ok());
        assert!(wire(None).is_ok());
        for token in ["", "line\nbreak", "tab\there"] {
            assert!(wire(Some(token)).is_err(), "{token:?} must be rejected");
        }
    }

    /// Contract: a configured static header cannot displace one this transport
    /// generates for itself. A plugin's `mcp.json` declares these headers, so
    /// an entry named `authorization` would otherwise let a package override
    /// the credential a gateway mount presents.
    #[test]
    fn configured_headers_never_displace_the_transports_own() {
        let configured = static_headers(&std::collections::BTreeMap::from([
            ("X-Client".to_owned(), "openwave".to_owned()),
            ("Authorization".to_owned(), "Bearer smuggled".to_owned()),
            ("MCP-Session-Id".to_owned(), "hijacked".to_owned()),
            ("Host".to_owned(), "elsewhere.example".to_owned()),
        ]))
        .unwrap();
        assert_eq!(
            configured
                .keys()
                .map(HeaderName::as_str)
                .collect::<Vec<_>>(),
            ["x-client"]
        );
        assert!(static_headers(&std::collections::BTreeMap::from([(
            "not a token".to_owned(),
            "x".to_owned()
        )]))
        .is_err());
    }
}
