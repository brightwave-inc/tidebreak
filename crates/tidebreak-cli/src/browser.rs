//! Tidebreak browser CLI commands and the `browser-mcp` stdio server.
//!
//! `tidebreak browser list|navigate|snapshot|wait|screenshot --json` drive a
//! running Tidebreak browser server through the session-private capability
//! file named by `TIDEBREAK_BROWSER_CAPFILE`. Every operation runs through a
//! single [`BrowserClient`] that is shared between the direct CLI and the MCP
//! tool implementations.
//!
//! `tidebreak browser-mcp` serves exactly five tools — browser_list,
//! browser_navigate, browser_snapshot, browser_wait, and browser_screenshot —
//! over MCP stdio, using the canonical core tool specs and validating typed
//! arguments before sending them to the browser server.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tidebreak_core::{
    browser_act_tool_spec, browser_list_tool_spec, browser_navigate_tool_spec,
    browser_screenshot_tool_spec, browser_snapshot_tool_spec, browser_wait_tool_spec,
    validate_browser_act_arguments, validate_browser_list_arguments,
    validate_browser_navigate_arguments, validate_browser_screenshot_arguments,
    validate_browser_snapshot_arguments, validate_browser_wait_arguments, AgentError,
    ApprovalClass, AutoApproveGate, BrowserActArgs, BrowserActResult, BrowserAction,
    BrowserListResult, BrowserNavigateArgs, BrowserNavigateResult, BrowserPageSnapshot,
    BrowserScreenshotArgs, BrowserScreenshotResult, BrowserSnapshotArgs, BrowserWaitArgs,
    BrowserWaitCondition, BrowserWaitResult, DocumentBlob, ImageData, ImageMediaType, ImageRef,
    Result, Tool, ToolCtx, ToolErrorCategory, ToolOutput, ToolRegistry, ToolSpec, MAX_IMAGE_BYTES,
    MAX_IMAGE_DIMENSION,
};

// ---------------------------------------------------------------------------
// Capability file
// ---------------------------------------------------------------------------

/// Maximum bytes the capfile is allowed to be (64 KiB).
const CAPFILE_MAX_BYTES: u64 = 65_536;

/// Hard ceiling on a successful snapshot response body. A reply larger than
/// this is refused rather than decoded unbounded.
const SNAPSHOT_BODY_MAX_BYTES: usize = 4 * 1024 * 1024; // 4 MiB

/// Ceiling on a list response body (small, bounded).
const LIST_BODY_MAX_BYTES: usize = 256 * 1024; // 256 KiB

/// Ceiling on a navigate response body (very small).
const NAVIGATE_BODY_MAX_BYTES: usize = 64 * 1024; // 64 KiB

/// Ceiling on an error body we attempt to parse for `{kind, message}`.
const ERROR_BODY_MAX_BYTES: usize = 8 * 1024; // 8 KiB

/// Ceiling on a wait response body (small, bounded).
const WAIT_BODY_MAX_BYTES: usize = 64 * 1024; // 64 KiB

/// Ceiling on a screenshot response body. The base-64 payload within can be up
/// to `ceil(MAX_BROWSER_SCREENSHOT_PNG_BYTES * 4/3)` = ~10.7 MiB, so the JSON
/// envelope needs generous headroom.
const SCREENSHOT_BODY_MAX_BYTES: usize = 16 * 1024 * 1024; // 16 MiB

/// Ceiling on a semantic action response body.
const ACT_BODY_MAX_BYTES: usize = 64 * 1024; // 64 KiB

/// Encoded form of the largest image the shared image pipeline will accept.
const MAX_SCREENSHOT_BASE64_CHARS: usize = (MAX_IMAGE_BYTES as usize).div_ceil(3) * 4;

const BODY_TOO_LARGE_DETAIL: &str = "response body exceeded the size limit";

const TOKEN_PREFIX: &str = "tbreak_bt_";
const TOKEN_LENGTH: usize = TOKEN_PREFIX.len() + 36; // prefix + UUID

/// The session-private capability file, read from the path named by
/// `TIDEBREAK_BROWSER_CAPFILE`. It carries exactly the data needed to reach a
/// running browser server: an endpoint and a bearer token.
///
/// This struct must never derive `Debug` — the token must not appear in debug
/// output or error formatting.
#[derive(Clone)]
struct BrowserCapfile {
    endpoint: String,
    token: String,
    semantic_actions: bool,
}

/// Wire shape of the capfile. The only supported version is 1.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserCapfileWire {
    version: u32,
    endpoint: String,
    token: String,
    #[serde(default)]
    semantic_actions: bool,
}

impl BrowserCapfile {
    /// Read and validate the capfile at `path`.
    ///
    /// Fails closed: the file must exist, fit in a hard byte cap, be regular
    /// UTF-8 JSON, declare version 1, carry a loopback HTTP endpoint with an
    /// explicit port and exact path `/code/browser`, no user/pass/query/fragment,
    /// and a non-empty bearer token of the canonical `tbreak_bt_<UUID>` shape.
    ///
    /// The endpoint, token, and capfile path are never printed in error output.
    fn load(path: &std::path::Path) -> Result<Self> {
        // Stat once for the soft guard; the read uses a hard cap so a racing
        // append cannot allocate unbounded.
        let metadata = std::fs::symlink_metadata(path).map_err(|error| {
            AgentError::config(format!("browser capfile cannot be read ({error})"))
        })?;
        if !metadata.file_type().is_file() {
            return Err(AgentError::config("browser capfile must be a regular file"));
        }
        if metadata.len() > CAPFILE_MAX_BYTES {
            return Err(AgentError::config("browser capfile exceeds the size limit"));
        }

        // Read at most CAPFILE_MAX_BYTES + 1: the +1 lets us detect oversized
        // files without allocating beyond the ceiling.
        let raw = read_file_capped(path, CAPFILE_MAX_BYTES as usize).map_err(|error| {
            AgentError::config(format!("browser capfile cannot be read ({error})"))
        })?;
        if raw.len() > CAPFILE_MAX_BYTES as usize {
            return Err(AgentError::config("browser capfile exceeds the size limit"));
        }

        let wire: BrowserCapfileWire = serde_json::from_str(&raw).map_err(|error| {
            AgentError::config(format!("browser capfile is not valid JSON ({error})"))
        })?;
        if wire.version != 1 {
            return Err(AgentError::config(format!(
                "browser capfile version {} is not supported (only version 1)",
                wire.version
            )));
        }
        Self::validate_endpoint(&wire.endpoint)?;
        Self::validate_token(&wire.token)?;
        Ok(Self {
            endpoint: wire.endpoint,
            token: wire.token,
            semantic_actions: wire.semantic_actions,
        })
    }

    /// Load from the path named by the `TIDEBREAK_BROWSER_CAPFILE` environment
    /// variable, or fail with a clear message if the variable is not set.
    fn from_env() -> Result<Self> {
        let path = std::env::var_os("TIDEBREAK_BROWSER_CAPFILE")
            .map(PathBuf::from)
            .ok_or_else(|| AgentError::config("TIDEBREAK_BROWSER_CAPFILE is not set"))?;
        Self::load(&path)
    }

