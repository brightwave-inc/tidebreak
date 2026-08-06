//! The model-provider contract.
//!
//! A [`ModelProvider`] normalizes one LLM API's streaming quirks into a single
//! [`ProviderEvent`] stream. Everything above the adapter — the agent loop, the
//! clients — speaks only this vocabulary, never a provider's native wire format.
//!
//! Messages use a normalized content-block model (text / tool-use / tool-result)
//! so tool-calling round-trips the same way across providers.

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::image::{ImageAttachments, ImageRef};
use crate::model::Role;

/// Stable identifier for a provider adapter (e.g. `anthropic`, `openai-compat`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderId(pub String);

impl ProviderId {
    /// Wrap a provider name.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl std::fmt::Display for ProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The exact model route that produced a set of reasoning blocks.
///
/// A reasoning block is minted by one model on one provider and is only valid
/// as input back to that same route. `provider` mirrors [`ChatRequest::provider`]
/// — `None` means the host let the router pick, in which case the model name
/// determines the route and comparing models alone is enough.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningOrigin {
    /// The explicit provider route, if the host pinned one.
    pub provider: Option<ProviderId>,
    /// The model that generated the blocks.
    pub model: String,
}

/// A message's opaque reasoning blocks, bound to the route that minted them.
///
/// The blocks are provider values kept in emission order and kept whole — a
/// message replays either all of them or none, because a provider that
/// validates replay (Anthropic) rejects rearranged, edited, or partially
/// dropped reasoning. They stay a `Vec`: a provider may emit several in one
/// step, and flattening them into one block would lose both the count and the
/// per-block signatures.
///
/// The origin travels with the blocks so a consumer can tell whether they are
/// valid input for the request it is about to send. Blocks and origin are set
/// together by [`MessageReasoning::captured`] and are private so they cannot
/// drift apart.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageReasoning {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<ReasoningOrigin>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    blocks: Vec<Value>,
}

impl MessageReasoning {
    /// Blocks captured from a live stream on `origin`'s route.
    ///
    /// An empty block list carries no origin: there is nothing to attribute.
    pub fn captured(origin: ReasoningOrigin, blocks: Vec<Value>) -> Self {
        if blocks.is_empty() {
            return Self::default();
        }
        Self {
            origin: Some(origin),
            blocks,
        }
    }

    /// Whether there is anything to replay.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// The blocks, whatever route they came from.
    pub fn blocks(&self) -> &[Value] {
        &self.blocks
    }

    /// The route that minted the blocks, if any were captured.
    pub fn origin(&self) -> Option<&ReasoningOrigin> {
        self.origin.as_ref()
    }

    /// The blocks, but only when they are valid input for this exact route.
    ///
    /// A block minted by another provider — or by another model on the same
    /// provider — is foreign input, so a mismatch yields nothing rather than
    /// failing: switching models mid-conversation is ordinary, and sending no
    /// reasoning is always a valid request shape. This is the flatten-on-switch
    /// rule for thinking blocks; see `docs/model-providers.md`.
    pub fn replayable_for(&self, provider: Option<&ProviderId>, model: &str) -> &[Value] {
        match &self.origin {
            Some(origin) if origin.provider.as_ref() == provider && origin.model == model => {
                &self.blocks
            }
            _ => &[],
        }
    }
}

/// Provider-native content blocks for a tool the provider already ran.
///
/// Anthropic's server-side web search is the motivating case: result bodies
/// live only in opaque `encrypted_content` that must be replayed verbatim to
/// the same route. The cleartext [`ContentBlock::ProviderExecutedToolCall`]
/// output stays the host/UI shape; this side channel carries what that
/// provider alone needs on a later request.
///
/// Origin-gated exactly like [`MessageReasoning`]: a foreign provider or
/// model gets nothing and adapters fall back to cleartext `output` (the
/// flatten-on-switch rule in `docs/model-providers.md`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderToolReplay {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<ReasoningOrigin>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    blocks: Vec<Value>,
}

