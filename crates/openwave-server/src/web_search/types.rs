use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

/// Maximum Unicode scalar values accepted in a search query.
pub const MAX_QUERY_CHARS: usize = openwave_core::MAX_WEB_SEARCH_QUERY_CHARS;
/// A provider request may ask for no more than this many normalized results.
pub const MAX_RESULTS: usize = openwave_core::MAX_WEB_SEARCH_RESULTS;
/// A request may restrict results to no more than this many domains.
pub const MAX_DOMAINS: usize = openwave_core::MAX_WEB_SEARCH_DOMAINS;
/// Maximum title length preserved from a provider response.
pub const MAX_RESULT_TITLE_CHARS: usize = 300;
/// Maximum snippet length preserved from a provider response.
pub const MAX_RESULT_SNIPPET_CHARS: usize = 2_000;
/// Maximum extracted-content length preserved from a provider response.
pub const MAX_RESULT_CONTENT_CHARS: usize = 4_000;
/// Maximum bytes in a canonical result or image URL.
pub const MAX_RESULT_URL_BYTES: usize = 2_048;
/// Maximum UTF-8 bytes in a serialized normalized response.
pub const MAX_OUTPUT_BYTES: usize = 16_000;
/// Maximum UTF-8 bytes in a serialized normalized extraction.
///
/// Wider than the search budget because one extraction carries a whole page's
/// readable content. It comfortably fits the native engine's 24,000-character
/// content budget as single-byte text; a page dense in multi-byte script is
/// trimmed to fit and says so through `truncated`.
pub const MAX_EXTRACT_OUTPUT_BYTES: usize = 64_000;
/// Marker an extraction engine inserts between the head and tail of content it
/// had to shorten from the middle.
///
/// It is public and lives here rather than beside the native engine because it
/// is a boundary in the extracted text, not an implementation detail of one
/// producer: text either side of it was not adjacent on the page, so anything
/// that quotes a run of the content has to know where it is.
pub const EXTRACT_TRUNCATION_MARKER: &str = "\n\n[... content truncated ...]\n\n";
/// Below this many extracted words a page has no readable content.
///
/// The floor is engine-neutral on purpose: a vendor that answers with a cookie
/// banner is exactly as unhelpful as a native fetch that lands on a script
/// shell, and both should resolve "nothing there" rather than a thin success.
pub const MIN_EXTRACT_WORDS: usize = 20;
/// Legacy name for the serialized response output budget.
pub const MAX_OUTPUT_CHARS: usize = MAX_OUTPUT_BYTES;
const MAX_DOMAIN_CHARS: usize = 253;
const MAX_METADATA_ENTRIES: usize = 8;
const MAX_METADATA_KEY_CHARS: usize = 64;
const MAX_METADATA_VALUE_CHARS: usize = 256;

/// A configured web-search backend. The stable string also selects its secret
/// reference; it is intentionally not a model-controlled argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchProviderKind {
    Exa,
    Tavily,
    Brave,
    Searxng,
}

