//! Tidebreak native computer-use CLI commands and the `computer-mcp` server.
//!
//! `tidebreak computer <tool> --json '<arguments>' [--output <path>]` drives
//! one native computer-use operation through the session-private capability
//! file named by `TIDEBREAK_NATIVE_CAPFILE`. `tidebreak computer-mcp` serves
//! the full canonical native tool set over MCP stdio.
//!
//! Both paths use one [`NativeClient`], and both take their tool list and
//! argument validation from `tidebreak_core::computer_session`: a primitive
//! registered there is advertised and accepted here without a bridge change.
//!
//! Screenshots come back as real image content. The MCP server publishes
//! them as MCP image blocks; the direct CLI writes the PNG bytes to the
//! private path named by `--output` so an engine without MCP image support
//! (Grok's `read_file` vision path) can read the actual pixels. Base-64
//! image data never enters model-facing text or stdout JSON.

use crate::image_output::write_image_private;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use tidebreak_core::computer_session::{
    computer_session_tool_specs, is_chrome_session_tool, validate_computer_session_arguments,
};
use tidebreak_core::computer_session::{ComputerUseCall, ComputerUseOutcome, ComputerUseResult};
use tidebreak_core::{
    is_computer_use_control_tool, AgentError, ApprovalClass, AutoApproveGate, DocumentBlob,
    ImageData, ImageMediaType, ImageRef, Result, Tool, ToolCtx, ToolErrorCategory, ToolOutput,
    ToolRegistry, ToolSpec, MAX_IMAGE_DIMENSION,
};

// ---------------------------------------------------------------------------
// Bounds
// ---------------------------------------------------------------------------

/// Maximum bytes the capfile is allowed to be (64 KiB).
const CAPFILE_MAX_BYTES: u64 = 65_536;

/// Hard ceiling on one native response frame (the whole HTTP body). The
/// desktop adapter fits results inside this; anything larger is refused
/// rather than decoded unbounded.
const NATIVE_FRAME_MAX_BYTES: usize = 2 * 1024 * 1024; // 2 MiB

/// Ceiling on one decoded image within a result. An image between this and
/// the frame budget gets one bounded recompression attempt; past that the
/// caller is told to request a smaller capture.
const NATIVE_IMAGE_MAX_BYTES: usize = 1024 * 1024; // 1 MiB

/// Encoded form of the largest image the frame can carry.
const NATIVE_IMAGE_MAX_BASE64_CHARS: usize = NATIVE_FRAME_MAX_BYTES.div_ceil(3) * 4;

/// Ceiling on an error body we attempt to parse for `{kind, message}`.
const ERROR_BODY_MAX_BYTES: usize = 8 * 1024; // 8 KiB

/// Most images one result may carry into MCP content.
const MAX_RESULT_IMAGES: usize = 3;

/// Widest image (in pixels per side) the bounded recompressor will decode.
/// Larger captures are refused with sizing guidance instead of decoded.
const RECOMPRESS_MAX_PIXELS: u64 = 16_000_000;

const TOKEN_PREFIX: &str = "tbreak_nt_";
const TOKEN_LENGTH: usize = TOKEN_PREFIX.len() + 36; // prefix + UUID

// ---------------------------------------------------------------------------
// Capability file
// ---------------------------------------------------------------------------

/// The session-private capability file, read from the path named by
/// `TIDEBREAK_NATIVE_CAPFILE`.
///
/// This struct must never derive `Debug` — the token must not appear in
/// debug output or error formatting.
#[derive(Clone)]
struct NativeCapfile {
    endpoint: String,
    token: String,
}

/// Wire shape of the capfile. The only supported version is 1.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeCapfileWire {
    version: u32,
    endpoint: String,
    token: String,
}

