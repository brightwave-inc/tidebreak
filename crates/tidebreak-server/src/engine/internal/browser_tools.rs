//! Session-scoped in-app browser tools for the internal engine.
//!
//! Decision 94 (computer use for coding harnesses): the internal engine
//! reaches the same trusted in-app browser channel every external harness
//! gets through the MCP bridge. The session
//! worker hands the engine a [`BrowserChannelSpec`] naming a session-private
//! capability file; these tools read it once at session launch and drive the
//! loopback `/code/browser` routes with the token it carries, so the owner,
//! workspace, and session scope is derived by the server from the token —
//! never asserted by the engine.
//!
//! The tool set mirrors the CLI bridge's registry: list, navigate, snapshot,
//! wait, and screenshot always register; `browser_act` registers when the
//! capfile advertises `semantic_actions`, open/close/activate when it
//! advertises `lifecycle`, and `browser_diagnostics` when it advertises
//! `developer_diagnostics`. Schemas and argument validation come from the
//! canonical `tidebreak_core::browser` contract — nothing is duplicated here.
//!
//! Screenshot pixels return as real [`ToolOutput`] image blocks bounded by
//! the same budget the MCP bridge enforces
//! ([`MAX_BROWSER_SCREENSHOT_IMAGE_BLOCK_BYTES`]) and never enter model text
//! or structured data.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use tidebreak_core::{
    browser_act_tool_spec, browser_activate_tool_spec, browser_close_tool_spec,
    browser_diagnostics_tool_spec, browser_list_tool_spec, browser_navigate_tool_spec,
    browser_open_tool_spec, browser_screenshot_tool_spec, browser_snapshot_tool_spec,
    browser_wait_tool_spec, validate_browser_act_arguments, validate_browser_activate_arguments,
    validate_browser_close_arguments, validate_browser_diagnostics_arguments,
    validate_browser_list_arguments, validate_browser_navigate_arguments,
    validate_browser_open_arguments, validate_browser_screenshot_arguments,
    validate_browser_snapshot_arguments, validate_browser_wait_arguments, ApprovalClass,
    BrowserActResult, BrowserDiagnosticsResult, BrowserLifecycleResult, BrowserListResult,
    BrowserNavigateResult, BrowserOpenResult, BrowserPageSnapshot, BrowserScreenshotResult,
    BrowserWaitResult, DocumentBlob, ImageData, ImageMediaType, ImageRef, Result as CoreResult,
    Tool, ToolCtx, ToolErrorCategory, ToolOutput, ToolSpec,
    MAX_BROWSER_SCREENSHOT_IMAGE_BLOCK_BYTES, MAX_IMAGE_DIMENSION,
};
use tidebreak_harness::BrowserChannelSpec;

/// Maximum bytes the capfile is allowed to be.
const CAPFILE_MAX_BYTES: u64 = 65_536;

/// Ceiling on one error response body.
const ERROR_BODY_MAX_BYTES: usize = 8 * 1024;

/// Ceiling on a semantic snapshot response frame.
const SNAPSHOT_FRAME_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Ceiling on a screenshot response frame: the budget-fitted image encodes
/// to at most ~1.4 MiB of base-64, so 2 MiB (the shared native frame
/// ceiling) leaves headroom for the JSON envelope.
const SCREENSHOT_FRAME_MAX_BYTES: usize = 2 * 1024 * 1024;

/// Ceiling on a diagnostics response frame.
const DIAGNOSTICS_FRAME_MAX_BYTES: usize = 1024 * 1024;

/// Ceiling on a browser list response frame.
const LIST_FRAME_MAX_BYTES: usize = 256 * 1024;

/// Ceiling on the small navigate/wait/act/lifecycle response frames.
const SMALL_FRAME_MAX_BYTES: usize = 64 * 1024;

/// Base-64 characters a budget-fitting screenshot can occupy. Anything
/// larger cannot decode within the image-block budget, so it is refused
/// before allocation.
const MAX_SCREENSHOT_BASE64_CHARS: usize = MAX_BROWSER_SCREENSHOT_IMAGE_BLOCK_BYTES.div_ceil(3) * 4;

const TOKEN_PREFIX: &str = "tbreak_bt_";

