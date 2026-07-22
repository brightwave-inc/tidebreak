//! Boot-time configuration for external MCP stdio servers.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use openwave_core::{AgentError, Result, ToolRegistry};
use openwave_mcp::McpClient;
use serde::Deserialize;
use tokio::process::Command;

const CONFIG_ENV: &str = "OPENWAVE_MCP_CONFIG";
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const MAX_SERVERS: usize = 32;
const MAX_ARGS: usize = 128;
const MAX_ENVIRONMENT_VARIABLES: usize = 128;
const MAX_REQUEST_TIMEOUT_MS: u64 = 60 * 60 * 1000;
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 60 * 1000;

/// Validated external servers selected for one process boot.
#[derive(Default)]
pub(crate) struct ConfiguredMcpServers(Vec<StdioServerConfig>);

impl ConfiguredMcpServers {
    pub(crate) fn from_env() -> Result<Self> {
        let Some(path) = std::env::var_os(CONFIG_ENV).filter(|path| !path.is_empty()) else {
            return Ok(Self::default());
        };
        Self::from_path(Path::new(&path))
    }

    fn from_path(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|error| {
            AgentError::config(format!(
                "could not open MCP config {}: {error}",
                path.display()
            ))
        })?;
        let mut bytes = Vec::new();
        file.take(MAX_CONFIG_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                AgentError::config(format!(
                    "could not read MCP config {}: {error}",
                    path.display()
                ))
            })?;
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err(AgentError::config(format!(
                "MCP config {} exceeds {MAX_CONFIG_BYTES} bytes",
                path.display()
            )));
        }
        Self::parse(path, &bytes)
    }

    fn parse(path: &Path, bytes: &[u8]) -> Result<Self> {
        let config: ConfigFile = serde_json::from_slice(bytes).map_err(|error| {
            AgentError::config(format!("invalid MCP config {}: {error}", path.display()))
        })?;
        validate_servers(config.servers).map(Self)
    }

    pub(crate) async fn mount(self, registry: &mut ToolRegistry) -> Result<()> {
        for server in self.0 {
            let name = server.name.clone();
            let client = server.connect().await.map_err(|error| {
                AgentError::config(format!(
                    "external MCP server {name} failed to start: {error}"
                ))
            })?;
            client.mount(registry);
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    servers: Vec<StdioServerConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StdioServerConfig {
    name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    env_from: Vec<String>,
    #[serde(default)]
    inherit_env: bool,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default = "default_request_timeout_ms")]
    request_timeout_ms: u64,
}

impl StdioServerConfig {
    fn build_command(&self) -> Result<Command> {
        let mut command = Command::new(&self.command);
        command.args(&self.args);
        if !self.inherit_env {
            command.env_clear();
        }
        for name in &self.env_from {
            let value = std::env::var_os(name).ok_or_else(|| {
                AgentError::config(format!(
                    "required parent environment variable {name:?} is not set"
                ))
            })?;
            command.env(name, value);
        }
        command.envs(&self.env);
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        Ok(command)
    }

    async fn connect(self) -> Result<McpClient> {
        let command = self.build_command()?;
        McpClient::spawn_with_timeout(
            self.name,
            command,
            Duration::from_millis(self.request_timeout_ms),
        )
        .await
    }
}

const fn default_request_timeout_ms() -> u64 {
    DEFAULT_REQUEST_TIMEOUT_MS
}

fn validate_servers(servers: Vec<StdioServerConfig>) -> Result<Vec<StdioServerConfig>> {
    if servers.len() > MAX_SERVERS {
        return Err(AgentError::config(format!(
            "MCP config contains more than {MAX_SERVERS} servers"
        )));
    }
    let mut names = HashSet::new();
    for server in &servers {
        validate_name(&server.name)?;
        validate_process_string(&server.name, "command", &server.command)?;
        if server.command.is_empty() {
            return Err(server_error(&server.name, "command must not be empty"));
        }
        if server.args.len() > MAX_ARGS {
            return Err(server_error(
                &server.name,
                format!("must not contain more than {MAX_ARGS} arguments"),
            ));
        }
        for argument in &server.args {
            validate_process_string(&server.name, "argument", argument)?;
        }
        if server.env.len().saturating_add(server.env_from.len()) > MAX_ENVIRONMENT_VARIABLES {
            return Err(server_error(
                &server.name,
                format!(
                    "must not contain more than {MAX_ENVIRONMENT_VARIABLES} environment variables"
                ),
            ));
        }
        let mut environment_names = HashSet::new();
        for (key, value) in &server.env {
            validate_environment_name(&server.name, key)?;
            environment_names.insert(key.as_str());
            validate_process_string(&server.name, "environment value", value)?;
        }
        for key in &server.env_from {
            validate_environment_name(&server.name, key)?;
            if !environment_names.insert(key) {
                return Err(server_error(
                    &server.name,
                    format!("environment variable {key:?} is configured more than once"),
                ));
            }
        }
        if server
            .cwd
            .as_ref()
            .and_then(|path| path.to_str())
            .is_some_and(|path| path.contains('\0'))
        {
            return Err(server_error(
                &server.name,
                "working directory must not contain NUL",
            ));
        }
        if !(1..=MAX_REQUEST_TIMEOUT_MS).contains(&server.request_timeout_ms) {
            return Err(server_error(
                &server.name,
                format!("request_timeout_ms must be between 1 and {MAX_REQUEST_TIMEOUT_MS}"),
            ));
        }
        if !names.insert(server.name.clone()) {
            return Err(server_error(&server.name, "server name is duplicated"));
        }
    }
    Ok(servers)
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(server_error(
            name,
            "name must contain only ASCII letters, digits, '_' or '-'",
        ));
    }
    Ok(())
}

