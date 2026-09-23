//! MCP server wiring for Claude Code: the approval, browser, native,
//! connected-apps, and human-tool servers in one `--mcp-config` document.
//!
//! Claude Code reads MCP server declarations from `--mcp-config`, which takes
//! inline JSON or a file path. A stdio MCP server is:
//!
//! ```json
//! {"type":"stdio","command":"/abs/path/to/tidebreak","args":["browser-mcp"]}
//! ```
//!
//! The browser and native children inherit their capability-file paths from
//! the engine child's environment (injected by
//! [`BrowserChannelSpec::inject_env_tokio`] via
//! [`crate::browser_channel::apply_child_env_tokio`]), so their entries carry
//! only the command and arguments. The approval and connected-apps entries are
//! HTTP servers with a bearer token, and any local account can read another
//! process's arguments. So the merged document never goes on argv: the session
//! writes it to a file only its user can read, and argv names that file
//! ([`McpLaunchConfig::flags`]).

use std::path::Path;

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
/// [`crate::ApprovalChannelSpec::mcp_config_json`]. Otherwise every present
/// channel's entry appears in one `mcpServers` object.
///
/// The approval and connected-apps entries carry bearer tokens. Write the
/// result to a private file; never put it on argv.
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

/// Claude Code's MCP wiring for one launch: the `--mcp-config` document and
/// whether the approval prompt tool rides with it.
///
/// The document carries bearer tokens, so it goes into a file only the
/// session's user can read, and argv names that file ([`Self::flags`]).
#[derive(Clone, PartialEq, Eq)]
pub struct McpLaunchConfig {
    document: String,
    permission_prompt_tool: bool,
}

impl std::fmt::Debug for McpLaunchConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpLaunchConfig")
            .field("document", &"<redacted>")
            .field("permission_prompt_tool", &self.permission_prompt_tool)
            .finish()
    }
}

impl McpLaunchConfig {
    /// The `--mcp-config` JSON document. It carries bearer tokens: write it to
    /// a private file, never to argv or a log.
    #[must_use]
    pub fn document(&self) -> &str {
        &self.document
    }

    /// The argv fragment that loads the document from `path`, plus
    /// `--permission-prompt-tool` when the approval channel is present.
    pub fn flags(&self, path: &Path) -> Result<Vec<String>, crate::HarnessError> {
        let path = path.to_str().ok_or_else(|| {
            crate::HarnessError::Other("the Claude MCP config path must be valid UTF-8".into())
        })?;
        let mut flags = vec!["--mcp-config".into(), path.to_owned()];
        if self.permission_prompt_tool {
            flags.push("--permission-prompt-tool".into());
            flags.push(crate::claude::approvals::PERMISSION_PROMPT_TOOL.into());
        }
        Ok(flags)
    }
}

