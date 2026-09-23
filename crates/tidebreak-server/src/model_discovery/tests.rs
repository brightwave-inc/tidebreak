use super::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use tidebreak_core::DbStore;

/// A stand-in credential, built at run time so no literal in this file reads
/// like a real key.
fn stand_in_key(label: &str) -> String {
    ["stand-in", label, "credential"].join("-")
}

fn fixture<T: DeserializeOwned>(json: &str) -> T {
    serde_json::from_str(json).expect("the fixture matches the documented listing shape")
}

fn ids(models: &[DiscoveredModel]) -> Vec<&str> {
    models.iter().map(|model| model.id.as_str()).collect()
}

fn find<'a>(models: &'a [DiscoveredModel], id: &str) -> &'a DiscoveredModel {
    models
        .iter()
        .find(|model| model.id == id)
        .unwrap_or_else(|| panic!("{id} is listed"))
}

#[test]
fn anthropic_listing_reports_limits_images_and_the_efforts_the_route_sends() {
    let (models, next) =
        parse_anthropic_page(fixture(include_str!("fixtures/anthropic_models.json")));
    assert_eq!(next, None, "the last page has no cursor");
    assert_eq!(
        ids(&models),
        [
            "claude-opus-5-5",
            "claude-opus-4-6",
            "claude-3-7-sonnet-20250219",
            "claude-legacy-unknown",
        ]
    );

    let opus = find(&models, "claude-opus-5-5");
    assert_eq!(opus.display_name.as_deref(), Some("Claude Opus 5.5"));
    assert_eq!(opus.context_window, Some(1_000_000));
    assert_eq!(opus.max_output_tokens, Some(128_000));
    assert_eq!(opus.image_input, Some(true));
    assert_eq!(opus.supports_reasoning, Some(true));
    assert_eq!(
        opus.reasoning_efforts,
        [
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
            ReasoningEffort::Max,
        ]
    );

    // A level reported as null is not a level the model takes.
    assert_eq!(
        find(&models, "claude-opus-4-6").reasoning_efforts,
        [
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::Max,
        ]
    );

    // The model thinks, but only on the budget form the adapter never sends,
    // so discovery must not offer it as a reasoning model.
    let sonnet = find(&models, "claude-3-7-sonnet-20250219");
    assert_eq!(sonnet.supports_reasoning, Some(false));
    assert!(sonnet.reasoning_efforts.is_empty());

    // Zero and null limits and a null capability object read as unknown.
    let legacy = find(&models, "claude-legacy-unknown");
    assert_eq!(legacy.context_window, None);
    assert_eq!(legacy.max_output_tokens, None);
    assert_eq!(legacy.image_input, None);
    assert_eq!(legacy.supports_reasoning, None);
}

#[test]
fn xai_listing_keeps_chat_models_and_joins_their_context_length() {
    let models = parse_xai(
        fixture(include_str!("fixtures/xai_language_models.json")),
        Some(fixture(include_str!("fixtures/xai_models.json"))),
    );
    // The image generation model appears only on the general listing, so it
    // never becomes a row.
    assert_eq!(ids(&models), ["grok-4.7", "grok-420-non-reasoning"]);

    let grok = find(&models, "grok-4.7");
    assert_eq!(grok.context_window, Some(500_000));
    assert_eq!(grok.image_input, Some(true));
    assert_eq!(grok.supports_reasoning, Some(true));
    assert_eq!(
        grok.reasoning_efforts,
        [
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
        ]
    );

    let plain = find(&models, "grok-420-non-reasoning");
    assert_eq!(plain.image_input, Some(false));
    assert_eq!(plain.supports_reasoning, None);

    // Without the general listing the rows still stand, minus context.
    let models = parse_xai(
        fixture(include_str!("fixtures/xai_language_models.json")),
        None,
    );
    assert_eq!(find(&models, "grok-4.7").context_window, None);
}