fn validate_process_string(name: &str, field: &str, value: &str) -> Result<()> {
    if value.contains('\0') {
        return Err(server_error(name, format!("{field} must not contain NUL")));
    }
    Ok(())
}

fn validate_environment_name(server_name: &str, name: &str) -> Result<()> {
    if name.is_empty() || name.contains('=') || name.contains('\0') {
        return Err(server_error(
            server_name,
            format!("invalid environment variable name {name:?}"),
        ));
    }
    Ok(())
}

fn server_error(name: &str, message: impl std::fmt::Display) -> AgentError {
    AgentError::config(format!("invalid external MCP server {name:?}: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<ConfiguredMcpServers> {
        ConfiguredMcpServers::parse(Path::new("test-mcp.json"), json.as_bytes())
    }

    #[test]
    fn parses_a_bounded_stdio_server_configuration() {
        let config = parse(
            r#"{
                "servers": [{
                    "name": "private_docs",
                    "command": "/usr/local/bin/docs-mcp",
                    "args": ["--stdio"],
                    "env": {"LOG_LEVEL": "info"},
                    "env_from": ["DOCS_TOKEN"],
                    "cwd": "/srv/docs",
                    "request_timeout_ms": 2500
                }]
            }"#,
        )
        .unwrap();
        let server = &config.0[0];
        assert_eq!(server.name, "private_docs");
        assert_eq!(server.command, "/usr/local/bin/docs-mcp");
        assert_eq!(server.args, ["--stdio"]);
        assert_eq!(server.env.get("LOG_LEVEL").unwrap(), "info");
        assert_eq!(server.env_from, ["DOCS_TOKEN"]);
        assert!(!server.inherit_env);
        assert_eq!(server.cwd.as_deref(), Some(Path::new("/srv/docs")));
        assert_eq!(server.request_timeout_ms, 2500);
    }

    #[test]
    fn defaults_to_an_isolated_environment_and_sixty_second_timeout() {
        let config = parse(r#"{"servers":[{"name":"docs","command":"/bin/docs"}]}"#).unwrap();
        let server = &config.0[0];
        assert!(!server.inherit_env);
        assert!(server.args.is_empty());
        assert!(server.env.is_empty());
        assert!(server.env_from.is_empty());
        assert_eq!(server.request_timeout_ms, 60_000);
    }

    #[test]
    fn rejects_duplicate_names_and_unsafe_process_strings() {
        let duplicate = parse(
            r#"{"servers":[
                {"name":"docs","command":"/bin/one"},
                {"name":"docs","command":"/bin/two"}
            ]}"#,
        )
        .err()
        .unwrap();
        assert!(duplicate.to_string().contains("duplicated"));

        let nul = parse("{\"servers\":[{\"name\":\"docs\",\"command\":\"bad\\u0000command\"}]}")
            .err()
            .unwrap();
        assert!(nul.to_string().contains("must not contain NUL"));
    }

    #[test]
    fn rejects_unknown_fields_and_out_of_range_timeouts() {
        let unknown =
            parse(r#"{"servers":[{"name":"docs","command":"/bin/docs","transport":"http"}]}"#)
                .err()
                .unwrap();
        assert!(unknown.to_string().contains("unknown field"));

        let timeout =
            parse(r#"{"servers":[{"name":"docs","command":"/bin/docs","request_timeout_ms":0}]}"#)
                .err()
                .unwrap();
        assert!(timeout.to_string().contains("request_timeout_ms"));
    }

    #[test]
    fn rejects_ambiguous_or_invalid_environment_sources() {
        let duplicate = parse(
            r#"{"servers":[{
                "name":"docs",
                "command":"/bin/docs",
                "env":{"DOCS_TOKEN":"literal"},
                "env_from":["DOCS_TOKEN"]
            }]}"#,
        )
        .err()
        .unwrap();
        assert!(duplicate.to_string().contains("configured more than once"));

        let invalid = parse(
            r#"{"servers":[{
                "name":"docs",
                "command":"/bin/docs",
                "env_from":["BAD=NAME"]
            }]}"#,
        )
        .err()
        .unwrap();
        assert!(invalid
            .to_string()
            .contains("invalid environment variable name"));
    }

    #[test]
    fn forwards_only_explicitly_selected_parent_environment_values() {
        let config = parse(
            r#"{"servers":[{
                "name":"docs",
                "command":"/bin/docs",
                "env_from":["PATH"]
            }]}"#,
        )
        .unwrap();
        let command = config.0[0].build_command().unwrap();
        let forwarded_path = command
            .as_std()
            .get_envs()
            .find(|(name, _)| *name == "PATH")
            .and_then(|(_, value)| value)
            .expect("PATH is selected for forwarding");
        assert_eq!(Some(forwarded_path), std::env::var_os("PATH").as_deref());
        assert!(command.as_std().get_envs().all(|(name, _)| name == "PATH"));
    }

    #[tokio::test]
    async fn missing_selected_parent_environment_fails_before_spawn() {
        const MISSING: &str = "OPENWAVE_TEST_MCP_ENV_FROM_MUST_NOT_EXIST_46F54489";
        assert!(std::env::var_os(MISSING).is_none());
        let config = parse(&format!(
            r#"{{"servers":[{{
                "name":"docs",
                "command":"/definitely/not/a/real/command",
                "env_from":["{MISSING}"]
            }}]}}"#
        ))
        .unwrap();
        let error = config
            .0
            .into_iter()
            .next()
            .unwrap()
            .connect()
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains(MISSING));
        assert!(error.to_string().contains("is not set"));
    }
}
