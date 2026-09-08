//! Tidebreak browser CLI commands and the `browser-mcp` stdio server.
//!
//! `tidebreak browser list|navigate|snapshot|wait|screenshot|act --json` drive a
//! running Tidebreak browser server through the session-private capability
//! file named by `TIDEBREAK_BROWSER_CAPFILE`. Every operation runs through a
//! single [`BrowserClient`] that is shared between the direct CLI and the MCP
//! tool implementations.
//!
//! `tidebreak browser-mcp` serves list, navigate, snapshot, wait, and screenshot
//! tools over MCP stdio. It also serves `browser_act` when the native runtime
//! supports semantic actions, `browser_open`/`browser_close`/`browser_activate`
//! when it supports agent tab lifecycle, and `browser_diagnostics` when it can
//! surface page diagnostics. All tools use the canonical core tool specs and
//! validate typed arguments before sending them to the browser server.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tidebreak_core::{
    browser_act_tool_spec, browser_activate_tool_spec, browser_close_tool_spec,
    browser_diagnostics_tool_spec, browser_list_tool_spec, browser_navigate_tool_spec,
    browser_open_tool_spec, browser_screenshot_tool_spec, browser_snapshot_tool_spec,
    browser_wait_tool_spec, validate_browser_act_arguments, validate_browser_activate_arguments,
    validate_browser_close_arguments, validate_browser_diagnostics_arguments,
    validate_browser_list_arguments, validate_browser_navigate_arguments,
    validate_browser_open_arguments, validate_browser_screenshot_arguments,
    validate_browser_snapshot_arguments, validate_browser_wait_arguments, AgentError,
    ApprovalClass, AutoApproveGate, BrowserActArgs, BrowserActResult, BrowserAction,
    BrowserActivateArgs, BrowserCloseArgs, BrowserDiagnosticsArgs, BrowserDiagnosticsResult,
    BrowserLifecycleResult, BrowserListResult, BrowserNavigateArgs, BrowserNavigateResult,
    BrowserOpenArgs, BrowserOpenResult, BrowserPageSnapshot, BrowserScreenshotArgs,
    BrowserScreenshotResult, BrowserSnapshotArgs, BrowserWaitArgs, BrowserWaitCondition,
    BrowserWaitResult, DocumentBlob, ImageData, ImageMediaType, ImageRef, Result, Tool, ToolCtx,
    ToolErrorCategory, ToolOutput, ToolRegistry, ToolSpec,
    MAX_BROWSER_SCREENSHOT_IMAGE_BLOCK_BYTES, MAX_IMAGE_BYTES, MAX_IMAGE_DIMENSION,
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

/// Ceiling on a lifecycle (open/close/activate) response body.
const LIFECYCLE_BODY_MAX_BYTES: usize = 64 * 1024; // 64 KiB

/// Ceiling on a diagnostics response body: 200 bounded entries fit well
/// within this.
const DIAGNOSTICS_BODY_MAX_BYTES: usize = 1024 * 1024; // 1 MiB

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
    lifecycle: bool,
    developer_diagnostics: bool,
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
    /// Whether the runtime can open, close, and activate tabs. Absent on
    /// servers that predate lifecycle support.
    #[serde(default)]
    lifecycle: bool,
    /// Whether the runtime can surface page diagnostics. Absent on servers
    /// that predate diagnostics support.
    #[serde(default)]
    developer_diagnostics: bool,
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
            lifecycle: wire.lifecycle,
            developer_diagnostics: wire.developer_diagnostics,
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

async fn browser_open(
    client: &BrowserClient,
    args: &BrowserOpenArgs,
) -> std::result::Result<BrowserOpenResult, ClientFailure> {
    let response = client
        .client
        .post(format!("{}/open", client.endpoint))
        .bearer_auth(&client.token)
        .json(args)
        .send()
        .await
        .map_err(|error| ClientFailure::TransportFailed {
            detail: format!("browser open request failed: {error}"),
        })?;
    BrowserClient::read_bounded_json(response, LIFECYCLE_BODY_MAX_BYTES).await
}

