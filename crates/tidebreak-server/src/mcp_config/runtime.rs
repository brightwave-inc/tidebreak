//! Live MCP connection supervision and tool registry publication.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures::future::join_all;
use tidebreak_core::connected_app::{validate_connected_app, ConnectedApp, ConnectedAppKind};
use tidebreak_core::id::ConnectedAppId;
use tidebreak_core::local_app::CREATE_APP_TOOL;
use tidebreak_core::{AgentError, Result, SecretProvider, Store, ToolRegistry};
use tidebreak_mcp::{McpClient, McpProbe, MAX_SERVER_NAME_BYTES};
use tokio::sync::Mutex;

use crate::connectors::{
    bind_mcp_loopback, build_authorize_url, pkce_pair, Discovery, McpOAuthClient,
    McpOAuthCredentialVault, PendingMcpSignIn, SignInFailure,
};
use crate::mcp_curated::{curation_for, McpCuration};
use crate::mcp_oauth_runtime::{McpOAuthState, McpOAuthStatus};

use super::oauth::{self, OAuthAccess, OAuthNeed, SignInProgress, SignInRecord, SignInView};
use super::types::*;
use super::validation::{
    failure_diagnostic, failure_park, validate_server, validate_servers, validation_reason,
};

/// One connection attempt's outcome, with what it taught the runtime about
/// OAuth.
type ConnectAttempt = (
    Result<(McpClient, HashMap<String, UiViewDocument>)>,
    Option<OAuthNeed>,
);

/// One sign-in the background task finishes: which server, at which URL,
/// and which attempt, so it can tell whether it is still the one on file.
struct FinishingSignIn {
    id: ConnectedAppId,
    name: String,
    server_url: String,
    generation: u64,
}

/// What [`McpRuntime::info`] reads under the state lock for one server that
/// signs in, to project its OAuth status after the lock is released.
struct OAuthSnapshot {
    index: usize,
    id: ConnectedAppId,
    url: String,
    flag: bool,
    need: Option<OAuthNeed>,
    progress: Option<SignInView>,
}

pub(super) struct ManagedServer {
    client: Option<McpClient>,
    health: McpHealth,
    diagnostic: Option<String>,
    resolved_command: Option<String>,
    pub(super) reconnect: Reconnect,
    pub(super) epoch: u64,
    pub(super) reconnect_lock: Arc<Mutex<()>>,
    /// Prefetched MCP Apps view documents, keyed by declared `ui://` URI.
    ui_views: HashMap<String, UiViewDocument>,
    /// Set when the last connection failed because the server asks for an
    /// OAuth sign-in. Cleared by the next successful connection.
    pub(super) oauth: Option<OAuthNeed>,
}

/// How the supervisor retries one server that is not connected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Reconnect {
    /// The wait before the next attempt. Each failure doubles it, up to
    /// [`MAX_RECONNECT_BACKOFF`].
    backoff: Duration,
    /// Set when retrying cannot help until something outside changes. The
    /// supervisor then leaves the server alone.
    pub(super) parked: Option<ReconnectPark>,
    /// The failure last written to the log, so a repeat of it stays quiet.
    reported: Option<String>,
}

impl Default for Reconnect {
    fn default() -> Self {
        Self {
            backoff: INITIAL_RECONNECT_BACKOFF,
            parked: None,
            reported: None,
        }
    }
}

impl Reconnect {
    /// The state after a first connection failed. The caller logs that
    /// failure itself, so it counts as reported.
    fn after_failure(park: Option<ReconnectPark>, diagnostic: String) -> Self {
        Self {
            backoff: INITIAL_RECONNECT_BACKOFF,
            parked: park,
            reported: Some(diagnostic),
        }
    }

    /// Record one more failed attempt. Returns whether it differs from the
    /// failure last logged, which is when it earns a line in the log.
    pub(super) fn failed(&mut self, park: Option<ReconnectPark>, diagnostic: &str) -> bool {
        self.backoff = self.backoff.saturating_mul(2).min(MAX_RECONNECT_BACKOFF);
        self.parked = park;
        let changed = self.reported.as_deref() != Some(diagnostic);
        self.reported = Some(diagnostic.to_owned());
        changed
    }

    /// Let a server parked for want of a gateway session retry now.
    fn resume_after_sign_in(&mut self) {
        if self.parked == Some(ReconnectPark::SignIn) {
            self.parked = None;
            self.backoff = INITIAL_RECONNECT_BACKOFF;
        }
    }
}

/// Log a reconnect failure that differs from the last one logged for the
/// server, saying whether the supervisor keeps retrying.
fn report_reconnect_failure(name: &str, park: Option<ReconnectPark>, error: &AgentError) {
    match park {
        Some(ReconnectPark::SignIn) => tracing::warn!(
            server = %name,
            "MCP server needs a model-gateway sign-in; it retries after the next sign-in: {error}"
        ),
        Some(ReconnectPark::Configuration) => tracing::warn!(
            server = %name,
            "MCP server cannot start with its configuration; it retries after a settings \
             change or a manual reconnect: {error}"
        ),
        Some(ReconnectPark::Authorization) => tracing::warn!(
            server = %name,
            "MCP server asks for an OAuth sign-in; it retries after someone connects it, \
             a settings change, or a manual reconnect: {error}"
        ),
        None => tracing::warn!(server = %name, "MCP server reconnect failed: {error}"),
    }
}

pub(super) struct RuntimeState {
    pub(super) definitions: Vec<McpServerDefinition>,
    /// The connected-app record id behind each configured server name. App
    /// manifests and grants bind these ids; the id survives edits to the
    /// definition and dies with the record, so a name-keyed lookup here is
    /// only ever a projection detail, never the consent key.
    ids: BTreeMap<String, ConnectedAppId>,
    pub(super) servers: HashMap<String, ManagedServer>,
    /// Saved records the loader could not load, kept as stored so a save
    /// writes them back unchanged instead of deleting them.
    skipped: Vec<SkippedRecord>,
}

/// One saved record the loader skipped, and why.
#[derive(Clone)]
struct SkippedRecord {
    record: ConnectedApp,
    reason: String,
}

/// The `create_app` roster inputs one registry rebuild reads.
struct Rosters {
    rest: Vec<RestRosterApp>,
    folders: Vec<crate::host_folders::ApprovedFolder>,
    gateway: Vec<GatewayRosterApp>,
}

/// Why a record whose environment values could not move into the credential
/// store is not loaded this time.
const MIGRATION_FAILED: &str = "Tidebreak could not move this server's environment values into \
                                the credential store, so it did not start the server without \
                                them. It tries again the next time Tidebreak starts.";

/// The saved servers [`McpRuntime::initialize`] published as connecting.
///
/// [`connect`](Self::connect) brings them up. Boot runs it in the background
/// after the listener binds, so a slow server never keeps the port closed.
#[must_use = "published servers stay connecting until their boot connections run"]
pub struct McpBoot {
    runtime: Arc<McpRuntime>,
    /// Each server to connect, with the epoch it was published under.
    servers: Vec<(String, u64)>,
}

impl McpBoot {
    /// Connect every published server at once, publishing each one's tools as
    /// soon as it is up.
    pub async fn connect(self) {
        self.runtime.connect_at_boot(self.servers).await;
    }
}

/// Owns the current MCP connection set and atomically published tool registry.
///
/// A turn asks for one [`snapshot`](Self::snapshot) and keeps that `Arc` for its
/// entire live execution. Reconfiguration therefore affects only later turns;
/// old sessions remain alive until their last turn snapshot is dropped.
pub struct McpRuntime {
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
    /// The provisioned-policy home for managed-mode resolution: the sticky
    /// pairing record, read together with the OS authority.
    provisioned_policy: Arc<dyn crate::managed_policy::ProvisionedPolicySource>,
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
    /// Each server's OAuth sign-in, by connected-app record id, while it runs
    /// and after it fails. Kept apart from the connection set, which a
    /// settings save replaces wholesale, so a save does not lose a sign-in
    /// that is waiting on the browser.
    sign_ins: std::sync::Mutex<HashMap<ConnectedAppId, SignInRecord>>,
    next_sign_in: AtomicU64,
    /// `false` while the saved servers published at boot are still making
    /// their first connection. A turn waits on it, briefly; see
    /// [`snapshot_after_boot`](Self::snapshot_after_boot).
    boot_settled: tokio::sync::watch::Sender<bool>,
    /// Lets a test's fake authorization server live on loopback.
    #[cfg(test)]
    oauth_loopback: AtomicBool,
}

