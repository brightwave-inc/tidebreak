//! Deep-link pairing: validate a gateway, then provision this profile.
//!
//! The desktop shell's `openwave://provision` handler lands here. The gateway
//! is probed (`GET /api/v1/meta`) before anything is written, so a mistyped
//! or unreachable URL never becomes durable policy; only then does the
//! managed-policy write path run and the ModelGateway provider config get
//! pointed at the gateway. This function is exported for native embedders
//! only — pairing changes policy, and policy must never be reachable from a
//! renderer-writable route.

use openwave_connectors::{GatewayAuth, GatewayAuthConfig};
use openwave_core::{AgentError, Result, Store};
use tokio::sync::Mutex;

use crate::managed_policy;
use crate::providers::{self, ProviderKind};

/// Serializes pairing end to end. [`managed_policy::provision`] is a
/// check-then-write over the settings store, and the [`Store`] API offers no
/// cross-call transaction to make it atomic; two links decoded concurrently
/// could otherwise both pass the conflict check and the second would
/// overwrite the first. One desktop process owns the store (the server's
/// instance lock guarantees it), so a process-local mutex is enough to make
/// the check-then-write effectively atomic.
static PAIRING: Mutex<()> = Mutex::const_new(());

/// Validate `gateway_url`, probe the gateway, and provision this profile.
///
/// Returns the normalized gateway base URL on success. Nothing is written
/// unless the URL passes the gateway contract and the deployment answers
/// `GET /api/v1/meta`; a conflicting re-provision is refused inside
/// [`managed_policy::provision`] and leaves both the policy and the provider
/// configuration untouched. Success also points the ModelGateway provider at
/// the gateway and enables it, dropping any model snapshot synced from a
/// previously configured base URL — sign-in resyncs the entitled set.
pub async fn pair_with_gateway(store: &dyn Store, gateway_url: &str) -> Result<String> {
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
    if let Some(existing) = managed_policy::provisioned_url(store).await? {
        if existing != base_url {
            return Err(AgentError::config(
                "this profile is already provisioned to a different gateway",
            ));
        }
    }
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
    Ok(base_url)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::routing::get;
    use axum::Json;
    use openwave_core::DbStore;
    use serde_json::json;

    use super::*;
    use crate::managed_policy::{resolve, NoOsPolicy};

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

        let normalized = pair_with_gateway(&*store, &base).await.unwrap();
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

        // Re-pairing the same gateway is idempotent.
        pair_with_gateway(&*store, &base).await.unwrap();
    }

    #[tokio::test]
    async fn a_failed_pairing_changes_nothing() {
        let (store, _directory) = test_store().await;

        // Unreachable gateway: the probe fails before any write.
        let dead = unreachable_base().await;
        assert!(pair_with_gateway(&*store, &dead).await.is_err());
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
        let normalized = pair_with_gateway(&*store, &first).await.unwrap();
        let error = pair_with_gateway(&*store, &second).await.err().unwrap();
        assert!(error.to_string().contains("already provisioned"));
        let policy = resolve(&*store, &NoOsPolicy).await.unwrap();
        assert_eq!(policy.gateway_url.as_deref(), Some(normalized.as_str()));
        let provider = providers::read_config(&*store, ProviderKind::ModelGateway)
            .await
            .unwrap();
        assert_eq!(provider.base_url.as_deref(), Some(normalized.as_str()));
    }
}
