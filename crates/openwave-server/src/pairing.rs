//! Deep-link pairing: validate a gateway, then provision this profile.
//!
//! The desktop shell's `openwave://provision` handler lands here. The gateway
//! is probed (`GET /api/v1/meta`) before anything is written, so a mistyped
//! or unreachable URL never becomes durable policy; only then does the
//! managed-policy write path run and the ModelGateway provider config get
//! pointed at the gateway. This function is exported for native embedders
//! only — pairing changes policy, and policy must never be reachable from a
//! renderer-writable route.

use std::sync::Arc;

use openwave_connectors::{GatewayAuth, GatewayAuthConfig};
use openwave_core::{AgentError, Result, Store};
use tokio::sync::Mutex;

use crate::managed_policy;
use crate::mcp_config::McpRuntime;
use crate::providers::{self, ProviderKind};

/// Serializes pairing end to end. [`managed_policy::provision`] is a
/// check-then-write over the settings store, and the [`Store`] API offers no
/// cross-call transaction to make it atomic; two links decoded concurrently
/// could otherwise both pass the conflict check and the second would
/// overwrite the first. One desktop process owns the store (the server's
/// instance lock guarantees it), so a process-local mutex is enough to make
/// the check-then-write effectively atomic.
static PAIRING: Mutex<()> = Mutex::const_new(());

/// The process-local handles pairing needs.
///
/// Pairing writes policy, and policy has live effects — the manual MCP
/// servers this process may already be running are locked the moment it
/// commits. The shell's deep-link task therefore carries this rather than a
/// bare store, so it cannot pair without also being able to apply what
/// pairing decided. Obtained from
/// [`Server::pairing_handle`](crate::Server::pairing_handle).
#[derive(Clone)]
pub struct PairingHandle {
    store: Arc<dyn Store>,
    mcp: Arc<McpRuntime>,
}

impl PairingHandle {
    pub(crate) fn new(store: Arc<dyn Store>, mcp: Arc<McpRuntime>) -> Self {
        Self { store, mcp }
    }

    /// The durable store this profile pairs against.
    pub fn store(&self) -> Arc<dyn Store> {
        self.store.clone()
    }
}

/// What a successful pairing did, beyond the writes themselves.
#[derive(Debug)]
pub struct PairingOutcome {
    /// The normalized gateway base URL now on record.
    pub base_url: String,
    /// True when this call is the one that provisioned the profile; false
    /// for an idempotent re-pair of the gateway already on record, which
    /// changed no policy. The desktop shell keys its restart prompt off
    /// this, and the claim is one-directional: enforcement that only a
    /// fresh boot can apply (the boot-scoped embedder) is outstanding only
    /// if this call did the provisioning. A profile already managed by OS
    /// policy was gated at boot and first-pairs with nothing outstanding,
    /// and a deferred restart stays outstanding through later re-pairs
    /// that report false.
    pub newly_provisioned: bool,
}

/// Why a pairing did not provision this profile.
///
/// The conflict is its own variant because it is a product refusal, not a
/// fault: the recorded decision is refuse-forever (gateway migration is the
/// MDM tier's job — OS-asserted policy already outranks provisioned state —
/// or a profile reset), and the shell owes the user an explanation naming
/// the gateway that actually manages this device rather than a log line.
/// Every other failure — invalid URL, unreachable gateway, bad manifest,
/// store errors — stays the [`AgentError`] it was.
#[derive(Debug)]
#[non_exhaustive]
pub enum PairingError {
    /// The link named a different gateway than the one this profile is
    /// provisioned to. Carries the provisioned base URL — normalized and
    /// secret-free by the gateway-URL contract — so callers can name it;
    /// user-facing surfaces should reduce it to the origin.
    Conflict {
        /// The normalized base URL this profile is provisioned to.
        provisioned_url: String,
    },
    /// Any other failure. No policy was written — though the provider
    /// configuration may already be repointed: it is deliberately written
    /// first, so a failure between the writes fails into a still-unmanaged
    /// profile recoverable from settings (see the ordering comment in
    /// [`pair_with_gateway`]).
    Other(AgentError),
}

impl std::fmt::Display for PairingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict { .. } => write!(
                f,
                "this profile is already provisioned to a different gateway"
            ),
            Self::Other(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for PairingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Conflict { .. } => None,
            Self::Other(error) => Some(error),
        }
    }
}

impl From<AgentError> for PairingError {
    fn from(error: AgentError) -> Self {
        Self::Other(error)
    }
}

