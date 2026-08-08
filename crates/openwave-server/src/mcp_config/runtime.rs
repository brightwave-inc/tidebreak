//! Live MCP connection supervision and tool registry publication.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures::future::join_all;
use openwave_core::connected_app::{ConnectedApp, ConnectedAppKind};
use openwave_core::id::ConnectedAppId;
use openwave_core::local_app::CREATE_APP_TOOL;
use openwave_core::{AgentError, Result, SecretProvider, Store, ToolRegistry};
use openwave_mcp::{McpClient, McpProbe, MAX_SERVER_NAME_BYTES};
use tokio::sync::Mutex;

use crate::mcp_curated::{curation_for, McpCuration};

use super::types::*;
use super::validation::{connection_diagnostic, validate_servers};

pub(super) struct ManagedServer {
    client: Option<McpClient>,
    health: McpHealth,
    diagnostic: Option<String>,
    reconnect_backoff: Duration,
    pub(super) epoch: u64,
    pub(super) reconnect_lock: Arc<Mutex<()>>,
    /// Prefetched MCP Apps view documents, keyed by declared `ui://` URI.
    ui_views: HashMap<String, UiViewDocument>,
}

pub(super) struct RuntimeState {
    pub(super) definitions: Vec<McpServerDefinition>,
    /// The connected-app record id behind each configured server name. App
    /// manifests and grants bind these ids; the id survives edits to the
    /// definition and dies with the record, so a name-keyed lookup here is
    /// only ever a projection detail, never the consent key.
    ids: BTreeMap<String, ConnectedAppId>,
    pub(super) servers: HashMap<String, ManagedServer>,
}