    /// Validate the endpoint: HTTP only, loopback host, explicit port, path
    /// exactly `/code/browser`, no user/pass/query/fragment.
    fn validate_endpoint(endpoint: &str) -> Result<()> {
        let url: url::Url = endpoint
            .parse()
            .map_err(|_| AgentError::config("browser capfile endpoint is not a valid URL"))?;
        if url.scheme() != "http" {
            return Err(AgentError::config("browser capfile endpoint must use http"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(AgentError::config(
                "browser capfile endpoint must not contain credentials",
            ));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(AgentError::config(
                "browser capfile endpoint must not contain a query or fragment",
            ));
        }
        if url.path() != "/code/browser" {
            return Err(AgentError::config(
                "browser capfile endpoint path must be /code/browser",
            ));
        }
        let Some(host) = url.host_str() else {
            return Err(AgentError::config("browser capfile endpoint has no host"));
        };
        if url.port().is_none() {
            return Err(AgentError::config(
                "browser capfile endpoint must include an explicit port",
            ));
        }
        let is_loopback = host.eq_ignore_ascii_case("localhost")
            || host.to_ascii_lowercase().ends_with(".localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|addr| addr.is_loopback());
        if !is_loopback {
            return Err(AgentError::config(
                "browser capfile endpoint is not a loopback address",
            ));
        }
        Ok(())
    }

    /// Validate the v1 token shape: `tbreak_bt_` prefix followed by a UUID.
    fn validate_token(token: &str) -> Result<()> {
        if token.is_empty() {
            return Err(AgentError::config("browser capfile token is empty"));
        }
        if token.len() != TOKEN_LENGTH {
            return Err(AgentError::config(
                "browser capfile token has an unexpected length",
            ));
        }
        if !token.starts_with(TOKEN_PREFIX) {
            return Err(AgentError::config(
                "browser capfile token has an unexpected prefix",
            ));
        }
        let uuid_part = &token[TOKEN_PREFIX.len()..];
        uuid::Uuid::parse_str(uuid_part)
            .map_err(|_| AgentError::config("browser capfile token suffix is not a UUID"))?;
        Ok(())
    }
}

/// Read `path` into a `String`, reading at most `cap + 1` bytes so a racing
/// append cannot allocate unbounded. Returns the full bytes read; the caller
/// checks against the cap.
fn read_file_capped(path: &std::path::Path, cap: usize) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut buf = Vec::with_capacity(cap.saturating_add(1).min(cap + 4096));
    let mut chunk = [0u8; 8192];
    loop {
        let n = file.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        let room = cap.saturating_add(1).saturating_sub(buf.len());
        let take = n.min(room);
        buf.extend_from_slice(&chunk[..take]);
        if buf.len() > cap {
            // The open file closes when this function returns; there is no
            // reason to drain a racing append after the hard cap is reached.
            break;
        }
    }
    String::from_utf8(buf)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

// ---------------------------------------------------------------------------
// Browser client
// ---------------------------------------------------------------------------

/// Shared HTTP client for the browser server, constructed once from the
/// capfile. Every request carries `Authorization: Bearer <token>`, has
/// redirects disabled, and uses bounded connect and read timeouts.
///
/// This struct must never derive `Debug` — the token must not appear in debug
/// output or error formatting.
#[derive(Clone)]
struct BrowserClient {
    client: reqwest::Client,
    endpoint: String,
    token: String,
}

impl BrowserClient {
    /// Build a client from the given capfile.
    fn new(cap: &BrowserCapfile) -> Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            // Headroom over the core 30 s native-operation ceiling. A valid
            // 30 000 ms wait must not be cut off at the client boundary.
            .timeout(Duration::from_secs(40))
            .build()
            .map_err(|error| {
                AgentError::config(format!("could not build browser HTTP client ({error})"))
            })?;
        Ok(Self {
            client,
            endpoint: cap.endpoint.clone(),
            token: cap.token.clone(),
        })
    }

    /// Read a bounded response body, then deserialize to `T`.
    async fn read_bounded_json<T: serde::de::DeserializeOwned>(
        response: reqwest::Response,
        max_bytes: usize,
    ) -> std::result::Result<T, ClientFailure> {
        let status = response.status();
        let body_limit = if status.is_success() {
            max_bytes
        } else {
            max_bytes.min(ERROR_BODY_MAX_BYTES)
        };
        let bytes = match read_response_body_bounded(response, body_limit).await {
            Ok(bytes) => bytes,
            Err(ClientFailure::ToolFailed { detail })
                if !status.is_success() && detail == BODY_TOO_LARGE_DETAIL =>
            {
                return Err(ClientFailure::from_http_status(
                    status.as_u16(),
                    "error_body_too_large",
                    "server error body exceeded the size limit",
                ));
            }
            Err(failure) => return Err(failure),
        };
        if status.is_success() {
            serde_json::from_slice(&bytes).map_err(|error| ClientFailure::TransportFailed {
                detail: format!("unreadable success body ({error})"),
            })
        } else {
            // Try to decode a stable server error {kind, message}.
            let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            let kind = body
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let message = body
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("no detail");
            Err(ClientFailure::from_http_status(
                status.as_u16(),
                kind,
                message,
            ))
        }
    }
}

/// Read a response body up to `max_bytes`. Beyond that, the remainder is
/// drained and the reader returns an error.
async fn read_response_body_bounded(
    response: reqwest::Response,
    max_bytes: usize,
) -> std::result::Result<Vec<u8>, ClientFailure> {
    use futures::StreamExt;

    let mut stream = response.bytes_stream();
    let mut buf = Vec::with_capacity(max_bytes.min(4096));
    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|error| ClientFailure::TransportFailed {
            detail: format!("body chunk error ({error})"),
        })?;
        let room = max_bytes.saturating_sub(buf.len());
        if chunk.len() > room {
            // Dropping the response closes or retires this connection. Return
            // immediately so an oversized stream cannot monopolize MCP stdio.
            return Err(ClientFailure::ToolFailed {
                detail: BODY_TOO_LARGE_DETAIL.to_string(),
            });
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

// -- client failure type ------------------------------------------------

/// Typed failure from the browser client, mapped to [`ToolErrorCategory`] for
/// the MCP tools and to [`AgentError`] for the direct CLI.
///
/// The bearer token and capfile path are never included in these errors.
#[derive(Debug, Clone)]
enum ClientFailure {
    InvalidArguments { detail: String },
    NotFound { detail: String },
    ConfigurationRequired { detail: String },
    TransportFailed { detail: String },
    ToolFailed { detail: String },
}

impl ClientFailure {
    /// Classify an HTTP error status plus the server's `{kind, message}` body
    /// into the right [`ClientFailure`] variant.
    fn from_http_status(status: u16, kind: &str, message: &str) -> ClientFailure {
        // Scrub the message: if it contains the token prefix or any plausible
        // bearer pattern, redact it.
        let scrubbed = scrub_server_message(message);
        let detail = format!("({kind}) {scrubbed}");
        match status {
            400 => ClientFailure::InvalidArguments { detail },
            401 | 403 | 501 => ClientFailure::ConfigurationRequired { detail },
            404 => ClientFailure::NotFound { detail },
            _ => ClientFailure::ToolFailed { detail },
        }
    }

    fn to_tool_error_category(&self) -> ToolErrorCategory {
        match self {
            ClientFailure::InvalidArguments { .. } => ToolErrorCategory::InvalidArguments,
            ClientFailure::NotFound { .. } => ToolErrorCategory::NotFound,
            ClientFailure::ConfigurationRequired { .. } => ToolErrorCategory::ConfigurationRequired,
            ClientFailure::TransportFailed { .. } => ToolErrorCategory::TransportFailed,
            ClientFailure::ToolFailed { .. } => ToolErrorCategory::ToolFailed,
        }
    }

    fn redacted_text(&self) -> String {
        match self {
            ClientFailure::InvalidArguments { detail } => {
                format!("browser: invalid arguments — {detail}")
            }
            ClientFailure::NotFound { detail } => {
                format!("browser: not found — {detail}")
            }
            ClientFailure::ConfigurationRequired { detail } => {
                format!("browser: configuration required — {detail}")
            }
            ClientFailure::TransportFailed { detail } => {
                format!("browser: transport failed — {detail}")
            }
            ClientFailure::ToolFailed { detail } => {
                format!("browser: tool failed — {detail}")
            }
        }
    }
}

impl std::fmt::Display for ClientFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.redacted_text())
    }
}