impl WebSearchProviderKind {
    /// Every backend this crate implements.
    ///
    /// Anything that has to enumerate providers reads this rather than
    /// spelling out a list that a new variant would silently fall out of.
    pub const ALL: [Self; 4] = [Self::Exa, Self::Tavily, Self::Brave, Self::Searxng];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exa => "exa",
            Self::Tavily => "tavily",
            Self::Brave => "brave",
            Self::Searxng => "searxng",
        }
    }

    /// Stable key in the application [`SecretProvider`](openwave_core::SecretProvider).
    ///
    /// `None` for a provider that authenticates with nothing at all. That is
    /// not the same as a key being optional: a provider named here is unusable
    /// until its key is stored, which is what
    /// [`WebSearchCredentialState`](crate::web_search::WebSearchCredentialState) keeps
    /// distinguishable.
    #[must_use]
    pub const fn credential_key(self) -> Option<&'static str> {
        match self {
            Self::Exa => Some("web_search.exa.api_key"),
            Self::Tavily => Some("web_search.tavily.api_key"),
            Self::Brave => Some("web_search.brave.api_key"),
            // A self-hosted instance the operator runs. There is no vendor
            // account behind it and so no key.
            Self::Searxng => None,
        }
    }

    /// Fixed search endpoint, or `None` for a provider whose address is host
    /// configuration because it has no single hosted address to pin.
    pub(crate) const fn search_url(self) -> Option<&'static str> {
        match self {
            Self::Exa => Some("https://api.exa.ai/search"),
            Self::Tavily => Some("https://api.tavily.com/search"),
            Self::Brave => Some("https://api.search.brave.com/res/v1/web/search"),
            Self::Searxng => None,
        }
    }

    /// Fixed page-extraction endpoint, on the same authority as
    /// [`Self::search_url`] so one transport binding covers both calls.
    ///
    /// `None` for a search-only backend. Extraction routing reads
    /// [`WebSearchProvider::supports_extract`], so a provider without an
    /// endpoint here simply never receives an extraction request.
    pub(crate) const fn extract_url(self) -> Option<&'static str> {
        match self {
            Self::Exa => Some("https://api.exa.ai/contents"),
            Self::Tavily => Some("https://api.tavily.com/extract"),
            Self::Brave | Self::Searxng => None,
        }
    }

    /// Exact HTTPS domain the host transport may contact for this provider.
    ///
    /// Keeping this mapping beside the fixed credential key and endpoint
    /// prevents host configuration or model arguments from selecting an
    /// outbound target.
    ///
    /// `None` for a self-hosted provider, whose address the operator supplies.
    /// The transport is still bound to exactly one origin — see
    /// [`OutboundOrigin`](crate::web_search::OutboundOrigin) — it is just an origin fixed
    /// at construction from validated host configuration rather than a
    /// constant.
    #[must_use]
    pub const fn outbound_domain(self) -> Option<&'static str> {
        match self {
            Self::Exa => Some("api.exa.ai"),
            Self::Tavily => Some("api.tavily.com"),
            Self::Brave => Some("api.search.brave.com"),
            Self::Searxng => None,
        }
    }
}

impl std::fmt::Display for WebSearchProviderKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// An allow-list domain. It is a host name only, never a URL or wildcard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SearchDomain(String);

