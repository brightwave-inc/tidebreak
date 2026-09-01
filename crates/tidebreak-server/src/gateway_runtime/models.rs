//! The model snapshot, entitled-catalog sync, and the per-request route token source.

use super::*;

/// Router-facing supplier of the gateway's `llm`-resource token. Refresh and
/// rotation live inside [`GatewayConnection`]; this is just the seam.
pub(super) struct GatewayTokenSource {
    pub(super) connection: Arc<GatewayConnection>,
    pub(super) installation_id: String,
    pub(super) store: Arc<dyn Store>,
    pub(super) provisioned_policy: Arc<dyn crate::managed_policy::ProvisionedPolicySource>,
    pub(super) os_policy: Arc<dyn crate::managed_policy::OsPolicySource>,
    pub(super) model_sync: Arc<RwLock<()>>,
}

impl GatewayTokenSource {
    /// Resolve one frozen selector against live policy/session/catalog state.
    /// Callers decide which authority guard they retain; this helper never
    /// acquires `model_sync`, so post-mint validation cannot recursively take
    /// a fair read lock while a writer waits.
    pub(super) async fn resolve_model_route(
        &self,
        route_model: &str,
    ) -> Result<Option<providers::ResolvedModelPolicy>> {
        let policy = crate::managed_policy::resolve(&*self.provisioned_policy, &*self.os_policy)?;
        let Some(snapshot) = providers::gateway_snapshot_for_policy(&*self.store, &policy).await?
        else {
            return Ok(None);
        };
        if snapshot.installation_id.as_deref() != Some(&self.installation_id) {
            return Ok(None);
        }
        let Some(credentials) = self.connection.stored_credentials().await? else {
            return Ok(None);
        };
        if credentials.installation_id != self.installation_id {
            return Ok(None);
        }
        let selection = crate::model_registry::selection_key(
            providers::ProviderKind::ModelGateway,
            route_model,
        );
        let Some(resolved) = providers::gateway_execution_policy(&snapshot, &selection) else {
            return Ok(None);
        };
        Ok((resolved.route_model == route_model).then_some(resolved))
    }

    pub(super) fn route_lease(
        resolved: providers::ResolvedModelPolicy,
        guard: tokio::sync::OwnedRwLockReadGuard<()>,
    ) -> ModelRouteLease {
        ModelRouteLease::with_request_shaping_model(
            resolved.id,
            resolved.request_shaping_model,
            guard,
        )
    }
}

#[async_trait]
impl BearerTokenSource for GatewayTokenSource {
    fn binding_id(&self) -> Option<&str> {
        Some(&self.installation_id)
    }

    fn requires_model_route_lease(&self) -> bool {
        true
    }

    async fn lease_model_route(&self, route_model: &str) -> Result<Option<ModelRouteLease>> {
        let guard = self.model_sync.clone().read_owned().await;
        Ok(self
            .resolve_model_route(route_model)
            .await?
            .map(|resolved| Self::route_lease(resolved, guard)))
    }

    async fn authorize_model_route(
        &self,
        route_model: &str,
        conversation: Option<tidebreak_core::id::ChatId>,
    ) -> Result<(String, Option<ModelRouteLease>)> {
        let guard = self.model_sync.clone().read_owned().await;
        if self.resolve_model_route(route_model).await?.is_none() {
            return Ok((String::new(), None));
        }
        let token = self.bearer_token_for(conversation).await?;
        // OS/MDM policy does not participate in the process-local lock and may
        // repoint while the network mint is delayed. Re-read every live input
        // after that slow operation, using the guard already held so local
        // sign-out/re-pair/deprovision remains fenced without recursive read
        // acquisition.
        let lease = self
            .resolve_model_route(route_model)
            .await?
            .map(|resolved| Self::route_lease(resolved, guard));
        Ok((token, lease))
    }

    async fn bearer_token(&self) -> Result<String> {
        self.connection
            .access_token_for_installation(RESOURCE_LLM, &self.installation_id)
            .await
    }

    /// A chat's inference rides a token minted inside that chat's
    /// attestation context, so the gateway records its tool calls as
    /// observations and attested MCP endpoints can match them. Requests
    /// with no conversation — titling, judging, other maintenance — keep
    /// the shared token: there is no chat for an observation to serve.
    async fn bearer_token_for(
        &self,
        conversation: Option<tidebreak_core::id::ChatId>,
    ) -> Result<String> {
        match conversation {
            Some(chat) => {
                self.connection
                    .attested_access_token_for_installation(
                        RESOURCE_LLM,
                        &chat.to_string(),
                        &self.installation_id,
                    )
                    .await
            }
            None => self.bearer_token().await,
        }
    }
}

