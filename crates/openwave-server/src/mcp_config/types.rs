//! MCP server definition types and boot-time configuration loading.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use openwave_core::id::ConnectedAppId;
use openwave_core::{AgentError, Result};
use openwave_mcp::McpClient;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::mcp_curated::McpCuration;

use super::validation::validate_servers;

pub(super) const CONFIG_ENV: &str = "OPENWAVE_MCP_CONFIG";
pub(super) const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
pub(crate) const MAX_CONFIG_BODY_BYTES: usize = 1024 * 1024;
pub(super) const MAX_SERVERS: usize = 32;
pub(super) const MAX_ARGS: usize = 128;
pub(super) const MAX_ENVIRONMENT_VARIABLES: usize = 128;
pub(super) const MAX_PROCESS_STRING_BYTES: usize = 32 * 1024;
pub(super) const MAX_ENVIRONMENT_NAME_BYTES: usize = 256;
pub(super) const MAX_REQUEST_TIMEOUT_MS: u64 = 60 * 60 * 1000;
pub(crate) const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 60 * 1000;
pub(super) const HEALTH_INTERVAL: Duration = Duration::from_secs(15);
pub(super) const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const INITIALIZATION_TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const INITIAL_RECONNECT_BACKOFF: Duration = Duration::from_secs(1);
pub(super) const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(30);

/// The diagnostic every manual (command/url) server carries while managed
/// policy holds. The definitions stay persisted — inert, not deleted — so an
/// unprovisioned profile is byte-for-byte unaffected and the list stays
/// legible instead of servers silently vanishing.
pub(crate) const MANAGED_DISABLED_DIAGNOSTIC: &str =
    "Disabled by managed policy. Gateway-managed MCP endpoints remain available.";

/// Persisted memory of the gateway endpoints the user explicitly unmounted:
/// the setting's value is a JSON array of endpoint slugs, nothing more —
/// the shape is closed and versioned by the key. A slug is recorded when a
/// committed settings replacement removes its mount, cleared when one
/// configures it again, and never touched by auto-mount, so "the user turned
/// this off" survives restarts without a second copy of the configuration.
pub(crate) const GATEWAY_ENDPOINT_UNMOUNTS_KEY: &str = "gateway.endpoint_unmounts_v1";

/// Upper bound on remembered unmounts. Entitled slugs are already bounded by
/// the gateway and configured mounts by [`MAX_SERVERS`]; this only caps what
/// years of shifting entitlements could accumulate. Oldest entries fall off
/// first — an ancient unmount degrading to a re-mount the user can undo.
pub(super) const MAX_REMEMBERED_UNMOUNTS: usize = 256;

/// How far managed policy locks the manual transports right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManualLockdown {
    /// Unmanaged: nothing is locked.
    Open,
    /// Managed with `AllowLocalMcpServers`: local stdio servers are the
    /// user's; remote manual (`url`) servers stay locked, because a
    /// credentialed remote endpoint outside the gateway is exactly the
    /// egress the entitlement list exists to answer for.
    RemoteManual,
    /// The managed default: every manual transport is locked.
    AllManual,
}

impl ManualLockdown {
    /// The lockdown a resolved policy asserts.
    pub(crate) fn for_policy(policy: &crate::managed_policy::ManagedPolicy) -> Self {
        if !policy.managed {
            Self::Open
        } else if policy.allow_local_mcp_servers {
            Self::RemoteManual
        } else {
            Self::AllManual
        }
    }
}