impl SearchDomain {
    pub fn parse(value: impl AsRef<str>) -> Result<Self, WebSearchError> {
        let value = value
            .as_ref()
            .trim()
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if value.is_empty() || value.chars().count() > MAX_DOMAIN_CHARS {
            return Err(WebSearchError::InvalidRequest(
                "invalid domain filter".into(),
            ));
        }
        let parsed = Url::parse(&format!("https://{value}"))
            .map_err(|_| WebSearchError::InvalidRequest("invalid domain filter".into()))?;
        if parsed.host_str() != Some(value.as_str())
            || parsed.path() != "/"
            || parsed.port().is_some()
            || value.contains('*')
        {
            return Err(WebSearchError::InvalidRequest(
                "invalid domain filter".into(),
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for SearchDomain {
    type Err = WebSearchError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// Credential-free request passed to a configured web-search provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchRequest {
    pub query: String,
    pub max_results: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<SearchDomain>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_published_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_published_at: Option<DateTime<Utc>>,
}

impl WebSearchRequest {
    pub fn new(query: impl AsRef<str>, max_results: usize) -> Result<Self, WebSearchError> {
        let query = query.as_ref().trim();
        if query.is_empty() || query.chars().count() > MAX_QUERY_CHARS || contains_control(query) {
            return Err(WebSearchError::InvalidRequest("invalid query".into()));
        }
        if !(1..=MAX_RESULTS).contains(&max_results) {
            return Err(WebSearchError::InvalidRequest(format!(
                "max_results must be between 1 and {MAX_RESULTS}"
            )));
        }
        let request = Self {
            query: query.to_owned(),
            max_results,
            domains: Vec::new(),
            start_published_at: None,
            end_published_at: None,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn with_domains(mut self, domains: Vec<SearchDomain>) -> Result<Self, WebSearchError> {
        if domains.len() > MAX_DOMAINS {
            return Err(WebSearchError::InvalidRequest(format!(
                "at most {MAX_DOMAINS} domain filters are allowed"
            )));
        }
        self.domains = domains;
        Ok(self)
    }

    pub fn with_published_between(
        mut self,
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
    ) -> Result<Self, WebSearchError> {
        if start.is_some_and(|start| end.is_some_and(|end| start > end)) {
            return Err(WebSearchError::InvalidRequest(
                "publication start must not be after end".into(),
            ));
        }
        self.start_published_at = start;
        self.end_published_at = end;
        Ok(self)
    }

    /// Revalidate public/deserialized fields at the egress boundary.
    pub fn validate(&self) -> Result<(), WebSearchError> {
        if self.query.trim().is_empty()
            || self.query != self.query.trim()
            || self.query.chars().count() > MAX_QUERY_CHARS
            || contains_control(&self.query)
        {
            return Err(WebSearchError::InvalidRequest("invalid query".into()));
        }
        if !(1..=MAX_RESULTS).contains(&self.max_results) {
            return Err(WebSearchError::InvalidRequest(format!(
                "max_results must be between 1 and {MAX_RESULTS}"
            )));
        }
        if self.domains.len() > MAX_DOMAINS
            || self
                .domains
                .iter()
                .any(|domain| SearchDomain::parse(domain.as_str()).is_err())
        {
            return Err(WebSearchError::InvalidRequest(
                "invalid domain filters".into(),
            ));
        }
        if self
            .start_published_at
            .is_some_and(|start| self.end_published_at.is_some_and(|end| start > end))
        {
            return Err(WebSearchError::InvalidRequest(
                "publication start must not be after end".into(),
            ));
        }
        Ok(())
    }
}

/// One normalized, citation-ready result. Provider-specific raw response data
/// is deliberately omitted so it cannot leak credentials or unbounded blobs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebSearchResult {
    pub url: String,
    pub title: String,
    pub snippet: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

impl WebSearchResult {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        url: impl AsRef<str>,
        title: impl AsRef<str>,
        snippet: impl AsRef<str>,
        content: Option<String>,
        score: Option<f64>,
        published_at: Option<DateTime<Utc>>,
        image_url: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> Result<Self, WebSearchError> {
        let url = canonical_http_url(url.as_ref())?;
        let title = truncate(title.as_ref().trim(), MAX_RESULT_TITLE_CHARS);
        let snippet = truncate(snippet.as_ref().trim(), MAX_RESULT_SNIPPET_CHARS);
        let content = content
            .map(|value| truncate(value.trim(), MAX_RESULT_CONTENT_CHARS))
            .filter(|value| !value.is_empty());
        let image_url = image_url.as_deref().map(canonical_http_url).transpose()?;
        let score = score.filter(|value| value.is_finite());
        let metadata = metadata
            .into_iter()
            .filter(|(key, value)| {
                !key.is_empty()
                    && key.chars().count() <= MAX_METADATA_KEY_CHARS
                    && value.chars().count() <= MAX_METADATA_VALUE_CHARS
            })
            .take(MAX_METADATA_ENTRIES)
            .collect();

        Ok(Self {
            url,
            title,
            snippet,
            content,
            score,
            published_at,
            image_url,
            metadata,
        })
    }

    fn text_len(&self) -> usize {
        self.title.chars().count()
            + self.snippet.chars().count()
            + self
                .content
                .as_deref()
                .map_or(0, |value| value.chars().count())
    }

    fn trim_to_text_budget(&mut self, budget: usize) {
        let title_len = self.title.chars().count();
        if title_len >= budget {
            self.title = truncate(&self.title, budget);
            self.snippet.clear();
            self.content = None;
            return;
        }
        let remaining = budget - title_len;
        let snippet_len = self.snippet.chars().count();
        if snippet_len >= remaining {
            self.snippet = truncate(&self.snippet, remaining);
            self.content = None;
            return;
        }
        if let Some(content) = &self.content {
            self.content = Some(truncate(content, remaining - snippet_len));
        }
    }
}

/// A fully normalized, bounded provider response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebSearchResponse {
    pub provider: WebSearchProviderKind,
    pub results: Vec<WebSearchResult>,
}

impl WebSearchResponse {
    pub fn new(provider: WebSearchProviderKind, mut results: Vec<WebSearchResult>) -> Self {
        results.truncate(MAX_RESULTS);
        let mut seen = std::collections::HashSet::new();
        results.retain(|result| seen.insert(result.url.clone()));
        let mut remaining = MAX_OUTPUT_BYTES;
        for result in &mut results {
            let allowed = remaining.min(result.text_len());
            result.trim_to_text_budget(allowed);
            remaining = remaining.saturating_sub(result.text_len());
        }
        results.retain(|result| !result.title.is_empty() || !result.snippet.is_empty());
        let mut response = Self { provider, results };
        response.enforce_output_budget();
        response
    }

    fn enforce_output_budget(&mut self) {
        while self.serialized_len() > MAX_OUTPUT_BYTES {
            let excess = self.output_excess();
            let Some(last) = self.results.last_mut() else {
                break;
            };
            if let Some(key) = last.metadata.keys().next_back().cloned() {
                last.metadata.remove(&key);
                continue;
            }
            if last.image_url.take().is_some() {
                continue;
            }
            if trim_string_field(&mut last.content, excess) {
                continue;
            }
            if trim_required_field(&mut last.snippet, excess) {
                continue;
            }
            if trim_required_field(&mut last.title, excess) {
                continue;
            }
            self.results.pop();
        }
    }

    fn serialized_len(&self) -> usize {
        serde_json::to_vec(self).map_or(usize::MAX, |value| value.len())
    }

    fn output_excess(&self) -> usize {
        self.serialized_len()
            .saturating_sub(MAX_OUTPUT_BYTES)
            .max(1)
    }
}

/// Credential-free request to extract one public page. One URL per call is
/// the whole v1 shape; every fetch policy knob is host-owned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebExtractRequest {
    url: String,
}

// The schema bound core advertises to models must equal the byte bound the
// fetch admission policy enforces, or the two drift silently.
const _: () =
    assert!(openwave_core::MAX_WEB_EXTRACT_URL_BYTES == super::fetch_policy::MAX_FETCH_URL_BYTES);

impl WebExtractRequest {
    /// Admit a URL for extraction, or say precisely why not.
    ///
    /// This runs the full fetch admission policy — https-only, no userinfo,
    /// default port, denied-network IP literals — before any provider or
    /// transport can see the value, and keeps the canonical fragment-stripped
    /// form. The reason text is closed policy prose, safe for a model.
    pub fn new(url: impl AsRef<str>) -> Result<Self, WebSearchError> {
        let admitted = super::fetch_policy::admit_fetch_url(url.as_ref().trim())
            .map_err(|violation| WebSearchError::InvalidRequest(violation.to_string()))?;
        Ok(Self {
            url: admitted.into(),
        })
    }

    /// Revalidate a deserialized request at the egress boundary.
    pub fn validate(&self) -> Result<(), WebSearchError> {
        let admitted = Self::new(&self.url)?;
        if admitted.url != self.url {
            return Err(WebSearchError::InvalidRequest(
                "page URL is not in canonical admitted form".into(),
            ));
        }
        Ok(())
    }

    /// The admitted, canonical page URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
}

/// How an extraction was produced.
///
/// Every successful extraction is stamped with its method so degraded
/// extraction (a vendor falling back to the native engine) stays visible
/// downstream and can flow into citations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractionMethod {
    /// The host's own admission-checked fetch and readability engine.
    Native,
    /// A configured provider's extraction endpoint.
    Provider(WebSearchProviderKind),
}

impl ExtractionMethod {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Provider(kind) => kind.as_str(),
        }
    }
}

impl std::fmt::Display for ExtractionMethod {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ExtractionMethod {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ExtractionMethod {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value == "native" {
            return Ok(Self::Native);
        }
        WebSearchProviderKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == value)
            .map(Self::Provider)
            .ok_or_else(|| serde::de::Error::custom("unknown extraction method"))
    }
}

/// One fully normalized, bounded extraction. Provider-native payloads are
/// deliberately unrepresentable, exactly as for [`WebSearchResponse`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebExtractResponse {
    /// Which engine produced this extraction.
    pub extraction_method: ExtractionMethod,
    /// Final canonical page URL after any redirects.
    pub url: String,
    /// Page title; empty when the page did not provide one.
    pub title: String,
    /// Readable page content as markdown or plain text.
    pub content: String,
    /// Words in the full extraction, counted before any truncation.
    pub word_count: usize,
    /// Whether `content` was shortened to fit an output budget.
    pub truncated: bool,
}

impl WebExtractResponse {
    /// Normalize and bound one extraction before it can reach a model context.
    pub fn new(
        extraction_method: ExtractionMethod,
        url: impl AsRef<str>,
        title: impl AsRef<str>,
        content: impl Into<String>,
        word_count: usize,
        truncated: bool,
    ) -> Result<Self, WebSearchError> {
        let url = canonical_http_url(url.as_ref())?;
        // Sanitized here rather than in one engine, because every engine's
        // output is a stranger's page: a vendor's rendered extraction can carry
        // a bidirectional override or a zero-width mark exactly as a local parse
        // can, and this is the one constructor both routes pass through before
        // the text reaches a model, a durable source, or a reader.
        let mut response = Self {
            extraction_method,
            url,
            title: truncate(&sanitized_title(title.as_ref()), MAX_RESULT_TITLE_CHARS),
            content: sanitized_content(&content.into()),
            word_count,
            truncated,
        };
        response.enforce_output_budget();
        Ok(response)
    }

