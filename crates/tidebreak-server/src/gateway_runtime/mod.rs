//! Server adapters for the extracted model-gateway runtime.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use tidebreak_core::{AgentError, OwnerId, Result, SecretProvider, Store};
use tidebreak_gateway_runtime as runtime;
use tidebreak_router::BearerTokenSource;

use crate::managed_policy::{OsPolicySource, ProvisionedPolicySource};
use crate::providers;

pub(crate) use runtime::{GatewayApps, GatewayMachineOffer, GatewayStatus};
#[cfg(test)]
pub(crate) use runtime::{SignInProgress, GATEWAY_STATE_WRITES};

#[cfg(test)]
use crate::providers::CustomModelConfig;
#[cfg(test)]
pub(super) use runtime::{
    CredentialVault, GatewayAuth, GatewayAuthConfig, GatewayConnection,
    SyncCommitPause as MigrationPause,
};
#[cfg(test)]
use std::time::Duration;

#[cfg(test)]
mod tests;

struct PolicyAdapter {
    provisioned: Arc<dyn ProvisionedPolicySource>,
    os: Arc<dyn OsPolicySource>,
}

impl runtime::GatewayPolicySource for PolicyAdapter {
    fn resolve(&self) -> Result<runtime::GatewayPolicy> {
        let policy = crate::managed_policy::resolve(&*self.provisioned, &*self.os)?;
        Ok(runtime::GatewayPolicy {
            managed: policy.managed,
            gateway_url: policy.gateway_url,
        })
    }
}

struct ModelStateAdapter {
    store: Arc<dyn Store>,
}

impl ModelStateAdapter {
    fn model(model: &runtime::SyncedGatewayModel) -> providers::CustomModelConfig {
        providers::CustomModelConfig {
            id: model.id.clone(),
            display_name: model.display_name.clone(),
            upstream_id: model.upstream_id.clone(),
            aliases: model.aliases.clone(),
            context_window: model.context_window,
            max_output_tokens: model.max_output_tokens,
            input_modalities: vec![crate::model_registry::InputModality::Text],
            supports_reasoning: false,
            reasoning_efforts: Vec::new(),
        }
    }

    fn protocol(protocol: runtime::GatewayModelProtocol) -> providers::GatewayModelProtocol {
        match protocol {
            runtime::GatewayModelProtocol::AnthropicMessages => {
                providers::GatewayModelProtocol::AnthropicMessages
            }
            runtime::GatewayModelProtocol::OpenaiResponses => {
                providers::GatewayModelProtocol::OpenaiResponses
            }
        }
    }

    fn snapshot(snapshot: &runtime::GatewayModelSnapshot) -> providers::GatewayModelSnapshot {
        providers::GatewayModelSnapshot {
            gateway_url: snapshot.gateway_url.clone(),
            installation_id: snapshot.installation_id.clone(),
            models: snapshot.models.iter().map(Self::model).collect(),
            model_protocols: snapshot
                .model_protocols
                .iter()
                .map(|(id, protocol)| (id.clone(), Self::protocol(*protocol)))
                .collect(),
            model_reasoning_efforts: snapshot.model_reasoning_efforts.clone(),
            member_catalog: snapshot.member_catalog.clone(),
            catalog_etag: snapshot.catalog_etag.clone(),
        }
    }

    fn runtime_protocol(
        protocol: providers::GatewayModelProtocol,
    ) -> runtime::GatewayModelProtocol {
        match protocol {
            providers::GatewayModelProtocol::AnthropicMessages => {
                runtime::GatewayModelProtocol::AnthropicMessages
            }
            providers::GatewayModelProtocol::OpenaiResponses => {
                runtime::GatewayModelProtocol::OpenaiResponses
            }
        }
    }

    fn runtime_snapshot(
        snapshot: providers::GatewayModelSnapshot,
    ) -> runtime::GatewayModelSnapshot {
        runtime::GatewayModelSnapshot {
            gateway_url: snapshot.gateway_url,
            installation_id: snapshot.installation_id,
            models: snapshot
                .models
                .into_iter()
                .map(|model| runtime::SyncedGatewayModel {
                    id: model.id,
                    display_name: model.display_name,
                    upstream_id: model.upstream_id,
                    aliases: model.aliases,
                    context_window: model.context_window,
                    max_output_tokens: model.max_output_tokens,
                })
                .collect(),
            model_protocols: snapshot
                .model_protocols
                .into_iter()
                .map(|(id, protocol)| (id, Self::runtime_protocol(protocol)))
                .collect(),
            model_reasoning_efforts: snapshot.model_reasoning_efforts,
            member_catalog: snapshot.member_catalog,
            catalog_etag: snapshot.catalog_etag,
        }
    }
}

