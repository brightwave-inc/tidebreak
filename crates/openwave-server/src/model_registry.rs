//! Host-owned metadata for the models OpenWave curates.
//!
//! This is the single source of truth for both the public model catalog and
//! provider routing. Provider configuration decides which registry entries are
//! available; it does not duplicate model ids or capabilities.

use openwave_core::ReasoningEffort;

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

/// How thoroughly OpenWave has exercised a model's agent-facing behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum VerificationTier {
    /// Tool-calling and streaming have been exercised end to end.
    Verified,
    /// The model is selectable, but OpenWave has not verified those contracts.
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
/// Gemini 3 Flash maps OpenWave's `none` to its `minimal` thinking level and
/// has no separate levels above `high`.
const EFFORT_NONE_TO_HIGH: &[ReasoningEffort] = &[
    ReasoningEffort::None,
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
];
/// Gemini 3.1 Pro Preview starts at `low`; it does not accept `minimal`.
const EFFORT_LOW_TO_HIGH: &[ReasoningEffort] = &[
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
];
const EFFORT_LOW_TO_MAX: &[ReasoningEffort] = &[
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::XHigh,
    ReasoningEffort::Max,
];
/// Claude Opus and Sonnet 4.6 accept `max`, but not the newer `xhigh` level.
const EFFORT_LOW_TO_HIGH_AND_MAX: &[ReasoningEffort] = &[
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::Max,
];
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
    /// Maximum context window in tokens.
    pub context_window: u32,
    /// Maximum model output in tokens.
    pub max_output_tokens: u32,
    /// Input modalities accepted by the model.
    pub input_modalities: &'static [InputModality],
    /// Whether the model can produce an internal reasoning stream.
    pub supports_reasoning: bool,
    /// Whether a turn on this model may enable the provider's own server-side
    /// web search tool instead of OpenWave's client-side one.
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
    pub fn vendor(&self) -> Option<ProviderKind> {
        if self.provider != ProviderKind::Vertex {
            return None;
        }
        if self.id.starts_with("gemini-") {
            Some(ProviderKind::Gemini)
        } else if self.id.starts_with("claude-") {
            Some(ProviderKind::Anthropic)
        } else {
            None
        }
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
}

