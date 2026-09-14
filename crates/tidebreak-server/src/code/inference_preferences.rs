//! Slack inference preferences never replace session, repository, or tool authority.
use super::runtime::CodeRuntime;
use crate::error::ServerError;
use crate::obo_gateway::external::InferenceSponsorshipConsent;
use serde::{Deserialize, Serialize};
use tidebreak_core::{CodeExternalGrant, CodeGrantId, HarnessKind, OwnerId, Store};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelSubscriptionPreference {
    #[default]
    PreferStarterSubscription,
    GatewayDefault,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DmSubscriptionPreference {
    #[default]
    PreferOwnedSubscription,
    GatewayDefault,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PersonalInferencePreferences {
    pub dm_subscription_preference: DmSubscriptionPreference,
    pub channel_sponsorship_enabled: bool,
    pub consent_version: Option<u32>,
}
#[derive(Clone, Debug, Serialize)]
pub struct PersonalInferenceSnapshot {
    #[serde(flatten)]
    pub preferences: PersonalInferencePreferences,
    pub inference_sponsorship_supported: bool,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationStarter {
    pub external_identity: String,
    #[serde(default)]
    pub personal_grant_id: Option<CodeGrantId>,
}

fn authority_error(error: tidebreak_core::AgentError) -> ServerError {
    match error {
        tidebreak_core::AgentError::SignInRequired(message) => {
            ServerError::conflict_kind("external_reconnect_required", message)
        }
        tidebreak_core::AgentError::InvalidTarget(message) => ServerError::forbidden(message),
        other => ServerError::from(other),
    }
}

fn preference_key(grant: &CodeExternalGrant) -> String {
    format!(
        "code.personal.inference:{}",
        serde_json::json!([
            grant.owner.as_str(),
            grant.channel_kind,
            grant.workspace_identity,
            grant.external_identity
        ])
    )
}
async fn dm_preference(
    runtime: &CodeRuntime,
    grant: &CodeExternalGrant,
) -> Result<DmSubscriptionPreference, ServerError> {
    runtime
        .db
        .get_setting(&preference_key(grant))
        .await?
        .map(serde_json::from_value)
        .transpose()
        .map(|value| value.unwrap_or_default())
        .map_err(|e| ServerError::internal(e.to_string()))
}

impl CodeRuntime {
    pub async fn inference_sponsorship_supported(&self) -> Result<bool, ServerError> {
        match self
            .harness_llm()
            .and_then(|r| r.external_delegations().cloned())
        {
            Some(external) => Ok(external.supports_inference_sponsorship().await?),
            None => Ok(false),
        }
    }
    pub async fn personal_inference_preferences(
        &self,
        owner: &OwnerId,
        id: CodeGrantId,
    ) -> Result<PersonalInferenceSnapshot, ServerError> {
        let grant = self.personal_preference_grant(owner, id).await?;
        let supported = self.inference_sponsorship_supported().await?;
        let consent = if supported {
            self.harness_llm()
                .and_then(|r| r.external_delegations().cloned())
                .expect("capability has gateway")
                .sponsorship_consent(owner, id)
                .await
                .map_err(authority_error)?
        } else {
            InferenceSponsorshipConsent::default()
        };
        Ok(PersonalInferenceSnapshot {
            preferences: PersonalInferencePreferences {
                dm_subscription_preference: dm_preference(self, &grant).await?,
                channel_sponsorship_enabled: consent.enabled,
                consent_version: consent.consent_version,
            },
            inference_sponsorship_supported: supported,
        })
    }
    async fn personal_preference_grant(
        &self,
        owner: &OwnerId,
        id: CodeGrantId,
    ) -> Result<CodeExternalGrant, ServerError> {
        tidebreak_core::db::code::get_external_grant(&self.db, owner, id)
            .await?
            .filter(|g| {
                !g.kind.is_workspace() && g.channel_kind == "slack" && g.revoked_at.is_none()
            })
            .ok_or_else(|| ServerError::not_found("personal Slack connection not found"))
    }
    pub async fn set_personal_inference_preferences(
        &self,
        owner: &OwnerId,
        id: CodeGrantId,
        preferences: &PersonalInferencePreferences,
        lease: Option<&crate::auth::GatewayAuthLease>,
    ) -> Result<PersonalInferenceSnapshot, ServerError> {
        let grant = self.personal_preference_grant(owner, id).await?;
        let consent = InferenceSponsorshipConsent {
            enabled: preferences.channel_sponsorship_enabled,
            consent_version: preferences.consent_version,
        };
        consent
            .validate()
            .map_err(|error| ServerError::bad_request(error.to_string()))?;
        if !self.inference_sponsorship_supported().await? {
            return Err(ServerError::conflict_kind(
                "inference_sponsorship_unsupported",
                "Update Gateway to configure subscription preferences.",
            ));
        }
        let external = self
            .harness_llm()
            .and_then(|r| r.external_delegations().cloned())
            .expect("capability has gateway");
        lease
            .ok_or_else(|| {
                ServerError::unauthorized("Sign in to save your subscription preference.")
            })?
            .update_inference_sponsorship(&external, owner, id, &consent)
            .await
            .map_err(authority_error)?;
        self.db
            .set_setting(
                &preference_key(&grant),
                &serde_json::to_value(preferences.dm_subscription_preference)
                    .map_err(|e| ServerError::internal(e.to_string()))?,
            )
            .await?;
        Ok(PersonalInferenceSnapshot {
            preferences: preferences.clone(),
            inference_sponsorship_supported: true,
        })
    }
}

pub use tidebreak_core::code::inference::PreparedSessionInference;

pub async fn prepare(
    runtime: &CodeRuntime,
    grant: &CodeExternalGrant,
    starter: Option<&ConversationStarter>,
    harness: HarnessKind,
    preference: ChannelSubscriptionPreference,
) -> Result<PreparedSessionInference, ServerError> {
    if let Some(starter) = starter {
        if starter.external_identity.trim().is_empty()
            || starter.external_identity.len() > 128
            || starter
                .external_identity
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(ServerError::bad_request(
                "the conversation starter has an invalid identity",
            ));
        }
        if !grant.kind.is_workspace()
            && (starter.external_identity != grant.external_identity
                || starter.personal_grant_id.is_some_and(|id| id != grant.id))
        {
            return Err(ServerError::bad_request(
                "a personal conversation takes its starter from its connection",
            ));
        }
    }
    let identity = if grant.kind.is_workspace() {
        starter.map(|s| s.external_identity.clone())
    } else {
        Some(grant.external_identity.clone())
    };
    // Explicit connection proof must be valid even when the preference is disabled.
    let explicit_person = match starter.and_then(|s| s.personal_grant_id) {
        Some(id) => tidebreak_core::db::code::personal_inference_grant_all_owners(
            &runtime.db,
            &grant.workspace_identity,
            identity
                .as_deref()
                .expect("a supplied starter has an identity"),
            Some(id),
        )
        .await
        .map_err(authority_error)?,
        None => None,
    };
    if let Some(person) = &explicit_person {
        let external = runtime
            .harness_llm()
            .and_then(|r| r.external_delegations().cloned())
            .ok_or_else(|| {
                ServerError::bad_request(
                    "the supplied personal connection has no Gateway delegation",
                )
            })?;
        external
            .personal_delegation_id(&person.owner, person.id)
            .await
            .map_err(authority_error)?;
    }
    let mut prepared = PreparedSessionInference {
        inherited: None,
        starter: identity.clone(),
        supported: false,
        personal: None,
        reason: None,
    };
    if runtime
        .remote_sessions()
        .is_some_and(|remote| !remote.settings.embedded_engine_registration)
        || !matches!(harness, HarnessKind::ClaudeCode | HarnessKind::Codex)
        || !runtime.inference_sponsorship_supported().await?
    {
        prepared.reason = Some("unsupported".into());
        return Ok(prepared);
    }
    prepared.supported = true;
    let prefer = if grant.kind.is_workspace() {
        preference == ChannelSubscriptionPreference::PreferStarterSubscription
    } else {
        dm_preference(runtime, grant).await? == DmSubscriptionPreference::PreferOwnedSubscription
    };
    if !prefer {
        prepared.reason = Some("connection_default".into());
        return Ok(prepared);
    }
    let person = if explicit_person.is_some() {
        explicit_person
    } else if grant.kind.is_workspace() {
        match identity {
            Some(identity) => tidebreak_core::db::code::personal_inference_grant_all_owners(
                &runtime.db,
                &grant.workspace_identity,
                &identity,
                starter.and_then(|s| s.personal_grant_id),
            )
            .await
            .map_err(authority_error)?,
            None => None,
        }
    } else {
        Some(grant.clone())
    };
    let Some(person) = person else {
        prepared.reason = Some("personal_connection_unavailable".into());
        return Ok(prepared);
    };
    let external = runtime
        .harness_llm()
        .and_then(|r| r.external_delegations().cloned())
        .expect("capability has gateway");
    if grant.kind.is_workspace()
        && !external
            .sponsorship_consent(&person.owner, person.id)
            .await
            .map_err(authority_error)?
            .enabled
    {
        prepared.reason = Some("channel_sponsorship_not_enabled".into());
        return Ok(prepared);
    }
    let delegation = external
        .personal_delegation_id(&person.owner, person.id)
        .await
        .map_err(authority_error)?;
    prepared.personal = Some((person.owner, person.id, delegation));
    Ok(prepared)
}
