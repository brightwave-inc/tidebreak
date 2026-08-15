//! Permission-prompt-tool types and SessionSpec wiring, captured on 2.1.233.
//!
//! `--help` does not list `--permission-prompt-tool`, but the flag is accepted
//! and required to park a print-mode turn. The CLI calls an MCP tool named
//! `mcp__tb-approvals__permission_prompt` with `{tool_name, input, tool_use_id}`
//! and unblocks on a single text block: `{"behavior":"allow"}` or
//! `{"behavior":"deny","message":"..."}`. HTTP MCP with a Bearer token works;
//! that is the loopback channel the server serves.
//!
//! This crate does not serve that endpoint. It composes the captured flags
//! and turns a captured request into [`crate::HarnessEvent::ApprovalRequested`].

use serde::{Deserialize, Serialize};

use crate::{ApprovalChannelSpec, ApprovalDecision, HarnessApprovalRef, HarnessEvent};

/// MCP server name used in `--mcp-config` and the tool prefix.
pub const APPROVAL_MCP_SERVER: &str = "tb-approvals";

/// Tool name advertised by the loopback MCP server.
pub const APPROVAL_MCP_TOOL: &str = "permission_prompt";

/// Value passed as `--permission-prompt-tool`.
pub const PERMISSION_PROMPT_TOOL: &str = "mcp__tb-approvals__permission_prompt";

/// Request the CLI sends to the permission-prompt MCP tool.
///
/// Captured on 2.1.233 (`approval-request.mcp.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionPromptRequest {
    /// Engine tool that is waiting.
    pub tool_name: String,
    /// Arguments the engine wanted to pass that tool.
    #[serde(default)]
    pub input: serde_json::Value,
    /// Engine-native tool-use id. This is the [`HarnessApprovalRef::call_id`].
    pub tool_use_id: String,
}

/// Body encoded as the single MCP text block the CLI requires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionPromptResponse {
    /// `allow` or `deny`.
    pub behavior: PermissionBehavior,
    /// Denial reason the model sees. Required on deny in the captured shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Captured `behavior` tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionBehavior {
    /// Let the engine run the tool.
    Allow,
    /// Reject; `message` is surfaced as the tool_result the model reads.
    Deny,
}

impl PermissionPromptResponse {
    /// Encode as the single `type=text` block the CLI accepts.
    #[must_use]
    pub fn as_text_block(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"behavior":"deny","message":"permission prompt failed to encode"}"#.into()
        })
    }

    /// Build the captured response shape from a normalized decision.
    #[must_use]
    pub fn from_decision(decision: &ApprovalDecision) -> Self {
        match decision {
            ApprovalDecision::Approve => Self {
                behavior: PermissionBehavior::Allow,
                message: None,
            },
            ApprovalDecision::Deny { feedback } => Self {
                behavior: PermissionBehavior::Deny,
                message: Some(
                    feedback
                        .clone()
                        .filter(|text| !text.is_empty())
                        .unwrap_or_else(|| "denied".into()),
                ),
            },
        }
    }
}

/// Map a captured prompt-tool request onto a normalized approval event.
#[must_use]
pub fn event_from_prompt_request(request: &PermissionPromptRequest) -> HarnessEvent {
    HarnessEvent::ApprovalRequested {
        harness_ref: HarnessApprovalRef {
            call_id: request.tool_use_id.clone(),
        },
        raw: serde_json::to_value(request).unwrap_or(serde_json::Value::Null),
    }
}

/// Launch argv fragments captured on 2.1.233. Always `Some` for this version.
#[must_use]
pub fn launch_args_for_approval_channel(channel: &ApprovalChannelSpec) -> Option<Vec<String>> {
    Some(vec![
        "--mcp-config".into(),
        channel.mcp_config_json(APPROVAL_MCP_SERVER),
        "--permission-prompt-tool".into(),
        PERMISSION_PROMPT_TOOL.into(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_request_round_trips() {
        let raw = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/claude-code/2.1.233/approval-request.mcp.json"
        ));
        let value: serde_json::Value = serde_json::from_str(raw).unwrap();
        let args = value["params"]["arguments"].clone();
        let request: PermissionPromptRequest = serde_json::from_value(args).unwrap();
        assert_eq!(request.tool_name, "Write");
        assert!(!request.tool_use_id.is_empty());
        match event_from_prompt_request(&request) {
            HarnessEvent::ApprovalRequested { harness_ref, raw } => {
                assert_eq!(harness_ref.call_id, request.tool_use_id);
                assert_eq!(raw["tool_name"], "Write");
            }
            other => panic!("expected ApprovalRequested, got {other:?}"),
        }
    }

    #[test]
    fn deny_response_is_a_single_text_json_object() {
        let text = PermissionPromptResponse::from_decision(&ApprovalDecision::Deny {
            feedback: Some("no — use the fixtures directory instead".into()),
        })
        .as_text_block();
        let parsed: PermissionPromptResponse = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.behavior, PermissionBehavior::Deny);
        assert_eq!(
            parsed.message.as_deref(),
            Some("no — use the fixtures directory instead")
        );
    }
}
