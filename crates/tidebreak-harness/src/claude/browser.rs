//! Browser MCP stdio server injection for Claude Code 2.1.233.
//!
//! Claude Code accepts MCP server declarations through `--mcp-config <json>`,
//! the same flag the approval channel already uses. A stdio MCP server is:
//!
//! ```json
//! {"type":"stdio","command":"/abs/path/to/tidebreak","args":["browser-mcp"]}
//! ```
//!
//! The child process inherits `TIDEBREAK_BROWSER_CAPFILE` from the engine
//! child's environment (injected by [`BrowserChannelSpec::inject_env_tokio`]
//! via [`crate::browser_channel::apply_child_env_tokio`]), so the config
//! carries no secrets — only the command and arguments.
//!
//! When both the approval and browser channels are present, the two
//! `mcpServers` entries are merged into a single `--mcp-config` flag so
//! the engine sees one config object.

use crate::BrowserChannelSpec;

/// MCP server name used in `--mcp-config` for the browser tool bridge.
pub const BROWSER_MCP_SERVER: &str = "tb-browser";

/// The `--mcp-config` JSON entry for the browser stdio MCP server.
///
/// `bridge_command` is the absolute path from [`BrowserChannelSpec::bridge_command`].
/// The child inherits `TIDEBREAK_BROWSER_CAPFILE` from the engine process
/// environment; no env block is needed in the config.
#[must_use]
pub fn browser_mcp_config_entry(bridge_command: &std::path::Path) -> serde_json::Value {
    serde_json::json!({
        "type": "stdio",
        "command": bridge_command.to_string_lossy(),
        "args": ["browser-mcp"],
    })
}

/// Build a merged `--mcp-config` JSON string containing every present MCP
/// server entry. Returns `None` when neither channel is present.
///
/// When only the approval channel is present, the output is identical to
/// [`crate::ApprovalChannelSpec::mcp_config_json`] so existing behavior is
/// unchanged. When only the browser channel is present, the output contains
/// only the browser entry. When both are present, both entries appear in one
/// `mcpServers` object under one `--mcp-config` flag.
#[must_use]
pub fn merged_mcp_config_json(
    approval: Option<&crate::ApprovalChannelSpec>,
    browser: Option<&BrowserChannelSpec>,
) -> Option<String> {
    match (approval, browser) {
        (None, None) => None,
        (Some(channel), None) => {
            // Existing behavior: approval-only config.
            Some(channel.mcp_config_json(crate::claude::approvals::APPROVAL_MCP_SERVER))
        }
        (None, Some(spec)) => {
            let mut servers = serde_json::Map::new();
            servers.insert(
                BROWSER_MCP_SERVER.into(),
                browser_mcp_config_entry(spec.bridge_command()),
            );
            Some(serde_json::json!({ "mcpServers": servers }).to_string())
        }
        (Some(channel), Some(spec)) => {
            // Merge both into one config object.
            let mut servers = serde_json::Map::new();
            // Approval entry (HTTP with bearer).
            servers.insert(
                crate::claude::approvals::APPROVAL_MCP_SERVER.into(),
                serde_json::json!({
                    "type": "http",
                    "url": channel.mcp_endpoint_url,
                    "headers": {
                        "Authorization": format!("Bearer {}", channel.token),
                    },
                }),
            );
            // Browser entry (stdio, inherits env).
            servers.insert(
                BROWSER_MCP_SERVER.into(),
                browser_mcp_config_entry(spec.bridge_command()),
            );
            Some(serde_json::json!({ "mcpServers": servers }).to_string())
        }
    }
}

