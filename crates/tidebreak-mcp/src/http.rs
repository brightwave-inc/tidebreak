//! Streamable HTTP client transport for external MCP servers.
//!
//! Each JSON-RPC request is one authenticated `POST`. A server may answer with
//! a single `application/json` message or with a `text/event-stream` carrying
//! interim notifications before the final response. Responses to
//! client notifications are expected to be empty (HTTP 202).
//!
//! The bearer token lives only inside the prebuilt `Authorization` header value
//! and is never echoed into errors or logs.

use serde_json::Value;
use tidebreak_core::Result;

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
    validate_http_url_with_credentials(url, false)
}

/// Validate an MCP HTTP endpoint with its credential posture.
///
/// Cleartext HTTP is accepted only without credentials, or when the URL names
/// a literal loopback address. A hostname such as `localhost` is not sufficient
/// evidence here: static bearer/configured headers must not depend on ambient
/// DNS or hosts-file configuration to remain on-machine.
pub fn validate_http_url_with_credentials(url: &str, has_credentials: bool) -> Result<()> {
    let parsed =
        reqwest::Url::parse(url).map_err(|_| mcp_message("external server URL is invalid"))?;
    validate_parsed_http_url(&parsed, has_credentials)
}

fn validate_parsed_http_url(parsed: &reqwest::Url, has_credentials: bool) -> Result<()> {
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
    if has_credentials && parsed.scheme() == "http" && !is_literal_loopback(parsed) {
        return Err(mcp_message(
            "credentialed external server URLs must use https unless they name a literal loopback address",
        ));
    }
    Ok(())
}

fn is_literal_loopback(url: &reqwest::Url) -> bool {
    url.host_str()
        .map(|host| {
            host.strip_prefix('[')
                .and_then(|host| host.strip_suffix(']'))
                .unwrap_or(host)
        })
        .and_then(|host| host.parse::<std::net::IpAddr>().ok())
        .is_some_and(|address| address.is_loopback())
}

