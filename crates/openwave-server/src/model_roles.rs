//! Named model roles.
//!
//! A role names a job the product needs a model for, so "use something cheap
//! for this" has somewhere to live. [`ModelRole::Chat`] is the model a
//! foreground turn runs on — the user's choice, unchanged by this module.
//! [`ModelRole::Utility`] is for work the user did not ask for: compacting a
//! transcript today, and other maintenance later. Two roles is deliberate;
//! adding a third is an entry in the tables below rather than a refactor.
//!
//! Each role's explicit selection is stored under `model.<role>`, matching the
//! `provider.<kind>` convention — except `chat`, whose selection has lived under
//! a bare `model` key since before roles existed and keeps it.
//!
//! A role also carries an ordered list of curated defaults, where order is both
//! preference and failover: [`resolve`] walks it and takes the first entry whose
//! provider is enabled and credentialed, so an Anthropic-only install and an
//! OpenAI-only install each get a sensible model without configuring one.
//! Resolution reads settings and credentials on every call rather than at boot,
//! so enabling a provider takes effect on the next turn instead of the next
//! launch. And it degrades instead of failing: no resolvable model means the
//! caller skips its work, never that a turn blocks or fails.

use serde::{Deserialize, Serialize};

use openwave_core::{
    AgentError, ProviderId, ReasoningEffort, Result, SecretProvider, Store, UtilityModel,
};

use crate::providers::{self, ResolvedModelPolicy};

/// The roles the product resolves a model for.
///
/// `#[non_exhaustive]` so a new role can land without breaking wire clients that
/// match on the string form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ModelRole {
    /// The model a foreground turn runs on.
    Chat,
    /// The model background work the user did not ask for runs on.
    Utility,
}

/// The `utility` role's ordered defaults.
///
/// One entry per provider that serves curated models, each that provider's
/// cheapest current row. Anthropic leads because it is the provider the product
/// defaults to for chat, so the common single-provider install resolves on the
/// first step; the rest follow so an install credentialed elsewhere still gets
/// a utility model. Any entry is a good answer for a multi-provider install,
/// so the order only has to be stable and explainable.
const UTILITY_DEFAULTS: &[&str] = &[
    "anthropic::claude-haiku-4-5-20251001",
    "openai::gpt-5.4-nano",
    "gemini::gemini-3.5-flash-lite",
    "vertex::gemini-3.5-flash-lite",
    "bedrock::anthropic.claude-sonnet-5",
    "fireworks::accounts/fireworks/models/deepseek-v4-flash",
    "together::deepseek-ai/DeepSeek-V4-Flash-0731",
];

impl ModelRole {
    /// All roles, in display order.
    pub const ALL: &'static [ModelRole] = &[ModelRole::Chat, ModelRole::Utility];

    /// Wire/path form (`chat`, `utility`).
    pub fn as_str(self) -> &'static str {
        match self {
            ModelRole::Chat => "chat",
            ModelRole::Utility => "utility",
        }
    }

    /// Parse a path segment into a role.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "chat" => Some(Self::Chat),
            "utility" => Some(Self::Utility),
            _ => None,
        }
    }

    /// Store setting key holding this role's explicit selection.
    pub fn setting_key(self) -> &'static str {
        match self {
            // Pre-roles key, kept so existing installs keep their default.
            ModelRole::Chat => "model",
            ModelRole::Utility => "model.utility",
        }
    }

    /// Ordered preference list of curated selection keys for this role.
    ///
    /// Empty for `chat`: a conversation runs on what the user picked, and its
    /// last resort is the model this process launched with — process state no
    /// registry list can name. Silently moving a foreground turn to another
    /// provider is exactly what we don't want there.
    pub fn defaults(self) -> &'static [&'static str] {
        match self {
            ModelRole::Chat => &[],
            ModelRole::Utility => UTILITY_DEFAULTS,
        }
    }

    /// The reasoning effort work in this role asks for, before the model's own
    /// accepted range clamps it.
    ///
    /// `None` for `chat`, where effort is the user's per-chat choice rather than
    /// a property of the role.
    pub fn reasoning_effort(self) -> Option<ReasoningEffort> {
        match self {
            ModelRole::Chat => None,
            // Compaction is summarization of text already written: buy speed,
            // not thought. Models that reject the level clamp up or drop it.
            ModelRole::Utility => Some(ReasoningEffort::None),
        }
    }

    /// Whether `model` carries the capabilities this role requires.
    ///
    /// Foreground chat accepts every selectable model. Utility work emits a
    /// strict checkpoint schema, so a model that cannot enforce that response
    /// shape is skipped instead of letting a provider rejection silently turn
    /// compaction into `None`.
    pub fn supports_model(self, model: &ResolvedModelPolicy) -> bool {
        match self {
            ModelRole::Chat => true,
            ModelRole::Utility => model.supports_structured_output,
        }
    }
}