/// Validated external servers selected by the legacy boot file.
#[derive(Default)]
pub(crate) struct ConfiguredMcpServers(pub(super) Vec<McpServerDefinition>);

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
    /// Names of the environment variables this server is given directly. The
    /// values live in the secret store under [`env_secret_key`] and never
    /// enter this type, so they neither persist in the connected-app record
    /// nor project through the API.
    #[serde(default)]
    pub(crate) env: BTreeSet<String>,
    /// Inbound-only: values for [`env`](Self::env) names being set or
    /// changed. A commit writes these into the secret store and drops them; a
    /// name present in `env` but absent here keeps the value already stored,
    /// which is what makes "leave blank to keep" work. `skip_serializing`
    /// keeps them out of both the persisted record and every projection.
    #[serde(default, skip_serializing)]
    pub(crate) env_values: BTreeMap<String, String>,
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
    /// The plugin this server was synthesized from, when it is plugin-sourced.
    ///
    /// Read-only over the API: `PUT /mcp/servers` refuses a body that sets it,
    /// and the runtime rebuilds these entries from the installed plugin tree
    /// rather than from anything a client sends or the store holds.
    #[serde(default)]
    pub(crate) plugin: Option<String>,
    /// Connect-time material for a plugin-sourced server: the two reserved
    /// directories, the literal environment and headers its `mcp.json`
    /// declared, and the reason it is inert when this client cannot run it.
    ///
    /// Never serialized. Environment values and header values are visible
    /// package data the specification tells clients not to treat as secrets,
    /// which is exactly the reason not to copy them into persisted records,
    /// API responses, or logs.
    #[serde(skip)]
    #[ts(skip)]
    pub(crate) launch: Option<Box<PluginLaunch>>,
}

/// Connect-time material a plugin-sourced server carries.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PluginLaunch {
    /// Absolute, resolved package root: `PLUGIN_ROOT`, and the directory a
    /// `./`-relative command is resolved against.
    pub(crate) root: PathBuf,
    /// Client-managed writable directory: `PLUGIN_DATA`.
    pub(crate) data: PathBuf,
    /// Literal environment for a stdio server, already expanded.
    pub(crate) env: BTreeMap<String, String>,
    /// Static headers for a streamable-http server.
    pub(crate) headers: BTreeMap<String, String>,
    /// Which root the working directory is anchored to, and so which one it
    /// has to stay inside.
    pub(crate) cwd_anchor: CwdAnchor,
    /// Why this entry is present but inert, for the transports this client
    /// does not implement.
    pub(crate) disabled_reason: Option<String>,
}

/// The root a plugin server's working directory is anchored to.
///
/// The specification lets `cwd` be rooted at either reserved variable, and the
/// two are not interchangeable: the package tree is immutable once installed
/// while the data tree is client-managed and starts empty. Which one was named
/// is therefore carried rather than inferred from the resolved path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CwdAnchor {
    Root,
    Data,
}

/// Values never appear: the whole point of keeping them off the definition.
impl std::fmt::Debug for PluginLaunch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginLaunch")
            .field("root", &self.root)
            .field("data", &self.data)
            .field("env_names", &self.env.keys().collect::<Vec<_>>())
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .field("cwd_anchor", &self.cwd_anchor)
            .field("disabled_reason", &self.disabled_reason)
            .finish()
    }
}

impl std::fmt::Debug for McpServerDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpServerDefinition")
            .field("name", &self.name)
            .field("command", &self.command)
            .field("argument_count", &self.args.len())
            .field("env_names", &self.env.iter().collect::<Vec<_>>())
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

    /// The bearer for one `tools/call` from `chat`. The gateway runtime
    /// mints inside the chat's attestation context so gateway-attested
    /// endpoints can match the call against the observation the chat's
    /// inference recorded; the default keeps the connect-time bearer for
    /// implementations that don't distinguish.
    async fn call_bearer(&self, slug: &str, chat: openwave_core::id::ChatId) -> Result<String> {
        let _ = chat;
        Ok(self.endpoint(slug).await?.bearer_token)
    }

    /// The gateway connected apps this profile could bind, with the operation
    /// ids the gateway declares for each — the `create_app` roster's gateway
    /// section.
    ///
    /// Best-effort and never load-bearing: the default answers empty, which is
    /// exactly what a profile with no gateway session should say. Implementors
    /// degrade the same way rather than failing a registry rebuild.
    async fn entitled_app_catalogs(&self) -> Vec<GatewayRosterApp> {
        Vec::new()
    }
}

/// [`openwave_mcp::CallBearerSource`] over the gateway resolver for one
/// mounted endpoint: each `tools/call` presents the calling chat's token.
pub(super) struct GatewayCallBearer {
    gateway: Arc<dyn GatewayEndpoints>,
    slug: String,
}

#[async_trait::async_trait]
impl openwave_mcp::CallBearerSource for GatewayCallBearer {
    async fn call_bearer(&self, chat: openwave_core::id::ChatId) -> Result<Option<String>> {
        self.gateway.call_bearer(&self.slug, chat).await.map(Some)
    }
}