impl NativeCapfile {
    /// Read and validate the capfile at `path`.
    ///
    /// Fails closed: the file must exist, fit in a hard byte cap, be regular
    /// UTF-8 JSON, declare version 1, carry a loopback HTTP endpoint with an
    /// explicit port and exact path `/code/native`, no user/pass/query/
    /// fragment, and a bearer token of the canonical `tbreak_nt_<UUID>`
    /// shape. The endpoint, token, and capfile path never enter error text.
    fn load(path: &std::path::Path) -> Result<Self> {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| {
            AgentError::config(format!("native capfile cannot be read ({error})"))
        })?;
        if !metadata.file_type().is_file() {
            return Err(AgentError::config("native capfile must be a regular file"));
        }
        if metadata.len() > CAPFILE_MAX_BYTES {
            return Err(AgentError::config("native capfile exceeds the size limit"));
        }
        let raw = read_file_capped(path, CAPFILE_MAX_BYTES as usize).map_err(|error| {
            AgentError::config(format!("native capfile cannot be read ({error})"))
        })?;
        if raw.len() > CAPFILE_MAX_BYTES as usize {
            return Err(AgentError::config("native capfile exceeds the size limit"));
        }
        let wire: NativeCapfileWire = serde_json::from_str(&raw).map_err(|error| {
            AgentError::config(format!("native capfile is not valid JSON ({error})"))
        })?;
        if wire.version != 1 {
            return Err(AgentError::config(format!(
                "native capfile version {} is not supported (only version 1)",
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

    /// Load from the path named by `TIDEBREAK_NATIVE_CAPFILE`.
    fn from_env() -> Result<Self> {
        let path = std::env::var_os("TIDEBREAK_NATIVE_CAPFILE")
            .map(PathBuf::from)
            .ok_or_else(|| AgentError::config("TIDEBREAK_NATIVE_CAPFILE is not set"))?;
        Self::load(&path)
    }

    /// Validate the endpoint: HTTP only, loopback host, explicit port, path
    /// exactly `/code/native`, no user/pass/query/fragment.
    fn validate_endpoint(endpoint: &str) -> Result<()> {
        let url: url::Url = endpoint
            .parse()
            .map_err(|_| AgentError::config("native capfile endpoint is not a valid URL"))?;
        if url.scheme() != "http" {
            return Err(AgentError::config("native capfile endpoint must use http"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(AgentError::config(
                "native capfile endpoint must not contain credentials",
            ));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(AgentError::config(
                "native capfile endpoint must not contain a query or fragment",
            ));
        }
        if url.path() != "/code/native" {
            return Err(AgentError::config(
                "native capfile endpoint path must be /code/native",
            ));
        }
        let Some(host) = url.host_str() else {
            return Err(AgentError::config("native capfile endpoint has no host"));
        };
        if url.port().is_none() {
            return Err(AgentError::config(
                "native capfile endpoint must include an explicit port",
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
                "native capfile endpoint is not a loopback address",
            ));
        }
        Ok(())
    }

    /// Validate the v1 token shape: `tbreak_nt_` prefix followed by a UUID.
    fn validate_token(token: &str) -> Result<()> {
        if token.len() != TOKEN_LENGTH || !token.starts_with(TOKEN_PREFIX) {
            return Err(AgentError::config(
                "native capfile token has an unexpected shape",
            ));
        }
        uuid::Uuid::parse_str(&token[TOKEN_PREFIX.len()..])
            .map_err(|_| AgentError::config("native capfile token suffix is not a UUID"))?;
        Ok(())
    }
}

/// Read `path` into a `String`, reading at most `cap + 1` bytes so a racing
/// append cannot allocate unbounded.
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
            break;
        }
    }
    String::from_utf8(buf)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

// ---------------------------------------------------------------------------
// Native client
// ---------------------------------------------------------------------------

/// Shared HTTP client for the native channel. Every request carries
/// `Authorization: Bearer <token>`, has redirects disabled, and uses bounded
/// connect and read timeouts.
///
/// This struct must never derive `Debug` — the token must not appear in
/// debug output or error formatting.
#[derive(Clone)]
struct NativeClient {
    client: reqwest::Client,
    endpoint: String,
    token: String,
}

impl NativeClient {
    fn new(cap: &NativeCapfile) -> Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            // Headroom over the 10 s wait ceiling plus consent latency: a
            // native operation may sit behind a human approval prompt.
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|error| {
                AgentError::config(format!("could not build native HTTP client ({error})"))
            })?;
        Ok(Self {
            client,
            endpoint: cap.endpoint.clone(),
            token: cap.token.clone(),
        })
    }

    /// Execute one call. A transport failure after the request may have
    /// reached the host retries once with the same request id: an exact
    /// duplicate recovers the stored result instead of replaying the action,
    /// and an action the host is unsure about answers `native_unknown_outcome`.
    async fn execute(
        &self,
        call: &ComputerUseCall,
    ) -> std::result::Result<ComputerUseResult, ClientFailure> {
        match self.post("execute", call).await {
            Err(ClientFailure::TransportFailed { .. }) => self.post("execute", call).await,
            other => other,
        }
    }

    /// Fetch the stored result for an exact prior call, if the host kept one.
    async fn result_for(
        &self,
        call: &ComputerUseCall,
    ) -> std::result::Result<ComputerUseResult, ClientFailure> {
        self.post("result", call).await
    }

    async fn post(
        &self,
        route: &str,
        call: &ComputerUseCall,
    ) -> std::result::Result<ComputerUseResult, ClientFailure> {
        let response = self
            .client
            .post(format!("{}/{route}", self.endpoint))
            .bearer_auth(&self.token)
            .json(call)
            .send()
            .await
            .map_err(|error| ClientFailure::TransportFailed {
                detail: format!("native request failed ({})", redact_reqwest_error(&error)),
            })?;
        let status = response.status();
        let limit = if status.is_success() {
            NATIVE_FRAME_MAX_BYTES
        } else {
            ERROR_BODY_MAX_BYTES
        };
        let bytes = read_response_body_bounded(response, limit).await?;
        if status.is_success() {
            serde_json::from_slice(&bytes).map_err(|error| ClientFailure::TransportFailed {
                detail: format!("unreadable native success body ({error})"),
            })
        } else {
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

/// Describe a reqwest error without its URL (which embeds the endpoint).
fn redact_reqwest_error(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "could not connect"
    } else if error.is_body() || error.is_decode() {
        "unreadable body"
    } else {
        "request error"
    }
}

/// Read a response body up to `max_bytes`; beyond that the request fails
/// rather than allocating unbounded.
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
        if chunk.len() > max_bytes.saturating_sub(buf.len()) {
            return Err(ClientFailure::ToolFailed {
                detail: format!(
                    "native response exceeded the {NATIVE_FRAME_MAX_BYTES}-byte frame limit; \
                     request a smaller capture (use app_id/max_dimension for native capture or max_width/max_height for Chrome)"
                ),
            });
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

// ---------------------------------------------------------------------------
// Client failure taxonomy
// ---------------------------------------------------------------------------

/// Typed failure from the native client. The bearer token, endpoint, and
/// capfile path are never included.
#[derive(Debug, Clone)]
enum ClientFailure {
    InvalidArguments {
        detail: String,
    },
    NotFound {
        detail: String,
    },
    ConfigurationRequired {
        detail: String,
    },
    /// The host cannot establish what a previous action did. Inspect the
    /// target (capture or read it) before proposing another action.
    UnknownOutcome {
        detail: String,
    },
    TransportFailed {
        detail: String,
    },
    ToolFailed {
        detail: String,
    },
}

impl ClientFailure {
    fn from_http_status(status: u16, kind: &str, message: &str) -> ClientFailure {
        let scrubbed = scrub_server_message(message);
        let detail = format!("({kind}) {scrubbed}");
        match (status, kind) {
            (409, "native_unknown_outcome") => ClientFailure::UnknownOutcome { detail },
            (400 | 409 | 422, _) => ClientFailure::InvalidArguments { detail },
            (401 | 403 | 501, _) => ClientFailure::ConfigurationRequired { detail },
            (404, _) => ClientFailure::NotFound { detail },
            _ => ClientFailure::ToolFailed { detail },
        }
    }

    fn to_tool_error_category(&self) -> ToolErrorCategory {
        match self {
            ClientFailure::InvalidArguments { .. } => ToolErrorCategory::InvalidArguments,
            ClientFailure::NotFound { .. } => ToolErrorCategory::NotFound,
            ClientFailure::ConfigurationRequired { .. } => ToolErrorCategory::ConfigurationRequired,
            ClientFailure::UnknownOutcome { .. } | ClientFailure::ToolFailed { .. } => {
                ToolErrorCategory::ToolFailed
            }
            ClientFailure::TransportFailed { .. } => ToolErrorCategory::TransportFailed,
        }
    }

    fn redacted_text(&self) -> String {
        match self {
            ClientFailure::InvalidArguments { detail } => {
                format!("computer: invalid arguments — {detail}")
            }
            ClientFailure::NotFound { detail } => format!("computer: not found — {detail}"),
            ClientFailure::ConfigurationRequired { detail } => {
                format!("computer: configuration required — {detail}")
            }
            ClientFailure::UnknownOutcome { detail } => format!(
                "computer: the previous action's effect is unknown — {detail}. Capture or read \
                 the app to see its current state before acting again."
            ),
            ClientFailure::TransportFailed { detail } => {
                format!("computer: transport failed — {detail}")
            }
            ClientFailure::ToolFailed { detail } => format!("computer: tool failed — {detail}"),
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
    if message.contains("tbreak_") || message.contains("bearer") || message.contains("Bearer") {
        return "[redacted]".to_string();
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

// ---------------------------------------------------------------------------
// Image handling
// ---------------------------------------------------------------------------

/// One decoded, validated, budget-fitted result image.
struct DecodedImage {
    image_ref: ImageRef,
    data: ImageData,
}

/// Decode, validate, and budget-fit every image in a result.
///
/// Each image must be PNG, decode to at most [`NATIVE_IMAGE_MAX_BYTES`].
/// An oversized image gets one bounded downscale-and-re-encode attempt;
/// if it still does not fit, the whole result fails with sizing guidance
/// so the model re-requests a smaller capture instead of receiving pixels
/// silently dropped.
fn decode_result_images(
    result: &ComputerUseResult,
) -> std::result::Result<Vec<DecodedImage>, ClientFailure> {
    use base64::Engine as _;

    if result.images.len() > MAX_RESULT_IMAGES {
        return Err(ClientFailure::ToolFailed {
            detail: format!(
                "native result carried {} images (limit {MAX_RESULT_IMAGES})",
                result.images.len()
            ),
        });
    }
    let mut decoded = Vec::with_capacity(result.images.len());
    for image in &result.images {
        if image.mime_type != "image/png" {
            return Err(ClientFailure::ToolFailed {
                detail: format!(
                    "native image mime type must be image/png, got {}",
                    image.mime_type
                ),
            });
        }
        if image.base64.len() > NATIVE_IMAGE_MAX_BASE64_CHARS {
            return Err(oversized_image_failure());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&image.base64)
            .map_err(|error| ClientFailure::ToolFailed {
                detail: format!("native image base-64 decode failed: {error}"),
            })?;
        if bytes.is_empty() {
            return Err(ClientFailure::ToolFailed {
                detail: "native image is empty".to_string(),
            });
        }
        let bytes = fit_png_to_budget(bytes)?;
        decoded.push(validated_png(bytes)?);
    }
    Ok(decoded)
}

fn oversized_image_failure() -> ClientFailure {
    ClientFailure::ToolFailed {
        detail: format!(
            "native image exceeds the {NATIVE_IMAGE_MAX_BYTES}-byte budget even after \
             recompression; request a smaller capture (use app_id/max_dimension for native capture or max_width/max_height for Chrome, \
             or capture a single window)"
        ),
    }
}

/// Return PNG bytes within [`NATIVE_IMAGE_MAX_BYTES`], downscaling and
/// re-encoding once when the input is too large. One attempt, bounded
/// decode size — never an unbounded compression loop.
fn fit_png_to_budget(bytes: Vec<u8>) -> std::result::Result<Vec<u8>, ClientFailure> {
    if bytes.len() <= NATIVE_IMAGE_MAX_BYTES {
        return Ok(bytes);
    }
    let (width, height) = png_dimensions(&bytes)?;
    if u64::from(width) * u64::from(height) > RECOMPRESS_MAX_PIXELS {
        return Err(oversized_image_failure());
    }
    let decoded =
        image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).map_err(|_| {
            ClientFailure::ToolFailed {
                detail: "native PNG could not be decoded".to_string(),
            }
        })?;
    // Scale the pixel count by the byte overshoot, floored so the result is
    // strictly smaller. PNG size does not scale exactly with pixels, so keep
    // a margin.
    let ratio = (NATIVE_IMAGE_MAX_BYTES as f64 / bytes.len() as f64).sqrt() * 0.9;
    let new_width = ((f64::from(width) * ratio) as u32).max(1);
    let new_height = ((f64::from(height) * ratio) as u32).max(1);
    let resized = decoded.resize(new_width, new_height, image::imageops::FilterType::Triangle);
    let mut out = Vec::new();
    resized
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|_| ClientFailure::ToolFailed {
            detail: "native PNG could not be re-encoded".to_string(),
        })?;
    if out.len() > NATIVE_IMAGE_MAX_BYTES {
        return Err(oversized_image_failure());
    }
    Ok(out)
}

fn png_dimensions(bytes: &[u8]) -> std::result::Result<(u32, u32), ClientFailure> {
    let format = image::guess_format(bytes).map_err(|_| ClientFailure::ToolFailed {
        detail: "native image bytes are not a recognized image".to_string(),
    })?;
    if format != image::ImageFormat::Png {
        return Err(ClientFailure::ToolFailed {
            detail: "native image bytes are not PNG".to_string(),
        });
    }
    image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png)
        .into_dimensions()
        .map_err(|_| ClientFailure::ToolFailed {
            detail: "native PNG header could not be read".to_string(),
        })
}

/// Build the content-addressed [`ImageRef`] + [`ImageData`] pair for
/// validated PNG bytes.
fn validated_png(bytes: Vec<u8>) -> std::result::Result<DecodedImage, ClientFailure> {
    let (width, height) = png_dimensions(&bytes)?;
    if width == 0 || height == 0 {
        return Err(ClientFailure::ToolFailed {
            detail: "native image has a zero dimension".to_string(),
        });
    }
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(ClientFailure::ToolFailed {
            detail: format!(
                "native image dimensions {width}×{height} exceed the maximum {MAX_IMAGE_DIMENSION}"
            ),
        });
    }
    let blob = DocumentBlob::from_bytes(&bytes);
    let image_ref = ImageRef {
        blob_id: blob.id,
        media_type: ImageMediaType::Png,
        width,
        height,
        byte_len: bytes.len() as u64,
    };
    image_ref
        .validate()
        .map_err(|reason| ClientFailure::ToolFailed {
            detail: format!("native image is invalid: {reason}"),
        })?;
    Ok(DecodedImage {
        image_ref,
        data: ImageData::new(ImageMediaType::Png, bytes),
    })
}

// ---------------------------------------------------------------------------
// Result → ToolOutput
// ---------------------------------------------------------------------------

/// Convert one result into a model-facing [`ToolOutput`]: summary text, the
/// structured payload without image bytes, and real image content blocks.
fn result_tool_output(
    result: &ComputerUseResult,
) -> std::result::Result<ToolOutput, ClientFailure> {
    let images = decode_result_images(result)?;
    let data = result_data_without_images(result);
    let mut text = match result.outcome {
        ComputerUseOutcome::Completed => result.text.clone(),
        ComputerUseOutcome::Rejected => {
            return Err(ClientFailure::ToolFailed {
                detail: format!("the host rejected the operation: {}", result.text),
            });
        }
        ComputerUseOutcome::Unknown => format!(
            "{} — the effect of this action is unknown. Capture or read the app to see its \
             current state before acting again.",
            result.text
        ),
    };
    if !data.is_null() && data != serde_json::json!({}) {
        text = format!("{text}\n\n{data}");
    }
    let mut output = ToolOutput::text(text);
    if !data.is_null() {
        output = output.with_data(data);
    }
    if !images.is_empty() {
        output = output.with_images(
            images
                .into_iter()
                .map(|image| (image.image_ref, image.data)),
        );
    }
    Ok(output)
}

/// The structured payload, guaranteed image-free: pixels ride as image
/// content only, never inside model-facing data or journals.
fn result_data_without_images(result: &ComputerUseResult) -> Value {
    let stripped = ComputerUseResult {
        images: Vec::new(),
        ..result.clone()
    };
    serde_json::to_value(&stripped).unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------
// computer-mcp: stdio MCP server with the dynamic canonical tool set
// ---------------------------------------------------------------------------

/// Serve `tidebreak computer-mcp`: load the capfile, construct the client,
/// register one MCP tool per canonical native tool spec, and run MCP over
/// stdio.
///
/// The registry is built dynamically from `computer_session_tool_specs()`, so a
/// primitive added to the core list is advertised without a bridge change.
/// Observation tools are read-only; tools that move the mouse, keyboard, or
/// focus are sensitive. The desktop authorizes every operation independently
/// against its per-session grants, consent, and Stop state.
pub(crate) async fn run_computer_mcp() -> Result<()> {
    let cap = NativeCapfile::from_env()?;
    let client = NativeClient::new(&cap)?;
    let mut tools = ToolRegistry::new();
    for spec in computer_session_tool_specs() {
        tools = tools.with(Box::new(NativeTool {
            spec,
            client: client.clone(),
        }));
    }
    let ctx = ToolCtx::without_private_scratch(tidebreak_core::SessionId::new(), None);
    let server = tidebreak_mcp::McpServer::new(Arc::new(tools), ctx)
        .with_approval_gate(Arc::new(AutoApproveGate));
    tidebreak_mcp::serve_stdio(server)
        .await
        .map_err(|error| AgentError::msg(format!("MCP stdio error: {error}")))
}

/// One canonical native tool as an MCP-registrable [`Tool`], backed by the
/// shared [`NativeClient`]. The spec comes verbatim from the core registry.
struct NativeTool {
    spec: ToolSpec,
    client: NativeClient,
}

#[async_trait::async_trait]
impl Tool for NativeTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn approval_class(&self) -> ApprovalClass {
        if is_computer_use_control_tool(&self.spec.name)
            || self.spec.name == "computer_return_to_tidebreak"
            || is_chrome_session_tool(&self.spec.name)
        {
            ApprovalClass::Sensitive
        } else {
            ApprovalClass::ReadOnly
        }
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        if !validate_computer_session_arguments(&self.spec.name, &args) {
            return Ok(mcp_failure(ClientFailure::InvalidArguments {
                detail: format!("invalid {} arguments", self.spec.name),
            }));
        }
        let call = ComputerUseCall {
            request_id: Uuid::new_v4(),
            name: self.spec.name.clone(),
            arguments: args,
        };
        match self.client.execute(&call).await {
            Ok(result) => Ok(result_tool_output(&result).unwrap_or_else(mcp_failure)),
            Err(ClientFailure::UnknownOutcome { detail }) => {
                // Surface whatever the host recorded so the model can inspect
                // the state instead of blindly retrying.
                match self.client.result_for(&call).await {
                    Ok(result) => Ok(result_tool_output(&result).unwrap_or_else(mcp_failure)),
                    Err(_) => Ok(mcp_failure(ClientFailure::UnknownOutcome { detail })),
                }
            }
            Err(failure) => Ok(mcp_failure(failure)),
        }
    }
}

/// Map a [`ClientFailure`] to a failed [`ToolOutput`] with a redacted message.
fn mcp_failure(failure: ClientFailure) -> ToolOutput {
    ToolOutput::failed(failure.to_tool_error_category(), failure.redacted_text())
}

// ---------------------------------------------------------------------------
// Direct CLI: `tidebreak computer <tool> --json '<arguments>' [--output p]`
// ---------------------------------------------------------------------------

pub(crate) const COMPUTER_USAGE: &str = "\
usage: tidebreak computer <tool> --json '<arguments-json>' [--output <path>]

<tool> is any canonical native or Chrome computer-use tool (run
`tidebreak computer list-tools` for the current set, e.g.
computer_list_windows, computer_capture_screen, computer_click,
computer_type_text, computer_key_press, computer_scroll,
computer_focus_window, computer_return_to_tidebreak, computer_wait).

--json     the tool's argument object, as one JSON string (use '{}' for none)
--output   write the first result image (PNG) to this private path instead of
           discarding it; read the file afterwards to see the pixels

Computer commands use the session-private capfile named by
TIDEBREAK_NATIVE_CAPFILE. They do not take --server/--attach.";

/// One parsed `tidebreak computer …` invocation.
pub(crate) struct ComputerCommand {
    tool: String,
    arguments: Value,
    output: Option<PathBuf>,
}

/// Parse `tidebreak computer …` argv (after the subcommand word).
pub(crate) fn parse_computer(raw: Vec<String>) -> std::result::Result<ComputerCommand, String> {
    let mut tool: Option<String> = None;
    let mut json: Option<String> = None;
    let mut output: Option<PathBuf> = None;
    let mut iter = raw.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => {
                if json.is_some() {
                    return Err("duplicate --json".into());
                }
                json = Some(iter.next().ok_or("--json requires a value")?);
            }
            "--output" => {
                if output.is_some() {
                    return Err("duplicate --output".into());
                }
                output = Some(PathBuf::from(
                    iter.next().ok_or("--output requires a value")?,
                ));
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown option {other}"));
            }
            other => {
                if tool.is_some() {
                    return Err(format!("unexpected argument {other}"));
                }
                tool = Some(other.to_string());
            }
        }
    }
    let tool = tool.ok_or("a tool name is required")?;
    if tool == "list-tools" {
        return Ok(ComputerCommand {
            tool,
            arguments: Value::Null,
            output: None,
        });
    }
    let known = computer_session_tool_specs()
        .iter()
        .any(|spec| spec.name == tool);
    if !known {
        return Err(format!("unknown computer tool {tool}"));
    }
    let json = json.ok_or("computer commands require --json")?;
    let arguments: Value = serde_json::from_str(&json)
        .map_err(|error| format!("--json is not valid JSON: {error}"))?;
    Ok(ComputerCommand {
        tool,
        arguments,
        output,
    })
}

/// Run one `tidebreak computer …` command.
///
/// The result JSON on stdout never contains image bytes. When the result
/// carries images and `--output` names a path, the first image's PNG bytes
/// are written there (private, replaced atomically) and the JSON names the
/// path so the caller can read the actual pixels from disk.
pub(crate) async fn run_computer(command: ComputerCommand) -> Result<()> {
    if command.tool == "list-tools" {
        for spec in computer_session_tool_specs() {
            println!(
                "{}",
                serde_json::json!({"name": spec.name, "description": spec.description, "input_schema": spec.input_schema})
            );
        }
        return Ok(());
    }
    let cap = NativeCapfile::from_env()?;
    let client = NativeClient::new(&cap)?;
    if !validate_computer_session_arguments(&command.tool, &command.arguments) {
        return Err(AgentError::msg(format!(
            "{} arguments are not well-formed",
            command.tool
        )));
    }
    let call = ComputerUseCall {
        request_id: Uuid::new_v4(),
        name: command.tool.clone(),
        arguments: command.arguments.clone(),
    };
    let result = match client.execute(&call).await {
        Ok(result) => result,
        Err(ClientFailure::UnknownOutcome { detail }) => match client.result_for(&call).await {
            Ok(result) => result,
            Err(_) => return Err(AgentError::msg(detail)),
        },
        Err(failure) => return Err(AgentError::msg(failure.redacted_text())),
    };
    let images = decode_result_images(&result).map_err(|f| AgentError::msg(f.redacted_text()))?;
    let mut printed = result_data_without_images(&result);
    printed["image_count"] = Value::from(images.len());
    if images.len() > 1 {
        printed["image_note"] = Value::String("The result contains several images. --output saves the first; use MCP to receive every image.".into());
    }
    if let Some(path) = &command.output {
        match images.first() {
            Some(image) => {
                write_image_private(path, image.data.bytes())?;
                printed["image_file"] = Value::String(path.display().to_string());
                printed["image_width"] = Value::from(image.image_ref.width);
                printed["image_height"] = Value::from(image.image_ref.height);
            }
            None => {
                printed["image_file"] = Value::Null;
            }
        }
    } else if !images.is_empty() {
        printed["image_note"] = Value::String(
            "the result contained an image; re-run with --output <path> to save it".into(),
        );
    }
    println!(
        "{}",
        serde_json::to_string(&printed)
            .map_err(|error| AgentError::msg(format!("JSON encode: {error}")))?
    );
    Ok(())
}

#[cfg(test)]
mod tests;