impl std::fmt::Display for ModelRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The explicit selection stored for `role`, if the user set one.
pub async fn read_selection(store: &dyn Store, role: ModelRole) -> Result<Option<String>> {
    Ok(store
        .get_setting(role.setting_key())
        .await?
        .and_then(|value| value.as_str().map(str::to_owned)))
}

/// Persist `selection` for `role`. `None` clears it back to automatic, stored as
/// JSON null so [`read_selection`] reads it back as unset.
pub async fn write_selection(
    store: &dyn Store,
    role: ModelRole,
    selection: Option<&str>,
) -> Result<()> {
    let value = selection.map_or(serde_json::Value::Null, |key| serde_json::json!(key));
    store.set_setting(role.setting_key(), &value).await
}

/// Resolve the model `role` runs on right now: the user's selection when its
/// provider can serve it, else the first usable entry in the role's ordered
/// defaults.
///
/// On a managed profile the curated defaults name BYOK providers the policy
/// has locked out, so the ordered walk is over the gateway's own entitled
/// models instead — otherwise background work would silently stop happening
/// for every managed install. The walk is the same shape either way: first
/// usable wins, and nothing usable degrades to `None`.
///
/// `None` means this install has no model for the role. Callers skip the work.
pub async fn resolve(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    os_policy: &dyn crate::managed_policy::OsPolicySource,
    role: ModelRole,
) -> Result<Option<ResolvedModelPolicy>> {
    let managed = crate::managed_policy::resolve(store, os_policy).await?;
    if let Some(selection) = read_selection(store, role).await? {
        // A selection whose provider has since been disabled or lost its
        // credential, or whose capabilities no longer satisfy the role, falls
        // through to the defaults rather than issuing a request that would
        // fail or silently skip the work.
        if let Some(policy) = usable_policy(store, secrets, &managed, &selection).await? {
            if role.supports_model(&policy) {
                return Ok(Some(policy));
            }
        }
    }
    // `chat` has no default list on purpose — its last resort is process
    // state, not a registry row — so the managed substitution applies only to
    // roles that walk one at all.
    let defaults: Vec<String> = if managed.managed && !role.defaults().is_empty() {
        gateway_defaults(store, &managed).await?
    } else {
        role.defaults()
            .iter()
            .map(|key| (*key).to_owned())
            .collect()
    };
    for key in defaults {
        if let Some(policy) = usable_policy(store, secrets, &managed, &key).await? {
            if role.supports_model(&policy) {
                return Ok(Some(policy));
            }
        }
    }
    Ok(None)
}

/// A managed profile's ordered role defaults: the models the gateway has
/// synced as entitled, smallest context window first.
///
/// The curated lists these stand in for name each provider's *cheapest* row,
/// because a role like `utility` runs work the user did not ask for and must
/// not bill it to a flagship. The gateway describes entitlement, not price,
/// so context window is the proxy available — it tracks model tier closely
/// enough to keep that intent, and ties keep the gateway's own order.
async fn gateway_defaults(
    store: &dyn Store,
    policy: &crate::managed_policy::ManagedPolicy,
) -> Result<Vec<String>> {
    let mut models = providers::gateway_models(store, policy).await?;
    models.sort_by_key(|model| model.context_window);
    Ok(selection_keys(&models))
}

/// The entitled models in the gateway's own listed order — the order the
/// composer picker renders them, and therefore the chat role's fallback
/// order. Contrast [`gateway_defaults`], which re-sorts the same list
/// cheapest-first for background work.
async fn gateway_listed(
    store: &dyn Store,
    policy: &crate::managed_policy::ManagedPolicy,
) -> Result<Vec<String>> {
    Ok(selection_keys(
        &providers::gateway_models(store, policy).await?,
    ))
}

fn selection_keys(models: &[providers::CustomModelConfig]) -> Vec<String> {
    models
        .iter()
        .map(|model| {
            crate::model_registry::selection_key(providers::ProviderKind::ModelGateway, &model.id)
        })
        .collect()
}

