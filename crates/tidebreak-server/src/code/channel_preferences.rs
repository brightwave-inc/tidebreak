//! Channel behavior belongs to Tidebreak; Gateway still authorizes execution.
use crate::error::ServerError;
use serde::{Deserialize, Serialize};
use tidebreak_core::db::DbStore;
use tidebreak_core::{CodeExternalGrant, HarnessKind, OwnerId, SessionId, Store};

pub const MAX_CHANNEL_INSTRUCTIONS: usize = 8_192;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ChannelPreferences {
    pub harness: Option<HarnessKind>,
    pub model: Option<String>,
    /// Applies only to replies in an established Tidebreak thread.
    pub respond_automatically: Option<bool>,
    #[serde(default)]
    pub instructions: String,
}

impl ChannelPreferences {
    pub fn validate(&self) -> Result<(), ServerError> {
        if self.instructions.len() > MAX_CHANNEL_INSTRUCTIONS || self.instructions.contains('\0') {
            return Err(ServerError::bad_request(
                "channel instructions must fit in 8192 bytes and contain no null characters",
            ));
        }
        if self
            .model
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
        {
            return Err(ServerError::bad_request(
                "model must contain 1 to 256 bytes and no control characters",
            ));
        }
        Ok(())
    }
}

pub fn validate_channel(grant: &CodeExternalGrant, channel: &str) -> Result<(), ServerError> {
    if grant.revoked_at.is_some() || grant.channel_kind != "slack" {
        return Err(ServerError::not_found("Slack connection not found"));
    }
    if channel.is_empty()
        || channel.len() > 128
        || !channel
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err(ServerError::bad_request(
            "channel must use 1 to 128 letters, digits, hyphens, or underscores",
        ));
    }
    Ok(())
}

fn key(grant: &CodeExternalGrant, channel: &str) -> String {
    // JSON tuple encoding keeps each component distinct without separator collisions.
    format!(
        "code.channel.preferences:{}",
        serde_json::json!([grant.channel_kind, grant.workspace_identity, channel])
    )
}

pub fn settings_path(grant: &CodeExternalGrant, channel: &str) -> String {
    format!("/settings/channels?grant={}&channel={channel}", grant.id)
}

pub async fn read(
    store: &DbStore,
    grant: &CodeExternalGrant,
    channel: &str,
) -> Result<ChannelPreferences, ServerError> {
    validate_channel(grant, channel)?;
    store
        .get_setting(&key(grant, channel))
        .await?
        .map(serde_json::from_value)
        .transpose()
        .map(|p| p.unwrap_or_default())
        .map_err(|error| ServerError::internal(error.to_string()))
}

pub async fn write(
    store: &DbStore,
    grant: &CodeExternalGrant,
    channel: &str,
    preferences: &ChannelPreferences,
) -> Result<(), ServerError> {
    validate_channel(grant, channel)?;
    preferences.validate()?;
    store
        .set_setting(
            &key(grant, channel),
            &serde_json::to_value(preferences)
                .map_err(|error| ServerError::internal(error.to_string()))?,
        )
        .await?;
    Ok(())
}

/// Freeze the channel instructions before accepting the first turn.
pub async fn freeze_instructions(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
    instructions: &str,
) -> Result<(), ServerError> {
    if tidebreak_core::db::code::get_session(store, owner, session)
        .await?
        .is_none()
    {
        return Err(ServerError::not_found("code session not found"));
    }
    store
        .set_setting(
            &format!("code.session.{session}.channel_instructions"),
            &serde_json::json!(instructions),
        )
        .await?;
    Ok(())
}

/// Read only after resolving the session owner. Empty means no channel additions.
pub async fn session_instructions(
    store: &DbStore,
    owner: &OwnerId,
    session: SessionId,
) -> Result<String, ServerError> {
    if tidebreak_core::db::code::get_session(store, owner, session)
        .await?
        .is_none()
    {
        return Err(ServerError::not_found("code session not found"));
    }
    Ok(store
        .get_setting(&format!("code.session.{session}.channel_instructions"))
        .await?
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_reject_unbounded_and_control_content() {
        assert!(ChannelPreferences {
            instructions: "a".repeat(MAX_CHANNEL_INSTRUCTIONS + 1),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(ChannelPreferences {
            model: Some("bad\nmodel".into()),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(ChannelPreferences {
            instructions: "Answer briefly.".into(),
            ..Default::default()
        }
        .validate()
        .is_ok());
    }
}
