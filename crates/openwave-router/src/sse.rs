//! Shared Server-Sent Events framing helpers for streaming provider adapters.
//!
//! Provider streams arrive as byte chunks that may split mid-frame or mid-UTF-8
//! character. These helpers accumulate raw bytes, drain complete frames, and
//! extract the `data:` JSON payload — the same shape Anthropic and OpenAI-compat
//! both speak.

use std::time::Duration;

use futures::{Stream, StreamExt};

/// Maximum provider error bytes inspected for classification.
///
/// Error responses are untrusted and may be arbitrarily large. The adapters
/// stop reading at this boundary before parsing or constructing a durable
/// client-visible failure.
pub const MAX_PROVIDER_ERROR_BODY_BYTES: usize = 8 * 1024;

/// Consume at most [`MAX_PROVIDER_ERROR_BODY_BYTES`] from an HTTP byte stream.
///
/// The caller drops the remaining response stream after this returns. Invalid
/// or truncated UTF-8 is tolerated because the result is only an untrusted
/// classification hint and is never forwarded verbatim.
pub async fn read_bounded_error_body<S, B, E>(chunks: S) -> String
where
    S: Stream<Item = Result<B, E>>,
    B: AsRef<[u8]>,
{
    futures::pin_mut!(chunks);
    let mut body = Vec::with_capacity(MAX_PROVIDER_ERROR_BODY_BYTES);
    while body.len() < MAX_PROVIDER_ERROR_BODY_BYTES {
        let Some(Ok(chunk)) = chunks.next().await else {
            break;
        };
        let bytes = chunk.as_ref();
        let take = bytes.len().min(MAX_PROVIDER_ERROR_BODY_BYTES - body.len());
        body.extend_from_slice(&bytes[..take]);
        if take < bytes.len() {
            break;
        }
    }
    String::from_utf8_lossy(&body).into_owned()
}

/// Naive byte-substring search.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Drain all complete SSE frames from `buffer`, returning each frame's decoded
/// text and leaving any incomplete trailing bytes behind.
///
/// Frames are separated by a blank line (`\n\n` or `\r\n\r\n`). Decoding to UTF-8
/// happens only on a complete frame, so a multi-byte character split across
/// network chunks is never decoded until all its bytes have arrived.
pub fn drain_frames(buffer: &mut Vec<u8>) -> Vec<String> {
    let mut frames = Vec::new();
    loop {
        let lf = find_subslice(buffer, b"\n\n");
        let crlf = find_subslice(buffer, b"\r\n\r\n");
        let (content_end, consume_to) = match (lf, crlf) {
            (Some(i), Some(j)) => {
                if i <= j {
                    (i, i + 2)
                } else {
                    (j, j + 4)
                }
            }
            (Some(i), None) => (i, i + 2),
            (None, Some(j)) => (j, j + 4),
            (None, None) => break,
        };
        frames.push(String::from_utf8_lossy(&buffer[..content_end]).into_owned());
        buffer.drain(..consume_to);
    }
    frames
}

/// Extract the concatenated `data:` payload from one SSE frame.
///
/// Returns `None` for comment/ping frames with no data, or when the payload is
/// the OpenAI stream terminator `[DONE]`.
pub fn frame_data_raw(frame: &str) -> Option<String> {
    let mut data = String::new();
    for line in frame.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data.push_str(rest.trim_start());
        }
    }
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    Some(data)
}

/// Extract and parse the `data:` JSON payload from one SSE frame.
pub fn frame_data(frame: &str) -> Option<serde_json::Value> {
    frame_data_raw(frame).and_then(|data| serde_json::from_str(&data).ok())
}

/// Build a client-safe provider error from structured JSON fields.
///
/// A compact stable `error.type` / `error.code` is included when present. The
/// provider's message is useful for field-level request failures, but can echo
/// credentials or request fragments, so only a short single-line excerpt that
/// passes conservative secret redaction reaches `TurnFailed`.
pub fn safe_http_error(provider: &str, status: u16, body: &str) -> String {
    safe_http_error_redacting(provider, status, body, &[])
}

