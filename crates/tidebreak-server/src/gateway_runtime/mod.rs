//! The server's handle on a signed-in model-gateway session.
//!
//! Owns the [`GatewayConnection`] built from the resolved managed policy —
//! the only gateway source in either direction — hands the router a
//! per-request token source for the gateway's short-lived `llm` tokens, and
//! syncs the entitled model list into the stored snapshot so the picker and
//! model policy work from durable local state while the gateway stays the
//! live authority at inference time. On an unmanaged profile every surface
//! here is inert: no connection, no routes, and the sign-in endpoints refuse
//! with a pointer at the pairing flow.
//!
//! This file holds the [`GatewayRuntime`] handle, its types, and its
//! construction. Each concern extends the same `impl GatewayRuntime` from
//! its own file:
//!
//! - [`session`]: pairing, sign-in, sign-out, status, and the connection.
//! - [`apps`]: entitled apps, endpoint mounts, and the MCP catalog.
//! - [`models`]: the model snapshot, catalog sync, and the route token source.
//! - [`relay`]: the shared-app invoke relay.

use std::sync::Arc;
use std::time::Duration;

use crate::connectors::{
    is_sign_in_required, CredentialVault, GatewayApp, GatewayAuth, GatewayAuthConfig,
    GatewayCatalogFetch, GatewayConnection, GatewayOperationSummary, MEMBER_CATALOG_V1,
    RESOURCE_CONTROL, RESOURCE_LLM,
};
use async_trait::async_trait;
use serde::Serialize;
use tidebreak_core::{AgentError, Result, SecretProvider, Store};
use tidebreak_router::{BearerTokenSource, ModelRouteLease};
use tokio::sync::{Mutex, OwnedRwLockWriteGuard, RwLock};

use crate::providers::{self, CustomModelConfig};

mod apps;
mod models;
mod relay;
mod session;
#[cfg(test)]
mod tests;

pub(crate) use relay::gateway_relay_dispatcher;
pub(crate) use session::retire_superseded_gateway_session;

use session::*;

/// How long a browser sign-in may stay pending before it fails.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

/// How often the background loop refreshes the entitled-model snapshot while
/// a session is connected. A sync is one small GET against the org's own
/// gateway, so a tight cadence is cheap — this is what bounds how quickly an
/// admin's entitlement change reaches the picker without a manual refresh.
const MODEL_SYNC_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Retry cadence after a failed background sync — short enough that a boot
/// racing the network coming up doesn't stay stale for a whole interval,
/// long enough not to hammer an unreachable gateway.
const MODEL_SYNC_RETRY: Duration = Duration::from_secs(60);

pub(crate) struct GatewayRuntime {
    store: Arc<dyn Store>,
    secrets: Arc<dyn SecretProvider>,
    /// The provisioned-policy home for managed-mode resolution: the sticky
    /// pairing record, durable beside (not inside) the SQLite profile.
    provisioned_policy: Arc<dyn crate::managed_policy::ProvisionedPolicySource>,
    /// The OS authority for managed-mode resolution: a managed profile's
    /// deployment URL comes from the resolved policy, not the stored row.
    os_policy: Arc<dyn crate::managed_policy::OsPolicySource>,
    /// One connection per configured base URL; rebuilt when the URL changes.
    cached: Mutex<Option<(String, Arc<GatewayConnection>)>>,
    /// Shared by every request-leg route lease and every local mutation of
    /// gateway execution authority. Request setup takes a read lease through
    /// dispatch; catalog sync, sign-out, deprovision, and session replacement
    /// take the write side. A writer therefore runs either before a leg
    /// validates and mints its bearer or after that leg has dispatched, never
    /// in the security-sensitive gap between them. Catalog sync deliberately
    /// keeps the write side across its fetch so older responses cannot commit
    /// after newer ones.
    model_sync: Arc<RwLock<()>>,
    /// The one in-flight browser sign-in, if any.
    sign_in: Mutex<SignInProgress>,
    /// Bumped by every `begin_sign_in` and `sign_out` — and by every pending-
    /// pairing registration or dismissal; a background exchange task may only
    /// act while its own generation is still current, so a stale attempt can
    /// neither clobber a newer one's status, resurrect a signed-out session,
    /// nor commit a pairing that was dismissed or replaced under it.
    sign_in_generation: std::sync::atomic::AtomicU64,
    /// The machine offer last read from the gateway's `/api/v1/meta`, keyed
    /// by the gateway it was read from so a re-pair reads the new one.
    /// Only a present offer is cached. An older gateway or a deployment that
    /// has not published its machine yet is retried while the process runs.
    machine_offer: Mutex<Option<(String, String)>>,
    /// A deep-link pairing awaiting the sign-in that is its consent.
    ///
    /// Process-ephemeral on purpose: nothing durable exists until the user
    /// completes a sign-in against this gateway, so an unwanted provision
    /// link dies with a dismissal or the process. Set only by the desktop
    /// shell through [`crate::register_pending_pairing`] — the renderer can
    /// read and dismiss it, never set it or choose its URL.
    pending_pairing: Mutex<Option<PendingPairing>>,
    #[cfg(test)]
    sync_commit_pause: Mutex<Option<Arc<MigrationPause>>>,
}

