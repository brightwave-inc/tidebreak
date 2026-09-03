//! OpenAI-compatible Chat Completions provider.
//!
//! Speaks the widely-supported `/v1/chat/completions` streaming protocol so the
//! same adapter covers Fireworks, Together, Ollama, OpenRouter, vLLM, LM Studio,
//! and other OpenAI-compatible gateways. Point `base_url` at the gateway's
//! `/v1` root (default: `https://api.openai.com/v1`).
//!
//! Native OpenAI uses the separate Responses API adapter; this one deliberately
//! targets the Chat Completions shape that local and third-party runtimes share.

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use futures::stream::{BoxStream, StreamExt};
use serde_json::{json, Value};

use tidebreak_core::error::{AgentError, ProviderErrorInfo, Result};
use tidebreak_core::provider::{
    provider_executed_tool_call_text, ChatRequest, ContentBlock, ModelProvider, ProviderEvent,
    ProviderId, ResponseFormat, StopReason, ToolChoice, Usage,
};
use tidebreak_core::tool::{strict_json_schema, OptionalProperties};
use tidebreak_core::{ImageAttachments, Role};

use crate::sse::{
    classify_in_band_error, classify_provider_error, frame_data, read_bounded_error_body, SseFramer,
};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// A [`ModelProvider`] for any OpenAI-compatible Chat Completions endpoint.
#[derive(Clone)]
pub struct OpenAiCompatProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    /// Per-request credential supplier for gateways that mint short-lived
    /// tokens. Takes precedence over `api_key` when present.
    token_source: Option<std::sync::Arc<dyn crate::BearerTokenSource>>,
    /// Whether to declare the request's conversation to a model gateway.
    /// Off for ordinary compatible endpoints, which are not parties to the
    /// host's chat organization.
    conversation_attribution: bool,
    /// Whether to ask the endpoint to include usage in its streaming response.
    /// Off for arbitrary compatible endpoints, which may reject this option.
    streaming_usage: bool,
    /// Stable route-specific id reported by [`ModelProvider::id`] and errors.
    provider_id: String,
}

impl OpenAiCompatProvider {
    /// Build a Chat Completions provider with OpenAI's default API root.
    ///
    /// Native OpenAI routing uses [`crate::OpenAiProvider`]; this constructor
    /// remains for direct embedders that already depend on the adapter.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: crate::http::streaming_client(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            token_source: None,
            conversation_attribution: false,
            streaming_usage: true,
            provider_id: "openai".to_string(),
        }
    }

    /// Build a provider for a custom OpenAI-compatible gateway.
    pub fn compatible(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            client: crate::http::streaming_client(),
            api_key: api_key.into(),
            base_url: base_url.into(),
            token_source: None,
            conversation_attribution: false,
            streaming_usage: false,
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

    /// Fetch the credential from `source` at each request instead of using a
    /// static key. Gateway token sources refresh behind their own lock.
    #[must_use]
    pub fn with_token_source(
        mut self,
        source: std::sync::Arc<dyn crate::BearerTokenSource>,
    ) -> Self {
        self.token_source = Some(source);
        self
    }

    /// Declare each request's conversation to the gateway this provider points
    /// at, so usage and attested tool calls are attributed to the same chat.
    #[must_use]
    pub fn with_conversation_attribution(mut self) -> Self {
        self.conversation_attribution = true;
        self
    }
}