#[test]
fn gemini_listing_keeps_generate_content_chat_models() {
    let (models, next) = parse_gemini_page(fixture(include_str!("fixtures/gemini_models.json")));
    assert_eq!(next.as_deref(), Some("page-2"));
    // Speech, embedding, and video models share the listing and are dropped.
    assert_eq!(ids(&models), ["gemini-3.8-flash", "gemini-3.1-pro-preview"]);
    let flash = find(&models, "gemini-3.8-flash");
    assert_eq!(flash.display_name.as_deref(), Some("Gemini 3.8 Flash"));
    assert_eq!(flash.context_window, Some(1_048_576));
    assert_eq!(flash.max_output_tokens, Some(65_536));
    assert_eq!(flash.supports_reasoning, Some(true));
    // The listing says nothing about images or levels.
    assert_eq!(flash.image_input, None);
    assert!(flash.reasoning_efforts.is_empty());
}

#[test]
fn together_listing_keeps_only_chat_models() {
    let models = parse_together(fixture(include_str!("fixtures/together_models.json")));
    assert_eq!(
        ids(&models),
        [
            "zai-org/GLM-5.3",
            "moonshotai/Kimi-K3",
            "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        ]
    );
    let glm = find(&models, "zai-org/GLM-5.3");
    assert_eq!(glm.display_name.as_deref(), Some("GLM-5.3"));
    assert_eq!(glm.context_window, Some(1_048_575));
}

#[test]
fn openrouter_listing_reads_modalities_limits_and_parameters() {
    let models = parse_openrouter(fixture(include_str!("fixtures/openrouter_models.json")));
    // A model that answers with images is not a chat model.
    assert_eq!(
        ids(&models),
        [
            "anthropic/claude-opus-5.5",
            "meta-llama/llama-3.3-70b-instruct"
        ]
    );
    let opus = find(&models, "anthropic/claude-opus-5.5");
    assert_eq!(
        opus.display_name.as_deref(),
        Some("Anthropic: Claude Opus 5.5")
    );
    assert_eq!(opus.context_window, Some(1_000_000));
    assert_eq!(opus.max_output_tokens, Some(128_000));
    assert_eq!(opus.image_input, Some(true));
    assert_eq!(opus.supports_tools, Some(true));
    assert_eq!(opus.supports_reasoning, Some(true));

    let llama = find(&models, "meta-llama/llama-3.3-70b-instruct");
    assert_eq!(llama.max_output_tokens, None);
    assert_eq!(llama.image_input, Some(false));
    assert_eq!(llama.supports_tools, Some(false));
    assert_eq!(llama.supports_reasoning, Some(false));
}

#[test]
fn ollama_models_drop_embedding_only_ones_and_read_capabilities() {
    let qwen = parse_ollama_model(
        "qwen3:8b".into(),
        Some(fixture(include_str!("fixtures/ollama_show_qwen3.json"))),
    )
    .expect("a completion model is kept");
    assert_eq!(qwen.context_window, Some(40_960));
    assert_eq!(qwen.image_input, Some(false));
    assert_eq!(qwen.supports_tools, Some(true));
    assert_eq!(qwen.supports_reasoning, Some(true));

    let gemma = parse_ollama_model(
        "gemma4:12b".into(),
        Some(fixture(include_str!("fixtures/ollama_show_gemma4.json"))),
    )
    .expect("a vision model is kept");
    assert_eq!(gemma.context_window, Some(131_072));
    assert_eq!(gemma.image_input, Some(true));
    assert_eq!(gemma.supports_tools, Some(false));

    assert!(parse_ollama_model(
        "embeddinggemma:latest".into(),
        Some(fixture(include_str!(
            "fixtures/ollama_show_embeddinggemma.json"
        ))),
    )
    .is_none());

    // A model whose details did not load keeps its name and nothing else.
    let unknown = parse_ollama_model("mystery:1b".into(), None).expect("kept");
    assert_eq!(unknown.context_window, None);
    assert_eq!(unknown.supports_tools, None);

    assert_eq!(
        ollama_native_root("http://127.0.0.1:11434/v1/"),
        "http://127.0.0.1:11434"
    );
    assert_eq!(
        ollama_native_root("https://ollama.com/v1"),
        "https://ollama.com"
    );
}

#[test]
fn compatible_listing_reads_the_vllm_context_and_skips_embedders() {
    let models = parse_compatible(fixture(include_str!("fixtures/compatible_models.json")));
    assert_eq!(
        ids(&models),
        ["meta-llama/Llama-3.3-70B-Instruct", "sql-lora"]
    );
    assert_eq!(
        find(&models, "meta-llama/Llama-3.3-70B-Instruct").context_window,
        Some(131_072)
    );
    assert_eq!(find(&models, "sql-lora").context_window, None);
}

#[test]
fn finishing_cleans_rows_marks_known_ids_and_never_echoes_the_key() {
    let key = stand_in_key("finish");
    let key = key.as_str();
    let mut built_in = DiscoveredModel::new("claude-opus-5-5");
    built_in.context_window = Some(1_000_000);
    built_in.max_output_tokens = Some(2_000_000);
    let mut added = DiscoveredModel::new("claude-next-preview");
    added.display_name = Some("  Claude Next  ".into());
    added.supports_reasoning = Some(true);
    added.reasoning_efforts = vec![
        ReasoningEffort::Max,
        ReasoningEffort::None,
        ReasoningEffort::Low,
    ];
    let mut tiny = DiscoveredModel::new("claude-tiny");
    tiny.context_window = Some(512);
    let mut no_reasoning = DiscoveredModel::new("claude-plain");
    no_reasoning.supports_reasoning = Some(false);
    no_reasoning.reasoning_efforts = vec![ReasoningEffort::High];
    let mut named_after_key = DiscoveredModel::new("claude-fine");
    named_after_key.display_name = Some(format!("tuned with {key}"));

    let models = finish(
        ProviderKind::Anthropic,
        vec![
            built_in,
            added.clone(),
            added,
            tiny,
            no_reasoning,
            named_after_key,
            DiscoveredModel::new(format!("echo-{key}")),
            DiscoveredModel::new("has space"),
            DiscoveredModel::new(""),
        ],
        &[CustomModelConfig {
            id: "claude-next-preview".into(),
            ..CustomModelConfig::default()
        }],
        Some(key),
    );

    // Sorted, deduplicated, and without the malformed or key-bearing ids.
    assert_eq!(
        ids(&models),
        [
            "claude-fine",
            "claude-next-preview",
            "claude-opus-5-5",
            "claude-plain",
            "claude-tiny",
        ]
    );
    let serialized = serde_json::to_string(&models).unwrap();
    assert!(!serialized.contains(key), "{serialized}");

    let opus = find(&models, "claude-opus-5-5");
    assert!(opus.built_in);
    assert!(!opus.added);
    // An output larger than the window is not a limit a row may carry.
    assert_eq!(opus.max_output_tokens, None);

    let next = find(&models, "claude-next-preview");
    assert!(next.added);
    assert!(!next.built_in);
    assert_eq!(next.display_name.as_deref(), Some("Claude Next"));
    // Anthropic sends no `none`; the rest come back ascending.
    assert_eq!(
        next.reasoning_efforts,
        [ReasoningEffort::Low, ReasoningEffort::Max]
    );

    assert_eq!(find(&models, "claude-tiny").context_window, None);
    assert!(find(&models, "claude-plain").reasoning_efforts.is_empty());
    assert_eq!(find(&models, "claude-fine").display_name, None);
}

// ---------------------------------------------------------------------------
// The HTTP path, against a local stand-in for each provider.

#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<(String, HeaderMap)>>>);

impl Seen {
    fn record(&self, path: &str, headers: &HeaderMap) {
        self.0
            .lock()
            .unwrap()
            .push((path.to_owned(), headers.clone()));
    }

    fn requests(&self) -> Vec<(String, HeaderMap)> {
        self.0.lock().unwrap().clone()
    }
}

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{address}")
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

#[tokio::test]
async fn anthropic_discovery_pages_through_the_listing_with_its_own_headers() {
    let seen = Seen::default();
    let router = Router::new()
        .route(
            "/v1/models",
            get(
                |State(seen): State<Seen>,
                 Query(query): Query<HashMap<String, String>>,
                 headers: HeaderMap| async move {
                    seen.record("/v1/models", &headers);
                    match query.get("after_id").map(String::as_str) {
                        None => Json(serde_json::json!({
                            "data": [{ "type": "model", "id": "claude-opus-5-5", "display_name": "Claude Opus 5.5" }],
                            "has_more": true,
                            "first_id": "claude-opus-5-5",
                            "last_id": "claude-opus-5-5"
                        })),
                        Some("claude-opus-5-5") => Json(serde_json::json!({
                            "data": [{ "type": "model", "id": "claude-sonnet-5", "display_name": "Claude Sonnet 5" }],
                            "has_more": false,
                            "first_id": "claude-sonnet-5",
                            "last_id": "claude-sonnet-5"
                        })),
                        Some(other) => panic!("unexpected cursor {other}"),
                    }
                },
            ),
        )
        .with_state(seen.clone());
    let base = serve(router).await;

    let found = discover_at(ProviderKind::Anthropic, &base, Some("sk-ant-test"), &[])
        .await
        .unwrap();

    assert_eq!(ids(&found.models), ["claude-opus-5-5", "claude-sonnet-5"]);
    assert!(found.models.iter().all(|model| model.built_in));
    let requests = seen.requests();
    assert_eq!(requests.len(), 2, "one request per page");
    for (_, headers) in requests {
        assert_eq!(header(&headers, "x-api-key"), Some("sk-ant-test"));
        assert_eq!(header(&headers, "anthropic-version"), Some("2023-06-01"));
        assert_eq!(header(&headers, "authorization"), None);
    }
}

#[tokio::test]
async fn gemini_discovery_sends_its_key_in_its_own_header() {
    let seen = Seen::default();
    let router = Router::new()
        .route(
            "/v1beta/models",
            get(|State(seen): State<Seen>, headers: HeaderMap| async move {
                seen.record("/v1beta/models", &headers);
                include_str!("fixtures/gemini_models.json").replace("\"page-2\"", "\"\"")
            }),
        )
        .with_state(seen.clone());
    let base = serve(router).await;

    let found = discover_at(ProviderKind::Gemini, &base, Some("gemini-key"), &[])
        .await
        .unwrap();

    assert_eq!(
        ids(&found.models),
        ["gemini-3.1-pro-preview", "gemini-3.8-flash"]
    );
    let requests = seen.requests();
    assert_eq!(requests.len(), 1, "an empty page token ends the listing");
    assert_eq!(header(&requests[0].1, "x-goog-api-key"), Some("gemini-key"));
    assert_eq!(header(&requests[0].1, "authorization"), None);
}

#[tokio::test]
async fn ollama_discovery_reads_tags_then_each_models_details() {
    let router = Router::new()
        .route(
            "/api/tags",
            get(|| async { include_str!("fixtures/ollama_tags.json") }),
        )
        .route(
            "/api/show",
            post(|Json(body): Json<serde_json::Value>| async move {
                match body["model"].as_str() {
                    Some("qwen3:8b") => include_str!("fixtures/ollama_show_qwen3.json"),
                    Some("gemma4:12b") => include_str!("fixtures/ollama_show_gemma4.json"),
                    Some("embeddinggemma:latest") => {
                        include_str!("fixtures/ollama_show_embeddinggemma.json")
                    }
                    other => panic!("unexpected model {other:?}"),
                }
            }),
        );
    let base = serve(router).await;

    let found = discover_at(ProviderKind::Ollama, &format!("{base}/v1"), None, &[])
        .await
        .unwrap();

    assert_eq!(ids(&found.models), ["gemma4:12b", "qwen3:8b"]);
    assert_eq!(find(&found.models, "qwen3:8b").context_window, Some(40_960));
}

#[tokio::test]
async fn a_rejected_key_is_reported_plainly_without_repeating_it() {
    let key = stand_in_key("rejected");
    let echoed = key.clone();
    let router = Router::new().route(
        "/models",
        get(move || {
            let echoed = echoed.clone();
            async move {
                (
                    StatusCode::UNAUTHORIZED,
                    format!("{{\"error\":{{\"message\":\"invalid key {echoed}\"}}}}"),
                )
            }
        }),
    );
    let base = serve(router).await;

    let error = discover_at(ProviderKind::Openrouter, &base, Some(&key), &[])
        .await
        .unwrap_err();

    assert_eq!(error.kind(), "provider_credential_rejected");
    assert!(error
        .message()
        .contains("OpenRouter rejected the saved API key"));
    assert!(error.message().contains("HTTP 401"));
    assert!(!error.message().contains(&key), "{}", error.message());
}

#[tokio::test]
async fn other_failures_name_the_status_or_the_broken_listing() {
    let router = Router::new()
        .route(
            "/failing/models",
            get(|| async { (StatusCode::SERVICE_UNAVAILABLE, "down for maintenance") }),
        )
        .route("/garbled/models", get(|| async { "not a model list" }));
    let base = serve(router).await;

    let error = discover_at(
        ProviderKind::OpenaiCompatible,
        &format!("{base}/failing"),
        Some("key"),
        &[],
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), "provider_discovery_failed");
    assert!(error.message().contains("HTTP 503"), "{}", error.message());
    assert!(!error.message().contains("maintenance"));

    let error = discover_at(
        ProviderKind::OpenaiCompatible,
        &format!("{base}/garbled"),
        Some("key"),
        &[],
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), "provider_discovery_failed");
    assert!(
        error.message().contains("could not read"),
        "{}",
        error.message()
    );
}