#[async_trait]
impl runtime::GatewayModelState for ModelStateAdapter {
    async fn snapshot(
        &self,
        policy: &runtime::GatewayPolicy,
    ) -> Result<Option<runtime::GatewayModelSnapshot>> {
        let Some(gateway_url) = policy.gateway_url.as_deref().filter(|_| policy.managed) else {
            return Ok(None);
        };
        Ok(providers::read_gateway_snapshot(&*self.store)
            .await?
            .filter(|snapshot| snapshot.gateway_url == gateway_url)
            .map(Self::runtime_snapshot))
    }

    async fn resolve_route(
        &self,
        snapshot: &runtime::GatewayModelSnapshot,
        route_model: &str,
    ) -> Result<Option<runtime::GatewayRoute>> {
        let snapshot = Self::snapshot(snapshot);
        let selection = crate::model_registry::selection_key(
            providers::ProviderKind::ModelGateway,
            route_model,
        );
        Ok(
            providers::gateway_execution_policy(&snapshot, &selection).map(|resolved| {
                runtime::GatewayRoute {
                    id: resolved.id,
                    route_model: resolved.route_model,
                    request_shaping_model: resolved.request_shaping_model,
                }
            }),
        )
    }

    async fn write_snapshot(&self, snapshot: &runtime::GatewayModelSnapshot) -> Result<()> {
        let snapshot = Self::snapshot(snapshot);
        providers::validate_custom_models(&snapshot.models).map_err(|error| {
            AgentError::config(format!("gateway model sync rejected: {error:?}"))
        })?;
        providers::write_gateway_snapshot(&*self.store, &snapshot).await
    }
}

struct AppUsageAdapter {
    store: Arc<dyn Store>,
}

#[async_trait]
impl runtime::GatewayAppUsageSource for AppUsageAdapter {
    async fn used_by_app_counts(&self, owner: &OwnerId) -> Result<BTreeMap<String, usize>> {
        let mut counts = BTreeMap::new();
        for grant in self.store.list_live_app_grants_scoped(owner).await? {
            for binding in &grant.bindings {
                if let Some(gateway_app) = binding.gateway_app() {
                    *counts.entry(gateway_app.to_owned()).or_default() += 1;
                }
            }
        }
        Ok(counts)
    }
}

struct McpControlAdapter(Arc<crate::mcp_config::McpRuntime>);

#[async_trait]
impl runtime::GatewayMcpControl for McpControlAdapter {
    async fn auto_mount_gateway_endpoints(&self, entitled: &[String]) -> Result<bool> {
        self.0.auto_mount_gateway_endpoints(entitled).await
    }

    async fn refresh_connected_app_roster(&self) {
        self.0.refresh_connected_app_roster().await;
    }
}

struct PairingCommitAdapter {
    provisioned: Arc<dyn ProvisionedPolicySource>,
    os: Arc<dyn OsPolicySource>,
    secrets: Arc<dyn SecretProvider>,
    mcp: Arc<crate::mcp_config::McpRuntime>,
}

#[async_trait]
impl runtime::GatewayPairingCommit for PairingCommitAdapter {
    async fn commit(&self, base_url: &str, replaces: Option<&str>) -> Result<()> {
        crate::pairing::commit_signed_in_pairing_locked(
            &*self.provisioned,
            &*self.os,
            self.secrets.clone(),
            &self.mcp,
            base_url,
            replaces,
        )
        .await
    }
}

pub(crate) struct GatewayRuntime {
    inner: Arc<runtime::GatewayRuntime>,
    store: Arc<dyn Store>,
    pub(super) secrets: Arc<dyn SecretProvider>,
    provisioned_policy: Arc<dyn ProvisionedPolicySource>,
    os_policy: Arc<dyn OsPolicySource>,
    #[cfg(test)]
    pub(super) sync_commit_pause: tokio::sync::Mutex<Option<Arc<MigrationPause>>>,
}

