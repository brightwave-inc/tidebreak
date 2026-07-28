//! Native Google Gemini Developer API provider.
//!
//! Gemini's GenerateContent protocol differs materially from OpenAI-compatible
//! chat completions: output limits live under `generationConfig`, streamed SSE
//! frames are complete (but partial) responses, and tool results must preserve
//! the model's function-call identity. Keeping that conversion here makes the
//! catalog's Gemini rows honest rather than depending on a compatibility layer
//! silently accepting or dropping fields it does not understand.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use futures::stream::{BoxStream, StreamExt};
use serde_json::{json, Value};
use uuid::Uuid;

use openwave_core::error::{AgentError, Result};
use openwave_core::provider::{
    ChatRequest, ContentBlock, ModelProvider, ProviderEvent, ProviderId, RefusalDetails,
    StopReason, Usage,
};
use openwave_core::{ImageAttachments, ReasoningEffort, Role};

use crate::google_auth::{valid_resource_segment, valid_vertex_location};
use crate::sse::{classify_provider_error, drain_frames, frame_data_raw, read_bounded_error_body};
use crate::BearerTokenSource;

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";
const DEFAULT_MAX_TOKENS: u32 = 4096;
/// Gemini accepts this documented value when an application cannot replay an
/// opaque thought signature. OpenWave stores provider-neutral tool calls, so
/// the bypass keeps their history portable across a per-chat model switch.
const THOUGHT_SIGNATURE_BYPASS: &str = "skip_thought_signature_validator";

#[derive(Clone)]
enum GeminiAuth {
    ApiKey(String),
    Bearer(Arc<dyn BearerTokenSource>),
}

#[derive(Clone)]
enum EndpointFamily {
    DeveloperApi,
    VertexAi {
        project_id: String,
        location: String,
    },
}

/// A [`ModelProvider`] for native Gemini GenerateContent over either the
/// Developer API or Vertex AI.
#[derive(Clone)]
pub struct GeminiProvider {
    client: reqwest::Client,
    auth: GeminiAuth,
    endpoint_family: EndpointFamily,
    base_url: String,
    base_url_overridden: bool,
}

impl GeminiProvider {
    /// Build a provider using a Gemini Developer API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            auth: GeminiAuth::ApiKey(api_key.into()),
            endpoint_family: EndpointFamily::DeveloperApi,
            base_url: DEFAULT_BASE_URL.to_string(),
            base_url_overridden: false,
        }
    }

    /// Build a provider using Vertex AI and a short-lived Google OAuth token.
    pub fn vertex(
        project_id: impl Into<String>,
        location: impl Into<String>,
        token_source: Arc<dyn BearerTokenSource>,
    ) -> Result<Self> {
        let project_id = project_id.into();
        let location = location.into();
        if !valid_resource_segment(&project_id) {
            return Err(AgentError::config("invalid Vertex AI project"));
        }
        if !valid_vertex_location(&location) {
            return Err(AgentError::config("invalid Vertex AI location"));
        }
        let base_url = if location == "global" {
            "https://aiplatform.googleapis.com".to_string()
        } else {
            format!("https://{location}-aiplatform.googleapis.com")
        };
        Ok(Self {
            client: reqwest::Client::new(),
            auth: GeminiAuth::Bearer(token_source),
            endpoint_family: EndpointFamily::VertexAi {
                project_id,
                location,
            },
            base_url,
            base_url_overridden: false,
        })
    }

    /// Override the selected endpoint family's base URL. This is primarily
    /// useful for controlled local test servers.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self.base_url_overridden = true;
        self
    }

    fn endpoint(&self, model: &str) -> Result<String> {
        if model.is_empty()
            || model.len() > 128
            || !model
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(AgentError::config("invalid Gemini model id"));
        }
        match &self.endpoint_family {
            EndpointFamily::DeveloperApi => {
                let base = self.base_url.trim_end_matches('/');
                Ok(format!(
                    "{base}/v1beta/models/{model}:streamGenerateContent?alt=sse"
                ))
            }
            EndpointFamily::VertexAi {
                project_id,
                location,
            } => {
                // Gemini 3 is global-only. Keep a regional setting useful for
                // older/future regional models without sending a curated 3.x
                // row to an endpoint Google cannot serve.
                let location = if requires_global_vertex(model) {
                    "global"
                } else {
                    location
                };
                let default_base;
                let base = if self.base_url_overridden {
                    self.base_url.trim_end_matches('/')
                } else if location == "global" {
                    "https://aiplatform.googleapis.com"
                } else {
                    default_base = format!("https://{location}-aiplatform.googleapis.com");
                    &default_base
                };
                Ok(format!(
                    "{base}/v1/projects/{project_id}/locations/{location}/publishers/google/models/{model}:streamGenerateContent?alt=sse"
                ))
            }
        }
    }
}