#[tokio::test]
async fn an_unreachable_endpoint_is_reported_as_unreachable() {
    // Bind and drop a listener so the port is known to be closed.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);

    let error = discover_at(
        ProviderKind::Ollama,
        &format!("http://{address}/v1"),
        None,
        &[],
    )
    .await
    .unwrap_err();

    assert_eq!(error.kind(), "provider_unreachable");
    assert!(error.message().contains("could not reach Ollama"));
}

// ---------------------------------------------------------------------------
// The guards in front of any request.

#[derive(Default)]
struct TestSecrets(Mutex<HashMap<String, String>>);

#[async_trait::async_trait]
impl SecretProvider for TestSecrets {
    async fn get_secret(&self, key: &str) -> tidebreak_core::Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }

    async fn set_secret(&self, key: &str, value: &str) -> tidebreak_core::Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    async fn delete_secret(&self, key: &str) -> tidebreak_core::Result<()> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}

async fn test_store() -> (DbStore, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let store = DbStore::connect(&format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("discovery.db").display()
    ))
    .await
    .unwrap();
    (store, directory)
}

fn unmanaged() -> ManagedPolicy {
    crate::managed_policy::resolve(
        &*crate::managed_policy::MemoryProvisionedPolicy::new(),
        &crate::managed_policy::NoOsPolicy,
    )
    .unwrap()
}

