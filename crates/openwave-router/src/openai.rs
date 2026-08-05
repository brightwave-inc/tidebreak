//! Native OpenAI Responses API provider.

use std::collections::{BTreeMap, HashSet};

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use futures::stream::{BoxStream, StreamExt};
use serde_json::{json, Value};

use openwave_core::error::{AgentError, ProviderErrorInfo, Result};
use openwave_core::provider::{
    ChatRequest, ContentBlock, ModelProvider, ProviderEvent, ProviderId, RefusalDetails,
    ResponseFormat, StopReason, ToolChoice, Usage,
};
use openwave_core::tool::{strict_json_schema, OptionalProperties};
use openwave_core::{ImageAttachments, Role};

use crate::sse::{
    classify_in_band_error, classify_provider_error, drain_frames, frame_data,
    read_bounded_error_body,
};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// A [`ModelProvider`] for OpenAI's native Responses API.
#[derive(Clone)]
pub struct OpenAiProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl OpenAiProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: crate::http::streaming_client(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

#[async_trait]
impl ModelProvider for OpenAiProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("openai")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let body = build_request_json(&req)?;
        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));
        let response = self
            .client
            .post(url)
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|_| AgentError::Provider("openai request failed".into()))?;

        let status = response.status();
        if !status.is_success() {
            let retry_after = crate::sse::retry_after_hint(response.headers());
            let body = read_bounded_error_body(response.bytes_stream()).await;
            return Err(classify_provider_error(
                "openai",
                status.as_u16(),
                &body,
                retry_after,
            ));
        }

        let ceiling = crate::http::timeouts().total_stream;
        let stream = async_stream::stream! {
            let bytes = crate::http::with_stream_deadline(response.bytes_stream(), ceiling);
            futures::pin_mut!(bytes);
            let mut buffer = Vec::new();
            let mut state = StreamState::default();
            while let Some(chunk) = bytes.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        yield ProviderEvent::Failed {
                            error: ProviderErrorInfo::provider(error.client_message("openai")),
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
            if !state.terminal {
                yield ProviderEvent::Failed {
                    error: ProviderErrorInfo::provider("openai stream ended before completion"),
                };
            }
        };
        Ok(Box::pin(stream))
    }
}

fn build_request_json(req: &ChatRequest) -> Result<Value> {
    let input = build_input(req)?;
    let mut body = json!({
        "model": req.model,
        "input": input,
        "stream": true,
        "store": false,
        "max_output_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
    });

    if req.reasoning_model {
        if let Some(effort) = req.reasoning_effort {
            body["reasoning"] = json!({ "effort": effort.as_str(), "summary": "auto" });
        }
    } else if let Some(temperature) = req.temperature {
        body["temperature"] = json!(temperature);
    }

    if !req.tools.is_empty() {
        body["tools"] = Value::Array(req.tools.iter().map(openai_tool).collect());
    }
    if let Some(choice) = &req.tool_choice {
        body["tool_choice"] = openai_tool_choice(choice)?;
    }
    match &req.response_format {
        Some(ResponseFormat::JsonSchema { name, schema }) => {
            let schema =
                strict_json_schema(schema, OptionalProperties::AcceptNull).ok_or_else(|| {
                    AgentError::Provider(format!(
                        "response format {name} has no strict JSON Schema form"
                    ))
                })?;
            body["text"] = json!({
                "format": {
                    "type": "json_schema",
                    "name": name,
                    "strict": true,
                    "schema": schema,
                }
            });
        }
        None => {}
        Some(other) => {
            return Err(AgentError::Provider(format!(
                "openai cannot enforce response format {other:?}"
            )))
        }
    }
    Ok(body)
}

fn build_input(req: &ChatRequest) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    if let Some(system) = &req.system {
        out.push(json!({
            "role": "system",
            "content": [{ "type": "input_text", "text": system }]
        }));
    }
    for message in &req.messages {
        extend_input(&mut out, message, &req.images)?;
    }
    Ok(sanitize_tool_pairs(out))
}

