//! Runtime configuration and supervision for external MCP servers.
//!
//! A definition is either a local stdio child process or a remote Streamable
//! HTTP endpoint. Definitions are typed data, never shell fragments. Every
//! child starts with a cleared environment and receives only literal values
//! explicitly marked non-secret plus values selected by *name* from the parent
//! environment; an HTTP server's bearer token is likewise selected by name.
//! Selected values are resolved only at the connection boundary and are never
//! stored or projected through the API.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures::future::join_all;
use openwave_core::{AgentError, Result, Store, ToolRegistry};
use openwave_mcp::{McpClient, McpProbe, MAX_SERVER_NAME_BYTES};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::sync::Mutex;

const CONFIG_ENV: &str = "OPENWAVE_MCP_CONFIG";
const SETTING_KEY: &str = "mcp_servers_v1";
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
pub(crate) const MAX_CONFIG_BODY_BYTES: usize = 1024 * 1024;
const MAX_SERVERS: usize = 32;
const MAX_ARGS: usize = 128;
const MAX_ENVIRONMENT_VARIABLES: usize = 128;
const MAX_PROCESS_STRING_BYTES: usize = 32 * 1024;
const MAX_ENVIRONMENT_NAME_BYTES: usize = 256;
const MAX_REQUEST_TIMEOUT_MS: u64 = 60 * 60 * 1000;
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 60 * 1000;
const HEALTH_INTERVAL: Duration = Duration::from_secs(15);
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const INITIALIZATION_TIMEOUT: Duration = Duration::from_secs(10);
const INITIAL_RECONNECT_BACKOFF: Duration = Duration::from_secs(1);
const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(30);

/// Validated external servers selected by the legacy boot file.
#[derive(Default)]
pub(crate) struct ConfiguredMcpServers(Vec<McpServerDefinition>);

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
        let config: McpServersConfig = serde_json::from_slice(&bytes).map_err(|error| {
            AgentError::config(format!("invalid MCP config {}: {error}", path.display()))
        })?;
        validate_servers(&config.servers)?;
        Ok(Self(config.servers))
    }
}

/// Complete persisted MCP configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpServersConfig {
    pub(crate) servers: Vec<McpServerDefinition>,
}

/// One external MCP server definition: a local stdio process (`command`) or a
/// remote Streamable HTTP endpoint (`url`). Exactly one of the two is set;
/// [`validate_servers`] enforces that process fields stay with `command` and
/// `bearer_token_env` stays with `url`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpServerDefinition {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) command: Option<String>,
    #[serde(default)]
    pub(crate) args: Vec<String>,
    /// Explicit literal values. The UI labels these as non-secret.
    #[serde(default)]
    pub(crate) env: BTreeMap<String, String>,
    /// Parent environment names to forward. Their values never enter this type.
    #[serde(default)]
    pub(crate) env_from: Vec<String>,
    #[serde(default)]
    pub(crate) cwd: Option<PathBuf>,
    /// Streamable HTTP endpoint for a remote server.
    #[serde(default)]
    pub(crate) url: Option<String>,
    /// Parent environment name holding the HTTP bearer token. The value is
    /// resolved at connect time and never enters this type.
    #[serde(default)]
    pub(crate) bearer_token_env: Option<String>,
    #[serde(default = "default_request_timeout_ms")]
    pub(crate) request_timeout_ms: u64,
    #[serde(default = "enabled_by_default")]
    pub(crate) enabled: bool,
}

impl std::fmt::Debug for McpServerDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpServerDefinition")
            .field("name", &self.name)
            .field("command", &self.command)
            .field("argument_count", &self.args.len())
            .field("env_names", &self.env.keys().collect::<Vec<_>>())
            .field("env_from", &self.env_from)
            .field("cwd", &self.cwd)
            .field("url", &self.url)
            .field("bearer_token_env", &self.bearer_token_env)
            .field("request_timeout_ms", &self.request_timeout_ms)
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl McpServerDefinition {
    fn build_command(&self) -> Result<Command> {
        let Some(program) = &self.command else {
            return Err(AgentError::config(
                "MCP server definition has no command to spawn",
            ));
        };
        let mut command = Command::new(program);
        command.args(&self.args);
        // Deliberately unconditional: a renderer cannot widen a child to the
        // desktop's provider credentials or other ambient environment.
        command.env_clear();
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

    async fn connect(&self) -> Result<McpClient> {
        if let Some(url) = &self.url {
            let bearer_token = self.resolve_bearer_token()?;
            return McpClient::connect_http_with_timeouts(
                self.name.clone(),
                url,
                bearer_token.as_deref(),
                INITIALIZATION_TIMEOUT,
                Duration::from_millis(self.request_timeout_ms),
            )
            .await;
        }
        McpClient::spawn_with_timeouts(
            self.name.clone(),
            self.build_command()?,
            INITIALIZATION_TIMEOUT,
            Duration::from_millis(self.request_timeout_ms),
        )
        .await
    }

    /// Resolve the selected bearer token by name at the connection boundary.
    fn resolve_bearer_token(&self) -> Result<Option<String>> {
        let Some(name) = &self.bearer_token_env else {
            return Ok(None);
        };
        std::env::var(name).map(Some).map_err(|_| {
            AgentError::config(format!(
                "required parent environment variable {name:?} is not set"
            ))
        })
    }
}

const fn default_request_timeout_ms() -> u64 {
    DEFAULT_REQUEST_TIMEOUT_MS
}

const fn enabled_by_default() -> bool {
    true
}

/// Renderer-safe connection lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub(crate) enum McpHealth {
    Initializing,
    Healthy,
    Degraded,
    Reconnecting,
    Disabled,
}

