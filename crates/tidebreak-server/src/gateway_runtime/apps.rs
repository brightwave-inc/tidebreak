//! Entitled apps, endpoint mounts, and the MCP catalog sources.

use super::*;

/// Attestation-context key for connect-time MCP traffic — `initialize` and
/// `tools/list` belong to no chat, but a gateway-attested endpoint refuses
/// any token minted without a context, so the handshake rides a shared one.
/// Not a UUID, so it can never collide with a chat key.
pub(super) const MCP_CONNECT_CONTEXT_KEY: &str = "mcp:connect";

#[async_trait]
impl crate::mcp_config::GatewayEndpoints for GatewayRuntime {
    /// Resolve a gateway MCP endpoint from the signed-in session: its URL
    /// under the configured base, and a fresh `mcp:<slug>` bearer minted (or
    /// served from cache) inside the connector's rotation lock. The bearer
    /// carries the shared connect context, which is what lets an attested
    /// endpoint accept the handshake and list its tools.
    async fn endpoint(&self, slug: &str) -> Result<crate::mcp_config::GatewayEndpointAccess> {
        let connection = self
            .connection()
            .await?
            .ok_or_else(|| AgentError::config("no model gateway is configured"))?;
        Ok(crate::mcp_config::GatewayEndpointAccess {
            url: connection.mcp_endpoint_url(slug)?,
            bearer_token: connection
                .attested_mcp_access_token(slug, MCP_CONNECT_CONTEXT_KEY)
                .await?,
        })
    }

    /// A `tools/call` from a chat rides a token minted inside that chat's
    /// attestation context — the same context the chat's inference tokens
    /// carry — so an attested endpoint can match the call against the
    /// observation that inference recorded. Direct endpoints ignore the
    /// context, so every gateway mount dispatches this way.
    async fn call_bearer(&self, slug: &str, chat: tidebreak_core::id::ChatId) -> Result<String> {
        let connection = self
            .connection()
            .await?
            .ok_or_else(|| AgentError::config("no model gateway is configured"))?;
        connection
            .attested_mcp_access_token(slug, &chat.to_string())
            .await
    }

    /// The gateway apps the `create_app` roster lists, read live through the
    /// stored session. Best-effort throughout: no session, an unreachable
    /// gateway, or a gateway predating either read all answer empty, which
    /// renders as an absent roster section rather than a failed registry
    /// rebuild.
    async fn entitled_app_catalogs(&self) -> Vec<crate::mcp_config::GatewayRosterApp> {
        self.app_roster().await.unwrap_or_default()
    }
}

#[async_trait]
impl crate::connected_apps::GatewayCatalogSource for GatewayRuntime {
    /// Resolve the requested gateway apps against the live session: one
    /// entitled-apps read, then one catalog read per requested id that the
    /// session actually reaches.
    ///
    /// Fail-closed and quiet. A profile that cannot answer at all — unmanaged,
    /// signed out, a gateway predating either read — answers `None`, and any
    /// single app that is disabled, unentitled, or catalog-less is simply
    /// absent from the map. Both readings make a grant naming it stale rather
    /// than matching a fingerprint over a catalog nobody read.
    async fn gateway_app_catalogs(
        &self,
        needed: &std::collections::BTreeSet<String>,
    ) -> Option<(
        String,
        std::collections::BTreeMap<String, crate::connected_apps::GatewayAppCatalog>,
    )> {
        let (base_url, entitled) = match self.entitled_apps_if_signed_in().await {
            Ok(Some(answered)) => answered,
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!("could not read the gateway's entitled apps: {error}");
                return None;
            }
        };
        let mut catalogs = std::collections::BTreeMap::new();
        for app in entitled
            .into_iter()
            .filter(|app| app.enabled && needed.contains(&app.id))
        {
            match self.app_catalog(&app.id).await {
                Ok(Some(operations)) => {
                    catalogs.insert(
                        app.id,
                        crate::connected_apps::GatewayAppCatalog {
                            name: app.name,
                            operation_ids: operations
                                .into_iter()
                                .map(|operation| operation.operation_id)
                                .collect(),
                        },
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        gateway_app = %app.id,
                        "could not read a gateway app's operation catalog: {error}"
                    );
                }
            }
        }
        Some((base_url, catalogs))
    }
}