#[async_trait]
impl ModelProvider for OpenAiCompatProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(self.provider_id.clone())
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let mut body = build_request_json_for(&req, &self.provider_id)?;
        // Chat Completions omits usage on streaming chunks unless asked. Local
        // openai_compatible servers often reject unknown fields, so callers
        // opt in only for endpoints known to implement this option.
        if self.streaming_usage {
            body["stream_options"] = json!({ "include_usage": true });
        }
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let authorization = match &self.token_source {
            Some(source) => Some(
                crate::router::authorize_bearer_request(
                    &**source,
                    &req.model,
                    req.wire_model(),
                    req.conversation,
                )
                .await?,
            ),
            None => None,
        };
        let api_key = authorization
            .as_ref()
            .map_or(self.api_key.as_str(), |(token, _lease)| token.as_str());

        let mut request = self
            .client
            .post(url)
            .bearer_auth(api_key)
            .header("content-type", "application/json");
        if let (true, Some(conversation)) = (self.conversation_attribution, req.conversation) {
            request = request.header(
                crate::router::GATEWAY_CONVERSATION_HEADER,
                conversation.to_string(),
            );
        }
        let response = request
            .json(&body)
            .send()
            .await
            // reqwest's display includes the URL, and a gateway URL can carry
            // tenant-identifying parts; `AgentError` strings reach the client
            // via TurnFailed. Only the fact of a failed request surfaces.
            .map_err(|_| AgentError::Provider(format!("{} request failed", self.provider_id)))?;
        drop(authorization);

        let status = response.status();
        if !status.is_success() {
            // Never forward the raw body — gateways sometimes echo key material
            // or request fragments, and `AgentError` strings reach the client.
            let retry_after = crate::sse::retry_after_hint(response.headers());
            // A managed gateway names its designed refusals in a header; a
            // known code renders as its own copy instead of a generic
            // provider fault, and unknown codes fall through.
            let gateway_code = crate::sse::gateway_denial_code(response.headers());
            let body = read_bounded_error_body(response.bytes_stream()).await;
            if let Some(denial) =
                gateway_code.and_then(|code| crate::sse::classify_gateway_denial(&code, &body))
            {
                return Err(denial);
            }
            return Err(classify_provider_error(
                &self.provider_id,
                status.as_u16(),
                &body,
                retry_after,
            ));
        }

        let ceiling = crate::http::timeouts().total_stream;
        let provider_id = self.provider_id.clone();
        let stream = async_stream::stream! {
            let bytes = crate::http::with_stream_deadline(response.bytes_stream(), ceiling);
            futures::pin_mut!(bytes);
            let mut framer = SseFramer::default();
            let mut state = StreamState::new(provider_id);
            while let Some(chunk) = bytes.next().await {
                // A mid-stream transport error must not read as a clean end:
                // the accumulated tool-call arguments may be truncated
                // mid-JSON, and acting on them silently corrupts the step.
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        yield ProviderEvent::Failed {
                            error: ProviderErrorInfo::provider(
                                error.client_message(&state.provider_id),
                            ),
                        };
                        return;
                    }
                };
                let frames = match framer.push(&chunk) {
                    Ok(frames) => frames,
                    Err(error) => {
                        yield ProviderEvent::Failed {
                            error: ProviderErrorInfo::provider(format!(
                                "{} {error}", state.provider_id
                            )),
                        };
                        return;
                    }
                };
                for frame in frames {
                    if let Some(data) = frame_data(&frame) {
                        for event in normalize(&data, &mut state) {
                            yield event;
                        }
                    }
                }
            }
            let final_frame = match framer.finish() {
                Ok(frame) => frame,
                Err(error) => {
                    yield ProviderEvent::Failed {
                        error: ProviderErrorInfo::provider(format!(
                            "{} {error}", state.provider_id
                        )),
                    };
                    return;
                }
            };
            if let Some(frame) = final_frame {
                if let Some(data) = frame_data(&frame) {
                    for event in normalize(&data, &mut state) {
                        yield event;
                    }
                }
            }
            // The stream ended without a finish_reason (some local servers
            // omit it on clean close).
            if !state.stopped && !state.terminal {
                for event in state.end_of_stream() {
                    yield event;
                }
            }
        };
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
fn build_request_json(req: &ChatRequest) -> Result<Value> {
    build_request_json_for(req, "openai-compat")
}

