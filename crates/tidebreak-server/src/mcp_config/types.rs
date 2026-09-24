//! MCP server definition types and boot-time configuration loading.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tidebreak_core::id::ConnectedAppId;
use tidebreak_core::{AgentError, Result};
use tidebreak_mcp::McpClient;
use tokio::process::Command;

use crate::mcp_curated::McpCuration;
use crate::mcp_oauth_runtime::McpOAuthStatus;

use super::oauth::OAuthAccess;
use super::validation::validate_servers;

pub(super) const CONFIG_ENV: &str = "TIDEBREAK_MCP_CONFIG";
pub(super) const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
pub const MAX_CONFIG_BODY_BYTES: usize = 1024 * 1024;
pub(super) const MAX_SERVERS: usize = 32;
pub(super) const MAX_ARGS: usize = 128;
pub(super) const MAX_ENVIRONMENT_VARIABLES: usize = 128;
pub(super) const MAX_PROCESS_STRING_BYTES: usize = 32 * 1024;
pub(super) const MAX_ENVIRONMENT_NAME_BYTES: usize = 256;
/// The most custom headers one HTTP server may send.
pub(super) const MAX_HEADERS: usize = 8;
pub(super) const MAX_HEADER_NAME_BYTES: usize = 64;
/// The longest stored bearer token or header value.
pub(super) const MAX_CREDENTIAL_VALUE_BYTES: usize = 8 * 1024;
pub(super) const MAX_REQUEST_TIMEOUT_MS: u64 = 60 * 60 * 1000;
pub const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 60 * 1000;
pub(super) const HEALTH_INTERVAL: Duration = Duration::from_secs(15);
pub(super) const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const INITIALIZATION_TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const INITIAL_RECONNECT_BACKOFF: Duration = Duration::from_secs(1);
pub(super) const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(30);
/// The longest one MCP Apps view prefetch may take before the server's tools
/// publish without it.
pub(super) const VIEW_PREFETCH_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest a turn that starts while saved servers are still connecting
/// waits for them. After it, the turn runs with the servers that are up; the
/// rest join later turns as they connect.
pub(super) const BOOT_TOOLS_WAIT: Duration = Duration::from_secs(3);

/// Why the supervisor stopped retrying a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReconnectPark {
    /// The model gateway has no usable session. The server waits for the next
    /// sign-in, a manual reconnect, or a settings change.
    SignIn,
    /// The server needs something this process lacks, such as a parent
    /// environment variable. It waits for a manual reconnect or a settings
    /// change.
    Configuration,
    /// The server asks for an OAuth sign-in. It waits for the person to
    /// connect it, a manual reconnect, or a settings change. A model-gateway
    /// sign-in does not wake it.
    Authorization,
}

/// The diagnostic every manual (command/url) server carries while managed
/// policy holds. The definitions stay persisted — inert, not deleted — so an
/// unprovisioned profile is byte-for-byte unaffected and the list stays
/// legible instead of servers silently vanishing.
pub const MANAGED_DISABLED_DIAGNOSTIC: &str =
    "Disabled by managed policy. Gateway-managed MCP endpoints remain available.";

/// Persisted memory of the gateway endpoints the user explicitly unmounted:
/// the setting's value is a JSON array of endpoint slugs, nothing more —
/// the shape is closed and versioned by the key. A slug is recorded when a
/// committed settings replacement removes its mount, cleared when one
/// configures it again, and never touched by auto-mount, so "the user turned
/// this off" survives restarts without a second copy of the configuration.
pub const GATEWAY_ENDPOINT_UNMOUNTS_KEY: &str = "gateway.endpoint_unmounts_v1";

/// Upper bound on remembered unmounts. Entitled slugs are already bounded by
/// the gateway and configured mounts by [`MAX_SERVERS`]; this only caps what
/// years of shifting entitlements could accumulate. Oldest entries fall off
/// first — an ancient unmount degrading to a re-mount the user can undo.
pub(super) const MAX_REMEMBERED_UNMOUNTS: usize = 256;