/// One session's browser tool registrations, shared by every turn the
/// session runs. Built once at launch from the session's capability file.
///
/// The registered set is gated by the capfile's capability flags, so the
/// engine never advertises a tool the native runtime reported it cannot
/// serve — the same honesty contract the MCP bridge keeps.
pub(super) fn browser_session_tools(
    browser: &BrowserChannelSpec,
) -> Result<Vec<Arc<dyn Tool>>, String> {
    let (client, capabilities) = BrowserSessionClient::from_capfile(&browser.capability_file)?;
    let client = Arc::new(client);
    let mut ops = vec![
        BrowserOp::List,
        BrowserOp::Navigate,
        BrowserOp::Snapshot,
        BrowserOp::Wait,
        BrowserOp::Screenshot,
    ];
    if capabilities.semantic_actions {
        ops.push(BrowserOp::Act);
    }
    if capabilities.lifecycle {
        ops.extend([BrowserOp::Open, BrowserOp::Close, BrowserOp::Activate]);
    }
    if capabilities.developer_diagnostics {
        ops.push(BrowserOp::Diagnostics);
    }
    Ok(ops
        .into_iter()
        .map(|op| {
            Arc::new(InternalBrowserTool {
                op,
                client: client.clone(),
            }) as Arc<dyn Tool>
        })
        .collect())
}

/// Capability flags read from the trusted capfile. The server derived them
/// from the actual native runtime, so gating on them keeps the advertised
/// tool set honest.
#[derive(Clone, Copy, Default)]
struct BrowserChannelFlags {
    semantic_actions: bool,
    lifecycle: bool,
    developer_diagnostics: bool,
}

/// Wire shape of the version-1 browser capfile.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserCapfileWire {
    version: u32,
    endpoint: String,
    token: String,
    #[serde(default)]
    semantic_actions: bool,
    /// Whether the runtime can open, close, and activate tabs. Absent on
    /// capfiles that predate lifecycle support.
    #[serde(default)]
    lifecycle: bool,
    /// Whether the runtime can surface page diagnostics. Absent on capfiles
    /// that predate diagnostics support.
    #[serde(default)]
    developer_diagnostics: bool,
}

/// Loopback client for the session's browser channel. Never derives `Debug`:
/// the bearer token must not appear in debug output or errors.
struct BrowserSessionClient {
    client: reqwest::Client,
    endpoint: String,
    token: String,
}

impl BrowserSessionClient {
    /// Read and validate the session capability file. Fails closed on
    /// anything but a version-1 loopback `/code/browser` endpoint and a
    /// canonical `tbreak_bt_<UUID>` token; nothing secret enters error text.
    fn from_capfile(path: &std::path::Path) -> Result<(Self, BrowserChannelFlags), String> {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| format!("browser capfile cannot be read ({error})"))?;
        if !metadata.file_type().is_file() || metadata.len() > CAPFILE_MAX_BYTES {
            return Err("browser capfile is not a small regular file".to_owned());
        }
        let raw = read_file_capped(path, CAPFILE_MAX_BYTES as usize)
            .map_err(|error| format!("browser capfile cannot be read ({error})"))?;
        let wire: BrowserCapfileWire = serde_json::from_str(&raw)
            .map_err(|error| format!("browser capfile is not valid JSON ({error})"))?;
        if wire.version != 1 {
            return Err(format!(
                "browser capfile version {} is not supported",
                wire.version
            ));
        }
        validate_endpoint(&wire.endpoint)?;
        if wire.token.len() != TOKEN_PREFIX.len() + 36
            || !wire.token.starts_with(TOKEN_PREFIX)
            || uuid::Uuid::parse_str(&wire.token[TOKEN_PREFIX.len()..]).is_err()
        {
            return Err("browser capfile token has an unexpected shape".to_owned());
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            // Headroom over the core 30 s browser-wait ceiling: a valid
            // 30 000 ms wait must not be cut off at the client boundary.
            .timeout(Duration::from_secs(40))
            .build()
            .map_err(|error| format!("could not build browser HTTP client ({error})"))?;
        Ok((
            Self {
                client,
                endpoint: wire.endpoint,
                token: wire.token,
            },
            BrowserChannelFlags {
                semantic_actions: wire.semantic_actions,
                lifecycle: wire.lifecycle,
                developer_diagnostics: wire.developer_diagnostics,
            },
        ))
    }

    /// Execute one call. A transport failure retries once only for a
    /// read-only operation; a state-changing operation whose transport
    /// failed has an unknown effect, so the refusal tells the model to
    /// take a fresh snapshot instead of replaying blindly (decision 94:
    /// unknown outcomes require inspection before another action).
    async fn fetch<T: serde::de::DeserializeOwned>(
        &self,
        op: BrowserOp,
        args: &Value,
    ) -> Result<T, ToolFailure> {
        match self.request(op, args).await {
            Err(failure)
                if matches!(failure.category, ToolErrorCategory::TransportFailed)
                    && op.is_read_only() =>
            {
                self.request(op, args).await
            }
            Err(failure) if matches!(failure.category, ToolErrorCategory::TransportFailed) => {
                Err(ToolFailure {
                    category: failure.category,
                    message: format!(
                        "{} — the effect of this operation is unknown. Take a new \
                         browser_snapshot to see the current page state before acting again.",
                        failure.message
                    ),
                })
            }
            other => other,
        }
    }