/// Build the Chat Completions request body from a normalized [`ChatRequest`].
fn build_request_json_for(req: &ChatRequest, provider_id: &str) -> Result<Value> {
    let mut messages = Vec::new();
    if let Some(system) = &req.system {
        messages.push(json!({
            "role": "system",
            "content": system,
        }));
    }
    for message in &req.messages {
        extend_openai_messages(
            &mut messages,
            message,
            &req.images,
            provider_id,
            req.provider.as_ref(),
            &req.model,
        )?;
    }

    let max_tokens = req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    let mut body = json!({
        "model": req.wire_model(),
        "messages": messages,
        "stream": true,
    });
    // Chat Completions `prompt_cache_key` is an OpenAI Platform field. Local
    // and third-party compatible servers often reject unknown keys, so only
    // the strict OpenAI id carries it — the same split as `stream_options`.
    if provider_id == "openai" {
        if let Some(conversation) = req.conversation {
            body["prompt_cache_key"] = json!(conversation.to_string());
        }
    }
    // OpenAI reasoning models reject `max_tokens` — they want
    // `max_completion_tokens`. Fireworks and Together expose reasoning models
    // through their compatible endpoints without adopting that OpenAI-only
    // token field, so they keep the ordinary `max_tokens` spelling.
    if req.reasoning_model && !matches!(provider_id, "fireworks" | "together") {
        body["max_completion_tokens"] = json!(max_tokens);
    } else {
        body["max_tokens"] = json!(max_tokens);
    }
    // Only reasoning models understand `reasoning_effort`; forwarding it to a
    // plain chat model would be rejected. Absent, the provider's default holds.
    if req.reasoning_model {
        if let Some(effort) = req
            .reasoning_effort
            .filter(|effort| *effort != tidebreak_core::ReasoningEffort::None)
        {
            body["reasoning_effort"] = json!(effort.as_str());
        }
    }

    if !req.tools.is_empty() {
        body["tools"] = Value::Array(req.tools.iter().map(openai_tool).collect());
    }
    if let Some(choice) = &req.tool_choice {
        body["tool_choice"] = openai_tool_choice(choice, provider_id)?;
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
                        "{provider_id} response format {name} has no strict JSON Schema form"
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
                "{provider_id} cannot enforce response format {other:?}"
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
fn openai_tool(tool: &tidebreak_core::tool::ToolSpec) -> Value {
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

fn openai_tool_choice(choice: &ToolChoice, provider_id: &str) -> Result<Value> {
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
                "{provider_id} cannot express tool choice {other:?}"
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
    message: &tidebreak_core::ChatMessage,
    images: &ImageAttachments,
    provider_id: &str,
    replay_provider: Option<&ProviderId>,
    replay_model: &str,
) -> Result<()> {
    let tool_results: Vec<_> = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                // Chat Completions has no error flag on tool messages, so a
                // failed result carries the signal in-band; without it the
                // model reads a declined or failed call as a success.
                let content = if *is_error {
                    format!("Error: the tool call failed.\n{content}")
                } else {
                    content.clone()
                };
                Some(json!({
                    "role": "tool",
                    "tool_call_id": tool_use_id,
                    "content": content,
                }))
            }
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
            ContentBlock::Text { text } => Some(text.clone()),
            // Chat Completions has no item for a call another provider ran
            // server-side, so it rides along as one compact line of prose.
            ContentBlock::ProviderExecutedToolCall {
                name,
                input,
                output,
                is_error,
                replay: _,
            } => Some(provider_executed_tool_call_text(
                name, input, output, *is_error,
            )),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    let role = match message.role {
        Role::Assistant => "assistant",
        Role::User | Role::System | Role::Tool => "user",
    };

    let image_blocks: Vec<&tidebreak_core::ImageRef> = message
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
                    "{provider_id} image attachment {} has no hydrated bytes",
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
    if message.role == Role::Assistant {
        // Compatible reasoning history is a provider-native assistant field,
        // not visible content. Replay only to the exact provider/model route
        // that minted it; a switch gets the ordinary text/tool projection.
        for block in message
            .reasoning
            .replayable_for(replay_provider, replay_model)
        {
            let Some(block) = block.as_object() else {
                continue;
            };
            for field in ["reasoning_content", "reasoning"] {
                let Some(fragment) = block.get(field).and_then(Value::as_str) else {
                    continue;
                };
                let prior = msg.get(field).and_then(Value::as_str).unwrap_or_default();
                msg[field] = json!(format!("{prior}{fragment}"));
            }
        }
    }
    out.push(msg);
    Ok(())
}

struct StreamState {
    /// Route-specific provider id used in every surfaced stream failure.
    provider_id: String,
    /// Tool-call argument buffers keyed by the stream-local index.
    tool_calls: std::collections::BTreeMap<u32, ToolCallBuf>,
    usage: Usage,
    /// Set once a `ProviderEvent::Usage` has gone out. The consumer sums usage
    /// events, and some servers repeat cumulative counts on several chunks, so
    /// the recorded total must be emitted exactly once per stream.
    usage_emitted: bool,
    stopped: bool,
    /// Set once the stream has terminalized on an in-band error. It suppresses
    /// both later frames and the synthetic end-of-stream event below.
    terminal: bool,
    /// Complete provider-native reasoning fields accumulated across deltas.
    /// They are emitted as opaque replay blocks only when the step finishes
    /// cleanly; a failed or interrupted stream never persists a partial trace.
    reasoning: std::collections::BTreeMap<&'static str, String>,
}

#[cfg(test)]
impl Default for StreamState {
    fn default() -> Self {
        Self::new("openai-compat")
    }
}

impl StreamState {
    fn new(provider_id: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            tool_calls: std::collections::BTreeMap::new(),
            usage: Usage::default(),
            usage_emitted: false,
            stopped: false,
            terminal: false,
            reasoning: std::collections::BTreeMap::new(),
        }
    }

    fn take_reasoning_blocks(&mut self) -> Vec<ProviderEvent> {
        std::mem::take(&mut self.reasoning)
            .into_iter()
            .filter(|(_, content)| !content.is_empty())
            .map(|(field, content)| ProviderEvent::ReasoningBlock {
                data: json!({ field: content }),
            })
            .collect()
    }

    /// True once a usage total worth reporting has been recorded but not yet
    /// emitted. Zero usage stays silent — a server that never reports usage
    /// records zero, exactly as before.
    fn has_unemitted_usage(&self) -> bool {
        !self.usage_emitted && (self.usage.input_tokens > 0 || self.usage.output_tokens > 0)
    }

    /// What the end of the byte stream means when no `finish_reason` arrived.
    ///
    /// Some local servers omit `finish_reason` on a clean close, so text alone
    /// still gets the synthetic `Stop` — preceded by any recorded usage, which
    /// otherwise has no chunk left to carry it out. An *announced* tool call is
    /// different: a silently dropped connection is indistinguishable from an
    /// omitted `finish_reason` on the wire, and the call's argument JSON may be
    /// truncated mid-value, so the conservative reading fails the step rather
    /// than committing the fragment as a finished call.
    fn end_of_stream(&mut self) -> Vec<ProviderEvent> {
        if self.tool_calls.values().any(|call| call.started) {
            vec![ProviderEvent::Failed {
                error: ProviderErrorInfo::provider(format!(
                    "{} stream ended mid-tool-call",
                    self.provider_id
                )),
            }]
        } else {
            let mut events = self.take_reasoning_blocks();
            if self.has_unemitted_usage() {
                events.push(ProviderEvent::Usage(self.usage));
            }
            events.push(ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            });
            events
        }
    }
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
            error: ProviderErrorInfo::from_error(&classify_in_band_error(
                &state.provider_id,
                error,
            )),
        }];
    }

    let mut events = Vec::new();

    if let Some(usage) = data.get("usage") {
        // With `stream_options.include_usage`, native OpenAI reports usage in a
        // trailing chunk with `"choices": []`, *after* the `finish_reason`
        // chunk. Other servers attach it to the finish chunk itself. Record it
        // either way; if the stream already stopped, this trailing chunk is the
        // only chance to emit it. OpenAI's `prompt_tokens` is the full prompt;
        // cached details are optional.
        let prompt = usage_u64_at(usage, "prompt_tokens");
        let cached = usage
            .get("prompt_tokens_details")
            .map(|d| usage_u64_at(d, "cached_tokens"))
            .unwrap_or(0);
        state.usage = Usage {
            input_tokens: saturating_u32(prompt.saturating_sub(cached)),
            output_tokens: saturating_u32(usage_u64_at(usage, "completion_tokens")),
            cache_read_input_tokens: saturating_u32(cached),
            cache_creation_input_tokens: 0,
        };
        if state.stopped && state.has_unemitted_usage() {
            state.usage_emitted = true;
            events.push(ProviderEvent::Usage(state.usage));
        }
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
                    state.reasoning.entry(key).or_default().push_str(text);
                    events.push(ProviderEvent::ReasoningDelta {
                        text: text.to_string(),
                    });
                }
            }
        }
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for tc in tool_calls {
                let index = match stream_index(tc, state) {
                    Ok(index) => index,
                    Err(error) => return vec![ProviderEvent::Failed { error }],
                };
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
        events.extend(state.take_reasoning_blocks());
        if state.has_unemitted_usage() {
            state.usage_emitted = true;
            events.push(ProviderEvent::Usage(state.usage));
        }
        events.push(ProviderEvent::Stop {
            reason: map_stop_reason(reason),
        });
        state.stopped = true;
    }

    events
}