/// How far managed policy locks the manual transports right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualLockdown {
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
    pub fn for_policy(policy: &crate::managed_policy::ManagedPolicy) -> Self {
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
pub struct ConfiguredMcpServers(pub(super) Vec<McpServerDefinition>);

impl ConfiguredMcpServers {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn from_env() -> Result<Self> {
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
pub struct McpServersConfig {
    pub servers: Vec<McpServerDefinition>,
}

/// One external MCP server definition: a local stdio process (`command`), a
/// remote Streamable HTTP endpoint (`url`), or a gateway-managed endpoint
/// (`gateway_endpoint`). Exactly one of the three is set;
/// [`validate_servers`] enforces that process fields stay with `command` and
/// the bearer and header fields stay with `url`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct McpServerDefinition {
    pub name: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// Names of the environment variables this server is given directly. The
    /// values live in the secret store under [`env_secret_key`] and never
    /// enter this type, so they neither persist in the connected-app record
    /// nor project through the API.
    #[serde(default)]
    pub env: BTreeSet<String>,
    /// Inbound-only: values for [`env`](Self::env) names being set or
    /// changed. A commit writes these into the secret store and drops them; a
    /// name present in `env` but absent here keeps the value already stored,
    /// which is what makes "leave blank to keep" work. `skip_serializing`
    /// keeps them out of both the persisted record and every projection.
    #[serde(default, skip_serializing)]
    pub env_values: BTreeMap<String, String>,
    /// Parent environment names to forward. Their values never enter this type.
    #[serde(default)]
    pub env_from: Vec<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// Streamable HTTP endpoint for a remote server.
    #[serde(default)]
    pub url: Option<String>,
    /// Parent environment name holding the HTTP bearer token. The value is
    /// resolved at connect time and never enters this type.
    #[serde(default)]
    pub bearer_token_env: Option<String>,
    /// Whether this HTTP server's bearer token is held in the OS credential
    /// store, under [`http_secret_key`], instead of a parent environment
    /// variable. Valid only with `url`, and exclusive with
    /// `bearer_token_env` and `oauth`. The value never enters this type.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bearer_token_stored: bool,
    /// Inbound-only: a new value for the stored bearer token. A commit writes
    /// it into the credential store and drops it; leaving it out keeps the
    /// value already stored. `skip_serializing` keeps it out of the persisted
    /// record and every projection.
    #[serde(default, skip_serializing)]
    pub bearer_token_value: Option<String>,
    /// Names of the custom headers this HTTP server receives on every request.
    /// The values live in the OS credential store under [`http_secret_key`],
    /// like a stdio server's [`env`](Self::env) values.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub headers: BTreeSet<String>,
    /// Inbound-only: values for [`headers`](Self::headers) names being set or
    /// changed. A name present in `headers` but absent here keeps the value
    /// already stored.
    #[serde(default, skip_serializing)]
    pub header_values: BTreeMap<String, String>,
    /// Whether this HTTP server always authenticates with OAuth (RFC 9728
    /// discovery, RFC 7591 registration, PKCE sign-in) instead of a static
    /// bearer. Valid only with `url`, and exclusive with `bearer_token_env`
    /// and `bearer_token_stored`. The flag is optional: a server without it that
    /// answers `401` with OAuth metadata signs in the same way. The obtained
    /// tokens live in the OS credential store under
    /// [`oauth_token_secret_key`], never in this type or the record.
    ///
    /// [`oauth_token_secret_key`]: crate::connectors::oauth_token_secret_key
    #[serde(default)]
    pub oauth: bool,
    /// Endpoint slug of a gateway MCP endpoint, mounted through the signed-in
    /// model-gateway session. The endpoint URL and its short-lived bearer are
    /// resolved from the session at every connection and never enter this
    /// type.
    #[serde(default)]
    pub gateway_endpoint: Option<String>,
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    /// The plugin this server was synthesized from, when it is plugin-sourced.
    ///
    /// Read-only over the API: `PUT /mcp/servers` refuses a body that sets it,
    /// and the runtime rebuilds these entries from the installed plugin tree
    /// rather than from anything a client sends or the store holds.
    #[serde(default)]
    pub plugin: Option<String>,
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
    pub launch: Option<Box<PluginLaunch>>,
}

/// Connect-time material a plugin-sourced server carries.
#[derive(Clone, PartialEq, Eq)]
pub struct PluginLaunch {
    /// Absolute, resolved package root: `PLUGIN_ROOT`, and the directory a
    /// `./`-relative command is resolved against.
    pub root: PathBuf,
    /// Client-managed writable directory: `PLUGIN_DATA`.
    pub data: PathBuf,
    /// Literal environment for a stdio server, already expanded.
    pub env: BTreeMap<String, String>,
    /// Static headers for a streamable-http server.
    pub headers: BTreeMap<String, String>,
    /// Which root the working directory is anchored to, and so which one it
    /// has to stay inside.
    pub cwd_anchor: CwdAnchor,
    /// Why this entry is present but inert, for the transports this client
    /// does not implement.
    pub disabled_reason: Option<String>,
}

/// The root a plugin server's working directory is anchored to.
///
/// The specification lets `cwd` be rooted at either reserved variable, and the
/// two are not interchangeable: the package tree is immutable once installed
/// while the data tree is client-managed and starts empty. Which one was named
/// is therefore carried rather than inferred from the resolved path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CwdAnchor {
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
            .field("bearer_token_stored", &self.bearer_token_stored)
            .field("header_names", &self.headers.iter().collect::<Vec<_>>())
            .field("oauth", &self.oauth)
            .field("gateway_endpoint", &self.gateway_endpoint)
            .field("request_timeout_ms", &self.request_timeout_ms)
            .field("enabled", &self.enabled)
            .finish()
    }
}