fn requires_global_vertex(model: &str) -> bool {
    model
        .strip_prefix("gemini-")
        .and_then(|version| version.split(['.', '-']).next())
        == Some("3")
}

#[async_trait]
impl ModelProvider for GeminiProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("gemini")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let body = build_request_json(&req)?;
        let request = self
            .client
            .post(self.endpoint(&req.model)?)
            .header("content-type", "application/json")
            .json(&body);
        let request = match &self.auth {
            GeminiAuth::ApiKey(api_key) => request.header("x-goog-api-key", api_key),
            GeminiAuth::Bearer(source) => request.bearer_auth(source.bearer_token().await?),
        };
        let response = request
            .send()
            .await
            // reqwest's display includes the URL; a Vertex URL contains the
            // project id extracted from the secret key file.
            .map_err(|_| AgentError::Provider("gemini request failed".into()))?;

        let status = response.status();
        if !status.is_success() {
            let body = read_bounded_error_body(response.bytes_stream()).await;
            return Err(classify_gemini_error(status.as_u16(), &body));
        }

        let stream = async_stream::stream! {
            let mut bytes = response.bytes_stream();
            let mut buffer = Vec::new();
            let mut state = StreamState::default();
            while let Some(chunk) = bytes.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(_) => {
                        yield ProviderEvent::Failed {
                            message: "gemini stream ended early".to_string(),
                        };
                        return;
                    }
                };
                buffer.extend_from_slice(&chunk);
                for frame in drain_frames(&mut buffer) {
                    let Some(data) = frame_data_raw(&frame) else {
                        continue;
                    };
                    let data = match serde_json::from_str::<Value>(&data) {
                        Ok(data) => data,
                        Err(_) => {
                            yield ProviderEvent::Failed {
                                message: "gemini returned an invalid stream frame".to_string(),
                            };
                            return;
                        }
                    };
                    for event in normalize(&data, &mut state) {
                        yield event;
                    }
                    if state.terminal {
                        return;
                    }
                }
            }
            if !buffer.is_empty() {
                let frame = String::from_utf8_lossy(&buffer).into_owned();
                if let Some(data) = frame_data_raw(&frame) {
                    let data = match serde_json::from_str::<Value>(&data) {
                        Ok(data) => data,
                        Err(_) => {
                            yield ProviderEvent::Failed {
                                message: "gemini returned an invalid stream frame".to_string(),
                            };
                            return;
                        }
                    };
                    for event in normalize(&data, &mut state) {
                        yield event;
                    }
                }
            }
            if !state.terminal {
                for event in finish_stream(&mut state) {
                    yield event;
                }
            }
        };
        Ok(Box::pin(stream))
    }
}

/// Build a native GenerateContent request.
///
/// Gemini 3 rejects the deprecated sampling controls, so `temperature` is
/// intentionally not forwarded. The registry only marks Gemini rows as
/// reasoning-capable when this adapter can turn the selected effort into the
/// native `thinkingLevel` shape.
fn build_request_json(req: &ChatRequest) -> Result<Value> {
    let mut generation_config = json!({
        "maxOutputTokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
    });
    if req.reasoning_model {
        if let Some(effort) = req.reasoning_effort {
            generation_config["thinkingConfig"] = json!({
                "thinkingLevel": gemini_thinking_level(effort),
            });
        }
    }

    let mut body = json!({
        "contents": gemini_contents(&req.messages, &req.images)?,
        "generationConfig": generation_config,
    });
    if let Some(system) = &req.system {
        body["systemInstruction"] = json!({ "parts": [{ "text": system }] });
    }
    if !req.tools.is_empty() {
        body["tools"] = json!([{
            "functionDeclarations": req.tools.iter().map(|tool| json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.input_schema,
            })).collect::<Vec<_>>(),
        }]);
        body["toolConfig"] = json!({ "functionCallingConfig": { "mode": "AUTO" } });
    }
    Ok(body)
}

