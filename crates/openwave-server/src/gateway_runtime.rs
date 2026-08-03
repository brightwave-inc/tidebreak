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

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use openwave_connectors::{
    CredentialVault, GatewayAuth, GatewayAuthConfig, GatewayConnection, RESOURCE_LLM,
};
use openwave_core::{AgentError, Result, SecretProvider, Store};
use openwave_router::BearerTokenSource;
use serde::Serialize;
use tokio::sync::Mutex;

use crate::providers::{self, CustomModelConfig};

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
    /// The OS authority for managed-mode resolution: a managed profile's
    /// deployment URL comes from the resolved policy, not the stored row.
    os_policy: Arc<dyn crate::managed_policy::OsPolicySource>,
    /// One connection per configured base URL; rebuilt when the URL changes.
    cached: Mutex<Option<(String, Arc<GatewayConnection>)>>,
    /// The one in-flight browser sign-in, if any.
    sign_in: Mutex<SignInProgress>,
    /// Bumped by every `begin_sign_in` and `sign_out` — and by every pending-
    /// pairing registration or dismissal; a background exchange task may only
    /// act while its own generation is still current, so a stale attempt can
    /// neither clobber a newer one's status, resurrect a signed-out session,
    /// nor commit a pairing that was dismissed or replaced under it.
    sign_in_generation: std::sync::atomic::AtomicU64,
    /// A deep-link pairing awaiting the sign-in that is its consent.
    ///
    /// Process-ephemeral on purpose: nothing durable exists until the user
    /// completes a sign-in against this gateway, so an unwanted provision
    /// link dies with a dismissal or the process. Set only by the desktop
    /// shell through [`crate::register_pending_pairing`] — the renderer can
    /// read and dismiss it, never set it or choose its URL.
    pending_pairing: Mutex<Option<PendingPairing>>,
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
    pub(crate) sign_in: SignInProgress,
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
}

