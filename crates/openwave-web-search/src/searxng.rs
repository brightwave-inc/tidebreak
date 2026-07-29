//! SearXNG's JSON search API adapter, for a self-hosted instance.
//!
//! SearXNG is the backend that needs no vendor account at all: the operator
//! runs the instance. That makes it the one provider that departs from two of
//! this crate's standing rules, and both departures are deliberate and narrow.
//!
//! **It carries no credential.** Every other provider holds an API key in the
//! host's secret store and is unusable until that key is present. SearXNG has
//! nothing to hold, which is a third state rather than an optional key — see
//! [`WebSearchCredentialState`](crate::WebSearchCredentialState). The
//! credentialed providers still fail closed exactly as before.
//!
//! **Its address is configuration.** Every other provider pins a fixed
//! `outbound_domain` so neither host settings nor a model argument can redirect
//! egress. A self-hosted instance has no fixed address to pin, so the base URL
//! is host configuration — validated by [`SearxngBaseUrl`] when it is stored,
//! and turned into exactly one [`OutboundOrigin`](crate::OutboundOrigin) the
//! transport is bound to before any request exists. It is never a model
//! argument and is not derivable from tool input.
//!
//! Instances commonly run on `http://localhost:8888` or a private LAN address,
//! so loopback and private destinations are reachable here. That is a different
//! trust class from the URLs `web_extract` fetches: the operator typed this one
//! into their own settings, whereas `fetch_policy` governs addresses the model
//! or a fetched page chose. `fetch_policy` is not relaxed by any of this.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::Deserialize;
use url::Url;

use crate::types::{
    domain_scoped_query, parse_iso8601_instant, result_within_domains,
    result_within_published_window,
};
use crate::{
    HttpClient, HttpGetRequest, OutboundOrigin, WebSearchError, WebSearchProvider,
    WebSearchProviderKind, WebSearchRequest, WebSearchResponse, WebSearchResult,
};

/// Longest base URL accepted for an instance.
const MAX_BASE_URL_BYTES: usize = 512;

/// Longest query this adapter will build.
///
/// SearXNG imposes no bound of its own — it forwards the query to upstream
/// engines — so this is the crate's own bound plus room for the `site:`
/// operators it may append.
const MAX_QUERY_CHARS: usize = 1_000;

/// A validated base URL for a self-hosted SearXNG instance.
///
/// Parsing happens once, where the value is configured, and the canonical form
/// is what gets stored and dialed. Everything a base URL has no business
/// carrying is rejected rather than normalized away: a non-HTTP scheme, embedded
/// credentials, a query string, or a fragment. The search endpoint is derived
/// from the result, never supplied alongside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearxngBaseUrl(String);

impl SearxngBaseUrl {
    pub fn parse(value: impl AsRef<str>) -> Result<Self, WebSearchError> {
        let value = value.as_ref().trim();
        let invalid = || WebSearchError::InvalidRequest("invalid SearXNG instance URL".into());
        if value.is_empty() || value.len() > MAX_BASE_URL_BYTES {
            return Err(invalid());
        }
        let parsed = Url::parse(value).map_err(|_| invalid())?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.cannot_be_a_base()
            || parsed.host_str().is_none_or(str::is_empty)
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(invalid());
        }
        // An instance may live under a path prefix, but a relative segment
        // means the value did not come from a settings field describing one.
        // The check reads the raw text because `Url::parse` resolves `..` and
        // `.` away, and quietly rewriting what the operator typed is worse
        // than saying no. Query and fragment are already refused above, so
        // everything after the authority is the path.
        let raw_path = value
            .split_once("://")
            .map_or("", |(_, rest)| rest)
            .split_once('/')
            .map_or("", |(_, path)| path);
        if raw_path
            .split('/')
            .any(|segment| matches!(segment, ".." | "."))
        {
            return Err(invalid());
        }
        let path = parsed.path().trim_end_matches('/');
        let origin = OutboundOrigin::parse(parsed.as_str())?;
        Ok(Self(format!("{}{path}", origin.as_str())))
    }

    /// The canonical base URL, with no trailing slash.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The one origin a transport for this instance may dial.
    #[must_use]
    pub fn origin(&self) -> OutboundOrigin {
        // The canonical form was built from a parsed origin, so this cannot
        // fail; falling back to re-parsing keeps the type total rather than
        // introducing a panic on host configuration.
        OutboundOrigin::parse(&self.0).unwrap_or_else(|_| unreachable!("canonical base URL"))
    }

    fn search_url(&self) -> String {
        format!("{}/search", self.0)
    }
}