/// One renderer-safe server projection. Resolved `env_from` values and child
/// process details are intentionally absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub(crate) struct McpServerInfo {
    #[serde(flatten)]
    pub(crate) definition: McpServerDefinition,
    pub(crate) health: McpHealth,
    pub(crate) tool_count: usize,
    pub(crate) diagnostic: Option<String>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
pub(crate) struct McpServersInfo {
    pub(crate) servers: Vec<McpServerInfo>,
}

struct ManagedServer {
    client: Option<McpClient>,
    health: McpHealth,
    diagnostic: Option<String>,
    reconnect_backoff: Duration,
    epoch: u64,
    reconnect_lock: Arc<Mutex<()>>,
}

struct RuntimeState {
    definitions: Vec<McpServerDefinition>,
    servers: HashMap<String, ManagedServer>,
}

/// Owns the current MCP connection set and atomically published tool registry.
///
/// A turn asks for one [`snapshot`](Self::snapshot) and keeps that `Arc` for its
/// entire live execution. Reconfiguration therefore affects only later turns;
/// old sessions remain alive until their last turn snapshot is dropped.
pub(crate) struct McpRuntime {
    base_tools: ToolRegistry,
    tools: RwLock<Arc<ToolRegistry>>,
    state: Mutex<RuntimeState>,
    mutation: Mutex<()>,
    store: Arc<dyn Store>,
    next_epoch: AtomicU64,
}

impl McpRuntime {
    pub(crate) fn new(base_tools: Arc<ToolRegistry>, store: Arc<dyn Store>) -> Self {
        Self {
            base_tools: (*base_tools).clone(),
            tools: RwLock::new(base_tools),
            state: Mutex::new(RuntimeState {
                definitions: Vec::new(),
                servers: HashMap::new(),
            }),
            mutation: Mutex::new(()),
            store,
            next_epoch: AtomicU64::new(1),
        }
    }

    /// Load persisted definitions when present, otherwise the legacy boot file.
    ///
    /// A boot file remains fail-closed. Persisted definitions degrade in place
    /// so the Settings UI remains available to repair a missing executable or
    /// selected environment variable.
    pub(crate) async fn initialize(&self, boot: ConfiguredMcpServers) -> Result<()> {
        match self.store.get_setting(SETTING_KEY).await? {
            Some(value) => {
                let config: McpServersConfig = serde_json::from_value(value).map_err(|error| {
                    AgentError::config(format!("invalid saved MCP config: {error}"))
                })?;
                validate_servers(&config.servers)?;
                self.replace_permissive(config.servers).await;
                Ok(())
            }
            None => self.replace_strict(boot.0, false).await.map(|_| ()),
        }
    }

    /// One immutable tool surface for a live turn.
    pub(crate) fn snapshot(&self) -> Arc<ToolRegistry> {
        self.tools
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) async fn info(&self) -> McpServersInfo {
        let state = self.state.lock().await;
        McpServersInfo {
            servers: state
                .definitions
                .iter()
                .map(|definition| {
                    let managed = state.servers.get(&definition.name);
                    McpServerInfo {
                        definition: definition.clone(),
                        health: managed.map_or(
                            if definition.enabled {
                                McpHealth::Initializing
                            } else {
                                McpHealth::Disabled
                            },
                            |server| server.health,
                        ),
                        tool_count: managed
                            .and_then(|server| server.client.as_ref())
                            .map_or(0, |client| client.tools().count()),
                        diagnostic: managed.and_then(|server| server.diagnostic.clone()),
                    }
                })
                .collect(),
        }
    }