/// The MCP wiring for the approval, browser, native, connected-apps, and
/// human-tool channels.
///
/// Returns `None` when no channel is present, so the launch carries no
/// `--mcp-config` flag. `--permission-prompt-tool` rides along only when the
/// approval channel is present.
pub fn mcp_launch_config(
    approval: Option<&crate::ApprovalChannelSpec>,
    browser: Option<&BrowserChannelSpec>,
    native: Option<&NativeChannelSpec>,
    apps: Option<&crate::AppsChannelSpec>,
    tools: Option<&crate::ToolBridgeSpec>,
) -> Result<Option<McpLaunchConfig>, crate::HarnessError> {
    let config = merged_mcp_config_json(approval, browser, native, apps)?;
    let document = if let Some(tools) = tools {
        let mut value: serde_json::Value = config
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|error| {
                crate::HarnessError::Other(format!("invalid MCP configuration: {error}"))
            })?
            .unwrap_or_else(|| serde_json::json!({"mcpServers":{}}));
        value["mcpServers"]["tb-human"] = tools.claude_mcp_config_entry()?;
        value.to_string()
    } else if let Some(config) = config {
        config
    } else {
        return Ok(None);
    };
    Ok(Some(McpLaunchConfig {
        document,
        permission_prompt_tool: approval.is_some(),
    }))
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

    /// Where these tests pretend the session wrote the document.
    const CONFIG_PATH: &str = "/private/tidebreak-claude-test/mcp-config.json";

    /// The flags a launch composes for these channels, and the document the
    /// file they name would hold.
    fn launch(
        approval: Option<&ApprovalChannelSpec>,
        browser: Option<&BrowserChannelSpec>,
        native: Option<&NativeChannelSpec>,
        apps: Option<&crate::AppsChannelSpec>,
    ) -> Result<Option<(Vec<String>, serde_json::Value)>, crate::HarnessError> {
        Ok(
            mcp_launch_config(approval, browser, native, apps, None)?.map(|config| {
                let flags = config.flags(Path::new(CONFIG_PATH)).unwrap();
                assert_eq!(
                    flags[..2],
                    ["--mcp-config", CONFIG_PATH],
                    "argv names the file, never the document"
                );
                (flags, serde_json::from_str(config.document()).unwrap())
            }),
        )
    }

    #[test]
    fn neither_channel_produces_no_flags() {
        assert!(launch(None, None, None, None).unwrap().is_none());
    }

    #[test]
    fn approval_only_keeps_the_approval_document_and_its_prompt_tool() {
        let channel = approval_channel();
        let config = mcp_launch_config(Some(&channel), None, None, None, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            config.document(),
            channel.mcp_config_json(crate::claude::approvals::APPROVAL_MCP_SERVER)
        );
        assert_eq!(
            config.flags(Path::new(CONFIG_PATH)).unwrap(),
            [
                "--mcp-config",
                CONFIG_PATH,
                "--permission-prompt-tool",
                crate::claude::approvals::PERMISSION_PROMPT_TOOL,
            ]
        );
    }

    /// Any local account can read another process's arguments, so the
    /// bearer tokens live only in the document the file holds.
    #[test]
    fn bearer_tokens_stay_in_the_document_and_off_the_flags() {
        let apps = crate::AppsChannelSpec {
            mcp_endpoint_url: "http://127.0.0.1:9999/code/mcp/connected-apps".into(),
            token: "apps-token".into(),
        };
        let config = mcp_launch_config(Some(&approval_channel()), None, None, Some(&apps), None)
            .unwrap()
            .unwrap();
        let flags = config.flags(Path::new(CONFIG_PATH)).unwrap();
        for token in ["test-token", "apps-token"] {
            assert!(config.document().contains(token));
            assert!(
                !flags.iter().any(|flag| flag.contains(token)),
                "{token} reached argv: {flags:?}"
            );
        }
        assert!(!format!("{config:?}").contains("test-token"));
    }

    #[test]
    fn browser_only_emits_stdio_config_without_prompt_tool() {
        let browser = browser_channel();
        let (flags, config) = launch(None, Some(&browser), None, None).unwrap().unwrap();
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
        let (flags, config) = launch(Some(&approval), Some(&browser), None, None)
            .unwrap()
            .unwrap();
        // Exactly one --mcp-config flag.
        assert_eq!(flags.iter().filter(|f| **f == "--mcp-config").count(), 1);
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
        for launched in [
            launch(None, Some(&browser), None, None),
            launch(Some(&approval_channel()), Some(&browser), None, None),
        ] {
            let (_, config) = launched.unwrap().unwrap();
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
        let (_, config) = launch(None, Some(&browser), None, None).unwrap().unwrap();
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
        let (flags, config) = launch(None, None, Some(&native), None).unwrap().unwrap();
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
        let (flags, config) = launch(Some(&approval), Some(&browser), Some(&native), None)
            .unwrap()
            .unwrap();
        assert_eq!(flags.iter().filter(|f| **f == "--mcp-config").count(), 1);
        assert!(config["mcpServers"].get("tb-approvals").is_some());
        assert!(config["mcpServers"].get("tb-browser").is_some());
        assert!(config["mcpServers"].get("tb-native").is_some());
        assert!(flags.iter().any(|f| f == "--permission-prompt-tool"));
    }

    #[test]
    fn native_config_carries_no_secrets() {
        let native = native_channel();
        let (_, config) = launch(None, None, Some(&native), None).unwrap().unwrap();
        let config_str = config.to_string();
        assert!(!config_str.contains("/tmp/tidebreak-native-cap.json"));
        assert!(!config_str.contains("TIDEBREAK_NATIVE_CAPFILE"));
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
        let error = launch(None, Some(&browser), None, None)
            .expect_err("non-UTF-8 bridge paths must fail closed");

        assert!(error.to_string().contains("must be valid UTF-8"));
    }

    #[test]
    fn apps_channel_joins_the_merged_config_as_an_http_server() {
        let apps = crate::AppsChannelSpec {
            mcp_endpoint_url: "http://127.0.0.1:9999/code/mcp/connected-apps".into(),
            token: "apps-token".into(),
        };
        let (flags, config) = launch(None, None, None, Some(&apps))
            .unwrap()
            .expect("an apps channel alone still mounts a server");
        assert!(
            !flags.contains(&"--permission-prompt-tool".to_string()),
            "no approval channel, no permission-prompt flag"
        );
        let entry = &config["mcpServers"]["tb-apps"];
        assert_eq!(entry["type"], "http");
        assert_eq!(
            entry["url"],
            "http://127.0.0.1:9999/code/mcp/connected-apps"
        );
        assert_eq!(entry["headers"]["Authorization"], "Bearer apps-token");

        let (_, config) = launch(Some(&approval_channel()), None, None, Some(&apps))
            .unwrap()
            .unwrap();
        assert!(config["mcpServers"].get("tb-approvals").is_some());
        assert!(config["mcpServers"].get("tb-apps").is_some());
    }
}