/// Build a client-safe provider error while treating exact caller-supplied
/// values as secrets.
///
/// Vertex adapters use this for the service-account project id: Google may put
/// the full `projects/{project_id}/...` resource in an otherwise ordinary
/// permission or not-found message. The status and an unrelated enum-style
/// code remain useful, but any code or message containing a known value is
/// omitted whole rather than partially rewritten.
pub fn safe_http_error_redacting(
    provider: &str,
    status: u16,
    body: &str,
    sensitive_values: &[&str],
) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    let raw_code = parsed.as_ref().and_then(provider_error_code);
    let code = raw_code
        .filter(|code| safe_error_code(code) && !contains_sensitive_value(code, sensitive_values));
    let message = raw_code
        .is_none_or(safe_error_code)
        .then(|| {
            parsed
                .as_ref()
                .and_then(provider_error_message)
                .and_then(|message| safe_error_message(message, sensitive_values))
        })
        .flatten();

    let mut result = format!("{provider} returned {status}");
    if let Some(code) = code {
        result.push_str(&format!(" ({code})"));
    }
    if let Some(message) = message {
        result.push_str(": ");
        result.push_str(&message);
    }
    result
}

/// Extract the stable enum-like identifier from provider JSON.
///
/// OpenAI's public API normally nests this under `error`, while its adjacent
/// gateways also use top-level and `detail` envelopes. Check every candidate
/// independently: a present-but-null `code` must not hide a useful `type`.
fn provider_error_code(value: &serde_json::Value) -> Option<&str> {
    let error = value.get("error").filter(|error| error.is_object());
    let detail = value.get("detail").filter(|detail| detail.is_object());
    [error, detail, Some(value)]
        .into_iter()
        .flatten()
        .flat_map(|container| [container.get("type"), container.get("code")])
        .flatten()
        .find_map(serde_json::Value::as_str)
        .or_else(|| {
            value
                .get("error")
                .and_then(serde_json::Value::as_str)
                .filter(|code| safe_error_code(code))
        })
}

/// Prefer the provider's specific `code` for behavioral classification, then
/// fall back to its broader `type`.
fn provider_error_classification_code(value: &serde_json::Value) -> Option<&str> {
    let error = value.get("error").filter(|error| error.is_object());
    let detail = value.get("detail").filter(|detail| detail.is_object());
    [error, detail, Some(value)]
        .into_iter()
        .flatten()
        .flat_map(|container| [container.get("code"), container.get("type")])
        .flatten()
        .find_map(serde_json::Value::as_str)
        .or_else(|| {
            value
                .get("error")
                .and_then(serde_json::Value::as_str)
                .filter(|code| safe_error_code(code))
        })
}

/// Extract a human diagnostic from the bounded provider body.
///
/// In addition to the canonical `{ "error": { "message": ... } }` shape,
/// OpenAI-facing services return top-level `message`, string `error`, string
/// `detail`, and OAuth-style `error_description` fields. Only this selected
/// scalar is considered; arrays and arbitrary JSON are never rendered.
fn provider_error_message(value: &serde_json::Value) -> Option<&str> {
    let error = value.get("error");
    let detail = value.get("detail");
    [
        error.and_then(|error| error.get("message")),
        value.get("message"),
        detail.and_then(|detail| detail.get("message")),
        detail.filter(|detail| detail.is_string()),
        value.get("error_description"),
        error.filter(|error| error.is_string()),
    ]
    .into_iter()
    .flatten()
    .find_map(serde_json::Value::as_str)
}

/// Classify an error delivered *inside* a 200 stream — the in-band counterpart
/// of [`classify_provider_error`].
///
/// Providers can accept a request, start streaming, and then emit an error
/// frame instead of finishing. The status is the numeric `code` some providers
/// send (Gemini), defaulting to 500 when the frame carries only a stable
/// enum-style `type`/`code` token (Anthropic, OpenAI-compatible). The kind
/// mapping and the client-safe message are the HTTP path's own — nothing from
/// the untrusted payload is forwarded beyond a vetted code token — so an
/// in-band `overloaded_error` surfaces exactly like the 529 that never started
/// streaming. A stream that accepted the request has no `Retry-After` header
/// to honor, so the throttling kinds carry no hint.
pub fn classify_in_band_error(
    provider: &str,
    error: &serde_json::Value,
) -> openwave_core::error::AgentError {
    classify_in_band_error_redacting(provider, error, &[])
}