    /// The same trim ladder idea as the search response: shed the largest
    /// field first and never return an over-budget serialization. The URL is
    /// already bounded, so content then title always suffices.
    fn enforce_output_budget(&mut self) {
        while self.serialized_len() > MAX_EXTRACT_OUTPUT_BYTES {
            let excess = self
                .serialized_len()
                .saturating_sub(MAX_EXTRACT_OUTPUT_BYTES)
                .max(1);
            if !self.content.is_empty() {
                self.truncated = true;
                trim_required_field(&mut self.content, excess);
                continue;
            }
            if trim_required_field(&mut self.title, excess) {
                continue;
            }
            break;
        }
    }

    fn serialized_len(&self) -> usize {
        serde_json::to_vec(self).map_or(usize::MAX, |value| value.len())
    }
}

/// Why one page could not be extracted, in a closed vocabulary a model can
/// act on.
///
/// Every variant renders fixed prose (plus at most an HTTP status number), so
/// no transport diagnostic, vendor payload, or host configuration detail can
/// ride along.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WebExtractFailure {
    #[error("the URL is not allowed by the page fetch policy")]
    UrlNotAllowed,
    #[error("the page could not be reached")]
    PageUnreachable,
    #[error("the page returned HTTP {0}")]
    HttpStatus(u16),
    #[error("the page redirected too many times or to an invalid location")]
    RedirectNotFollowed,
    #[error("the page response exceeded the size limit")]
    PageTooLarge,
    #[error("the page markup was too large or too deeply nested to extract")]
    PageTooComplex,
    #[error("the page took longer than the extraction budget allows")]
    ExtractionTimedOut,
    #[error("the page is not a readable text or HTML document")]
    UnsupportedContentType,
    #[error("no readable content could be extracted from the page")]
    NoReadableContent,
}