impl McpRuntime {
    pub fn new(
        base_tools: Arc<ToolRegistry>,
        store: Arc<dyn Store>,
        secrets: Arc<dyn SecretProvider>,
        gateway: Arc<dyn GatewayEndpoints>,
        provisioned_policy: Arc<dyn crate::managed_policy::ProvisionedPolicySource>,
        os_policy: Arc<dyn crate::managed_policy::OsPolicySource>,
    ) -> Self {
        Self {
            base_tools: (*base_tools).clone(),
            tools: RwLock::new(base_tools),
            state: Mutex::new(RuntimeState {
                definitions: Vec::new(),
                ids: BTreeMap::new(),
                servers: HashMap::new(),
                skipped: Vec::new(),
            }),
            mutation: Mutex::new(()),
            store,
            secrets,
            gateway,
            provisioned_policy,
            os_policy,
            host_folders: std::sync::OnceLock::new(),
            plugin_catalog: std::sync::OnceLock::new(),
            next_epoch: AtomicU64::new(1),
            sign_ins: std::sync::Mutex::new(HashMap::new()),
            next_sign_in: AtomicU64::new(1),
            boot_settled: tokio::sync::watch::Sender::new(true),
            #[cfg(test)]
            oauth_loopback: AtomicBool::new(false),
        }
    }

    /// Admit plain-`http` loopback OAuth endpoints, so a test can stand up a
    /// fake authorization server beside a fake MCP server.
    #[cfg(test)]
    pub(super) fn admit_loopback_oauth_for_tests(&self) {
        self.oauth_loopback.store(true, Ordering::Relaxed);
    }

    /// The HTTP client for OAuth discovery, registration, and tokens.
    fn oauth_client(&self) -> Result<McpOAuthClient> {
        #[cfg(test)]
        if self.oauth_loopback.load(Ordering::Relaxed) {
            return McpOAuthClient::admitting_loopback_for_tests();
        }
        McpOAuthClient::new()
    }

    /// Connect one definition.
    ///
    /// A server that [signs in](oauth::signs_in) presents its stored OAuth
    /// session, if it has one. When such a server refuses the handshake with
    /// a `401`, it is asked how to authorize: one that names an OAuth sign-in
    /// fails as "sign in required" rather than as an authentication failure,
    /// so Settings can offer Connect.
    async fn connect_server(
        &self,
        definition: &McpServerDefinition,
        env: &BTreeMap<String, String>,
        app_id: Option<ConnectedAppId>,
    ) -> ConnectAttempt {
        let access = match app_id {
            Some(id) if oauth::signs_in(definition) => {
                self.oauth_client().ok().map(|client| OAuthAccess {
                    secrets: Arc::clone(&self.secrets),
                    id,
                    client,
                })
            }
            _ => None,
        };
        let result = definition
            .connect_with_views(&self.gateway, env, access.as_ref())
            .await;
        let (Err(error), Some(access), Some(url)) = (&result, &access, &definition.url) else {
            return (result, None);
        };
        if !tidebreak_mcp::is_unauthorized(error) {
            return (result, None);
        }
        match oauth::detect(url, &access.client).await {
            Some(need) => {
                let error = match &need {
                    OAuthNeed::SignIn { .. } => AgentError::SignInRequired(
                        "the MCP server asks for an OAuth sign-in".into(),
                    ),
                    OAuthNeed::Unsupported(reason) => AgentError::config(format!(
                        "the MCP server asks for an OAuth sign-in Tidebreak cannot complete: {}",
                        reason.reason()
                    )),
                    OAuthNeed::Unavailable => AgentError::config(
                        "the MCP server asks for an OAuth sign-in, and its sign-in service did \
                         not answer",
                    ),
                };
                (Err(error), Some(need))
            }
            None => (result, None),
        }
    }

    /// Install the host-folder seam so the `create_app` roster can list
    /// approved folders. At most once, at assembly.
    pub fn set_host_folders(&self, host: Arc<dyn crate::host_folders::HostFolders>) {
        let _ = self.host_folders.set(host);
    }

