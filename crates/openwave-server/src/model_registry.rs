//! Host-owned metadata for the models OpenWave curates.
//!
//! This is the single source of truth for both the public model catalog and
//! provider routing. Provider configuration decides which registry entries are
//! available; it does not duplicate model ids or capabilities.

use crate::providers::ProviderKind;

/// An input modality a model accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputModality {
    /// Plain text and structured text content.
    Text,
    /// Image content alongside text.
    Image,
}

impl InputModality {
    /// Stable wire representation used by `GET /models`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
        }
    }
}

const TEXT_AND_IMAGE: &[InputModality] = &[InputModality::Text, InputModality::Image];

/// Capability and presentation metadata for a curated model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    /// Identifier passed to the provider and stored as `chat.model`.
    pub id: &'static str,
    /// Human-readable label for model selectors.
    pub display_name: &'static str,
    /// Provider that serves the model.
    pub provider: ProviderKind,
    /// Maximum context window in tokens.
    pub context_window: u32,
    /// Maximum model output in tokens.
    pub max_output_tokens: u32,
    /// Input modalities accepted by the model.
    pub input_modalities: &'static [InputModality],
    /// Whether the model can produce an internal reasoning stream.
    pub supports_reasoning: bool,
    /// Whether callers can choose a reasoning-effort level.
    pub supports_reasoning_effort: bool,
}

impl ModelSpec {
    /// Whether this model accepts `modality`.
    pub fn accepts(&self, modality: InputModality) -> bool {
        self.input_modalities.contains(&modality)
    }
}

/// Curated models in picker display order.
const MODEL_REGISTRY: &[ModelSpec] = &[
    ModelSpec {
        id: "claude-opus-4-8",
        display_name: "Claude Opus 4.8",
        provider: ProviderKind::Anthropic,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        // The Anthropic adapter can stream thinking blocks, but it does not
        // currently send a caller-selected effort or thinking budget.
        supports_reasoning_effort: false,
    },
    ModelSpec {
        id: "claude-sonnet-5",
        display_name: "Claude Sonnet 5",
        provider: ProviderKind::Anthropic,
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_reasoning_effort: false,
    },
    ModelSpec {
        id: "claude-haiku-4-5-20251001",
        display_name: "Claude Haiku 4.5",
        provider: ProviderKind::Anthropic,
        context_window: 200_000,
        max_output_tokens: 64_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_reasoning_effort: false,
    },
    ModelSpec {
        id: "gpt-4o",
        display_name: "GPT-4o",
        provider: ProviderKind::Openai,
        context_window: 128_000,
        max_output_tokens: 16_384,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: false,
        supports_reasoning_effort: false,
    },
    ModelSpec {
        id: "gpt-4o-mini",
        display_name: "GPT-4o mini",
        provider: ProviderKind::Openai,
        context_window: 128_000,
        max_output_tokens: 16_384,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: false,
        supports_reasoning_effort: false,
    },
    ModelSpec {
        id: "o3",
        display_name: "o3",
        provider: ProviderKind::Openai,
        context_window: 200_000,
        max_output_tokens: 100_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_reasoning_effort: true,
    },
    ModelSpec {
        id: "o4-mini",
        display_name: "o4-mini",
        provider: ProviderKind::Openai,
        context_window: 200_000,
        max_output_tokens: 100_000,
        input_modalities: TEXT_AND_IMAGE,
        supports_reasoning: true,
        supports_reasoning_effort: true,
    },
];

/// Curated entries belonging to `provider`, preserving registry display order.
pub fn models_for(provider: ProviderKind) -> impl Iterator<Item = &'static ModelSpec> + Clone {
    MODEL_REGISTRY
        .iter()
        .filter(move |spec| spec.provider == provider)
}

/// Find an exact curated model id.
pub fn find(id: &str) -> Option<&'static ModelSpec> {
    MODEL_REGISTRY.iter().find(|spec| spec.id == id)
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

    #[test]
    fn registry_ids_are_unique_and_capabilities_are_well_formed() {
        let mut ids = HashSet::new();
        for spec in MODEL_REGISTRY {
            assert!(ids.insert(spec.id), "duplicate model id {}", spec.id);
            assert!(spec.context_window > 0);
            assert!(spec.max_output_tokens > 0);
            assert!(spec.context_window >= spec.max_output_tokens);
            assert!(spec.accepts(InputModality::Text));
            assert!(
                !spec.supports_reasoning_effort || spec.supports_reasoning,
                "{} exposes reasoning effort without reasoning",
                spec.id
            );
        }
    }

    #[test]
    fn providers_receive_only_their_registry_entries() {
        for &provider in ProviderKind::ALL {
            assert!(models_for(provider).all(|spec| spec.provider == provider));
        }
        assert_eq!(models_for(ProviderKind::Anthropic).count(), 3);
        assert_eq!(models_for(ProviderKind::Openai).count(), 4);
        assert_eq!(models_for(ProviderKind::OpenaiCompatible).count(), 0);
    }

    #[test]
    fn anthropic_reasoning_and_limit_metadata_is_model_specific() {
        let sonnet = find("claude-sonnet-5").unwrap();
        assert_eq!(sonnet.context_window, 1_000_000);
        assert_eq!(sonnet.max_output_tokens, 128_000);
        assert!(sonnet.supports_reasoning);
        assert!(!sonnet.supports_reasoning_effort);

        let haiku = find("claude-haiku-4-5-20251001").unwrap();
        assert_eq!(haiku.context_window, 200_000);
        assert_eq!(haiku.max_output_tokens, 64_000);
        assert!(haiku.supports_reasoning);
        assert!(!haiku.supports_reasoning_effort);
    }

    #[test]
    fn display_name_prefers_registry_and_falls_back_for_unknown_ids() {
        assert_eq!(display_name_for("claude-opus-4-8"), "Claude Opus 4.8");
        assert_eq!(display_name_for("gpt-4o-mini"), "GPT-4o mini");
        assert_eq!(
            display_name_for("claude-sonnet-9-20260101"),
            "Claude Sonnet 9"
        );
        assert_eq!(display_name_for("gpt-6-2026-01-01"), "GPT 6");
        assert_eq!(display_name_for("local-model"), "Local Model");
    }
}