impl ProviderToolReplay {
    /// Native blocks captured from a live stream on `origin`'s route.
    ///
    /// An empty block list still records the origin when the call was
    /// provider-executed but has nothing opaque to replay (OpenAI citations
    /// are already in the cleartext output). Callers that have neither an
    /// origin nor blocks should leave the field absent instead.
    pub fn captured(origin: ReasoningOrigin, blocks: Vec<Value>) -> Self {
        Self {
            origin: Some(origin),
            blocks,
        }
    }

    /// Whether there is anything to attribute or replay.
    pub fn is_empty(&self) -> bool {
        self.origin.is_none() && self.blocks.is_empty()
    }

    /// The route that minted the blocks, if any were captured.
    pub fn origin(&self) -> Option<&ReasoningOrigin> {
        self.origin.as_ref()
    }

    /// The blocks, whatever route they came from.
    pub fn blocks(&self) -> &[Value] {
        &self.blocks
    }

    /// The blocks, but only when they are valid input for this exact route.
    pub fn replayable_for(&self, provider: Option<&ProviderId>, model: &str) -> &[Value] {
        match &self.origin {
            Some(origin) if origin.provider.as_ref() == provider && origin.model == model => {
                &self.blocks
            }
            _ => &[],
        }
    }
}

/// One piece of a message. Assistant messages carry text (and, when the model
/// calls tools, `ToolUse` blocks); tool results come back as `ToolResult`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContentBlock {
    /// Plain text.
    Text {
        /// The text body.
        text: String,
    },
    /// A tool invocation emitted by the model.
    ToolUse {
        /// Provider-assigned id correlating this call to its result.
        id: String,
        /// The tool name.
        name: String,
        /// The arguments (JSON) the model produced.
        input: Value,
    },
    /// An image attached to the message.
    ///
    /// Carries identity only — the pixels travel out of band on
    /// [`ChatRequest::images`], so no path that serializes, stores, or
    /// debug-prints a content block can leak them.
    Image {
        /// Blob identity, media type, and bounded dimensions.
        image: ImageRef,
    },
    /// The result of a tool call, fed back to the model.
    ToolResult {
        /// The `ToolUse::id` this result answers.
        tool_use_id: String,
        /// Result text.
        content: String,
        /// Whether the tool failed.
        #[serde(default)]
        is_error: bool,
    },
    /// A tool call the provider executed on its own infrastructure during the
    /// turn.
    ///
    /// Both the call and its result are already final — there is no client
    /// execution loop and nothing to answer. Hosts persist and display it like
    /// an ordinary tool call, but must never dispatch it to the tool registry:
    /// the work is done, and re-running it would repeat an effect the provider
    /// already had.
    ProviderExecutedToolCall {
        /// Tool name as shown to users and the journal, e.g. `web_search`.
        name: String,
        /// The arguments the provider ran the tool with.
        input: Value,
        /// The result, normalized to the shape the host's own tool of that
        /// name produces, so one renderer draws both.
        output: Value,
        /// Whether the provider's tool failed.
        #[serde(default)]
        is_error: bool,
        /// Provider-native blocks for same-route replay, when the adapter
        /// captured any. Absent for host-shaped history and for providers
        /// whose cleartext `output` is already enough.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replay: Option<ProviderToolReplay>,
    },
}

