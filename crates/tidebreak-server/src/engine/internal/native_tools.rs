//! Session-native computer-use tools for the internal engine.
//!
//! Decision 94: the internal engine calls the same host-owned native runtime
//! every external harness does, with the same authority. The session worker
//! hands the engine a [`NativeChannelSpec`] naming a session-private
//! capability file; these tools read it once at session launch and drive the
//! loopback `/code/native` routes with the token it carries, so the owner,
//! workspace, and session scope is derived by the server from the token —
//! never asserted by the engine.
//!
//! The tool set is built dynamically from `computer_session_tool_specs()`: a
//! primitive registered in `tidebreak_core::computer_use` is advertised here
//! without an engine change. Capture pixels return as real [`ToolOutput`]
//! image blocks with the same budgets the MCP bridge enforces — decoded
//! images at 1 MiB, response frames at 2 MiB — and never enter model text.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use uuid::Uuid;

use tidebreak_core::computer_session::{
    computer_session_tool_specs, validate_computer_session_arguments,
};
use tidebreak_core::computer_session::{ComputerUseCall, ComputerUseOutcome, ComputerUseResult};
use tidebreak_core::{
    ApprovalClass, DocumentBlob, ImageData, ImageMediaType, ImageRef, Result as CoreResult, Tool,
    ToolCtx, ToolErrorCategory, ToolOutput, ToolSpec, MAX_IMAGE_DIMENSION,
};
use tidebreak_harness::NativeChannelSpec;

/// Hard ceiling on one native response frame.
const NATIVE_FRAME_MAX_BYTES: usize = 2 * 1024 * 1024;

/// Ceiling on one decoded image within a result.
const NATIVE_IMAGE_MAX_BYTES: usize = 1024 * 1024;

/// Maximum bytes the capfile is allowed to be.
const CAPFILE_MAX_BYTES: u64 = 65_536;

const TOKEN_PREFIX: &str = "tbreak_nt_";

/// One session's native tool registrations, shared by every turn the session
/// runs. Built once at launch from the session's capability file.
pub(super) fn native_session_tools(
    native: &NativeChannelSpec,
) -> Result<Vec<Arc<dyn Tool>>, String> {
    let client = Arc::new(NativeSessionClient::from_capfile(&native.capability_file)?);
    Ok(computer_session_tool_specs()
        .into_iter()
        .map(|spec| {
            Arc::new(InternalNativeTool {
                spec,
                client: client.clone(),
            }) as Arc<dyn Tool>
        })
        .collect())
}

/// Delegating wrapper so shared per-session tools can be registered into a
/// per-turn [`tidebreak_core::ToolRegistry`], which owns its tools by `Box`.
pub(crate) struct SharedTool(pub(crate) Arc<dyn Tool>);

#[async_trait::async_trait]
impl Tool for SharedTool {
    fn spec(&self) -> ToolSpec {
        self.0.spec()
    }
    fn approval_class(&self) -> ApprovalClass {
        self.0.approval_class()
    }
    async fn execute(&self, ctx: &ToolCtx, args: Value) -> CoreResult<ToolOutput> {
        self.0.execute(ctx, args).await
    }
}

/// Loopback client for the session's native channel. Never derives `Debug`:
/// the bearer token must not appear in debug output or errors.
struct NativeSessionClient {
    client: reqwest::Client,
    endpoint: String,
    token: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeCapfileWire {
    version: u32,
    endpoint: String,
    token: String,
}

impl NativeSessionClient {
    /// Read and validate the session capability file. Fails closed on
    /// anything but a version-1 loopback `/code/native` endpoint and a
    /// canonical `tbreak_nt_<UUID>` token; nothing secret enters error text.
    fn from_capfile(path: &std::path::Path) -> Result<Self, String> {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| format!("native capfile cannot be read ({error})"))?;
        if !metadata.file_type().is_file() || metadata.len() > CAPFILE_MAX_BYTES {
            return Err("native capfile is not a small regular file".to_owned());
        }
        let raw = read_file_capped(path, CAPFILE_MAX_BYTES as usize)
            .map_err(|error| format!("native capfile cannot be read ({error})"))?;
        let wire: NativeCapfileWire = serde_json::from_str(&raw)
            .map_err(|error| format!("native capfile is not valid JSON ({error})"))?;
        if wire.version != 1 {
            return Err(format!(
                "native capfile version {} is not supported",
                wire.version
            ));
        }
        validate_endpoint(&wire.endpoint)?;
        if wire.token.len() != TOKEN_PREFIX.len() + 36
            || !wire.token.starts_with(TOKEN_PREFIX)
            || Uuid::parse_str(&wire.token[TOKEN_PREFIX.len()..]).is_err()
        {
            return Err("native capfile token has an unexpected shape".to_owned());
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            // Headroom for a consent prompt the user answers slowly.
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|error| format!("could not build native HTTP client ({error})"))?;
        Ok(Self {
            client,
            endpoint: wire.endpoint,
            token: wire.token,
        })
    }

