//! Shared native Responses API transport for OpenAI and xAI.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use futures::stream::{BoxStream, StreamExt};
use serde_json::{json, Value};

use tidebreak_core::error::{AgentError, ProviderErrorInfo, Result};
use tidebreak_core::provider::{
    provider_executed_tool_call_text, ChatRequest, ContentBlock, ModelProvider, ProviderEvent,
    ProviderId, RefusalDetails, ResponseFormat, StopReason, ToolChoice, Usage,
};
use tidebreak_core::tool::{strict_json_schema, OptionalProperties};
use tidebreak_core::Role;

use crate::sse::{
    classify_in_band_error, classify_provider_error, frame_data, read_bounded_error_body,
    safe_http_error, SseFramer,
};
use crate::BearerTokenSource;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MAX_TOKENS: u32 = 4096;
const CHATGPT_BETA_HEADER: &str = "responses=v1";
const DEFAULT_CHATGPT_ORIGINATOR: &str = "tidebreak";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResponsesProfile {
    OpenAi,
    Xai,
}

impl ResponsesProfile {
    const fn provider(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Xai => "xai",
        }
    }

    fn classify_http_error(
        self,
        status: u16,
        body: &str,
        retry_after: Option<std::time::Duration>,
    ) -> AgentError {
        // xAI currently reports a bad API key as a 400 with a human message,
        // rather than the 401 / `invalid_api_key` shape the shared classifier
        // already recognizes. Inspect only for classification; the client-safe
        // message still comes from the same bounded/redacted formatter.
        if self == Self::Xai && status == 400 && xai_bad_api_key(body) {
            return AgentError::Authentication(safe_http_error("xai", status, body));
        }
        classify_provider_error(self.provider(), status, body, retry_after)
    }
}

fn xai_bad_api_key(body: &str) -> bool {
    let parsed = serde_json::from_str::<Value>(body).ok();
    let top_level_error = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let error = parsed
        .as_ref()
        .map(|value| value.get("error").unwrap_or(value));
    let code = error
        .and_then(|value| value.get("code").or_else(|| value.get("type")))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let message = error
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(top_level_error)
        .to_ascii_lowercase();
    matches!(code.as_str(), "invalid_api_key" | "authentication_error")
        || message.contains("incorrect api key")
        || message.contains("invalid api key")
}

/// A [`ModelProvider`] for OpenAI's native Responses API.
#[derive(Clone)]
pub struct OpenAiProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    provider_label: &'static str,
    /// Per-request credential supplier for ChatGPT OAuth (and similar)
    /// short-lived tokens. Takes precedence over `api_key` when present.
    token_source: Option<Arc<dyn BearerTokenSource>>,
    /// When set, inference is ChatGPT-subscription shaped: send the account
    /// id and Codex originator headers expected by the ChatGPT backend.
    chatgpt_account_id: Option<String>,
    /// `originator` header value when [`Self::chatgpt_account_id`] is set.
    chatgpt_originator: String,
    profile: ResponsesProfile,
    conversation_attribution: bool,
}

impl OpenAiProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: crate::http::streaming_client(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            provider_label: "openai",
            token_source: None,
            chatgpt_account_id: None,
            chatgpt_originator: DEFAULT_CHATGPT_ORIGINATOR.to_string(),
            profile: ResponsesProfile::OpenAi,
            conversation_attribution: false,
        }
    }

    pub(crate) fn for_profile(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        profile: ResponsesProfile,
    ) -> Self {
        Self {
            client: crate::http::streaming_client(),
            api_key: api_key.into(),
            base_url: base_url.into(),
            provider_label: profile.provider(),
            token_source: None,
            chatgpt_account_id: None,
            chatgpt_originator: DEFAULT_CHATGPT_ORIGINATOR.to_string(),
            profile,
            conversation_attribution: false,
        }
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    #[cfg(test)]
    pub(crate) fn base_url_for_test(&self) -> &str {
        &self.base_url
    }

    /// Fetch the credential from `source` at each request instead of using a
    /// static key. For ChatGPT OAuth whose access tokens rotate under the
    /// adapter.
    #[must_use]
    pub fn with_token_source(mut self, source: Arc<dyn BearerTokenSource>) -> Self {
        self.token_source = Some(source);
        self
    }

    /// Mark this provider as ChatGPT-subscription auth: attach the account id
    /// and Codex-compatible headers on every request.
    #[must_use]
    pub fn with_chatgpt_account_id(mut self, account_id: impl Into<String>) -> Self {
        self.chatgpt_account_id = Some(account_id.into());
        self
    }

    /// Override the `originator` header sent in ChatGPT mode.
    #[must_use]
    pub fn with_chatgpt_originator(mut self, originator: impl Into<String>) -> Self {
        self.chatgpt_originator = originator.into();
        self
    }

    /// Override the reported provider id and error-message label.
    #[must_use]
    pub fn with_provider_label(mut self, label: &'static str) -> Self {
        self.provider_label = label;
        self
    }

    /// Declare each request's conversation to the gateway this provider points
    /// at, so its usage views can group inference the way the app does.
    ///
    /// Only for a model-gateway base URL. OpenAI's own API is not a party to
    /// how the host organizes conversations, and the header would be sent to
    /// whatever `base_url` names.
    #[must_use]
    pub fn with_conversation_attribution(mut self) -> Self {
        self.conversation_attribution = true;
        self
    }
}

