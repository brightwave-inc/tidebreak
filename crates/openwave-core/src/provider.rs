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
}

/// A single message in the conversation sent to a provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Who authored the message.
    pub role: Role,
    /// The ordered content blocks.
    pub content: Vec<ContentBlock>,
}

impl ChatMessage {
    /// A single-text-block message.
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }
}

/// A request for one streamed model completion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    /// Explicit provider route selected by the host. Composite routers must
    /// honor this hint exactly and must not silently fall back to another
    /// provider. Direct adapters may ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderId>,
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
    /// The caller cancelled the turn.
    Cancelled,
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
    /// Final token usage for the completion.
    Usage(Usage),
    /// The completion has stopped.
    Stop {
        /// Why it stopped.
        reason: StopReason,
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
    use std::sync::Arc;

    use futures::stream::{self, StreamExt};

    use super::*;

    struct Dummy;

    #[async_trait]
    impl ModelProvider for Dummy {
        fn id(&self) -> ProviderId {
            ProviderId::new("dummy")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            Ok(stream::empty().boxed())
        }
    }

    /// The returned stream must be `'static` — holdable after the provider it
    /// came from is dropped, as the agent loop needs when the provider lives in
    /// an `Arc<dyn ModelProvider>` registry.
    #[test]
    fn provider_stream_outlives_the_provider_borrow() {
        let provider: Arc<dyn ModelProvider> = Arc::new(Dummy);
        let stream = futures::executor::block_on(provider.stream(ChatRequest {
            provider: None,
            model: "m".into(),
            reasoning_model: false,
            system: None,
            messages: vec![],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            reasoning_effort: None,
        }))
        .unwrap();
        drop(provider);
        // Holding the stream after dropping `provider` only compiles if it is
        // `'static` and did not borrow `self`.
        let _held: BoxStream<'static, ProviderEvent> = stream;
    }

    #[test]
    fn content_block_tags_its_variant() {
        let json = serde_json::to_value(ContentBlock::Text { text: "hi".into() }).unwrap();
        assert_eq!(json["type"], "text");
        assert_eq!(json["text"], "hi");
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
    }
}