/// Classify an error delivered inside a 200 stream while preventing known
/// values from entering the client-visible message.
///
/// This is the streaming counterpart of
/// [`classify_provider_error_redacting`]. Vertex adapters pass the
/// service-account project id because Google can include the full resource
/// path in an otherwise ordinary permission error frame.
pub fn classify_in_band_error_redacting(
    provider: &str,
    error: &serde_json::Value,
    sensitive_values: &[&str],
) -> openwave_core::error::AgentError {
    let status = error
        .get("code")
        .and_then(serde_json::Value::as_u64)
        .and_then(|code| u16::try_from(code).ok())
        .filter(|code| (100..=599).contains(code))
        .unwrap_or(500);
    classify_provider_error_redacting(provider, status, &error.to_string(), None, sensitive_values)
}

/// Accept only a compact enum-style token. Error fields come from an
/// untrusted gateway and may otherwise contain echoed credentials, prompts, or
/// control characters that would reach the renderer through `TurnFailed`.
fn safe_error_code(code: &str) -> bool {
    const MAX_ERROR_CODE_BYTES: usize = 48;
    let bytes = code.as_bytes();
    matches!(bytes.first(), Some(b'a'..=b'z'))
        && bytes.len() <= MAX_ERROR_CODE_BYTES
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

fn safe_error_message(message: &str, sensitive_values: &[&str]) -> Option<String> {
    const MAX_ERROR_MESSAGE_CHARS: usize = 240;
    let collapsed = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty()
        || contains_secret_marker(&collapsed)
        || contains_sensitive_value(&collapsed, sensitive_values)
    {
        return None;
    }
    let mut excerpt: String = collapsed.chars().take(MAX_ERROR_MESSAGE_CHARS).collect();
    if collapsed.chars().count() > MAX_ERROR_MESSAGE_CHARS {
        excerpt.push('…');
    }
    Some(excerpt)
}

fn contains_sensitive_value(text: &str, sensitive_values: &[&str]) -> bool {
    let lower = text.to_ascii_lowercase();
    sensitive_values
        .iter()
        .any(|value| !value.is_empty() && lower.contains(value.to_ascii_lowercase().as_str()))
}

fn contains_secret_marker(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    if [
        "sk-",
        "aiza",
        "absk",
        "bearer ",
        "api_key",
        "api-key",
        "api key",
        "apikey",
        "authorization:",
        "x-goog-api-key",
        "private_key",
        "private key",
        "access_key_id",
        "access key id",
        "secret_access_key",
        "secret access key",
        "session_token",
        "session token",
        "x-amz-security-token",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return true;
    }

    // AWS credential fields are emitted in several naming styles: process
    // credentials use PascalCase, SDKs commonly use camelCase, and environment
    // variables and shared config use separators. Compare a compact form so
    // every spelling fails closed, including harmless punctuation or spacing
    // inserted around a field name.
    let compact = lower
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(char::from)
        .collect::<String>();
    if [
        "accesskeyid",
        "secretaccesskey",
        "sessiontoken",
        "xamzsecuritytoken",
        "aws4hmacsha256credential",
        "xamzcredential",
    ]
    .iter()
    .any(|marker| compact.contains(marker))
    {
        return true;
    }

    contains_aws_access_key_id(message)
}

/// Recognize a bare 20-character AWS access key ID without treating ordinary
/// words containing `asia` as credentials. Long-term keys use `AKIA`; temporary
/// STS credentials use `ASIA`.
fn contains_aws_access_key_id(message: &str) -> bool {
    const ACCESS_KEY_ID_LEN: usize = 20;
    let bytes = message.as_bytes();
    bytes
        .windows(ACCESS_KEY_ID_LEN)
        .enumerate()
        .any(|(start, candidate)| {
            let has_prefix = candidate[..4].eq_ignore_ascii_case(b"AKIA")
                || candidate[..4].eq_ignore_ascii_case(b"ASIA");
            let has_valid_body = candidate[4..].iter().all(u8::is_ascii_alphanumeric);
            let starts_at_boundary = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
            let end = start + ACCESS_KEY_ID_LEN;
            let ends_at_boundary = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
            has_prefix && has_valid_body && starts_at_boundary && ends_at_boundary
        })
}

/// Longest provider-requested wait taken at face value.
///
/// The header is untrusted; a gateway that asks for a day-long wait would park
/// a turn indefinitely. The retry scheduler applies its own wall-clock envelope
/// on top of this — the clamp only keeps the value in a sane range.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(3600);

/// Read the `Retry-After` hint from a provider's error response.
///
/// Returns `None` when the header is absent, unparseable, or not a form we
/// understand; a missing hint just leaves the caller on its own backoff.
#[cfg(any(
    feature = "anthropic",
    feature = "openai",
    feature = "openai-compat",
    feature = "gemini"
))]
pub fn retry_after_hint(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    parse_retry_after(headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?)
}

