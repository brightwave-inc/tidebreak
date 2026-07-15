use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

/// Maximum Unicode scalar values accepted in a search query.
pub const MAX_QUERY_CHARS: usize = 400;
/// A provider request may ask for no more than this many normalized results.
pub const MAX_RESULTS: usize = 10;
/// A request may restrict results to no more than this many domains.
pub const MAX_DOMAINS: usize = 20;
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
/// Legacy name for the serialized response output budget.
pub const MAX_OUTPUT_CHARS: usize = MAX_OUTPUT_BYTES;
const MAX_DOMAIN_CHARS: usize = 253;
const MAX_METADATA_ENTRIES: usize = 8;
const MAX_METADATA_KEY_CHARS: usize = 64;
const MAX_METADATA_VALUE_CHARS: usize = 256;

/// A configured web-search backend. The stable string also selects its secret
/// reference; it is intentionally not a model-controlled argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchProviderKind {
    Exa,
    Tavily,
}

impl WebSearchProviderKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exa => "exa",
            Self::Tavily => "tavily",
        }
    }

    /// Stable key in the application [`SecretProvider`](openwave_core::SecretProvider).
    #[must_use]
    pub const fn credential_key(self) -> &'static str {
        match self {
            Self::Exa => "web_search.exa.api_key",
            Self::Tavily => "web_search.tavily.api_key",
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

/// A configured provider. Implementations must reject invalid requests before
/// egress and return [`WebSearchResponse`] rather than provider-native JSON.
#[async_trait]
pub trait WebSearchProvider: Send + Sync {
    fn kind(&self) -> WebSearchProviderKind;

    async fn search(&self, request: WebSearchRequest) -> Result<WebSearchResponse, WebSearchError>;
}

#[derive(Debug, Error)]
pub enum WebSearchError {
    #[error("invalid web search request: {0}")]
    InvalidRequest(String),
    #[error("web search is not configured for {0}")]
    NotConfigured(WebSearchProviderKind),
    #[error("web search transport failed: {0}")]
    Transport(String),
    #[error("web search provider {provider} returned HTTP {status}")]
    HttpStatus {
        provider: WebSearchProviderKind,
        status: u16,
    },
    #[error("web search provider {provider} returned an invalid response")]
    InvalidResponse { provider: WebSearchProviderKind },
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

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
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
