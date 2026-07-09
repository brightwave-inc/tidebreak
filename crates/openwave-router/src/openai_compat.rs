//! OpenAI-compatible Chat Completions provider.
//!
//! Speaks the widely-supported `/v1/chat/completions` streaming protocol so the
//! same adapter covers OpenAI itself, OpenRouter, Fireworks, vLLM, LM Studio,
//! and other OpenAI-compatible gateways. Point `base_url` at the gateway's
//! `/v1` root (default: `https://api.openai.com/v1`).
//!
//! Native OpenAI's Responses API is a separate concern; this adapter deliberately
//! targets the Chat Completions shape that local and third-party runtimes share.

use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use serde_json::{json, Value};

use openwave_core::error::{AgentError, Result};
use openwave_core::provider::{
    ChatRequest, ContentBlock, ModelProvider, ProviderEvent, ProviderId, StopReason, Usage,
};
use openwave_core::Role;

use crate::sse::{classify_provider_error, drain_frames, frame_data};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MAX_TOKENS: u32 = 4096;

fn provider_err(err: impl std::fmt::Display) -> AgentError {
    AgentError::Provider(err.to_string())
}

/// A [`ModelProvider`] for any OpenAI-compatible Chat Completions endpoint.
#[derive(Clone)]
pub struct OpenAiCompatProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    /// Stable id reported by [`ModelProvider::id`] — `"openai"` or
    /// `"openai_compatible"` depending on how the caller configured it.
    provider_id: String,
}

impl OpenAiCompatProvider {
    /// Build a provider hitting OpenAI's Chat Completions API.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            provider_id: "openai".to_string(),
        }
    }

    /// Build a provider for a custom OpenAI-compatible gateway.
    pub fn compatible(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: base_url.into(),
            provider_id: "openai_compatible".to_string(),
        }
    }

    /// Override the base URL (the `/v1` root; `/chat/completions` is appended).
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Override the reported provider id.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.provider_id = id.into();
        self
    }
}

#[async_trait]
impl ModelProvider for OpenAiCompatProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(self.provider_id.clone())
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let mut body = build_request_json(&req)?;
        // Native OpenAI omits usage on streaming chunks unless asked. Local
        // openai_compatible servers often reject unknown fields, so only set
        // this for the openai provider id.
        if self.provider_id == "openai" {
            body["stream_options"] = json!({ "include_usage": true });
        }
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let response = self
            .client
            .post(url)
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(provider_err)?;

        let status = response.status();
        if !status.is_success() {
            // Never forward the raw body — gateways sometimes echo key material
            // or request fragments, and `AgentError` strings reach the client.
            let body = response.text().await.unwrap_or_default();
            return Err(classify_provider_error(
                "openai-compat",
                status.as_u16(),
                &body,
            ));
        }

        let stream = async_stream::stream! {
            let mut bytes = response.bytes_stream();
            let mut buffer: Vec<u8> = Vec::new();
            let mut state = StreamState::default();
            while let Some(chunk) = bytes.next().await {
                let Ok(chunk) = chunk else { break };
                buffer.extend_from_slice(&chunk);
                for frame in drain_frames(&mut buffer) {
                    if let Some(data) = frame_data(&frame) {
                        for event in normalize(&data, &mut state) {
                            yield event;
                        }
                    }
                }
            }
            if !buffer.is_empty() {
                let frame = String::from_utf8_lossy(&buffer).into_owned();
                if let Some(data) = frame_data(&frame) {
                    for event in normalize(&data, &mut state) {
                        yield event;
                    }
                }
            }
            // Emit a Stop if the stream ended without a finish_reason (some
            // local servers omit it on clean close).
            if !state.stopped {
                yield ProviderEvent::Stop {
                    reason: if state.saw_tool_call {
                        StopReason::ToolUse
                    } else {
                        StopReason::EndTurn
                    },
                };
            }
        };
        Ok(Box::pin(stream))
    }
}

/// Build the Chat Completions request body from a normalized [`ChatRequest`].
fn build_request_json(req: &ChatRequest) -> Result<Value> {
    let mut messages = Vec::new();
    if let Some(system) = &req.system {
        messages.push(json!({
            "role": "system",
            "content": system,
        }));
    }
    for message in &req.messages {
        extend_openai_messages(&mut messages, message)?;
    }

    let max_tokens = req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": true,
    });
    // Reasoning models (o-series) reject `max_tokens` — they want
    // `max_completion_tokens`. Everything else still speaks `max_tokens`.
    if is_reasoning_model(&req.model) {
        body["max_completion_tokens"] = json!(max_tokens);
    } else {
        body["max_tokens"] = json!(max_tokens);
    }

    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.input_schema,
                        }
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

/// OpenAI reasoning models reject `max_tokens` in favor of
/// `max_completion_tokens`. Match the common `o`-prefix ids (o1, o3, o4-mini, …).
fn is_reasoning_model(model: &str) -> bool {
    let base = model.split('/').next_back().unwrap_or(model);
    let base = base.strip_prefix("openai.").unwrap_or(base);
    matches!(base.as_bytes().first(), Some(b'o'))
        && base.as_bytes().get(1).is_some_and(|b| b.is_ascii_digit())
}