    /// Install the installed-plugin seam. At most once, at assembly; the
    /// startup reconcile that first connects bundled servers runs after it.
    pub fn set_plugin_catalog(&self, catalog: Arc<dyn crate::plugin_mcp::PluginMcpCatalog>) {
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
            self.commit_env_value(definition, id).await?;
        }
        Ok(())
    }

    /// Commit one definition's environment values under record `id` and
    /// empty its `env_values`, as [`commit_env_values`](Self::commit_env_values)
    /// does for each definition.
    async fn commit_env_value(
        &self,
        definition: &mut McpServerDefinition,
        id: ConnectedAppId,
    ) -> Result<()> {
        let mut values = self.stored_env(id).await;
        values.append(&mut definition.env_values);
        values.retain(|name, _| definition.env.contains(name));
        let key = env_secret_key(id);
        if values.is_empty() {
            let _ = self.secrets.delete_secret(&key).await;
            return Ok(());
        }
        let encoded = serde_json::to_string(&values).map_err(|error| {
            AgentError::config(format!("could not encode MCP environment values: {error}"))
        })?;
        self.secrets.set_secret(&key, &encoded).await
    }

    /// The gateway resolver MCP dispatch rides on — exposed so tests can pin
    /// that it is the same instance as `AppState::gateway` (#1441: a second
    /// runtime splits attestation contexts and every attested `tools/call`
    /// is refused).
    #[cfg(test)]
    pub fn gateway_endpoints(&self) -> Arc<dyn GatewayEndpoints> {
        self.gateway.clone()
    }

    /// The secret store this runtime writes environment values to — so a test
    /// can read back exactly what landed there.
    #[cfg(test)]
    pub fn secrets(&self) -> Arc<dyn SecretProvider> {
        self.secrets.clone()
    }

    /// How far managed policy currently locks the manual transports.
    ///
    /// Read per operation rather than cached at boot, like every other policy
    /// consumer, so an MDM push or removal takes effect without a restart. An
    /// unreadable policy fails closed to the full lockdown — the same judgment
    /// the BYOK boot paths make.
    async fn manual_lockdown(&self) -> ManualLockdown {
        match crate::managed_policy::resolve(&*self.provisioned_policy, &*self.os_policy) {
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
    pub async fn enforce_manual_lockdown(&self) -> bool {
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

    /// Load the saved `mcp_server` records, or the legacy boot file when there
    /// are none, and publish them.
    ///
    /// Saved records never fail boot. A record that does not decode, fails
    /// validation, or cannot move its stored environment values into the
    /// credential store is skipped: it stays on file, and Connected apps lists
    /// it with the reason. Every other saved server is published as
    /// connecting, without a network wait, and the returned [`McpBoot`]
    /// connects them. Boot runs it after the listener binds.
    ///
    /// The boot file stays fail-closed: its servers connect here, and one that
    /// fails stops boot, so a headless deployment never starts with fewer tools
    /// than its file names.
    pub async fn initialize(self: &Arc<Self>, boot: ConfiguredMcpServers) -> Result<McpBoot> {
        let records: Vec<ConnectedApp> = self
            .store
            .list_connected_apps()
            .await?
            .into_iter()
            .filter(|record| record.kind == ConnectedAppKind::McpServer)
            .collect();
        if !records.is_empty() {
            let (definitions, ids, skipped) = self.load_records(records).await;
            return Ok(self.publish_starting(definitions, ids, skipped).await);
        }
        if boot.is_empty() {
            return Ok(self
                .publish_starting(Vec::new(), BTreeMap::new(), Vec::new())
                .await);
        }
        // The boot file is a host-environment artifact: on a managed
        // profile it is exactly the channel the lockdown exists to
        // close, so it is inert rather than partially honored — unless
        // the org's `AllowLocalMcpServers` opt-in re-opens the local
        // channel, in which case any remote (`url`) definitions it names
        // are still forced down per definition below. The warning is the
        // operator's diagnostic for the silence.
        if self.manual_lockdown().await == ManualLockdown::AllManual {
            tracing::warn!(
                "{CONFIG_ENV} is ignored on a managed profile; \
                 mount MCP endpoints from the model gateway instead"
            );
            return Ok(self
                .publish_starting(Vec::new(), BTreeMap::new(), Vec::new())
                .await);
        }
        self.replace_strict(boot.0, false).await?;
        Ok(McpBoot {
            runtime: Arc::clone(self),
            servers: Vec::new(),
        })
    }

    /// Decode, validate, and migrate each saved record, setting aside the ones
    /// that cannot be loaded.
    ///
    /// Records written before literal environment values moved into the
    /// secret store carry them in cleartext. Their values move now, one record
    /// at a time, and the records are rewritten without them. A record whose
    /// values cannot move is skipped rather than started without its
    /// credentials, and its record keeps the values for the next boot.
    async fn load_records(
        &self,
        records: Vec<ConnectedApp>,
    ) -> (
        Vec<McpServerDefinition>,
        BTreeMap<String, ConnectedAppId>,
        Vec<SkippedRecord>,
    ) {
        let mut definitions: Vec<McpServerDefinition> = Vec::with_capacity(records.len());
        let mut ids = BTreeMap::new();
        let mut skipped = Vec::new();
        let mut migrated = Vec::new();
        for record in records {
            let loaded = match decode_record(&record) {
                Err(reason) => Err(reason),
                Ok(_) if ids.contains_key(&record.name) => {
                    Err("Another saved server already uses this name.".to_string())
                }
                Ok(_) if definitions.len() >= MAX_SERVERS => Err(format!(
                    "Tidebreak loads at most {MAX_SERVERS} MCP servers, and this one is past \
                     that limit."
                )),
                Ok(mut definition) if !definition.env_values.is_empty() => {
                    match self.commit_env_value(&mut definition, record.id).await {
                        Ok(()) => {
                            migrated.push(record.name.clone());
                            Ok(definition)
                        }
                        Err(error) => {
                            tracing::warn!(
                                server = %record.name,
                                "could not move stored MCP environment values into the secret \
                                 store: {error}"
                            );
                            Err(MIGRATION_FAILED.to_string())
                        }
                    }
                }
                Ok(definition) => Ok(definition),
            };
            match loaded {
                Ok(definition) => {
                    ids.insert(record.name.clone(), record.id);
                    definitions.push(definition);
                }
                Err(reason) => {
                    tracing::warn!(
                        server = %record.name,
                        "skipped a saved MCP server Tidebreak could not load: {reason}"
                    );
                    skipped.push(SkippedRecord { record, reason });
                }
            }
        }
        if !migrated.is_empty() {
            tracing::info!(
                servers = ?migrated,
                "moved stored MCP environment values into the secret store"
            );
            // The values are stored now; rewrite the records without them. A
            // failure leaves the cleartext in the records, and the next boot
            // moves the same values again.
            if let Err(error) = self.persist_definitions(&definitions, &ids, &skipped).await {
                tracing::warn!(
                    "could not rewrite MCP server records without their environment values: \
                     {error}"
                );
            }
        }
        (definitions, ids, skipped)
    }

    /// Publish `configured` and the plugin servers beside it without
    /// connecting anything: each server that connects reads as connecting,
    /// the rest as off. Returns the connections to run.
    ///
    /// The registry published here carries the local `create_app` roster
    /// only. The gateway's part of it is a network read, so it joins when the
    /// boot connections have landed.
    async fn publish_starting(
        self: &Arc<Self>,
        configured: Vec<McpServerDefinition>,
        ids: BTreeMap<String, ConnectedAppId>,
        skipped: Vec<SkippedRecord>,
    ) -> McpBoot {
        let plugin = self.plugin_definitions(&configured).await;
        let definitions: Vec<McpServerDefinition> = configured.into_iter().chain(plugin).collect();
        let lockdown = self.manual_lockdown().await;
        let mut servers = HashMap::with_capacity(definitions.len());
        let mut connecting = Vec::new();
        for definition in &definitions {
            let epoch = self.fresh_epoch();
            let starts = connects(definition, lockdown);
            if starts {
                connecting.push((definition.name.clone(), epoch));
            }
            servers.insert(
                definition.name.clone(),
                ManagedServer {
                    client: None,
                    health: if starts {
                        McpHealth::Initializing
                    } else {
                        McpHealth::Disabled
                    },
                    diagnostic: if starts {
                        None
                    } else {
                        disabled_diagnostic(definition, lockdown)
                    },
                    resolved_command: None,
                    reconnect: Reconnect::default(),
                    epoch,
                    reconnect_lock: Arc::new(Mutex::new(())),
                    ui_views: HashMap::new(),
                    oauth: None,
                },
            );
        }
        if !connecting.is_empty() {
            self.boot_settled.send_replace(false);
        }
        let rosters = self.local_rosters().await;
        self.forget_stale_sign_ins(&definitions, &ids);
        let mut state = self.state.lock().await;
        state.definitions = definitions;
        state.ids = ids;
        state.servers = servers;
        state.skipped = skipped;
        self.write_registry(&state, &rosters);
        McpBoot {
            runtime: Arc::clone(self),
            servers: connecting,
        }
    }

    /// Make the first connection to every server [`publish_starting`]
    /// published, all at once, then mark boot settled and publish the
    /// registry with the gateway's roster.
    ///
    /// [`publish_starting`]: Self::publish_starting
    async fn connect_at_boot(self: &Arc<Self>, servers: Vec<(String, u64)>) {
        if !servers.is_empty() {
            // Before any connection presents a stored session, as a
            // replacement does: a session issued for another URL goes.
            let (definitions, ids) = {
                let state = self.state.lock().await;
                (state.definitions.clone(), state.ids.clone())
            };
            self.reconcile_oauth_sessions(&definitions, &ids).await;
            let rosters = self.local_rosters().await;
            join_all(
                servers
                    .iter()
                    .map(|(name, epoch)| self.connect_booting(name, *epoch, &rosters)),
            )
            .await;
        }
        self.boot_settled.send_replace(true);
        self.republish().await;
    }

    /// Make one published server's first connection, and publish its tools
    /// the moment it is up.
    ///
    /// It holds the server's reconnect lock while it connects, so a manual
    /// reconnect or the supervisor waits for this attempt instead of starting
    /// a second child. A settings save that replaced the server meanwhile
    /// wins: this result is dropped.
    async fn connect_booting(&self, name: &str, epoch: u64, rosters: &Rosters) {
        let (reconnect_lock, definition, app_id) = {
            let state = self.state.lock().await;
            let Some(server) = state
                .servers
                .get(name)
                .filter(|server| server.epoch == epoch)
            else {
                return;
            };
            let Some(definition) = state
                .definitions
                .iter()
                .find(|definition| definition.name == name)
                .cloned()
            else {
                return;
            };
            (
                server.reconnect_lock.clone(),
                definition,
                state.ids.get(name).copied(),
            )
        };
        let _reconnect = reconnect_lock.lock().await;
        if self
            .state
            .lock()
            .await
            .servers
            .get(name)
            .is_none_or(|server| server.epoch != epoch)
        {
            return;
        }
        // Read here rather than before boot: a keychain read can wait on a
        // prompt, and that wait must not hold the port closed.
        let env = match app_id {
            Some(id) if !definition.env.is_empty() => self.stored_env(id).await,
            _ => BTreeMap::new(),
        };
        let (result, oauth) = self.connect_server(&definition, &env, app_id).await;
        let resolved_command = match &result {
            Ok(_) => super::stdio::resolved_display(&definition).await,
            Err(_) => None,
        };
        let mut state = self.state.lock().await;
        if state
            .definitions
            .iter()
            .find(|candidate| candidate.name == name)
            != Some(&definition)
        {
            return;
        }
        let fresh_epoch = self.fresh_epoch();
        let Some(server) = state
            .servers
            .get_mut(name)
            .filter(|server| server.epoch == epoch)
        else {
            return;
        };
        match result {
            Ok((client, ui_views)) => {
                server.client = Some(client);
                server.health = McpHealth::Healthy;
                server.diagnostic = None;
                server.resolved_command = resolved_command;
                server.ui_views = ui_views;
                server.oauth = None;
                server.reconnect = Reconnect::default();
            }
            Err(error) => {
                // As in `replace_strict`: the error chain is URL- and
                // secret-free, and the warn serves `tidebreak serve` until the
                // desktop installs a tracing subscriber.
                tracing::warn!(
                    server = %name,
                    "MCP server did not connect at startup: {error}"
                );
                let diagnostic = failure_diagnostic(&definition, &error, oauth.as_ref());
                server.client = None;
                server.health = McpHealth::Degraded;
                server.reconnect = Reconnect::after_failure(
                    failure_park(&definition, &error, oauth.as_ref()),
                    diagnostic.clone(),
                );
                server.diagnostic = Some(diagnostic);
                server.ui_views = HashMap::new();
                server.oauth = oauth;
            }
        }
        // A reconnect that waited on the lock with the old epoch returns this
        // result instead of starting another child.
        server.epoch = fresh_epoch;
        self.write_registry(&state, rosters);
    }

    /// The tool surface for work that starts now: a turn, or an external
    /// engine listing its tools.
    ///
    /// While saved servers are still making their first connection after
    /// boot, this waits for them, but never longer than [`BOOT_TOOLS_WAIT`].
    /// After that it answers with the servers that are up; the rest reach
    /// later snapshots as they connect. Once boot has settled, it answers at
    /// once, like [`snapshot`](Self::snapshot).
    pub async fn snapshot_after_boot(&self) -> Arc<ToolRegistry> {
        let mut settled = self.boot_settled.subscribe();
        let _ = tokio::time::timeout(BOOT_TOOLS_WAIT, settled.wait_for(|settled| *settled)).await;
        self.snapshot()
    }

    /// The saved records the loader skipped, with why, in storage order.
    pub async fn skipped_servers(&self) -> Vec<McpSkippedServer> {
        self.state
            .lock()
            .await
            .skipped
            .iter()
            .map(|skipped| McpSkippedServer {
                id: skipped.record.id,
                name: skipped.record.name.clone(),
                reason: skipped.reason.clone(),
            })
            .collect()
    }

    /// Delete one skipped record, with the environment values and the OAuth
    /// session stored under its id. Returns whether it was skipped.
    pub async fn remove_skipped(&self, id: ConnectedAppId) -> Result<bool> {
        let _mutation = self.mutation.lock().await;
        let (configured, ids, remaining) = {
            let state = self.state.lock().await;
            if !state.skipped.iter().any(|skipped| skipped.record.id == id) {
                return Ok(false);
            }
            (
                state
                    .definitions
                    .iter()
                    .filter(|definition| definition.plugin.is_none())
                    .cloned()
                    .collect::<Vec<_>>(),
                state.ids.clone(),
                state
                    .skipped
                    .iter()
                    .filter(|skipped| skipped.record.id != id)
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };
        self.persist_definitions(&configured, &ids, &remaining)
            .await?;
        // Best effort, like removing a server: nothing references the id now,
        // so a leftover entry is unreachable.
        let _ = self.secrets.delete_secret(&env_secret_key(id)).await;
        let _ = McpOAuthCredentialVault::new(self.secrets.clone(), id)
            .clear_all()
            .await;
        self.state
            .lock()
            .await
            .skipped
            .retain(|skipped| skipped.record.id != id);
        Ok(true)
    }

    /// Add one remote server to the saved configuration, connecting only it.
    ///
    /// A settings save connects every server again, so one server that is
    /// down fails it. An add leaves the configured servers alone. It saves the
    /// new server even when it cannot connect yet, so its row says what it
    /// needs: a sign-in, a token variable, or a network that answers. One
    /// failure saves nothing: an OAuth sign-in Tidebreak cannot complete,
    /// because that server could never connect.
    ///
    /// The server takes the definition's name, or the first free variant of
    /// it. When a configured server already has the definition's URL, nothing
    /// changes and that server's name comes back.
    pub async fn add_server(
        &self,
        mut definition: McpServerDefinition,
        lockdown: ManualLockdown,
    ) -> Result<McpAddOutcome> {
        let _mutation = self.mutation.lock().await;
        let (configured, plugin, mut ids, skipped) = {
            let state = self.state.lock().await;
            let (configured, plugin): (Vec<_>, Vec<_>) = state
                .definitions
                .iter()
                .cloned()
                .partition(|definition| definition.plugin.is_none());
            (configured, plugin, state.ids.clone(), state.skipped.clone())
        };
        if let Some(existing) = configured
            .iter()
            .find(|configured| same_endpoint(configured.url.as_deref(), definition.url.as_deref()))
        {
            let name = existing.name.clone();
            return Ok(McpAddOutcome::Added {
                name,
                info: self.info().await,
            });
        }
        if manual_lockdown_applies(&definition, lockdown) {
            return Ok(McpAddOutcome::RefusedManual);
        }
        let taken: HashSet<String> = configured
            .iter()
            .chain(&plugin)
            .map(|definition| definition.name.clone())
            .chain(skipped.iter().map(|skipped| skipped.record.name.clone()))
            .collect();
        definition.name = unused_name(&definition.name, &taken);
        let mut candidate = configured;
        candidate.push(definition.clone());
        validate_servers(&candidate)?;
        let id = ConnectedAppId::new();
        ids.insert(definition.name.clone(), id);
        let (result, oauth) = self
            .connect_server(&definition, &BTreeMap::new(), Some(id))
            .await;
        if let (Err(error), Some(need)) = (&result, &oauth) {
            if !need.saves() {
                return Err(AgentError::config(failure_diagnostic(
                    &definition,
                    error,
                    Some(need),
                )));
            }
        }
        self.persist_definitions(&candidate, &ids, &skipped).await?;
        let resolved_command = match &result {
            Ok(_) => super::stdio::resolved_display(&definition).await,
            Err(_) => None,
        };
        let managed = match result {
            Ok((client, ui_views)) => ManagedServer {
                client: Some(client),
                health: McpHealth::Healthy,
                diagnostic: None,
                resolved_command,
                reconnect: Reconnect::default(),
                epoch: self.fresh_epoch(),
                reconnect_lock: Arc::new(Mutex::new(())),
                ui_views,
                oauth: None,
            },
            Err(error) => {
                tracing::warn!(
                    server = %definition.name,
                    "added MCP server did not connect: {error}"
                );
                let diagnostic = failure_diagnostic(&definition, &error, oauth.as_ref());
                ManagedServer {
                    client: None,
                    health: McpHealth::Degraded,
                    diagnostic: Some(diagnostic.clone()),
                    resolved_command: None,
                    reconnect: Reconnect::after_failure(
                        failure_park(&definition, &error, oauth.as_ref()),
                        diagnostic,
                    ),
                    epoch: self.fresh_epoch(),
                    reconnect_lock: Arc::new(Mutex::new(())),
                    ui_views: HashMap::new(),
                    oauth,
                }
            }
        };
        let name = definition.name.clone();
        let rosters = self.rosters().await;
        {
            let mut state = self.state.lock().await;
            state.definitions = candidate.into_iter().chain(plugin).collect();
            state.ids = ids;
            state.servers.insert(name.clone(), managed);
            self.write_registry(&state, &rosters);
        }
        Ok(McpAddOutcome::Added {
            name,
            info: self.info().await,
        })
    }

    /// One prefetched MCP Apps view document, when the named server is
    /// connected and declared it.
    pub async fn ui_view_document(&self, server: &str, uri: &str) -> Option<UiViewDocument> {
        let state = self.state.lock().await;
        state.servers.get(server)?.ui_views.get(uri).cloned()
    }

    /// The current namespace and definition fingerprint of every configured
    /// connected app, by record id.
    ///
    /// Read live per call — never cached across a request — so grant
    /// enforcement always compares against the definition an id resolves to
    /// *now*, including a definition swapped in while the app stayed open.
    pub async fn app_fingerprints(&self) -> BTreeMap<ConnectedAppId, McpAppFingerprint> {
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
    pub fn snapshot(&self) -> Arc<ToolRegistry> {
        self.tools
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub async fn definitions(&self) -> Vec<McpServerDefinition> {
        self.state.lock().await.definitions.clone()
    }

    pub async fn info(&self) -> McpServersInfo {
        // Snapshot the projection under a short lock, recording every server
        // that can sign in with OAuth, then release the lock before touching
        // the credential store. `oauth_status_of` reads the OS keychain,
        // which must never run while the state lock is held, and re-locking
        // to call `oauth_status` from here would deadlock. The sign-in
        // progress is read under the same lock as what the last connection
        // learned: a finished sign-in updates both under it, so no read sees
        // one without the other.
        let (mut servers, oauth_servers) = {
            let state = self.state.lock().await;
            let sign_ins = self
                .sign_ins
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut servers = Vec::with_capacity(state.definitions.len());
            let mut oauth_servers = Vec::new();
            for (index, definition) in state.definitions.iter().enumerate() {
                let managed = state.servers.get(&definition.name);
                if let (Some(id), Some(url)) = (
                    state.ids.get(&definition.name),
                    oauth::sign_in_url(definition),
                ) {
                    oauth_servers.push(OAuthSnapshot {
                        index,
                        id: *id,
                        url: url.to_string(),
                        flag: definition.oauth,
                        need: managed.and_then(|server| server.oauth.clone()),
                        progress: sign_ins
                            .get(id)
                            .filter(|record| record.server_url == url)
                            .map(|record| record.progress.view()),
                    });
                }
                servers.push(McpServerInfo {
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
                    resolved_command: managed.and_then(|server| server.resolved_command.clone()),
                    curated: curation(definition),
                    oauth_status: None,
                    definition: definition.clone(),
                });
            }
            (servers, oauth_servers)
        };
        for snapshot in oauth_servers {
            servers[snapshot.index].oauth_status = self.oauth_status_of(&snapshot).await;
        }
        McpServersInfo { servers }
    }

    /// The OAuth status of one server that can sign in, read with no state
    /// lock held. See [`oauth::project_status`] for how the pieces combine.
    /// Only a session issued for this server's URL counts, and an unreadable
    /// one reads as none, so Connect is offered.
    async fn oauth_status_of(&self, snapshot: &OAuthSnapshot) -> Option<McpOAuthStatus> {
        let stored = McpOAuthCredentialVault::new(self.secrets.clone(), snapshot.id)
            .load_for(&snapshot.url)
            .await
            .ok()
            .flatten();
        oauth::project_status(
            snapshot.flag,
            snapshot.need.as_ref(),
            snapshot.progress.as_ref(),
            stored.as_ref(),
        )
    }

    /// Clear every stored OAuth session that no longer belongs to its server.
    ///
    /// A session is bound to the URL it was issued for. When a replacement
    /// removes a server, or keeps its name but changes its URL, or stops it
    /// signing in at all, the session goes: otherwise a later server of the
    /// same name, or the server at its new address, could inherit it. Best
    /// effort, like the environment cleanup beside it: a failed delete leaves
    /// a session that [`live_oauth_connection`](super::types) still refuses to
    /// present, because it checks the URL on every load.
    async fn reconcile_oauth_sessions(
        &self,
        definitions: &[McpServerDefinition],
        ids: &BTreeMap<String, ConnectedAppId>,
    ) {
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
            let _ = McpOAuthCredentialVault::new(self.secrets.clone(), id)
                .clear_all()
                .await;
        }
        for definition in definitions {
            let Some(id) = ids.get(&definition.name).copied() else {
                continue;
            };
            let vault = McpOAuthCredentialVault::new(self.secrets.clone(), id);
            let bound = match vault.load_registration().await {
                Ok(None) => continue,
                Ok(Some(registration)) => {
                    registration.server_url.is_some()
                        && registration.server_url.as_deref() == oauth::sign_in_url(definition)
                }
                Err(_) => false,
            };
            if !bound {
                if let Err(error) = vault.clear_all().await {
                    tracing::warn!(
                        server = %definition.name,
                        "could not clear an MCP OAuth session for a changed server: {error}"
                    );
                }
            }
        }
    }

    /// The bare mounted tool names of every connected server, by namespace —
    /// the name each tool carries after the `mcp__{server}__` mount prefix.
    ///
    /// Names only, never remote-authored descriptions or schemas: the same
    /// renderer-safety posture as the consent sheet. Bounded by the
    /// per-server discovery cap the client enforces at connect time.
    pub async fn tool_names(&self) -> BTreeMap<String, Vec<String>> {
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
    pub async fn replace(&self, config: McpServersConfig) -> Result<McpServersInfo> {
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
    pub async fn replace_under_policy(
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
    pub async fn auto_mount_gateway_endpoints(&self, entitled: &[String]) -> Result<bool> {
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
            let name = unused_name(slug, &taken);
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
                oauth: false,
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
        // A skipped record keeps its name on file, and a save writes it back
        // unchanged, so a new server cannot take the name until the record is
        // removed.
        let skipped = self.state.lock().await.skipped.clone();
        if persist {
            if let Some(taken) = definitions.iter().find(|definition| {
                skipped
                    .iter()
                    .any(|skipped| skipped.record.name == definition.name)
            }) {
                return Err(AgentError::config(format!(
                    "a saved MCP server named {:?} could not be loaded and still holds that \
                     name; remove it under Connected apps, then save again",
                    taken.name
                )));
            }
        }
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
        // Before anything connects, so no connection below can present a
        // session issued for a URL this replacement no longer names.
        self.reconcile_oauth_sessions(&definitions, &ids).await;
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
        let lockdown = self.manual_lockdown().await;
        let mut servers = HashMap::new();
        let connections = join_all(definitions.iter().map(|definition| {
            let env = envs.get(&definition.name).cloned().unwrap_or_default();
            let app_id = ids.get(&definition.name).copied();
            async move {
                if connects(definition, lockdown) {
                    let (result, oauth) = self.connect_server(definition, &env, app_id).await;
                    (result.map(Some), oauth)
                } else {
                    (Ok(None), None)
                }
            }
        }))
        .await;
        for (definition, (connection, oauth)) in definitions.iter().zip(connections) {
            let connection = match connection {
                Ok(connection) => connection,
                // A gateway mount depends on session state that changes out
                // of band (sign-out, revoked entitlement), so its failure
                // degrades the mount instead of rejecting the candidate —
                // otherwise a signed-out mount would block every unrelated
                // settings save until it was deleted. A plugin-sourced server
                // degrades for the same reason from the other direction: it is
                // not part of the candidate at all, so it must never be able
                // to fail somebody's settings save. A server that asks for an
                // OAuth sign-in degrades as well: only a saved server can be
                // signed in to, so refusing the save would leave no way to
                // connect it.
                Err(error)
                    if definition.gateway_endpoint.is_some()
                        || definition.plugin.is_some()
                        || oauth.as_ref().is_some_and(OAuthNeed::saves) =>
                {
                    // The projected diagnostic is classified and URL-/secret-
                    // free; keep the typed cause in the log. The desktop
                    // installs no tracing subscriber yet, so this surfaces
                    // under `tidebreak serve` until it does.
                    tracing::warn!(
                        server = %definition.name,
                        "MCP server degraded during replacement: {error}"
                    );
                    let diagnostic = failure_diagnostic(definition, &error, oauth.as_ref());
                    servers.insert(
                        definition.name.clone(),
                        ManagedServer {
                            client: None,
                            health: McpHealth::Degraded,
                            diagnostic: Some(diagnostic.clone()),
                            resolved_command: None,
                            reconnect: Reconnect::after_failure(
                                failure_park(definition, &error, oauth.as_ref()),
                                diagnostic,
                            ),
                            epoch: self.fresh_epoch(),
                            reconnect_lock: Arc::new(Mutex::new(())),
                            ui_views: HashMap::new(),
                            oauth,
                        },
                    );
                    continue;
                }
                Err(error) => {
                    return Err(AgentError::config(format!(
                        "external MCP server {} failed to start: {}",
                        definition.name,
                        failure_diagnostic(definition, &error, oauth.as_ref())
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
                        resolved_command: None,
                        reconnect: Reconnect::default(),
                        epoch: self.fresh_epoch(),
                        reconnect_lock: Arc::new(Mutex::new(())),
                        ui_views: HashMap::new(),
                        oauth: None,
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
                    resolved_command: super::stdio::resolved_display(definition).await,
                    reconnect: Reconnect::default(),
                    epoch: self.fresh_epoch(),
                    reconnect_lock: Arc::new(Mutex::new(())),
                    ui_views,
                    oauth: None,
                },
            );
        }
        if persist {
            // Before the new state publishes, while the published set is
            // still what this replacement diffs against. Plugin-sourced
            // definitions are excluded: they are derived, so persisting them
            // would create a second, staler home for the same facts.
            self.remember_endpoint_unmounts(&configured).await;
            self.persist_definitions(&configured, &ids, &skipped)
                .await?;
        }
        self.publish(definitions, ids, servers).await;
        Ok(())
    }

    /// Write the definitions as this profile's complete `mcp_server`
    /// connected-app set. `env_values` is `skip_serializing`, so the record
    /// carries environment *names* and nothing more; the values are already in
    /// the secret store by the time this runs.
    ///
    /// `skipped` records are written back exactly as stored, after the
    /// definitions, so no save deletes a record this build could not load.
    /// One the store's own record checks now refuse cannot be carried
    /// forward, and goes.
    async fn persist_definitions(
        &self,
        definitions: &[McpServerDefinition],
        ids: &BTreeMap<String, ConnectedAppId>,
        skipped: &[SkippedRecord],
    ) -> Result<()> {
        let now = chrono::Utc::now();
        let mut records: Vec<ConnectedApp> = definitions
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
        for skipped in skipped {
            match validate_connected_app(&skipped.record) {
                Ok(()) => records.push(skipped.record.clone()),
                Err(problem) => tracing::warn!(
                    server = %skipped.record.name,
                    "dropping a saved MCP server record the store no longer accepts: {problem}"
                ),
            }
        }
        self.store
            .replace_connected_apps(ConnectedAppKind::McpServer, &records)
            .await
    }

    /// Publish `definitions` the way boot does and wait for every connection,
    /// so a test sees the settled state.
    #[cfg(test)]
    pub(super) async fn replace_permissive(
        self: &Arc<Self>,
        definitions: Vec<McpServerDefinition>,
        ids: BTreeMap<String, ConnectedAppId>,
    ) {
        self.publish_starting(definitions, ids, Vec::new())
            .await
            .connect()
            .await;
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
    pub async fn reconcile_plugin_servers(&self) -> bool {
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
        // Only the entries that are new or changed are connected; the rest
        // keep the client they already hold.
        let fresh: Vec<&McpServerDefinition> = desired
            .iter()
            .filter(|definition| !live.contains(definition))
            .collect();
        let connections = join_all(fresh.iter().map(|definition| {
            let app_id = ids.get(&definition.name).copied();
            async move {
                if connects(definition, lockdown) {
                    // Plugin servers never sign in, so the OAuth half of the
                    // attempt is always empty here.
                    let (result, _) = self
                        .connect_server(definition, &BTreeMap::new(), app_id)
                        .await;
                    result.map(Some)
                } else {
                    Ok(None)
                }
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
                    resolved_command: super::stdio::resolved_display(definition).await,
                    reconnect: Reconnect::default(),
                    epoch: self.fresh_epoch(),
                    reconnect_lock: Arc::new(Mutex::new(())),
                    ui_views,
                    oauth: None,
                },
                Ok(None) => ManagedServer {
                    client: None,
                    health: McpHealth::Disabled,
                    diagnostic: disabled_diagnostic(definition, lockdown),
                    resolved_command: None,
                    reconnect: Reconnect::default(),
                    epoch: self.fresh_epoch(),
                    reconnect_lock: Arc::new(Mutex::new(())),
                    ui_views: HashMap::new(),
                    oauth: None,
                },
                Err(error) => {
                    tracing::warn!(
                        server = %definition.name,
                        plugin = ?definition.plugin,
                        "plugin MCP server connection failed: {error}"
                    );
                    let diagnostic = failure_diagnostic(definition, &error, None);
                    ManagedServer {
                        client: None,
                        health: McpHealth::Degraded,
                        diagnostic: Some(diagnostic.clone()),
                        resolved_command: None,
                        reconnect: Reconnect::after_failure(
                            failure_park(definition, &error, None),
                            diagnostic,
                        ),
                        epoch: self.fresh_epoch(),
                        reconnect_lock: Arc::new(Mutex::new(())),
                        ui_views: HashMap::new(),
                        oauth: None,
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
        let gateway = self.gateway.entitled_app_catalogs().await;
        let registry = self.registry_with(&servers, &rest, &folders, &gateway);
        self.forget_stale_sign_ins(&definitions, &ids);
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
    pub async fn refresh_connected_app_roster(&self) {
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
        let gateway = self.gateway.entitled_app_catalogs().await;
        self.registry_with(&state.servers, &rest, &folders, &gateway)
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

    /// The rosters this machine answers for itself: the stored REST apps and
    /// the approved folders. The gateway's stays empty; see
    /// [`rosters`](Self::rosters).
    async fn local_rosters(&self) -> Rosters {
        Rosters {
            rest: self.rest_roster().await,
            folders: self.folder_roster().await,
            gateway: Vec::new(),
        }
    }

    /// Every roster, the gateway's included. That one is a network read while
    /// a gateway session exists, so callers read it before they take the
    /// state lock.
    async fn rosters(&self) -> Rosters {
        let mut rosters = self.local_rosters().await;
        rosters.gateway = self.gateway.entitled_app_catalogs().await;
        rosters
    }

    /// Build the registry from `state` and publish it. Callers hold the state
    /// lock, so the registry never runs ahead of the state it describes.
    fn write_registry(&self, state: &RuntimeState, rosters: &Rosters) {
        let registry = self.registry_with(
            &state.servers,
            &rosters.rest,
            &rosters.folders,
            &rosters.gateway,
        );
        *self
            .tools
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(registry);
    }

    /// Rebuild the registry with fresh rosters. They are read before the
    /// state lock, so a slow gateway never holds up a read of the server list.
    async fn republish(&self) {
        let rosters = self.rosters().await;
        let state = self.state.lock().await;
        self.write_registry(&state, &rosters);
    }

    fn registry_with(
        &self,
        servers: &HashMap<String, ManagedServer>,
        rest: &[RestRosterApp],
        folders: &[crate::host_folders::ApprovedFolder],
        gateway: &[GatewayRosterApp],
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
                roster: connected_app_roster(rest, folders, gateway),
            }));
        }
        registry
    }

    /// Force a fresh connection and tool discovery for one configured server.
    /// The answer carries every server's OAuth status, as [`info`](Self::info)
    /// does, so a Connect action does not vanish after a reconnect.
    pub async fn reconnect(&self, name: &str) -> Result<McpServersInfo> {
        self.reconnect_if_epoch(name, None).await?;
        Ok(self.info().await)
    }

    /// Begin the OAuth sign-in for one configured server.
    ///
    /// Discovers the server's authorization server, registers a client for a
    /// fresh loopback redirect, and returns `Authorizing` with the page the
    /// desktop opens in the person's browser. The server never opens a
    /// browser itself: it may run on another machine. The rest happens in the
    /// background. Once the browser returns, the tokens are stored, bound to
    /// this server's URL, and the server reconnects; [`info`](Self::info)
    /// reports `Connected`, or why the sign-in stopped. A second Connect
    /// replaces a sign-in still waiting.
    pub async fn oauth_connect(self: &Arc<Self>, name: &str) -> Result<McpOAuthStatus> {
        let (definition, id, need) = {
            let state = self.state.lock().await;
            let definition = state
                .definitions
                .iter()
                .find(|definition| definition.name == name)
                .cloned()
                .ok_or_else(|| AgentError::config("MCP server not found"))?;
            let id = state
                .ids
                .get(name)
                .copied()
                .ok_or_else(|| AgentError::config("MCP server record is missing"))?;
            let need = state
                .servers
                .get(name)
                .and_then(|server| server.oauth.clone());
            (definition, id, need)
        };
        let Some(url) = oauth::sign_in_url(&definition).map(str::to_string) else {
            return Ok(McpOAuthStatus::failed(
                McpOAuthState::Unsupported,
                oauth::NO_OAUTH,
            ));
        };
        // Managed lockdown gates every other connect and reconnect path, and an
        // OAuth server is a remote (`url`, no `command`) definition — exactly
        // what `RemoteManual` locks. Refuse the sign-in before it opens a
        // browser or stores credentials, so authorization cannot smuggle a
        // remote mount past the egress restriction lockdown enforces.
        let lockdown = self.manual_lockdown().await;
        if manual_lockdown_applies(&definition, lockdown) {
            return Ok(McpOAuthStatus::failed(
                McpOAuthState::Unsupported,
                "Managed policy has locked this MCP server.",
            ));
        }
        let resource = url::Url::parse(&url)
            .map_err(|_| AgentError::config("this MCP server has no HTTP endpoint"))?;

        let client = self.oauth_client()?;
        let challenge = tidebreak_mcp::authorization_challenge(&url, oauth::CHALLENGE_TIMEOUT)
            .await
            .ok()
            .flatten();
        let discovered = match client.discover(&resource, challenge.as_deref()).await {
            Some(Discovery::Supported(discovered)) => *discovered,
            // These answers are kept, so the row goes on saying why after the
            // next read, for a server known to sign in. A server that never
            // asked for OAuth only gets the answer.
            Some(Discovery::Unsupported(reason)) => {
                return Ok(self.record_sign_in_stop(
                    id,
                    &url,
                    None,
                    McpOAuthState::Unsupported,
                    reason.reason(),
                ));
            }
            Some(Discovery::Unavailable) => {
                return Ok(self.record_sign_in_stop(
                    id,
                    &url,
                    None,
                    McpOAuthState::NotConnected,
                    oauth::METADATA_UNREADABLE,
                ));
            }
            None if need.is_some() || definition.oauth => {
                return Ok(self.record_sign_in_stop(
                    id,
                    &url,
                    None,
                    McpOAuthState::NotConnected,
                    oauth::METADATA_UNREADABLE,
                ));
            }
            None => {
                return Ok(McpOAuthStatus::failed(
                    McpOAuthState::Unsupported,
                    oauth::NO_OAUTH,
                ));
            }
        };

        let (listeners, redirect_uri) = bind_mcp_loopback().await?;
        // Register a fresh public client for this sign-in rather than reusing a
        // stored one. A loopback redirect binds an ephemeral port (RFC 8252
        // §7.3), so a client registered for an earlier sign-in carries a
        // different port than the one just bound. Reusing it would send an
        // authorize request whose `redirect_uri` the authorization server never
        // registered — it may reject the request or fall back to a different
        // registered redirect, and neither PKCE nor state makes an unregistered
        // redirect safe. Registering per sign-in keeps the redirect and the
        // client the server has on file in lockstep. Refresh needs no redirect
        // and still reuses the stored registration, which this sign-in
        // replaces only once it succeeds.
        let mut registration = match client
            .register(&discovered.registration_endpoint, &redirect_uri)
            .await
        {
            Ok(registration) => registration,
            Err(failure) => return Ok(self.record_sign_in_failure(id, &url, None, failure)),
        };
        registration.token_endpoint = Some(discovered.token_endpoint.as_str().to_string());
        registration.scopes = discovered.scopes.clone();
        registration.resource = Some(discovered.resource.clone());
        registration.server_url = Some(url.clone());
        registration.sign_in_host = Some(discovered.sign_in_host.clone());

        let pkce = pkce_pair();
        let state = format!("{}-{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let authorization_url = build_authorize_url(
            &discovered.authorization_endpoint,
            &registration.client_id,
            &redirect_uri,
            &pkce.challenge,
            &state,
            &registration.scopes,
            registration.resource.as_deref(),
        );
        let pending = PendingMcpSignIn {
            authorization_url: authorization_url.clone(),
            redirect_uri,
            listeners,
            verifier: pkce.verifier,
            state,
            token_endpoint: discovered.token_endpoint,
            registration,
        };
        let generation = self.next_sign_in.fetch_add(1, Ordering::Relaxed);
        {
            // The task is spawned under the lock, so it cannot record its
            // outcome before this sign-in is on file.
            let mut sign_ins = self
                .sign_ins
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let runtime = Arc::clone(self);
            let sign_in = FinishingSignIn {
                id,
                name: name.to_string(),
                server_url: url.clone(),
                generation,
            };
            let task = tokio::spawn(async move {
                runtime.finish_oauth_sign_in(sign_in, client, pending).await;
            });
            let replaced = sign_ins.insert(
                id,
                SignInRecord {
                    server_url: url,
                    progress: SignInProgress::Pending {
                        authorization_url: authorization_url.to_string(),
                        sign_in_host: discovered.sign_in_host.clone(),
                        generation,
                        task: task.abort_handle(),
                    },
                },
            );
            if let Some(SignInRecord {
                progress: SignInProgress::Pending { task, .. },
                ..
            }) = replaced
            {
                task.abort();
            }
        }
        Ok(McpOAuthStatus::authorizing(authorization_url.to_string())
            .with_sign_in_host(Some(discovered.sign_in_host)))
    }

    /// Wait for the browser to come back, then store the session and
    /// reconnect the server, or record why the sign-in stopped.
    ///
    /// The session is kept only for the server the person signed in to (same
    /// record, same URL) and only while this sign-in is still the one on file,
    /// not canceled or replaced. The pending sign-in stays on file while the
    /// session is stored; then clearing the server's sign-in-needed state and
    /// retiring the pending sign-in happen together under the state lock. So
    /// a read of [`info`](Self::info) sees either the wait or the new
    /// session, never the new session beside the old "sign in required",
    /// which would read as a rejected sign-in.
    async fn finish_oauth_sign_in(
        self: Arc<Self>,
        sign_in: FinishingSignIn,
        client: McpOAuthClient,
        pending: PendingMcpSignIn,
    ) {
        let (registration, credentials) = match pending.finish(&client).await {
            Ok(session) => session,
            Err(failure) => {
                self.record_sign_in_failure(
                    sign_in.id,
                    &sign_in.server_url,
                    Some(sign_in.generation),
                    failure,
                );
                return;
            }
        };
        if !self.sign_in_is_current(&sign_in) {
            return;
        }
        if definition_signing_in(&*self.state.lock().await, &sign_in).is_none() {
            self.record_sign_in_stop(
                sign_in.id,
                &sign_in.server_url,
                Some(sign_in.generation),
                McpOAuthState::NotConnected,
                oauth::CHANGED_DURING_SIGN_IN,
            );
            return;
        }
        let vault = McpOAuthCredentialVault::new(self.secrets.clone(), sign_in.id);
        let stored = match vault.save_registration(&registration).await {
            Ok(()) => vault.save(&credentials).await,
            Err(error) => Err(error),
        };
        if let Err(error) = stored {
            tracing::warn!(server = %sign_in.name, "could not store the MCP OAuth session: {error}");
            let _ = vault.clear_all().await;
            self.record_sign_in_failure(
                sign_in.id,
                &sign_in.server_url,
                Some(sign_in.generation),
                SignInFailure::Failed,
            );
            return;
        }
        let lockdown = self.manual_lockdown().await;
        let mut state = self.state.lock().await;
        let Some(definition) = definition_signing_in(&state, &sign_in) else {
            // The server changed while the session was being stored. The
            // session is for a URL this record no longer has, so it goes.
            drop(state);
            let _ = vault.clear_all().await;
            self.record_sign_in_stop(
                sign_in.id,
                &sign_in.server_url,
                Some(sign_in.generation),
                McpOAuthState::NotConnected,
                oauth::CHANGED_DURING_SIGN_IN,
            );
            return;
        };
        {
            let mut sign_ins = self
                .sign_ins
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if matches!(
                sign_ins.get(&sign_in.id),
                Some(SignInRecord {
                    progress: SignInProgress::Pending { generation, .. },
                    ..
                }) if *generation == sign_in.generation
            ) {
                sign_ins.remove(&sign_in.id);
            }
        }
        if let Some(server) = state.servers.get_mut(&sign_in.name) {
            server.oauth = None;
            if connects(&definition, lockdown) {
                // The reconnect below takes it from here; until it lands the
                // row reads as signed in and connecting, not as failed.
                server.health = McpHealth::Reconnecting;
                server.diagnostic = None;
            }
        }
        drop(state);
        // The reconnect runs as its own task: a newer Connect aborts this one,
        // and an abort mid-reconnect would leave the server showing
        // `reconnecting`.
        let name = sign_in.name;
        tokio::spawn(async move {
            let _ = self.reconnect(&name).await;
        });
    }

    /// Whether `sign_in` is still the pending sign-in on file for its server:
    /// not canceled, disconnected, replaced, or dropped with its server.
    fn sign_in_is_current(&self, sign_in: &FinishingSignIn) -> bool {
        matches!(
            self.sign_ins
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&sign_in.id),
            Some(SignInRecord {
                server_url,
                progress: SignInProgress::Pending { generation, .. },
            }) if *generation == sign_in.generation && *server_url == sign_in.server_url
        )
    }

    /// Record why a sign-in stopped. See [`Self::record_sign_in_stop`].
    fn record_sign_in_failure(
        &self,
        id: ConnectedAppId,
        server_url: &str,
        generation: Option<u64>,
        failure: SignInFailure,
    ) -> McpOAuthStatus {
        let state = if failure == SignInFailure::Denied {
            McpOAuthState::AccessDenied
        } else {
            McpOAuthState::NotConnected
        };
        self.record_sign_in_stop(id, server_url, generation, state, failure.message())
    }

    /// Record that a sign-in stopped in `state`, unless it is a `generation`
    /// a newer sign-in, a Cancel, or a Disconnect already replaced, and
    /// return the status it leaves. `None` records a stop from before any
    /// sign-in was on file, which never replaces one that is waiting.
    fn record_sign_in_stop(
        &self,
        id: ConnectedAppId,
        server_url: &str,
        generation: Option<u64>,
        state: McpOAuthState,
        message: &str,
    ) -> McpOAuthStatus {
        let mut sign_ins = self
            .sign_ins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = match (generation, sign_ins.get(&id).map(|record| &record.progress)) {
            (
                Some(generation),
                Some(SignInProgress::Pending {
                    generation: current,
                    ..
                }),
            ) => *current == generation,
            (Some(_), _) => false,
            (None, Some(SignInProgress::Pending { .. })) => false,
            (None, _) => true,
        };
        if current {
            sign_ins.insert(
                id,
                SignInRecord {
                    server_url: server_url.to_string(),
                    progress: SignInProgress::Failed {
                        state,
                        message: message.to_string(),
                    },
                },
            );
        }
        McpOAuthStatus::failed(state, message)
    }

    /// Drop the sign-ins that no longer match a configured server that signs
    /// in at the same URL — the server was removed, its URL changed, or it
    /// stopped signing in — stopping any that still wait on the browser.
    fn forget_stale_sign_ins(
        &self,
        definitions: &[McpServerDefinition],
        ids: &BTreeMap<String, ConnectedAppId>,
    ) {
        let urls: HashMap<ConnectedAppId, &str> = definitions
            .iter()
            .filter_map(|definition| {
                Some((*ids.get(&definition.name)?, oauth::sign_in_url(definition)?))
            })
            .collect();
        let mut sign_ins = self
            .sign_ins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sign_ins.retain(|id, record| {
            let keep = urls.get(id) == Some(&record.server_url.as_str());
            if !keep {
                if let SignInProgress::Pending { task, .. } = &record.progress {
                    task.abort();
                }
            }
            keep
        });
    }

    /// The configured server `name` that signs in, with its record id and URL.
    async fn signing_in_server(&self, name: &str) -> Result<Option<(ConnectedAppId, String)>> {
        let state = self.state.lock().await;
        let definition = state
            .definitions
            .iter()
            .find(|definition| definition.name == name)
            .ok_or_else(|| AgentError::config("MCP server not found"))?;
        let id = state
            .ids
            .get(name)
            .copied()
            .ok_or_else(|| AgentError::config("MCP server record is missing"))?;
        Ok(oauth::sign_in_url(definition).map(|url| (id, url.to_string())))
    }

    /// Stop a sign-in that still waits on the browser, leaving any stored
    /// session alone, and return the status the server has without it.
    pub async fn oauth_cancel(&self, name: &str) -> Result<McpOAuthStatus> {
        let Some((id, _url)) = self.signing_in_server(name).await? else {
            return Ok(McpOAuthStatus::failed(
                McpOAuthState::Unsupported,
                oauth::NO_OAUTH,
            ));
        };
        {
            let mut sign_ins = self
                .sign_ins
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(SignInRecord {
                progress: SignInProgress::Pending { task, .. },
                ..
            }) = sign_ins.get(&id)
            {
                task.abort();
                sign_ins.remove(&id);
            }
        }
        self.oauth_status(name).await
    }

    /// Clear a server's stored OAuth session and drop its live connection.
    /// Stops a sign-in that still waits on the browser.
    pub async fn oauth_disconnect(&self, name: &str) -> Result<McpOAuthStatus> {
        let Some((id, _url)) = self.signing_in_server(name).await? else {
            return Ok(McpOAuthStatus::failed(
                McpOAuthState::Unsupported,
                oauth::NO_OAUTH,
            ));
        };
        if let Some(SignInRecord {
            progress: SignInProgress::Pending { task, .. },
            ..
        }) = self
            .sign_ins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id)
        {
            task.abort();
        }
        McpOAuthCredentialVault::new(self.secrets.clone(), id)
            .clear_all()
            .await?;
        let _ = self.reconnect(name).await;
        Ok(McpOAuthStatus::not_connected())
    }

    /// Report one server's current OAuth connection state without mutating
    /// it: the same status [`info`](Self::info) projects, and `Unsupported`
    /// for a server with no OAuth sign-in.
    pub async fn oauth_status(&self, name: &str) -> Result<McpOAuthStatus> {
        let snapshot = {
            let state = self.state.lock().await;
            let definition = state
                .definitions
                .iter()
                .find(|definition| definition.name == name)
                .ok_or_else(|| AgentError::config("MCP server not found"))?;
            match (state.ids.get(name), oauth::sign_in_url(definition)) {
                (Some(id), Some(url)) => Some(OAuthSnapshot {
                    index: 0,
                    id: *id,
                    url: url.to_string(),
                    flag: definition.oauth,
                    need: state
                        .servers
                        .get(name)
                        .and_then(|server| server.oauth.clone()),
                    progress: self
                        .sign_ins
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .get(id)
                        .filter(|record| record.server_url == url)
                        .map(|record| record.progress.view()),
                }),
                _ => None,
            }
        };
        let unsupported = || McpOAuthStatus::failed(McpOAuthState::Unsupported, oauth::NO_OAUTH);
        let Some(snapshot) = snapshot else {
            return Ok(unsupported());
        };
        Ok(self
            .oauth_status_of(&snapshot)
            .await
            .unwrap_or_else(unsupported))
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
        let (result, oauth) = self.connect_server(&definition, &env, app_id).await;
        match result {
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
                        resolved_command: None,
                        reconnect: Reconnect::default(),
                        epoch: self.fresh_epoch(),
                        reconnect_lock: Arc::new(Mutex::new(())),
                        ui_views: HashMap::new(),
                        oauth: None,
                    });
                server.client = Some(client);
                server.health = McpHealth::Healthy;
                server.diagnostic = None;
                server.resolved_command = super::stdio::resolved_display(&definition).await;
                server.ui_views = ui_views;
                server.oauth = None;
                if server.reconnect.reported.is_some() {
                    tracing::info!(server = %name, "MCP server reconnected");
                }
                server.reconnect = Reconnect::default();
                server.epoch = self.fresh_epoch();
                let registry = self.registry_for(&state).await;
                *self
                    .tools
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(registry);
                Ok(self.info_locked(&state))
            }
            Err(error) => {
                let diagnostic = failure_diagnostic(&definition, &error, oauth.as_ref());
                let park = failure_park(&definition, &error, oauth.as_ref());
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
                    server.oauth = oauth;
                    // As in `replace_strict`, the error chain is URL- and
                    // secret-free. A failure that repeats the last one logged
                    // stays at debug, so a server that stays down for hours
                    // writes one warning, not one per attempt.
                    if server.reconnect.failed(park, &diagnostic) {
                        report_reconnect_failure(name, park, &error);
                    } else {
                        tracing::debug!(server = %name, "MCP server reconnect failed again: {error}");
                    }
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
    pub async fn supervise(self: Arc<Self>) {
        loop {
            tokio::time::sleep(HEALTH_INTERVAL).await;
            let lockdown = self.manual_lockdown().await;
            // Policy may have flipped since the last sweep. Enforce the effect
            // before probing: a server that is now locked must be taken down,
            // not merely left out of the probe set.
            if lockdown != ManualLockdown::Open {
                self.take_down_locked_manual_servers(lockdown).await;
            }
            let probes = self.supervised_servers(lockdown).await;
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

    /// The servers one supervisor sweep probes or reconnects, as (name,
    /// client, backoff, epoch).
    ///
    /// A parked server is left out: retrying it cannot succeed until a
    /// sign-in, a settings change, or a manual reconnect, and each of those
    /// brings it back on its own.
    pub(super) async fn supervised_servers(
        &self,
        lockdown: ManualLockdown,
    ) -> Vec<(String, Option<McpClient>, Duration, u64)> {
        let state = self.state.lock().await;
        state
            .definitions
            .iter()
            .filter(|definition| connects(definition, lockdown))
            .filter_map(|definition| {
                state
                    .servers
                    .get(&definition.name)
                    .filter(|server| server.reconnect.parked.is_none())
                    .map(|server| {
                        (
                            definition.name.clone(),
                            server.client.clone(),
                            server.reconnect.backoff,
                            server.epoch,
                        )
                    })
            })
            .collect()
    }

    /// A sign-in stored a new model-gateway session. Servers parked for want
    /// of one go back to the supervisor, which retries them on its next sweep.
    pub async fn gateway_session_changed(&self) {
        let mut state = self.state.lock().await;
        for server in state.servers.values_mut() {
            server.reconnect.resume_after_sign_in();
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
                Some("Health check failed. Tidebreak will retry this server.".to_string());
            server.ui_views = HashMap::new();
            server.reconnect.backoff = backoff;
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
                        resolved_command: managed
                            .and_then(|server| server.resolved_command.clone()),
                        curated: curation(definition),
                        // This projection is synchronous and holds the state
                        // lock, so it cannot read the credential store. Callers
                        // that need the OAuth status use the async `info`; the
                        // reconnect paths that build this only render health.
                        oauth_status: None,
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

/// The configured definition a finishing sign-in belongs to: the same name,
/// still under the same record id, still signing in at the same URL. `None`
/// once the server was removed, renamed, or pointed somewhere else.
fn definition_signing_in(
    state: &RuntimeState,
    sign_in: &FinishingSignIn,
) -> Option<McpServerDefinition> {
    state
        .definitions
        .iter()
        .find(|definition| definition.name == sign_in.name)
        .filter(|definition| {
            state.ids.get(&definition.name) == Some(&sign_in.id)
                && oauth::sign_in_url(definition) == Some(sign_in.server_url.as_str())
        })
        .cloned()
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

/// A valid, unused namespace derived from `base`: `base` itself, truncated to
/// the name limit, or its first free `_2`, `_3`, … variant. An auto-mounted
/// gateway endpoint gets the same name the desktop's mount toggle would give
/// it (`mountName` in `McpPanel.tsx`), whichever side creates it, and a
/// directory server gets its directory id or a variant of it. Both inputs are
/// ASCII by contract, so byte slicing is safe.
fn unused_name(base: &str, taken: &HashSet<String>) -> String {
    let base = &base[..base.len().min(MAX_SERVER_NAME_BYTES)];
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

/// Whether two server URLs name the same endpoint: the same scheme, host,
/// port, and query, and the same path apart from a trailing slash.
fn same_endpoint(left: Option<&str>, right: Option<&str>) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    let (Ok(left), Ok(right)) = (url::Url::parse(left), url::Url::parse(right)) else {
        return left == right;
    };
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
        && left.path().trim_end_matches('/') == right.path().trim_end_matches('/')
        && left.query() == right.query()
}

/// Type and check one saved record, or say why it cannot load.
///
/// Records written before literal values moved into the secret store carry
/// them in cleartext; they are lifted out before the definition is typed, so
/// no value ever enters the type again, and come back in `env_values` for the
/// loader to move. The record's name is authoritative for the namespace: the
/// stored definition mirrors it and is repaired if they ever disagree.
fn decode_record(record: &ConnectedApp) -> std::result::Result<McpServerDefinition, String> {
    let mut stored = record.definition.clone();
    let legacy = take_legacy_env_values(&mut stored);
    let mut definition: McpServerDefinition =
        serde_json::from_value(stored).map_err(|error| decode_reason(&error))?;
    definition.env_values = legacy;
    definition.name = record.name.clone();
    validate_server(&definition).map_err(|error| validation_reason(&definition.name, &error))?;
    Ok(definition)
}

/// Why a saved definition did not decode, as a sentence.
///
/// A serde message can quote a string it refused, and a definition's strings
/// include arguments and URLs, so only a field name is ever carried over.
fn decode_reason(error: &serde_json::Error) -> String {
    let message = error.to_string();
    if let Some(field) = quoted_field(&message, "unknown field `") {
        return format!(
            "It has a setting this version of Tidebreak does not know, \"{field}\". A newer \
             version of Tidebreak may have saved it."
        );
    }
    if let Some(field) = quoted_field(&message, "missing field `") {
        return format!("It is missing the \"{field}\" setting.");
    }
    "Its saved settings are not in a form this version of Tidebreak can read.".to_string()
}

/// The field name serde quotes after `prefix` in `message`, when it is a
/// plain identifier.
fn quoted_field<'a>(message: &'a str, prefix: &str) -> Option<&'a str> {
    let start = message.find(prefix)? + prefix.len();
    let length = message[start..].find('`')?;
    let field = &message[start..start + length];
    (!field.is_empty()
        && field.len() <= 64
        && field
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
    .then_some(field)
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
