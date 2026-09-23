//! Host-owned metadata for the models Tidebreak curates.
//!
//! This is the single source of truth for both the public model catalog and
//! provider routing. Provider configuration decides which registry entries are
//! available; it does not duplicate model ids or capabilities.

use tidebreak_core::ReasoningEffort;

use crate::providers::ProviderKind;

/// An input modality a model accepts.
///
/// `snake_case` matches the strings `as_str` has always produced, so the enum
/// serializes exactly as the hand-built list of strings it replaces on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum InputModality {
    /// Plain text and structured text content.
    Text,
    /// Image content alongside text.
    Image,
}

/// How thoroughly Tidebreak has exercised a model's agent-facing behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum VerificationTier {
    /// Tool-calling and streaming have been exercised end to end.
    Verified,
    /// The model is selectable, but Tidebreak has not verified those contracts.
    Unverified,
}

impl InputModality {
    /// The wire spelling this modality has always had.
    ///
    /// Serde owns the wire form now that `ModelInfo` carries the enum, so this is
    /// no longer what produces it — it is the independently written expectation
    /// that the move did not change it, checked by the test below. Test-only,
    /// because a second live spelling of the same strings is the duplication this
    /// replaced.
    #[cfg(test)]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
        }
    }
}

const TEXT_AND_IMAGE: &[InputModality] = &[InputModality::Text, InputModality::Image];

/// The reasoning-effort scales the curated rows draw from, ascending.
///
/// No provider offers one scale across its whole line, so these are named for
/// their contents rather than for a provider or a generation.
const EFFORT_NONE_TO_MAX: &[ReasoningEffort] = &[
    ReasoningEffort::None,
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::XHigh,
    ReasoningEffort::Max,
];
const EFFORT_NONE_TO_XHIGH: &[ReasoningEffort] = &[
    ReasoningEffort::None,
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::XHigh,
];
/// Gemini 3 Flash maps Tidebreak's `none` to its `minimal` thinking level and
/// has no separate levels above `high`.
const EFFORT_NONE_TO_HIGH: &[ReasoningEffort] = &[
    ReasoningEffort::None,
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
];
/// Gemini 3.1 Pro Preview and Gemini Flash from 3.7 on start at `low`; they
/// reject `minimal`. Grok 4.5 treats `xhigh` as `high`, so it stops here too.
const EFFORT_LOW_TO_HIGH: &[ReasoningEffort] = &[
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
];
const EFFORT_LOW_TO_XHIGH: &[ReasoningEffort] = &[
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::XHigh,
];
const EFFORT_LOW_TO_MAX: &[ReasoningEffort] = &[
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::XHigh,
    ReasoningEffort::Max,
];
/// Claude Opus and Sonnet 4.6 accept `max`, but not the newer `xhigh` level.
/// GLM-5.3 on Together documents the same four levels.
const EFFORT_LOW_TO_HIGH_AND_MAX: &[ReasoningEffort] = &[
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::Max,
];
/// Kimi K3's always-on thinking control exposes exactly these three levels,
/// and so do DeepSeek V4 Flash and GLM-5.3 Flash on Together.
const EFFORT_LOW_HIGH_MAX: &[ReasoningEffort] = &[
    ReasoningEffort::Low,
    ReasoningEffort::High,
    ReasoningEffort::Max,
];
/// DeepSeek V4 Pro maps `low` and `medium` to `high`, and `xhigh` to `max`.
const EFFORT_HIGH_AND_MAX: &[ReasoningEffort] = &[ReasoningEffort::High, ReasoningEffort::Max];
/// For a model that takes no effort parameter at all. Not the same as a model
/// that ignores one: Claude Haiku 4.5 rejects the request.
const EFFORT_UNSUPPORTED: &[ReasoningEffort] = &[];

/// Separator in the stable provider-scoped selection key persisted for new
/// defaults, chat overrides, and turn receipts.
pub const MODEL_KEY_SEPARATOR: &str = "::";

/// Capability and presentation metadata for a curated model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    /// Identifier passed to the provider and stored as `chat.model`.
    pub id: &'static str,
    /// Human-readable label for model selectors.
    pub display_name: &'static str,
    /// Provider that serves the model.
    pub provider: ProviderKind,
    /// How thoroughly this exact provider/model row has been exercised.
    pub verification: VerificationTier,
    /// Whether this row is visible by default in a model picker.
    ///
    /// Curation, not capability: a row that is not recommended is exactly as
    /// selectable, replayable, and supported as one that is — it simply does
    /// not appear until the reader asks for the full catalog. The recommended
    /// set is the current flagship tier of each provider; superseded
    /// generations, speed and cost trims, and non-flagship variants are not.
    /// The field is mandatory so a new catalog row cannot forget to take a
    /// stance.
    pub recommended: bool,
    /// Maximum context window in tokens.
    pub context_window: u32,
    /// Maximum model output in tokens.
    pub max_output_tokens: u32,
    /// Input modalities accepted by the model.
    pub input_modalities: &'static [InputModality],
    /// Whether the model can produce an internal reasoning stream.
    pub supports_reasoning: bool,
    /// Whether a turn on this model may enable the provider's own server-side
    /// web search tool instead of Tidebreak's client-side one.
    ///
    /// Like the other capability flags, this gates behavior rather than
    /// display: setting it asserts that the routing adapter for this row's
    /// provider emits the vendor tool on the request, not merely that the
    /// vendor documents one. Gateway and custom compatible routes cannot make
    /// that promise, and the invariant below holds them to it.
    pub supports_vendor_web_search: bool,
    /// The reasoning-effort levels this model accepts, ascending.
    ///
    /// A single flag cannot describe the range: a model may take `high` and
    /// reject `xhigh`, or take `xhigh` and reject `max`. Empty means the model
    /// exposes no effort control, and the parameter is left off its requests.
    pub reasoning_efforts: &'static [ReasoningEffort],
}

impl ModelSpec {
    /// Upstream vendor for a model hosted through a provider that serves more
    /// than one native protocol family.
    ///
    /// No curated provider serves more than one family today, so every row
    /// reports its serving provider as its own vendor. The projection stays
    /// because the client uses it to pick a model's mark and to keep a legacy
    /// bare-id selection on the direct provider that owns it.
    pub fn vendor(&self) -> Option<ProviderKind> {
        None
    }

    /// Whether this model accepts `modality`.
    #[cfg(test)]
    pub fn accepts(&self, modality: InputModality) -> bool {
        self.input_modalities.contains(&modality)
    }

    /// Whether callers can choose a reasoning-effort level at all.
    #[cfg(test)]
    pub const fn supports_reasoning_effort(&self) -> bool {
        !self.reasoning_efforts.is_empty()
    }

    /// Whether this model's provider can serve a dedicated web-search
    /// sub-request: one tool-free call that carries only the provider's own
    /// hosted search and returns what it cited.
    ///
    /// This is deliberately not [`Self::supports_vendor_web_search`], which
    /// answers a different question — whether the routing adapter hands the
    /// model a hosted search *during an agent turn*, alongside the host's own
    /// tools. Both OpenAI and Gemini keep that false, and for good reason:
    /// neither endpoint can bound how many searches one turn spends. A
    /// sub-request has no such problem. The host issues it, gets one answer
    /// back, and never continues it, so the host's own call count is the
    /// budget.
    ///
    /// Anthropic is absent because it needs nothing here: its rows already
    /// carry a native in-turn search, which is strictly better than a
    /// round-trip through a second call.
    pub fn supports_search_subrequest(&self) -> bool {
        matches!(self.provider, ProviderKind::Openai | ProviderKind::Gemini)
    }

    /// Whether this OpenAI row may run under ChatGPT / Codex subscription auth.
    ///
    /// ChatGPT OAuth routes inference through the Codex backend, which rejects
    /// some API-only model ids (today: `gpt-5.4-nano`) with a 400. Non-OpenAI
    /// rows return `true` — ChatGPT auth does not apply to them, and callers
    /// gate on auth mode before consulting this.
    pub fn supports_chatgpt_auth(&self) -> bool {
        !matches!(
            (self.provider, self.id),
            (ProviderKind::Openai, "gpt-5.4-nano")
        )
    }