/// Append one normalized message as one or more OpenAI Chat Completions messages.
///
/// Tool results become individual `role: tool` messages (the wire format requires
/// one per `tool_call_id`); everything else collapses into a single message.
fn extend_openai_messages(
    out: &mut Vec<Value>,
    message: &openwave_core::ChatMessage,
) -> Result<()> {
    let tool_results: Vec<_> = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => Some(json!({
                "role": "tool",
                "tool_call_id": tool_use_id,
                "content": content,
            })),
            _ => None,
        })
        .collect();

    // Pure tool-result message(s): emit one OpenAI tool message per result.
    if !tool_results.is_empty()
        && tool_results.len()
            == message
                .content
                .iter()
                .filter(|b| matches!(b, ContentBlock::ToolResult { .. }))
                .count()
        && message
            .content
            .iter()
            .all(|b| matches!(b, ContentBlock::ToolResult { .. }))
    {
        out.extend(tool_results);
        return Ok(());
    }

    let tool_calls: Vec<Value> = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, input } => Some(json!({
                "id": id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": serde_json::to_string(input).unwrap_or_else(|_| "{}".into()),
                }
            })),
            _ => None,
        })
        .collect();

    let text: String = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    let role = match message.role {
        Role::Assistant => "assistant",
        Role::User | Role::System | Role::Tool => "user",
    };

    let mut msg = json!({ "role": role });
    if !text.is_empty() {
        msg["content"] = json!(text);
    } else if tool_calls.is_empty() {
        msg["content"] = json!("");
    }
    if !tool_calls.is_empty() {
        msg["tool_calls"] = Value::Array(tool_calls);
    }
    out.push(msg);
    Ok(())
}

#[derive(Default)]
struct StreamState {
    /// Tool-call argument buffers keyed by the stream-local index.
    tool_calls: std::collections::BTreeMap<u32, ToolCallBuf>,
    usage: Usage,
    stopped: bool,
    saw_tool_call: bool,
}

#[derive(Default)]
struct ToolCallBuf {
    id: String,
    name: String,
    started: bool,
    /// Argument fragments that arrived before id+name were known. Flushed as
    /// `ToolCallArgsDelta`s once [`Self::started`] flips true — never invent a
    /// synthetic call id the gateway won't accept on the tool-result round-trip.
    pending_args: String,
}

/// Map one parsed Chat Completions stream chunk into zero or more events.
fn normalize(data: &Value, state: &mut StreamState) -> Vec<ProviderEvent> {
    let mut events = Vec::new();

    if let Some(usage) = data.get("usage") {
        // Chat Completions usage often arrives on the final chunk. OpenAI's
        // `prompt_tokens` is the full prompt; cached details are optional.
        let prompt = u32_at(usage, "prompt_tokens");
        let cached = usage
            .get("prompt_tokens_details")
            .map(|d| u32_at(d, "cached_tokens"))
            .unwrap_or(0);
        state.usage = Usage {
            input_tokens: prompt.saturating_sub(cached),
            output_tokens: u32_at(usage, "completion_tokens"),
            cache_read_input_tokens: cached,
            cache_creation_input_tokens: 0,
        };
    }

    let Some(choice) = data
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
    else {
        return events;
    };

    if let Some(delta) = choice.get("delta") {
        if let Some(content) = delta.get("content").and_then(Value::as_str) {
            if !content.is_empty() {
                events.push(ProviderEvent::TextDelta {
                    text: content.to_string(),
                });
            }
        }
        // Some gateways stream reasoning as `reasoning_content` or `reasoning`.
        for key in ["reasoning_content", "reasoning"] {
            if let Some(text) = delta.get(key).and_then(Value::as_str) {
                if !text.is_empty() {
                    events.push(ProviderEvent::ReasoningDelta {
                        text: text.to_string(),
                    });
                }
            }
        }
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for tc in tool_calls {
                let index = u32_at(tc, "index");
                let buf = state.tool_calls.entry(index).or_default();
                if let Some(id) = tc.get("id").and_then(Value::as_str) {
                    if !id.is_empty() {
                        buf.id = id.to_string();
                    }
                }
                if let Some(name) = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                {
                    if !name.is_empty() {
                        buf.name = name.to_string();
                    }
                }
                if let Some(args) = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                {
                    if !args.is_empty() {
                        if buf.started {
                            events.push(ProviderEvent::ToolCallArgsDelta {
                                index,
                                fragment: args.to_string(),
                            });
                        } else {
                            buf.pending_args.push_str(args);
                        }
                    }
                }
                // Emit ToolCallStarted only once both id and name are known —
                // never invent a synthetic id. Flush any buffered args after.
                if !buf.started && !buf.id.is_empty() && !buf.name.is_empty() {
                    buf.started = true;
                    state.saw_tool_call = true;
                    events.push(ProviderEvent::ToolCallStarted {
                        index,
                        id: buf.id.clone(),
                        name: buf.name.clone(),
                    });
                    if !buf.pending_args.is_empty() {
                        let fragment = std::mem::take(&mut buf.pending_args);
                        events.push(ProviderEvent::ToolCallArgsDelta { index, fragment });
                    }
                }
            }
        }
    }

    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        if state.usage.input_tokens > 0 || state.usage.output_tokens > 0 {
            events.push(ProviderEvent::Usage(state.usage));
        }
        events.push(ProviderEvent::Stop {
            reason: map_stop_reason(reason),
        });
        state.stopped = true;
    }

    events
}