impl GatewayRuntime {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        secrets: Arc<dyn SecretProvider>,
        os_policy: Arc<dyn crate::managed_policy::OsPolicySource>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            secrets,
            os_policy,
            cached: Mutex::new(None),
            sign_in: Mutex::new(SignInProgress::Idle),
            sign_in_generation: std::sync::atomic::AtomicU64::new(0),
            pending_pairing: Mutex::new(None),
        })
    }

    /// The one policy read every surface here shares.
    pub(crate) async fn policy(&self) -> Result<crate::managed_policy::ManagedPolicy> {
        crate::managed_policy::resolve(&*self.store, &*self.os_policy).await
    }

    /// Park a shell-validated pairing until a sign-in consents to it,
    /// replacing any earlier one — the latest link is the one the user acted
    /// on. Invalidate any in-flight browser flow the same way `sign_out`
    /// does: an exchange started against a replaced pairing must abandon
    /// rather than commit it.
    pub(crate) async fn register_pending_pairing(
        &self,
        base_url: String,
        mcp: Arc<crate::mcp_config::McpRuntime>,
        replaces: Option<String>,
    ) {
        let mut sign_in = self.sign_in.lock().await;
        self.sign_in_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *sign_in = SignInProgress::Idle;
        *self.pending_pairing.lock().await = Some(PendingPairing {
            base_url,
            mcp,
            replaces,
        });
    }

    /// The pending pairing's gateway URL, for the `/policy` projection.
    pub(crate) async fn pending_pairing_url(&self) -> Option<String> {
        self.pending_pairing
            .lock()
            .await
            .as_ref()
            .map(|pending| pending.base_url.clone())
    }

    /// Decline the pending pairing: clear it and invalidate any browser flow
    /// it started. Renderer-reachable, deliberately — declining changes
    /// nothing durable, so the failure direction is safe. With nothing
    /// pending it is a strict no-op: the generation must not move, or a
    /// stray dismiss could abandon a legitimate managed sign-in mid-flight.
    pub(crate) async fn dismiss_pending_pairing(&self) {
        let mut sign_in = self.sign_in.lock().await;
        let mut pending = self.pending_pairing.lock().await;
        if pending.is_none() {
            return;
        }
        self.sign_in_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *sign_in = SignInProgress::Idle;
        *pending = None;
    }

    /// The renderer-facing connection status, derived from policy alone: a
    /// profile is gateway-connected exactly when managed policy asserts it,
    /// so an unmanaged profile reads no gateway whatever legacy rows
    /// persist, and a managed policy whose URL is missing (misconfigured)
    /// reads none, honestly.
    pub(crate) async fn status(&self) -> Result<GatewayStatus> {
        // One policy read for the whole projection: the renderer polls this
        // every couple of seconds while a sign-in is pending.
        let policy = crate::managed_policy::resolve(&*self.store, &*self.os_policy).await?;
        let base_url = policy.gateway_url.clone();
        let credentials = match self.connection_for(&policy).await? {
            Some(connection) => connection.stored_credentials().await?,
            None => None,
        };
        Ok(GatewayStatus {
            base_url,
            signed_in: credentials.is_some(),
            account_hint: credentials
                .as_ref()
                .and_then(|credentials| credentials.account_hint.clone()),
            installation_id: credentials
                .as_ref()
                .map(|credentials| credentials.installation_id.clone()),
            model_count: providers::gateway_models(&*self.store, &policy)
                .await?
                .len(),
            sign_in: self.sign_in.lock().await.clone(),
        })
    }

    /// The entitled connected apps, fetched live from the gateway with the
    /// stored session. Managed-only, like the whole sign-in surface; a
    /// gateway without the JSON apps surface reports `supported: false`.
    pub(crate) async fn apps(&self) -> Result<GatewayApps> {
        let connection = self.managed_connection().await?;
        Ok(match connection.apps().await? {
            Some(apps) => GatewayApps {
                supported: true,
                apps: apps
                    .into_iter()
                    .map(|app| GatewayAppInfo {
                        id: app.id,
                        name: app.name,
                        app_kind: app.app_kind,
                        enabled: app.enabled,
                        mcp_endpoint_slugs: app.mcp_endpoint_slugs,
                    })
                    .collect(),
            },
            None => GatewayApps {
                supported: false,
                apps: Vec::new(),
            },
        })
    }

    /// Mount newly entitled gateway MCP endpoints into the configured server
    /// set — mount-by-default for a managed profile, where the organization
    /// already curated the entitlements.
    ///
    /// The entitlement source is the same `/api/v1/cli/apps` read the
    /// settings panel lists, so server and UI cannot disagree about what is
    /// entitled. Endpoints the user explicitly unmounted are remembered by
    /// the MCP runtime and never re-mounted here; a repeat reconcile with no
    /// new entitlements changes nothing. Every state where a reconcile cannot
    /// run — unmanaged profile, misconfigured policy, no session for the
    /// policy's deployment, a gateway predating the apps surface — is
    /// "nothing to do", not an error, so callers may run this on every
    /// trigger without gating.
    pub(crate) async fn reconcile_endpoint_mounts(
        &self,
        mcp: &crate::mcp_config::McpRuntime,
    ) -> Result<()> {
        let policy = self.policy().await?;
        let Some(connection) = self.connection_for(&policy).await? else {
            return Ok(());
        };
        if connection.stored_credentials().await?.is_none() {
            return Ok(());
        }
        let Some(apps) = connection.apps().await? else {
            return Ok(());
        };
        let entitled: Vec<String> = apps
            .into_iter()
            .flat_map(|app| app.mcp_endpoint_slugs)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if entitled.is_empty() {
            return Ok(());
        }
        if mcp.auto_mount_gateway_endpoints(&entitled).await? {
            tracing::info!("auto-mounted newly entitled gateway MCP endpoints");
        }
        Ok(())
    }

    /// Start a browser sign-in and return the URL to open.
    ///
    /// A pending pairing wins the target: the sign-in runs against the
    /// pairing's gateway, and a successful exchange is what commits the
    /// provision — the sign-in the user chose to complete is the pairing's
    /// consent, so nothing durable exists until it succeeds. That holds on a
    /// managed profile too: a pending pairing can only exist there through
    /// the shell's confirmed re-pair flow, never a bare deep link, so
    /// honoring it is honoring that confirmation. With nothing pending, a
    /// managed profile's sign-in targets the policy's gateway, and an
    /// unmanaged one keeps the legible refusal.
    ///
    /// The exchange completes in a background task: on success the session is
    /// stored (after any pairing commit), the entitled models synced, and the
    /// entitled MCP endpoints auto-mounted into `mcp`; on failure the status
    /// surface carries the bounded error until the next attempt.
    pub(crate) async fn begin_sign_in(
        self: &Arc<Self>,
        mcp: Arc<crate::mcp_config::McpRuntime>,
    ) -> Result<String> {
        let policy = self.policy().await?;
        let pairing = self.pending_pairing.lock().await.clone();
        let connection = match &pairing {
            Some(pending) => self.connection_at(pending.base_url.clone()).await?,
            None => self.connection_at(require_managed(&policy)?).await?,
        };
        let pending = connection.auth().start_sign_in().await?;
        let authorization_url = pending.authorization_url().to_string();
        let generation = {
            let mut sign_in = self.sign_in.lock().await;
            let generation = self
                .sign_in_generation
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            *sign_in = SignInProgress::Pending {
                authorization_url: authorization_url.clone(),
            };
            generation
        };

        let runtime = self.clone();
        tokio::spawn(async move {
            let finished = pending.finish(SIGN_IN_TIMEOUT).await;
            // Hold the state lock across the generation check and every
            // effect: `sign_out` bumps the generation and revokes under this
            // same lock, so a completion racing a sign-out either observes
            // the bump here and abandons, or finishes entirely before the
            // sign-out proceeds — a signed-out session can never be
            // resurrected between check and store.
            let mut sign_in = runtime.sign_in.lock().await;
            if runtime
                .sign_in_generation
                .load(std::sync::atomic::Ordering::SeqCst)
                != generation
            {
                return;
            }
            *sign_in = match finished {
                Ok(session) => {
                    // A pairing commits before its session persists: if the
                    // provision cannot be written (an MDM push claimed the
                    // profile mid-flow), no session lands on a profile the
                    // pairing's gateway does not manage. The generation
                    // being current is what proves the pairing was neither
                    // dismissed nor replaced while the browser flow ran —
                    // both bump it under this same lock.
                    let committed = match &pairing {
                        Some(pending) => runtime.commit_pairing(pending).await,
                        None => Ok(()),
                    };
                    let stored = match committed {
                        Ok(()) => connection.store_session(&session).await,
                        Err(error) => Err(error),
                    };
                    match stored {
                        Ok(()) => {
                            // Best-effort: a failed first sync leaves an
                            // explicit refresh affordance, not a failed
                            // sign-in.
                            let _ = runtime.sync_models().await;
                            // Likewise: mount-by-default must never fail a
                            // sign-in, and the background sync retries it.
                            if let Err(error) = runtime.reconcile_endpoint_mounts(&mcp).await {
                                tracing::warn!(
                                    "gateway endpoint auto-mount after sign-in failed \
                                     (the background sync will retry): {error}"
                                );
                            }
                            SignInProgress::Idle
                        }
                        Err(error) => SignInProgress::Failed {
                            message: error.to_string(),
                        },
                    }
                }
                Err(error) => SignInProgress::Failed {
                    message: error.to_string(),
                },
            };
        });
        Ok(authorization_url)
    }

    /// Commit the pairing a finishing sign-in consented to, then clear it.
    /// Runs from the exchange task with the sign-in state lock held, so it
    /// cannot interleave with a dismissal or re-registration.
    async fn commit_pairing(&self, pending: &PendingPairing) -> Result<()> {
        crate::pairing::commit_signed_in_pairing(
            &*self.store,
            &*self.os_policy,
            self.secrets.clone(),
            &pending.mcp,
            &pending.base_url,
            pending.replaces.as_deref(),
        )
        .await?;
        *self.pending_pairing.lock().await = None;
        Ok(())
    }

    /// Revoke the session (best-effort at the gateway), clear local state, and
    /// drop the synced model snapshot. Managed-only, like sign-in.
    pub(crate) async fn sign_out(&self) -> Result<()> {
        let policy = crate::managed_policy::resolve(&*self.store, &*self.os_policy).await?;
        let base_url = require_managed(&policy)?;
        // Take the state lock for the whole operation and invalidate any
        // pending browser flow before revoking anything: an exchange that
        // completes during the revoke round-trip serializes behind this lock
        // and then observes the bump, so it abandons instead of re-saving
        // the session it just minted.
        let mut sign_in = self.sign_in.lock().await;
        self.sign_in_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.connection_at(base_url.clone())
            .await?
            .sign_out()
            .await?;
        {
            // Serialize the snapshot clear with the other snapshot writers so
            // it cannot land inside one of their recheck-and-write windows.
            // Lock order matches the sign-in task's sync path: the sign-in
            // state lock is already held, the snapshot lock nests inside it.
            let _lock = providers::GATEWAY_STATE_WRITES.lock().await;
            if !providers::gateway_models(&*self.store, &policy)
                .await?
                .is_empty()
            {
                providers::write_gateway_snapshot(
                    &*self.store,
                    &providers::GatewayModelSnapshot {
                        gateway_url: base_url,
                        models: Vec::new(),
                    },
                )
                .await?;
            }
        }
        *sign_in = SignInProgress::Idle;
        Ok(())
    }

    /// The connection for the policy's gateway, or `None` when the profile is
    /// unmanaged (or the managed policy is misconfigured).
    ///
    /// The deployment comes from the resolved policy and nowhere else. The
    /// retired provider row was renderer-writable while unmanaged, so
    /// honoring it here would let a pre-provisioning write redirect sign-in
    /// and every minted bearer; it is never read.
    pub(crate) async fn connection(&self) -> Result<Option<Arc<GatewayConnection>>> {
        let policy = crate::managed_policy::resolve(&*self.store, &*self.os_policy).await?;
        self.connection_for(&policy).await
    }

    /// [`connection`](Self::connection) against an already-resolved policy, for
    /// callers that have one in hand.
    async fn connection_for(
        &self,
        policy: &crate::managed_policy::ManagedPolicy,
    ) -> Result<Option<Arc<GatewayConnection>>> {
        let Some(base_url) = policy.gateway_url.clone().filter(|_| policy.managed) else {
            return Ok(None);
        };
        Ok(Some(self.connection_at(base_url).await?))
    }

    /// The connection for the managed gateway, refusing legibly when the
    /// profile is unmanaged: the sign-in surface (sign-in, sign-out, apps,
    /// model sync) exists only under managed policy.
    async fn managed_connection(&self) -> Result<Arc<GatewayConnection>> {
        let policy = crate::managed_policy::resolve(&*self.store, &*self.os_policy).await?;
        let base_url = require_managed(&policy)?;
        self.connection_at(base_url).await
    }

    /// The cached connection for `base_url`, rebuilt when the URL changes.
    async fn connection_at(&self, base_url: String) -> Result<Arc<GatewayConnection>> {
        let mut cached = self.cached.lock().await;
        if let Some((url, connection)) = cached.as_ref() {
            if *url == base_url {
                return Ok(connection.clone());
            }
        }
        let auth_config = GatewayAuthConfig::new(&base_url)?;
        let connection = Arc::new(GatewayConnection::new(
            GatewayAuth::new(auth_config)?,
            CredentialVault::new(self.secrets.clone()),
        ));
        *cached = Some((base_url, connection.clone()));
        Ok(connection)
    }

    /// A router token source, when policy names a gateway and a session for
    /// that deployment is stored. `None` keeps the gateway route out of the
    /// router entirely — including on unmanaged profiles and when the stored
    /// session belongs to a different gateway than the policy URL.
    pub(crate) async fn route_token_source(&self) -> Option<Arc<dyn BearerTokenSource>> {
        let connection = self.connection().await.ok().flatten()?;
        connection.stored_credentials().await.ok().flatten()?;
        Some(Arc::new(GatewayTokenSource(connection)))
    }

    /// Fetch the entitled models and persist them as the stored snapshot,
    /// stamped with the deployment they came from. Managed-only.
    ///
    /// Returns how many models are entitled. The persisted snapshot drives the
    /// picker and model policy; entitlement itself stays live at the gateway,
    /// which refuses a revoked model at inference time regardless of what is
    /// cached here.
    pub(crate) async fn sync_models(
        &self,
    ) -> std::result::Result<usize, crate::error::ServerError> {
        let policy = crate::managed_policy::resolve(&*self.store, &*self.os_policy).await?;
        let base_url = require_managed(&policy)?;
        let connection = self.connection_at(base_url.clone()).await?;
        // The gateway routes these models over its Anthropic-compatible
        // surface, so sync exactly the set that protocol can serve. Fetched
        // before the row lock is taken: the entitlement round-trip must not
        // stall the other row writers.
        let models: Vec<CustomModelConfig> = connection
            .models(Some("anthropic_messages"))
            .await?
            .into_iter()
            .map(|model| CustomModelConfig {
                id: model.id,
                display_name: Some(model.name),
                context_window: clamp_u32(model.context_window, 32_768),
                max_output_tokens: clamp_u32(model.max_output_tokens, 4_096),
            })
            .collect();
        // The gateway is trusted for entitlements, not for shapes: the synced
        // set is held to the same bounds as user-entered custom models.
        providers::validate_custom_models(&models).map_err(|error| {
            AgentError::config(format!("gateway model sync rejected: {error:?}"))
        })?;
        let count = models.len();
        let _lock = providers::GATEWAY_STATE_WRITES.lock().await;
        // The fetch ran outside the lock, so the policy authority (an MDM
        // push) may have re-pointed the deployment while it was in flight.
        // Re-resolve under the lock and refuse to stamp a snapshot the new
        // policy never entitled.
        let policy = crate::managed_policy::resolve(&*self.store, &*self.os_policy).await?;
        if policy.gateway_url.as_deref() != Some(base_url.as_str()) {
            // A benign, retryable race — the deployment was re-pointed while
            // the fetch was in flight — not an internal fault. The stable
            // kind lets clients branch on it, as with `managed_profile`.
            return Err(crate::error::ServerError::conflict_kind(
                "gateway_changed",
                "the model gateway configuration changed during model sync",
            ));
        }
        providers::write_gateway_snapshot(
            &*self.store,
            &providers::GatewayModelSnapshot {
                gateway_url: base_url,
                models,
            },
        )
        .await?;
        Ok(count)
    }

    /// Keep the entitled-model snapshot fresh without a manual refresh: sync
    /// once immediately (the boot case) and then on a long interval, for as
    /// long as the process runs. Every state where a sync cannot run —
    /// unmanaged profile, misconfigured policy, no session for the policy's
    /// deployment — is "nothing to do", not an error: the loop waits for the
    /// state to change rather than exiting, because sign-in, pairing, and MDM
    /// pushes can all happen at any time.
    ///
    /// The same tick reconciles the entitled MCP endpoint mounts into `mcp`:
    /// the boot-with-stored-session case lands on the immediate first tick,
    /// and an admin's new entitlement reaches the tool surface within the
    /// sync interval. Each half fails independently — a failed entitlement
    /// fetch degrades to "no reconcile this tick", never touching the
    /// configuration.
    pub(crate) async fn sync_models_periodically(
        self: Arc<Self>,
        mcp: Arc<crate::mcp_config::McpRuntime>,
    ) {
        // One warning per outage, not one per retry: the failure state can
        // legitimately persist for hours on an offline laptop.
        let mut warned = false;
        let mut mount_warned = false;
        loop {
            let delay = match self.sync_models_if_connected().await {
                Ok(synced) => {
                    if let Some(count) = synced {
                        tracing::debug!("background gateway model sync: {count} models entitled");
                    }
                    warned = false;
                    MODEL_SYNC_INTERVAL
                }
                Err(error) => {
                    let message = error.message();
                    if warned {
                        tracing::debug!("background gateway model sync still failing: {message}");
                    } else {
                        tracing::warn!(
                            "background gateway model sync failed (will retry): {message}"
                        );
                        warned = true;
                    }
                    MODEL_SYNC_RETRY
                }
            };
            match self.reconcile_endpoint_mounts(&mcp).await {
                Ok(()) => mount_warned = false,
                Err(error) if mount_warned => {
                    tracing::debug!("gateway endpoint auto-mount still failing: {error}");
                }
                Err(error) => {
                    tracing::warn!("gateway endpoint auto-mount failed (will retry): {error}");
                    mount_warned = true;
                }
            }
            tokio::time::sleep(delay).await;
        }
    }

    /// One background sync attempt: `Ok(None)` when there is nothing to sync,
    /// `Ok(Some(count))` after a successful sync. The connected check mirrors
    /// [`status`](Self::status): a stored session counts only when it belongs
    /// to the policy's deployment.
    async fn sync_models_if_connected(
        &self,
    ) -> std::result::Result<Option<usize>, crate::error::ServerError> {
        let policy = self.policy().await?;
        let Some(connection) = self.connection_for(&policy).await? else {
            return Ok(None);
        };
        if connection.stored_credentials().await?.is_none() {
            return Ok(None);
        }
        Ok(Some(self.sync_models().await?))
    }
}