/// Render a provider-executed tool call as assistant prose a foreign adapter
/// can carry.
///
/// Used when the wire format has no place for a call this (or another)
/// provider ran server-side, or when native replay is not valid for the
/// current route. Includes titles and URLs from the cleartext result so a
/// later step is not left with only a count.
#[must_use]
pub fn provider_executed_tool_call_text(
    name: &str,
    input: &Value,
    output: &Value,
    is_error: bool,
) -> String {
    /// Cap on the rendered subject, so an unbounded input cannot grow the
    /// prompt one line at a time.
    const MAX_SUBJECT_CHARS: usize = 200;
    /// How many result rows ride in the prose; the rest are tallied.
    const MAX_LISTED_RESULTS: usize = 8;
    /// Cap on each listed title.
    const MAX_TITLE_CHARS: usize = 120;

    let subject = match input.get("query").and_then(Value::as_str) {
        Some(query) => query.to_owned(),
        None => input.to_string(),
    };
    let subject: String = subject.chars().take(MAX_SUBJECT_CHARS).collect();
    if is_error {
        let outcome = match output.get("error_code").and_then(Value::as_str) {
            Some(code) => format!("failed ({code})"),
            None => "failed".to_owned(),
        };
        return format!("[{name}: {subject} -> {outcome}]");
    }
    let Some(results) = output.get("results").and_then(Value::as_array) else {
        return format!("[{name}: {subject} -> done]");
    };
    if results.is_empty() {
        return format!("[{name}: {subject} -> 0 results]");
    }
    let mut lines = Vec::with_capacity(1 + MAX_LISTED_RESULTS.min(results.len()));
    lines.push(format!("[{name}: {subject} -> {} results]", results.len()));
    for result in results.iter().take(MAX_LISTED_RESULTS) {
        let title = result
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(MAX_TITLE_CHARS)
            .collect::<String>();
        let url = result
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if title.is_empty() && url.is_empty() {
            continue;
        }
        if title.is_empty() {
            lines.push(format!("- {url}"));
        } else if url.is_empty() {
            lines.push(format!("- {title}"));
        } else {
            lines.push(format!("- {title} — {url}"));
        }
    }
    let omitted = results.len().saturating_sub(MAX_LISTED_RESULTS);
    if omitted > 0 {
        lines.push(format!("- …and {omitted} more"));
    }
    lines.join("\n")
}

/// A single message in the conversation sent to a provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Who authored the message.
    pub role: Role,
    /// The ordered content blocks.
    pub content: Vec<ContentBlock>,
    /// Reasoning blocks the model produced in the step this message came
    /// from, for verbatim replay on a later request to the same route.
    ///
    /// A side channel, not content: serde never reads or writes it here, so no
    /// wire request changes shape. Replay is gated on the origin recorded with
    /// the blocks — see [`MessageReasoning::replayable_for`] — because a block
    /// from another provider or another model is not valid input.
    #[serde(default, skip)]
    pub reasoning: MessageReasoning,
}

impl ChatMessage {
    /// A single-text-block message.
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ContentBlock::Text { text: text.into() }],
            reasoning: MessageReasoning::default(),
        }
    }
}

/// A constraint on the shape of a completion's output.
///
/// This is a constraint, not a hint. An adapter either enforces it with the
/// provider's native mechanism or fails the request; none of them sends an
/// unconstrained completion and hopes. A caller that asked for JSON therefore
/// never has to guess whether it got JSON — only whether the turn ran.
///
/// **Return channel.** Constrained output always arrives on
/// [`ProviderEvent::TextDelta`], whatever mechanism the adapter used to obtain
/// it. Providers disagree here — OpenAI and Gemini have a native JSON mode that
/// streams as ordinary text, while Anthropic's form is a forced tool call whose
/// arguments stream as [`ProviderEvent::ToolCallArgsDelta`] — and normalizing in
/// the adapter is what keeps that disagreement out of every consumer. Both
/// consumers of this event stream match exhaustively and reject a tool call
/// they did not advertise, so the alternative would be a provider-conditional
/// branch in each of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ResponseFormat {
    /// The completion must be a single JSON value satisfying `schema`.
    JsonSchema {
        /// Short identifier for the schema, e.g. `context_checkpoint`.
        ///
        /// Providers surface it in errors, and the Anthropic adapter uses it as
        /// the name of the tool it forces, so it must match
        /// `^[a-zA-Z0-9_-]{1,64}$`.
        name: String,
        /// The JSON Schema (draft 2020-12) the output must satisfy.
        ///
        /// Providers accept only a strict subset — see
        /// [`crate::tool::strict_json_schema`], which produces it.
        schema: Value,
    },
}

/// Whether the model may, must, or must not call a tool this turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolChoice {
    /// The model decides. This is every provider's default.
    Auto,
    /// The model must call one of the advertised tools.
    Required,
    /// The model must not call a tool.
    None,
    /// The model must call exactly this tool.
    Tool {
        /// The name of the tool, which must be one the request advertises.
        name: String,
    },
}