/// The chat role's effective policy for `selection` under `managed`: the
/// selection itself while its provider can serve it, else the first entitled
/// gateway model.
///
/// This is one function on purpose. The roles read labels the default with it
/// and the turn-accept seam freezes a model from it, so the model a client
/// shows for "default" and the model the next turn actually runs cannot
/// diverge. Unmanaged, the selection resolves as-is, dead or not: the open
/// experience refuses loudly at send rather than silently moving a foreground
/// turn to another provider.
pub async fn effective_chat_policy(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    managed: &crate::managed_policy::ManagedPolicy,
    selection: &str,
) -> Result<Option<ResolvedModelPolicy>> {
    let resolved = providers::resolve_model_policy(store, selection, true).await?;
    if !managed.managed {
        return Ok(resolved);
    }
    if let Some(policy) = resolved {
        if providers::provider_is_usable(store, secrets, policy.provider, managed).await? {
            return Ok(Some(policy));
        }
    }
    for key in gateway_listed(store, managed).await? {
        if let Some(policy) = usable_policy(store, secrets, managed, &key).await? {
            return Ok(Some(policy));
        }
    }
    Ok(None)
}

/// Resolve `selection` only if its provider is usable under `managed`.
async fn usable_policy(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    managed: &crate::managed_policy::ManagedPolicy,
    selection: &str,
) -> Result<Option<ResolvedModelPolicy>> {
    let Some(policy) = providers::resolve_model_policy(store, selection, false).await? else {
        return Ok(None);
    };
    Ok(providers::model_is_usable(store, secrets, &policy, managed)
        .await?
        .then_some(policy))
}

