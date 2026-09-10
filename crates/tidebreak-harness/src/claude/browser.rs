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

use crate::{BrowserChannelSpec, NativeChannelSpec};

/// MCP server name used in `--mcp-config` for the browser tool bridge.
pub const BROWSER_MCP_SERVER: &str = "tb-browser";

/// MCP server name used in `--mcp-config` for the native computer-use bridge.
pub const NATIVE_MCP_SERVER: &str = "tb-native";

/// The `--mcp-config` JSON entry for the browser stdio MCP server.
///
/// `bridge_command` is the absolute path from [`BrowserChannelSpec::bridge_command`].
/// The child inherits `TIDEBREAK_BROWSER_CAPFILE` from the engine process
/// environment; no env block is needed in the config.
pub fn browser_mcp_config_entry(
    bridge_command: &std::path::Path,
) -> Result<serde_json::Value, crate::HarnessError> {
    stdio_mcp_config_entry(bridge_command, "browser-mcp", "browser")
}

/// The `--mcp-config` JSON entry for the native computer-use stdio MCP
/// server. The child inherits `TIDEBREAK_NATIVE_CAPFILE` from the engine
/// process environment; the config carries only the command and arguments.
pub fn native_mcp_config_entry(
    bridge_command: &std::path::Path,
) -> Result<serde_json::Value, crate::HarnessError> {
    stdio_mcp_config_entry(bridge_command, "computer-mcp", "native")
}

fn stdio_mcp_config_entry(
    bridge_command: &std::path::Path,
    subcommand: &str,
    channel: &str,
) -> Result<serde_json::Value, crate::HarnessError> {
    let bridge_command = bridge_command.to_str().ok_or_else(|| {
        crate::HarnessError::Other(format!(
            "{channel} bridge command must be valid UTF-8 for Claude MCP config"
        ))
    })?;
    Ok(serde_json::json!({
        "type": "stdio",
        "command": bridge_command,
        "args": [subcommand],
    }))
}

/// Build a merged `--mcp-config` JSON string containing every present MCP
/// server entry. Returns `None` when no channel is present.
///
/// When only the approval channel is present, the output is identical to
/// [`crate::ApprovalChannelSpec::mcp_config_json`] so existing behavior is
/// unchanged. Otherwise every present channel's entry appears in one
/// `mcpServers` object under one `--mcp-config` flag.
pub fn merged_mcp_config_json(
    approval: Option<&crate::ApprovalChannelSpec>,
    browser: Option<&BrowserChannelSpec>,
    native: Option<&NativeChannelSpec>,
    apps: Option<&crate::AppsChannelSpec>,
) -> Result<Option<String>, crate::HarnessError> {
    if approval.is_none() && browser.is_none() && native.is_none() && apps.is_none() {
        return Ok(None);
    }
    if let (Some(channel), None, None, None) = (approval, browser, native, apps) {
        // Existing behavior: approval-only config.
        return Ok(Some(
            channel.mcp_config_json(crate::claude::approvals::APPROVAL_MCP_SERVER),
        ));
    }
    let mut servers = serde_json::Map::new();
    if let Some(channel) = approval {
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
    }
    if let Some(spec) = browser {
        // Browser entry (stdio, inherits env).
        servers.insert(
            BROWSER_MCP_SERVER.into(),
            browser_mcp_config_entry(spec.bridge_command())?,
        );
    }
    if let Some(spec) = native {
        // Native computer-use entry (stdio, inherits env).
        servers.insert(
            NATIVE_MCP_SERVER.into(),
            native_mcp_config_entry(spec.bridge_command())?,
        );
    }
    if let Some(spec) = apps {
        // Connected-apps entry (HTTP with bearer): every MCP server
        // Tidebreak has mounted, served from the loopback bridge.
        servers.insert(
            crate::AppsChannelSpec::MCP_SERVER.into(),
            spec.claude_mcp_config_entry(),
        );
    }
    Ok(Some(
        serde_json::json!({ "mcpServers": servers }).to_string(),
    ))
}

