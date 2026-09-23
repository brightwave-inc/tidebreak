//! Find the models a direct provider serves, with the credential the reader
//! already saved.
//!
//! Discovery reads the provider's own model listing, keeps the rows that can
//! hold a conversation, and fills in whatever limits and capabilities the
//! listing reports. It marks the ids Tidebreak already knows and adds nothing
//! by itself: the reader picks what to add, and saving goes through the
//! ordinary configured-model validation.
//!
//! The credential never leaves this process. The response carries model data
//! only, a row that echoes the key is dropped, and an error names the status
//! or the failure but never repeats the provider's response body.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use futures::stream::{self, StreamExt};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tidebreak_core::{ReasoningEffort, SecretProvider, Store};

use crate::error::ServerError;
use crate::managed_policy::ManagedPolicy;
use crate::model_registry;
use crate::providers::{self, CustomModelConfig, ProviderCredential, ProviderKind};

/// The whole budget for one discovery, every request included.
pub const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);
/// The largest listing body read from a provider. OpenRouter's full catalog,
/// the largest listing here, is a few megabytes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
/// The most listing pages followed on a paginated provider.
const MAX_PAGES: usize = 8;
/// The most rows one discovery returns.
const MAX_MODELS: usize = 2_000;
/// The most local Ollama models inspected one at a time for their details.
const MAX_OLLAMA_MODELS: usize = 100;
/// How many Ollama models are inspected at once.
const OLLAMA_SHOW_CONCURRENCY: usize = 4;

/// The endpoints the chat adapters call when nothing overrides them. Discovery
/// has to reach the same host the saved key belongs to.
const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Response for `POST /providers/{kind}/models/discover`.
//
// A response record, so it ignores unknown keys like the other REST records:
// a client a release behind must still read a newer server's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct DiscoveredModels {
    /// The provider that was asked.
    pub provider: ProviderKind,
    /// The chat models the provider reported, sorted by id.
    pub models: Vec<DiscoveredModel>,
}

/// One chat model a provider reported.
///
/// A field the provider's listing does not report is absent rather than
/// guessed, so a form built from this row can fall back to its own default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct DiscoveredModel {
    /// Exact model id the provider accepts.
    pub id: String,
    /// The provider's own name for the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub display_name: Option<String>,
    /// Context window in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub context_window: Option<u32>,
    /// Maximum output in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_output_tokens: Option<u32>,
    /// Whether the model accepts image input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub image_input: Option<bool>,
    /// Whether the model reasons in a way this provider's route can request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub supports_reasoning: Option<bool>,
    /// Reasoning-effort levels the provider reports, limited to the ones this
    /// provider's route sends, ascending.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_efforts: Vec<ReasoningEffort>,
    /// Whether the model accepts function tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub supports_tools: Option<bool>,
    /// A built-in model already covers this id.
    pub built_in: bool,
    /// A custom model with this id is already saved.
    pub added: bool,
}

impl DiscoveredModel {
    fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            display_name: None,
            context_window: None,
            max_output_tokens: None,
            image_input: None,
            supports_reasoning: None,
            reasoning_efforts: Vec::new(),
            supports_tools: None,
            built_in: false,
            added: false,
        }
    }
}

/// List the chat models `kind` serves, read with its saved credential.
///
/// Refused on a managed profile, whose provider keys are locked, and for the
/// gateway, whose models come from its own catalog.
pub async fn discover_models(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    kind: ProviderKind,
    policy: &ManagedPolicy,
) -> std::result::Result<DiscoveredModels, ServerError> {
    if !kind.accepts_configured_models() {
        return Err(ServerError::bad_request_kind(
            "discovery_unsupported",
            "the model gateway lists its models through its own catalog",
        ));
    }
    if policy.managed {
        return Err(providers::managed_profile_refusal(
            "this profile is managed by a model gateway; provider API keys are locked",
        ));
    }
    let config = providers::read_config(store, kind).await?;
    let api_key = discovery_key(secrets, kind).await?;
    let base = discovery_base(kind, config.base_url.as_deref())?;
    if !providers::base_url_is_allowed(&base, !kind.requires_credential() && api_key.is_none()) {
        return Err(ServerError::bad_request_kind(
            "provider_endpoint_insecure",
            format!(
                "{} needs an HTTPS endpoint before Tidebreak sends it a key",
                kind.display_name()
            ),
        ));
    }
    discover_at(kind, &base, api_key.as_deref(), &config.models).await
}

