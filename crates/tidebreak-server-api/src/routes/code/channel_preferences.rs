use super::external::ExternalGrantAuth;
use crate::code::channel_preferences::{self, ChannelPreferences};
use crate::{
    code::ScopedCode,
    error::ServerError,
    extract::{Json, Path},
    state::AppState,
};
use axum::extract::State;

#[derive(serde::Serialize)]
pub struct ChannelPreferencesSnapshot {
    #[serde(flatten)]
    pub preferences: ChannelPreferences,
    pub channel_id: String,
    pub workspace_identity: String,
    pub settings_path: String,
    pub can_edit: bool,
}

fn snapshot(
    grant: &tidebreak_core::CodeExternalGrant,
    channel: String,
    preferences: ChannelPreferences,
    can_edit: bool,
) -> ChannelPreferencesSnapshot {
    ChannelPreferencesSnapshot {
        settings_path: channel_preferences::settings_path(grant, &channel),
        workspace_identity: grant.workspace_identity.clone(),
        channel_id: channel,
        preferences,
        can_edit,
    }
}

pub async fn get_channel_preferences(
    code: ScopedCode,
    Path((id, channel)): Path<(tidebreak_core::CodeGrantId, String)>,
) -> Result<Json<ChannelPreferencesSnapshot>, ServerError> {
    let grant = code.channel_preferences_grant(id, false).await?;
    let preferences = code.channel_preferences(id, &channel).await?;
    Ok(Json(snapshot(
        &grant,
        channel,
        preferences,
        code.is_admin() && !code.is_service(),
    )))
}

pub async fn put_channel_preferences(
    code: ScopedCode,
    Path((id, channel)): Path<(tidebreak_core::CodeGrantId, String)>,
    Json(preferences): Json<ChannelPreferences>,
) -> Result<Json<ChannelPreferencesSnapshot>, ServerError> {
    let grant = code.channel_preferences_grant(id, true).await?;
    code.set_channel_preferences(id, &channel, &preferences)
        .await?;
    Ok(Json(snapshot(&grant, channel, preferences, true)))
}

pub async fn external_channel_preferences(
    State(state): State<AppState>,
    ExternalGrantAuth(grant): ExternalGrantAuth,
    Path(channel): Path<String>,
) -> Result<Json<ChannelPreferencesSnapshot>, ServerError> {
    let runtime = state
        .code
        .as_ref()
        .ok_or_else(|| ServerError::unauthorized("adapter access is not configured"))?;
    let preferences = channel_preferences::read(&runtime.db, &grant, &channel).await?;
    Ok(Json(snapshot(&grant, channel, preferences, false)))
}

#[derive(serde::Deserialize)]
pub struct ChannelHarnessQuery {
    pub kind: Option<tidebreak_core::HarnessKind>,
}

#[derive(serde::Serialize)]
pub struct ChannelHarnessCatalog {
    pub harnesses: Vec<tidebreak_core::HarnessKind>,
    pub models: Vec<super::types::HarnessModel>,
    /// Local internal engines use the ordinary chat catalog.
    pub use_chat_catalog: bool,
}

/// Channel model reads use the connection's identity and execution location.
pub async fn get_channel_harness_catalog(
    code: ScopedCode,
    Path((id, channel)): Path<(tidebreak_core::CodeGrantId, String)>,
    axum::extract::Query(query): axum::extract::Query<ChannelHarnessQuery>,
) -> Result<Json<ChannelHarnessCatalog>, ServerError> {
    let grant = code.channel_preferences_grant(id, false).await?;
    channel_preferences::validate_channel(&grant, &channel)?;
    let harnesses = match code.channel_sandbox_harnesses() {
        Some(harnesses) => harnesses,
        None => {
            let mut ready = Vec::new();
            for kind in tidebreak_core::HarnessKind::ALL {
                if let Some(adapter) = code.adapters().get(*kind) {
                    if code.probe(adapter.as_ref()).await.found {
                        ready.push(*kind);
                    }
                }
            }
            ready
        }
    };
    let mut result = ChannelHarnessCatalog {
        harnesses,
        models: Vec::new(),
        use_chat_catalog: false,
    };
    let Some(kind) = query.kind else {
        return Ok(Json(result));
    };
    if !result.harnesses.contains(&kind) {
        return Err(ServerError::unprocessable_kind(
            "channel_harness_unavailable",
            "this harness is not available for channel sessions on this deployment",
        ));
    }
    if let Some(relay) = code.harness_llm() {
        let gateway = relay
            .external_delegations()
            .ok_or_else(|| {
                ServerError::conflict_kind(
                    "external_reconnect_required",
                    "reconnect this Slack connection before selecting a model",
                )
            })?
            .for_grant(&grant.owner, grant.id)
            .await?;
        if kind.is_in_process() {
            let snapshot = gateway.snapshot_for(&grant.owner).await?.ok_or_else(|| {
                ServerError::conflict_kind(
                    "model_provider_unavailable",
                    "this Slack connection has no available model catalog",
                )
            })?;
            result.models = snapshot
                .models
                .iter()
                .filter_map(|model| {
                    let key = crate::model_registry::selection_key(
                        crate::providers::ProviderKind::ModelGateway,
                        &model.id,
                    );
                    let policy = crate::providers::gateway_execution_policy(&snapshot, &key)?;
                    policy.supports_tools.then_some(super::types::HarnessModel {
                        id: key,
                        label: policy.display_name,
                        default: false,
                        reasoning_efforts: Vec::new(),
                        fast_mode: false,
                    })
                })
                .collect();
        } else {
            let (anthropic, openai) = gateway.compat_listings(&grant.owner).await?;
            result.models = super::harnesses::hosted_models_from_listings(kind, anthropic, openai)?;
        }
    } else if kind.is_in_process() {
        result.use_chat_catalog = true;
    } else {
        let adapter = code.adapter(kind)?;
        let probe = code.probe(adapter.as_ref()).await;
        if !probe.found {
            return Err(ServerError::unprocessable_kind(
                "harness_not_found",
                "this deployment cannot list models without the harness or a Gateway connection",
            ));
        }
        result.models = adapter
            .list_models(&probe)
            .await
            .into_iter()
            .map(|model| super::types::HarnessModel {
                id: model.id,
                label: model.label,
                default: model.default,
                reasoning_efforts: model.reasoning_efforts,
                fast_mode: model.fast_mode,
            })
            .collect();
    }
    Ok(Json(result))
}