/// Launch argv fragments for the browser and/or approval MCP config.
///
/// Returns `None` when neither channel is present (no `--mcp-config` flag).
/// When only the approval channel is present, the output matches the
/// existing [`crate::claude::approvals::launch_args_for_approval_channel`]
/// exactly. When the browser channel is present, the `--mcp-config` flag
/// carries the merged config and the `--permission-prompt-tool` flag is
/// added only when the approval channel is also present.
#[must_use]
pub fn launch_args_for_mcp_channels(
    approval: Option<&crate::ApprovalChannelSpec>,
    browser: Option<&BrowserChannelSpec>,
) -> Option<Vec<String>> {
    let config = merged_mcp_config_json(approval, browser)?;
    let mut flags = vec!["--mcp-config".into(), config];
    if approval.is_some() {
        flags.push("--permission-prompt-tool".into());
        flags.push(crate::claude::approvals::PERMISSION_PROMPT_TOOL.into());
    }
    Some(flags)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ApprovalChannelSpec;
    use std::path::PathBuf;
    use std::sync::Arc;

    /// A no-op completer for test ApprovalChannelSpecs.
    struct NoopCompleter;

    #[async_trait::async_trait]
    impl crate::ApprovalCompleter for NoopCompleter {
        async fn complete(
            &self,
            _call_id: &str,
            _decision: crate::ApprovalDecision,
        ) -> Result<(), crate::HarnessError> {
            Ok(())
        }
    }

    fn approval_channel() -> ApprovalChannelSpec {
        ApprovalChannelSpec {
            mcp_endpoint_url: "http://127.0.0.1:9999/code/mcp/approval-prompt".into(),
            token: "test-token".into(),
            completer: Arc::new(NoopCompleter),
        }
    }

    fn browser_channel() -> BrowserChannelSpec {
        BrowserChannelSpec::new(
            PathBuf::from("/tmp/tidebreak-browser-cap.json"),
            PathBuf::from("/usr/local/bin/tidebreak"),
        )
    }

    #[test]
    fn neither_channel_produces_no_flags() {
        assert!(launch_args_for_mcp_channels(None, None).is_none());
    }

    #[test]
    fn approval_only_matches_existing_behavior() {
        let channel = approval_channel();
        let flags = launch_args_for_mcp_channels(Some(&channel), None).unwrap();
        let existing =
            crate::claude::approvals::launch_args_for_approval_channel(&channel).unwrap();
        assert_eq!(flags, existing);
    }

    #[test]
    fn browser_only_emits_stdio_config_without_prompt_tool() {
        let browser = browser_channel();
        let flags = launch_args_for_mcp_channels(None, Some(&browser)).unwrap();
        assert_eq!(flags[0], "--mcp-config");
        let config: serde_json::Value = serde_json::from_str(&flags[1]).unwrap();
        assert_eq!(config["mcpServers"]["tb-browser"]["type"], "stdio");
        assert_eq!(
            config["mcpServers"]["tb-browser"]["command"],
            "/usr/local/bin/tidebreak"
        );
        assert_eq!(config["mcpServers"]["tb-browser"]["args"][0], "browser-mcp");
        // No --permission-prompt-tool when approval is absent.
        assert!(!flags.iter().any(|f| f == "--permission-prompt-tool"));
        // No approval server when approval is absent.
        assert!(config["mcpServers"].get("tb-approvals").is_none());
    }

    #[test]
    fn both_channels_merge_into_one_config() {
        let approval = approval_channel();
        let browser = browser_channel();
        let flags = launch_args_for_mcp_channels(Some(&approval), Some(&browser)).unwrap();
        // Exactly one --mcp-config flag.
        assert_eq!(flags.iter().filter(|f| **f == "--mcp-config").count(), 1);
        let config: serde_json::Value = serde_json::from_str(&flags[1]).unwrap();
        assert!(config["mcpServers"].get("tb-approvals").is_some());
        assert!(config["mcpServers"].get("tb-browser").is_some());
        // Approval is HTTP with bearer.
        assert_eq!(config["mcpServers"]["tb-approvals"]["type"], "http");
        // Browser is stdio.
        assert_eq!(config["mcpServers"]["tb-browser"]["type"], "stdio");
        // --permission-prompt-tool present with approval.
        assert!(flags.iter().any(|f| f == "--permission-prompt-tool"));
    }

    #[test]
    fn browser_config_carries_no_secrets() {
        let browser = browser_channel();
        for flags in [
            launch_args_for_mcp_channels(None, Some(&browser)),
            launch_args_for_mcp_channels(Some(&approval_channel()), Some(&browser)),
        ] {
            let flags = flags.unwrap();
            let config: serde_json::Value = serde_json::from_str(&flags[1]).unwrap();
            let config_str = config.to_string();
            // No capfile path or env key in the config text.
            assert!(!config_str.contains("/tmp/tidebreak-browser-cap.json"));
            assert!(!config_str.contains("TIDEBREAK_BROWSER_CAPFILE"));
            // No browser bearer token; the only Authorization header belongs
            // to the existing approval HTTP server and is expected there.
            let browser_entry = &config["mcpServers"]["tb-browser"];
            assert!(
                browser_entry.get("headers").is_none(),
                "browser entry must not carry headers"
            );
            assert!(
                browser_entry.get("env").is_none(),
                "browser entry must not carry an env block or secret"
            );
        }
    }

    #[test]
    fn bridge_command_with_spaces_remains_one_command_value() {
        let browser = BrowserChannelSpec::new(
            PathBuf::from("/tmp/with spaces/cap.json"),
            PathBuf::from("/Applications/Tidebreak.app/Contents/bin/tidebreak"),
        );
        let flags = launch_args_for_mcp_channels(None, Some(&browser)).unwrap();
        let config: serde_json::Value = serde_json::from_str(&flags[1]).unwrap();
        let command = config["mcpServers"]["tb-browser"]["command"]
            .as_str()
            .unwrap();
        // The command must be one JSON string value, not split on spaces.
        assert_eq!(
            command,
            "/Applications/Tidebreak.app/Contents/bin/tidebreak"
        );
        // args must still be exactly ["browser-mcp"].
        let args = config["mcpServers"]["tb-browser"]["args"]
            .as_array()
            .unwrap();
        assert_eq!(args.len(), 1);
        assert_eq!(args[0], "browser-mcp");
    }
}