impl std::error::Error for ClientFailure {}

/// Remove any plausible token-bearing text from a server error message.
fn scrub_server_message(message: &str) -> String {
    // If the message literally contains the token prefix, redact the whole
    // thing to a static label.
    if message.contains("tbreak_") || message.contains("bearer") || message.contains("Bearer") {
        return "[redacted]".to_string();
    }
    // Also scrub any substring that looks like a UUID.
    let mut result = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(start) = rest.char_indices().find_map(|(start, _)| {
        let end = start.checked_add(36)?;
        rest.get(start..end)
            .filter(|candidate| uuid::Uuid::parse_str(candidate).is_ok())
            .map(|_| start)
    }) {
        result.push_str(&rest[..start]);
        result.push_str("[redacted]");
        rest = &rest[start + 36..];
    }
    result.push_str(rest);
    result
}

// -- typed operations ---------------------------------------------------

async fn browser_list(
    client: &BrowserClient,
) -> std::result::Result<BrowserListResult, ClientFailure> {
    let response = client
        .client
        .get(format!("{}/list", client.endpoint))
        .bearer_auth(&client.token)
        .send()
        .await
        .map_err(|error| ClientFailure::TransportFailed {
            detail: format!("browser list request failed: {error}"),
        })?;
    BrowserClient::read_bounded_json(response, LIST_BODY_MAX_BYTES).await
}

async fn browser_navigate(
    client: &BrowserClient,
    args: &BrowserNavigateArgs,
) -> std::result::Result<BrowserNavigateResult, ClientFailure> {
    let response = client
        .client
        .post(format!("{}/navigate", client.endpoint))
        .bearer_auth(&client.token)
        .json(args)
        .send()
        .await
        .map_err(|error| ClientFailure::TransportFailed {
            detail: format!("browser navigate request failed: {error}"),
        })?;
    BrowserClient::read_bounded_json(response, NAVIGATE_BODY_MAX_BYTES).await
}

async fn browser_snapshot(
    client: &BrowserClient,
    args: &BrowserSnapshotArgs,
) -> std::result::Result<BrowserPageSnapshot, ClientFailure> {
    let response = client
        .client
        .post(format!("{}/snapshot", client.endpoint))
        .bearer_auth(&client.token)
        .json(args)
        .send()
        .await
        .map_err(|error| ClientFailure::TransportFailed {
            detail: format!("browser snapshot request failed: {error}"),
        })?;
    BrowserClient::read_bounded_json(response, SNAPSHOT_BODY_MAX_BYTES).await
}

async fn browser_wait(
    client: &BrowserClient,
    args: &BrowserWaitArgs,
) -> std::result::Result<BrowserWaitResult, ClientFailure> {
    let response = client
        .client
        .post(format!("{}/wait", client.endpoint))
        .bearer_auth(&client.token)
        .json(args)
        .send()
        .await
        .map_err(|error| ClientFailure::TransportFailed {
            detail: format!("browser wait request failed: {error}"),
        })?;
    BrowserClient::read_bounded_json(response, WAIT_BODY_MAX_BYTES).await
}

async fn browser_screenshot(
    client: &BrowserClient,
    args: &BrowserScreenshotArgs,
) -> std::result::Result<BrowserScreenshotResult, ClientFailure> {
    let response = client
        .client
        .post(format!("{}/screenshot", client.endpoint))
        .bearer_auth(&client.token)
        .json(args)
        .send()
        .await
        .map_err(|error| ClientFailure::TransportFailed {
            detail: format!("browser screenshot request failed: {error}"),
        })?;
    BrowserClient::read_bounded_json(response, SCREENSHOT_BODY_MAX_BYTES).await
}

async fn browser_act(
    client: &BrowserClient,
    args: &BrowserActArgs,
) -> std::result::Result<BrowserActResult, ClientFailure> {
    let response = client
        .client
        .post(format!("{}/act", client.endpoint))
        .bearer_auth(&client.token)
        .json(args)
        .send()
        .await
        .map_err(|error| ClientFailure::TransportFailed {
            detail: format!("browser act request failed: {error}"),
        })?;
    BrowserClient::read_bounded_json(response, ACT_BODY_MAX_BYTES).await
}

// ---------------------------------------------------------------------------
// CLI parsing
// ---------------------------------------------------------------------------

/// Parsed `tidebreak browser …` subcommand.
#[derive(Debug, Clone)]
pub(crate) enum BrowserCommand {
    List,
    Navigate {
        browser_id: String,
        url: String,
    },
    Snapshot {
        browser_id: String,
        max_nodes: Option<usize>,
    },
    Wait {
        browser_id: String,
        snapshot_id: String,
        document_epoch: u64,
        condition: BrowserWaitCondition,
        timeout_ms: Option<u64>,
    },
    Screenshot {
        browser_id: String,
        snapshot_id: String,
        document_epoch: u64,
        max_width: Option<u64>,
        max_height: Option<u64>,
    },
    Act {
        browser_id: String,
        snapshot_id: String,
        document_epoch: u64,
        target_ref: String,
        action: BrowserAction,
    },
}