/// Parse a `Retry-After` header value into a wait.
///
/// RFC 9110 permits two forms. Delay-seconds is what model providers send and
/// is parsed exactly. The HTTP-date form is accepted and resolved against the
/// current clock; a date already in the past yields a zero wait rather than an
/// error, since the condition it described has passed. Anything else is
/// `None`.
#[must_use]
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds).min(MAX_RETRY_AFTER));
    }
    let deadline = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let wait = deadline.signed_duration_since(chrono::Utc::now());
    Some(wait.to_std().unwrap_or(Duration::ZERO).min(MAX_RETRY_AFTER))
}

/// Classify a provider HTTP error, detecting prompt-too-long patterns that the
/// agent loop can retry with a tighter context budget.
///
/// `retry_after` is the response's parsed `Retry-After`, which rides along on
/// the throttling variants so the turn's retry schedule can honor it instead of
/// guessing.
pub fn classify_provider_error(
    provider: &str,
    status: u16,
    body: &str,
    retry_after: Option<Duration>,
) -> openwave_core::error::AgentError {
    classify_provider_error_redacting(provider, status, body, retry_after, &[])
}

/// Classify a provider HTTP error while preventing known values from entering
/// the client-visible message.
pub fn classify_provider_error_redacting(
    provider: &str,
    status: u16,
    body: &str,
    retry_after: Option<Duration>,
    sensitive_values: &[&str],
) -> openwave_core::error::AgentError {
    use openwave_core::error::{AgentError, ProviderFailure};

    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    let code = parsed
        .as_ref()
        .and_then(provider_error_classification_code)
        .unwrap_or("");
    let message = parsed
        .as_ref()
        .and_then(provider_error_message)
        .unwrap_or("");
    let safe = || safe_http_error_redacting(provider, status, body, sensitive_values);

    if status == 400
        && (code == "context_length_exceeded" || message.contains("prompt is too long"))
    {
        return AgentError::PromptTooLong(safe());
    }
    if matches!(status, 401 | 403) || matches!(code, "authentication_error" | "invalid_api_key") {
        return AgentError::Authentication(safe());
    }
    if status == 429 || code == "rate_limit_error" {
        return AgentError::RateLimited(ProviderFailure::new(safe(), retry_after));
    }
    if matches!(status, 502 | 503 | 504 | 529)
        || matches!(code, "overloaded_error" | "server_overloaded")
    {
        return AgentError::Overloaded(ProviderFailure::new(safe(), retry_after));
    }
    if matches!(
        code,
        "content_policy_violation" | "content_filter" | "refusal"
    ) {
        return AgentError::Refusal(safe());
    }
    if status == 400 || status == 422 || code == "invalid_request_error" {
        return AgentError::InvalidRequest(safe());
    }
    AgentError::Provider(safe())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn provider_error_body_reader_stops_at_the_fixed_byte_limit() {
        let chunks = futures::stream::iter([
            Ok::<_, ()>(vec![b'a'; MAX_PROVIDER_ERROR_BODY_BYTES - 2]),
            Ok(vec![b'b'; 16 * 1024]),
            Ok(b"must not be consumed".to_vec()),
        ]);
        let body = read_bounded_error_body(chunks).await;
        assert_eq!(body.len(), MAX_PROVIDER_ERROR_BODY_BYTES);
        assert!(body.ends_with("bb"));
        assert!(!body.contains("must not be consumed"));
    }

    #[test]
    fn drain_frames_handles_lf_crlf_and_partial_tail() {
        let mut buf = b"data: {\"a\":1}\n\ndata: partial".to_vec();
        let frames = drain_frames(&mut buf);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].contains("{\"a\":1}"));
        assert_eq!(buf, b"data: partial");

        let mut buf = b"event: x\r\ndata: {\"b\":2}\r\n\r\n".to_vec();
        let frames = drain_frames(&mut buf);
        assert_eq!(frames.len(), 1);
        assert_eq!(frame_data(&frames[0]).unwrap()["b"], 2);
        assert!(buf.is_empty());
    }

    #[test]
    fn multibyte_char_split_across_chunks_is_not_corrupted() {
        let full = "data: {\"t\":\"café\"}\n\n".as_bytes().to_vec();
        let split = full.iter().position(|&b| b == 0xC3).unwrap() + 1;
        let (head, tail) = full.split_at(split);

        let mut buf = head.to_vec();
        assert!(drain_frames(&mut buf).is_empty(), "no complete frame yet");
        buf.extend_from_slice(tail);
        let frames = drain_frames(&mut buf);
        assert_eq!(frames.len(), 1);
        assert_eq!(frame_data(&frames[0]).unwrap()["t"], "café");
    }

    #[test]
    fn frame_data_skips_done_terminator() {
        assert!(frame_data_raw("data: [DONE]").is_none());
        assert!(frame_data("event: ping").is_none());
    }

    #[test]
    fn classify_detects_anthropic_prompt_too_long() {
        use openwave_core::error::AgentError;
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 250000 tokens > 200000 maximum"}}"#;
        let err = classify_provider_error("anthropic", 400, body, None);
        assert!(
            matches!(err, AgentError::PromptTooLong(_)),
            "expected PromptTooLong, got {err:?}"
        );
    }

    #[test]
    fn classify_detects_openai_context_length_exceeded() {
        use openwave_core::error::AgentError;
        let body = r#"{"error":{"message":"maximum context length","type":"invalid_request_error","code":"context_length_exceeded"}}"#;
        let err = classify_provider_error("openai-compat", 400, body, None);
        assert!(
            matches!(err, AgentError::PromptTooLong(_)),
            "expected PromptTooLong, got {err:?}"
        );
    }

    #[test]
    fn classify_returns_invalid_request_for_other_400s() {
        use openwave_core::error::AgentError;
        let body = r#"{"error":{"type":"invalid_request_error","message":"invalid api key"}}"#;
        let err = classify_provider_error("anthropic", 400, body, None);
        assert!(
            matches!(err, AgentError::InvalidRequest(_)),
            "expected InvalidRequest, got {err:?}"
        );
    }

    #[test]
    fn classify_distinguishes_auth_rate_limit_overload_and_refusal() {
        use openwave_core::error::AgentError;
        assert!(matches!(
            classify_provider_error("provider", 401, "{}", None),
            AgentError::Authentication(_)
        ));
        assert!(matches!(
            classify_provider_error("provider", 429, "{}", None),
            AgentError::RateLimited(_)
        ));
        assert!(matches!(
            classify_provider_error(
                "anthropic",
                529,
                r#"{"error":{"type":"overloaded_error"}}"#,
                None
            ),
            AgentError::Overloaded(_)
        ));
        assert!(matches!(
            classify_provider_error(
                "provider",
                400,
                r#"{"error":{"code":"content_filter"}}"#,
                None
            ),
            AgentError::Refusal(_)
        ));
    }

    #[test]
    fn in_band_error_classifies_like_the_http_status_path() {
        use openwave_core::error::AgentError;
        // The Anthropic overloaded frame arrives inside a 200 stream, so there
        // is no status to read — the stable `type` token must still land on
        // the same kind as the 529 that never started streaming.
        let error = serde_json::json!({"type": "overloaded_error", "message": "Overloaded"});
        assert!(matches!(
            classify_in_band_error("anthropic", &error),
            AgentError::Overloaded(_)
        ));
        // A numeric `code` (Gemini's shape) is the in-band status.
        let error = serde_json::json!({"code": 401, "message": "bad key"});
        assert!(matches!(
            classify_in_band_error("gemini", &error),
            AgentError::Authentication(_)
        ));
        // Nothing provider-supplied rides into the client-visible message.
        let error = serde_json::json!({
            "code": "rate_limit_error",
            "message": "slow down, key sk-secret"
        });
        let classified = classify_in_band_error("openai-compat", &error);
        assert!(matches!(classified, AgentError::RateLimited(_)));
        assert!(!classified.to_string().contains("sk-secret"));
    }

    #[test]
    fn safe_http_error_rejects_untrusted_code_text() {
        let raw = "sk-secret request fragment\nnext";
        let body = serde_json::json!({
            "error": {
                "code": raw,
                "message": "another secret"
            }
        })
        .to_string();
        let error = safe_http_error("openai-compat", 401, &body);
        assert_eq!(error, "openai-compat returned 401");
        assert!(!error.contains("secret"));
        assert!(!error.contains("fragment"));

        assert_eq!(
            safe_http_error("anthropic", 429, r#"{"error":{"type":"rate_limit_error"}}"#),
            "anthropic returned 429 (rate_limit_error)"
        );
    }

    #[test]
    fn safe_http_error_surfaces_a_bounded_structured_message() {
        let body = serde_json::json!({
            "error": {
                "type": "invalid_request_error",
                "message": format!("Unknown model: {}", "x".repeat(300))
            }
        })
        .to_string();
        let error = safe_http_error("openai-compat", 400, &body);
        assert!(error
            .starts_with("openai-compat returned 400 (invalid_request_error): Unknown model: "));
        assert!(error.ends_with('…'));
        assert!(error.chars().count() < 320);

        let secret = r#"{"error":{"message":"invalid x-goog-api-key abc123"}}"#;
        assert_eq!(
            safe_http_error("gemini", 400, secret),
            "gemini returned 400"
        );
    }

    #[test]
    fn safe_http_error_handles_openai_adjacent_error_envelopes() {
        assert_eq!(
            safe_http_error(
                "openai",
                400,
                r#"{"error":{"message":"Unsupported parameter: temperature","type":"invalid_request_error","param":"temperature","code":null}}"#,
            ),
            "openai returned 400 (invalid_request_error): Unsupported parameter: temperature"
        );
        assert_eq!(
            safe_http_error(
                "openai",
                400,
                r#"{"detail":"Model gpt-example is not available for this account"}"#,
            ),
            "openai returned 400: Model gpt-example is not available for this account"
        );
        assert_eq!(
            safe_http_error(
                "openai",
                400,
                r#"{"error":"invalid_request","error_description":"The requested model is unavailable"}"#,
            ),
            "openai returned 400 (invalid_request): The requested model is unavailable"
        );
    }

    #[test]
    fn safe_http_error_redacts_fallback_openai_messages() {
        assert_eq!(
            safe_http_error(
                "openai",
                400,
                r#"{"detail":"Request rejected for bearer sk-secret"}"#,
            ),
            "openai returned 400"
        );
        assert_eq!(
            safe_http_error(
                "openai",
                400,
                r#"{"detail":[{"msg":"echoed request must not be rendered"}]}"#,
            ),
            "openai returned 400"
        );
    }

    #[test]
    fn bedrock_http_and_in_band_errors_redact_credential_material() {
        let markers = [
            "ABSKDISTINCTIVEBEDROCKKEY",
            "access_key_id=AKIAEXAMPLE",
            "secret_access_key=distinctive-secret",
            "session_token=distinctive-session",
            "x-amz-security-token: distinctive-token",
            "AccessKeyId=AKIAEXAMPLE",
            "SecretAccessKey=distinctive-compact-secret",
            "secretAccessKey: distinctive-camel-secret",
            "SessionToken=distinctive-compact-session",
            "sessionToken: distinctive-camel-session",
            "AKIAIOSFODNN7EXAMPLE",
            "ASIAIOSFODNN7EXAMPLE",
            concat!(
                "AWS4-HMAC-SHA256 Credential=EXAMPLEKEY/20260807/us-east-1/",
                "bedrock-mantle/aws4_request, SignedHeaders=host;x-amz-date, ",
                "Signature=distinctive-signature"
            ),
            concat!(
                "X-Amz-Credential=EXAMPLEKEY%2F20260807%2Fus-east-1%2F",
                "bedrock-mantle%2Faws4_request"
            ),
        ];

        for marker in markers {
            let body = serde_json::json!({
                "error": {
                    "type": "invalid_request_error",
                    "message": format!("provider echoed {marker}")
                }
            })
            .to_string();
            let http = safe_http_error("bedrock", 400, &body);
            assert_eq!(http, "bedrock returned 400 (invalid_request_error)");
            assert!(!http.contains(marker));

            let frame = serde_json::json!({
                "code": 400,
                "message": format!("provider echoed {marker}")
            });
            let in_band = classify_in_band_error("bedrock", &frame).to_string();
            assert!(!in_band.contains(marker));
            assert!(!in_band.contains("provider echoed"));
        }
    }

    #[test]
    fn known_vertex_project_is_removed_while_status_and_code_remain() {
        let project_id = "customer-project-123";
        let body = serde_json::json!({
            "error": {
                "code": "permission_denied",
                "message": format!(
                    "Permission denied on projects/{project_id}/locations/global"
                ),
            }
        })
        .to_string();
        let error = safe_http_error_redacting("vertex", 403, &body, &[project_id]);
        assert_eq!(error, "vertex returned 403 (permission_denied)");
        assert!(!error.contains(project_id));
    }
}