    /// Whether this exact hosted row accepts Chat Completions function tools.
    ///
    /// Native providers and most hosted-compatible rows carry the tool path
    /// Tidebreak uses. Rows whose host does not offer function calling, or whose
    /// same-model continuation requires reasoning content the compatible
    /// adapter cannot yet replay, run as chat-only models rather than receiving
    /// schemas they would reject or could not safely continue after a tool call.
    pub fn supports_tools(&self) -> bool {
        !matches!(
            (self.provider, self.id),
            (
                ProviderKind::Fireworks,
                "accounts/fireworks/models/deepseek-v4p1-flash"
            ) | (ProviderKind::Fireworks, "accounts/fireworks/models/glm-5p2")
                | (ProviderKind::Together, "Qwen/Qwen3.7-Max")
                | (ProviderKind::Together, "Qwen/Qwen3.7-Plus")
                | (ProviderKind::Together, "deepseek-ai/DeepSeek-V4-Pro-0813")
                | (ProviderKind::Together, "deepseek-ai/DeepSeek-V4.1-Flash")
                | (ProviderKind::Together, "deepseek-ai/DeepSeek-V4-Flash-0731")
        )
    }

    /// Whether this exact hosted row can enforce the strict JSON Schema used
    /// by utility work.
    ///
    /// Native adapters carry this contract for every curated row. Fireworks
    /// exposes strict schema output at the platform layer. Together documents
    /// it per serverless model, so that host uses an allow-list: a new row stays
    /// ineligible for utility work until its endpoint explicitly supports the
    /// response shape Tidebreak sends. Configured rows follow the same
    /// provider-level answer through [`ProviderKind::enforces_structured_output`].
    pub fn supports_structured_output(&self) -> bool {
        match self.provider {
            ProviderKind::Together => matches!(
                self.id,
                "thinkingmachines/Inkling"
                    | "MiniMaxAI/MiniMax-M3"
                    | "moonshotai/Kimi-K3"
                    | "zai-org/GLM-5.3"
                    | "zai-org/GLM-5.3-Flash"
                    | "zai-org/GLM-5.2"
                    | "deepseek-ai/DeepSeek-V4-Pro-0813"
                    | "deepseek-ai/DeepSeek-V4.1-Flash"
                    | "deepseek-ai/DeepSeek-V4-Flash-0731"
            ),
            provider => provider.enforces_structured_output(),
        }
    }
}