#[tokio::test]
async fn discovery_refuses_before_any_request_when_it_cannot_proceed() {
    let (store, _directory) = test_store().await;
    let secrets = TestSecrets::default();

    let error = discover_models(&store, &secrets, ProviderKind::ModelGateway, &unmanaged())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "discovery_unsupported");

    let provisioned = crate::managed_policy::MemoryProvisionedPolicy::new();
    crate::managed_policy::provision(&*provisioned, "https://gateway.example").unwrap();
    let managed =
        crate::managed_policy::resolve(&*provisioned, &crate::managed_policy::NoOsPolicy).unwrap();
    let error = discover_models(&store, &secrets, ProviderKind::Anthropic, &managed)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "managed_profile");

    // No stored key and no environment fallback for this kind.
    let error = discover_models(
        &store,
        &secrets,
        ProviderKind::OpenaiCompatible,
        &unmanaged(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), "provider_credential_missing");
    assert!(error
        .message()
        .contains("Save an API key for OpenAI-compatible first"));

    // A key but no endpoint to send it to.
    providers::write_credential(
        &secrets,
        ProviderKind::OpenaiCompatible,
        &ProviderCredential::api_key("compat-key"),
    )
    .await
    .unwrap();
    let error = discover_models(
        &store,
        &secrets,
        ProviderKind::OpenaiCompatible,
        &unmanaged(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), "provider_endpoint_missing");

    // A cleartext endpoint on another host never receives the key.
    providers::write_config(
        &store,
        ProviderKind::OpenaiCompatible,
        &providers::ProviderConfig {
            enabled: true,
            base_url: Some("http://models.internal/v1".into()),
            models: Vec::new(),
        },
    )
    .await
    .unwrap();
    let error = discover_models(
        &store,
        &secrets,
        ProviderKind::OpenaiCompatible,
        &unmanaged(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), "provider_endpoint_insecure");
}

#[tokio::test]
async fn chatgpt_sign_in_alone_cannot_list_openai_models() {
    let (store, _directory) = test_store().await;
    let secrets = TestSecrets::default();
    providers::write_credential(
        &secrets,
        ProviderKind::Openai,
        &ProviderCredential::Oauth {},
    )
    .await
    .unwrap();

    let error = discover_models(&store, &secrets, ProviderKind::Openai, &unmanaged())
        .await
        .unwrap_err();

    assert_eq!(error.kind(), "provider_credential_missing");
    assert!(error.message().contains("needs an OpenAI API key"));
}