/// SearXNG adapter backed by an injected HTTP client.
///
/// It takes no credential, because there is none to take.
#[derive(Clone, Debug)]
pub struct SearxngProvider<C> {
    client: C,
    base_url: SearxngBaseUrl,
}

impl<C> SearxngProvider<C> {
    pub fn new(client: C, base_url: SearxngBaseUrl) -> Self {
        Self { client, base_url }
    }
}

#[async_trait]
impl<C: HttpClient> WebSearchProvider for SearxngProvider<C> {
    fn kind(&self) -> WebSearchProviderKind {
        WebSearchProviderKind::Searxng
    }

    async fn search(&self, request: WebSearchRequest) -> Result<WebSearchResponse, WebSearchError> {
        request.validate()?;
        let response = self
            .client
            .get(HttpGetRequest {
                url: self.base_url.search_url(),
                query: search_query(&request),
                headers: vec![("accept".into(), "application/json".into())],
            })
            .await?;
        response.ensure_bounded()?;
        if !(200..300).contains(&response.status) {
            return Err(status_error(response.status));
        }
        let payload: SearxngSearchResponse = serde_json::from_slice(&response.body)
            // An instance without the JSON format enabled does not always
            // refuse: some deployments answer 200 with the HTML results page.
            // Naming the configuration is more actionable than "invalid
            // response", and it is the same repair either way.
            .map_err(|_| WebSearchError::JsonApiUnavailable(self.kind()))?;
        let results = payload
            .results
            .into_iter()
            .filter_map(|result| normalize_result(result).ok())
            // SearXNG passes the query through to upstream engines, so the
            // `site:` operators are only as good as whichever engine served
            // the result. These two filters are what hold the request's
            // contract regardless.
            .filter(|result| result_within_domains(&result.url, &request.domains))
            .filter(|result| {
                result_within_published_window(
                    result.published_at,
                    request.start_published_at,
                    request.end_published_at,
                )
            })
            // The API returns a whole page of results and takes no count, so
            // the requested bound is applied here.
            .take(request.max_results)
            .collect();
        Ok(WebSearchResponse::new(self.kind(), results))
    }
}

/// The documented search parameters.
///
/// `time_range` is not sent: its vocabulary is `day`/`month`/`year`, which
/// cannot express the request's window, and a coarser filter than was asked for
/// would quietly change the question. The window is applied to the results
/// instead.
fn search_query(request: &WebSearchRequest) -> Vec<(String, String)> {
    vec![
        (
            "q".into(),
            domain_scoped_query(&request.query, &request.domains, MAX_QUERY_CHARS),
        ),
        ("format".into(), "json".into()),
        ("pageno".into(), "1".into()),
    ]
}

/// Project a request-level status onto the crate's typed errors.
fn status_error(status: u16) -> WebSearchError {
    let provider = WebSearchProviderKind::Searxng;
    match status {
        // The documented answer to a format that is not enabled in the
        // instance's `settings.yml`. It is off by default in many deployments,
        // so this is the failure a new operator is most likely to hit.
        403 => WebSearchError::JsonApiUnavailable(provider),
        // SearXNG ships a request limiter that answers 429.
        429 => WebSearchError::RateLimited(provider),
        status => WebSearchError::HttpStatus { provider, status },
    }
}

#[derive(Deserialize)]
struct SearxngSearchResponse {
    #[serde(default)]
    results: Vec<SearxngResult>,
}