    async fn request<T: serde::de::DeserializeOwned>(
        &self,
        op: BrowserOp,
        args: &Value,
    ) -> Result<T, ToolFailure> {
        let url = format!("{}/{}", self.endpoint, op.route());
        let request = match op {
            // The list route is the one GET on the channel; it takes no body.
            BrowserOp::List => self.client.get(url),
            _ => self.client.post(url).json(args),
        };
        let response = request
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| ToolFailure::transport("the browser channel is unreachable"))?;
        let status = response.status();
        let cap = if status.is_success() {
            op.frame_cap()
        } else {
            ERROR_BODY_MAX_BYTES
        };
        let bytes = read_body_bounded(response, cap).await?;
        if status.is_success() {
            return serde_json::from_slice(&bytes)
                .map_err(|_| ToolFailure::transport("the browser response did not parse"));
        }
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let kind = body
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let message = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("no detail");
        Err(ToolFailure::from_status(status.as_u16(), kind, message))
    }
}

/// Read `path` into a `String`, reading at most `cap + 1` bytes so a racing
/// append after the metadata check cannot allocate unbounded.
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
        buf.extend_from_slice(&chunk[..n.min(room)]);
        if buf.len() > cap {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "capfile exceeds the size limit",
            ));
        }
    }
    String::from_utf8(buf)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// Read a response body up to `max_bytes`, refusing — not buffering — the
