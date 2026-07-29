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
    /// Bumped by every `begin_sign_in` and `sign_out`; a background exchange
    /// task may only store its session and stamp its outcome while its own
    /// generation is still current, so a stale attempt can neither clobber a
    /// newer one's status nor resurrect a signed-out session.
    sign_in_generation: std::sync::atomic::AtomicU64,
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
/// token material — only what the settings surface displays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub(crate) struct GatewayStatus {
    pub(crate) configured: bool,
    pub(crate) enabled: bool,
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
        })
    }

    /// The renderer-facing connection status, derived from policy alone: a
    /// profile is gateway-connected exactly when managed policy asserts it,
    /// so an unmanaged profile reads unconfigured whatever legacy rows
    /// persist, and a managed policy whose URL is missing (misconfigured)
    /// reads unconfigured, honestly.
    ///
    /// `configured` and `enabled` are now the same bit — kept apart only for
    /// wire-shape stability until the renderer slice retires them.
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
            configured: base_url.is_some(),
            enabled: base_url.is_some(),
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

    /// Start a browser sign-in and return the URL to open.
    ///
    /// The exchange completes in a background task: on success the session is
    /// stored and the entitled models synced; on failure the status surface
    /// carries the bounded error until the next attempt.
    pub(crate) async fn begin_sign_in(self: &Arc<Self>) -> Result<String> {
        let connection = self.managed_connection().await?;
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
                Ok(session) => match connection.store_session(&session).await {
                    Ok(()) => {
                        // Best-effort: a failed first sync leaves an explicit
                        // refresh affordance, not a failed sign-in.
                        let _ = runtime.sync_models().await;
                        SignInProgress::Idle
                    }
                    Err(error) => SignInProgress::Failed {
                        message: error.to_string(),
                    },
                },
                Err(error) => SignInProgress::Failed {
                    message: error.to_string(),
                },
            };
        });
        Ok(authorization_url)
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

    async fn serve(gateway: Arc<FakeGateway>) -> std::net::SocketAddr {
        let app = AxumRouter::new()
            .route("/oauth/token", post(token))
            .route("/api/v1/cli/models", get(models))
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
        assert!(status.configured && status.enabled && status.signed_in);
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

        let mcp = Arc::new(crate::mcp_config::McpRuntime::new(
            Arc::new(openwave_core::ToolRegistry::new()),
            store.clone(),
            runtime.clone(),
            Arc::new(crate::managed_policy::NoOsPolicy),
        ));
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

    #[tokio::test]
    async fn a_session_for_a_different_gateway_reads_signed_out() {
        let address = serve(Arc::new(FakeGateway::default())).await;
        let base = format!("http://{address}");
        // The profile is managed to one deployment while the stored session
        // (minted against `base`) belongs to another.
        let (runtime, _store, _directory) = signed_in_runtime_at(&base, "http://127.0.0.1:9").await;

        let status = runtime.status().await.unwrap();
        assert!(status.configured);
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
        assert!(status.configured && status.enabled && status.signed_in);
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

        // The status surface reads unconfigured and signed out.
        let status = runtime.status().await.unwrap();
        assert!(!status.configured && !status.enabled && !status.signed_in);
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
        let error = runtime.begin_sign_in().await.err().unwrap();
        assert!(
            error.to_string().contains("pair via your gateway"),
            "{error}"
        );
        assert!(runtime.sync_models().await.is_err());
        assert!(runtime.apps().await.is_err());
        assert!(runtime.sign_out().await.is_err());
    }

    /// The boot cutover's carry-forward: a managed profile whose legacy row
    /// was stamped by the policy's own deployment keeps its synced models
    /// across the upgrade — once, and only from that deployment.
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
            base_url: Some("https://corp.gateway/".to_string()),
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

        // One-shot: once a snapshot exists, later boots never overwrite it
        // from the retired row.
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
}
