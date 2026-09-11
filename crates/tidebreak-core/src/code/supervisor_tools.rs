//! Bounded native-tool results over Gateway’s ordinary string message transport.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Native tools available to a supervised conversation.
pub const TOOLS: &[&str] = &[
    "code_repos",
    "code_session_create",
    "code_run_turn",
    "code_wait",
    "code_sessions",
    "conversation_read",
    "conversation_export",
    "conversation_attachment",
];
/// Maximum decoded artifact bytes across one result.
pub const MAX_ARTIFACT_BYTES: usize = 2 * 1024 * 1024;
/// Maximum complete serialized result, including base64 artifacts.
pub const MAX_RESULT_BYTES: usize = 3 * 1024 * 1024;
/// Maximum model-facing result JSON.
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// Maximum request JSON. Gateway’s event limit is larger.
pub const MAX_REQUEST_BYTES: usize = 40 * 1024;
/// Maximum string frame length, below Gateway’s 32 KiB message ceiling.
pub const MAX_FRAME_BYTES: usize = 24 * 1024;
const CHUNK_BYTES: usize = 16 * 1024;
const MAX_CHUNKS: usize = MAX_RESULT_BYTES.div_ceil(CHUNK_BYTES);
const PREFIX: &str = "tidebreak-tool-result-v1\n";

/// A complete result returned to one waiting sandbox helper call.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SupervisorToolResult {
    pub request_id: String,
    pub output: serde_json::Value,
    #[serde(default)]
    pub artifacts: Vec<SupervisorArtifact>,
}

/// A file whose bytes belong in sandbox-private conversation scratch.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SupervisorArtifact {
    pub path: String,
    pub media_type: String,
    #[serde(with = "base64_bytes")]
    pub bytes: Vec<u8>,
}

mod base64_bytes {
    use super::*;
    pub fn serialize<S: serde::Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() > MAX_ARTIFACT_BYTES.div_ceil(3) * 4 {
            return Err(serde::de::Error::custom(
                "artifact exceeds the decoded byte limit",
            ));
        }
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(serde::de::Error::custom)
    }
}

pub fn validate_request(request: &super::SupervisorToolRequest) -> Result<(), String> {
    validate_request_id(&request.request_id)?;
    if !TOOLS.contains(&request.tool.as_str()) {
        return Err("this native tool is unavailable".into());
    }
    if !request.arguments.is_object() {
        return Err("native tool arguments must be an object".into());
    }
    if serde_json::to_vec(request)
        .map_err(|e| e.to_string())?
        .len()
        > MAX_REQUEST_BYTES
    {
        return Err("native tool request exceeds the byte limit".into());
    }
    Ok(())
}

fn validate_request_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(
            "native tool request_id must be 1–128 letters, digits, dots, underscores, or hyphens"
                .into(),
        );
    }
    Ok(())
}