/// A configured provider. Implementations must reject invalid requests before
/// egress and return [`WebSearchResponse`] rather than provider-native JSON.
#[async_trait]
pub trait WebSearchProvider: Send + Sync {
    fn kind(&self) -> WebSearchProviderKind;

    /// Whether this provider implements the search contract.
    fn supports_search(&self) -> bool {
        true
    }

    /// Whether this provider implements the extraction contract.
    ///
    /// Extraction routing is derived from this capability alone — no
    /// heuristics and no escalation. An extract-capable provider receives
    /// extraction requests; a search-only provider never does.
    fn supports_extract(&self) -> bool {
        false
    }

    async fn search(&self, request: WebSearchRequest) -> Result<WebSearchResponse, WebSearchError>;

    /// Extract one admitted page through the provider.
    ///
    /// The default refuses: a provider that does not opt in through
    /// [`Self::supports_extract`] has no extraction endpoint to call, and a
    /// silent fallback here would hide a routing bug.
    async fn extract(
        &self,
        request: WebExtractRequest,
    ) -> Result<WebExtractResponse, WebSearchError> {
        let _ = request;
        Err(WebSearchError::ExtractNotSupported(self.kind()))
    }
}

#[derive(Debug, Error)]
pub enum WebSearchError {
    #[error("invalid web search request: {0}")]
    InvalidRequest(String),
    #[error("web search is not configured for {0}")]
    NotConfigured(WebSearchProviderKind),
    #[error("web extraction is not supported by {0}")]
    ExtractNotSupported(WebSearchProviderKind),
    #[error("web search transport failed: {0}")]
    Transport(String),
    #[error("web search outbound request is not allowed")]
    OutboundNotAllowed,
    #[error("web search provider {provider} returned HTTP {status}")]
    HttpStatus {
        provider: WebSearchProviderKind,
        status: u16,
    },
    /// The provider refused the configured key. This is host configuration
    /// rather than a property of the request, so it is worth distinguishing
    /// from every other provider failure: it will recur on the next call.
    #[error("web search provider {0} rejected the configured API key")]
    CredentialRejected(WebSearchProviderKind),
    /// The account's plan or prepaid balance is spent. Distinct from
    /// [`Self::RateLimited`] because waiting does not clear it.
    #[error("web search provider {0} has no remaining quota")]
    QuotaExhausted(WebSearchProviderKind),
    #[error("web search provider {0} rate limited the request")]
    RateLimited(WebSearchProviderKind),
    /// The provider accepted the request and reported that this one page could
    /// not be extracted. Both vendors answer HTTP 200 in that case, so this is
    /// what stops a per-URL failure becoming an empty-content success.
    #[error("web search provider {provider} could not extract the page")]
    PageNotExtracted { provider: WebSearchProviderKind },
    #[error("web search provider {provider} returned an invalid response")]
    InvalidResponse { provider: WebSearchProviderKind },
    /// A self-hosted instance answered, but not with its JSON API. The JSON
    /// output format is off by default in many deployments, which makes this a
    /// configuration problem on the instance rather than a bad response — and
    /// far more actionable to say so.
    #[error("web search provider {0} did not return its JSON API")]
    JsonApiUnavailable(WebSearchProviderKind),
    #[error("web search provider returned an invalid result URL")]
    InvalidResultUrl,
}