impl GatewayRuntime {
    /// The synced model snapshot for the deployment named by managed policy.
    pub(crate) async fn model_snapshot(&self) -> Result<Option<providers::GatewayModelSnapshot>> {
        let policy = self.policy()?;
        providers::gateway_snapshot_for_policy(&*self.store, &policy).await
    }

    /// A router token source, when policy names a gateway and a session for
    /// that deployment is stored. `None` keeps the gateway route out of the
    /// router entirely — including on unmanaged profiles and when the stored
    /// session belongs to a different gateway than the policy URL.
    pub(crate) async fn route_token_source(self: &Arc<Self>) -> Option<Arc<dyn BearerTokenSource>> {
        let connection = self.connection().await.ok().flatten()?;
        let credentials = connection.stored_credentials().await.ok().flatten()?;
        Some(Arc::new(GatewayTokenSource {
            connection,
            installation_id: credentials.installation_id,
            store: self.store.clone(),
            provisioned_policy: self.provisioned_policy.clone(),
            os_policy: self.os_policy.clone(),
            model_sync: self.model_sync.clone(),
        }))
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
        // The lock covers the fetch, not merely the write tail. Otherwise two
        // callers can start from the same ETag and commit in response order,
        // allowing an older response to replace a newer catalog.
        let _sync = self.model_sync.write().await;
        let policy = crate::managed_policy::resolve(&*self.provisioned_policy, &*self.os_policy)?;
        let base_url = require_managed(&policy)?;
        let connection = self.connection_at(base_url.clone()).await?;
        // Populate the control-token cache before taking the credential
        // fingerprint. The catalog call then reuses that token instead of
        // rotating this session's refresh token after we captured it.
        connection.access_token(RESOURCE_CONTROL).await?;
        let session = connection.stored_credentials().await?.ok_or_else(|| {
            crate::error::ServerError::conflict_kind(
                "gateway_changed",
                "the model gateway session changed during model sync",
            )
        })?;
        // The stored snapshot's ETag makes an unchanged catalog a 304: the
        // background loop runs this every few minutes, and most ticks change
        // nothing. Read before the row lock — like the fetch itself, the
        // conditional round-trip must not stall other writers.
        let held = providers::gateway_snapshot_for_policy(&*self.store, &policy).await?;
        let held_etag = held
            .as_ref()
            .filter(|snapshot| {
                snapshot.installation_id.as_deref() == Some(&session.installation_id)
            })
            .and_then(|snapshot| snapshot.catalog_etag.clone());
        // Fetch the full entitled set before the row lock is taken. The
        // member catalog is preferred: one envelope, merged protocols per
        // model, and the alias list that matches curated rows. A gateway
        // that predates it degrades to the per-protocol
        // `/api/v1/cli/models` read.
        let mut model_protocols = std::collections::BTreeMap::new();
        let mut model_reasoning_efforts = std::collections::BTreeMap::new();
        let mut member_catalog = None;
        let mut catalog_etag = None;
        let models: Vec<CustomModelConfig> = match connection.catalog(held_etag.as_deref()).await? {
            GatewayCatalogFetch::NotModified => {
                // Nothing moved since the held revision; the snapshot the
                // policy already honors stays as it is. Recheck the policy and
                // session before accepting the response so sign-out, session
                // replacement, and MDM repoints fence every sync outcome.
                let count = held
                    .as_ref()
                    .map(|snapshot| snapshot.models.len())
                    .unwrap_or_default();
                self.pause_sync_commit_for_test().await;
                let _lock = providers::GATEWAY_STATE_WRITES.lock().await;
                self.recheck_sync_context(&base_url, &session).await?;
                return Ok(count);
            }
            GatewayCatalogFetch::Fresh { catalog, etag } => {
                member_catalog = Some(MEMBER_CATALOG_V1.to_owned());
                catalog_etag = etag;
                let converted = providers::member_catalog_models(catalog);
                model_protocols = converted.model_protocols;
                model_reasoning_efforts = converted.model_reasoning_efforts;
                converted.models
            }
            GatewayCatalogFetch::Unsupported => connection
                .models(None)
                .await?
                .into_iter()
                .filter_map(|model| {
                    let Some(protocol) = providers::GatewayModelProtocol::parse(&model.protocol)
                    else {
                        tracing::debug!(
                            "gateway model sync skipped a model with an unsupported inference protocol"
                        );
                        return None;
                    };
                    let id = model.id;
                    model_protocols.insert(id.clone(), protocol);
                    Some(CustomModelConfig {
                        id,
                        display_name: Some(model.name),
                        // Carried so a deployment-aliased id can still be matched to
                        // its curated row; gateways older than the field send none.
                        upstream_id: model.upstream_id,
                        aliases: Vec::new(),
                        context_window: providers::clamp_u32(model.context_window, 32_768),
                        max_output_tokens: providers::clamp_u32(model.max_output_tokens, 4_096),
                        input_modalities: vec![crate::model_registry::InputModality::Text],
                        supports_reasoning: false,
                        reasoning_efforts: Vec::new(),
                    })
                })
                .collect(),
        };
        // The gateway is trusted for entitlements, not for shapes: the synced
        // set is held to the same bounds as user-entered custom models.
        providers::validate_custom_models(&models).map_err(|error| {
            AgentError::config(format!("gateway model sync rejected: {error:?}"))
        })?;
        let count = models.len();
        self.pause_sync_commit_for_test().await;
        let _lock = providers::GATEWAY_STATE_WRITES.lock().await;
        // The fetch ran outside the lock, so the policy authority (an MDM
        // push) may have re-pointed the deployment while it was in flight.
        // Re-resolve under the lock and refuse to stamp a snapshot the new
        // policy never entitled.
        self.recheck_sync_context(&base_url, &session).await?;
        providers::write_gateway_snapshot(
            &*self.store,
            &providers::GatewayModelSnapshot {
                gateway_url: base_url.clone(),
                installation_id: Some(session.installation_id.clone()),
                models,
                model_protocols,
                model_reasoning_efforts,
                member_catalog,
                catalog_etag,
            },
        )
        .await?;
        Ok(count)
    }

