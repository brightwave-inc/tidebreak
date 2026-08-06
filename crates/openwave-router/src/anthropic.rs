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
use openwave_core::error::{AgentError, ProviderErrorInfo, Result};
use openwave_core::provider::{
    provider_executed_tool_call_text, ChatRequest, ContentBlock, ModelProvider, ProviderEvent,
    ProviderId, ProviderToolReplay, ReasoningOrigin, RefusalDetails, ResponseFormat, StopReason,
    ToolChoice, Usage,
};
use openwave_core::tool::{strict_json_schema, OptionalProperties};
use openwave_core::{ImageAttachments, Role};

use crate::sse::{
    classify_in_band_error, classify_provider_error, drain_frames, frame_data,
    read_bounded_error_body,
};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Header a model-gateway deployment reads to group inference into one
/// conversation. The gateway digests the value per user and harness; it is
/// never forwarded upstream.
const GATEWAY_CONVERSATION_HEADER: &str = "x-model-gateway-conversation-id";

/// Description for the synthetic tool that carries a constrained response.
///
/// The model is told what the call is for, because a forced tool with an opaque
/// name reads to it as an unexplained side quest and it starts narrating around
/// the call.
const STRUCTURED_OUTPUT_TOOL_DESCRIPTION: &str =
    "Return the result of this request. Call this once, with the whole answer in \
     its arguments, and write nothing else.";

/// A [`ModelProvider`] for Anthropic's Messages API.
#[derive(Clone)]
pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    /// Per-request credential supplier for gateways that mint short-lived
    /// tokens. Takes precedence over `api_key` when present.
    token_source: Option<std::sync::Arc<dyn crate::BearerTokenSource>>,
    /// Whether to declare the request's conversation to a model gateway.
    /// Off for direct Anthropic, whose API has no such header and no business
    /// learning how the host groups its chats.
    conversation_attribution: bool,
}

