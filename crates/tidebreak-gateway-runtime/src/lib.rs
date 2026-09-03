//! Model-gateway connection, session, catalog, and relay runtime.
//!
//! The embedding server supplies managed policy, model persistence, pairing,
//! and MCP configuration through narrow traits. The runtime owns the network
//! session, authority fence, sign-in lifecycle, catalog refresh, endpoint
//! entitlements, and shared-app relay.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use tidebreak_core::id::{AppId, SessionId};
use tidebreak_core::{AgentError, OwnerId, ReasoningEffort, Result, SecretProvider};
use tidebreak_router::{BearerTokenSource, ModelRouteLease};
use tokio::sync::{Mutex, OwnedRwLockWriteGuard, RwLock};

#[doc(hidden)]
pub mod callback_page;
mod connection;

pub use connection::{
    has_stored_credentials, has_stored_credentials_for, is_sign_in_required,
    stored_installation_id_for, validate_mcp_endpoint_slug, AuthorizedSession, CredentialVault,
    GatewayApp, GatewayAuth, GatewayAuthConfig, GatewayCatalog, GatewayCatalogApp,
    GatewayCatalogFetch, GatewayCatalogModel, GatewayConnection, GatewayConsentOutcome,
    GatewayCredentials, GatewayIdentity, GatewayInvokeOutcome, GatewayMeta, GatewayModel,
    GatewayOperationSummary, GatewayRegistrationOutcome, GatewaySurfaces, PendingSignIn, TokenSet,
    MEMBER_CATALOG_V1, RESOURCE_CONTROL, RESOURCE_LLM, RESOURCE_TIDEBREAK,
    SECRET_KEY as GATEWAY_SECRET_KEY,
};

mod apps;
mod models;
mod relay;
mod session;

pub use relay::{gateway_relay_dispatcher, relay_with_consent_self_heal, shared_app_invoke_body};
pub use session::retire_superseded_gateway_session;

use session::*;

const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);
const MODEL_SYNC_INTERVAL: Duration = Duration::from_secs(5 * 60);
const MODEL_SYNC_RETRY: Duration = Duration::from_secs(60);

/// Serializes every gateway snapshot recheck-and-write sequence in one process.
pub static GATEWAY_STATE_WRITES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Serializes pairing policy checks and writes with sign-in completion.
/// Gateway authority mutations acquire this after the model-authority fence
/// and before sign-in state.
pub static GATEWAY_PAIRING_WRITES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The managed-policy fields that govern gateway execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayPolicy {
    pub managed: bool,
    pub gateway_url: Option<String>,
}

/// Resolves the active gateway policy on every use.
pub trait GatewayPolicySource: Send + Sync {
    fn resolve(&self) -> Result<GatewayPolicy>;
}

/// One model persisted in the gateway entitlement snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncedGatewayModel {
    pub id: String,
    pub display_name: Option<String>,
    pub upstream_id: Option<String>,
    pub aliases: Vec<String>,
    pub context_window: u32,
    pub max_output_tokens: u32,
}

/// The compatibility protocol used for one gateway model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GatewayModelProtocol {
    #[default]
    AnthropicMessages,
    OpenaiResponses,
}

impl GatewayModelProtocol {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "anthropic" | "anthropic_messages" => Some(Self::AnthropicMessages),
            "openai" | "openai_compatible" | "openai_chat_completions" | "openai_responses" => {
                Some(Self::OpenaiResponses)
            }
            _ => None,
        }
    }
}

/// The stored gateway model snapshot used by runtime policy checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayModelSnapshot {
    pub gateway_url: String,
    pub installation_id: Option<String>,
    pub models: Vec<SyncedGatewayModel>,
    pub model_protocols: BTreeMap<String, GatewayModelProtocol>,
    pub model_reasoning_efforts: BTreeMap<String, Vec<ReasoningEffort>>,
    pub member_catalog: Option<String>,
    pub catalog_etag: Option<String>,
}

/// The route fields the router needs after the host validates a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRoute {
    pub id: String,
    pub route_model: String,
    pub request_shaping_model: String,
}

/// Persists and resolves gateway model snapshots without coupling to server models.
#[async_trait]
pub trait GatewayModelState: Send + Sync {
    async fn snapshot(&self, policy: &GatewayPolicy) -> Result<Option<GatewayModelSnapshot>>;

    async fn resolve_route(
        &self,
        snapshot: &GatewayModelSnapshot,
        route_model: &str,
    ) -> Result<Option<GatewayRoute>>;

    async fn write_snapshot(&self, snapshot: &GatewayModelSnapshot) -> Result<()>;
}

