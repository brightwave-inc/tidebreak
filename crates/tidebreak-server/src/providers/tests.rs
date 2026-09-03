use super::*;
use std::collections::HashMap;
use std::sync::Mutex;
use tidebreak_core::DbStore;

#[derive(Default)]
struct TestSecrets(Mutex<HashMap<String, String>>);

#[async_trait::async_trait]
impl SecretProvider for TestSecrets {
    async fn get_secret(&self, key: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }

    async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    async fn delete_secret(&self, key: &str) -> Result<()> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}

#[test]
fn member_catalog_reasoning_efforts_match_generated_engine_ids() {
    let catalog = crate::connectors::GatewayCatalog {
        models: vec![crate::connectors::GatewayCatalogModel {
            id: "glm-5.3".into(),
            name: "GLM 5.3".into(),
            protocols: vec!["openai_responses".into()],
            aliases: vec!["zai-glm-5.3".into()],
            supports_tools: true,
            supports_vision: false,
            supported_reasoning_efforts: Some(vec![
                ReasoningEffort::Low,
                ReasoningEffort::High,
                ReasoningEffort::Max,
            ]),
            context_window: Some(200_000),
            max_output_tokens: Some(16_000),
            provider_name: "Z.ai".into(),
        }],
        apps: Vec::new(),
    };
    let MemberCatalogModels {
        models,
        model_protocols,
        model_reasoning_efforts,
    } = member_catalog_models(catalog);
    let snapshot = GatewayModelSnapshot {
        gateway_url: "https://gateway.example/".into(),
        installation_id: None,
        models,
        model_protocols,
        model_reasoning_efforts,
        member_catalog: Some("v1".into()),
        catalog_etag: None,
    };
    let expected = [
        ReasoningEffort::Low,
        ReasoningEffort::High,
        ReasoningEffort::Max,
    ];
    for selection in [
        "glm-5.3",
        "model-gateway/glm-5.3",
        "model-gateway-model-gateway/glm-5.3",
        "model-gateway-default/zai-glm-5.3",
    ] {
        assert_eq!(
            gateway_reasoning_efforts_for_model(&snapshot, selection),
            Some(expected.as_slice()),
            "{selection}"
        );
    }
}

#[test]
fn gateway_efforts_preserve_a_codex_rows_narrower_ladder() {
    let listed = [
        ReasoningEffort::Low,
        ReasoningEffort::High,
        ReasoningEffort::Ultra,
    ];
    let gateway = [
        ReasoningEffort::Low,
        ReasoningEffort::High,
        ReasoningEffort::Max,
    ];

    assert_eq!(
        effective_gateway_reasoning_efforts(false, Some(&listed), ReasoningEffort::ALL, &gateway),
        vec![ReasoningEffort::Low, ReasoningEffort::High]
    );
    assert_eq!(
        effective_gateway_reasoning_efforts(false, None, &[], &gateway),
        gateway
    );
    assert!(effective_gateway_reasoning_efforts(false, Some(&[]), &[], &gateway).is_empty());
    assert_eq!(
        effective_gateway_reasoning_efforts(true, Some(&listed), &[], &gateway),
        gateway,
        "hosted compat rows do not overlay against Codex CLI ladders"
    );
}

async fn gateway_migration_test_store() -> (
    DbStore,
    tempfile::TempDir,
    crate::managed_policy::ManagedPolicy,
) {
    let directory = tempfile::tempdir().unwrap();
    let store = DbStore::connect(&format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("providers.db").display()
    ))
    .await
    .unwrap();
    let provisioned = crate::managed_policy::MemoryProvisionedPolicy::new();
    crate::managed_policy::provision(&*provisioned, "https://gateway.example").unwrap();
    let policy =
        crate::managed_policy::resolve(&*provisioned, &crate::managed_policy::NoOsPolicy).unwrap();
    write_gateway_snapshot(
        &store,
        &GatewayModelSnapshot {
            gateway_url: policy.gateway_url.clone().unwrap(),
            installation_id: Some("install-1".into()),
            models: vec![CustomModelConfig {
                id: "sample-claude".into(),
                upstream_id: Some("claude-opus-5".into()),
                context_window: 200_000,
                max_output_tokens: 32_000,
                ..Default::default()
            }],
            model_protocols: BTreeMap::new(),
            model_reasoning_efforts: BTreeMap::new(),
            member_catalog: Some("v1".into()),
            catalog_etag: None,
        },
    )
    .await
    .unwrap();
    (store, directory, policy)
}

async fn provider_test_store() -> (DbStore, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let store = DbStore::connect(&format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("provider-transport.db").display()
    ))
    .await
    .unwrap();
    (store, directory)
}