impl AnthropicProvider {
    /// Build a provider with the given API key, hitting api.anthropic.com.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: crate::http::streaming_client(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            token_source: None,
            conversation_attribution: false,
        }
    }

    /// Override the base URL — e.g. to route through a gateway that speaks the
    /// native Messages API.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Fetch the credential from `source` at each request instead of using a
    /// static key. For gateways whose tokens rotate under the adapter.
    #[must_use]
    pub fn with_token_source(
        mut self,
        source: std::sync::Arc<dyn crate::BearerTokenSource>,
    ) -> Self {
        self.token_source = Some(source);
        self
    }

    /// Declare each request's conversation to the gateway this provider points
    /// at, so its usage views can group inference the way the app does.
    ///
    /// Only for a model-gateway base URL. Anthropic's own API is not a party to
    /// how the host organizes conversations, and the header would be sent to
    /// whatever `base_url` names.
    #[must_use]
    pub fn with_conversation_attribution(mut self) -> Self {
        self.conversation_attribution = true;
        self
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("anthropic")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let mut body = build_request_json(&req)?;
        // `build_request_json` has already rejected a format it cannot enforce.
        let output_tool = match &req.response_format {
            Some(ResponseFormat::JsonSchema { name, .. }) => Some(name.clone()),
            _ => None,
        };
        let api_key = match &self.token_source {
            Some(source) => source.bearer_token_for(req.conversation).await?,
            None => self.api_key.clone(),
        };

        // Setup failures (connection, auth, 4xx/5xx) surface here as `Err` so the
        // router can classify and fail over; the returned stream only yields
        // normalized events.
        let response = self.send(&body, &api_key, req.conversation).await?;

        // Only a request that can pause needs its raw blocks kept, and only
        // Anthropic's own server tools pause a turn.
        let continuations_allowed = req.vendor_web_search.is_some();
        let replay_origin = ReasoningOrigin {
            provider: req.provider.clone(),
            model: req.model.clone(),
        };
        let provider = self.clone();
        let conversation = req.conversation;
        let ceiling = crate::http::timeouts().total_stream;
        let stream = async_stream::stream! {
            let mut response = response;
            let mut state = StreamState {
                output_tool,
                raw_blocks: continuations_allowed.then(RawAssistantBlocks::default),
                replay_origin: Some(replay_origin),
                ..StreamState::default()
            };
            let mut continuations = 0u32;
            'legs: loop {
                let bytes = crate::http::with_stream_deadline(response.bytes_stream(), ceiling);
                futures::pin_mut!(bytes);
                // Accumulate raw BYTES, not a String: a chunk may split a multi-byte
                // UTF-8 character, so we only decode once a whole frame is buffered.
                let mut buffer: Vec<u8> = Vec::new();
                while let Some(chunk) = bytes.next().await {
                    // A mid-stream transport error must not read as a clean end:
                    // the accumulated tool-call arguments may be truncated
                    // mid-JSON, and acting on them silently corrupts the step.
                    let chunk = match chunk {
                        Ok(chunk) => chunk,
                        Err(error) => {
                            yield ProviderEvent::Failed {
                                error: ProviderErrorInfo::provider(
                                    error.client_message("anthropic"),
                                ),
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
                // The provider suspended the turn for its own server-side tool and
                // resumes from the paused response replayed back to it. That
                // continuation belongs here rather than above the adapter: the
                // turn is one turn, and a consumer must only ever see it finish
                // once.
                if state.paused {
                    state.paused = false;
                    if continuations >= MAX_PAUSE_CONTINUATIONS {
                        // Out of continuations. Everything streamed so far is
                        // real output the model produced, so it ends as an
                        // ordinary turn rather than as a failure that would
                        // discard it.
                        yield ProviderEvent::Stop { reason: StopReason::EndTurn };
                        return;
                    }
                    continuations += 1;
                    let blocks = state
                        .raw_blocks
                        .as_ref()
                        .map(RawAssistantBlocks::sealed)
                        .unwrap_or_default();
                    replace_paused_assistant_message(&mut body, blocks, continuations == 1);
                    state.begin_continuation();
                    match provider.send(&body, &api_key, conversation).await {
                        Ok(next) => {
                            response = next;
                            continue 'legs;
                        }
                        // The paused turn cannot be resumed, and what streamed
                        // before the pause is a fragment of an unfinished answer.
                        Err(error) => {
                            yield ProviderEvent::Failed {
                                error: ProviderErrorInfo::from_error(&error),
                            };
                            return;
                        }
                    }
                }
                // A stream that closes with a tool call's argument JSON still open
                // was truncated — a clean TCP close carries no transport error.
                // Fail the step rather than committing the fragment as a finish.
                if let Some(event) = end_of_stream(&state) {
                    yield event;
                }
                break 'legs;
            }
        };
        Ok(Box::pin(stream))
    }
}

impl AnthropicProvider {
    /// POST one Messages request and hand back its streaming response.
    ///
    /// Every leg of a paused turn goes out through here, so the credential,
    /// version header, and error classification are identical on the first
    /// request and on each continuation.
    async fn send(
        &self,
        body: &Value,
        api_key: &str,
        conversation: Option<openwave_core::id::ChatId>,
    ) -> Result<reqwest::Response> {
        let url = format!("{}/v1/messages", self.base_url);
        let mut request = self
            .client
            .post(url)
            .header("x-api-key", api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json");
        // A conversation is declared only where one is configured to be read.
        // The id is a UUID, so it satisfies the gateway's bound on the value
        // (1-256 ASCII graphic bytes) by construction.
        if let (true, Some(conversation)) = (self.conversation_attribution, conversation) {
            request = request.header(GATEWAY_CONVERSATION_HEADER, conversation.to_string());
        }
        let response = request
            .json(body)
            .send()
            .await
            // reqwest's display includes the URL, and a gateway URL can carry
            // tenant-identifying parts; `AgentError` strings reach the client
            // via TurnFailed. Only the fact of a failed request surfaces.
            .map_err(|_| AgentError::Provider("anthropic request failed".into()))?;

        // Surface non-2xx without the raw body — it can echo key material, and
        // `AgentError` strings reach the client via TurnFailed. Status (+ a
        // stable error type/code when present) is enough for classification.
        let status = response.status();
        if !status.is_success() {
            let retry_after = crate::sse::retry_after_hint(response.headers());
            let body = read_bounded_error_body(response.bytes_stream()).await;
            return Err(classify_provider_error(
                "anthropic",
                status.as_u16(),
                &body,
                retry_after,
            ));
        }
        Ok(response)
    }
}

/// How many times one turn may be resumed after the provider pauses it.
///
/// A pause costs a whole extra request, and a turn that has paused this many
/// times is not converging. The cap bounds that without cutting short the
/// ordinary case, where a turn pauses once or twice around its searches.
const MAX_PAUSE_CONTINUATIONS: u32 = 8;

/// Put the paused assistant response back on the request so the provider can
/// resume it.
///
/// The blocks go back exactly as they arrived — encrypted search content
/// included — because that is what the provider validates the resumed turn
/// against. No user message is added: nothing new is being asked, the same
/// turn is being continued. Later pauses rewrite the message rather than
/// appending another, since two assistant messages cannot sit next to each
/// other and the turn only ever produced one.
fn replace_paused_assistant_message(body: &mut Value, blocks: &[Value], first_pause: bool) {
    let message = json!({ "role": "assistant", "content": blocks });
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    if first_pause {
        messages.push(message);
    } else if let Some(last) = messages.last_mut() {
        *last = message;
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
    let rename_client_web_search = req.vendor_web_search.is_some();
    let mut messages = req
        .messages
        .iter()
        .map(|message| {
            Ok(json!({
                "role": anthropic_role(message.role),
                "content": anthropic_content(
                    &message.content,
                    &req.images,
                    rename_client_web_search,
                    req.provider.as_ref(),
                    &req.model,
                )?,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    mark_cacheable_transcript_tail(&mut messages);

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
        // Block form, not a bare string, so the prefix through the system
        // prompt carries a cache breakpoint.
        body["system"] = json!([{
            "type": "text",
            "text": system,
            "cache_control": ephemeral_cache_control(),
        }]);
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
    // The vendor search is a server tool, declared alongside the client tools
    // rather than instead of them: the model may still call anything the host
    // advertised in the same turn.
    if let Some(search) = req.vendor_web_search {
        let vendor_tool = json!({
            "type": web_search_tool_type(&req.model),
            "name": VENDOR_WEB_SEARCH_TOOL,
            "max_uses": search.max_uses,
        });
        match body["tools"].as_array_mut() {
            Some(tools) => tools.push(vendor_tool),
            None => body["tools"] = json!([vendor_tool]),
        }
    }
    match &req.response_format {
        Some(ResponseFormat::JsonSchema { name, schema }) => {
            // The Messages API constrains output through a forced tool call, so
            // the schema becomes a tool nothing above the adapter ever sees: the
            // stream re-reads its arguments as text (see `normalize`), which is
            // the channel every other provider delivers structured output on.
            let schema =
                strict_json_schema(schema, OptionalProperties::AcceptNull).ok_or_else(|| {
                    AgentError::Provider(format!(
                        "response format {name} has no strict JSON Schema form"
                    ))
                })?;
            let output_tool = json!({
                "name": name,
                "description": STRUCTURED_OUTPUT_TOOL_DESCRIPTION,
                "input_schema": schema,
            });
            match body["tools"].as_array_mut() {
                Some(tools) => tools.push(output_tool),
                None => body["tools"] = json!([output_tool]),
            }
            // Forcing the output tool is what makes the constraint a
            // constraint. It also takes the one tool-choice slot on the wire, so
            // an explicit `tool_choice` loses to it — see
            // `ChatRequest::tool_choice`.
            body["tool_choice"] = json!({ "type": "tool", "name": name });
        }
        None => {
            if let Some(choice) = &req.tool_choice {
                body["tool_choice"] = anthropic_tool_choice(choice)?;
            }
        }
        // `ResponseFormat` is open. A format this adapter has not learned must
        // fail the request rather than stream an unconstrained answer that only
        // looks like a success.
        Some(other) => {
            return Err(AgentError::Provider(format!(
                "anthropic cannot enforce response format {other:?}"
            )))
        }
    }
    // After the whole tool array is settled, including a structured-output tool
    // appended above.
    mark_last_tool_cacheable(&mut body);
    if let Some(temperature) = req.temperature {
        body["temperature"] = json!(temperature);
    }
    // Extended thinking and a forced tool are mutually exclusive on the Messages
    // API: with thinking on, `tool_choice` may only be `auto` or `none`. The
    // forcing is the caller's explicit ask and reasoning is a quality
    // preference, so the ask wins and this request does not think.
    if req.reasoning_model && !forces_a_tool(req) && takes_adaptive_thinking(&req.model) {
        // An omitted `thinking` means thinking is *off* on Opus 4.7 and later,
        // so a reasoning model only reasons when the request says so.
        //
        // `display` is an opt-in: the API default, `omitted`, still streams
        // thinking blocks but with empty text, which reads in a live transcript
        // as a long silent pause before the answer. `summarized` is what makes
        // the reasoning stream the renderer already draws worth drawing.
        //
        // Reasoning rides back on the assistant messages that produced it
        // (see `attach_reasoning_blocks`), restoring the model's inter-tool
        // plan continuity — within a turn from its own stream, and across
        // turns from the durable message the step wrote. A step that never
        // reached the store, or one whose blocks belong to another route,
        // replays nothing. Adaptive mode relaxes turn validation for exactly
        // that case: no assistant turn has to begin with a thinking block,
        // and history assembled from mixed sources needs none reinserted, so
        // sending zero of them is always a valid shape. The 400 lives in
        // *partial* replay — within the latest assistant message the
        // consecutive thinking blocks must match what the model generated
        // verbatim, `redacted_thinking` included, which is why capture and
        // replay below keep the raw blocks whole rather than filtering on
        // `type == "thinking"`.
        body["thinking"] = json!({ "type": "adaptive", "display": "summarized" });
        if let Some(effort) = req.reasoning_effort {
            body["output_config"] = json!({ "effort": effort.as_str() });
        }
        attach_reasoning_blocks(&mut body, req);
    }
    Ok(body)
}

/// Replay the captured reasoning blocks ahead of the content of the assistant
/// messages that produced them.
///
/// Anthropic validates the consecutive thinking prefix of an assistant
/// message against what the model generated, so the blocks go back exactly
/// as captured: whole, in stream order, and block-type-agnostic. A
/// `redacted_thinking` block rides along untouched — filtering on
/// `type == "thinking"` would split the prefix and fail the request. This
/// runs only on a request that actually thinks (see the caller): replaying
/// reasoning into a request with thinking off buys nothing and risks a
/// shape the API rejects, while omission is always valid.
///
/// A block is only valid input back to the exact route that minted it, so
/// each message replays only what `replayable_for` clears against this
/// request's provider and model. A chat may switch between Anthropic, OpenAI
/// and Gemini models mid-conversation, and even between two Anthropic models;
/// history rebuilt across such a switch carries foreign blocks whose
/// signatures this model never produced. Dropping them is the right answer
/// rather than failing the turn: sending no reasoning is always a valid
/// shape, and the alternative is a request the API rejects.
fn attach_reasoning_blocks(body: &mut Value, req: &ChatRequest) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for (message, wire) in req.messages.iter().zip(messages.iter_mut()) {
        let blocks = message
            .reasoning
            .replayable_for(req.provider.as_ref(), &req.model);
        if blocks.is_empty() {
            continue;
        }
        let Some(content) = wire.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        let mut combined = blocks.to_vec();
        combined.append(content);
        *content = combined;
    }
}

/// The breakpoint marker. Five-minute TTL: an agentic turn's model calls land
/// seconds apart, so the longer TTL would only double the write price for reads
/// that already happen well inside the default window.
fn ephemeral_cache_control() -> Value {
    json!({ "type": "ephemeral" })
}

/// Put a breakpoint on the last tool definition.
///
/// Tools render before `system`, so the system breakpoint already covers them.
/// This second one buys the segment on its own: it is the only breakpoint when
/// a request carries no system prompt, and it survives a system prompt that
/// differs between two chats sharing one tool set. Both breakpoints sit inside
/// the same prefix, so the tokens are written once either way.
///
/// Requires deterministic tool order to hit at all — see the note on
/// `mark_cacheable_transcript_tail`.
fn mark_last_tool_cacheable(body: &mut Value) {
    if let Some(last) = body
        .get_mut("tools")
        .and_then(Value::as_array_mut)
        .and_then(|tools| tools.last_mut())
        .and_then(Value::as_object_mut)
    {
        last.insert("cache_control".into(), ephemeral_cache_control());
    }
}

/// Put breakpoints on the tail of the transcript and on a block lagging it.
///
/// The tail breakpoint moves to the last block on every call rather than being
/// anchored to a fixed early message. An agentic turn appends an assistant
/// message and its tool results per step and re-sends the whole transcript, so
/// the tail is exactly the boundary between what the previous call already
/// wrote and what this call adds: each step writes only its own delta and
/// reads everything before it. Anchoring instead — at the head of the
/// conversation, say — would cache a fixed prefix and pay full price for the
/// growth, and the head is the worst place to anchor here besides: context
/// fitting rewrites it when it truncates, checkpoints it, or evicts an old
/// image, while leaving the tail untouched.
///
/// Anthropic's cache lookup walks back at most
/// [`CACHE_LOOKUP_LOOKBACK_BLOCKS`] content blocks from a breakpoint to find a
/// matching prior prefix, so a lone tail breakpoint misses when one step
/// appends more blocks than that — a wide parallel tool-call fan-out does
/// exactly this, and the next call silently pays a full-price read. The
/// lagging breakpoint sits one lookback window behind the tail, so consecutive
/// calls always have a breakpoint within the window of one the previous call
/// wrote: a step of up to two windows of new blocks still hits, at the lagging
/// breakpoint. Both breakpoints sit inside the same prefix, so the tokens are
/// written once either way, and a transcript shorter than the window simply
/// gets no lagging breakpoint.
///
/// A write costs more than uncached input and a read costs far less, so a
/// breakpoint only pays if the prefix under it is byte-identical next call.
/// Two things above these must therefore stay stable: the tool advertisement
/// order (see #1088) and the system prompt, which is fixed for a chat's
/// configuration. A prefix that is under the model's minimum cacheable length
/// is not a loss — the breakpoint is ignored rather than charged.
fn mark_cacheable_transcript_tail(messages: &mut [Value]) {
    /// How far Anthropic's cache lookup walks back from a breakpoint, in
    /// content blocks. The lagging breakpoint trails the tail by exactly one
    /// window, so consecutive calls keep a breakpoint within reach of one the
    /// previous call cached.
    const CACHE_LOOKUP_LOOKBACK_BLOCKS: usize = 20;

    let mut blocks_from_tail = 0usize;
    let mut marked = 0;
    'messages: for message in messages.iter_mut().rev() {
        let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for block in content.iter_mut().rev() {
            if blocks_from_tail == 0 || blocks_from_tail == CACHE_LOOKUP_LOOKBACK_BLOCKS {
                if let Some(block) = block.as_object_mut() {
                    block.insert("cache_control".into(), ephemeral_cache_control());
                    marked += 1;
                    if marked == 2 {
                        break 'messages;
                    }
                }
            }
            blocks_from_tail += 1;
        }
    }
}

/// Whether the request obliges the model to call a specific tool or any tool.
fn forces_a_tool(req: &ChatRequest) -> bool {
    req.response_format.is_some()
        || matches!(
            req.tool_choice,
            Some(ToolChoice::Required | ToolChoice::Tool { .. })
        )
}

fn anthropic_tool_choice(choice: &ToolChoice) -> Result<Value> {
    Ok(match choice {
        ToolChoice::Auto => json!({ "type": "auto" }),
        ToolChoice::Required => json!({ "type": "any" }),
        ToolChoice::None => json!({ "type": "none" }),
        ToolChoice::Tool { name } => json!({ "type": "tool", "name": name }),
        // `ToolChoice` is open so a provider-neutral mode can be added without
        // a breaking change. Silently substituting the model's own judgement
        // for a mode this adapter has not learned would turn "must not call a
        // tool" into "may".
        other => {
            return Err(AgentError::Provider(format!(
                "anthropic cannot express tool choice {other:?}"
            )))
        }
    })
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

/// The name Anthropic gives its server-side web search tool, and the name the
/// search surfaces under to the host.
const VENDOR_WEB_SEARCH_TOOL: &str = "web_search";

/// The `type` of Anthropic's server-side web search tool for `model`.
///
/// The tool is versioned by date and the versions are not interchangeable: a
/// model that has not been trained against the newer one rejects it. The
/// 2026-02-09 revision ships on the larger current models — Opus and Sonnet
/// from 4.6 on, and the whole 5 generation — while the smaller and older
/// models keep the original 2025-03-05 tool. Read from the id's generation for
/// the same reason `takes_adaptive_thinking` does: a pinned list goes stale on
/// the next release, and an id with no readable generation gets the basic tool,
/// which every search-capable model accepts.
fn web_search_tool_type(model: &str) -> &'static str {
    /// The revision the current large models take.
    const CURRENT: &str = "web_search_20260209";
    /// The original tool, and the safe answer for anything else.
    const BASIC: &str = "web_search_20250305";
    /// First generation on the current revision, for Opus and Sonnet.
    const FIRST_CURRENT: (u32, u32) = (4, 6);

    // Haiku is the small tier across every generation and stays on the basic
    // tool.
    if model.contains("haiku") {
        return BASIC;
    }
    match claude_generation(model) {
        Some(generation) if generation >= FIRST_CURRENT => CURRENT,
        _ => BASIC,
    }
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
///
/// `rename_client_web_search` rewrites the name of historical client-style
/// `web_search` tool calls — see [`PRIOR_WEB_SEARCH_TOOL`].
///
/// `provider` and `model` gate native search replay: only blocks minted on
/// this exact route go back as `server_tool_use` / `web_search_tool_result`.
fn anthropic_content(
    blocks: &[ContentBlock],
    images: &ImageAttachments,
    rename_client_web_search: bool,
    provider: Option<&ProviderId>,
    model: &str,
) -> Result<Value> {
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
            // A search this or another provider ran server-side. Same-route
            // Anthropic searches carry the native pair (encrypted content
            // included); anything else becomes cleartext prose with titles
            // and URLs so a model switch still keeps usable history.
            ContentBlock::ProviderExecutedToolCall {
                name,
                input,
                output,
                is_error,
                replay,
            } => {
                if let Some(replay) = replay {
                    let native = replay.replayable_for(provider, model);
                    if !native.is_empty() {
                        for block in native {
                            out.push(block.clone());
                        }
                        continue;
                    }
                }
                out.push(json!({
                    "type": "text",
                    "text": provider_executed_tool_call_text(name, input, output, *is_error),
                }));
            }
            ContentBlock::ToolUse { id, name, input } if rename_client_web_search => {
                out.push(json!({
                    "type": "tool_use",
                    "id": id,
                    "name": rename_prior_web_search(name),
                    "input": input,
                }));
            }
            other => out.push(serde_json::to_value(other)?),
        }
    }
    Ok(Value::Array(out))
}

/// The name a historical client-side `web_search` call is replayed under once
/// the request declares Anthropic's server tool of that name.
///
/// The request would then carry two different tools called `web_search` — one
/// the client executes, one the provider does — and replaying client
/// `tool_use` blocks under the server tool's name is undefined. Renaming keeps
/// the history well-formed and readable; `tool_result` blocks pair by id, so
/// they need no matching change.
const PRIOR_WEB_SEARCH_TOOL: &str = "web_search_prior";

fn rename_prior_web_search(name: &str) -> &str {
    if name == VENDOR_WEB_SEARCH_TOOL {
        PRIOR_WEB_SEARCH_TOOL
    } else {
        name
    }
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
    /// Name of the synthetic tool carrying a constrained response, when the
    /// request set a [`ResponseFormat`].
    output_tool: Option<String>,
    /// Content-block index that tool occupied, once it starts.
    output_block: Option<u32>,
    /// Whether any *other* tool call was announced this stream.
    saw_other_tool_call: bool,
    /// Reasoning blocks in flight, keyed by content-block index.
    ///
    /// A `thinking` block opens empty at `content_block_start` and accumulates
    /// `thinking_delta` text and the terminal `signature_delta` until
    /// `content_block_stop` closes it; a `redacted_thinking` block arrives
    /// whole at the start. The raw JSON is the state — never a parsed struct
    /// — so a block goes back out byte-identical to what the model produced,
    /// whatever fields a future API revision adds.
    pending_reasoning: std::collections::HashMap<u32, Value>,
    /// Set once the stream has terminalized on an in-band error, so no later
    /// frame can append events after the failure.
    terminal: bool,
    /// Tool-use content blocks still open — started, not yet stopped. Their
    /// argument JSON is incomplete until `content_block_stop` arrives.
    open_tool_blocks: std::collections::HashSet<u32>,
    /// Set once the provider reported how the message ended (a stop, a
    /// refusal, or an interrupting failure). Without it a silent close is
    /// indistinguishable from a finished message on the wire.
    finished: bool,
    /// Server-side tool calls in flight, keyed by content-block index: the
    /// provider-assigned id, the tool name, and the arguments accumulating
    /// from `input_json_delta` exactly as a client tool call's would.
    open_server_tools: std::collections::HashMap<u32, PendingServerTool>,
    /// Closed server-side calls still waiting for their result block, keyed by
    /// the `tool_use_id` the result will name.
    awaiting_server_results: std::collections::HashMap<String, PendingServerTool>,
    /// This message's assistant content blocks exactly as the provider sent
    /// them, kept only to replay a `pause_turn` continuation. Populated only
    /// while the request allows continuations, so an ordinary turn carries no
    /// second copy of its own output.
    raw_blocks: Option<RawAssistantBlocks>,
    /// Set when the provider paused the turn and this stream is allowed to
    /// resume it. The stop is then withheld: the turn is not over, and one
    /// finished turn is all a consumer may ever see.
    paused: bool,
    /// Route that minted this stream's provider-executed calls, so their
    /// native blocks can be origin-gated on a later request.
    replay_origin: Option<ReasoningOrigin>,
}

impl StreamState {
    /// Reset the per-response bookkeeping before the next leg of a paused
    /// turn.
    ///
    /// Content-block indices restart at zero on each response, so anything
    /// keyed by index has to go. What survives is what belongs to the turn
    /// rather than the response: the accumulated raw blocks, the token counts,
    /// and a server tool call still waiting for the result the next leg will
    /// deliver.
    fn begin_continuation(&mut self) {
        self.output_block = None;
        self.pending_reasoning.clear();
        self.open_tool_blocks.clear();
        self.open_server_tools.clear();
        self.finished = false;
    }
}

/// A server-side tool call whose arguments are still streaming, or which has
/// closed and is waiting for its result block.
#[derive(Clone, Default)]
struct PendingServerTool {
    id: String,
    name: String,
    input_json: String,
}

impl PendingServerTool {
    /// The accumulated arguments, or an empty object when the provider sent
    /// none or sent something unparseable — the call still happened, and
    /// dropping it over its arguments would lose the search entirely.
    fn input(&self) -> Value {
        serde_json::from_str(&self.input_json).unwrap_or_else(|_| json!({}))
    }
}

/// The assistant content blocks of the message being streamed, kept in the
/// provider's own shape.
///
/// Only a `pause_turn` continuation reads this: Anthropic resumes a paused
/// turn from the paused response replayed back verbatim, and a normalized
/// block is not that — a `web_search_tool_result` in particular has to go back
/// with the encrypted content the model was given. Blocks accumulate across
/// every leg of one turn, so the replayed assistant message stays a single
/// message however many times the provider pauses.
#[derive(Default)]
struct RawAssistantBlocks {
    /// Blocks from legs that have already ended, in wire order.
    sealed: Vec<Value>,
    /// The current leg's blocks, keyed by content-block index.
    open: std::collections::BTreeMap<u32, Value>,
    /// Argument JSON accumulating for the current leg's tool blocks.
    partial_json: std::collections::HashMap<u32, String>,
}

impl RawAssistantBlocks {
    fn start(&mut self, index: u32, block: &Value) {
        self.open.insert(index, block.clone());
    }

    fn append_text(&mut self, index: u32, key: &str, fragment: &str) {
        if let Some(block) = self.open.get_mut(&index) {
            append_str_field(block, key, fragment);
        }
    }

    fn set(&mut self, index: u32, key: &str, value: &str) {
        if let Some(block) = self.open.get_mut(&index) {
            block[key] = json!(value);
        }
    }

    fn append_json(&mut self, index: u32, fragment: &str) {
        self.partial_json
            .entry(index)
            .or_default()
            .push_str(fragment);
    }

    /// End the current leg, folding its blocks into the replayable sequence.
    ///
    /// A block whose argument JSON never parsed is dropped rather than sent
    /// back malformed; the provider rejects a `tool_use` with invalid input,
    /// and one lost block is better than a failed continuation.
    fn seal(&mut self) {
        for (index, mut block) in std::mem::take(&mut self.open) {
            if let Some(partial) = self.partial_json.remove(&index) {
                match serde_json::from_str::<Value>(&partial) {
                    Ok(input) => block["input"] = input,
                    Err(_) => continue,
                }
            }
            self.sealed.push(block);
        }
        self.partial_json.clear();
    }

    fn sealed(&self) -> &[Value] {
        &self.sealed
    }
}

/// What a silent end of the byte stream means.
///
/// Anthropic closes every healthy message with a `message_delta` carrying a
/// `stop_reason`, so a stream that ends without one and with a tool call's
/// argument JSON still open was truncated: a clean TCP close mid-response
/// carries no transport error, and committing the fragment would hand
/// `parse_args` a cut-off call. Text alone keeps the clean-end reading the
/// agent loop already gives an exhausted stream.
fn end_of_stream(state: &StreamState) -> Option<ProviderEvent> {
    if state.terminal || state.finished || state.open_tool_blocks.is_empty() {
        return None;
    }
    Some(ProviderEvent::Failed {
        error: ProviderErrorInfo::provider("anthropic stream ended mid-tool-call"),
    })
}

fn u32_at(value: &Value, key: &str) -> u32 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0) as u32
}

/// Map one parsed Anthropic stream event into zero or more `ProviderEvent`s.
fn normalize(data: &Value, state: &mut StreamState) -> Vec<ProviderEvent> {
    if state.terminal {
        return Vec::new();
    }
    match data.get("type").and_then(Value::as_str) {
        // Anthropic answers 200, starts streaming, and can then send an error
        // frame instead of finishing — overloaded_error and api_error both
        // arrive this way. The stream closes right after, so without this arm
        // the truncated step would read as a clean end and commit.
        Some("error") => {
            state.terminal = true;
            vec![ProviderEvent::Failed {
                error: ProviderErrorInfo::from_error(&classify_in_band_error(
                    "anthropic",
                    data.get("error").unwrap_or(data),
                )),
            }]
        }
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
            // Capture before normalizing: a continuation replays the provider's
            // own blocks, not this adapter's reading of them.
            if let (Some(raw), Some(block)) = (state.raw_blocks.as_mut(), block) {
                raw.start(index, block);
            }
            match block.and_then(|b| b.get("type")).and_then(Value::as_str) {
                Some("tool_use") => {
                    let block = block.unwrap();
                    let name = str_at(block, "name");
                    state.open_tool_blocks.insert(index);
                    // The synthetic output tool is the request's own scaffolding.
                    // Announcing it as a tool call would hand consumers a call they
                    // never advertised and cannot answer.
                    if state.output_tool.as_deref() == Some(name.as_str()) {
                        state.output_block = Some(index);
                        return Vec::new();
                    }
                    state.saw_other_tool_call = true;
                    vec![ProviderEvent::ToolCallStarted {
                        index,
                        id: str_at(block, "id"),
                        name,
                    }]
                }
                // A thinking block opens (usually empty) and is filled by
                // deltas; a redacted one arrives complete. Both are captured
                // raw and emitted once they close — capturing is whole-block
                // or nothing, so a partial block can never be replayed.
                Some("thinking" | "redacted_thinking") => {
                    state
                        .pending_reasoning
                        .insert(index, block.unwrap().clone());
                    Vec::new()
                }
                // The provider running a tool on its own infrastructure. Its
                // arguments stream exactly like a client tool call's, but no
                // consumer ever answers it, so nothing is announced until the
                // matching result block completes the pair.
                Some("server_tool_use") => {
                    let block = block.unwrap();
                    state.open_tool_blocks.insert(index);
                    state.open_server_tools.insert(
                        index,
                        PendingServerTool {
                            id: str_at(block, "id"),
                            name: str_at(block, "name"),
                            input_json: String::new(),
                        },
                    );
                    Vec::new()
                }
                // The result of one of those calls, arriving whole. This is
                // where the search becomes visible to the host.
                Some("web_search_tool_result") => {
                    let block = block.unwrap();
                    // An unpaired result still means a search ran; reporting it
                    // with what is known beats dropping it.
                    let call = state
                        .awaiting_server_results
                        .remove(&str_at(block, "tool_use_id"))
                        .unwrap_or_default();
                    let (output, is_error) = web_search_output(block.get("content"));
                    let input = call.input();
                    let id = if call.id.is_empty() {
                        str_at(block, "tool_use_id")
                    } else {
                        call.id
                    };
                    let name = if call.name.is_empty() {
                        VENDOR_WEB_SEARCH_TOOL.to_string()
                    } else {
                        call.name
                    };
                    // Cleartext `output` is for the host; the native pair keeps
                    // encrypted content for same-route replay after host tools
                    // force another model step in this turn.
                    let replay = state.replay_origin.as_ref().map(|origin| {
                        ProviderToolReplay::captured(
                            origin.clone(),
                            vec![
                                json!({
                                    "type": "server_tool_use",
                                    "id": id,
                                    "name": &name,
                                    "input": &input,
                                }),
                                json!({
                                    "type": "web_search_tool_result",
                                    "tool_use_id": id,
                                    "content": block.get("content").cloned().unwrap_or(Value::Null),
                                }),
                            ],
                        )
                    });
                    vec![ProviderEvent::ProviderExecutedToolCall {
                        name,
                        input,
                        output,
                        is_error,
                        replay,
                    }]
                }
                _ => Vec::new(),
            }
        }
        Some("content_block_delta") => {
            let index = u32_at(data, "index");
            let delta = match data.get("delta") {
                Some(delta) => delta,
                None => return Vec::new(),
            };
            if let Some(raw) = state.raw_blocks.as_mut() {
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => raw.append_text(index, "text", &str_at(delta, "text")),
                    Some("thinking_delta") => {
                        raw.append_text(index, "thinking", &str_at(delta, "thinking"));
                    }
                    Some("signature_delta") => {
                        raw.set(index, "signature", &str_at(delta, "signature"));
                    }
                    Some("input_json_delta") => {
                        raw.append_json(index, &str_at(delta, "partial_json"));
                    }
                    _ => {}
                }
            }
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => vec![ProviderEvent::TextDelta {
                    text: str_at(delta, "text"),
                }],
                Some("thinking_delta") => {
                    if let Some(block) = state.pending_reasoning.get_mut(&index) {
                        append_str_field(block, "thinking", &str_at(delta, "thinking"));
                    }
                    vec![ProviderEvent::ReasoningDelta {
                        text: str_at(delta, "thinking"),
                    }]
                }
                // The signature is what makes the block replayable; without
                // it the thinking text is display-only.
                Some("signature_delta") => {
                    if let Some(block) = state.pending_reasoning.get_mut(&index) {
                        block["signature"] = json!(str_at(delta, "signature"));
                    }
                    Vec::new()
                }
                // A constrained response is JSON in a tool call's arguments;
                // every other provider streams it as text, so it becomes text
                // here too and consumers stay provider-blind.
                Some("input_json_delta") if state.output_block == Some(index) => {
                    vec![ProviderEvent::TextDelta {
                        text: str_at(delta, "partial_json"),
                    }]
                }
                // A server tool's arguments accumulate the same way, but go
                // out with its result rather than as they stream: there is no
                // call for a consumer to watch, only a finished one to record.
                Some("input_json_delta") if state.open_server_tools.contains_key(&index) => {
                    if let Some(call) = state.open_server_tools.get_mut(&index) {
                        call.input_json.push_str(&str_at(delta, "partial_json"));
                    }
                    Vec::new()
                }
                Some("input_json_delta") => vec![ProviderEvent::ToolCallArgsDelta {
                    index,
                    fragment: str_at(delta, "partial_json"),
                }],
                _ => Vec::new(),
            }
        }
        Some("content_block_stop") => {
            let index = u32_at(data, "index");
            state.open_tool_blocks.remove(&index);
            // A closed server tool call has its whole argument JSON; park it
            // under the id its result block will name.
            if let Some(call) = state.open_server_tools.remove(&index) {
                state.awaiting_server_results.insert(call.id.clone(), call);
            }
            match state.pending_reasoning.remove(&index) {
                Some(block) => vec![ProviderEvent::ReasoningBlock { data: block }],
                None => Vec::new(),
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
                // However the message ended, the provider said it ended: a
                // later silent close is no longer truncation evidence.
                state.finished = true;
                // A pause this stream is allowed to resume is not an ending at
                // all. Seal the leg's blocks for replay and report nothing:
                // the caller re-issues the request and keeps streaming into
                // the same turn.
                if reason == "pause_turn" {
                    if let Some(raw) = state.raw_blocks.as_mut() {
                        raw.seal();
                        state.paused = true;
                        return events;
                    }
                }
                let mut reason = match map_stop_reason(reason) {
                    StopOutcome::Reason(reason) => reason,
                    // Whatever streamed before this stop is a fragment, so no
                    // clean stop may follow it. Failing the stream is what the
                    // agent loop reads as "discard this step's partial output";
                    // reporting a stop instead would commit the fragment — and
                    // any tool call inside it — as a finished answer.
                    StopOutcome::Interrupted(error) => {
                        events.push(ProviderEvent::Failed { error });
                        return events;
                    }
                };
                // The forced output tool is not a tool call the consumer has to
                // answer; the model said everything it had to say. Reporting
                // `ToolUse` here would read as a turn waiting on a tool result.
                if reason == StopReason::ToolUse
                    && state.output_block.is_some()
                    && !state.saw_other_tool_call
                {
                    reason = StopReason::EndTurn;
                }
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

/// Normalize a `web_search_tool_result` block's `content` into the shape the
/// host's own `web_search` tool returns, so one renderer draws both.
///
/// The content is a JSON array of results on success and a single error object
/// on failure, so the branch is on the shape rather than on any field. The
/// encrypted content Anthropic attaches to each result is dropped whole: it is
/// an opaque blob only replayable to Anthropic, and nothing here replays it.
/// `page_age` is a fuzzy human date rather than a timestamp, so it rides in
/// the result metadata the host shape already carries rather than being forced
/// into a typed field.
///
/// Returns the output and whether it is an error.
fn web_search_output(content: Option<&Value>) -> (Value, bool) {
    /// Same cap the host tool applies, so a vendor result cannot put more into
    /// context than the host's own search would.
    const MAX_RESULTS: usize = openwave_core::MAX_WEB_SEARCH_RESULTS;

    match content {
        Some(Value::Array(results)) => {
            let results: Vec<Value> = results
                .iter()
                .filter(|result| !str_at(result, "url").is_empty())
                .take(MAX_RESULTS)
                .map(|result| {
                    let mut normalized = json!({
                        "url": str_at(result, "url"),
                        "title": str_at(result, "title"),
                        // Anthropic returns no excerpt in the clear; the field
                        // stays present so the shape does not vary by provider.
                        "snippet": "",
                    });
                    let page_age = str_at(result, "page_age");
                    if !page_age.is_empty() {
                        normalized["metadata"] = json!({ "page_age": page_age });
                    }
                    normalized
                })
                .collect();
            (
                json!({ "provider": "anthropic", "results": results }),
                false,
            )
        }
        Some(Value::Object(error)) => {
            let code = error
                .get("error_code")
                .and_then(Value::as_str)
                .unwrap_or("unavailable");
            (json!({ "error_code": code }), true)
        }
        // No content, or a shape this adapter has not seen. The search still
        // ran, so it is reported as a failed one rather than dropped.
        _ => (json!({ "error_code": "unrecognized_result" }), true),
    }
}

fn str_at(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Append `fragment` to the string at `key` in a raw JSON block.
fn append_str_field(block: &mut Value, key: &str, fragment: &str) {
    let mut text = block
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    text.push_str(fragment);
    block[key] = json!(text);
}

/// What a provider `stop_reason` means for the step that was streaming.
#[derive(Debug, PartialEq, Eq)]
enum StopOutcome {
    /// A stop the agent may commit as this step's outcome.
    Reason(StopReason),
    /// A stop that invalidates everything streamed before it.
    Interrupted(ProviderErrorInfo),
}

fn map_stop_reason(reason: &str) -> StopOutcome {
    match reason {
        "max_tokens" => StopOutcome::Reason(StopReason::MaxTokens),
        "tool_use" => StopOutcome::Reason(StopReason::ToolUse),
        "stop_sequence" => StopOutcome::Reason(StopReason::StopSequence),
        "refusal" => StopOutcome::Reason(StopReason::Refusal),
        // The conversation outgrew the model's context window while the
        // response was streaming. The 400 that reports the same overflow
        // before a stream starts becomes `PromptTooLong` and drives the agent
        // loop's context-reduction climb. Preserve that classification so the
        // agent can discard this candidate and restart the same step against a
        // tighter transcript instead of spending a turn-level retry on the
        // same oversized request.
        "model_context_window_exceeded" => {
            StopOutcome::Interrupted(ProviderErrorInfo::from_error(&AgentError::PromptTooLong(
                "anthropic: the model's context window was exceeded mid-response".into(),
            )))
        }
        // The provider suspended the turn for a long-running server-side tool
        // and expects the paused response replayed back to resume it. Nothing
        // here drives that continuation, so the paused fragment is incomplete
        // by construction and must not read as a finished answer.
        "pause_turn" => StopOutcome::Interrupted(ProviderErrorInfo::provider(
            "anthropic: the provider paused the turn",
        )),
        // "end_turn" and anything we don't yet model fall back to a clean end.
        _ => StopOutcome::Reason(StopReason::EndTurn),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwave_core::provider::{ChatMessage, MessageReasoning, ReasoningOrigin};
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
            ..Default::default()
        };
        let body = build_request_json(&req).unwrap();
        assert_eq!(body["model"], "claude-opus-4-8");
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
        assert_eq!(body["stream"], true);
        assert_eq!(body["system"][0]["text"], "be brief");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        assert_eq!(body["tools"][0]["name"], "read_file");
        assert_eq!(body["temperature"], 0.5);
        assert!(
            body.get("fallbacks").is_none(),
            "fallback models require an explicit registry contract"
        );
    }

    #[test]
    fn cache_breakpoints_sit_on_the_last_tool_system_block_and_transcript_tail() {
        let req = ChatRequest {
            provider: Some(ProviderId::new("anthropic")),
            model: "claude-opus-4-8".into(),
            system: Some("be brief".into()),
            messages: vec![
                ChatMessage::text(Role::User, "hi"),
                ChatMessage::text(Role::Assistant, "hello"),
            ],
            tools: vec![
                ToolSpec {
                    name: "read_file".into(),
                    description: "read a file".into(),
                    input_schema: json!({"type": "object"}),
                },
                ToolSpec {
                    name: "write_file".into(),
                    description: "write a file".into(),
                    input_schema: json!({"type": "object"}),
                },
            ],
            images: ImageAttachments::new(),
            ..Default::default()
        };
        let body = build_request_json(&req).unwrap();

        let ephemeral = json!({ "type": "ephemeral" });
        // Tools render before the system prompt, which renders before the
        // messages, so these three breakpoints are the ends of the three
        // segments — in ascending prefix order.
        assert!(body["tools"][0].get("cache_control").is_none());
        assert_eq!(body["tools"][1]["cache_control"], ephemeral);
        assert_eq!(body["system"][0]["cache_control"], ephemeral);
        assert!(body["messages"][0]["content"][0]
            .get("cache_control")
            .is_none());
        assert_eq!(
            body["messages"][1]["content"][0]["cache_control"],
            ephemeral
        );
        // Four is the hard cap; going over rejects the request outright.
        let breakpoints = serde_json::to_string(&body)
            .unwrap()
            .matches("cache_control")
            .count();
        assert_eq!(breakpoints, 3, "{body}");
    }

    #[test]
    fn a_lagging_breakpoint_keeps_a_wide_tool_fan_out_inside_the_lookup_window() {
        // One model step appends an assistant message with 15 tool calls and a
        // user message with their 15 results: 30 blocks at once, beyond the
        // 20-block cache-lookup window. A lone tail breakpoint would then sit
        // further than 20 blocks from everything the previous call cached, and
        // the next call would silently pay a full-price read. This pins the
        // lagging breakpoint that keeps the previous tail inside a window.
        let mut messages = vec![
            ChatMessage::text(Role::User, "hi"),
            ChatMessage::text(Role::Assistant, "on it"),
        ];
        // Flattened block index of the previous call's tail breakpoint.
        let previous_tail = 1usize;
        messages.push(ChatMessage {
            role: Role::Assistant,
            content: (0..15)
                .map(|i| ContentBlock::ToolUse {
                    id: format!("call_{i}"),
                    name: "read_file".into(),
                    input: json!({"path": format!("f{i}.rs")}),
                })
                .collect(),
            reasoning: MessageReasoning::default(),
        });
        messages.push(ChatMessage {
            role: Role::User,
            content: (0..15)
                .map(|i| ContentBlock::ToolResult {
                    tool_use_id: format!("call_{i}"),
                    content: "ok".into(),
                    is_error: false,
                })
                .collect(),
            reasoning: MessageReasoning::default(),
        });
        let req = ChatRequest {
            provider: Some(ProviderId::new("anthropic")),
            model: "claude-opus-4-8".into(),
            system: Some("be brief".into()),
            messages,
            tools: vec![ToolSpec {
                name: "read_file".into(),
                description: "read a file".into(),
                input_schema: json!({"type": "object"}),
            }],
            images: ImageAttachments::new(),
            ..Default::default()
        };
        let body = build_request_json(&req).unwrap();

        // Flatten the transcript's content blocks in wire order and locate the
        // breakpoints.
        let mut breakpoints = Vec::new();
        let mut index = 0usize;
        for message in body["messages"].as_array().unwrap() {
            for block in message["content"].as_array().unwrap() {
                if block.get("cache_control").is_some() {
                    breakpoints.push(index);
                }
                index += 1;
            }
        }
        assert_eq!(index, 32);
        let [lagging, tail] = breakpoints.as_slice() else {
            panic!("expected exactly two transcript breakpoints: {body}");
        };
        assert_eq!(*tail, index - 1);
        // The lagging breakpoint trails the tail by exactly one window, and
        // the previous call's tail — 30 blocks back from the new tail, out of
        // reach of it — sits inside the lagging breakpoint's window.
        assert_eq!(tail - lagging, 20);
        assert!(*lagging > previous_tail);
        assert!(*lagging - previous_tail <= 20);
        // Four is the hard cap; going over rejects the request outright.
        let all_breakpoints = serde_json::to_string(&body)
            .unwrap()
            .matches("cache_control")
            .count();
        assert_eq!(all_breakpoints, 4, "{body}");
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
            ..Default::default()
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
    fn a_constrained_response_is_a_forced_tool_read_back_as_text() {
        let mut req = reasoning_request("claude-opus-5", Some(ReasoningEffort::Low));
        req.response_format = Some(ResponseFormat::JsonSchema {
            name: "note".into(),
            schema: json!({
                "type": "object",
                "properties": { "body": { "type": "string" } },
                "required": ["body"],
            }),
        });
        let body = build_request_json(&req).unwrap();
        assert_eq!(
            body["tool_choice"],
            json!({ "type": "tool", "name": "note" })
        );
        assert_eq!(body["tools"][0]["name"], "note");
        assert_eq!(
            body["tools"][0]["input_schema"]["additionalProperties"],
            false
        );
        // With extended thinking on, this API accepts only `auto` or `none` for
        // `tool_choice`, so a constrained request cannot also think.
        assert!(body.get("thinking").is_none());

        // The constrained value arrives on the text channel, and the turn ends
        // rather than reading as one waiting on a tool result. Both consumers of
        // this stream reject a tool call they never advertised.
        let mut state = StreamState {
            output_tool: Some("note".into()),
            ..StreamState::default()
        };
        let events: Vec<ProviderEvent> = [
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "tool_use", "id": "toolu_1", "name": "note" },
            }),
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "input_json_delta", "partial_json": "{\"body\":\"hi\"}" },
            }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" } }),
        ]
        .iter()
        .flat_map(|frame| normalize(frame, &mut state))
        .collect();
        assert_eq!(
            events,
            vec![
                ProviderEvent::TextDelta {
                    text: "{\"body\":\"hi\"}".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        );
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
        run_with_origin(events, None)
    }

    fn run_with_origin(
        events: &[Value],
        replay_origin: Option<ReasoningOrigin>,
    ) -> Vec<ProviderEvent> {
        let mut state = StreamState {
            replay_origin,
            ..StreamState::default()
        };
        events
            .iter()
            .flat_map(|e| normalize(e, &mut state))
            .collect()
    }

    fn search_origin(model: &str) -> ReasoningOrigin {
        ReasoningOrigin {
            provider: Some(ProviderId::new("anthropic")),
            model: model.into(),
        }
    }

    #[test]
    fn in_band_error_fails_the_stream_instead_of_ending_it_cleanly() {
        let out = run(&[
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "partial"}}),
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
            // Anything after the error must not resurrect a clean stop.
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
        ]);
        assert_eq!(
            out,
            vec![
                ProviderEvent::TextDelta {
                    text: "partial".into()
                },
                ProviderEvent::Failed {
                    error: ProviderErrorInfo {
                        kind: "overloaded".into(),
                        message: "anthropic returned 500 (overloaded_error): Overloaded".into(),
                    },
                },
            ]
        );
    }

    #[test]
    fn mid_stream_context_overflow_fails_the_stream_and_strands_no_tool_call() {
        let out = run(&[
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "read_file"}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"path\":\"a"}}),
            json!({"type": "message_delta", "delta": {"stop_reason": "model_context_window_exceeded"}, "usage": {"output_tokens": 9}}),
        ]);
        assert_eq!(
            out,
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "toolu_1".into(),
                    name: "read_file".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: "{\"path\":\"a".into(),
                },
                ProviderEvent::Usage(Usage {
                    output_tokens: 9,
                    ..Usage::default()
                }),
                ProviderEvent::Failed {
                    error: ProviderErrorInfo::from_error(&AgentError::PromptTooLong(
                        "anthropic: the model's context window was exceeded mid-response".into(),
                    )),
                },
            ]
        );
    }

    #[test]
    fn a_silent_close_with_an_open_tool_call_fails_the_stream() {
        // A clean TCP close mid-response carries no transport error and no
        // stop_reason. With a tool call's argument JSON still open the stream
        // was truncated, so the adapter must not let the fragment read as a
        // finished turn. This changes behavior on streams that reported
        // success before — the remaining silent-close route after the
        // transport-error and in-band-error paths were closed.
        let mut state = StreamState::default();
        let out: Vec<ProviderEvent> = [
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "read_file"}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"path\":\"a"}}),
        ]
        .iter()
        .flat_map(|frame| normalize(frame, &mut state))
        .collect();
        assert!(matches!(
            out.last(),
            Some(ProviderEvent::ToolCallArgsDelta { .. })
        ));
        let ending = end_of_stream(&state).expect("an open tool call fails the stream");
        assert!(
            matches!(
                &ending,
                ProviderEvent::Failed { error } if error.kind == "provider"
            ),
            "expected a failure, got {ending:?}"
        );

        // Once the block stops and the provider reports a stop_reason, the
        // close is clean and the end-of-stream check stays silent.
        let mut state = StreamState::default();
        for frame in [
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "read_file"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}}),
        ] {
            let _ = normalize(&frame, &mut state);
        }
        assert!(end_of_stream(&state).is_none());

        // Text alone keeps the clean-end reading an exhausted stream already
        // had: no tool-call arguments exist that truncation could corrupt.
        let mut state = StreamState::default();
        let _ = normalize(
            &json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "hi"}}),
            &mut state,
        );
        assert!(end_of_stream(&state).is_none());
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
    fn reasoning_blocks_are_captured_whole_and_opaque() {
        let out = run(&[
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "plan: "}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "read first"}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig-1"}}),
            json!({"type": "content_block_stop", "index": 0}),
            // A redacted block arrives complete and must round-trip untouched:
            // filtering capture on `type == "thinking"` would drop it and
            // split the message's reasoning prefix on replay.
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "redacted_thinking", "data": "opaque-blob"}}),
            json!({"type": "content_block_stop", "index": 1}),
        ]);
        assert_eq!(
            out,
            vec![
                ProviderEvent::ReasoningDelta {
                    text: "plan: ".into()
                },
                ProviderEvent::ReasoningDelta {
                    text: "read first".into()
                },
                ProviderEvent::ReasoningBlock {
                    data: json!({
                        "type": "thinking",
                        "thinking": "plan: read first",
                        "signature": "sig-1",
                    }),
                },
                ProviderEvent::ReasoningBlock {
                    data: json!({
                        "type": "redacted_thinking",
                        "data": "opaque-blob",
                    }),
                },
            ]
        );
    }

    #[test]
    fn captured_reasoning_is_replayed_verbatim_ahead_of_its_content() {
        let reasoning = vec![
            json!({"type": "thinking", "thinking": "plan: read first", "signature": "sig-1"}),
            json!({"type": "redacted_thinking", "data": "opaque-blob"}),
        ];
        let mut req = reasoning_request("claude-opus-5", None);
        req.messages = vec![
            ChatMessage::text(Role::User, "hi"),
            ChatMessage {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "checking".into(),
                    },
                    ContentBlock::ToolUse {
                        id: "toolu_1".into(),
                        name: "read_file".into(),
                        input: json!({"path": "a"}),
                    },
                ],
                reasoning: MessageReasoning::captured(
                    ReasoningOrigin {
                        provider: Some(ProviderId::new("anthropic")),
                        model: "claude-opus-5".into(),
                    },
                    reasoning.clone(),
                ),
            },
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "toolu_1".into(),
                    content: "ok".into(),
                    is_error: false,
                }],
                reasoning: MessageReasoning::default(),
            },
        ];
        let body = build_request_json(&req).unwrap();
        let content = body["messages"][1]["content"].as_array().unwrap();
        assert_eq!(
            content[..2],
            reasoning[..],
            "the blocks go back byte-identical, redacted included"
        );
        assert_eq!(content[2]["type"], "text");
        assert_eq!(content[3]["type"], "tool_use");
        // The transcript-tail breakpoint still lands on the true tail.
        assert_eq!(
            body["messages"][2]["content"][0]["cache_control"],
            ephemeral_cache_control()
        );
    }

    #[test]
    fn a_route_switch_replays_no_foreign_reasoning() {
        // A chat may move between Anthropic, OpenAI and Gemini models — and
        // between two Anthropic models — mid-conversation. History rebuilt
        // across such a switch still carries the blocks the earlier model
        // signed, and this model would reject them. Dropping them is the
        // answer: sending no reasoning is always a valid shape.
        let block = json!({"type": "thinking", "thinking": "plan", "signature": "sig-1"});
        let step = |provider: Option<&str>, model: &str| ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "checking".into(),
            }],
            reasoning: MessageReasoning::captured(
                ReasoningOrigin {
                    provider: provider.map(ProviderId::new),
                    model: model.into(),
                },
                vec![block.clone()],
            ),
        };
        for origin in [
            // Another Anthropic model: same wire shape, signature this model
            // never produced.
            step(Some("anthropic"), "claude-sonnet-5"),
            // Another provider entirely.
            step(Some("openai"), "claude-opus-5"),
        ] {
            let mut req = reasoning_request("claude-opus-5", None);
            req.messages = vec![ChatMessage::text(Role::User, "hi"), origin];
            let body = build_request_json(&req).unwrap();
            let content = body["messages"][1]["content"].as_array().unwrap();
            assert_eq!(content.len(), 1, "no block was replayed: {body}");
            assert_eq!(content[0]["type"], "text");
        }
    }

    #[test]
    fn reasoning_is_omitted_when_the_request_does_not_think() {
        // A constrained request cannot think (forced tool), so the replay
        // stays off entirely: omission is always valid, and blocks sent to a
        // non-thinking request would be a shape the API has not promised.
        let mut req = reasoning_request("claude-opus-5", None);
        req.response_format = Some(ResponseFormat::JsonSchema {
            name: "note".into(),
            schema: json!({
                "type": "object",
                "properties": { "body": { "type": "string" } },
                "required": ["body"],
            }),
        });
        req.messages.push(ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "checking".into(),
            }],
            reasoning: MessageReasoning::captured(
                ReasoningOrigin {
                    provider: Some(ProviderId::new("anthropic")),
                    model: "claude-opus-5".into(),
                },
                vec![json!({"type": "thinking", "thinking": "t", "signature": "s"})],
            ),
        });
        req.messages.push(ChatMessage::text(Role::User, "go on"));
        let body = build_request_json(&req).unwrap();
        assert!(body.get("thinking").is_none());
        assert_eq!(
            body["messages"][1]["content"].as_array().unwrap().len(),
            1,
            "no reasoning block is attached: {body}"
        );
    }

    #[test]
    fn maps_stop_reasons() {
        use StopOutcome::{Interrupted, Reason};
        assert_eq!(map_stop_reason("tool_use"), Reason(StopReason::ToolUse));
        assert_eq!(map_stop_reason("max_tokens"), Reason(StopReason::MaxTokens));
        assert_eq!(map_stop_reason("refusal"), Reason(StopReason::Refusal));
        assert_eq!(
            map_stop_reason("future_reason"),
            Reason(StopReason::EndTurn)
        );
        assert!(matches!(map_stop_reason("pause_turn"), Interrupted(_)));
        assert!(matches!(
            map_stop_reason("model_context_window_exceeded"),
            Interrupted(_)
        ));
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
    fn refusal_after_text_deltas_preserves_the_streamed_prefix() {
        let out = run(&[
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "A partial "}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "answer"}}),
            json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": "refusal",
                    "stop_details": {"type": "refusal", "category": "general_harms"}
                },
                "usage": {"output_tokens": 3}
            }),
        ]);
        assert_eq!(
            out,
            vec![
                ProviderEvent::TextDelta {
                    text: "A partial ".into(),
                },
                ProviderEvent::TextDelta {
                    text: "answer".into(),
                },
                ProviderEvent::Usage(Usage {
                    output_tokens: 3,
                    ..Usage::default()
                }),
                ProviderEvent::Refusal {
                    details: RefusalDetails::from_category(Some("general_harms")),
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
                reasoning: MessageReasoning::default(),
            }],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            reasoning_effort: None,
            images,
            ..Default::default()
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
        let shaped = anthropic_content(
            &blocks,
            &ImageAttachments::new(),
            false,
            Some(&ProviderId::new("anthropic")),
            "claude-opus-5",
        )
        .unwrap();
        assert_eq!(shaped, serde_json::to_value(&blocks).unwrap());
    }

    // ── Vendor web search ──────────────────────────────────────────

    fn search_request(model: &str) -> ChatRequest {
        ChatRequest {
            provider: Some(ProviderId::new("anthropic")),
            model: model.into(),
            messages: vec![ChatMessage::text(Role::User, "what happened today?")],
            tools: vec![ToolSpec {
                name: "read_file".into(),
                description: "read a file".into(),
                input_schema: json!({"type": "object"}),
            }],
            vendor_web_search: Some(openwave_core::provider::VendorWebSearch { max_uses: 3 }),
            ..Default::default()
        }
    }

    #[test]
    fn the_vendor_search_tool_is_declared_beside_the_client_tools() {
        let body = build_request_json(&search_request("claude-opus-5")).unwrap();
        let tools = body["tools"].as_array().unwrap();
        // The client tool is untouched: a turn that may search may still call
        // everything the host advertised.
        assert_eq!(tools[0]["name"], "read_file");
        assert_eq!(
            tools[1],
            json!({
                "type": "web_search_20260209",
                "name": "web_search",
                "max_uses": 3,
                // The last tool carries the cache breakpoint, which is settled
                // after the whole array is built.
                "cache_control": {"type": "ephemeral"},
            })
        );

        // Absent the control, nothing about the request changes.
        let mut plain = search_request("claude-opus-5");
        plain.vendor_web_search = None;
        let body = build_request_json(&plain).unwrap();
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn the_search_tool_version_follows_the_model() {
        for id in [
            "claude-opus-5",
            "claude-sonnet-5",
            "claude-opus-4-6",
            "claude-sonnet-4-6-20260101",
            "claude-opus-4-8",
        ] {
            assert_eq!(web_search_tool_type(id), "web_search_20260209", "{id}");
        }
        for id in [
            // Haiku is the small tier in every generation.
            "claude-haiku-4-5-20251001",
            "claude-haiku-5",
            // Older than the current revision.
            "claude-opus-4-5",
            "claude-3-5-sonnet-20241022",
            // No readable generation: the basic tool is the one every
            // search-capable model accepts.
            "some-gateway-alias",
        ] {
            assert_eq!(web_search_tool_type(id), "web_search_20250305", "{id}");
        }
    }

    #[test]
    fn a_prior_client_side_search_is_replayed_under_another_name() {
        // The request declares a *server* tool called `web_search`. Replaying
        // client tool_use blocks under that same name is undefined, so history
        // goes back renamed; results pair by id and are untouched.
        let mut req = search_request("claude-opus-5");
        req.messages.push(ChatMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolUse {
                    id: "toolu_1".into(),
                    name: "web_search".into(),
                    input: json!({"query": "yesterday"}),
                },
                ContentBlock::ToolUse {
                    id: "toolu_2".into(),
                    name: "read_file".into(),
                    input: json!({"path": "a"}),
                },
            ],
            reasoning: MessageReasoning::default(),
        });
        req.messages.push(ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "toolu_1".into(),
                content: "{}".into(),
                is_error: false,
            }],
            reasoning: MessageReasoning::default(),
        });

        let body = build_request_json(&req).unwrap();
        let assistant = &body["messages"][1]["content"];
        assert_eq!(assistant[0]["name"], "web_search_prior");
        assert_eq!(assistant[0]["id"], "toolu_1");
        assert_eq!(assistant[0]["input"], json!({"query": "yesterday"}));
        assert_eq!(assistant[1]["name"], "read_file");
        assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "toolu_1");

        // Without the vendor tool there is no collision, so the name stands.
        req.vendor_web_search = None;
        let body = build_request_json(&req).unwrap();
        assert_eq!(body["messages"][1]["content"][0]["name"], "web_search");
    }

    #[test]
    fn a_provider_executed_call_without_native_replay_becomes_cleartext_prose() {
        // No same-route native blocks — foreign provider, missing capture, or
        // truncated history — so the call goes back as titles/URLs the next
        // model can still use.
        let mut req = search_request("claude-opus-5");
        req.messages.push(ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ProviderExecutedToolCall {
                name: "web_search".into(),
                input: json!({"query": "rust 2027"}),
                output: json!({
                    "provider": "anthropic",
                    "results": [{"title": "A", "url": "https://a"}]
                }),
                is_error: false,
                replay: None,
            }],
            reasoning: MessageReasoning::default(),
        });
        let body = build_request_json(&req).unwrap();
        let block = &body["messages"][1]["content"][0];
        assert_eq!(block["type"], "text");
        assert_eq!(
            block["text"],
            "[web_search: rust 2027 -> 1 results]\n- A — https://a"
        );
    }

    #[test]
    fn a_same_route_provider_executed_call_replays_native_blocks() {
        let origin = search_origin("claude-opus-5");
        let native = vec![
            json!({
                "type": "server_tool_use",
                "id": "srvtoolu_1",
                "name": "web_search",
                "input": {"query": "rust 2027"},
            }),
            json!({
                "type": "web_search_tool_result",
                "tool_use_id": "srvtoolu_1",
                "content": [{
                    "type": "web_search_result",
                    "url": "https://a",
                    "title": "A",
                    "encrypted_content": "opaque",
                }],
            }),
        ];
        let mut req = search_request("claude-opus-5");
        req.messages.push(ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ProviderExecutedToolCall {
                name: "web_search".into(),
                input: json!({"query": "rust 2027"}),
                output: json!({
                    "provider": "anthropic",
                    "results": [{"title": "A", "url": "https://a", "snippet": ""}]
                }),
                is_error: false,
                replay: Some(ProviderToolReplay::captured(origin, native.clone())),
            }],
            reasoning: MessageReasoning::default(),
        });
        let body = build_request_json(&req).unwrap();
        let content = body["messages"][1]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0], native[0]);
        assert_eq!(content[1]["type"], "web_search_tool_result");
        assert_eq!(
            content[1]["content"][0]["encrypted_content"], "opaque",
            "native encrypted content survives encoding"
        );
        // The transcript-tail cache breakpoint lands on the last block; that
        // is orthogonal to search replay and must not strip the result.
        assert_eq!(content[1]["cache_control"], json!({"type": "ephemeral"}));

        // A different model on the same provider cannot take those blocks.
        req.model = "claude-sonnet-5".into();
        let body = build_request_json(&req).unwrap();
        assert_eq!(body["messages"][1]["content"][0]["type"], "text");
        assert!(body["messages"][1]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("https://a"));
    }

    /// The frames Anthropic sends for one completed server-side search.
    fn search_frames(result_content: Value) -> Vec<Value> {
        vec![
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text", "text": ""},
            }),
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": "Let me check."},
            }),
            json!({"type": "content_block_stop", "index": 0}),
            json!({
                "type": "content_block_start",
                "index": 1,
                "content_block": {
                    "type": "server_tool_use",
                    "id": "srvtoolu_1",
                    "name": "web_search",
                    "input": {},
                },
            }),
            json!({
                "type": "content_block_delta",
                "index": 1,
                "delta": {"type": "input_json_delta", "partial_json": "{\"query\":\"rus"},
            }),
            json!({
                "type": "content_block_delta",
                "index": 1,
                "delta": {"type": "input_json_delta", "partial_json": "t 2027\"}"},
            }),
            json!({"type": "content_block_stop", "index": 1}),
            json!({
                "type": "content_block_start",
                "index": 2,
                "content_block": {
                    "type": "web_search_tool_result",
                    "tool_use_id": "srvtoolu_1",
                    "content": result_content,
                },
            }),
            json!({"type": "content_block_stop", "index": 2}),
        ]
    }

    #[test]
    fn a_completed_vendor_search_becomes_one_provider_executed_call() {
        let mut frames = search_frames(json!([
            {
                "type": "web_search_result",
                "url": "https://example.com/a",
                "title": "A",
                "encrypted_content": "opaque-and-enormous",
                "page_age": "April 30, 2026",
            },
            {
                "type": "web_search_result",
                "url": "https://example.com/b",
                "title": "B",
                "encrypted_content": "opaque",
            },
            // No url: nothing citable, so nothing to report.
            {"type": "web_search_result", "title": "C"},
        ]));
        frames.push(json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}));
        let origin = search_origin("claude-opus-5");
        let out = run_with_origin(&frames, Some(origin.clone()));

        let ProviderEvent::ProviderExecutedToolCall {
            name,
            input,
            output,
            is_error,
            replay,
        } = &out[1]
        else {
            panic!("expected provider-executed search: {out:?}");
        };
        assert_eq!(name, "web_search");
        assert_eq!(input, &json!({"query": "rust 2027"}));
        assert_eq!(
            output,
            &json!({
                "provider": "anthropic",
                "results": [
                    {
                        "url": "https://example.com/a",
                        "title": "A",
                        "snippet": "",
                        "metadata": {"page_age": "April 30, 2026"},
                    },
                    {"url": "https://example.com/b", "title": "B", "snippet": ""},
                ],
            })
        );
        assert!(!*is_error);
        let replay = replay.as_ref().expect("native replay was captured");
        assert_eq!(replay.origin(), Some(&origin));
        assert_eq!(
            replay.blocks()[0],
            json!({
                "type": "server_tool_use",
                "id": "srvtoolu_1",
                "name": "web_search",
                "input": {"query": "rust 2027"},
            })
        );
        assert_eq!(
            replay.blocks()[1]["content"][0]["encrypted_content"],
            "opaque-and-enormous"
        );
        assert!(matches!(
            out.last(),
            Some(ProviderEvent::Stop {
                reason: StopReason::EndTurn
            })
        ));
    }

    #[test]
    fn a_failed_vendor_search_is_reported_rather_than_dropped() {
        // The result content is a single error object here, not an array.
        // Indexing it as one would panic or silently report zero results.
        let frames = search_frames(json!({
            "type": "web_search_tool_result_error",
            "error_code": "max_uses_exceeded",
        }));
        let out = run_with_origin(&frames, Some(search_origin("claude-opus-5")));
        let ProviderEvent::ProviderExecutedToolCall {
            name,
            input,
            output,
            is_error,
            replay,
        } = out.last().unwrap()
        else {
            panic!("expected failed search: {out:?}");
        };
        assert_eq!(name, "web_search");
        assert_eq!(input, &json!({"query": "rust 2027"}));
        assert_eq!(output, &json!({"error_code": "max_uses_exceeded"}));
        assert!(*is_error);
        assert!(replay.is_some());

        // A shape this adapter has not seen is still a search that ran.
        let out = run(&search_frames(json!("surprise")));
        assert!(matches!(
            out.last().unwrap(),
            ProviderEvent::ProviderExecutedToolCall { is_error: true, .. }
        ));
    }

    #[tokio::test]
    async fn a_paused_turn_is_resumed_inside_the_adapter() {
        use axum::extract::State;
        use axum::response::IntoResponse;
        use axum::routing::post;
        use axum::Router;
        use std::sync::{Arc, Mutex};

        fn sse(frames: &[Value]) -> String {
            frames
                .iter()
                .map(|frame| format!("data: {frame}\n\n"))
                .collect()
        }

        #[derive(Clone, Default)]
        struct Script(Arc<Mutex<Vec<Value>>>);

        async fn respond(
            State(script): State<Script>,
            axum::Json(body): axum::Json<Value>,
        ) -> impl IntoResponse {
            let mut seen = script.0.lock().unwrap();
            seen.push(body);
            let leg = seen.len();
            let mut frames = search_frames(json!([{
                "type": "web_search_result",
                "url": "https://example.com/a",
                "title": "A",
                "encrypted_content": "opaque",
            }]));
            // The first response pauses mid-turn; the second finishes it.
            frames.push(json!({
                "type": "message_delta",
                "delta": {"stop_reason": if leg == 1 { "pause_turn" } else { "end_turn" }},
                "usage": {"output_tokens": 5},
            }));
            (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                sse(&frames),
            )
        }

        let script = Script::default();
        let app = Router::new()
            .fallback(post(respond))
            .with_state(script.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let provider = AnthropicProvider::new("key").with_base_url(format!("http://{address}"));
        let events: Vec<ProviderEvent> = provider
            .stream(search_request("claude-opus-5"))
            .await
            .unwrap()
            .collect()
            .await;
        server.abort();

        // Two requests, one turn: the pause never reaches the consumer, and
        // exactly one stop closes the stream.
        let requests = script.0.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ProviderEvent::Stop { .. }))
                .count(),
            1
        );
        assert!(matches!(
            events.last(),
            Some(ProviderEvent::Stop {
                reason: StopReason::EndTurn
            })
        ));
        // Both legs' searches surface.
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ProviderEvent::ProviderExecutedToolCall { .. }))
                .count(),
            2
        );

        // The continuation replays the paused response verbatim — encrypted
        // search content included, because that is what the provider resumes
        // against — with no user message invented to carry it.
        let messages = requests[1]["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["role"], "assistant");
        let blocks = messages[1]["content"].as_array().unwrap();
        assert_eq!(blocks[0], json!({"type": "text", "text": "Let me check."}));
        assert_eq!(
            blocks[1],
            json!({
                "type": "server_tool_use",
                "id": "srvtoolu_1",
                "name": "web_search",
                "input": {"query": "rust 2027"},
            })
        );
        assert_eq!(blocks[2]["type"], "web_search_tool_result");
        assert_eq!(
            blocks[2]["content"][0]["encrypted_content"], "opaque",
            "the provider validates the resumed turn against what it sent"
        );
    }

    #[tokio::test]
    async fn a_conversation_is_declared_to_a_gateway_and_withheld_from_anthropic() {
        use axum::extract::State;
        use axum::http::{header, HeaderMap};
        use axum::response::IntoResponse;
        use axum::routing::post;
        use axum::Router;
        use std::sync::{Arc, Mutex};

        #[derive(Clone, Default)]
        struct Capture(Arc<Mutex<Vec<HeaderMap>>>);

        async fn capture(State(capture): State<Capture>, headers: HeaderMap) -> impl IntoResponse {
            capture.0.lock().unwrap().push(headers);
            (
                [(header::CONTENT_TYPE, "text/event-stream")],
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
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

        let conversation = openwave_core::id::ChatId::new();
        let request = || ChatRequest {
            model: "claude-opus-4-8".into(),
            conversation: Some(conversation),
            messages: vec![ChatMessage::text(Role::User, "hi")],
            ..Default::default()
        };

        let gateway = AnthropicProvider::new("token")
            .with_base_url(&base_url)
            .with_conversation_attribution();
        let mut stream = gateway.stream(request()).await.unwrap();
        while stream.next().await.is_some() {}

        // Same request, same conversation, an adapter that was not configured
        // for a gateway: how the host groups its chats is not Anthropic's.
        let direct = AnthropicProvider::new("key").with_base_url(&base_url);
        let mut stream = direct.stream(request()).await.unwrap();
        while stream.next().await.is_some() {}

        let requests = capture_state.0.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].get(GATEWAY_CONVERSATION_HEADER).unwrap(),
            conversation.to_string().as_str()
        );
        assert!(requests[1].get(GATEWAY_CONVERSATION_HEADER).is_none());
        server.abort();
    }

    #[tokio::test]
    async fn the_token_source_is_asked_for_the_request_conversation() {
        // The conversation must reach the token source, not just the header:
        // a gateway source mints inside the chat's attestation context, and a
        // token fetched without the conversation would record no observations
        // for the chat's tool calls.
        struct Recording(std::sync::Mutex<Vec<Option<openwave_core::id::ChatId>>>);

        #[async_trait::async_trait]
        impl crate::BearerTokenSource for Recording {
            async fn bearer_token(&self) -> openwave_core::Result<String> {
                unreachable!("the adapter must ask per conversation");
            }

            async fn bearer_token_for(
                &self,
                conversation: Option<openwave_core::id::ChatId>,
            ) -> openwave_core::Result<String> {
                self.0.lock().unwrap().push(conversation);
                Ok("mg_at_test".into())
            }
        }

        let source = std::sync::Arc::new(Recording(std::sync::Mutex::new(Vec::new())));
        let provider = AnthropicProvider::new("unused")
            .with_base_url("http://127.0.0.1:9")
            .with_token_source(source.clone());
        let conversation = openwave_core::id::ChatId::new();
        // The request itself fails (nothing listens); the token exchange has
        // already happened by then, which is all this asserts.
        let _ = provider
            .stream(ChatRequest {
                model: "claude-opus-4-8".into(),
                conversation: Some(conversation),
                messages: vec![ChatMessage::text(Role::User, "hi")],
                ..Default::default()
            })
            .await;
        assert_eq!(source.0.lock().unwrap().as_slice(), &[Some(conversation)]);
    }
}
