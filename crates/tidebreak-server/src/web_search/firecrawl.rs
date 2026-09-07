//! Firecrawl's v2 `/search` adapter.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::types::result_within_domains;
use crate::web_search::{
    HttpClient, HttpRequest, WebSearchCredential, WebSearchError, WebSearchProvider,
    WebSearchProviderKind, WebSearchRequest, WebSearchResponse, WebSearchResult,
};

/// Largest result count accepted by Firecrawl's v2 search endpoint.
const MAX_LIMIT: usize = 100;

const _: () = assert!(crate::web_search::MAX_RESULTS <= MAX_LIMIT);

/// Firecrawl adapter backed by an injected HTTP client.
#[derive(Clone, Debug)]
pub struct FirecrawlProvider<C> {
    client: C,
    credential: WebSearchCredential,
}

impl<C> FirecrawlProvider<C> {
    pub fn new(client: C, credential: WebSearchCredential) -> Result<Self, WebSearchError> {
        if credential.kind() != WebSearchProviderKind::Firecrawl {
            return Err(WebSearchError::NotConfigured(
                WebSearchProviderKind::Firecrawl,
            ));
        }
        Ok(Self { client, credential })
    }
}

#[async_trait]
impl<C: HttpClient> WebSearchProvider for FirecrawlProvider<C> {
    fn kind(&self) -> WebSearchProviderKind {
        WebSearchProviderKind::Firecrawl
    }

    async fn search(&self, request: WebSearchRequest) -> Result<WebSearchResponse, WebSearchError> {
        request.validate()?;
        let response = self
            .client
            .post_json(HttpRequest {
                url: self
                    .kind()
                    .search_url()
                    .ok_or(WebSearchError::NotConfigured(self.kind()))?
                    .into(),
                headers: vec![
                    (
                        "authorization".into(),
                        format!("Bearer {}", self.credential.api_key()),
                    ),
                    ("content-type".into(), "application/json".into()),
                ],
                body: request_body(&request),
            })
            .await?;
        response.ensure_bounded()?;
        if !(200..300).contains(&response.status) {
            return Err(status_error(response.status));
        }
        let payload: FirecrawlSearchResponse =
            serde_json::from_slice(&response.body).map_err(|_| {
                WebSearchError::InvalidResponse {
                    provider: self.kind(),
                }
            })?;
        if !payload.success {
            return Err(WebSearchError::InvalidResponse {
                provider: self.kind(),
            });
        }
        let results = payload
            .data
            .web
            .into_iter()
            .filter_map(|result| normalize_result(result).ok())
            // Keep the domain filter as a hard boundary even if the upstream
            // ranker returns a page outside the requested set.
            .filter(|result| result_within_domains(&result.url, &request.domains))
            .collect();
        Ok(WebSearchResponse::new(self.kind(), results))
    }
}

/// Build the documented v2 search request without asking Firecrawl to scrape
/// result pages. Page opening stays on Tidebreak's bounded extraction path.
fn request_body(request: &WebSearchRequest) -> serde_json::Value {
    let mut body = json!({
        "query": request.query,
        "limit": request.max_results,
        "sources": ["web"],
    });
    if !request.domains.is_empty() {
        body["includeDomains"] = json!(request
            .domains
            .iter()
            .map(|domain| domain.as_str())
            .collect::<Vec<_>>());
    }
    // Firecrawl documents custom date ranges through Google's `tbs` syntax.
    // A half-open range has no documented representation, so it stays absent.
    if let (Some(start), Some(end)) = (request.start_published_at, request.end_published_at) {
        body["tbs"] = json!(format!(
            "cdr:1,cd_min:{},cd_max:{}",
            start.format("%m/%d/%Y"),
            end.format("%m/%d/%Y")
        ));
    }
    body
}

fn status_error(status: u16) -> WebSearchError {
    let provider = WebSearchProviderKind::Firecrawl;
    match status {
        401 => WebSearchError::CredentialRejected(provider),
        402 => WebSearchError::QuotaExhausted(provider),
        429 => WebSearchError::RateLimited(provider),
        status => WebSearchError::HttpStatus { provider, status },
    }
}

#[derive(Deserialize)]
struct FirecrawlSearchResponse {
    success: bool,
    data: FirecrawlSearchData,
}

#[derive(Deserialize)]
struct FirecrawlSearchData {
    #[serde(default)]
    web: Vec<FirecrawlResult>,
}

#[derive(Deserialize)]
struct FirecrawlResult {
    url: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
}

