//! The shared-app invoke relay that forwards proxy_api calls through the gateway.

use super::*;

/// The production gateway relay: the one gateway runtime plus the
/// registration seam, wired behind [`GatewayInvokeDispatcher`].
///
/// Two gates before any operation is relayed, both fail-closed: the profile
/// must hold a session at the deployment policy names, and the app must be
/// registered there — which the registration seam establishes on the spot
/// when it is not, so a granted app self-heals into a servable one on its
/// first call.
pub(crate) struct GatewayRelayDispatcher {
    runtime: Arc<GatewayRuntime>,
    drafts: Arc<dyn crate::connected_apps::GatewayDraftSource>,
}

/// The relay as production assembles it: over the process's one gateway
/// runtime and the store-backed registration registry.
pub(crate) fn gateway_relay_dispatcher(
    runtime: Arc<GatewayRuntime>,
    drafts: Arc<dyn crate::connected_apps::GatewayDraftSource>,
) -> Arc<dyn crate::connected_apps::GatewayInvokeDispatcher> {
    Arc::new(GatewayRelayDispatcher { runtime, drafts })
}

impl GatewayRelayDispatcher {
    /// One relay attempt against an already-resolved shared app.
    pub(super) async fn relay(
        &self,
        connection: &crate::connectors::GatewayConnection,
        shared_app_id: &str,
        body: &serde_json::Value,
    ) -> std::result::Result<
        crate::connectors::GatewayInvokeOutcome,
        crate::connected_apps::GatewayDispatchError,
    > {
        use crate::connected_apps::GatewayDispatchError;

        match connection.invoke_shared_app(shared_app_id, body).await {
            Ok(Some(outcome)) => Ok(outcome),
            Ok(None) => Err(GatewayDispatchError::NotRegistered),
            // A session the gateway no longer honors is "no session" here, the
            // same reading every other gateway read gives it.
            Err(error) if is_sign_in_required(&error) => Err(GatewayDispatchError::NoSession),
            Err(error) => {
                tracing::warn!("gateway shared-app invoke failed: {error}");
                Err(GatewayDispatchError::Unreachable(
                    "the gateway could not complete this call".to_owned(),
                ))
            }
        }
    }
}

#[async_trait]
impl crate::connected_apps::GatewayInvokeDispatcher for GatewayRelayDispatcher {
    async fn dispatch(
        &self,
        owner: &tidebreak_core::OwnerId,
        app: tidebreak_core::id::AppId,
        request: &crate::connected_apps::GatewayOperationRequest,
    ) -> std::result::Result<
        crate::connectors::GatewayInvokeOutcome,
        crate::connected_apps::GatewayDispatchError,
    > {
        use crate::connected_apps::GatewayDispatchError;

        // A read that faults is reported as unreachable rather than as "no
        // session": the distinction the route draws is whether a session
        // exists, and a failed policy or vault read does not answer that.
        let unreachable = |context: &'static str| {
            move |error: tidebreak_core::AgentError| {
                tracing::warn!("gateway relay could not {context}: {error}");
                GatewayDispatchError::Unreachable(format!(
                    "the gateway could not be reached to {context}"
                ))
            }
        };
        let policy = self
            .runtime
            .policy()
            .map_err(unreachable("read this profile's gateway policy"))?;
        let Some(base_url) = policy.gateway_url.clone().filter(|_| policy.managed) else {
            return Err(GatewayDispatchError::NoSession);
        };
        let Some(connection) = self
            .runtime
            .connection_for(&policy)
            .await
            .map_err(unreachable("open a gateway connection"))?
        else {
            return Err(GatewayDispatchError::NoSession);
        };
        if connection
            .stored_credentials()
            .await
            .map_err(unreachable("read the stored gateway session"))?
            .is_none()
        {
            return Err(GatewayDispatchError::NoSession);
        }
        let shared_app_id = match self
            .drafts
            .ensure_registered(owner, app, &base_url)
            .await
            .map_err(unreachable("register this app at the gateway"))?
        {
            crate::connected_apps::GatewayRegistration::Registered { shared_app_id, .. } => {
                shared_app_id
            }
            crate::connected_apps::GatewayRegistration::NotRegistered => {
                return Err(GatewayDispatchError::NotRegistered)
            }
            crate::connected_apps::GatewayRegistration::Refused { message } => {
                return Err(GatewayDispatchError::Unreachable(message))
            }
        };
        let body = shared_app_invoke_body(request);
        crate::gateway_drafts::relay_with_consent_self_heal(
            &*self.drafts,
            owner,
            app,
            &base_url,
            || self.relay(&connection, &shared_app_id, &body),
        )
        .await
    }
}

/// The gateway's own invoke vocabulary, verbatim. Absent halves are omitted
/// rather than sent as null: the gateway's argument fields default when
/// missing but refuse an explicit null, matching its `proxy_api` tool's
/// schema — a null here makes every relayed call an `invalid_request`.
pub(super) fn shared_app_invoke_body(
    request: &crate::connected_apps::GatewayOperationRequest,
) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    body.insert(
        "connected_app_id".into(),
        serde_json::Value::String(request.gateway_app.clone()),
    );
    body.insert(
        "operation_id".into(),
        serde_json::Value::String(request.operation_id.clone()),
    );
    if let Some(path_parameters) = &request.path_parameters {
        body.insert("path_parameters".into(), path_parameters.clone());
    }
    if let Some(query) = &request.query {
        body.insert("query".into(), query.clone());
    }
    if let Some(request_body) = &request.body {
        body.insert("body".into(), request_body.clone());
    }
    serde_json::Value::Object(body)
}