/// Parse one `tidebreak browser <verb> …` invocation from positional strings.
/// Returns a usage message on failure.
pub(crate) fn parse_browser(args: Vec<String>) -> std::result::Result<BrowserCommand, String> {
    let mut args = args.into_iter();
    let Some(verb) = args.next() else {
        return Err(BROWSER_USAGE.to_string());
    };
    match verb.as_str() {
        "list" => {
            if args.next().is_some() {
                return Err("browser list takes no arguments".to_string());
            }
            Ok(BrowserCommand::List)
        }
        "navigate" => {
            let mut browser_id = None;
            let mut url = None;
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--browser-id" => {
                        if browser_id.is_some() {
                            return Err("duplicate --browser-id".to_string());
                        }
                        let Some(value) = args.next() else {
                            return Err("--browser-id requires a value".to_string());
                        };
                        if value.starts_with("--") {
                            return Err("--browser-id requires a value".to_string());
                        }
                        browser_id = Some(value);
                    }
                    "--url" => {
                        if url.is_some() {
                            return Err("duplicate --url".to_string());
                        }
                        let Some(value) = args.next() else {
                            return Err("--url requires a value".to_string());
                        };
                        if value.starts_with("--") {
                            return Err("--url requires a value".to_string());
                        }
                        url = Some(value);
                    }
                    other => {
                        return Err(format!("unknown browser navigate argument {other:?}"));
                    }
                }
            }
            let Some(browser_id) = browser_id else {
                return Err("browser navigate requires --browser-id".to_string());
            };
            let Some(url) = url else {
                return Err("browser navigate requires --url".to_string());
            };
            Ok(BrowserCommand::Navigate { browser_id, url })
        }
        "snapshot" => {
            let mut browser_id = None;
            let mut max_nodes = None;
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--browser-id" => {
                        if browser_id.is_some() {
                            return Err("duplicate --browser-id".to_string());
                        }
                        let Some(value) = args.next() else {
                            return Err("--browser-id requires a value".to_string());
                        };
                        if value.starts_with("--") {
                            return Err("--browser-id requires a value".to_string());
                        }
                        browser_id = Some(value);
                    }
                    "--max-nodes" => {
                        if max_nodes.is_some() {
                            return Err("duplicate --max-nodes".to_string());
                        }
                        let Some(value) = args.next() else {
                            return Err("--max-nodes requires a value".to_string());
                        };
                        if value.starts_with("--") {
                            return Err("--max-nodes requires a value".to_string());
                        }
                        let n: usize = value.parse().map_err(|_| {
                            format!("--max-nodes expects a positive integer, got {value:?}")
                        })?;
                        max_nodes = Some(n);
                    }
                    other => {
                        return Err(format!("unknown browser snapshot argument {other:?}"));
                    }
                }
            }
            let Some(browser_id) = browser_id else {
                return Err("browser snapshot requires --browser-id".to_string());
            };
            Ok(BrowserCommand::Snapshot {
                browser_id,
                max_nodes,
            })
        }
        "wait" => {
            let mut browser_id = None;
            let mut snapshot_id = None;
            let mut document_epoch = None;
            let mut condition = None;
            let mut timeout_ms = None;
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--browser-id" => {
                        if browser_id.is_some() {
                            return Err("duplicate --browser-id".to_string());
                        }
                        let Some(value) = args.next() else {
                            return Err("--browser-id requires a value".to_string());
                        };
                        if value.starts_with("--") {
                            return Err("--browser-id requires a value".to_string());
                        }
                        browser_id = Some(value);
                    }
                    "--snapshot-id" => {
                        if snapshot_id.is_some() {
                            return Err("duplicate --snapshot-id".to_string());
                        }
                        let Some(value) = args.next() else {
                            return Err("--snapshot-id requires a value".to_string());
                        };
                        if value.starts_with("--") {
                            return Err("--snapshot-id requires a value".to_string());
                        }
                        snapshot_id = Some(value);
                    }
                    "--document-epoch" => {
                        if document_epoch.is_some() {
                            return Err("duplicate --document-epoch".to_string());
                        }
                        let Some(value) = args.next() else {
                            return Err("--document-epoch requires a value".to_string());
                        };
                        if value.starts_with("--") {
                            return Err("--document-epoch requires a value".to_string());
                        }
                        let e: u64 = value.parse().map_err(|_| {
                            format!(
                                "--document-epoch expects a non-negative integer, got {value:?}"
                            )
                        })?;
                        document_epoch = Some(e);
                    }
                    "--timeout-ms" => {
                        if timeout_ms.is_some() {
                            return Err("duplicate --timeout-ms".to_string());
                        }
                        let Some(value) = args.next() else {
                            return Err("--timeout-ms requires a value".to_string());
                        };
                        if value.starts_with("--") {
                            return Err("--timeout-ms requires a value".to_string());
                        }
                        let ms: u64 = value.parse().map_err(|_| {
                            format!("--timeout-ms expects a non-negative integer, got {value:?}")
                        })?;
                        if !(100..=30_000).contains(&ms) {
                            return Err("--timeout-ms must be between 100 and 30000".to_string());
                        }
                        timeout_ms = Some(ms);
                    }
                    "--url-changed" => {
                        if condition.is_some() {
                            return Err("only one wait condition is allowed".to_string());
                        }
                        condition = Some(BrowserWaitCondition::UrlChanged);
                    }
                    "--load-state" => {
                        if condition.is_some() {
                            return Err("only one wait condition is allowed".to_string());
                        }
                        let Some(value) = args.next() else {
                            return Err("--load-state requires idle, loading, or ready".to_string());
                        };
                        if value.starts_with("--") {
                            return Err("--load-state requires idle, loading, or ready".to_string());
                        }
                        let state = match value.as_str() {
                            "idle" => tidebreak_core::BrowserLoadState::Idle,
                            "loading" => tidebreak_core::BrowserLoadState::Loading,
                            "ready" => tidebreak_core::BrowserLoadState::Ready,
                            other => {
                                return Err(format!(
                                    "unknown load state {other:?}: expected idle, loading, or ready"
                                ));
                            }
                        };
                        condition = Some(BrowserWaitCondition::LoadState { state });
                    }
                    "--text-present" => {
                        if condition.is_some() {
                            return Err("only one wait condition is allowed".to_string());
                        }
                        let Some(value) = args.next() else {
                            return Err("--text-present requires a value".to_string());
                        };
                        if value.starts_with("--") {
                            return Err("--text-present requires a value".to_string());
                        }
                        if value.chars().count() > 512 {
                            return Err(
                                "--text-present value must be at most 512 characters".to_string()
                            );
                        }
                        condition = Some(BrowserWaitCondition::TextPresent { text: value });
                    }
                    "--text-absent" => {
                        if condition.is_some() {
                            return Err("only one wait condition is allowed".to_string());
                        }
                        let Some(value) = args.next() else {
                            return Err("--text-absent requires a value".to_string());
                        };
                        if value.starts_with("--") {
                            return Err("--text-absent requires a value".to_string());
                        }
                        if value.chars().count() > 512 {
                            return Err(
                                "--text-absent value must be at most 512 characters".to_string()
                            );
                        }
                        condition = Some(BrowserWaitCondition::TextAbsent { text: value });
                    }
                    other => {
                        return Err(format!("unknown browser wait argument {other:?}"));
                    }
                }
            }
            let Some(browser_id) = browser_id else {
                return Err("browser wait requires --browser-id".to_string());
            };
            let Some(snapshot_id) = snapshot_id else {
                return Err("browser wait requires --snapshot-id".to_string());
            };
            let Some(document_epoch) = document_epoch else {
                return Err("browser wait requires --document-epoch".to_string());
            };
            let Some(condition) = condition else {
                return Err(
                    "a wait condition is required: one of --url-changed, --load-state <idle|loading|ready>, --text-present <text>, or --text-absent <text>"
                        .to_string(),
                );
            };
            Ok(BrowserCommand::Wait {
                browser_id,
                snapshot_id,
                document_epoch,
                condition,
                timeout_ms,
            })
        }
        "screenshot" => {
            let mut browser_id = None;
            let mut snapshot_id = None;
            let mut document_epoch = None;
            let mut max_width = None;
            let mut max_height = None;
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--browser-id" => {
                        if browser_id.is_some() {
                            return Err("duplicate --browser-id".to_string());
                        }
                        let Some(value) = args.next() else {
                            return Err("--browser-id requires a value".to_string());
                        };
                        if value.starts_with("--") {
                            return Err("--browser-id requires a value".to_string());
                        }
                        browser_id = Some(value);
                    }
                    "--snapshot-id" => {
                        if snapshot_id.is_some() {
                            return Err("duplicate --snapshot-id".to_string());
                        }
                        let Some(value) = args.next() else {
                            return Err("--snapshot-id requires a value".to_string());
                        };
                        if value.starts_with("--") {
                            return Err("--snapshot-id requires a value".to_string());
                        }
                        snapshot_id = Some(value);
                    }
                    "--document-epoch" => {
                        if document_epoch.is_some() {
                            return Err("duplicate --document-epoch".to_string());
                        }
                        let Some(value) = args.next() else {
                            return Err("--document-epoch requires a value".to_string());
                        };
                        if value.starts_with("--") {
                            return Err("--document-epoch requires a value".to_string());
                        }
                        let e: u64 = value.parse().map_err(|_| {
                            format!(
                                "--document-epoch expects a non-negative integer, got {value:?}"
                            )
                        })?;
                        document_epoch = Some(e);
                    }
                    "--max-width" => {
                        if max_width.is_some() {
                            return Err("duplicate --max-width".to_string());
                        }
                        let Some(value) = args.next() else {
                            return Err("--max-width requires a value".to_string());
                        };
                        if value.starts_with("--") {
                            return Err("--max-width requires a value".to_string());
                        }
                        let w: u64 = value.parse().map_err(|_| {
                            format!("--max-width expects a non-negative integer, got {value:?}")
                        })?;
                        if !(1..=4_096).contains(&w) {
                            return Err("--max-width must be between 1 and 4096".to_string());
                        }
                        max_width = Some(w);
                    }
                    "--max-height" => {
                        if max_height.is_some() {
                            return Err("duplicate --max-height".to_string());
                        }
                        let Some(value) = args.next() else {
                            return Err("--max-height requires a value".to_string());
                        };
                        if value.starts_with("--") {
                            return Err("--max-height requires a value".to_string());
                        }
                        let h: u64 = value.parse().map_err(|_| {
                            format!("--max-height expects a non-negative integer, got {value:?}")
                        })?;
                        if h > 4_096 {
                            return Err("--max-height must be between 0 and 4096".to_string());
                        }
                        max_height = Some(h);
                    }
                    other => {
                        return Err(format!("unknown browser screenshot argument {other:?}"));
                    }
                }
            }
            let Some(browser_id) = browser_id else {
                return Err("browser screenshot requires --browser-id".to_string());
            };
            let Some(snapshot_id) = snapshot_id else {
                return Err("browser screenshot requires --snapshot-id".to_string());
            };
            let Some(document_epoch) = document_epoch else {
                return Err("browser screenshot requires --document-epoch".to_string());
            };
            Ok(BrowserCommand::Screenshot {
                browser_id,
                snapshot_id,
                document_epoch,
                max_width,
                max_height,
            })
        }
        "act" => parse_browser_act(args),
        other => Err(format!("unknown browser command {other:?}")),
    }
}