/// Counts local-app grants that use each gateway app.
#[async_trait]
pub trait GatewayAppUsageSource: Send + Sync {
    async fn used_by_app_counts(&self, owner: &OwnerId) -> Result<BTreeMap<String, usize>>;
}

/// One resolved gateway MCP endpoint.
pub struct GatewayEndpointAccess {
    pub url: String,
    pub bearer_token: String,
}

/// One gateway app exposed to the app-authoring roster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRosterApp {
    pub id: String,
    pub name: String,
    pub operation_ids: Vec<String>,
}

/// Resolves gateway endpoints and app catalogs for the MCP runtime.
#[async_trait]
pub trait GatewayEndpoints: Send + Sync {
    async fn endpoint(&self, slug: &str) -> Result<GatewayEndpointAccess>;

    async fn call_bearer(&self, slug: &str, chat: SessionId) -> Result<String> {
        let _ = chat;
        Ok(self.endpoint(slug).await?.bearer_token)
    }

    async fn entitled_app_catalogs(&self) -> Vec<GatewayRosterApp> {
        Vec::new()
    }
}

/// Applies runtime-owned gateway changes to the embedding MCP configuration.
#[async_trait]
pub trait GatewayMcpControl: Send + Sync {
    async fn auto_mount_gateway_endpoints(&self, entitled: &[String]) -> Result<bool>;
    async fn refresh_connected_app_roster(&self);
}

/// Commits one signed-in pairing through the embedding server's policy store.
/// The runtime calls this while it holds [`GATEWAY_PAIRING_WRITES`].
#[async_trait]
pub trait GatewayPairingCommit: Send + Sync {
    async fn commit(&self, base_url: &str, replaces: Option<&str>) -> Result<()>;
}

/// One gateway app catalog used by connected-app fingerprinting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayAppCatalog {
    pub name: String,
    pub operation_ids: Vec<String>,
}

/// Reads live gateway app catalogs.
#[async_trait]
pub trait GatewayCatalogSource: Send + Sync {
    async fn gateway_app_catalogs(
        &self,
        needed: &BTreeSet<String>,
    ) -> Option<(String, BTreeMap<String, GatewayAppCatalog>)>;
}

/// One shared-app operation call.
#[derive(Debug, Clone, PartialEq)]
pub struct GatewayOperationRequest {
    pub gateway_app: String,
    pub operation_id: String,
    pub path_parameters: Option<serde_json::Value>,
    pub query: Option<serde_json::Value>,
    pub body: Option<serde_json::Value>,
}

/// Why a gateway shared-app relay could not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayDispatchError {
    NoSession,
    NotRegistered,
    Unreachable(String),
}

/// Relays one governed shared-app operation.
#[async_trait]
pub trait GatewayInvokeDispatcher: Send + Sync {
    async fn dispatch(
        &self,
        owner: &OwnerId,
        app: AppId,
        request: &GatewayOperationRequest,
    ) -> std::result::Result<GatewayInvokeOutcome, GatewayDispatchError>;
}

/// One local app's registration state at a gateway deployment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayRegistration {
    Registered {
        shared_app_id: String,
        revision_id: String,
    },
    NotRegistered,
    Refused {
        message: String,
    },
}

/// The result of relaying a local grant to the gateway's consent record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayConsentRelay {
    Consented,
    NotRegistered,
    Refused { message: String },
}

/// Registers local apps and relays their consent at a gateway deployment.
#[async_trait]
pub trait GatewayDraftSource: Send + Sync {
    async fn ensure_registered(
        &self,
        owner: &OwnerId,
        app: AppId,
        gateway_base_url: &str,
    ) -> Result<GatewayRegistration>;

    async fn relay_consent(
        &self,
        owner: &OwnerId,
        app: AppId,
        gateway_base_url: &str,
    ) -> Result<GatewayConsentRelay>;
}

/// A catalog sync failed after the request started.
#[derive(Debug, thiserror::Error)]
pub enum GatewaySyncError {
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error("{message}")]
    Conflict { kind: &'static str, message: String },
}

impl GatewaySyncError {
    pub fn conflict(kind: &'static str, message: impl Into<String>) -> Self {
        Self::Conflict {
            kind,
            message: message.into(),
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Agent(error) => error.to_string(),
            Self::Conflict { message, .. } => message.clone(),
        }
    }
}

/// The process handle for one signed-in model-gateway session.
pub struct GatewayRuntime {
    secrets: Arc<dyn SecretProvider>,
    policy_source: Arc<dyn GatewayPolicySource>,
    model_state: Arc<dyn GatewayModelState>,
    app_usage: Arc<dyn GatewayAppUsageSource>,
    cached: Mutex<Option<(String, Arc<GatewayConnection>)>>,
    model_sync: Arc<RwLock<()>>,
    sign_in: Mutex<SignInProgress>,
    sign_in_generation: std::sync::atomic::AtomicU64,
    machine_offer: Mutex<Option<(String, String)>>,
    pending_pairing: Mutex<Option<PendingPairing>>,
    sync_commit_pause: Mutex<Option<Arc<SyncCommitPause>>>,
}

