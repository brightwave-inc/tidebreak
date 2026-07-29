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
/// How long a minted view-frame token stays redeemable. One iframe load
/// consumes it; a remount mints a fresh one.
const VIEW_FRAME_TOKEN_TTL: Duration = Duration::from_secs(60);
const MAX_VIEW_FRAME_TOKENS: usize = 64;

/// The diagnostic every manual (command/url) server carries while managed
/// policy holds. The definitions stay persisted — inert, not deleted — so an
/// unprovisioned profile is byte-for-byte unaffected and the list stays
/// legible instead of servers silently vanishing.
pub(crate) const MANAGED_DISABLED_DIAGNOSTIC: &str =
    "Disabled by managed policy. Gateway-managed MCP endpoints remain available.";

/// Validated external servers selected by the legacy boot file.
#[derive(Default)]
pub(crate) struct ConfiguredMcpServers(Vec<McpServerDefinition>);

impl ConfiguredMcpServers {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

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

/// One external MCP server definition: a local stdio process (`command`), a
/// remote Streamable HTTP endpoint (`url`), or a gateway-managed endpoint
/// (`gateway_endpoint`). Exactly one of the three is set;
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
    /// Endpoint slug of a gateway MCP endpoint, mounted through the signed-in
    /// model-gateway session. The endpoint URL and its short-lived bearer are
    /// resolved from the session at every connection and never enter this
    /// type.
    #[serde(default)]
    pub(crate) gateway_endpoint: Option<String>,
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
            .field("gateway_endpoint", &self.gateway_endpoint)
            .field("request_timeout_ms", &self.request_timeout_ms)
            .field("enabled", &self.enabled)
            .finish()
    }
}

/// Resolves gateway-managed MCP endpoints at the connection boundary: the
/// endpoint URL and a fresh session bearer for `mcp:<slug>`. Implemented by
/// the gateway runtime over the signed-in session; token renewal stays inside
/// the connector's rotation lock and no token value is ever stored here.
#[async_trait::async_trait]
pub(crate) trait GatewayEndpoints: Send + Sync {
    async fn endpoint(&self, slug: &str) -> Result<GatewayEndpointAccess>;
}

/// One resolved gateway endpoint: where to connect and the bearer to present.
/// Deliberately no `Debug`/`Serialize`: the token exists only on the path
/// from resolution into the transport's prebuilt header.
pub(crate) struct GatewayEndpointAccess {
    pub(crate) url: String,
    pub(crate) bearer_token: String,
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