async fn browser_close(
    client: &BrowserClient,
    args: &BrowserCloseArgs,
) -> std::result::Result<BrowserLifecycleResult, ClientFailure> {
    let response = client
        .client
        .post(format!("{}/close", client.endpoint))
        .bearer_auth(&client.token)
        .json(args)
        .send()
        .await
        .map_err(|error| ClientFailure::TransportFailed {
            detail: format!("browser close request failed: {error}"),
        })?;
    BrowserClient::read_bounded_json(response, LIFECYCLE_BODY_MAX_BYTES).await
}

async fn browser_activate(
    client: &BrowserClient,
    args: &BrowserActivateArgs,
) -> std::result::Result<BrowserLifecycleResult, ClientFailure> {
    let response = client
        .client
        .post(format!("{}/activate", client.endpoint))
        .bearer_auth(&client.token)
        .json(args)
        .send()
        .await
        .map_err(|error| ClientFailure::TransportFailed {
            detail: format!("browser activate request failed: {error}"),
        })?;
    BrowserClient::read_bounded_json(response, LIFECYCLE_BODY_MAX_BYTES).await
}

async fn browser_diagnostics(
    client: &BrowserClient,
    args: &BrowserDiagnosticsArgs,
) -> std::result::Result<BrowserDiagnosticsResult, ClientFailure> {
    let response = client
        .client
        .post(format!("{}/diagnostics", client.endpoint))
        .bearer_auth(&client.token)
        .json(args)
        .send()
        .await
        .map_err(|error| ClientFailure::TransportFailed {
            detail: format!("browser diagnostics request failed: {error}"),
        })?;
    BrowserClient::read_bounded_json(response, DIAGNOSTICS_BODY_MAX_BYTES).await
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
        /// Write the decoded image privately to this path instead of
        /// printing base-64 pixels on stdout.
        output: Option<PathBuf>,
    },
    Act {
        browser_id: String,
        snapshot_id: String,
        document_epoch: u64,
        target_ref: String,
        action: BrowserAction,
        execution_mode: tidebreak_core::BrowserExecutionMode,
    },
    Open {
        url: String,
    },
    Close {
        browser_id: String,
    },
    Activate {
        browser_id: String,
    },
    Diagnostics {
        browser_id: String,
        after_sequence: Option<u64>,
        max_entries: Option<usize>,
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
            let mut output = None;
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
                    "--output" => {
                        if output.is_some() {
                            return Err("duplicate --output".to_string());
                        }
                        let Some(value) = args.next() else {
                            return Err("--output requires a path".to_string());
                        };
                        if value.starts_with("--") {
                            return Err("--output requires a path".to_string());
                        }
                        output = Some(PathBuf::from(value));
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
                output,
            })
        }
        "act" => parse_browser_act(args),
        "open" => {
            let mut url = None;
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--url" => parse_string_flag(&mut args, &mut url, "--url")?,
                    other => return Err(format!("unknown browser open argument {other:?}")),
                }
            }
            let Some(url) = url else {
                return Err("browser open requires --url".to_string());
            };
            Ok(BrowserCommand::Open { url })
        }
        "close" => {
            let mut browser_id = None;
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--browser-id" => {
                        parse_string_flag(&mut args, &mut browser_id, "--browser-id")?
                    }
                    other => return Err(format!("unknown browser close argument {other:?}")),
                }
            }
            let Some(browser_id) = browser_id else {
                return Err("browser close requires --browser-id".to_string());
            };
            Ok(BrowserCommand::Close { browser_id })
        }
        "activate" => {
            let mut browser_id = None;
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--browser-id" => {
                        parse_string_flag(&mut args, &mut browser_id, "--browser-id")?
                    }
                    other => return Err(format!("unknown browser activate argument {other:?}")),
                }
            }
            let Some(browser_id) = browser_id else {
                return Err("browser activate requires --browser-id".to_string());
            };
            Ok(BrowserCommand::Activate { browser_id })
        }
        "diagnostics" => {
            let mut browser_id = None;
            let mut after_sequence = None;
            let mut max_entries = None;
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--browser-id" => {
                        parse_string_flag(&mut args, &mut browser_id, "--browser-id")?
                    }
                    "--after-sequence" => {
                        if after_sequence.is_some() {
                            return Err("duplicate --after-sequence".to_string());
                        }
                        let value = required_flag_value(&mut args, "--after-sequence")?;
                        after_sequence = Some(value.parse::<u64>().map_err(|_| {
                            format!(
                                "--after-sequence expects a non-negative integer, got {value:?}"
                            )
                        })?);
                    }
                    "--max-entries" => {
                        if max_entries.is_some() {
                            return Err("duplicate --max-entries".to_string());
                        }
                        let value = required_flag_value(&mut args, "--max-entries")?;
                        let entries = value.parse::<usize>().map_err(|_| {
                            format!("--max-entries expects a positive integer, got {value:?}")
                        })?;
                        if !(1..=200).contains(&entries) {
                            return Err("--max-entries must be between 1 and 200".to_string());
                        }
                        max_entries = Some(entries);
                    }
                    other => return Err(format!("unknown browser diagnostics argument {other:?}")),
                }
            }
            let Some(browser_id) = browser_id else {
                return Err("browser diagnostics requires --browser-id".to_string());
            };
            Ok(BrowserCommand::Diagnostics {
                browser_id,
                after_sequence,
                max_entries,
            })
        }
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
    let mut execution_mode = None;

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
            "--execution-mode" => {
                if execution_mode.is_some() {
                    return Err("duplicate --execution-mode".into());
                }
                execution_mode = Some(
                    match required_flag_value(&mut args, "--execution-mode")?.as_str() {
                        "background" => tidebreak_core::BrowserExecutionMode::Background,
                        "foreground" => tidebreak_core::BrowserExecutionMode::Foreground,
                        _ => return Err("--execution-mode expects background or foreground".into()),
                    },
                );
            }
            "--click" => set_browser_action(&mut action, BrowserAction::Click { at: None })?,
            "--focus" => set_browser_action(&mut action, BrowserAction::Focus)?,
            "--hover" => set_browser_action(&mut action, BrowserAction::Hover { at: None })?,
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
    let execution_mode = execution_mode.unwrap_or_default();
    let arguments = BrowserActArgs {
        browser_id,
        snapshot_id,
        document_epoch,
        target_ref,
        action,
        execution_mode,
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
        execution_mode,
    } = arguments;
    Ok(BrowserCommand::Act {
        browser_id,
        snapshot_id,
        document_epoch,
        target_ref,
        action,
        execution_mode,
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
              --document-epoch <n> [--max-width <px>] [--max-height <px>] \
              [--output <path>] --json
       tidebreak browser act --browser-id <id> --snapshot-id <id> \
              --document-epoch <n> --ref <ref> \
              (--click | --focus | --hover | --fill <text> | --select <value> | \
               --check | --uncheck | --press <key> | --scroll-into-view) \
              [--execution-mode <background|foreground>] --json
       tidebreak browser open --url <url> --json
       tidebreak browser close --browser-id <id> --json
       tidebreak browser activate --browser-id <id> --json
       tidebreak browser diagnostics --browser-id <id> \
              [--after-sequence <n>] [--max-entries <n>] --json

With --output, screenshot writes the decoded image to the given path with
private permissions and prints JSON without base-64 pixels.

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
            output,
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
            match output {
                Some(path) => {
                    let receipt = write_screenshot_output(&result, &path)
                        .map_err(|failure| AgentError::msg(failure.redacted_text()))?;
                    println!(
                        "{}",
                        serde_json::to_string(&receipt)
                            .map_err(|error| AgentError::msg(format!("JSON encode: {error}")))?
                    );
                }
                None => {
                    println!(
                        "{}",
                        serde_json::to_string(&result)
                            .map_err(|error| AgentError::msg(format!("JSON encode: {error}")))?
                    );
                }
            }
            Ok(())
        }
        BrowserCommand::Act {
            browser_id,
            snapshot_id,
            document_epoch,
            target_ref,
            action,
            execution_mode,
        } => {
            let args = BrowserActArgs {
                browser_id,
                snapshot_id,
                document_epoch,
                target_ref,
                action,
                execution_mode,
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
        BrowserCommand::Open { url } => {
            let args = BrowserOpenArgs { url };
            if !args.is_well_formed() {
                return Err(AgentError::msg("browser open url is not well-formed"));
            }
            let result = browser_open(&client, &args)
                .await
                .map_err(|failure| AgentError::msg(failure.redacted_text()))?;
            println!(
                "{}",
                serde_json::to_string(&result)
                    .map_err(|error| AgentError::msg(format!("JSON encode: {error}")))?
            );
            Ok(())
        }
        BrowserCommand::Close { browser_id } => {
            let args = BrowserCloseArgs { browser_id };
            if !args.is_well_formed() {
                return Err(AgentError::msg(
                    "browser close arguments are not well-formed",
                ));
            }
            let result = browser_close(&client, &args)
                .await
                .map_err(|failure| AgentError::msg(failure.redacted_text()))?;
            println!(
                "{}",
                serde_json::to_string(&result)
                    .map_err(|error| AgentError::msg(format!("JSON encode: {error}")))?
            );
            Ok(())
        }
        BrowserCommand::Activate { browser_id } => {
            let args = BrowserActivateArgs { browser_id };
            if !args.is_well_formed() {
                return Err(AgentError::msg(
                    "browser activate arguments are not well-formed",
                ));
            }
            let result = browser_activate(&client, &args)
                .await
                .map_err(|failure| AgentError::msg(failure.redacted_text()))?;
            println!(
                "{}",
                serde_json::to_string(&result)
                    .map_err(|error| AgentError::msg(format!("JSON encode: {error}")))?
            );
            Ok(())
        }
        BrowserCommand::Diagnostics {
            browser_id,
            after_sequence,
            max_entries,
        } => {
            let args = BrowserDiagnosticsArgs {
                browser_id,
                after_sequence,
                max_entries,
            };
            if !args.is_well_formed() {
                return Err(AgentError::msg(
                    "browser diagnostics arguments are not well-formed",
                ));
            }
            let result = browser_diagnostics(&client, &args)
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
    let tools = Arc::new(browser_tool_registry(
        &client,
        BrowserToolCapabilities::from_capfile(&cap),
    ));

    // No filesystem workspace: the tools reach the loopback server only.
    let ctx = ToolCtx::without_private_scratch(tidebreak_core::SessionId::new(), None);

    let server =
        tidebreak_mcp::McpServer::new(tools, ctx).with_approval_gate(Arc::new(AutoApproveGate));

    tidebreak_mcp::serve_stdio(server)
        .await
        .map_err(|error| AgentError::msg(format!("MCP stdio error: {error}")))
}

/// Capability flags that decide which optional MCP tools register. Every
/// flag comes from the trusted capfile, which the server derived from the
/// actual native runtime — the registry never advertises a tool the runtime
/// reported it cannot serve.
#[derive(Clone, Copy, Default)]
struct BrowserToolCapabilities {
    semantic_actions: bool,
    lifecycle: bool,
    developer_diagnostics: bool,
}

impl BrowserToolCapabilities {
    fn from_capfile(cap: &BrowserCapfile) -> Self {
        Self {
            semantic_actions: cap.semantic_actions,
            lifecycle: cap.lifecycle,
            developer_diagnostics: cap.developer_diagnostics,
        }
    }
}

fn browser_tool_registry(
    client: &BrowserClient,
    capabilities: BrowserToolCapabilities,
) -> ToolRegistry {
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
    if capabilities.semantic_actions {
        tools = tools.with(Box::new(BrowserActTool {
            client: client.clone(),
        }));
    }
    if capabilities.lifecycle {
        tools = tools
            .with(Box::new(BrowserOpenTool {
                client: client.clone(),
            }))
            .with(Box::new(BrowserCloseTool {
                client: client.clone(),
            }))
            .with(Box::new(BrowserActivateTool {
                client: client.clone(),
            }));
    }
    if capabilities.developer_diagnostics {
        tools = tools.with(Box::new(BrowserDiagnosticsTool {
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
                Ok(browser_result_output(text, data.unwrap_or(Value::Null)))
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
                Ok(browser_result_output(text, data.unwrap_or(Value::Null)))
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
                Ok(browser_result_output(text, data.unwrap_or(Value::Null)))
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
                Ok(browser_result_output(text, data.unwrap_or(Value::Null)))
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
                Ok(browser_result_output(text, data))
            }
            Err(failure) => Ok(mcp_failure(failure)),
        }
    }
}

/// [`tidebreak_core::BROWSER_OPEN_TOOL`] as an MCP-registrable [`Tool`].
struct BrowserOpenTool {
    client: BrowserClient,
}

#[async_trait::async_trait]
impl Tool for BrowserOpenTool {
    fn spec(&self) -> ToolSpec {
        browser_open_tool_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        if !validate_browser_open_arguments(&args) {
            return Ok(mcp_failure(ClientFailure::InvalidArguments {
                detail: "invalid browser_open arguments".to_string(),
            }));
        }
        let parsed: BrowserOpenArgs = match serde_json::from_value(args) {
            Ok(value) => value,
            Err(_) => {
                return Ok(mcp_failure(ClientFailure::InvalidArguments {
                    detail: "browser_open arguments do not match the schema".to_string(),
                }))
            }
        };
        match browser_open(&self.client, &parsed).await {
            Ok(result) => {
                let data = serde_json::to_value(&result).unwrap_or(Value::Null);
                let text = format!(
                    "Opened browser {} at {}. Load state: {:?}, epoch: {}.",
                    result.browser_id, result.url, result.load_state, result.document_epoch
                );
                Ok(browser_result_output(text, data))
            }
            Err(failure) => Ok(mcp_failure(failure)),
        }
    }
}

/// [`tidebreak_core::BROWSER_CLOSE_TOOL`] as an MCP-registrable [`Tool`].
struct BrowserCloseTool {
    client: BrowserClient,
}

#[async_trait::async_trait]
impl Tool for BrowserCloseTool {
    fn spec(&self) -> ToolSpec {
        browser_close_tool_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        if !validate_browser_close_arguments(&args) {
            return Ok(mcp_failure(ClientFailure::InvalidArguments {
                detail: "invalid browser_close arguments".to_string(),
            }));
        }
        let parsed: BrowserCloseArgs = match serde_json::from_value(args) {
            Ok(value) => value,
            Err(_) => {
                return Ok(mcp_failure(ClientFailure::InvalidArguments {
                    detail: "browser_close arguments do not match the schema".to_string(),
                }))
            }
        };
        match browser_close(&self.client, &parsed).await {
            Ok(result) => {
                let data = serde_json::to_value(&result).unwrap_or(Value::Null);
                let text = format_lifecycle_summary("close", &result);
                Ok(browser_result_output(text, data))
            }
            Err(failure) => Ok(mcp_failure(failure)),
        }
    }
}

/// [`tidebreak_core::BROWSER_ACTIVATE_TOOL`] as an MCP-registrable [`Tool`].
struct BrowserActivateTool {
    client: BrowserClient,
}

#[async_trait::async_trait]
impl Tool for BrowserActivateTool {
    fn spec(&self) -> ToolSpec {
        browser_activate_tool_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        if !validate_browser_activate_arguments(&args) {
            return Ok(mcp_failure(ClientFailure::InvalidArguments {
                detail: "invalid browser_activate arguments".to_string(),
            }));
        }
        let parsed: BrowserActivateArgs = match serde_json::from_value(args) {
            Ok(value) => value,
            Err(_) => {
                return Ok(mcp_failure(ClientFailure::InvalidArguments {
                    detail: "browser_activate arguments do not match the schema".to_string(),
                }))
            }
        };
        match browser_activate(&self.client, &parsed).await {
            Ok(result) => {
                let data = serde_json::to_value(&result).unwrap_or(Value::Null);
                let text = format_lifecycle_summary("activate", &result);
                Ok(browser_result_output(text, data))
            }
            Err(failure) => Ok(mcp_failure(failure)),
        }
    }
}

/// [`tidebreak_core::BROWSER_DIAGNOSTICS_TOOL`] as an MCP-registrable [`Tool`].
struct BrowserDiagnosticsTool {
    client: BrowserClient,
}

#[async_trait::async_trait]
impl Tool for BrowserDiagnosticsTool {
    fn spec(&self) -> ToolSpec {
        browser_diagnostics_tool_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        if !validate_browser_diagnostics_arguments(&args) {
            return Ok(mcp_failure(ClientFailure::InvalidArguments {
                detail: "invalid browser_diagnostics arguments".to_string(),
            }));
        }
        let parsed: BrowserDiagnosticsArgs = match serde_json::from_value(args) {
            Ok(value) => value,
            Err(_) => {
                return Ok(mcp_failure(ClientFailure::InvalidArguments {
                    detail: "browser_diagnostics arguments do not match the schema".to_string(),
                }))
            }
        };
        match browser_diagnostics(&self.client, &parsed).await {
            Ok(result) => {
                let data = serde_json::to_value(&result).unwrap_or(Value::Null);
                let text = format_diagnostics_summary(&result);
                Ok(browser_result_output(text, data))
            }
            Err(failure) => Ok(mcp_failure(failure)),
        }
    }
}

// -- helper functions --

/// Keep the complete, bounded result available to MCP hosts that forward only
/// text content to their model. Snapshot identities and refs must reach the
/// model even when the host does not consume `structuredContent`. Screenshots
/// use a separate path so their base-64 pixels never enter text.
fn browser_result_output(summary: String, data: Value) -> ToolOutput {
    ToolOutput::text(format!("{summary}\n\n{data}")).with_data(data)
}

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

/// Build a concise model-readable summary of a lifecycle result.
fn format_lifecycle_summary(operation: &str, result: &BrowserLifecycleResult) -> String {
    format!(
        "Browser {} {:?} on {}: {}",
        operation, result.status, result.browser_id, result.message
    )
}

/// Build a concise model-readable summary of a diagnostics read. Entry text
/// is untrusted page data and stays in the structured payload only.
fn format_diagnostics_summary(result: &BrowserDiagnosticsResult) -> String {
    format!(
        "Diagnostics for browser {} (epoch {}): {} entries{}{}.",
        result.browser_id,
        result.document_epoch,
        result.entries.len(),
        if result.truncated { ", truncated" } else { "" },
        if result.network_captured {
            ", network captured"
        } else {
            ", network not captured by this engine"
        },
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

/// JSON receipt printed by `browser screenshot --output`: everything about
/// the capture except the pixels, which live only in the private file.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ScreenshotFileReceipt {
    browser_id: String,
    snapshot_id: String,
    document_epoch: u64,
    mime_type: String,
    path: String,
    byte_len: u64,
    width: u32,
    height: u32,
}

/// Decode, validate, and budget-fit the screenshot, then write the image
/// bytes to `path` with private permissions. The written bytes are exactly
/// the bytes a model-facing image block would carry, so a harness reading
/// the file sees the same pixels the model would.
fn write_screenshot_output(
    result: &BrowserScreenshotResult,
    path: &std::path::Path,
) -> std::result::Result<ScreenshotFileReceipt, ClientFailure> {
    use std::io::Write as _;

    let fitted = decode_and_fit_screenshot(result)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| ClientFailure::ToolFailed {
            detail: format!("could not open screenshot output file: {error}"),
        })?;
    file.write_all(&fitted.bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| ClientFailure::ToolFailed {
            detail: format!("could not write screenshot output file: {error}"),
        })?;
    Ok(ScreenshotFileReceipt {
        browser_id: result.browser_id.clone(),
        snapshot_id: result.snapshot_id.clone(),
        document_epoch: result.document_epoch,
        mime_type: fitted.media_type.as_str().to_owned(),
        path: path.display().to_string(),
        byte_len: fitted.bytes.len() as u64,
        width: fitted.width,
        height: fitted.height,
    })
}

/// A decoded, validated, budget-fitted screenshot image.
struct FittedScreenshot {
    bytes: Vec<u8>,
    media_type: ImageMediaType,
    width: u32,
    height: u32,
}

/// Decode and validate the base-64 screenshot payload and fit it within the
/// model-facing image-block budget, returning a content-addressed
/// [`ImageRef`] + [`ImageData`] pair.
///
/// The base-64 bytes never appear in text, logs, errors, or structured
/// data. Only identity, dimensions, and the opaque [`ImageData`] pixels
/// escape this function.
fn decode_screenshot_image(
    result: &BrowserScreenshotResult,
) -> std::result::Result<(ImageRef, ImageData), ClientFailure> {
    let fitted = decode_and_fit_screenshot(result)?;
    let blob = DocumentBlob::from_bytes(&fitted.bytes);
    let image_ref = ImageRef {
        blob_id: blob.id,
        media_type: fitted.media_type,
        width: fitted.width,
        height: fitted.height,
        byte_len: fitted.bytes.len() as u64,
    };
    image_ref
        .validate()
        .map_err(|reason| ClientFailure::ToolFailed {
            detail: format!("screenshot image is invalid: {reason}"),
        })?;
    Ok((image_ref, ImageData::new(fitted.media_type, fitted.bytes)))
}

fn decode_and_fit_screenshot(
    result: &BrowserScreenshotResult,
) -> std::result::Result<FittedScreenshot, ClientFailure> {
    use base64::Engine as _;

    let expected_format = match result.mime_type.as_str() {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        other => {
            return Err(ClientFailure::ToolFailed {
                detail: format!(
                    "screenshot mime type must be image/png or image/jpeg, got {other}"
                ),
            });
        }
    };
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

    // Sniff the magic bytes. Reject anything that does not match the
    // declared mime type.
    let format = image::guess_format(&bytes).map_err(|_| ClientFailure::ToolFailed {
        detail: "screenshot bytes are not a recognized image".to_string(),
    })?;
    if format != expected_format {
        return Err(ClientFailure::ToolFailed {
            detail: "screenshot bytes do not match the declared mime type".to_string(),
        });
    }

    // Read dimensions from the header without decoding pixels.
    let (width, height) = image::ImageReader::with_format(std::io::Cursor::new(&bytes), format)
        .into_dimensions()
        .map_err(|_| ClientFailure::ToolFailed {
            detail: "screenshot image header could not be read".to_string(),
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
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(ClientFailure::ToolFailed {
            detail: format!(
                "screenshot size {} exceeds the maximum {MAX_IMAGE_BYTES}",
                bytes.len()
            ),
        });
    }

    let media_type = match expected_format {
        image::ImageFormat::Jpeg => ImageMediaType::Jpeg,
        _ => ImageMediaType::Png,
    };
    if bytes.len() <= MAX_BROWSER_SCREENSHOT_IMAGE_BLOCK_BYTES {
        return Ok(FittedScreenshot {
            bytes,
            media_type,
            width,
            height,
        });
    }
    fit_screenshot_to_image_block(&bytes, format)
}

/// Re-encode an oversized screenshot until it fits the model-facing
/// image-block budget: first as progressively stronger JPEG, then at halved
/// resolutions. Failing to fit is reported, never silently dropped.
fn fit_screenshot_to_image_block(
    bytes: &[u8],
    format: image::ImageFormat,
) -> std::result::Result<FittedScreenshot, ClientFailure> {
    let decoded = image::load_from_memory_with_format(bytes, format).map_err(|_| {
        ClientFailure::ToolFailed {
            detail: "screenshot image could not be decoded for re-encoding".to_string(),
        }
    })?;
    let mut current = decoded;
    for _ in 0..4 {
        for quality in [85u8, 70, 55, 40] {
            let mut encoded = Vec::new();
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
                std::io::Cursor::new(&mut encoded),
                quality,
            );
            if current.to_rgb8().write_with_encoder(encoder).is_err() {
                continue;
            }
            if encoded.len() <= MAX_BROWSER_SCREENSHOT_IMAGE_BLOCK_BYTES {
                return Ok(FittedScreenshot {
                    width: current.width(),
                    height: current.height(),
                    bytes: encoded,
                    media_type: ImageMediaType::Jpeg,
                });
            }
        }
        let (width, height) = (current.width() / 2, current.height() / 2);
        if width < 64 || height < 64 {
            break;
        }
        current = current.resize_exact(width, height, image::imageops::FilterType::Lanczos3);
    }
    Err(ClientFailure::ToolFailed {
        detail: "screenshot could not be compressed within the image budget".to_string(),
    })
}

#[cfg(test)]
mod tests;