/// Curated models in picker display order: each provider's current generation
/// first, then the earlier generations a chat can pin when the newest one
/// regresses on its workload.
const MODEL_REGISTRY: &[ModelSpec] = &[
    ModelSpec {
        id: "claude-opus-5",
        display_name: "Claude Opus 5",
        provider: ProviderKind::Anthropic,
        verification: VerificationTier::Verified,
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
    // Second, not first: the built-in default is this provider's first curated
    // row, and the default stays Opus 5.
    ModelSpec {
        id: "claude-fable-5",
        display_name: "Claude Fable 5",
        provider: ProviderKind::Anthropic,
        verification: VerificationTier::Verified,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: true,
        // Same adaptive-thinking generation as Opus 5 and Sonnet 5.
        reasoning_efforts: EFFORT_LOW_TO_MAX,
    },
    ModelSpec {
        id: "claude-sonnet-5",
        display_name: "Claude Sonnet 5",
        provider: ProviderKind::Anthropic,
        verification: VerificationTier::Verified,
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
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: true,
        reasoning_efforts: EFFORT_LOW_TO_HIGH_AND_MAX,
    },
    ModelSpec {
        id: "gpt-5.6-sol",
        display_name: "GPT-5.6 Sol",
        provider: ProviderKind::Openai,
        verification: VerificationTier::Verified,
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: true,
        // The whole GPT-5 line reasons on a caller-selected effort, which the
        // OpenAI-compatible adapter already sends alongside
        // `max_completion_tokens`. Only the 5.6 generation added `max`.
        reasoning_efforts: EFFORT_NONE_TO_MAX,
    },
    ModelSpec {
        id: "gpt-5.6-terra",
        display_name: "GPT-5.6 Terra",
        provider: ProviderKind::Openai,
        verification: VerificationTier::Verified,
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: true,
        reasoning_efforts: EFFORT_NONE_TO_MAX,
    },
    ModelSpec {
        id: "gpt-5.6-luna",
        display_name: "GPT-5.6 Luna",
        provider: ProviderKind::Openai,
        verification: VerificationTier::Verified,
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: true,
        reasoning_efforts: EFFORT_NONE_TO_MAX,
    },
    ModelSpec {
        id: "gpt-5.5",
        display_name: "GPT-5.5",
        provider: ProviderKind::Openai,
        verification: VerificationTier::Verified,
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: true,
        reasoning_efforts: EFFORT_NONE_TO_XHIGH,
    },
    ModelSpec {
        id: "gpt-5.4-mini",
        display_name: "GPT-5.4 mini",
        provider: ProviderKind::Openai,
        verification: VerificationTier::Verified,
        context_window: 400_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: true,
        reasoning_efforts: EFFORT_NONE_TO_XHIGH,
    },
    ModelSpec {
        id: "gpt-5.4-nano",
        display_name: "GPT-5.4 nano",
        provider: ProviderKind::Openai,
        verification: VerificationTier::Verified,
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
    // Gemini rows are intentionally limited to ids currently published by
    // Google. All four accept images, expose thinking levels through `high`,
    // and have 1,048,576 input / 65,536 output token limits; the Flash rows
    // start at `minimal`, while Pro Preview starts at `low`. The native adapter
    // owns the corresponding GenerateContent wire shape.
    ModelSpec {
        id: "gemini-3.6-flash",
        display_name: "Gemini 3.6 Flash",
        provider: ProviderKind::Gemini,
        verification: VerificationTier::Verified,
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
        context_window: 1_048_576,
        max_output_tokens: 65_536,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_TO_HIGH,
    },
    // Vertex is one explicit serving provider with two native protocol
    // families. These rows intentionally mirror only models in the current
    // direct catalogs above: provider-qualified keys keep routing unambiguous,
    // while the vendor projection keeps icons and legacy bare-id migration on
    // the original direct provider. Request-shape fixtures cover both Vertex
    // protocols, but the live provider/model combinations remain unverified.
    ModelSpec {
        id: "gemini-3.6-flash",
        display_name: "Gemini 3.6 Flash",
        provider: ProviderKind::Vertex,
        verification: VerificationTier::Unverified,
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
        provider: ProviderKind::Vertex,
        verification: VerificationTier::Unverified,
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
        provider: ProviderKind::Vertex,
        verification: VerificationTier::Unverified,
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
        provider: ProviderKind::Vertex,
        verification: VerificationTier::Unverified,
        context_window: 1_048_576,
        max_output_tokens: 65_536,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_TO_HIGH,
    },
    ModelSpec {
        id: "claude-opus-5",
        display_name: "Claude Opus 5",
        provider: ProviderKind::Vertex,
        verification: VerificationTier::Unverified,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_TO_MAX,
    },
    ModelSpec {
        id: "claude-fable-5",
        display_name: "Claude Fable 5",
        provider: ProviderKind::Vertex,
        verification: VerificationTier::Unverified,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_TO_MAX,
    },
    ModelSpec {
        id: "claude-sonnet-5",
        display_name: "Claude Sonnet 5",
        provider: ProviderKind::Vertex,
        verification: VerificationTier::Unverified,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_TO_MAX,
    },
    ModelSpec {
        id: "claude-opus-4-8",
        display_name: "Claude Opus 4.8",
        provider: ProviderKind::Vertex,
        verification: VerificationTier::Unverified,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_TO_MAX,
    },
    ModelSpec {
        id: "claude-opus-4-7",
        display_name: "Claude Opus 4.7",
        provider: ProviderKind::Vertex,
        verification: VerificationTier::Unverified,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_TO_MAX,
    },
    ModelSpec {
        id: "claude-opus-4-6",
        display_name: "Claude Opus 4.6",
        provider: ProviderKind::Vertex,
        verification: VerificationTier::Unverified,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_TO_HIGH_AND_MAX,
    },
    ModelSpec {
        id: "claude-sonnet-4-6",
        display_name: "Claude Sonnet 4.6",
        provider: ProviderKind::Vertex,
        verification: VerificationTier::Unverified,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_vendor_web_search: false,
        reasoning_efforts: EFFORT_LOW_TO_HIGH_AND_MAX,
    },
    ModelSpec {
        id: "claude-haiku-4-5",
        display_name: "Claude Haiku 4.5",
        provider: ProviderKind::Vertex,
        verification: VerificationTier::Unverified,
        context_window: 200_000,
        max_output_tokens: 64_000,
        input_modalities: TEXT_AND_IMAGE,
        // The Vertex route speaks the same adapter request shape as direct
        // Anthropic, so it cannot claim classic extended thinking either.
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

    /// Whether OpenWave can actually put an image on the wire for `provider`.
    ///
    /// Provider documentation alone does not earn a model `InputModality::Image`.
    /// Each provider has its own request adapter, and a custom compatible
    /// endpoint is not known to support the image branch at all.
    const fn provider_carries_image_input(provider: ProviderKind) -> bool {
        match provider {
            ProviderKind::Anthropic => true,
            ProviderKind::Openai => true,
            ProviderKind::Xai => true,
            ProviderKind::Gemini => true,
            ProviderKind::Vertex => true,
            ProviderKind::OpenaiCompatible => false,
            ProviderKind::ModelGateway => true,
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
        assert_eq!(spec.provider, ProviderKind::Anthropic);
        // The default has to be the current generation, not a pin that happened
        // to be current when it was written down.
        assert_eq!(
            models_for(ProviderKind::Anthropic).next().map(|s| s.id),
            Some(crate::DEFAULT_MODEL),
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
    fn claude_4_6_effort_catalogs_match_on_direct_and_vertex() {
        for id in ["claude-opus-4-6", "claude-sonnet-4-6"] {
            let direct = find_for(ProviderKind::Anthropic, id).unwrap();
            let vertex = find_for(ProviderKind::Vertex, id).unwrap();

            assert_eq!(direct.reasoning_efforts, EFFORT_LOW_TO_HIGH_AND_MAX);
            assert_eq!(vertex.reasoning_efforts, direct.reasoning_efforts);
            assert!(!direct.reasoning_efforts.contains(&ReasoningEffort::XHigh));
            assert!(direct.reasoning_efforts.contains(&ReasoningEffort::Max));
        }

        // The generations that do accept `xhigh` keep advertising it.
        for id in [
            "claude-opus-5",
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
    fn haiku_4_5_is_non_reasoning_on_direct_and_vertex() {
        for (provider, id) in [
            (ProviderKind::Anthropic, "claude-haiku-4-5-20251001"),
            (ProviderKind::Vertex, "claude-haiku-4-5"),
        ] {
            let spec = find_for(provider, id).unwrap();
            assert!(!spec.supports_reasoning, "{}::{id}", provider.as_str());
            assert!(
                spec.reasoning_efforts.is_empty(),
                "{}::{id}",
                provider.as_str()
            );
        }
    }

    #[test]
    fn every_openai_entry_reasons_on_a_caller_selected_effort() {
        let openai: Vec<_> = models_for(ProviderKind::Openai).collect();
        assert!(!openai.is_empty());
        for spec in openai {
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
    fn no_pass_through_route_claims_the_vendor_web_search_tool() {
        for spec in MODEL_REGISTRY {
            assert!(
                !spec.supports_vendor_web_search
                    || !matches!(
                        spec.provider,
                        ProviderKind::OpenaiCompatible | ProviderKind::ModelGateway
                    ),
                "{} claims a provider-executed web search under `{}`, whose endpoint OpenWave cannot assume implements one",
                spec.id,
                spec.provider
            );
        }
    }

    #[test]
    fn every_gemini_entry_has_the_native_adapter_contract() {
        let gemini: Vec<_> = models_for(ProviderKind::Gemini).collect();
        assert_eq!(gemini.len(), 4);
        for spec in gemini {
            assert_eq!(spec.context_window, 1_048_576, "{}", spec.id);
            assert_eq!(spec.max_output_tokens, 65_536, "{}", spec.id);
            assert!(spec.supports_reasoning, "{}", spec.id);
            if spec.id != "gemini-3.1-pro-preview" {
                assert_eq!(spec.reasoning_efforts, EFFORT_NONE_TO_HIGH, "{}", spec.id);
            }
        }
    }

    #[test]
    fn gemini_3_1_pro_excludes_minimal_on_direct_and_vertex() {
        let direct = find_for(ProviderKind::Gemini, "gemini-3.1-pro-preview").unwrap();
        let vertex = find_for(ProviderKind::Vertex, "gemini-3.1-pro-preview").unwrap();

        assert_eq!(direct.reasoning_efforts, EFFORT_LOW_TO_HIGH);
        assert_eq!(vertex.reasoning_efforts, direct.reasoning_efforts);
        assert!(!direct.reasoning_efforts.contains(&ReasoningEffort::None));
    }

    #[test]
    fn vertex_rows_are_curated_as_two_native_unverified_families() {
        let vertex: Vec<_> = models_for(ProviderKind::Vertex).collect();
        assert_eq!(vertex.len(), 12);
        assert_eq!(
            vertex
                .iter()
                .filter(|spec| spec.vendor() == Some(ProviderKind::Gemini))
                .count(),
            4
        );
        assert_eq!(
            vertex
                .iter()
                .filter(|spec| spec.vendor() == Some(ProviderKind::Anthropic))
                .count(),
            8
        );
        for spec in vertex {
            assert_eq!(spec.verification, VerificationTier::Unverified);
            assert!(!spec.supports_vendor_web_search);
            assert!(spec.vendor().is_some(), "{} has no Vertex family", spec.id);
        }
        assert_eq!(
            find("claude-opus-5").map(|spec| spec.provider),
            Some(ProviderKind::Anthropic),
            "legacy bare ids stay on their direct provider"
        );
        assert_eq!(
            find("gemini-3.6-flash").map(|spec| spec.provider),
            Some(ProviderKind::Gemini)
        );
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