/// Ask the routing adapter to enable the provider's own server-side web search
/// tool for this request.
///
/// The search runs on the provider's infrastructure: OpenWave advertises no
/// tool for it, never sees the fetch, and makes no egress of its own, so none
/// of the host's network policy applies to it. Present means the host decided
/// the turn may search; absent means it may not.
///
/// Adapters that cannot express a vendor search — a route whose endpoint
/// OpenWave does not control, or a provider with no such tool — must ignore
/// this and send the request unchanged. Failing the turn over a control the
/// host offered as an enhancement would trade a working answer for an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorWebSearch {
    /// Upper bound on searches the provider may run in one turn.
    pub max_uses: u32,
}

impl VendorWebSearch {
    /// Search budget for a turn when the host has no reason to pick another.
    ///
    /// Enough for a question that needs following up on its first results,
    /// while still bounding what one turn can spend.
    pub const DEFAULT_MAX_USES: u32 = 5;
}

/// A request for one streamed model completion.
///
/// The struct is constructed as a literal rather than through a builder, so it
/// derives [`Default`] and callers spread `..Default::default()` over the
/// controls they do not set. New request controls then reach the adapters
/// without a mechanical edit at every construction site — and, more usefully,
/// without the temptation to make the struct `#[non_exhaustive]`, which would
/// hide from the compiler the one place that genuinely has to opt in.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    /// Explicit provider route selected by the host. Composite routers must
    /// honor this hint exactly and must not silently fall back to another
    /// provider. Direct adapters may ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderId>,
    /// The conversation this request belongs to, for gateway-side attribution.
    ///
    /// Only a gateway route declares it, and only as a header: no provider
    /// receives it as wire data, and a direct provider never sees it at all.
    /// Absent, the gateway records the inference without a conversation, which
    /// is what every request did before this existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<crate::id::ChatId>,
    /// Provider-specific model identifier (e.g. `claude-opus-4-8`).
    pub model: String,
    /// Whether the resolved model uses a reasoning-model request shape.
    ///
    /// This is host-owned registry policy, rather than an adapter guess based
    /// on the model name.
    #[serde(default)]
    pub reasoning_model: bool,
    /// System prompt, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Conversation history.
    pub messages: Vec<ChatMessage>,
    /// Tools advertised to the model this turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<crate::tool::ToolSpec>,
    /// Upper bound on tokens to generate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Reasoning-effort hint. Adapters shape it for models that expose the
    /// control and ignore it otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<crate::model::ReasoningEffort>,
    /// Constraint on the output's shape. Absent, the model writes prose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    /// Whether the model may, must, or must not call a tool. Absent, the
    /// provider's own default holds, which is always the automatic choice.
    ///
    /// A [`ResponseFormat`] overrides this on adapters that constrain output by
    /// forcing a tool: there is only one tool-choice slot on the wire, and the
    /// output constraint is the stronger promise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    /// Whether the provider's own web search tool is enabled for this request,
    /// and the budget it may spend. Absent, the model has no vendor search and
    /// reaches the web only through the tools OpenWave advertises.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_web_search: Option<VendorWebSearch>,
    /// Pixels for the [`ContentBlock::Image`] blocks in `messages`.
    ///
    /// Hydrated from the blob store for exactly this request. Skipped by serde
    /// so no serialized request — a debug log, an error payload, a journal
    /// entry — can carry image bytes, and dropped as soon as the request is
    /// built. Adapters resolve each block's blob id here and fail when it is
    /// missing rather than quietly sending a question about an absent image.
    #[serde(skip)]
    pub images: ImageAttachments,
}

/// Token accounting for a completion.
///
/// Prompt tokens are split so prompt-cache cost is legible: `input_tokens` is the
/// fresh (uncached) prompt, `cache_read_input_tokens` is what was served from the
/// provider's cache, and `cache_creation_input_tokens` is what was written to it
/// (Anthropic-style). Providers that don't report caching leave the cache fields
/// at zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Fresh (uncached) prompt tokens.
    pub input_tokens: u32,
    /// Tokens generated.
    pub output_tokens: u32,
    /// Prompt tokens served from the provider's cache (a read hit).
    #[serde(default)]
    pub cache_read_input_tokens: u32,
    /// Prompt tokens written to the provider's cache.
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
}