fn u32_at(value: &Value, key: &str) -> u32 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0) as u32
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "length" => StopReason::MaxTokens,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        _ => StopReason::EndTurn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwave_core::provider::ChatMessage;
    use openwave_core::tool::ToolSpec;

    #[test]
    fn request_maps_system_tools_and_messages() {
        let req = ChatRequest {
            model: "gpt-4o".into(),
            system: Some("be brief".into()),
            messages: vec![ChatMessage::text(Role::User, "hi")],
            tools: vec![ToolSpec {
                name: "read_file".into(),
                description: "read a file".into(),
                input_schema: json!({"type": "object"}),
            }],
            max_tokens: None,
            temperature: Some(0.2),
        };
        let body = build_request_json(&req).unwrap();
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
        assert!(body.get("max_completion_tokens").is_none());
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
        assert!((body["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
    }

    #[test]
    fn reasoning_models_use_max_completion_tokens() {
        let req = ChatRequest {
            model: "o3".into(),
            system: None,
            messages: vec![ChatMessage::text(Role::User, "hi")],
            tools: vec![],
            max_tokens: Some(1024),
            temperature: None,
        };
        let body = build_request_json(&req).unwrap();
        assert_eq!(body["max_completion_tokens"], 1024);
        assert!(body.get("max_tokens").is_none());
        assert!(is_reasoning_model("o4-mini"));
        assert!(is_reasoning_model("o1-preview"));
        assert!(!is_reasoning_model("gpt-4o"));
        assert!(!is_reasoning_model("claude-opus-4-8"));
    }

    #[test]
    fn assistant_tool_use_becomes_tool_calls() {
        let msg = openwave_core::ChatMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "calling".into(),
                },
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: json!({"path": "a"}),
                },
            ],
        };
        let mut out = Vec::new();
        extend_openai_messages(&mut out, &msg).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "assistant");
        assert_eq!(out[0]["content"], "calling");
        assert_eq!(out[0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(out[0]["tool_calls"][0]["function"]["name"], "read_file");
    }

    #[test]
    fn tool_result_becomes_tool_role() {
        let msg = openwave_core::ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".into(),
                content: "ok".into(),
                is_error: false,
            }],
        };
        let mut out = Vec::new();
        extend_openai_messages(&mut out, &msg).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "tool");
        assert_eq!(out[0]["tool_call_id"], "call_1");
        assert_eq!(out[0]["content"], "ok");
    }

    fn run(chunks: &[Value]) -> Vec<ProviderEvent> {
        let mut state = StreamState::default();
        chunks
            .iter()
            .flat_map(|c| normalize(c, &mut state))
            .collect()
    }

    #[test]
    fn normalizes_text_usage_and_stop() {
        let out = run(&[
            json!({"choices":[{"delta":{"content":"he"}}]}),
            json!({"choices":[{"delta":{"content":"llo"}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":4}}}),
        ]);
        assert_eq!(
            out,
            vec![
                ProviderEvent::TextDelta { text: "he".into() },
                ProviderEvent::TextDelta { text: "llo".into() },
                ProviderEvent::Usage(Usage {
                    input_tokens: 6,
                    output_tokens: 2,
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
    fn normalizes_streaming_tool_calls() {
        let out = run(&[
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":""}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"p\""}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":1}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        ]);
        assert_eq!(
            out,
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_1".into(),
                    name: "read_file".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: "{\"p\"".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: ":1}".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse
                },
            ]
        );
    }

    #[test]
    fn buffers_args_until_id_and_name_are_known() {
        // Some gateways send argument fragments before id/name. Never invent a
        // synthetic call id — buffer args and flush once both are present.
        let out = run(&[
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"p\":"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_real","function":{"name":"read_file","arguments":"1}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        ]);
        assert_eq!(
            out,
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_real".into(),
                    name: "read_file".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: "{\"p\":1}".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse
                },
            ]
        );
    }

    #[test]
    fn safe_http_error_omits_raw_body() {
        let msg = crate::sse::safe_http_error(
            "openai-compat",
            401,
            r#"{"error":{"type":"invalid_api_key","message":"sk-leaked"}}"#,
        );
        assert_eq!(msg, "openai-compat returned 401 (invalid_api_key)");
        assert!(!msg.contains("sk-leaked"));
    }

    #[test]
    fn map_stop_reasons() {
        assert_eq!(map_stop_reason("tool_calls"), StopReason::ToolUse);
        assert_eq!(map_stop_reason("length"), StopReason::MaxTokens);
        assert_eq!(map_stop_reason("stop"), StopReason::EndTurn);
    }
}
