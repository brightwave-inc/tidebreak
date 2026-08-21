//! Tidebreak browser CLI commands and the `browser-mcp` stdio server.
//!
//! `tidebreak browser list|navigate|snapshot --json` drive a running Tidebreak
//! browser server through the session-private capability file named by
//! `TIDEBREAK_BROWSER_CAPFILE`. Every operation runs through a single
//! [`BrowserClient`] that is shared between the direct CLI and the MCP tool
//! implementations.
//!
//! `tidebreak browser-mcp` serves exactly three tools — browser_list,
//! browser_navigate, browser_snapshot — over MCP stdio, using the canonical
//! core tool specs and validating typed arguments before sending them to the
//! browser server.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tidebreak_core::{
    browser_list_tool_spec, browser_navigate_tool_spec, browser_snapshot_tool_spec,
    validate_browser_list_arguments, validate_browser_navigate_arguments,
    validate_browser_snapshot_arguments, AgentError, ApprovalClass, AutoApproveGate,
    BrowserListResult, BrowserNavigateArgs, BrowserNavigateResult, BrowserPageSnapshot,
    BrowserSnapshotArgs, Result, Tool, ToolCtx, ToolErrorCategory, ToolOutput, ToolRegistry,
    ToolSpec,
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
}

/// Wire shape of the capfile. The only supported version is 1.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserCapfileWire {
    version: u32,
    endpoint: String,
    token: String,
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
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
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
        other => Err(format!("unknown browser command {other:?}")),
    }
}

/// Usage text shown for `tidebreak browser` (and `browser-mcp`).
pub(crate) const BROWSER_USAGE: &str = "\
usage: tidebreak browser list --json
       tidebreak browser navigate --browser-id <id> --url <url> --json
       tidebreak browser snapshot --browser-id <id> [--max-nodes <n>] --json

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
    }
}

// ---------------------------------------------------------------------------
// browser-mcp: stdio MCP server with exactly three tools
// ---------------------------------------------------------------------------