impl Usage {
    /// Accumulate token counts without wrapping any component.
    #[must_use]
    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        Some(Self {
            input_tokens: self.input_tokens.checked_add(rhs.input_tokens)?,
            output_tokens: self.output_tokens.checked_add(rhs.output_tokens)?,
            cache_read_input_tokens: self
                .cache_read_input_tokens
                .checked_add(rhs.cache_read_input_tokens)?,
            cache_creation_input_tokens: self
                .cache_creation_input_tokens
                .checked_add(rhs.cache_creation_input_tokens)?,
        })
    }
}

/// Why a completion stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StopReason {
    /// The model finished its turn naturally.
    EndTurn,
    /// The output token cap was hit.
    MaxTokens,
    /// The model stopped to call tools.
    ToolUse,
    /// A stop sequence was produced.
    StopSequence,
    /// The model declined the request under a safety policy.
    Refusal,
    /// The caller cancelled the turn.
    Cancelled,
}

/// Bounded provider-neutral detail for a model refusal.
///
/// Providers may add categories over time, so the value stays open rather than
/// becoming an enum. The constructor admits only a short identifier suitable
/// for durable events and renderer projection; prose belongs to renderer-owned
/// copy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RefusalDetails {
    category: Option<String>,
}

impl RefusalDetails {
    /// Maximum UTF-8 bytes retained from a provider category.
    pub const MAX_CATEGORY_BYTES: usize = 64;

    /// Build details from an optional provider category, dropping malformed or
    /// overlong values rather than persisting untrusted provider text.
    #[must_use]
    pub fn from_category(category: Option<&str>) -> Self {
        let category = category
            .filter(|category| {
                !category.is_empty()
                    && category.len() <= Self::MAX_CATEGORY_BYTES
                    && category.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            })
            .map(str::to_owned);
        Self { category }
    }

    /// The provider's bounded policy-category identifier, when present.
    #[must_use]
    pub fn category(&self) -> Option<&str> {
        self.category.as_deref()
    }
}

impl<'de> Deserialize<'de> for RefusalDetails {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct SerializedRefusalDetails {
            #[serde(default)]
            category: Option<String>,
        }

        let details = SerializedRefusalDetails::deserialize(deserializer)?;
        Ok(Self::from_category(details.category.as_deref()))
    }
}

/// Durable outcome metadata for a refused completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusalOutcome {
    details: RefusalDetails,
    partial_output: bool,
}

impl RefusalOutcome {
    /// Build an outcome at the consumer boundary that observed the text stream.
    #[must_use]
    pub fn new(details: RefusalDetails, partial_output: bool) -> Self {
        Self {
            details,
            partial_output,
        }
    }

    /// The bounded provider category, when one was supplied.
    #[must_use]
    pub fn category(&self) -> Option<&str> {
        self.details.category()
    }

    /// Whether assistant text arrived before the refusal.
    #[must_use]
    pub fn partial_output(&self) -> bool {
        self.partial_output
    }
}