    /// Execute one call. A transport failure retries once with the same
    /// request id — an exact duplicate recovers the stored result instead of
    /// replaying — and an unknown-outcome refusal is answered by fetching
    /// what the host recorded, so the model inspects rather than replays.
    async fn execute(&self, call: &ComputerUseCall) -> Result<ComputerUseResult, ToolFailure> {
        let first = self.post("execute", call).await;
        let result = match first {
            Err(ToolFailure {
                category: ToolErrorCategory::TransportFailed,
                ..
            }) => self.post("execute", call).await,
            other => other,
        };
        match result {
            Err(failure) if failure.unknown_outcome => self.post("result", call).await,
            other => other,
        }
    }

    async fn post(
        &self,
        route: &str,
        call: &ComputerUseCall,
    ) -> Result<ComputerUseResult, ToolFailure> {
        let response = self
            .client
            .post(format!("{}/{route}", self.endpoint))
            .bearer_auth(&self.token)
            .json(call)
            .send()
            .await
            .map_err(|_| ToolFailure::transport("the native channel is unreachable"))?;
        let status = response.status();
        let bytes = read_body_bounded(response, NATIVE_FRAME_MAX_BYTES).await?;
        if status.is_success() {
            return serde_json::from_slice(&bytes)
                .map_err(|_| ToolFailure::transport("the native response did not parse"));
        }
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let kind = body
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let message = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("no detail")
            .to_owned();
        Err(ToolFailure::from_status(status.as_u16(), &kind, &message))
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
            .map_err(|_| ToolFailure::transport("the native response body was unreadable"))?;
        if chunk.len() > max_bytes.saturating_sub(buf.len()) {
            return Err(ToolFailure::failed(format!(
                "the native response exceeded the {max_bytes}-byte frame limit; request a \
                 smaller capture (use app_id/max_dimension for native capture or max_width/max_height for Chrome)"
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

fn validate_endpoint(endpoint: &str) -> Result<(), String> {
    let url: url::Url = endpoint
        .parse()
        .map_err(|_| "native capfile endpoint is not a valid URL".to_owned())?;
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
        || url.path() != "/code/native"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("native capfile endpoint is not a loopback /code/native URL".to_owned());
    }
    Ok(())
}

/// A refusal, sized for the model. Never contains the token or endpoint.
struct ToolFailure {
    category: ToolErrorCategory,
    message: String,
    unknown_outcome: bool,
}

impl ToolFailure {
    fn transport(message: &str) -> Self {
        Self {
            category: ToolErrorCategory::TransportFailed,
            message: message.to_owned(),
            unknown_outcome: false,
        }
    }
    fn failed(message: String) -> Self {
        Self {
            category: ToolErrorCategory::ToolFailed,
            message,
            unknown_outcome: false,
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
            message: format!("({kind}) {message}"),
            unknown_outcome: kind == "native_unknown_outcome",
        }
    }
}

/// One canonical native tool bound to the session channel.
struct InternalNativeTool {
    spec: ToolSpec,
    client: Arc<NativeSessionClient>,
}

#[async_trait::async_trait]
impl Tool for InternalNativeTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn approval_class(&self) -> ApprovalClass {
        if tidebreak_core::is_computer_use_control_tool(&self.spec.name) {
            ApprovalClass::Sensitive
        } else {
            ApprovalClass::ReadOnly
        }
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> CoreResult<ToolOutput> {
        if !validate_computer_session_arguments(&self.spec.name, &args) {
            return Ok(ToolOutput::failed(
                ToolErrorCategory::InvalidArguments,
                format!("invalid {} arguments", self.spec.name),
            ));
        }
        let call = ComputerUseCall {
            request_id: Uuid::new_v4(),
            name: self.spec.name.clone(),
            arguments: args,
        };
        match self.client.execute(&call).await {
            Ok(result) => Ok(result_tool_output(&result)),
            Err(failure) => Ok(ToolOutput::failed(
                failure.category,
                format!("computer: {}", failure.message),
            )),
        }
    }
}

/// Convert one wire result into model text, structured data, and image
/// blocks. Image bytes never enter text or data.
fn result_tool_output(result: &ComputerUseResult) -> ToolOutput {
    let images = match decode_images(result) {
        Ok(images) => images,
        Err(message) => return ToolOutput::failed(ToolErrorCategory::ToolFailed, message),
    };
    let stripped = ComputerUseResult {
        images: Vec::new(),
        ..result.clone()
    };
    let data = serde_json::to_value(&stripped).unwrap_or(Value::Null);
    let text = match result.outcome {
        ComputerUseOutcome::Completed => format!("{}\n\n{}", result.text, stripped.data),
        ComputerUseOutcome::Rejected => {
            return ToolOutput::failed(
                ToolErrorCategory::ToolFailed,
                format!("computer: {}", result.text),
            );
        }
        ComputerUseOutcome::Unknown => format!(
            "{} — the effect of this action is unknown. Capture or read the app to see its \
             current state before acting again.",
            result.text
        ),
    };
    let mut output = ToolOutput::text(text).with_data(data);
    if !images.is_empty() {
        output = output.with_images(images);
    }
    output
}

fn decode_images(result: &ComputerUseResult) -> Result<Vec<(ImageRef, ImageData)>, String> {
    use base64::Engine as _;
    let mut decoded = Vec::with_capacity(result.images.len());
    for image in &result.images {
        if image.mime_type != "image/png" {
            return Err(format!(
                "native image mime type must be image/png, got {}",
                image.mime_type
            ));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&image.base64)
            .map_err(|_| "native image base-64 did not decode".to_owned())?;
        if bytes.len() > NATIVE_IMAGE_MAX_BYTES {
            return Err(format!(
                "native image exceeds the {NATIVE_IMAGE_MAX_BYTES}-byte budget; request a \
                 smaller capture (use app_id/max_dimension for native capture or max_width/max_height for Chrome)"
            ));
        }
        let (width, height) = png_dimensions(&bytes)
            .ok_or_else(|| "native image bytes are not a readable PNG".to_owned())?;
        if width == 0 || height == 0 || width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION
        {
            return Err(format!(
                "native image dimensions {width}×{height} are out of range"
            ));
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
            .map_err(|reason| format!("native image is invalid: {reason}"))?;
        decoded.push((image_ref, ImageData::new(ImageMediaType::Png, bytes)));
    }
    Ok(decoded)
}

/// Width and height from a PNG IHDR header, without decoding pixels.
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    if bytes.len() < 24 || bytes[..8] != SIGNATURE || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_toolset_tracks_the_canonical_registry() {
        let dir = tempfile::tempdir().unwrap();
        let capfile = dir.path().join("cap.json");
        std::fs::write(
            &capfile,
            format!(
                r#"{{"version":1,"endpoint":"http://127.0.0.1:4567/code/native","token":"tbreak_nt_{}"}}"#,
                Uuid::new_v4()
            ),
        )
        .unwrap();
        let native = NativeChannelSpec::new(capfile, "/usr/local/bin/tidebreak".into());
        let tools = native_session_tools(&native).unwrap();
        let canonical = computer_session_tool_specs();
        assert_eq!(tools.len(), canonical.len());
        for (tool, spec) in tools.iter().zip(canonical) {
            assert_eq!(tool.spec().name, spec.name);
            let expected = if tidebreak_core::is_computer_use_control_tool(&spec.name) {
                ApprovalClass::Sensitive
            } else {
                ApprovalClass::ReadOnly
            };
            assert_eq!(tool.approval_class(), expected, "{}", spec.name);
        }
    }

    #[test]
    fn a_malformed_capfile_fails_launch_instead_of_silently_dropping_tools() {
        let dir = tempfile::tempdir().unwrap();
        let capfile = dir.path().join("cap.json");
        std::fs::write(
            &capfile,
            r#"{"version":1,"endpoint":"http://evil.example:80/code/native","token":"tbreak_nt_00000000-0000-4000-8000-000000000000"}"#,
        )
        .unwrap();
        let native = NativeChannelSpec::new(capfile, "/usr/local/bin/tidebreak".into());
        let error = match native_session_tools(&native) {
            Err(error) => error,
            Ok(_) => panic!("non-loopback must fail"),
        };
        assert!(!error.contains("tbreak_nt_"));
    }

    #[test]
    fn png_headers_parse_without_decoding() {
        let mut bytes = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&640u32.to_be_bytes());
        bytes.extend_from_slice(&480u32.to_be_bytes());
        assert_eq!(png_dimensions(&bytes), Some((640, 480)));
        assert_eq!(png_dimensions(b"not a png"), None);
    }
}