    /// Validate and connect a complete candidate before atomically replacing the
    /// active connection set. A failed candidate leaves both persisted config
    /// and the live tool registry unchanged.
    pub(crate) async fn replace(&self, config: McpServersConfig) -> Result<McpServersInfo> {
        // Keep durable settings and the live projection in one commit order.
        // Candidate startup may be slow, but concurrent replacements must not
        // overtake one another between persistence and publication.
        let _mutation = self.mutation.lock().await;
        self.replace_strict(config.servers, true).await?;
        Ok(self.info().await)
    }

    #[cfg(test)]
    async fn replace_with_commit_pause(
        &self,
        config: McpServersConfig,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    ) -> Result<McpServersInfo> {
        let _mutation = self.mutation.lock().await;
        entered.notify_one();
        release.notified().await;
        self.replace_strict(config.servers, true).await?;
        Ok(self.info().await)
    }

    async fn replace_strict(
        &self,
        definitions: Vec<McpServerDefinition>,
        persist: bool,
    ) -> Result<()> {
        validate_servers(&definitions)?;
        let mut servers = HashMap::new();
        let connections = join_all(definitions.iter().map(|definition| async move {
            if definition.enabled {
                definition.connect().await.map(Some)
            } else {
                Ok(None)
            }
        }))
        .await;
        for (definition, connection) in definitions.iter().zip(connections) {
            let Some(client) = connection.map_err(|_| {
                AgentError::config(format!(
                    "external MCP server {} failed to start: {}",
                    definition.name,
                    connection_diagnostic(definition)
                ))
            })?
            else {
                servers.insert(
                    definition.name.clone(),
                    ManagedServer {
                        client: None,
                        health: McpHealth::Disabled,
                        diagnostic: None,
                        reconnect_backoff: INITIAL_RECONNECT_BACKOFF,
                        epoch: self.fresh_epoch(),
                        reconnect_lock: Arc::new(Mutex::new(())),
                    },
                );
                continue;
            };
            servers.insert(
                definition.name.clone(),
                ManagedServer {
                    client: Some(client),
                    health: McpHealth::Healthy,
                    diagnostic: None,
                    reconnect_backoff: INITIAL_RECONNECT_BACKOFF,
                    epoch: self.fresh_epoch(),
                    reconnect_lock: Arc::new(Mutex::new(())),
                },
            );
        }
        if persist {
            self.store
                .set_setting(
                    SETTING_KEY,
                    &serde_json::to_value(McpServersConfig {
                        servers: definitions.clone(),
                    })?,
                )
                .await?;
        }
        self.publish(definitions, servers).await;
        Ok(())
    }

    async fn replace_permissive(&self, definitions: Vec<McpServerDefinition>) {
        let mut servers = HashMap::new();
        let connections = join_all(definitions.iter().map(|definition| async move {
            if definition.enabled {
                definition.connect().await.map(Some)
            } else {
                Ok(None)
            }
        }))
        .await;
        for (definition, connection) in definitions.iter().zip(connections) {
            let managed = match connection {
                Ok(None) => ManagedServer {
                    client: None,
                    health: McpHealth::Disabled,
                    diagnostic: None,
                    reconnect_backoff: INITIAL_RECONNECT_BACKOFF,
                    epoch: self.fresh_epoch(),
                    reconnect_lock: Arc::new(Mutex::new(())),
                },
                Ok(Some(client)) => ManagedServer {
                    client: Some(client),
                    health: McpHealth::Healthy,
                    diagnostic: None,
                    reconnect_backoff: INITIAL_RECONNECT_BACKOFF,
                    epoch: self.fresh_epoch(),
                    reconnect_lock: Arc::new(Mutex::new(())),
                },
                Err(_) => ManagedServer {
                    client: None,
                    health: McpHealth::Degraded,
                    diagnostic: Some(connection_diagnostic(definition)),
                    reconnect_backoff: INITIAL_RECONNECT_BACKOFF,
                    epoch: self.fresh_epoch(),
                    reconnect_lock: Arc::new(Mutex::new(())),
                },
            };
            servers.insert(definition.name.clone(), managed);
        }
        self.publish(definitions, servers).await;
    }

    async fn publish(
        &self,
        definitions: Vec<McpServerDefinition>,
        servers: HashMap<String, ManagedServer>,
    ) {
        let registry = self.registry_for(&servers);
        let mut state = self.state.lock().await;
        state.definitions = definitions;
        state.servers = servers;
        *self
            .tools
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(registry);
    }

    fn registry_for(&self, servers: &HashMap<String, ManagedServer>) -> ToolRegistry {
        let mut registry = self.base_tools.clone();
        for server in servers.values() {
            if let Some(client) = &server.client {
                client.mount(&mut registry);
            }
        }
        registry
    }

    /// Force a fresh connection and tool discovery for one configured server.
    pub(crate) async fn reconnect(&self, name: &str) -> Result<McpServersInfo> {
        self.reconnect_if_epoch(name, None).await
    }