#[async_trait]
impl ModelProvider for OpenAiProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(self.provider_label)
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let mut body = build_request_json_for(&req, self.profile).map_err(|error| match error {
            AgentError::Provider(message) => {
                AgentError::Provider(format!("{} {message}", self.provider_label))
            }
            other => other,
        })?;
        if self.chatgpt_account_id.is_some() {
            // The ChatGPT Codex backend rejects `max_output_tokens` outright
            // ("Unsupported parameter"); only Platform API requests may carry it.
            if let Some(object) = body.as_object_mut() {
                object.remove("max_output_tokens");
            }
        }
        let wire_provider = self.profile.provider();
        let provider_label = self.provider_label;
        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));
        let url = reqwest::Url::parse(&url)
            .map_err(|_| AgentError::config("invalid Responses API endpoint"))?;
        let body = serde_json::to_vec(&body)?;
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
            .post(url.clone())
            .header("content-type", "application/json")
            .bearer_auth(api_key);
        if let Some(account_id) = &self.chatgpt_account_id {
            request = request
                .header("ChatGPT-Account-ID", account_id)
                .header("OpenAI-Beta", CHATGPT_BETA_HEADER)
                .header("originator", &self.chatgpt_originator);
        }
        // A conversation is declared only where one is configured to be read.
        // The id is a UUID, so it satisfies the gateway's bound on the value
        // (1-256 ASCII graphic bytes) by construction.
        if let (true, Some(conversation)) = (self.conversation_attribution, req.conversation) {
            request = request.header(
                crate::router::GATEWAY_CONVERSATION_HEADER,
                conversation.to_string(),
            );
        }
        let response = request
            .body(body)
            .send()
            .await
            .map_err(|_| AgentError::Provider(format!("{provider_label} request failed")))?;
        drop(authorization);

        let status = response.status();
        if !status.is_success() {
            let retry_after = crate::sse::retry_after_hint(response.headers());
            let body = read_bounded_error_body(response.bytes_stream()).await;
            return Err(if provider_label == wire_provider {
                self.profile
                    .classify_http_error(status.as_u16(), &body, retry_after)
            } else {
                classify_provider_error(provider_label, status.as_u16(), &body, retry_after)
            });
        }

        let ceiling = crate::http::timeouts().total_stream;
        let stream = async_stream::stream! {
            let bytes = crate::http::with_stream_deadline(response.bytes_stream(), ceiling);
            futures::pin_mut!(bytes);
            let mut framer = SseFramer::default();
            let mut state = StreamState {
                provider_label,
                ..StreamState::default()
            };
            while let Some(chunk) = bytes.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        yield ProviderEvent::Failed {
                            error: ProviderErrorInfo::provider(error.client_message(provider_label)),
                        };
                        return;
                    }
                };
                let frames = match framer.push(&chunk) {
                    Ok(frames) => frames,
                    Err(error) => {
                        yield ProviderEvent::Failed {
                            error: ProviderErrorInfo::provider(format!(
                                "{provider_label} {error}"
                            )),
                        };
                        return;
                    }
                };
                for frame in frames {
                    if let Some(data) = frame_data(&frame) {
                        for event in normalize_for(&data, &mut state, wire_provider) {
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
                            "{provider_label} {error}"
                        )),
                    };
                    return;
                }
            };
            if let Some(frame) = final_frame {
                if let Some(data) = frame_data(&frame) {
                    for event in normalize_for(&data, &mut state, wire_provider) {
                        yield event;
                    }
                }
            }
            if !state.terminal {
                yield ProviderEvent::Failed {
                    error: ProviderErrorInfo::provider(format!(
                        "{provider_label} stream ended before completion"
                    )),
                };
            }
        };
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
fn build_request_json(req: &ChatRequest) -> Result<Value> {
    build_request_json_for(req, ResponsesProfile::OpenAi)
}