impl HttpWire {
    pub(crate) fn with_headers(
        url: &str,
        bearer_token: Option<&str>,
        configured: reqwest::header::HeaderMap,
    ) -> Result<Self> {
        let url =
            reqwest::Url::parse(url).map_err(|_| mcp_message("external server URL is invalid"))?;
        validate_parsed_http_url(&url, bearer_token.is_some() || !configured.is_empty())?;
        if bearer_token.is_some_and(|token| {
            token.is_empty() || token.bytes().any(|byte| byte.is_ascii_control())
        }) {
            return Err(mcp_message(
                "external server bearer token must be a non-empty header-safe value",
            ));
        }
        let mut builder = reqwest::Client::builder()
            // A credentialed client must not follow redirects: reqwest
            // strips Authorization cross-host, but a same-host https→http
            // downgrade would resend the bearer in cleartext. This is also
            // what keeps configured static headers — which reqwest knows
            // nothing about and would carry across a hop — from ever
            // reaching a different origin.
            .redirect(reqwest::redirect::Policy::none());
        // Loopback MCP servers live on this machine. An inherited HTTP proxy
        // would steal those connections and turn Save and verify into a hang
        // or a 403 from the proxy.
        if is_literal_loopback(&url) {
            builder = builder.no_proxy();
        }
        Ok(Self {
            client: builder
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
            self.read_event_stream(expected_id, response, tools_list_changed, bearer)
                .await
        } else if content_type.starts_with("application/json") {
            let body = read_bounded_body(response).await?;
            let value: Value =
                serde_json::from_slice(&body).map_err(|_| protocol_negotiation_failed(&body))?;
            match crate::client::classify_incoming(value, expected_id, tools_list_changed)? {
                crate::client::Incoming::FinalResult(result) => Ok(result),
                crate::client::Incoming::ServerRequest { .. }
                | crate::client::Incoming::Ignored => Err(protocol_negotiation_failed(&body)),
            }
        } else {
            let body = read_bounded_body(response).await.unwrap_or_default();
            Err(protocol_negotiation_failed(&body))
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
        validate_parsed_http_url(
            &self.url,
            authorization.is_some() || !self.configured.is_empty() || self.session_id.is_some(),
        )?;
        self.ensure_host_resolves().await?;
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
            let mut value = reqwest::header::HeaderValue::from_str(session_id)
                .map_err(|_| mcp_message("external server session id is not header-safe"))?;
            value.set_sensitive(true);
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
            .map_err(|error| {
                classify_http_transport_error(error, self.url.host_str().unwrap_or("unknown"))
            })
    }

    async fn read_event_stream(
        &mut self,
        expected_id: u64,
        response: reqwest::Response,
        tools_list_changed: &mut bool,
        bearer: Option<&str>,
    ) -> Result<Value> {
        use futures::StreamExt;

        let mut stream = response.bytes_stream();
        let mut parser = SseParser::default();
        let mut outcome = None;
        'read: while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                mcp_error("could not read external server stream", without_url(error))
            })?;
            for data in parser.push(&chunk)? {
                let value: Value = serde_json::from_slice(data.as_bytes())
                    .map_err(|_| protocol_negotiation_failed(data.as_bytes()))?;
                match crate::client::classify_incoming(value, expected_id, tools_list_changed)? {
                    crate::client::Incoming::FinalResult(result) => {
                        outcome = Some(result);
                        break 'read;
                    }
                    crate::client::Incoming::ServerRequest { id, method } => {
                        // Streamable HTTP carries server requests down the
                        // open response stream, but their replies travel in a
                        // separate POST. Answer before reading the next event:
                        // a prompt server may wait for this reply before it
                        // emits the original request's final result.
                        let message = crate::client::server_request_response(id, &method)?;
                        let response = self.post(&message, bearer).await?;
                        check_status(response.status())?;
                        self.absorb_session_id(&response)?;
                    }
                    crate::client::Incoming::Ignored => {}
                }
            }
        }
        outcome.ok_or_else(|| mcp_message("external server closed the stream before replying"))
    }

    async fn ensure_host_resolves(&self) -> Result<()> {
        let Some(host) = self.url.host_str() else {
            return Ok(());
        };
        if host.parse::<std::net::IpAddr>().is_ok() {
            return Ok(());
        }
        let port = self.url.port_or_known_default().unwrap_or(80);
        match std::net::ToSocketAddrs::to_socket_addrs(&(host, port)) {
            Ok(addresses) => {
                if addresses.into_iter().next().is_some() {
                    Ok(())
                } else {
                    Err(mcp_message(format!("DNS resolution failed ({host}).")))
                }
            }
            Err(_) => Err(mcp_message(format!("DNS resolution failed ({host})."))),
        }
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
    if status.is_success() {
        return Ok(());
    }
    Err(mcp_message(http_status_diagnostic(status)))
}

fn http_status_diagnostic(status: reqwest::StatusCode) -> String {
    let status_line = status
        .canonical_reason()
        .map(|reason| format!("{} {reason}", status.as_u16()))
        .unwrap_or_else(|| status.as_u16().to_string());
    match status.as_u16() {
        401 | 403 => format!("Authentication failed ({status_line})."),
        404 => format!("Wrong path ({status_line})."),
        500..=599 => format!("Server error ({status_line})."),
        _ => format!("HTTP status {status_line}."),
    }
}

fn protocol_negotiation_failed(body: &[u8]) -> tidebreak_core::AgentError {
    mcp_message(format!(
        "Protocol negotiation failed. The endpoint answered but not with MCP JSON-RPC. First bytes: {}.",
        quote_first_bytes(body)
    ))
}