fn extend_input(
    out: &mut Vec<Value>,
    message: &openwave_core::ChatMessage,
    images: &ImageAttachments,
) -> Result<()> {
    if message.role == Role::Assistant {
        return extend_assistant_input(out, message);
    }
    let mut message_parts = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text } => {
                message_parts.push(json!({ "type": "input_text", "text": text }));
            }
            ContentBlock::Image { image } => {
                let data = images.get(image.blob_id).ok_or_else(|| {
                    AgentError::Provider(format!(
                        "image attachment {} has no hydrated bytes",
                        image.blob_id
                    ))
                })?;
                message_parts.push(json!({
                    "type": "input_image",
                    "image_url": format!(
                        "data:{};base64,{}",
                        data.media_type().as_str(),
                        BASE64.encode(data.bytes())
                    )
                }));
            }
            ContentBlock::ToolUse { id, name, input } => {
                out.push(json!({
                    "type": "function_call",
                    "call_id": id,
                    "name": name,
                    "arguments": serde_json::to_string(input).unwrap_or_else(|_| "{}".into()),
                }));
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let output = if *is_error {
                    format!("Error: the tool call failed.\n{content}")
                } else {
                    content.clone()
                };
                out.push(json!({
                    "type": "function_call_output",
                    "call_id": tool_use_id,
                    "output": output,
                }));
            }
            other => {
                return Err(AgentError::Provider(format!(
                    "openai cannot express content block {other:?}"
                )))
            }
        }
    }
    if !message_parts.is_empty() {
        // Assistant messages took the branch above; everything else is input.
        out.push(json!({ "role": "user", "content": message_parts }));
    }
    Ok(())
}

/// Assistant history items cannot carry `input_text` parts: the Responses API
/// only accepts `output_text` and `refusal` there. Prior assistant prose goes
/// back as a message item whose `content` is a bare string, which the API
/// accepts and stores as assistant output text.
fn extend_assistant_input(
    out: &mut Vec<Value>,
    message: &openwave_core::ChatMessage,
) -> Result<()> {
    let mut texts = Vec::new();
    let mut calls = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text } => texts.push(text.as_str()),
            ContentBlock::Image { .. } => {
                return Err(AgentError::Provider(
                    "openai cannot express an image in assistant history".into(),
                ))
            }
            ContentBlock::ToolUse { id, name, input } => {
                calls.push(json!({
                    "type": "function_call",
                    "call_id": id,
                    "name": name,
                    "arguments": serde_json::to_string(input).unwrap_or_else(|_| "{}".into()),
                }));
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let output = if *is_error {
                    format!("Error: the tool call failed.\n{content}")
                } else {
                    content.clone()
                };
                calls.push(json!({
                    "type": "function_call_output",
                    "call_id": tool_use_id,
                    "output": output,
                }));
            }
            other => {
                return Err(AgentError::Provider(format!(
                    "openai cannot express content block {other:?}"
                )))
            }
        }
    }
    if !texts.is_empty() {
        out.push(json!({ "role": "assistant", "content": texts.join("\n\n") }));
    }
    out.extend(calls);
    Ok(())
}

fn sanitize_tool_pairs(items: Vec<Value>) -> Vec<Value> {
    let calls: HashSet<String> = items
        .iter()
        .filter(|item| item["type"] == "function_call")
        .filter_map(|item| item["call_id"].as_str().map(str::to_owned))
        .collect();
    let outputs: HashSet<String> = items
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .filter_map(|item| item["call_id"].as_str().map(str::to_owned))
        .collect();
    items
        .into_iter()
        .filter(|item| match item["type"].as_str() {
            Some("function_call") | Some("function_call_output") => item["call_id"]
                .as_str()
                .is_some_and(|id| calls.contains(id) && outputs.contains(id)),
            _ => true,
        })
        .collect()
}

fn openai_tool(tool: &openwave_core::tool::ToolSpec) -> Value {
    let schema = strict_json_schema(&tool.input_schema, OptionalProperties::Reject)
        .unwrap_or_else(|| tool.input_schema.clone());
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": schema,
        "strict": strict_json_schema(&tool.input_schema, OptionalProperties::Reject).is_some(),
    })
}

fn openai_tool_choice(choice: &ToolChoice) -> Result<Value> {
    Ok(match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::Required => json!("required"),
        ToolChoice::None => json!("none"),
        ToolChoice::Tool { name } => json!({ "type": "function", "name": name }),
        other => {
            return Err(AgentError::Provider(format!(
                "openai cannot express tool choice {other:?}"
            )))
        }
    })
}