/// A normalized streaming event from a provider. Adapters translate each API's
/// native stream into this; the loop reduces these into [`crate::model`] rows
/// and `AgentEvent`s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderEvent {
    /// A chunk of assistant text.
    TextDelta {
        /// The text fragment.
        text: String,
    },
    /// A chunk of reasoning/thinking text, where the provider exposes it.
    ReasoningDelta {
        /// The reasoning fragment.
        text: String,
    },
    /// One reasoning block, complete and opaque, as the provider emitted it.
    ///
    /// Where `ReasoningDelta` is display text, this is the replayable
    /// artifact (an Anthropic `thinking` or `redacted_thinking` block,
    /// signature included). `data` is the block exactly as the provider
    /// accepts it back; consumers must not parse, filter, or reorder it,
    /// because replay validity depends on the blocks matching what the model
    /// generated. Carried in-memory for one turn at most — it is never
    /// journaled or persisted.
    ReasoningBlock {
        /// The provider-native block, replayed verbatim.
        data: Value,
    },
    /// A tool call has begun; name and id are known.
    ToolCallStarted {
        /// Stream-local index correlating this call's arg deltas.
        index: u32,
        /// Provider-assigned call id.
        id: String,
        /// The tool being called.
        name: String,
    },
    /// A fragment of a tool call's JSON arguments.
    ToolCallArgsDelta {
        /// The index from the matching `ToolCallStarted`.
        index: u32,
        /// Partial JSON to concatenate.
        fragment: String,
    },
    /// A tool the provider ran server-side, call and result both complete.
    ///
    /// Nothing downstream executes it — see
    /// [`ContentBlock::ProviderExecutedToolCall`], which this becomes. An
    /// adapter emits it only once the provider has reported the result, so a
    /// consumer never has to correlate it with anything.
    ProviderExecutedToolCall {
        /// Tool name as shown to users and the journal, e.g. `web_search`.
        name: String,
        /// The arguments the provider ran the tool with.
        input: Value,
        /// The result, normalized to the host tool's own output shape.
        output: Value,
        /// Whether the provider's tool failed.
        is_error: bool,
        /// Provider-native blocks for same-route replay, when captured.
        replay: Option<ProviderToolReplay>,
    },
    /// Final token usage for the completion.
    Usage(Usage),
    /// The completion has stopped.
    Stop {
        /// Why it stopped.
        reason: StopReason,
    },
    /// The completion stopped because the model refused the request.
    ///
    /// This terminal form carries refusal detail atomically. The agent derives
    /// whether output was partial from text it actually accumulated rather
    /// than trusting provider metadata.
    Refusal {
        /// Bounded provider-neutral refusal detail.
        details: RefusalDetails,
    },
    /// The stream ended abnormally — a transport error cut it off mid-flight.
    ///
    /// Distinct from [`ProviderEvent::Stop`]: whatever text and tool calls have
    /// accumulated so far are incomplete, and a tool call's JSON arguments in
    /// particular may be truncated mid-value. Consumers must discard the
    /// partial step and treat the completion as failed rather than acting on
    /// it.
    ///
    /// The payload is classified the way the equivalent HTTP-status failure
    /// would be, so a mid-stream overload or rate limit surfaces to the client
    /// under that kind rather than the generic `provider`.
    Failed {
        /// Why the stream broke, classified for the turn's failure detail.
        error: crate::error::ProviderErrorInfo,
    },
}