    pub(super) fn recheck_sync_policy(
        &self,
        base_url: &str,
    ) -> std::result::Result<crate::managed_policy::ManagedPolicy, crate::error::ServerError> {
        let policy = crate::managed_policy::resolve(&*self.provisioned_policy, &*self.os_policy)?;
        if policy.gateway_url.as_deref() != Some(base_url) {
            // A benign, retryable race — the deployment was re-pointed while
            // the fetch was in flight — not an internal fault. The stable
            // kind lets clients branch on it, as with `managed_profile`.
            return Err(crate::error::ServerError::conflict_kind(
                "gateway_changed",
                "the model gateway configuration changed during model sync",
            ));
        }
        Ok(policy)
    }

    /// Revalidate both authorities that made a catalog response admissible.
    /// The policy URL alone does not distinguish sign-out or replacement by a
    /// different session at the same deployment.
    pub(super) async fn recheck_sync_context(
        &self,
        base_url: &str,
        expected_session: &crate::connectors::GatewayCredentials,
    ) -> std::result::Result<crate::managed_policy::ManagedPolicy, crate::error::ServerError> {
        let policy = self.recheck_sync_policy(base_url)?;
        let current = CredentialVault::new(self.secrets.clone()).load().await?;
        let unchanged = current.as_ref().is_some_and(|credentials| {
            credentials.matches_base_url(base_url)
                && same_gateway_session(credentials, expected_session)
        });
        if !unchanged {
            return Err(crate::error::ServerError::conflict_kind(
                "gateway_changed",
                "the model gateway session changed during model sync",
            ));
        }
        Ok(policy)
    }

    #[cfg(test)]
    pub(super) async fn pause_sync_commit_for_test(&self) {
        let pause = self.sync_commit_pause.lock().await.take();
        if let Some(pause) = pause {
            pause.arrived.notify_one();
            pause.release.notified().await;
        }
    }

    #[cfg(not(test))]
    pub(super) async fn pause_sync_commit_for_test(&self) {}

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
    pub(super) async fn sync_models_if_connected(
        &self,
    ) -> std::result::Result<Option<usize>, crate::error::ServerError> {
        let policy = self.policy()?;
        let Some(connection) = self.connection_for(&policy).await? else {
            return Ok(None);
        };
        if connection.stored_credentials().await?.is_none() {
            return Ok(None);
        }
        Ok(Some(self.sync_models().await?))
    }
}