impl GatewayRuntime {
    /// The entitled connected apps, fetched live from the gateway with the
    /// stored session. Managed-only, like the whole sign-in surface; a
    /// gateway without the JSON apps surface reports `supported: false`.
    pub(crate) async fn apps(&self, owner: &tidebreak_core::OwnerId) -> Result<GatewayApps> {
        let connection = self.managed_connection().await?;
        let Some(apps) = Self::entitled_apps(&connection).await? else {
            return Ok(GatewayApps {
                supported: false,
                apps: Vec::new(),
            });
        };
        // How many local mini-apps currently bind each gateway app: distinct
        // live grants naming the id. The binding grammar forbids a grant
        // naming one gateway app twice, so grants and apps count one-to-one.
        // Best-effort, like every other projection field here — an unreadable
        // grant table leaves the counts at zero rather than failing the list.
        let mut used_by: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        match self.store.list_live_app_grants_scoped(owner).await {
            Ok(grants) => {
                for grant in grants {
                    for binding in &grant.bindings {
                        if let Some(gateway_app) = binding.gateway_app() {
                            *used_by.entry(gateway_app.to_owned()).or_default() += 1;
                        }
                    }
                }
            }
            Err(error) => {
                tracing::warn!("could not count local apps bound to gateway apps: {error}");
            }
        }
        Ok(GatewayApps {
            supported: true,
            apps: apps
                .into_iter()
                .map(|mut app| {
                    app.used_by_app_count = used_by.get(&app.id).copied().unwrap_or_default();
                    app
                })
                .collect(),
        })
    }

    /// The entitled connected apps, or `None` when the gateway serves
    /// neither apps surface.
    ///
    /// The member catalog carries per-app readiness the older surface
    /// cannot express; prefer it, and degrade to `/api/v1/cli/apps`
    /// (readiness unknown) against a gateway that predates it. Always an
    /// unconditional fetch: this is the live projection a revoked grant
    /// must fall out of, so a cached revision would be a lie.
    pub(super) async fn entitled_apps(
        connection: &GatewayConnection,
    ) -> Result<Option<Vec<GatewayAppInfo>>> {
        match connection.catalog(None).await? {
            GatewayCatalogFetch::Fresh { catalog, .. } => Ok(Some(
                catalog
                    .apps
                    .into_iter()
                    .map(|app| GatewayAppInfo {
                        used_by_app_count: 0,
                        id: app.id,
                        name: app.name,
                        app_kind: app.app_kind,
                        enabled: app.enabled,
                        mcp_endpoint_slugs: app.mcp_endpoint_slugs,
                        connection: Some(app.connection),
                    })
                    .collect(),
            )),
            GatewayCatalogFetch::NotModified => Err(AgentError::msg(
                "gateway answered an unconditional catalog fetch with 304",
            )),
            GatewayCatalogFetch::Unsupported => Ok(connection.apps().await?.map(|apps| {
                apps.into_iter()
                    .map(|app| GatewayAppInfo {
                        used_by_app_count: 0,
                        id: app.id,
                        name: app.name,
                        app_kind: app.app_kind,
                        enabled: app.enabled,
                        mcp_endpoint_slugs: app.mcp_endpoint_slugs,
                        connection: None,
                    })
                    .collect()
            })),
        }
    }