fn gemini_thinking_level(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::None => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High | ReasoningEffort::XHigh | ReasoningEffort::Max => "high",
    }
}

/// Translate durable, provider-neutral history to Gemini content messages.
///
/// Gemini requires all responses to the parallel calls in one model turn to
/// form one following user message. The stored transcript keeps tool results as
/// independent messages, so consecutive pure result messages are coalesced.
fn gemini_contents(
    messages: &[openwave_core::provider::ChatMessage],
    images: &ImageAttachments,
) -> Result<Vec<Value>> {
    let tool_names: HashMap<&str, &str> = messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, .. } => Some((id.as_str(), name.as_str())),
            _ => None,
        })
        .collect();

    let mut contents = Vec::with_capacity(messages.len());
    for message in messages {
        let role = match message.role {
            Role::Assistant => "model",
            Role::System | Role::Tool | Role::User => "user",
        };
        let mut parts = Vec::with_capacity(message.content.len());
        let mut attached_signature = false;
        for block in &message.content {
            match block {
                ContentBlock::Text { text } => parts.push(json!({ "text": text })),
                ContentBlock::ToolUse { id, name, input } => {
                    let mut part = json!({
                        "functionCall": {
                            "id": id,
                            "name": name,
                            "args": input,
                        },
                    });
                    // Gemini validates the first call part of each model turn.
                    // We deliberately use its portable bypass rather than
                    // persisting opaque provider state in a cross-model chat.
                    if !attached_signature {
                        part["thoughtSignature"] = json!(THOUGHT_SIGNATURE_BYPASS);
                        attached_signature = true;
                    }
                    parts.push(part);
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    let name = tool_names.get(tool_use_id.as_str()).ok_or_else(|| {
                        AgentError::Provider(
                            "gemini tool result has no matching function call".to_string(),
                        )
                    })?;
                    let mut response = json!({ "result": content });
                    if *is_error {
                        response["is_error"] = json!(true);
                    }
                    parts.push(json!({
                        "functionResponse": {
                            "id": tool_use_id,
                            "name": name,
                            "response": response,
                        },
                    }));
                }
                ContentBlock::Image { image } => {
                    let data = images.get(image.blob_id).ok_or_else(|| {
                        AgentError::Provider(format!(
                            "image attachment {} has no hydrated bytes",
                            image.blob_id
                        ))
                    })?;
                    parts.push(json!({
                        "inlineData": {
                            "mimeType": data.media_type().as_str(),
                            "data": BASE64.encode(data.bytes()),
                        },
                    }));
                }
                // `ContentBlock` is deliberately open for new provider-neutral
                // variants. Dropping one here would silently change the model
                // prompt, so make the adapter fail until that new variant gains
                // an explicit Gemini representation.
                _ => {
                    return Err(AgentError::Provider(
                        "gemini cannot encode an unsupported content block".to_string(),
                    ));
                }
            }
        }
        contents.push(json!({ "role": role, "parts": parts }));
    }

    let function_responses_only = |content: &Value| {
        content.get("role").and_then(Value::as_str) == Some("user")
            && content
                .get("parts")
                .and_then(Value::as_array)
                .is_some_and(|parts| {
                    !parts.is_empty()
                        && parts
                            .iter()
                            .all(|part| part.get("functionResponse").is_some())
                })
    };
    let mut merged = Vec::with_capacity(contents.len());
    for content in contents {
        if function_responses_only(&content) && merged.last().is_some_and(function_responses_only) {
            let previous = merged.last_mut().expect("last was checked");
            let parts = content
                .get("parts")
                .and_then(Value::as_array)
                .expect("function responses have parts")
                .clone();
            previous
                .get_mut("parts")
                .and_then(Value::as_array_mut)
                .expect("function responses have mutable parts")
                .extend(parts);
        } else {
            merged.push(content);
        }
    }
    Ok(merged)
}

