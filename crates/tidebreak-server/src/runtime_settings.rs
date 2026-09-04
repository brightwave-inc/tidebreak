//! Stored runtime policy shared by route handlers and background workers.

use serde::{Deserialize, Serialize};
use tidebreak_core::{
    AgentRun, CompactionPolicy, PromptCacheRetention, Store,
    DEFAULT_COMPACTION_MIN_THRESHOLD_TOKENS, DEFAULT_COMPACTION_PROTECT_RECENT_MESSAGES,
    DEFAULT_COMPACTION_TARGET_FRACTION, DEFAULT_COMPACTION_THRESHOLD_FRACTION,
};

use crate::model_roles::{self, ModelRole};

pub const MAX_ACTIVE_BACKGROUND_AGENTS_SETTING: &str = "agents.max_active_background_agents";
pub const SANDBOX_AGENT_CHECKIN_STEPS_SETTING: &str = "agents.sandbox_agent_checkin_steps";
pub const SANDBOX_AGENT_ERROR_CHECKIN_SETTING: &str = "agents.sandbox_agent_error_checkin";
pub const COMPACTION_THRESHOLD_FRACTION_SETTING: &str = "compaction.threshold_fraction";
pub const COMPACTION_TARGET_FRACTION_SETTING: &str = "compaction.target_fraction";
pub const COMPACTION_MIN_THRESHOLD_TOKENS_SETTING: &str = "compaction.min_threshold_tokens";
pub const COMPACTION_PROTECT_RECENT_MESSAGES_SETTING: &str = "compaction.protect_recent_messages";
pub const MODEL_VISIBILITY_OVERRIDES_SETTING: &str = "models.visibility_overrides";
pub const PROMPT_CACHE_RETENTION_SETTING: &str = "models.prompt_cache_retention";
pub const COMPUTER_USE_ENABLED_SETTING: &str = "computer_use.enabled";
pub const MEMORY_ENABLED_SETTING: &str = "memory.enabled";
pub const MEMORY_CAPTURE_ENABLED_SETTING: &str = "memory.capture.enabled";

pub const MAX_SANDBOX_AGENT_CHECKIN_STEPS: u32 = 1_000;
pub const MAX_SANDBOX_AGENT_ERROR_CHECKIN: u32 = 100;
const MIN_COMPACTION_MIN_THRESHOLD_TOKENS: u32 = 1_000;
const MAX_COMPACTION_MIN_THRESHOLD_TOKENS: u32 = 2_000_000;

/// Host-tunable chat compaction cadence and retention.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct CompactionSettings {
    /// Compact when unabridged tokens exceed this fraction of the context window.
    pub threshold_fraction: f64,
    /// After compaction, keep about this fraction of the window as raw recent history.
    pub target_fraction: f64,
    /// Absolute floor applied before scaling by context window.
    pub min_threshold_tokens: u32,
    /// Newest durable messages that must never enter the compacted prefix.
    pub protect_recent_messages: u32,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            threshold_fraction: DEFAULT_COMPACTION_THRESHOLD_FRACTION,
            target_fraction: DEFAULT_COMPACTION_TARGET_FRACTION,
            min_threshold_tokens: DEFAULT_COMPACTION_MIN_THRESHOLD_TOKENS as u32,
            protect_recent_messages: DEFAULT_COMPACTION_PROTECT_RECENT_MESSAGES as u32,
        }
    }
}

impl From<&CompactionSettings> for CompactionPolicy {
    fn from(settings: &CompactionSettings) -> Self {
        Self {
            threshold_fraction: settings.threshold_fraction,
            target_fraction: settings.target_fraction,
            min_threshold_tokens: settings.min_threshold_tokens as usize,
            protect_recent_messages: settings.protect_recent_messages as usize,
        }
    }
}

pub async fn read_prompt_cache_retention(
    store: &dyn Store,
) -> tidebreak_core::Result<PromptCacheRetention> {
    Ok(store
        .get_setting(PROMPT_CACHE_RETENTION_SETTING)
        .await?
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default())
}

pub async fn chat_role_model(
    store: &dyn Store,
    boot_default: &str,
) -> tidebreak_core::Result<String> {
    Ok(model_roles::read_selection(store, ModelRole::Chat)
        .await?
        .unwrap_or_else(|| boot_default.to_owned()))
}

