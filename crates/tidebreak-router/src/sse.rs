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

/// Largest one provider SSE frame retained before normalization.
///
/// Provider frames are untrusted successful-response bytes. A missing blank-line
/// delimiter must not turn the residual buffer into an unbounded allocation,
/// and a delimited frame receives the same ceiling before UTF-8 decoding and
/// JSON parsing. One MiB leaves ample room for ordinary provider events while
/// staying well below the larger durable turn payload limits.
pub const MAX_PROVIDER_SSE_FRAME_BYTES: usize = 1024 * 1024;

/// A provider SSE frame or unterminated residual exceeded the fixed ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SseFrameTooLarge;

impl std::fmt::Display for SseFrameTooLarge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "stream frame exceeds the {MAX_PROVIDER_SSE_FRAME_BYTES}-byte limit"
        )
    }
}

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

fn find_frame_delimiter(buffer: &[u8], start: usize) -> Option<(usize, usize)> {
    for index in start..buffer.len() {
        let remaining = &buffer[index..];
        if remaining.starts_with(b"\n\n") {
            return Some((index, 2));
        }
        if remaining.starts_with(b"\r\n\r\n") {
            return Some((index, 4));
        }
    }
    None
}

/// Incremental provider SSE framer with a retained delimiter scan position.
///
/// Frames are separated by a blank line (`\n\n` or `\r\n\r\n`). Decoding to UTF-8
/// happens only on a complete frame, so a multi-byte character split across
/// network chunks is never decoded until all its bytes have arrived. The scan
/// resumes at the first byte that could participate in a delimiter completed
/// by the next chunk, so an unterminated frame split into tiny chunks remains
/// linear rather than rescanning its whole prefix after every append.
#[derive(Default)]
pub struct SseFramer {
    buffer: Vec<u8>,
    scan_from: usize,
    #[cfg(test)]
    inspected_bytes: usize,
}

fn residual_fits_frame_limit(buffer: &[u8]) -> bool {
    if buffer.len() <= MAX_PROVIDER_SSE_FRAME_BYTES {
        return true;
    }
    matches!(
        &buffer[MAX_PROVIDER_SSE_FRAME_BYTES..],
        b"\n" | b"\r" | b"\r\n" | b"\r\n\r"
    )
}

impl SseFramer {
    const MAX_PARTIAL_DELIMITER_BYTES: usize = 3;

    /// Append one network chunk without allowing either a complete frame or
    /// the incomplete residual to grow beyond
    /// [`MAX_PROVIDER_SSE_FRAME_BYTES`].
    ///
    /// The chunk is admitted in bounded slices, draining complete frames
    /// between slices. This matters because one HTTP body chunk can itself be
    /// much larger than the frame ceiling; extending the residual with the
    /// whole chunk before checking would merely move the unbounded allocation
    /// one layer earlier.
    pub fn push(&mut self, mut chunk: &[u8]) -> Result<Vec<String>, SseFrameTooLarge> {
        let mut frames = Vec::new();
        while !chunk.is_empty() {
            let hard_buffer_limit =
                MAX_PROVIDER_SSE_FRAME_BYTES + Self::MAX_PARTIAL_DELIMITER_BYTES;
            let room = hard_buffer_limit.saturating_sub(self.buffer.len());
            if room == 0 {
                // The only valid residual at this length is an exact-size frame
                // followed by the first three bytes of a CRLF delimiter. Admit
                // its final newline directly, then drain the now-complete frame.
                if residual_fits_frame_limit(&self.buffer) && chunk.first() == Some(&b'\n') {
                    self.buffer.push(b'\n');
                    chunk = &chunk[1..];
                } else {
                    return Err(SseFrameTooLarge);
                }
            } else {
                let take = room.min(chunk.len());
                self.buffer.extend_from_slice(&chunk[..take]);
                chunk = &chunk[take..];
            }

            frames.extend(self.drain()?);

            // Bytes beyond an exact-size frame are admissible only when they
            // are a partial delimiter. This rejects oversized content
            // immediately rather than treating any arbitrary three-byte
            // suffix as framing overhead.
            if !residual_fits_frame_limit(&self.buffer) {
                return Err(SseFrameTooLarge);
            }
        }
        Ok(frames)
    }