#[cfg(test)]
pub use tidebreak_gateway_runtime::GatewayEndpointAccess;
pub use tidebreak_gateway_runtime::{GatewayEndpoints, GatewayRosterApp};

/// [`tidebreak_mcp::CallBearerSource`] over the gateway resolver for one
/// mounted endpoint: each `tools/call` presents the calling chat's token.
pub(super) struct GatewayCallBearer {
    gateway: Arc<dyn GatewayEndpoints>,
    slug: String,
}

#[async_trait::async_trait]
impl tidebreak_mcp::CallBearerSource for GatewayCallBearer {
    async fn call_bearer(&self, chat: tidebreak_core::id::SessionId) -> Result<Option<String>> {
        self.gateway.call_bearer(&self.slug, chat).await.map(Some)
    }
}

/// Load a live OAuth connection when a session for exactly `url` is stored.
/// A session issued for any other URL — the server's URL before an edit, or
/// a server of the same name that was removed — is never loaded, so its token
/// cannot reach this one. Missing credentials mean the person has not
/// connected yet: the HTTP client is built without a bearer rather than
/// failing the connect, and a `401` then tells the runtime to ask the server
/// how to sign in.
async fn live_oauth_connection(
    access: &OAuthAccess,
    url: &str,
) -> Option<Arc<crate::connectors::McpOAuthConnection>> {
    let vault = crate::connectors::McpOAuthCredentialVault::new(access.secrets.clone(), access.id);
    let (registration, _tokens) = vault.load_for(url).await.ok().flatten()?;
    let token_endpoint = registration.token_endpoint.as_deref()?;
    let token_endpoint = url::Url::parse(token_endpoint).ok()?;
    Some(Arc::new(crate::connectors::McpOAuthConnection::new(
        access.client.clone(),
        vault,
        token_endpoint,
        registration,
    )))
}