/// One resolved gateway endpoint: where to connect and the bearer to present.
/// Deliberately no `Debug`/`Serialize`: the token exists only on the path
/// from resolution into the transport's prebuilt header.
pub(crate) struct GatewayEndpointAccess {
    pub(crate) url: String,
    pub(crate) bearer_token: String,
}

/// One prefetched MCP Apps view document, served to the renderer only through
/// the dedicated view route and rendered only inside its sandboxed frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct UiViewDocument {
    pub(crate) mime_type: Option<String>,
    pub(crate) html: String,
}

impl McpServerDefinition {
    /// Build the child command. `env` is the definition's literal environment
    /// as resolved from the secret store — passed in rather than read off the
    /// definition, because the definition never holds values.
    ///
    /// A plugin-sourced server takes its environment from its own launch
    /// material instead: package data, never secret-store entries. Its working
    /// directory is re-checked for containment here rather than trusted from
    /// the textual rule the importer applied, because only a check at launch
    /// sees symlinks and edits made since.
    pub(super) fn build_command(&self, env: &BTreeMap<String, String>) -> Result<Command> {
        let Some(program) = &self.command else {
            return Err(AgentError::config(
                "MCP server definition has no command to spawn",
            ));
        };
        // A plugin's `./`-relative command names a file inside the package and
        // is resolved against the package root before the child is built;
        // everything else — including a plugin's bare command name — keeps the
        // platform resolution every other stdio server gets, so the two paths
        // cannot drift.
        let program = match &self.launch {
            Some(launch) => crate::plugin_mcp::resolve_command(program, &launch.root)
                .map_err(AgentError::config)?,
            None => PathBuf::from(program),
        };
        let mut command = Command::new(program);
        command.args(&self.args);
        // Deliberately unconditional: a renderer cannot widen a child to the
        // desktop's provider credentials or other ambient environment.
        command.env_clear();
        if let Some(launch) = &self.launch {
            let cwd = self.cwd.as_deref().unwrap_or(&launch.root);
            crate::plugin_mcp::working_directory(launch, cwd).map_err(AgentError::config)?;
            for (name, value) in crate::plugin_mcp::launch_environment(launch) {
                command.env(name, value);
            }
            command.current_dir(cwd);
            return Ok(command);
        }
        for name in &self.env_from {
            let value = std::env::var_os(name).ok_or_else(|| {
                AgentError::config(format!(
                    "required parent environment variable {name:?} is not set"
                ))
            })?;
            command.env(name, value);
        }
        // Only names the definition still declares: a resolved map that has
        // drifted ahead of a just-edited definition must not widen the child.
        for (name, value) in env {
            if self.env.contains(name) {
                command.env(name, value);
            }
        }
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        Ok(command)
    }

