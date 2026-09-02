//! HTTP and WebSocket route handlers.
//!
//! Document lifecycle and search handlers live in the dedicated `document`
//! submodule; other handler clusters live in sibling modules under `routes/`.

pub(crate) const MAX_ACTIVE_BACKGROUND_AGENTS_SETTING: &str = "agents.max_active_background_agents";
pub(crate) const SANDBOX_AGENT_CHECKIN_STEPS_SETTING: &str = "agents.sandbox_agent_checkin_steps";
pub(crate) const SANDBOX_AGENT_ERROR_CHECKIN_SETTING: &str = "agents.sandbox_agent_error_checkin";
pub(crate) const COMPACTION_THRESHOLD_FRACTION_SETTING: &str = "compaction.threshold_fraction";
pub(crate) const COMPACTION_TARGET_FRACTION_SETTING: &str = "compaction.target_fraction";
pub(crate) const COMPACTION_MIN_THRESHOLD_TOKENS_SETTING: &str = "compaction.min_threshold_tokens";
pub(crate) const COMPACTION_PROTECT_RECENT_MESSAGES_SETTING: &str =
    "compaction.protect_recent_messages";
pub(crate) const MODEL_VISIBILITY_OVERRIDES_SETTING: &str = "models.visibility_overrides";
pub(crate) const PROMPT_CACHE_RETENTION_SETTING: &str = "models.prompt_cache_retention";
/// Master switch for the computer-use capability (screen capture + app
/// control). Default on; setting it false unregisters the tools at boot.
pub(crate) const COMPUTER_USE_ENABLED_SETTING: &str = "computer_use.enabled";
/// Master switch for durable memory. Default on; explicit reads and writes
/// stay available even when no utility model is configured.
pub(crate) const MEMORY_ENABLED_SETTING: &str = "memory.enabled";
/// Master switch for post-turn memory capture. Capture also requires a
/// resolvable utility model.
pub(crate) const MEMORY_CAPTURE_ENABLED_SETTING: &str = "memory.capture.enabled";

mod agent_runs;
mod app_gateway_page;
mod app_grant;
mod app_invoke;
mod app_library;
mod approvals;
pub(crate) mod client_execution;
pub(crate) mod code;
mod compaction;
mod connected_apps;
mod delegated_file_execution;
mod document;
mod events;
pub(crate) mod image_attachment;
mod inbox;
mod mcp_gateway;
mod memory;
mod notifications;
mod outputs;
mod plans;
mod plugins;
mod projects_chats;
pub(crate) mod providers_models;
mod root_attachment;
mod settings;
mod task_plan;
mod turn_control;
mod user_questions;

pub use agent_runs::*;
pub use app_gateway_page::*;
pub use app_grant::*;
pub use app_invoke::*;
pub use app_library::*;
pub use approvals::*;
pub use client_execution::*;
pub use compaction::*;
pub use connected_apps::*;
pub use delegated_file_execution::*;
pub use document::*;
pub use events::*;
pub use image_attachment::*;
pub use inbox::*;
pub use mcp_gateway::*;
pub use memory::*;
pub use notifications::*;
pub use outputs::*;
pub use plans::*;
pub use plugins::*;
pub use projects_chats::*;
pub use providers_models::*;
pub use root_attachment::*;
pub use settings::*;
pub use task_plan::*;
pub use turn_control::*;
pub use user_questions::*;

/// The policy every stored-bytes response carries.
///
/// Both byte-serving routes hand back content that originated outside Tidebreak
/// — a reader's file, or an image an agent produced — from the API's own
/// origin, so a response a browser ever renders must be unable to reach back
/// into that origin. `sandbox` drops the response into an opaque origin with
/// scripting off, and `default-src 'none'` denies it every subresource and
/// every outbound request.
///
/// It is shared rather than duplicated so the two routes cannot drift into
/// serving comparable bytes under different rules.
pub(crate) const SERVED_BYTES_CONTENT_POLICY: &str =
    "default-src 'none'; sandbox; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";