fn parse_browser_act(
    mut args: impl Iterator<Item = String>,
) -> std::result::Result<BrowserCommand, String> {
    let mut browser_id = None;
    let mut snapshot_id = None;
    let mut document_epoch = None;
    let mut target_ref = None;
    let mut action = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--browser-id" => parse_string_flag(&mut args, &mut browser_id, "--browser-id")?,
            "--snapshot-id" => parse_string_flag(&mut args, &mut snapshot_id, "--snapshot-id")?,
            "--document-epoch" => {
                if document_epoch.is_some() {
                    return Err("duplicate --document-epoch".to_string());
                }
                let Some(value) = args.next() else {
                    return Err("--document-epoch requires a value".to_string());
                };
                if value.starts_with("--") {
                    return Err("--document-epoch requires a value".to_string());
                }
                document_epoch = Some(value.parse::<u64>().map_err(|_| {
                    format!("--document-epoch expects a non-negative integer, got {value:?}")
                })?);
            }
            "--ref" => parse_string_flag(&mut args, &mut target_ref, "--ref")?,
            "--click" => set_browser_action(&mut action, BrowserAction::Click)?,
            "--focus" => set_browser_action(&mut action, BrowserAction::Focus)?,
            "--hover" => set_browser_action(&mut action, BrowserAction::Hover)?,
            "--fill" => {
                let value = required_flag_value(&mut args, "--fill")?;
                set_browser_action(&mut action, BrowserAction::Fill { value })?;
            }
            "--select" => {
                let value = required_flag_value(&mut args, "--select")?;
                set_browser_action(&mut action, BrowserAction::Select { value })?;
            }
            "--check" => set_browser_action(&mut action, BrowserAction::Check { checked: true })?,
            "--uncheck" => {
                set_browser_action(&mut action, BrowserAction::Check { checked: false })?
            }
            "--press" => {
                let key = required_flag_value(&mut args, "--press")?;
                set_browser_action(&mut action, BrowserAction::Press { key })?;
            }
            "--scroll-into-view" => set_browser_action(&mut action, BrowserAction::ScrollIntoView)?,
            other => return Err(format!("unknown browser act argument {other:?}")),
        }
    }

    let Some(browser_id) = browser_id else {
        return Err("browser act requires --browser-id".to_string());
    };
    let Some(snapshot_id) = snapshot_id else {
        return Err("browser act requires --snapshot-id".to_string());
    };
    let Some(document_epoch) = document_epoch else {
        return Err("browser act requires --document-epoch".to_string());
    };
    let Some(target_ref) = target_ref else {
        return Err("browser act requires --ref".to_string());
    };
    let Some(action) = action else {
        return Err(
            "browser act requires exactly one action: --click, --focus, --hover, --fill, --select, --check, --uncheck, --press, or --scroll-into-view"
                .to_string(),
        );
    };
    let arguments = BrowserActArgs {
        browser_id,
        snapshot_id,
        document_epoch,
        target_ref,
        action,
    };
    if !arguments.is_well_formed() {
        return Err("browser act arguments are not well-formed".to_string());
    }
    let BrowserActArgs {
        browser_id,
        snapshot_id,
        document_epoch,
        target_ref,
        action,
    } = arguments;
    Ok(BrowserCommand::Act {
        browser_id,
        snapshot_id,
        document_epoch,
        target_ref,
        action,
    })
}

fn parse_string_flag(
    args: &mut impl Iterator<Item = String>,
    slot: &mut Option<String>,
    flag: &str,
) -> std::result::Result<(), String> {
    if slot.is_some() {
        return Err(format!("duplicate {flag}"));
    }
    *slot = Some(required_flag_value(args, flag)?);
    Ok(())
}