    pub(super) async fn connect(
        &self,
        gateway: &Arc<dyn GatewayEndpoints>,
        env: &BTreeMap<String, String>,
    ) -> Result<McpClient> {
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
            .await
            .map(|client| {
                // Dispatch rides per-chat tokens; the connect-time bearer
                // serves only the handshake and discovery above.
                client.with_call_bearer_source(std::sync::Arc::new(GatewayCallBearer {
                    gateway: Arc::clone(gateway),
                    slug: slug.clone(),
                }))
            });
        }
        if let Some(url) = &self.url {
            let bearer_token = self.resolve_bearer_token()?;
            let headers = match &self.launch {
                Some(launch) => {
                    admit_plugin_endpoint(url).await?;
                    launch.headers.clone()
                }
                None => BTreeMap::new(),
            };
            return McpClient::connect_http_with_headers(
                self.name.clone(),
                url,
                bearer_token.as_deref(),
                &headers,
                INITIALIZATION_TIMEOUT,
                Duration::from_millis(self.request_timeout_ms),
            )
            .await;
        }
        McpClient::spawn_with_timeouts(
            self.name.clone(),
            self.build_command(env)?,
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
    pub(super) async fn connect_with_views(
        &self,
        gateway: &Arc<dyn GatewayEndpoints>,
        env: &BTreeMap<String, String>,
    ) -> Result<(McpClient, HashMap<String, UiViewDocument>)> {
        let client = self.connect(gateway, env).await?;
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

/// Admit a plugin-declared HTTP endpoint before any connection is opened.
///
/// A package can name any endpoint it likes, so the destination is admitted
/// the way the native web-fetch path admits a model-chosen URL: every address
/// the host resolves to must clear the denied-network list — loopback, RFC
/// 1918, link-local (which includes cloud metadata services), CGNAT, and the
/// rest — before the transport dials.
///
/// **Two deliberate divergences from the web-fetch rules**, both because this
/// is a package-declared endpoint rather than a model-chosen page:
///
/// * A **loopback** URL is admitted. The Agent Plugins specification allows
///   plain HTTP for loopback precisely so a plugin can ship a local server,
///   and refusing it would make bundled local servers unrunnable. That is the
///   one destination the fetch policy denies which this path allows, and it is
///   permitted only when the URL's own host is a loopback literal or
///   `localhost` — never when a DNS name merely resolves to one.
/// * A **non-default port** is admitted. Local and self-hosted MCP endpoints
///   routinely listen off 443; the fetch policy pins the default port because
///   a model-chosen URL has no reason to need another.
///
/// Resolution here is advisory rather than a hard guarantee: the transport
/// resolves again when it connects, so a name whose answer changes in between
/// is not caught. That is the same TOCTOU the native fetch path lives with,
/// and it is a weaker exposure here — the URL is fixed package data reviewed
/// at install, not a string the model just produced. Failing admission is a
/// per-server connection failure: the plugin's other servers still start.
async fn admit_plugin_endpoint(url: &str) -> Result<()> {
    use crate::web_search::admit_fetch_address;

    let parsed = url::Url::parse(url)
        .map_err(|_| AgentError::config("plugin MCP server URL is not a valid URL"))?;
    let refused = || AgentError::config("plugin MCP server endpoint is not an allowed destination");
    let host = parsed.host().ok_or_else(refused)?;
    let loopback = match host {
        url::Host::Domain(name) => name == "localhost",
        url::Host::Ipv4(address) => address.is_loopback(),
        url::Host::Ipv6(address) => address.is_loopback(),
    };
    if loopback {
        return Ok(());
    }
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| AgentError::config("plugin MCP server URL has no port"))?;
    let addresses: Vec<std::net::SocketAddr> =
        tokio::net::lookup_host((parsed.host_str().unwrap_or_default(), port))
            .await
            .map_err(|_| AgentError::config("plugin MCP server host could not be resolved"))?
            .collect();
    if addresses.is_empty() {
        return Err(AgentError::config(
            "plugin MCP server host resolved to no addresses",
        ));
    }
    for address in addresses {
        admit_fetch_address(address.ip()).map_err(|_| refused())?;
    }
    Ok(())
}

/// SHA-256 fingerprint of a server definition as configured, the value an app
/// grant pins each bound connected app to.
///
/// The digest is taken over the UTF-8 bytes of a compact JSON object with
/// **exactly these keys, in exactly this order** (serde serializes struct
/// fields in declaration order, and every key is always present):
///
/// ```json
/// {"v":2,
///  "kind":"mcp_server",
///  "namespace":string,
///  "transport":"stdio"|"http"|"gateway",
///  "command":string|null,
///  "args":[string,...],
///  "cwd":string|null,
///  "env_names":[string,...],
///  "env_from":[string,...],
///  "url":string|null,
///  "bearer_token_env_set":bool,
///  "gateway_endpoint":string|null}
/// ```
///
/// `env_names` is the sorted *names* of the literal `env` entries and
/// `env_from` the sorted selected parent-environment names — configuration
/// values and resolved secrets never enter the canonical form, so the
/// fingerprint can never be a value oracle. That is also why moving the
/// literal values out of the definition and into the secret store did not
/// bump `v`: the canonical form only ever saw the names, which are unchanged,
/// so every grant issued before the move still matches after it. `bearer_token_env_set` records
/// only whether a bearer name is selected. `cwd` is the configured path,
/// lossily UTF-8. The `v:1` form excluded the server name because grants were
/// keyed by it; app-keyed grants pin a record id instead, so `namespace`
/// (the configured name, which decides which `mcp__{namespace}__…` mounted
/// names the binding covers) is now part of what the user consented to.
/// `kind` roots the form in the connected-app vocabulary so no two kinds can
/// collide on a canonical serialization. `enabled` and `request_timeout_ms`
/// stay excluded: toggling or re-timing a server does not change *what* the
/// user consented to run. Fingerprints are always computed from definition
/// fields, never from storage.
///
/// **This canonical form is a compatibility surface.** Persisted grants store
/// the digest; changing the form (or the meaning of any field in it)
/// invalidates every existing grant and must bump `v`.
pub(crate) fn definition_fingerprint(definition: &McpServerDefinition) -> [u8; 32] {
    use sha2::Digest as _;

    #[derive(Serialize)]
    struct CanonicalDefinition<'a> {
        v: u32,
        kind: &'static str,
        namespace: &'a str,
        transport: &'static str,
        command: Option<&'a str>,
        args: &'a [String],
        cwd: Option<String>,
        env_names: Vec<&'a str>,
        env_from: Vec<&'a str>,
        url: Option<&'a str>,
        bearer_token_env_set: bool,
        gateway_endpoint: Option<&'a str>,
    }

    let mut env_names: Vec<&str> = definition.env.iter().map(String::as_str).collect();
    env_names.sort_unstable();
    let mut env_from: Vec<&str> = definition.env_from.iter().map(String::as_str).collect();
    env_from.sort_unstable();
    let canonical = CanonicalDefinition {
        v: 2,
        kind: "mcp_server",
        namespace: &definition.name,
        transport: if definition.gateway_endpoint.is_some() {
            "gateway"
        } else if definition.url.is_some() {
            "http"
        } else {
            "stdio"
        },
        command: definition.command.as_deref(),
        args: &definition.args,
        cwd: definition
            .cwd
            .as_ref()
            .map(|cwd| cwd.to_string_lossy().into_owned()),
        env_names,
        env_from,
        url: definition.url.as_deref(),
        bearer_token_env_set: definition.bearer_token_env.is_some(),
        gateway_endpoint: definition.gateway_endpoint.as_deref(),
    };
    let bytes = serde_json::to_vec(&canonical)
        .expect("a canonical definition serializes infallibly to JSON");
    sha2::Sha256::digest(&bytes).into()
}

pub(super) const fn default_request_timeout_ms() -> u64 {
    DEFAULT_REQUEST_TIMEOUT_MS
}

/// Secret-store key holding one server's literal environment: a JSON object
/// of name → value.
///
/// Derived from the connected-app record id, never from anything in a
/// request, so this surface can only ever read and write its own secrets.
/// Mirrors `rest_credential_secret_key` for the REST connected-app kind.
pub(super) fn env_secret_key(id: ConnectedAppId) -> String {
    format!("mcp.{id}.env_v1")
}

/// Lift literal `env` values out of a connected-app record persisted before
/// the values moved into the secret store, leaving `env` in the name-array
/// shape the current definition uses.
///
/// Definitions written before that move stored `env` as a JSON object of
/// name → value, in cleartext, in `connected_app.definition_json`. Rejecting
/// them would take working MCP servers down at boot, so they are migrated
/// instead: the values come out here and the loader writes them to the secret
/// store and rewrites the record. Names are unchanged, so the definition
/// fingerprint — and therefore every app grant pinned to it — survives the
/// migration untouched. A record already in the new shape yields nothing and
/// takes no write.
pub(super) fn take_legacy_env_values(
    definition: &mut serde_json::Value,
) -> BTreeMap<String, String> {
    let Some(entries) = definition
        .get("env")
        .and_then(serde_json::Value::as_object)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_string())))
                .collect::<BTreeMap<String, String>>()
        })
    else {
        return BTreeMap::new();
    };
    definition["env"] = serde_json::Value::Array(
        entries
            .keys()
            .map(|name| serde_json::Value::String(name.clone()))
            .collect(),
    );
    entries
}
pub(super) const fn enabled_by_default() -> bool {
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
    /// The curated-list entry this definition matches, when OpenWave has
    /// exercised the server end to end. `null` means community: mounted and
    /// usable, just not something we have driven ourselves. Derived from the
    /// definition on every read, never stored.
    pub(crate) curated: Option<McpCuration>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
pub(crate) struct McpServersInfo {
    pub(crate) servers: Vec<McpServerInfo>,
}

/// One configured connected app's current namespace and definition
/// fingerprint, the pair grant enforcement compares per record id.
pub(crate) struct McpAppFingerprint {
    /// The configured server name — the namespace its tools mount under.
    pub(crate) name: String,
    /// [`definition_fingerprint`] of the current definition.
    pub(crate) fingerprint: [u8; 32],
}

/// `create_app` re-registered with the live connected-app roster appended to
/// its description. Manifest bindings name records by opaque id, which the
/// model cannot derive from mounted tool names; the roster is where those ids
/// come from. Everything else delegates to the shared tool.
pub(super) struct CreateAppWithRoster {
    pub(super) inner: Arc<dyn openwave_core::Tool>,
    pub(super) roster: String,
}

#[async_trait::async_trait]
impl openwave_core::Tool for CreateAppWithRoster {
    fn spec(&self) -> openwave_core::ToolSpec {
        let mut spec = self.inner.spec();
        spec.description.push_str(&self.roster);
        spec
    }

    fn approval_class(&self) -> openwave_core::ApprovalClass {
        self.inner.approval_class()
    }

    async fn execute(
        &self,
        ctx: &openwave_core::ToolCtx,
        args: serde_json::Value,
    ) -> Result<openwave_core::ToolOutput> {
        self.inner.execute(ctx, args).await
    }
}

/// How many of a `rest_api` record's operation ids the roster lists before
/// eliding the rest — enough to author against without letting a 256-entry
/// catalog balloon the tool description.
pub(super) const ROSTER_OPERATION_IDS: usize = 20;

/// One configured `rest_api` connected app's roster inputs: the record id a
/// binding names and the operation ids its catalog declares.
pub(crate) struct RestRosterApp {
    pub(crate) id: ConnectedAppId,
    pub(crate) name: String,
    pub(crate) operation_ids: Vec<String>,
}

/// One gateway connected app's roster inputs: the gateway's own id a binding
/// names and the operation ids the gateway declares for it.
pub(crate) struct GatewayRosterApp {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) operation_ids: Vec<String>,
}