fn canonical_http_url(value: &str) -> Result<String, WebSearchError> {
    let mut parsed = Url::parse(value).map_err(|_| WebSearchError::InvalidResultUrl)?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(WebSearchError::InvalidResultUrl);
    }
    parsed.set_fragment(None);
    let canonical: String = parsed.into();
    if canonical.len() > MAX_RESULT_URL_BYTES {
        return Err(WebSearchError::InvalidResultUrl);
    }
    Ok(canonical)
}

pub(crate) fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

/// A character that can misrepresent what a reader is looking at: a control
/// code, a zero-width mark, or a bidirectional override.
pub(crate) fn is_display_hazard(value: char) -> bool {
    (value.is_control() && value != '\n' && value != '\t')
        || matches!(value,
            '\u{200b}'..='\u{200f}'   // zero-width and directional marks
            | '\u{202a}'..='\u{202e}' // bidirectional embedding and override
            | '\u{2066}'..='\u{2069}' // bidirectional isolates
            | '\u{feff}') // zero-width no-break space
}

/// Strip display hazards from extracted content.
///
/// The sibling search-result path rejects a whole result that carries a
/// control character. Extraction sanitizes instead: it holds one page that
/// cost a fetch and a parse, and a single stray control character somewhere in
/// a long article is not a reason to return nothing. The characters that could
/// mislead a reader are removed; the article survives.
pub(crate) fn sanitized_content(value: &str) -> String {
    value
        .chars()
        .filter(|value| !is_display_hazard(*value))
        .collect()
}