#[async_trait]
impl crate::mcp_config::GatewayEndpoints for GatewayRuntime {
    /// Resolve a gateway MCP endpoint from the signed-in session: its URL
    /// under the configured base, and a fresh `mcp:<slug>` bearer minted (or
    /// served from cache) inside the connector's rotation lock.
    async fn endpoint(&self, slug: &str) -> Result<crate::mcp_config::GatewayEndpointAccess> {
        let connection = self
            .connection()
            .await?
            .ok_or_else(|| AgentError::config("no model gateway is configured"))?;
        Ok(crate::mcp_config::GatewayEndpointAccess {
            url: connection.mcp_endpoint_url(slug)?,
            bearer_token: connection.mcp_access_token(slug).await?,
        })
    }
}

/// Retire a stored gateway session the resolved policy no longer stands
/// behind.
///
/// Two ways a session ends up superseded. An unmanaged profile can carry one
/// signed in under the retired additive mode: nothing reaches it any more —
/// the whole sign-in surface is managed-only, and the renderer has no
/// gateway page. And a managed profile's policy authority can re-point the
/// deployment (an MDM push): the old deployment's session is filtered out
/// of every route and token path, but its refresh token stays live at the
/// old gateway. Either way no surface will ever revoke it, so boot owns
/// that cleanup.
///
/// Revocation is best-effort and bounded: an unreachable gateway can no more
/// hold this hostage than it can a normal sign-out (the server-side session
/// still dies at refresh-token expiry), and boot must not stall on it. The
/// local clear afterwards is unconditional, so the session is gone locally
/// whether or not the gateway ever answered — and because it is gone, this
/// whole step runs at most once per superseded session.
///
/// An unreadable stored blob is left alone: it carries no usable refresh
/// token, so it is not the live zombie this exists to kill. So is the
/// session under a managed policy with no usable URL — that is a
/// misconfigured policy to repair, not a supersession, and the session may
/// well match it once repaired.
pub(crate) async fn retire_superseded_gateway_session(
    secrets: Arc<dyn SecretProvider>,
    policy: &crate::managed_policy::ManagedPolicy,
) -> Result<()> {
    /// Long enough for a healthy gateway to answer a revoke, short enough
    /// that a dead one is a hiccup at boot rather than a hang.
    const REVOKE_TIMEOUT: Duration = Duration::from_secs(5);

    let vault = CredentialVault::new(secrets.clone());
    let Ok(Some(credentials)) = vault.load().await else {
        return Ok(());
    };
    if policy.managed {
        let Some(gateway_url) = policy.gateway_url.as_deref() else {
            return Ok(());
        };
        if credentials.matches_base_url(gateway_url) {
            return Ok(());
        }
        tracing::warn!(
            "retiring the model-gateway session for {}: the managed policy \
             now resolves {gateway_url}; sign in there to connect",
            credentials.base_url
        );
    } else {
        tracing::warn!(
            "clearing a model-gateway session left by the retired additive \
             configuration ({}); pair via your gateway's page to sign in again",
            credentials.base_url
        );
    }
    // The connection owns revoke-then-clear (the refresh token never leaves
    // the connectors crate), so the session is retired through the same path
    // an explicit sign-out takes.
    if let Ok(config) = GatewayAuthConfig::new(&credentials.base_url) {
        if let Ok(auth) = GatewayAuth::new(config) {
            let connection = GatewayConnection::new(auth, CredentialVault::new(secrets.clone()));
            let _ = tokio::time::timeout(REVOKE_TIMEOUT, connection.sign_out()).await;
        }
    }
    // Unconditional: a gateway that never answered, or a stored base URL that
    // no longer parses, must not leave the credential behind.
    CredentialVault::new(secrets).clear().await
}

/// The one refusal for every managed-only gateway surface: unmanaged
/// profiles have no gateway (policy is the only source), and a managed
/// policy without a usable URL is misconfigured rather than open.
fn require_managed(policy: &crate::managed_policy::ManagedPolicy) -> Result<String> {
    if !policy.managed {
        return Err(AgentError::config(
            "this profile is not connected to a model gateway; \
             pair via your gateway's page to connect",
        ));
    }
    policy.gateway_url.clone().ok_or_else(|| {
        AgentError::config(
            "the managed gateway policy has no usable gateway URL; repair the policy authority",
        )
    })
}