pub async fn resolve_chat_model(
    store: &dyn Store,
    chat: &tidebreak_core::Chat,
    boot_default: &str,
) -> tidebreak_core::Result<String> {
    match chat.model.clone() {
        Some(model) => Ok(model),
        None => chat_role_model(store, boot_default).await,
    }
}

pub async fn read_max_active_background_agents(store: &dyn Store) -> tidebreak_core::Result<u32> {
    Ok(store
        .get_setting(MAX_ACTIVE_BACKGROUND_AGENTS_SETTING)
        .await?
        .and_then(|value| serde_json::from_value::<u32>(value).ok())
        .filter(|limit| *limit > 0 && *limit <= AgentRun::MAX_CONCURRENCY_LIMIT)
        .unwrap_or(AgentRun::DEFAULT_MAX_ACTIVE_BACKGROUND_AGENTS))
}

pub async fn read_sandbox_agent_checkin_steps(store: &dyn Store) -> tidebreak_core::Result<u32> {
    Ok(read_sandbox_agent_checkin_steps_override(store)
        .await?
        .unwrap_or(tidebreak_core::DEFAULT_SANDBOX_AGENT_CHECKIN_STEPS as u32))
}

pub async fn read_sandbox_agent_checkin_steps_override(
    store: &dyn Store,
) -> tidebreak_core::Result<Option<u32>> {
    Ok(store
        .get_setting(SANDBOX_AGENT_CHECKIN_STEPS_SETTING)
        .await?
        .and_then(|value| serde_json::from_value::<u32>(value).ok())
        .filter(|steps| *steps > 0 && *steps <= MAX_SANDBOX_AGENT_CHECKIN_STEPS))
}

pub async fn read_sandbox_agent_error_checkin(store: &dyn Store) -> tidebreak_core::Result<u32> {
    Ok(store
        .get_setting(SANDBOX_AGENT_ERROR_CHECKIN_SETTING)
        .await?
        .and_then(|value| serde_json::from_value::<u32>(value).ok())
        .filter(|errors| *errors > 0 && *errors <= MAX_SANDBOX_AGENT_ERROR_CHECKIN)
        .unwrap_or(tidebreak_core::DEFAULT_SANDBOX_AGENT_ERROR_CHECKIN as u32))
}

pub async fn read_compaction_settings(
    store: &dyn Store,
) -> tidebreak_core::Result<CompactionSettings> {
    let defaults = CompactionSettings::default();
    let settings = CompactionSettings {
        threshold_fraction: store
            .get_setting(COMPACTION_THRESHOLD_FRACTION_SETTING)
            .await?
            .and_then(|value| serde_json::from_value::<f64>(value).ok())
            .filter(|value| *value > 0.0 && *value <= 1.0)
            .unwrap_or(defaults.threshold_fraction),
        target_fraction: store
            .get_setting(COMPACTION_TARGET_FRACTION_SETTING)
            .await?
            .and_then(|value| serde_json::from_value::<f64>(value).ok())
            .filter(|value| *value > 0.0 && *value <= 1.0)
            .unwrap_or(defaults.target_fraction),
        min_threshold_tokens: store
            .get_setting(COMPACTION_MIN_THRESHOLD_TOKENS_SETTING)
            .await?
            .and_then(|value| serde_json::from_value::<u32>(value).ok())
            .filter(|value| {
                (MIN_COMPACTION_MIN_THRESHOLD_TOKENS..=MAX_COMPACTION_MIN_THRESHOLD_TOKENS)
                    .contains(value)
            })
            .unwrap_or(defaults.min_threshold_tokens),
        protect_recent_messages: store
            .get_setting(COMPACTION_PROTECT_RECENT_MESSAGES_SETTING)
            .await?
            .and_then(|value| serde_json::from_value::<u32>(value).ok())
            .filter(|value| *value >= 1)
            .unwrap_or(defaults.protect_recent_messages),
    };
    if settings.threshold_fraction <= settings.target_fraction {
        return Ok(defaults);
    }
    Ok(settings)
}

pub async fn read_compaction_policy(store: &dyn Store) -> tidebreak_core::Result<CompactionPolicy> {
    Ok(CompactionPolicy::from(
        &read_compaction_settings(store).await?,
    ))
}