    async fn reconnect_if_epoch(
        &self,
        name: &str,
        expected_epoch: Option<u64>,
    ) -> Result<McpServersInfo> {
        // Serialize connection attempts for the same published server without
        // blocking unrelated servers. A waiter captures the current epoch, so
        // it returns the first attempt's result instead of launching a duplicate
        // child after the lock becomes available.
        let (reconnect_lock, requested_epoch) = {
            let state = self.state.lock().await;
            let definition = state
                .definitions
                .iter()
                .find(|definition| definition.name == name)
                .ok_or_else(|| AgentError::config("MCP server not found"))?;
            if !definition.enabled {
                return Err(AgentError::config("disabled MCP server cannot reconnect"));
            }
            let server = state
                .servers
                .get(name)
                .ok_or_else(|| AgentError::config("MCP server runtime is missing"))?;
            if expected_epoch.is_some_and(|epoch| epoch != server.epoch) {
                return Ok(self.info_locked(&state));
            }
            (server.reconnect_lock.clone(), server.epoch)
        };
        let _reconnect = reconnect_lock.lock().await;
        let (definition, start_epoch) = {
            let mut state = self.state.lock().await;
            let definition = state
                .definitions
                .iter()
                .find(|definition| definition.name == name)
                .cloned()
                .ok_or_else(|| AgentError::config("MCP server not found"))?;
            if !definition.enabled {
                return Err(AgentError::config("disabled MCP server cannot reconnect"));
            }
            let Some(server) = state.servers.get_mut(name) else {
                return Err(AgentError::config("MCP server runtime is missing"));
            };
            if server.epoch != requested_epoch
                || expected_epoch.is_some_and(|epoch| epoch != server.epoch)
            {
                return Ok(self.info_locked(&state));
            }
            server.health = McpHealth::Reconnecting;
            server.diagnostic = None;
            (definition, server.epoch)
        };
        match definition.connect().await {
            Ok(client) => {
                let mut state = self.state.lock().await;
                // A settings replacement may have won while the process started.
                if state
                    .definitions
                    .iter()
                    .find(|candidate| candidate.name == name)
                    != Some(&definition)
                    || state
                        .servers
                        .get(name)
                        .is_none_or(|server| server.epoch != start_epoch)
                {
                    return Ok(self.info_locked(&state));
                }
                let server = state
                    .servers
                    .entry(name.to_string())
                    .or_insert(ManagedServer {
                        client: None,
                        health: McpHealth::Initializing,
                        diagnostic: None,
                        reconnect_backoff: INITIAL_RECONNECT_BACKOFF,
                        epoch: self.fresh_epoch(),
                        reconnect_lock: Arc::new(Mutex::new(())),
                    });
                server.client = Some(client);
                server.health = McpHealth::Healthy;
                server.diagnostic = None;
                server.reconnect_backoff = INITIAL_RECONNECT_BACKOFF;
                server.epoch = self.fresh_epoch();
                let registry = self.registry_for(&state.servers);
                *self
                    .tools
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(registry);
                Ok(self.info_locked(&state))
            }
            Err(_) => {
                let diagnostic = connection_diagnostic(&definition);
                let mut state = self.state.lock().await;
                if let Some(server) = state
                    .servers
                    .get_mut(name)
                    .filter(|server| server.epoch == start_epoch)
                {
                    server.client = None;
                    server.health = McpHealth::Degraded;
                    server.diagnostic = Some(diagnostic.clone());
                    server.reconnect_backoff = server
                        .reconnect_backoff
                        .saturating_mul(2)
                        .min(MAX_RECONNECT_BACKOFF);
                    server.epoch = self.fresh_epoch();
                } else {
                    return Ok(self.info_locked(&state));
                }
                let registry = self.registry_for(&state.servers);
                *self
                    .tools
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(registry);
                Err(AgentError::config(format!(
                    "external MCP server {name} failed to reconnect: {diagnostic}"
                )))
            }
        }
    }