#[derive(Default)]
struct StreamState {
    next_tool_index: u32,
    seen_tool_ids: HashSet<String>,
    usage: Option<Usage>,
    saw_tool_call: bool,
    terminal: bool,
}

/// Convert one complete Gemini response frame into provider-neutral events.
fn normalize(data: &Value, state: &mut StreamState) -> Vec<ProviderEvent> {
    if state.terminal {
        return Vec::new();
    }
    if let Some(error) = data.get("error") {
        state.terminal = true;
        return vec![ProviderEvent::Failed {
            message: safe_in_band_error(error),
        }];
    }
    if let Some(block_reason) = data
        .get("promptFeedback")
        .and_then(|feedback| feedback.get("blockReason"))
        .and_then(Value::as_str)
    {
        state.terminal = true;
        return vec![ProviderEvent::Refusal {
            details: refusal_details(block_reason),
        }];
    }

    if let Some(metadata) = data.get("usageMetadata") {
        state.usage = Some(gemini_usage(metadata));
    }

    let Some(candidate) = data
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
    else {
        return Vec::new();
    };

    let mut events = Vec::new();
    if let Some(parts) = candidate
        .get("content")
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
    {
        for part in parts {
            if part.get("thought").and_then(Value::as_bool) == Some(true) {
                if let Some(text) = part
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    events.push(ProviderEvent::ReasoningDelta {
                        text: text.to_string(),
                    });
                }
                continue;
            }
            if let Some(text) = part
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                events.push(ProviderEvent::TextDelta {
                    text: text.to_string(),
                });
            }
            if let Some(call) = part.get("functionCall") {
                let Some(name) = call
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                else {
                    state.terminal = true;
                    events.push(ProviderEvent::Failed {
                        message: "gemini returned a malformed function call".to_string(),
                    });
                    return events;
                };
                let id = call
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
                    // Current Gemini 3 endpoints supply ids. Synthesizing for
                    // older or proxy responses still keeps the agent's own
                    // call/result pairing well-formed.
                    .unwrap_or_else(|| format!("gemini-{}", Uuid::new_v4()));
                if !state.seen_tool_ids.insert(id.clone()) {
                    continue;
                }
                let args = call.get("args").cloned().unwrap_or_else(|| json!({}));
                if !args.is_object() {
                    state.terminal = true;
                    events.push(ProviderEvent::Failed {
                        message: "gemini returned non-object function arguments".to_string(),
                    });
                    return events;
                }
                let index = state.next_tool_index;
                state.next_tool_index = state.next_tool_index.saturating_add(1);
                state.saw_tool_call = true;
                events.push(ProviderEvent::ToolCallStarted {
                    index,
                    id,
                    name: name.to_string(),
                });
                events.push(ProviderEvent::ToolCallArgsDelta {
                    index,
                    fragment: serde_json::to_string(&args).expect("JSON values serialize"),
                });
            }
        }
    }

    if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
        finish_candidate(reason, state, &mut events);
    }
    events
}

fn finish_stream(state: &mut StreamState) -> Vec<ProviderEvent> {
    let mut events = Vec::new();
    emit_usage(state, &mut events);
    events.push(ProviderEvent::Stop {
        reason: if state.saw_tool_call {
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        },
    });
    state.terminal = true;
    events
}

fn finish_candidate(reason: &str, state: &mut StreamState, events: &mut Vec<ProviderEvent>) {
    emit_usage(state, events);
    state.terminal = true;
    match reason {
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" => {
            events.push(ProviderEvent::Refusal {
                details: refusal_details(reason),
            });
        }
        "MALFORMED_FUNCTION_CALL" | "UNEXPECTED_TOOL_CALL" => {
            events.push(ProviderEvent::Failed {
                message: "gemini rejected a function call".to_string(),
            });
        }
        "MAX_TOKENS" => events.push(ProviderEvent::Stop {
            reason: StopReason::MaxTokens,
        }),
        // A Gemini tool call can report `STOP`, so tools must win over the
        // provider's finish code when the normalized stream contains one.
        _ => events.push(ProviderEvent::Stop {
            reason: if state.saw_tool_call {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            },
        }),
    }
}

