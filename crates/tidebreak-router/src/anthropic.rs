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
use tidebreak_core::error::{AgentError, ProviderErrorInfo, Result};
use tidebreak_core::provider::{
    provider_executed_tool_call_text, ChatRequest, ContentBlock, ModelProvider,
    PromptCacheRetention, ProviderEvent, ProviderId, ProviderToolReplay, ReasoningOrigin,
    RefusalDetails, ResponseFormat, StopReason, ToolChoice, Usage,
};
use tidebreak_core::tool::{strict_json_schema, OptionalProperties};
use tidebreak_core::{ImageAttachments, ReasoningEffort, Role};

use crate::sse::{
    classify_in_band_error, classify_provider_error, frame_data, read_bounded_error_body, SseFramer,
};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// The beta that unlocks `thinking.block_binding` — see [`fable_5_1_or_later`].
const THINKING_BINDING_BETA: &str = "thinking-binding-controls-2026-08-01";
const DEFAULT_MAX_TOKENS: u32 = 4096;

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
    provider_label: &'static str,
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
            provider_label: "anthropic",
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
        ProviderId::new(self.provider_label)
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let mut body = build_request_json(&req)?;
        // `build_request_json` has already rejected a format it cannot enforce.
        // A model on the native constraint answers on the text channel with no
        // tool to read back.
        let output_tool = match &req.response_format {
            Some(ResponseFormat::JsonSchema { name, .. })
                if !fable_5_1_or_later(req.request_shaping_model()) =>
            {
                Some(name.clone())
            }
            _ => None,
        };
        // Setup failures (connection, auth, 4xx/5xx) surface here as `Err` so the
        // router can classify and fail over; the returned stream only yields
        // normalized events.
        let response = self
            .send_authorized(&body, &req.model, req.wire_model(), req.conversation)
            .await?;

        // Only a request that can pause needs its raw blocks kept, and only
        // Anthropic's own server tools pause a turn.
        let continuations_allowed = req.vendor_web_search.is_some();
        let replay_origin = ReasoningOrigin {
            provider: req.provider.clone(),
            model: req.model.clone(),
        };
        let provider = self.clone();
        let conversation = req.conversation;
        let route_model = req.model.clone();
        let wire_model = req.wire_model().to_owned();
        let provider_name = self.provider_name();
        let ceiling = crate::http::timeouts().total_stream;
        let stream = async_stream::stream! {
            let mut response = response;
            let mut state = StreamState {
                provider_label: provider.provider_label,
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
                let mut framer = SseFramer::default();
                while let Some(chunk) = bytes.next().await {
                    // A mid-stream transport error must not read as a clean end:
                    // the accumulated tool-call arguments may be truncated
                    // mid-JSON, and acting on them silently corrupts the step.
                    let chunk = match chunk {
                        Ok(chunk) => chunk,
                        Err(error) => {
                            yield ProviderEvent::Failed {
                                error: ProviderErrorInfo::provider(
                                    error.client_message(provider_name),
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
                                    "{provider_name} {error}"
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
                // Flush a final frame that wasn't terminated by a blank line.
                let final_frame = match framer.finish() {
                    Ok(frame) => frame,
                    Err(error) => {
                        yield ProviderEvent::Failed {
                            error: ProviderErrorInfo::provider(format!(
                                "{provider_name} {error}"
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
                    match provider
                        .send_authorized(&body, &route_model, &wire_model, conversation)
                        .await
                    {
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
    /// Authorize and send one Messages leg.
    ///
    /// Managed-gateway requests revalidate their frozen selector and mint a
    /// fresh installation-pinned bearer immediately before every HTTP leg.
    /// The request-scoped lease stays alive until the response is established.
    async fn send_authorized(
        &self,
        body: &Value,
        route_model: &str,
        wire_model: &str,
        conversation: Option<tidebreak_core::id::SessionId>,
    ) -> Result<reqwest::Response> {
        match &self.token_source {
            Some(source) => {
                let (api_key, _route_lease) = crate::router::authorize_bearer_request(
                    &**source,
                    route_model,
                    wire_model,
                    conversation,
                )
                .await?;
                self.send(body, &api_key, conversation).await
            }
            None => self.send(body, &self.api_key, conversation).await,
        }
    }

    /// POST one Messages request and hand back its streaming response.
    ///
    /// Every leg of a paused turn goes out through here, so the credential,
    /// version header, and error classification are identical on the first
    /// request and on each continuation.
    async fn send(
        &self,
        body: &Value,
        api_key: &str,
        conversation: Option<tidebreak_core::id::SessionId>,
    ) -> Result<reqwest::Response> {
        // The binding control is a beta field, and the header that admits it
        // is decided from the body so every leg of a paused turn agrees with
        // the first.
        let binds_thinking = body.pointer("/thinking/block_binding").is_some();
        let body = serde_json::to_vec(body)?;
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let url = reqwest::Url::parse(&url)
            .map_err(|_| AgentError::config("invalid Messages API endpoint"))?;
        let request = self
            .client
            .post(url.clone())
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json");
        let mut request = request.header("x-api-key", api_key);
        if binds_thinking {
            request = request.header("anthropic-beta", THINKING_BINDING_BETA);
        }
        // A conversation is declared only where one is configured to be read.
        // The id is a UUID, so it satisfies the gateway's bound on the value
        // (1-256 ASCII graphic bytes) by construction.
        if let (true, Some(conversation)) = (self.conversation_attribution, conversation) {
            request = request.header(
                crate::router::GATEWAY_CONVERSATION_HEADER,
                conversation.to_string(),
            );
        }
        let response = request
            .body(body)
            .send()
            .await
            // reqwest's display includes the URL, and a gateway URL can carry
            // tenant-identifying parts; `AgentError` strings reach the client
            // via TurnFailed. Only the fact of a failed request surfaces.
            .map_err(|_| AgentError::Provider(format!("{} request failed", self.provider_label)))?;

        // Surface non-2xx without the raw body — it can echo key material, and
        // `AgentError` strings reach the client via TurnFailed. Status (+ a
        // stable error type/code when present) is enough for classification.
        let status = response.status();
        if !status.is_success() {
            let retry_after = crate::sse::retry_after_hint(response.headers());
            // A managed gateway names its designed refusals in a header; a
            // known code renders as its own copy ("an administrator revoked
            // this model") instead of a generic provider fault. Unknown
            // codes fall through to ordinary classification.
            let gateway_code = crate::sse::gateway_denial_code(response.headers());
            let body = read_bounded_error_body(response.bytes_stream()).await;
            if let Some(denial) =
                gateway_code.and_then(|code| crate::sse::classify_gateway_denial(&code, &body))
            {
                return Err(denial);
            }
            return Err(classify_provider_error(
                self.provider_label,
                status.as_u16(),
                &body,
                retry_after,
            ));
        }
        Ok(response)
    }

    fn provider_name(&self) -> &'static str {
        self.provider_label
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
    let caches_prompt = req.prompt_cache.writes_cache();
    let retention = req.prompt_cache_retention;
    let messages = req
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

    let mut body = json!({
        "model": req.wire_model(),
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
        // prompt can carry a cache breakpoint.
        let mut block = json!({ "type": "text", "text": system });
        if caches_prompt {
            block["cache_control"] = ephemeral_cache_control(retention);
        }
        body["system"] = json!([block]);
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
            "type": web_search_tool_type(req.request_shaping_model()),
            "name": VENDOR_WEB_SEARCH_TOOL,
            "max_uses": search.max_uses,
        });
        match body["tools"].as_array_mut() {
            Some(tools) => tools.push(vendor_tool),
            None => body["tools"] = json!([vendor_tool]),
        }
    }
    let shaping_model = req.request_shaping_model();
    match &req.response_format {
        Some(ResponseFormat::JsonSchema { name, schema }) => {
            let schema =
                strict_json_schema(schema, OptionalProperties::AcceptNull).ok_or_else(|| {
                    AgentError::Provider(format!(
                        "response format {name} has no strict JSON Schema form"
                    ))
                })?;
            if fable_5_1_or_later(shaping_model) {
                // Fable 5.1 rejects the forced call below, and it has the
                // native constraint the forced tool stood in for: the text
                // channel itself carries the schema-valid answer, so there is no
                // tool to re-read and the request keeps thinking.
                set_output_config(
                    &mut body,
                    "format",
                    json!({ "type": "json_schema", "schema": schema }),
                );
                return finish_request(body, req, caches_prompt, retention);
            }
            // Elsewhere the Messages API constrains output through a forced
            // tool call, so the schema becomes a tool nothing above the adapter
            // ever sees: the stream re-reads its arguments as text (see
            // `normalize`), which is the channel every other provider delivers
            // structured output on.
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
                // A forced choice is the caller's explicit ask; on a model that
                // returns 400 for it, failing here names the cause instead of
                // relaying the provider's rejection as a generic fault.
                if fable_5_1_or_later(shaping_model)
                    && matches!(choice, ToolChoice::Required | ToolChoice::Tool { .. })
                {
                    return Err(AgentError::Provider(format!(
                        "{shaping_model} rejects a forced tool choice"
                    )));
                }
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
    finish_request(body, req, caches_prompt, retention)
}

/// The tail of [`build_request_json`], after the tool array and the output
/// constraint are settled.
fn finish_request(
    mut body: Value,
    req: &ChatRequest,
    caches_prompt: bool,
    retention: PromptCacheRetention,
) -> Result<Value> {
    // After the whole tool array is settled, including a structured-output tool
    // appended above.
    if caches_prompt {
        mark_last_tool_cacheable(&mut body, retention);
    }
    if let Some(temperature) = req.temperature {
        body["temperature"] = json!(temperature);
    }
    // Extended thinking and a forced tool are mutually exclusive on the Messages
    // API: with thinking on, `tool_choice` may only be `auto` or `none`. The
    // forcing is the caller's explicit ask and reasoning is a quality
    // preference, so the ask wins and this request does not think.
    if req.reasoning_model
        && !forces_a_tool(req)
        && takes_adaptive_thinking(req.request_shaping_model())
    {
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
        if fable_5_1_or_later(req.request_shaping_model()) {
            // Fable 5.1 signs each thinking block against the prefix that
            // produced it and rejects a replayed block whose prefix has since
            // changed. This host does rewrite earlier turns — compaction and
            // image reduction both do — so the request asks for the stale
            // blocks to be dropped rather than the 400: the turn proceeds and
            // the model re-plans without them. The field is a beta; `send`
            // adds the header that admits it.
            body["thinking"]["block_binding"] = json!({ "prefix_mismatch_behavior": "drop_block" });
        }
        if let Some(effort) = req.reasoning_effort {
            set_output_config(
                &mut body,
                "effort",
                json!(wire_reasoning_effort(req.request_shaping_model(), effort).as_str()),
            );
        }
        attach_reasoning_blocks(&mut body, req);
    }
    // Last, so the breakpoints are placed against the block layout that
    // actually goes on the wire — `attach_reasoning_blocks` prepends thinking
    // blocks, which count toward the lookup window.
    if caches_prompt {
        mark_cacheable_transcript_tail(&mut body, retention);
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

/// The breakpoint marker.
///
/// The bare marker is the 5-minute TTL — right when calls land seconds apart,
/// as an agentic turn's do, where the 1-hour TTL would only double the write
/// price for reads that already happen well inside the default window. The
/// user's retention setting opts a conversation into `ttl: "1h"` (2× write,
/// same read price) for the human-paced case, where replies routinely arrive
/// after the default window has expired and every such reply rewrites the
/// whole prefix at the write premium.
///
/// The chosen TTL applies to every breakpoint this request places — tools,
/// system, lagging, and tail alike. Anthropic rejects a request whose
/// longer-TTL breakpoint appears after a shorter-TTL one, and the transcript
/// breakpoints render last, so extending only them (the intuitive "just the
/// tail" reading) is exactly the invalid order. A uniform TTL is also the
/// cheaper-to-reason-about policy: all four breakpoints sit inside one prefix
/// whose entries are written once and refreshed together on every read.
fn ephemeral_cache_control(retention: PromptCacheRetention) -> Value {
    match retention {
        PromptCacheRetention::FiveMinutes => json!({ "type": "ephemeral" }),
        PromptCacheRetention::OneHour => json!({ "type": "ephemeral", "ttl": "1h" }),
    }
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
fn mark_last_tool_cacheable(body: &mut Value, retention: PromptCacheRetention) {
    if let Some(last) = body
        .get_mut("tools")
        .and_then(Value::as_array_mut)
        .and_then(|tools| tools.last_mut())
        .and_then(Value::as_object_mut)
    {
        last.insert("cache_control".into(), ephemeral_cache_control(retention));
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
///
/// This runs last in `build_request_json`, after `attach_reasoning_blocks` has
/// prepended replayed thinking blocks: those are wire blocks and count toward
/// the lookup window, so spacing measured before them would understate the
/// real distance and quietly push the lagging breakpoint out of reach. Nothing
/// may add or remove a transcript block after this point — with one known
/// exception: a pause continuation (`replace_paused_assistant_message`) appends
/// or rewrites the paused assistant message on the already-built body, so a
/// continuation leg's newest blocks sit past the tail breakpoint and go
/// uncached. That costs one leg's delta, not correctness, and re-marking there
/// is not worth touching blocks that must replay byte-exact.
///
/// `cache_control` is not valid on a `thinking` or `redacted_thinking` block,
/// so a target position landing on one moves *toward the tail* — never away.
/// Moving away would grow the distance between the two breakpoints past the
/// window this function exists to respect, while moving toward it only
/// shortens it. The tail itself is the one position with nothing after it, so
/// there it moves backwards instead and the trailing thinking blocks go
/// uncached, which costs one step's delta and never correctness.
fn mark_cacheable_transcript_tail(body: &mut Value, retention: PromptCacheRetention) {
    /// How far Anthropic's cache lookup walks back from a breakpoint, in
    /// content blocks. The lagging breakpoint trails the tail by exactly one
    /// window, so consecutive calls keep a breakpoint within reach of one the
    /// previous call cached.
    const CACHE_LOOKUP_LOOKBACK_BLOCKS: usize = 20;

    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    // Flattened wire order: every content block counts for distance, but only
    // a markable one can carry the breakpoint.
    let mut blocks: Vec<(usize, usize, bool)> = Vec::new();
    for (message_index, message) in messages.iter().enumerate() {
        let Some(content) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for (block_index, block) in content.iter().enumerate() {
            blocks.push((message_index, block_index, is_cache_markable(block)));
        }
    }

    let Some(tail) = blocks.iter().rposition(|&(_, _, markable)| markable) else {
        return;
    };
    let lagging = tail
        .checked_sub(CACHE_LOOKUP_LOOKBACK_BLOCKS)
        .and_then(|target| {
            blocks[target..tail]
                .iter()
                .position(|&(_, _, markable)| markable)
                .map(|offset| target + offset)
        });

    for position in [Some(tail), lagging].into_iter().flatten() {
        let (message_index, block_index, _) = blocks[position];
        if let Some(block) = messages[message_index]["content"][block_index].as_object_mut() {
            block.insert("cache_control".into(), ephemeral_cache_control(retention));
        }
    }
}

/// Whether a content block may carry a `cache_control` marker.
///
/// Anthropic accepts one on text, image, tool_use, tool_result and document
/// blocks. Thinking and redacted_thinking blocks reject it, and the request
/// fails outright rather than degrading.
fn is_cache_markable(block: &Value) -> bool {
    !matches!(
        block.get("type").and_then(Value::as_str),
        Some("thinking" | "redacted_thinking")
    ) && block.is_object()
}

/// Whether the request obliges the model to call a specific tool or any tool.
///
/// A response format forces the synthetic output tool everywhere except on a
/// model with the native constraint, where nothing on the wire is forced.
fn forces_a_tool(req: &ChatRequest) -> bool {
    (req.response_format.is_some() && !fable_5_1_or_later(req.request_shaping_model()))
        || matches!(
            req.tool_choice,
            Some(ToolChoice::Required | ToolChoice::Tool { .. })
        )
}

/// Set one key of the request's `output_config`, keeping the others.
///
/// Effort and the output format both live under it and are decided at
/// different points of the build.
fn set_output_config(body: &mut Value, key: &str, value: Value) {
    match body.get_mut("output_config") {
        Some(Value::Object(config)) => {
            config.insert(key.to_owned(), value);
        }
        _ => body["output_config"] = json!({ key: value }),
    }
}

/// Whether `model` is Claude Fable 5.1, its Mythos twin, or a later release of
/// that line.
///
/// Fable 5.1 changed two things the rest of the line did not. It returns a 400
/// for a forced `tool_choice` (`any` and `tool`), where Opus 5 and Fable 5 both
/// think by default and still accept one — so the structured-output constraint
/// goes on `output_config.format` instead of a forced tool. And it binds each
/// thinking block to the conversation prefix that produced it, so a replayed
/// block behind a rewritten turn is rejected unless the request opts into
/// dropping it. Neither follows from the generation alone: an Opus of the same
/// generation keeps the old contract. So this reads the family and the
/// generation together, the way `web_search_tool_type` carves out Haiku.
fn fable_5_1_or_later(model: &str) -> bool {
    /// First release of the line on this contract.
    const FIRST: (u32, u32) = (5, 1);
    let leaf = model.rsplit('/').next().unwrap_or(model);
    (leaf.contains("fable") || leaf.contains("mythos"))
        && claude_generation(model).is_some_and(|generation| generation >= FIRST)
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

/// The final effort value safe to put on an Anthropic request.
///
/// Host model policy normally clamps a stored selection against the curated
/// catalog before building a [`ChatRequest`]. Keep the same guard at this last
/// request boundary for embedders and stale in-memory configurations that can
/// construct a request directly: Opus and Sonnet 4.6 accept `max`, but reject
/// the newer `xhigh` token.
fn wire_reasoning_effort(model: &str, effort: ReasoningEffort) -> ReasoningEffort {
    let claude_4_6 = claude_generation(model) == Some((4, 6))
        && (model.starts_with("claude-opus-") || model.starts_with("claude-sonnet-"));
    if claude_4_6 && effort == ReasoningEffort::XHigh {
        ReasoningEffort::High
    } else {
        effort
    }
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
    let leaf = model.rsplit('/').next().unwrap_or(model);
    let model = ["us.", "eu.", "apac.", "jp.", "au.", "global."]
        .iter()
        .find_map(|prefix| leaf.strip_prefix(prefix))
        .unwrap_or(leaf);
    let model = model.strip_prefix("anthropic.").unwrap_or(model);
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
    provider_label: &'static str,
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
        error: ProviderErrorInfo::provider(format!(
            "{} stream ended mid-tool-call",
            stream_provider_label(state)
        )),
    })
}

fn usage_u32_at(value: &Value, key: &str) -> u32 {
    u32::try_from(value.get(key).and_then(Value::as_u64).unwrap_or(0)).unwrap_or(u32::MAX)
}

fn stream_index(
    data: &Value,
    state: &mut StreamState,
) -> std::result::Result<u32, ProviderErrorInfo> {
    data.get("index")
        .and_then(Value::as_u64)
        .and_then(|index| u32::try_from(index).ok())
        .ok_or_else(|| {
            let provider = stream_provider_label(state).to_string();
            state.terminal = true;
            ProviderErrorInfo::provider(format!(
                "{provider} returned an invalid stream block index"
            ))
        })
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
            let error = data.get("error").unwrap_or(data);
            let error = classify_in_band_error(stream_provider_label(state), error);
            vec![ProviderEvent::Failed {
                error: ProviderErrorInfo::from_error(&error),
            }]
        }
        Some("message_start") => {
            if let Some(usage) = data.get("message").and_then(|m| m.get("usage")) {
                state.input_tokens = usage_u32_at(usage, "input_tokens");
                state.cache_read_input_tokens = usage_u32_at(usage, "cache_read_input_tokens");
                state.cache_creation_input_tokens =
                    usage_u32_at(usage, "cache_creation_input_tokens");
            }
            Vec::new()
        }
        Some("content_block_start") => {
            let index = match stream_index(data, state) {
                Ok(index) => index,
                Err(error) => return vec![ProviderEvent::Failed { error }],
            };
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
            let index = match stream_index(data, state) {
                Ok(index) => index,
                Err(error) => return vec![ProviderEvent::Failed { error }],
            };
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
                        raw.append_text(index, "signature", &str_at(delta, "signature"));
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
                        append_str_field(block, "signature", &str_at(delta, "signature"));
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
            let index = match stream_index(data, state) {
                Ok(index) => index,
                Err(error) => return vec![ProviderEvent::Failed { error }],
            };
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
                    output_tokens: usage_u32_at(usage, "output_tokens"),
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
                let mut reason = match map_stop_reason(stream_provider_label(state), reason) {
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

fn stream_provider_label(state: &StreamState) -> &'static str {
    if state.provider_label.is_empty() {
        "anthropic"
    } else {
        state.provider_label
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
    const MAX_RESULTS: usize = tidebreak_core::MAX_WEB_SEARCH_RESULTS;

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

fn map_stop_reason(provider_label: &str, reason: &str) -> StopOutcome {
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
                format!("{provider_label}: the model's context window was exceeded mid-response"),
            )))
        }
        // The provider suspended the turn for a long-running server-side tool
        // and expects the paused response replayed back to resume it. Nothing
        // here drives that continuation, so the paused fragment is incomplete
        // by construction and must not read as a finished answer.
        "pause_turn" => StopOutcome::Interrupted(ProviderErrorInfo::provider(format!(
            "{provider_label}: the provider paused the turn"
        ))),
        // "end_turn" and anything we don't yet model fall back to a clean end.
        _ => StopOutcome::Reason(StopReason::EndTurn),
    }
}

#[cfg(test)]
mod tests;