#[derive(Default)]
struct StreamState {
    calls: BTreeMap<String, CallState>,
    next_index: u32,
    terminal: bool,
    refused: bool,
}

#[derive(Default)]
struct CallState {
    index: u32,
    name: String,
    started: bool,
    saw_argument_delta: bool,
}

fn normalize(data: &Value, state: &mut StreamState) -> Vec<ProviderEvent> {
    if state.terminal {
        return Vec::new();
    }
    let event_type = data["type"].as_str().unwrap_or_default();
    match event_type {
        "error" => {
            state.terminal = true;
            vec![ProviderEvent::Failed {
                error: ProviderErrorInfo::from_error(&classify_in_band_error(
                    "openai",
                    data.get("error").unwrap_or(data),
                )),
            }]
        }
        "response.output_text.delta" => data["delta"]
            .as_str()
            .map(|text| vec![ProviderEvent::TextDelta { text: text.into() }])
            .unwrap_or_default(),
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => data["delta"]
            .as_str()
            .map(|text| vec![ProviderEvent::ReasoningDelta { text: text.into() }])
            .unwrap_or_default(),
        "response.refusal.delta" => data["delta"]
            .as_str()
            .map(|text| vec![ProviderEvent::TextDelta { text: text.into() }])
            .unwrap_or_default(),
        "response.refusal.done" => {
            state.refused = true;
            Vec::new()
        }
        "response.output_item.added" => start_call(data, state, false),
        "response.function_call_arguments.delta" => {
            let call_id = data["call_id"].as_str().unwrap_or_default();
            let mut events = ensure_call(call_id, None, state);
            if let (Some(call), Some(fragment)) =
                (state.calls.get_mut(call_id), data["delta"].as_str())
            {
                call.saw_argument_delta = true;
                events.push(ProviderEvent::ToolCallArgsDelta {
                    index: call.index,
                    fragment: fragment.into(),
                });
            }
            events
        }
        "response.output_item.done" => start_call(data, state, true),
        "response.completed" => {
            state.terminal = true;
            let response = &data["response"];
            let mut events = usage_event(response).into_iter().collect::<Vec<_>>();
            if state.refused {
                events.push(ProviderEvent::Refusal {
                    details: RefusalDetails::from_category(None),
                });
            } else {
                let has_calls = !state.calls.is_empty();
                events.push(ProviderEvent::Stop {
                    reason: if has_calls {
                        StopReason::ToolUse
                    } else {
                        StopReason::EndTurn
                    },
                });
            }
            events
        }
        "response.incomplete" => {
            state.terminal = true;
            let response = &data["response"];
            let mut events = usage_event(response).into_iter().collect::<Vec<_>>();
            match response["incomplete_details"]["reason"].as_str() {
                Some("max_output_tokens") => events.push(ProviderEvent::Stop {
                    reason: StopReason::MaxTokens,
                }),
                Some("content_filter") => events.push(ProviderEvent::Refusal {
                    details: RefusalDetails::from_category(Some("content_filter")),
                }),
                _ => events.push(ProviderEvent::Failed {
                    error: ProviderErrorInfo::provider(
                        "openai response ended incomplete without a supported reason",
                    ),
                }),
            }
            events
        }
        "response.failed" => {
            state.terminal = true;
            let error = data["response"].get("error").unwrap_or(&data["response"]);
            vec![ProviderEvent::Failed {
                error: ProviderErrorInfo::from_error(&classify_in_band_error("openai", error)),
            }]
        }
        _ => Vec::new(),
    }
}

fn start_call(
    data: &Value,
    state: &mut StreamState,
    emit_complete_args: bool,
) -> Vec<ProviderEvent> {
    let item = &data["item"];
    if item["type"] != "function_call" {
        return Vec::new();
    }
    let call_id = item["call_id"].as_str().unwrap_or_default();
    let name = item["name"].as_str();
    let mut events = ensure_call(call_id, name, state);
    if emit_complete_args {
        let saw_deltas = state
            .calls
            .get(call_id)
            .is_some_and(|call| call.saw_argument_delta);
        if !saw_deltas {
            if let (Some(call), Some(arguments)) =
                (state.calls.get(call_id), item["arguments"].as_str())
            {
                if !arguments.is_empty() {
                    events.push(ProviderEvent::ToolCallArgsDelta {
                        index: call.index,
                        fragment: arguments.to_owned(),
                    });
                }
            }
        }
    }
    events
}