fn required_flag_value(
    args: &mut impl Iterator<Item = String>,
    flag: &str,
) -> std::result::Result<String, String> {
    let Some(value) = args.next() else {
        return Err(format!("{flag} requires a value"));
    };
    if value.starts_with("--") {
        return Err(format!("{flag} requires a value"));
    }
    Ok(value)
}

fn set_browser_action(
    slot: &mut Option<BrowserAction>,
    action: BrowserAction,
) -> std::result::Result<(), String> {
    if slot.replace(action).is_some() {
        return Err("browser act accepts exactly one action".to_string());
    }
    Ok(())
}

/// Usage text shown for `tidebreak browser` (and `browser-mcp`).
pub(crate) const BROWSER_USAGE: &str = "\
usage: tidebreak browser list --json
       tidebreak browser navigate --browser-id <id> --url <url> --json
       tidebreak browser snapshot --browser-id <id> [--max-nodes <n>] --json
       tidebreak browser wait --browser-id <id> --snapshot-id <id> --document-epoch <n> \
              (--url-changed | --load-state <idle|loading|ready> | \
               --text-present <text> | --text-absent <text>) \
              [--timeout-ms <ms>] --json
       tidebreak browser screenshot --browser-id <id> --snapshot-id <id> \
              --document-epoch <n> [--max-width <px>] [--max-height <px>] --json
       tidebreak browser act --browser-id <id> --snapshot-id <id> \
              --document-epoch <n> --ref <ref> \
              (--click | --focus | --hover | --fill <text> | --select <value> | \
               --check | --uncheck | --press <key> | --scroll-into-view) --json

Browser commands use the session-private capfile named by
TIDEBREAK_BROWSER_CAPFILE. They do not take --server/--attach.";

// ---------------------------------------------------------------------------
// CLI runner
// ---------------------------------------------------------------------------