impl GatewayRuntime {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        secrets: Arc<dyn SecretProvider>,
        provisioned_policy: Arc<dyn ProvisionedPolicySource>,
        os_policy: Arc<dyn OsPolicySource>,
    ) -> Arc<Self> {
        let policy = Arc::new(PolicyAdapter {
            provisioned: provisioned_policy.clone(),
            os: os_policy.clone(),
        });
        let models = Arc::new(ModelStateAdapter {
            store: store.clone(),
        });
        let usage = Arc::new(AppUsageAdapter {
            store: store.clone(),
        });
        Arc::new(Self {
            inner: runtime::GatewayRuntime::new(secrets.clone(), policy, models, usage),
            store,
            secrets,
            provisioned_policy,
            os_policy,
            #[cfg(test)]
            sync_commit_pause: tokio::sync::Mutex::new(None),
        })
    }

    pub(crate) fn provisioned_policy(&self) -> &Arc<dyn ProvisionedPolicySource> {
        &self.provisioned_policy
    }

    pub(crate) fn policy(&self) -> Result<crate::managed_policy::ManagedPolicy> {
        crate::managed_policy::resolve(&*self.provisioned_policy, &*self.os_policy)
    }

    fn mcp_adapter(mcp: Arc<crate::mcp_config::McpRuntime>) -> Arc<dyn runtime::GatewayMcpControl> {
        Arc::new(McpControlAdapter(mcp))
    }

    fn pairing_adapter(
        &self,
        mcp: Arc<crate::mcp_config::McpRuntime>,
    ) -> Arc<dyn runtime::GatewayPairingCommit> {
        Arc::new(PairingCommitAdapter {
            provisioned: self.provisioned_policy.clone(),
            os: self.os_policy.clone(),
            secrets: self.secrets.clone(),
            mcp,
        })
    }

    #[cfg(test)]
    async fn install_sync_pause(&self) {
        if let Some(pause) = self.sync_commit_pause.lock().await.take() {
            self.inner.pause_next_sync_commit(pause).await;
        }
    }

    #[cfg(not(test))]
    async fn install_sync_pause(&self) {}

    pub(crate) async fn register_pending_pairing(
        &self,
        base_url: String,
        mcp: Arc<crate::mcp_config::McpRuntime>,
        replaces: Option<String>,
    ) {
        self.inner
            .register_pending_pairing(
                base_url,
                Self::mcp_adapter(mcp.clone()),
                self.pairing_adapter(mcp),
                replaces,
            )
            .await;
    }

    pub(crate) async fn pending_pairing_url(&self) -> Option<String> {
        self.inner.pending_pairing_url().await
    }

    pub(crate) async fn dismiss_pending_pairing(&self) {
        self.inner.dismiss_pending_pairing().await;
    }

    pub(crate) async fn status(&self) -> Result<GatewayStatus> {
        self.inner.status().await
    }

    pub(crate) async fn offered_machine(&self) -> GatewayMachineOffer {
        self.inner.offered_machine().await
    }

    pub(crate) async fn begin_sign_in(
        self: &Arc<Self>,
        mcp: Arc<crate::mcp_config::McpRuntime>,
    ) -> Result<String> {
        self.inner.begin_sign_in(Self::mcp_adapter(mcp)).await
    }

    pub(crate) async fn lock_model_authority_mutation(
        &self,
    ) -> tokio::sync::OwnedRwLockWriteGuard<()> {
        self.inner.lock_model_authority_mutation().await
    }

    #[cfg(test)]
    pub(crate) async fn commit_signed_in_pairing_for_test(
        &self,
        mcp: &Arc<crate::mcp_config::McpRuntime>,
        base_url: &str,
        replaces: Option<&str>,
    ) -> Result<()> {
        self.inner
            .commit_signed_in_pairing_for_test(
                self.pairing_adapter(mcp.clone()),
                base_url,
                replaces,
            )
            .await
    }

    pub(crate) async fn sign_out(&self) -> Result<()> {
        self.inner.sign_out().await
    }

    pub(crate) async fn abandon_sign_in_and_pairing(&self) {
        self.inner.abandon_sign_in_and_pairing().await;
    }

    pub(crate) async fn retire_session_for_current_policy(
        &self,
        authority: &tokio::sync::OwnedRwLockWriteGuard<()>,
    ) -> Result<()> {
        self.inner
            .retire_session_for_current_policy(authority)
            .await
    }

    pub(crate) async fn connection(&self) -> Result<Option<Arc<runtime::GatewayConnection>>> {
        self.inner.connection().await
    }

    pub(crate) async fn model_snapshot(&self) -> Result<Option<providers::GatewayModelSnapshot>> {
        providers::gateway_snapshot_for_policy(&*self.store, &self.policy()?).await
    }

    pub(crate) async fn route_token_source(self: &Arc<Self>) -> Option<Arc<dyn BearerTokenSource>> {
        self.inner.route_token_source().await
    }

    pub(crate) async fn sync_models(
        &self,
    ) -> std::result::Result<usize, crate::error::ServerError> {
        self.install_sync_pause().await;
        self.inner.sync_models().await.map_err(map_sync_error)
    }

    #[cfg(test)]
    pub(crate) async fn sync_models_if_connected(
        &self,
    ) -> std::result::Result<Option<usize>, crate::error::ServerError> {
        self.install_sync_pause().await;
        self.inner
            .sync_models_if_connected()
            .await
            .map_err(map_sync_error)
    }

    pub(crate) async fn sync_models_periodically(
        self: Arc<Self>,
        mcp: Arc<crate::mcp_config::McpRuntime>,
    ) {
        self.inner
            .clone()
            .sync_models_periodically(Self::mcp_adapter(mcp))
            .await;
    }

    pub(crate) async fn apps(&self, owner: &OwnerId) -> Result<GatewayApps> {
        self.inner.apps(owner).await
    }

    pub(crate) async fn reconcile_endpoint_mounts(
        &self,
        mcp: &Arc<crate::mcp_config::McpRuntime>,
    ) -> Result<()> {
        self.inner
            .reconcile_endpoint_mounts(&*Self::mcp_adapter(mcp.clone()))
            .await
    }

    pub(crate) async fn app_roster(&self) -> Option<Vec<runtime::GatewayRosterApp>> {
        self.inner.app_roster().await
    }
}