    /// Monitor healthy sessions and reconnect degraded or tool-changed servers
    /// with capped exponential backoff.
    pub(crate) async fn supervise(self: Arc<Self>) {
        loop {
            tokio::time::sleep(HEALTH_INTERVAL).await;
            let probes = {
                let state = self.state.lock().await;
                state
                    .definitions
                    .iter()
                    .filter(|definition| definition.enabled)
                    .filter_map(|definition| {
                        state.servers.get(&definition.name).map(|server| {
                            (
                                definition.name.clone(),
                                server.client.clone(),
                                server.reconnect_backoff,
                                server.epoch,
                            )
                        })
                    })
                    .collect::<Vec<_>>()
            };
            join_all(probes.into_iter().map(|(name, client, backoff, epoch)| {
                let runtime = self.clone();
                async move {
                    let refresh = match client {
                        Some(client) => {
                            match tokio::time::timeout(HEALTH_PROBE_TIMEOUT, client.probe()).await {
                                Ok(Ok(McpProbe::Busy)) => false,
                                Ok(Ok(McpProbe::Ready { tools_list_changed })) => {
                                    tools_list_changed
                                }
                                Ok(Err(_)) | Err(_) => {
                                    runtime.mark_degraded(&name, epoch, backoff).await;
                                    true
                                }
                            }
                        }
                        None => true,
                    };
                    if refresh {
                        tokio::time::sleep(backoff).await;
                        let _ = runtime.reconnect_if_epoch(&name, Some(epoch)).await;
                    }
                }
            }))
            .await;
        }
    }

    async fn mark_degraded(&self, name: &str, epoch: u64, backoff: Duration) {
        let mut state = self.state.lock().await;
        if let Some(server) = state
            .servers
            .get_mut(name)
            .filter(|server| server.epoch == epoch)
        {
            server.client = None;
            server.health = McpHealth::Degraded;
            server.diagnostic =
                Some("Health check failed. OpenWave will retry this server.".to_string());
            server.reconnect_backoff = backoff;
        }
        let registry = self.registry_for(&state.servers);
        *self
            .tools
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(registry);
    }

    fn fresh_epoch(&self) -> u64 {
        self.next_epoch.fetch_add(1, Ordering::Relaxed)
    }

    fn info_locked(&self, state: &RuntimeState) -> McpServersInfo {
        McpServersInfo {
            servers: state
                .definitions
                .iter()
                .map(|definition| {
                    let managed = state.servers.get(&definition.name);
                    McpServerInfo {
                        definition: definition.clone(),
                        health: managed.map_or(McpHealth::Initializing, |server| server.health),
                        tool_count: managed
                            .and_then(|server| server.client.as_ref())
                            .map_or(0, |client| client.tools().count()),
                        diagnostic: managed.and_then(|server| server.diagnostic.clone()),
                    }
                })
                .collect(),
        }
    }
}

