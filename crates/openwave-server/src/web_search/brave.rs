//! Brave Search's `/res/v1/web/search` adapter.
//!
//! Brave is the search-only backend with a free tier, so it is the one a host
//! can turn on without a paid plan. It publishes no page-extraction endpoint;
//! [`WebSearchProvider::supports_extract`] therefore stays false and
//! `web_extract` routes to the native engine, which needs no configuration.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::Deserialize;

use super::types::{
    domain_scoped_query, parse_iso8601_instant, result_within_domains,
    result_within_published_window,
};
use crate::web_search::{
    HttpClient, HttpGetRequest, WebSearchCredential, WebSearchError, WebSearchProvider,
    WebSearchProviderKind, WebSearchRequest, WebSearchResponse, WebSearchResult,
};

/// Largest `count` the web-search endpoint accepts on one call.
const MAX_COUNT: usize = 20;

/// The crate never asks for more results than Brave will return, so `count`
/// can carry `max_results` unclamped.
const _: () = assert!(crate::web_search::MAX_RESULTS <= MAX_COUNT);

/// Longest query Brave accepts in `q`.
///
/// It happens to equal this crate's own query bound, which is why appending
/// `site:` operators can overflow it and why [`domain_scoped_query`] is given
/// the limit rather than assuming the operators always fit.
const MAX_QUERY_CHARS: usize = 400;

const _: () = assert!(crate::web_search::MAX_QUERY_CHARS <= MAX_QUERY_CHARS);

/// Brave adapter backed by an injected HTTP client.
#[derive(Clone, Debug)]
pub struct BraveProvider<C> {
    client: C,
    credential: WebSearchCredential,
}

impl<C> BraveProvider<C> {
    pub fn new(client: C, credential: WebSearchCredential) -> Result<Self, WebSearchError> {
        if credential.kind() != WebSearchProviderKind::Brave {
            return Err(WebSearchError::NotConfigured(WebSearchProviderKind::Brave));
        }
        Ok(Self { client, credential })
    }
}

#[async_trait]
impl<C: HttpClient> WebSearchProvider for BraveProvider<C> {
    fn kind(&self) -> WebSearchProviderKind {
        WebSearchProviderKind::Brave
    }

    async fn search(&self, request: WebSearchRequest) -> Result<WebSearchResponse, WebSearchError> {
        request.validate()?;
        let response = self
            .client
            .get(HttpGetRequest {
                url: self
                    .kind()
                    .search_url()
                    .ok_or(WebSearchError::NotConfigured(self.kind()))?
                    .into(),
                query: search_query(&request),
                headers: vec![
                    (
                        "x-subscription-token".into(),
                        self.credential.api_key().into(),
                    ),
                    ("accept".into(), "application/json".into()),
                ],
            })
            .await?;
        response.ensure_bounded()?;
        if !(200..300).contains(&response.status) {
            return Err(status_error(response.status));
        }
        let payload: BraveSearchResponse =
            serde_json::from_slice(&response.body).map_err(|_| {
                WebSearchError::InvalidResponse {
                    provider: self.kind(),
                }
            })?;
        let results = payload
            .web
            .map(|web| web.results)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|result| normalize_result(result).ok())
            // Brave has no include-domains parameter, so the operators in `q`
            // are a hint and this is the filter. Enforcing it here means a
            // domain-restricted call can never return an off-domain page,
            // whatever the upstream ranker did with the operator.
            .filter(|result| result_within_domains(&result.url, &request.domains))
            // `freshness` only carries a fully closed window, so a half-open
            // request has nothing sent for it; the window is applied here so it
            // is honoured either way.
            .filter(|result| {
                result_within_published_window(
                    result.published_at,
                    request.start_published_at,
                    request.end_published_at,
                )
            })
            .collect();
        Ok(WebSearchResponse::new(self.kind(), results))
    }
}

/// The documented query parameters for one bounded web search.
///
/// `text_decorations` is switched off so `description` arrives as plain text:
/// with the default on, Brave wraps matched terms in `<strong>` markup, and
/// asking the vendor not to send markup beats stripping it afterwards.
/// `result_filter` keeps the response to the web cluster — the news, video,
/// discussion, and infobox clusters are not part of this crate's contract and
/// would only cost payload against the response byte cap.
fn search_query(request: &WebSearchRequest) -> Vec<(String, String)> {
    let mut query = vec![
        (
            "q".into(),
            domain_scoped_query(&request.query, &request.domains, MAX_QUERY_CHARS),
        ),
        ("count".into(), request.max_results.to_string()),
        ("result_filter".into(), "web".into()),
        ("text_decorations".into(), "false".into()),
    ];
    // Brave expresses a custom window as one `freshness` range and requires
    // both ends, so a half-open request cannot be sent. The unsent end is not
    // silently dropped either: the normalized results still carry
    // `published_at`, which is what a caller filters on.
    if let (Some(start), Some(end)) = (request.start_published_at, request.end_published_at) {
        query.push((
            "freshness".into(),
            format!("{}to{}", start.date_naive(), end.date_naive()),
        ));
    }
    query
}