/// remainder of an oversized stream.
async fn read_body_bounded(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, ToolFailure> {
    use futures::StreamExt;
    let mut stream = response.bytes_stream();
    let mut buf = Vec::with_capacity(max_bytes.min(4096));
    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result
            .map_err(|_| ToolFailure::transport("the browser response body was unreadable"))?;
        if chunk.len() > max_bytes.saturating_sub(buf.len()) {
            return Err(ToolFailure::failed(format!(
                "the browser response exceeded the {max_bytes}-byte frame limit; request a \
                 smaller result (lower max_nodes for snapshots, max_width/max_height for \
                 screenshots, or max_entries for diagnostics)"
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

fn validate_endpoint(endpoint: &str) -> Result<(), String> {
    let url: url::Url = endpoint
        .parse()
        .map_err(|_| "browser capfile endpoint is not a valid URL".to_owned())?;
    let host_ok = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|addr| addr.is_loopback())
    });
    if url.scheme() != "http"
        || !host_ok
        || url.port().is_none()
        || url.path() != "/code/browser"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("browser capfile endpoint is not a loopback /code/browser URL".to_owned());
    }
    Ok(())
}

/// A refusal, sized for the model. Never contains the token or endpoint;
/// server messages are scrubbed before they are embedded.
struct ToolFailure {
    category: ToolErrorCategory,
    message: String,
}

impl ToolFailure {
    fn transport(message: &str) -> Self {
        Self {
            category: ToolErrorCategory::TransportFailed,
            message: message.to_owned(),
        }
    }
    fn failed(message: String) -> Self {
        Self {
            category: ToolErrorCategory::ToolFailed,
            message,
        }
    }
    fn from_status(status: u16, kind: &str, message: &str) -> Self {
        let category = match status {
            400 | 409 | 422 => ToolErrorCategory::InvalidArguments,
            401 | 403 | 501 => ToolErrorCategory::ConfigurationRequired,
            404 => ToolErrorCategory::NotFound,
            _ => ToolErrorCategory::ToolFailed,
        };
        Self {
            category,
            message: format!(
                "({}) {}",
                scrub_server_message(kind),
                scrub_server_message(message)
            ),
        }
    }
}

/// Remove any plausible token-bearing text from a server error message.
fn scrub_server_message(message: &str) -> String {
    if message.contains("tbreak_") || message.contains("bearer") || message.contains("Bearer") {
        return "[redacted]".to_owned();
    }
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

/// One canonical browser operation the channel serves.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BrowserOp {
    List,
    Navigate,
    Snapshot,
    Wait,
    Screenshot,
    Act,
    Open,
    Close,
    Activate,
    Diagnostics,
}

impl BrowserOp {
    fn spec(self) -> ToolSpec {
        match self {
            Self::List => browser_list_tool_spec(),
            Self::Navigate => browser_navigate_tool_spec(),
            Self::Snapshot => browser_snapshot_tool_spec(),
            Self::Wait => browser_wait_tool_spec(),
            Self::Screenshot => browser_screenshot_tool_spec(),
            Self::Act => browser_act_tool_spec(),
            Self::Open => browser_open_tool_spec(),
            Self::Close => browser_close_tool_spec(),
            Self::Activate => browser_activate_tool_spec(),
            Self::Diagnostics => browser_diagnostics_tool_spec(),
        }
    }

    fn route(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Navigate => "navigate",
            Self::Snapshot => "snapshot",
            Self::Wait => "wait",
            Self::Screenshot => "screenshot",
            Self::Act => "act",
            Self::Open => "open",
            Self::Close => "close",
            Self::Activate => "activate",
            Self::Diagnostics => "diagnostics",
        }
    }

    /// Whether the operation observes the page without changing it. Only
    /// these retry after a transport failure; replaying a state-changing
    /// operation with an unknown outcome is never safe.
    fn is_read_only(self) -> bool {
        matches!(
            self,
            Self::List | Self::Snapshot | Self::Wait | Self::Screenshot | Self::Diagnostics
        )
    }

    fn approval_class(self) -> ApprovalClass {
        if self.is_read_only() {
            ApprovalClass::ReadOnly
        } else {
            ApprovalClass::Sensitive
        }
    }

    fn frame_cap(self) -> usize {
        match self {
            Self::List => LIST_FRAME_MAX_BYTES,
            Self::Snapshot => SNAPSHOT_FRAME_MAX_BYTES,
            Self::Screenshot => SCREENSHOT_FRAME_MAX_BYTES,
            Self::Diagnostics => DIAGNOSTICS_FRAME_MAX_BYTES,
            Self::Navigate | Self::Wait | Self::Act | Self::Open | Self::Close | Self::Activate => {
                SMALL_FRAME_MAX_BYTES
            }
        }
    }

    fn validate(self, args: &Value) -> bool {
        match self {
            Self::List => validate_browser_list_arguments(args),
            Self::Navigate => validate_browser_navigate_arguments(args),
            Self::Snapshot => validate_browser_snapshot_arguments(args),
            Self::Wait => validate_browser_wait_arguments(args),
            Self::Screenshot => validate_browser_screenshot_arguments(args),
            Self::Act => validate_browser_act_arguments(args),
            Self::Open => validate_browser_open_arguments(args),
            Self::Close => validate_browser_close_arguments(args),
            Self::Activate => validate_browser_activate_arguments(args),
            Self::Diagnostics => validate_browser_diagnostics_arguments(args),
        }
    }
}

/// One canonical browser tool bound to the session channel.
struct InternalBrowserTool {
    op: BrowserOp,
    client: Arc<BrowserSessionClient>,
}

#[async_trait::async_trait]
impl Tool for InternalBrowserTool {
    fn spec(&self) -> ToolSpec {
        self.op.spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        self.op.approval_class()
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> CoreResult<ToolOutput> {
        if !self.op.validate(&args) {
            return Ok(ToolOutput::failed(
                ToolErrorCategory::InvalidArguments,
                format!("invalid {} arguments", self.op.spec().name),
            ));
        }
        let output = match self.op {
            BrowserOp::List => self
                .client
                .fetch::<BrowserListResult>(self.op, &args)
                .await
                .map(|result| result_output(format_list_summary(&result), &result)),
            BrowserOp::Navigate => self
                .client
                .fetch::<BrowserNavigateResult>(self.op, &args)
                .await
                .map(|result| {
                    result_output(
                        format!(
                            "Navigated browser {} to {}. Load state: {:?}, epoch: {}.",
                            result.browser_id, result.url, result.load_state, result.document_epoch
                        ),
                        &result,
                    )
                }),
            BrowserOp::Snapshot => self
                .client
                .fetch::<BrowserPageSnapshot>(self.op, &args)
                .await
                .map(|snapshot| result_output(format_snapshot_summary(&snapshot), &snapshot)),
            BrowserOp::Wait => self
                .client
                .fetch::<BrowserWaitResult>(self.op, &args)
                .await
                .map(|result| {
                    result_output(
                        format!(
                            "Wait {:?} on browser {} (epoch {}): {}.",
                            result.status, result.browser_id, result.document_epoch, result.message
                        ),
                        &result,
                    )
                }),
            BrowserOp::Screenshot => match self
                .client
                .fetch::<BrowserScreenshotResult>(self.op, &args)
                .await
            {
                Ok(result) => screenshot_tool_output(&result),
                Err(failure) => Err(failure),
            },
            BrowserOp::Act => self
                .client
                .fetch::<BrowserActResult>(self.op, &args)
                .await
                .map(|result| {
                    result_output(
                        format!(
                            "Browser action {} returned {:?}. {}",
                            result.action, result.status, result.message
                        ),
                        &result,
                    )
                }),
            BrowserOp::Open => self
                .client
                .fetch::<BrowserOpenResult>(self.op, &args)
                .await
                .map(|result| {
                    result_output(
                        format!(
                            "Opened browser {} at {}. Load state: {:?}, epoch: {}, visible: {}.",
                            result.browser_id,
                            result.url,
                            result.load_state,
                            result.document_epoch,
                            result.visible
                        ),
                        &result,
                    )
                }),
            BrowserOp::Close | BrowserOp::Activate => self
                .client
                .fetch::<BrowserLifecycleResult>(self.op, &args)
                .await
                .map(|result| {
                    result_output(
                        format!(
                            "Browser {} {:?} on {}: {}",
                            self.op.route(),
                            result.status,
                            result.browser_id,
                            result.message
                        ),
                        &result,
                    )
                }),
            BrowserOp::Diagnostics => self
                .client
                .fetch::<BrowserDiagnosticsResult>(self.op, &args)
                .await
                .map(|result| result_output(format_diagnostics_summary(&result), &result)),
        };
        Ok(output.unwrap_or_else(|failure| {
            ToolOutput::failed(failure.category, format!("browser: {}", failure.message))
        }))
    }
}

/// Keep the complete, bounded result available in both text and structured
/// data: snapshot identities and refs must reach the model even when the
/// caller consumes only text. Screenshots use a separate path so base-64
/// pixels never enter text or data.
fn result_output<T: serde::Serialize>(summary: String, result: &T) -> ToolOutput {
    let data = serde_json::to_value(result).unwrap_or(Value::Null);
    ToolOutput::text(format!("{summary}\n\n{data}")).with_data(data)
}

fn format_list_summary(result: &BrowserListResult) -> String {
    if result.sessions.is_empty() {
        return "No browser sessions available.".to_owned();
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

fn format_snapshot_summary(snapshot: &BrowserPageSnapshot) -> String {
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

/// Entry text is untrusted page data and stays in the structured payload.
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

/// Convert one screenshot response into model text plus an out-of-band image
/// block. No structured payload is attached because it would carry the
/// server's base-64 field into journals and model-facing tool data.
fn screenshot_tool_output(result: &BrowserScreenshotResult) -> Result<ToolOutput, ToolFailure> {
    let (image_ref, image_data) = decode_screenshot_image(result)?;
    let text = format!(
        "Screenshot of browser {} captured at epoch {} (snapshot {}): {}×{}, {:.1} kiB.",
        result.browser_id,
        result.document_epoch,
        result.snapshot_id,
        image_ref.width,
        image_ref.height,
        image_ref.byte_len as f64 / 1024.0,
    );
    Ok(ToolOutput::text(text).with_images([(image_ref, image_data)]))
}

/// Decode and validate the screenshot within the model-facing image budget.
/// The channel already fits captures to [`MAX_BROWSER_SCREENSHOT_IMAGE_BLOCK_BYTES`];
/// a response over that budget is refused with guidance rather than
/// re-encoded, mirroring the native adapter's fail-closed posture.
fn decode_screenshot_image(
    result: &BrowserScreenshotResult,
) -> Result<(ImageRef, ImageData), ToolFailure> {
    use base64::Engine as _;
    let expected_format = match result.mime_type.as_str() {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        other => {
            return Err(ToolFailure::failed(format!(
                "screenshot mime type must be image/png or image/jpeg, got {other}"
            )));
        }
    };
    if result.image_base64.len() > MAX_SCREENSHOT_BASE64_CHARS {
        return Err(oversized_screenshot_failure());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&result.image_base64)
        .map_err(|_| ToolFailure::failed("screenshot base-64 did not decode".to_owned()))?;
    if bytes.is_empty() {
        return Err(ToolFailure::failed("screenshot image is empty".to_owned()));
    }
    if bytes.len() > MAX_BROWSER_SCREENSHOT_IMAGE_BLOCK_BYTES {
        return Err(oversized_screenshot_failure());
    }
    let format = image::guess_format(&bytes).map_err(|_| {
        ToolFailure::failed("screenshot bytes are not a recognized image".to_owned())
    })?;
    if format != expected_format {
        return Err(ToolFailure::failed(
            "screenshot bytes do not match the declared mime type".to_owned(),
        ));
    }
    let (width, height) = image::ImageReader::with_format(std::io::Cursor::new(&bytes), format)
        .into_dimensions()
        .map_err(|_| ToolFailure::failed("screenshot image header could not be read".to_owned()))?;
    if width == 0 || height == 0 || width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(ToolFailure::failed(format!(
            "screenshot dimensions {width}×{height} are out of range"
        )));
    }
    let media_type = match format {
        image::ImageFormat::Jpeg => ImageMediaType::Jpeg,
        _ => ImageMediaType::Png,
    };
    let blob = DocumentBlob::from_bytes(&bytes);
    let image_ref = ImageRef {
        blob_id: blob.id,
        media_type,
        width,
        height,
        byte_len: bytes.len() as u64,
    };
    image_ref
        .validate()
        .map_err(|reason| ToolFailure::failed(format!("screenshot image is invalid: {reason}")))?;
    Ok((image_ref, ImageData::new(media_type, bytes)))
}

fn oversized_screenshot_failure() -> ToolFailure {
    ToolFailure::failed(format!(
        "screenshot exceeds the {MAX_BROWSER_SCREENSHOT_IMAGE_BLOCK_BYTES}-byte image budget; \
         request a smaller capture with max_width/max_height"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write_capfile(
        dir: &std::path::Path,
        endpoint: &str,
        token: &str,
        flags: BrowserChannelFlags,
    ) -> PathBuf {
        let path = dir.join(format!("browser-cap-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "endpoint": endpoint,
                "token": token,
                "semantic_actions": flags.semantic_actions,
                "lifecycle": flags.lifecycle,
                "developer_diagnostics": flags.developer_diagnostics,
            })
            .to_string(),
        )
        .unwrap();
        path
    }

    fn test_token() -> String {
        format!("tbreak_bt_{}", uuid::Uuid::new_v4())
    }

    fn spec_for(capfile: PathBuf) -> BrowserChannelSpec {
        BrowserChannelSpec::new(capfile, PathBuf::from("/usr/local/bin/tidebreak"))
    }

    const LOOPBACK_ENDPOINT: &str = "http://127.0.0.1:15003/code/browser";

    fn tool_names(tools: &[Arc<dyn Tool>]) -> Vec<String> {
        tools.iter().map(|tool| tool.spec().name).collect()
    }

    #[test]
    fn the_toolset_tracks_the_capfile_capability_flags() {
        let dir = tempfile::tempdir().unwrap();
        let base = vec![
            "browser_list",
            "browser_navigate",
            "browser_snapshot",
            "browser_wait",
            "browser_screenshot",
        ];

        let none = browser_session_tools(&spec_for(write_capfile(
            dir.path(),
            LOOPBACK_ENDPOINT,
            &test_token(),
            BrowserChannelFlags::default(),
        )))
        .unwrap();
        assert_eq!(tool_names(&none), base);

        let lifecycle_only = browser_session_tools(&spec_for(write_capfile(
            dir.path(),
            LOOPBACK_ENDPOINT,
            &test_token(),
            BrowserChannelFlags {
                lifecycle: true,
                ..BrowserChannelFlags::default()
            },
        )))
        .unwrap();
        let mut expected = base.clone();
        expected.extend(["browser_open", "browser_close", "browser_activate"]);
        assert_eq!(tool_names(&lifecycle_only), expected);

        let all = browser_session_tools(&spec_for(write_capfile(
            dir.path(),
            LOOPBACK_ENDPOINT,
            &test_token(),
            BrowserChannelFlags {
                semantic_actions: true,
                lifecycle: true,
                developer_diagnostics: true,
            },
        )))
        .unwrap();
        assert_eq!(
            tool_names(&all),
            vec![
                "browser_list",
                "browser_navigate",
                "browser_snapshot",
                "browser_wait",
                "browser_screenshot",
                "browser_act",
                "browser_open",
                "browser_close",
                "browser_activate",
                "browser_diagnostics",
            ]
        );
        for tool in &all {
            let name = tool.spec().name;
            let expected = match name.as_str() {
                "browser_navigate" | "browser_act" | "browser_open" | "browser_close"
                | "browser_activate" => ApprovalClass::Sensitive,
                _ => ApprovalClass::ReadOnly,
            };
            assert_eq!(tool.approval_class(), expected, "{name}");
        }
    }

    #[test]
    fn a_malformed_capfile_fails_launch_without_leaking_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let token = test_token();
        let cases = [
            // Non-loopback endpoint.
            write_capfile(
                dir.path(),
                "http://evil.example:80/code/browser",
                &token,
                BrowserChannelFlags::default(),
            ),
            // Wrong route path.
            write_capfile(
                dir.path(),
                "http://127.0.0.1:15003/code/native",
                &token,
                BrowserChannelFlags::default(),
            ),
            // Native-channel token on the browser channel.
            write_capfile(
                dir.path(),
                LOOPBACK_ENDPOINT,
                &format!("tbreak_nt_{}", uuid::Uuid::new_v4()),
                BrowserChannelFlags::default(),
            ),
        ];
        for capfile in cases {
            let error = match browser_session_tools(&spec_for(capfile)) {
                Err(error) => error,
                Ok(_) => panic!("malformed capfile must fail launch"),
            };
            assert!(!error.contains("tbreak_bt_"), "{error}");
            assert!(!error.contains(&token), "{error}");
        }

        let unsupported = write_capfile(dir.path(), LOOPBACK_ENDPOINT, &token, {
            BrowserChannelFlags::default()
        });
        let raw = std::fs::read_to_string(&unsupported).unwrap();
        std::fs::write(&unsupported, raw.replace("\"version\":1", "\"version\":2")).unwrap();
        assert!(browser_session_tools(&spec_for(unsupported)).is_err());
    }

    #[test]
    fn only_read_only_operations_retry_after_a_transport_failure() {
        for op in [
            BrowserOp::List,
            BrowserOp::Snapshot,
            BrowserOp::Wait,
            BrowserOp::Screenshot,
            BrowserOp::Diagnostics,
        ] {
            assert!(op.is_read_only());
        }
        for op in [
            BrowserOp::Navigate,
            BrowserOp::Act,
            BrowserOp::Open,
            BrowserOp::Close,
            BrowserOp::Activate,
        ] {
            assert!(!op.is_read_only());
        }
    }

    fn png_fixture(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::new_rgb8(width, height)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    /// The image path proper, independent of any live socket: real PNG bytes
    /// become a model-facing image block, and neither text nor structured
    /// data ever carries the base-64 payload.
    #[test]
    fn screenshots_decode_into_image_blocks_and_never_into_text_or_data() {
        use base64::Engine as _;
        let screenshot = |image_base64: String, mime_type: &str| BrowserScreenshotResult {
            browser_id: "browser-1".to_owned(),
            snapshot_id: "snap-1".to_owned(),
            document_epoch: 4,
            image_base64,
            mime_type: mime_type.to_owned(),
        };

        let encoded = base64::engine::general_purpose::STANDARD.encode(png_fixture(8, 6));
        let output = match screenshot_tool_output(&screenshot(encoded.clone(), "image/png")) {
            Ok(output) => output,
            Err(failure) => panic!("a small PNG must decode: {}", failure.message),
        };
        assert_eq!(output.images.len(), 1);
        assert_eq!(output.images[0].media_type, ImageMediaType::Png);
        assert_eq!((output.images[0].width, output.images[0].height), (8, 6));
        assert!(
            output.image_data.contains(output.images[0].blob_id),
            "pixels ride beside the reference so the next model call can see them"
        );
        assert!(output.data.is_none());
        assert!(!output.content.contains(&encoded[..24]));

        // Over the image budget: refused with actionable guidance.
        let oversized = screenshot("A".repeat(MAX_SCREENSHOT_BASE64_CHARS + 4), "image/png");
        let Err(failure) = screenshot_tool_output(&oversized) else {
            panic!("an over-budget capture must be refused");
        };
        assert!(failure.message.contains("max_width/max_height"));

        // Bytes that contradict the declared mime type are refused.
        let mislabeled = screenshot(
            base64::engine::general_purpose::STANDARD.encode(png_fixture(4, 4)),
            "image/jpeg",
        );
        let Err(failure) = screenshot_tool_output(&mislabeled) else {
            panic!("mislabeled bytes must be refused");
        };
        assert!(failure.message.contains("declared mime type"));
    }

    fn fake_channel_router(token: String) -> axum::Router {
        use axum::http::{HeaderMap, StatusCode};
        use axum::response::{IntoResponse, Response};
        use base64::Engine as _;

        fn bearer(headers: &HeaderMap) -> Option<&str> {
            headers
                .get(axum::http::header::AUTHORIZATION)?
                .to_str()
                .ok()?
                .strip_prefix("Bearer ")
        }

        fn unauthorized() -> Response {
            (
                StatusCode::UNAUTHORIZED,
                axum::Json(serde_json::json!({
                    "kind": "unauthorized",
                    "message": "unknown or revoked browser capability token",
                })),
            )
                .into_response()
        }

        let snapshot_token = token.clone();
        let screenshot_token = token.clone();
        let act_token = token;
        axum::Router::new()
            .route(
                "/code/browser/snapshot",
                axum::routing::post(
                    move |headers: HeaderMap, axum::Json(args): axum::Json<Value>| async move {
                        if bearer(&headers) != Some(snapshot_token.as_str()) {
                            return unauthorized();
                        }
                        assert_eq!(args["browser_id"], "browser-1");
                        axum::Json(serde_json::json!({
                            "browserId": "browser-1",
                            "snapshotId": "snap-1",
                            "documentEpoch": 4,
                            "contentTrust": "untrusted_page",
                            "url": "https://example.com/",
                            "title": "Example",
                            "viewport": {"width": 800.0, "height": 600.0, "scrollX": 0.0, "scrollY": 0.0},
                            "nodes": [],
                            "frames": [],
                            "truncated": false,
                        }))
                        .into_response()
                    },
                ),
            )
            .route(
                "/code/browser/screenshot",
                axum::routing::post(
                    move |headers: HeaderMap, axum::Json(args): axum::Json<Value>| async move {
                        if bearer(&headers) != Some(screenshot_token.as_str()) {
                            return unauthorized();
                        }
                        let image_base64 = if args["browser_id"] == "browser-oversized" {
                            "A".repeat(MAX_SCREENSHOT_BASE64_CHARS + 4)
                        } else {
                            base64::engine::general_purpose::STANDARD.encode(png_fixture(8, 6))
                        };
                        axum::Json(serde_json::json!({
                            "browserId": args["browser_id"],
                            "snapshotId": "snap-1",
                            "documentEpoch": 4,
                            "imageBase64": image_base64,
                            "mimeType": "image/png",
                        }))
                        .into_response()
                    },
                ),
            )
            .route(
                "/code/browser/act",
                axum::routing::post(
                    move |headers: HeaderMap, axum::Json(_): axum::Json<Value>| async move {
                        if bearer(&headers) != Some(act_token.as_str()) {
                            return unauthorized();
                        }
                        StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    },
                ),
            )
    }

    #[tokio::test]
    async fn the_channel_round_trips_results_and_images_over_loopback() {
        let token = test_token();
        let token_for_server = token.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!(
            "http://127.0.0.1:{}/code/browser",
            listener.local_addr().unwrap().port()
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, fake_channel_router(token_for_server))
                .await
                .unwrap();
        });

        let dir = tempfile::tempdir().unwrap();
        let capfile = write_capfile(
            dir.path(),
            &endpoint,
            &token,
            BrowserChannelFlags {
                semantic_actions: true,
                lifecycle: true,
                developer_diagnostics: true,
            },
        );
        let tools = browser_session_tools(&spec_for(capfile)).unwrap();
        let tool = |name: &str| {
            tools
                .iter()
                .find(|tool| tool.spec().name == name)
                .unwrap_or_else(|| panic!("{name} is registered"))
                .clone()
        };
        let ctx = ToolCtx::without_private_scratch(tidebreak_core::SessionId::new(), None);

        // Invalid arguments are refused by the canonical validator before
        // any request leaves the process.
        let refused = tool("browser_snapshot")
            .execute(
                &ctx,
                serde_json::json!({"browser_id": "browser-1", "max_nodes": 0}),
            )
            .await
            .unwrap();
        assert!(refused.is_error);
        assert_eq!(
            refused.error_category,
            Some(ToolErrorCategory::InvalidArguments)
        );

        // A structured result reaches the model as summary text plus the
        // full payload in both text and data.
        let snapshot = tool("browser_snapshot")
            .execute(&ctx, serde_json::json!({"browser_id": "browser-1"}))
            .await
            .unwrap();
        assert!(!snapshot.is_error, "{}", snapshot.content);
        assert!(snapshot
            .content
            .contains("Snapshot of https://example.com/"));
        assert!(snapshot.content.contains("snap-1"));
        assert_eq!(snapshot.data.as_ref().unwrap()["snapshotId"], "snap-1");

        // Screenshot pixels arrive as a real image block, never as text or
        // structured data.
        let screenshot = tool("browser_screenshot")
            .execute(
                &ctx,
                serde_json::json!({
                    "browser_id": "browser-1",
                    "snapshot_id": "snap-1",
                    "document_epoch": 4,
                }),
            )
            .await
            .unwrap();
        assert!(!screenshot.is_error, "{}", screenshot.content);
        assert_eq!(screenshot.images.len(), 1);
        assert_eq!(screenshot.images[0].media_type, ImageMediaType::Png);
        assert_eq!(
            (screenshot.images[0].width, screenshot.images[0].height),
            (8, 6)
        );
        assert!(screenshot.data.is_none());
        assert!(screenshot.content.len() < 512, "no pixels in model text");

        // A capture beyond the image budget is refused with guidance, not
        // buffered or truncated into the model context.
        let oversized = tool("browser_screenshot")
            .execute(
                &ctx,
                serde_json::json!({
                    "browser_id": "browser-oversized",
                    "snapshot_id": "snap-1",
                    "document_epoch": 4,
                }),
            )
            .await
            .unwrap();
        assert!(oversized.is_error);
        assert!(oversized.content.contains("max_width/max_height"));

        // A revoked token is a configuration refusal that never echoes the
        // bearer back into model text.
        let (revoked_client, _) = BrowserSessionClient::from_capfile(&write_capfile(
            dir.path(),
            &endpoint,
            &test_token(),
            BrowserChannelFlags::default(),
        ))
        .unwrap();
        let revoked = InternalBrowserTool {
            op: BrowserOp::Snapshot,
            client: Arc::new(revoked_client),
        }
        .execute(&ctx, serde_json::json!({"browser_id": "browser-1"}))
        .await
        .unwrap();
        assert!(revoked.is_error);
        assert_eq!(
            revoked.error_category,
            Some(ToolErrorCategory::ConfigurationRequired)
        );
        assert!(revoked.content.contains("unauthorized"));
        assert!(!revoked.content.contains("tbreak_bt_"));

        // Plain server failures map to tool failures with scrubbed detail.
        let failed = tool("browser_act")
            .execute(
                &ctx,
                serde_json::json!({
                    "browser_id": "browser-1",
                    "snapshot_id": "snap-1",
                    "document_epoch": 4,
                    "ref": "@e1",
                    "action": {"type": "click"},
                }),
            )
            .await
            .unwrap();
        assert!(failed.is_error);
        assert_eq!(failed.error_category, Some(ToolErrorCategory::ToolFailed));

        server.abort();
    }
}