/// The roster text appended to `create_app`'s description: every configured
/// `rest_api` connected app with the id a manifest binding names and a
/// bounded sample of its declared operation ids, every approved connected
/// folder with the root id a folder binding names, and — when this profile
/// holds a gateway session — every gateway connected app with the id a
/// gateway binding names.
///
/// `mcp_server` records are deliberately absent: mounted-tool bindings are
/// retired (#1332), so listing them would invite the model to author
/// manifests the door refuses. A profile with no gateway session lists no
/// gateway section at all, rather than an empty one: absence is what the
/// door's refusal already says.
pub(super) fn connected_app_roster(
    rest: &[RestRosterApp],
    folders: &[crate::host_folders::ApprovedFolder],
    gateway: &[GatewayRosterApp],
) -> String {
    if rest.is_empty() && folders.is_empty() && gateway.is_empty() {
        return "\n\nNo rest_api connected apps are configured and no folders are \
                connected, so only manifests with an empty bindings list can be \
                created."
            .to_owned();
    }
    let mut roster = String::from(
        "\n\nAvailable bindings (set each binding's `app`, `folder`, or `gateway_app` \
         to an id):",
    );
    for app in rest {
        roster.push_str(&format!(
            "\n- {id} — {name} (rest_api): bind with `operation_ids` from: {operations}",
            id = app.id,
            name = app.name,
            operations = listed_operation_ids(&app.operation_ids).join(", ")
        ));
    }
    for folder in folders {
        roster.push_str(&format!(
            "\n- {id} — {name} (folder): bind with `{{\"folder\": id, \"access\": \
             \"read\"|\"read_write\"}}`",
            id = folder.root_id,
            name = folder.display_name,
        ));
    }
    for app in gateway {
        roster.push_str(&format!(
            "\n- {id} — {name} (gateway app): bind with `{{\"gateway_app\": id, \
             \"operation_ids\": [...]}}` from: {operations}",
            id = app.id,
            name = app.name,
            operations = listed_operation_ids(&app.operation_ids).join(", ")
        ));
    }
    roster
}

/// The operation ids one roster line prints, elided past
/// [`ROSTER_OPERATION_IDS`].
fn listed_operation_ids(operation_ids: &[String]) -> Vec<&str> {
    let mut listed: Vec<&str> = operation_ids
        .iter()
        .take(ROSTER_OPERATION_IDS)
        .map(String::as_str)
        .collect();
    if operation_ids.len() > ROSTER_OPERATION_IDS {
        listed.push("…");
    }
    listed
}
