//! Runtime configuration and supervision for external MCP servers.
//!
//! A definition is either a local stdio child process or a remote Streamable
//! HTTP endpoint. Definitions are typed data, never shell fragments. Every
//! child starts with a cleared environment and receives only values named by
//! the definition: literal values held in the secret store, plus values
//! selected by *name* from the parent environment. An HTTP server's bearer
//! token is likewise selected by name. **No environment value of any kind
//! lives in a definition**: the connected-app record and every API projection
//! carry names only, and values are resolved at the connection boundary.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures::future::join_all;
use openwave_core::connected_app::{ConnectedApp, ConnectedAppKind};
use openwave_core::id::ConnectedAppId;
use openwave_core::local_app::CREATE_APP_TOOL;
use openwave_core::{AgentError, Result, SecretProvider, Store, ToolRegistry};
use openwave_mcp::{McpClient, McpProbe, MAX_SERVER_NAME_BYTES};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::sync::Mutex;

const CONFIG_ENV: &str = "OPENWAVE_MCP_CONFIG";
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
const MAX_REMEMBERED_UNMOUNTS: usize = 256;

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
}

/// [`openwave_mcp::CallBearerSource`] over the gateway resolver for one
/// mounted endpoint: each `tools/call` presents the calling chat's token.
struct GatewayCallBearer {
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

impl McpServerDefinition {
    /// Build the child command. `env` is the definition's literal environment
    /// as resolved from the secret store — passed in rather than read off the
    /// definition, because the definition never holds values.
    fn build_command(&self, env: &BTreeMap<String, String>) -> Result<Command> {
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

    async fn connect(
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
    async fn connect_with_views(
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

const fn default_request_timeout_ms() -> u64 {
    DEFAULT_REQUEST_TIMEOUT_MS
}

/// Secret-store key holding one server's literal environment: a JSON object
/// of name → value.
///
/// Derived from the connected-app record id, never from anything in a
/// request, so this surface can only ever read and write its own secrets.
/// Mirrors `rest_credential_secret_key` for the REST connected-app kind.
fn env_secret_key(id: ConnectedAppId) -> String {
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
fn take_legacy_env_values(definition: &mut serde_json::Value) -> BTreeMap<String, String> {
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
struct CreateAppWithRoster {
    inner: Arc<dyn openwave_core::Tool>,
    roster: String,
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
const ROSTER_OPERATION_IDS: usize = 20;

/// One configured `rest_api` connected app's roster inputs: the record id a
/// binding names and the operation ids its catalog declares.
pub(crate) struct RestRosterApp {
    pub(crate) id: ConnectedAppId,
    pub(crate) name: String,
    pub(crate) operation_ids: Vec<String>,
}

/// The roster text appended to `create_app`'s description: every configured
/// connected app with the id a manifest binding names — the namespace an
/// `mcp_server`'s mounted tools carry, or a bounded sample of a `rest_api`
/// record's operation ids.
///
/// A server without a live client (disabled, degraded, locked down) stays
/// listed — authoring a binding against it is legitimate, and the grant and
/// invoke gates enforce at call time — but its line says so, instead of
/// promising mounted tool names that do not currently exist.
fn connected_app_roster(
    definitions: &[McpServerDefinition],
    ids: &BTreeMap<String, ConnectedAppId>,
    servers: &HashMap<String, ManagedServer>,
    rest: &[RestRosterApp],
) -> String {
    if definitions.is_empty() && rest.is_empty() {
        return "\n\nNo connected apps are configured, so only manifests with an \
                empty bindings list can be created."
            .to_owned();
    }
    let mut roster =
        String::from("\n\nConfigured connected apps (set each binding's `app` to an id):");
    for definition in definitions {
        let Some(id) = ids.get(&definition.name) else {
            continue;
        };
        let unavailable = servers
            .get(&definition.name)
            .is_none_or(|server| server.client.is_none());
        roster.push_str(&format!(
            "\n- {id} — {name}: tools are named `mcp__{name}__{{tool}}`{status}",
            name = definition.name,
            status = if unavailable {
                " (currently unavailable — configured but not connected)"
            } else {
                ""
            }
        ));
    }
    for app in rest {
        let mut listed: Vec<&str> = app
            .operation_ids
            .iter()
            .take(ROSTER_OPERATION_IDS)
            .map(String::as_str)
            .collect();
        if app.operation_ids.len() > ROSTER_OPERATION_IDS {
            listed.push("…");
        }
        roster.push_str(&format!(
            "\n- {id} — {name} (rest_api): bind with `operation_ids` from: {operations}",
            id = app.id,
            name = app.name,
            operations = listed.join(", ")
        ));
    }
    roster
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
    /// The connected-app record id behind each configured server name. App
    /// manifests and grants bind these ids; the id survives edits to the
    /// definition and dies with the record, so a name-keyed lookup here is
    /// only ever a projection detail, never the consent key.
    ids: BTreeMap<String, ConnectedAppId>,
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
    /// Holds each server's literal environment values, keyed by record id.
    /// Definitions carry only the names.
    secrets: Arc<dyn SecretProvider>,
    /// Resolves gateway-managed endpoints at every connection.
    gateway: Arc<dyn GatewayEndpoints>,
    /// The OS authority for managed-mode resolution. Managed policy locks the
    /// manual transports; the gateway-endpoint transport is the sanctioned
    /// path and stays open.
    os_policy: Arc<dyn crate::managed_policy::OsPolicySource>,
    next_epoch: AtomicU64,
}

impl McpRuntime {
    pub(crate) fn new(
        base_tools: Arc<ToolRegistry>,
        store: Arc<dyn Store>,
        secrets: Arc<dyn SecretProvider>,
        gateway: Arc<dyn GatewayEndpoints>,
        os_policy: Arc<dyn crate::managed_policy::OsPolicySource>,
    ) -> Self {
        Self {
            base_tools: (*base_tools).clone(),
            tools: RwLock::new(base_tools),
            state: Mutex::new(RuntimeState {
                definitions: Vec::new(),
                ids: BTreeMap::new(),
                servers: HashMap::new(),
            }),
            mutation: Mutex::new(()),
            store,
            secrets,
            gateway,
            os_policy,
            next_epoch: AtomicU64::new(1),
        }
    }

    /// One server's literal environment as stored, by record id. A missing or
    /// unreadable entry resolves empty: the child then starts without those
    /// names and fails with the server's own diagnostic, which beats taking
    /// an unrelated settings save down.
    async fn stored_env(&self, id: ConnectedAppId) -> BTreeMap<String, String> {
        match self.secrets.get_secret(&env_secret_key(id)).await {
            Ok(Some(raw)) => serde_json::from_str(&raw).unwrap_or_default(),
            Ok(None) => BTreeMap::new(),
            Err(error) => {
                tracing::warn!(%error, "could not read stored MCP environment values");
                BTreeMap::new()
            }
        }
    }

    /// The environment to hand each definition's child, resolved once for a
    /// whole replacement so the connections below can run concurrently.
    async fn resolve_envs(
        &self,
        definitions: &[McpServerDefinition],
        ids: &BTreeMap<String, ConnectedAppId>,
    ) -> HashMap<String, BTreeMap<String, String>> {
        let mut resolved = HashMap::with_capacity(definitions.len());
        for definition in definitions {
            if definition.env.is_empty() {
                resolved.insert(definition.name.clone(), BTreeMap::new());
                continue;
            }
            let Some(id) = ids.get(&definition.name).copied() else {
                resolved.insert(definition.name.clone(), BTreeMap::new());
                continue;
            };
            resolved.insert(definition.name.clone(), self.stored_env(id).await);
        }
        resolved
    }

    /// Commit each definition's environment values to the secret store and
    /// return the definitions with `env_values` emptied, ready to persist.
    ///
    /// The stored entry becomes exactly what the definition declares: values
    /// just set win, names dropped from `env` lose their stored value, and a
    /// name kept without a new value keeps the one already stored. Records
    /// that no longer exist have their entry deleted, so removing a server
    /// takes its credentials with it.
    async fn commit_env_values(
        &self,
        definitions: &mut [McpServerDefinition],
        ids: &BTreeMap<String, ConnectedAppId>,
    ) -> Result<()> {
        let live: HashSet<ConnectedAppId> = ids.values().copied().collect();
        let stale: Vec<ConnectedAppId> = {
            let state = self.state.lock().await;
            state
                .ids
                .values()
                .copied()
                .filter(|id| !live.contains(id))
                .collect()
        };
        for id in stale {
            // Best effort: a leftover entry is unreachable (nothing
            // references the id) and a failure here must not fail the save.
            let _ = self.secrets.delete_secret(&env_secret_key(id)).await;
        }
        for definition in definitions {
            let Some(id) = ids.get(&definition.name).copied() else {
                definition.env_values.clear();
                continue;
            };
            let mut values = self.stored_env(id).await;
            values.append(&mut definition.env_values);
            values.retain(|name, _| definition.env.contains(name));
            let key = env_secret_key(id);
            if values.is_empty() {
                let _ = self.secrets.delete_secret(&key).await;
                continue;
            }
            let encoded = serde_json::to_string(&values).map_err(|error| {
                AgentError::config(format!("could not encode MCP environment values: {error}"))
            })?;
            self.secrets.set_secret(&key, &encoded).await?;
        }
        Ok(())
    }

    /// The gateway resolver MCP dispatch rides on — exposed so tests can pin
    /// that it is the same instance as `AppState::gateway` (#1441: a second
    /// runtime splits attestation contexts and every attested `tools/call`
    /// is refused).
    #[cfg(test)]
    pub(crate) fn gateway_endpoints(&self) -> Arc<dyn GatewayEndpoints> {
        self.gateway.clone()
    }

    /// The secret store this runtime writes environment values to — so a test
    /// can read back exactly what landed there.
    #[cfg(test)]
    pub(crate) fn secrets(&self) -> Arc<dyn SecretProvider> {
        self.secrets.clone()
    }

    /// How far managed policy currently locks the manual transports.
    ///
    /// Read per operation rather than cached at boot, like every other policy
    /// consumer, so an MDM push or removal takes effect without a restart. An
    /// unreadable policy fails closed to the full lockdown — the same judgment
    /// the BYOK boot paths make.
    async fn manual_lockdown(&self) -> ManualLockdown {
        match crate::managed_policy::resolve(&*self.store, &*self.os_policy).await {
            Ok(policy) => ManualLockdown::for_policy(&policy),
            Err(error) => {
                tracing::warn!(
                    "managed policy is unreadable; locking manual MCP transports: {error}"
                );
                ManualLockdown::AllManual
            }
        }
    }

    /// The names in `candidate` that would add or change a manual server the
    /// lockdown covers, relative to what is already configured.
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
    async fn manual_additions(
        &self,
        candidate: &McpServersConfig,
        lockdown: ManualLockdown,
    ) -> Vec<String> {
        let existing = &self.state.lock().await.definitions;
        candidate
            .servers
            .iter()
            .filter(|server| manual_lockdown_applies(server, lockdown))
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
        match self.manual_lockdown().await {
            ManualLockdown::Open => false,
            lockdown => self.take_down_locked_manual_servers(lockdown).await,
        }
    }

    async fn take_down_locked_manual_servers(&self, lockdown: ManualLockdown) -> bool {
        let mut state = self.state.lock().await;
        let locked: Vec<String> = state
            .definitions
            .iter()
            .filter(|definition| manual_lockdown_applies(definition, lockdown))
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
            let registry = self.registry_for(&state).await;
            *self
                .tools
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(registry);
        }
        torn_down
    }

    /// Load persisted `mcp_server` connected-app records when present,
    /// otherwise the legacy boot file.
    ///
    /// A boot file remains fail-closed. Persisted definitions degrade in place
    /// so the Settings UI remains available to repair a missing executable or
    /// selected environment variable.
    pub(crate) async fn initialize(&self, boot: ConfiguredMcpServers) -> Result<()> {
        let records: Vec<ConnectedApp> = self
            .store
            .list_connected_apps()
            .await?
            .into_iter()
            .filter(|record| record.kind == ConnectedAppKind::McpServer)
            .collect();
        if !records.is_empty() {
            let mut ids = BTreeMap::new();
            let mut definitions = Vec::with_capacity(records.len());
            let mut migrated = Vec::new();
            for record in records {
                let mut stored = record.definition;
                // Records written before literal values moved into the secret
                // store carry them here in cleartext; lift them out before the
                // definition is typed, so no value ever enters the type again.
                let legacy = take_legacy_env_values(&mut stored);
                let mut definition: McpServerDefinition =
                    serde_json::from_value(stored).map_err(|error| {
                        AgentError::config(format!(
                            "invalid saved connected-app definition {:?}: {error}",
                            record.name
                        ))
                    })?;
                if !legacy.is_empty() {
                    migrated.push(record.name.clone());
                    definition.env_values = legacy;
                }
                // The record's name is authoritative for the namespace; the
                // stored definition mirrors it and is repaired if they ever
                // disagree.
                definition.name = record.name.clone();
                ids.insert(record.name, record.id);
                definitions.push(definition);
            }
            validate_servers(&definitions)?;
            if !migrated.is_empty() {
                tracing::info!(
                    servers = ?migrated,
                    "moving stored MCP environment values into the secret store"
                );
                // Writes the values, empties `env_values`, and rewrites the
                // records without them. A failure leaves the cleartext records
                // untouched and retries next boot rather than starting servers
                // whose credentials just went missing.
                self.commit_env_values(&mut definitions, &ids).await?;
                self.persist_definitions(&definitions, &ids).await?;
            }
            self.replace_permissive(definitions, ids).await;
            return Ok(());
        }
        // The boot file is a host-environment artifact: on a managed
        // profile it is exactly the channel the lockdown exists to
        // close, so it is inert rather than partially honored — unless
        // the org's `AllowLocalMcpServers` opt-in re-opens the local
        // channel, in which case any remote (`url`) definitions it names
        // are still forced down per definition below. The warning is the
        // operator's diagnostic for the silence.
        if !boot.is_empty() && self.manual_lockdown().await == ManualLockdown::AllManual {
            tracing::warn!(
                "{CONFIG_ENV} is ignored on a managed profile; \
                 mount MCP endpoints from the model gateway instead"
            );
            return Ok(());
        }
        self.replace_strict(boot.0, false).await.map(|_| ())
    }

    /// One prefetched MCP Apps view document, when the named server is
    /// connected and declared it.
    pub(crate) async fn ui_view_document(&self, server: &str, uri: &str) -> Option<UiViewDocument> {
        let state = self.state.lock().await;
        state.servers.get(server)?.ui_views.get(uri).cloned()
    }

    /// The current namespace and definition fingerprint of every configured
    /// connected app, by record id.
    ///
    /// Read live per call — never cached across a request — so grant
    /// enforcement always compares against the definition an id resolves to
    /// *now*, including a definition swapped in while the app stayed open.
    pub(crate) async fn app_fingerprints(&self) -> BTreeMap<ConnectedAppId, McpAppFingerprint> {
        let state = self.state.lock().await;
        state
            .definitions
            .iter()
            .filter_map(|definition| {
                let id = state.ids.get(&definition.name)?;
                Some((
                    *id,
                    McpAppFingerprint {
                        name: definition.name.clone(),
                        fingerprint: definition_fingerprint(definition),
                    },
                ))
            })
            .collect()
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

    /// The bare mounted tool names of every connected server, by namespace —
    /// the name each tool carries after the `mcp__{server}__` mount prefix.
    ///
    /// Names only, never remote-authored descriptions or schemas: the same
    /// renderer-safety posture as the consent sheet. Bounded by the
    /// per-server discovery cap the client enforces at connect time.
    pub(crate) async fn tool_names(&self) -> BTreeMap<String, Vec<String>> {
        let state = self.state.lock().await;
        state
            .servers
            .iter()
            .map(|(name, server)| {
                let prefix = format!("mcp__{name}__");
                let tools = server.client.as_ref().map_or_else(Vec::new, |client| {
                    client
                        .tools()
                        .map(|spec| {
                            spec.name
                                .strip_prefix(&prefix)
                                .unwrap_or(&spec.name)
                                .to_string()
                        })
                        .collect()
                });
                (name.clone(), tools)
            })
            .collect()
    }

    /// The unmanaged shape of [`replace_under_policy`](Self::replace_under_policy),
    /// whose refusal arm is unreachable. Production has one entry point; this
    /// keeps the tests that predate the policy check reading as they did.
    #[cfg(test)]
    pub(crate) async fn replace(&self, config: McpServersConfig) -> Result<McpServersInfo> {
        match self
            .replace_under_policy(config, ManualLockdown::Open)
            .await?
        {
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
        lockdown: ManualLockdown,
    ) -> Result<McpReplaceOutcome> {
        let _mutation = self.mutation.lock().await;
        let refused = self.manual_additions(&config, lockdown).await;
        if !refused.is_empty() {
            return Ok(McpReplaceOutcome::RefusedManual(refused));
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

    /// Mount every entitled gateway endpoint that is neither configured nor
    /// remembered as explicitly unmounted, appending a fresh enabled
    /// `gateway_endpoint` definition for each through the same
    /// mutation-locked commit a settings save uses — so an auto-mount can
    /// never interleave with a concurrent PUT. Returns whether anything was
    /// mounted; with nothing to add there is no store write and no registry
    /// churn, so repeat reconciles are free.
    ///
    /// Gateway mounts are the sanctioned transport under managed policy (the
    /// admission check refuses only manual additions), so no lockdown branch
    /// is needed here. An unreadable unmount memory fails closed — no
    /// mounting — rather than resurrecting an endpoint the user turned off.
    pub(crate) async fn auto_mount_gateway_endpoints(&self, entitled: &[String]) -> Result<bool> {
        let _mutation = self.mutation.lock().await;
        let mut servers = self.state.lock().await.definitions.clone();
        let unmounts = read_endpoint_unmounts(&*self.store).await?;
        let mut configured: HashSet<String> = servers
            .iter()
            .filter_map(|definition| definition.gateway_endpoint.clone())
            .collect();
        let mut taken: HashSet<String> = servers
            .iter()
            .map(|definition| definition.name.clone())
            .collect();
        let before = servers.len();
        for slug in entitled {
            if configured.contains(slug) || unmounts.iter().any(|unmounted| unmounted == slug) {
                continue;
            }
            // The gateway is trusted for entitlements, not for shapes: a slug
            // outside the endpoint contract is skipped, never persisted.
            if openwave_connectors::validate_mcp_endpoint_slug(slug).is_err() {
                tracing::warn!(
                    slug = %slug,
                    "entitled gateway MCP endpoint slug is invalid; not auto-mounting"
                );
                continue;
            }
            if servers.len() >= MAX_SERVERS {
                tracing::warn!(
                    slug = %slug,
                    "MCP server list is full; not auto-mounting this gateway endpoint"
                );
                continue;
            }
            let name = gateway_mount_name(slug, &taken);
            taken.insert(name.clone());
            configured.insert(slug.clone());
            servers.push(McpServerDefinition {
                name,
                command: None,
                args: Vec::new(),
                env: BTreeSet::new(),
                env_values: BTreeMap::new(),
                env_from: Vec::new(),
                cwd: None,
                url: None,
                bearer_token_env: None,
                gateway_endpoint: Some(slug.clone()),
                request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
                enabled: true,
            });
        }
        if servers.len() == before {
            return Ok(false);
        }
        self.replace_committed(McpServersConfig { servers }).await?;
        Ok(true)
    }

    /// Record the gateway-endpoint intent a committed settings replacement
    /// expressed, against the set published just before it: a mount removed
    /// is an explicit unmount (auto-mount only ever adds, so a removal seen
    /// here is always deliberate), and a slug configured again clears its
    /// memory so a manual remount stays remounted. Best-effort by design:
    /// the replacement itself has already committed, so a failed memory
    /// write degrades to a possible future auto-remount, never a failed
    /// save. Callers hold the mutation lock and have not yet published the
    /// new state.
    async fn remember_endpoint_unmounts(&self, new_definitions: &[McpServerDefinition]) {
        let new_slugs: HashSet<&str> = new_definitions
            .iter()
            .filter_map(|definition| definition.gateway_endpoint.as_deref())
            .collect();
        let removed: Vec<String> = {
            let state = self.state.lock().await;
            state
                .definitions
                .iter()
                .filter_map(|definition| definition.gateway_endpoint.as_deref())
                .filter(|slug| !new_slugs.contains(slug))
                .map(str::to_string)
                .collect()
        };
        let mut memory = match read_endpoint_unmounts(&*self.store).await {
            Ok(memory) => memory,
            // The write below repairs the malformed value; losing it degrades
            // to auto-remounts the user can undo, unlike failing every save.
            Err(error) => {
                tracing::warn!("gateway unmount memory is unreadable; rebuilding it: {error}");
                Vec::new()
            }
        };
        let before = memory.clone();
        memory.retain(|slug| !new_slugs.contains(slug.as_str()));
        for slug in removed {
            if !memory.contains(&slug) {
                memory.push(slug);
            }
        }
        if memory.len() > MAX_REMEMBERED_UNMOUNTS {
            let excess = memory.len() - MAX_REMEMBERED_UNMOUNTS;
            memory.drain(..excess);
        }
        if memory == before {
            return;
        }
        let value = serde_json::to_value(&memory).expect("a list of strings serializes infallibly");
        if let Err(error) = self
            .store
            .set_setting(GATEWAY_ENDPOINT_UNMOUNTS_KEY, &value)
            .await
        {
            tracing::warn!("could not persist the gateway unmount memory: {error}");
        }
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

    /// Assign the connected-app record id behind each candidate name: a name
    /// already configured keeps its record id (so editing a definition
    /// invalidates grants by fingerprint, not by identity), a new name mints a
    /// fresh one, and a removed name's record — and with it every binding
    /// naming its id — simply stops resolving.
    async fn assign_app_ids(
        &self,
        definitions: &[McpServerDefinition],
    ) -> BTreeMap<String, ConnectedAppId> {
        let state = self.state.lock().await;
        definitions
            .iter()
            .map(|definition| {
                let id = state
                    .ids
                    .get(&definition.name)
                    .copied()
                    .unwrap_or_else(ConnectedAppId::new);
                (definition.name.clone(), id)
            })
            .collect()
    }

    async fn replace_strict(
        &self,
        mut definitions: Vec<McpServerDefinition>,
        persist: bool,
    ) -> Result<()> {
        validate_servers(&definitions)?;
        let ids = if persist {
            self.assign_app_ids(&definitions).await
        } else {
            // The legacy boot file configures servers without persisting
            // records; ids derived from the configured names keep app grants
            // valid across restarts of a boot-file profile.
            definitions
                .iter()
                .map(|definition| {
                    (
                        definition.name.clone(),
                        ConnectedAppId::for_boot_server(&definition.name),
                    )
                })
                .collect()
        };
        // Before anything connects, so the children below see the environment
        // this replacement declares rather than the previous one's. A boot
        // file's values land in the same store under the same derived key:
        // one resolution path, and the file stops being a second home for
        // credentials.
        self.commit_env_values(&mut definitions, &ids).await?;
        let definitions = definitions;
        let envs = self.resolve_envs(&definitions, &ids).await;
        let gateway = &self.gateway;
        let lockdown = self.manual_lockdown().await;
        let mut servers = HashMap::new();
        let connections = join_all(definitions.iter().map(|definition| {
            let env = envs.get(&definition.name).cloned().unwrap_or_default();
            async move {
                if connects(definition, lockdown) {
                    definition.connect_with_views(gateway, &env).await.map(Some)
                } else {
                    Ok(None)
                }
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
                    // The projected diagnostic is deliberately generic; keep
                    // the real cause in the log (already URL- and
                    // secret-free). The desktop installs no tracing
                    // subscriber yet, so this surfaces under `openwave serve`
                    // until it does.
                    tracing::warn!(
                        server = %definition.name,
                        "gateway MCP mount degraded during replacement: {error}"
                    );
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
                        diagnostic: managed_lockdown_diagnostic(definition, lockdown),
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
            // Before the new state publishes, while the published set is
            // still what this replacement diffs against.
            self.remember_endpoint_unmounts(&definitions).await;
            self.persist_definitions(&definitions, &ids).await?;
        }
        self.publish(definitions, ids, servers).await;
        Ok(())
    }

    /// Write the definitions as this profile's complete `mcp_server`
    /// connected-app set. `env_values` is `skip_serializing`, so the record
    /// carries environment *names* and nothing more; the values are already in
    /// the secret store by the time this runs.
    async fn persist_definitions(
        &self,
        definitions: &[McpServerDefinition],
        ids: &BTreeMap<String, ConnectedAppId>,
    ) -> Result<()> {
        let now = chrono::Utc::now();
        let records: Vec<ConnectedApp> = definitions
            .iter()
            .map(|definition| {
                Ok(ConnectedApp {
                    id: ids[&definition.name],
                    name: definition.name.clone(),
                    kind: ConnectedAppKind::McpServer,
                    definition: serde_json::to_value(definition)?,
                    created_at: now,
                    updated_at: now,
                })
            })
            .collect::<Result<_>>()?;
        self.store
            .replace_connected_apps(ConnectedAppKind::McpServer, &records)
            .await
    }

    async fn replace_permissive(
        &self,
        definitions: Vec<McpServerDefinition>,
        ids: BTreeMap<String, ConnectedAppId>,
    ) {
        let envs = self.resolve_envs(&definitions, &ids).await;
        let gateway = &self.gateway;
        let lockdown = self.manual_lockdown().await;
        let mut servers = HashMap::new();
        let connections = join_all(definitions.iter().map(|definition| {
            let env = envs.get(&definition.name).cloned().unwrap_or_default();
            async move {
                if connects(definition, lockdown) {
                    definition.connect_with_views(gateway, &env).await.map(Some)
                } else {
                    Ok(None)
                }
            }
        }))
        .await;
        for (definition, connection) in definitions.iter().zip(connections) {
            let managed = match connection {
                Ok(None) => ManagedServer {
                    client: None,
                    health: McpHealth::Disabled,
                    diagnostic: managed_lockdown_diagnostic(definition, lockdown),
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
                Err(error) => {
                    // As in `replace_strict`: the error chain is already URL-
                    // and secret-free, and the warn serves `openwave serve`
                    // until the desktop installs a tracing subscriber.
                    tracing::warn!(
                        server = %definition.name,
                        "MCP server connection failed during permissive replacement: {error}"
                    );
                    ManagedServer {
                        client: None,
                        health: McpHealth::Degraded,
                        diagnostic: Some(connection_diagnostic(definition, &error)),
                        reconnect_backoff: INITIAL_RECONNECT_BACKOFF,
                        epoch: self.fresh_epoch(),
                        reconnect_lock: Arc::new(Mutex::new(())),
                        ui_views: HashMap::new(),
                    }
                }
            };
            servers.insert(definition.name.clone(), managed);
        }
        self.publish(definitions, ids, servers).await;
    }

    async fn publish(
        &self,
        definitions: Vec<McpServerDefinition>,
        ids: BTreeMap<String, ConnectedAppId>,
        servers: HashMap<String, ManagedServer>,
    ) {
        let rest = self.rest_roster().await;
        let registry = self.registry_with(&definitions, &ids, &servers, &rest);
        let mut state = self.state.lock().await;
        state.definitions = definitions;
        state.ids = ids;
        state.servers = servers;
        *self
            .tools
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(registry);
    }

    /// Every stored `rest_api` connected app's roster inputs, read from the
    /// store when a registry is (re)built. The roster is authoring
    /// legibility, never the gate, so an unreadable store or an unparseable
    /// definition degrades to an absent roster line rather than failing the
    /// registry rebuild.
    async fn rest_roster(&self) -> Vec<RestRosterApp> {
        let records = match self.store.list_connected_apps().await {
            Ok(records) => records,
            Err(error) => {
                tracing::warn!("could not read connected apps for the create_app roster: {error}");
                return Vec::new();
            }
        };
        records
            .into_iter()
            .filter(|record| record.kind == ConnectedAppKind::RestApi)
            .filter_map(|record| {
                let operations = record
                    .definition
                    .get("catalog")?
                    .get("operations")?
                    .as_object()?;
                Some(RestRosterApp {
                    id: record.id,
                    name: record.name,
                    operation_ids: operations.keys().cloned().collect(),
                })
            })
            .collect()
    }

    /// [`registry_with`](Self::registry_with) over the already-published
    /// state, for the paths that refresh connections without changing the
    /// configuration.
    async fn registry_for(&self, state: &RuntimeState) -> ToolRegistry {
        let rest = self.rest_roster().await;
        self.registry_with(&state.definitions, &state.ids, &state.servers, &rest)
    }

    fn registry_with(
        &self,
        definitions: &[McpServerDefinition],
        ids: &BTreeMap<String, ConnectedAppId>,
        servers: &HashMap<String, ManagedServer>,
        rest: &[RestRosterApp],
    ) -> ToolRegistry {
        let mut registry = self.base_tools.clone();
        for (name, server) in servers {
            if let Some(client) = &server.client {
                let refused = client.mount(&mut registry);
                if !refused.is_empty() {
                    tracing::warn!(
                        server = %name,
                        tools = %refused.join(", "),
                        "MCP tools were not mounted because the names are already registered"
                    );
                }
            }
        }
        // Manifest bindings name connected apps by record id; the roster on
        // the `create_app` description is where the model learns those ids.
        if let Some(inner) = registry.server_tool(CREATE_APP_TOOL) {
            registry.register(Box::new(CreateAppWithRoster {
                inner,
                roster: connected_app_roster(definitions, ids, servers, rest),
            }));
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
        let lockdown = self.manual_lockdown().await;
        let (reconnect_lock, requested_epoch) = {
            let state = self.state.lock().await;
            let definition = state
                .definitions
                .iter()
                .find(|definition| definition.name == name)
                .ok_or_else(|| AgentError::config("MCP server not found"))?;
            if manual_lockdown_applies(definition, lockdown) {
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
        let (definition, app_id, start_epoch) = {
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
            if manual_lockdown_applies(&definition, lockdown) {
                return Err(AgentError::config(MANAGED_DISABLED_DIAGNOSTIC));
            }
            if !definition.enabled {
                return Err(AgentError::config("disabled MCP server cannot reconnect"));
            }
            let app_id = state.ids.get(name).copied();
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
            (definition, app_id, server.epoch)
        };
        // Resolved fresh for this attempt, outside the state lock: a
        // credential rotated since the last connection takes effect on the
        // next reconnect without a settings save.
        let env = match app_id {
            Some(id) if !definition.env.is_empty() => self.stored_env(id).await,
            _ => BTreeMap::new(),
        };
        match definition.connect_with_views(&self.gateway, &env).await {
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
                let registry = self.registry_for(&state).await;
                *self
                    .tools
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(registry);
                Ok(self.info_locked(&state))
            }
            Err(error) => {
                // As in `replace_strict`: the error chain is already URL- and
                // secret-free, and the warn serves `openwave serve` until the
                // desktop installs a tracing subscriber.
                tracing::warn!(server = %name, "MCP server reconnect failed: {error}");
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
                let registry = self.registry_for(&state).await;
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
            let lockdown = self.manual_lockdown().await;
            // Policy may have flipped since the last sweep. Enforce the effect
            // before probing: a server that is now locked must be taken down,
            // not merely left out of the probe set.
            if lockdown != ManualLockdown::Open {
                self.take_down_locked_manual_servers(lockdown).await;
            }
            let probes = {
                let state = self.state.lock().await;
                state
                    .definitions
                    .iter()
                    .filter(|definition| connects(definition, lockdown))
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
        let registry = self.registry_for(&state).await;
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
fn connects(definition: &McpServerDefinition, lockdown: ManualLockdown) -> bool {
    definition.enabled && !manual_lockdown_applies(definition, lockdown)
}

/// Whether the managed lockdown applies to this definition. A gateway mount
/// is the sanctioned path and is never forced down; under the org's
/// `AllowLocalMcpServers` opt-in a local stdio (`command`) server is spared
/// while a remote (`url`) one stays covered.
fn manual_lockdown_applies(definition: &McpServerDefinition, lockdown: ManualLockdown) -> bool {
    if definition.gateway_endpoint.is_some() {
        return false;
    }
    match lockdown {
        ManualLockdown::Open => false,
        ManualLockdown::RemoteManual => definition.command.is_none(),
        ManualLockdown::AllManual => true,
    }
}

/// The remembered explicit unmounts, an empty list when nothing was ever
/// recorded. A malformed value is an error: the auto-mount caller must fail
/// closed on it instead of treating "unreadable" as "nothing unmounted".
async fn read_endpoint_unmounts(store: &dyn Store) -> Result<Vec<String>> {
    let Some(value) = store.get_setting(GATEWAY_ENDPOINT_UNMOUNTS_KEY).await? else {
        return Ok(Vec::new());
    };
    serde_json::from_value(value).map_err(|error| {
        AgentError::config(format!(
            "invalid {GATEWAY_ENDPOINT_UNMOUNTS_KEY} setting: {error}"
        ))
    })
}

/// A valid, unused namespace for an auto-mounted endpoint: the slug,
/// truncated to the name limit and de-duplicated against every configured
/// server — the same derivation the desktop's mount toggle uses
/// (`mountName` in `McpPanel.tsx`), so a mount gets the same name whichever
/// side creates it. Slugs are ASCII by contract, so byte slicing is safe.
fn gateway_mount_name(slug: &str, taken: &HashSet<String>) -> String {
    let base = &slug[..slug.len().min(MAX_SERVER_NAME_BYTES)];
    if !taken.contains(base) {
        return base.to_string();
    }
    for n in 2u64.. {
        let suffix = format!("_{n}");
        let keep = base.len().min(MAX_SERVER_NAME_BYTES - suffix.len());
        let candidate = format!("{}{suffix}", &base[..keep]);
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("some numeric suffix is always free")
}

/// The diagnostic a forced-down manual server carries, so the settings list
/// says why it is off instead of showing an unexplained disabled row.
fn managed_lockdown_diagnostic(
    definition: &McpServerDefinition,
    lockdown: ManualLockdown,
) -> Option<String> {
    manual_lockdown_applies(definition, lockdown).then(|| MANAGED_DISABLED_DIAGNOSTIC.to_string())
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
        for key in &server.env {
            validate_environment_name(&server.name, key)?;
            environment_names.insert(key.as_str());
        }
        for (key, value) in &server.env_values {
            if !server.env.contains(key) {
                return Err(server_error(
                    &server.name,
                    format!("environment value {key:?} names no configured variable"),
                ));
            }
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

/// The user-facing reason a server failed to connect.
///
/// Every returned string is fixed or interpolates a configured *name* only —
/// never URL, token, or upstream error text. For gateway mounts the split
/// follows where the failure happened: sign-in state, then resolution/token
/// exchange (these fail as [`AgentError::Config`] inside the connectors and
/// gateway runtime, before any wire I/O), then the wire itself (openwave-mcp
/// failures arrive as non-`Config` classes).
fn connection_diagnostic(definition: &McpServerDefinition, error: &AgentError) -> String {
    if definition.gateway_endpoint.is_some() {
        if openwave_connectors::is_sign_in_required(error) {
            return "Sign in to the model gateway to reconnect this server.".to_string();
        }
        if matches!(error, AgentError::Config(_)) {
            return "Could not get access to this gateway endpoint. Check your \
                    entitlements for it."
                .to_string();
        }
        return "Could not connect to this gateway endpoint. Check that it is reachable \
                and allows this kind of access."
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

    /// Fresh boot-style ids for a test's definitions.
    fn ids_for(definitions: &[McpServerDefinition]) -> BTreeMap<String, ConnectedAppId> {
        definitions
            .iter()
            .map(|definition| (definition.name.clone(), ConnectedAppId::new()))
            .collect()
    }

    /// The persisted `mcp_server` definitions, read back through the
    /// connected-app record the way `initialize` does.
    async fn saved_definitions(store: &Arc<dyn Store>) -> Vec<McpServerDefinition> {
        store
            .list_connected_apps()
            .await
            .unwrap()
            .into_iter()
            .filter(|record| record.kind == ConnectedAppKind::McpServer)
            .map(|record| serde_json::from_value(record.definition).unwrap())
            .collect()
    }

    /// The persisted `mcp_server` records themselves, so a test can look at
    /// the stored JSON rather than the type it parses into.
    async fn saved_records(store: &Arc<dyn Store>) -> Vec<ConnectedApp> {
        store
            .list_connected_apps()
            .await
            .unwrap()
            .into_iter()
            .filter(|record| record.kind == ConnectedAppKind::McpServer)
            .collect()
    }

    /// Persist definitions as connected-app records, the way a settings save
    /// would, without connecting anything.
    async fn seed_records(store: &Arc<dyn Store>, definitions: &[McpServerDefinition]) {
        let now = chrono::Utc::now();
        let records: Vec<ConnectedApp> = definitions
            .iter()
            .map(|definition| ConnectedApp {
                id: ConnectedAppId::new(),
                name: definition.name.clone(),
                kind: ConnectedAppKind::McpServer,
                definition: serde_json::to_value(definition).unwrap(),
                created_at: now,
                updated_at: now,
            })
            .collect();
        store
            .replace_connected_apps(ConnectedAppKind::McpServer, &records)
            .await
            .unwrap();
    }

    /// The signed-out stand-in: every resolution demands a session.
    struct NoGateway;

    /// An in-memory secret store, so a test can assert what the runtime put
    /// there — and what it did not.
    #[derive(Default)]
    struct TestSecrets(std::sync::Mutex<BTreeMap<String, String>>);

    #[async_trait::async_trait]
    impl SecretProvider for TestSecrets {
        async fn get_secret(&self, key: &str) -> Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }
        async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
            self.0.lock().unwrap().insert(key.into(), value.into());
            Ok(())
        }
        async fn delete_secret(&self, key: &str) -> Result<()> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
    }
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
            env: BTreeSet::new(),
            env_values: BTreeMap::new(),
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
            env: BTreeSet::new(),
            env_values: BTreeMap::new(),
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
            env: BTreeSet::new(),
            env_values: BTreeMap::new(),
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
        test_runtime_with(gateway, Arc::new(crate::managed_policy::NoOsPolicy)).await
    }

    async fn test_runtime_with(
        gateway: Arc<dyn GatewayEndpoints>,
        os_policy: Arc<dyn crate::managed_policy::OsPolicySource>,
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
                Arc::new(TestSecrets::default()),
                gateway,
                os_policy,
            )),
            store,
            directory,
        )
    }

    /// The explicit-unmount memory end to end: a settings save that removes
    /// a gateway mount records the slug, auto-mount never resurrects it, and
    /// a manual remount clears the record so it stays remounted. (Signed
    /// out, the mount persists degraded — exactly what lets this run without
    /// a live gateway.)
    #[tokio::test]
    async fn an_explicit_unmount_is_remembered_and_never_auto_remounted() {
        let (runtime, store, _directory) = test_runtime().await;
        assert!(runtime
            .auto_mount_gateway_endpoints(&["docs".to_string()])
            .await
            .unwrap());
        let saved = saved_definitions(&store).await;
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].name, "docs");
        assert_eq!(saved[0].gateway_endpoint.as_deref(), Some("docs"));
        assert!(saved[0].enabled);

        // The user unmounts: a complete settings save without the mount.
        runtime
            .replace(McpServersConfig {
                servers: Vec::new(),
            })
            .await
            .unwrap();
        assert_eq!(
            store
                .get_setting(GATEWAY_ENDPOINT_UNMOUNTS_KEY)
                .await
                .unwrap()
                .unwrap(),
            serde_json::json!(["docs"])
        );

        // Auto-mount refuses to fight the recorded intent.
        assert!(!runtime
            .auto_mount_gateway_endpoints(&["docs".to_string()])
            .await
            .unwrap());
        assert!(runtime.info().await.servers.is_empty());

        // A manual remount clears the memory.
        runtime
            .replace(McpServersConfig {
                servers: vec![gateway_definition("docs", "docs")],
            })
            .await
            .unwrap();
        assert_eq!(
            store
                .get_setting(GATEWAY_ENDPOINT_UNMOUNTS_KEY)
                .await
                .unwrap()
                .unwrap(),
            serde_json::json!([])
        );
        // Already mounted: nothing to add, nothing rewritten.
        assert!(!runtime
            .auto_mount_gateway_endpoints(&["docs".to_string()])
            .await
            .unwrap());
    }

    /// An entitled slug colliding with a configured server name derives the
    /// same suffixed namespace the desktop's mount toggle would, instead of
    /// failing validation on the duplicate.
    #[tokio::test]
    async fn auto_mount_suffixes_a_name_a_manual_server_already_took() {
        let (runtime, _store, _directory) = test_runtime().await;
        runtime
            .replace(McpServersConfig {
                servers: vec![disabled_definition("docs", "/usr/local/bin/docs-mcp")],
            })
            .await
            .unwrap();

        assert!(runtime
            .auto_mount_gateway_endpoints(&["docs".to_string()])
            .await
            .unwrap());
        let info = runtime.info().await;
        let names: Vec<&str> = info
            .servers
            .iter()
            .map(|server| server.definition.name.as_str())
            .collect();
        assert_eq!(names, ["docs", "docs_2"]);
        assert_eq!(
            info.servers[1].definition.gateway_endpoint.as_deref(),
            Some("docs")
        );
    }

    #[test]
    fn parses_a_bounded_stdio_server_configuration() {
        let config = parse(
            r#"{
                "servers": [{
                    "name": "private_docs",
                    "command": "/usr/local/bin/docs-mcp",
                    "args": ["--stdio"],
                    "env": ["LOG_LEVEL"],
                    "env_values": {"LOG_LEVEL": "info"},
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
        assert!(server.env.contains("LOG_LEVEL"));
        assert_eq!(server.env_values.get("LOG_LEVEL").unwrap(), "info");
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
        let command = server.build_command(&BTreeMap::new()).unwrap();
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
                "env":["DOCS_TOKEN"],
                "env_from":["DOCS_TOKEN"]
            }]}"#,
        )
        .err()
        .unwrap();
        assert!(duplicate.to_string().contains("configured more than once"));

        let orphan = parse(
            r#"{"servers":[{
                "name":"docs",
                "command":"/bin/docs",
                "env_values":{"DOCS_TOKEN":"literal"}
            }]}"#,
        )
        .err()
        .unwrap();
        assert!(orphan.to_string().contains("names no configured variable"));

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
        let command = config.0[0].build_command(&BTreeMap::new()).unwrap();
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
        let gateway: Arc<dyn GatewayEndpoints> = Arc::new(NoGateway);
        let error = config.0[0]
            .connect(&gateway, &BTreeMap::new())
            .await
            .err()
            .unwrap();
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

        let saved = saved_definitions(&store).await;
        let live = runtime.info().await;
        assert_eq!(saved[0].name, "second");
        assert_eq!(live.servers[0].definition, saved[0]);
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
        let definitions = vec![definition];
        let ids = ids_for(&definitions);
        runtime.replace_permissive(definitions, ids).await;
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
            (r#""env":["A"]"#, "environment applies only"),
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
            .replace_permissive(
                vec![gateway_definition("tools", "tools")],
                ids_for(&[gateway_definition("tools", "tools")]),
            )
            .await;
        let info = runtime.info().await;
        assert_eq!(info.servers[0].health, McpHealth::Degraded);
        assert_eq!(
            info.servers[0].diagnostic.as_deref(),
            Some("Sign in to the model gateway to reconnect this server.")
        );

        // The degraded mount stays on the create_app roster, but its line
        // stops promising mounted tool names it does not have.
        let state = runtime.state.lock().await;
        let roster = connected_app_roster(&state.definitions, &state.ids, &state.servers, &[]);
        assert!(
            roster.contains(
                "tools are named `mcp__tools__{tool}` \
                 (currently unavailable — configured but not connected)"
            ),
            "{roster}"
        );
    }

    /// The two non-sign-in gateway failures are different problems with
    /// different fixes, and the diagnostic must say which one happened: a
    /// refused resolution/token exchange (`AgentError::Config`, before any
    /// wire I/O) is an entitlement problem, while a reached-or-unreachable
    /// endpoint (any other class) is an endpoint problem.
    #[tokio::test]
    async fn gateway_diagnostics_separate_refused_access_from_endpoint_failures() {
        // The gateway refuses to mint `mcp:<slug>` access: no wire I/O ever
        // happened, so "check the endpoint" would send the user the wrong way.
        struct RefusedGateway;

        #[async_trait::async_trait]
        impl GatewayEndpoints for RefusedGateway {
            async fn endpoint(&self, _slug: &str) -> Result<GatewayEndpointAccess> {
                Err(AgentError::config(
                    "model-gateway token request: the requested resource is not entitled",
                ))
            }
        }

        let (runtime, _store, _directory) =
            test_runtime_with_gateway(Arc::new(RefusedGateway)).await;
        let definitions = vec![gateway_definition("tools", "tools")];
        let ids = ids_for(&definitions);
        runtime.replace_permissive(definitions, ids).await;
        let info = runtime.info().await;
        assert_eq!(info.servers[0].health, McpHealth::Degraded);
        assert_eq!(
            info.servers[0].diagnostic.as_deref(),
            Some("Could not get access to this gateway endpoint. Check your entitlements for it.")
        );

        // Resolution succeeded but the endpoint itself answers 403: the wire
        // was reached, so entitlement language would be a lie.
        struct ResolvedGateway(std::net::SocketAddr);

        #[async_trait::async_trait]
        impl GatewayEndpoints for ResolvedGateway {
            async fn endpoint(&self, _slug: &str) -> Result<GatewayEndpointAccess> {
                Ok(GatewayEndpointAccess {
                    url: format!("http://{}/mcp", self.0),
                    bearer_token: "session-token".to_string(),
                })
            }
        }

        let app = axum::Router::new().route(
            "/mcp",
            axum::routing::post(|| async { axum::http::StatusCode::FORBIDDEN }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let (runtime, _store, _directory) =
            test_runtime_with_gateway(Arc::new(ResolvedGateway(address))).await;
        let definitions = vec![gateway_definition("tools", "tools")];
        let ids = ids_for(&definitions);
        runtime.replace_permissive(definitions, ids).await;
        let info = runtime.info().await;
        assert_eq!(info.servers[0].health, McpHealth::Degraded);
        assert_eq!(
            info.servers[0].diagnostic.as_deref(),
            Some(
                "Could not connect to this gateway endpoint. Check that it is reachable \
                 and allows this kind of access."
            )
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
        assert_eq!(saved_definitions(&store).await.len(), 2);

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
        assert_eq!(
            saved_definitions(&store).await.len(),
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
        let gateway: Arc<dyn GatewayEndpoints> = Arc::new(NoGateway);
        let error = definition
            .connect(&gateway, &BTreeMap::new())
            .await
            .err()
            .unwrap();
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

        // A connected server's roster line carries no availability caveat.
        {
            let state = runtime.state.lock().await;
            let roster = connected_app_roster(&state.definitions, &state.ids, &state.servers, &[]);
            assert!(roster.contains("tools are named `mcp__gateway__{tool}`"));
            assert!(!roster.contains("currently unavailable"), "{roster}");
        }

        // The declared view was prefetched at connect and is served from
        // memory, keyed by the configured namespace and declared URI.
        let view = runtime
            .ui_view_document("gateway", "ui://fixture/app.html")
            .await
            .expect("declared view is prefetched");
        assert_eq!(view.html, "<html>fixture view</html>");
        assert_eq!(view.mime_type.as_deref(), Some("text/html;profile=mcp-app"));

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
        seed_records(&store, &[manual, gateway_definition("tools", "tools")]).await;
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
        assert!(store.list_connected_apps().await.unwrap().is_empty());
    }

    /// The org's `AllowLocalMcpServers` opt-in narrows the managed lockdown
    /// to remote manual servers. A local stdio definition is the user's again
    /// — the runtime attempts its child (the fixture command doesn't exist,
    /// so it degrades) instead of forcing it down — and its edits are
    /// admitted, while a `url` server stays forced down with the managed
    /// diagnostic and adding one is still refused.
    #[tokio::test]
    async fn allow_local_mcp_scopes_the_lockdown_to_remote_transports() {
        struct ManagedAllowingLocal;

        impl crate::managed_policy::OsPolicySource for ManagedAllowingLocal {
            fn gateway_url(&self) -> Result<Option<String>> {
                Ok(Some("https://corp.gateway".to_string()))
            }
            fn allow_local_mcp_servers(&self) -> Result<Option<bool>> {
                Ok(Some(true))
            }
        }

        let (runtime, store, _directory) =
            test_runtime_with(Arc::new(NoGateway), Arc::new(ManagedAllowingLocal)).await;
        let mut local = disabled_definition("local_docs", "/bin/docs");
        local.enabled = true;
        let remote = http_definition("remote", "http://127.0.0.1:9/mcp");
        seed_records(&store, &[local.clone(), remote.clone()]).await;

        runtime
            .initialize(ConfiguredMcpServers::default())
            .await
            .unwrap();
        let info = runtime.info().await;
        assert_eq!(info.servers[0].health, McpHealth::Degraded);
        assert_ne!(
            info.servers[0].diagnostic.as_deref(),
            Some(MANAGED_DISABLED_DIAGNOSTIC)
        );
        assert_eq!(info.servers[1].health, McpHealth::Disabled);
        assert_eq!(
            info.servers[1].diagnostic.as_deref(),
            Some(MANAGED_DISABLED_DIAGNOSTIC)
        );

        // The admission check draws the same line: a body adding another
        // remote server is refused by name, while one that only edits the
        // stdio definition (disabling it) lands.
        let extra_remote = http_definition("extra_remote", "http://127.0.0.1:9/mcp");
        let outcome = runtime
            .replace_under_policy(
                McpServersConfig {
                    servers: vec![local.clone(), remote.clone(), extra_remote],
                },
                ManualLockdown::RemoteManual,
            )
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            McpReplaceOutcome::RefusedManual(refused) if refused == ["extra_remote"]
        ));

        local.enabled = false;
        let outcome = runtime
            .replace_under_policy(
                McpServersConfig {
                    servers: vec![local, remote],
                },
                ManualLockdown::RemoteManual,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, McpReplaceOutcome::Replaced(_)));
    }

    /// The v:2 canonical-form invariants that keep consent honest: derived
    /// from definition fields only, never a value oracle, covering the
    /// namespace (which decides what mounted names a grant reaches), and
    /// indifferent to toggles that don't change what the user consented to.
    #[test]
    fn fingerprints_derive_from_fields_cover_the_namespace_and_leak_no_values() {
        let mut definition = disabled_definition("docs", "/bin/docs");
        definition.env.insert("TOKEN".into());
        definition
            .env_values
            .insert("TOKEN".into(), "secret-a".into());
        let baseline = definition_fingerprint(&definition);

        let mut toggled = definition.clone();
        toggled.enabled = true;
        toggled.request_timeout_ms += 1;
        assert_eq!(
            definition_fingerprint(&toggled),
            baseline,
            "enabling or re-timing is not a change of what the user consented to"
        );

        let mut rotated = definition.clone();
        rotated.env_values.insert("TOKEN".into(), "secret-b".into());
        assert_eq!(
            definition_fingerprint(&rotated),
            baseline,
            "environment values never enter the canonical form"
        );

        let stored_only = disabled_definition("docs", "/bin/docs");
        let mut stored_only = stored_only;
        stored_only.env.insert("TOKEN".into());
        assert_eq!(
            definition_fingerprint(&stored_only),
            baseline,
            "moving the values into the secret store leaves every grant pinned \
             to this definition still matching"
        );

        let mut renamed = definition.clone();
        renamed.name = "docs2".into();
        assert_ne!(
            definition_fingerprint(&renamed),
            baseline,
            "app-keyed grants no longer key by name, so the namespace is \
             part of what a grant pins"
        );

        let mut swapped = definition.clone();
        swapped.command = Some("/bin/other".into());
        assert_ne!(definition_fingerprint(&swapped), baseline);
    }

    /// The whole point of the change: a value the user typed into Settings
    /// lands in the secret store, and nothing that leaves this process — the
    /// persisted record or the projection the renderer reads — carries it.
    #[tokio::test]
    async fn environment_values_reach_the_secret_store_and_nothing_else() {
        const VALUE: &str = "sk-not-a-real-key-2f1c";
        let (runtime, store, _directory) = test_runtime().await;
        let mut definition = disabled_definition("docs", "/bin/docs");
        definition.env.insert("DOCS_TOKEN".to_string());
        definition
            .env_values
            .insert("DOCS_TOKEN".to_string(), VALUE.to_string());
        runtime
            .replace(McpServersConfig {
                servers: vec![definition],
            })
            .await
            .unwrap();

        // The projection the renderer reads, serialized exactly as the route
        // sends it.
        let projected = serde_json::to_string(&runtime.info().await).unwrap();
        assert!(!projected.contains(VALUE), "{projected}");
        assert!(projected.contains("DOCS_TOKEN"), "{projected}");

        // The durable record.
        let record = &saved_records(&store).await[0];
        let stored = serde_json::to_string(&record.definition).unwrap();
        assert!(!stored.contains(VALUE), "{stored}");
        assert_eq!(record.definition["env"], serde_json::json!(["DOCS_TOKEN"]));

        // And where it did go.
        let secret = runtime
            .secrets()
            .get_secret(&env_secret_key(record.id))
            .await
            .unwrap()
            .expect("the value is in the secret store");
        assert_eq!(secret, format!(r#"{{"DOCS_TOKEN":"{VALUE}"}}"#));
    }

    /// A save that leaves a value blank keeps the stored one; dropping the
    /// name takes the value with it. Without this, editing any other field of
    /// a server would silently wipe its credentials.
    #[tokio::test]
    async fn a_blank_value_keeps_the_stored_one_and_removing_a_name_drops_it() {
        let (runtime, store, _directory) = test_runtime().await;
        let mut definition = disabled_definition("docs", "/bin/docs");
        definition.env.insert("DOCS_TOKEN".to_string());
        definition
            .env_values
            .insert("DOCS_TOKEN".to_string(), "first".to_string());
        runtime
            .replace(McpServersConfig {
                servers: vec![definition.clone()],
            })
            .await
            .unwrap();
        let id = saved_records(&store).await[0].id;

        // The renderer round-trips the definition it was given, which carries
        // names only — no `env_values` at all.
        let mut retimed = definition.clone();
        retimed.env_values.clear();
        retimed.request_timeout_ms += 1;
        runtime
            .replace(McpServersConfig {
                servers: vec![retimed],
            })
            .await
            .unwrap();
        assert_eq!(
            runtime
                .stored_env(id)
                .await
                .get("DOCS_TOKEN")
                .map(String::as_str),
            Some("first")
        );

        let mut cleared = definition.clone();
        cleared.env.clear();
        cleared.env_values.clear();
        runtime
            .replace(McpServersConfig {
                servers: vec![cleared],
            })
            .await
            .unwrap();
        assert!(runtime.stored_env(id).await.is_empty());
    }

    /// Definitions persisted before the values moved out carry them in
    /// cleartext. Boot migrates them into the secret store and rewrites the
    /// record, and the definition fingerprint — what every app grant is
    /// pinned to — comes out unchanged, so no grant is invalidated.
    #[tokio::test]
    async fn a_legacy_record_migrates_its_cleartext_values_without_moving_the_fingerprint() {
        const VALUE: &str = "legacy-secret-9a4d";
        let (runtime, store, _directory) = test_runtime().await;
        let expected = {
            let mut definition = disabled_definition("docs", "/bin/docs");
            definition.env.insert("DOCS_TOKEN".to_string());
            definition_fingerprint(&definition)
        };
        // The pre-migration shape, written straight to the store.
        let now = chrono::Utc::now();
        let id = ConnectedAppId::new();
        let legacy = serde_json::json!({
            "name": "docs",
            "command": "/bin/docs",
            "args": [],
            "env": {"DOCS_TOKEN": VALUE},
            "env_from": [],
            "cwd": null,
            "url": null,
            "bearer_token_env": null,
            "gateway_endpoint": null,
            "request_timeout_ms": DEFAULT_REQUEST_TIMEOUT_MS,
            "enabled": false,
        });
        store
            .replace_connected_apps(
                ConnectedAppKind::McpServer,
                &[ConnectedApp {
                    id,
                    name: "docs".to_string(),
                    kind: ConnectedAppKind::McpServer,
                    definition: legacy,
                    created_at: now,
                    updated_at: now,
                }],
            )
            .await
            .unwrap();

        runtime
            .initialize(ConfiguredMcpServers::default())
            .await
            .unwrap();

        let record = &saved_records(&store).await[0];
        assert_eq!(record.id, id, "the record keeps its identity");
        assert_eq!(record.definition["env"], serde_json::json!(["DOCS_TOKEN"]));
        assert!(!serde_json::to_string(&record.definition)
            .unwrap()
            .contains(VALUE));
        assert_eq!(
            runtime
                .stored_env(id)
                .await
                .get("DOCS_TOKEN")
                .map(String::as_str),
            Some(VALUE)
        );
        assert_eq!(
            runtime.app_fingerprints().await[&id].fingerprint,
            expected,
            "the canonical form only ever saw the names, so grants survive"
        );
    }

    #[test]
    fn debug_projection_redacts_argument_and_literal_environment_values() {
        let mut definition = disabled_definition("docs", "/bin/docs");
        definition.args = vec!["argument-secret".to_string()];
        definition.env.insert("TOKEN".to_string());
        definition
            .env_values
            .insert("TOKEN".to_string(), "literal-secret".to_string());
        let debug = format!("{definition:?}");
        assert!(!debug.contains("argument-secret"));
        assert!(!debug.contains("literal-secret"));
        assert!(debug.contains("TOKEN"));
    }
}
