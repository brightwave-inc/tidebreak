//! A conversation's inference preference, separate from its execution authority.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Private host-to-Gateway contract. This never grants model or tool access.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "preference", rename_all = "snake_case", deny_unknown_fields)]
pub enum InferenceSponsor {
    ExecutionDefault {
        inference_scope_id: Uuid,
    },
    PreferOwnedSubscription {
        inference_scope_id: Uuid,
        external_delegation_id: Uuid,
    },
}

impl InferenceSponsor {
    pub fn validate(&self) -> crate::Result<()> {
        let valid = match self {
            Self::ExecutionDefault { inference_scope_id } => !inference_scope_id.is_nil(),
            Self::PreferOwnedSubscription {
                inference_scope_id,
                external_delegation_id,
            } => !inference_scope_id.is_nil() && !external_delegation_id.is_nil(),
        };
        if valid {
            Ok(())
        } else {
            Err(crate::AgentError::InvalidTarget(
                "inference identities must not be nil".into(),
            ))
        }
    }
}

/// Captured before the session can execute. A child keeps the root's selection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionInference {
    pub root_session_id: crate::SessionId,
    pub starter_external_identity: Option<String>,
    /// Absent on a Gateway that does not support the private extension.
    pub sponsor: Option<InferenceSponsor>,
    /// The personal connection to revalidate before spending a selected subscription.
    pub personal_grant_id: Option<super::CodeGrantId>,
    pub personal_owner: Option<crate::OwnerId>,
    pub reason: Option<String>,
}

/// Gateway reports each provider only after it resolves the actual model route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct InferenceResolution {
    pub scope_id: Uuid,
    pub provider: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
    #[serde(default, skip_serializing)]
    #[ts(skip)]
    pub subscription_label: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolution_accepts_legacy_labels_without_forwarding_them() {
        let resolution:InferenceResolution=serde_json::from_value(serde_json::json!({
            "scope_id":Uuid::new_v4(),"provider":"provider","source":"owned_subscription", "subscription_label":"Personal account"
        })).unwrap();
        assert_eq!(
            resolution.subscription_label.as_deref(),
            Some("Personal account")
        );
        assert!(serde_json::to_value(resolution)
            .unwrap()
            .get("subscription_label")
            .is_none());
    }

    #[test]
    fn sponsor_contract_refuses_incomplete_or_ambiguous_authority() {
        let scope = Uuid::new_v4();
        for body in [
            serde_json::json!({"preference":"prefer_owned_subscription","inference_scope_id":scope}),
            serde_json::json!({"preference":"execution_default","inference_scope_id":scope,"external_delegation_id":Uuid::new_v4()}),
            serde_json::json!({"preference":"execution_default","inference_scope_id":scope,"user_id":"someone"}),
        ] {
            assert!(serde_json::from_value::<InferenceSponsor>(body).is_err());
        }
        assert!(InferenceSponsor::ExecutionDefault {
            inference_scope_id: Uuid::nil()
        }
        .validate()
        .is_err());
    }
}

/// A selection prepared from verified consent; the binding transaction supplies its root UUID.
#[derive(Clone, Debug)]
pub struct PreparedSessionInference {
    pub inherited: Option<SessionInference>,
    pub starter: Option<String>,
    pub supported: bool,
    pub personal: Option<(crate::OwnerId, super::CodeGrantId, super::CodeHandshakeId)>,
    pub reason: Option<String>,
}
impl PreparedSessionInference {
    pub fn inherit(selection: SessionInference) -> Self {
        Self {
            inherited: Some(selection),
            starter: None,
            supported: false,
            personal: None,
            reason: None,
        }
    }
    pub fn freeze(&self, session: crate::SessionId) -> SessionInference {
        if let Some(inherited) = &self.inherited {
            return inherited.clone();
        }
        SessionInference {
            root_session_id: session,
            starter_external_identity: self.starter.clone(),
            sponsor: self.supported.then(|| match &self.personal {
                Some((_, _, delegation)) => InferenceSponsor::PreferOwnedSubscription {
                    inference_scope_id: session.0,
                    external_delegation_id: delegation.0,
                },
                None => InferenceSponsor::ExecutionDefault {
                    inference_scope_id: session.0,
                },
            }),
            personal_grant_id: self.personal.as_ref().map(|p| p.1),
            personal_owner: self.personal.as_ref().map(|p| p.0.clone()),
            reason: self.reason.clone(),
        }
    }
}