/// One prefetched MCP Apps view document, served to the renderer only through
/// the dedicated view route and rendered only inside its sandboxed frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UiViewDocument {
    pub mime_type: Option<String>,
    pub html: String,
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
    pub(super) async fn build_command(&self, env: &BTreeMap<String, String>) -> Result<Command> {
        let Some(program) = &self.command else {
            return Err(AgentError::config(
                "MCP server definition has no command to spawn",
            ));
        };
        // A plugin's command must be `./`-relative and is resolved against
        // the package root before the child is built. User-configured servers
        // resolve a bare name through the host PATH (process PATH extended
        // with the login-shell PATH the harness probe captures) without
        // invoking a shell.
        let program = match &self.launch {
            Some(launch) => crate::plugin_mcp::resolve_command(program, &launch.root)
                .map_err(AgentError::config)?,
            None => super::stdio::resolve_stdio_command(program).await?,
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
        // HOME and the PATH the command was resolved on, so a script such as
        // `npx` finds `node` and its cache. A name the definition sets itself
        // replaces the default below.
        for (name, value) in super::stdio::forwarded_by_default().await {
            command.env(name, value);
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

    /// Open a session with this server. `http` is what the credential store
    /// holds for an HTTP server: its stored bearer token and header values.
    /// `oauth` is how to present a stored OAuth session, given only for a
    /// server that [signs in](super::oauth::signs_in).
    pub(super) async fn connect(
        &self,
        gateway: &Arc<dyn GatewayEndpoints>,
        env: &BTreeMap<String, String>,
        http: &StoredHttpValues,
        oauth: Option<&OAuthAccess>,
    ) -> Result<McpClient> {
        let request_timeout = Duration::from_millis(self.request_timeout_ms);
        let initialization_timeout = request_timeout.min(INITIALIZATION_TIMEOUT);
        if let Some(slug) = &self.gateway_endpoint {
            // Resolved per connection: a reconnect always presents a token
            // that is fresh at that moment, so expiry is survived by the
            // ordinary supervision/reconnect cycle.
            let access = gateway.endpoint(slug).await?;
            return McpClient::connect_http_with_timeouts(
                self.name.clone(),
                &access.url,
                Some(&access.bearer_token),
                initialization_timeout,
                request_timeout,
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
            // A server that signs in with OAuth carries no static bearer: a
            // stored session's refreshing access token is the only
            // credential, loaded from the OS credential store and attached as
            // a per-call bearer. That holds whatever the saved `oauth` flag
            // says, so a server someone connected without the flag keeps its
            // session. A stored bearer or header value goes only to the
            // origin it was stored for: values stored before the URL moved to
            // another origin are never sent.
            let http = http.for_url(url);
            let bearer_token = self.resolve_bearer_token(&http)?;
            let headers = match &self.launch {
                Some(launch) => {
                    admit_plugin_endpoint(url).await?;
                    launch.headers.clone()
                }
                None => self.resolve_headers(&http)?,
            };
            let oauth_connection = match oauth {
                Some(access) => live_oauth_connection(access, url).await,
                None => None,
            };
            let handshake_bearer = match &oauth_connection {
                Some(connection) => match connection.access_token().await {
                    Ok(token) => Some(token),
                    // The sign-in service refused the refresh token: the
                    // session is over, and the server's `401` says what to
                    // do next.
                    Err(error) if crate::connectors::is_oauth_sign_in_required(&error) => None,
                    // Anything else is temporary. Keep the session and fail
                    // this attempt, so the supervisor retries on its usual
                    // backoff instead of connecting without the token.
                    Err(error) => return Err(error),
                },
                None => bearer_token,
            };
            return McpClient::connect_http_with_headers(
                self.name.clone(),
                url,
                handshake_bearer.as_deref(),
                &headers,
                initialization_timeout,
                request_timeout,
            )
            .await
            .map(|client| match oauth_connection {
                Some(connection) => client.with_call_bearer_source(std::sync::Arc::new(
                    crate::connectors::McpOAuthCallBearer::new(connection),
                )),
                None => client,
            });
        }
        McpClient::spawn_with_timeouts(
            self.name.clone(),
            self.build_command(env).await?,
            initialization_timeout,
            request_timeout,
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
        http: &StoredHttpValues,
        oauth: Option<&OAuthAccess>,
    ) -> Result<(McpClient, HashMap<String, UiViewDocument>)> {
        let client = self.connect(gateway, env, http, oauth).await?;
        let views = prefetch_views(&client, VIEW_PREFETCH_TIMEOUT).await;
        Ok((client, views))
    }

    /// Resolve the bearer token at the connection boundary: by name from the
    /// parent environment, or from the credential store.
    fn resolve_bearer_token(&self, http: &StoredHttpValues) -> Result<Option<String>> {
        if let Some(name) = &self.bearer_token_env {
            return std::env::var(name).map(Some).map_err(|_| {
                AgentError::config(format!(
                    "required parent environment variable {name:?} is not set"
                ))
            });
        }
        if self.bearer_token_stored {
            return http.bearer.clone().map(Some).ok_or_else(|| {
                AgentError::config(format!(
                    "{NOT_STORED} this server's bearer token is not in the credential store on \
                     this computer. Enter it under Authentication, then save."
                ))
            });
        }
        Ok(None)
    }

    /// Every custom header this definition declares, with its stored value.
    /// A declared header whose value is not stored fails by name, before
    /// anything is sent.
    fn resolve_headers(&self, http: &StoredHttpValues) -> Result<BTreeMap<String, String>> {
        self.headers
            .iter()
            .map(|name| match http.headers.get(name) {
                Some(value) => Ok((name.clone(), value.clone())),
                None => Err(AgentError::config(format!(
                    "{NOT_STORED} the value of header {name:?} is not in the credential store \
                     on this computer. Enter it under Headers, then save."
                ))),
            })
            .collect()
    }
}

/// Fetch every MCP Apps view the connected server declared, all at once, each
/// bounded by `per_view`.
///
/// A view that does not arrive in time is left out, like one that fails: its
/// transcript card degrades to a reconnect hint and the server's tools are
/// unaffected. The bound is what keeps one slow view from holding a server's
/// tools back, because the tools publish only after this returns. A stdio
/// session answers one request at a time, so on that transport the views
/// share the bound rather than each getting its own.
pub(super) async fn prefetch_views(
    client: &McpClient,
    per_view: Duration,
) -> HashMap<String, UiViewDocument> {
    let uris: HashSet<String> = client
        .tools()
        .filter_map(|spec| client.ui_resource_uri(&spec.name))
        .map(str::to_string)
        .collect();
    let fetched = futures::future::join_all(uris.into_iter().map(|uri| async move {
        let content = tokio::time::timeout(per_view, client.read_resource(&uri))
            .await
            .ok()?
            .ok()?;
        // MCP Apps views are HTML text; a binary body has no sandbox story,
        // and a non-HTML mime must not be served as a document.
        let html = content.text?;
        if !content
            .mime_type
            .as_deref()
            .is_none_or(|mime| mime.starts_with("text/html"))
        {
            return None;
        }
        Some((
            uri,
            UiViewDocument {
                mime_type: content.mime_type,
                html,
            },
        ))
    }))
    .await;
    fetched.into_iter().flatten().collect()
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
/// {"v":4,
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
///  "bearer_token_stored":bool,
///  "header_names":[string,...],
///  "oauth":bool,
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
/// only whether a bearer name is selected, `bearer_token_stored` only whether
/// the bearer is held in the credential store, and `header_names` the sorted
/// names of the custom headers the server receives, never their values.
/// Sending a stored credential or a custom header is a different thing to
/// have consented to run, which is why adding them bumped `v` from 3 to 4.
/// `cwd` is the configured path,
/// lossily UTF-8. `oauth` records whether the definition forces an
/// OAuth-obtained token rather than a static env bearer — a different thing to
/// have consented to run, which is why adding it bumped `v` from 2 to 3. A
/// server without the flag signs in only when it asks for OAuth and the person
/// selects Connect. The session is bound to the exact URL it was issued for:
/// it is never presented to another URL, and editing the URL or removing the
/// server clears it, so a token cannot outlive the `url` this form covers. The
/// `v:1` form excluded the server name because grants were keyed by it;
/// app-keyed grants pin a record id instead, so `namespace`
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
pub fn definition_fingerprint(definition: &McpServerDefinition) -> [u8; 32] {
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
        bearer_token_stored: bool,
        header_names: Vec<&'a str>,
        oauth: bool,
        gateway_endpoint: Option<&'a str>,
    }

    let mut env_names: Vec<&str> = definition.env.iter().map(String::as_str).collect();
    env_names.sort_unstable();
    let mut env_from: Vec<&str> = definition.env_from.iter().map(String::as_str).collect();
    env_from.sort_unstable();
    let mut header_names: Vec<&str> = definition.headers.iter().map(String::as_str).collect();
    header_names.sort_unstable();
    let canonical = CanonicalDefinition {
        v: 4,
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
        bearer_token_stored: definition.bearer_token_stored,
        header_names,
        oauth: definition.oauth,
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
pub fn env_secret_key(id: ConnectedAppId) -> String {
    format!("mcp.{id}.env_v1")
}

/// Secret-store key holding one HTTP server's stored credentials: its bearer
/// token and custom header values, as a [`StoredHttpValues`] JSON object.
///
/// Derived from the connected-app record id, like [`env_secret_key`], so this
/// surface can only ever read and write its own secrets.
pub fn http_secret_key(id: ConnectedAppId) -> String {
    format!("mcp.{id}.http_v1")
}

/// How a connection failure starts when the credential store lacks a value
/// the definition declares. Settings shows the sentence as it is, and the
/// supervisor stops retrying until a settings change or a manual reconnect.
pub(super) const NOT_STORED: &str = "Not stored:";

/// One HTTP server's stored credentials, bound to the origin they were
/// entered for.
///
/// A value goes only to the origin it was stored for: a save that moves the
/// server to another origin drops the values it does not set again, and a
/// connection loads nothing stored for a different origin.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct StoredHttpValues {
    /// `scheme://host[:port]` of the URL the values were stored for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) origin: Option<String>,
    /// The stored bearer token, sent as `Authorization: Bearer …`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) bearer: Option<String>,
    /// Stored header values, by header name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) headers: BTreeMap<String, String>,
}

impl StoredHttpValues {
    /// Whether there is nothing to store.
    pub(super) fn is_empty(&self) -> bool {
        self.bearer.is_none() && self.headers.is_empty()
    }

    /// These values when they were stored for `url`'s origin, and nothing
    /// otherwise.
    pub(super) fn for_url(&self, url: &str) -> Self {
        match (&self.origin, http_origin(url)) {
            (Some(stored), Some(origin)) if *stored == origin => self.clone(),
            _ => Self::default(),
        }
    }
}

/// Values never appear, only which ones are stored.
impl std::fmt::Debug for StoredHttpValues {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoredHttpValues")
            .field("origin", &self.origin)
            .field("bearer_stored", &self.bearer.is_some())
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// The `scheme://host[:port]` origin of an HTTP URL, or `None` when the URL
/// does not parse or has no host.
pub(super) fn http_origin(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let origin = parsed.origin();
    origin.is_tuple().then(|| origin.ascii_serialization())
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
pub enum McpReplaceOutcome {
    Replaced(McpServersInfo),
    /// Managed policy refused these manual servers. Nothing changed.
    RefusedManual(Vec<String>),
}

/// What adding one server did.
pub enum McpAddOutcome {
    /// The server is saved under `name`, or a configured server already had
    /// its URL and `name` is that one. `info` is the configuration after it.
    Added { name: String, info: McpServersInfo },
    /// Managed policy refused the server. Nothing changed.
    RefusedManual,
}

/// A saved MCP server record Tidebreak could not load.
///
/// Its definition does not decode, it fails validation, or its stored
/// environment values could not move into the credential store. The record
/// stays on file, unused: a save keeps it, and removing it is an explicit
/// action. Its tools never mount, and a grant that binds its id stays stale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct McpSkippedServer {
    /// The connected-app record id, which removing the record names.
    pub id: ConnectedAppId,
    /// The record's saved name.
    pub name: String,
    /// Why Tidebreak did not load it, as a sentence. Names fields and
    /// environment variable names, never a value.
    pub reason: String,
}

/// Renderer-safe connection lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum McpHealth {
    Initializing,
    Healthy,
    Degraded,
    Reconnecting,
    Disabled,
}

impl McpHealth {
    /// The wire spelling, for a client that prints the state without a serde
    /// round trip. Pinned to the serde form by a test in [`crate::wire`].
    pub fn as_str(self) -> &'static str {
        match self {
            McpHealth::Initializing => "initializing",
            McpHealth::Healthy => "healthy",
            McpHealth::Degraded => "degraded",
            McpHealth::Reconnecting => "reconnecting",
            McpHealth::Disabled => "disabled",
        }
    }
}

/// One renderer-safe server projection. Resolved `env_from` values and child
/// process details are intentionally absent.
//
// Also read back by the CLI through [`crate::wire`], which ignores keys a
// record does not declare, like every record on that surface. `definition`
// is flattened, and serde would not honor `deny_unknown_fields` across a
// flatten anyway. A plain comment, not a doc comment, so the generated
// `wire.ts` does not carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct McpServerInfo {
    #[serde(flatten)]
    pub definition: McpServerDefinition,
    pub health: McpHealth,
    pub tool_count: usize,
    pub diagnostic: Option<String>,
    /// Absolute path the stdio command resolved to at the last verify or
    /// launch. Absent for HTTP/gateway servers and when resolution failed.
    /// The stored definition still holds what the user typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub resolved_command: Option<String>,
    /// The curated-list entry this definition matches, when Tidebreak has
    /// exercised the server end to end. `null` means community: mounted and
    /// usable, just not something we have driven ourselves. Derived from the
    /// definition on every read, never stored.
    pub curated: Option<McpCuration>,
    /// OAuth connection status for a remote HTTP server that authenticates
    /// with OAuth. Absent for stdio, gateway, and static-token servers. Read
    /// from the OS credential store per request, never stored in the
    /// definition. The status carries no token material.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub oauth_status: Option<McpOAuthStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct McpServersInfo {
    pub servers: Vec<McpServerInfo>,
}

/// One configured connected app's current namespace and definition
/// fingerprint, the pair grant enforcement compares per record id.
pub struct McpAppFingerprint {
    /// The configured server name — the namespace its tools mount under.
    pub name: String,
    /// [`definition_fingerprint`] of the current definition.
    pub fingerprint: [u8; 32],
}

/// `create_app` re-registered with the live connected-app roster appended to
/// its description. Manifest bindings name records by opaque id, which the
/// model cannot derive from mounted tool names; the roster is where those ids
/// come from. Everything else delegates to the shared tool.
pub(super) struct CreateAppWithRoster {
    pub(super) inner: Arc<dyn tidebreak_core::Tool>,
    pub(super) roster: String,
}

#[async_trait::async_trait]
impl tidebreak_core::Tool for CreateAppWithRoster {
    fn spec(&self) -> tidebreak_core::ToolSpec {
        let mut spec = self.inner.spec();
        spec.description.push_str(&self.roster);
        spec
    }

    fn approval_class(&self) -> tidebreak_core::ApprovalClass {
        self.inner.approval_class()
    }

    async fn execute(
        &self,
        ctx: &tidebreak_core::ToolCtx,
        args: serde_json::Value,
    ) -> Result<tidebreak_core::ToolOutput> {
        self.inner.execute(ctx, args).await
    }
}

/// How many of a `rest_api` record's operation ids the roster lists before
/// eliding the rest — enough to author against without letting a 256-entry
/// catalog balloon the tool description.
pub(super) const ROSTER_OPERATION_IDS: usize = 20;

/// One configured `rest_api` connected app's roster inputs: the record id a
/// binding names and the operation ids its catalog declares.
pub struct RestRosterApp {
    pub id: ConnectedAppId,
    pub name: String,
    pub operation_ids: Vec<String>,
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
