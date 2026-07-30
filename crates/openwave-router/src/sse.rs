//! Shared Server-Sent Events framing helpers for streaming provider adapters.
//!
//! Provider streams arrive as byte chunks that may split mid-frame or mid-UTF-8
//! character. These helpers accumulate raw bytes, drain complete frames, and
//! extract the `data:` JSON payload — the same shape Anthropic and OpenAI-compat
//! both speak.

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

/// Build a client-safe provider error: status + optional stable `error.type` /
/// `error.code` from the JSON body. Never include the raw body (it can echo
/// secrets, and `AgentError` strings reach the client via `TurnFailed`).
pub fn safe_http_error(provider: &str, status: u16, body: &str) -> String {
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            let err = v.get("error").unwrap_or(&v);
            err.get("type")
                .or_else(|| err.get("code"))
                .and_then(serde_json::Value::as_str)
                .filter(|code| safe_error_code(code))
                .map(str::to_owned)
        });
    match detail {
        Some(code) => format!("{provider} returned {status} ({code})"),
        None => format!("{provider} returned {status}"),
    }
}

/// Build a client-safe message for an error delivered *inside* a 200 stream.
///
/// Providers can accept a request, start streaming, and then emit an error
/// frame instead of finishing. There is no HTTP status to report in that case:
/// some providers carry a numeric `code` (Gemini), others only a stable
/// enum-style `type` / `code` token (Anthropic, OpenAI-compatible). As with
/// [`safe_http_error`], nothing else from the untrusted payload is forwarded —
/// these strings reach the client through `TurnFailed`.
pub fn safe_in_band_error(provider: &str, error: &serde_json::Value) -> String {
    let status = error
        .get("code")
        .and_then(serde_json::Value::as_u64)
        .and_then(|code| u16::try_from(code).ok())
        .filter(|code| (100..=599).contains(code))
        .unwrap_or(500);
    let detail = error
        .get("type")
        .or_else(|| error.get("code"))
        .and_then(serde_json::Value::as_str)
        .filter(|code| safe_error_code(code));
    match detail {
        Some(code) => format!("{provider} returned {status} ({code})"),
        None => format!("{provider} returned {status}"),
    }
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

/// Classify a provider HTTP error, detecting prompt-too-long patterns that the
/// agent loop can retry with a tighter context budget.
pub fn classify_provider_error(
    provider: &str,
    status: u16,
    body: &str,
) -> openwave_core::error::AgentError {
    use openwave_core::error::AgentError;

    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    let error = parsed
        .as_ref()
        .map(|value| value.get("error").unwrap_or(value));
    let code = error
        .and_then(|error| error.get("code").or_else(|| error.get("type")))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let safe = || safe_http_error(provider, status, body);

    if status == 400
        && (code == "context_length_exceeded" || message.contains("prompt is too long"))
    {
        return AgentError::PromptTooLong(safe());
    }
    if matches!(status, 401 | 403) || matches!(code, "authentication_error" | "invalid_api_key") {
        return AgentError::Authentication(safe());
    }
    if status == 429 || code == "rate_limit_error" {
        return AgentError::RateLimited(safe());
    }
    if matches!(status, 502 | 503 | 504 | 529)
        || matches!(code, "overloaded_error" | "server_overloaded")
    {
        return AgentError::Overloaded(safe());
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
        let err = classify_provider_error("anthropic", 400, body);
        assert!(
            matches!(err, AgentError::PromptTooLong(_)),
            "expected PromptTooLong, got {err:?}"
        );
    }

    #[test]
    fn classify_detects_openai_context_length_exceeded() {
        use openwave_core::error::AgentError;
        let body = r#"{"error":{"message":"maximum context length","type":"invalid_request_error","code":"context_length_exceeded"}}"#;
        let err = classify_provider_error("openai-compat", 400, body);
        assert!(
            matches!(err, AgentError::PromptTooLong(_)),
            "expected PromptTooLong, got {err:?}"
        );
    }

    #[test]
    fn classify_returns_invalid_request_for_other_400s() {
        use openwave_core::error::AgentError;
        let body = r#"{"error":{"type":"invalid_request_error","message":"invalid api key"}}"#;
        let err = classify_provider_error("anthropic", 400, body);
        assert!(
            matches!(err, AgentError::InvalidRequest(_)),
            "expected InvalidRequest, got {err:?}"
        );
    }

    #[test]
    fn classify_distinguishes_auth_rate_limit_overload_and_refusal() {
        use openwave_core::error::AgentError;
        assert!(matches!(
            classify_provider_error("provider", 401, "{}"),
            AgentError::Authentication(_)
        ));
        assert!(matches!(
            classify_provider_error("provider", 429, "{}"),
            AgentError::RateLimited(_)
        ));
        assert!(matches!(
            classify_provider_error("anthropic", 529, r#"{"error":{"type":"overloaded_error"}}"#),
            AgentError::Overloaded(_)
        ));
        assert!(matches!(
            classify_provider_error("provider", 400, r#"{"error":{"code":"content_filter"}}"#),
            AgentError::Refusal(_)
        ));
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
}