/// Reduce an extracted title to one clean line.
///
/// A title is a single-line field, so every hazard becomes a space and runs of
/// whitespace collapse — a title cannot smuggle line breaks into a list.
pub(crate) fn sanitized_title(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|value| if is_display_hazard(value) { ' ' } else { value })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Words carrying at least one alphanumeric character.
///
/// Punctuation-only tokens are not words, so a page of navigation glyphs
/// cannot clear [`MIN_EXTRACT_WORDS`] on separators alone.
pub(crate) fn count_words(value: &str) -> usize {
    value
        .split_whitespace()
        .filter(|token| token.chars().any(char::is_alphanumeric))
        .count()
}

/// Fold domain filters into the query string as `site:` operators.
///
/// Some backends have no domain-filter parameter and instead pass search
/// operators through in the query. The operators are best effort: whether an
/// upstream engine honours `site:` is not something this crate can promise, so
/// [`result_within_domains`] is what actually holds the contract. That also
/// makes it safe to leave the query alone when the operators would not fit the
/// engine's query bound — the filter is still enforced, just later.
pub(crate) fn domain_scoped_query(
    query: &str,
    domains: &[SearchDomain],
    max_chars: usize,
) -> String {
    if domains.is_empty() {
        return query.to_owned();
    }
    let operators = domains
        .iter()
        .map(|domain| format!("site:{}", domain.as_str()))
        .collect::<Vec<_>>()
        .join(" OR ");
    let scoped = if domains.len() == 1 {
        format!("{query} {operators}")
    } else {
        format!("{query} ({operators})")
    };
    if scoped.chars().count() > max_chars {
        return query.to_owned();
    }
    scoped
}

/// Whether one result URL falls inside the requested domain filters.
///
/// This is what makes a domain filter real for a backend that only understands
/// operators: a result the engine returned anyway is dropped here. A filter
/// matches the host itself or any subdomain of it, which is the meaning the
/// backends that filter natively already give it.
pub(crate) fn result_within_domains(url: &str, domains: &[SearchDomain]) -> bool {
    if domains.is_empty() {
        return true;
    }
    let Some(host) = Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
    else {
        return false;
    };
    domains.iter().any(|domain| {
        let domain = domain.as_str();
        host == domain || host.ends_with(&format!(".{domain}"))
    })
}

/// Whether one result falls inside the requested publication window.
///
/// Backends that cannot express the window natively still have to honour it.
/// A result carrying a date outside the window is dropped; an undated result is
/// kept, because most engines report no date at all and dropping those would
/// empty an answer rather than narrow it.
pub(crate) fn result_within_published_window(
    published_at: Option<DateTime<Utc>>,
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
) -> bool {
    let Some(published_at) = published_at else {
        return true;
    };
    start.is_none_or(|start| published_at >= start) && end.is_none_or(|end| published_at <= end)
}

/// Parse an ISO 8601 instant, with or without a zone offset.
///
/// Several backends report a naive local-looking timestamp
/// (`2026-07-01T09:30:00`) or a bare date. Both are read as UTC, which is what
/// they mean in practice and what keeps a missing offset from becoming a
/// dropped date.
pub(crate) fn parse_iso8601_instant(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|value| value.and_utc())
        })
        .or_else(|| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()
                .and_then(|date| date.and_hms_opt(0, 0, 0))
                .map(|value| value.and_utc())
        })
}

/// Whether a URL a provider echoed back names the page that was requested.
///
/// Both extract endpoints answer HTTP 200 on a partial failure and report the
/// per-URL outcome in a side array, so a response must be matched by URL key
/// and never by array position. Providers may normalize what they echo — host
/// case, an added root path — so parsed forms are compared, with exact bytes as
/// the fallback for an identifier that is not a URL at all.
pub(crate) fn same_page_url(candidate: &str, requested: &str) -> bool {
    if candidate == requested {
        return true;
    }
    match (Url::parse(candidate), Url::parse(requested)) {
        (Ok(mut candidate), Ok(mut requested)) => {
            candidate.set_fragment(None);
            requested.set_fragment(None);
            candidate == requested
        }
        _ => false,
    }
}

