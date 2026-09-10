//! Connection proposals for host-owned Chrome computer use.
//! The native host chooses the executable, profile, endpoint, and grant.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ToolSpec;

pub const CHROME_CONNECT_TOOL: &str = "chrome_connect";
pub const CHROME_DISCONNECT_TOOL: &str = "chrome_disconnect";
pub const CHROME_CONNECTION_STATE_TOOL: &str = "chrome_connection_state";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChromeConnectionMode {
    /// Launch Chrome without a foreground window, with a temporary profile for this session.
    Managed,
    /// Request access to the default Chrome profile through native consent.
    Existing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChromeConnectArgs {
    pub mode: ChromeConnectionMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChromeConnectionEmptyArgs {}

#[must_use]
pub fn chrome_connection_tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec::for_args::<ChromeConnectArgs>(
            CHROME_CONNECT_TOOL,
            "Request a Chrome connection for this coding session. Use managed to start Chrome in the background with a temporary isolated profile for app testing. New tabs preserve focus. Use existing only when the task needs the user's signed-in Chrome profile. The native host asks permission; existing Chrome also asks the user to approve remote debugging. The host discovers the local endpoint. After connection, use chrome_* tools. A stopped connection resumes only after fresh native consent.",
        ),
        ToolSpec::for_args::<ChromeConnectionEmptyArgs>(
            CHROME_DISCONNECT_TOOL,
            "Disconnect this session from Chrome and revoke its tab references. A managed browser closes and its temporary profile is removed. The user's existing Chrome stays open.",
        ),
        ToolSpec::for_args::<ChromeConnectionEmptyArgs>(
            CHROME_CONNECTION_STATE_TOOL,
            "Read this session's Chrome connection state without opening or controlling Chrome.",
        ),
    ]
}

#[must_use]
pub fn validate_chrome_connection_arguments(name: &str, arguments: &Value) -> bool {
    match name {
        CHROME_CONNECT_TOOL => {
            serde_json::from_value::<ChromeConnectArgs>(arguments.clone()).is_ok()
        }
        CHROME_DISCONNECT_TOOL | CHROME_CONNECTION_STATE_TOOL => {
            serde_json::from_value::<ChromeConnectionEmptyArgs>(arguments.clone()).is_ok()
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn connection_proposals_cannot_choose_endpoints_profiles_or_grants() {
        for mode in ["managed", "existing"] {
            assert!(validate_chrome_connection_arguments(
                CHROME_CONNECT_TOOL,
                &json!({"mode": mode})
            ));
            for field in [
                "endpoint",
                "websocketEndpoint",
                "profile",
                "targetId",
                "owner",
                "grant",
                "resume",
            ] {
                let mut arguments = json!({"mode": mode});
                arguments[field] = json!("untrusted");
                assert!(
                    !validate_chrome_connection_arguments(CHROME_CONNECT_TOOL, &arguments),
                    "{field}"
                );
            }
        }
        assert!(!validate_chrome_connection_arguments(
            CHROME_CONNECT_TOOL,
            &json!({"mode": "remote"})
        ));
        assert!(!validate_chrome_connection_arguments(
            CHROME_CONNECT_TOOL,
            &json!({})
        ));
    }

    #[test]
    fn lifecycle_tools_do_not_accept_another_sessions_identity() {
        for name in [CHROME_DISCONNECT_TOOL, CHROME_CONNECTION_STATE_TOOL] {
            assert!(validate_chrome_connection_arguments(name, &json!({})));
            assert!(!validate_chrome_connection_arguments(
                name,
                &json!({"session": "other"})
            ));
        }
    }
}