pub(crate) fn build_request_json_for(
    req: &ChatRequest,
    profile: ResponsesProfile,
) -> Result<Value> {
    // Responses has no wire field for `VendorWebSearch::max_uses`, so a request
    // that hands the model a hosted search alongside its own tools spends an
    // unbounded number of searches inside one agent turn. That shape stays
    // refused.
    //
    // A request carrying no tools of its own is the other case: it is one
    // dedicated search the host issued and will not continue, so the host counts
    // the calls and the budget is enforced before egress rather than on the
    // wire. This is the same predicate the Gemini adapter already applies in
    // `declares_search_grounding`, for the same reason.
    if profile == ResponsesProfile::OpenAi
        && req.vendor_web_search.is_some()
        && !req.tools.is_empty()
    {
        return Err(AgentError::Provider(
            "Responses API cannot enforce the vendor web-search max_uses budget \
             alongside host tools"
                .into(),
        ));
    }

    let input = build_input_for(req, profile)?;
    let mut body = json!({
        "model": req.wire_model(),
        "input": input,
        "stream": true,
        "store": false,
        "max_output_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
    });

    if req.reasoning_model {
        if let Some(effort) = req.reasoning_effort {
            body["reasoning"] = if profile == ResponsesProfile::OpenAi {
                json!({ "effort": effort.as_str(), "summary": "auto" })
            } else {
                json!({ "effort": effort.as_str() })
            };
        }
        if profile == ResponsesProfile::Xai {
            // xAI is stateless here (`store: false`). Its encrypted reasoning
            // output is the only provider-native state that preserves full
            // context across later turns and tool continuations.
            body["include"] = json!(["reasoning.encrypted_content"]);
        }
    } else {
        if let Some(temperature) = req.temperature {
            body["temperature"] = json!(temperature);
        }
    }

    if !req.tools.is_empty() {
        body["tools"] = Value::Array(req.tools.iter().map(openai_tool).collect());
    }
    // xAI's vendor search is a hosted tool, declared alongside the function
    // tools rather than instead of them: the model may still call anything the
    // host advertised in the same response. An OpenAI request reaches here only
    // when it carries no tools of its own — the guard above refused the mixed
    // shape — so on that profile this is the sole tool on the request.
    if req.vendor_web_search.is_some() {
        let vendor_tool = json!({ "type": VENDOR_WEB_SEARCH_TOOL });
        match body["tools"].as_array_mut() {
            Some(tools) => tools.push(vendor_tool),
            None => body["tools"] = json!([vendor_tool]),
        }
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
                "Responses API cannot enforce response format {other:?}"
            )))
        }
    }
    Ok(body)
}

fn build_input_for(req: &ChatRequest, profile: ResponsesProfile) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    if let Some(system) = &req.system {
        out.push(json!({
            "role": "system",
            "content": [{ "type": "input_text", "text": system }]
        }));
    }
    // Declaring the hosted search puts a tool named `web_search` on the request
    // that the provider executes; historical calls to the host's client-side
    // tool of that name are replayed under another name so one request never
    // carries two different tools called the same thing.
    let rename_client_web_search = req.vendor_web_search.is_some();
    for message in &req.messages {
        extend_input(&mut out, message, req, rename_client_web_search, profile)?;
    }
    Ok(sanitize_tool_pairs(out))
}

#[cfg(test)]
fn build_input(req: &ChatRequest) -> Result<Vec<Value>> {
    build_input_for(req, ResponsesProfile::OpenAi)
}