/// Launch argv fragments for the approval, browser, and native MCP config.
///
/// Returns `None` when no channel is present (no `--mcp-config` flag).
/// When only the approval channel is present, the output matches the
/// existing [`crate::claude::approvals::launch_args_for_approval_channel`]
/// exactly. The `--permission-prompt-tool` flag is added only when the
/// approval channel is present.
pub fn launch_args_for_mcp_channels(
    approval: Option<&crate::ApprovalChannelSpec>,
    browser: Option<&BrowserChannelSpec>,
    native: Option<&NativeChannelSpec>,
    apps: Option<&crate::AppsChannelSpec>,
) -> Result<Option<Vec<String>>, crate::HarnessError> {
    let Some(config) = merged_mcp_config_json(approval, browser, native, apps)? else {
        return Ok(None);
    };
    let mut flags = vec!["--mcp-config".into(), config];
    if approval.is_some() {
        flags.push("--permission-prompt-tool".into());
        flags.push(crate::claude::approvals::PERMISSION_PROMPT_TOOL.into());
    }
    Ok(Some(flags))
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
            _approval: &crate::HarnessApprovalRef,
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
        assert!(launch_args_for_mcp_channels(None, None, None, None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn approval_only_matches_existing_behavior() {
        let channel = approval_channel();
        let flags = launch_args_for_mcp_channels(Some(&channel), None, None, None)
            .unwrap()
            .unwrap();
        let existing =
            crate::claude::approvals::launch_args_for_approval_channel(&channel).unwrap();
        assert_eq!(flags, existing);
    }

    #[test]
    fn browser_only_emits_stdio_config_without_prompt_tool() {
        let browser = browser_channel();
        let flags = launch_args_for_mcp_channels(None, Some(&browser), None, None)
            .unwrap()
            .unwrap();
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
        let flags = launch_args_for_mcp_channels(Some(&approval), Some(&browser), None, None)
            .unwrap()
            .unwrap();
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
            launch_args_for_mcp_channels(None, Some(&browser), None, None),
            launch_args_for_mcp_channels(Some(&approval_channel()), Some(&browser), None, None),
        ] {
            let flags = flags.unwrap().unwrap();
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
        let flags = launch_args_for_mcp_channels(None, Some(&browser), None, None)
            .unwrap()
            .unwrap();
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

    fn native_channel() -> NativeChannelSpec {
        NativeChannelSpec::new(
            PathBuf::from("/tmp/tidebreak-native-cap.json"),
            PathBuf::from("/usr/local/bin/tidebreak"),
        )
    }

    #[test]
    fn native_only_emits_stdio_config_without_prompt_tool() {
        let native = native_channel();
        let flags = launch_args_for_mcp_channels(None, None, Some(&native), None)
            .unwrap()
            .unwrap();
        assert_eq!(flags[0], "--mcp-config");
        let config: serde_json::Value = serde_json::from_str(&flags[1]).unwrap();
        assert_eq!(config["mcpServers"]["tb-native"]["type"], "stdio");
        assert_eq!(
            config["mcpServers"]["tb-native"]["command"],
            "/usr/local/bin/tidebreak"
        );
        assert_eq!(config["mcpServers"]["tb-native"]["args"][0], "computer-mcp");
        assert!(!flags.iter().any(|f| f == "--permission-prompt-tool"));
        assert!(config["mcpServers"].get("tb-browser").is_none());
    }

    #[test]
    fn all_three_channels_merge_into_one_config() {
        let approval = approval_channel();
        let browser = browser_channel();
        let native = native_channel();
        let flags =
            launch_args_for_mcp_channels(Some(&approval), Some(&browser), Some(&native), None)
                .unwrap()
                .unwrap();
        assert_eq!(flags.iter().filter(|f| **f == "--mcp-config").count(), 1);
        let config: serde_json::Value = serde_json::from_str(&flags[1]).unwrap();
        assert!(config["mcpServers"].get("tb-approvals").is_some());
        assert!(config["mcpServers"].get("tb-browser").is_some());
        assert!(config["mcpServers"].get("tb-native").is_some());
        assert!(flags.iter().any(|f| f == "--permission-prompt-tool"));
    }

    #[test]
    fn native_config_carries_no_secrets() {
        let native = native_channel();
        let flags = launch_args_for_mcp_channels(None, None, Some(&native), None)
            .unwrap()
            .unwrap();
        let config_str = flags[1].clone();
        assert!(!config_str.contains("/tmp/tidebreak-native-cap.json"));
        assert!(!config_str.contains("TIDEBREAK_NATIVE_CAPFILE"));
        let config: serde_json::Value = serde_json::from_str(&config_str).unwrap();
        let entry = &config["mcpServers"]["tb-native"];
        assert!(entry.get("headers").is_none());
        assert!(entry.get("env").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_bridge_command_is_rejected_instead_of_changed() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let browser = BrowserChannelSpec::new(
            PathBuf::from("/tmp/tidebreak-browser-cap.json"),
            PathBuf::from(OsString::from_vec(b"/tmp/tidebreak-\xff".to_vec())),
        );
        let error = launch_args_for_mcp_channels(None, Some(&browser), None, None)
            .expect_err("non-UTF-8 bridge paths must fail closed");

        assert!(error.to_string().contains("must be valid UTF-8"));
    }

    #[test]
    fn apps_channel_joins_the_merged_config_as_an_http_server() {
        let apps = crate::AppsChannelSpec {
            mcp_endpoint_url: "http://127.0.0.1:9999/code/mcp/connected-apps".into(),
            token: "apps-token".into(),
        };
        let flags = launch_args_for_mcp_channels(None, None, None, Some(&apps))
            .unwrap()
            .expect("an apps channel alone still mounts a server");
        assert_eq!(flags[0], "--mcp-config");
        assert!(
            !flags.contains(&"--permission-prompt-tool".to_string()),
            "no approval channel, no permission-prompt flag"
        );
        let config: serde_json::Value = serde_json::from_str(&flags[1]).unwrap();
        let entry = &config["mcpServers"]["tb-apps"];
        assert_eq!(entry["type"], "http");
        assert_eq!(
            entry["url"],
            "http://127.0.0.1:9999/code/mcp/connected-apps"
        );
        assert_eq!(entry["headers"]["Authorization"], "Bearer apps-token");

        let with_approval =
            launch_args_for_mcp_channels(Some(&approval_channel()), None, None, Some(&apps))
                .unwrap()
                .unwrap();
        let config: serde_json::Value = serde_json::from_str(&with_approval[1]).unwrap();
        assert!(config["mcpServers"].get("tb-approvals").is_some());
        assert!(config["mcpServers"].get("tb-apps").is_some());
    }
}
