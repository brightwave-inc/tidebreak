//! Model snapshots, catalog sync, and per-request route tokens.

use super::*;

struct GatewayTokenSource {
    connection: Arc<GatewayConnection>,
    installation_id: String,
    policy_source: Arc<dyn GatewayPolicySource>,
    model_state: Arc<dyn GatewayModelState>,
    model_sync: Arc<RwLock<()>>,
}

impl GatewayTokenSource {
    async fn resolve_model_route(&self, route_model: &str) -> Result<Option<GatewayRoute>> {
        let policy = self.policy_source.resolve()?;
        let Some(snapshot) = self.model_state.snapshot(&policy).await? else {
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
        Ok(self
            .model_state
            .resolve_route(&snapshot, route_model)
            .await?
            .filter(|resolved| resolved.route_model == route_model))
    }

    fn route_lease(
        resolved: GatewayRoute,
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
        conversation: Option<tidebreak_core::id::SessionId>,
    ) -> Result<(String, Option<ModelRouteLease>)> {
        let guard = self.model_sync.clone().read_owned().await;
        if self.resolve_model_route(route_model).await?.is_none() {
            return Ok((String::new(), None));
        }
        let token = self.bearer_token_for(conversation).await?;
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

    async fn bearer_token_for(
        &self,
        conversation: Option<tidebreak_core::id::SessionId>,
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
    pub async fn route_token_source(self: &Arc<Self>) -> Option<Arc<dyn BearerTokenSource>> {
        let connection = self.connection().await.ok().flatten()?;
        let credentials = connection.stored_credentials().await.ok().flatten()?;
        Some(Arc::new(GatewayTokenSource {
            connection,
            installation_id: credentials.installation_id,
            policy_source: self.policy_source.clone(),
            model_state: self.model_state.clone(),
            model_sync: self.model_sync.clone(),
        }))
    }

    pub async fn sync_models(&self) -> std::result::Result<usize, GatewaySyncError> {
        let _sync = self.model_sync.write().await;
        let policy = self.policy()?;
        let base_url = require_managed(&policy)?;
        let connection = self.connection_at(base_url.clone()).await?;
        connection.access_token(RESOURCE_CONTROL).await?;
        let session = connection.stored_credentials().await?.ok_or_else(|| {
            GatewaySyncError::conflict(
                "gateway_changed",
                "the model gateway session changed during model sync",
            )
        })?;
        let held = self.model_state.snapshot(&policy).await?;
        let held_etag = held
            .as_ref()
            .filter(|snapshot| {
                snapshot.installation_id.as_deref() == Some(&session.installation_id)
            })
            .and_then(|snapshot| snapshot.catalog_etag.clone());

        let mut model_protocols = BTreeMap::new();
        let mut model_reasoning_efforts = BTreeMap::new();
        let mut member_catalog = None;
        let mut catalog_etag = None;
        let models: Vec<SyncedGatewayModel> =
            match connection.catalog(held_etag.as_deref()).await? {
            GatewayCatalogFetch::NotModified => {
                let count = held
                    .as_ref()
                    .map(|snapshot| snapshot.models.len())
                    .unwrap_or_default();
                self.pause_sync_commit_for_test().await;
                let _lock = GATEWAY_STATE_WRITES.lock().await;
                self.recheck_sync_context(&base_url, &session).await?;
                return Ok(count);
            }
            GatewayCatalogFetch::Fresh { catalog, etag } => {
                member_catalog = Some(MEMBER_CATALOG_V1.to_owned());
                catalog_etag = etag;
                catalog
                    .models
                    .into_iter()
                    .filter_map(|model| {
                        let protocols: Vec<_> = model
                            .protocols
                            .iter()
                            .filter_map(|protocol| GatewayModelProtocol::parse(protocol))
                            .collect();
                        let protocol = if protocols
                            .contains(&GatewayModelProtocol::AnthropicMessages)
                        {
                            GatewayModelProtocol::AnthropicMessages
                        } else {
                            *protocols.first()?
                        };
                        let id = model.id;
                        model_protocols.insert(id.clone(), protocol);
                        if let Some(efforts) = model.supported_reasoning_efforts {
                            model_reasoning_efforts.insert(id.clone(), efforts.clone());
                            for alias in &model.aliases {
                                model_reasoning_efforts.insert(alias.clone(), efforts.clone());
                            }
                        }
                        Some(SyncedGatewayModel {
                            id,
                            display_name: Some(model.name),
                            upstream_id: None,
                            aliases: model.aliases,
                            context_window: clamp_u32(model.context_window, 32_768),
                            max_output_tokens: clamp_u32(model.max_output_tokens, 4_096),
                        })
                    })
                    .collect()
            }
            GatewayCatalogFetch::Unsupported => connection
                .models(None)
                .await?
                .into_iter()
                .filter_map(|model| {
                    let Some(protocol) = GatewayModelProtocol::parse(&model.protocol) else {
                        tracing::debug!(
                            "gateway model sync skipped a model with an unsupported inference protocol"
                        );
                        return None;
                    };
                    let id = model.id;
                    model_protocols.insert(id.clone(), protocol);
                    Some(SyncedGatewayModel {
                        id,
                        display_name: Some(model.name),
                        upstream_id: model.upstream_id,
                        aliases: Vec::new(),
                        context_window: clamp_u32(model.context_window, 32_768),
                        max_output_tokens: clamp_u32(model.max_output_tokens, 4_096),
                    })
                })
                .collect(),
            };
        let count = models.len();
        self.pause_sync_commit_for_test().await;
        let _lock = GATEWAY_STATE_WRITES.lock().await;
        self.recheck_sync_context(&base_url, &session).await?;
        self.model_state
            .write_snapshot(&GatewayModelSnapshot {
                gateway_url: base_url,
                installation_id: Some(session.installation_id),
                models,
                model_protocols,
                model_reasoning_efforts,
                member_catalog,
                catalog_etag,
            })
            .await?;
        Ok(count)
    }

    fn recheck_sync_policy(
        &self,
        base_url: &str,
    ) -> std::result::Result<GatewayPolicy, GatewaySyncError> {
        let policy = self.policy()?;
        if policy.gateway_url.as_deref() != Some(base_url) {
            return Err(GatewaySyncError::conflict(
                "gateway_changed",
                "the model gateway configuration changed during model sync",
            ));
        }
        Ok(policy)
    }

    async fn recheck_sync_context(
        &self,
        base_url: &str,
        expected_session: &GatewayCredentials,
    ) -> std::result::Result<GatewayPolicy, GatewaySyncError> {
        let policy = self.recheck_sync_policy(base_url)?;
        let current = CredentialVault::new(self.secrets.clone()).load().await?;
        let unchanged = current.as_ref().is_some_and(|credentials| {
            credentials.matches_base_url(base_url)
                && same_gateway_session(credentials, expected_session)
        });
        if !unchanged {
            return Err(GatewaySyncError::conflict(
                "gateway_changed",
                "the model gateway session changed during model sync",
            ));
        }
        Ok(policy)
    }

    async fn pause_sync_commit_for_test(&self) {
        let pause = self.sync_commit_pause.lock().await.take();
        if let Some(pause) = pause {
            pause.arrived.notify_one();
            pause.release.notified().await;
        }
    }

    pub async fn sync_models_periodically(self: Arc<Self>, mcp: Arc<dyn GatewayMcpControl>) {
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
            match self.reconcile_endpoint_mounts(&*mcp).await {
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

    #[doc(hidden)]
    pub async fn sync_models_if_connected(
        &self,
    ) -> std::result::Result<Option<usize>, GatewaySyncError> {
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