/// Discover against an explicit endpoint. Split from [`discover_models`] so
/// tests can point a fixed-endpoint provider at a local stand-in.
pub(crate) async fn discover_at(
    kind: ProviderKind,
    base: &str,
    api_key: Option<&str>,
    configured: &[CustomModelConfig],
) -> std::result::Result<DiscoveredModels, ServerError> {
    let http = reqwest::Client::builder()
        .timeout(DISCOVERY_TIMEOUT)
        // A redirect could carry the key to another host.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| ServerError::internal("could not build the model discovery client"))?;
    let fetcher = Fetcher {
        http,
        kind,
        api_key: api_key.map(str::to_owned),
    };
    let listing = tokio::time::timeout(DISCOVERY_TIMEOUT, fetch_listing(&fetcher, base))
        .await
        .unwrap_or(Err(FetchError::TimedOut))
        .map_err(|error| error.into_server_error(kind))?;
    Ok(DiscoveredModels {
        provider: kind,
        models: finish(kind, listing, configured, api_key),
    })
}

/// The key discovery sends. Ollama may go without one; every other provider
/// needs the key the reader saved, or its environment fallback.
async fn discovery_key(
    secrets: &dyn SecretProvider,
    kind: ProviderKind,
) -> std::result::Result<Option<String>, ServerError> {
    let name = kind.display_name();
    match providers::read_credential(secrets, kind).await {
        Ok(Some(ProviderCredential::Oauth {})) => {
            return Err(ServerError::conflict_kind(
                "provider_credential_missing",
                "Finding models needs an OpenAI API key. ChatGPT sign-in cannot list models.",
            ));
        }
        Err(_) => {
            return Err(ServerError::conflict_kind(
                "provider_credential_unreadable",
                format!("The saved {name} credential could not be read. Save it again."),
            ));
        }
        Ok(_) => {}
    }
    let key = providers::resolve_api_key(secrets, kind).await;
    if key.is_none() && kind.requires_credential() {
        return Err(ServerError::conflict_kind(
            "provider_credential_missing",
            format!("Save an API key for {name} first. Finding models uses the key you saved."),
        ));
    }
    Ok(key)
}

/// The API root discovery calls: the same one the chat route uses.
fn discovery_base(
    kind: ProviderKind,
    configured: Option<&str>,
) -> std::result::Result<String, ServerError> {
    let base = match kind {
        ProviderKind::Anthropic => kind
            .effective_base_url(configured)
            .unwrap_or_else(|| ANTHROPIC_BASE_URL.to_owned()),
        ProviderKind::Openai => kind
            .effective_base_url(configured)
            .unwrap_or_else(|| OPENAI_BASE_URL.to_owned()),
        // Fixed first-party endpoints: a stored value never redirects them.
        ProviderKind::Xai => tidebreak_router::xai::DEFAULT_BASE_URL.to_owned(),
        ProviderKind::Gemini => GEMINI_BASE_URL.to_owned(),
        _ => kind.effective_base_url(configured).ok_or_else(|| {
            ServerError::bad_request_kind(
                "provider_endpoint_missing",
                format!("Set the {} base URL first.", kind.display_name()),
            )
        })?,
    };
    Ok(base.trim_end_matches('/').to_owned())
}

/// Why a provider's listing could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FetchError {
    /// The provider refused the credential.
    Rejected(u16),
    /// Any other unsuccessful status.
    Status(u16),
    /// No connection could be made.
    Unreachable,
    /// The provider took longer than the discovery budget.
    TimedOut,
    /// The body was not the listing this provider documents.
    Unreadable,
    /// The body was larger than any listing should be.
    TooLarge,
}

impl FetchError {
    fn into_server_error(self, kind: ProviderKind) -> ServerError {
        let name = kind.display_name();
        match self {
            Self::Rejected(status) => ServerError::bad_gateway_kind(
                "provider_credential_rejected",
                format!("{name} rejected the saved API key (HTTP {status}). Save a valid key and try again."),
            ),
            Self::Status(status) => ServerError::bad_gateway_kind(
                "provider_discovery_failed",
                format!("{name} answered the model list request with HTTP {status}."),
            ),
            Self::Unreachable => ServerError::bad_gateway_kind(
                "provider_unreachable",
                format!("Tidebreak could not reach {name}. Check the endpoint and your network connection."),
            ),
            Self::TimedOut => ServerError::bad_gateway_kind(
                "provider_unreachable",
                format!(
                    "{name} did not answer within {} seconds.",
                    DISCOVERY_TIMEOUT.as_secs()
                ),
            ),
            Self::Unreadable => ServerError::bad_gateway_kind(
                "provider_discovery_failed",
                format!("{name} returned a model list Tidebreak could not read."),
            ),
            Self::TooLarge => ServerError::bad_gateway_kind(
                "provider_discovery_failed",
                format!("{name} returned a model list larger than Tidebreak reads."),
            ),
        }
    }
}

