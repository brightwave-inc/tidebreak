//! The Anthropic provider — the Messages API, normalized to `ProviderEvent`.
//!
//! Talks the native Anthropic Messages format (so extended-thinking and
//! cache-token usage survive), streams Server-Sent Events, and maps each event
//! into the shared [`ProviderEvent`] vocabulary. Pointing `base_url` at a gateway
//! (e.g. agentgateway's native Messages route) is a drop-in for the enterprise
//! path.

use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use serde_json::{json, Value};

use openwave_core::error::{AgentError, Result};
use openwave_core::provider::{
    ChatRequest, ModelProvider, ProviderEvent, ProviderId, StopReason, Usage,
};
use openwave_core::Role;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 4096;

fn provider_err(err: impl std::fmt::Display) -> AgentError {
    AgentError::Provider(err.to_string())
}

/// A [`ModelProvider`] for Anthropic's Messages API.
#[derive(Clone)]
pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl AnthropicProvider {
    /// Build a provider with the given API key, hitting api.anthropic.com.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Override the base URL — e.g. to route through a gateway that speaks the
    /// native Messages API.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("anthropic")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let body = build_request_json(&req)?;
        let url = format!("{}/v1/messages", self.base_url);

        // Setup failures (connection, auth, 4xx/5xx) surface here as `Err` so the
        // router can classify and fail over; the returned stream only yields
        // normalized events.
        let response = self
            .client
            .post(url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(provider_err)?;

        // Surface non-2xx with the provider's error body (the Router needs the
        // body to classify rate-limit vs auth vs bad-request).
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(AgentError::Provider(format!(
                "anthropic returned {status}: {body}"
            )));
        }

        let stream = async_stream::stream! {
            let mut bytes = response.bytes_stream();
            // Accumulate raw BYTES, not a String: a chunk may split a multi-byte
            // UTF-8 character, so we only decode once a whole frame is buffered.
            let mut buffer: Vec<u8> = Vec::new();
            let mut state = StreamState::default();
            while let Some(chunk) = bytes.next().await {
                let Ok(chunk) = chunk else { break }; // mid-stream transport error: end
                buffer.extend_from_slice(&chunk);
                for frame in drain_frames(&mut buffer) {
                    if let Some(data) = frame_data(&frame) {
                        for event in normalize(&data, &mut state) {
                            yield event;
                        }
                    }
                }
            }
            // Flush a final frame that wasn't terminated by a blank line.
            if !buffer.is_empty() {
                let frame = String::from_utf8_lossy(&buffer).into_owned();
                if let Some(data) = frame_data(&frame) {
                    for event in normalize(&data, &mut state) {
                        yield event;
                    }
                }
            }
        };
        Ok(Box::pin(stream))
    }
}

/// Build the Anthropic request body from a normalized [`ChatRequest`].
///
/// Our [`ContentBlock`](openwave_core::ContentBlock) serialization already
/// matches Anthropic's content-block shape, so message content passes through
/// as-is; the system prompt becomes a top-level field, and only `user`/
/// `assistant` roles are valid on messages.
fn build_request_json(req: &ChatRequest) -> Result<Value> {
    let messages = req
        .messages
        .iter()
        .map(|message| {
            Ok(json!({
                "role": anthropic_role(message.role),
                "content": serde_json::to_value(&message.content)?,
            }))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "messages": messages,
        "stream": true,
    });

    if let Some(system) = &req.system {
        body["system"] = json!(system);
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.input_schema,
                    })
                })
                .collect(),
        );
    }
    if let Some(temperature) = req.temperature {
        body["temperature"] = json!(temperature);
    }
    Ok(body)
}

fn anthropic_role(role: Role) -> &'static str {
    // Anthropic messages carry only user/assistant; the system prompt is a
    // top-level field and tool results ride inside user messages.
    match role {
        Role::Assistant => "assistant",
        _ => "user",
    }
}

/// Prompt-cache-aware token counts accumulated across a stream.
#[derive(Default)]
struct StreamState {
    input_tokens: u32,
    cache_read_input_tokens: u32,
    cache_creation_input_tokens: u32,
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
fn drain_frames(buffer: &mut Vec<u8>) -> Vec<String> {
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

/// Extract and parse the `data:` JSON payload from one SSE frame.
fn frame_data(frame: &str) -> Option<Value> {
    let mut data = String::new();
    for line in frame.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data.push_str(rest.trim_start());
        }
    }
    if data.is_empty() {
        return None;
    }
    serde_json::from_str(&data).ok()
}

fn u32_at(value: &Value, key: &str) -> u32 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0) as u32
}

/// Map one parsed Anthropic stream event into zero or more `ProviderEvent`s.
fn normalize(data: &Value, state: &mut StreamState) -> Vec<ProviderEvent> {
    match data.get("type").and_then(Value::as_str) {
        Some("message_start") => {
            if let Some(usage) = data.get("message").and_then(|m| m.get("usage")) {
                state.input_tokens = u32_at(usage, "input_tokens");
                state.cache_read_input_tokens = u32_at(usage, "cache_read_input_tokens");
                state.cache_creation_input_tokens = u32_at(usage, "cache_creation_input_tokens");
            }
            Vec::new()
        }
        Some("content_block_start") => {
            let index = u32_at(data, "index");
            let block = data.get("content_block");
            if block.and_then(|b| b.get("type")).and_then(Value::as_str) == Some("tool_use") {
                let block = block.unwrap();
                vec![ProviderEvent::ToolCallStarted {
                    index,
                    id: str_at(block, "id"),
                    name: str_at(block, "name"),
                }]
            } else {
                Vec::new()
            }
        }
        Some("content_block_delta") => {
            let index = u32_at(data, "index");
            let delta = match data.get("delta") {
                Some(delta) => delta,
                None => return Vec::new(),
            };
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => vec![ProviderEvent::TextDelta {
                    text: str_at(delta, "text"),
                }],
                Some("thinking_delta") => vec![ProviderEvent::ReasoningDelta {
                    text: str_at(delta, "thinking"),
                }],
                Some("input_json_delta") => vec![ProviderEvent::ToolCallArgsDelta {
                    index,
                    fragment: str_at(delta, "partial_json"),
                }],
                _ => Vec::new(),
            }
        }
        Some("message_delta") => {
            let mut events = Vec::new();
            if let Some(usage) = data.get("usage") {
                events.push(ProviderEvent::Usage(Usage {
                    input_tokens: state.input_tokens,
                    output_tokens: u32_at(usage, "output_tokens"),
                    cache_read_input_tokens: state.cache_read_input_tokens,
                    cache_creation_input_tokens: state.cache_creation_input_tokens,
                }));
            }
            if let Some(reason) = data
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(Value::as_str)
            {
                events.push(ProviderEvent::Stop {
                    reason: map_stop_reason(reason),
                });
            }
            events
        }
        _ => Vec::new(),
    }
}

