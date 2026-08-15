//! Permission-prompt-tool types and SessionSpec wiring points.
//!
//! The server layer must provide:
//! - a loopback MCP HTTP endpoint that implements the engine's
//!   permission-prompt tool
//! - a session-scoped bearer token accepted only on that loopback
//!
//! This crate does not serve that endpoint. Claude Code 2.1.233's `--help`
//! does not list a `--permission-prompt-tool` flag, and no live approval
//! channel was captured in the fixture suite (the `permission-denied`
//! fixture is a denial already resolved by the engine). The adapter
//! therefore reports [`tidebreak_core::CapLevel::Unknown`] for
//! `structured_approvals` and does not compose uncaptured flags.

use serde::{Deserialize, Serialize};

use crate::ApprovalChannelSpec;

/// Request shape the loopback permission-prompt tool is expected to receive.
///
/// Field names follow the shapes seen on the `permission_denied` fixture
/// (`tool_name`, `tool_use_id`, `message`) plus a size-capped raw payload.
/// Do not treat this as a complete protocol: it has not been captured as a
/// live MCP tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionPromptRequest {
    /// Engine-native tool-use id.
    pub tool_use_id: String,
    /// Tool name, when reported.
    #[serde(default)]
    pub tool_name: String,
    /// Human-readable request.
    #[serde(default)]
    pub message: String,
    /// Size-capped raw engine payload.
    #[serde(default)]
    pub raw: serde_json::Value,
}

/// Response the loopback tool should return to unblock the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionPromptResponse {
    /// Whether the request is allowed.
    pub allowed: bool,
    /// Denial reason the model should see, when denied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// How a [`ApprovalChannelSpec`] would be applied once the flag surface is
/// captured. Returns `None` today: composing unverified flags would guess.
#[must_use]
pub fn launch_args_for_approval_channel(_channel: &ApprovalChannelSpec) -> Option<Vec<String>> {
    None
}
