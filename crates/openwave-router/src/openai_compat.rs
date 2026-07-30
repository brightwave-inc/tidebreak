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
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use futures::stream::{BoxStream, StreamExt};
use serde_json::{json, Value};

use openwave_core::error::{AgentError, ProviderErrorInfo, Result};
use openwave_core::provider::{
    ChatRequest, ContentBlock, ModelProvider, ProviderEvent, ProviderId, ResponseFormat,
    StopReason, ToolChoice, Usage,
};
use openwave_core::tool::{strict_json_schema, OptionalProperties};
use openwave_core::{ImageAttachments, Role};

use crate::sse::{
    classify_in_band_error, classify_provider_error, drain_frames, frame_data,
    read_bounded_error_body,
};

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
            client: crate::http::streaming_client(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            provider_id: "openai".to_string(),
        }
    }

    /// Build a provider for a custom OpenAI-compatible gateway.
    pub fn compatible(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            client: crate::http::streaming_client(),
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
            let retry_after = crate::sse::retry_after_hint(response.headers());
            let body = read_bounded_error_body(response.bytes_stream()).await;
            return Err(classify_provider_error(
                "openai-compat",
                status.as_u16(),
                &body,
                retry_after,
            ));
        }

        let ceiling = crate::http::timeouts().total_stream;
        let stream = async_stream::stream! {
            let bytes = crate::http::with_stream_deadline(response.bytes_stream(), ceiling);
            futures::pin_mut!(bytes);
            let mut buffer: Vec<u8> = Vec::new();
            let mut state = StreamState::default();
            while let Some(chunk) = bytes.next().await {
                // A mid-stream transport error must not read as a clean end:
                // the accumulated tool-call arguments may be truncated
                // mid-JSON, and acting on them silently corrupts the step.
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        yield ProviderEvent::Failed {
                            error: ProviderErrorInfo::provider(format!(
                                "stream ended early: {error}"
                            )),
                        };
                        return;
                    }
                };
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
            if !state.stopped && !state.terminal {
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
        extend_openai_messages(&mut messages, message, &req.images)?;
    }

    let max_tokens = req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": true,
    });
    // Reasoning models (o-series) reject `max_tokens` — they want
    // `max_completion_tokens`. Everything else still speaks `max_tokens`.
    if req.reasoning_model {
        body["max_completion_tokens"] = json!(max_tokens);
        // Only reasoning models understand `reasoning_effort`; forwarding it to a
        // plain chat model would be rejected. Absent, the provider's default holds.
        if let Some(effort) = req.reasoning_effort {
            body["reasoning_effort"] = json!(effort.as_str());
        }
    } else {
        body["max_tokens"] = json!(max_tokens);
    }

    if !req.tools.is_empty() {
        body["tools"] = Value::Array(req.tools.iter().map(openai_tool).collect());
    }
    if let Some(choice) = &req.tool_choice {
        body["tool_choice"] = openai_tool_choice(choice)?;
    }
    match &req.response_format {
        Some(ResponseFormat::JsonSchema { name, schema }) => {
            // Without `strict` the schema is a strong suggestion; with it the
            // provider constrains decoding. Refuse the turn rather than send the
            // loose form: a caller that asked for a shape it can parse would
            // otherwise get prose back and no way to tell why.
            let schema =
                strict_json_schema(schema, OptionalProperties::AcceptNull).ok_or_else(|| {
                    AgentError::Provider(format!(
                        "response format {name} has no strict JSON Schema form"
                    ))
                })?;
            body["response_format"] = json!({
                "type": "json_schema",
                "json_schema": { "name": name, "strict": true, "schema": schema },
            });
        }
        None => {}
        // `ResponseFormat` is open. A format this adapter has not learned must
        // fail the request rather than stream an unconstrained answer that only
        // looks like a success.
        Some(other) => {
            return Err(AgentError::Provider(format!(
                "openai-compat cannot enforce response format {other:?}"
            )))
        }
    }
    if let Some(temperature) = req.temperature {
        body["temperature"] = json!(temperature);
    }
    Ok(body)
}