/// Curated models in picker display order: each provider's current generation
/// first, then the earlier generations a chat can pin when the newest one
/// regresses on its workload.
const MODEL_REGISTRY: &[ModelSpec] = &[
    // The first curated Anthropic row is what a fresh Anthropic-only install
    // resolves to. It stays Opus 5 until the owner makes Opus 5.5, which
    // Anthropic made its default Opus on 2026-09-22, the default here too.
    ModelSpec {
        id: "claude-opus-5",
        display_name: "Claude Opus 5",
        provider: ProviderKind::Anthropic,
        verification: VerificationTier::Verified,
        recommended: true,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: true,
        reasoning_efforts: EFFORT_LOW_TO_MAX,
    },
    // Fable 5.1 is the first Claude that rejects a forced tool choice and
    // binds its thinking blocks to the conversation prefix. The Anthropic
    // adapter reads both from the id, so the row carries nothing for them.
    ModelSpec {
        id: "claude-fable-5-1",
        display_name: "Claude Fable 5.1",
        provider: ProviderKind::Anthropic,
        verification: VerificationTier::Verified,
        recommended: true,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        // Image input is advertised only where the provider documents vision
        // and this provider's adapter shapes hydrated image bytes into the
        // request format. The capability guard below keeps that promise honest.
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: true,
        // Claude 4.6 and later reason on an adaptive thinking block, and the
        // Anthropic adapter sends one along with the chat's chosen effort. The
        // newest generation takes this full scale; 4.6 has the narrower scale
        // declared on its own rows below. There is no `none`, because a model
        // on the adaptive block always reasons.
        reasoning_efforts: EFFORT_LOW_TO_MAX,
    },
    ModelSpec {
        id: "claude-fable-5",
        display_name: "Claude Fable 5",
        provider: ProviderKind::Anthropic,
        verification: VerificationTier::Verified,
        recommended: false,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: true,
        reasoning_efforts: EFFORT_LOW_TO_MAX,
    },
    // Anthropic's default Opus since 2026-09-22. Its adaptive thinking is
    // always on (`none` stays off the scale, as on every Claude row), and it
    // takes the request contract Fable 5.1 introduced: no forced tool choice,
    // native structured output, and thinking bound to the prompt prefix. The
    // adapter reads that contract from the id. Unverified until a live turn
    // runs.
    ModelSpec {
        id: "claude-opus-5-5",
        display_name: "Claude Opus 5.5",
        provider: ProviderKind::Anthropic,
        verification: VerificationTier::Unverified,
        recommended: true,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: true,
        reasoning_efforts: EFFORT_LOW_TO_MAX,
    },
    ModelSpec {
        id: "claude-sonnet-5",
        display_name: "Claude Sonnet 5",
        provider: ProviderKind::Anthropic,
        verification: VerificationTier::Verified,
        recommended: true,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: true,
        reasoning_efforts: EFFORT_LOW_TO_MAX,
    },
    ModelSpec {
        id: "claude-haiku-4-5-20251001",
        display_name: "Claude Haiku 4.5",
        provider: ProviderKind::Anthropic,
        verification: VerificationTier::Verified,
        recommended: false,
        context_window: 200_000,
        max_output_tokens: 64_000,
        input_modalities: TEXT_AND_IMAGE,
        // Haiku 4.5 uses classic extended thinking with a token budget. Until
        // the adapter can send that request shape, keep the runtime honest and
        // do not advertise reasoning that every request would silently omit.
        supports_reasoning: false,
        supports_vendor_web_search: true,
        reasoning_efforts: EFFORT_UNSUPPORTED,
    },
    ModelSpec {
        id: "claude-opus-4-8",
        display_name: "Claude Opus 4.8",
        provider: ProviderKind::Anthropic,
        verification: VerificationTier::Verified,
        recommended: false,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: true,
        reasoning_efforts: EFFORT_LOW_TO_MAX,
    },
    ModelSpec {
        id: "claude-opus-4-7",
        display_name: "Claude Opus 4.7",
        provider: ProviderKind::Anthropic,
        verification: VerificationTier::Verified,
        recommended: false,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: true,
        reasoning_efforts: EFFORT_LOW_TO_MAX,
    },
    ModelSpec {
        id: "claude-opus-4-6",
        display_name: "Claude Opus 4.6",
        provider: ProviderKind::Anthropic,
        verification: VerificationTier::Verified,
        recommended: false,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: true,
        reasoning_efforts: EFFORT_LOW_TO_HIGH_AND_MAX,
    },
    ModelSpec {
        id: "claude-sonnet-4-6",
        display_name: "Claude Sonnet 4.6",
        provider: ProviderKind::Anthropic,
        verification: VerificationTier::Verified,
        recommended: false,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: true,
        reasoning_efforts: EFFORT_LOW_TO_HIGH_AND_MAX,
    },
    // OpenAI's Responses hosted-search tool has no wire control for the
    // mandatory per-turn `VendorWebSearch::max_uses` budget. Keep every OpenAI
    // row honest until the adapter can enforce that cap before provider egress.
    //
    // First, because the built-in default is this provider's first curated
    // row and the default stays GPT-5.6 Sol. GPT-6 Sol follows it.
    ModelSpec {
        id: "gpt-5.6-sol",
        display_name: "GPT-5.6 Sol",
        provider: ProviderKind::Openai,
        verification: VerificationTier::Verified,
        recommended: true,
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        // The whole GPT-5 line reasons on a caller-selected effort, which the
        // OpenAI-compatible adapter already sends alongside
        // `max_completion_tokens`. Only the 5.6 generation added `max`.
        reasoning_efforts: EFFORT_NONE_TO_MAX,
    },
    // GPT-6 Sol and Luna shipped on 2026-09-22 with GPT-5.6 Sol's limits and
    // effort scale. Their docs limit function calling in Chat Completions to
    // `none` effort; Tidebreak calls OpenAI through Responses, which has no
    // such limit. Unverified until a live turn runs.
    ModelSpec {
        id: "gpt-6-sol",
        display_name: "GPT-6 Sol",
        provider: ProviderKind::Openai,
        verification: VerificationTier::Unverified,
        recommended: true,
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_NONE_TO_MAX,
    },
    // Astra is OpenAI's most capable model, but its API page still sends
    // keys through early-access setup, so it stays out of the default view
    // until access is general and a live turn has run.
    ModelSpec {
        id: "gpt-6-astra",
        display_name: "GPT-6 Astra",
        provider: ProviderKind::Openai,
        verification: VerificationTier::Unverified,
        recommended: false,
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_TO_MAX,
    },
    // The cost tier of the GPT-6 line, so not shown by default.
    ModelSpec {
        id: "gpt-6-luna",
        display_name: "GPT-6 Luna",
        provider: ProviderKind::Openai,
        verification: VerificationTier::Unverified,
        recommended: false,
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_NONE_TO_MAX,
    },
    ModelSpec {
        id: "gpt-5.6-terra",
        display_name: "GPT-5.6 Terra",
        provider: ProviderKind::Openai,
        verification: VerificationTier::Verified,
        recommended: false,
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_NONE_TO_MAX,
    },
    ModelSpec {
        id: "gpt-5.6-luna",
        display_name: "GPT-5.6 Luna",
        provider: ProviderKind::Openai,
        verification: VerificationTier::Verified,
        recommended: false,
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_NONE_TO_MAX,
    },
    ModelSpec {
        id: "gpt-5.5",
        display_name: "GPT-5.5",
        provider: ProviderKind::Openai,
        verification: VerificationTier::Verified,
        recommended: true,
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_NONE_TO_XHIGH,
    },
    ModelSpec {
        id: "gpt-5.4-mini",
        display_name: "GPT-5.4 mini",
        provider: ProviderKind::Openai,
        verification: VerificationTier::Verified,
        recommended: false,
        context_window: 400_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_NONE_TO_XHIGH,
    },
    ModelSpec {
        id: "gpt-5.4-nano",
        display_name: "GPT-5.4 nano",
        provider: ProviderKind::Openai,
        verification: VerificationTier::Verified,
        recommended: false,
        context_window: 400_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        // The smallest tier is the one OpenAI has consistently left off the
        // hosted tools, and a request declaring a tool the model does not take
        // is rejected outright. Left false until the row is exercised against
        // the live API.
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_NONE_TO_XHIGH,
    },
    // xAI publishes a 500,000-token context window for these rows and no
    // output ceiling; its Responses API defaults `max_output_tokens` to
    // 128,000 with reasoning included. Keep the output cap conservative until
    // each row is exercised end to end. The first-party Responses route
    // carries image input (PNG and JPEG). Reasoning cannot be turned off, so
    // no row offers `none`.
    //
    // The first curated xAI row is what a fresh xAI-only install resolves to,
    // so Grok 4.6 stays first until the owner makes Grok 4.7 the default.
    ModelSpec {
        id: "grok-4.6",
        display_name: "Grok 4.6",
        provider: ProviderKind::Xai,
        verification: VerificationTier::Unverified,
        recommended: true,
        context_window: 500_000,
        max_output_tokens: 32_768,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_TO_XHIGH,
    },
    // xAI's current general flagship. Unverified until a live turn runs.
    ModelSpec {
        id: "grok-4.7",
        display_name: "Grok 4.7",
        provider: ProviderKind::Xai,
        verification: VerificationTier::Unverified,
        recommended: true,
        context_window: 500_000,
        max_output_tokens: 32_768,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_TO_XHIGH,
    },
    // Still the model behind xAI's `grok-build-latest` alias, so it stays in
    // the default view. xAI's reasoning guide treats `xhigh` as `high` on
    // this model, so its scale stops at `high`.
    ModelSpec {
        id: "grok-4.5",
        display_name: "Grok 4.5",
        provider: ProviderKind::Xai,
        verification: VerificationTier::Unverified,
        recommended: true,
        context_window: 500_000,
        max_output_tokens: 32_768,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_TO_HIGH,
    },
    // Gemini rows are intentionally limited to ids currently published by
    // Google. All six accept images, expose thinking levels through `high`,
    // and have 1,048,576 input / 65,536 output token limits. Flash 3.6 and
    // earlier start at `minimal`; Flash 3.7 and later reject it and start at
    // `low`, as Pro Preview does. The native adapter owns the corresponding
    // GenerateContent wire shape.
    ModelSpec {
        id: "gemini-3.8-flash",
        display_name: "Gemini 3.8 Flash",
        provider: ProviderKind::Gemini,
        verification: VerificationTier::Verified,
        recommended: true,
        context_window: 1_048_576,
        max_output_tokens: 65_536,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_TO_HIGH,
    },
    ModelSpec {
        id: "gemini-3.7-flash",
        display_name: "Gemini 3.7 Flash",
        provider: ProviderKind::Gemini,
        verification: VerificationTier::Verified,
        recommended: false,
        context_window: 1_048_576,
        max_output_tokens: 65_536,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_TO_HIGH,
    },
    ModelSpec {
        id: "gemini-3.6-flash",
        display_name: "Gemini 3.6 Flash",
        provider: ProviderKind::Gemini,
        verification: VerificationTier::Verified,
        recommended: true,
        context_window: 1_048_576,
        max_output_tokens: 65_536,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_NONE_TO_HIGH,
    },
    ModelSpec {
        id: "gemini-3.5-flash",
        display_name: "Gemini 3.5 Flash",
        provider: ProviderKind::Gemini,
        verification: VerificationTier::Verified,
        recommended: true,
        context_window: 1_048_576,
        max_output_tokens: 65_536,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_NONE_TO_HIGH,
    },
    ModelSpec {
        id: "gemini-3.5-flash-lite",
        display_name: "Gemini 3.5 Flash-Lite",
        provider: ProviderKind::Gemini,
        verification: VerificationTier::Verified,
        recommended: false,
        context_window: 1_048_576,
        max_output_tokens: 65_536,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_NONE_TO_HIGH,
    },
    ModelSpec {
        id: "gemini-3.1-pro-preview",
        display_name: "Gemini 3.1 Pro Preview",
        provider: ProviderKind::Gemini,
        verification: VerificationTier::Verified,
        recommended: true,
        context_window: 1_048_576,
        max_output_tokens: 65_536,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_TO_HIGH,
    },
    // Fireworks serverless catalog, checked against the public model pages and
    // changelog on 2026-09-23. The ids are the exact paths sent to Chat
    // Completions. Where Fireworks does not publish a generation ceiling,
    // Tidebreak deliberately uses a conservative cap instead of treating the
    // context window as output.
    ModelSpec {
        id: "accounts/fireworks/models/kimi-k3",
        display_name: "Kimi K3",
        provider: ProviderKind::Fireworks,
        verification: VerificationTier::Unverified,
        recommended: true,
        context_window: 1_040_000,
        max_output_tokens: 32_768,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_HIGH_MAX,
    },
    // GLM-5.3 replaces GLM-5.2, which leaves Fireworks serverless on
    // 2026-09-25. Fireworks publishes no reasoning control for it.
    ModelSpec {
        id: "accounts/fireworks/models/glm-5p3",
        display_name: "GLM-5.3",
        provider: ProviderKind::Fireworks,
        verification: VerificationTier::Unverified,
        recommended: true,
        context_window: 1_040_000,
        max_output_tokens: 65_536,
        input_modalities: &[InputModality::Text],
        supports_reasoning: false,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_UNSUPPORTED,
    },
    ModelSpec {
        id: "accounts/fireworks/models/glm-5p3-flash",
        display_name: "GLM-5.3 Flash",
        provider: ProviderKind::Fireworks,
        verification: VerificationTier::Unverified,
        recommended: false,
        context_window: 1_040_000,
        max_output_tokens: 65_536,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: false,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_UNSUPPORTED,
    },
    // Fireworks lists no context length for Qwen3.8 Max. Alibaba documents a
    // 1M-token window for the model, but a host may serve less, so the row
    // keeps the previous Qwen row's 262K. The model thinks, and Fireworks
    // documents no effort values for it, so the row offers no levels.
    ModelSpec {
        id: "accounts/fireworks/models/qwen3p8-max",
        display_name: "Qwen3.8 Max",
        provider: ProviderKind::Fireworks,
        verification: VerificationTier::Unverified,
        recommended: true,
        context_window: 262_000,
        max_output_tokens: 65_536,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_UNSUPPORTED,
    },
    ModelSpec {
        id: "accounts/fireworks/models/minimax-m3",
        display_name: "MiniMax M3",
        provider: ProviderKind::Fireworks,
        verification: VerificationTier::Unverified,
        recommended: false,
        context_window: 512_000,
        max_output_tokens: 32_768,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: false,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_UNSUPPORTED,
    },
    // The one DeepSeek model left on Fireworks serverless, and the named
    // successor to V4 Pro. The model reasons, but Fireworks documents no
    // effort values for it, so the row offers no levels.
    ModelSpec {
        id: "accounts/fireworks/models/deepseek-v4p1-flash",
        display_name: "DeepSeek V4.1 Flash",
        provider: ProviderKind::Fireworks,
        verification: VerificationTier::Unverified,
        recommended: true,
        context_window: 1_040_000,
        max_output_tokens: 65_536,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_UNSUPPORTED,
    },
    ModelSpec {
        id: "accounts/fireworks/models/nemotron-3-ultra-nvfp4",
        display_name: "Nemotron 3 Ultra",
        provider: ProviderKind::Fireworks,
        verification: VerificationTier::Unverified,
        recommended: false,
        context_window: 262_000,
        max_output_tokens: 65_536,
        input_modalities: &[InputModality::Text],
        supports_reasoning: false,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_UNSUPPORTED,
    },
    ModelSpec {
        id: "accounts/fireworks/models/inkling",
        display_name: "Inkling",
        provider: ProviderKind::Fireworks,
        verification: VerificationTier::Unverified,
        recommended: false,
        context_window: 1_040_000,
        max_output_tokens: 65_536,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: false,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_UNSUPPORTED,
    },
    // Fireworks retires these three from serverless on 2026-09-25, naming
    // GLM-5.3 or Kimi K3 as the replacement. They stay until then, out of the
    // default view; the release after that date removes them.
    ModelSpec {
        id: "accounts/fireworks/models/kimi-k2p7-code",
        display_name: "Kimi K2.7 Code",
        provider: ProviderKind::Fireworks,
        verification: VerificationTier::Unverified,
        recommended: false,
        context_window: 262_000,
        max_output_tokens: 32_768,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_UNSUPPORTED,
    },
    ModelSpec {
        id: "accounts/fireworks/models/kimi-k2p6",
        display_name: "Kimi K2.6",
        provider: ProviderKind::Fireworks,
        verification: VerificationTier::Unverified,
        recommended: false,
        context_window: 262_000,
        max_output_tokens: 32_768,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_UNSUPPORTED,
    },
    ModelSpec {
        id: "accounts/fireworks/models/glm-5p2",
        display_name: "GLM-5.2",
        provider: ProviderKind::Fireworks,
        verification: VerificationTier::Unverified,
        recommended: false,
        context_window: 1_040_000,
        max_output_tokens: 65_536,
        input_modalities: &[InputModality::Text],
        supports_reasoning: false,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_UNSUPPORTED,
    },
    // Together serverless catalog, checked against the serverless model table,
    // model pages, and deprecation list on 2026-09-23. Same-family rows
    // intentionally keep Together's own ids and limits rather than inheriting
    // Fireworks metadata by canonical model name.
    ModelSpec {
        id: "thinkingmachines/Inkling",
        display_name: "Inkling",
        provider: ProviderKind::Together,
        verification: VerificationTier::Unverified,
        recommended: false,
        context_window: 524_288,
        max_output_tokens: 65_536,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: false,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_UNSUPPORTED,
    },
    // Hybrid thinking, on by default; Together publishes no effort values.
    ModelSpec {
        id: "MiniMaxAI/MiniMax-M3",
        display_name: "MiniMax M3",
        provider: ProviderKind::Together,
        verification: VerificationTier::Unverified,
        recommended: false,
        context_window: 524_288,
        max_output_tokens: 32_768,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_UNSUPPORTED,
    },
    ModelSpec {
        id: "Qwen/Qwen3.7-Max",
        display_name: "Qwen3.7 Max",
        provider: ProviderKind::Together,
        verification: VerificationTier::Unverified,
        recommended: true,
        // Together does not publish a context value for this row. This stays
        // below the model vendor's current window until the host documents its
        // own limit.
        context_window: 262_144,
        max_output_tokens: 65_536,
        input_modalities: &[InputModality::Text],
        supports_reasoning: false,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_UNSUPPORTED,
    },
    ModelSpec {
        id: "moonshotai/Kimi-K3",
        display_name: "Kimi K3",
        provider: ProviderKind::Together,
        verification: VerificationTier::Unverified,
        recommended: true,
        context_window: 1_048_576,
        max_output_tokens: 32_768,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_HIGH_MAX,
    },
    // GLM-5.3 thinks on every request; `none` is not one of its levels.
    ModelSpec {
        id: "zai-org/GLM-5.3",
        display_name: "GLM-5.3",
        provider: ProviderKind::Together,
        verification: VerificationTier::Unverified,
        recommended: true,
        context_window: 1_048_575,
        max_output_tokens: 65_536,
        input_modalities: &[InputModality::Text],
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_TO_HIGH_AND_MAX,
    },
    // Text only: Together's serverless vision table does not list it.
    ModelSpec {
        id: "zai-org/GLM-5.3-Flash",
        display_name: "GLM-5.3 Flash",
        provider: ProviderKind::Together,
        verification: VerificationTier::Unverified,
        recommended: false,
        context_window: 1_048_575,
        max_output_tokens: 65_536,
        input_modalities: &[InputModality::Text],
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_HIGH_MAX,
    },
    // Superseded by GLM-5.3. Thinking is on by default; Together names two
    // effort levels without publishing their values.
    ModelSpec {
        id: "zai-org/GLM-5.2",
        display_name: "GLM-5.2",
        provider: ProviderKind::Together,
        verification: VerificationTier::Unverified,
        recommended: false,
        context_window: 1_048_575,
        max_output_tokens: 131_072,
        input_modalities: &[InputModality::Text],
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_UNSUPPORTED,
    },
    // The serverless V4 Pro since 2026-08-27. `low` and `medium` map to
    // `high`, and `xhigh` to `max`, so only the two distinct levels are
    // offered. DeepSeek's samples cap output at 384,000 tokens.
    ModelSpec {
        id: "deepseek-ai/DeepSeek-V4-Pro-0813",
        display_name: "DeepSeek V4 Pro",
        provider: ProviderKind::Together,
        verification: VerificationTier::Unverified,
        recommended: true,
        context_window: 1_048_576,
        max_output_tokens: 384_000,
        input_modalities: &[InputModality::Text],
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_HIGH_AND_MAX,
    },
    // V4.1 Flash takes its effort as an integer from 1 to 100, which the
    // Chat Completions adapter does not send, so the row offers no levels.
    // Text only: Together's serverless vision table does not list it.
    ModelSpec {
        id: "deepseek-ai/DeepSeek-V4.1-Flash",
        display_name: "DeepSeek V4.1 Flash",
        provider: ProviderKind::Together,
        verification: VerificationTier::Unverified,
        recommended: false,
        context_window: 1_000_000,
        max_output_tokens: 65_536,
        input_modalities: &[InputModality::Text],
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_UNSUPPORTED,
    },
    ModelSpec {
        id: "deepseek-ai/DeepSeek-V4-Flash-0731",
        display_name: "DeepSeek V4 Flash",
        provider: ProviderKind::Together,
        verification: VerificationTier::Unverified,
        recommended: false,
        context_window: 1_048_576,
        max_output_tokens: 384_000,
        input_modalities: &[InputModality::Text],
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_HIGH_MAX,
    },
    ModelSpec {
        id: "Qwen/Qwen3.7-Plus",
        display_name: "Qwen3.7 Plus",
        provider: ProviderKind::Together,
        verification: VerificationTier::Unverified,
        recommended: false,
        context_window: 1_000_000,
        max_output_tokens: 65_536,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: false,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_UNSUPPORTED,
    },
];