#[cfg(test)]
#[derive(Default)]
struct MigrationPause {
    arrived: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

/// The shell-registered pairing a sign-in may commit.
#[derive(Clone)]
struct PendingPairing {
    /// Normalized gateway base URL, already held to the connectors contract.
    base_url: String,
    /// Carried from the shell's [`crate::PairingHandle`] at registration so
    /// the commit cannot run without also applying what it decides to the
    /// MCP servers this process is running.
    mcp: Arc<crate::mcp_config::McpRuntime>,
    /// For a re-pairing the user confirmed: the provisioned URL the
    /// confirmation named, which the commit's compare-and-swap holds the row
    /// to. `None` for a plain first-time pairing.
    replaces: Option<String>,
}

/// Renderer-safe progress of the current sign-in attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum SignInProgress {
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
pub(crate) struct GatewayStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub(crate) base_url: Option<String>,
    pub(crate) signed_in: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub(crate) account_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub(crate) installation_id: Option<String>,
    pub(crate) model_count: usize,
    /// The member-catalog contract revision the last model sync read, or
    /// `None` while unsynced or against a gateway that predates
    /// `/api/v1/me/catalog`. The settings panel uses its absence (while
    /// signed in with models) to note that the deployment is older than
    /// this Tidebreak.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub(crate) member_catalog: Option<String>,
    pub(crate) sign_in: SignInProgress,
}

/// The hosted Tidebreak machine this profile's gateway offers, if it offers
/// one. Absent means the address field stays empty — a gateway that hosts no
/// machine and a gateway older than the field are the same thing here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GatewayMachineOffer {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) url: Option<String>,
}

/// Renderer-safe list of the connected apps the signed-in user is entitled
/// to, fetched live from the gateway (never cached: a revoked grant is gone
/// on the next request).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub(crate) struct GatewayApps {
    /// False when the connected gateway predates the JSON apps surface; the
    /// renderer hides the section instead of showing an empty list as "none".
    pub(crate) supported: bool,
    pub(crate) apps: Vec<GatewayAppInfo>,
}

/// One entitled connected app, with the slugs of the MCP endpoints that
/// aggregate it — the `mcp:<slug>` resources a mount would request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub(crate) struct GatewayAppInfo {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) app_kind: String,
    pub(crate) enabled: bool,
    pub(crate) mcp_endpoint_slugs: Vec<String>,
    /// The gateway's readiness for this app when the member catalog reports
    /// one: `ready`, `not_connected`, or `authorization_required`. `None`
    /// against a gateway that predates the catalog — the panel then shows
    /// no readiness rather than guessing. An unfamiliar value renders as
    /// not-ready copy, never an error: the set is the gateway's to grow.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub(crate) connection: Option<String>,
    /// How many live local-app grants bind this gateway app — the same
    /// "Used by N local apps" line the connected-apps page carries per record,
    /// so a user can see what a revocation here would break.
    pub(crate) used_by_app_count: usize,
}

impl GatewayRuntime {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        secrets: Arc<dyn SecretProvider>,
        provisioned_policy: Arc<dyn crate::managed_policy::ProvisionedPolicySource>,
        os_policy: Arc<dyn crate::managed_policy::OsPolicySource>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            secrets,
            provisioned_policy,
            os_policy,
            cached: Mutex::new(None),
            model_sync: Arc::new(RwLock::new(())),
            sign_in: Mutex::new(SignInProgress::Idle),
            sign_in_generation: std::sync::atomic::AtomicU64::new(0),
            machine_offer: Mutex::new(None),
            pending_pairing: Mutex::new(None),
            #[cfg(test)]
            sync_commit_pause: Mutex::new(None),
        })
    }

    /// The provisioned-policy home this runtime resolves against — the
    /// pairing write path must commit through the same instance.
    pub(crate) fn provisioned_policy(
        &self,
    ) -> &Arc<dyn crate::managed_policy::ProvisionedPolicySource> {
        &self.provisioned_policy
    }

    /// The one policy read every surface here shares.
    pub(crate) fn policy(&self) -> Result<crate::managed_policy::ManagedPolicy> {
        crate::managed_policy::resolve(&*self.provisioned_policy, &*self.os_policy)
    }
}
