//! Transport contracts for session-scoped computer use.
//! Session identity and grants come from the host, never from these arguments.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// One identified operation. Reuse the id only to recover the same operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerUseCall {
    pub request_id: Uuid,
    pub name: String,
    pub arguments: Value,
}

/// Whether the host can establish that the operation took effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerUseOutcome {
    Completed,
    Rejected,
    /// Inspect the target before proposing another action. Do not replay it.
    Unknown,
}

/// Image transport data. Agent adapters emit pixels as images, never as text.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerUseImage {
    pub mime_type: String,
    pub base64: String,
}

/// A bounded operation result with images separated from model-facing text.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerUseResult {
    pub request_id: Uuid,
    pub outcome: ComputerUseOutcome,
    pub text: String,
    pub data: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ComputerUseImage>,
}

/// Native apps and Chrome share the coding-session capability channel. The
/// in-app browser retains its compatible browser channel and tool schemas.
pub fn computer_session_tool_specs() -> Vec<crate::ToolSpec> {
    let mut specs = crate::computer_use::computer_use_tool_specs();
    specs.extend(crate::chrome_computer_use::chrome_computer_use_tool_specs());
    specs.extend(crate::chrome_connection::chrome_connection_tool_specs());
    specs
}

pub fn validate_computer_session_arguments(name: &str, arguments: &Value) -> bool {
    crate::computer_use::validate_computer_use_arguments(name, arguments)
        || crate::chrome_computer_use::validate_chrome_computer_use_arguments(name, arguments)
        || crate::chrome_connection::validate_chrome_connection_arguments(name, arguments)
}

pub fn is_chrome_session_tool(name: &str) -> bool {
    crate::chrome_computer_use::is_chrome_computer_use_tool(name)
        || matches!(
            name,
            crate::chrome_connection::CHROME_CONNECT_TOOL
                | crate::chrome_connection::CHROME_DISCONNECT_TOOL
                | crate::chrome_connection::CHROME_CONNECTION_STATE_TOOL
        )
}

#[cfg(test)]
mod session_tool_tests {
    use super::*;

    #[test]
    fn session_surface_includes_native_chrome_and_connection_tools_without_authority_fields() {
        let specs = computer_session_tool_specs();
        let names: std::collections::BTreeSet<_> =
            specs.iter().map(|spec| spec.name.as_str()).collect();
        assert_eq!(names.len(), specs.len());
        for name in [
            "computer_click",
            "computer_drag",
            "chrome_connect",
            "chrome_act",
            "chrome_screenshot",
        ] {
            assert!(names.contains(name), "{name}");
        }
        assert!(validate_computer_session_arguments(
            "chrome_connect",
            &serde_json::json!({"mode": "managed"})
        ));
        assert!(!validate_computer_session_arguments(
            "chrome_connect",
            &serde_json::json!({"mode": "managed", "endpoint": "ws://127.0.0.1:1"})
        ));
        assert!(!validate_computer_session_arguments(
            "computer_wait",
            &serde_json::json!({"session": "other"})
        ));
        assert!(!is_chrome_session_tool("computer_click"));
    }
}