/// Shape one advertised tool as a Chat Completions function tool.
///
/// `strict` is what makes the arguments conform to `parameters` rather than
/// merely resemble it, but it is only offered for a schema that already
/// enumerates every property as required. Widening an optional property to
/// accept `null` — the usual way to satisfy strict mode — would start feeding
/// `null` to tools whose argument types take a `#[serde(default)]` non-`Option`
/// field, turning working calls into deserialization errors. A schema that needs
/// widening therefore goes out unconstrained, exactly as it does today.
fn openai_tool(tool: &openwave_core::tool::ToolSpec) -> Value {
    let mut function = json!({
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.input_schema,
    });
    if let Some(schema) = strict_json_schema(&tool.input_schema, OptionalProperties::Reject) {
        function["parameters"] = schema;
        function["strict"] = json!(true);
    }
    json!({ "type": "function", "function": function })
}

fn openai_tool_choice(choice: &ToolChoice) -> Result<Value> {
    Ok(match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::Required => json!("required"),
        ToolChoice::None => json!("none"),
        ToolChoice::Tool { name } => json!({ "type": "function", "function": { "name": name } }),
        // `ToolChoice` is open so a provider-neutral mode can be added without
        // a breaking change. Silently substituting the model's own judgement
        // for a mode this adapter has not learned would turn "must not call a
        // tool" into "may".
        other => {
            return Err(AgentError::Provider(format!(
                "openai-compat cannot express tool choice {other:?}"
            )))
        }
    })
}