fn usage_u64_at(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn saturating_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn stream_index(
    data: &Value,
    state: &mut StreamState,
) -> std::result::Result<u32, ProviderErrorInfo> {
    let Some(value) = data.get("index") else {
        return Ok(0);
    };
    value
        .as_u64()
        .and_then(|index| u32::try_from(index).ok())
        .ok_or_else(|| {
            state.terminal = true;
            ProviderErrorInfo::provider(format!(
                "{} returned an invalid tool-call index",
                state.provider_id
            ))
        })
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
    use tidebreak_core::provider::{ChatMessage, MessageReasoning, ReasoningOrigin};
    use tidebreak_core::tool::ToolSpec;
    use tidebreak_core::ReasoningEffort;

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
        assert!(body.get("prompt_cache_key").is_none());

        let conversation = tidebreak_core::id::SessionId::new();
        let with_conversation = ChatRequest {
            conversation: Some(conversation),
            ..req
        };
        // The test helper speaks as an arbitrary compatible backend.
        assert!(build_request_json(&with_conversation)
            .unwrap()
            .get("prompt_cache_key")
            .is_none());
        let openai = build_request_json_for(&with_conversation, "openai").unwrap();
        assert_eq!(openai["prompt_cache_key"], conversation.to_string());
        let fireworks = build_request_json_for(&with_conversation, "fireworks").unwrap();
        assert!(fireworks.get("prompt_cache_key").is_none());
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
                    name: "read_document".into(),
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
            model: "gpt-5.6-sol".into(),
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
            model: "gpt-5.6-sol".into(),
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
    fn hosted_kimi_reasoning_uses_compatible_token_and_effort_fields() {
        let req = ChatRequest {
            provider: Some(ProviderId::new("fireworks")),
            model: "accounts/fireworks/models/kimi-k3".into(),
            reasoning_model: true,
            messages: vec![ChatMessage::text(Role::User, "hi")],
            max_tokens: Some(4096),
            reasoning_effort: Some(ReasoningEffort::Max),
            ..Default::default()
        };
        let body = build_request_json_for(&req, "fireworks").unwrap();
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["reasoning_effort"], "max");
        assert!(body.get("max_completion_tokens").is_none());
    }

    #[test]
    fn compatible_reasoning_replays_only_to_the_exact_route() {
        let model = "accounts/fireworks/models/kimi-k3";
        let assistant = ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "answer".into(),
            }],
            reasoning: MessageReasoning::captured(
                ReasoningOrigin {
                    provider: Some(ProviderId::new("fireworks")),
                    model: model.into(),
                },
                vec![json!({"reasoning_content": "private plan"})],
            ),
        };
        let request = |provider: &str, model: &str| ChatRequest {
            provider: Some(ProviderId::new(provider)),
            model: model.into(),
            messages: vec![assistant.clone(), ChatMessage::text(Role::User, "continue")],
            ..Default::default()
        };

        let same = build_request_json_for(&request("fireworks", model), "fireworks").unwrap();
        assert_eq!(same["messages"][0]["reasoning_content"], "private plan");

        let other_provider =
            build_request_json_for(&request("together", model), "together").unwrap();
        assert!(other_provider["messages"][0]
            .get("reasoning_content")
            .is_none());

        let other_model =
            build_request_json_for(&request("fireworks", "other-model"), "fireworks").unwrap();
        assert!(other_model["messages"][0]
            .get("reasoning_content")
            .is_none());
    }

    #[test]
    fn gpt_5_6_none_uses_the_provider_default() {
        let req = ChatRequest {
            provider: Some(ProviderId::new("openai")),
            model: "gpt-5.6-sol".into(),
            reasoning_model: true,
            messages: vec![ChatMessage::text(Role::User, "hi")],
            max_tokens: Some(1024),
            reasoning_effort: Some(ReasoningEffort::None),
            ..Default::default()
        };
        let body = build_request_json(&req).unwrap();
        assert!(body.get("reasoning_effort").is_none());
        assert_eq!(
            body["messages"],
            json!([{ "role": "user", "content": "hi" }])
        );
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn assistant_tool_use_becomes_tool_calls() {
        let msg = tidebreak_core::ChatMessage {
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
            reasoning: MessageReasoning::default(),
        };
        let mut out = Vec::new();
        extend_openai_messages(
            &mut out,
            &msg,
            &ImageAttachments::new(),
            "openai-compat",
            None,
            "test-model",
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "assistant");
        assert_eq!(out[0]["content"], "calling");
        assert_eq!(out[0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(out[0]["tool_calls"][0]["function"]["name"], "read_file");
    }

    #[test]
    fn tool_result_becomes_tool_role() {
        let msg = tidebreak_core::ChatMessage {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: "ok".into(),
                    is_error: false,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "call_2".into(),
                    content: "not run: the user declined.".into(),
                    is_error: true,
                },
            ],
            reasoning: MessageReasoning::default(),
        };
        let mut out = Vec::new();
        extend_openai_messages(
            &mut out,
            &msg,
            &ImageAttachments::new(),
            "openai-compat",
            None,
            "test-model",
        )
        .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["role"], "tool");
        assert_eq!(out[0]["tool_call_id"], "call_1");
        assert_eq!(out[0]["content"], "ok");
        // The wire format has no error flag, so the failure marker in the
        // content is the only signal the model gets.
        assert_eq!(out[1]["role"], "tool");
        assert_eq!(out[1]["tool_call_id"], "call_2");
        assert_eq!(
            out[1]["content"],
            "Error: the tool call failed.\nnot run: the user declined."
        );
    }

    fn run(chunks: &[Value]) -> Vec<ProviderEvent> {
        let mut state = StreamState::default();
        chunks
            .iter()
            .flat_map(|c| normalize(c, &mut state))
            .collect()
    }

    #[test]
    fn usage_counts_saturate_instead_of_wrapping() {
        let events = run(&[json!({
            "usage": {
                "prompt_tokens": u64::from(u32::MAX) + 1,
                "prompt_tokens_details": {"cached_tokens": 1},
                "completion_tokens": u64::from(u32::MAX) + 1
            },
            "choices": [{"delta": {}, "finish_reason": "stop"}]
        })]);
        assert!(events.iter().any(|event| matches!(
            event,
            ProviderEvent::Usage(Usage {
                input_tokens: u32::MAX,
                output_tokens: u32::MAX,
                cache_read_input_tokens: 1,
                ..
            })
        )));
    }

    #[test]
    fn oversized_tool_call_index_fails_the_stream() {
        let events = run(&[json!({
            "choices": [{"delta": {"tool_calls": [{
                "index": u64::from(u32::MAX) + 1,
                "id": "call_1",
                "function": {"name": "read_file", "arguments": "{}"}
            }]}}]
        })]);
        assert!(matches!(events.as_slice(), [ProviderEvent::Failed { .. }]));
    }

    #[test]
    fn present_non_unsigned_tool_call_indices_cannot_alias_an_open_call() {
        for invalid in [json!(-1), json!(1.5), json!("0"), Value::Null] {
            let mut state = StreamState::new("together");
            let events: Vec<ProviderEvent> = [
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":"{\"path\":\""}}]}}]}),
                json!({"choices":[{"delta":{"tool_calls":[{"index":invalid,"function":{"arguments":"a\"}"}}]}}]}),
            ]
            .iter()
            .flat_map(|chunk| normalize(chunk, &mut state))
            .collect();

            assert!(matches!(
                events.as_slice(),
                [
                    ProviderEvent::ToolCallStarted { index: 0, .. },
                    ProviderEvent::ToolCallArgsDelta { index: 0, fragment },
                    ProviderEvent::Failed { .. },
                ] if fragment == "{\"path\":\""
            ));
            assert!(state.terminal);
        }
    }

    #[test]
    fn in_band_error_fails_the_stream_and_suppresses_the_synthetic_stop() {
        let mut state = StreamState::new("together");
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
                        message: "together returned 500 (server_error): upstream is overloaded"
                            .into(),
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
    fn a_silent_close_mid_tool_call_fails_instead_of_stopping_cleanly() {
        // A clean TCP close mid-response carries no transport error and no
        // finish_reason. An announced tool call whose argument stream never
        // closed is truncation evidence, so the end-of-stream fallback fails
        // the step rather than committing the fragment. This changes behavior
        // on streams that reported success before: a complete call whose
        // server dropped only `finish_reason` now fails too — the two are
        // indistinguishable on the wire.
        let mut state = StreamState::new("fireworks");
        let out: Vec<ProviderEvent> = [
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":"{\"pa"}}]}}]}),
        ]
        .iter()
        .flat_map(|chunk| normalize(chunk, &mut state))
        .collect();
        assert!(matches!(
            out.last(),
            Some(ProviderEvent::ToolCallArgsDelta { .. })
        ));
        let ending = state.end_of_stream();
        assert_eq!(
            ending,
            vec![ProviderEvent::Failed {
                error: ProviderErrorInfo::provider("fireworks stream ended mid-tool-call"),
            }]
        );

        // Text alone keeps the synthetic `Stop`: some local servers omit
        // `finish_reason` on every clean close.
        let mut state = StreamState::default();
        let _ = normalize(&json!({"choices":[{"delta":{"content":"hi"}}]}), &mut state);
        assert_eq!(
            state.end_of_stream(),
            vec![ProviderEvent::Stop {
                reason: StopReason::EndTurn
            }]
        );
    }

    #[test]
    fn normalizes_text_usage_and_stop() {
        // Native OpenAI with `stream_options.include_usage`: usage arrives in a
        // trailing chunk with empty `choices`, after the finish chunk. This is
        // the shape that used to lose usage entirely (#1089).
        let out = run(&[
            json!({"choices":[{"delta":{"content":"he"}}]}),
            json!({"choices":[{"delta":{"content":"llo"}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
            json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":4}}}),
        ]);
        assert_eq!(
            out,
            vec![
                ProviderEvent::TextDelta { text: "he".into() },
                ProviderEvent::TextDelta { text: "llo".into() },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn
                },
                ProviderEvent::Usage(Usage {
                    input_tokens: 6,
                    output_tokens: 2,
                    cache_read_input_tokens: 4,
                    cache_creation_input_tokens: 0,
                }),
            ]
        );
    }

    #[test]
    fn reasoning_content_is_captured_whole_before_a_tool_stop() {
        let out = run(&[
            json!({"choices":[{"delta":{"reasoning_content":"plan "}}]}),
            json!({"choices":[{"delta":{"reasoning_content":"carefully","tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":"{}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        ]);
        assert_eq!(
            out,
            vec![
                ProviderEvent::ReasoningDelta {
                    text: "plan ".into()
                },
                ProviderEvent::ReasoningDelta {
                    text: "carefully".into()
                },
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_1".into(),
                    name: "read_file".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: "{}".into(),
                },
                ProviderEvent::ReasoningBlock {
                    data: json!({"reasoning_content": "plan carefully"}),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ]
        );
    }

    #[test]
    fn usage_on_the_finish_chunk_is_emitted_once() {
        // Some compatible servers attach usage to the finish chunk itself, and
        // a few repeat the cumulative total on a trailing chunk too. The total
        // must go out exactly once — the consumer sums usage events.
        let out = run(&[
            json!({"choices":[{"delta":{"content":"hi"}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2}}),
            json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2}}),
        ]);
        assert_eq!(
            out,
            vec![
                ProviderEvent::TextDelta { text: "hi".into() },
                ProviderEvent::Usage(Usage {
                    input_tokens: 10,
                    output_tokens: 2,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                }),
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn
                },
            ]
        );
    }

    #[test]
    fn usage_without_a_finish_reason_survives_the_end_of_stream_fallback() {
        // A server that reports usage but omits `finish_reason` on a clean
        // close: the synthetic end-of-stream Stop must not drop the total.
        let mut state = StreamState::default();
        let _ = normalize(&json!({"choices":[{"delta":{"content":"hi"}}]}), &mut state);
        let _ = normalize(
            &json!({"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":3}}),
            &mut state,
        );
        assert_eq!(
            state.end_of_stream(),
            vec![
                ProviderEvent::Usage(Usage {
                    input_tokens: 7,
                    output_tokens: 3,
                    cache_read_input_tokens: 0,
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

    fn png_ref(blob: u128) -> tidebreak_core::ImageRef {
        tidebreak_core::ImageRef {
            blob_id: uuid::Uuid::from_u128(blob),
            media_type: tidebreak_core::ImageMediaType::Png,
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
            tidebreak_core::ImageData::new(tidebreak_core::ImageMediaType::Png, vec![1, 2, 3]),
        );
        let msg = ChatMessage {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "what is this?".into(),
                },
                ContentBlock::Image { image },
            ],
            reasoning: MessageReasoning::default(),
        };

        let mut out = Vec::new();
        extend_openai_messages(&mut out, &msg, &images, "openai-compat", None, "test-model")
            .unwrap();
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

    #[tokio::test]
    async fn a_gateway_unhydrated_image_failure_keeps_the_public_provider_id() {
        let image = png_ref(9);
        let blob_id = image.blob_id;
        let msg = ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Image { image }],
            reasoning: MessageReasoning::default(),
        };
        let result = compatible_router(
            crate::RouteKind::ModelGatewayOpenai,
            "127.0.0.1:1".parse().unwrap(),
        )
        .stream(ChatRequest {
            provider: Some(ProviderId::new("model_gateway")),
            model: "hosted-model".into(),
            messages: vec![msg],
            ..Default::default()
        })
        .await;
        let err = match result {
            Err(error) => error,
            Ok(_) => panic!("the gateway unexpectedly accepted an unhydrated image"),
        };
        match err {
            AgentError::Provider(message) => assert_eq!(
                message,
                format!("model_gateway image attachment {blob_id} has no hydrated bytes")
            ),
            other => panic!("wrong unhydrated-image error: {other:?}"),
        }
    }

    #[test]
    fn tool_result_text_precedes_its_preview_image() {
        let image = png_ref(7);
        let mut images = ImageAttachments::new();
        images.insert(
            image.blob_id,
            tidebreak_core::ImageData::new(tidebreak_core::ImageMediaType::Png, vec![1, 2, 3]),
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
            reasoning: MessageReasoning::default(),
        };

        let mut out = Vec::new();
        extend_openai_messages(&mut out, &msg, &images, "openai-compat", None, "test-model")
            .unwrap();

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
        extend_openai_messages(
            &mut out,
            &msg,
            &ImageAttachments::new(),
            "openai-compat",
            None,
            "test-model",
        )
        .unwrap();
        assert_eq!(out[0]["content"], "hi");
    }

    #[tokio::test]
    async fn compatible_provider_keeps_the_chat_completions_endpoint() {
        use axum::extract::{Request, State};
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::routing::post;
        use axum::Router;
        use tokio::sync::oneshot;

        async fn capture(
            State(tx): State<std::sync::Arc<std::sync::Mutex<Option<oneshot::Sender<String>>>>>,
            request: Request,
        ) -> impl IntoResponse {
            if let Some(tx) = tx.lock().unwrap().take() {
                let _ = tx.send(request.uri().path().to_owned());
            }
            (
                StatusCode::OK,
                [("content-type", "text/event-stream")],
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n"
                ),
            )
        }

        let (tx, rx) = oneshot::channel();
        let state = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
        let app = Router::new().fallback(post(capture)).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let provider = OpenAiCompatProvider::compatible("key", format!("http://{address}/v1"));
        let stream = provider
            .stream(ChatRequest {
                model: "local-model".into(),
                messages: vec![ChatMessage::text(Role::User, "hi")],
                ..Default::default()
            })
            .await
            .unwrap();
        let events: Vec<_> = stream.collect().await;
        assert_eq!(rx.await.unwrap(), "/v1/chat/completions");
        assert_eq!(
            events,
            vec![
                ProviderEvent::TextDelta { text: "ok".into() },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn
                },
            ]
        );
    }

    struct StaticGatewayTokenSource;

    #[async_trait::async_trait]
    impl crate::BearerTokenSource for StaticGatewayTokenSource {
        async fn bearer_token(&self) -> tidebreak_core::Result<String> {
            Ok("mg_at_test".to_string())
        }
    }

    fn compatible_router(kind: crate::RouteKind, address: std::net::SocketAddr) -> crate::Router {
        let is_gateway = kind == crate::RouteKind::ModelGatewayOpenai;
        crate::Router::build(vec![crate::Route {
            kind,
            api_key: if is_gateway {
                String::new()
            } else {
                "hosted-key".to_string()
            },
            base_url: Some(format!("http://{address}/v1")),
            curated_models: vec!["hosted-model".to_string()],
            model_rewrites: std::collections::HashMap::new(),
            token_source: is_gateway.then(|| {
                std::sync::Arc::new(StaticGatewayTokenSource)
                    as std::sync::Arc<dyn crate::BearerTokenSource>
            }),
            chatgpt_account_id: None,
        }])
    }

    #[tokio::test]
    async fn compatible_route_http_errors_keep_the_public_provider_id() {
        use axum::http::{header, StatusCode};
        use axum::response::IntoResponse;
        use axum::routing::post;
        use axum::Router;

        async fn unauthorized() -> impl IntoResponse {
            (
                StatusCode::UNAUTHORIZED,
                [(header::CONTENT_TYPE, "application/json")],
                r#"{"error":{"type":"invalid_api_key","message":"credential rejected"}}"#,
            )
        }

        let app = Router::new().fallback(post(unauthorized));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        for (kind, provider_id) in [
            (crate::RouteKind::Fireworks, "fireworks"),
            (crate::RouteKind::Together, "together"),
            (crate::RouteKind::ModelGatewayOpenai, "model_gateway"),
        ] {
            let result = compatible_router(kind, address)
                .stream(ChatRequest {
                    provider: Some(ProviderId::new(provider_id)),
                    model: "hosted-model".into(),
                    messages: vec![ChatMessage::text(Role::User, "hi")],
                    ..Default::default()
                })
                .await;
            match result {
                Err(AgentError::Authentication(message)) => assert_eq!(
                    message,
                    format!("{provider_id} returned 401 (invalid_api_key): credential rejected")
                ),
                Err(other) => panic!("wrong error attribution for {provider_id}: {other:?}"),
                Ok(_) => panic!("the {provider_id} preset unexpectedly accepted the request"),
            }
        }

        server.abort();
    }

    #[tokio::test]
    async fn compatible_route_in_band_errors_keep_the_public_provider_id() {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::routing::post;
        use axum::Router;

        async fn error_stream(uri: axum::http::Uri) -> impl IntoResponse {
            // The gateway's OpenAI route speaks Responses; its in-band
            // terminal failure is `response.failed`. Chat-Completions routes
            // carry the bare `error` frame.
            let body = if uri.path().ends_with("/responses") {
                "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"upstream is overloaded\",\"type\":\"server_error\",\"code\":\"server_overloaded\"}}}\n\n"
            } else {
                "data: {\"error\":{\"message\":\"upstream is overloaded\",\"type\":\"server_error\",\"code\":\"server_overloaded\"}}\n\n"
            };
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                body,
            )
        }

        let app = Router::new().fallback(post(error_stream));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        for (kind, provider_id) in [
            (crate::RouteKind::Fireworks, "fireworks"),
            (crate::RouteKind::Together, "together"),
            (crate::RouteKind::ModelGatewayOpenai, "model_gateway"),
        ] {
            let stream = compatible_router(kind, address)
                .stream(ChatRequest {
                    provider: Some(ProviderId::new(provider_id)),
                    model: "hosted-model".into(),
                    messages: vec![ChatMessage::text(Role::User, "hi")],
                    ..Default::default()
                })
                .await
                .unwrap();
            assert_eq!(
                stream.collect::<Vec<_>>().await,
                vec![ProviderEvent::Failed {
                    error: ProviderErrorInfo {
                        kind: "overloaded".into(),
                        message: format!(
                            "{provider_id} returned 500 (server_error): upstream is overloaded"
                        ),
                    },
                }],
            );
        }

        server.abort();
    }

    #[tokio::test]
    async fn a_gateway_request_uses_conversation_credentials_on_the_responses_surface() {
        use axum::body::Bytes;
        use axum::extract::State;
        use axum::http::{header, HeaderMap, StatusCode};
        use axum::response::IntoResponse;
        use axum::routing::post;
        use axum::Router;
        use std::sync::{Arc, Mutex};

        struct CapturedRequest {
            path: String,
            headers: HeaderMap,
            body: Value,
        }

        #[derive(Clone, Default)]
        struct Capture(Arc<Mutex<Vec<CapturedRequest>>>);

        async fn capture(
            State(capture): State<Capture>,
            headers: HeaderMap,
            uri: axum::http::Uri,
            body: Bytes,
        ) -> impl IntoResponse {
            capture.0.lock().unwrap().push(CapturedRequest {
                path: uri.path().to_owned(),
                headers,
                body: serde_json::from_slice(&body).expect("request body is JSON"),
            });
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                "data: {\"type\":\"response.completed\",\"response\":{}}\n\n\
                 data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            )
        }

        struct RecordingTokenSource(Mutex<Vec<Option<tidebreak_core::id::SessionId>>>);

        #[async_trait::async_trait]
        impl crate::BearerTokenSource for RecordingTokenSource {
            async fn bearer_token(&self) -> tidebreak_core::Result<String> {
                unreachable!("the gateway adapter must ask for a conversation token");
            }

            async fn bearer_token_for(
                &self,
                conversation: Option<tidebreak_core::id::SessionId>,
            ) -> tidebreak_core::Result<String> {
                self.0.lock().unwrap().push(conversation);
                Ok("mg_at_rotating".to_string())
            }
        }

        let capture_state = Capture::default();
        let app = Router::new()
            .fallback(post(capture))
            .with_state(capture_state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let source = Arc::new(RecordingTokenSource(Mutex::new(Vec::new())));
        let provider = crate::Router::build(vec![crate::Route {
            kind: crate::RouteKind::ModelGatewayOpenai,
            api_key: String::new(),
            base_url: Some(format!("http://{address}/compat/openai/v1")),
            curated_models: vec!["gpt-fable-5".to_string()],
            model_rewrites: std::collections::HashMap::new(),
            token_source: Some(source.clone()),
            chatgpt_account_id: None,
        }]);
        let conversation = tidebreak_core::id::SessionId::new();
        let stream = provider
            .stream(ChatRequest {
                provider: Some(ProviderId::new("model_gateway")),
                model: "gpt-fable-5".into(),
                conversation: Some(conversation),
                messages: vec![ChatMessage::text(Role::User, "hi")],
                ..Default::default()
            })
            .await
            .unwrap();
        let _: Vec<_> = stream.collect().await;

        // A normal compatible endpoint uses its static key and must not learn
        // how the host groups conversations merely because the request has an
        // id available.
        let direct = OpenAiCompatProvider::compatible(
            "direct-key",
            format!("http://{address}/compat/openai/v1"),
        );
        let stream = direct
            .stream(ChatRequest {
                model: "gpt-fable-5".into(),
                conversation: Some(conversation),
                messages: vec![ChatMessage::text(Role::User, "hi")],
                ..Default::default()
            })
            .await
            .unwrap();
        let _: Vec<_> = stream.collect().await;

        assert_eq!(source.0.lock().unwrap().as_slice(), &[Some(conversation)]);
        let requests = capture_state.0.lock().unwrap();
        assert_eq!(requests.len(), 2);
        // The gateway's OpenAI surface is the Responses API; it serves no
        // northbound Chat Completions route.
        assert_eq!(requests[0].path, "/compat/openai/v1/responses");
        assert_eq!(
            requests[0].headers.get(header::AUTHORIZATION).unwrap(),
            "Bearer mg_at_rotating"
        );
        assert_eq!(
            requests[0]
                .headers
                .get(crate::router::GATEWAY_CONVERSATION_HEADER)
                .unwrap(),
            conversation.to_string().as_str()
        );
        assert_eq!(requests[0].body["stream"], json!(true));
        assert!(requests[0].body.get("stream_options").is_none());
        assert_eq!(requests[1].path, "/compat/openai/v1/chat/completions");
        assert_eq!(
            requests[1].headers.get(header::AUTHORIZATION).unwrap(),
            "Bearer direct-key"
        );
        assert!(requests[1]
            .headers
            .get(crate::router::GATEWAY_CONVERSATION_HEADER)
            .is_none());
        assert!(requests[1].body.get("stream_options").is_none());
        server.abort();
    }
}