fn contains_control(value: &str) -> bool {
    value.chars().any(char::is_control)
}

fn trim_string_field(value: &mut Option<String>, excess: usize) -> bool {
    let Some(current) = value else {
        return false;
    };
    let trimmed = truncate_utf8_bytes(current, current.len().saturating_sub(excess));
    if trimmed.is_empty() {
        *value = None;
    } else {
        *current = trimmed;
    }
    true
}

fn trim_required_field(value: &mut String, excess: usize) -> bool {
    if value.is_empty() {
        return false;
    }
    *value = truncate_utf8_bytes(value, value.len().saturating_sub(excess));
    true
}

fn truncate_utf8_bytes(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_rejects_empty_controls_and_large_values() {
        assert!(WebSearchRequest::new("", 1).is_err());
        assert!(WebSearchRequest::new("one\ntwo", 1).is_err());
        assert!(WebSearchRequest::new("x".repeat(MAX_QUERY_CHARS + 1), 1).is_err());
        assert!(WebSearchRequest::new("ok", 0).is_err());
        assert!(WebSearchRequest::new("ok", MAX_RESULTS + 1).is_err());
    }

    #[test]
    fn domain_filters_are_hosts_not_urls_or_wildcards() {
        assert_eq!(
            SearchDomain::parse("Docs.Example.com.").unwrap().as_str(),
            "docs.example.com"
        );
        assert!(SearchDomain::parse("https://example.com").is_err());
        assert!(SearchDomain::parse("*.example.com").is_err());
        assert!(SearchDomain::parse("example.com/path").is_err());
    }

    #[test]
    fn response_deduplicates_and_trims_its_total_output() {
        let result = |url: &str| {
            WebSearchResult::new(
                url,
                "x".repeat(MAX_RESULT_TITLE_CHARS),
                "x".repeat(MAX_RESULT_SNIPPET_CHARS),
                Some("x".repeat(MAX_RESULT_CONTENT_CHARS)),
                Some(f64::NAN),
                None,
                None,
                BTreeMap::new(),
            )
            .unwrap()
        };
        let response = WebSearchResponse::new(
            WebSearchProviderKind::Exa,
            (0..MAX_RESULTS)
                .map(|index| result(&format!("https://example.com/{index}#fragment")))
                .chain(std::iter::once(result("https://example.com/0")))
                .collect(),
        );
        assert!(response.results.len() <= MAX_RESULTS);
        assert!(!response.results.is_empty());
        assert!(response.results.iter().all(|result| result.score.is_none()));
        assert!(
            response
                .results
                .iter()
                .map(WebSearchResult::text_len)
                .sum::<usize>()
                <= MAX_OUTPUT_BYTES
        );
        assert!(serde_json::to_vec(&response).unwrap().len() <= MAX_OUTPUT_BYTES);
        assert_eq!(response.results[0].url, "https://example.com/0");
    }

    #[test]
    fn long_valid_urls_are_rejected_and_all_serialized_fields_fit_the_budget() {
        let long_url = format!("https://example.com/{}", "a".repeat(MAX_RESULT_URL_BYTES));
        assert!(WebSearchResult::new(
            long_url,
            "title",
            "snippet",
            None,
            None,
            None,
            None,
            BTreeMap::new(),
        )
        .is_err());

        let response = WebSearchResponse::new(
            WebSearchProviderKind::Tavily,
            vec![WebSearchResult::new(
                "https://example.com/short",
                "\"".repeat(MAX_RESULT_TITLE_CHARS),
                "\"".repeat(MAX_RESULT_SNIPPET_CHARS),
                Some("\"".repeat(MAX_RESULT_CONTENT_CHARS)),
                None,
                None,
                Some("https://example.com/image".into()),
                (0..MAX_METADATA_ENTRIES)
                    .map(|index| {
                        (
                            format!("key-{index}"),
                            "\"".repeat(MAX_METADATA_VALUE_CHARS),
                        )
                    })
                    .collect(),
            )
            .unwrap()],
        );
        assert!(serde_json::to_vec(&response).unwrap().len() <= MAX_OUTPUT_BYTES);
    }
}