#[derive(Deserialize)]
struct SearxngResult {
    /// Absent on the non-web result templates an instance may mix into the
    /// list; those are skipped rather than guessed at.
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    score: Option<f64>,
    /// ISO 8601, and usually without a zone offset because the value is
    /// serialized straight from a naive datetime.
    #[serde(default, rename = "publishedDate")]
    published_date: Option<String>,
}

/// `content` is a snippet, not page text, so it becomes the snippet and nothing
/// else. The `engine`/`engines` provenance fields are deliberately not carried:
/// they would spend the output budget on an upstream engine name a model cannot
/// act on.
fn normalize_result(result: SearxngResult) -> Result<WebSearchResult, WebSearchError> {
    let url = result.url.ok_or(WebSearchError::InvalidResultUrl)?;
    let published_at = result
        .published_date
        .as_deref()
        .and_then(parse_iso8601_instant);
    WebSearchResult::new(
        url,
        result.title,
        result.content,
        None,
        result.score,
        published_at,
        None,
        BTreeMap::new(),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::{HttpRequest, HttpResponse, SearchDomain, WebExtractRequest};

    #[derive(Clone)]
    struct FakeHttpClient {
        request: Arc<Mutex<Option<HttpGetRequest>>>,
        response: HttpResponse,
    }

    #[async_trait]
    impl HttpClient for FakeHttpClient {
        async fn post_json(&self, _request: HttpRequest) -> Result<HttpResponse, WebSearchError> {
            unreachable!("the SearXNG adapter is a GET-only backend")
        }

        async fn get(&self, request: HttpGetRequest) -> Result<HttpResponse, WebSearchError> {
            *self.request.lock().unwrap() = Some(request);
            Ok(self.response.clone())
        }
    }

    const BASE_URL: &str = "http://localhost:8888";

    fn provider(status: u16, body: &[u8]) -> SearxngProvider<FakeHttpClient> {
        SearxngProvider::new(
            FakeHttpClient {
                request: Arc::new(Mutex::new(None)),
                response: HttpResponse {
                    status,
                    body: body.to_vec(),
                },
            },
            SearxngBaseUrl::parse(BASE_URL).unwrap(),
        )
    }

    /// A response shaped like the JSON API's own output, including the
    /// sibling arrays and per-result fields this adapter ignores.
    const RESPONSE: &[u8] = br#"{
      "query": "openwave",
      "number_of_results": 0,
      "results": [
        {
          "url": "https://example.com/release",
          "title": "OpenWave release notes",
          "content": "Everything that shipped this month.",
          "engine": "duckduckgo",
          "parsed_url": ["https", "example.com", "/release", "", "", ""],
          "template": "default.html",
          "engines": ["duckduckgo", "brave"],
          "positions": [1, 2],
          "publishedDate": "2026-07-01T09:30:00",
          "score": 3.5,
          "category": "general"
        },
        {
          "url": "https://other.invalid/page",
          "title": "Unrelated",
          "content": "Off-domain result.",
          "engine": "duckduckgo",
          "score": 1.0
        }
      ],
      "answers": [],
      "corrections": [],
      "infoboxes": [],
      "suggestions": [],
      "unresponsive_engines": []
    }"#;

    #[tokio::test]
    async fn maps_the_json_api_and_sends_the_documented_query() {
        let provider = provider(200, RESPONSE);
        let sent = provider.client.request.clone();
        let response = provider
            .search(WebSearchRequest::new("openwave", 3).unwrap())
            .await
            .unwrap();

        assert_eq!(response.provider, WebSearchProviderKind::Searxng);
        assert_eq!(response.results.len(), 2);
        let first = &response.results[0];
        assert_eq!(first.url, "https://example.com/release");
        assert_eq!(first.title, "OpenWave release notes");
        assert_eq!(first.snippet, "Everything that shipped this month.");
        assert_eq!(first.score, Some(3.5));
        // A naive `publishedDate` is read as UTC rather than dropped.
        assert_eq!(
            first.published_at.unwrap().to_rfc3339(),
            "2026-07-01T09:30:00+00:00"
        );
        assert!(serde_json::to_vec(&response).unwrap().len() <= crate::MAX_OUTPUT_BYTES);

        let sent = sent.lock().unwrap();
        let sent = sent.as_ref().unwrap();
        assert_eq!(sent.url, "http://localhost:8888/search");
        let pairs: BTreeMap<&str, &str> = sent
            .query
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        assert_eq!(pairs["q"], "openwave");
        assert_eq!(pairs["format"], "json");
        assert!(sent.headers.iter().all(|(name, _)| name != "authorization"));
    }

    #[tokio::test]
    async fn honours_domain_and_publication_filters_the_api_cannot_express() {
        let provider = provider(200, RESPONSE);
        let sent = provider.client.request.clone();
        let response = provider
            .search(
                WebSearchRequest::new("openwave", 3)
                    .unwrap()
                    .with_domains(vec![SearchDomain::parse("example.com").unwrap()])
                    .unwrap()
                    .with_published_between(
                        Some(parse_iso8601_instant("2026-07-01").unwrap()),
                        None,
                    )
                    .unwrap(),
            )
            .await
            .unwrap();

        // The off-domain result the instance returned anyway does not survive.
        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].url, "https://example.com/release");

        let sent = sent.lock().unwrap();
        let sent = sent.as_ref().unwrap();
        let query = &sent.query.iter().find(|(name, _)| name == "q").unwrap().1;
        assert_eq!(query, "openwave site:example.com");
        // The window has no faithful parameter, so none is sent and the filter
        // is applied to the results instead.
        assert!(sent.query.iter().all(|(name, _)| name != "time_range"));
    }

    async fn search_status(status: u16, body: &[u8]) -> WebSearchError {
        provider(status, body)
            .search(WebSearchRequest::new("openwave", 1).unwrap())
            .await
            .unwrap_err()
    }

    #[tokio::test]
    async fn an_instance_without_the_json_format_is_a_named_configuration_failure() {
        // The documented refusal for a format that is not enabled.
        assert!(matches!(
            search_status(403, b"").await,
            WebSearchError::JsonApiUnavailable(WebSearchProviderKind::Searxng)
        ));
        // And the deployments that answer the HTML page at 200 instead.
        assert!(matches!(
            search_status(200, b"<!DOCTYPE html><html><body>results</body></html>").await,
            WebSearchError::JsonApiUnavailable(WebSearchProviderKind::Searxng)
        ));
        assert!(matches!(
            search_status(429, b"").await,
            WebSearchError::RateLimited(WebSearchProviderKind::Searxng)
        ));
        assert!(matches!(
            search_status(502, b"").await,
            WebSearchError::HttpStatus { status: 502, .. }
        ));
    }

    #[test]
    fn base_url_validation_rejects_everything_that_is_not_an_origin() {
        assert_eq!(
            SearxngBaseUrl::parse("http://localhost:8888/")
                .unwrap()
                .as_str(),
            "http://localhost:8888"
        );
        // A private LAN address and a path prefix are both ordinary
        // self-hosted deployments.
        assert_eq!(
            SearxngBaseUrl::parse("https://Search.Lan/searxng/")
                .unwrap()
                .as_str(),
            "https://search.lan/searxng"
        );

        for invalid in [
            "",
            "not a url",
            "localhost:8888",
            "ftp://localhost:8888",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "http://user:password@localhost:8888",
            "http://localhost:8888/search?q=leak",
            "http://localhost:8888/#fragment",
            "http://localhost:8888/../admin",
            "http://",
        ] {
            assert!(
                SearxngBaseUrl::parse(invalid).is_err(),
                "{invalid} was accepted as an instance URL"
            );
        }
    }

    #[tokio::test]
    async fn is_search_only_so_extraction_routes_to_the_native_engine() {
        let provider = provider(200, RESPONSE);
        assert!(!provider.supports_extract());
        assert!(matches!(
            provider
                .extract(WebExtractRequest::new("https://example.com/article").unwrap())
                .await,
            Err(WebSearchError::ExtractNotSupported(
                WebSearchProviderKind::Searxng
            ))
        ));
    }
}