fn str_at(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "max_tokens" => StopReason::MaxTokens,
        "tool_use" => StopReason::ToolUse,
        "stop_sequence" => StopReason::StopSequence,
        // "end_turn" and anything we don't yet model (e.g. refusal, pause_turn)
        // fall back to a clean end.
        _ => StopReason::EndTurn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwave_core::provider::ChatMessage;
    use openwave_core::tool::ToolSpec;

    #[test]
    fn request_maps_system_messages_and_tools() {
        let req = ChatRequest {
            model: "claude-opus-4-8".into(),
            system: Some("be brief".into()),
            messages: vec![ChatMessage::text(Role::User, "hi")],
            tools: vec![ToolSpec {
                name: "read_file".into(),
                description: "read a file".into(),
                input_schema: json!({"type": "object"}),
            }],
            max_tokens: None,
            temperature: Some(0.5),
        };
        let body = build_request_json(&req).unwrap();
        assert_eq!(body["model"], "claude-opus-4-8");
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
        assert_eq!(body["stream"], true);
        assert_eq!(body["system"], "be brief");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        assert_eq!(body["tools"][0]["name"], "read_file");
        assert_eq!(body["temperature"], 0.5);
    }

    fn run(events: &[Value]) -> Vec<ProviderEvent> {
        let mut state = StreamState::default();
        events
            .iter()
            .flat_map(|e| normalize(e, &mut state))
            .collect()
    }

    #[test]
    fn normalizes_text_and_usage_and_stop() {
        let out = run(&[
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 10, "cache_read_input_tokens": 4}}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "he"}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "llo"}}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 7}}),
        ]);
        assert_eq!(
            out,
            vec![
                ProviderEvent::TextDelta { text: "he".into() },
                ProviderEvent::TextDelta { text: "llo".into() },
                ProviderEvent::Usage(Usage {
                    input_tokens: 10,
                    output_tokens: 7,
                    cache_read_input_tokens: 4,
                    cache_creation_input_tokens: 0,
                }),
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn
                },
            ]
        );
    }

    #[test]
    fn normalizes_tool_call_and_reasoning() {
        let out = run(&[
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "read_file"}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"path\":"}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "hmm"}}),
        ]);
        assert_eq!(
            out,
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 1,
                    id: "toolu_1".into(),
                    name: "read_file".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 1,
                    fragment: "{\"path\":".into(),
                },
                ProviderEvent::ReasoningDelta { text: "hmm".into() },
            ]
        );
    }

    #[test]
    fn drain_frames_handles_lf_crlf_and_partial_tail() {
        // LF-separated, plus a partial trailing frame left in the buffer.
        let mut buf = b"data: {\"a\":1}\n\ndata: partial".to_vec();
        let frames = drain_frames(&mut buf);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].contains("{\"a\":1}"));
        assert_eq!(buf, b"data: partial");

        // CRLF-separated frame.
        let mut buf = b"event: x\r\ndata: {\"b\":2}\r\n\r\n".to_vec();
        let frames = drain_frames(&mut buf);
        assert_eq!(frames.len(), 1);
        assert_eq!(frame_data(&frames[0]).unwrap()["b"], 2);
        assert!(buf.is_empty());
    }

    #[test]
    fn multibyte_char_split_across_chunks_is_not_corrupted() {
        // The é in "café" is two bytes (0xC3 0xA9); split the buffer between them
        // to mimic a network chunk boundary landing mid-character.
        let full = "data: {\"t\":\"café\"}\n\n".as_bytes().to_vec();
        let split = full.iter().position(|&b| b == 0xC3).unwrap() + 1;
        let (head, tail) = full.split_at(split);

        let mut buf = head.to_vec();
        assert!(drain_frames(&mut buf).is_empty(), "no complete frame yet");
        buf.extend_from_slice(tail);
        let frames = drain_frames(&mut buf);
        assert_eq!(frames.len(), 1);
        // Would be "caf\u{fffd}\u{fffd}" if we had decoded the partial chunk.
        assert_eq!(frame_data(&frames[0]).unwrap()["t"], "café");
    }

    #[test]
    fn frame_data_extracts_json_and_maps_stop_reasons() {
        let data = frame_data("event: message_stop\ndata: {\"type\":\"message_stop\"}").unwrap();
        assert_eq!(data["type"], "message_stop");
        assert!(frame_data("event: ping").is_none());

        assert_eq!(map_stop_reason("tool_use"), StopReason::ToolUse);
        assert_eq!(map_stop_reason("max_tokens"), StopReason::MaxTokens);
        assert_eq!(map_stop_reason("refusal"), StopReason::EndTurn);
    }
}