fn ensure_call(call_id: &str, name: Option<&str>, state: &mut StreamState) -> Vec<ProviderEvent> {
    if call_id.is_empty() {
        return Vec::new();
    }
    let call = state.calls.entry(call_id.to_owned()).or_insert_with(|| {
        let index = state.next_index;
        state.next_index = state.next_index.saturating_add(1);
        CallState {
            index,
            name: String::new(),
            started: false,
            saw_argument_delta: false,
        }
    });
    if let Some(name) = name.filter(|name| !name.is_empty()) {
        call.name = name.to_owned();
    }
    if !call.started && !call.name.is_empty() {
        call.started = true;
        return vec![ProviderEvent::ToolCallStarted {
            index: call.index,
            id: call_id.to_owned(),
            name: call.name.clone(),
        }];
    }
    Vec::new()
}

fn usage_event(response: &Value) -> Option<ProviderEvent> {
    let usage = response.get("usage")?;
    let total = usage["input_tokens"].as_u64().unwrap_or(0);
    let cached = usage["input_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap_or(0);
    Some(ProviderEvent::Usage(Usage {
        input_tokens: u32::try_from(total.saturating_sub(cached)).unwrap_or(u32::MAX),
        output_tokens: u32::try_from(usage["output_tokens"].as_u64().unwrap_or(0))
            .unwrap_or(u32::MAX),
        cache_read_input_tokens: u32::try_from(cached).unwrap_or(u32::MAX),
        cache_creation_input_tokens: 0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwave_core::provider::{ChatMessage, MessageReasoning};
    use openwave_core::tool::ToolSpec;
    use openwave_core::{ImageData, ImageMediaType, ImageRef, ReasoningEffort};

    fn tool() -> ToolSpec {
        ToolSpec {
            name: "exec".into(),
            description: "run a command".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"]
            }),
        }
    }

    #[test]
    fn tools_and_reasoning_use_responses_shape_together() {
        let req = ChatRequest {
            model: "gpt-5.6-sol".into(),
            reasoning_model: true,
            messages: vec![ChatMessage::text(Role::User, "call a tool")],
            tools: vec![tool()],
            reasoning_effort: Some(ReasoningEffort::High),
            max_tokens: Some(1024),
            ..Default::default()
        };
        let body = build_request_json(&req).unwrap();
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["reasoning"]["summary"], "auto");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "exec");
        assert_eq!(body["max_output_tokens"], 1024);
        assert!(body.get("messages").is_none());
    }

    #[test]
    fn tool_history_uses_function_items_and_drops_orphans() {
        let paired = openwave_core::ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_1".into(),
                name: "exec".into(),
                input: json!({"command":"pwd"}),
            }],
            reasoning: MessageReasoning::default(),
        };
        let result = openwave_core::ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".into(),
                content: "ok".into(),
                is_error: false,
            }],
            reasoning: MessageReasoning::default(),
        };
        let orphan = openwave_core::ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "missing".into(),
                content: "bad".into(),
                is_error: true,
            }],
            reasoning: MessageReasoning::default(),
        };
        let req = ChatRequest {
            model: "gpt-5.6-sol".into(),
            messages: vec![paired, result, orphan],
            ..Default::default()
        };
        let input = build_input(&req).unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["type"], "function_call");
        assert_eq!(input[1]["type"], "function_call_output");
    }

    #[test]
    fn assistant_history_uses_bare_string_content() {
        let messages = vec![
            ChatMessage::text(Role::User, "what is here?"),
            ChatMessage::text(Role::Assistant, "Let me look."),
            openwave_core::ChatMessage {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "Running it.".into(),
                    },
                    ContentBlock::ToolUse {
                        id: "call_1".into(),
                        name: "exec".into(),
                        input: json!({"command":"pwd"}),
                    },
                ],
                reasoning: MessageReasoning::default(),
            },
            openwave_core::ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: "/tmp".into(),
                    is_error: false,
                }],
                reasoning: MessageReasoning::default(),
            },
            ChatMessage::text(Role::User, "thanks"),
        ];
        let req = ChatRequest {
            model: "gpt-5.6-sol".into(),
            messages,
            ..Default::default()
        };
        let input = build_input(&req).unwrap();

        let assistant: Vec<_> = input
            .iter()
            .filter(|item| item["role"] == "assistant")
            .collect();
        assert_eq!(assistant.len(), 2);
        assert_eq!(assistant[0]["content"], json!("Let me look."));
        assert_eq!(assistant[1]["content"], json!("Running it."));
        assert!(
            !input
                .iter()
                .filter(|item| item["role"] == "assistant")
                .any(|item| item.to_string().contains("input_text")),
            "assistant items must not carry input_text parts: {input:?}"
        );

        let user: Vec<_> = input
            .iter()
            .filter(|item| item["role"] == "user" && item["content"].is_array())
            .collect();
        assert_eq!(user.len(), 2);
        for item in user {
            assert_eq!(item["content"][0]["type"], "input_text");
        }

        // The assistant's prose precedes the call it made, and the pair survives.
        let types: Vec<_> = input
            .iter()
            .map(|item| {
                item["type"]
                    .as_str()
                    .unwrap_or_else(|| item["role"].as_str().unwrap())
            })
            .collect();
        assert_eq!(
            types,
            vec![
                "user",
                "assistant",
                "assistant",
                "function_call",
                "function_call_output",
                "user"
            ]
        );
        let call = input
            .iter()
            .find(|item| item["type"] == "function_call")
            .unwrap();
        assert_eq!(call["call_id"], "call_1");
        let output = input
            .iter()
            .find(|item| item["type"] == "function_call_output")
            .unwrap();
        assert_eq!(output["call_id"], "call_1");
        assert_eq!(output["output"], "/tmp");
    }

    #[test]
    fn strict_output_and_named_tool_choice_are_native() {
        let req = ChatRequest {
            model: "gpt-5.6-sol".into(),
            messages: vec![ChatMessage::text(Role::User, "run")],
            tools: vec![tool()],
            tool_choice: Some(ToolChoice::Tool {
                name: "exec".into(),
            }),
            response_format: Some(ResponseFormat::JsonSchema {
                name: "answer".into(),
                schema: json!({
                    "type":"object",
                    "properties":{"answer":{"type":"string"}},
                    "required":["answer"]
                }),
            }),
            ..Default::default()
        };
        let body = build_request_json(&req).unwrap();
        assert_eq!(
            body["tool_choice"],
            json!({"type":"function","name":"exec"})
        );
        assert_eq!(body["text"]["format"]["type"], "json_schema");
        assert_eq!(body["text"]["format"]["strict"], true);
    }

    #[test]
    fn image_input_is_hydrated_as_a_data_url() {
        let image = ImageRef {
            blob_id: uuid::Uuid::from_u128(1),
            media_type: ImageMediaType::Png,
            width: 1,
            height: 1,
            byte_len: 3,
        };
        let mut images = ImageAttachments::new();
        images.insert(
            image.blob_id,
            ImageData::new(ImageMediaType::Png, vec![1, 2, 3]),
        );
        let req = ChatRequest {
            model: "gpt-5.6-sol".into(),
            messages: vec![openwave_core::ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Image { image }],
                reasoning: MessageReasoning::default(),
            }],
            images,
            ..Default::default()
        };
        let body = build_request_json(&req).unwrap();
        assert!(body["input"][0]["content"][0]["image_url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
    }

    #[test]
    fn normalizes_text_reasoning_tool_usage_and_stop() {
        let mut state = StreamState::default();
        let events: Vec<_> = [
            json!({"type":"response.reasoning_summary_text.delta","delta":"think"}),
            json!({"type":"response.output_text.delta","delta":"hello"}),
            json!({"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_1","name":"exec"}}),
            json!({"type":"response.function_call_arguments.delta","call_id":"call_1","delta":"{\"command\":\"pwd\"}"}),
            json!({"type":"response.completed","response":{"usage":{"input_tokens":10,"input_tokens_details":{"cached_tokens":4},"output_tokens":3}}}),
        ]
        .iter()
        .flat_map(|event| normalize(event, &mut state))
        .collect();
        assert_eq!(
            events,
            vec![
                ProviderEvent::ReasoningDelta {
                    text: "think".into()
                },
                ProviderEvent::TextDelta {
                    text: "hello".into()
                },
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_1".into(),
                    name: "exec".into()
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: "{\"command\":\"pwd\"}".into()
                },
                ProviderEvent::Usage(Usage {
                    input_tokens: 6,
                    output_tokens: 3,
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
    fn completed_function_item_supplies_arguments_when_no_deltas_arrived() {
        let mut state = StreamState::default();
        let events = normalize(
            &json!({
                "type":"response.output_item.done",
                "item":{
                    "type":"function_call",
                    "call_id":"call_1",
                    "name":"exec",
                    "arguments":"{\"command\":\"pwd\"}"
                }
            }),
            &mut state,
        );
        assert_eq!(
            events,
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_1".into(),
                    name: "exec".into()
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: "{\"command\":\"pwd\"}".into()
                },
            ]
        );
    }

    #[test]
    fn completed_function_item_does_not_duplicate_streamed_arguments() {
        let mut state = StreamState::default();
        let _ = normalize(
            &json!({"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_1","name":"exec"}}),
            &mut state,
        );
        let _ = normalize(
            &json!({"type":"response.function_call_arguments.delta","call_id":"call_1","delta":"{}"}),
            &mut state,
        );
        assert!(normalize(
            &json!({"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_1","name":"exec","arguments":"{}"}}),
            &mut state,
        )
        .is_empty());
    }

    #[test]
    fn incomplete_and_failed_responses_are_terminal() {
        let mut state = StreamState::default();
        assert_eq!(
            normalize(
                &json!({"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}}),
                &mut state
            ),
            vec![ProviderEvent::Stop {
                reason: StopReason::MaxTokens
            }]
        );
        let mut state = StreamState::default();
        let events = normalize(
            &json!({"type":"response.failed","response":{"error":{"type":"server_error","message":"upstream failed"}}}),
            &mut state,
        );
        assert!(matches!(events.as_slice(), [ProviderEvent::Failed { .. }]));

        let mut state = StreamState::default();
        assert_eq!(
            normalize(
                &json!({"type":"response.incomplete","response":{"incomplete_details":{"reason":"content_filter"}}}),
                &mut state,
            ),
            vec![ProviderEvent::Refusal {
                details: RefusalDetails::from_category(Some("content_filter"))
            }]
        );
    }

    #[test]
    fn refusal_text_streams_and_terminal_refusal_is_normalized() {
        let mut state = StreamState::default();
        assert_eq!(
            normalize(
                &json!({"type":"response.refusal.delta","delta":"cannot"}),
                &mut state
            ),
            vec![ProviderEvent::TextDelta {
                text: "cannot".into()
            }]
        );
        assert!(normalize(
            &json!({"type":"response.refusal.done","refusal":"cannot"}),
            &mut state
        )
        .is_empty());
        assert_eq!(
            normalize(
                &json!({"type":"response.completed","response":{}}),
                &mut state
            ),
            vec![ProviderEvent::Refusal {
                details: RefusalDetails::from_category(None)
            }]
        );
    }

    #[tokio::test]
    async fn native_provider_posts_to_responses_and_streams_completion() {
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
                    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n"
                ),
            )
        }

        let (tx, rx) = oneshot::channel();
        let state = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
        let app = Router::new().fallback(post(capture)).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let provider = OpenAiProvider::new("key").with_base_url(format!("http://{address}/v1"));
        let stream = provider
            .stream(ChatRequest {
                model: "gpt-5.6-sol".into(),
                messages: vec![ChatMessage::text(Role::User, "hi")],
                ..Default::default()
            })
            .await
            .unwrap();
        let events: Vec<_> = stream.collect().await;
        assert_eq!(rx.await.unwrap(), "/v1/responses");
        assert_eq!(
            events,
            vec![
                ProviderEvent::TextDelta { text: "ok".into() },
                ProviderEvent::Usage(Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                }),
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn
                },
            ]
        );
    }
}