    fn drain(&mut self) -> Result<Vec<String>, SseFrameTooLarge> {
        let mut frames = Vec::new();
        let mut frame_start = 0;
        let mut search_from = self.scan_from;
        while let Some((content_end, delimiter_len)) = self.find_delimiter(search_from) {
            if content_end.saturating_sub(frame_start) > MAX_PROVIDER_SSE_FRAME_BYTES {
                return Err(SseFrameTooLarge);
            }
            frames
                .push(String::from_utf8_lossy(&self.buffer[frame_start..content_end]).into_owned());
            frame_start = content_end + delimiter_len;
            search_from = frame_start;
        }
        if frame_start != 0 {
            self.buffer.drain(..frame_start);
        }
        self.scan_from = self
            .buffer
            .len()
            .saturating_sub(Self::MAX_PARTIAL_DELIMITER_BYTES);
        Ok(frames)
    }

    fn find_delimiter(&mut self, start: usize) -> Option<(usize, usize)> {
        #[cfg(test)]
        {
            self.inspected_bytes = self
                .inspected_bytes
                .saturating_add(self.buffer.len().saturating_sub(start));
        }
        find_frame_delimiter(&self.buffer, start)
    }

    /// Decode an unterminated final SSE frame after applying the same byte
    /// ceiling as [`Self::push`].
    pub fn finish(self) -> Result<Option<String>, SseFrameTooLarge> {
        if self.buffer.is_empty() {
            return Ok(None);
        }
        if self.buffer.len() > MAX_PROVIDER_SSE_FRAME_BYTES {
            return Err(SseFrameTooLarge);
        }
        Ok(Some(
            String::from_utf8_lossy(self.buffer.as_slice()).into_owned(),
        ))
    }
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
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    let raw_code = parsed.as_ref().and_then(provider_error_code);
    let code = raw_code.filter(|code| safe_error_code(code));
    let message = raw_code
        .is_none_or(safe_error_code)
        .then(|| {
            parsed
                .as_ref()
                .and_then(provider_error_message)
                .and_then(safe_error_message)
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
) -> tidebreak_core::error::AgentError {
    let status = error
        .get("code")
        .and_then(serde_json::Value::as_u64)
        .and_then(|code| u16::try_from(code).ok())
        .filter(|code| (100..=599).contains(code))
        .unwrap_or(500);
    classify_provider_error(provider, status, &error.to_string(), None)
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

fn safe_error_message(message: &str) -> Option<String> {
    const MAX_ERROR_MESSAGE_CHARS: usize = 240;
    let collapsed = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() || contains_secret_marker(&collapsed) {
        return None;
    }
    let mut excerpt: String = collapsed.chars().take(MAX_ERROR_MESSAGE_CHARS).collect();
    if collapsed.chars().count() > MAX_ERROR_MESSAGE_CHARS {
        excerpt.push('…');
    }
    Some(excerpt)
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

/// The response header a managed model gateway stamps its designed denial
/// code on (`x-model-gateway-error`). The body spells the code differently
/// per compatibility protocol, so the header is the one spelling a client
/// can read without knowing which protocol the request rode.
pub const GATEWAY_ERROR_HEADER: &str = "x-model-gateway-error";

/// The gateway denial code on a response, when the origin stamped one.
#[cfg(any(
    feature = "anthropic",
    feature = "openai",
    feature = "openai-compat",
    feature = "gemini"
))]
#[must_use]
pub fn gateway_denial_code(headers: &reqwest::header::HeaderMap) -> Option<String> {
    Some(headers.get(GATEWAY_ERROR_HEADER)?.to_str().ok()?.to_owned())
}

/// Turn a known gateway denial into its designed, user-facing error, or
/// `None` for a code this client has not learned — the caller then falls
/// back to [`classify_provider_error`], so the code set stays open on the
/// gateway side without a client release riding every addition.
///
/// The distinction earns its keep on two codes. `model_not_granted` arrives
/// as a 404 (so coding harnesses do not read it as a credential problem),
/// which the generic classifier would render as a provider fault rather
/// than "an administrator revoked this". The subscription codes arrive as
/// 403, which the generic classifier would render as *re-authenticate* —
/// exactly the wrong advice, since the session is fine and the fix lives at
/// the gateway's account page.
#[must_use]
pub fn classify_gateway_denial(
    code: &str,
    body: &str,
) -> Option<tidebreak_core::error::AgentError> {
    use tidebreak_core::error::AgentError;

    // The gateway's own message is designed to be shown to the user, but the
    // body is still an untrusted network payload: accept its strings only at
    // toast-sized lengths, and fall back to this client's own copy.
    const MAX_DENIAL_MESSAGE_CHARS: usize = 240;
    const MAX_DENIAL_RESOURCE_CHARS: usize = 100;
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    let message = parsed
        .as_ref()
        .and_then(provider_error_message)
        .map(str::trim)
        .filter(|message| {
            !message.is_empty() && message.chars().count() <= MAX_DENIAL_MESSAGE_CHARS
        });
    // The model the denial is about: top-level on the Anthropic protocol,
    // inside `error` on the OpenAI protocol.
    let resource = parsed
        .as_ref()
        .and_then(|value| {
            value
                .get("resource")
                .or_else(|| value.get("error")?.get("resource"))
        })
        .and_then(serde_json::Value::as_str)
        .filter(|resource| {
            !resource.is_empty() && resource.chars().count() <= MAX_DENIAL_RESOURCE_CHARS
        });

    match code {
        "model_not_granted" => Some(AgentError::Message(match resource {
            Some(model) => format!(
                "You no longer have access to {model} on your gateway — an administrator may have revoked it. Pick another model."
            ),
            None => "You no longer have access to this model on your gateway — an administrator may have revoked it. Pick another model.".to_owned(),
        })),
        "model_not_found" => Some(AgentError::Message(
            message.map_or_else(
                || "Your gateway does not serve this model. Refresh the model list and pick another.".to_owned(),
                str::to_owned,
            ),
        )),
        "subscription_connection_required" | "subscription_client_incompatible" => {
            Some(AgentError::Message(message.map_or_else(
                || "Your gateway needs a provider subscription connected before this model can run. Connect it from the gateway's account page.".to_owned(),
                str::to_owned,
            )))
        }
        _ => None,
    }
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
) -> tidebreak_core::error::AgentError {
    use tidebreak_core::error::{AgentError, ProviderFailure};

    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    let code = parsed
        .as_ref()
        .and_then(provider_error_classification_code)
        .unwrap_or("");
    let message = parsed
        .as_ref()
        .and_then(provider_error_message)
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
        let mut framer = SseFramer::default();
        let frames = framer.push(b"data: {\"a\":1}\n\ndata: partial").unwrap();
        assert_eq!(frames.len(), 1);
        assert!(frames[0].contains("{\"a\":1}"));
        assert_eq!(framer.buffer, b"data: partial");

        let mut framer = SseFramer::default();
        let frames = framer.push(b"event: x\r\ndata: {\"b\":2}\r\n\r\n").unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frame_data(&frames[0]).unwrap()["b"], 2);
        assert!(framer.buffer.is_empty());
    }

    #[test]
    fn multibyte_char_split_across_chunks_is_not_corrupted() {
        let full = "data: {\"t\":\"café\"}\n\n".as_bytes().to_vec();
        let split = full.iter().position(|&b| b == 0xC3).unwrap() + 1;
        let (head, tail) = full.split_at(split);

        let mut framer = SseFramer::default();
        assert!(
            framer.push(head).unwrap().is_empty(),
            "no complete frame yet"
        );
        let frames = framer.push(tail).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frame_data(&frames[0]).unwrap()["t"], "café");
    }

    #[test]
    fn bounded_push_accepts_many_frames_in_one_large_chunk() {
        let frame = b"data: {\"ok\":true}\n\n";
        let repetitions = MAX_PROVIDER_SSE_FRAME_BYTES / frame.len() + 2;
        let chunk = frame.repeat(repetitions);
        let mut framer = SseFramer::default();
        let frames = framer.push(&chunk).unwrap();
        assert_eq!(frames.len(), repetitions);
        assert!(framer.buffer.is_empty());
    }

    #[test]
    fn bounded_push_rejects_an_oversized_delimited_frame() {
        let mut chunk = vec![b'x'; MAX_PROVIDER_SSE_FRAME_BYTES + 1];
        chunk.extend_from_slice(b"\n\n");
        let mut framer = SseFramer::default();
        assert_eq!(framer.push(&chunk), Err(SseFrameTooLarge));
        assert!(framer.buffer.len() <= MAX_PROVIDER_SSE_FRAME_BYTES + 3);
    }

    #[test]
    fn bounded_push_rejects_an_oversized_unterminated_residual() {
        let chunk = vec![b'x'; MAX_PROVIDER_SSE_FRAME_BYTES + 4];
        let mut framer = SseFramer::default();
        assert_eq!(framer.push(&chunk), Err(SseFrameTooLarge));
        assert!(framer.buffer.len() <= MAX_PROVIDER_SSE_FRAME_BYTES + 3);
    }

    #[test]
    fn exact_size_frames_accept_split_lf_and_crlf_delimiters() {
        for delimiter in [b"\n\n".as_slice(), b"\r\n\r\n".as_slice()] {
            let mut framer = SseFramer::default();
            assert!(framer
                .push(&vec![b'x'; MAX_PROVIDER_SSE_FRAME_BYTES])
                .unwrap()
                .is_empty());
            for (index, byte) in delimiter.iter().enumerate() {
                let frames = framer.push(std::slice::from_ref(byte)).unwrap();
                if index + 1 == delimiter.len() {
                    assert_eq!(frames, vec!["x".repeat(MAX_PROVIDER_SSE_FRAME_BYTES)]);
                } else {
                    assert!(frames.is_empty());
                }
            }
            assert!(framer.buffer.is_empty());
        }
    }

    #[test]
    fn delimiter_does_not_excuse_oversized_frame_content() {
        let mut chunk = vec![b'x'; MAX_PROVIDER_SSE_FRAME_BYTES + 1];
        chunk.extend_from_slice(b"\r\n\r\n");
        let mut framer = SseFramer::default();
        assert_eq!(framer.push(&chunk), Err(SseFrameTooLarge));
    }

    #[test]
    fn final_frame_obeys_the_same_ceiling() {
        let mut exact = SseFramer::default();
        assert!(exact
            .push(&vec![b'x'; MAX_PROVIDER_SSE_FRAME_BYTES])
            .unwrap()
            .is_empty());
        assert_eq!(
            exact.finish().unwrap().unwrap().len(),
            MAX_PROVIDER_SSE_FRAME_BYTES
        );

        let oversized = SseFramer {
            buffer: vec![b'x'; MAX_PROVIDER_SSE_FRAME_BYTES + 1],
            ..SseFramer::default()
        };
        assert_eq!(oversized.finish(), Err(SseFrameTooLarge));
    }

    #[test]
    fn delimiter_free_tiny_chunks_are_scanned_linearly() {
        let payload = vec![b'x'; 64 * 1024];
        let mut framer = SseFramer::default();
        for byte in &payload {
            assert!(framer.push(std::slice::from_ref(byte)).unwrap().is_empty());
        }
        assert!(
            framer.inspected_bytes <= payload.len() * 4,
            "inspected {} bytes for a {}-byte payload",
            framer.inspected_bytes,
            payload.len()
        );
    }

    #[test]
    fn frame_data_skips_done_terminator() {
        assert!(frame_data_raw("data: [DONE]").is_none());
        assert!(frame_data("event: ping").is_none());
    }

    #[test]
    fn classify_detects_anthropic_prompt_too_long() {
        use tidebreak_core::error::AgentError;
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 250000 tokens > 200000 maximum"}}"#;
        let err = classify_provider_error("anthropic", 400, body, None);
        assert!(
            matches!(err, AgentError::PromptTooLong(_)),
            "expected PromptTooLong, got {err:?}"
        );
    }

    #[test]
    fn classify_detects_openai_context_length_exceeded() {
        use tidebreak_core::error::AgentError;
        let body = r#"{"error":{"message":"maximum context length","type":"invalid_request_error","code":"context_length_exceeded"}}"#;
        let err = classify_provider_error("openai-compat", 400, body, None);
        assert!(
            matches!(err, AgentError::PromptTooLong(_)),
            "expected PromptTooLong, got {err:?}"
        );
    }

    #[test]
    fn classify_returns_invalid_request_for_other_400s() {
        use tidebreak_core::error::AgentError;
        let body = r#"{"error":{"type":"invalid_request_error","message":"invalid api key"}}"#;
        let err = classify_provider_error("anthropic", 400, body, None);
        assert!(
            matches!(err, AgentError::InvalidRequest(_)),
            "expected InvalidRequest, got {err:?}"
        );
    }

    #[test]
    fn classify_distinguishes_auth_rate_limit_overload_and_refusal() {
        use tidebreak_core::error::AgentError;
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
        use tidebreak_core::error::AgentError;
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
    fn cloud_credential_material_is_redacted_from_http_and_in_band_errors() {
        let markers = [
            "ABSKDISTINCTIVEEXAMPLEKEY",
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
                "service/aws4_request, SignedHeaders=host;x-amz-date, ",
                "Signature=distinctive-signature"
            ),
            concat!(
                "X-Amz-Credential=EXAMPLEKEY%2F20260807%2Fus-east-1%2F",
                "service%2Faws4_request"
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
            let http = safe_http_error("openai_compatible", 400, &body);
            assert_eq!(
                http,
                "openai_compatible returned 400 (invalid_request_error)"
            );
            assert!(!http.contains(marker));

            let frame = serde_json::json!({
                "code": 400,
                "message": format!("provider echoed {marker}")
            });
            let in_band = classify_in_band_error("openai_compatible", &frame).to_string();
            assert!(!in_band.contains(marker));
            assert!(!in_band.contains("provider echoed"));
        }
    }

    /// The gateway spells `code`/`resource` top-level on the Anthropic
    /// protocol and inside `error` on the OpenAI protocol; both must name
    /// the revoked model in the designed copy.
    #[test]
    fn a_gateway_revocation_names_the_model_on_both_protocols() {
        let anthropic = serde_json::json!({
            "type": "error",
            "error": {"type": "not_found_error", "message": "You no longer have access to this model."},
            "code": "model_not_granted",
            "resource": "claude-opus-5",
        })
        .to_string();
        let error = classify_gateway_denial("model_not_granted", &anthropic)
            .expect("a known denial classifies");
        assert!(matches!(
            error,
            tidebreak_core::error::AgentError::Message(_)
        ));
        assert!(error.to_string().contains("claude-opus-5"));
        assert!(error.to_string().contains("no longer have access"));

        let openai = serde_json::json!({
            "error": {
                "type": "not_found_error",
                "message": "You no longer have access to this model.",
                "code": "model_not_granted",
                "resource": "acme-coder",
            }
        })
        .to_string();
        let error = classify_gateway_denial("model_not_granted", &openai)
            .expect("a known denial classifies");
        assert!(error.to_string().contains("acme-coder"));
    }

    /// A subscription denial is a 403 the generic classifier would read as
    /// re-authenticate — the wrong advice, since the fix lives at the
    /// gateway's account page. The gateway's own message rides through.
    #[test]
    fn a_subscription_denial_is_not_an_authentication_error() {
        let body = serde_json::json!({
            "error": {
                "type": "subscription_connection_required",
                "message": "Connect your provider subscription from the Account page, then retry this model request.",
                "code": "subscription_connection_required",
            }
        })
        .to_string();
        let error = classify_gateway_denial("subscription_connection_required", &body)
            .expect("a known denial classifies");
        assert!(matches!(
            error,
            tidebreak_core::error::AgentError::Message(_)
        ));
        assert!(error.to_string().contains("Account page"));
    }

    /// Unknown codes and oversized payloads fall back rather than break:
    /// the denial-code set is the gateway's to grow, and the body is an
    /// untrusted network payload even when the header names a known code.
    #[test]
    fn unknown_denial_codes_and_oversized_copy_degrade() {
        assert!(classify_gateway_denial("model_disabled_next_quarter", "{}").is_none());

        let oversized = serde_json::json!({
            "error": {"message": "x".repeat(4096), "code": "model_not_found"},
        })
        .to_string();
        let error = classify_gateway_denial("model_not_found", &oversized)
            .expect("a known denial classifies");
        assert!(
            error.to_string().contains("Refresh the model list"),
            "an oversized message yields this client's own copy"
        );
        assert!(!error.to_string().contains("xxxx"));
    }
}