    async fn connect(&self, gateway: &dyn GatewayEndpoints) -> Result<McpClient> {
        if let Some(slug) = &self.gateway_endpoint {
            // Resolved per connection: a reconnect always presents a token
            // that is fresh at that moment, so expiry is survived by the
            // ordinary supervision/reconnect cycle.
            let access = gateway.endpoint(slug).await?;
            return McpClient::connect_http_with_timeouts(
                self.name.clone(),
                &access.url,
                Some(&access.bearer_token),
                INITIALIZATION_TIMEOUT,
                Duration::from_millis(self.request_timeout_ms),
            )
            .await;
        }
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

    /// Connect and prefetch every declared MCP Apps view document.
    ///
    /// Views are fetched once per connection and served from memory: they are
    /// re-fetchable templates, not evidence, so a reconnect refreshes them and
    /// a fetch failure just leaves that view unavailable (the transcript card
    /// degrades; tools are unaffected).
    async fn connect_with_views(
        &self,
        gateway: &dyn GatewayEndpoints,
    ) -> Result<(McpClient, HashMap<String, UiViewDocument>)> {
        let client = self.connect(gateway).await?;
        let uris: HashSet<String> = client
            .tools()
            .filter_map(|spec| client.ui_resource_uri(&spec.name))
            .map(str::to_string)
            .collect();
        let mut views = HashMap::new();
        for uri in uris {
            let Ok(content) = client.read_resource(&uri).await else {
                continue;
            };
            // MCP Apps views are HTML text; a binary body has no sandbox
            // story, and a non-HTML mime must not be served as a document.
            let Some(html) = content.text else { continue };
            if !content
                .mime_type
                .as_deref()
                .is_none_or(|mime| mime.starts_with("text/html"))
            {
                continue;
            }
            views.insert(
                uri,
                UiViewDocument {
                    mime_type: content.mime_type,
                    html,
                },
            );
        }
        Ok((client, views))
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

/// What a policy-aware replacement did.
pub(crate) enum McpReplaceOutcome {
    Replaced(McpServersInfo),
    /// Managed policy refused these manual servers. Nothing changed.
    RefusedManual(Vec<String>),
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
    /// Prefetched MCP Apps view documents, keyed by declared `ui://` URI.
    ui_views: HashMap<String, UiViewDocument>,
}

/// One prefetched MCP Apps view document, served to the renderer only through
/// the dedicated view route and rendered only inside its sandboxed frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct UiViewDocument {
    pub(crate) mime_type: Option<String>,
    pub(crate) html: String,
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
    /// Resolves gateway-managed endpoints at every connection.
    gateway: Arc<dyn GatewayEndpoints>,
    /// The OS authority for managed-mode resolution. Managed policy locks the
    /// manual transports; the gateway-endpoint transport is the sanctioned
    /// path and stays open.
    os_policy: Arc<dyn crate::managed_policy::OsPolicySource>,
    next_epoch: AtomicU64,
    /// Outstanding single-use view-frame tokens: token → (server, uri, minted).
    view_frame_tokens: Mutex<HashMap<uuid::Uuid, (String, String, std::time::Instant)>>,
}

impl McpRuntime {
    pub(crate) fn new(
        base_tools: Arc<ToolRegistry>,
        store: Arc<dyn Store>,
        gateway: Arc<dyn GatewayEndpoints>,
        os_policy: Arc<dyn crate::managed_policy::OsPolicySource>,
    ) -> Self {
        Self {
            base_tools: (*base_tools).clone(),
            tools: RwLock::new(base_tools),
            state: Mutex::new(RuntimeState {
                definitions: Vec::new(),
                servers: HashMap::new(),
            }),
            mutation: Mutex::new(()),
            store,
            gateway,
            os_policy,
            next_epoch: AtomicU64::new(1),
            view_frame_tokens: Mutex::new(HashMap::new()),
        }
    }

    /// Whether managed policy currently locks the manual transports.
    ///
    /// Read per operation rather than cached at boot, like every other policy
    /// consumer, so an MDM push or removal takes effect without a restart. An
    /// unreadable policy fails closed to locked — the same judgment the BYOK
    /// boot paths make.
    async fn manual_transports_locked(&self) -> bool {
        match crate::managed_policy::resolve(&*self.store, &*self.os_policy).await {
            Ok(policy) => policy.managed,
            Err(error) => {
                tracing::warn!(
                    "managed policy is unreadable; locking manual MCP transports: {error}"
                );
                true
            }
        }
    }

    /// The names in `candidate` that would add or change a manual (stdio or
    /// HTTP) server relative to what is already configured.
    ///
    /// Managed lockdown refuses these rather than every manual definition in
    /// the body: a profile that carried manual servers before it was managed
    /// keeps them (inert, see [`MANAGED_DISABLED_DIAGNOSTIC`]), and the MCP
    /// servers page — which saves the complete server list to mount a gateway
    /// endpoint — is not blocked by their presence. Removing one is a
    /// candidate without it, so nothing is trapped in the configuration.
    ///
    /// "Unchanged" is the whole definition, by equality: a flipped `enabled`,
    /// a renamed server, a widened timeout are all edits.
    async fn manual_additions(&self, candidate: &McpServersConfig) -> Vec<String> {
        let existing = &self.state.lock().await.definitions;
        candidate
            .servers
            .iter()
            .filter(|server| server.gateway_endpoint.is_none())
            .filter(|server| !existing.iter().any(|current| current == *server))
            .map(|server| server.name.clone())
            .collect()
    }

    /// Take down every manual server the managed lockdown now covers.
    ///
    /// Policy is resolved live, so a profile can become managed with manual
    /// children already running — an MDM push, or the deep-link pairing flow
    /// mid-session. Their connections are dropped and their tools leave the
    /// registry here; without this the decision would change without the
    /// effect, and a locked server would keep serving turns until the process
    /// restarted. Idempotent, and a no-op on an unmanaged profile.
    ///
    /// Returns whether anything was taken down.
    pub(crate) async fn enforce_manual_lockdown(&self) -> bool {
        if !self.manual_transports_locked().await {
            return false;
        }
        self.take_down_locked_manual_servers().await
    }

    async fn take_down_locked_manual_servers(&self) -> bool {
        let mut state = self.state.lock().await;
        let locked: Vec<String> = state
            .definitions
            .iter()
            .filter(|definition| definition.gateway_endpoint.is_none())
            .map(|definition| definition.name.clone())
            .collect();
        let mut torn_down = false;
        for name in locked {
            let Some(server) = state.servers.get_mut(&name) else {
                continue;
            };
            if server.client.is_none()
                && server.health == McpHealth::Disabled
                && server.diagnostic.as_deref() == Some(MANAGED_DISABLED_DIAGNOSTIC)
            {
                continue;
            }
            server.client = None;
            server.ui_views = HashMap::new();
            server.health = McpHealth::Disabled;
            server.diagnostic = Some(MANAGED_DISABLED_DIAGNOSTIC.to_string());
            // A reconnect that started before the flip lands on a stale epoch
            // and abandons its result instead of republishing what was just
            // torn down.
            server.epoch = self.fresh_epoch();
            torn_down = true;
        }
        if torn_down {
            let registry = self.registry_for(&state.servers);
            *self
                .tools
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(registry);
        }
        torn_down
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
            None => {
                // The boot file is a host-environment artifact: on a managed
                // profile it is exactly the channel the lockdown exists to
                // close, so it is inert rather than partially honored. The
                // warning is the operator's diagnostic for the silence.
                if !boot.is_empty() && self.manual_transports_locked().await {
                    tracing::warn!(
                        "{CONFIG_ENV} is ignored on a managed profile; \
                         mount MCP endpoints from the model gateway instead"
                    );
                    return Ok(());
                }
                self.replace_strict(boot.0, false).await.map(|_| ())
            }
        }
    }

    /// One prefetched MCP Apps view document, when the named server is
    /// connected and declared it.
    pub(crate) async fn ui_view_document(&self, server: &str, uri: &str) -> Option<UiViewDocument> {
        let state = self.state.lock().await;
        state.servers.get(server)?.ui_views.get(uri).cloned()
    }

    /// Mint a single-use, short-lived token addressing one prefetched view.
    ///
    /// An iframe cannot carry the API bearer, so the frame route is reached
    /// by capability instead: the authenticated renderer trades its bearer
    /// for a token here, and the unauthenticated frame route redeems it
    /// exactly once within [`VIEW_FRAME_TOKEN_TTL`].
    pub(crate) async fn mint_view_frame(&self, server: &str, uri: &str) -> Option<uuid::Uuid> {
        self.ui_view_document(server, uri).await?;
        let token = uuid::Uuid::new_v4();
        let mut tokens = self.view_frame_tokens.lock().await;
        let now = std::time::Instant::now();
        tokens.retain(|_, (_, _, minted)| now.duration_since(*minted) < VIEW_FRAME_TOKEN_TTL);
        if tokens.len() >= MAX_VIEW_FRAME_TOKENS {
            return None;
        }
        tokens.insert(token, (server.to_string(), uri.to_string(), now));
        Some(token)
    }

    /// Redeem a frame token, consuming it.
    pub(crate) async fn take_view_frame(&self, token: uuid::Uuid) -> Option<UiViewDocument> {
        let (server, uri) = {
            let mut tokens = self.view_frame_tokens.lock().await;
            let (server, uri, minted) = tokens.remove(&token)?;
            if minted.elapsed() >= VIEW_FRAME_TOKEN_TTL {
                return None;
            }
            (server, uri)
        };
        self.ui_view_document(&server, &uri).await
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

    /// The unmanaged shape of [`replace_under_policy`](Self::replace_under_policy),
    /// whose refusal arm is unreachable. Production has one entry point; this
    /// keeps the tests that predate the policy check reading as they did.
    #[cfg(test)]
    pub(crate) async fn replace(&self, config: McpServersConfig) -> Result<McpServersInfo> {
        match self.replace_under_policy(config, false).await? {
            McpReplaceOutcome::Replaced(info) => Ok(info),
            McpReplaceOutcome::RefusedManual(_) => {
                unreachable!("an unmanaged replacement is never refused")
            }
        }
    }

    /// Validate and connect a complete candidate, then atomically replace the
    /// active connection set — with the managed-lockdown admission check in
    /// the same critical section as the commit.
    ///
    /// A failed candidate leaves both persisted config and the live tool
    /// registry unchanged. Keeping durable settings and the live projection in
    /// one commit order matters under concurrency: candidate startup may be
    /// slow, but concurrent replacements must not overtake one another between
    /// persistence and publication.
    ///
    /// The admission check reads the current definition set, so running it
    /// outside the mutation lock would let a concurrent save move that set
    /// between the verdict and the commit — admitting a manual definition the
    /// policy refuses. Refusing changes nothing at all: it happens before
    /// validation and before any child is started.
    pub(crate) async fn replace_under_policy(
        &self,
        config: McpServersConfig,
        managed: bool,
    ) -> Result<McpReplaceOutcome> {
        let _mutation = self.mutation.lock().await;
        if managed {
            let refused = self.manual_additions(&config).await;
            if !refused.is_empty() {
                return Ok(McpReplaceOutcome::RefusedManual(refused));
            }
        }
        Ok(McpReplaceOutcome::Replaced(
            self.replace_committed(config).await?,
        ))
    }

    /// The commit itself. Callers hold the mutation lock.
    async fn replace_committed(&self, config: McpServersConfig) -> Result<McpServersInfo> {
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
        let gateway = &*self.gateway;
        let locked = self.manual_transports_locked().await;
        let mut servers = HashMap::new();
        let connections = join_all(definitions.iter().map(|definition| async move {
            if connects(definition, locked) {
                definition.connect_with_views(gateway).await.map(Some)
            } else {
                Ok(None)
            }
        }))
        .await;
        for (definition, connection) in definitions.iter().zip(connections) {
            let connection = match connection {
                Ok(connection) => connection,
                // A gateway mount depends on session state that changes out
                // of band (sign-out, revoked entitlement), so its failure
                // degrades the mount instead of rejecting the candidate —
                // otherwise a signed-out mount would block every unrelated
                // settings save until it was deleted.
                Err(error) if definition.gateway_endpoint.is_some() => {
                    servers.insert(
                        definition.name.clone(),
                        ManagedServer {
                            client: None,
                            health: McpHealth::Degraded,
                            diagnostic: Some(connection_diagnostic(definition, &error)),
                            reconnect_backoff: INITIAL_RECONNECT_BACKOFF,
                            epoch: self.fresh_epoch(),
                            reconnect_lock: Arc::new(Mutex::new(())),
                            ui_views: HashMap::new(),
                        },
                    );
                    continue;
                }
                Err(error) => {
                    return Err(AgentError::config(format!(
                        "external MCP server {} failed to start: {}",
                        definition.name,
                        connection_diagnostic(definition, &error)
                    )));
                }
            };
            let Some((client, ui_views)) = connection else {
                servers.insert(
                    definition.name.clone(),
                    ManagedServer {
                        client: None,
                        health: McpHealth::Disabled,
                        diagnostic: managed_lockdown_diagnostic(definition, locked),
                        reconnect_backoff: INITIAL_RECONNECT_BACKOFF,
                        epoch: self.fresh_epoch(),
                        reconnect_lock: Arc::new(Mutex::new(())),
                        ui_views: HashMap::new(),
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
                    ui_views,
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
        let gateway = &*self.gateway;
        let locked = self.manual_transports_locked().await;
        let mut servers = HashMap::new();
        let connections = join_all(definitions.iter().map(|definition| async move {
            if connects(definition, locked) {
                definition.connect_with_views(gateway).await.map(Some)
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
                    diagnostic: managed_lockdown_diagnostic(definition, locked),
                    reconnect_backoff: INITIAL_RECONNECT_BACKOFF,
                    epoch: self.fresh_epoch(),
                    reconnect_lock: Arc::new(Mutex::new(())),
                    ui_views: HashMap::new(),
                },
                Ok(Some((client, ui_views))) => ManagedServer {
                    client: Some(client),
                    health: McpHealth::Healthy,
                    diagnostic: None,
                    reconnect_backoff: INITIAL_RECONNECT_BACKOFF,
                    epoch: self.fresh_epoch(),
                    reconnect_lock: Arc::new(Mutex::new(())),
                    ui_views,
                },
                Err(error) => ManagedServer {
                    client: None,
                    health: McpHealth::Degraded,
                    diagnostic: Some(connection_diagnostic(definition, &error)),
                    reconnect_backoff: INITIAL_RECONNECT_BACKOFF,
                    epoch: self.fresh_epoch(),
                    reconnect_lock: Arc::new(Mutex::new(())),
                    ui_views: HashMap::new(),
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
        let locked = self.manual_transports_locked().await;
        let (reconnect_lock, requested_epoch) = {
            let state = self.state.lock().await;
            let definition = state
                .definitions
                .iter()
                .find(|definition| definition.name == name)
                .ok_or_else(|| AgentError::config("MCP server not found"))?;
            if manual_lockdown_applies(definition, locked) {
                return Err(AgentError::config(MANAGED_DISABLED_DIAGNOSTIC));
            }
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
            // Re-checked against the definition as it stands now: a
            // replacement may have swapped this name onto a manual transport
            // while this caller waited for the per-server lock.
            if manual_lockdown_applies(&definition, locked) {
                return Err(AgentError::config(MANAGED_DISABLED_DIAGNOSTIC));
            }
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
        match definition.connect_with_views(&*self.gateway).await {
            Ok((client, ui_views)) => {
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
                        ui_views: HashMap::new(),
                    });
                server.client = Some(client);
                server.health = McpHealth::Healthy;
                server.diagnostic = None;
                server.ui_views = ui_views;
                server.reconnect_backoff = INITIAL_RECONNECT_BACKOFF;
                server.epoch = self.fresh_epoch();
                let registry = self.registry_for(&state.servers);
                *self
                    .tools
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(registry);
                Ok(self.info_locked(&state))
            }
            Err(error) => {
                let diagnostic = connection_diagnostic(&definition, &error);
                let mut state = self.state.lock().await;
                if let Some(server) = state
                    .servers
                    .get_mut(name)
                    .filter(|server| server.epoch == start_epoch)
                {
                    server.client = None;
                    server.health = McpHealth::Degraded;
                    server.diagnostic = Some(diagnostic.clone());
                    server.ui_views = HashMap::new();
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
            let locked = self.manual_transports_locked().await;
            // Policy may have flipped since the last sweep. Enforce the effect
            // before probing: a server that is now locked must be taken down,
            // not merely left out of the probe set.
            if locked {
                self.take_down_locked_manual_servers().await;
            }
            let probes = {
                let state = self.state.lock().await;
                state
                    .definitions
                    .iter()
                    .filter(|definition| connects(definition, locked))
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
            server.ui_views = HashMap::new();
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

/// Whether this definition should hold a live connection right now.
///
/// Managed policy forces every manual transport down whatever its stored flag
/// says; the definition itself is left untouched, so lifting the policy
/// restores exactly what the profile had.
fn connects(definition: &McpServerDefinition, manual_locked: bool) -> bool {
    definition.enabled && !manual_lockdown_applies(definition, manual_locked)
}

/// Whether the managed lockdown applies to this definition: manual transports
/// only. A gateway mount is the sanctioned path and is never forced down.
fn manual_lockdown_applies(definition: &McpServerDefinition, manual_locked: bool) -> bool {
    manual_locked && definition.gateway_endpoint.is_none()
}

/// The diagnostic a forced-down manual server carries, so the settings list
/// says why it is off instead of showing an unexplained disabled row.
fn managed_lockdown_diagnostic(
    definition: &McpServerDefinition,
    manual_locked: bool,
) -> Option<String> {
    manual_lockdown_applies(definition, manual_locked)
        .then(|| MANAGED_DISABLED_DIAGNOSTIC.to_string())
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
        match (&server.command, &server.url, &server.gateway_endpoint) {
            (None, None, None) => {
                return Err(server_error(
                    &server.name,
                    "must configure a command, a url, or a gateway endpoint",
                ));
            }
            (Some(command), None, None) => {
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
            (None, Some(url), None) => {
                validate_process_string(&server.name, "url", url)?;
                openwave_mcp::validate_http_url(url)
                    .map_err(|error| server_error(&server.name, error))?;
                validate_no_process_fields(server)?;
                if let Some(bearer_name) = &server.bearer_token_env {
                    validate_environment_name(&server.name, bearer_name)?;
                }
            }
            (None, None, Some(slug)) => {
                validate_gateway_endpoint_slug(&server.name, slug)?;
                validate_no_process_fields(server)?;
                if server.bearer_token_env.is_some() {
                    return Err(server_error(
                        &server.name,
                        "bearer_token_env applies only to url servers; a gateway \
                         endpoint's bearer comes from the signed-in session",
                    ));
                }
            }
            _ => {
                return Err(server_error(
                    &server.name,
                    "must configure exactly one of command, url, or gateway endpoint",
                ));
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

/// Remote transports (url and gateway endpoint) never spawn a child, so no
/// process field may accompany them.
fn validate_no_process_fields(server: &McpServerDefinition) -> Result<()> {
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
    Ok(())
}

/// The gateway's endpoint-slug contract, checked when the configuration is
/// saved rather than when a connection first resolves it. The contract
/// itself lives in one place — the connector that embeds the slug into the
/// request path and token resource.
fn validate_gateway_endpoint_slug(server_name: &str, slug: &str) -> Result<()> {
    openwave_connectors::validate_mcp_endpoint_slug(slug).map_err(|_| {
        server_error(
            server_name,
            "gateway endpoint must be 1-127 ASCII letters, digits, '_' or '-'",
        )
    })
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

fn connection_diagnostic(definition: &McpServerDefinition, error: &AgentError) -> String {
    if definition.gateway_endpoint.is_some() {
        if openwave_connectors::is_sign_in_required(error) {
            return "Sign in to the model gateway to reconnect this server.".to_string();
        }
        return "Could not connect to this gateway endpoint. Check the gateway connection \
                and your entitlements."
            .to_string();
    }
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

    /// The signed-out stand-in: every resolution demands a session.
    struct NoGateway;

    #[async_trait::async_trait]
    impl GatewayEndpoints for NoGateway {
        async fn endpoint(&self, _slug: &str) -> Result<GatewayEndpointAccess> {
            Err(AgentError::Authentication(
                "gateway sign-in required: no gateway session is stored".to_string(),
            ))
        }
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
            gateway_endpoint: None,
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
            gateway_endpoint: None,
            request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
            enabled: true,
        }
    }

    fn gateway_definition(name: &str, slug: &str) -> McpServerDefinition {
        McpServerDefinition {
            name: name.to_string(),
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            env_from: Vec::new(),
            cwd: None,
            url: None,
            bearer_token_env: None,
            gateway_endpoint: Some(slug.to_string()),
            request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
            enabled: true,
        }
    }

    async fn test_runtime() -> (Arc<McpRuntime>, Arc<dyn Store>, tempfile::TempDir) {
        test_runtime_with_gateway(Arc::new(NoGateway)).await
    }

    async fn test_runtime_with_gateway(
        gateway: Arc<dyn GatewayEndpoints>,
    ) -> (Arc<McpRuntime>, Arc<dyn Store>, tempfile::TempDir) {
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
                gateway,
                Arc::new(crate::managed_policy::NoOsPolicy),
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
        let error = config.0[0].connect(&NoGateway).await.err().unwrap();
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
        let failure = AgentError::config("connect failed");
        let missing = connection_diagnostic(&config.0[0], &failure);
        assert!(missing.contains(MISSING));
        assert!(!missing.contains('\n'));

        let generic = parse(r#"{"servers":[{"name":"docs","command":"/bin/docs"}]}"#).unwrap();
        assert_eq!(
            connection_diagnostic(&generic.0[0], &failure),
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
        for extra in [
            r#""url":"http://127.0.0.1/mcp""#,
            r#""gateway_endpoint":"tools""#,
        ] {
            let both = parse(&format!(
                r#"{{"servers":[{{"name":"docs","command":"/bin/docs",{extra}}}]}}"#
            ))
            .err()
            .unwrap();
            assert!(both.to_string().contains("exactly one"), "{extra}: {both}");
        }

        let neither = parse(r#"{"servers":[{"name":"docs"}]}"#).err().unwrap();
        assert!(neither
            .to_string()
            .contains("command, a url, or a gateway endpoint"));
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
            for transport in [
                r#""url":"http://127.0.0.1/mcp""#,
                r#""gateway_endpoint":"tools""#,
            ] {
                let error = parse(&format!(
                    r#"{{"servers":[{{"name":"docs",{transport},{field}}}]}}"#
                ))
                .err()
                .unwrap();
                assert!(
                    error.to_string().contains(fragment),
                    "{transport} {field}: {error}"
                );
            }
        }

        // A gateway endpoint's bearer comes from the session, never from a
        // selected environment variable.
        let bearer_on_gateway = parse(
            r#"{"servers":[{"name":"docs","gateway_endpoint":"tools","bearer_token_env":"TOKEN"}]}"#,
        )
        .err()
        .unwrap();
        assert!(bearer_on_gateway.to_string().contains("signed-in session"));
    }

    #[test]
    fn gateway_endpoint_slugs_follow_the_gateway_contract() {
        for slug in ["tools", "example-security_2"] {
            parse(&format!(
                r#"{{"servers":[{{"name":"docs","gateway_endpoint":"{slug}"}}]}}"#
            ))
            .unwrap();
        }
        let overlong = "a".repeat(128);
        for slug in ["", "has space", "path/../escape", "mcp:tools", &overlong] {
            let error = parse(&format!(
                r#"{{"servers":[{{"name":"docs","gateway_endpoint":"{slug}"}}]}}"#
            ))
            .err()
            .unwrap();
            assert!(
                error.to_string().contains("gateway endpoint must be"),
                "{slug}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn signed_out_gateway_mounts_degrade_to_a_sign_in_diagnostic() {
        let (runtime, _store, _directory) = test_runtime().await;
        runtime
            .replace_permissive(vec![gateway_definition("tools", "tools")])
            .await;
        let info = runtime.info().await;
        assert_eq!(info.servers[0].health, McpHealth::Degraded);
        assert_eq!(
            info.servers[0].diagnostic.as_deref(),
            Some("Sign in to the model gateway to reconnect this server.")
        );
    }

    #[tokio::test]
    async fn a_failing_gateway_mount_never_blocks_a_settings_replacement() {
        let (runtime, store, _directory) = test_runtime().await;
        // Saving a configuration that contains an unconnectable gateway
        // mount (signed out) plus an ordinary edit must persist both: the
        // mount degrades in place instead of rejecting the candidate.
        let info = runtime
            .replace(McpServersConfig {
                servers: vec![
                    gateway_definition("tools", "tools"),
                    disabled_definition("docs", "/bin/docs"),
                ],
            })
            .await
            .unwrap();
        assert_eq!(info.servers[0].health, McpHealth::Degraded);
        assert_eq!(
            info.servers[0].diagnostic.as_deref(),
            Some("Sign in to the model gateway to reconnect this server.")
        );
        assert_eq!(info.servers[1].health, McpHealth::Disabled);
        let saved: McpServersConfig =
            serde_json::from_value(store.get_setting(SETTING_KEY).await.unwrap().unwrap()).unwrap();
        assert_eq!(saved.servers.len(), 2);

        // A non-gateway failure keeps save-and-verify semantics: reject and
        // change nothing.
        let error = runtime
            .replace(McpServersConfig {
                servers: vec![http_definition("dead", "http://127.0.0.1:1/mcp")],
            })
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("failed to start"));
        let saved: McpServersConfig =
            serde_json::from_value(store.get_setting(SETTING_KEY).await.unwrap().unwrap()).unwrap();
        assert_eq!(
            saved.servers.len(),
            2,
            "rejected candidate must not persist"
        );
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
        let error = definition.connect(&NoGateway).await.err().unwrap();
        assert!(error.to_string().contains(MISSING));
        assert!(error.to_string().contains("is not set"));

        let diagnostic = connection_diagnostic(&definition, &error);
        assert!(diagnostic.contains(MISSING));
        assert!(!diagnostic.contains('\n'));
    }

    #[test]
    fn http_diagnostics_are_fixed_strings_without_the_url() {
        let definition = http_definition("gateway", "http://127.0.0.1:9/mcp");
        let diagnostic = connection_diagnostic(&definition, &AgentError::config("connect failed"));
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
                        "inputSchema": {"type": "object"},
                        "_meta": {"ui": {"resourceUri": "ui://fixture/app.html"}}
                    }]
                }),
                "resources/read" => serde_json::json!({
                    "contents": [{
                        "uri": "ui://fixture/app.html",
                        "mimeType": "text/html;profile=mcp-app",
                        "text": "<html>fixture view</html>"
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

        // The declared view was prefetched at connect and is served from
        // memory, keyed by the configured namespace and declared URI.
        let view = runtime
            .ui_view_document("gateway", "ui://fixture/app.html")
            .await
            .expect("declared view is prefetched");
        assert_eq!(view.html, "<html>fixture view</html>");
        assert_eq!(view.mime_type.as_deref(), Some("text/html;profile=mcp-app"));

        // Frame tokens are single-use capabilities over the prefetched view.
        let token = runtime
            .mint_view_frame("gateway", "ui://fixture/app.html")
            .await
            .expect("declared view mints a frame token");
        let redeemed = runtime
            .take_view_frame(token)
            .await
            .expect("first redemption serves the document");
        assert_eq!(redeemed.html, "<html>fixture view</html>");
        assert!(runtime.take_view_frame(token).await.is_none());
        assert!(runtime
            .mint_view_frame("gateway", "ui://fixture/other.html")
            .await
            .is_none());
        assert!(runtime
            .ui_view_document("gateway", "ui://fixture/other.html")
            .await
            .is_none());
        assert!(runtime
            .ui_view_document("unknown", "ui://fixture/app.html")
            .await
            .is_none());
    }

    /// The mid-process flip: a manual server that was healthy when the policy
    /// was open must not keep serving tools for the rest of the process. The
    /// decision is re-read live, and so is the effect — its client is dropped,
    /// its tools leave the registry, and it reports the managed diagnostic.
    #[tokio::test]
    async fn a_running_manual_server_is_torn_down_when_policy_flips_managed() {
        let address = serve_fake_http_mcp().await;
        let (runtime, store, _directory) = test_runtime().await;
        let mut definition = http_definition("gateway", &format!("http://{address}/mcp"));
        definition.bearer_token_env = Some("PATH".to_string());
        runtime
            .replace(McpServersConfig {
                servers: vec![definition],
            })
            .await
            .unwrap();
        assert_eq!(runtime.info().await.servers[0].health, McpHealth::Healthy);
        assert!(runtime.snapshot().get("mcp__gateway__lookup").is_some());

        // The profile becomes managed with the child already connected — an
        // MDM push, or deep-link pairing mid-session.
        crate::managed_policy::provision(&*store, "https://corp.gateway")
            .await
            .unwrap();
        assert!(runtime.enforce_manual_lockdown().await);

        assert!(
            runtime.snapshot().get("mcp__gateway__lookup").is_none(),
            "a locked server must stop serving tools to new turns"
        );
        let info = runtime.info().await;
        assert_eq!(info.servers[0].health, McpHealth::Disabled);
        assert_eq!(
            info.servers[0].diagnostic.as_deref(),
            Some(MANAGED_DISABLED_DIAGNOSTIC)
        );
        assert_eq!(info.servers[0].tool_count, 0);
        // Idempotent: a second sweep has nothing left to take down.
        assert!(!runtime.enforce_manual_lockdown().await);
    }

    /// Managed lockdown at the runtime boundary: persisted manual servers stay
    /// on file but never connect — disabled with a legible reason rather than
    /// silently deleted — while gateway mounts still resolve, and the
    /// host-environment boot file, the one channel the lockdown exists to
    /// close, is ignored outright.
    #[tokio::test]
    async fn managed_policy_forces_manual_servers_down_and_ignores_the_boot_file() {
        let (runtime, store, _directory) = test_runtime().await;
        let mut manual = disabled_definition("private_docs", "/bin/docs");
        manual.enabled = true;
        store
            .set_setting(
                SETTING_KEY,
                &serde_json::to_value(McpServersConfig {
                    servers: vec![manual, gateway_definition("tools", "tools")],
                })
                .unwrap(),
            )
            .await
            .unwrap();
        crate::managed_policy::provision(&*store, "https://corp.gateway")
            .await
            .unwrap();

        runtime
            .initialize(ConfiguredMcpServers::default())
            .await
            .unwrap();
        let info = runtime.info().await;
        assert_eq!(info.servers[0].health, McpHealth::Disabled);
        assert_eq!(
            info.servers[0].diagnostic.as_deref(),
            Some(MANAGED_DISABLED_DIAGNOSTIC)
        );
        assert!(
            info.servers[0].definition.enabled,
            "the stored definition is untouched, so lifting the policy restores it"
        );
        assert!(runtime
            .snapshot()
            .get("mcp__private_docs__lookup")
            .is_none());
        // The gateway mount is the sanctioned path and still attempts its
        // session-backed connection (signed out here, so it degrades).
        assert_eq!(info.servers[1].health, McpHealth::Degraded);
        assert_eq!(
            info.servers[1].diagnostic.as_deref(),
            Some("Sign in to the model gateway to reconnect this server.")
        );
        assert!(runtime.reconnect("private_docs").await.is_err());

        // A fresh profile whose only configuration is the boot file: managed,
        // so the file is inert and nothing is configured or persisted.
        let (runtime, store, _directory) = test_runtime().await;
        crate::managed_policy::provision(&*store, "https://corp.gateway")
            .await
            .unwrap();
        let boot = parse(r#"{"servers":[{"name":"docs","command":"/bin/docs"}]}"#).unwrap();
        runtime.initialize(boot).await.unwrap();
        assert!(runtime.info().await.servers.is_empty());
        assert!(store.get_setting(SETTING_KEY).await.unwrap().is_none());
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