fn emit_usage(state: &mut StreamState, events: &mut Vec<ProviderEvent>) {
    if let Some(usage) = state.usage.take() {
        events.push(ProviderEvent::Usage(usage));
    }
}

fn gemini_usage(metadata: &Value) -> Usage {
    let prompt = u32_at(metadata, "promptTokenCount");
    let cached = u32_at(metadata, "cachedContentTokenCount");
    Usage {
        // Gemini's prompt count includes the cached portion.
        input_tokens: prompt.saturating_sub(cached),
        // Thoughts are billed output in addition to candidate tokens.
        output_tokens: u32_at(metadata, "candidatesTokenCount")
            .saturating_add(u32_at(metadata, "thoughtsTokenCount")),
        cache_read_input_tokens: cached,
        cache_creation_input_tokens: 0,
    }
}

fn u32_at(value: &Value, key: &str) -> u32 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u64::from(u32::MAX)) as u32
}

fn refusal_details(reason: &str) -> RefusalDetails {
    let category = reason
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' => char::from(byte.to_ascii_lowercase()),
            b'a'..=b'z' | b'0'..=b'9' => char::from(byte),
            _ => '_',
        })
        .collect::<String>();
    RefusalDetails::from_category(Some(&category))
}

fn safe_in_band_error(error: &Value) -> String {
    let status = error
        .get("code")
        .and_then(Value::as_u64)
        .and_then(|code| u16::try_from(code).ok())
        .filter(|code| (100..=599).contains(code))
        .unwrap_or(500);
    format!("gemini returned {status}")
}