/// Run a `tidebreak browser …` command.
pub(crate) async fn run_browser(command: BrowserCommand) -> Result<()> {
    let cap = BrowserCapfile::from_env()?;
    let client = BrowserClient::new(&cap)?;
    match command {
        BrowserCommand::List => {
            let result = browser_list(&client)
                .await
                .map_err(|failure| AgentError::msg(failure.redacted_text()))?;
            println!(
                "{}",
                serde_json::to_string(&result)
                    .map_err(|error| AgentError::msg(format!("JSON encode: {error}")))?
            );
            Ok(())
        }
        BrowserCommand::Navigate { browser_id, url } => {
            let args = BrowserNavigateArgs { browser_id, url };
            if !args.is_well_formed() {
                return Err(AgentError::msg(
                    "browser_id or url is not well-formed for navigate",
                ));
            }
            let result = browser_navigate(&client, &args)
                .await
                .map_err(|failure| AgentError::msg(failure.redacted_text()))?;
            println!(
                "{}",
                serde_json::to_string(&result)
                    .map_err(|error| AgentError::msg(format!("JSON encode: {error}")))?
            );
            Ok(())
        }
        BrowserCommand::Snapshot {
            browser_id,
            max_nodes,
        } => {
            let args = BrowserSnapshotArgs {
                browser_id,
                max_nodes,
            };
            if !args.is_well_formed() {
                return Err(AgentError::msg(
                    "browser_id or max_nodes is not well-formed for snapshot",
                ));
            }
            let result = browser_snapshot(&client, &args)
                .await
                .map_err(|failure| AgentError::msg(failure.redacted_text()))?;
            println!(
                "{}",
                serde_json::to_string(&result)
                    .map_err(|error| AgentError::msg(format!("JSON encode: {error}")))?
            );
            Ok(())
        }
        BrowserCommand::Wait {
            browser_id,
            snapshot_id,
            document_epoch,
            condition,
            timeout_ms,
        } => {
            let args = BrowserWaitArgs {
                browser_id,
                snapshot_id,
                document_epoch,
                condition,
                timeout_ms,
            };
            if !args.is_well_formed() {
                return Err(AgentError::msg("wait arguments are not well-formed"));
            }
            let result = browser_wait(&client, &args)
                .await
                .map_err(|failure| AgentError::msg(failure.redacted_text()))?;
            println!(
                "{}",
                serde_json::to_string(&result)
                    .map_err(|error| AgentError::msg(format!("JSON encode: {error}")))?
            );
            Ok(())
        }
        BrowserCommand::Screenshot {
            browser_id,
            snapshot_id,
            document_epoch,
            max_width,
            max_height,
        } => {
            let args = BrowserScreenshotArgs {
                browser_id,
                snapshot_id,
                document_epoch,
                max_width,
                max_height,
            };
            if !args.is_well_formed() {
                return Err(AgentError::msg("screenshot arguments are not well-formed"));
            }
            let result = browser_screenshot(&client, &args)
                .await
                .map_err(|failure| AgentError::msg(failure.redacted_text()))?;
            println!(
                "{}",
                serde_json::to_string(&result)
                    .map_err(|error| AgentError::msg(format!("JSON encode: {error}")))?
            );
            Ok(())
        }
        BrowserCommand::Act {
            browser_id,
            snapshot_id,
            document_epoch,
            target_ref,
            action,
        } => {
            let args = BrowserActArgs {
                browser_id,
                snapshot_id,
                document_epoch,
                target_ref,
                action,
            };
            if !args.is_well_formed() {
                return Err(AgentError::msg("browser act arguments are not well-formed"));
            }
            let result = browser_act(&client, &args)
                .await
                .map_err(|failure| AgentError::msg(failure.redacted_text()))?;
            println!(
                "{}",
                serde_json::to_string(&result)
                    .map_err(|error| AgentError::msg(format!("JSON encode: {error}")))?
            );
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// browser-mcp: stdio MCP server with capability-gated tools
// ---------------------------------------------------------------------------

/// Serve `tidebreak browser-mcp`: load the capfile, construct the client,
/// build a capability-gated registry backed by that client, and run MCP over
/// stdio.
///
/// ## Tool classification
///
/// | Tool               | Class     | Rationale                                                   |
/// |--------------------|-----------|-------------------------------------------------------------|
/// | browser_list       | ReadOnly  | Observes session state; no mutation.                        |
/// | browser_navigate   | Sensitive | Mutates the shared visible browser; the user sees the change.|
/// | browser_snapshot    | ReadOnly | Reads untrusted page data; no side effects.                 |
/// | browser_wait       | ReadOnly  | Polls a deterministic predicate; no mutation.               |
/// | browser_screenshot | ReadOnly  | Captures epoch-bound pixels; no mutation.                   |
/// | browser_act        | Sensitive | Sends trusted native input when the capfile enables it.     |
///
/// ## Authorization
///
/// The registry always contains the five observation and navigation tools.
/// It adds `browser_act` only when the capfile says that the native runtime
/// supports trusted semantic actions. The browser server authorizes every
/// operation independently against its origin-scoped grants.
pub(crate) async fn run_browser_mcp() -> Result<()> {
    let cap = BrowserCapfile::from_env()?;
    let client = BrowserClient::new(&cap)?;
    let tools = Arc::new(browser_tool_registry(&client, cap.semantic_actions));

    // No filesystem workspace: the tools reach the loopback server only.
    let ctx = ToolCtx::without_private_scratch(tidebreak_core::ChatId::new(), None);

    let server =
        tidebreak_mcp::McpServer::new(tools, ctx).with_approval_gate(Arc::new(AutoApproveGate));

    tidebreak_mcp::serve_stdio(server)
        .await
        .map_err(|error| AgentError::msg(format!("MCP stdio error: {error}")))
}

fn browser_tool_registry(client: &BrowserClient, semantic_actions: bool) -> ToolRegistry {
    let mut tools = ToolRegistry::new()
        .with(Box::new(BrowserListTool {
            client: client.clone(),
        }))
        .with(Box::new(BrowserNavigateTool {
            client: client.clone(),
        }))
        .with(Box::new(BrowserSnapshotTool {
            client: client.clone(),
        }))
        .with(Box::new(BrowserWaitTool {
            client: client.clone(),
        }))
        .with(Box::new(BrowserScreenshotTool {
            client: client.clone(),
        }));
    if semantic_actions {
        tools = tools.with(Box::new(BrowserActTool {
            client: client.clone(),
        }));
    }
    tools
}

// ---------------------------------------------------------------------------
// MCP tool implementations — one struct per tool, backed by BrowserClient
// ---------------------------------------------------------------------------

/// Map a [`ClientFailure`] to a [`ToolOutput`] carrying the right
/// [`ToolErrorCategory`] and a concise redacted message.
fn mcp_failure(output: ClientFailure) -> ToolOutput {
    ToolOutput::failed(output.to_tool_error_category(), output.redacted_text())
}

/// [`BROWSER_LIST_TOOL`] as an MCP-registrable [`Tool`].
struct BrowserListTool {
    client: BrowserClient,
}

#[async_trait::async_trait]
impl Tool for BrowserListTool {
    fn spec(&self) -> ToolSpec {
        browser_list_tool_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        if !validate_browser_list_arguments(&args) {
            return Ok(mcp_failure(ClientFailure::InvalidArguments {
                detail: "invalid browser_list arguments".to_string(),
            }));
        }
        match browser_list(&self.client).await {
            Ok(result) => {
                let data = serde_json::to_value(&result).ok();
                let text = format_browser_list_summary(&result);
                Ok(ToolOutput::text(text).with_data(data.unwrap_or(Value::Null)))
            }
            Err(failure) => Ok(mcp_failure(failure)),
        }
    }
}

/// [`BROWSER_NAVIGATE_TOOL`] as an MCP-registrable [`Tool`].
struct BrowserNavigateTool {
    client: BrowserClient,
}

#[async_trait::async_trait]
impl Tool for BrowserNavigateTool {
    fn spec(&self) -> ToolSpec {
        browser_navigate_tool_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        if !validate_browser_navigate_arguments(&args) {
            return Ok(mcp_failure(ClientFailure::InvalidArguments {
                detail: "invalid browser_navigate arguments".to_string(),
            }));
        }
        let parsed: BrowserNavigateArgs = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(_) => {
                return Ok(mcp_failure(ClientFailure::InvalidArguments {
                    detail: "browser_navigate arguments do not match the schema".to_string(),
                }))
            }
        };
        match browser_navigate(&self.client, &parsed).await {
            Ok(result) => {
                let data = serde_json::to_value(&result).ok();
                let text = format!(
                    "Navigated browser {} to {}. Load state: {:?}, epoch: {}.",
                    result.browser_id, result.url, result.load_state, result.document_epoch
                );
                Ok(ToolOutput::text(text).with_data(data.unwrap_or(Value::Null)))
            }
            Err(failure) => Ok(mcp_failure(failure)),
        }
    }
}

/// [`BROWSER_SNAPSHOT_TOOL`] as an MCP-registrable [`Tool`].
struct BrowserSnapshotTool {
    client: BrowserClient,
}

#[async_trait::async_trait]
impl Tool for BrowserSnapshotTool {
    fn spec(&self) -> ToolSpec {
        browser_snapshot_tool_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        if !validate_browser_snapshot_arguments(&args) {
            return Ok(mcp_failure(ClientFailure::InvalidArguments {
                detail: "invalid browser_snapshot arguments".to_string(),
            }));
        }
        let parsed: BrowserSnapshotArgs = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(_) => {
                return Ok(mcp_failure(ClientFailure::InvalidArguments {
                    detail: "browser_snapshot arguments do not match the schema".to_string(),
                }))
            }
        };
        match browser_snapshot(&self.client, &parsed).await {
            Ok(snapshot) => {
                let data = serde_json::to_value(&snapshot).ok();
                let text = format_browser_snapshot_summary(&snapshot);
                Ok(ToolOutput::text(text).with_data(data.unwrap_or(Value::Null)))
            }
            Err(failure) => Ok(mcp_failure(failure)),
        }
    }
}

/// [`BROWSER_WAIT_TOOL`] as an MCP-registrable [`Tool`].
struct BrowserWaitTool {
    client: BrowserClient,
}

#[async_trait::async_trait]
impl Tool for BrowserWaitTool {
    fn spec(&self) -> ToolSpec {
        browser_wait_tool_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        if !validate_browser_wait_arguments(&args) {
            return Ok(mcp_failure(ClientFailure::InvalidArguments {
                detail: "invalid browser_wait arguments".to_string(),
            }));
        }
        let parsed: BrowserWaitArgs = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(_) => {
                return Ok(mcp_failure(ClientFailure::InvalidArguments {
                    detail: "browser_wait arguments do not match the schema".to_string(),
                }))
            }
        };
        match browser_wait(&self.client, &parsed).await {
            Ok(result) => {
                let data = serde_json::to_value(&result).ok();
                let text = format_browser_wait_summary(&result);
                Ok(ToolOutput::text(text).with_data(data.unwrap_or(Value::Null)))
            }
            Err(failure) => Ok(mcp_failure(failure)),
        }
    }
}

/// [`BROWSER_SCREENSHOT_TOOL`] as an MCP-registrable [`Tool`].
///
/// The base-64 payload is decoded, validated, and published as a
/// content-addressed [`ImageRef`] + [`ImageData`] pair. The pixel bytes
/// never enter model-facing text, logs, or data fields.
struct BrowserScreenshotTool {
    client: BrowserClient,
}

#[async_trait::async_trait]
impl Tool for BrowserScreenshotTool {
    fn spec(&self) -> ToolSpec {
        browser_screenshot_tool_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        if !validate_browser_screenshot_arguments(&args) {
            return Ok(mcp_failure(ClientFailure::InvalidArguments {
                detail: "invalid browser_screenshot arguments".to_string(),
            }));
        }
        let parsed: BrowserScreenshotArgs = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(_) => {
                return Ok(mcp_failure(ClientFailure::InvalidArguments {
                    detail: "browser_screenshot arguments do not match the schema".to_string(),
                }))
            }
        };
        match browser_screenshot(&self.client, &parsed).await {
            Ok(result) => Ok(screenshot_tool_output(&result).unwrap_or_else(mcp_failure)),
            Err(failure) => Ok(mcp_failure(failure)),
        }
    }
}