    /// Mount newly entitled gateway MCP endpoints into the configured server
    /// set — mount-by-default for a managed profile, where the organization
    /// already curated the entitlements.
    ///
    /// The entitlement source is [`entitled_apps`](Self::entitled_apps) — the
    /// same read the settings panel lists, so server and UI cannot disagree
    /// about what is entitled. Endpoints the user explicitly unmounted are
    /// remembered by the MCP runtime and never re-mounted here; a repeat
    /// reconcile with no new entitlements changes nothing. Every state where
    /// a reconcile cannot run — unmanaged profile, misconfigured policy, no
    /// session for the policy's deployment, a gateway predating the apps
    /// surface — is "nothing to do", not an error, so callers may run this on
    /// every trigger without gating.
    pub(crate) async fn reconcile_endpoint_mounts(
        &self,
        mcp: &crate::mcp_config::McpRuntime,
    ) -> Result<()> {
        let policy = self.policy()?;
        let Some(connection) = self.connection_for(&policy).await? else {
            return Ok(());
        };
        if connection.stored_credentials().await?.is_none() {
            return Ok(());
        }
        let Some(apps) = Self::entitled_apps(&connection).await? else {
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

    /// The gateway's deployment URL and the apps the signed-in user is
    /// entitled to, or `None` when this profile cannot answer that question.
    ///
    /// The non-refusing twin of [`apps`](Self::apps): every state where there
    /// is nothing to read — unmanaged profile, misconfigured policy, no
    /// session for the policy's deployment, a session the gateway has since
    /// revoked, a gateway predating the apps surface — answers `None` rather
    /// than an error, exactly as [`reconcile_endpoint_mounts`] treats them as
    /// "nothing to do". Callers use this where a gateway that cannot answer
    /// must degrade (an authoring roster) or fail closed (a fingerprint read),
    /// never fault.
    ///
    /// The URL is the resolved policy's, the same string every other gateway
    /// identity is stamped with — so a fingerprint taken from it moves when
    /// and only when the profile is re-paired.
    pub(crate) async fn entitled_apps_if_signed_in(
        &self,
    ) -> Result<Option<(String, Vec<GatewayApp>)>> {
        let policy = self.policy()?;
        let Some(base_url) = policy.gateway_url.clone().filter(|_| policy.managed) else {
            return Ok(None);
        };
        let Some(connection) = self.connection_for(&policy).await? else {
            return Ok(None);
        };
        if connection.stored_credentials().await?.is_none() {
            return Ok(None);
        }
        match connection.apps().await {
            Ok(Some(apps)) => Ok(Some((base_url, apps))),
            Ok(None) => Ok(None),
            // A revoked or expired session is "signed out", not a fault: the
            // stored blob outlives the gateway's own session record.
            Err(error) if is_sign_in_required(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// One entitled app's declared operation catalog, under the same gating as
    /// [`entitled_apps_if_signed_in`](Self::entitled_apps_if_signed_in): a
    /// profile that cannot read it answers `None`, never an error.
    pub(crate) async fn app_catalog(
        &self,
        app_id: &str,
    ) -> Result<Option<Vec<GatewayOperationSummary>>> {
        let policy = self.policy()?;
        let Some(connection) = self.connection_for(&policy).await? else {
            return Ok(None);
        };
        if connection.stored_credentials().await?.is_none() {
            return Ok(None);
        }
        match connection.app_operations(app_id).await {
            Ok(catalog) => Ok(catalog),
            Err(error) if is_sign_in_required(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Every bindable gateway connected app with the operation ids its
    /// catalog declares — the one live read both the `create_app` roster and
    /// the authoring door work from.
    ///
    /// `None` is "this profile has no gateway session to ask", which the door
    /// turns into a teachable refusal; `Some(vec![])` is a session that
    /// answered with nothing bindable. An app whose catalog cannot be read is
    /// left out: nothing about it could be pinned, so listing it would only
    /// invite a refusal one step later.
    pub(crate) async fn app_roster(&self) -> Option<Vec<crate::mcp_config::GatewayRosterApp>> {
        let entitled = match self.entitled_apps_if_signed_in().await {
            Ok(Some((_, entitled))) => entitled,
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!("could not read the gateway's entitled apps: {error}");
                return None;
            }
        };
        let mut roster = Vec::new();
        for app in entitled.into_iter().filter(|app| app.enabled) {
            match self.app_catalog(&app.id).await {
                Ok(Some(operations)) => roster.push(crate::mcp_config::GatewayRosterApp {
                    id: app.id,
                    name: app.name,
                    operation_ids: operations
                        .into_iter()
                        .map(|operation| operation.operation_id)
                        .collect(),
                }),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        gateway_app = %app.id,
                        "could not read a gateway app's operation catalog: {error}"
                    );
                }
            }
        }
        Some(roster)
    }
}