/// Project a request-level status onto the crate's typed errors.
///
/// Brave documents no distinct quota status: a spent monthly allowance and a
/// burst overrun both answer `429`, so [`WebSearchError::QuotaExhausted`] has
/// no code to map from here and inventing one would be a guess about billing.
fn status_error(status: u16) -> WebSearchError {
    let provider = WebSearchProviderKind::Brave;
    match status {
        401 => WebSearchError::CredentialRejected(provider),
        429 => WebSearchError::RateLimited(provider),
        status => WebSearchError::HttpStatus { provider, status },
    }
}

#[derive(Deserialize)]
struct BraveSearchResponse {
    /// Absent when the query produced no web cluster at all, which is a valid
    /// empty answer rather than a malformed response.
    #[serde(default)]
    web: Option<BraveWebResults>,
}

#[derive(Deserialize)]
struct BraveWebResults {
    #[serde(default)]
    results: Vec<BraveResult>,
}

#[derive(Deserialize)]
struct BraveResult {
    url: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    /// ISO 8601 publication timestamp. The sibling `age` field holds a
    /// human-readable relative string ("2 days ago") that carries no instant,
    /// so it is deliberately not read.
    #[serde(default)]
    page_age: Option<String>,
}

/// Brave returns a snippet and no page text, so `content` stays empty and no
/// score is reported: the response carries no relevance number, and a
/// synthesized one would read as vendor data that does not exist.
fn normalize_result(result: BraveResult) -> Result<WebSearchResult, WebSearchError> {
    let published_at = result.page_age.as_deref().and_then(parse_iso8601_instant);
    WebSearchResult::new(
        result.url,
        result.title,
        result.description,
        None,
        None,
        published_at,
        None,
        BTreeMap::new(),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::web_search::{
        HttpRequest, HttpResponse, SearchDomain, WebExtractRequest, WebSearchCredentialState,
        WebSearchCredentials,
    };
    use openwave_core::{AgentError, SecretProvider};

    struct StaticSecrets;

    #[async_trait]
    impl SecretProvider for StaticSecrets {
        async fn get_secret(&self, key: &str) -> openwave_core::Result<Option<String>> {
            Ok((Some(key) == WebSearchProviderKind::Brave.credential_key())
                .then(|| "brave-key".into()))
        }

        async fn set_secret(&self, _key: &str, _value: &str) -> openwave_core::Result<()> {
            Err(AgentError::Secret("read only test secrets".into()))
        }

        async fn delete_secret(&self, _key: &str) -> openwave_core::Result<()> {
            Err(AgentError::Secret("read only test secrets".into()))
        }
    }

    /// The stored key as a usable credential, failing the test by name if any
    /// other credential state comes back.
    async fn test_credential() -> WebSearchCredential {
        match WebSearchCredentials::resolve(&StaticSecrets, WebSearchProviderKind::Brave).await {
            Ok(WebSearchCredentialState::Present(credential)) => credential,
            other => panic!("expected a stored Brave key, got {other:?}"),
        }
    }

    #[derive(Clone)]
    struct FakeHttpClient {
        request: Arc<Mutex<Option<HttpGetRequest>>>,
        response: HttpResponse,
    }

    #[async_trait]
    impl HttpClient for FakeHttpClient {
        async fn post_json(&self, _request: HttpRequest) -> Result<HttpResponse, WebSearchError> {
            unreachable!("the Brave adapter is a GET-only backend")
        }

        async fn get(&self, request: HttpGetRequest) -> Result<HttpResponse, WebSearchError> {
            *self.request.lock().unwrap() = Some(request);
            Ok(self.response.clone())
        }
    }

    async fn provider(status: u16, body: &[u8]) -> BraveProvider<FakeHttpClient> {
        let credential = test_credential().await;
        BraveProvider::new(
            FakeHttpClient {
                request: Arc::new(Mutex::new(None)),
                response: HttpResponse {
                    status,
                    body: body.to_vec(),
                },
            },
            credential,
        )
        .unwrap()
    }

    /// A response shaped like the documented payload, including the sibling
    /// clusters and per-result fields this adapter deliberately ignores.
    const RESPONSE: &[u8] = br#"{
      "type": "search",
      "query": { "original": "openwave", "more_results_available": true },
      "mixed": { "type": "mixed", "main": [{ "type": "web", "index": 0, "all": false }] },
      "web": {
        "type": "search",
        "family_friendly": true,
        "results": [
          {
            "type": "search_result",
            "url": "https://example.com/release",
            "title": "OpenWave release notes",
            "description": "Everything that shipped this month.",
            "age": "3 days ago",
            "page_age": "2026-07-01T09:30:00",
            "language": "en",
            "family_friendly": true,
            "meta_url": { "scheme": "https", "netloc": "example.com", "hostname": "example.com", "path": "/release" },
            "thumbnail": { "src": "https://imgs.example.com/thumb", "original": "https://example.com/hero.png" },
            "profile": { "name": "Example", "long_name": "example.com" }
          },
          {
            "type": "search_result",
            "url": "https://other.invalid/page",
            "title": "Unrelated",
            "description": "Off-domain result.",
            "page_age": "2026-06-01"
          }
        ]
      }
    }"#;

    #[tokio::test]
    async fn maps_the_web_cluster_and_sends_the_documented_query() {
        let provider = provider(200, RESPONSE).await;
        let sent = provider.client.request.clone();
        let response = provider
            .search(WebSearchRequest::new("openwave", 3).unwrap())
            .await
            .unwrap();

        assert_eq!(response.provider, WebSearchProviderKind::Brave);
        assert_eq!(response.results.len(), 2);
        let first = &response.results[0];
        assert_eq!(first.url, "https://example.com/release");
        assert_eq!(first.title, "OpenWave release notes");
        assert_eq!(first.snippet, "Everything that shipped this month.");
        // Brave returns a snippet, not page text, and no relevance number.
        assert_eq!(first.content, None);
        assert_eq!(first.score, None);
        assert_eq!(
            first.published_at.unwrap().to_rfc3339(),
            "2026-07-01T09:30:00+00:00"
        );
        assert!(
            serde_json::to_vec(&response).unwrap().len() <= crate::web_search::MAX_OUTPUT_BYTES
        );

        let sent = sent.lock().unwrap();
        let sent = sent.as_ref().unwrap();
        assert_eq!(
            Some(sent.url.as_str()),
            WebSearchProviderKind::Brave.search_url()
        );
        let pairs: BTreeMap<&str, &str> = sent
            .query
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        assert_eq!(pairs["q"], "openwave");
        assert_eq!(pairs["count"], "3");
        assert_eq!(pairs["result_filter"], "web");
        // Markup in `description` is refused at the source rather than
        // stripped after the fact.
        assert_eq!(pairs["text_decorations"], "false");
        assert!(!pairs.contains_key("freshness"));
        assert_eq!(
            sent.headers[0],
            ("x-subscription-token".into(), "brave-key".into())
        );
        // The key rides in a header, never in the query string, which is the
        // part of a GET that lands in proxy and server logs.
        assert!(sent.query.iter().all(|(_, value)| value != "brave-key"));
    }

    #[tokio::test]
    async fn domain_filters_become_operators_and_a_hard_result_filter() {
        let provider = provider(200, RESPONSE).await;
        let sent = provider.client.request.clone();
        let response = provider
            .search(
                WebSearchRequest::new("openwave", 3)
                    .unwrap()
                    .with_domains(vec![SearchDomain::parse("example.com").unwrap()])
                    .unwrap(),
            )
            .await
            .unwrap();

        // The off-domain result Brave returned anyway does not survive.
        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].url, "https://example.com/release");

        let sent = sent.lock().unwrap();
        let query = sent
            .as_ref()
            .unwrap()
            .query
            .iter()
            .find(|(name, _)| name == "q")
            .unwrap()
            .1
            .clone();
        assert_eq!(query, "openwave site:example.com");
    }

    async fn search_status(status: u16) -> WebSearchError {
        provider(status, b"{}")
            .await
            .search(WebSearchRequest::new("openwave", 1).unwrap())
            .await
            .unwrap_err()
    }

    #[tokio::test]
    async fn maps_distinctive_statuses_to_typed_errors() {
        assert!(matches!(
            search_status(401).await,
            WebSearchError::CredentialRejected(WebSearchProviderKind::Brave)
        ));
        // A spent monthly allowance answers 429 as well, so this one code
        // covers both and no quota status is invented for Brave.
        assert!(matches!(
            search_status(429).await,
            WebSearchError::RateLimited(WebSearchProviderKind::Brave)
        ));
        for status in [422, 503] {
            assert!(
                matches!(
                    search_status(status).await,
                    WebSearchError::HttpStatus { status: seen, .. } if seen == status
                ),
                "HTTP {status} did not stay a plain status"
            );
        }
    }

    #[tokio::test]
    async fn is_search_only_so_extraction_routes_to_the_native_engine() {
        let provider = provider(200, RESPONSE).await;
        assert!(!provider.supports_extract());
        assert!(matches!(
            provider
                .extract(WebExtractRequest::new("https://example.com/article").unwrap())
                .await,
            Err(WebSearchError::ExtractNotSupported(
                WebSearchProviderKind::Brave
            ))
        ));
    }
}
