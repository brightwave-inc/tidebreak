//! The server's handle on a signed-in model-gateway session.
//!
//! Owns the [`GatewayConnection`] built from the persisted provider
//! configuration, hands the router a per-request token source for the
//! gateway's short-lived `llm` tokens, and syncs the entitled model list into
//! the provider's stored custom-model set so the picker and model policy work
//! from durable local state while the gateway stays the live authority at
//! inference time.

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

use crate::providers::{self, CustomModelConfig, ProviderKind};

/// How long a browser sign-in may stay pending before it fails.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

pub(crate) struct GatewayRuntime {
    store: Arc<dyn Store>,
    secrets: Arc<dyn SecretProvider>,
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
    pub(crate) fn new(store: Arc<dyn Store>, secrets: Arc<dyn SecretProvider>) -> Arc<Self> {
        Arc::new(Self {
            store,
            secrets,
            cached: Mutex::new(None),
            sign_in: Mutex::new(SignInProgress::Idle),
            sign_in_generation: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// The renderer-facing connection status.
    pub(crate) async fn status(&self) -> Result<GatewayStatus> {
        let config = providers::read_config(&*self.store, ProviderKind::ModelGateway).await?;
        let credentials = match self.connection().await? {
            Some(connection) => connection.stored_credentials().await?,
            None => None,
        };
        Ok(GatewayStatus {
            configured: config.base_url.is_some(),
            enabled: config.enabled,
            base_url: config.base_url,
            signed_in: credentials.is_some(),
            account_hint: credentials
                .as_ref()
                .and_then(|credentials| credentials.account_hint.clone()),
            installation_id: credentials
                .as_ref()
                .map(|credentials| credentials.installation_id.clone()),
            model_count: config.models.len(),
            sign_in: self.sign_in.lock().await.clone(),
        })
    }

    /// The entitled connected apps, fetched live from the gateway with the
    /// stored session. Requires a configured gateway and a signed-in session;
    /// a gateway without the JSON apps surface reports `supported: false`.
    pub(crate) async fn apps(&self) -> Result<GatewayApps> {
        let connection = self
            .connection()
            .await?
            .ok_or_else(|| AgentError::config("no model gateway is configured"))?;
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
        let connection = self
            .connection()
            .await?
            .ok_or_else(|| AgentError::config("no model gateway is configured"))?;
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
    /// drop the synced model snapshot.
    pub(crate) async fn sign_out(&self) -> Result<()> {
        // Take the state lock for the whole operation and invalidate any
        // pending browser flow before revoking anything: an exchange that
        // completes during the revoke round-trip serializes behind this lock
        // and then observes the bump, so it abandons instead of re-saving
        // the session it just minted.
        let mut sign_in = self.sign_in.lock().await;
        self.sign_in_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(connection) = self.connection().await? {
            connection.sign_out().await?;
        }
        let mut config = providers::read_config(&*self.store, ProviderKind::ModelGateway).await?;
        if !config.models.is_empty() {
            config.models = Vec::new();
            providers::write_config(&*self.store, ProviderKind::ModelGateway, &config).await?;
        }
        *sign_in = SignInProgress::Idle;
        Ok(())
    }

    /// The connection for the configured gateway, or `None` when no base URL
    /// is configured.
    pub(crate) async fn connection(&self) -> Result<Option<Arc<GatewayConnection>>> {
        let config = providers::read_config(&*self.store, ProviderKind::ModelGateway).await?;
        let Some(base_url) = config.base_url else {
            return Ok(None);
        };
        let mut cached = self.cached.lock().await;
        if let Some((url, connection)) = cached.as_ref() {
            if *url == base_url {
                return Ok(Some(connection.clone()));
            }
        }
        let auth_config = GatewayAuthConfig::new(&base_url)?;
        let connection = Arc::new(GatewayConnection::new(
            GatewayAuth::new(auth_config)?,
            CredentialVault::new(self.secrets.clone()),
        ));
        *cached = Some((base_url, connection.clone()));
        Ok(Some(connection))
    }

    /// A router token source, when the provider is configured and a session
    /// for that deployment is stored. `None` keeps the gateway route out of
    /// the router entirely — including when the stored session belongs to a
    /// different gateway than the configured base URL.
    pub(crate) async fn route_token_source(&self) -> Option<Arc<dyn BearerTokenSource>> {
        let connection = self.connection().await.ok().flatten()?;
        connection.stored_credentials().await.ok().flatten()?;
        Some(Arc::new(GatewayTokenSource(connection)))
    }

    /// Fetch the entitled models and persist them as the provider's model set.
    ///
    /// Returns how many models are entitled. The persisted snapshot drives the
    /// picker and model policy; entitlement itself stays live at the gateway,
    /// which refuses a revoked model at inference time regardless of what is
    /// cached here.
    pub(crate) async fn sync_models(&self) -> Result<usize> {
        let connection = self
            .connection()
            .await?
            .ok_or_else(|| AgentError::config("no model gateway is configured"))?;
        // The gateway routes these models over its Anthropic-compatible
        // surface, so sync exactly the set that protocol can serve.
        let models = connection.models(Some("anthropic_messages")).await?;
        let mut config = providers::read_config(&*self.store, ProviderKind::ModelGateway).await?;
        config.models = models
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
        providers::validate_custom_models(&config.models).map_err(|error| {
            AgentError::config(format!("gateway model sync rejected: {error:?}"))
        })?;
        let count = config.models.len();
        providers::write_config(&*self.store, ProviderKind::ModelGateway, &config).await?;
        Ok(count)
    }
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

    async fn serve(gateway: Arc<FakeGateway>) -> std::net::SocketAddr {
        let app = AxumRouter::new()
            .route("/oauth/token", post(token))
            .route("/api/v1/cli/models", get(models))
            .with_state(gateway);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        address
    }

    async fn signed_in_runtime(
        base_url: &str,
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
            "base_url": base_url,
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
        providers::write_config(
            &*store,
            ProviderKind::ModelGateway,
            &providers::ProviderConfig {
                enabled: true,
                base_url: Some(base_url.to_string()),
                vertex_location: None,
                models: Vec::new(),
            },
        )
        .await
        .unwrap();
        (
            GatewayRuntime::new(store.clone(), secrets),
            store,
            directory,
        )
    }

    #[tokio::test]
    async fn syncs_entitled_models_into_the_provider_config() {
        let address = serve(Arc::new(FakeGateway::default())).await;
        let base = format!("http://{address}");
        let (runtime, store, _directory) = signed_in_runtime(&base).await;

        assert_eq!(runtime.sync_models().await.unwrap(), 2);

        let config = providers::read_config(&*store, ProviderKind::ModelGateway)
            .await
            .unwrap();
        assert_eq!(config.models.len(), 2);
        assert_eq!(config.models[0].id, "sample-claude");
        assert_eq!(
            config.models[0].display_name.as_deref(),
            Some("Sample Claude")
        );
        assert_eq!(config.models[0].context_window, 200_000);
        // Absent limits fall back to the conservative custom-model defaults.
        assert_eq!(config.models[1].context_window, 32_768);
        assert_eq!(config.models[1].max_output_tokens, 4_096);

        // The synced snapshot resolves as a model policy under the gateway key.
        let policy =
            providers::resolve_model_policy(&*store, "model_gateway::sample-claude", false)
                .await
                .unwrap()
                .expect("synced model resolves");
        assert_eq!(policy.provider, ProviderKind::ModelGateway);
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
        runtime.sync_models().await.unwrap();
        let routes = providers::collect_routes(
            &*store,
            &*runtime.secrets,
            runtime.route_token_source().await,
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

        // Managed policy is a separate layer with a separate lifecycle:
        // disconnecting the session must never deprovision the profile.
        crate::managed_policy::provision(&*store, &base)
            .await
            .unwrap();

        runtime.sign_out().await.unwrap();
        let status = runtime.status().await.unwrap();
        assert!(!status.signed_in);
        let policy = crate::managed_policy::resolve(&*store, &crate::managed_policy::NoOsPolicy)
            .await
            .unwrap();
        assert!(policy.managed);
        assert_eq!(status.model_count, 0);
        assert!(status.account_hint.is_none());
        let config = providers::read_config(&*store, ProviderKind::ModelGateway)
            .await
            .unwrap();
        assert!(config.models.is_empty());
    }

    #[tokio::test]
    async fn a_session_for_a_different_gateway_reads_signed_out() {
        let address = serve(Arc::new(FakeGateway::default())).await;
        let base = format!("http://{address}");
        let (runtime, store, _directory) = signed_in_runtime(&base).await;

        // Repoint the provider at a different deployment; the stored session
        // (minted against `base`) stays in the vault untouched.
        let mut config = providers::read_config(&*store, ProviderKind::ModelGateway)
            .await
            .unwrap();
        config.base_url = Some("http://127.0.0.1:9".to_string());
        providers::write_config(&*store, ProviderKind::ModelGateway, &config)
            .await
            .unwrap();

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
        providers::write_config(
            &*store,
            ProviderKind::ModelGateway,
            &providers::ProviderConfig {
                enabled: true,
                base_url: Some("http://127.0.0.1:1".to_string()),
                vertex_location: None,
                models: Vec::new(),
            },
        )
        .await
        .unwrap();
        let runtime = GatewayRuntime::new(store.clone(), secrets);

        assert!(runtime.route_token_source().await.is_none());
        let routes = providers::collect_routes(&*store, &*runtime.secrets, None).await;
        assert!(routes
            .iter()
            .all(|route| route.kind != openwave_router::RouteKind::ModelGateway));
    }
}