/// Quote a bounded prefix of an upstream body. Never the URL or a token.
fn quote_first_bytes(body: &[u8]) -> String {
    const MAX: usize = 64;
    let slice = &body[..body.len().min(MAX)];
    let mut out = String::from("\"");
    for &byte in slice {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            0x20..=0x7e => out.push(byte as char),
            _ => out.push_str(&format!("\\x{byte:02x}")),
        }
    }
    out.push('"');
    if body.len() > MAX {
        out.push('…');
    }
    out
}

fn classify_http_transport_error(error: reqwest::Error, host: &str) -> tidebreak_core::AgentError {
    let error = error.without_url();
    if error.is_timeout() {
        return mcp_message("Timed out after the HTTP client deadline.");
    }
    let chain = error_chain_lower(&error);
    if is_dns_failure(&chain) {
        return mcp_message(format!("DNS resolution failed ({host})."));
    }
    if is_tls_failure(&chain) {
        return mcp_message(format!(
            "TLS handshake failed ({}).",
            tls_reason(&error.to_string())
        ));
    }
    mcp_error("could not reach external server", error)
}

fn error_chain_lower(error: &reqwest::Error) -> String {
    let mut text = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(err) = source {
        text.push('\n');
        text.push_str(&err.to_string());
        source = err.source();
    }
    text.to_ascii_lowercase()
}

fn is_dns_failure(chain: &str) -> bool {
    chain.contains("dns error")
        || chain.contains("failed to lookup address information")
        || chain.contains("no such host")
        || chain.contains("name or service not known")
        || chain.contains("nodename nor servname provided")
}

fn is_tls_failure(chain: &str) -> bool {
    chain.contains("tls")
        || chain.contains("certificate")
        || chain.contains("ssl")
        || chain.contains("rustls")
        || chain.contains("webpki")
        || chain.contains("unknownissuer")
        || chain.contains("handshake")
        || chain.contains("close_notify")
        || chain.contains("corrupt message")
}