fn map_sync_error(error: runtime::GatewaySyncError) -> crate::error::ServerError {
    match error {
        runtime::GatewaySyncError::Agent(error) => error.into(),
        runtime::GatewaySyncError::Conflict { kind, message } => {
            crate::error::ServerError::conflict_kind(kind, message)
        }
    }
}

pub(crate) async fn retire_superseded_gateway_session(
    secrets: Arc<dyn SecretProvider>,
    policy: &crate::managed_policy::ManagedPolicy,
) -> Result<()> {
    runtime::retire_superseded_gateway_session(
        secrets,
        &runtime::GatewayPolicy {
            managed: policy.managed,
            gateway_url: policy.gateway_url.clone(),
        },
    )
    .await
}

pub(crate) fn gateway_relay_dispatcher(
    runtime: Arc<GatewayRuntime>,
    drafts: Arc<dyn runtime::GatewayDraftSource>,
) -> Arc<dyn runtime::GatewayInvokeDispatcher> {
    runtime::gateway_relay_dispatcher(runtime.inner.clone(), drafts)
}

#[async_trait]
impl runtime::GatewayEndpoints for GatewayRuntime {
    async fn endpoint(&self, slug: &str) -> Result<runtime::GatewayEndpointAccess> {
        runtime::GatewayEndpoints::endpoint(&*self.inner, slug).await
    }

    async fn call_bearer(&self, slug: &str, chat: tidebreak_core::id::SessionId) -> Result<String> {
        runtime::GatewayEndpoints::call_bearer(&*self.inner, slug, chat).await
    }

    async fn entitled_app_catalogs(&self) -> Vec<runtime::GatewayRosterApp> {
        runtime::GatewayEndpoints::entitled_app_catalogs(&*self.inner).await
    }
}

#[async_trait]
impl runtime::GatewayCatalogSource for GatewayRuntime {
    async fn gateway_app_catalogs(
        &self,
        needed: &std::collections::BTreeSet<String>,
    ) -> Option<(
        String,
        std::collections::BTreeMap<String, runtime::GatewayAppCatalog>,
    )> {
        runtime::GatewayCatalogSource::gateway_app_catalogs(&*self.inner, needed).await
    }
}

#[cfg(test)]
pub(super) mod relay {
    pub(super) use tidebreak_gateway_runtime::shared_app_invoke_body;
}