/// Test hook that pauses one catalog sync before its context recheck.
#[doc(hidden)]
#[derive(Default)]
pub struct SyncCommitPause {
    pub arrived: tokio::sync::Notify,
    pub release: tokio::sync::Notify,
}

impl SyncCommitPause {
    pub async fn wait_until_arrived(&self) {
        self.arrived.notified().await;
    }

    pub fn release(&self) {
        self.release.notify_one();
    }
}

#[derive(Clone)]
struct PendingPairing {
    base_url: String,
    mcp: Arc<dyn GatewayMcpControl>,
    commit: Arc<dyn GatewayPairingCommit>,
    replaces: Option<String>,
}

/// Renderer-safe progress of the current sign-in attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SignInProgress {
    /// No sign-in is running.
    Idle,
    /// The browser flow is open; the renderer should offer this URL.
    Pending { authorization_url: String },
    /// The last attempt failed with a bounded, secret-free message.
    Failed { message: String },
}

/// Renderer-safe projection of the gateway connection state. Never carries
/// token material — only what the settings surface displays. `base_url` is
/// the policy's gateway origin: present exactly when the profile is managed
/// with a usable URL (the retired `configured`/`enabled` bits collapsed into
/// its presence).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct GatewayStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base_url: Option<String>,
    pub signed_in: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub account_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub installation_id: Option<String>,
    pub model_count: usize,
    /// The member-catalog contract revision the last model sync read, or
    /// `None` while unsynced or against a gateway that predates
    /// `/api/v1/me/catalog`. The settings panel uses its absence (while
    /// signed in with models) to note that the deployment is older than
    /// this Tidebreak.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub member_catalog: Option<String>,
    pub sign_in: SignInProgress,
}

/// The hosted Tidebreak machine this profile's gateway offers, if it offers
/// one. Absent means the address field stays empty — a gateway that hosts no
/// machine and a gateway older than the field are the same thing here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GatewayMachineOffer {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Renderer-safe list of the connected apps the signed-in user is entitled
/// to, fetched live from the gateway (never cached: a revoked grant is gone
/// on the next request).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct GatewayApps {
    /// False when the connected gateway predates the JSON apps surface; the
    /// renderer hides the section instead of showing an empty list as "none".
    pub supported: bool,
    pub apps: Vec<GatewayAppInfo>,
}

/// One entitled connected app, with the slugs of the MCP endpoints that
/// aggregate it — the `mcp:<slug>` resources a mount would request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct GatewayAppInfo {
    pub id: String,
    pub name: String,
    pub app_kind: String,
    pub enabled: bool,
    pub mcp_endpoint_slugs: Vec<String>,
    /// The gateway's readiness for this app when the member catalog reports
    /// one: `ready`, `not_connected`, or `authorization_required`. `None`
    /// against a gateway that predates the catalog — the panel then shows
    /// no readiness rather than guessing. An unfamiliar value renders as
    /// not-ready copy, never an error: the set is the gateway's to grow.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub connection: Option<String>,
    /// How many live local-app grants bind this gateway app — the same
    /// "Used by N local apps" line the connected-apps page carries per record,
    /// so a user can see what a revocation here would break.
    pub used_by_app_count: usize,
}

impl GatewayRuntime {
    pub fn new(
        secrets: Arc<dyn SecretProvider>,
        policy_source: Arc<dyn GatewayPolicySource>,
        model_state: Arc<dyn GatewayModelState>,
        app_usage: Arc<dyn GatewayAppUsageSource>,
    ) -> Arc<Self> {
        Arc::new(Self {
            secrets,
            policy_source,
            model_state,
            app_usage,
            cached: Mutex::new(None),
            model_sync: Arc::new(RwLock::new(())),
            sign_in: Mutex::new(SignInProgress::Idle),
            sign_in_generation: std::sync::atomic::AtomicU64::new(0),
            machine_offer: Mutex::new(None),
            pending_pairing: Mutex::new(None),
            sync_commit_pause: Mutex::new(None),
        })
    }

    fn policy(&self) -> Result<GatewayPolicy> {
        self.policy_source.resolve()
    }

    #[doc(hidden)]
    pub async fn pause_next_sync_commit(&self, pause: Arc<SyncCommitPause>) {
        *self.sync_commit_pause.lock().await = Some(pause);
    }
}

fn clamp_u32(value: Option<i64>, default: u32) -> u32 {
    value
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}