/// An LLM backend the agent streams completions from. Held as a trait object,
/// so it must stay object-safe (`#[async_trait]`).
#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// This adapter's stable identifier.
    fn id(&self) -> ProviderId;

    /// Stream a completion, normalizing the provider's events into
    /// [`ProviderEvent`]s.
    ///
    /// The stream is `'static`: it must not borrow `self`, so the agent loop can
    /// hold a provider behind `Arc<dyn ModelProvider>` and keep polling the
    /// stream across awaits. Adapters move (or cheaply `Arc`-clone, e.g.
    /// `reqwest::Client`) whatever they need into the returned stream.
    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_block_tags_its_variant() {
        let json = serde_json::to_value(ContentBlock::Text { text: "hi".into() }).unwrap();
        assert_eq!(json["type"], "text");
        assert_eq!(json["text"], "hi");
    }

    #[test]
    fn a_provider_executed_call_renders_titles_and_stays_bounded() {
        // Adapters with no wire form for another provider's server-side call
        // send this instead, and it rides in context on every later request.
        let results = serde_json::json!({
            "provider": "anthropic",
            "results": [
                {"title": "A", "url": "https://example.com/a"},
                {"title": "B", "url": "https://example.com/b"},
            ]
        });
        assert_eq!(
            provider_executed_tool_call_text(
                "web_search",
                &serde_json::json!({"query": "rust 2027"}),
                &results,
                false
            ),
            "[web_search: rust 2027 -> 2 results]\n- A — https://example.com/a\n- B — https://example.com/b"
        );
        assert_eq!(
            provider_executed_tool_call_text(
                "web_search",
                &serde_json::json!({"query": "q"}),
                &serde_json::json!({"error_code": "max_uses_exceeded"}),
                true
            ),
            "[web_search: q -> failed (max_uses_exceeded)]"
        );
        // An unbounded input cannot grow the prompt one line at a time.
        let long = provider_executed_tool_call_text(
            "web_search",
            &serde_json::json!({"query": "x".repeat(10_000)}),
            &results,
            false,
        );
        assert!(long.chars().count() < 400, "{}", long.chars().count());
    }

    #[test]
    fn provider_tool_replay_is_gated_on_origin() {
        let replay = ProviderToolReplay::captured(
            ReasoningOrigin {
                provider: Some(ProviderId::new("anthropic")),
                model: "claude-opus-5".into(),
            },
            vec![serde_json::json!({"type": "server_tool_use"})],
        );
        assert_eq!(
            replay
                .replayable_for(Some(&ProviderId::new("anthropic")), "claude-opus-5")
                .len(),
            1
        );
        assert!(replay
            .replayable_for(Some(&ProviderId::new("openai")), "claude-opus-5")
            .is_empty());
        assert!(replay
            .replayable_for(Some(&ProviderId::new("anthropic")), "claude-sonnet-5")
            .is_empty());
    }

    #[test]
    fn chat_request_omits_empty_optionals() {
        let req = ChatRequest {
            provider: None,
            model: "m".into(),
            reasoning_model: false,
            system: None,
            messages: vec![ChatMessage::text(Role::User, "hello")],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            reasoning_effort: None,
            images: ImageAttachments::new(),
            ..Default::default()
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("system"), "{json}");
        assert!(!json.contains("tools"), "{json}");
        assert!(!json.contains("max_tokens"), "{json}");
        assert!(!json.contains("reasoning_effort"), "{json}");
    }

    #[test]
    fn usage_cache_fields_default_to_zero_and_roundtrip() {
        // Absent cache fields (e.g. a provider that doesn't report caching)
        // deserialize to zero.
        let u: Usage = serde_json::from_str(r#"{"input_tokens":10,"output_tokens":5}"#).unwrap();
        assert_eq!(u.cache_read_input_tokens, 0);
        assert_eq!(u.cache_creation_input_tokens, 0);

        let full = Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_input_tokens: 100,
            cache_creation_input_tokens: 20,
        };
        assert_eq!(
            serde_json::from_str::<Usage>(&serde_json::to_string(&full).unwrap()).unwrap(),
            full
        );
        assert_eq!(full.checked_add(Usage::default()), Some(full));
        assert_eq!(
            Usage {
                input_tokens: u32::MAX,
                ..Usage::default()
            }
            .checked_add(Usage {
                input_tokens: 1,
                ..Usage::default()
            }),
            None
        );
    }

    #[test]
    fn provider_event_roundtrips() {
        let ev = ProviderEvent::Stop {
            reason: StopReason::ToolUse,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(serde_json::from_str::<ProviderEvent>(&json).unwrap(), ev);

        let failed = ProviderEvent::Failed {
            error: crate::error::ProviderErrorInfo::from_error(
                &crate::error::AgentError::Overloaded("p returned 529 (overloaded_error)".into()),
            ),
        };
        let json = serde_json::to_string(&failed).unwrap();
        assert_eq!(
            serde_json::from_str::<ProviderEvent>(&json).unwrap(),
            failed
        );
    }

    #[test]
    fn refusal_categories_are_bounded_identifiers() {
        assert_eq!(
            RefusalDetails::from_category(Some("reasoning_extraction")).category(),
            Some("reasoning_extraction")
        );
        assert_eq!(
            RefusalDetails::from_category(Some("General Harms")).category(),
            None
        );
        assert_eq!(
            RefusalDetails::from_category(Some(
                &"x".repeat(RefusalDetails::MAX_CATEGORY_BYTES + 1)
            ))
            .category(),
            None
        );
        let decoded: RefusalDetails =
            serde_json::from_value(serde_json::json!({"category": "NOT SAFE"})).unwrap();
        assert_eq!(
            decoded.category(),
            None,
            "deserialization must preserve the constructor's bounds"
        );
    }
}