/// Curated entries belonging to `provider`, preserving registry display order.
pub fn models_for(provider: ProviderKind) -> impl Iterator<Item = &'static ModelSpec> + Clone {
    MODEL_REGISTRY
        .iter()
        .filter(move |spec| spec.provider == provider)
}

/// Find the canonical direct-vendor owner of a bare curated model id.
///
/// First-class hosted providers may mirror an upstream id under their own
/// provider-qualified key. They do not make an old bare selection change
/// providers or become unresolvable: only the direct row participates here.
pub fn find(id: &str) -> Option<&'static ModelSpec> {
    let mut matches = models_named(id).filter(|spec| spec.vendor().is_none());
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

/// Curated provider owners for a raw model id.
pub fn models_named(id: &str) -> impl Iterator<Item = &'static ModelSpec> + Clone + '_ {
    MODEL_REGISTRY.iter().filter(move |spec| spec.id == id)
}

/// Find an exact curated model under the provider that owns it.
pub fn find_for(provider: ProviderKind, id: &str) -> Option<&'static ModelSpec> {
    MODEL_REGISTRY
        .iter()
        .find(|spec| spec.provider == provider && spec.id == id)
}

/// Build the stable provider-scoped key used at all public selection
/// boundaries. The provider is never inferred again after this point.
pub fn selection_key(provider: ProviderKind, id: &str) -> String {
    format!("{}{MODEL_KEY_SEPARATOR}{id}", provider.as_str())
}

