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

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use openwave_core::error::{AgentError, Result};
use openwave_core::provider::{
    ChatRequest, ContentBlock, ModelProvider, ProviderEvent, ProviderId, RefusalDetails,
    StopReason, Usage,
};
use openwave_core::{ImageAttachments, Role};

use crate::sse::{classify_provider_error, drain_frames, frame_data, read_bounded_error_body};

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

        // Surface non-2xx without the raw body — it can echo key material, and
        // `AgentError` strings reach the client via TurnFailed. Status (+ a
        // stable error type/code when present) is enough for classification.
        let status = response.status();
        if !status.is_success() {
            let body = read_bounded_error_body(response.bytes_stream()).await;
            return Err(classify_provider_error("anthropic", status.as_u16(), &body));
        }

        let stream = async_stream::stream! {
            let mut bytes = response.bytes_stream();
            // Accumulate raw BYTES, not a String: a chunk may split a multi-byte
            // UTF-8 character, so we only decode once a whole frame is buffered.
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
                            message: format!("anthropic stream ended early: {error}"),
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
/// Text, tool-use, and tool-result blocks serialize to exactly Anthropic's
/// content-block shape, so they pass through untouched. Image blocks do not:
/// they carry blob identity rather than pixels, so they are shaped explicitly
/// against the bytes hydrated on [`ChatRequest::images`]. The system prompt
/// becomes a top-level field, and only `user`/`assistant` roles are valid on
/// messages.
fn build_request_json(req: &ChatRequest) -> Result<Value> {
    let messages = req
        .messages
        .iter()
        .map(|message| {
            Ok(json!({
                "role": anthropic_role(message.role),
                "content": anthropic_content(&message.content, &req.images)?,
            }))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "messages": messages,
        "stream": true,
    });
    // Do not opt into Anthropic's server-side fallback beta implicitly. The
    // normalized request and model registry currently identify exactly one
    // model, so inventing a fallback here would lose explicit compatibility
    // policy and make durable model attribution ambiguous.

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
    if req.reasoning_model && takes_adaptive_thinking(&req.model) {
        // An omitted `thinking` means thinking is *off* on Opus 4.7 and later,
        // so a reasoning model only reasons when the request says so.
        //
        // `display` is an opt-in: the API default, `omitted`, still streams
        // thinking blocks but with empty text, which reads in a live transcript
        // as a long silent pause before the answer. `summarized` is what makes
        // the reasoning stream the renderer already draws worth drawing.
        body["thinking"] = json!({ "type": "adaptive", "display": "summarized" });
        if let Some(effort) = req.reasoning_effort {
            body["output_config"] = json!({ "effort": effort.as_str() });
        }
    }
    Ok(body)
}

/// Whether `model` takes the adaptive-thinking request shape.
///
/// Anthropic split the reasoning switch at Claude 4.6. That generation and
/// later take `thinking: {"type": "adaptive"}` (plus `output_config.effort`)
/// and reject a `budget_tokens` thinking block outright; earlier models are the
/// mirror image, understanding only the budget form. Curated ids carry the
/// generation in the id itself, so it is read from there rather than pinned to
/// a list that goes stale on the next release — and going stale here means the
/// newest model silently stops thinking, which is the bug this exists to
/// prevent.
///
/// An id with no readable generation keeps the request it has today. Sending a
/// parameter the model rejects fails the whole turn; omitting one only leaves
/// the model where it already was.
fn takes_adaptive_thinking(model: &str) -> bool {
    /// First Claude generation that reasons on `thinking: {"type": "adaptive"}`.
    const FIRST_ADAPTIVE: (u32, u32) = (4, 6);
    claude_generation(model).is_some_and(|generation| generation >= FIRST_ADAPTIVE)
}

/// Read the `(major, minor)` generation out of a Claude model id.
///
/// Handles the shapes Anthropic ships: split across tokens (`claude-opus-4-8`),
/// major only (`claude-opus-5`), and either with a trailing dated snapshot
/// (`claude-haiku-4-5-20251001`). A trailing date is not a minor version, so
/// anything longer than two digits does not count as one.
fn claude_generation(model: &str) -> Option<(u32, u32)> {
    let starts_with_digit = |token: &&str| token.starts_with(|c: char| c.is_ascii_digit());
    let mut tokens = model
        .strip_prefix("claude-")?
        .split('-')
        .skip_while(|token| !starts_with_digit(token));

    let major = tokens.next()?;
    // A dotted generation keeps both halves in one token.
    if let Some((major, minor)) = major.split_once('.') {
        return Some((major.parse().ok()?, minor.parse().ok()?));
    }
    let minor = tokens
        .next()
        .filter(|token| token.len() <= 2)
        .and_then(|token| token.parse().ok())
        .unwrap_or(0);
    Some((major.parse().ok()?, minor))
}

/// Shape one message's blocks into Anthropic's `content` array.
///
/// When a message carries more than one image, each is preceded by an
/// `Image N:` label. Anthropic's vision guidance calls for this in multi-image
/// prompts so the model can refer to a specific image unambiguously; with a
/// single image the label is noise and is omitted.
fn anthropic_content(blocks: &[ContentBlock], images: &ImageAttachments) -> Result<Value> {
    let image_count = blocks
        .iter()
        .filter(|block| matches!(block, ContentBlock::Image { .. }))
        .count();
    let label_images = image_count > 1;

    let mut out: Vec<Value> = Vec::with_capacity(blocks.len());
    let mut image_index = 0usize;
    for block in blocks {
        match block {
            ContentBlock::Image { image } => {
                let data = images.get(image.blob_id).ok_or_else(|| {
                    // Reduction rewrites a deliberately dropped image into a
                    // text stand-in, so an unhydrated image block here means
                    // the bytes were lost, not omitted on purpose. Sending the
                    // turn anyway would ask the model about an image it never
                    // received.
                    AgentError::Provider(format!(
                        "image attachment {} has no hydrated bytes",
                        image.blob_id
                    ))
                })?;
                image_index += 1;
                if label_images {
                    out.push(json!({ "type": "text", "text": format!("Image {image_index}:") }));
                }
                out.push(json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": data.media_type().as_str(),
                        "data": BASE64.encode(data.bytes()),
                    }
                }));
            }
            other => out.push(serde_json::to_value(other)?),
        }
    }
    Ok(Value::Array(out))
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
                let reason = map_stop_reason(reason);
                if reason == StopReason::Refusal {
                    let category = data
                        .get("delta")
                        .and_then(|delta| delta.get("stop_details"))
                        .and_then(|details| details.get("category"))
                        .and_then(Value::as_str);
                    events.push(ProviderEvent::Refusal {
                        details: RefusalDetails::from_category(category),
                    });
                } else {
                    events.push(ProviderEvent::Stop { reason });
                }
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
        "refusal" => StopReason::Refusal,
        // "end_turn" and anything we don't yet model (e.g. pause_turn) fall
        // back to a clean end.
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
    fn request_maps_system_messages_and_tools() {
        let req = ChatRequest {
            provider: Some(ProviderId::new("anthropic")),
            model: "claude-opus-4-8".into(),
            reasoning_model: true,
            system: Some("be brief".into()),
            messages: vec![ChatMessage::text(Role::User, "hi")],
            tools: vec![ToolSpec {
                name: "read_file".into(),
                description: "read a file".into(),
                input_schema: json!({"type": "object"}),
            }],
            max_tokens: None,
            temperature: Some(0.5),
            reasoning_effort: None,
            images: ImageAttachments::new(),
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
        assert!(
            body.get("fallbacks").is_none(),
            "fallback models require an explicit registry contract"
        );
    }

    fn reasoning_request(model: &str, effort: Option<ReasoningEffort>) -> ChatRequest {
        ChatRequest {
            provider: Some(ProviderId::new("anthropic")),
            model: model.into(),
            reasoning_model: true,
            system: None,
            messages: vec![ChatMessage::text(Role::User, "hi")],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            reasoning_effort: effort,
            images: ImageAttachments::new(),
        }
    }

    #[test]
    fn a_reasoning_model_is_asked_to_think_out_loud() {
        // Omitting `thinking` means thinking is off on Opus 4.7 and later, and
        // the default `display` streams empty thinking blocks — a silent pause
        // where the transcript should be showing reasoning.
        let body = build_request_json(&reasoning_request("claude-opus-5", None)).unwrap();
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["thinking"]["display"], "summarized");
        // Absent a per-chat override the provider's own effort default holds.
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn a_per_chat_effort_override_reaches_the_wire() {
        let body = build_request_json(&reasoning_request(
            "claude-opus-5",
            Some(ReasoningEffort::Low),
        ))
        .unwrap();
        assert_eq!(body["output_config"]["effort"], "low");
    }

    #[test]
    fn a_non_reasoning_request_asks_for_no_thinking() {
        let req = request_with(
            vec![ContentBlock::Text { text: "hi".into() }],
            ImageAttachments::new(),
        );
        assert!(!req.reasoning_model);
        let body = build_request_json(&req).unwrap();
        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn a_pre_adaptive_model_keeps_the_request_it_understands() {
        // Claude Haiku 4.5 rejects both an adaptive thinking block and
        // `output_config.effort`; sending either would fail the whole turn.
        let body = build_request_json(&reasoning_request(
            "claude-haiku-4-5-20251001",
            Some(ReasoningEffort::High),
        ))
        .unwrap();
        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn the_adaptive_switch_follows_the_generation_in_the_model_id() {
        for id in [
            "claude-opus-5",
            "claude-sonnet-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "claude-opus-6.2",
            "claude-opus-5-20260101",
        ] {
            assert!(takes_adaptive_thinking(id), "{id} should reason adaptively");
        }
        for id in [
            "claude-haiku-4-5-20251001",
            "claude-opus-4-5",
            "claude-opus-4-1-20250805",
            "claude-3-5-sonnet-20241022",
            // No readable generation: keep today's request rather than risk a
            // parameter the endpoint rejects.
            "some-gateway-alias",
            "claude-next",
        ] {
            assert!(!takes_adaptive_thinking(id), "{id} should not");
        }
        assert_eq!(claude_generation("claude-haiku-4-5-20251001"), Some((4, 5)));
        assert_eq!(claude_generation("claude-opus-5"), Some((5, 0)));
        assert_eq!(claude_generation("claude-opus-6.2"), Some((6, 2)));
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
    fn maps_stop_reasons() {
        assert_eq!(map_stop_reason("tool_use"), StopReason::ToolUse);
        assert_eq!(map_stop_reason("max_tokens"), StopReason::MaxTokens);
        assert_eq!(map_stop_reason("refusal"), StopReason::Refusal);
        assert_eq!(map_stop_reason("future_reason"), StopReason::EndTurn);
    }

    #[test]
    fn refusal_carries_bounded_category_when_present() {
        let out = run(&[json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": "refusal",
                "stop_details": {
                    "type": "refusal",
                    "category": "cyber"
                }
            },
            "usage": {"output_tokens": 0}
        })]);
        assert_eq!(
            out,
            vec![
                ProviderEvent::Usage(Usage::default()),
                ProviderEvent::Refusal {
                    details: RefusalDetails::from_category(Some("cyber")),
                },
            ]
        );
    }

    #[test]
    fn refusal_allows_missing_or_null_stop_details() {
        for delta in [
            json!({"stop_reason": "refusal"}),
            json!({"stop_reason": "refusal", "stop_details": null}),
            json!({"stop_reason": "refusal", "stop_details": {"category": null}}),
        ] {
            let out = run(&[json!({"type": "message_delta", "delta": delta})]);
            assert_eq!(
                out,
                vec![ProviderEvent::Refusal {
                    details: RefusalDetails::default(),
                }]
            );
        }
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

    fn request_with(content: Vec<ContentBlock>, images: ImageAttachments) -> ChatRequest {
        ChatRequest {
            provider: Some(ProviderId::new("anthropic")),
            model: "claude-opus-4-8".into(),
            reasoning_model: false,
            system: None,
            messages: vec![ChatMessage {
                role: Role::User,
                content,
            }],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            reasoning_effort: None,
            images,
        }
    }

    #[test]
    fn an_image_block_becomes_a_base64_source_block() {
        let image = png_ref(1);
        let mut images = ImageAttachments::new();
        images.insert(
            image.blob_id,
            openwave_core::ImageData::new(openwave_core::ImageMediaType::Png, vec![1, 2, 3]),
        );
        let req = request_with(
            vec![
                ContentBlock::Text {
                    text: "what is this?".into(),
                },
                ContentBlock::Image { image },
            ],
            images,
        );

        let body = build_request_json(&req).unwrap();
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/png");
        assert_eq!(content[1]["source"]["data"], BASE64.encode([1, 2, 3]));
        // A single image needs no ordinal label.
        assert_eq!(content.as_array().unwrap().len(), 2);
    }

    #[test]
    fn multiple_images_are_labelled_so_the_model_can_refer_to_each() {
        let (first, second) = (png_ref(1), png_ref(2));
        let mut images = ImageAttachments::new();
        for image in [&first, &second] {
            images.insert(
                image.blob_id,
                openwave_core::ImageData::new(openwave_core::ImageMediaType::Png, vec![7]),
            );
        }
        let req = request_with(
            vec![
                ContentBlock::Image { image: first },
                ContentBlock::Image { image: second },
            ],
            images,
        );

        let body = build_request_json(&req).unwrap();
        let content = body["messages"][0]["content"].as_array().unwrap().clone();
        assert_eq!(content.len(), 4);
        assert_eq!(content[0]["text"], "Image 1:");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[2]["text"], "Image 2:");
        assert_eq!(content[3]["type"], "image");
    }

    #[test]
    fn an_unhydrated_image_fails_the_request_instead_of_being_dropped() {
        // Reduction rewrites intentionally dropped images into text, so bytes
        // missing here mean something went wrong. Silently sending the turn
        // would ask the model about an image it never received.
        let req = request_with(
            vec![ContentBlock::Image { image: png_ref(9) }],
            ImageAttachments::new(),
        );
        let err = build_request_json(&req).unwrap_err();
        assert!(err.to_string().contains("no hydrated bytes"), "{err}");
    }

    #[test]
    fn text_and_tool_blocks_keep_their_existing_wire_shape() {
        // Guards the refactor away from blanket passthrough: non-image blocks
        // must serialize exactly as before.
        let blocks = vec![
            ContentBlock::Text { text: "hi".into() },
            ContentBlock::ToolResult {
                tool_use_id: "toolu_1".into(),
                content: "done".into(),
                is_error: false,
            },
        ];
        let shaped = anthropic_content(&blocks, &ImageAttachments::new()).unwrap();
        assert_eq!(shaped, serde_json::to_value(&blocks).unwrap());
    }
}