fn clamp_u32(value: Option<i64>, default: u32) -> u32 {
    value
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

/// Router-facing supplier of the gateway's `llm`-resource token. Refresh and
/// rotation live inside [`GatewayConnection`]; this is just the seam.
struct GatewayTokenSource(Arc<GatewayConnection>);

#[async_trait]
impl BearerTokenSource for GatewayTokenSource {
    async fn bearer_token(&self) -> Result<String> {
        self.0.access_token(RESOURCE_LLM).await
    }

    /// A chat's inference rides a token minted inside that chat's
    /// attestation context, so the gateway records its tool calls as
    /// observations and attested MCP endpoints can match them. Requests
    /// with no conversation — titling, judging, other maintenance — keep
    /// the shared token: there is no chat for an observation to serve.
    async fn bearer_token_for(
        &self,
        conversation: Option<openwave_core::id::ChatId>,
    ) -> Result<String> {
        match conversation {
            Some(chat) => {
                self.0
                    .attested_access_token(RESOURCE_LLM, &chat.to_string())
                    .await
            }
            None => self.bearer_token().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::extract::{Form, State};
    use axum::http::HeaderMap;
    use axum::response::{IntoResponse, Json, Response};
    use axum::routing::{get, post};
    use axum::Router as AxumRouter;
    use openwave_core::DbStore;
    use serde_json::{json, Value};

    use super::*;

    #[derive(Default)]
    struct MockSecrets(std::sync::Mutex<HashMap<String, String>>);

    #[async_trait]
    impl SecretProvider for MockSecrets {
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

    #[derive(Default)]
    struct FakeGateway {
        refreshes: AtomicUsize,
        /// Every refresh token posted to `/oauth/revoke`, in order.
        revoked: std::sync::Mutex<Vec<String>>,
        /// When set, `/api/v1/cli/apps` answers 500 — the outage shape the
        /// endpoint-mount reconcile must degrade quietly on.
        apps_fail: std::sync::atomic::AtomicBool,
    }

    /// One entitled connected app aggregating the fixture's `tools` MCP
    /// endpoint, the shape `/api/v1/cli/apps` serves.
    async fn apps(State(gateway): State<Arc<FakeGateway>>, headers: HeaderMap) -> Response {
        if gateway.apps_fail.load(Ordering::SeqCst) {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "apps are down",
            )
                .into_response();
        }
        let bearer = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert!(bearer.starts_with("Bearer mg_at_control_"), "{bearer}");
        Json(json!({
            "apps": [{
                "id": "app-1",
                "name": "Tools",
                "app_kind": "mcp_endpoint",
                "enabled": true,
                "mcp_endpoint_slugs": ["tools"]
            }]
        }))
        .into_response()
    }

    async fn token(
        State(gateway): State<Arc<FakeGateway>>,
        Form(form): Form<HashMap<String, String>>,
    ) -> Json<Value> {
        assert_eq!(
            form.get("grant_type").map(String::as_str),
            Some("refresh_token")
        );
        let sequence = gateway.refreshes.fetch_add(1, Ordering::SeqCst);
        let resource = form.get("resource").cloned().unwrap_or_default();
        Json(json!({
            "access_token": format!("mg_at_{resource}_{sequence}"),
            "token_type": "Bearer",
            "expires_in": 600,
            "refresh_token": format!("mg_rt_{sequence}"),
            "scope": "models:read inference:invoke",
            "resource": resource,
            "installation_id": "install-1",
        }))
    }

    async fn models(
        axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
        headers: HeaderMap,
    ) -> Response {
        assert_eq!(
            query.get("protocol").map(String::as_str),
            Some("anthropic_messages")
        );
        let bearer = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert!(bearer.starts_with("Bearer mg_at_control_"), "{bearer}");
        Json(json!({
            "models": [
                {
                    "id": "sample-claude",
                    "name": "Sample Claude",
                    "context_window": 200000,
                    "max_output_tokens": 8192,
                    "supports_tools": true,
                    "supports_vision": true
                },
                {
                    "id": "sample-coder",
                    "name": "Sample Coder",
                    "context_window": null,
                    "max_output_tokens": null,
                    "supports_tools": true,
                    "supports_vision": false
                }
            ]
        }))
        .into_response()
    }

    /// Minimal Streamable HTTP MCP endpoint that requires an `mcp:tools`
    /// session bearer, exactly as the gateway's `/mcp/{slug}` route does.
    async fn mcp_endpoint(headers: HeaderMap, body: String) -> Response {
        let bearer = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert!(bearer.starts_with("Bearer mg_at_mcp:tools_"), "{bearer}");
        let request: Value = serde_json::from_str(&body).unwrap();
        let id = request.get("id").cloned().unwrap_or_default();
        let result = match request["method"].as_str().unwrap_or_default() {
            "initialize" => json!({
                "protocolVersion": openwave_mcp::PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "gateway-fixture", "version": "1"}
            }),
            "tools/list" => json!({
                "tools": [{
                    "name": "lookup",
                    "description": "Look something up",
                    "inputSchema": {"type": "object"}
                }]
            }),
            _ => json!({}),
        };
        Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
    }

    async fn revoke(
        State(gateway): State<Arc<FakeGateway>>,
        Form(form): Form<HashMap<String, String>>,
    ) -> Json<Value> {
        gateway
            .revoked
            .lock()
            .unwrap()
            .push(form.get("token").cloned().unwrap_or_default());
        Json(json!({}))
    }

    async fn serve(gateway: Arc<FakeGateway>) -> std::net::SocketAddr {
        let app = AxumRouter::new()
            .route(
                "/api/v1/meta",
                get(|| async {
                    Json(json!({
                        "api_version": "v1",
                        "installation_id": "install-1",
                        "gateway_version": "1.0.0",
                        "public_url": "http://gateway.test",
                        "auth_mode": "oidc",
                    }))
                }),
            )
            .route("/oauth/token", post(token))
            .route("/oauth/revoke", post(revoke))
            .route("/api/v1/cli/models", get(models))
            .route("/api/v1/cli/apps", get(apps))
            .route("/mcp/{slug}", post(mcp_endpoint))
            .with_state(gateway);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        address
    }

    /// A runtime with a stored session for `session_base`, on a profile
    /// provisioned (managed) to `provisioned` — policy being the only way a
    /// profile is gateway-connected at all.
    async fn signed_in_runtime_at(
        session_base: &str,
        provisioned: &str,
    ) -> (Arc<GatewayRuntime>, Arc<dyn Store>, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("gateway.db").display()
            ))
            .await
            .unwrap(),
        );
        let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
        // Stored credentials are private to the connectors crate by design;
        // seed the vault through its serialized form, exactly as a completed
        // sign-in would have persisted it.
        let credentials: openwave_connectors::GatewayCredentials = serde_json::from_value(json!({
            "base_url": session_base,
            "installation_id": "install-1",
            "user_id": "user-1",
            "account_hint": "abaas@example.test",
            "refresh_token": "mg_rt_seed",
            "access_tokens": {}
        }))
        .unwrap();
        CredentialVault::new(secrets.clone())
            .save(&credentials)
            .await
            .unwrap();
        crate::managed_policy::provision(&*store, provisioned)
            .await
            .unwrap();
        (
            GatewayRuntime::new(
                store.clone(),
                secrets,
                Arc::new(crate::managed_policy::NoOsPolicy),
            ),
            store,
            directory,
        )
    }

    async fn signed_in_runtime(
        base_url: &str,
    ) -> (Arc<GatewayRuntime>, Arc<dyn Store>, tempfile::TempDir) {
        signed_in_runtime_at(base_url, base_url).await
    }

    /// An MCP runtime resolving gateway endpoints through `runtime`, the way
    /// the server wires the two together.
    fn mcp_for(
        runtime: &Arc<GatewayRuntime>,
        store: &Arc<dyn Store>,
    ) -> Arc<crate::mcp_config::McpRuntime> {
        Arc::new(crate::mcp_config::McpRuntime::new(
            Arc::new(openwave_core::ToolRegistry::new()),
            store.clone(),
            runtime.clone(),
            Arc::new(crate::managed_policy::NoOsPolicy),
        ))
    }

    #[tokio::test]
    async fn syncs_entitled_models_into_the_snapshot() {
        let address = serve(Arc::new(FakeGateway::default())).await;
        let base = format!("http://{address}");
        let (runtime, store, _directory) = signed_in_runtime(&base).await;

        assert_eq!(runtime.sync_models().await.unwrap(), 2);

        let policy = crate::managed_policy::resolve(&*store, &crate::managed_policy::NoOsPolicy)
            .await
            .unwrap();
        let models = providers::gateway_models(&*store, &policy).await.unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "sample-claude");
        assert_eq!(models[0].display_name.as_deref(), Some("Sample Claude"));
        assert_eq!(models[0].context_window, 200_000);
        // Absent limits fall back to the conservative custom-model defaults.
        assert_eq!(models[1].context_window, 32_768);
        assert_eq!(models[1].max_output_tokens, 4_096);

        // The synced snapshot resolves as a model policy under the gateway key.
        let policy =
            providers::resolve_model_policy(&*store, "model_gateway::sample-claude", false)
                .await
                .unwrap()
                .expect("synced model resolves");
        assert_eq!(
            policy.provider,
            crate::providers::ProviderKind::ModelGateway
        );
        assert_eq!(policy.display_name, "Sample Claude");
    }

    /// The background loop's first attempt is immediate — the boot case it
    /// exists for: a signed-in profile gets a fresh snapshot without anyone
    /// pressing Refresh.
    #[tokio::test]
    async fn the_background_sync_populates_the_snapshot_without_a_manual_refresh() {
        let address = serve(Arc::new(FakeGateway::default())).await;
        let base = format!("http://{address}");
        let (runtime, store, _directory) = signed_in_runtime(&base).await;

        let task = tokio::spawn(
            runtime
                .clone()
                .sync_models_periodically(mcp_for(&runtime, &store)),
        );
        let synced = async {
            loop {
                if let Some(snapshot) = providers::read_gateway_snapshot(&*store).await.unwrap() {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        let snapshot = tokio::time::timeout(Duration::from_secs(5), synced)
            .await
            .expect("the boot-time sync lands without a manual refresh");
        task.abort();
        assert_eq!(snapshot.models.len(), 2);
    }

    #[tokio::test]
    async fn the_route_token_source_mints_llm_tokens_per_rotation() {
        let gateway = Arc::new(FakeGateway::default());
        let address = serve(gateway.clone()).await;
        let base = format!("http://{address}");
        let (runtime, store, _directory) = signed_in_runtime(&base).await;

        let source = runtime
            .route_token_source()
            .await
            .expect("signed-in runtime offers a token source");
        let token = source.bearer_token().await.unwrap();
        assert!(token.starts_with("mg_at_llm_"), "{token}");
        // A fresh token is cached: no second refresh inside the expiry leeway.
        assert_eq!(source.bearer_token().await.unwrap(), token);
        assert_eq!(gateway.refreshes.load(Ordering::SeqCst), 1);

        // A conversation mints inside that chat's attestation context: cached
        // per chat, distinct across chats, and never the shared token.
        let chat = openwave_core::id::ChatId::new();
        let attested = source.bearer_token_for(Some(chat)).await.unwrap();
        assert_ne!(attested, token);
        assert_eq!(source.bearer_token_for(Some(chat)).await.unwrap(), attested);
        let other = source
            .bearer_token_for(Some(openwave_core::id::ChatId::new()))
            .await
            .unwrap();
        assert_ne!(other, attested);
        // No conversation — titling, judging, maintenance — keeps the shared
        // token and records nothing.
        assert_eq!(source.bearer_token_for(None).await.unwrap(), token);

        // The route set includes the gateway with its synced models claimed.
        // A legacy provider row — even one pointing at a different deployment
        // — is never read, so it changes nothing about the composite route.
        runtime.sync_models().await.unwrap();
        providers::write_config(
            &*store,
            crate::providers::ProviderKind::ModelGateway,
            &providers::ProviderConfig {
                enabled: true,
                base_url: Some("http://127.0.0.1:9".to_string()),
                vertex_location: None,
                models: Vec::new(),
            },
        )
        .await
        .unwrap();
        let policy = crate::managed_policy::resolve(&*store, &crate::managed_policy::NoOsPolicy)
            .await
            .unwrap();
        let routes = providers::collect_routes(
            &*store,
            &*runtime.secrets,
            runtime.route_token_source().await,
            &policy,
        )
        .await;
        let gateway_route = routes
            .iter()
            .find(|route| route.kind == openwave_router::RouteKind::ModelGateway)
            .expect("gateway route present");
        assert_eq!(
            gateway_route.base_url.as_deref(),
            Some(format!("{base}/compat/anthropic").as_str())
        );
        assert!(gateway_route.api_key.is_empty());
        assert!(gateway_route
            .curated_models
            .contains(&"sample-claude".to_string()));
        assert!(providers::provider_is_usable(
            &*store,
            &*runtime.secrets,
            crate::providers::ProviderKind::ModelGateway,
            &policy
        )
        .await
        .unwrap());
    }

    /// The race #896 named, in its surviving form: with policy the only
    /// gateway source, the one authority that can re-point a deployment
    /// mid-sync is an OS (MDM) push. A sync whose entitlement fetch is still
    /// in flight when that happens must not stamp the old gateway's model
    /// list into the snapshot — what this pins is the under-lock policy
    /// recheck refusing the stale write.
    #[tokio::test]
    async fn a_sync_racing_a_policy_repoint_cannot_stamp_the_old_gateways_models() {
        // Gateway A: answers token refreshes normally but parks the model
        // list until released — the window the pairing lands in.
        let (arrived_tx, mut arrived_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let release = Arc::new(tokio::sync::Notify::new());
        let parked_models = {
            let release = release.clone();
            move || {
                let release = release.clone();
                let arrived = arrived_tx.clone();
                async move {
                    arrived.send(()).expect("the test is listening");
                    release.notified().await;
                    Json(json!({
                        "models": [{
                            "id": "stale-model",
                            "name": "Stale Model",
                            "context_window": 200000,
                            "max_output_tokens": 8192,
                            "supports_tools": true,
                            "supports_vision": false
                        }]
                    }))
                }
            }
        };
        let app = AxumRouter::new()
            .route("/oauth/token", post(token))
            .route("/api/v1/cli/models", get(parked_models))
            .with_state(Arc::new(FakeGateway::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_a = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // An OS authority whose asserted deployment can change mid-test, as
        // an MDM push would.
        struct SwappableOs(std::sync::Mutex<String>);

        impl crate::managed_policy::OsPolicySource for SwappableOs {
            fn gateway_url(&self) -> Result<Option<String>> {
                Ok(Some(self.0.lock().unwrap().clone()))
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("gateway.db").display()
            ))
            .await
            .unwrap(),
        );
        let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
        let credentials: openwave_connectors::GatewayCredentials = serde_json::from_value(json!({
            "base_url": base_a,
            "installation_id": "install-1",
            "user_id": "user-1",
            "refresh_token": "mg_rt_seed",
            "access_tokens": {}
        }))
        .unwrap();
        CredentialVault::new(secrets.clone())
            .save(&credentials)
            .await
            .unwrap();
        let os = Arc::new(SwappableOs(std::sync::Mutex::new(base_a.clone())));
        let runtime = GatewayRuntime::new(store.clone(), secrets, os.clone());

        let sync = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.sync_models().await }
        });
        arrived_rx
            .recv()
            .await
            .expect("the fetch reaches gateway A");

        // While A's model list is in flight, the MDM authority re-points the
        // profile at gateway B.
        *os.0.lock().unwrap() = "https://gateway-b.test".to_string();

        release.notify_one();
        let error = sync
            .await
            .unwrap()
            .expect_err("a sync whose deployment changed mid-fetch must refuse to write");
        assert_eq!(
            error.kind(),
            "gateway_changed",
            "the refusal is the stable retryable conflict, not a fault: {error:?}"
        );

        assert!(
            providers::read_gateway_snapshot(&*store)
                .await
                .unwrap()
                .is_none(),
            "gateway A's models must not be stamped into the snapshot"
        );
    }

    #[tokio::test]
    async fn status_reflects_the_session_and_sign_out_clears_the_snapshot() {
        let address = serve(Arc::new(FakeGateway::default())).await;
        let base = format!("http://{address}");
        let (runtime, store, _directory) = signed_in_runtime(&base).await;
        runtime.sync_models().await.unwrap();

        let status = runtime.status().await.unwrap();
        assert!(status.base_url.is_some() && status.signed_in);
        assert_eq!(status.account_hint.as_deref(), Some("abaas@example.test"));
        assert_eq!(status.installation_id.as_deref(), Some("install-1"));
        assert_eq!(status.model_count, 2);
        assert_eq!(status.sign_in, SignInProgress::Idle);
        // The projection carries no token-shaped material.
        let json = serde_json::to_string(&status).unwrap();
        assert!(!json.contains("mg_at_"));
        assert!(!json.contains("mg_rt_"));

        runtime.sign_out().await.unwrap();
        let status = runtime.status().await.unwrap();
        assert!(!status.signed_in);
        assert_eq!(status.model_count, 0);
        assert!(status.account_hint.is_none());
        let policy = crate::managed_policy::resolve(&*store, &crate::managed_policy::NoOsPolicy)
            .await
            .unwrap();
        assert!(providers::gateway_models(&*store, &policy)
            .await
            .unwrap()
            .is_empty());
        // Managed policy is a separate layer with a separate lifecycle:
        // disconnecting the session must never deprovision the profile.
        assert!(policy.managed, "sign-out must not deprovision the profile");
    }

    #[tokio::test]
    async fn mounts_a_gateway_endpoint_from_the_signed_in_session() {
        let address = serve(Arc::new(FakeGateway::default())).await;
        let base = format!("http://{address}");
        let (runtime, store, _directory) = signed_in_runtime(&base).await;

        let mcp = mcp_for(&runtime, &store);
        let definition = crate::mcp_config::McpServerDefinition {
            name: "tools".to_string(),
            command: None,
            args: Vec::new(),
            env: std::collections::BTreeMap::new(),
            env_from: Vec::new(),
            cwd: None,
            url: None,
            bearer_token_env: None,
            gateway_endpoint: Some("tools".to_string()),
            request_timeout_ms: 60_000,
            enabled: true,
        };
        // No environment variable anywhere: the URL and bearer come from the
        // stored session via a resource-scoped refresh (asserted inside the
        // fixture endpoint).
        let info = mcp
            .replace(crate::mcp_config::McpServersConfig {
                servers: vec![definition],
            })
            .await
            .unwrap();
        assert_eq!(
            info.servers[0].health,
            crate::mcp_config::McpHealth::Healthy
        );
        assert!(mcp.snapshot().get("mcp__tools__lookup").is_some());

        // Sign-out degrades the mount to a secret-free sign-in diagnostic and
        // keeps the definition; the tool leaves the registry.
        runtime.sign_out().await.unwrap();
        let error = mcp.reconnect("tools").await.err().unwrap();
        assert!(!error.to_string().contains("mg_at_"), "{error}");
        let info = mcp.info().await;
        assert_eq!(
            info.servers[0].health,
            crate::mcp_config::McpHealth::Degraded
        );
        assert_eq!(
            info.servers[0].diagnostic.as_deref(),
            Some("Sign in to the model gateway to reconnect this server.")
        );
        assert_eq!(
            info.servers[0].definition.gateway_endpoint.as_deref(),
            Some("tools")
        );
        assert!(mcp.snapshot().get("mcp__tools__lookup").is_none());
    }

    /// Mount-by-default, end to end: a reconcile against the entitled apps
    /// list mounts the endpoint enabled and connected, and a second
    /// reconcile with unchanged entitlements is a strict no-op — the
    /// persisted records are untouched, not rewritten to the same shape.
    #[tokio::test]
    async fn reconcile_auto_mounts_a_newly_entitled_endpoint_exactly_once() {
        let address = serve(Arc::new(FakeGateway::default())).await;
        let base = format!("http://{address}");
        let (runtime, store, _directory) = signed_in_runtime(&base).await;
        let mcp = mcp_for(&runtime, &store);

        runtime.reconcile_endpoint_mounts(&mcp).await.unwrap();
        let info = mcp.info().await;
        assert_eq!(info.servers.len(), 1);
        assert_eq!(info.servers[0].definition.name, "tools");
        assert_eq!(
            info.servers[0].definition.gateway_endpoint.as_deref(),
            Some("tools")
        );
        assert!(info.servers[0].definition.enabled);
        assert_eq!(
            info.servers[0].health,
            crate::mcp_config::McpHealth::Healthy
        );
        assert!(mcp.snapshot().get("mcp__tools__lookup").is_some());

        let records = store.list_connected_apps().await.unwrap();
        runtime.reconcile_endpoint_mounts(&mcp).await.unwrap();
        assert_eq!(
            store.list_connected_apps().await.unwrap(),
            records,
            "a reconcile with no new entitlements must not rewrite the records"
        );
    }

    /// A failing entitlements fetch degrades to "no reconcile this tick":
    /// the error surfaces to the caller (which logs and retries) and the
    /// configuration is untouched.
    #[tokio::test]
    async fn a_failing_entitlements_fetch_leaves_the_configuration_untouched() {
        let gateway = Arc::new(FakeGateway::default());
        gateway.apps_fail.store(true, Ordering::SeqCst);
        let address = serve(gateway).await;
        let base = format!("http://{address}");
        let (runtime, store, _directory) = signed_in_runtime(&base).await;
        let mcp = mcp_for(&runtime, &store);

        assert!(runtime.reconcile_endpoint_mounts(&mcp).await.is_err());
        assert!(mcp.info().await.servers.is_empty());
        assert!(store.list_connected_apps().await.unwrap().is_empty());
    }

    /// The sign-in surface on an unmanaged profile exists exactly while a
    /// pending pairing is parked: it targets the pairing's gateway, commits
    /// nothing by merely starting, and a dismissal restores the refusal —
    /// the write-path guard the retired confirmation dialog used to be.
    #[tokio::test]
    async fn a_pending_pairing_is_what_sign_in_targets_until_dismissed() {
        let address = serve(Arc::new(FakeGateway::default())).await;
        let base = format!("http://{address}/");
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("gateway.db").display()
            ))
            .await
            .unwrap(),
        );
        let runtime = GatewayRuntime::new(
            store.clone(),
            Arc::new(MockSecrets::default()),
            Arc::new(crate::managed_policy::NoOsPolicy),
        );
        let mcp = mcp_for(&runtime, &store);

        // Unmanaged with nothing pending: the legible refusal.
        assert!(runtime.begin_sign_in(mcp.clone()).await.is_err());

        runtime
            .register_pending_pairing(base.clone(), mcp.clone(), None)
            .await;
        let url = runtime.begin_sign_in(mcp.clone()).await.unwrap();
        assert!(
            url.starts_with(&format!("{base}oauth/authorize")),
            "sign-in must target the pending gateway: {url}"
        );
        // Starting the flow wrote nothing durable.
        let policy = crate::managed_policy::resolve(&*store, &crate::managed_policy::NoOsPolicy)
            .await
            .unwrap();
        assert!(!policy.managed);

        runtime.dismiss_pending_pairing().await;
        assert_eq!(runtime.pending_pairing_url().await, None);
        assert_eq!(
            runtime.status().await.unwrap().sign_in,
            SignInProgress::Idle
        );
        assert!(runtime.begin_sign_in(mcp).await.is_err());
    }

    #[tokio::test]
    async fn a_session_for_a_different_gateway_reads_signed_out() {
        let address = serve(Arc::new(FakeGateway::default())).await;
        let base = format!("http://{address}");
        // The profile is managed to one deployment while the stored session
        // (minted against `base`) belongs to another.
        let (runtime, _store, _directory) = signed_in_runtime_at(&base, "http://127.0.0.1:9").await;

        let status = runtime.status().await.unwrap();
        assert!(status.base_url.is_some());
        assert!(
            !status.signed_in,
            "a foreign session must not read signed-in"
        );
        assert!(status.account_hint.is_none());
        assert!(status.installation_id.is_none());
        assert!(runtime.route_token_source().await.is_none());
    }

    /// An OS (MDM) authority asserting one gateway, as a device-managed
    /// profile has.
    struct OsManaged(String);

    impl crate::managed_policy::OsPolicySource for OsManaged {
        fn gateway_url(&self) -> Result<Option<String>> {
            Ok(Some(self.0.clone()))
        }
    }

    /// A pure-MDM profile has no stored provider row at all — nothing ever
    /// wrote one — so reading the status from that row rendered "not
    /// configured" while sign-in, routing, and mounts all worked. Policy is
    /// the authority for a managed profile here, exactly as it is for the
    /// connection itself.
    #[tokio::test]
    async fn a_pure_mdm_profile_reads_configured_from_policy_without_a_stored_row() {
        let address = serve(Arc::new(FakeGateway::default())).await;
        let base = format!("http://{address}");
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("gateway.db").display()
            ))
            .await
            .unwrap(),
        );
        let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
        let credentials: openwave_connectors::GatewayCredentials = serde_json::from_value(json!({
            "base_url": base,
            "installation_id": "install-1",
            "user_id": "user-1",
            "account_hint": "abaas@example.test",
            "refresh_token": "mg_rt_seed",
            "access_tokens": {}
        }))
        .unwrap();
        CredentialVault::new(secrets.clone())
            .save(&credentials)
            .await
            .unwrap();
        let runtime =
            GatewayRuntime::new(store.clone(), secrets, Arc::new(OsManaged(base.clone())));

        assert!(
            providers::read_gateway_snapshot(&*store)
                .await
                .unwrap()
                .is_none(),
            "the fixture must have no stored gateway state for the assertion to mean anything"
        );
        let status = runtime.status().await.unwrap();
        assert!(status.signed_in);
        assert_eq!(
            status.base_url.as_deref(),
            Some(format!("{base}/").as_str())
        );
        assert_eq!(status.account_hint.as_deref(), Some("abaas@example.test"));
    }

    #[tokio::test]
    async fn a_signed_out_runtime_offers_no_route() {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("gateway.db").display()
            ))
            .await
            .unwrap(),
        );
        let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
        crate::managed_policy::provision(&*store, "http://127.0.0.1:1")
            .await
            .unwrap();
        let runtime = GatewayRuntime::new(
            store.clone(),
            secrets,
            Arc::new(crate::managed_policy::NoOsPolicy),
        );

        assert!(runtime.route_token_source().await.is_none());
        let policy = crate::managed_policy::resolve(&*store, &crate::managed_policy::NoOsPolicy)
            .await
            .unwrap();
        let routes = providers::collect_routes(&*store, &*runtime.secrets, None, &policy).await;
        assert!(routes
            .iter()
            .all(|route| route.kind != openwave_router::RouteKind::ModelGateway));
    }

    /// The legacy hard cut: an unmanaged profile with a stored additive
    /// gateway row — and even a leftover signed-in session — has zero
    /// gateway surface. The row is ignored, never auto-converted to
    /// managed: lockdown must not be imposed without the pairing consent
    /// flow, so the remedy is pairing, and the sign-in surface says so.
    #[tokio::test]
    async fn an_unmanaged_profile_with_a_legacy_row_has_no_gateway_surface() {
        struct StaticTokens;

        #[async_trait]
        impl BearerTokenSource for StaticTokens {
            async fn bearer_token(&self) -> Result<String> {
                Ok("mg_at_test".into())
            }
        }

        let address = serve(Arc::new(FakeGateway::default())).await;
        let base = format!("http://{address}");
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("gateway.db").display()
            ))
            .await
            .unwrap(),
        );
        let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
        let credentials: openwave_connectors::GatewayCredentials = serde_json::from_value(json!({
            "base_url": base,
            "installation_id": "install-1",
            "user_id": "user-1",
            "refresh_token": "mg_rt_seed",
            "access_tokens": {}
        }))
        .unwrap();
        CredentialVault::new(secrets.clone())
            .save(&credentials)
            .await
            .unwrap();
        providers::write_config(
            &*store,
            crate::providers::ProviderKind::ModelGateway,
            &providers::ProviderConfig {
                enabled: true,
                base_url: Some(format!("{base}/")),
                vertex_location: None,
                models: vec![CustomModelConfig {
                    id: "legacy-model".to_string(),
                    display_name: None,
                    context_window: 32_768,
                    max_output_tokens: 4_096,
                }],
            },
        )
        .await
        .unwrap();
        let runtime = GatewayRuntime::new(
            store.clone(),
            secrets.clone(),
            Arc::new(crate::managed_policy::NoOsPolicy),
        );

        // The boot cutover warns and ignores the row: no snapshot appears
        // and the profile stays unmanaged.
        let policy = crate::managed_policy::resolve(&*store, &crate::managed_policy::NoOsPolicy)
            .await
            .unwrap();
        providers::retire_legacy_gateway_row(&*store, &policy)
            .await
            .unwrap();
        assert!(providers::read_gateway_snapshot(&*store)
            .await
            .unwrap()
            .is_none());
        assert!(
            !policy.managed,
            "a legacy row must never auto-convert the profile to managed"
        );

        // The status surface reads no gateway and signed out.
        let status = runtime.status().await.unwrap();
        assert!(!status.signed_in);
        assert!(status.base_url.is_none());
        assert_eq!(status.model_count, 0);

        // Routing, the picker, and enumeration offer nothing — even with a
        // token source in hand, the gateway route is not built.
        assert!(runtime.route_token_source().await.is_none());
        let tokens: Arc<dyn BearerTokenSource> = Arc::new(StaticTokens);
        let routes = providers::collect_routes(&*store, &*secrets, Some(tokens), &policy).await;
        assert!(routes
            .iter()
            .all(|route| route.kind != openwave_router::RouteKind::ModelGateway));
        assert!(providers::catalog_models(&*store, &*secrets, &policy)
            .await
            .unwrap()
            .iter()
            .all(|model| model.policy.provider != crate::providers::ProviderKind::ModelGateway));
        assert!(providers::list_providers(&*store, &*secrets, &policy)
            .await
            .unwrap()
            .iter()
            .all(|provider| provider.kind != crate::providers::ProviderKind::ModelGateway));
        assert!(!providers::provider_is_usable(
            &*store,
            &*secrets,
            crate::providers::ProviderKind::ModelGateway,
            &policy
        )
        .await
        .unwrap());

        // The sign-in surface is managed-only, and the refusal names the
        // remedy.
        let error = runtime
            .begin_sign_in(mcp_for(&runtime, &store))
            .await
            .err()
            .unwrap();
        assert!(
            error.to_string().contains("pair via your gateway"),
            "{error}"
        );
        assert!(runtime.sync_models().await.is_err());
        assert!(runtime.apps().await.is_err());
        assert!(runtime.sign_out().await.is_err());
    }

    /// The boot cutover's carry-forward: a managed profile — the shape a
    /// gateway-page pairing produces — keeps the models its row had synced,
    /// once, and only from the deployment policy actually names.
    ///
    /// Nothing re-syncs the entitled set for a reader who is already signed
    /// in, so dropping this leaves their picker empty until they find the
    /// refresh button.
    ///
    /// The row URL is the VERBATIM form here: the old provider write path
    /// did not normalize (#935), so a profile that became managed by MDM
    /// over such a row holds "https://corp.gateway" beside a policy's
    /// "https://corp.gateway/". Compare deployments as strings instead of
    /// URLs and that profile silently reaches the picker empty.
    #[tokio::test]
    async fn boot_carries_a_managed_rows_snapshot_forward_once() {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("gateway.db").display()
            ))
            .await
            .unwrap(),
        );
        crate::managed_policy::provision(&*store, "https://corp.gateway")
            .await
            .unwrap();
        let legacy_row = |id: &str| providers::ProviderConfig {
            enabled: true,
            base_url: Some("https://corp.gateway".to_string()),
            vertex_location: None,
            models: vec![CustomModelConfig {
                id: id.to_string(),
                display_name: None,
                context_window: 32_768,
                max_output_tokens: 4_096,
            }],
        };
        providers::write_config(
            &*store,
            crate::providers::ProviderKind::ModelGateway,
            &legacy_row("carried-model"),
        )
        .await
        .unwrap();

        let policy = crate::managed_policy::resolve(&*store, &crate::managed_policy::NoOsPolicy)
            .await
            .unwrap();
        providers::retire_legacy_gateway_row(&*store, &policy)
            .await
            .unwrap();
        let models = providers::gateway_models(&*store, &policy).await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "carried-model");

        // The row is gone once it has been dealt with, so "retired entirely"
        // is true of the store and not only of the read paths.
        assert!(
            providers::read_config(&*store, crate::providers::ProviderKind::ModelGateway)
                .await
                .unwrap()
                .base_url
                .is_none()
        );

        // One-shot: once a snapshot exists, a row that reappears never
        // overwrites it.
        providers::write_config(
            &*store,
            crate::providers::ProviderKind::ModelGateway,
            &legacy_row("other-model"),
        )
        .await
        .unwrap();
        providers::retire_legacy_gateway_row(&*store, &policy)
            .await
            .unwrap();
        let models = providers::gateway_models(&*store, &policy).await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "carried-model");
    }

    /// A row from a deployment the profile is no longer managed by is not
    /// carried forward: its models describe another gateway's entitlements.
    /// The row still goes.
    #[tokio::test]
    async fn boot_discards_a_snapshot_from_a_foreign_deployment() {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("gateway.db").display()
            ))
            .await
            .unwrap(),
        );
        crate::managed_policy::provision(&*store, "https://corp.gateway")
            .await
            .unwrap();
        providers::write_config(
            &*store,
            crate::providers::ProviderKind::ModelGateway,
            &providers::ProviderConfig {
                enabled: true,
                base_url: Some("https://other.gateway".to_string()),
                vertex_location: None,
                models: vec![CustomModelConfig {
                    id: "foreign-model".to_string(),
                    display_name: None,
                    context_window: 32_768,
                    max_output_tokens: 4_096,
                }],
            },
        )
        .await
        .unwrap();

        let policy = crate::managed_policy::resolve(&*store, &crate::managed_policy::NoOsPolicy)
            .await
            .unwrap();
        providers::retire_legacy_gateway_row(&*store, &policy)
            .await
            .unwrap();
        assert!(providers::read_gateway_snapshot(&*store)
            .await
            .unwrap()
            .is_none());
        assert!(
            providers::read_config(&*store, crate::providers::ProviderKind::ModelGateway)
                .await
                .unwrap()
                .base_url
                .is_none()
        );
    }

    /// The unmanaged half of the cutover: the row is ignored (never
    /// converted to managed) and dropped, so the warning naming the remedy
    /// is a one-time upgrade notice rather than a line on every boot.
    #[tokio::test]
    async fn boot_drops_an_unmanaged_legacy_row_without_making_it_managed() {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("gateway.db").display()
            ))
            .await
            .unwrap(),
        );
        providers::write_config(
            &*store,
            crate::providers::ProviderKind::ModelGateway,
            &providers::ProviderConfig {
                enabled: true,
                base_url: Some("https://corp.gateway".to_string()),
                vertex_location: None,
                models: vec![CustomModelConfig {
                    id: "legacy-model".to_string(),
                    display_name: None,
                    context_window: 32_768,
                    max_output_tokens: 4_096,
                }],
            },
        )
        .await
        .unwrap();

        let policy = crate::managed_policy::resolve(&*store, &crate::managed_policy::NoOsPolicy)
            .await
            .unwrap();
        providers::retire_legacy_gateway_row(&*store, &policy)
            .await
            .unwrap();

        assert!(
            !crate::managed_policy::resolve(&*store, &crate::managed_policy::NoOsPolicy)
                .await
                .unwrap()
                .managed,
            "a legacy row must never auto-convert the profile to managed"
        );
        assert!(providers::read_gateway_snapshot(&*store)
            .await
            .unwrap()
            .is_none());
        assert!(
            providers::read_config(&*store, crate::providers::ProviderKind::ModelGateway)
                .await
                .unwrap()
                .base_url
                .is_none()
        );
    }

    /// The session half of the legacy hard cut: an unmanaged profile with a
    /// session left over from the retired additive mode has no surface that
    /// could ever revoke it, so boot clears it. Without this the refresh
    /// token lives in the keychain forever.
    #[tokio::test]
    async fn boot_clears_a_gateway_session_left_on_an_unmanaged_profile() {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("gateway.db").display()
            ))
            .await
            .unwrap(),
        );
        let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
        let seed = |secrets: Arc<dyn SecretProvider>, base_url: &'static str| async move {
            let credentials: openwave_connectors::GatewayCredentials =
                serde_json::from_value(json!({
                    "base_url": base_url,
                    "installation_id": "install-1",
                    "user_id": "user-1",
                    "refresh_token": "mg_rt_zombie",
                    "access_tokens": {}
                }))
                .unwrap();
            CredentialVault::new(secrets)
                .save(&credentials)
                .await
                .unwrap();
        };
        // Nothing listens here: the revoke fails fast and the clear happens
        // anyway, which is the contract.
        seed(secrets.clone(), "http://127.0.0.1:1").await;

        let policy = crate::managed_policy::resolve(&*store, &crate::managed_policy::NoOsPolicy)
            .await
            .unwrap();
        assert!(!policy.managed);
        retire_superseded_gateway_session(secrets.clone(), &policy)
            .await
            .unwrap();
        assert!(
            !openwave_connectors::has_stored_credentials(&*secrets).await,
            "the retired session must not survive boot on an unmanaged profile"
        );

        // A stored base URL that no longer passes the gateway contract has
        // no connection to revoke through, so only the unconditional clear
        // can retire it. Drop that clear and the credential survives here.
        seed(secrets.clone(), "http://user:pw@stale.example").await;
        retire_superseded_gateway_session(secrets.clone(), &policy)
            .await
            .unwrap();
        assert!(
            !openwave_connectors::has_stored_credentials(&*secrets).await,
            "a session whose stored URL cannot be parsed must still be cleared"
        );

        // A managed profile's session is untouched: it is the credential the
        // profile actually runs on.
        seed(secrets.clone(), "https://corp.gateway/").await;
        crate::managed_policy::provision(&*store, "https://corp.gateway")
            .await
            .unwrap();
        let policy = crate::managed_policy::resolve(&*store, &crate::managed_policy::NoOsPolicy)
            .await
            .unwrap();
        retire_superseded_gateway_session(secrets.clone(), &policy)
            .await
            .unwrap();
        assert!(openwave_connectors::has_stored_credentials(&*secrets).await);
    }

    /// The managed analogue of the legacy hard cut: an MDM re-point
    /// supersedes the stored session, and boot is the only surface that will
    /// ever revoke it at the old deployment.
    #[tokio::test]
    async fn boot_retires_the_session_an_mdm_repoint_superseded() {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("gateway.db").display()
            ))
            .await
            .unwrap(),
        );
        let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
        let old_gateway = Arc::new(FakeGateway::default());
        let old_base = format!("http://{}", serve(old_gateway.clone()).await);
        let credentials: openwave_connectors::GatewayCredentials = serde_json::from_value(json!({
            "base_url": old_base,
            "installation_id": "install-1",
            "user_id": "user-1",
            "refresh_token": "mg_rt_zombie",
            "access_tokens": {}
        }))
        .unwrap();
        CredentialVault::new(secrets.clone())
            .save(&credentials)
            .await
            .unwrap();
        crate::managed_policy::provision(&*store, "https://corp-new.gateway")
            .await
            .unwrap();
        let mut policy =
            crate::managed_policy::resolve(&*store, &crate::managed_policy::NoOsPolicy)
                .await
                .unwrap();
        assert!(policy.managed);

        // A managed policy with no usable URL is misconfiguration, not a
        // re-point: the session is left for the repaired policy to judge.
        policy.gateway_url = None;
        retire_superseded_gateway_session(secrets.clone(), &policy)
            .await
            .unwrap();
        assert!(openwave_connectors::has_stored_credentials(&*secrets).await);

        policy.gateway_url = Some("https://corp-new.gateway".into());
        retire_superseded_gateway_session(secrets.clone(), &policy)
            .await
            .unwrap();
        assert!(
            !openwave_connectors::has_stored_credentials(&*secrets).await,
            "the superseded session must not survive the re-point"
        );
        // Revoked at the old deployment, with the superseded refresh token.
        assert_eq!(
            old_gateway.revoked.lock().unwrap().as_slice(),
            ["mg_rt_zombie"]
        );
    }

    /// A completed sign-in supersedes whatever session is stored — possibly
    /// one minted at a different deployment, after a re-point. Its refresh
    /// token is revoked at its own gateway before the overwrite orphans it.
    #[tokio::test]
    async fn signing_in_revokes_the_superseded_session_at_its_own_gateway() {
        let old_gateway = Arc::new(FakeGateway::default());
        let old_base = format!("http://{}", serve(old_gateway.clone()).await);
        let new_gateway = Arc::new(FakeGateway::default());
        let new_base = format!("http://{}", serve(new_gateway.clone()).await);
        let secrets: Arc<dyn SecretProvider> = Arc::new(MockSecrets::default());
        let superseded: openwave_connectors::GatewayCredentials = serde_json::from_value(json!({
            "base_url": old_base,
            "installation_id": "install-1",
            "user_id": "user-1",
            "refresh_token": "mg_rt_zombie",
            "access_tokens": {}
        }))
        .unwrap();
        CredentialVault::new(secrets.clone())
            .save(&superseded)
            .await
            .unwrap();

        let connection = GatewayConnection::new(
            GatewayAuth::new(GatewayAuthConfig::new(&new_base).unwrap()).unwrap(),
            CredentialVault::new(secrets.clone()),
        );
        connection
            .store_session(&openwave_connectors::AuthorizedSession {
                meta: openwave_connectors::GatewayMeta {
                    api_version: "1".into(),
                    installation_id: "install-2".into(),
                    gateway_version: "1".into(),
                    public_url: new_base.clone(),
                    auth_mode: "oauth".into(),
                },
                identity: openwave_connectors::GatewayIdentity {
                    user_id: "user-2".into(),
                    email: Some("user@example.test".into()),
                    display_name: None,
                    session_id: "session-2".into(),
                    installation_id: "install-2".into(),
                },
                tokens: openwave_connectors::TokenSet {
                    access_token: "mg_at_fresh".into(),
                    refresh_token: "mg_rt_fresh".into(),
                    expires_at_unix: u64::MAX,
                    scope: "models:read inference:invoke".into(),
                    resource: "control".into(),
                    installation_id: "install-2".into(),
                },
            })
            .await
            .unwrap();

        // The revoke went to the superseded session's own deployment — not
        // the connection's — and the new session is stored for the new one.
        assert_eq!(
            old_gateway.revoked.lock().unwrap().as_slice(),
            ["mg_rt_zombie"]
        );
        assert!(new_gateway.revoked.lock().unwrap().is_empty());
        assert!(openwave_connectors::has_stored_credentials_for(&*secrets, &new_base).await);
    }
}