fn extend_input(
    out: &mut Vec<Value>,
    message: &tidebreak_core::ChatMessage,
    req: &ChatRequest,
    rename_client_web_search: bool,
    profile: ResponsesProfile,
) -> Result<()> {
    if message.role == Role::Assistant {
        return extend_assistant_input(out, message, req, rename_client_web_search, profile);
    }
    let mut message_parts = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text } => {
                message_parts.push(json!({ "type": "input_text", "text": text }));
            }
            ContentBlock::Image { image } => {
                let data = req.images.get(image.blob_id).ok_or_else(|| {
                    AgentError::Provider(format!(
                        "image attachment {} has no hydrated bytes",
                        image.blob_id
                    ))
                })?;
                if profile == ResponsesProfile::Xai
                    && matches!(
                        data.media_type(),
                        tidebreak_core::ImageMediaType::Webp | tidebreak_core::ImageMediaType::Gif
                    )
                {
                    return Err(AgentError::Provider(format!(
                        "xai image input supports only PNG and JPEG, not {}",
                        data.media_type()
                    )));
                }
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
                    "name": prior_tool_name(name, rename_client_web_search),
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
            // A call another provider ran server-side has no Responses item to
            // replay it as, and the Responses API rejects a `function_call`
            // without a matching output from this conversation. One line of
            // prose keeps the fact of the search in context instead.
            ContentBlock::ProviderExecutedToolCall {
                name,
                input,
                output,
                is_error,
                replay: _,
            } => {
                message_parts.push(json!({
                    "type": "input_text",
                    "text": provider_executed_tool_call_text(name, input, output, *is_error),
                }));
            }
            other => {
                return Err(AgentError::Provider(format!(
                    "Responses API cannot express content block {other:?}"
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
    message: &tidebreak_core::ChatMessage,
    req: &ChatRequest,
    rename_client_web_search: bool,
    profile: ResponsesProfile,
) -> Result<()> {
    if profile == ResponsesProfile::Xai && req.reasoning_model {
        out.extend(
            message
                .reasoning
                .replayable_for(req.provider.as_ref(), &req.model)
                .iter()
                .cloned(),
        );
    }
    let mut texts = Vec::new();
    let mut calls = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text } => texts.push(text.clone()),
            ContentBlock::Image { .. } => {
                return Err(AgentError::Provider(
                    "Responses API cannot express an image in assistant history".into(),
                ))
            }
            ContentBlock::ToolUse { id, name, input } => {
                calls.push(json!({
                    "type": "function_call",
                    "call_id": id,
                    "name": prior_tool_name(name, rename_client_web_search),
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
            // No Responses item replays another provider's server-side call,
            // so it joins the assistant prose as one compact line.
            ContentBlock::ProviderExecutedToolCall {
                name,
                input,
                output,
                is_error,
                replay: _,
            } => texts.push(provider_executed_tool_call_text(
                name, input, output, *is_error,
            )),
            other => {
                return Err(AgentError::Provider(format!(
                    "Responses API cannot express content block {other:?}"
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

/// The Responses API's own name for its hosted web search tool, and the name
/// each completed search surfaces under to the host.
const VENDOR_WEB_SEARCH_TOOL: &str = "web_search";

/// The name a historical client-side `web_search` call is replayed under once
/// the request declares the hosted tool of that name.
///
/// The request would otherwise carry two different tools called `web_search` —
/// one the client executes, one the provider does — and a replayed
/// `function_call` naming the hosted tool is undefined. Only the name changes;
/// items still pair by `call_id`, so the matching output needs no change and
/// `sanitize_tool_pairs` still sees a complete pair.
const PRIOR_WEB_SEARCH_TOOL: &str = "web_search_prior";

fn prior_tool_name(name: &str, renaming: bool) -> &str {
    if renaming && name == VENDOR_WEB_SEARCH_TOOL {
        PRIOR_WEB_SEARCH_TOOL
    } else {
        name
    }
}

fn openai_tool(tool: &tidebreak_core::tool::ToolSpec) -> Value {
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
                "Responses API cannot express tool choice {other:?}"
            )))
        }
    })
}

#[derive(Default)]
struct StreamState {
    provider_label: &'static str,
    calls: BTreeMap<String, CallState>,
    next_index: u32,
    terminal: bool,
    refused: bool,
    /// The most recently completed hosted search, still collecting the
    /// citations that describe what it found. See [`PendingSearch`].
    pending_search: Option<PendingSearch>,
}

/// A hosted web search that has finished running, held back until the results
/// it produced can be read off the response.
///
/// The Responses API returns no result list with the search itself: the search
/// call item carries only the query, and what it found shows up afterwards as
/// `url_citation` annotations on the message the model writes from it. So the
/// finished search waits here, gathering annotations, and is emitted when the
/// next search completes or the response ends — whichever comes first.
///
/// Annotations are attributed to the most recent completed search. Where a
/// response runs several searches back to back before writing anything, that
/// puts every citation on the last of them; the alternative is correlating
/// citations to searches by content the API does not relate, and a search
/// reported with no results is still a search the host can show.
struct PendingSearch {
    input: Value,
    results: Vec<Value>,
    /// Canonical URL of every result already taken, so a source cited in
    /// several places or with several titles is reported once.
    seen: HashSet<String>,
}

#[derive(Default)]
struct CallState {
    index: u32,
    name: String,
    started: bool,
    saw_argument_delta: bool,
}

#[cfg(test)]
fn normalize(data: &Value, state: &mut StreamState) -> Vec<ProviderEvent> {
    normalize_for(data, state, "openai")
}

fn normalize_for(
    data: &Value,
    state: &mut StreamState,
    provider: &'static str,
) -> Vec<ProviderEvent> {
    if state.terminal {
        return Vec::new();
    }
    let event_type = data["type"].as_str().unwrap_or_default();
    match event_type {
        "error" => {
            state.terminal = true;
            let mut events = flush_search(state, provider);
            events.push(ProviderEvent::Failed {
                error: ProviderErrorInfo::from_error(&classify_in_band_error(
                    stream_provider_label(state),
                    data.get("error").unwrap_or(data),
                )),
            });
            events
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
        // Every citation on the model's output text describes a page one of
        // its searches reached, and is the only account of what the search
        // returned.
        "response.output_text.annotation.added" => {
            collect_citation(&data["annotation"], state);
            Vec::new()
        }
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
        "response.output_item.done" => {
            if data["item"]["type"] == "web_search_call" {
                return finish_search(&data["item"], state, provider);
            }
            if provider == "xai"
                && data["item"]["type"] == "reasoning"
                && data["item"]["encrypted_content"]
                    .as_str()
                    .is_some_and(|content| !content.is_empty())
            {
                return vec![ProviderEvent::ReasoningBlock {
                    data: data["item"].clone(),
                }];
            }
            start_call(data, state, true)
        }
        "response.completed" => {
            state.terminal = true;
            let response = &data["response"];
            let mut events = flush_search(state, provider);
            events.extend(usage_event(response));
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
            let mut events = flush_search(state, provider);
            events.extend(usage_event(response));
            match response["incomplete_details"]["reason"].as_str() {
                Some("max_output_tokens") => events.push(ProviderEvent::Stop {
                    reason: StopReason::MaxTokens,
                }),
                Some("content_filter") => events.push(ProviderEvent::Refusal {
                    details: RefusalDetails::from_category(Some("content_filter")),
                }),
                _ => events.push(ProviderEvent::Failed {
                    error: ProviderErrorInfo::provider(format!(
                        "{} response ended incomplete without a supported reason",
                        stream_provider_label(state)
                    )),
                }),
            }
            events
        }
        "response.failed" => {
            state.terminal = true;
            let error = data["response"].get("error").unwrap_or(&data["response"]);
            let mut events = flush_search(state, provider);
            events.push(ProviderEvent::Failed {
                error: ProviderErrorInfo::from_error(&classify_in_band_error(
                    stream_provider_label(state),
                    error,
                )),
            });
            events
        }
        _ => Vec::new(),
    }
}

fn stream_provider_label(state: &StreamState) -> &'static str {
    if state.provider_label.is_empty() {
        "openai"
    } else {
        state.provider_label
    }
}

/// Handle a finished `web_search_call` output item.
///
/// A completed search becomes the pending one, which first flushes whatever
/// search was pending before it. A search that ended any other way produced
/// nothing to cite, so it goes out immediately as a failure — dropping it
/// would hide from the host that the model tried to search at all.
fn finish_search(
    item: &Value,
    state: &mut StreamState,
    provider: &'static str,
) -> Vec<ProviderEvent> {
    let input = search_input(item.get("action"));
    let status = item["status"].as_str().unwrap_or_default();
    let mut events = flush_search(state, provider);
    if status == "completed" {
        state.pending_search = Some(PendingSearch {
            input,
            results: Vec::new(),
            seen: HashSet::new(),
        });
    } else {
        events.push(ProviderEvent::ProviderExecutedToolCall {
            name: VENDOR_WEB_SEARCH_TOOL.to_owned(),
            input,
            output: json!({
                "error_code": if status.is_empty() { "failed" } else { status },
            }),
            is_error: true,
            replay: None,
        });
    }
    events
}

/// The arguments to report a hosted search ran with.
///
/// A plain `search` action carries the query, which is what a reader wants and
/// what the host's own `web_search` tool takes. Reasoning models also browse
/// with `open_page` and `find_in_page` actions that have no query; there the
/// action itself is the only description of what ran, so it goes through whole
/// rather than being flattened into a sentence this adapter invented.
fn search_input(action: Option<&Value>) -> Value {
    match action {
        Some(action) => match action.get("query").and_then(Value::as_str) {
            Some(query) => json!({ "query": query }),
            None => json!({ "query": action }),
        },
        None => json!({}),
    }
}

/// Take a `url_citation` annotation as a result of the pending search.
fn collect_citation(annotation: &Value, state: &mut StreamState) {
    /// The cap the host's own search applies, so a hosted search cannot put
    /// more into context than the tool it stands in for.
    const MAX_RESULTS: usize = tidebreak_core::MAX_WEB_SEARCH_RESULTS;
    /// Kept equal to the canonical provider-search receipt title bound. The
    /// adapter must not emit a row the core admission gate will discard.
    const MAX_TITLE_CHARS: usize = 300;

    if annotation["type"] != "url_citation" {
        return;
    }
    let Some(search) = state.pending_search.as_mut() else {
        return;
    };
    if search.results.len() >= MAX_RESULTS {
        return;
    }
    let Some(url) = annotation["url"].as_str().and_then(canonical_citation_url) else {
        return;
    };
    // OpenAI supplies no page excerpt with a citation, so an absent, blank,
    // or otherwise unsafe title would produce a row with neither a title nor
    // a snippet. Omit that row while retaining usable sibling citations.
    let Some(title) = annotation["title"].as_str().map(str::trim).filter(|title| {
        !title.is_empty()
            && title.chars().count() <= MAX_TITLE_CHARS
            && !title.chars().any(char::is_control)
    }) else {
        return;
    };
    if !search.seen.insert(url.clone()) {
        return;
    }
    search.results.push(json!({
        "url": url,
        "title": title,
        // The API cites a location in the model's own text rather than an
        // excerpt of the page; the field stays present so the result shape does
        // not vary by provider.
        "snippet": "",
    }));
}

/// Normalize a provider citation URL into the exact network-URL shape the
/// canonical provider-search receipt accepts.
fn canonical_citation_url(raw: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(raw).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.fragment().is_some() {
        return None;
    }

    let host = parsed.host_str()?;
    if host.is_empty() || !host.is_ascii() {
        return None;
    }
    if host.contains(':') {
        host.parse::<std::net::Ipv6Addr>().ok()?;
    } else if host.parse::<std::net::Ipv4Addr>().is_err()
        && !host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
    {
        return None;
    }

    let canonical = parsed.to_string();
    (canonical.len() <= tidebreak_core::MAX_WEB_EXTRACT_URL_BYTES
        && !canonical.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(
                    character,
                    '\u{200b}'..='\u{200f}'
                        | '\u{202a}'..='\u{202e}'
                        | '\u{2066}'..='\u{2069}'
                        | '\u{feff}'
                )
        }))
    .then_some(canonical)
}

/// Emit the pending search, if there is one, as a finished provider-executed
/// call.
fn flush_search(state: &mut StreamState, provider: &'static str) -> Vec<ProviderEvent> {
    let Some(search) = state.pending_search.take() else {
        return Vec::new();
    };
    vec![ProviderEvent::ProviderExecutedToolCall {
        name: VENDOR_WEB_SEARCH_TOOL.to_owned(),
        input: search.input,
        // A search that cited nothing still ran, and is reported as a search
        // that found nothing rather than as a failure.
        output: json!({ "provider": provider, "results": search.results }),
        is_error: false,
        replay: None,
    }]
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
    use tidebreak_core::provider::{ChatMessage, MessageReasoning, VendorWebSearch};
    use tidebreak_core::tool::ToolSpec;
    use tidebreak_core::{ImageAttachments, ImageData, ImageMediaType, ImageRef, ReasoningEffort};

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
        let paired = tidebreak_core::ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_1".into(),
                name: "exec".into(),
                input: json!({"command":"pwd"}),
            }],
            reasoning: MessageReasoning::default(),
        };
        let result = tidebreak_core::ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".into(),
                content: "ok".into(),
                is_error: false,
            }],
            reasoning: MessageReasoning::default(),
        };
        let orphan = tidebreak_core::ChatMessage {
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
            tidebreak_core::ChatMessage {
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
            tidebreak_core::ChatMessage {
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
    fn openai_refuses_unbounded_vendor_search_without_changing_xai() {
        let history = vec![
            tidebreak_core::ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "web_search".into(),
                    input: json!({"query":"rust"}),
                }],
                reasoning: MessageReasoning::default(),
            },
            tidebreak_core::ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: "one result".into(),
                    is_error: false,
                }],
                reasoning: MessageReasoning::default(),
            },
        ];
        let req = ChatRequest {
            model: "gpt-5.6-sol".into(),
            messages: history,
            tools: vec![tool()],
            vendor_web_search: Some(VendorWebSearch {
                max_uses: VendorWebSearch::DEFAULT_MAX_USES,
            }),
            ..Default::default()
        };
        let error = build_request_json(&req)
            .expect_err("OpenAI must not receive a hosted search without a wire budget");
        assert!(error.to_string().contains("max_uses"), "{error}");

        let body = build_request_json_for(&req, ResponsesProfile::Xai)
            .expect("the OpenAI capability guard must not change xAI requests");
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[1], json!({"type":"web_search"}));

        let call = body["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "function_call")
            .unwrap();
        assert_eq!(call["name"], "web_search_prior");
        // The rename must not break the pairing that keeps the call in history.
        assert_eq!(call["call_id"], "call_1");
        assert!(body["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "function_call_output" && item["call_id"] == "call_1"));

        let without = ChatRequest {
            vendor_web_search: None,
            ..req
        };
        let body = build_request_json(&without).unwrap();
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
        assert!(body["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["name"] == "web_search"));
    }

    /// The dedicated search sub-request the host issues on the model's behalf.
    ///
    /// It is the mixed shape that Responses cannot bound, not the hosted tool
    /// itself: with no other tool on the request there is nothing for the model
    /// to continue into, so the host's own call count is the budget. Refusing
    /// this shape too would leave OpenAI models unable to search at all.
    #[test]
    fn openai_accepts_a_hosted_search_on_a_request_carrying_no_other_tools() {
        let req = ChatRequest {
            model: "gpt-5.6-sol".into(),
            messages: vec![ChatMessage::text(Role::User, "who won the match")],
            tools: Vec::new(),
            vendor_web_search: Some(VendorWebSearch { max_uses: 1 }),
            ..Default::default()
        };

        let body = build_request_json(&req)
            .expect("a tool-free search sub-request is the shape the host can bound");
        assert_eq!(body["tools"], json!([{"type":"web_search"}]));

        // Nothing to collide with: the rename only exists to keep one request
        // from carrying two different tools named `web_search`.
        assert!(!body["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["name"] == "web_search_prior"));
    }

    #[test]
    fn a_hosted_search_reports_the_sources_its_answer_cited() {
        let mut state = StreamState::default();
        let events: Vec<_> = [
            json!({
                "type":"response.output_item.done",
                "item":{
                    "type":"web_search_call",
                    "id":"ws_1",
                    "status":"completed",
                    "action":{"type":"search","query":"rust 2027"}
                }
            }),
            json!({"type":"response.output_text.delta","delta":"Rust 2027 ships"}),
            json!({
                "type":"response.output_text.annotation.added",
                "annotation":{
                    "type":"url_citation",
                    "url":"https://example.com/a",
                    "title":"A",
                    "start_index":0,
                    "end_index":5
                }
            }),
            // The same source cited twice is one result.
            json!({
                "type":"response.output_text.annotation.added",
                "annotation":{
                    "type":"url_citation",
                    "url":"https://EXAMPLE.com:443/a",
                    "title":"A second title"
                }
            }),
            json!({
                "type":"response.output_text.annotation.added",
                "annotation":{"type":"url_citation","url":"https://example.com/b","title":"B"}
            }),
            json!({"type":"response.completed","response":{}}),
        ]
        .iter()
        .flat_map(|event| normalize(event, &mut state))
        .collect();
        assert_eq!(
            events,
            vec![
                ProviderEvent::TextDelta {
                    text: "Rust 2027 ships".into()
                },
                ProviderEvent::ProviderExecutedToolCall {
                    name: "web_search".into(),
                    input: json!({"query":"rust 2027"}),
                    output: json!({
                        "provider": "openai",
                        "results": [
                            {"url":"https://example.com/a","title":"A","snippet":""},
                            {"url":"https://example.com/b","title":"B","snippet":""},
                        ]
                    }),
                    is_error: false,
                    replay: None,
                },
                // A hosted search is not a call anyone answers, so the turn
                // still ends rather than asking for tool results.
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn
                },
            ]
        );
    }

    #[test]
    fn a_hosted_search_omits_untitled_citations_without_dropping_valid_siblings() {
        let mut state = StreamState::default();
        let events: Vec<_> = [
            json!({
                "type":"response.output_item.done",
                "item":{
                    "type":"web_search_call",
                    "id":"ws_1",
                    "status":"completed",
                    "action":{"type":"search","query":"rust 2027"}
                }
            }),
            json!({
                "type":"response.output_text.annotation.added",
                "annotation":{
                    "type":"url_citation",
                    "url":"https://example.com/untitled"
                }
            }),
            json!({
                "type":"response.output_text.annotation.added",
                "annotation":{
                    "type":"url_citation",
                    "url":"https://example.com/titled",
                    "title":"Release notes"
                }
            }),
            json!({"type":"response.completed","response":{}}),
        ]
        .iter()
        .flat_map(|event| normalize(event, &mut state))
        .collect();

        assert_eq!(
            events,
            vec![
                ProviderEvent::ProviderExecutedToolCall {
                    name: "web_search".into(),
                    input: json!({"query":"rust 2027"}),
                    output: json!({
                        "provider": "openai",
                        "results": [{
                            "url":"https://example.com/titled",
                            "title":"Release notes",
                            "snippet":""
                        }]
                    }),
                    is_error: false,
                    replay: None,
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn
                },
            ]
        );
    }

    #[test]
    fn a_hosted_search_with_only_untitled_citations_reports_empty_results() {
        let mut state = StreamState::default();
        let events: Vec<_> = [
            json!({
                "type":"response.output_item.done",
                "item":{
                    "type":"web_search_call",
                    "id":"ws_1",
                    "status":"completed",
                    "action":{"type":"search","query":"rust 2027"}
                }
            }),
            json!({
                "type":"response.output_text.annotation.added",
                "annotation":{
                    "type":"url_citation",
                    "url":"https://example.com/untitled",
                    "title":"   "
                }
            }),
            json!({"type":"response.completed","response":{}}),
        ]
        .iter()
        .flat_map(|event| normalize(event, &mut state))
        .collect();

        assert_eq!(
            events,
            vec![
                ProviderEvent::ProviderExecutedToolCall {
                    name: "web_search".into(),
                    input: json!({"query":"rust 2027"}),
                    output: json!({"provider":"openai","results":[]}),
                    is_error: false,
                    replay: None,
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn
                },
            ]
        );
    }

    #[test]
    fn a_failed_hosted_search_is_reported_rather_than_dropped() {
        let mut state = StreamState::default();
        let events = normalize(
            &json!({
                "type":"response.output_item.done",
                "item":{
                    "type":"web_search_call",
                    "id":"ws_1",
                    "status":"failed",
                    "action":{"type":"search","query":"rust 2027"}
                }
            }),
            &mut state,
        );
        assert_eq!(
            events,
            vec![ProviderEvent::ProviderExecutedToolCall {
                name: "web_search".into(),
                input: json!({"query":"rust 2027"}),
                output: json!({"error_code":"failed"}),
                is_error: true,
                replay: None,
            }]
        );
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
            messages: vec![tidebreak_core::ChatMessage {
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

    #[tokio::test]
    async fn native_provider_surfaces_bounded_openai_detail_error() {
        use axum::http::StatusCode;
        use axum::routing::post;
        use axum::Router;

        async fn reject() -> (StatusCode, &'static str) {
            (
                StatusCode::BAD_REQUEST,
                include_str!("../tests/fixtures/openai/detail_error.response.json"),
            )
        }

        let app = Router::new().route("/v1/responses", post(reject));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let provider = OpenAiProvider::new("key").with_base_url(format!("http://{address}/v1"));
        let result = provider
            .stream(ChatRequest {
                model: "gpt-5.6-sol".into(),
                messages: vec![ChatMessage::text(Role::User, "hey hows it going")],
                ..Default::default()
            })
            .await;
        let Err(error) = result else {
            panic!("expected the provider request to fail");
        };

        assert!(matches!(error, AgentError::InvalidRequest(_)));
        assert_eq!(
            error.to_string(),
            "invalid provider request: openai returned 400: The requested model is not available for this account."
        );
    }

    #[tokio::test]
    async fn chatgpt_oauth_sends_codex_headers_without_max_output_tokens() {
        use std::sync::Arc;

        use axum::extract::{Request, State};
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::routing::post;
        use axum::Router;
        use tokio::sync::oneshot;

        use crate::BearerTokenSource;

        struct StaticSource;

        #[async_trait]
        impl BearerTokenSource for StaticSource {
            async fn bearer_token(&self) -> Result<String> {
                Ok("chatgpt-access".into())
            }
        }

        #[derive(Default)]
        struct Captured {
            authorization: Option<String>,
            account: Option<String>,
            beta: Option<String>,
            originator: Option<String>,
            body: Value,
        }

        async fn capture(
            State(tx): State<Arc<std::sync::Mutex<Option<oneshot::Sender<Captured>>>>>,
            request: Request,
        ) -> impl IntoResponse {
            let headers = request.headers();
            let mut captured = Captured {
                authorization: headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned),
                account: headers
                    .get("chatgpt-account-id")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned),
                beta: headers
                    .get("openai-beta")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned),
                originator: headers
                    .get("originator")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned),
                body: Value::Null,
            };
            let bytes = axum::body::to_bytes(request.into_body(), 64 * 1024)
                .await
                .unwrap();
            captured.body = serde_json::from_slice(&bytes).unwrap();
            if let Some(tx) = tx.lock().unwrap().take() {
                let _ = tx.send(captured);
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
        let state = Arc::new(std::sync::Mutex::new(Some(tx)));
        let app = Router::new().fallback(post(capture)).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let provider = OpenAiProvider::new(String::new())
            .with_base_url(format!("http://{address}/codex"))
            .with_token_source(Arc::new(StaticSource))
            .with_chatgpt_account_id("acct-xyz");
        let stream = provider
            .stream(ChatRequest {
                model: "gpt-5.6-sol".into(),
                messages: vec![ChatMessage::text(Role::User, "hi")],
                ..Default::default()
            })
            .await
            .unwrap();
        let _events: Vec<_> = stream.collect().await;
        let captured = rx.await.unwrap();
        assert_eq!(
            captured.authorization.as_deref(),
            Some("Bearer chatgpt-access")
        );
        assert_eq!(captured.account.as_deref(), Some("acct-xyz"));
        assert_eq!(captured.beta.as_deref(), Some("responses=v1"));
        assert_eq!(captured.originator.as_deref(), Some("tidebreak"));
        // The ChatGPT Codex backend rejects `max_output_tokens` with a 400.
        assert!(captured.body.get("max_output_tokens").is_none());
    }
}