/// Resolve the `utility` role into the shape the agent carries for one turn.
///
/// `None` means there is no utility model, and the agent skips maintenance work
/// instead of billing it to the conversation's model.
pub async fn resolve_utility_model(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    os_policy: &dyn crate::managed_policy::OsPolicySource,
) -> Result<Option<UtilityModel>> {
    let role = ModelRole::Utility;
    let Some(policy) = resolve(store, secrets, os_policy, role).await? else {
        return Ok(None);
    };
    Ok(Some(UtilityModel {
        provider: Some(ProviderId::new(policy.provider.as_str())),
        model: policy.id.clone(),
        reasoning_model: policy.supports_reasoning,
        // Same reconciliation `apply_model_policy` performs for a foreground
        // turn: a level this model does not accept degrades to the nearest one
        // it does, or is dropped when it exposes no effort control.
        reasoning_effort: role
            .reasoning_effort()
            .and_then(|effort| effort.clamp_to(&policy.reasoning_efforts)),
        context_window: usize::try_from(policy.context_window)
            .map_err(|_| AgentError::config("model context window is unsupported"))?,
    }))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use openwave_core::DbStore;

    use super::*;
    use crate::model_registry;
    use crate::providers::{CustomModelConfig, ProviderConfig, ProviderKind};

    #[derive(Default)]
    struct TestSecrets(Mutex<HashMap<String, String>>);

    #[async_trait::async_trait]
    impl SecretProvider for TestSecrets {
        async fn get_secret(&self, key: &str) -> Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }
        async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
            self.0.lock().unwrap().insert(key.into(), value.into());
            Ok(())
        }
        async fn delete_secret(&self, key: &str) -> Result<()> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
    }

    struct RegionalVertexUtilitySecrets;

    #[async_trait::async_trait]
    impl SecretProvider for RegionalVertexUtilitySecrets {
        async fn get_secret(&self, key: &str) -> Result<Option<String>> {
            if key == ProviderKind::Vertex.credential_key() {
                panic!("regional Vertex must be rejected before its credential is read");
            }
            if key == ProviderKind::Openai.credential_key() {
                return Ok(Some(
                    serde_json::to_string(&crate::providers::ProviderCredential::api_key(
                        "sk-openai",
                    ))
                    .unwrap(),
                ));
            }
            Ok(None)
        }

        async fn set_secret(&self, key: &str, _value: &str) -> Result<()> {
            panic!("test must not write secret `{key}`")
        }

        async fn delete_secret(&self, key: &str) -> Result<()> {
            panic!("test must not delete secret `{key}`")
        }
    }

    #[test]
    fn every_role_default_names_a_curated_model() {
        for &role in ModelRole::ALL {
            for key in role.defaults() {
                let (provider, id) = model_registry::parse_selection_key(key)
                    .unwrap_or_else(|| panic!("{role}'s default `{key}` is not a selection key"));
                assert!(
                    model_registry::find_for(provider, id).is_some(),
                    "{role}'s default `{key}` is not in the model registry",
                );
            }
        }
    }

    /// A user credentialed on one provider must still get a utility model, or
    /// the work that depends on it silently stops happening for them.
    #[test]
    fn the_utility_defaults_cover_every_provider_that_serves_curated_models() {
        for &provider in ProviderKind::ALL {
            if model_registry::models_for(provider).next().is_none() {
                // Providers whose models come from their own configuration — a
                // custom compatible endpoint, the gateway — have nothing a
                // curated list can name. Those installs point `model.utility`
                // at one of the models they configured.
                continue;
            }
            assert!(
                ModelRole::Utility.defaults().iter().any(|key| {
                    model_registry::parse_selection_key(key)
                        .is_some_and(|(kind, _)| kind == provider)
                }),
                "a user credentialed only on {provider} has no default utility model",
            );
        }
    }

    /// The compatible adapter always sends a strict JSON Schema for utility
    /// work. Keep every default on a row whose model contract promises that
    /// output; function calling is separate and cannot stand in for it.
    #[test]
    fn utility_defaults_support_strict_structured_output() {
        for key in ModelRole::Utility.defaults() {
            let (provider, id) = model_registry::parse_selection_key(key)
                .unwrap_or_else(|| panic!("utility default `{key}` is not a selection key"));
            assert!(
                model_registry::find_for(provider, id)
                    .is_some_and(model_registry::ModelSpec::supports_structured_output),
                "utility default `{key}` cannot enforce its strict structured response",
            );
        }
    }

    /// Utility resolution distinguishes function tools from structured output:
    /// an incompatible pin falls through, while chat-only Kimi K3 remains a
    /// valid utility model because its strict response contract is independent.
    #[tokio::test]
    async fn utility_resolution_uses_the_structured_output_contract() {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("incompatible-pin.db").display()
            ))
            .await
            .unwrap(),
        );
        let secrets: Arc<dyn SecretProvider> = Arc::new(TestSecrets::default());
        let os_policy = crate::managed_policy::NoOsPolicy;

        providers::write_credential(
            &*secrets,
            ProviderKind::Together,
            &crate::providers::ProviderCredential::api_key("together-key"),
        )
        .await
        .unwrap();
        providers::write_config(
            &*store,
            ProviderKind::Together,
            &ProviderConfig {
                enabled: true,
                ..ProviderConfig::disabled()
            },
        )
        .await
        .unwrap();
        write_selection(
            &*store,
            ModelRole::Utility,
            Some("together::thinkingmachines/Inkling-Small"),
        )
        .await
        .unwrap();

        let utility = resolve_utility_model(&*store, &*secrets, &os_policy)
            .await
            .unwrap()
            .expect("the capable Together default keeps utility work enabled");
        assert_eq!(utility.model, "deepseek-ai/DeepSeek-V4-Flash-0731");
        assert_eq!(
            utility.provider,
            Some(ProviderId::new(ProviderKind::Together.as_str()))
        );

        write_selection(
            &*store,
            ModelRole::Utility,
            Some("together::moonshotai/Kimi-K3"),
        )
        .await
        .unwrap();
        let utility = resolve_utility_model(&*store, &*secrets, &os_policy)
            .await
            .unwrap()
            .expect("Kimi K3 supports the strict utility response contract");
        assert_eq!(utility.model, "moonshotai/Kimi-K3");
        assert_eq!(
            utility.provider,
            Some(ProviderId::new(ProviderKind::Together.as_str()))
        );
    }

    #[tokio::test]
    async fn utility_selection_skips_a_stored_regional_vertex_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory
                    .path()
                    .join("regional-vertex-utility.db")
                    .display()
            ))
            .await
            .unwrap(),
        );
        let secrets: Arc<dyn SecretProvider> = Arc::new(RegionalVertexUtilitySecrets);
        let os_policy = crate::managed_policy::NoOsPolicy;

        providers::write_config(
            &*store,
            ProviderKind::Vertex,
            &ProviderConfig {
                enabled: true,
                vertex_location: Some("us-east5".into()),
                ..ProviderConfig::disabled()
            },
        )
        .await
        .unwrap();
        providers::write_config(
            &*store,
            ProviderKind::Openai,
            &ProviderConfig {
                enabled: true,
                ..ProviderConfig::disabled()
            },
        )
        .await
        .unwrap();
        write_selection(
            &*store,
            ModelRole::Utility,
            Some("vertex::gemini-3.5-flash-lite"),
        )
        .await
        .unwrap();

        let utility = resolve_utility_model(&*store, &*secrets, &os_policy)
            .await
            .unwrap()
            .expect("utility falls through to another usable provider");
        assert_eq!(utility.provider, Some(ProviderId::new("openai")));
        assert_eq!(utility.model, "gpt-5.4-nano");
    }

    /// Utility work on a managed profile: the curated defaults all name BYOK
    /// providers the policy locked out, so before this the role resolved to
    /// nothing and chat titling silently stopped. It now walks the gateway's
    /// entitled models — and still degrades to `None`, exactly as an install
    /// with no model configured does, when the gateway has none to offer.
    #[tokio::test]
    async fn a_managed_profile_resolves_the_utility_role_to_a_gateway_model() {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("roles.db").display()
            ))
            .await
            .unwrap(),
        );
        let secrets: Arc<dyn SecretProvider> = Arc::new(TestSecrets::default());
        let os_policy = crate::managed_policy::NoOsPolicy;

        // A credentialed BYOK provider: the role resolves to its curated
        // default while the profile is open.
        providers::write_credential(
            &*secrets,
            ProviderKind::Anthropic,
            &crate::providers::ProviderCredential::api_key("sk-anthropic"),
        )
        .await
        .unwrap();
        providers::write_config(
            &*store,
            ProviderKind::Anthropic,
            &ProviderConfig {
                enabled: true,
                ..ProviderConfig::disabled()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            resolve_utility_model(&*store, &*secrets, &os_policy)
                .await
                .unwrap()
                .map(|model| model.model),
            Some("claude-haiku-4-5-20251001".to_string())
        );

        // Managed, with no gateway session yet: the BYOK key is locked out and
        // there is nothing to fall back to, so consumers skip their work.
        crate::managed_policy::provision(&*store, "https://corp.gateway")
            .await
            .unwrap();
        assert!(resolve_utility_model(&*store, &*secrets, &os_policy)
            .await
            .unwrap()
            .is_none());

        // Signed in, with entitled models synced: the walk lands on the
        // gateway's first usable model.
        let credentials: crate::connectors::GatewayCredentials =
            serde_json::from_value(serde_json::json!({
                "base_url": "https://corp.gateway/",
                "installation_id": "install-1",
                "user_id": "user-1",
                "refresh_token": "mg_rt_seed",
                "access_tokens": {}
            }))
            .unwrap();
        crate::connectors::CredentialVault::new(secrets.clone())
            .save(&credentials)
            .await
            .unwrap();
        providers::write_gateway_snapshot(
            &*store,
            &providers::GatewayModelSnapshot {
                gateway_url: "https://corp.gateway/".to_string(),
                // Listed flagship-first, as a gateway well might: the walk
                // must not bill background work to the biggest model it is
                // entitled to.
                models: vec![
                    CustomModelConfig {
                        id: "gateway-flagship".to_string(),
                        upstream_id: Some("claude-opus-5".to_string()),
                        display_name: Some("Gateway Flagship".to_string()),
                        context_window: 1_000_000,
                        max_output_tokens: 64_000,
                        ..Default::default()
                    },
                    CustomModelConfig {
                        id: "gateway-haiku".to_string(),
                        upstream_id: Some("claude-haiku-4-5-20251001".to_string()),
                        display_name: Some("Gateway Haiku".to_string()),
                        context_window: 200_000,
                        max_output_tokens: 8_192,
                        ..Default::default()
                    },
                ],
                model_protocols: Default::default(),
            },
        )
        .await
        .unwrap();
        let utility = resolve_utility_model(&*store, &*secrets, &os_policy)
            .await
            .unwrap()
            .expect("a managed profile resolves the utility role to a gateway model");
        assert_eq!(
            utility.model, "gateway-haiku",
            "the cheapest entitled model, not the first listed"
        );
        assert_eq!(
            utility.provider,
            Some(ProviderId::new(ProviderKind::ModelGateway.as_str()))
        );
    }
}