impl SupervisorToolResult {
    pub fn failed(request_id: String, message: &str) -> Self {
        Self {
            request_id,
            output: serde_json::json!({"content":message,"is_error":true}),
            artifacts: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_request_id(&self.request_id)?;
        if serde_json::to_vec(&self.output)
            .map_err(|e| e.to_string())?
            .len()
            > MAX_OUTPUT_BYTES
        {
            return Err("native tool output exceeds 64 KiB".into());
        }
        if self.artifacts.len() > 8 {
            return Err("native tool returned too many artifacts".into());
        }
        let mut total = 0_usize;
        let mut paths = std::collections::HashSet::new();
        for artifact in &self.artifacts {
            validate_artifact_path(&artifact.path)?;
            if !paths.insert(&artifact.path) {
                return Err("native tool returned a duplicate artifact path".into());
            }
            if artifact.media_type.len() > 256 || artifact.media_type.contains('\0') {
                return Err("invalid artifact media type".into());
            }
            total = total.saturating_add(artifact.bytes.len());
            if total > MAX_ARTIFACT_BYTES {
                return Err("native tool artifacts exceed 2 MiB".into());
            }
        }
        Ok(())
    }
}

pub fn validate_artifact_path(path: &str) -> Result<(), String> {
    if path.len() > 512
        || path.contains('\0')
        || path.contains('\\')
        || !path.starts_with("conversation/")
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("artifact must name a file under conversation scratch".into());
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    request_id: String,
    index: usize,
    count: usize,
    digest: String,
    data: String,
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Encode complete validated results as strings; no Gateway wire fields change.
pub fn encode_result_frames(result: &SupervisorToolResult) -> Result<Vec<String>, String> {
    result.validate()?;
    let bytes = serde_json::to_vec(result).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_RESULT_BYTES {
        return Err("native tool result exceeds 3 MiB".into());
    }
    let digest = hex_digest(&bytes);
    let count = bytes.len().div_ceil(CHUNK_BYTES);
    bytes
        .chunks(CHUNK_BYTES)
        .enumerate()
        .map(|(index, bytes)| {
            let frame = Frame {
                request_id: result.request_id.clone(),
                index,
                count,
                digest: digest.clone(),
                data: base64::engine::general_purpose::STANDARD.encode(bytes),
            };
            let frame = format!(
                "{PREFIX}{}",
                serde_json::to_string(&frame).map_err(|e| e.to_string())?
            );
            if frame.len() > MAX_FRAME_BYTES {
                return Err("native tool frame exceeds its byte limit".into());
            }
            Ok(frame)
        })
        .collect()
}

/// Whether the message uses the reserved native-tool result prefix.
pub fn is_result_frame(message: &str) -> bool {
    message.starts_with(PREFIX)
}

/// Return a correlation ID only from a bounded valid frame.
pub fn frame_request_id(message: &str) -> Option<String> {
    if message.len() > MAX_FRAME_BYTES {
        return None;
    }
    let frame: Frame = serde_json::from_str(message.strip_prefix(PREFIX)?).ok()?;
    validate_request_id(&frame.request_id).ok()?;
    Some(frame.request_id)
}

/// One awaiting request’s bounded, duplicate-tolerant result assembly.
#[derive(Default)]
pub struct ResultAssembler {
    digest: Option<String>,
    parts: Vec<Option<Vec<u8>>>,
    bytes: usize,
}

impl ResultAssembler {
    pub fn push(
        &mut self,
        request_id: &str,
        message: &str,
    ) -> Result<Option<SupervisorToolResult>, String> {
        if message.len() > MAX_FRAME_BYTES {
            return Err("native tool frame is too large".into());
        }
        let frame: Frame = serde_json::from_str(
            message
                .strip_prefix(PREFIX)
                .ok_or("invalid native tool frame")?,
        )
        .map_err(|_| "invalid native tool frame")?;
        if frame.request_id != request_id
            || frame.count == 0
            || frame.count > MAX_CHUNKS
            || frame.index >= frame.count
            || frame.digest.len() != 64
            || !frame.digest.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("native tool frame has invalid correlation or bounds".into());
        }
        if self
            .digest
            .as_ref()
            .is_some_and(|digest| digest != &frame.digest || self.parts.len() != frame.count)
        {
            return Err("native tool result changed during delivery".into());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(frame.data)
            .map_err(|_| "invalid native tool frame data")?;
        if bytes.is_empty() || bytes.len() > CHUNK_BYTES {
            return Err("native tool chunk is outside its byte bounds".into());
        }
        if self.digest.is_none() {
            self.digest = Some(frame.digest);
            self.parts = vec![None; frame.count];
        }
        if let Some(previous) = &self.parts[frame.index] {
            if previous != &bytes {
                return Err("native tool frame changed during retry".into());
            }
        } else {
            self.bytes = self.bytes.saturating_add(bytes.len());
            if self.bytes > MAX_RESULT_BYTES {
                return Err("native tool result exceeds 3 MiB".into());
            }
            self.parts[frame.index] = Some(bytes);
        }
        if self.parts.iter().any(Option::is_none) {
            return Ok(None);
        }
        let bytes: Vec<u8> = self.parts.iter().flatten().flatten().copied().collect();
        if Some(hex_digest(&bytes)).as_ref() != self.digest.as_ref() {
            return Err("native tool result digest mismatch".into());
        }
        let result: SupervisorToolResult =
            serde_json::from_slice(&bytes).map_err(|_| "invalid native tool result")?;
        if result.request_id != request_id {
            return Err("native tool result correlation mismatch".into());
        }
        result.validate()?;
        Ok(Some(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn large_artifacts_round_trip_as_bounded_strings_with_duplicate_frames() {
        let result = SupervisorToolResult {
            request_id: "request-1".into(),
            output: serde_json::json!({"path":"conversation/export.txt"}),
            artifacts: vec![SupervisorArtifact {
                path: "conversation/export.txt".into(),
                media_type: "text/plain".into(),
                bytes: vec![255; MAX_ARTIFACT_BYTES],
            }],
        };
        let frames = encode_result_frames(&result).unwrap();
        assert!(frames.len() > 1);
        assert!(frames.iter().all(|frame| frame.len() <= MAX_FRAME_BYTES));
        assert!(serde_json::to_vec(&result).unwrap().len() <= MAX_RESULT_BYTES);
        let mut assembler = ResultAssembler::default();
        assert!(assembler.push("request-1", &frames[0]).unwrap().is_none());
        for frame in &frames[..frames.len() - 1] {
            assert!(assembler.push("request-1", frame).unwrap().is_none());
        }
        assert_eq!(
            assembler.push("request-1", frames.last().unwrap()).unwrap(),
            Some(result)
        );
    }
    #[test]
    fn wrong_request_and_aggregate_overflow_are_refused() {
        let result = SupervisorToolResult::failed("request-1".into(), "test");
        let frame = encode_result_frames(&result).unwrap().remove(0);
        assert!(ResultAssembler::default().push("other", &frame).is_err());
        let mut result = result;
        result.artifacts = vec![SupervisorArtifact {
            path: "conversation/a".into(),
            media_type: "text/plain".into(),
            bytes: vec![0; MAX_ARTIFACT_BYTES + 1],
        }];
        assert!(encode_result_frames(&result).is_err());
        for path in [
            "/tmp/file",
            "conversation/../file",
            "conversation/a/./b",
            "conversation//b",
            "output/file",
        ] {
            assert!(validate_artifact_path(path).is_err());
        }
    }
    #[test]
    fn corrupt_or_unbounded_frames_are_refused() {
        let result = SupervisorToolResult::failed("call-1".into(), "test");
        let frame = encode_result_frames(&result).unwrap().remove(0);
        let original: serde_json::Value =
            serde_json::from_str(frame.strip_prefix(PREFIX).unwrap()).unwrap();
        for (key, value) in [
            ("count", serde_json::json!(0)),
            ("count", serde_json::json!(MAX_CHUNKS + 1)),
            ("index", serde_json::json!(1)),
            ("digest", serde_json::json!("0".repeat(64))),
            ("data", serde_json::json!("%%%")),
        ] {
            let mut changed = original.clone();
            changed[key] = value;
            assert!(ResultAssembler::default()
                .push("call-1", &format!("{PREFIX}{changed}"))
                .is_err());
        }
        let mut result = result;
        result.artifacts = vec![SupervisorArtifact {
            path: "conversation/file".into(),
            media_type: "text/plain".into(),
            bytes: vec![1; 32000],
        }];
        let frames = encode_result_frames(&result).unwrap();
        let mut assembler = ResultAssembler::default();
        assert!(assembler.push("call-1", &frames[0]).unwrap().is_none());
        let mut changed: serde_json::Value =
            serde_json::from_str(frames[0].strip_prefix(PREFIX).unwrap()).unwrap();
        changed["data"] = serde_json::json!(
            base64::engine::general_purpose::STANDARD.encode(vec![2; CHUNK_BYTES])
        );
        assert!(assembler
            .push("call-1", &format!("{PREFIX}{changed}"))
            .is_err());
    }
}

/// Prefix for durable steering instructions sent over the ordinary string
/// message transport. Send only to supervisors that support this protocol.
/// The supervisor consumes these frames without using them as ordinary input.
pub const STEER_PREFIX: &str = "tidebreak-steer-v1\n";

/// One durable steering admission framed for the supervised sandbox.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SupervisorSteerFrame {
    /// Active Tidebreak turn the instruction targets.
    pub expected_turn_id: String,
    /// The sandbox incarnation that owns the target native turn. A stale
    /// frame from an older incarnation must never steer a newer one; the
    /// supervisor compares this against the sandbox id it was spawned with.
    pub sandbox_id: String,
    /// Identifies this supervisor process, including restarts within one sandbox.
    pub runtime_id: String,
    /// The agent's current native turn counter; the supervisor only steers
    /// when this matches its running turn.
    pub native_turn: u32,
    /// Caller correlation id, echoed back in `steer_ack`. Required for
    /// sandbox steering so admission settlement is never ambiguous.
    pub correlation_uuid: String,
    /// User text to inject into the running native turn.
    pub body: String,
}

/// Encode a steering admission as an ordinary string message.
pub fn encode_steer_frame(frame: &SupervisorSteerFrame) -> String {
    format!(
        "{STEER_PREFIX}{}",
        serde_json::to_string(frame).expect("a bounded steer frame serializes")
    )
}

/// Whether a message is a steering admission frame.
pub fn is_steer_frame(message: &str) -> bool {
    message.starts_with(STEER_PREFIX)
}

/// Decode a steering admission frame. `None` for malformed frames.
pub fn decode_steer_frame(message: &str) -> Option<SupervisorSteerFrame> {
    let payload = message.strip_prefix(STEER_PREFIX)?;
    serde_json::from_str(payload).ok()
}