type FetchResult<T> = std::result::Result<T, FetchError>;

/// One provider's authenticated reader.
struct Fetcher {
    http: reqwest::Client,
    kind: ProviderKind,
    api_key: Option<String>,
}

impl Fetcher {
    async fn get<T: DeserializeOwned>(&self, url: &str) -> FetchResult<T> {
        self.send(self.http.get(url)).await
    }

    async fn post<T: DeserializeOwned>(&self, url: &str, body: &Value) -> FetchResult<T> {
        self.send(self.http.post(url).json(body)).await
    }

    async fn send<T: DeserializeOwned>(&self, request: reqwest::RequestBuilder) -> FetchResult<T> {
        let response = self.authorize(request).send().await.map_err(transport)?;
        let status = response.status();
        if matches!(status.as_u16(), 401 | 403) {
            return Err(FetchError::Rejected(status.as_u16()));
        }
        if !status.is_success() {
            return Err(FetchError::Status(status.as_u16()));
        }
        let body = read_bounded(response).await?;
        serde_json::from_slice(&body).map_err(|_| FetchError::Unreadable)
    }

    /// Each provider's own credential header: Anthropic and Gemini name
    /// theirs, everyone else takes a bearer token.
    fn authorize(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let request = match self.kind {
            ProviderKind::Anthropic => request.header("anthropic-version", ANTHROPIC_VERSION),
            _ => request,
        };
        let Some(key) = self.api_key.as_deref() else {
            return request;
        };
        match self.kind {
            ProviderKind::Anthropic => request.header("x-api-key", key),
            ProviderKind::Gemini => request.header("x-goog-api-key", key),
            _ => request.bearer_auth(key),
        }
    }
}

/// A listing URL with its query encoded, so a page token the provider chose
/// cannot reshape the request.
fn listing_url(path: &str, query: &[(&str, &str)]) -> FetchResult<String> {
    let mut url = reqwest::Url::parse(path).map_err(|_| FetchError::Unreachable)?;
    url.query_pairs_mut().extend_pairs(query);
    Ok(url.into())
}

fn transport(error: reqwest::Error) -> FetchError {
    if error.is_timeout() {
        FetchError::TimedOut
    } else {
        FetchError::Unreachable
    }
}

