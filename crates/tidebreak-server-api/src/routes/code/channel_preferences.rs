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