/// Append one normalized message as one or more OpenAI Chat Completions messages.
///
/// Tool results become individual `role: tool` messages (the wire format requires
/// one per `tool_call_id`); everything else collapses into a single message.
///
/// A message carrying images cannot use the plain string `content` form —
/// Chat Completions requires an array of typed parts, with each image as an
/// `image_url` part holding a `data:` URL.
fn extend_openai_messages(
    out: &mut Vec<Value>,
    message: &openwave_core::ChatMessage,
    images: &ImageAttachments,
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

    // OpenAI requires each tool result to be its own `role: tool` message.
    // Preview images follow in a separate user message, preserving the
    // provider-neutral order: result text first, pixels second.
    if !tool_results.is_empty() {
        out.extend(tool_results);
        if message
            .content
            .iter()
            .all(|block| matches!(block, ContentBlock::ToolResult { .. }))
        {
            return Ok(());
        }
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

    let image_blocks: Vec<&openwave_core::ImageRef> = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Image { image } => Some(image),
            _ => None,
        })
        .collect();

    let mut msg = json!({ "role": role });
    if image_blocks.is_empty() {
        if !text.is_empty() {
            msg["content"] = json!(text);
        } else if tool_calls.is_empty() {
            msg["content"] = json!("");
        }
    } else {
        // Text first, then images, mirroring the block order a caller built.
        let mut parts: Vec<Value> = Vec::with_capacity(image_blocks.len() + 1);
        if !text.is_empty() {
            parts.push(json!({ "type": "text", "text": text }));
        }
        for image in image_blocks {
            let data = images.get(image.blob_id).ok_or_else(|| {
                // Reduction turns a deliberately dropped image into a text
                // stand-in, so a missing hydration here is lost bytes rather
                // than an intended omission — fail instead of asking the model
                // about an image it was never sent.
                AgentError::Provider(format!(
                    "image attachment {} has no hydrated bytes",
                    image.blob_id
                ))
            })?;
            parts.push(json!({
                "type": "image_url",
                "image_url": {
                    "url": format!(
                        "data:{};base64,{}",
                        data.media_type().as_str(),
                        BASE64.encode(data.bytes())
                    )
                }
            }));
        }
        msg["content"] = Value::Array(parts);
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
    /// Set once the stream has terminalized on an in-band error. It suppresses
    /// both later frames and the synthetic end-of-stream `Stop` below.
    terminal: bool,
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
    if state.terminal {
        return Vec::new();
    }
    // Gateways accept the request, stream a few chunks, and then push an error
    // object in place of the next chunk. Nothing follows it, so ignoring it
    // would leave the synthetic end-of-stream `Stop` below to report the
    // truncated step as a clean finish.
    if let Some(error) = data.get("error") {
        state.terminal = true;
        return vec![ProviderEvent::Failed {
            error: ProviderErrorInfo::from_error(&classify_in_band_error("openai-compat", error)),
        }];
    }

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
    use openwave_core::ReasoningEffort;

    #[test]
    fn request_maps_system_tools_and_messages() {
        let req = ChatRequest {
            provider: Some(ProviderId::new("openai")),
            model: "gpt-4o".into(),
            reasoning_model: false,
            system: Some("be brief".into()),
            messages: vec![ChatMessage::text(Role::User, "hi")],
            tools: vec![ToolSpec {
                name: "read_file".into(),
                description: "read a file".into(),
                input_schema: json!({"type": "object"}),
            }],
            max_tokens: None,
            temperature: Some(0.2),
            reasoning_effort: Some(ReasoningEffort::High),
            images: ImageAttachments::new(),
            ..Default::default()
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
        // A non-reasoning model must never receive `reasoning_effort`, even when
        // the chat sets one — the field would be rejected by the endpoint.
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn strict_is_offered_only_where_the_schema_already_allows_it() {
        let req = ChatRequest {
            model: "gpt-4o".into(),
            messages: vec![ChatMessage::text(Role::User, "hi")],
            tools: vec![
                ToolSpec {
                    name: "write_file".into(),
                    description: "write a file".into(),
                    input_schema: json!({
                        "type": "object",
                        "properties": { "path": { "type": "string" } },
                        "required": ["path"],
                    }),
                },
                ToolSpec {
                    name: "read_source".into(),
                    description: "read a source".into(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "document_id": { "type": "string" },
                            "offset": { "type": "integer", "minimum": 0 },
                        },
                        "required": ["document_id"],
                    }),
                },
            ],
            response_format: Some(ResponseFormat::JsonSchema {
                name: "note".into(),
                schema: json!({
                    "type": "object",
                    "properties": { "body": { "type": "string" } },
                    "required": ["body"],
                }),
            }),
            ..Default::default()
        };
        let body = build_request_json(&req).unwrap();
        let format = &body["response_format"];
        assert_eq!(format["type"], "json_schema");
        assert_eq!(format["json_schema"]["name"], "note");
        assert_eq!(format["json_schema"]["strict"], true);
        assert_eq!(
            format["json_schema"]["schema"]["additionalProperties"],
            false
        );

        // A schema that already requires every property is constrained.
        assert_eq!(body["tools"][0]["function"]["strict"], true);
        assert_eq!(
            body["tools"][0]["function"]["parameters"]["additionalProperties"],
            false
        );
        // One with an optional property is not: strict mode would require it,
        // and the `null` that makes that safe for the model is not safe for
        // every argument type on our side. It goes out as it does today.
        assert!(body["tools"][1]["function"].get("strict").is_none());
        assert_eq!(
            body["tools"][1]["function"]["parameters"],
            req.tools[1].input_schema
        );
    }

    #[test]
    fn reasoning_models_use_max_completion_tokens() {
        let req = ChatRequest {
            provider: Some(ProviderId::new("openai")),
            model: "o3".into(),
            reasoning_model: true,
            system: None,
            messages: vec![ChatMessage::text(Role::User, "hi")],
            tools: vec![],
            max_tokens: Some(1024),
            temperature: None,
            reasoning_effort: None,
            images: ImageAttachments::new(),
            ..Default::default()
        };
        let body = build_request_json(&req).unwrap();
        assert_eq!(body["max_completion_tokens"], 1024);
        assert!(body.get("max_tokens").is_none());
        // Absent an override, the request carries no `reasoning_effort`.
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn reasoning_models_forward_reasoning_effort() {
        let req = ChatRequest {
            provider: Some(ProviderId::new("openai")),
            model: "o3".into(),
            reasoning_model: true,
            system: None,
            messages: vec![ChatMessage::text(Role::User, "hi")],
            tools: vec![],
            max_tokens: Some(1024),
            temperature: None,
            reasoning_effort: Some(ReasoningEffort::Low),
            images: ImageAttachments::new(),
            ..Default::default()
        };
        let body = build_request_json(&req).unwrap();
        assert_eq!(body["reasoning_effort"], "low");
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
        extend_openai_messages(&mut out, &msg, &ImageAttachments::new()).unwrap();
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
        extend_openai_messages(&mut out, &msg, &ImageAttachments::new()).unwrap();
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
    fn in_band_error_fails_the_stream_and_suppresses_the_synthetic_stop() {
        let mut state = StreamState::default();
        let out: Vec<ProviderEvent> = [
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":"{\"pa"}}]}}]}),
            json!({"error":{"message":"upstream is overloaded","type":"server_error","code":"server_overloaded"}}),
        ]
        .iter()
        .flat_map(|chunk| normalize(chunk, &mut state))
        .collect();

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
                    fragment: "{\"pa".into(),
                },
                ProviderEvent::Failed {
                    error: ProviderErrorInfo {
                        kind: "overloaded".into(),
                        message: "openai-compat returned 500 (server_error)".into(),
                    },
                },
            ]
        );
        // The stream's end-of-stream fallback keys off this flag; without it a
        // truncated tool call would be handed on under a clean `Stop`.
        assert!(state.terminal);
        assert!(!state.stopped);
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

    // ── Image blocks ───────────────────────────────────────────────

    fn png_ref(blob: u128) -> openwave_core::ImageRef {
        openwave_core::ImageRef {
            blob_id: uuid::Uuid::from_u128(blob),
            media_type: openwave_core::ImageMediaType::Png,
            width: 800,
            height: 600,
            byte_len: 3,
        }
    }

    #[test]
    fn an_image_turns_content_into_typed_parts_with_a_data_url() {
        // Chat Completions cannot express an image with the plain string
        // `content` form; it must become an array of typed parts.
        let image = png_ref(1);
        let mut images = ImageAttachments::new();
        images.insert(
            image.blob_id,
            openwave_core::ImageData::new(openwave_core::ImageMediaType::Png, vec![1, 2, 3]),
        );
        let msg = ChatMessage {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "what is this?".into(),
                },
                ContentBlock::Image { image },
            ],
        };

        let mut out = Vec::new();
        extend_openai_messages(&mut out, &msg, &images).unwrap();
        assert_eq!(out.len(), 1);
        let parts = out[0]["content"].as_array().unwrap();
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "what is this?");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(
            parts[1]["image_url"]["url"],
            format!("data:image/png;base64,{}", BASE64.encode([1, 2, 3]))
        );
    }

    #[test]
    fn an_unhydrated_image_fails_the_request_instead_of_being_dropped() {
        let msg = ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Image { image: png_ref(9) }],
        };
        let mut out = Vec::new();
        let err = extend_openai_messages(&mut out, &msg, &ImageAttachments::new()).unwrap_err();
        assert!(err.to_string().contains("no hydrated bytes"), "{err}");
    }

    #[test]
    fn tool_result_text_precedes_its_preview_image() {
        let image = png_ref(7);
        let mut images = ImageAttachments::new();
        images.insert(
            image.blob_id,
            openwave_core::ImageData::new(openwave_core::ImageMediaType::Png, vec![1, 2, 3]),
        );
        let msg = ChatMessage {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: "one preview attached below".into(),
                    is_error: false,
                },
                ContentBlock::Image { image },
            ],
        };

        let mut out = Vec::new();
        extend_openai_messages(&mut out, &msg, &images).unwrap();

        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["role"], "tool");
        assert_eq!(out[0]["content"], "one preview attached below");
        assert_eq!(out[1]["role"], "user");
        assert_eq!(out[1]["content"][0]["type"], "image_url");
    }

    #[test]
    fn a_message_without_images_keeps_the_plain_string_content_form() {
        // Guards against regressing every existing text-only request into the
        // parts form, which some OpenAI-compatible servers do not accept.
        let msg = ChatMessage::text(Role::User, "hi");
        let mut out = Vec::new();
        extend_openai_messages(&mut out, &msg, &ImageAttachments::new()).unwrap();
        assert_eq!(out[0]["content"], "hi");
    }
}