fn classify_gemini_error(status: u16, body: &str) -> AgentError {
    let prompt_too_long = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value.get("error").cloned().or(Some(value)))
        .and_then(|error| {
            error
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|message| {
            let message = message.to_ascii_lowercase();
            message.contains("too many tokens")
                || message.contains("input token count")
                || message.contains("maximum number of tokens")
        });
    if prompt_too_long {
        return AgentError::PromptTooLong(crate::sse::safe_http_error("gemini", status, body));
    }
    classify_provider_error("gemini", status, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwave_core::provider::ChatMessage;
    use openwave_core::tool::ToolSpec;

    fn request(messages: Vec<ChatMessage>) -> ChatRequest {
        ChatRequest {
            provider: Some(ProviderId::new("gemini")),
            model: "gemini-3.6-flash".into(),
            reasoning_model: true,
            system: Some("be brief".into()),
            messages,
            tools: vec![ToolSpec {
                name: "read_file".into(),
                description: "Read a file".into(),
                input_schema: json!({"type": "object"}),
            }],
            max_tokens: Some(65_536),
            temperature: Some(0.2),
            reasoning_effort: Some(ReasoningEffort::High),
            images: ImageAttachments::new(),
        }
    }

    #[test]
    fn request_uses_native_output_cap_and_current_tool_shape() {
        let body = build_request_json(&request(vec![ChatMessage::text(Role::User, "hi")])).unwrap();
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 65_536);
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "high"
        );
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be brief");
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            "read_file"
        );
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");
        assert!(body.get("temperature").is_none());
        assert!(body["generationConfig"].get("max_tokens").is_none());
    }

    #[test]
    fn tool_results_keep_call_identity_and_merge_parallel_results() {
        let messages = vec![
            ChatMessage {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::ToolUse {
                        id: "call_one".into(),
                        name: "read_file".into(),
                        input: json!({"path": "one"}),
                    },
                    ContentBlock::ToolUse {
                        id: "call_two".into(),
                        name: "read_file".into(),
                        input: json!({"path": "two"}),
                    },
                ],
            },
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_one".into(),
                    content: "one".into(),
                    is_error: false,
                }],
            },
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_two".into(),
                    content: "two".into(),
                    is_error: false,
                }],
            },
        ];
        let body = build_request_json(&request(messages)).unwrap();
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0]["role"], "model");
        assert_eq!(
            contents[0]["parts"][0]["thoughtSignature"],
            THOUGHT_SIGNATURE_BYPASS
        );
        assert!(contents[0]["parts"][1].get("thoughtSignature").is_none());
        let responses = contents[1]["parts"].as_array().unwrap();
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["functionResponse"]["id"], "call_one");
        assert_eq!(responses[0]["functionResponse"]["name"], "read_file");
        assert_eq!(responses[1]["functionResponse"]["id"], "call_two");
    }

    // ── Image blocks ───────────────────────────────────────────────

    fn png_ref(blob: u128) -> openwave_core::ImageRef {
        openwave_core::ImageRef {
            blob_id: Uuid::from_u128(blob),
            media_type: openwave_core::ImageMediaType::Png,
            width: 800,
            height: 600,
            byte_len: 3,
        }
    }

    #[test]
    fn image_blocks_become_hydrated_inline_data() {
        let image = png_ref(1);
        let mut req = request(vec![ChatMessage {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "what is this?".into(),
                },
                ContentBlock::Image { image },
            ],
        }]);
        req.images.insert(
            image.blob_id,
            openwave_core::ImageData::new(openwave_core::ImageMediaType::Png, vec![1, 2, 3]),
        );

        let body = build_request_json(&req).unwrap();
        let parts = body["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["text"], "what is this?");
        assert_eq!(parts[1]["inlineData"]["mimeType"], "image/png");
        assert_eq!(parts[1]["inlineData"]["data"], BASE64.encode([1, 2, 3]));
    }

    #[test]
    fn unhydrated_images_fail_instead_of_being_dropped() {
        let req = request(vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Image { image: png_ref(9) }],
        }]);
        let err = build_request_json(&req).unwrap_err();
        assert!(err.to_string().contains("no hydrated bytes"), "{err}");
    }

    fn run(chunks: &[Value]) -> Vec<ProviderEvent> {
        let mut state = StreamState::default();
        let mut out = chunks
            .iter()
            .flat_map(|chunk| normalize(chunk, &mut state))
            .collect::<Vec<_>>();
        if !state.terminal {
            out.extend(finish_stream(&mut state));
        }
        out
    }

    #[test]
    fn normalizes_partial_responses_usage_and_parallel_tool_calls() {
        let out = run(&[
            json!({"candidates":[{"content":{"parts":[
                {"thought": true, "text":"considering"},
                {"text":"I'll inspect it."},
                {"functionCall":{"id":"call_1", "name":"read_file", "args":{"path":"a"}}},
                {"functionCall":{"id":"call_2", "name":"read_file", "args":{"path":"b"}}}
            ]}}]}),
            json!({"candidates":[{"finishReason":"STOP"}], "usageMetadata": {
                "promptTokenCount": 10,
                "cachedContentTokenCount": 4,
                "candidatesTokenCount": 2,
                "thoughtsTokenCount": 3
            }}),
        ]);
        assert_eq!(
            out,
            vec![
                ProviderEvent::ReasoningDelta {
                    text: "considering".into()
                },
                ProviderEvent::TextDelta {
                    text: "I'll inspect it.".into()
                },
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_1".into(),
                    name: "read_file".into()
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"path":"a"}"#.into()
                },
                ProviderEvent::ToolCallStarted {
                    index: 1,
                    id: "call_2".into(),
                    name: "read_file".into()
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 1,
                    fragment: r#"{"path":"b"}"#.into()
                },
                ProviderEvent::Usage(Usage {
                    input_tokens: 6,
                    output_tokens: 5,
                    cache_read_input_tokens: 4,
                    cache_creation_input_tokens: 0
                }),
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse
                },
            ]
        );
    }

    #[test]
    fn prompt_blocks_and_stream_errors_are_terminal_and_safe() {
        let blocked = run(&[json!({"promptFeedback":{"blockReason":"SAFETY"}})]);
        assert_eq!(
            blocked,
            vec![ProviderEvent::Refusal {
                details: RefusalDetails::from_category(Some("safety")),
            }]
        );
        let failed = run(&[json!({"error":{"code":401,"message":"AIza-secret"}})]);
        assert_eq!(
            failed,
            vec![ProviderEvent::Failed {
                message: "gemini returned 401".into(),
            }]
        );
    }

    #[test]
    fn max_tokens_is_not_reclassified_as_a_retryable_end_turn() {
        let out = run(&[json!({"candidates":[{"finishReason":"MAX_TOKENS"}]})]);
        assert_eq!(
            out,
            vec![ProviderEvent::Stop {
                reason: StopReason::MaxTokens
            }]
        );
    }

    #[test]
    fn endpoint_uses_developer_api_streaming_route() {
        let provider = GeminiProvider::new("key").with_base_url("http://127.0.0.1:8080/");
        assert_eq!(
            provider.endpoint("gemini-3.6-flash").unwrap(),
            "http://127.0.0.1:8080/v1beta/models/gemini-3.6-flash:streamGenerateContent?alt=sse"
        );
    }

    struct StaticToken;

    #[async_trait]
    impl BearerTokenSource for StaticToken {
        async fn bearer_token(&self) -> Result<String> {
            Ok("vertex-bearer".into())
        }
    }

    #[test]
    fn vertex_endpoints_distinguish_global_and_regional_hosts() {
        let global =
            GeminiProvider::vertex("test-project", "global", Arc::new(StaticToken)).unwrap();
        assert_eq!(
            global.endpoint("gemini-3.6-flash").unwrap(),
            "https://aiplatform.googleapis.com/v1/projects/test-project/locations/global/publishers/google/models/gemini-3.6-flash:streamGenerateContent?alt=sse"
        );

        let regional =
            GeminiProvider::vertex("test-project", "us-central1", Arc::new(StaticToken)).unwrap();
        assert_eq!(
            regional.endpoint("gemini-2.5-flash").unwrap(),
            "https://us-central1-aiplatform.googleapis.com/v1/projects/test-project/locations/us-central1/publishers/google/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
        assert_eq!(
            regional.endpoint("gemini-3.6-flash").unwrap(),
            "https://aiplatform.googleapis.com/v1/projects/test-project/locations/global/publishers/google/models/gemini-3.6-flash:streamGenerateContent?alt=sse"
        );
    }

    #[tokio::test]
    async fn endpoint_families_share_body_semantics_and_use_exclusive_auth_headers() {
        use axum::extract::State;
        use axum::http::{header, HeaderMap};
        use axum::response::IntoResponse;
        use axum::routing::post;
        use axum::{Json, Router};
        use std::sync::Mutex;

        #[derive(Clone, Default)]
        struct Capture(Arc<Mutex<Vec<(HeaderMap, Value)>>>);

        async fn capture(
            State(capture): State<Capture>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> impl IntoResponse {
            capture.0.lock().unwrap().push((headers, body));
            (
                [(header::CONTENT_TYPE, "text/event-stream")],
                "data: {\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n",
            )
        }

        let capture_state = Capture::default();
        let app = Router::new()
            .fallback(post(capture))
            .with_state(capture_state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base_url = format!("http://{address}");

        let developer = GeminiProvider::new("developer-key").with_base_url(&base_url);
        let mut stream = developer
            .stream(request(vec![ChatMessage::text(Role::User, "hi")]))
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let vertex = GeminiProvider::vertex("test-project", "global", Arc::new(StaticToken))
            .unwrap()
            .with_base_url(&base_url);
        let mut stream = vertex
            .stream(request(vec![ChatMessage::text(Role::User, "hi")]))
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let requests = capture_state.0.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].0.get("x-goog-api-key").unwrap(),
            "developer-key"
        );
        assert!(requests[0].0.get(header::AUTHORIZATION).is_none());
        assert_eq!(
            requests[1].0.get(header::AUTHORIZATION).unwrap(),
            "Bearer vertex-bearer"
        );
        assert!(requests[1].0.get("x-goog-api-key").is_none());
        assert_eq!(requests[0].1, requests[1].1);
        assert_eq!(requests[1].1["generationConfig"]["maxOutputTokens"], 65_536);
        assert_eq!(
            requests[1].1["tools"][0]["functionDeclarations"][0]["name"],
            "read_file"
        );
        server.abort();
    }
}