/// Owns the current MCP connection set and atomically published tool registry.
///
/// A turn asks for one [`snapshot`](Self::snapshot) and keeps that `Arc` for its
/// entire live execution. Reconfiguration therefore affects only later turns;
/// old sessions remain alive until their last turn snapshot is dropped.
pub(crate) struct McpRuntime {
    base_tools: ToolRegistry,
    tools: RwLock<Arc<ToolRegistry>>,
    pub(super) state: Mutex<RuntimeState>,
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
    /// The host-folder seam, for the `create_app` roster's folders section.
    /// Installed after assembly on desktop embeddings (like
    /// `AppState::host_folders`); unset, the roster lists no folders.
    host_folders: std::sync::OnceLock<Arc<dyn crate::host_folders::HostFolders>>,
    /// The installed-plugin seam bundled MCP servers are derived from.
    /// Installed after assembly, like the host-folder seam, because the
    /// code-execution provider that owns the plugin tree is built after this
    /// runtime. Unset, no plugin contributes servers.
    plugin_catalog: std::sync::OnceLock<Arc<dyn crate::plugin_mcp::PluginMcpCatalog>>,
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
            host_folders: std::sync::OnceLock::new(),
            plugin_catalog: std::sync::OnceLock::new(),
            next_epoch: AtomicU64::new(1),
        }
    }

    /// Install the host-folder seam so the `create_app` roster can list
    /// approved folders. At most once, at assembly.
    pub(crate) fn set_host_folders(&self, host: Arc<dyn crate::host_folders::HostFolders>) {
        let _ = self.host_folders.set(host);
    }

    /// Install the installed-plugin seam. At most once, at assembly; the
    /// startup reconcile that first connects bundled servers runs after it.
    pub(crate) fn set_plugin_catalog(&self, catalog: Arc<dyn crate::plugin_mcp::PluginMcpCatalog>) {
        let _ = self.plugin_catalog.set(catalog);
    }

    /// The plugin-sourced definitions that belong beside `configured` right
    /// now, read live from the installed tree and the enable flags.
    ///
    /// Nothing here is persisted or remembered: the answer is recomputed on
    /// every reconcile and every replacement, so installing, uninstalling, or
    /// toggling a plugin needs no second copy of this state to stay in sync.
    /// Sources are sorted by plugin name so a namespace contest resolves the
    /// same way on every host regardless of directory iteration order.
    async fn plugin_definitions(
        &self,
        configured: &[McpServerDefinition],
    ) -> Vec<McpServerDefinition> {
        let Some(catalog) = self.plugin_catalog.get() else {
            return Vec::new();
        };
        let mut sources = catalog.sources().await;
        sources.sort_by(|left, right| left.plugin.cmp(&right.plugin));
        let taken: HashSet<String> = configured
            .iter()
            .map(|definition| definition.name.clone())
            .collect();
        let (definitions, skipped) = crate::plugin_mcp::derive_definitions(&sources, &taken);
        for entry in skipped {
            tracing::warn!(
                plugin = %entry.plugin,
                server = %entry.server,
                "plugin MCP server was not mounted: {}",
                entry.reason
            );
        }
        definitions
    }

    /// One server's literal environment as stored, by record id. A missing or
    /// unreadable entry resolves empty: the child then starts without those
    /// names and fails with the server's own diagnostic, which beats taking
    /// an unrelated settings save down.
    pub(super) async fn stored_env(&self, id: ConnectedAppId) -> BTreeMap<String, String> {
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
                        curated: curation(definition),
                        definition: definition.clone(),
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
            if crate::connectors::validate_mcp_endpoint_slug(slug).is_err() {
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
                plugin: None,
                launch: None,
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
    pub(super) async fn replace_with_commit_pause(
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
        let configured = definitions;
        // Plugin-sourced servers ride along the same connection pass but are
        // never part of what is persisted or validated as a candidate: they
        // are derived from the installed tree, so a replacement that fails
        // cannot take them down and a replacement that succeeds cannot edit
        // them.
        let plugin = self.plugin_definitions(&configured).await;
        let definitions: Vec<McpServerDefinition> =
            configured.iter().cloned().chain(plugin).collect();
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
                // settings save until it was deleted. A plugin-sourced server
                // degrades for the same reason from the other direction: it is
                // not part of the candidate at all, so it must never be able
                // to fail somebody's settings save.
                Err(error)
                    if definition.gateway_endpoint.is_some() || definition.plugin.is_some() =>
                {
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
                        diagnostic: disabled_diagnostic(definition, lockdown),
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
            // still what this replacement diffs against. Plugin-sourced
            // definitions are excluded: they are derived, so persisting them
            // would create a second, staler home for the same facts.
            self.remember_endpoint_unmounts(&configured).await;
            self.persist_definitions(&configured, &ids).await?;
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

    pub(super) async fn replace_permissive(
        &self,
        definitions: Vec<McpServerDefinition>,
        ids: BTreeMap<String, ConnectedAppId>,
    ) {
        let plugin = self.plugin_definitions(&definitions).await;
        let definitions: Vec<McpServerDefinition> = definitions.into_iter().chain(plugin).collect();
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
                    diagnostic: disabled_diagnostic(definition, lockdown),
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

    /// Bring the plugin-sourced slice of the connection set in line with the
    /// installed tree and the current enable flags.
    ///
    /// Run at startup, after a plugin is installed, and after the enable flags
    /// change — the three moments the derived set can move. Only the plugin
    /// slice is touched: a server whose definition is unchanged keeps its live
    /// connection, one that appeared connects, and one whose plugin was
    /// switched off or uninstalled is disconnected and its tools unmount.
    /// User-configured servers are never reconnected by this, so toggling a
    /// plugin does not churn somebody's stdio child.
    ///
    /// Returns whether the published set changed.
    pub(crate) async fn reconcile_plugin_servers(&self) -> bool {
        if self.plugin_catalog.get().is_none() {
            return false;
        }
        let _mutation = self.mutation.lock().await;
        let (configured, live): (Vec<McpServerDefinition>, Vec<McpServerDefinition>) = {
            let state = self.state.lock().await;
            state
                .definitions
                .iter()
                .cloned()
                .partition(|definition| definition.plugin.is_none())
        };
        let desired = self.plugin_definitions(&configured).await;
        if desired == live {
            return false;
        }
        let ids = self.state.lock().await.ids.clone();
        let lockdown = self.manual_lockdown().await;
        let gateway = &self.gateway;
        // Only the entries that are new or changed are connected; the rest
        // keep the client they already hold.
        let fresh: Vec<&McpServerDefinition> = desired
            .iter()
            .filter(|definition| !live.contains(definition))
            .collect();
        let connections = join_all(fresh.iter().map(|definition| async move {
            if connects(definition, lockdown) {
                definition
                    .connect_with_views(gateway, &BTreeMap::new())
                    .await
                    .map(Some)
            } else {
                Ok(None)
            }
        }))
        .await;

        let mut state = self.state.lock().await;
        // Everything the desired set does not name goes away, taking its
        // client — and so its mounted tools — with it.
        let keep: HashSet<&str> = desired
            .iter()
            .map(|definition| definition.name.as_str())
            .chain(configured.iter().map(|definition| definition.name.as_str()))
            .collect();
        state.servers.retain(|name, _| keep.contains(name.as_str()));
        for (definition, connection) in fresh.into_iter().zip(connections) {
            let managed = match connection {
                Ok(Some((client, ui_views))) => ManagedServer {
                    client: Some(client),
                    health: McpHealth::Healthy,
                    diagnostic: None,
                    reconnect_backoff: INITIAL_RECONNECT_BACKOFF,
                    epoch: self.fresh_epoch(),
                    reconnect_lock: Arc::new(Mutex::new(())),
                    ui_views,
                },
                Ok(None) => ManagedServer {
                    client: None,
                    health: McpHealth::Disabled,
                    diagnostic: disabled_diagnostic(definition, lockdown),
                    reconnect_backoff: INITIAL_RECONNECT_BACKOFF,
                    epoch: self.fresh_epoch(),
                    reconnect_lock: Arc::new(Mutex::new(())),
                    ui_views: HashMap::new(),
                },
                Err(error) => {
                    tracing::warn!(
                        server = %definition.name,
                        plugin = ?definition.plugin,
                        "plugin MCP server connection failed: {error}"
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
            state.servers.insert(definition.name.clone(), managed);
        }
        state.definitions = configured.into_iter().chain(desired).collect();
        state.ids = ids;
        let registry = self.registry_for(&state).await;
        *self
            .tools
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(registry);
        true
    }

    async fn publish(
        &self,
        definitions: Vec<McpServerDefinition>,
        ids: BTreeMap<String, ConnectedAppId>,
        servers: HashMap<String, ManagedServer>,
    ) {
        let rest = self.rest_roster().await;
        let folders = self.folder_roster().await;
        let registry = self.registry_with(&servers, &rest, &folders);
        let mut state = self.state.lock().await;
        state.definitions = definitions;
        state.ids = ids;
        state.servers = servers;
        *self
            .tools
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(registry);
    }

    /// Republish the tool registry from the current state so the
    /// `create_app` roster reflects the stored `rest_api` records right now.
    ///
    /// The registry is otherwise rebuilt only when MCP configuration or
    /// connections change, so without this a REST record saved from Settings
    /// stays invisible to `create_app` — the description keeps claiming no
    /// connected apps exist — until something unrelated republishes. The
    /// connected-apps CRUD surface calls this after every store write; MCP
    /// connections are untouched.
    pub(crate) async fn refresh_connected_app_roster(&self) {
        let state = self.state.lock().await;
        let registry = self.registry_for(&state).await;
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
        let folders = self.folder_roster().await;
        self.registry_with(&state.servers, &rest, &folders)
    }

    /// Every approved connected folder, for the roster's folders section.
    /// Best-effort like the rest roster: no seam or an unreadable host
    /// degrades to an absent section, never a failed registry rebuild.
    async fn folder_roster(&self) -> Vec<crate::host_folders::ApprovedFolder> {
        let Some(host) = self.host_folders.get() else {
            return Vec::new();
        };
        match host.approved_roots().await {
            Ok(folders) => folders,
            Err(error) => {
                tracing::warn!(
                    "could not read approved folders for the create_app roster: {error}"
                );
                Vec::new()
            }
        }
    }

    fn registry_with(
        &self,
        servers: &HashMap<String, ManagedServer>,
        rest: &[RestRosterApp],
        folders: &[crate::host_folders::ApprovedFolder],
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
                roster: connected_app_roster(rest, folders),
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

    pub(super) async fn mark_degraded(&self, name: &str, epoch: u64, backoff: Duration) {
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
                        health: managed.map_or(McpHealth::Initializing, |server| server.health),
                        tool_count: managed
                            .and_then(|server| server.client.as_ref())
                            .map_or(0, |client| client.tools().count()),
                        diagnostic: managed.and_then(|server| server.diagnostic.clone()),
                        curated: curation(definition),
                        definition: definition.clone(),
                    }
                })
                .collect(),
        }
    }
}

/// The curated-list entry this definition matches, if any.
///
/// A gateway mount carries neither a command nor a URL here — the endpoint is
/// resolved from the session at connect time — so it never matches, which is
/// the honest answer: the curated list is about servers we drove ourselves.
fn curation(definition: &McpServerDefinition) -> Option<McpCuration> {
    curation_for(
        definition.command.as_deref(),
        &definition.args,
        definition.url.as_deref(),
    )
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

/// Why a server that is not holding a connection is off.
///
/// The managed lockdown comes first: a plugin server the policy covers reads
/// as locked, exactly like a user-typed manual server, rather than advertising
/// the transport reason behind it. Otherwise a plugin-sourced entry explains
/// itself — an `sse` server is listed and inert with the reason, because the
/// specification keeps that transport optional and silently omitting the entry
/// would leave nothing to explain the absence.
fn disabled_diagnostic(
    definition: &McpServerDefinition,
    lockdown: ManualLockdown,
) -> Option<String> {
    managed_lockdown_diagnostic(definition, lockdown).or_else(|| {
        definition
            .launch
            .as_ref()
            .and_then(|launch| launch.disabled_reason.clone())
    })
}