async fn read_bounded(mut response: reqwest::Response) -> FetchResult<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY_BYTES as u64)
    {
        return Err(FetchError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport)? {
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(FetchError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn fetch_listing(fetcher: &Fetcher, base: &str) -> FetchResult<Vec<DiscoveredModel>> {
    match fetcher.kind {
        ProviderKind::Anthropic => fetch_anthropic(fetcher, base).await,
        ProviderKind::Openai => Ok(parse_openai(fetcher.get(&format!("{base}/models")).await?)),
        ProviderKind::Xai => fetch_xai(fetcher, base).await,
        ProviderKind::Gemini => fetch_gemini(fetcher, base).await,
        ProviderKind::Fireworks => fetch_fireworks(fetcher, base).await,
        ProviderKind::Together => Ok(parse_together(
            fetcher.get(&format!("{base}/models")).await?,
        )),
        ProviderKind::Openrouter => Ok(parse_openrouter(
            fetcher.get(&format!("{base}/models")).await?,
        )),
        ProviderKind::Ollama => fetch_ollama(fetcher, base).await,
        ProviderKind::OpenaiCompatible => Ok(parse_compatible(
            fetcher.get(&format!("{base}/models")).await?,
        )),
        ProviderKind::ModelGateway => Ok(Vec::new()),
    }
}

/// Clean and mark what a provider reported: drop malformed and duplicate ids
/// and any row that echoes the key, keep only limits a configured row may
/// carry, and say which ids Tidebreak already knows.
fn finish(
    kind: ProviderKind,
    listing: Vec<DiscoveredModel>,
    configured: &[CustomModelConfig],
    api_key: Option<&str>,
) -> Vec<DiscoveredModel> {
    let accepted_efforts = kind.custom_reasoning_efforts();
    let echoes_key = |text: &str| api_key.is_some_and(|key| !key.is_empty() && text.contains(key));
    let mut seen = HashSet::new();
    let mut models: Vec<DiscoveredModel> = listing
        .into_iter()
        .filter(|model| providers::is_valid_configured_model_id(&model.id))
        .filter(|model| !echoes_key(&model.id))
        .filter(|model| seen.insert(model.id.clone()))
        .map(|mut model| {
            model.display_name = model
                .display_name
                .map(|name| name.trim().to_owned())
                .filter(|name| {
                    !name.is_empty()
                        && name.chars().count() <= providers::MAX_DISPLAY_NAME_CHARS
                        && !name.chars().any(char::is_control)
                        && !echoes_key(name)
                });
            model.context_window = model.context_window.filter(|window| {
                (providers::MIN_CONTEXT_WINDOW..=providers::MAX_CONTEXT_WINDOW).contains(window)
            });
            let context = model.context_window;
            model.max_output_tokens = model
                .max_output_tokens
                .filter(|output| *output > 0 && context.is_none_or(|window| *output <= window));
            if model.supports_reasoning == Some(false) {
                model.reasoning_efforts.clear();
            }
            model
                .reasoning_efforts
                .retain(|effort| accepted_efforts.contains(effort));
            model.reasoning_efforts.sort_unstable();
            model.reasoning_efforts.dedup();
            model.built_in = model_registry::find_for(kind, &model.id).is_some();
            model.added = configured.iter().any(|row| row.id == model.id);
            model
        })
        .collect();
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.truncate(MAX_MODELS);
    models
}

/// Keep each row of a listing that has the shape this reader expects, and
/// skip the rest rather than failing the whole listing over one odd row.
fn rows<T: DeserializeOwned>(values: Vec<Value>) -> Vec<T> {
    values
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect()
}

/// A token count as the configured row carries it. Zero, negative, and
/// out-of-range values mean "not reported".
fn tokens(value: Option<u64>) -> Option<u32> {
    value
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
}

fn effort_named(name: &str) -> Option<ReasoningEffort> {
    ReasoningEffort::from_str(&name.trim().to_ascii_lowercase())
}

/// The levels a listing names, ascending and without repeats, whatever order
/// the provider wrote them in.
fn effort_levels<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<ReasoningEffort> {
    let mut levels: Vec<_> = names.into_iter().filter_map(effort_named).collect();
    levels.sort_unstable();
    levels.dedup();
    levels
}

// ---------------------------------------------------------------------------
// Anthropic: GET /v1/models, paginated, with limits and capabilities.

#[derive(Deserialize)]
struct AnthropicPage {
    data: Vec<Value>,
    #[serde(default)]
    has_more: bool,
    #[serde(default)]
    last_id: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicModel {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    max_input_tokens: Option<u64>,
    #[serde(default)]
    max_tokens: Option<u64>,
    #[serde(default)]
    capabilities: Option<AnthropicCapabilities>,
}

#[derive(Deserialize)]
struct AnthropicCapabilities {
    #[serde(default)]
    image_input: Option<Supported>,
    #[serde(default)]
    thinking: Option<Supported>,
    #[serde(default)]
    effort: Option<BTreeMap<String, Value>>,
}

#[derive(Deserialize)]
struct Supported {
    #[serde(default)]
    supported: bool,
}

async fn fetch_anthropic(fetcher: &Fetcher, base: &str) -> FetchResult<Vec<DiscoveredModel>> {
    let mut models = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut query = vec![("limit", "1000")];
        if let Some(after) = after.as_deref() {
            query.push(("after_id", after));
        }
        let url = listing_url(&format!("{base}/v1/models"), &query)?;
        let page: AnthropicPage = fetcher.get(&url).await?;
        let (rows, next) = parse_anthropic_page(page);
        models.extend(rows);
        match next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    Ok(models)
}

/// One page of Anthropic's listing, and the cursor for the next one.
fn parse_anthropic_page(page: AnthropicPage) -> (Vec<DiscoveredModel>, Option<String>) {
    let models = rows::<AnthropicModel>(page.data)
        .into_iter()
        .map(|model| {
            let mut row = DiscoveredModel::new(model.id);
            row.display_name = model.display_name;
            row.context_window = tokens(model.max_input_tokens);
            row.max_output_tokens = tokens(model.max_tokens);
            if let Some(capabilities) = model.capabilities {
                row.image_input = capabilities.image_input.map(|flag| flag.supported);
                // The adapter reads the reasoning shape from the id and sends
                // a thinking block only to Claude 4.6 and later. An older model
                // that reasons on the budget form would never be asked to.
                let thinks = capabilities.thinking.map(|flag| flag.supported);
                row.supports_reasoning = thinks
                    .map(|thinks| thinks && tidebreak_router::anthropic::sends_reasoning(&row.id));
                if row.supports_reasoning == Some(true) {
                    row.reasoning_efforts = capabilities
                        .effort
                        .map(|levels| anthropic_efforts(&levels))
                        .unwrap_or_default();
                }
            }
            row
        })
        .collect();
    let next = page.has_more.then_some(page.last_id).flatten();
    (models, next)
}

/// The levels in Anthropic's `capabilities.effort` object whose `supported`
/// flag is set. The object also carries a top-level `supported` flag for the
/// whole control, which names no level.
fn anthropic_efforts(levels: &BTreeMap<String, Value>) -> Vec<ReasoningEffort> {
    effort_levels(
        levels
            .iter()
            .filter(|(_, flag)| flag.get("supported").and_then(Value::as_bool) == Some(true))
            .map(|(name, _)| name.as_str()),
    )
}

// ---------------------------------------------------------------------------
// OpenAI: GET /v1/models lists ids only, text and media alike.

#[derive(Deserialize)]
struct IdList {
    data: Vec<Value>,
}

#[derive(Deserialize)]
struct IdOnly {
    id: String,
}

fn parse_openai(list: IdList) -> Vec<DiscoveredModel> {
    rows::<IdOnly>(list.data)
        .into_iter()
        .filter(|model| openai_text_model(&model.id))
        .map(|model| DiscoveredModel::new(model.id))
        .collect()
}

/// Whether an OpenAI model id names a text model the Responses API serves.
///
/// The listing carries no capability fields, so the id is all there is. It
/// keeps the GPT and o-series families, including fine-tunes of them, and
/// drops the audio, voice, image, video, embedding, moderation, and legacy
/// completion models the same listing contains. `search` also catches the
/// deep research models, which answer only with their own research tools.
fn openai_text_model(id: &str) -> bool {
    const FAMILIES: &[&str] = &[
        "gpt-", "o1", "o3", "o4", "chatgpt-", "codex-", "ft:gpt-", "ft:o",
    ];
    const NOT_TEXT: &[&str] = &[
        "embedding",
        "tts",
        "whisper",
        "transcribe",
        "audio",
        "realtime",
        "live",
        "image",
        "dall-e",
        "moderation",
        "search",
        "sora",
        "instruct",
    ];
    let id = id.to_ascii_lowercase();
    FAMILIES.iter().any(|family| id.starts_with(family))
        && !NOT_TEXT.iter().any(|marker| id.contains(marker))
}

// ---------------------------------------------------------------------------
// Generic OpenAI-compatible servers: GET {base}/models.

#[derive(Deserialize)]
struct CompatibleModel {
    id: String,
    /// vLLM's name for the context window.
    #[serde(default)]
    max_model_len: Option<u64>,
    /// The name several hosted servers use for the same thing.
    #[serde(default)]
    context_length: Option<u64>,
}

fn parse_compatible(list: IdList) -> Vec<DiscoveredModel> {
    rows::<CompatibleModel>(list.data)
        .into_iter()
        .filter(|model| !embedding_like(&model.id))
        .map(|model| {
            let mut row = DiscoveredModel::new(model.id);
            row.context_window = tokens(model.max_model_len.or(model.context_length));
            row
        })
        .collect()
}

/// Whether an id from a listing with no capability fields names a model that
/// cannot hold a conversation.
fn embedding_like(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    ["embed", "rerank", "whisper", "tts"]
        .iter()
        .any(|marker| id.contains(marker))
}

// ---------------------------------------------------------------------------
// xAI: GET /v1/language-models for chat models and their modalities, joined
// with GET /v1/models for context length.

#[derive(Deserialize)]
struct XaiLanguageModels {
    models: Vec<Value>,
}

#[derive(Deserialize)]
struct XaiLanguageModel {
    id: String,
    #[serde(default)]
    input_modalities: Option<Vec<String>>,
    #[serde(default)]
    output_modalities: Option<Vec<String>>,
    #[serde(default)]
    capabilities: Option<XaiCapabilities>,
}

#[derive(Deserialize)]
struct XaiCapabilities {
    #[serde(default)]
    reasoning_effort: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct XaiModel {
    id: String,
    #[serde(default)]
    context_length: Option<u64>,
}

async fn fetch_xai(fetcher: &Fetcher, base: &str) -> FetchResult<Vec<DiscoveredModel>> {
    let language: XaiLanguageModels = fetcher.get(&format!("{base}/language-models")).await?;
    // Context length lives only on the general listing. Without it the rows
    // still stand; the reader's own values or the defaults fill the gap.
    let context: Option<IdList> = fetcher.get(&format!("{base}/models")).await.ok();
    Ok(parse_xai(language, context))
}

fn parse_xai(language: XaiLanguageModels, context: Option<IdList>) -> Vec<DiscoveredModel> {
    let windows: BTreeMap<String, u64> = context
        .map(|list| rows::<XaiModel>(list.data))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|model| Some((model.id, model.context_length?)))
        .collect();
    rows::<XaiLanguageModel>(language.models)
        .into_iter()
        .filter(|model| {
            model
                .output_modalities
                .as_ref()
                .is_none_or(|outputs| outputs.iter().any(|output| output == "text"))
        })
        .map(|model| {
            let mut row = DiscoveredModel::new(model.id);
            row.context_window = tokens(windows.get(&row.id).copied());
            row.image_input = model
                .input_modalities
                .map(|inputs| inputs.iter().any(|input| input == "image"));
            if let Some(levels) = model
                .capabilities
                .and_then(|capabilities| capabilities.reasoning_effort)
            {
                row.supports_reasoning = Some(!levels.is_empty());
                row.reasoning_efforts = effort_levels(levels.iter().map(String::as_str));
            }
            row
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Gemini: GET /v1beta/models, paginated.

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiPage {
    #[serde(default)]
    models: Vec<Value>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiModel {
    name: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    input_token_limit: Option<u64>,
    #[serde(default)]
    output_token_limit: Option<u64>,
    #[serde(default)]
    supported_generation_methods: Option<Vec<String>>,
    #[serde(default)]
    thinking: Option<bool>,
}

async fn fetch_gemini(fetcher: &Fetcher, base: &str) -> FetchResult<Vec<DiscoveredModel>> {
    let mut models = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut query = vec![("pageSize", "1000")];
        if let Some(token) = token.as_deref() {
            query.push(("pageToken", token));
        }
        let url = listing_url(&format!("{base}/v1beta/models"), &query)?;
        let page: GeminiPage = fetcher.get(&url).await?;
        let (rows, next) = parse_gemini_page(page);
        models.extend(rows);
        match next {
            Some(next) => token = Some(next),
            None => break,
        }
    }
    Ok(models)
}

fn parse_gemini_page(page: GeminiPage) -> (Vec<DiscoveredModel>, Option<String>) {
    let models = rows::<GeminiModel>(page.models)
        .into_iter()
        .filter(|model| {
            model
                .supported_generation_methods
                .as_ref()
                .is_some_and(|methods| methods.iter().any(|method| method == "generateContent"))
        })
        .filter_map(|model| {
            let id = model.name.strip_prefix("models/").unwrap_or(&model.name);
            if !gemini_chat_model(id) {
                return None;
            }
            let mut row = DiscoveredModel::new(id);
            row.display_name = model.display_name;
            row.context_window = tokens(model.input_token_limit);
            row.max_output_tokens = tokens(model.output_token_limit);
            row.supports_reasoning = model.thinking;
            Some(row)
        })
        .collect();
    let next = page.next_page_token.filter(|token| !token.is_empty());
    (models, next)
}

/// Whether a Gemini id that serves `generateContent` is a text chat model.
///
/// The Model resource has no field for what a model outputs, so the families
/// that speak, draw, film, or embed are told apart by name.
fn gemini_chat_model(id: &str) -> bool {
    const NOT_CHAT: &[&str] = &[
        "embedding",
        "imagen",
        "veo",
        "tts",
        "live",
        "native-audio",
        "transcribe",
        "translate",
        "robotics",
        "lyria",
        "image",
        "antigravity",
        "aqa",
    ];
    let id = id.to_ascii_lowercase();
    !NOT_CHAT.iter().any(|marker| id.contains(marker))
}

// ---------------------------------------------------------------------------
// Fireworks: the documented model listing is the control plane's
// GET /v1/accounts/fireworks/models, filtered to serverless models and
// paginated. The inference root's /models route is undocumented.

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FireworksPage {
    #[serde(default)]
    models: Vec<Value>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FireworksModel {
    /// The full `accounts/<account>/models/<model>` path, which is exactly
    /// the id Chat Completions takes.
    name: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    context_length: Option<u64>,
    #[serde(default)]
    supports_image_input: Option<bool>,
    #[serde(default)]
    supports_tools: Option<bool>,
    /// Present when the Chat Completions API is enabled for the model.
    #[serde(default)]
    conversation_config: Option<Value>,
    #[serde(default)]
    kind: Option<String>,
}

/// The control-plane root beside the Chat Completions root
/// (`https://api.fireworks.ai/inference/v1` → `https://api.fireworks.ai`).
fn fireworks_control_root(base: &str) -> &str {
    let base = base.trim_end_matches('/');
    base.strip_suffix("/inference/v1").unwrap_or(base)
}

async fn fetch_fireworks(fetcher: &Fetcher, base: &str) -> FetchResult<Vec<DiscoveredModel>> {
    let root = fireworks_control_root(base);
    let mut models = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut query = vec![("filter", "supports_serverless=true"), ("pageSize", "200")];
        if let Some(token) = token.as_deref() {
            query.push(("pageToken", token));
        }
        let url = listing_url(&format!("{root}/v1/accounts/fireworks/models"), &query)?;
        let page: FireworksPage = fetcher.get(&url).await?;
        let (rows, next) = parse_fireworks_page(page);
        models.extend(rows);
        match next {
            Some(next) => token = Some(next),
            None => break,
        }
    }
    Ok(models)
}

fn parse_fireworks_page(page: FireworksPage) -> (Vec<DiscoveredModel>, Option<String>) {
    let models = rows::<FireworksModel>(page.models)
        .into_iter()
        .filter(|model| {
            model
                .conversation_config
                .as_ref()
                .is_some_and(|config| !config.is_null())
                && model.kind.as_deref() != Some("EMBEDDING_MODEL")
        })
        .map(|model| {
            let mut row = DiscoveredModel::new(model.name);
            row.display_name = model.display_name;
            row.context_window = tokens(model.context_length);
            row.image_input = model.supports_image_input;
            row.supports_tools = model.supports_tools;
            row
        })
        .collect();
    let next = page.next_page_token.filter(|token| !token.is_empty());
    (models, next)
}

// ---------------------------------------------------------------------------
// Together: GET /v1/models returns a bare array with a `type` per model.

#[derive(Deserialize)]
struct TogetherModel {
    id: String,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    context_length: Option<u64>,
}

fn parse_together(list: Vec<Value>) -> Vec<DiscoveredModel> {
    rows::<TogetherModel>(list)
        .into_iter()
        .filter(|model| model.kind.as_deref() == Some("chat"))
        .map(|model| {
            let mut row = DiscoveredModel::new(model.id);
            row.display_name = model.display_name;
            row.context_window = tokens(model.context_length);
            row
        })
        .collect()
}

// ---------------------------------------------------------------------------
// OpenRouter: GET /api/v1/models, with modalities and supported parameters.

#[derive(Deserialize)]
struct OpenRouterModel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    context_length: Option<u64>,
    #[serde(default)]
    architecture: Option<OpenRouterArchitecture>,
    #[serde(default)]
    top_provider: Option<OpenRouterTopProvider>,
    #[serde(default)]
    supported_parameters: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct OpenRouterArchitecture {
    #[serde(default)]
    input_modalities: Option<Vec<String>>,
    #[serde(default)]
    output_modalities: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct OpenRouterTopProvider {
    #[serde(default)]
    context_length: Option<u64>,
    #[serde(default)]
    max_completion_tokens: Option<u64>,
}

fn parse_openrouter(list: IdList) -> Vec<DiscoveredModel> {
    rows::<OpenRouterModel>(list.data)
        .into_iter()
        .filter(|model| {
            let architecture = model.architecture.as_ref();
            let takes_text = architecture
                .and_then(|architecture| architecture.input_modalities.as_ref())
                .is_none_or(|inputs| inputs.iter().any(|input| input == "text"));
            let writes_text = architecture
                .and_then(|architecture| architecture.output_modalities.as_ref())
                .is_none_or(|outputs| outputs.iter().any(|output| output == "text"));
            takes_text && writes_text
        })
        .map(|model| {
            let mut row = DiscoveredModel::new(model.id);
            row.display_name = model.name;
            let top = model.top_provider.as_ref();
            row.context_window = tokens(
                model
                    .context_length
                    .or_else(|| top.and_then(|top| top.context_length)),
            );
            row.max_output_tokens = tokens(top.and_then(|top| top.max_completion_tokens));
            row.image_input = model
                .architecture
                .as_ref()
                .and_then(|architecture| architecture.input_modalities.as_ref())
                .map(|inputs| inputs.iter().any(|input| input == "image"));
            if let Some(parameters) = model.supported_parameters.as_ref() {
                let supports = |name: &str| parameters.iter().any(|parameter| parameter == name);
                row.supports_tools = Some(supports("tools"));
                row.supports_reasoning = Some(supports("reasoning"));
            }
            row
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Ollama: the native API beside the OpenAI-compatible root. GET /api/tags
// names the local models, and POST /api/show describes each one.

#[derive(Deserialize)]
struct OllamaTags {
    #[serde(default)]
    models: Vec<Value>,
}

#[derive(Deserialize)]
struct OllamaTag {
    name: String,
}

#[derive(Deserialize)]
struct OllamaShow {
    #[serde(default)]
    capabilities: Option<Vec<String>>,
    #[serde(default)]
    model_info: Option<serde_json::Map<String, Value>>,
}

/// The native API root for an Ollama chat endpoint: the configured `/v1`
/// root without its `/v1`.
fn ollama_native_root(base: &str) -> &str {
    let base = base.trim_end_matches('/');
    base.strip_suffix("/v1").unwrap_or(base)
}

async fn fetch_ollama(fetcher: &Fetcher, base: &str) -> FetchResult<Vec<DiscoveredModel>> {
    let root = ollama_native_root(base);
    let tags: OllamaTags = fetcher.get(&format!("{root}/api/tags")).await?;
    let names: Vec<String> = rows::<OllamaTag>(tags.models)
        .into_iter()
        .map(|tag| tag.name)
        .take(MAX_OLLAMA_MODELS)
        .collect();
    let show_url = format!("{root}/api/show");
    let details: Vec<(String, Option<OllamaShow>)> = stream::iter(names)
        .map(|name| {
            let show_url = show_url.as_str();
            async move {
                // One model's details failing leaves that row with unknowns
                // rather than failing the whole listing.
                let show = fetcher
                    .post(show_url, &serde_json::json!({ "model": name }))
                    .await
                    .ok();
                (name, show)
            }
        })
        .buffered(OLLAMA_SHOW_CONCURRENCY)
        .collect()
        .await;
    Ok(details
        .into_iter()
        .filter_map(|(name, show)| parse_ollama_model(name, show))
        .collect())
}

/// One local Ollama model, or `None` for one that cannot chat.
fn parse_ollama_model(name: String, show: Option<OllamaShow>) -> Option<DiscoveredModel> {
    let mut row = DiscoveredModel::new(name);
    let Some(show) = show else {
        return Some(row);
    };
    if let Some(capabilities) = show.capabilities.as_ref() {
        let has = |capability: &str| capabilities.iter().any(|entry| entry == capability);
        if !has("completion") {
            return None;
        }
        row.image_input = Some(has("vision"));
        row.supports_tools = Some(has("tools"));
        row.supports_reasoning = Some(has("thinking"));
    }
    row.context_window = show.model_info.as_ref().and_then(|info| {
        // `general.architecture` names the prefix the limit sits under. A
        // remote model may name its family instead, so any one
        // `*.context_length` key is the fallback.
        let named = info
            .get("general.architecture")
            .and_then(Value::as_str)
            .and_then(|architecture| info.get(&format!("{architecture}.context_length")));
        let window = named.or_else(|| {
            info.iter()
                .find(|(key, _)| key.ends_with(".context_length"))
                .map(|(_, value)| value)
        })?;
        tokens(window.as_u64())
    });
    Some(row)
}

#[cfg(test)]
mod tests;