fn validate_servers(servers: &[McpServerDefinition]) -> Result<()> {
    if servers.len() > MAX_SERVERS {
        return Err(AgentError::config(format!(
            "MCP config contains more than {MAX_SERVERS} servers"
        )));
    }
    let mut names = HashSet::new();
    for server in servers {
        validate_name(&server.name)?;
        match (&server.command, &server.url) {
            (Some(_), Some(_)) => {
                return Err(server_error(
                    &server.name,
                    "must configure either command or url, not both",
                ));
            }
            (None, None) => {
                return Err(server_error(
                    &server.name,
                    "must configure a command or a url",
                ));
            }
            (Some(command), None) => {
                validate_process_string(&server.name, "command", command)?;
                if command.is_empty() {
                    return Err(server_error(&server.name, "command must not be empty"));
                }
                if server.bearer_token_env.is_some() {
                    return Err(server_error(
                        &server.name,
                        "bearer_token_env applies only to url servers",
                    ));
                }
            }
            (None, Some(url)) => {
                validate_process_string(&server.name, "url", url)?;
                openwave_mcp::validate_http_url(url)
                    .map_err(|error| server_error(&server.name, error))?;
                if !server.args.is_empty() {
                    return Err(server_error(
                        &server.name,
                        "args apply only to command servers",
                    ));
                }
                if !server.env.is_empty() || !server.env_from.is_empty() {
                    return Err(server_error(
                        &server.name,
                        "process environment applies only to command servers",
                    ));
                }
                if server.cwd.is_some() {
                    return Err(server_error(
                        &server.name,
                        "cwd applies only to command servers",
                    ));
                }
                if let Some(bearer_name) = &server.bearer_token_env {
                    validate_environment_name(&server.name, bearer_name)?;
                }
            }
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
        if let Some(path) = server.cwd.as_ref().and_then(|path| path.to_str()) {
            validate_process_string(&server.name, "working directory", path)?;
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
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > MAX_SERVER_NAME_BYTES
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
    if value.len() > MAX_PROCESS_STRING_BYTES {
        return Err(server_error(
            name,
            format!("{field} exceeds {MAX_PROCESS_STRING_BYTES} bytes"),
        ));
    }
    if value.contains('\0') {
        return Err(server_error(name, format!("{field} must not contain NUL")));
    }
    Ok(())
}

fn validate_environment_name(server_name: &str, name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > MAX_ENVIRONMENT_NAME_BYTES
        || name.contains('=')
        || name.contains('\0')
    {
        return Err(server_error(
            server_name,
            format!("invalid environment variable name {name:?}"),
        ));
    }
    Ok(())
}

fn connection_diagnostic(definition: &McpServerDefinition) -> String {
    if let Some(name) = definition
        .env_from
        .iter()
        .chain(&definition.bearer_token_env)
        .find(|name| std::env::var_os(name).is_none())
    {
        return format!("Required parent environment variable {name:?} is not set.");
    }
    if definition.url.is_some() {
        return "Could not connect to this server. Check its URL and credentials.".to_string();
    }
    "Could not initialize this server. Check its executable, arguments, and working directory."
        .to_string()
}

fn server_error(name: &str, message: impl std::fmt::Display) -> AgentError {
    AgentError::config(format!("invalid external MCP server {name:?}: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwave_core::DbStore;

    fn parse(json: &str) -> Result<ConfiguredMcpServers> {
        let config: McpServersConfig = serde_json::from_str(json)?;
        validate_servers(&config.servers)?;
        Ok(ConfiguredMcpServers(config.servers))
    }

    fn disabled_definition(name: &str, command: &str) -> McpServerDefinition {
        McpServerDefinition {
            name: name.to_string(),
            command: Some(command.to_string()),
            args: Vec::new(),
            env: BTreeMap::new(),
            env_from: Vec::new(),
            cwd: None,
            url: None,
            bearer_token_env: None,
            request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
            enabled: false,
        }
    }

    fn http_definition(name: &str, url: &str) -> McpServerDefinition {
        McpServerDefinition {
            name: name.to_string(),
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            env_from: Vec::new(),
            cwd: None,
            url: Some(url.to_string()),
            bearer_token_env: None,
            request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
            enabled: true,
        }
    }

    async fn test_runtime() -> (Arc<McpRuntime>, Arc<dyn Store>, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("mcp.db").display()
            ))
            .await
            .unwrap(),
        );
        (
            Arc::new(McpRuntime::new(
                Arc::new(ToolRegistry::new()),
                store.clone(),
            )),
            store,
            directory,
        )
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
        assert_eq!(server.command.as_deref(), Some("/usr/local/bin/docs-mcp"));
        assert_eq!(server.args, ["--stdio"]);
        assert_eq!(server.env.get("LOG_LEVEL").unwrap(), "info");
        assert_eq!(server.env_from, ["DOCS_TOKEN"]);
        assert_eq!(server.cwd.as_deref(), Some(Path::new("/srv/docs")));
        assert_eq!(server.request_timeout_ms, 2500);
        assert!(server.enabled);
    }

    #[test]
    fn defaults_to_an_isolated_environment_and_sixty_second_timeout() {
        let config = parse(r#"{"servers":[{"name":"docs","command":"/bin/docs"}]}"#).unwrap();
        let server = &config.0[0];
        assert!(server.args.is_empty());
        assert!(server.env.is_empty());
        assert!(server.env_from.is_empty());
        assert_eq!(server.request_timeout_ms, 60_000);
        let command = server.build_command().unwrap();
        assert!(command.as_std().get_envs().next().is_none());
    }

    #[test]
    fn rejects_environment_inheritance_and_unknown_fields() {
        let error =
            parse(r#"{"servers":[{"name":"docs","command":"/bin/docs","inherit_env":true}]}"#)
                .err()
                .unwrap();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_duplicate_names_unsafe_strings_and_timeouts() {
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
    async fn missing_selected_parent_environment_fails_before_spawn_without_a_value() {
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
        let error = config.0[0].connect().await.err().unwrap();
        assert!(error.to_string().contains(MISSING));
        assert!(error.to_string().contains("is not set"));
        assert!(!error.to_string().contains("secret-value"));
    }

    #[test]
    fn projected_diagnostics_are_fixed_or_name_only() {
        const MISSING: &str = "OPENWAVE_TEST_MCP_DIAGNOSTIC_MISSING_13B2";
        assert!(std::env::var_os(MISSING).is_none());
        let config = parse(&format!(
            r#"{{"servers":[{{"name":"docs","command":"/bin/docs","env_from":["{MISSING}"]}}]}}"#
        ))
        .unwrap();
        let missing = connection_diagnostic(&config.0[0]);
        assert!(missing.contains(MISSING));
        assert!(!missing.contains('\n'));

        let generic = parse(r#"{"servers":[{"name":"docs","command":"/bin/docs"}]}"#).unwrap();
        assert_eq!(
            connection_diagnostic(&generic.0[0]),
            "Could not initialize this server. Check its executable, arguments, and working directory."
        );
    }

    #[tokio::test]
    async fn concurrent_replacements_keep_durable_and_live_configuration_in_commit_order() {
        let (runtime, store, _directory) = test_runtime().await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let first = {
            let runtime = runtime.clone();
            let entered = entered.clone();
            let release = release.clone();
            tokio::spawn(async move {
                runtime
                    .replace_with_commit_pause(
                        McpServersConfig {
                            servers: vec![disabled_definition("first", "/bin/first")],
                        },
                        entered,
                        release,
                    )
                    .await
            })
        };
        entered.notified().await;
        let mut second = {
            let runtime = runtime.clone();
            tokio::spawn(async move {
                runtime
                    .replace(McpServersConfig {
                        servers: vec![disabled_definition("second", "/bin/second")],
                    })
                    .await
            })
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut second)
                .await
                .is_err(),
            "second replacement bypassed the fence"
        );
        release.notify_one();
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();

        let saved: McpServersConfig =
            serde_json::from_value(store.get_setting(SETTING_KEY).await.unwrap().unwrap()).unwrap();
        let live = runtime.info().await;
        assert_eq!(saved.servers[0].name, "second");
        assert_eq!(live.servers[0].definition, saved.servers[0]);
    }

    #[tokio::test]
    async fn stale_supervisor_result_cannot_overwrite_a_replacement() {
        let (runtime, _store, _directory) = test_runtime().await;
        runtime
            .replace(McpServersConfig {
                servers: vec![disabled_definition("docs", "/bin/old")],
            })
            .await
            .unwrap();
        let old_epoch = runtime
            .state
            .lock()
            .await
            .servers
            .get("docs")
            .unwrap()
            .epoch;
        runtime
            .replace(McpServersConfig {
                servers: vec![disabled_definition("docs", "/bin/new")],
            })
            .await
            .unwrap();

        runtime
            .mark_degraded("docs", old_epoch, INITIAL_RECONNECT_BACKOFF)
            .await;
        let info = runtime.info().await;
        assert_eq!(
            info.servers[0].definition.command.as_deref(),
            Some("/bin/new")
        );
        assert_eq!(info.servers[0].health, McpHealth::Disabled);
        assert!(info.servers[0].diagnostic.is_none());
    }

    #[tokio::test]
    async fn concurrent_reconnects_share_one_attempt_for_a_published_server() {
        let (runtime, _store, _directory) = test_runtime().await;
        let mut definition = disabled_definition("docs", "/usr/bin/true");
        definition.enabled = true;
        runtime.replace_permissive(vec![definition]).await;
        let reconnect_lock = runtime
            .state
            .lock()
            .await
            .servers
            .get("docs")
            .unwrap()
            .reconnect_lock
            .clone();
        let held = reconnect_lock.lock().await;
        let first = {
            let runtime = runtime.clone();
            tokio::spawn(async move { runtime.reconnect("docs").await })
        };
        let second = {
            let runtime = runtime.clone();
            tokio::spawn(async move { runtime.reconnect("docs").await })
        };
        tokio::time::timeout(Duration::from_secs(1), async {
            while Arc::strong_count(&reconnect_lock) < 4 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("both reconnects should wait on the published server lock");
        drop(held);

        let first = first.await.unwrap();
        let second = second.await.unwrap();
        assert_ne!(
            first.is_err(),
            second.is_err(),
            "exactly one caller should perform the failed connection attempt"
        );
        assert_eq!(runtime.info().await.servers[0].health, McpHealth::Degraded);
    }

    #[test]
    fn parses_a_streamable_http_server_configuration() {
        let config = parse(
            r#"{
                "servers": [{
                    "name": "gateway",
                    "url": "http://127.0.0.1:28081/mcp/tools",
                    "bearer_token_env": "GATEWAY_TOKEN",
                    "request_timeout_ms": 2500
                }]
            }"#,
        )
        .unwrap();
        let server = &config.0[0];
        assert_eq!(
            server.url.as_deref(),
            Some("http://127.0.0.1:28081/mcp/tools")
        );
        assert_eq!(server.bearer_token_env.as_deref(), Some("GATEWAY_TOKEN"));
        assert!(server.command.is_none());
    }

    #[test]
    fn each_server_is_exactly_one_transport() {
        let both = parse(
            r#"{"servers":[{"name":"docs","command":"/bin/docs","url":"http://127.0.0.1/mcp"}]}"#,
        )
        .err()
        .unwrap();
        assert!(both.to_string().contains("not both"));

        let neither = parse(r#"{"servers":[{"name":"docs"}]}"#).err().unwrap();
        assert!(neither.to_string().contains("command or a url"));
    }

    #[test]
    fn transport_specific_fields_stay_with_their_transport() {
        let bearer_on_stdio = parse(
            r#"{"servers":[{"name":"docs","command":"/bin/docs","bearer_token_env":"TOKEN"}]}"#,
        )
        .err()
        .unwrap();
        assert!(bearer_on_stdio.to_string().contains("only to url servers"));

        for (field, fragment) in [
            (r#""args":["--stdio"]"#, "args apply only"),
            (r#""env":{"A":"b"}"#, "environment applies only"),
            (r#""env_from":["TOKEN"]"#, "environment applies only"),
            (r#""cwd":"/srv""#, "cwd applies only"),
        ] {
            let error = parse(&format!(
                r#"{{"servers":[{{"name":"docs","url":"http://127.0.0.1/mcp",{field}}}]}}"#
            ))
            .err()
            .unwrap();
            assert!(error.to_string().contains(fragment), "{field}: {error}");
        }
    }

    #[test]
    fn rejects_invalid_http_urls() {
        for url in ["ftp://host/mcp", "http://user:secret@host/mcp", "not a url"] {
            let error = parse(&format!(
                r#"{{"servers":[{{"name":"docs","url":"{url}"}}]}}"#
            ))
            .err()
            .unwrap();
            assert!(!error.to_string().contains("secret"), "{url}: {error}");
        }
    }

    #[tokio::test]
    async fn missing_selected_bearer_token_fails_by_name_without_a_value() {
        const MISSING: &str = "OPENWAVE_TEST_MCP_BEARER_MUST_NOT_EXIST_8A31";
        assert!(std::env::var_os(MISSING).is_none());
        let mut definition = http_definition("gateway", "http://127.0.0.1:1/mcp");
        definition.bearer_token_env = Some(MISSING.to_string());
        let error = definition.connect().await.err().unwrap();
        assert!(error.to_string().contains(MISSING));
        assert!(error.to_string().contains("is not set"));

        let diagnostic = connection_diagnostic(&definition);
        assert!(diagnostic.contains(MISSING));
        assert!(!diagnostic.contains('\n'));
    }

    #[test]
    fn http_diagnostics_are_fixed_strings_without_the_url() {
        let definition = http_definition("gateway", "http://127.0.0.1:9/mcp");
        let diagnostic = connection_diagnostic(&definition);
        assert_eq!(
            diagnostic,
            "Could not connect to this server. Check its URL and credentials."
        );
    }

    async fn serve_fake_http_mcp() -> std::net::SocketAddr {
        use axum::http::HeaderMap;
        use axum::routing::post;

        async fn handler(
            headers: HeaderMap,
            body: String,
        ) -> ([(&'static str, &'static str); 1], String) {
            // The config layer resolved the selected variable into the header.
            let expected = format!("Bearer {}", std::env::var("PATH").unwrap());
            assert_eq!(
                headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok()),
                Some(expected.as_str())
            );
            let request: serde_json::Value = serde_json::from_str(&body).unwrap();
            let id = request.get("id").cloned().unwrap_or_default();
            let result = match request["method"].as_str().unwrap_or_default() {
                "initialize" => serde_json::json!({
                    "protocolVersion": openwave_mcp::PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "config-fixture", "version": "1"}
                }),
                "tools/list" => serde_json::json!({
                    "tools": [{
                        "name": "lookup",
                        "description": "Look something up",
                        "inputSchema": {"type": "object"}
                    }]
                }),
                _ => serde_json::json!({}),
            };
            (
                [("content-type", "application/json")],
                serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
            )
        }

        let app = axum::Router::new().route("/mcp", post(handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        address
    }

    #[tokio::test]
    async fn replaces_with_a_streamable_http_server_and_mounts_its_tools() {
        let address = serve_fake_http_mcp().await;
        let (runtime, _store, _directory) = test_runtime().await;
        let mut definition = http_definition("gateway", &format!("http://{address}/mcp"));
        // PATH always exists, so the selected-name path is exercised for real
        // without mutating the test process environment.
        definition.bearer_token_env = Some("PATH".to_string());
        runtime
            .replace(McpServersConfig {
                servers: vec![definition],
            })
            .await
            .unwrap();

        let info = runtime.info().await;
        assert_eq!(info.servers[0].health, McpHealth::Healthy);
        assert_eq!(info.servers[0].tool_count, 1);
        assert!(runtime.snapshot().get("mcp__gateway__lookup").is_some());
    }

    #[test]
    fn debug_projection_redacts_argument_and_literal_environment_values() {
        let mut definition = disabled_definition("docs", "/bin/docs");
        definition.args = vec!["argument-secret".to_string()];
        definition
            .env
            .insert("TOKEN".to_string(), "literal-secret".to_string());
        let debug = format!("{definition:?}");
        assert!(!debug.contains("argument-secret"));
        assert!(!debug.contains("literal-secret"));
        assert!(debug.contains("TOKEN"));
    }
}