/// Validate `gateway_url`, probe the gateway, and provision this profile.
///
/// Returns a [`PairingOutcome`] on success — the normalized gateway base URL
/// plus whether this call is the one that provisioned the profile. Nothing
/// is written unless the URL passes the gateway contract and the deployment
/// answers `GET /api/v1/meta`; a conflicting re-provision is refused as the
/// typed [`PairingError::Conflict`] before either write, leaving both the
/// policy and the provider configuration untouched — the desktop shell
/// surfaces that refusal instead of treating it as a generic fault. Success
/// also points the ModelGateway provider at
/// the gateway and enables it, dropping any model snapshot synced from a
/// previously configured base URL — sign-in resyncs the entitled set.
///
/// The new policy is applied to this process before returning: manual MCP
/// servers running under the previously open profile are taken down here
/// rather than at the supervisor's next sweep, so no locked child keeps
/// serving tools across the window in between.
pub async fn pair_with_gateway(
    handle: &PairingHandle,
    gateway_url: &str,
) -> Result<PairingOutcome, PairingError> {
    let store = &*handle.store;
    let config = GatewayAuthConfig::new(gateway_url)?;
    let base_url = config.base_url().to_string();
    GatewayAuth::new(config)?.meta().await?;
    let _guard = PAIRING.lock().await;
    // Refuse a conflicting pairing before either write, then write the
    // provider configuration before the sticky policy. Neither write is
    // transactional with the other, so the order decides the failure mode:
    // provider-first fails into a still-unmanaged profile recoverable from
    // settings, while policy-first could strand a permanently managed
    // profile with no configured provider.
    let already_provisioned = match managed_policy::provisioned_url(store).await? {
        Some(existing) if existing != base_url => {
            return Err(PairingError::Conflict {
                provisioned_url: existing,
            });
        }
        other => other.is_some(),
    };
    let mut provider = providers::read_config(store, ProviderKind::ModelGateway).await?;
    if provider.base_url.as_deref() != Some(base_url.as_str()) {
        // A model snapshot synced from a previously configured gateway does
        // not describe this one.
        provider.models = Vec::new();
        provider.base_url = Some(base_url.clone());
    }
    provider.enabled = true;
    providers::write_config(store, ProviderKind::ModelGateway, &provider).await?;
    managed_policy::provision(store, &base_url).await?;
    handle.mcp.enforce_manual_lockdown().await;
    Ok(PairingOutcome {
        base_url,
        newly_provisioned: !already_provisioned,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::routing::get;
    use axum::Json;
    use openwave_core::DbStore;
    use serde_json::json;

    use openwave_core::ToolRegistry;

    use super::*;
    use crate::managed_policy::{resolve, NoOsPolicy};
    use crate::mcp_config::{
        GatewayEndpointAccess, GatewayEndpoints, McpHealth, McpServerDefinition, McpServersConfig,
        MANAGED_DISABLED_DIAGNOSTIC,
    };

    /// The signed-out stand-in: pairing never resolves an endpoint.
    struct NoGateway;

    #[async_trait::async_trait]
    impl GatewayEndpoints for NoGateway {
        async fn endpoint(&self, _slug: &str) -> Result<GatewayEndpointAccess> {
            Err(AgentError::config("no gateway session in tests"))
        }
    }

    /// The handle plus the runtime behind it, for the assertions that are
    /// about what pairing *did* to this process rather than what it wrote.
    fn test_handle_with_runtime(store: &Arc<dyn Store>) -> (PairingHandle, Arc<McpRuntime>) {
        let mcp = Arc::new(McpRuntime::new(
            Arc::new(ToolRegistry::new()),
            store.clone(),
            Arc::new(NoGateway),
            Arc::new(NoOsPolicy),
        ));
        (PairingHandle::new(store.clone(), mcp.clone()), mcp)
    }

    fn test_handle(store: &Arc<dyn Store>) -> PairingHandle {
        test_handle_with_runtime(store).0
    }

    /// A minimal Streamable HTTP MCP server, standing in for a manual server
    /// the profile was already running when the provision link arrived.
    async fn serve_manual_mcp() -> String {
        async fn handler(body: String) -> ([(&'static str, &'static str); 1], String) {
            let request: serde_json::Value = serde_json::from_str(&body).unwrap();
            let id = request.get("id").cloned().unwrap_or_default();
            let result = match request["method"].as_str().unwrap_or_default() {
                "initialize" => json!({
                    "protocolVersion": openwave_mcp::PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "pairing-fixture", "version": "1"}
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
            (
                [("content-type", "application/json")],
                json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
            )
        }

        let app = axum::Router::new().route("/mcp", axum::routing::post(handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}/mcp")
    }

    async fn test_store() -> (Arc<dyn Store>, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("pairing.db").display()
            ))
            .await
            .unwrap(),
        );
        (store, directory)
    }

    /// An address nothing listens on: bound to reserve an ephemeral port,
    /// then dropped, so the probe fails fast and deterministically.
    async fn unreachable_base() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{address}")
    }

    /// A gateway that answers only the unauthenticated identity probe.
    async fn serve_meta() -> String {
        let app = axum::Router::new().route(
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
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn pairing_provisions_policy_and_points_the_provider() {
        let (store, _directory) = test_store().await;
        let base = serve_meta().await;

        // A stale snapshot from a manually configured endpoint must not
        // survive as this gateway's model set.
        providers::write_config(
            &*store,
            ProviderKind::ModelGateway,
            &providers::ProviderConfig {
                enabled: false,
                base_url: Some("http://old.gateway.test".into()),
                vertex_location: None,
                models: vec![providers::CustomModelConfig {
                    id: "stale-model".into(),
                    display_name: None,
                    context_window: 32_768,
                    max_output_tokens: 4_096,
                }],
            },
        )
        .await
        .unwrap();

        let outcome = pair_with_gateway(&test_handle(&store), &base)
            .await
            .unwrap();
        assert!(
            outcome.newly_provisioned,
            "the first pairing is the one that provisions"
        );
        let normalized = outcome.base_url;
        assert_eq!(normalized, format!("{base}/"));

        let policy = resolve(&*store, &NoOsPolicy).await.unwrap();
        assert!(policy.managed);
        assert_eq!(policy.gateway_url.as_deref(), Some(normalized.as_str()));

        let provider = providers::read_config(&*store, ProviderKind::ModelGateway)
            .await
            .unwrap();
        assert!(provider.enabled);
        assert_eq!(provider.base_url.as_deref(), Some(normalized.as_str()));
        assert!(provider.models.is_empty());

        // Re-pairing the same gateway is idempotent, and reports that no
        // transition happened — the desktop shell keys its restart prompt
        // off this distinction.
        let repaired = pair_with_gateway(&test_handle(&store), &base)
            .await
            .unwrap();
        assert!(!repaired.newly_provisioned);
    }

    /// Pairing applies the policy it writes, not just persists it: a manual
    /// MCP server this process was already running when the link arrived must
    /// stop serving tools before `pair_with_gateway` returns, rather than at
    /// the supervisor's next sweep. Dropping the enforcement call fails here.
    #[tokio::test]
    async fn pairing_takes_down_a_manual_mcp_server_this_process_is_running() {
        let (store, _directory) = test_store().await;
        let (handle, mcp) = test_handle_with_runtime(&store);

        // A real connection, established while the profile was still open.
        mcp.replace(McpServersConfig {
            servers: vec![McpServerDefinition {
                name: "private_docs".to_string(),
                command: None,
                args: Vec::new(),
                env: std::collections::BTreeMap::new(),
                env_from: Vec::new(),
                cwd: None,
                url: Some(serve_manual_mcp().await),
                bearer_token_env: None,
                gateway_endpoint: None,
                request_timeout_ms: 60_000,
                enabled: true,
            }],
        })
        .await
        .unwrap();
        assert_eq!(mcp.info().await.servers[0].health, McpHealth::Healthy);
        assert!(mcp.snapshot().get("mcp__private_docs__lookup").is_some());

        pair_with_gateway(&handle, &serve_meta().await)
            .await
            .unwrap();

        assert!(
            mcp.snapshot().get("mcp__private_docs__lookup").is_none(),
            "pairing must stop the server serving tools, not only record policy"
        );
        let info = mcp.info().await;
        assert_eq!(info.servers[0].health, McpHealth::Disabled);
        assert_eq!(
            info.servers[0].diagnostic.as_deref(),
            Some(MANAGED_DISABLED_DIAGNOSTIC)
        );
    }

    #[tokio::test]
    async fn a_failed_pairing_changes_nothing() {
        let (store, _directory) = test_store().await;

        // Unreachable gateway: the probe fails before any write, and the
        // failure is a fault, not the typed conflict refusal.
        let dead = unreachable_base().await;
        let error = pair_with_gateway(&test_handle(&store), &dead)
            .await
            .err()
            .unwrap();
        assert!(matches!(error, PairingError::Other(_)));
        assert!(!resolve(&*store, &NoOsPolicy).await.unwrap().managed);
        let provider = providers::read_config(&*store, ProviderKind::ModelGateway)
            .await
            .unwrap();
        assert!(!provider.enabled);
        assert!(provider.base_url.is_none());

        // A reachable but conflicting gateway: refused after the probe, and
        // the original pairing survives in both policy and provider config.
        let first = serve_meta().await;
        let second = serve_meta().await;
        let normalized = pair_with_gateway(&test_handle(&store), &first)
            .await
            .unwrap()
            .base_url;
        // The refusal is typed — the desktop dialog keys off the variant,
        // not the message — and names the gateway actually on record.
        let error = pair_with_gateway(&test_handle(&store), &second)
            .await
            .err()
            .unwrap();
        match &error {
            PairingError::Conflict { provisioned_url } => {
                assert_eq!(provisioned_url, &normalized)
            }
            other => panic!("expected the typed conflict, got {other:?}"),
        }
        assert!(error.to_string().contains("already provisioned"));
        let policy = resolve(&*store, &NoOsPolicy).await.unwrap();
        assert_eq!(policy.gateway_url.as_deref(), Some(normalized.as_str()));
        let provider = providers::read_config(&*store, ProviderKind::ModelGateway)
            .await
            .unwrap();
        assert_eq!(provider.base_url.as_deref(), Some(normalized.as_str()));
    }
}