async fn store_chatgpt_session(secrets: &TestSecrets) {
    write_credential(secrets, ProviderKind::Openai, &ProviderCredential::Oauth {})
        .await
        .unwrap();
    secrets
        .set_secret(
            crate::connectors::CHATGPT_SECRET_KEY,
            &serde_json::json!({
                "access_token": "access",
                "refresh_token": "refresh",
                "account_id": "acct-test",
                "expires_at_unix": 4_102_444_800_u64,
            })
            .to_string(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn upgrade_enables_missing_provider_rows_for_existing_authentication() {
    let (store, _directory) = provider_test_store().await;
    let secrets = TestSecrets::default();
    store_chatgpt_session(&secrets).await;
    secrets
        .set_secret(LEGACY_ANTHROPIC_API_KEY, "legacy-anthropic-key")
        .await
        .unwrap();

    assert!(store
        .get_setting(&ProviderKind::Openai.setting_key())
        .await
        .unwrap()
        .is_none());

    migrate_legacy_provider_enablement(&store, &secrets)
        .await
        .unwrap();

    assert!(
        read_config(&store, ProviderKind::Openai)
            .await
            .unwrap()
            .enabled
    );
    assert!(
        read_config(&store, ProviderKind::Anthropic)
            .await
            .unwrap()
            .enabled
    );
    let policy = crate::managed_policy::resolve(
        &*crate::managed_policy::MemoryProvisionedPolicy::new(),
        &crate::managed_policy::NoOsPolicy,
    )
    .unwrap();
    assert!(
        provider_is_usable(&store, &secrets, ProviderKind::Openai, &policy, None)
            .await
            .unwrap()
    );
    assert!(catalog_models(&store, &secrets, &policy, None)
        .await
        .unwrap()
        .iter()
        .any(|model| model.policy.id == "gpt-5.6-sol" && model.available));
}

#[tokio::test]
async fn upgrade_preserves_an_explicitly_disabled_openai_provider() {
    let (store, _directory) = provider_test_store().await;
    let secrets = TestSecrets::default();
    store_chatgpt_session(&secrets).await;
    write_config(&store, ProviderKind::Openai, &ProviderConfig::disabled())
        .await
        .unwrap();

    migrate_legacy_provider_enablement(&store, &secrets)
        .await
        .unwrap();

    assert!(
        !read_config(&store, ProviderKind::Openai)
            .await
            .unwrap()
            .enabled
    );
}

#[tokio::test]
async fn upgrade_does_not_enable_openai_for_orphaned_oauth_tokens() {
    let (store, _directory) = provider_test_store().await;
    let secrets = TestSecrets::default();
    secrets
        .set_secret(
            crate::connectors::CHATGPT_SECRET_KEY,
            &serde_json::json!({
                "access_token": "access",
                "refresh_token": "refresh",
                "account_id": "acct-test",
                "expires_at_unix": 4_102_444_800_u64,
            })
            .to_string(),
        )
        .await
        .unwrap();

    migrate_legacy_provider_enablement(&store, &secrets)
        .await
        .unwrap();

    assert!(store
        .get_setting(&ProviderKind::Openai.setting_key())
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn provider_updates_reject_credentials_on_cleartext_endpoints() {
    let (store, _directory) = provider_test_store().await;
    let secrets = TestSecrets::default();
    let provisioned = crate::managed_policy::MemoryProvisionedPolicy::new();

    let error = update_provider(
        &store,
        &secrets,
        ProviderKind::OpenaiCompatible,
        ProviderUpdate {
            enabled: Some(true),
            base_url: Some(Some("http://127.0.0.1:1234/v1".into())),
            credential: Some(ProviderCredential::api_key("secret")),
            models: None,
        },
        &*provisioned,
        &crate::managed_policy::NoOsPolicy,
    )
    .await
    .expect_err("a reusable credential must not be sent over cleartext HTTP");
    assert!(error.message().contains("must use HTTPS"));
    assert_eq!(
        read_credential(&secrets, ProviderKind::OpenaiCompatible)
            .await
            .unwrap(),
        None,
        "validation must happen before secret storage"
    );
}

#[tokio::test]
async fn credentialless_ollama_keeps_loopback_http_support() {
    let (store, _directory) = provider_test_store().await;
    let secrets = TestSecrets::default();
    let provisioned = crate::managed_policy::MemoryProvisionedPolicy::new();

    let info = update_provider(
        &store,
        &secrets,
        ProviderKind::Ollama,
        ProviderUpdate {
            enabled: Some(true),
            base_url: Some(Some("http://localhost:11434/v1".into())),
            credential: None,
            models: None,
        },
        &*provisioned,
        &crate::managed_policy::NoOsPolicy,
    )
    .await
    .expect("credentialless loopback HTTP remains supported");
    assert_eq!(info.base_url.as_deref(), Some("http://localhost:11434/v1"));

    let error = update_provider(
        &store,
        &secrets,
        ProviderKind::Ollama,
        ProviderUpdate {
            enabled: None,
            base_url: Some(Some("http://192.168.1.10:11434/v1".into())),
            credential: None,
            models: None,
        },
        &*provisioned,
        &crate::managed_policy::NoOsPolicy,
    )
    .await
    .expect_err("credentialless cleartext is loopback-only");
    assert!(error.message().contains("loopback"));
}

fn gateway_test_snapshot(
    installation_id: &str,
    models: Vec<CustomModelConfig>,
) -> GatewayModelSnapshot {
    GatewayModelSnapshot {
        gateway_url: "https://gateway.example/".into(),
        installation_id: Some(installation_id.into()),
        models,
        model_protocols: BTreeMap::new(),
        model_reasoning_efforts: BTreeMap::new(),
        member_catalog: Some("v1".into()),
        catalog_etag: None,
    }
}

#[tokio::test]
async fn a_unique_bare_curated_id_migrates_to_its_gateway_equivalent() {
    let (store, _directory, policy) = gateway_migration_test_store().await;
    let snapshot = gateway_snapshot_for_policy(&store, &policy)
        .await
        .unwrap()
        .unwrap();

    let resolved = resolve_model_policy(&store, "claude-opus-5", false, None)
        .await
        .unwrap()
        .expect("the unique bare id resolves to Anthropic");
    assert_eq!(resolved.provider, ProviderKind::Anthropic);
    let equivalent = unique_gateway_equivalent(&store, &snapshot, "claude-opus-5")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(equivalent.key, "model_gateway::sample-claude");
    assert_eq!(equivalent.id, "sample-claude");
    assert_ne!(equivalent.route_model, equivalent.id);
}

#[tokio::test]
async fn already_admitted_plain_gateway_key_is_not_a_valid_execution_policy() {
    let (store, _directory, policy) = gateway_migration_test_store().await;
    let snapshot = gateway_snapshot_for_policy(&store, &policy)
        .await
        .unwrap()
        .unwrap();

    let mutable = resolve_model_policy(&store, "model_gateway::sample-claude", false, None)
        .await
        .unwrap()
        .expect("the current catalog still contains the local id");
    assert_eq!(mutable.route_model, "sample-claude");
    assert!(!is_valid_execution_policy(&mutable));

    let frozen = unique_gateway_equivalent(&store, &snapshot, "claude-opus-5")
        .await
        .unwrap()
        .expect("the canonical model has one gateway equivalent");
    assert!(is_valid_execution_policy(&frozen));

    let direct = resolve_model_policy(&store, "anthropic::claude-opus-5", false, None)
        .await
        .unwrap()
        .expect("the direct curated route is registered");
    assert!(is_valid_execution_policy(&direct));
}

#[tokio::test]
async fn a_bare_curated_id_shadowed_by_a_configured_model_does_not_migrate() {
    let (store, _directory, policy) = gateway_migration_test_store().await;
    let snapshot = gateway_snapshot_for_policy(&store, &policy)
        .await
        .unwrap()
        .unwrap();
    write_config(
        &store,
        ProviderKind::OpenaiCompatible,
        &ProviderConfig {
            enabled: true,
            base_url: Some("https://custom.example/v1".into()),
            models: vec![CustomModelConfig {
                id: "claude-opus-5".into(),
                context_window: 32_768,
                max_output_tokens: 4_096,
                ..Default::default()
            }],
        },
    )
    .await
    .unwrap();

    assert!(resolve_model_policy(&store, "claude-opus-5", false, None)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        unique_gateway_equivalent(&store, &snapshot, "claude-opus-5")
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn gateway_identity_conflicting_local_and_upstream_ids_are_not_equivalent_or_enriched() {
    let (store, _directory, _policy) = gateway_migration_test_store().await;
    let snapshot = gateway_test_snapshot(
        "install-1",
        vec![CustomModelConfig {
            id: "claude-opus-5".into(),
            upstream_id: Some("gpt-5.6-sol".into()),
            input_modalities: vec![InputModality::Text, InputModality::Image],
            supports_reasoning: true,
            reasoning_efforts: vec![ReasoningEffort::Low, ReasoningEffort::High],
            ..Default::default()
        }],
    );

    for selection in ["anthropic::claude-opus-5", "openai::gpt-5.6-sol"] {
        assert_eq!(
            unique_gateway_equivalent(&store, &snapshot, selection)
                .await
                .unwrap(),
            None,
            "a conflicting identity must not be equivalent to {selection}"
        );
    }

    let policy = gateway_execution_policy(&snapshot, "model_gateway::claude-opus-5")
        .expect("the deployment-local route remains selectable without canonical claims");
    assert_eq!(policy.vendor, None);
    assert_eq!(policy.verification, VerificationTier::Unverified);
    assert_eq!(policy.input_modalities, vec![InputModality::Text]);
    assert!(!policy.supports_structured_output);
    assert!(!policy.supports_reasoning);
    assert!(policy.reasoning_efforts.is_empty());
}

#[tokio::test]
async fn gateway_identity_conflicting_recognized_aliases_are_not_equivalent_or_enriched() {
    let (store, _directory, _policy) = gateway_migration_test_store().await;
    let snapshot = gateway_test_snapshot(
        "install-1",
        vec![CustomModelConfig {
            id: "deployment-model".into(),
            aliases: vec!["claude-opus-5".into(), "gpt-5.6-sol".into()],
            input_modalities: vec![InputModality::Text, InputModality::Image],
            supports_reasoning: true,
            reasoning_efforts: vec![ReasoningEffort::Low, ReasoningEffort::High],
            ..Default::default()
        }],
    );

    for selection in ["anthropic::claude-opus-5", "openai::gpt-5.6-sol"] {
        assert_eq!(
            unique_gateway_equivalent(&store, &snapshot, selection)
                .await
                .unwrap(),
            None,
            "conflicting aliases must not be equivalent to {selection}"
        );
    }

    let policy = gateway_execution_policy(&snapshot, "model_gateway::deployment-model")
        .expect("the deployment-local route remains selectable without canonical claims");
    assert_eq!(policy.vendor, None);
    assert_eq!(policy.verification, VerificationTier::Unverified);
    assert_eq!(policy.input_modalities, vec![InputModality::Text]);
    assert!(!policy.supports_structured_output);
    assert!(!policy.supports_reasoning);
    assert!(policy.reasoning_efforts.is_empty());
}

#[test]
fn frozen_gateway_key_stops_resolving_when_the_same_local_id_is_retargeted() {
    let original = gateway_test_snapshot(
        "install-1",
        vec![CustomModelConfig {
            id: "deployment-model".into(),
            upstream_id: Some("claude-opus-5".into()),
            ..Default::default()
        }],
    );
    let frozen = gateway_execution_policy(&original, "model_gateway::deployment-model")
        .expect("the original route resolves")
        .execution_key();
    let retargeted = gateway_test_snapshot(
        "install-1",
        vec![CustomModelConfig {
            id: "deployment-model".into(),
            upstream_id: Some("gpt-5.6-sol".into()),
            ..Default::default()
        }],
    );

    assert_eq!(gateway_execution_policy(&retargeted, &frozen), None);
}

#[test]
fn frozen_gateway_key_survives_unrelated_catalog_row_changes() {
    let selected = CustomModelConfig {
        id: "deployment-model".into(),
        upstream_id: Some("claude-opus-5".into()),
        ..Default::default()
    };
    let original = gateway_test_snapshot("install-1", vec![selected.clone()]);
    let frozen = gateway_execution_policy(&original, "model_gateway::deployment-model")
        .expect("the original route resolves")
        .execution_key();
    let changed = gateway_test_snapshot(
        "install-1",
        vec![
            CustomModelConfig {
                id: "unrelated-model".into(),
                upstream_id: Some("gpt-5.6-sol".into()),
                context_window: 64_000,
                max_output_tokens: 8_000,
                ..Default::default()
            },
            selected,
        ],
    );

    let resolved = gateway_execution_policy(&changed, &frozen)
        .expect("an unrelated row must not invalidate the selected route");
    assert_eq!(resolved.id, "deployment-model");
    assert_eq!(resolved.execution_key(), frozen);
}

#[test]
fn frozen_gateway_key_stops_resolving_when_the_installation_changes() {
    let model = CustomModelConfig {
        id: "deployment-model".into(),
        upstream_id: Some("claude-opus-5".into()),
        ..Default::default()
    };
    let original = gateway_test_snapshot("install-1", vec![model.clone()]);
    let frozen = gateway_execution_policy(&original, "model_gateway::deployment-model")
        .expect("the original route resolves")
        .execution_key();
    let replaced = gateway_test_snapshot("install-2", vec![model]);

    assert_eq!(gateway_execution_policy(&replaced, &frozen), None);
}

#[test]
fn api_key_credential_roundtrips_and_redacts_debug() {
    let cred = ProviderCredential::api_key("sk-secret");
    let json = serde_json::to_string(&cred).unwrap();
    assert!(json.contains("api_key"));
    assert!(json.contains("sk-secret"));
    let back: ProviderCredential = serde_json::from_str(&json).unwrap();
    assert_eq!(back.as_api_key(), Some("sk-secret"));
    assert!(!format!("{cred:?}").contains("sk-secret"));

    // The OAuth marker is additive on the same tagged wire and carries no
    // key material of its own.
    let oauth: ProviderCredential = serde_json::from_str(r#"{"type":"oauth"}"#).unwrap();
    assert_eq!(oauth, ProviderCredential::Oauth {});
    assert_eq!(oauth.as_api_key(), None);
    assert_eq!(
        serde_json::to_string(&oauth).unwrap(),
        r#"{"type":"oauth"}"#
    );
}

#[tokio::test]
async fn credentials_roundtrip_through_secret_storage() {
    let secrets = TestSecrets::default();
    let credentials = [
        (
            ProviderKind::Anthropic,
            ProviderCredential::api_key("existing-api-key"),
        ),
        (
            ProviderKind::Xai,
            ProviderCredential::api_key("existing-xai-api-key"),
        ),
        (ProviderKind::Openai, ProviderCredential::Oauth {}),
        (
            ProviderKind::Gemini,
            ProviderCredential::api_key("existing-gemini-api-key"),
        ),
    ];

    for (kind, credential) in credentials {
        write_credential(&secrets, kind, &credential)
            .await
            .expect("a valid credential writes to secret storage");
        assert_eq!(
            read_credential(&secrets, kind).await.unwrap(),
            Some(credential)
        );
    }
    assert!(
        write_credential(&secrets, ProviderKind::Xai, &ProviderCredential::Oauth {})
            .await
            .is_err()
    );
}

#[tokio::test]
async fn existing_api_key_blob_deserializes_unchanged() {
    let secrets = TestSecrets::default();
    secrets
        .set_secret(
            &ProviderKind::Openai.credential_key(),
            r#"{"type":"api_key","key":"existing-api-key"}"#,
        )
        .await
        .unwrap();

    assert_eq!(
        read_credential(&secrets, ProviderKind::Openai)
            .await
            .unwrap()
            .unwrap()
            .as_api_key(),
        Some("existing-api-key")
    );
}

#[tokio::test]
async fn legacy_bare_api_key_remains_readable() {
    let secrets = TestSecrets::default();
    secrets
        .set_secret(
            &ProviderKind::Fireworks.credential_key(),
            "legacy-fireworks-key",
        )
        .await
        .unwrap();

    assert_eq!(
        read_credential(&secrets, ProviderKind::Fireworks)
            .await
            .unwrap()
            .and_then(|credential| credential.as_api_key().map(str::to_owned)),
        Some("legacy-fireworks-key".to_owned())
    );
}

#[tokio::test]
async fn unreadable_structured_credentials_fail_closed_without_echoing_secrets() {
    let secrets = TestSecrets::default();
    let secret = "distinctive-provider-secret";
    let blobs = [
        format!(r#"{{"type":"future_credential","key":"{secret}"}}"#),
        format!(r#"{{"type":"api_key","key":"{secret}""#),
    ];

    for raw in blobs {
        secrets
            .set_secret(&ProviderKind::Fireworks.credential_key(), &raw)
            .await
            .unwrap();

        let error = read_credential(&secrets, ProviderKind::Fireworks)
            .await
            .expect_err("structured credential material must not become an API key");
        assert!(!error.to_string().contains(secret));
        assert!(!has_credential(&secrets, ProviderKind::Fireworks).await);
        assert_eq!(
            resolve_api_key(&secrets, ProviderKind::Fireworks).await,
            None
        );
    }
}

#[test]
fn kind_parse_and_keys() {
    assert_eq!(
        ProviderKind::parse("openai_compatible"),
        Some(ProviderKind::OpenaiCompatible)
    );
    assert_eq!(
        ProviderKind::Anthropic.credential_key(),
        "provider.anthropic.credential"
    );
    assert_eq!(ProviderKind::Openai.setting_key(), "provider.openai");
    assert_eq!(ProviderKind::parse("xai"), Some(ProviderKind::Xai));
    assert_eq!(
        ProviderKind::Xai.credential_key(),
        "provider.xai.credential"
    );
    assert_eq!(
        ProviderKind::parse("fireworks"),
        Some(ProviderKind::Fireworks)
    );
    assert_eq!(
        ProviderKind::Fireworks.default_base_url(),
        Some("https://api.fireworks.ai/inference/v1")
    );
    assert_eq!(
        ProviderKind::Together.default_base_url(),
        Some("https://api.together.ai/v1")
    );
    assert_eq!(
        ProviderKind::parse("openrouter"),
        Some(ProviderKind::Openrouter)
    );
    assert_eq!(
        ProviderKind::Openrouter.default_base_url(),
        Some("https://openrouter.ai/api/v1")
    );
    assert!(ProviderKind::Openrouter.has_fixed_endpoint());
    assert!(ProviderKind::Openrouter.requires_credential());
    assert!(ProviderKind::Openrouter.accepts_configured_models());
    assert_eq!(
        ProviderKind::Openrouter
            .effective_base_url(Some("https://attacker.invalid/v1"))
            .as_deref(),
        Some("https://openrouter.ai/api/v1")
    );
    assert_eq!(ProviderKind::parse("ollama"), Some(ProviderKind::Ollama));
    assert_eq!(
        ProviderKind::Ollama.default_base_url(),
        Some("http://127.0.0.1:11434/v1")
    );
    assert!(!ProviderKind::Ollama.has_fixed_endpoint());
    assert!(!ProviderKind::Ollama.requires_credential());
    assert!(ProviderKind::Ollama.accepts_configured_models());
    assert_eq!(
        ProviderKind::Ollama.effective_base_url(None).as_deref(),
        Some("http://127.0.0.1:11434/v1")
    );
    assert_eq!(
        ProviderKind::Ollama
            .effective_base_url(Some("http://192.168.1.10:11434/v1"))
            .as_deref(),
        Some("http://192.168.1.10:11434/v1")
    );
    assert_eq!(
        ProviderKind::Fireworks
            .effective_base_url(Some("https://attacker.invalid/v1"))
            .as_deref(),
        Some("https://api.fireworks.ai/inference/v1")
    );
}

/// A lookup that answers one variable, standing in for the process
/// environment so the precedence tests never mutate it.
fn env_of(name: &'static str, value: &'static str) -> impl Fn(&str) -> Option<String> {
    move |requested| (requested == name).then(|| value.to_string())
}

#[test]
fn a_stored_base_url_wins_over_the_environment_fallback() {
    assert_eq!(
        ProviderKind::Anthropic
            .effective_base_url_from(
                Some("https://stored.example/v1"),
                env_of("ANTHROPIC_BASE_URL", "https://env.example/v1"),
            )
            .as_deref(),
        Some("https://stored.example/v1")
    );
}

#[test]
fn the_environment_fallback_answers_when_nothing_is_stored() {
    assert_eq!(
        ProviderKind::Anthropic
            .effective_base_url_from(None, env_of("ANTHROPIC_BASE_URL", "https://env.example"),)
            .as_deref(),
        Some("https://env.example")
    );
    assert_eq!(
        ProviderKind::Openai
            .effective_base_url_from(None, env_of("OPENAI_BASE_URL", "https://env.example/v1"))
            .as_deref(),
        Some("https://env.example/v1")
    );
    assert_eq!(
        ProviderKind::OpenaiCompatible
            .effective_base_url_from(
                None,
                env_of("OPENAI_COMPATIBLE_BASE_URL", "https://env.example/v1"),
            )
            .as_deref(),
        Some("https://env.example/v1")
    );
    // An empty stored value is not a value, so the fallback still answers.
    assert_eq!(
        ProviderKind::Openai
            .effective_base_url_from(
                Some(""),
                env_of("OPENAI_BASE_URL", "https://env.example/v1"),
            )
            .as_deref(),
        Some("https://env.example/v1")
    );
}

/// A credentialless kind may be pointed at loopback HTTP from the
/// environment, exactly as it may from a stored value.
#[test]
fn the_ollama_fallback_accepts_loopback_http() {
    assert_eq!(
        ProviderKind::Ollama
            .effective_base_url_from(None, env_of("OLLAMA_BASE_URL", "http://localhost:11434/v1"),)
            .as_deref(),
        Some("http://localhost:11434/v1")
    );
}

/// Same posture as an empty `*_API_KEY`: an unusable value is ignored,
/// never an error, and the built-in default still applies.
#[test]
fn an_unusable_environment_base_url_is_ignored() {
    for value in [
        "",
        "not a url",
        "ftp://example.com/v1",
        // Cleartext to a non-loopback host, for a kind that carries a key.
        "http://proxy.internal/v1",
        // Credentials embedded in the URL.
        "https://user:pass@example.com/v1",
    ] {
        let lookup =
            |requested: &str| (requested == "ANTHROPIC_BASE_URL").then(|| value.to_string());
        assert_eq!(
            ProviderKind::Anthropic.effective_base_url_from(None, lookup),
            None,
            "`{value}` must not become Anthropic's endpoint"
        );
        let lookup = |requested: &str| (requested == "OLLAMA_BASE_URL").then(|| value.to_string());
        assert_eq!(
            ProviderKind::Ollama
                .effective_base_url_from(None, lookup)
                .as_deref(),
            Some("http://127.0.0.1:11434/v1"),
            "`{value}` must leave Ollama on its default endpoint"
        );
    }
}

/// Fixed first-party and hosted-preset endpoints ignore the environment,
/// exactly as they ignore a stored value.
#[test]
fn fixed_endpoint_kinds_have_no_environment_fallback() {
    for kind in [
        ProviderKind::Xai,
        ProviderKind::Gemini,
        ProviderKind::Fireworks,
        ProviderKind::Together,
        ProviderKind::Openrouter,
        ProviderKind::ModelGateway,
    ] {
        assert_eq!(kind.base_url_env_var(), None);
        assert_eq!(
            kind.effective_base_url_from(None, |_| Some("https://attacker.invalid/v1".to_string())),
            kind.default_base_url().map(str::to_owned),
            "{kind} must keep its fixed endpoint"
        );
    }
}

#[test]
fn a_pre_protocol_gateway_snapshot_keeps_its_anthropic_route() {
    let snapshot: GatewayModelSnapshot = serde_json::from_value(serde_json::json!({
        "gateway_url": "https://gateway.example/",
        "models": [{
            "id": "legacy-claude",
            "context_window": 32768,
            "max_output_tokens": 4096
        }]
    }))
    .expect("the persisted shape from before per-model protocols still loads");

    assert!(snapshot.model_protocols.is_empty());
    assert_eq!(
        snapshot
            .model_protocols
            .get("legacy-claude")
            .copied()
            .unwrap_or_default(),
        GatewayModelProtocol::AnthropicMessages
    );
}

#[test]
fn a_chat_completions_era_snapshot_still_routes_its_openai_models() {
    // Snapshots written while the gateway's OpenAI route spoke Chat
    // Completions recorded `openai_chat_completions`; the route now speaks
    // Responses, the only OpenAI surface a gateway serves.
    let snapshot: GatewayModelSnapshot = serde_json::from_value(serde_json::json!({
        "gateway_url": "https://gateway.example/",
        "models": [{
            "id": "legacy-coder",
            "context_window": 32768,
            "max_output_tokens": 4096
        }],
        "model_protocols": {"legacy-coder": "openai_chat_completions"}
    }))
    .expect("the persisted chat-completions spelling still loads");

    assert_eq!(
        snapshot.model_protocols.get("legacy-coder"),
        Some(&GatewayModelProtocol::OpenaiResponses)
    );
    for spelling in [
        "openai",
        "openai_compatible",
        "openai_chat_completions",
        "openai_responses",
    ] {
        assert_eq!(
            GatewayModelProtocol::parse(spelling),
            Some(GatewayModelProtocol::OpenaiResponses),
            "{spelling}"
        );
    }
}

/// An unset display name is represented by the key being absent, in both
/// directions. The desktop used to send an explicit `null` while declaring
/// the field non-optional, so its type claimed a key the server never sends.
///
/// `deny_unknown_fields` makes the inbound half worth asserting rather than
/// assuming: the body has to be accepted with the key missing entirely.
#[test]
fn an_unset_display_name_is_absent_rather_than_null() {
    let unset = CustomModelConfig {
        id: "local/model".into(),
        upstream_id: None,
        display_name: None,
        context_window: 32_768,
        max_output_tokens: 4_096,
        ..Default::default()
    };
    let json = serde_json::to_value(&unset).expect("a model config serializes");
    assert!(
        json.get("display_name").is_none(),
        "the server should omit an unset display name, not send null: {json}"
    );

    // What the desktop now sends: no key at all.
    let parsed: CustomModelConfig = serde_json::from_str(
        r#"{"id":"local/model","context_window":32768,"max_output_tokens":4096}"#,
    )
    .expect("an absent display name is accepted");
    assert_eq!(parsed, unset);

    // Still accepted, so an older client is not broken by the change.
    let explicit_null: CustomModelConfig = serde_json::from_str(
        r#"{"id":"local/model","display_name":null,"context_window":32768,"max_output_tokens":4096}"#,
    )
    .expect("an explicit null is still accepted");
    assert_eq!(explicit_null, unset);
}

/// The candidate the registry is offered for a deployment-aliased upstream
/// id. The risk here is over-stripping: every extra rule is another way to
/// hand one model another model's capabilities, and curated ids contain
/// dots and digits of their own.
#[test]
fn upstream_ids_are_normalized_only_by_region_vendor_and_version() {
    assert_eq!(
        curated_id_candidate("us.anthropic.claude-opus-5"),
        "claude-opus-5"
    );
    assert_eq!(
        curated_id_candidate("anthropic.claude-sonnet-4-5-v1:0"),
        "claude-sonnet-4-5"
    );
    // Not a region prefix, not a version suffix: left exactly as reported.
    assert_eq!(curated_id_candidate("gpt-5.6-sol"), "gpt-5.6-sol");
    assert_eq!(curated_id_candidate("acme.llm-v"), "acme.llm-v");
    assert_eq!(
        curated_id_candidate("us.acme.private-model"),
        "acme.private-model"
    );
    // One region prefix, not repeated stripping.
    assert_eq!(curated_id_candidate("us.eu.model"), "eu.model");
}

#[test]
fn custom_model_validation_is_conservative_and_rejects_duplicates() {
    let valid = CustomModelConfig {
        id: "local/model".into(),
        upstream_id: None,
        display_name: Some("Local Model".into()),
        context_window: 32_768,
        max_output_tokens: 4_096,
        ..Default::default()
    };
    assert!(validate_custom_models(std::slice::from_ref(&valid)).is_ok());
    assert!(validate_custom_models(&[valid.clone(), valid.clone()]).is_err());
    assert!(
        validate_configured_models_against(
            ProviderKind::OpenaiCompatible,
            std::slice::from_ref(&valid),
            |id| id == "local/model"
        )
        .is_err(),
        "custom ids must not shadow a curated id under the same provider"
    );
    assert!(validate_custom_models(&[CustomModelConfig {
        id: "bad model".into(),
        upstream_id: None,
        display_name: None,
        context_window: 32_768,
        max_output_tokens: 4_096,
        ..Default::default()
    }])
    .is_err());
    assert!(validate_custom_models(&[CustomModelConfig {
        id: "bad".into(),
        upstream_id: None,
        display_name: None,
        context_window: 1_000,
        max_output_tokens: 4_096,
        ..Default::default()
    }])
    .is_err());
}

#[test]
fn xai_configured_models_carry_only_supported_capabilities() {
    let model = CustomModelConfig {
        id: "grok-account-model".into(),
        display_name: Some("Grok account model".into()),
        context_window: 500_000,
        max_output_tokens: 32_768,
        input_modalities: vec![InputModality::Text, InputModality::Image],
        supports_reasoning: true,
        reasoning_efforts: vec![
            ReasoningEffort::None,
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
        ],
        ..Default::default()
    };
    validate_configured_models(ProviderKind::Xai, std::slice::from_ref(&model)).unwrap();

    let policy = ResolvedModelPolicy::custom_for(ProviderKind::Xai, &model);
    assert_eq!(policy.provider, ProviderKind::Xai);
    assert_eq!(policy.input_modalities, model.input_modalities);
    assert!(policy.supports_reasoning);
    assert_eq!(policy.reasoning_efforts, model.reasoning_efforts);

    let mut unsupported = model.clone();
    unsupported.reasoning_efforts.push(ReasoningEffort::Max);
    assert!(validate_configured_models(ProviderKind::Xai, &[unsupported]).is_err());
    assert!(validate_configured_models(ProviderKind::OpenaiCompatible, &[model]).is_err());
}

#[test]
fn registry_policy_controls_context_output_provider_and_reasoning() {
    let mut config = AgentConfig {
        temperature: Some(0.7),
        reasoning_effort: Some(ReasoningEffort::High),
        ..AgentConfig::default()
    };
    let opus = ResolvedModelPolicy::curated(
        model_registry::find_for(ProviderKind::Anthropic, "claude-opus-4-8").unwrap(),
    );
    apply_model_policy(&mut config, &opus, Some(ReasoningEffort::XHigh)).unwrap();
    assert_eq!(config.provider, Some(ProviderId::new("anthropic")));
    assert_eq!(config.model, "claude-opus-4-8");
    assert_eq!(config.context_window, 1_000_000);
    assert_eq!(config.max_tokens, Some(128_000));
    assert_eq!(config.reasoning_effort, Some(ReasoningEffort::XHigh));
    assert_eq!(config.temperature, None);
    assert!(config.image_input);

    // Haiku 4.5 needs a classic token-budget thinking request that the
    // adapter cannot send yet, so policy keeps both reasoning and effort
    // off rather than promising reasoning that disappears on the wire.
    let mut config = AgentConfig {
        temperature: Some(0.7),
        ..AgentConfig::default()
    };
    let haiku = ResolvedModelPolicy::curated(
        model_registry::find_for(ProviderKind::Anthropic, "claude-haiku-4-5-20251001").unwrap(),
    );
    apply_model_policy(&mut config, &haiku, Some(ReasoningEffort::High)).unwrap();
    assert!(!config.reasoning_model);
    assert_eq!(config.reasoning_effort, None);
    assert_eq!(config.temperature, Some(0.7));

    let mut config = AgentConfig::default();
    let gpt = ResolvedModelPolicy::curated(
        model_registry::find_for(ProviderKind::Openai, "gpt-5.6-sol").unwrap(),
    );
    apply_model_policy(&mut config, &gpt, Some(ReasoningEffort::Low)).unwrap();
    assert_eq!(config.provider, Some(ProviderId::new("openai")));
    assert_eq!(config.context_window, 1_050_000);
    assert_eq!(config.max_tokens, Some(128_000));
    assert!(config.reasoning_model);
    assert_eq!(config.reasoning_effort, Some(ReasoningEffort::Low));
    assert!(config.image_input);

    // A custom endpoint keeps the conservative shape: no reasoning, and the
    // requested effort is dropped rather than sent to something that would
    // reject it.
    let mut config = AgentConfig::default();
    let custom = ResolvedModelPolicy::custom_for(
        ProviderKind::OpenaiCompatible,
        &CustomModelConfig {
            id: "local-model".into(),
            upstream_id: None,
            display_name: None,
            context_window: 32_768,
            max_output_tokens: 4_096,
            ..Default::default()
        },
    );
    assert_eq!(custom.verification, VerificationTier::Unverified);
    apply_model_policy(&mut config, &custom, Some(ReasoningEffort::High)).unwrap();
    assert!(!config.image_input);
    assert!(config.tools_supported);
    assert_eq!(config.provider, Some(ProviderId::new("openai_compatible")));
    assert_eq!(config.context_window, 32_768);
    assert_eq!(config.max_tokens, Some(4_096));
    assert!(!config.reasoning_model);
    assert_eq!(config.reasoning_effort, None);
}

#[test]
fn a_level_a_model_does_not_accept_never_survives_policy_application() {
    let apply = |id: &str, provider: ProviderKind, effort: ReasoningEffort| {
        let mut config = AgentConfig::default();
        let policy = ResolvedModelPolicy::curated(model_registry::find_for(provider, id).unwrap());
        apply_model_policy(&mut config, &policy, Some(effort)).unwrap();
        config.reasoning_effort
    };

    // `max` arrived with GPT-5.6; on 5.5 the same stored choice degrades to
    // the top level that generation takes rather than failing the turn.
    assert_eq!(
        apply("gpt-5.6-sol", ProviderKind::Openai, ReasoningEffort::Max),
        Some(ReasoningEffort::Max)
    );
    assert_eq!(
        apply("gpt-5.5", ProviderKind::Openai, ReasoningEffort::Max),
        Some(ReasoningEffort::XHigh)
    );
    // Anthropic has no "don't reason" level, so `none` comes up to `low`.
    assert_eq!(
        apply(
            "claude-opus-5",
            ProviderKind::Anthropic,
            ReasoningEffort::None
        ),
        Some(ReasoningEffort::Low)
    );
    assert_eq!(
        apply(
            "claude-opus-5",
            ProviderKind::Anthropic,
            ReasoningEffort::XHigh
        ),
        Some(ReasoningEffort::XHigh)
    );
}

#[test]
fn claude_policy_clamps_4_6_xhigh_and_disables_haiku_4_5_before_requests() {
    for provider in [ProviderKind::Anthropic] {
        for id in ["claude-opus-4-6", "claude-sonnet-4-6"] {
            let policy =
                ResolvedModelPolicy::curated(model_registry::find_for(provider, id).unwrap());

            let mut config = AgentConfig::default();
            apply_model_policy(&mut config, &policy, Some(ReasoningEffort::XHigh)).unwrap();
            assert!(config.reasoning_model, "{}::{id}", provider.as_str());
            assert_eq!(
                config.reasoning_effort,
                Some(ReasoningEffort::High),
                "{}::{id} let xhigh survive policy",
                provider.as_str()
            );

            let mut config = AgentConfig::default();
            apply_model_policy(&mut config, &policy, Some(ReasoningEffort::Max)).unwrap();
            assert_eq!(
                config.reasoning_effort,
                Some(ReasoningEffort::Max),
                "{}::{id} lost its supported max level",
                provider.as_str()
            );
        }
    }

    for (provider, id) in [(ProviderKind::Anthropic, "claude-haiku-4-5-20251001")] {
        let policy = ResolvedModelPolicy::curated(model_registry::find_for(provider, id).unwrap());
        for effort in ReasoningEffort::ALL {
            let mut config = AgentConfig {
                temperature: Some(0.7),
                ..AgentConfig::default()
            };
            apply_model_policy(&mut config, &policy, Some(*effort)).unwrap();
            assert!(!config.reasoning_model, "{}::{id}", provider.as_str());
            assert_eq!(config.reasoning_effort, None, "{}::{id}", provider.as_str());
            assert_eq!(config.temperature, Some(0.7), "{}::{id}", provider.as_str());
        }
    }
}

#[test]
fn gemini_3_1_pro_none_clamps_to_low() {
    let mut config = AgentConfig::default();
    let policy = ResolvedModelPolicy::curated(
        model_registry::find_for(ProviderKind::Gemini, "gemini-3.1-pro-preview").unwrap(),
    );

    apply_model_policy(&mut config, &policy, Some(ReasoningEffort::None)).unwrap();

    assert_eq!(config.provider, Some(ProviderId::new("gemini")));
    assert_eq!(config.reasoning_effort, Some(ReasoningEffort::Low));
}

/// Repro: a chat pinned to `openai::gpt-5.6-sol` reached the
/// OpenAI-compatible `/v1/chat/completions` route with `reasoning_effort`
/// attached and the vendor refused the request. The free-form turn path
/// assigned the raw model and effort without applying the registry policy,
/// so the request carried no provider hint and the router was free to serve
/// a model the registry already owns from any OpenAI-compatible route.
#[test]
fn a_registered_model_keeps_its_policy_on_the_free_form_path() {
    let mut config = AgentConfig::default();
    apply_free_form_model(
        &mut config,
        "openai::gpt-5.6-sol".into(),
        Some(ReasoningEffort::Max),
    )
    .unwrap();
    assert_eq!(config.provider, Some(ProviderId::new("openai")));
    assert_eq!(config.model, "gpt-5.6-sol");
    assert!(config.reasoning_model);
    assert_eq!(config.reasoning_effort, Some(ReasoningEffort::Max));

    // A bare curated id resolves the same way, and the effort is clamped to
    // what that generation actually takes.
    let mut config = AgentConfig::default();
    apply_free_form_model(&mut config, "gpt-5.5".into(), Some(ReasoningEffort::Max)).unwrap();
    assert_eq!(config.provider, Some(ProviderId::new("openai")));
    assert_eq!(config.reasoning_effort, Some(ReasoningEffort::XHigh));

    // An id the registry does not claim keeps the free-form contract, and
    // its effort stays off the wire because nothing declared the model a
    // reasoning model.
    let mut config = AgentConfig::default();
    apply_free_form_model(
        &mut config,
        "local-model".into(),
        Some(ReasoningEffort::High),
    )
    .unwrap();
    assert_eq!(config.provider, None);
    assert_eq!(config.model, "local-model");
    assert!(!config.reasoning_model);
}

#[test]
fn every_curated_model_applies_its_exact_runtime_contract() {
    for &provider in ProviderKind::ALL {
        for spec in model_registry::models_for(provider) {
            let mut config = AgentConfig {
                temperature: Some(0.7),
                reasoning_effort: Some(ReasoningEffort::High),
                ..AgentConfig::default()
            };
            let policy = ResolvedModelPolicy::curated(spec);
            apply_model_policy(&mut config, &policy, Some(ReasoningEffort::Low)).unwrap();

            assert_eq!(config.provider, Some(ProviderId::new(provider.as_str())));
            assert_eq!(config.model, spec.id);
            assert_eq!(
                config.context_window,
                usize::try_from(spec.context_window).unwrap()
            );
            assert_eq!(config.max_tokens, Some(spec.max_output_tokens));
            assert_eq!(config.tools_supported, spec.supports_tools());
            assert_eq!(config.reasoning_model, spec.supports_reasoning);
            assert_eq!(
                config.reasoning_effort,
                ReasoningEffort::Low.clamp_to(spec.reasoning_efforts)
            );
            assert_eq!(
                config.temperature,
                (!spec.supports_reasoning).then_some(0.7)
            );
        }
    }
}