fn tls_reason(display: &str) -> String {
    let lower = display.to_ascii_lowercase();
    let start = [
        "invalid peer certificate",
        "certificate",
        "tls handshake",
        "handshake failure",
        "tls",
    ]
    .iter()
    .filter_map(|marker| lower.find(marker))
    .min()
    .unwrap_or(0);
    let snippet = display.get(start..).unwrap_or(display).trim();
    const MAX: usize = 80;
    if snippet.len() <= MAX {
        snippet.trim_end_matches('.').to_string()
    } else {
        format!("{}…", snippet[..MAX].trim())
    }
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
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use axum::body::{Body, Bytes};
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::{Json, Router};
    use reqwest::header::HeaderName;

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
        assert!(validate_http_url("http://remote.example/mcp").is_ok());
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
    fn credentialed_http_requires_a_literal_loopback_address() {
        for url in [
            "http://127.0.0.1:9000/mcp",
            "http://127.27.4.9/mcp",
            "http://[::1]:9000/mcp",
            "https://gateway.example/mcp",
        ] {
            assert!(
                validate_http_url_with_credentials(url, true).is_ok(),
                "{url} should be accepted"
            );
        }
        for url in [
            "http://gateway.example/mcp",
            "http://192.0.2.10/mcp",
            "http://localhost:9000/mcp",
        ] {
            assert!(
                validate_http_url_with_credentials(url, true).is_err(),
                "{url} must be rejected"
            );
        }
    }

    #[test]
    fn transport_rejects_remote_cleartext_credentials() {
        assert!(HttpWire::with_headers(
            "http://gateway.example/mcp",
            Some("secret"),
            HeaderMap::new(),
        )
        .is_err());

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-api-key"),
            reqwest::header::HeaderValue::from_static("secret"),
        );
        assert!(HttpWire::with_headers("http://gateway.example/mcp", None, headers).is_err());
    }

    #[tokio::test]
    async fn dynamic_bearer_rejects_an_otherwise_uncredentialed_remote_http_url() {
        let wire =
            HttpWire::with_headers("http://gateway.example/mcp", None, HeaderMap::new()).unwrap();
        let error = wire
            .post(&serde_json::json!({}), Some("dynamic-secret"))
            .await
            .expect_err("dynamic credentials must not cross remote cleartext HTTP");
        assert!(error.to_string().contains("must use https"), "{error}");
    }

    #[tokio::test]
    async fn server_issued_session_is_not_sent_over_remote_cleartext_http() {
        async fn handler(State(requests): State<Arc<AtomicUsize>>) -> impl IntoResponse {
            requests.fetch_add(1, Ordering::SeqCst);
            (
                StatusCode::OK,
                [
                    ("content-type", "application/json"),
                    (SESSION_ID_HEADER, "session-secret"),
                ],
                serde_json::json!({"jsonrpc":"2.0","id":1,"result":{}}).to_string(),
            )
        }

        let requests = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/mcp", post(handler))
            .with_state(Arc::clone(&requests));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let mut wire = HttpWire {
            client: reqwest::Client::builder()
                .no_proxy()
                .resolve("remote.example", address)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            url: reqwest::Url::parse(&format!("http://remote.example:{}/mcp", address.port()))
                .unwrap(),
            authorization: None,
            configured: HeaderMap::new(),
            session_id: None,
        };
        let mut tools_list_changed = false;
        wire.request(
            1,
            &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
            &mut tools_list_changed,
            None,
        )
        .await
        .unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 1);

        let error = wire
            .notify(&serde_json::json!({
                "jsonrpc":"2.0",
                "method":"notifications/initialized"
            }))
            .await
            .expect_err("the session credential must not cross remote cleartext HTTP");
        assert!(error.to_string().contains("must use https"), "{error}");
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn replies_to_server_requests_before_the_http_stream_finishes() {
        #[derive(Clone, Default)]
        struct PromptState {
            reply: Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
        }

        async fn handler(
            State(state): State<PromptState>,
            headers: HeaderMap,
            Json(message): Json<Value>,
        ) -> axum::response::Response {
            assert_eq!(
                headers
                    .get(reqwest::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer chat-token")
            );

            if message.get("method").is_some() {
                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                *state.reply.lock().await = Some(reply_tx);
                let (event_tx, event_rx) = tokio::sync::mpsc::channel(2);
                event_tx
                    .send(Ok::<_, Infallible>(Bytes::from_static(
                        b"data: {\"jsonrpc\":\"2.0\",\"id\":\"server-ping\",\"method\":\"ping\"}\n\n",
                    )))
                    .await
                    .unwrap();
                tokio::spawn(async move {
                    reply_rx.await.unwrap();
                    event_tx
                        .send(Ok(Bytes::from_static(
                            b"data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n",
                        )))
                        .await
                        .unwrap();
                });
                let stream = futures::stream::unfold(event_rx, |mut receiver| async move {
                    receiver.recv().await.map(|event| (event, receiver))
                });
                return axum::response::Response::builder()
                    .status(StatusCode::OK)
                    .header(reqwest::header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(stream))
                    .unwrap();
            }

            assert_eq!(
                message,
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "server-ping",
                    "result": {}
                })
            );
            state
                .reply
                .lock()
                .await
                .take()
                .expect("the response stream registered its reply channel")
                .send(())
                .unwrap();
            axum::response::Response::builder()
                .status(StatusCode::ACCEPTED)
                .body(Body::empty())
                .unwrap()
        }

        let app = Router::new()
            .route("/mcp", post(handler))
            .with_state(PromptState::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let mut wire =
            HttpWire::with_headers(&format!("http://{address}/mcp"), None, HeaderMap::new())
                .unwrap();
        let mut tools_list_changed = false;
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            wire.request(
                1,
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/list",
                    "params": {}
                }),
                &mut tools_list_changed,
                Some("chat-token"),
            ),
        )
        .await
        .expect("the prompt reply must unblock the open response stream")
        .unwrap();

        assert_eq!(result, serde_json::json!({"tools": []}));
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
            ("X-Client".to_owned(), "tidebreak".to_owned()),
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