/// Parse a provider-scoped selection key. Model ids may themselves contain the
/// separator; only the first separator is structural.
pub fn parse_selection_key(value: &str) -> Option<(ProviderKind, &str)> {
    let (provider, id) = value.split_once(MODEL_KEY_SEPARATOR)?;
    let provider = ProviderKind::parse(provider)?;
    if id.is_empty() {
        return None;
    }
    Some((provider, id))
}

/// Canonicalize an old bare curated id without changing which provider owns it.
#[cfg(test)]
pub fn migrate_curated_selection(value: &str) -> Option<String> {
    if let Some((provider, id)) = parse_selection_key(value) {
        return find_for(provider, id).map(|_| selection_key(provider, id));
    }
    find(value).map(|spec| selection_key(spec.provider, spec.id))
}

/// Human label for a model id, including a readable fallback for custom ids.
pub fn display_name_for(id: &str) -> String {
    find(id)
        .map(|spec| spec.display_name.to_string())
        .unwrap_or_else(|| derive_display_name(id))
}

fn derive_display_name(id: &str) -> String {
    strip_date_suffix(id)
        .split('-')
        .map(title_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_date_suffix(id: &str) -> &str {
    let tokens: Vec<&str> = id.split('-').collect();
    if tokens.len() < 2 {
        return id;
    }
    let is_digits = |token: &str, len: usize| {
        token.len() == len && token.bytes().all(|byte| byte.is_ascii_digit())
    };

    let last = tokens[tokens.len() - 1];
    if is_digits(last, 8) {
        return &id[..id.len() - last.len() - 1];
    }
    if tokens.len() >= 4 {
        let (year, month, day) = (
            tokens[tokens.len() - 3],
            tokens[tokens.len() - 2],
            tokens[tokens.len() - 1],
        );
        if is_digits(year, 4) && is_digits(month, 2) && is_digits(day, 2) {
            let cut = year.len() + month.len() + day.len() + 3;
            return &id[..id.len() - cut];
        }
    }
    id
}

fn title_token(token: &str) -> String {
    match token {
        "gpt" => return "GPT".to_string(),
        "claude" => return "Claude".to_string(),
        "opus" => return "Opus".to_string(),
        "sonnet" => return "Sonnet".to_string(),
        "haiku" => return "Haiku".to_string(),
        "mini" => return "Mini".to_string(),
        "nano" => return "Nano".to_string(),
        _ => {}
    }
    let mut chars = token.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Whether Tidebreak can actually put an image on the wire for `provider`.
    ///
    /// Provider documentation alone does not earn a model `InputModality::Image`.
    /// Each provider has its own request adapter, and each one here shapes
    /// hydrated image bytes into its request format: the Chat Completions
    /// adapter behind OpenRouter, Ollama, and custom endpoints included, which
    /// is why a configured row on those routes may declare image input. The
    /// match stays exhaustive so a new provider kind has to answer.
    const fn provider_carries_image_input(provider: ProviderKind) -> bool {
        match provider {
            ProviderKind::Anthropic
            | ProviderKind::Openai
            | ProviderKind::Xai
            | ProviderKind::Gemini
            | ProviderKind::Fireworks
            | ProviderKind::Together
            | ProviderKind::Openrouter
            | ProviderKind::Ollama
            | ProviderKind::OpenaiCompatible
            | ProviderKind::ModelGateway => true,
        }
    }

    #[test]
    fn registry_provider_model_keys_are_unique_and_capabilities_are_well_formed() {
        let mut keys = HashSet::new();
        for spec in MODEL_REGISTRY {
            assert!(
                keys.insert((spec.provider, spec.id)),
                "duplicate provider/model key {}::{}",
                spec.provider,
                spec.id
            );
            assert!(spec.context_window > 0);
            assert!(spec.max_output_tokens > 0);
            assert!(spec.context_window >= spec.max_output_tokens);
            assert!(spec.accepts(InputModality::Text));
            assert!(
                !spec.accepts(InputModality::Image) || provider_carries_image_input(spec.provider),
                "{} advertises image input under `{}`, which cannot carry an image block through the complete request path",
                spec.id,
                spec.provider
            );
            assert!(
                !spec.supports_reasoning_effort() || spec.supports_reasoning,
                "{} exposes reasoning effort without reasoning",
                spec.id
            );
            assert!(
                spec.reasoning_efforts
                    .windows(2)
                    .all(|pair| pair[0] < pair[1]),
                "{} lists reasoning-effort levels out of order or with duplicates",
                spec.id
            );
        }
    }

    #[test]
    fn every_curated_row_declares_a_verification_tier() {
        assert!(MODEL_REGISTRY.iter().all(|spec| matches!(
            spec.verification,
            VerificationTier::Verified | VerificationTier::Unverified
        )));
    }

    #[test]
    fn providers_receive_only_their_registry_entries() {
        for &provider in ProviderKind::ALL {
            assert!(models_for(provider).all(|spec| spec.provider == provider));
        }
    }

    #[test]
    fn the_built_in_default_is_the_first_curated_row_of_its_provider() {
        let spec = find(crate::DEFAULT_MODEL).expect("the default model must be curated");
        assert_eq!(spec.provider, ProviderKind::Openai);
        // The default has to be the current generation, not a pin that happened
        // to be current when it was written down.
        assert_eq!(
            models_for(ProviderKind::Openai).next().map(|s| s.id),
            Some(crate::DEFAULT_MODEL),
        );
        // A default the picker hides by default would be selected for every
        // fresh install and visible in none of them.
        assert!(spec.recommended, "the default model must be recommended");
    }

    #[test]
    fn every_provider_with_catalog_rows_recommends_at_least_one() {
        for &provider in ProviderKind::ALL {
            let mut rows = models_for(provider).peekable();
            if rows.peek().is_none() {
                continue;
            }
            assert!(
                rows.any(|spec| spec.recommended),
                "`{provider}` has curated models but recommends none, so a client that \
                 credentials it renders an empty provider"
            );
        }
    }

    #[test]
    fn grok_4_6_stays_first_and_each_grok_row_carries_its_published_scale() {
        // The first curated xAI row is what a fresh xAI-only install resolves
        // to, so Grok 4.6 keeps that place until the owner moves it.
        let grok_ids: Vec<_> = models_for(ProviderKind::Xai).map(|spec| spec.id).collect();
        assert_eq!(grok_ids, ["grok-4.6", "grok-4.7", "grok-4.5"]);
        for id in grok_ids {
            let grok = find_for(ProviderKind::Xai, id).unwrap();
            assert_eq!(grok.context_window, 500_000, "{id}");
            assert_eq!(grok.max_output_tokens, 32_768, "{id}");
            assert!(grok.accepts(InputModality::Image), "{id}");
            // xAI reasoning cannot be turned off, so no row offers "Off".
            assert!(
                !grok.reasoning_efforts.contains(&ReasoningEffort::None),
                "{id}"
            );
        }
        let grok = |id| find_for(ProviderKind::Xai, id).unwrap();
        assert_eq!(grok("grok-4.7").reasoning_efforts, EFFORT_LOW_TO_XHIGH);
        assert_eq!(grok("grok-4.6").reasoning_efforts, EFFORT_LOW_TO_XHIGH);
        // xAI's reasoning guide treats `xhigh` as `high` on Grok 4.5.
        assert_eq!(grok("grok-4.5").reasoning_efforts, EFFORT_LOW_TO_HIGH);
        // 4.6 is still the default, 4.7 is the general flagship, and 4.5
        // still backs the build alias.
        assert!(grok("grok-4.6").recommended);
        assert!(grok("grok-4.7").recommended);
        assert!(grok("grok-4.5").recommended);

        assert!(
            find_for(ProviderKind::Gemini, "gemini-3.5-flash")
                .unwrap()
                .recommended
        );
    }

    #[test]
    fn anthropic_reasoning_and_limit_metadata_is_model_specific() {
        let sonnet = find("claude-sonnet-5").unwrap();
        assert_eq!(sonnet.context_window, 1_000_000);
        assert_eq!(sonnet.max_output_tokens, 128_000);
        assert!(sonnet.supports_reasoning);
        assert_eq!(sonnet.reasoning_efforts, EFFORT_LOW_TO_MAX);

        // Haiku 4.5 needs the classic token-budget thinking request that the
        // adapter does not implement yet, so the catalog must not promise a
        // reasoning stream or an effort control.
        let haiku = find("claude-haiku-4-5-20251001").unwrap();
        assert_eq!(haiku.context_window, 200_000);
        assert_eq!(haiku.max_output_tokens, 64_000);
        assert!(!haiku.supports_reasoning);
        assert!(!haiku.supports_reasoning_effort());
        assert!(haiku.reasoning_efforts.is_empty());
    }

    #[test]
    fn anthropic_effort_catalogs_refuse_none() {
        for spec in
            models_for(ProviderKind::Anthropic).filter(|spec| spec.supports_reasoning_effort())
        {
            assert!(
                !spec.reasoning_efforts.contains(&ReasoningEffort::None),
                "{} offers an OpenAI-only level the Anthropic route rejects",
                spec.id
            );
        }
    }

    #[test]
    fn claude_4_6_effort_catalogs_exclude_xhigh() {
        for id in ["claude-opus-4-6", "claude-sonnet-4-6"] {
            let direct = find_for(ProviderKind::Anthropic, id).unwrap();

            assert_eq!(direct.reasoning_efforts, EFFORT_LOW_TO_HIGH_AND_MAX);
            assert!(!direct.reasoning_efforts.contains(&ReasoningEffort::XHigh));
            assert!(direct.reasoning_efforts.contains(&ReasoningEffort::Max));
        }

        // The generations that do accept `xhigh` keep advertising it.
        for id in [
            "claude-opus-5-5",
            "claude-opus-5",
            "claude-fable-5-1",
            "claude-fable-5",
            "claude-sonnet-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
        ] {
            assert!(
                find_for(ProviderKind::Anthropic, id)
                    .unwrap()
                    .reasoning_efforts
                    .contains(&ReasoningEffort::XHigh),
                "{id} lost xhigh while narrowing only Claude 4.6"
            );
        }
    }

    #[test]
    fn haiku_4_5_is_non_reasoning() {
        let spec = find_for(ProviderKind::Anthropic, "claude-haiku-4-5-20251001").unwrap();
        assert!(!spec.supports_reasoning);
        assert!(spec.reasoning_efforts.is_empty());
    }

    #[test]
    fn openai_chatgpt_auth_excludes_api_only_nano_and_keeps_a_usable_flagship() {
        let nano = find_for(ProviderKind::Openai, "gpt-5.4-nano").unwrap();
        assert!(
            !nano.supports_chatgpt_auth(),
            "gpt-5.4-nano is API-only; Codex rejects it under a ChatGPT account"
        );
        assert!(
            find_for(ProviderKind::Openai, "gpt-5.6-sol")
                .unwrap()
                .supports_chatgpt_auth(),
            "a ChatGPT-signed-in picker must still have a flagship to offer"
        );
        assert!(find_for(ProviderKind::Openai, "gpt-5.4-mini")
            .unwrap()
            .supports_chatgpt_auth());
        // Non-OpenAI rows are not gated by this stance.
        assert!(
            find_for(ProviderKind::Anthropic, "claude-haiku-4-5-20251001")
                .unwrap()
                .supports_chatgpt_auth()
        );
    }

    #[test]
    fn every_gpt_5_entry_reasons_on_a_caller_selected_effort() {
        let gpt_5: Vec<_> = models_for(ProviderKind::Openai)
            .filter(|spec| spec.id.starts_with("gpt-5"))
            .collect();
        assert!(!gpt_5.is_empty());
        for spec in gpt_5 {
            assert!(
                spec.supports_reasoning && spec.supports_reasoning_effort(),
                "{} is curated on the OpenAI route without the reasoning shape that route sends",
                spec.id
            );
            assert!(
                spec.reasoning_efforts.contains(&ReasoningEffort::None),
                "{} drops the level that turns GPT-5 reasoning off",
                spec.id
            );
            assert_eq!(
                spec.max_output_tokens, 128_000,
                "{} carries an output cap the GPT-5 line does not have",
                spec.id
            );
        }
        // `max` arrived with the 5.6 generation; the rows behind it stop at
        // `xhigh` and would reject it.
        assert_eq!(
            find("gpt-5.6-sol").unwrap().reasoning_efforts,
            EFFORT_NONE_TO_MAX
        );
        assert_eq!(
            find("gpt-5.5").unwrap().reasoning_efforts,
            EFFORT_NONE_TO_XHIGH
        );
        assert_eq!(
            find("gpt-5.4-mini").unwrap().reasoning_efforts,
            EFFORT_NONE_TO_XHIGH
        );
        assert_eq!(
            find("gpt-5.4-nano").unwrap().reasoning_efforts,
            EFFORT_NONE_TO_XHIGH
        );
    }

    #[test]
    fn gpt_6_astra_rejects_none_and_exposes_its_documented_contract() {
        let astra = find_for(ProviderKind::Openai, "gpt-6-astra").unwrap();

        assert_eq!(astra.verification, VerificationTier::Unverified);
        assert!(!astra.recommended);
        assert_eq!(astra.context_window, 1_050_000);
        assert_eq!(astra.max_output_tokens, 128_000);
        assert!(astra.accepts(InputModality::Text));
        assert!(astra.accepts(InputModality::Image));
        assert!(astra.supports_reasoning);
        assert!(astra.supports_tools());
        assert_eq!(astra.reasoning_efforts, EFFORT_LOW_TO_MAX);
        assert!(!astra.reasoning_efforts.contains(&ReasoningEffort::None));
    }

    #[test]
    fn no_pass_through_route_claims_the_vendor_web_search_tool() {
        for spec in MODEL_REGISTRY {
            assert!(
                !spec.supports_vendor_web_search
                    || !matches!(
                        spec.provider,
                        ProviderKind::Fireworks
                            | ProviderKind::Together
                            | ProviderKind::Openrouter
                            | ProviderKind::Ollama
                            | ProviderKind::OpenaiCompatible
                            | ProviderKind::ModelGateway
                    ),
                "{} claims a provider-executed web search under `{}`, whose endpoint Tidebreak cannot assume implements one",
                spec.id,
                spec.provider
            );
        }
    }

    #[test]
    fn no_openai_entry_claims_an_unenforceable_vendor_search_budget() {
        let openai: Vec<_> = models_for(ProviderKind::Openai).collect();
        assert!(!openai.is_empty());
        for spec in openai {
            assert!(
                !spec.supports_vendor_web_search,
                "{} claims vendor search even though the OpenAI route cannot enforce max_uses",
                spec.id
            );
        }
    }

    #[test]
    fn every_gemini_entry_has_the_native_adapter_contract() {
        let gemini: Vec<_> = models_for(ProviderKind::Gemini).collect();
        assert_eq!(gemini.len(), 6);
        for spec in gemini {
            assert_eq!(spec.context_window, 1_048_576, "{}", spec.id);
            assert_eq!(spec.max_output_tokens, 65_536, "{}", spec.id);
            assert!(spec.supports_reasoning, "{}", spec.id);
            if !GEMINI_WITHOUT_MINIMAL.contains(&spec.id) {
                assert_eq!(spec.reasoning_efforts, EFFORT_NONE_TO_HIGH, "{}", spec.id);
            }
        }
    }

    /// The Gemini rows whose endpoint returns an error for `minimal`.
    const GEMINI_WITHOUT_MINIMAL: &[&str] = &[
        "gemini-3.8-flash",
        "gemini-3.7-flash",
        "gemini-3.1-pro-preview",
    ];

    #[test]
    fn gemini_3_1_pro_and_flash_3_7_onward_exclude_minimal() {
        for id in GEMINI_WITHOUT_MINIMAL {
            let direct = find_for(ProviderKind::Gemini, id).unwrap();
            assert_eq!(direct.reasoning_efforts, EFFORT_LOW_TO_HIGH, "{id}");
            assert!(!direct.reasoning_efforts.contains(&ReasoningEffort::None));
        }
    }

    #[test]
    fn the_anthropic_default_row_stays_opus_5_ahead_of_fable() {
        // The first curated Anthropic row is what a fresh Anthropic-only install
        // resolves to, so Opus 5 keeps that place until the owner makes Opus
        // 5.5 the default. Fable is the demanding-work option, not the default,
        // and the current Fable sits ahead of the one it replaced.
        let ids: Vec<_> = models_for(ProviderKind::Anthropic)
            .map(|spec| spec.id)
            .collect();
        assert_eq!(
            &ids[..4],
            [
                "claude-opus-5",
                "claude-fable-5-1",
                "claude-fable-5",
                "claude-opus-5-5"
            ]
        );
        assert!(find("claude-opus-5").unwrap().recommended);
        assert!(find("claude-fable-5-1").unwrap().recommended);
        assert!(!find("claude-fable-5").unwrap().recommended);
        assert!(find("claude-opus-5-5").unwrap().recommended);
    }

    #[test]
    fn opus_5_5_carries_its_published_contract() {
        let opus = find_for(ProviderKind::Anthropic, "claude-opus-5-5").unwrap();
        assert_eq!(opus.verification, VerificationTier::Unverified);
        assert_eq!(opus.context_window, 1_000_000);
        assert_eq!(opus.max_output_tokens, 128_000);
        assert!(opus.accepts(InputModality::Image));
        assert!(opus.supports_reasoning);
        assert!(opus.supports_vendor_web_search);
        assert!(opus.supports_tools());
        assert!(opus.supports_structured_output());
        // All five documented levels; thinking is always on, so no `none`.
        assert_eq!(opus.reasoning_efforts, EFFORT_LOW_TO_MAX);
    }

    #[test]
    fn gpt_6_sol_and_luna_follow_the_default_with_the_published_contract() {
        let ids: Vec<_> = models_for(ProviderKind::Openai)
            .map(|spec| spec.id)
            .collect();
        // The default stays first; GPT-6 Sol is the next flagship row.
        assert_eq!(&ids[..2], ["gpt-5.6-sol", "gpt-6-sol"]);
        for id in ["gpt-6-sol", "gpt-6-luna"] {
            let gpt = find_for(ProviderKind::Openai, id).unwrap();
            assert_eq!(gpt.verification, VerificationTier::Unverified, "{id}");
            assert_eq!(gpt.context_window, 1_050_000, "{id}");
            assert_eq!(gpt.max_output_tokens, 128_000, "{id}");
            assert!(gpt.accepts(InputModality::Image), "{id}");
            // Function calling is limited to `none` only on Chat Completions;
            // this route speaks Responses.
            assert!(gpt.supports_tools(), "{id}");
            assert!(gpt.supports_chatgpt_auth(), "{id}");
            assert_eq!(gpt.reasoning_efforts, EFFORT_NONE_TO_MAX, "{id}");
        }
        assert!(find("gpt-6-sol").unwrap().recommended);
        // The cost tier is not shown by default.
        assert!(!find("gpt-6-luna").unwrap().recommended);
        assert!(find_for(ProviderKind::Openai, "gpt-6-terra").is_none());
    }

    #[test]
    fn hosted_compatible_catalogs_keep_provider_specific_ids_and_capabilities() {
        // One family, two hosts, two sets of published limits and controls.
        let fireworks_glm =
            find_for(ProviderKind::Fireworks, "accounts/fireworks/models/glm-5p3").unwrap();
        let together_glm = find_for(ProviderKind::Together, "zai-org/GLM-5.3").unwrap();
        assert_eq!(fireworks_glm.context_window, 1_040_000);
        assert_eq!(together_glm.context_window, 1_048_575);
        assert!(!fireworks_glm.accepts(InputModality::Image));
        assert!(!together_glm.accepts(InputModality::Image));
        assert!(fireworks_glm.supports_tools());
        assert!(together_glm.supports_tools());
        assert!(!fireworks_glm.supports_reasoning);
        assert!(together_glm.supports_reasoning);
        assert_eq!(together_glm.reasoning_efforts, EFFORT_LOW_TO_HIGH_AND_MAX);

        for id in [
            "accounts/fireworks/models/kimi-k3",
            "accounts/fireworks/models/glm-5p3-flash",
            "accounts/fireworks/models/qwen3p8-max",
            "accounts/fireworks/models/minimax-m3",
            "accounts/fireworks/models/deepseek-v4p1-flash",
            "accounts/fireworks/models/inkling",
        ] {
            assert!(
                find_for(ProviderKind::Fireworks, id)
                    .unwrap()
                    .accepts(InputModality::Image),
                "Fireworks publishes image input for {id}",
            );
        }
        for id in [
            "moonshotai/Kimi-K3",
            "MiniMaxAI/MiniMax-M3",
            "thinkingmachines/Inkling",
            "Qwen/Qwen3.7-Plus",
        ] {
            assert!(
                find_for(ProviderKind::Together, id)
                    .unwrap()
                    .accepts(InputModality::Image),
                "Together publishes image input for {id}",
            );
        }
        // Together's serverless vision table does not list these.
        for id in ["zai-org/GLM-5.3-Flash", "deepseek-ai/DeepSeek-V4.1-Flash"] {
            assert!(
                !find_for(ProviderKind::Together, id)
                    .unwrap()
                    .accepts(InputModality::Image),
                "{id} is text only on Together",
            );
        }

        // Fireworks documents no effort values for these, so they offer none,
        // and it documents no reasoning for MiniMax M3 at all.
        for id in [
            "accounts/fireworks/models/qwen3p8-max",
            "accounts/fireworks/models/deepseek-v4p1-flash",
        ] {
            let model = find_for(ProviderKind::Fireworks, id).unwrap();
            assert!(model.supports_reasoning, "{id}");
            assert!(model.reasoning_efforts.is_empty(), "{id}");
        }
        assert!(
            !find_for(
                ProviderKind::Fireworks,
                "accounts/fireworks/models/minimax-m3"
            )
            .unwrap()
            .supports_reasoning
        );

        // DeepSeek continues a tool call only with its own reasoning trace
        // sent back, so its rows stay chat-only until that path is exercised.
        // Strict utility responses do not depend on it.
        for (provider, id) in [
            (
                ProviderKind::Fireworks,
                "accounts/fireworks/models/deepseek-v4p1-flash",
            ),
            (ProviderKind::Together, "deepseek-ai/DeepSeek-V4-Pro-0813"),
            (ProviderKind::Together, "deepseek-ai/DeepSeek-V4.1-Flash"),
            (ProviderKind::Together, "deepseek-ai/DeepSeek-V4-Flash-0731"),
        ] {
            let model = find_for(provider, id).unwrap();
            assert!(
                !model.supports_tools(),
                "{provider} `{id}` stays chat-only until reasoning content can be replayed",
            );
            assert!(
                model.supports_structured_output(),
                "chat-only tool routing must not disable strict utility responses for {provider} `{id}`",
            );
            assert!(model.supports_reasoning, "{provider} `{id}` reasons");
        }
        assert_eq!(
            find_for(ProviderKind::Together, "deepseek-ai/DeepSeek-V4-Pro-0813")
                .unwrap()
                .reasoning_efforts,
            EFFORT_HIGH_AND_MAX,
        );
        // V4.1 Flash on Together takes an integer effort this route never sends.
        assert!(
            find_for(ProviderKind::Together, "deepseek-ai/DeepSeek-V4.1-Flash")
                .unwrap()
                .reasoning_efforts
                .is_empty()
        );

        for id in [
            "moonshotai/Kimi-K3",
            "zai-org/GLM-5.3",
            "zai-org/GLM-5.3-Flash",
            "zai-org/GLM-5.2",
            "MiniMaxAI/MiniMax-M3",
            "thinkingmachines/Inkling",
        ] {
            assert!(
                find_for(ProviderKind::Together, id)
                    .unwrap()
                    .supports_structured_output(),
                "Together documents strict structured output for `{id}`",
            );
        }
        // Together's catalog leaves both columns blank for these.
        for id in ["Qwen/Qwen3.7-Max", "Qwen/Qwen3.7-Plus"] {
            let model = find_for(ProviderKind::Together, id).unwrap();
            assert!(!model.supports_tools(), "{id}");
            assert!(!model.supports_structured_output(), "{id}");
        }

        for (provider, id) in [
            (ProviderKind::Fireworks, "accounts/fireworks/models/kimi-k3"),
            (ProviderKind::Together, "moonshotai/Kimi-K3"),
        ] {
            let model = find_for(provider, id).unwrap();
            assert!(model.supports_tools(), "{provider} `{id}` supports tools");
            assert!(model.supports_reasoning, "{provider} `{id}` reasons");
            assert_eq!(
                model.reasoning_efforts, EFFORT_LOW_HIGH_MAX,
                "{provider} `{id}` exposes only its documented effort scale",
            );
        }

        // Fireworks retires these on 2026-09-25. Until then they stay
        // selectable, out of the default view.
        for id in [
            "accounts/fireworks/models/kimi-k2p7-code",
            "accounts/fireworks/models/kimi-k2p6",
            "accounts/fireworks/models/glm-5p2",
        ] {
            let model = find_for(ProviderKind::Fireworks, id).unwrap();
            assert!(!model.recommended, "{id}");
        }
        assert!(
            !find_for(ProviderKind::Fireworks, "accounts/fireworks/models/glm-5p2")
                .unwrap()
                .supports_tools()
        );

        // Rows the hosts no longer serve are gone, not hidden.
        for (provider, id) in [
            (
                ProviderKind::Fireworks,
                "accounts/fireworks/models/qwen3p7-plus",
            ),
            (
                ProviderKind::Fireworks,
                "accounts/fireworks/models/deepseek-v4-flash",
            ),
            (
                ProviderKind::Fireworks,
                "accounts/fireworks/models/deepseek-v4-pro",
            ),
            (ProviderKind::Together, "moonshotai/Kimi-K2.7-Code"),
            (ProviderKind::Together, "moonshotai/Kimi-K2.6"),
            (ProviderKind::Together, "deepseek-ai/DeepSeek-V4-Pro"),
            (ProviderKind::Together, "nvidia/nemotron-3-ultra-550b-a55b"),
            (ProviderKind::Together, "google/gemma-4-31B-it"),
            (ProviderKind::Together, "pearl-ai/gemma-4-31b-it"),
            (ProviderKind::Together, "thinkingmachines/Inkling-Small"),
        ] {
            assert!(find_for(provider, id).is_none(), "{provider} `{id}`");
        }

        assert_eq!(models_for(ProviderKind::Fireworks).count(), 11);
        assert_eq!(models_for(ProviderKind::Together).count(), 11);
        assert!(models_for(ProviderKind::Fireworks)
            .chain(models_for(ProviderKind::Together))
            .all(|model| model.verification == VerificationTier::Unverified));
    }

    #[test]
    fn display_name_prefers_registry_and_falls_back_for_unknown_ids() {
        assert_eq!(display_name_for("claude-opus-4-8"), "Claude Opus 4.8");
        assert_eq!(display_name_for("gpt-5.6-sol"), "GPT-5.6 Sol");
        assert_eq!(display_name_for("gpt-4o-mini"), "GPT 4o Mini");
        assert_eq!(
            display_name_for("claude-sonnet-9-20260101"),
            "Claude Sonnet 9"
        );
        assert_eq!(display_name_for("gpt-6-2026-01-01"), "GPT 6");
        assert_eq!(display_name_for("local-model"), "Local Model");
    }

    #[test]
    fn selection_keys_are_provider_scoped_and_legacy_ids_migrate_losslessly() {
        let key = selection_key(ProviderKind::Openai, "gpt-5.6-sol");
        assert_eq!(key, "openai::gpt-5.6-sol");
        assert_eq!(
            parse_selection_key(&key),
            Some((ProviderKind::Openai, "gpt-5.6-sol"))
        );
        assert_eq!(
            parse_selection_key("openai_compatible::vendor::model"),
            Some((ProviderKind::OpenaiCompatible, "vendor::model"))
        );
        assert_eq!(
            migrate_curated_selection("gpt-5.6-sol").as_deref(),
            Some("openai::gpt-5.6-sol")
        );
        assert!(migrate_curated_selection("anthropic::gpt-5.6-sol").is_none());
        assert!(parse_selection_key("unknown::gpt-5.6-sol").is_none());
        // A retired curated id no longer resolves; the picker surfaces it as
        // unavailable rather than silently routing it somewhere else.
        assert!(migrate_curated_selection("gpt-4o").is_none());
    }

    /// `ModelInfo.input_modalities` used to be a hand-built `Vec<String>` filled
    /// from `as_str`; it now carries the enum so the generated TypeScript is a
    /// union rather than `Array<string>`. That is only safe if serde produces the
    /// same strings, so this pins the two together instead of assuming.
    ///
    /// It is also what keeps `as_str` honest: with serde owning the wire form,
    /// an unchecked second spelling of it would be exactly the kind of duplicate
    /// that drifts.
    #[test]
    fn a_modality_serializes_as_the_string_it_has_always_been() {
        for modality in [InputModality::Text, InputModality::Image] {
            assert_eq!(
                serde_json::to_value(modality).expect("a modality serializes"),
                serde_json::json!(modality.as_str()),
                "{modality:?} changed its wire spelling"
            );
        }
    }
}