/// Serve `tidebreak browser-mcp`: load the capfile, construct the client,
/// build a registry of three tools backed by that client, and run MCP over
/// stdio.
///
/// ## Tool classification
///
/// | Tool            | Class     | Rationale                                                   |
/// |-----------------|-----------|-------------------------------------------------------------|
/// | browser_list    | ReadOnly  | Observes session state; no mutation.                        |
/// | browser_navigate| Sensitive | Mutates the shared visible browser; the user sees the change.|
/// | browser_snapshot | ReadOnly | Reads untrusted page data; no side effects.                 |
///
/// ## Authorization
///
/// The registry contains only these three tools. We wire
/// [`AutoApproveGate`] because possession of the session-private capfile
/// *is* the actual scoped authorization boundary for this process: without
/// the capfile the server cannot be reached, and with it every operation is
/// exactly the one the capfile's issuer intended. The browser server
/// authorizes each operation independently against its own origin-scoped
/// grants.
pub(crate) async fn run_browser_mcp() -> Result<()> {
    let cap = BrowserCapfile::from_env()?;
    let client = BrowserClient::new(&cap)?;

    let tools = Arc::new(
        ToolRegistry::new()
            .with(Box::new(BrowserListTool {
                client: client.clone(),
            }))
            .with(Box::new(BrowserNavigateTool {
                client: client.clone(),
            }))
            .with(Box::new(BrowserSnapshotTool {
                client: client.clone(),
            })),
    );

    // No filesystem workspace: the tools reach the loopback server only.
    let ctx = ToolCtx::without_private_scratch(tidebreak_core::ChatId::new(), None);

    let server =
        tidebreak_mcp::McpServer::new(tools, ctx).with_approval_gate(Arc::new(AutoApproveGate));

    tidebreak_mcp::serve_stdio(server)
        .await
        .map_err(|error| AgentError::msg(format!("MCP stdio error: {error}")))
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_TOKEN: &str = "tbreak_bt_00000000-0000-0000-0000-000000000000";

    /// Extract the error from `BrowserCapfile::load` without requiring
    /// `Debug` on the `Ok` type. `BrowserCapfile` intentionally does not
    /// implement `Debug` because it carries the bearer token, so
    /// `Result::unwrap_err` (which requires `T: Debug`) cannot be used.
    fn capfile_load_err(path: &std::path::Path) -> AgentError {
        match BrowserCapfile::load(path) {
            Ok(_) => panic!("expected BrowserCapfile::load to fail, but it succeeded"),
            Err(error) => error,
        }
    }

    // -- Capfile parsing ---------------------------------------------------

    #[test]
    fn capfile_accepts_valid_v1() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "endpoint": "http://127.0.0.1:9876/code/browser",
                "token": VALID_TOKEN
            })
            .to_string(),
        )
        .unwrap();
        let cap = BrowserCapfile::load(&path).unwrap();
        assert_eq!(cap.endpoint, "http://127.0.0.1:9876/code/browser");
        assert_eq!(cap.token, VALID_TOKEN);
    }

    #[test]
    fn capfile_accepts_localhost() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "endpoint": "http://localhost:3000/code/browser",
                "token": VALID_TOKEN
            })
            .to_string(),
        )
        .unwrap();
        assert!(BrowserCapfile::load(&path).is_ok());
    }

    #[test]
    fn capfile_accepts_ipv6_loopback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "endpoint": "http://[::1]:8080/code/browser",
                "token": VALID_TOKEN
            })
            .to_string(),
        )
        .unwrap();
        assert!(BrowserCapfile::load(&path).is_ok());
    }

    #[test]
    fn capfile_rejects_non_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let err = capfile_load_err(dir.path());
        assert!(err.to_string().contains("regular file"));
    }

    // -- Capfile rejection: endpoint validation ----------------------------

    #[test]
    fn capfile_rejects_https() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "endpoint": "https://127.0.0.1:9876/code/browser",
                "token": VALID_TOKEN
            })
            .to_string(),
        )
        .unwrap();
        assert!(BrowserCapfile::load(&path).is_err());
    }

    #[test]
    fn capfile_rejects_non_loopback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "endpoint": "http://192.168.1.1:8080/code/browser",
                "token": VALID_TOKEN
            })
            .to_string(),
        )
        .unwrap();
        let err = capfile_load_err(&path);
        let msg = err.to_string();
        assert!(msg.contains("loopback"), "error: {msg}");
    }

    #[test]
    fn capfile_rejects_missing_port() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "endpoint": "http://127.0.0.1/code/browser",
                "token": VALID_TOKEN
            })
            .to_string(),
        )
        .unwrap();
        let err = capfile_load_err(&path);
        let msg = err.to_string();
        assert!(msg.contains("port"), "error: {msg}");
    }

    #[test]
    fn capfile_rejects_wrong_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "endpoint": "http://127.0.0.1:9876/some/other/path",
                "token": VALID_TOKEN
            })
            .to_string(),
        )
        .unwrap();
        let err = capfile_load_err(&path);
        let msg = err.to_string();
        assert!(msg.contains("/code/browser"), "error: {msg}");
    }

    #[test]
    fn capfile_rejects_username_in_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "endpoint": "http://user@127.0.0.1:9876/code/browser",
                "token": VALID_TOKEN
            })
            .to_string(),
        )
        .unwrap();
        assert!(BrowserCapfile::load(&path).is_err());
    }

    #[test]
    fn capfile_rejects_query_in_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "endpoint": "http://127.0.0.1:9876/code/browser?x=1",
                "token": VALID_TOKEN
            })
            .to_string(),
        )
        .unwrap();
        assert!(BrowserCapfile::load(&path).is_err());
    }

    // -- Capfile rejection: token validation -------------------------------

    #[test]
    fn capfile_rejects_wrong_token_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "endpoint": "http://127.0.0.1:9876/code/browser",
                "token": "notbreak__00000000-0000-0000-0000-000000000000"
            })
            .to_string(),
        )
        .unwrap();
        let err = capfile_load_err(&path);
        let msg = err.to_string();
        assert!(msg.contains("prefix"), "error: {msg}");
    }

    #[test]
    fn capfile_rejects_token_without_uuid_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "endpoint": "http://127.0.0.1:9876/code/browser",
                "token": "tbreak_bt_00000000-0000-0000-0000-00000000000Z"
            })
            .to_string(),
        )
        .unwrap();
        let err = capfile_load_err(&path);
        let msg = err.to_string();
        assert!(msg.contains("UUID"), "error: {msg}");
    }

    #[test]
    fn capfile_rejects_short_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "endpoint": "http://127.0.0.1:9876/code/browser",
                "token": "tbreak_bt_short"
            })
            .to_string(),
        )
        .unwrap();
        let err = capfile_load_err(&path);
        let msg = err.to_string();
        assert!(msg.contains("length"), "error: {msg}");
    }

    // -- Capfile rejection: version, JSON, size, unknown fields ------------

    #[test]
    fn capfile_rejects_unsupported_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 2,
                "endpoint": "http://127.0.0.1:9876/code/browser",
                "token": VALID_TOKEN
            })
            .to_string(),
        )
        .unwrap();
        let err = capfile_load_err(&path);
        let msg = err.to_string();
        assert!(msg.contains("version 2"), "error: {msg}");
    }

    #[test]
    fn capfile_rejects_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "endpoint": "http://127.0.0.1:9876/code/browser",
                "token": VALID_TOKEN,
                "extra": "should be rejected"
            })
            .to_string(),
        )
        .unwrap();
        assert!(BrowserCapfile::load(&path).is_err());
    }

    #[test]
    fn capfile_rejects_malformed_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.json");
        std::fs::write(&path, "not json").unwrap();
        assert!(BrowserCapfile::load(&path).is_err());
    }

    #[test]
    fn capfile_rejects_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.json");
        let big = "x".repeat((CAPFILE_MAX_BYTES + 1) as usize);
        std::fs::write(&path, big).unwrap();
        assert!(BrowserCapfile::load(&path).is_err());
    }

    #[test]
    fn capfile_rejects_non_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.json");
        std::fs::write(&path, vec![0xff, 0xfe, 0xfd]).unwrap();
        assert!(BrowserCapfile::load(&path).is_err());
    }

    // -- Capfile: error redaction -----------------------------------------

    #[test]
    fn capfile_error_never_contains_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.json");
        let token = VALID_TOKEN;
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "endpoint": "http://192.168.1.1:8080/code/browser",
                "token": token
            })
            .to_string(),
        )
        .unwrap();
        let err = capfile_load_err(&path);
        let msg = err.to_string();
        assert!(!msg.contains(token), "token leaked into error: {msg}");
    }

    #[test]
    fn capfile_error_never_contains_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.json");
        let path_str = path.to_string_lossy().to_string();
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "endpoint": "http://192.168.1.1:8080/code/browser",
                "token": VALID_TOKEN
            })
            .to_string(),
        )
        .unwrap();
        let err = capfile_load_err(&path);
        let msg = err.to_string();
        assert!(
            !msg.contains(&path_str),
            "capfile path leaked into error: {msg}"
        );
    }

    #[test]
    fn capfile_error_never_contains_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.json");
        let endpoint = "http://192.168.1.1:8080/code/browser";
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "endpoint": endpoint,
                "token": VALID_TOKEN
            })
            .to_string(),
        )
        .unwrap();
        let err = capfile_load_err(&path);
        let msg = err.to_string();
        assert!(!msg.contains(endpoint), "endpoint leaked into error: {msg}");
    }

    // -- Capfile: read_file_capped ----------------------------------------

    #[test]
    fn read_file_capped_returns_exact_contents_within_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        let content = "hello world";
        std::fs::write(&path, content).unwrap();
        let result = read_file_capped(&path, 1024).unwrap();
        assert_eq!(result, content);
    }

    #[test]
    fn read_file_capped_truncates_at_cap_plus_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        let big: String = (0..200).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        std::fs::write(&path, &big).unwrap();
        let result = read_file_capped(&path, 100).unwrap();
        // Should have at most 101 bytes.
        assert!(result.len() <= 101);
    }

    // -- Command parsing ---------------------------------------------------

    #[test]
    fn parse_browser_list() {
        let cmd = parse_browser(vec!["list".to_string()]).unwrap();
        assert!(matches!(cmd, BrowserCommand::List));
    }

    #[test]
    fn parse_browser_list_rejects_extra_args() {
        let err = parse_browser(vec!["list".to_string(), "extra".to_string()]).unwrap_err();
        assert!(err.contains("no arguments"), "error: {err}");
    }

    #[test]
    fn parse_browser_navigate() {
        let cmd = parse_browser(vec![
            "navigate".to_string(),
            "--browser-id".to_string(),
            "browser-123".to_string(),
            "--url".to_string(),
            "https://example.com".to_string(),
        ])
        .unwrap();
        match cmd {
            BrowserCommand::Navigate { browser_id, url } => {
                assert_eq!(browser_id, "browser-123");
                assert_eq!(url, "https://example.com");
            }
            _ => panic!("expected Navigate"),
        }
    }

    #[test]
    fn parse_browser_navigate_rejects_duplicate_browser_id() {
        let err = parse_browser(vec![
            "navigate".to_string(),
            "--browser-id".to_string(),
            "browser-1".to_string(),
            "--browser-id".to_string(),
            "browser-2".to_string(),
            "--url".to_string(),
            "https://example.com".to_string(),
        ])
        .unwrap_err();
        assert!(err.contains("duplicate"), "error: {err}");
    }

    #[test]
    fn parse_browser_navigate_rejects_duplicate_url() {
        let err = parse_browser(vec![
            "navigate".to_string(),
            "--browser-id".to_string(),
            "browser-1".to_string(),
            "--url".to_string(),
            "https://example.com".to_string(),
            "--url".to_string(),
            "https://other.com".to_string(),
        ])
        .unwrap_err();
        assert!(err.contains("duplicate"), "error: {err}");
    }

    #[test]
    fn parse_browser_navigate_requires_browser_id() {
        let err = parse_browser(vec![
            "navigate".to_string(),
            "--url".to_string(),
            "https://example.com".to_string(),
        ])
        .unwrap_err();
        assert!(err.contains("--browser-id"), "error: {err}");
    }

    #[test]
    fn parse_browser_navigate_requires_url() {
        let err = parse_browser(vec![
            "navigate".to_string(),
            "--browser-id".to_string(),
            "browser-123".to_string(),
        ])
        .unwrap_err();
        assert!(err.contains("--url"), "error: {err}");
    }

    #[test]
    fn parse_browser_snapshot_default() {
        let cmd = parse_browser(vec![
            "snapshot".to_string(),
            "--browser-id".to_string(),
            "browser-123".to_string(),
        ])
        .unwrap();
        match cmd {
            BrowserCommand::Snapshot {
                browser_id,
                max_nodes,
            } => {
                assert_eq!(browser_id, "browser-123");
                assert_eq!(max_nodes, None);
            }
            _ => panic!("expected Snapshot"),
        }
    }

    #[test]
    fn parse_browser_snapshot_with_max_nodes() {
        let cmd = parse_browser(vec![
            "snapshot".to_string(),
            "--browser-id".to_string(),
            "browser-123".to_string(),
            "--max-nodes".to_string(),
            "100".to_string(),
        ])
        .unwrap();
        match cmd {
            BrowserCommand::Snapshot { max_nodes, .. } => {
                assert_eq!(max_nodes, Some(100));
            }
            _ => panic!("expected Snapshot"),
        }
    }

    #[test]
    fn parse_browser_snapshot_rejects_duplicate_max_nodes() {
        let err = parse_browser(vec![
            "snapshot".to_string(),
            "--browser-id".to_string(),
            "browser-123".to_string(),
            "--max-nodes".to_string(),
            "100".to_string(),
            "--max-nodes".to_string(),
            "200".to_string(),
        ])
        .unwrap_err();
        assert!(err.contains("duplicate"), "error: {err}");
    }

    #[test]
    fn parse_browser_snapshot_requires_browser_id() {
        let err = parse_browser(vec![
            "snapshot".to_string(),
            "--max-nodes".to_string(),
            "100".to_string(),
        ])
        .unwrap_err();
        assert!(err.contains("--browser-id"), "error: {err}");
    }

    #[test]
    fn parse_browser_rejects_unknown_flag() {
        let err = parse_browser(vec!["list".to_string(), "--unknown".to_string()]).unwrap_err();
        assert!(err.contains("no arguments"), "error: {err}");
    }

    #[test]
    fn parse_browser_navigate_rejects_unknown_flag() {
        let err = parse_browser(vec![
            "navigate".to_string(),
            "--browser-id".to_string(),
            "browser-1".to_string(),
            "--url".to_string(),
            "https://example.com".to_string(),
            "--unknown".to_string(),
        ])
        .unwrap_err();
        assert!(err.contains("unknown"), "error: {err}");
    }

    #[test]
    fn parse_browser_rejects_unknown_verb() {
        let err = parse_browser(vec!["unknown".to_string()]).unwrap_err();
        assert!(err.contains("unknown browser command"), "error: {err}");
    }

    #[test]
    fn parse_browser_empty_is_usage() {
        let err = parse_browser(vec![]).unwrap_err();
        assert!(err.contains("usage"), "error: {err}");
    }

    // -- JSON round-trip contracts -----------------------------------------

    #[test]
    fn browser_list_result_round_trips() {
        let result = BrowserListResult { sessions: vec![] };
        let json = serde_json::to_string(&result).unwrap();
        let back: BrowserListResult = serde_json::from_str(&json).unwrap();
        assert!(back.sessions.is_empty());
    }

    #[test]
    fn browser_navigate_result_round_trips() {
        use tidebreak_core::BrowserLoadState;
        let result = BrowserNavigateResult {
            browser_id: "browser-1".to_string(),
            url: "https://example.com".to_string(),
            load_state: BrowserLoadState::Ready,
            document_epoch: 5,
        };
        let json = serde_json::to_string(&result).unwrap();
        let back: BrowserNavigateResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.document_epoch, 5);
    }

    #[test]
    fn browser_snapshot_args_well_formed_validates() {
        let args = BrowserSnapshotArgs {
            browser_id: "browser-123".to_string(),
            max_nodes: None,
        };
        assert!(args.is_well_formed());
    }

    // -- Tool advertisement (three exact tools) ----------------------------

    #[test]
    fn registry_contains_exactly_three_browser_tools() {
        let client = browser_client_stub();
        let tools = ToolRegistry::new()
            .with(Box::new(BrowserListTool {
                client: client.clone(),
            }))
            .with(Box::new(BrowserNavigateTool {
                client: client.clone(),
            }))
            .with(Box::new(BrowserSnapshotTool {
                client: client.clone(),
            }));
        let specs = tools.specs();
        let mut names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            ["browser_list", "browser_navigate", "browser_snapshot"]
        );
    }

    #[test]
    fn tool_classes_match_spec() {
        let client = browser_client_stub();
        let list = BrowserListTool {
            client: client.clone(),
        };
        let navigate = BrowserNavigateTool {
            client: client.clone(),
        };
        let snapshot = BrowserSnapshotTool {
            client: client.clone(),
        };
        assert_eq!(list.approval_class(), ApprovalClass::ReadOnly);
        assert_eq!(navigate.approval_class(), ApprovalClass::Sensitive);
        assert_eq!(snapshot.approval_class(), ApprovalClass::ReadOnly);
    }

    fn browser_client_stub() -> BrowserClient {
        // Build a client that points nowhere — fine for registration tests
        // since they never send a request.
        BrowserClient {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            endpoint: "http://127.0.0.1:1/code/browser".to_string(),
            token: "tbreak_bt_00000000-0000-0000-0000-000000000000".to_string(),
        }
    }

    // -- ClientFailure classification -------------------------------------

    #[test]
    fn client_failure_maps_http_statuses_to_correct_categories() {
        let cases: &[(u16, ToolErrorCategory)] = &[
            (400, ToolErrorCategory::InvalidArguments),
            (401, ToolErrorCategory::ConfigurationRequired),
            (403, ToolErrorCategory::ConfigurationRequired),
            (404, ToolErrorCategory::NotFound),
            (500, ToolErrorCategory::ToolFailed),
            (501, ToolErrorCategory::ConfigurationRequired),
            (502, ToolErrorCategory::ToolFailed),
        ];
        for (status, expected) in cases {
            let failure = ClientFailure::from_http_status(*status, "test_kind", "test message");
            assert_eq!(
                failure.to_tool_error_category(),
                *expected,
                "status {status}"
            );
        }
    }

    #[test]
    fn client_failure_scrubs_token_from_server_messages() {
        let failure = ClientFailure::from_http_status(
            500,
            "internal",
            &format!("server echoed token {VALID_TOKEN} in error"),
        );
        let text = failure.redacted_text();
        assert!(!text.contains(VALID_TOKEN));
        assert!(text.contains("[redacted]"));
    }

    #[test]
    fn client_failure_scrubs_bearer_keyword() {
        let failure =
            ClientFailure::from_http_status(500, "internal", "Authorization: Bearer secret-token");
        let text = failure.redacted_text();
        assert!(text.contains("[redacted]"));
        assert!(!text.contains("Bearer"));
    }

    #[test]
    fn scrub_server_message_replaces_embedded_uuids() {
        let msg = "resource 550e8400-e29b-41d4-a716-446655440000 is gone";
        let scrubbed = scrub_server_message(msg);
        assert!(!scrubbed.contains("550e8400"));
        assert!(scrubbed.contains("[redacted]"));
    }

    #[test]
    fn scrub_server_message_handles_unicode_before_uuid() {
        let id = uuid::Uuid::nil().to_string();
        let msg = format!("é resource {id} is gone");
        let scrubbed = scrub_server_message(&msg);
        assert!(!scrubbed.contains(&id));
        assert!(scrubbed.starts_with("é resource [redacted]"));
    }

    // -- HTTP: redirects are not followed ---------------------------------

    #[tokio::test]
    async fn client_does_not_follow_redirects() {
        // A server that returns 301 → the client must see the 301, not the
        // redirect target.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let endpoint = format!("http://127.0.0.1:{}", addr.port());

        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, writer) = stream.into_split();
            let mut buf_reader = tokio::io::BufReader::new(reader);
            // Drain the request headers.
            loop {
                let mut line = String::new();
                if tokio::io::AsyncBufReadExt::read_line(&mut buf_reader, &mut line)
                    .await
                    .unwrap()
                    == 0
                {
                    break;
                }
                if line == "\r\n" {
                    break;
                }
            }
            // Send a 301 redirect with a body containing a server error.
            let body = serde_json::json!({
                "kind": "redirect",
                "message": "this resource has moved"
            });
            let body_bytes = serde_json::to_vec(&body).unwrap();
            let response = format!(
                "HTTP/1.1 301 Moved Permanently\r\n\
                 Location: http://evil.example.com/\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                body_bytes.len()
            );
            let mut writer = writer;
            tokio::io::AsyncWriteExt::write_all(&mut writer, response.as_bytes())
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut writer, &body_bytes)
                .await
                .unwrap();
        });

        // Build a client pointed at *this* listener on an arbitrary port
        // (not the /code/browser path, but that doesn't matter for the
        // redirect test).
        let cap = BrowserCapfile {
            endpoint: format!("http://127.0.0.1:{}/code/browser", addr.port()),
            token: VALID_TOKEN.to_string(),
        };
        let client = BrowserClient::new(&cap).unwrap();

        // Override endpoint to point at the raw listener (no /code/browser).
        let mut redirect_client = client.clone();
        redirect_client.endpoint = endpoint;

        let result = browser_list(&redirect_client).await;
        // Must fail — reqwest with redirect::Policy::none() returns the 301
        // without following it. Our client treats non-2xx as errors.
        assert!(result.is_err(), "client must not follow redirects");
        let failure = result.unwrap_err();
        // The 301 body should be parsed, giving "redirect" kind.
        let text = failure.redacted_text();
        assert!(text.contains("redirect"), "error text: {text}");

        handle.abort();
    }

    // -- HTTP: response body bounding -------------------------------------

    #[tokio::test]
    async fn overlimit_response_body_is_refused() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let endpoint = format!("http://127.0.0.1:{}", addr.port());

        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, writer) = stream.into_split();
            let mut buf_reader = tokio::io::BufReader::new(reader);
            loop {
                let mut line = String::new();
                if tokio::io::AsyncBufReadExt::read_line(&mut buf_reader, &mut line)
                    .await
                    .unwrap()
                    == 0
                {
                    break;
                }
                if line == "\r\n" {
                    break;
                }
            }
            // Send a response claiming a tiny body but actually streaming a
            // huge one.
            let padding = "x".repeat(128 * 1024); // 128 KiB > 64 KiB navigate cap
            let response = "HTTP/1.1 200 OK\r\n\
                 Content-Type: application/json\r\n\
                 Transfer-Encoding: chunked\r\n\
                 Connection: close\r\n\r\n";
            let mut writer = writer;
            tokio::io::AsyncWriteExt::write_all(&mut writer, response.as_bytes())
                .await
                .unwrap();
            // Write chunked body that exceeds the cap.
            let chunk_header = format!("{:x}\r\n", padding.len());
            tokio::io::AsyncWriteExt::write_all(&mut writer, chunk_header.as_bytes())
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut writer, padding.as_bytes())
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut writer, b"\r\n")
                .await
                .unwrap();
            // Terminal chunk.
            tokio::io::AsyncWriteExt::write_all(&mut writer, b"0\r\n\r\n")
                .await
                .unwrap();
        });

        let cap = BrowserCapfile {
            endpoint: format!("http://127.0.0.1:{}/code/browser", addr.port()),
            token: VALID_TOKEN.to_string(),
        };
        let client = BrowserClient::new(&cap).unwrap();
        let mut big_client = client.clone();
        big_client.endpoint = endpoint;

        let result = browser_navigate(
            &big_client,
            &BrowserNavigateArgs {
                browser_id: "browser-1".to_string(),
                url: "https://example.com".to_string(),
            },
        )
        .await;
        assert!(result.is_err(), "oversized body must be refused");
        let text = result.unwrap_err().redacted_text();
        assert!(text.contains("size limit"), "error text: {text}");

        handle.abort();
    }

    // -- HTTP: success path (actual JSON decode) ---------------------------

    #[tokio::test]
    async fn browser_list_decodes_successful_json_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let endpoint = format!("http://127.0.0.1:{}", addr.port());

        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, writer) = stream.into_split();
            let mut buf_reader = tokio::io::BufReader::new(reader);
            let mut headers = Vec::new();
            loop {
                let mut line = String::new();
                if tokio::io::AsyncBufReadExt::read_line(&mut buf_reader, &mut line)
                    .await
                    .unwrap()
                    == 0
                {
                    break;
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
                headers.push(line);
            }
            let has_auth = headers
                .iter()
                .any(|h| h.to_lowercase().starts_with("authorization"));
            assert!(has_auth, "headers: {headers:?}");

            let body = serde_json::json!({
                "sessions": [{
                    "browserId": "browser-1",
                    "url": "https://example.com",
                    "title": "Example",
                    "loadState": "ready",
                    "visible": true,
                    "engine": {
                        "name": "web_kit_gtk",
                        "capabilities": {
                            "lifecycle": true,
                            "persistentProfile": false,
                            "semanticSnapshot": true,
                            "semanticActions": false,
                            "screenshot": false,
                            "crossOriginFrames": false,
                            "profileReset": false
                        }
                    },
                    "controller": {
                        "kind": "agent",
                        "halted": false,
                        "takeoverRequired": false
                    }
                }]
            });
            let body_bytes = serde_json::to_vec(&body).unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                body_bytes.len()
            );
            let mut writer = writer;
            tokio::io::AsyncWriteExt::write_all(&mut writer, response.as_bytes())
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut writer, &body_bytes)
                .await
                .unwrap();
        });

        let cap = BrowserCapfile {
            endpoint: format!("http://127.0.0.1:{}/code/browser", addr.port()),
            token: VALID_TOKEN.to_string(),
        };
        let client = BrowserClient::new(&cap).unwrap();
        let mut test_client = client.clone();
        test_client.endpoint = endpoint;

        let result = browser_list(&test_client).await.unwrap();
        assert_eq!(result.sessions.len(), 1);
        assert_eq!(result.sessions[0].browser_id, "browser-1");
        handle.abort();
    }

    #[tokio::test]
    async fn browser_list_decodes_server_error_body() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let endpoint = format!("http://127.0.0.1:{}", addr.port());

        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, writer) = stream.into_split();
            let mut buf_reader = tokio::io::BufReader::new(reader);
            loop {
                let mut line = String::new();
                if tokio::io::AsyncBufReadExt::read_line(&mut buf_reader, &mut line)
                    .await
                    .unwrap()
                    == 0
                {
                    break;
                }
                if line == "\r\n" {
                    break;
                }
            }
            let body = serde_json::json!({
                "kind": "browser_not_found",
                "message": "no browser session for this capability"
            });
            let body_bytes = serde_json::to_vec(&body).unwrap();
            let response = format!(
                "HTTP/1.1 404 Not Found\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                body_bytes.len()
            );
            let mut writer = writer;
            tokio::io::AsyncWriteExt::write_all(&mut writer, response.as_bytes())
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut writer, &body_bytes)
                .await
                .unwrap();
        });

        let cap = BrowserCapfile {
            endpoint: format!("http://127.0.0.1:{}/code/browser", addr.port()),
            token: VALID_TOKEN.to_string(),
        };
        let client = BrowserClient::new(&cap).unwrap();
        let mut test_client = client.clone();
        test_client.endpoint = endpoint;

        let err = browser_list(&test_client).await.unwrap_err();
        assert_eq!(err.to_tool_error_category(), ToolErrorCategory::NotFound);
        let text = err.redacted_text();
        assert!(text.contains("browser_not_found"), "error: {text}");
        assert!(text.contains("no browser session"), "error: {text}");
        handle.abort();
    }

    // -- Token scrubbing in server errors ---------------------------------

    #[tokio::test]
    async fn server_error_body_containing_token_is_scrubbed() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let endpoint = format!("http://127.0.0.1:{}", addr.port());
        let token = VALID_TOKEN;

        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, writer) = stream.into_split();
            let mut buf_reader = tokio::io::BufReader::new(reader);
            loop {
                let mut line = String::new();
                if tokio::io::AsyncBufReadExt::read_line(&mut buf_reader, &mut line)
                    .await
                    .unwrap()
                    == 0
                {
                    break;
                }
                if line == "\r\n" {
                    break;
                }
            }
            let body = serde_json::json!({
                "kind": "internal",
                "message": format!("token is {token}")
            });
            let body_bytes = serde_json::to_vec(&body).unwrap();
            let response = format!(
                "HTTP/1.1 500 Internal Server Error\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                body_bytes.len()
            );
            let mut writer = writer;
            tokio::io::AsyncWriteExt::write_all(&mut writer, response.as_bytes())
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut writer, &body_bytes)
                .await
                .unwrap();
        });

        let cap = BrowserCapfile {
            endpoint: format!("http://127.0.0.1:{}/code/browser", addr.port()),
            token: token.to_string(),
        };
        let client = BrowserClient::new(&cap).unwrap();
        let mut test_client = client.clone();
        test_client.endpoint = endpoint;

        let err = browser_list(&test_client).await.unwrap_err();
        let text = err.redacted_text();
        assert!(!text.contains(token), "token leaked: {text}");
        assert!(text.contains("[redacted]"), "should be scrubbed: {text}");
        assert_eq!(err.to_tool_error_category(), ToolErrorCategory::ToolFailed);
        handle.abort();
    }

    // -- Navigate and snapshot success paths -------------------------------

    #[tokio::test]
    async fn browser_navigate_decodes_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let endpoint = format!("http://127.0.0.1:{}", addr.port());

        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, writer) = stream.into_split();
            let mut buf_reader = tokio::io::BufReader::new(reader);
            let mut content_length: usize = 0;
            loop {
                let mut line = String::new();
                if tokio::io::AsyncBufReadExt::read_line(&mut buf_reader, &mut line)
                    .await
                    .unwrap()
                    == 0
                {
                    break;
                }
                if line == "\r\n" {
                    break;
                }
                if line.to_lowercase().starts_with("content-length:") {
                    content_length = line.split(':').nth(1).unwrap().trim().parse().unwrap_or(0);
                }
            }
            let mut body_bytes = vec![0u8; content_length];
            if content_length > 0 {
                tokio::io::AsyncReadExt::read_exact(&mut buf_reader, &mut body_bytes)
                    .await
                    .unwrap();
            }
            let body: Value = serde_json::from_slice(&body_bytes).unwrap();
            assert_eq!(body["browser_id"], "browser-1");
            assert_eq!(body["url"], "https://example.com/");

            let result = serde_json::json!({
                "browserId": "browser-1",
                "url": "https://example.com/",
                "loadState": "loading",
                "documentEpoch": 7
            });
            let result_bytes = serde_json::to_vec(&result).unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                result_bytes.len()
            );
            let mut writer = writer;
            tokio::io::AsyncWriteExt::write_all(&mut writer, response.as_bytes())
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut writer, &result_bytes)
                .await
                .unwrap();
        });

        let cap = BrowserCapfile {
            endpoint: format!("http://127.0.0.1:{}/code/browser", addr.port()),
            token: VALID_TOKEN.to_string(),
        };
        let client = BrowserClient::new(&cap).unwrap();
        let mut test_client = client.clone();
        test_client.endpoint = endpoint;

        let args = BrowserNavigateArgs {
            browser_id: "browser-1".to_string(),
            url: "https://example.com/".to_string(),
        };
        let result = browser_navigate(&test_client, &args).await.unwrap();
        assert_eq!(result.document_epoch, 7);
        handle.abort();
    }

    #[tokio::test]
    async fn browser_snapshot_decodes_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let endpoint = format!("http://127.0.0.1:{}", addr.port());

        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, writer) = stream.into_split();
            let mut buf_reader = tokio::io::BufReader::new(reader);
            let mut content_length: usize = 0;
            loop {
                let mut line = String::new();
                if tokio::io::AsyncBufReadExt::read_line(&mut buf_reader, &mut line)
                    .await
                    .unwrap()
                    == 0
                {
                    break;
                }
                if line == "\r\n" {
                    break;
                }
                if line.to_lowercase().starts_with("content-length:") {
                    content_length = line.split(':').nth(1).unwrap().trim().parse().unwrap_or(0);
                }
            }
            let mut body_bytes = vec![0u8; content_length];
            if content_length > 0 {
                tokio::io::AsyncReadExt::read_exact(&mut buf_reader, &mut body_bytes)
                    .await
                    .unwrap();
            }
            let result = serde_json::json!({
                "browserId": "browser-1",
                "snapshotId": "snap-1",
                "documentEpoch": 3,
                "contentTrust": "untrusted_page",
                "url": "https://example.com/",
                "title": "Test Page",
                "viewport": {
                    "width": 1024.0, "height": 768.0,
                    "scrollX": 0.0, "scrollY": 0.0
                },
                "nodes": [],
                "frames": [],
                "truncated": false
            });
            let result_bytes = serde_json::to_vec(&result).unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                result_bytes.len()
            );
            let mut writer = writer;
            tokio::io::AsyncWriteExt::write_all(&mut writer, response.as_bytes())
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut writer, &result_bytes)
                .await
                .unwrap();
        });

        let cap = BrowserCapfile {
            endpoint: format!("http://127.0.0.1:{}/code/browser", addr.port()),
            token: VALID_TOKEN.to_string(),
        };
        let client = BrowserClient::new(&cap).unwrap();
        let mut test_client = client.clone();
        test_client.endpoint = endpoint;

        let args = BrowserSnapshotArgs {
            browser_id: "browser-1".to_string(),
            max_nodes: Some(250),
        };
        let result = browser_snapshot(&test_client, &args).await.unwrap();
        assert_eq!(result.title, "Test Page");
        assert_eq!(result.document_epoch, 3);
        handle.abort();
    }

    // -- MCP failure mapping ----------------------------------------------

    #[test]
    fn mcp_failure_uses_correct_tool_error_categories() {
        let cases: &[(ClientFailure, ToolErrorCategory)] = &[
            (
                ClientFailure::InvalidArguments {
                    detail: "bad".to_string(),
                },
                ToolErrorCategory::InvalidArguments,
            ),
            (
                ClientFailure::NotFound {
                    detail: "gone".to_string(),
                },
                ToolErrorCategory::NotFound,
            ),
            (
                ClientFailure::ConfigurationRequired {
                    detail: "setup".to_string(),
                },
                ToolErrorCategory::ConfigurationRequired,
            ),
            (
                ClientFailure::TransportFailed {
                    detail: "timeout".to_string(),
                },
                ToolErrorCategory::TransportFailed,
            ),
            (
                ClientFailure::ToolFailed {
                    detail: "crash".to_string(),
                },
                ToolErrorCategory::ToolFailed,
            ),
        ];
        for (failure, expected) in cases {
            let output = mcp_failure(failure.clone());
            assert_eq!(output.error_category, Some(*expected));
            assert!(output.is_error);
        }
    }
}