/// [`tidebreak_core::BROWSER_ACT_TOOL`] as an MCP-registrable [`Tool`].
struct BrowserActTool {
    client: BrowserClient,
}

#[async_trait::async_trait]
impl Tool for BrowserActTool {
    fn spec(&self) -> ToolSpec {
        browser_act_tool_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        if !validate_browser_act_arguments(&args) {
            return Ok(mcp_failure(ClientFailure::InvalidArguments {
                detail: "invalid browser_act arguments".to_string(),
            }));
        }
        let parsed: BrowserActArgs = match serde_json::from_value(args) {
            Ok(value) => value,
            Err(_) => {
                return Ok(mcp_failure(ClientFailure::InvalidArguments {
                    detail: "browser_act arguments do not match the schema".to_string(),
                }))
            }
        };
        match browser_act(&self.client, &parsed).await {
            Ok(result) => {
                let data = serde_json::to_value(&result).unwrap_or(Value::Null);
                let text = format!(
                    "Browser action {} returned {:?}. {}",
                    result.action, result.status, result.message
                );
                Ok(ToolOutput::text(text).with_data(data))
            }
            Err(failure) => Ok(mcp_failure(failure)),
        }
    }
}

// -- helper functions --

/// Build a concise model-readable summary of a page snapshot.
fn format_browser_snapshot_summary(snapshot: &BrowserPageSnapshot) -> String {
    let truncated = if snapshot.truncated {
        " (truncated)"
    } else {
        ""
    };
    format!(
        "Snapshot of {} ({}). {} nodes, {} frames, epoch {}{}. Title: \"{}\".",
        snapshot.url,
        snapshot.browser_id,
        snapshot.nodes.len(),
        snapshot.frames.len(),
        snapshot.document_epoch,
        truncated,
        snapshot.title
    )
}

/// Build a concise model-readable summary of a browser list result.
fn format_browser_list_summary(result: &BrowserListResult) -> String {
    if result.sessions.is_empty() {
        return "No browser sessions available.".to_string();
    }
    let lines: Vec<String> = result
        .sessions
        .iter()
        .map(|session| {
            let url = session.url.as_deref().unwrap_or("(no url)");
            let title = session.title.as_deref().unwrap_or("(no title)");
            format!(
                "browser {}: {:?} at {} — \"{}\"",
                session.browser_id, session.load_state, url, title
            )
        })
        .collect();
    format!("{} browser session(s):\n{}", lines.len(), lines.join("\n"))
}

/// Build a concise model-readable summary of a wait result.
fn format_browser_wait_summary(result: &BrowserWaitResult) -> String {
    format!(
        "Wait {:?} on browser {} (epoch {}): {}.",
        result.status, result.browser_id, result.document_epoch, result.message
    )
}

/// Build a concise model-readable summary of a screenshot capture.
/// The text mentions dimensions and epoch; pixel bytes are in the
/// accompanying [`ImageRef`] / [`ImageData`] pair only.
fn format_screenshot_summary(result: &BrowserScreenshotResult, image: &ImageRef) -> String {
    format!(
        "Screenshot of browser {} captured at epoch {} (snapshot {}): {}×{}, {:.1} kiB.",
        result.browser_id,
        result.document_epoch,
        result.snapshot_id,
        image.width,
        image.height,
        image.byte_len as f64 / 1024.0,
    )
}

/// Convert one server screenshot response into model text plus an out-of-band
/// image attachment. No structured payload is added because it would carry the
/// server's base-64 field into journals and model-facing tool data.
fn screenshot_tool_output(
    result: &BrowserScreenshotResult,
) -> std::result::Result<ToolOutput, ClientFailure> {
    let (image_ref, image_data) = decode_screenshot_image(result)?;
    let text = format_screenshot_summary(result, &image_ref);
    Ok(ToolOutput::text(text).with_images([(image_ref, image_data)]))
}

/// Decode and validate the base-64 screenshot payload, returning a
/// content-addressed [`ImageRef`] + [`ImageData`] pair.
///
/// The base-64 bytes never appear in text, logs, errors, or structured
/// data. Only identity, dimensions, and the opaque [`ImageData`] pixels
/// escape this function.
fn decode_screenshot_image(
    result: &BrowserScreenshotResult,
) -> std::result::Result<(ImageRef, ImageData), ClientFailure> {
    use base64::Engine as _;

    if result.mime_type != "image/png" {
        return Err(ClientFailure::ToolFailed {
            detail: format!(
                "screenshot mime type must be image/png, got {}",
                result.mime_type
            ),
        });
    }
    if result.image_base64.len() > MAX_SCREENSHOT_BASE64_CHARS {
        return Err(ClientFailure::ToolFailed {
            detail: "screenshot image exceeds the maximum size".to_string(),
        });
    }

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&result.image_base64)
        .map_err(|error| ClientFailure::ToolFailed {
            detail: format!("screenshot base-64 decode failed: {error}"),
        })?;

    if bytes.is_empty() {
        return Err(ClientFailure::ToolFailed {
            detail: "screenshot image is empty".to_string(),
        });
    }

    // Sniff the magic bytes. Reject anything that is not actually PNG.
    let format = image::guess_format(&bytes).map_err(|_| ClientFailure::ToolFailed {
        detail: "screenshot bytes are not a recognized image".to_string(),
    })?;
    if format != image::ImageFormat::Png {
        return Err(ClientFailure::ToolFailed {
            detail: "screenshot bytes are not PNG".to_string(),
        });
    }

    // Read dimensions from the PNG header without decoding pixels.
    let (width, height) =
        image::ImageReader::with_format(std::io::Cursor::new(&bytes), image::ImageFormat::Png)
            .into_dimensions()
            .map_err(|_| ClientFailure::ToolFailed {
                detail: "screenshot PNG header could not be read".to_string(),
            })?;

    if width == 0 || height == 0 {
        return Err(ClientFailure::ToolFailed {
            detail: "screenshot has a zero dimension".to_string(),
        });
    }
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(ClientFailure::ToolFailed {
            detail: format!(
                "screenshot dimensions {}×{} exceed the maximum {MAX_IMAGE_DIMENSION}",
                width, height
            ),
        });
    }

    let byte_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if byte_len > MAX_IMAGE_BYTES {
        return Err(ClientFailure::ToolFailed {
            detail: format!("screenshot size {byte_len} exceeds the maximum {MAX_IMAGE_BYTES}"),
        });
    }

    // Build the content-addressed identity. Use DocumentBlob for the
    // canonical v5 UUID → byte mapping every Tidebreak image path shares.
    let blob = DocumentBlob::from_bytes(&bytes);
    let image_ref = ImageRef {
        blob_id: blob.id,
        media_type: ImageMediaType::Png,
        width,
        height,
        byte_len,
    };
    image_ref
        .validate()
        .map_err(|reason| ClientFailure::ToolFailed {
            detail: format!("screenshot image is invalid: {reason}"),
        })?;

    Ok((image_ref, ImageData::new(ImageMediaType::Png, bytes)))
}

#[cfg(test)]
mod tests;