fn normalize_result(result: FirecrawlResult) -> Result<WebSearchResult, WebSearchError> {
    WebSearchResult::new(
        result.url,
        result.title,
        result.description,
        None,
        None,
        None,
        None,
        BTreeMap::new(),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use chrono::{TimeZone, Utc};
    use tidebreak_core::{AgentError, SecretProvider};

    use super::*;
    use crate::web_search::{
        HttpGetRequest, HttpResponse, SearchDomain, WebExtractRequest, WebSearchCredentialState,
        WebSearchCredentials,
    };

    struct StaticSecrets;

    #[async_trait]
    impl SecretProvider for StaticSecrets {
        async fn get_secret(&self, key: &str) -> tidebreak_core::Result<Option<String>> {
            Ok(
                (Some(key) == WebSearchProviderKind::Firecrawl.credential_key())
                    .then(|| "firecrawl-key".into()),
            )
        }

        async fn set_secret(&self, _key: &str, _value: &str) -> tidebreak_core::Result<()> {
            Err(AgentError::Secret("read only test secrets".into()))
        }

        async fn delete_secret(&self, _key: &str) -> tidebreak_core::Result<()> {
            Err(AgentError::Secret("read only test secrets".into()))
        }
    }

    async fn test_credential() -> WebSearchCredential {
        match WebSearchCredentials::resolve(&StaticSecrets, WebSearchProviderKind::Firecrawl).await
        {
            Ok(WebSearchCredentialState::Present(credential)) => credential,
            other => panic!("expected a stored Firecrawl key, got {other:?}"),
        }
    }

    #[derive(Clone)]
    struct FakeHttpClient {
        request: Arc<Mutex<Option<HttpRequest>>>,
        response: HttpResponse,
    }

    #[async_trait]
    impl HttpClient for FakeHttpClient {
        async fn post_json(&self, request: HttpRequest) -> Result<HttpResponse, WebSearchError> {
            *self.request.lock().unwrap() = Some(request);
            Ok(self.response.clone())
        }

        async fn get(&self, _request: HttpGetRequest) -> Result<HttpResponse, WebSearchError> {
            unreachable!("the Firecrawl adapter posts JSON")
        }
    }

    async fn provider(status: u16, body: serde_json::Value) -> FirecrawlProvider<FakeHttpClient> {
        FirecrawlProvider::new(
            FakeHttpClient {
                request: Arc::new(Mutex::new(None)),
                response: HttpResponse {
                    status,
                    body: serde_json::to_vec(&body).unwrap(),
                },
            },
            test_credential().await,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn maps_web_results_and_sends_bounded_filters_with_bearer_auth() {
        let provider = provider(
            200,
            json!({
                "success": true,
                "data": {
                    "web": [
                        {
                            "url": "https://docs.example.com/release",
                            "title": "Tidebreak release notes",
                            "description": "Everything that shipped.",
                            "category": "github"
                        },
                        {
                            "url": "https://other.example/page",
                            "title": "Other",
                            "description": "Off-domain result."
                        }
                    ]
                }
            }),
        )
        .await;
        let sent = provider.client.request.clone();
        let response = provider
            .search(
                WebSearchRequest::new("tidebreak", 3)
                    .unwrap()
                    .with_domains(vec![SearchDomain::parse("docs.example.com").unwrap()])
                    .unwrap()
                    .with_published_between(
                        Some(Utc.with_ymd_and_hms(2026, 7, 1, 12, 0, 0).unwrap()),
                        Some(Utc.with_ymd_and_hms(2026, 7, 31, 12, 0, 0).unwrap()),
                    )
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.provider, WebSearchProviderKind::Firecrawl);
        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].url, "https://docs.example.com/release");
        assert_eq!(response.results[0].snippet, "Everything that shipped.");
        assert_eq!(response.results[0].content, None);

        let sent = sent.lock().unwrap();
        let sent = sent.as_ref().unwrap();
        assert_eq!(
            Some(sent.url.as_str()),
            WebSearchProviderKind::Firecrawl.search_url()
        );
        assert_eq!(sent.body["query"], "tidebreak");
        assert_eq!(sent.body["limit"], 3);
        assert_eq!(sent.body["sources"], json!(["web"]));
        assert_eq!(sent.body["includeDomains"], json!(["docs.example.com"]));
        assert_eq!(
            sent.body["tbs"],
            "cdr:1,cd_min:07/01/2026,cd_max:07/31/2026"
        );
        assert_eq!(
            sent.headers[0],
            ("authorization".into(), "Bearer firecrawl-key".into())
        );
        assert!(sent.body.get("scrapeOptions").is_none());
    }

    async fn search_status(status: u16) -> WebSearchError {
        provider(status, json!({}))
            .await
            .search(WebSearchRequest::new("tidebreak", 1).unwrap())
            .await
            .unwrap_err()
    }

    #[tokio::test]
    async fn maps_credential_quota_and_rate_statuses() {
        assert!(matches!(
            search_status(401).await,
            WebSearchError::CredentialRejected(WebSearchProviderKind::Firecrawl)
        ));
        assert!(matches!(
            search_status(402).await,
            WebSearchError::QuotaExhausted(WebSearchProviderKind::Firecrawl)
        ));
        assert!(matches!(
            search_status(429).await,
            WebSearchError::RateLimited(WebSearchProviderKind::Firecrawl)
        ));
        assert!(matches!(
            search_status(503).await,
            WebSearchError::HttpStatus { status: 503, .. }
        ));
    }

    #[tokio::test]
    async fn rejects_false_success_and_uses_native_page_extraction() {
        let provider = provider(200, json!({ "success": false, "data": { "web": [] } })).await;
        assert!(matches!(
            provider
                .search(WebSearchRequest::new("tidebreak", 1).unwrap())
                .await,
            Err(WebSearchError::InvalidResponse {
                provider: WebSearchProviderKind::Firecrawl
            })
        ));
        assert!(!provider.supports_extract());
        assert!(matches!(
            provider
                .extract(WebExtractRequest::new("https://example.com/article").unwrap())
                .await,
            Err(WebSearchError::ExtractNotSupported(
                WebSearchProviderKind::Firecrawl
            ))
        ));
    }
}
