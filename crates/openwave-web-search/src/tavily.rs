//! Tavily's direct `/search` and `/extract` adapter.

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::json;

use crate::types::{count_words, same_page_url};
use crate::{
    admit_fetch_url, ExtractionMethod, HttpClient, HttpRequest, WebExtractRequest,
    WebExtractResponse, WebSearchCredential, WebSearchError, WebSearchProvider,
    WebSearchProviderKind, WebSearchRequest, WebSearchResponse, WebSearchResult, MIN_EXTRACT_WORDS,
};

/// Tavily adapter backed by an injected HTTP client.
#[derive(Clone, Debug)]
pub struct TavilyProvider<C> {
    client: C,
    credential: WebSearchCredential,
}

impl<C> TavilyProvider<C> {
    pub fn new(client: C, credential: WebSearchCredential) -> Result<Self, WebSearchError> {
        if credential.kind() != WebSearchProviderKind::Tavily {
            return Err(WebSearchError::NotConfigured(WebSearchProviderKind::Tavily));
        }
        Ok(Self { client, credential })
    }
}

#[async_trait]
impl<C: HttpClient> WebSearchProvider for TavilyProvider<C> {
    fn kind(&self) -> WebSearchProviderKind {
        WebSearchProviderKind::Tavily
    }

    async fn search(&self, request: WebSearchRequest) -> Result<WebSearchResponse, WebSearchError> {
        request.validate()?;
        let response = self
            .client
            .post_json(HttpRequest {
                url: self.kind().search_url().into(),
                headers: vec![("content-type".into(), "application/json".into())],
                body: request_body(&request, self.credential.api_key()),
            })
            .await?;
        response.ensure_bounded()?;
        if !(200..300).contains(&response.status) {
            return Err(WebSearchError::HttpStatus {
                provider: self.kind(),
                status: response.status,
            });
        }
        let payload: TavilySearchResponse =
            serde_json::from_slice(&response.body).map_err(|_| {
                WebSearchError::InvalidResponse {
                    provider: self.kind(),
                }
            })?;
        let results = payload
            .results
            .into_iter()
            .filter_map(|result| normalize_result(result).ok())
            .collect();
        Ok(WebSearchResponse::new(self.kind(), results))
    }

    fn supports_extract(&self) -> bool {
        true
    }

    async fn extract(
        &self,
        request: WebExtractRequest,
    ) -> Result<WebExtractResponse, WebSearchError> {
        request.validate()?;
        let response = self
            .client
            .post_json(HttpRequest {
                url: self
                    .kind()
                    .extract_url()
                    .ok_or(WebSearchError::ExtractNotSupported(self.kind()))?
                    .into(),
                headers: vec![
                    // `/extract` takes the key as a bearer token only; the body
                    // `api_key` field the search path still sends is not part
                    // of this endpoint's contract.
                    (
                        "authorization".into(),
                        format!("Bearer {}", self.credential.api_key()),
                    ),
                    ("content-type".into(), "application/json".into()),
                ],
                body: extract_request_body(request.url()),
            })
            .await?;
        response.ensure_bounded()?;
        if !(200..300).contains(&response.status) {
            return Err(extract_status_error(response.status));
        }
        let payload: TavilyExtractResponse =
            serde_json::from_slice(&response.body).map_err(|_| {
                WebSearchError::InvalidResponse {
                    provider: self.kind(),
                }
            })?;
        normalize_extraction(request.url(), payload)
    }
}

fn request_body(request: &WebSearchRequest, api_key: &str) -> serde_json::Value {
    let mut body = json!({
        "api_key": api_key,
        "query": request.query,
        "max_results": request.max_results,
        "search_depth": "basic",
        "include_raw_content": false,
        "include_images": false,
    });
    if !request.domains.is_empty() {
        body["include_domains"] = json!(request
            .domains
            .iter()
            .map(|domain| domain.as_str())
            .collect::<Vec<_>>());
    }
    if let Some(start) = request.start_published_at {
        body["start_date"] = json!(start.date_naive().to_string());
    }
    if let Some(end) = request.end_published_at {
        body["end_date"] = json!(end.date_naive().to_string());
    }
    body
}

#[derive(Deserialize)]
struct TavilySearchResponse {
    results: Vec<TavilyResult>,
}

#[derive(Deserialize)]
struct TavilyResult {
    url: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    score: Option<f64>,
    #[serde(default, rename = "published_date")]
    published_date: Option<String>,
}

fn normalize_result(result: TavilyResult) -> Result<WebSearchResult, WebSearchError> {
    let published_at = result
        .published_date
        .as_deref()
        .and_then(parse_provider_timestamp);
    WebSearchResult::new(
        result.url,
        result.title,
        result.content.clone(),
        Some(result.content),
        result.score,
        published_at,
        None,
        BTreeMap::new(),
    )
}

/// One URL, whole page, markdown.
///
/// `query` is deliberately absent. Supplying one switches `raw_content` from
/// the page to reranked chunks joined by a literal `"[...]"` marker, which is a
/// different product from the extraction contract this adapter implements.
/// `basic` depth is the whole-page read; `advanced` doubles the cost for table
/// and dynamic-content recovery that the native engine already backstops. No
/// body `timeout` is sent either — the host's clamped transport timeout is the
/// single deadline, and a second one in the payload could only disagree with it.
fn extract_request_body(url: &str) -> serde_json::Value {
    json!({
        "urls": [url],
        "extract_depth": "basic",
        "format": "markdown",
        "include_images": false,
    })
}

/// Project a request-level status onto the crate's typed errors.
fn extract_status_error(status: u16) -> WebSearchError {
    let provider = WebSearchProviderKind::Tavily;
    match status {
        401 => WebSearchError::CredentialRejected(provider),
        // Tavily splits "you are going too fast" from "you are out of credit"
        // across nonstandard statuses: 432 is the plan allowance, 433 the
        // pay-as-you-go balance. Neither is fixed by backing off, so they must
        // not be folded into 429.
        432 | 433 => WebSearchError::QuotaExhausted(provider),
        429 => WebSearchError::RateLimited(provider),
        status => WebSearchError::HttpStatus { provider, status },
    }
}

#[derive(Deserialize)]
struct TavilyExtractResponse {
    #[serde(default)]
    results: Vec<TavilyExtractResult>,
    /// URLs the vendor could not read. Its per-entry `error` prose is
    /// deliberately not deserialized: it would only ever be forwarded, and no
    /// vendor string belongs in a model context.
    #[serde(default)]
    failed_results: Vec<TavilyFailedResult>,
}

#[derive(Deserialize)]
struct TavilyExtractResult {
    #[serde(default)]
    url: String,
    /// Undocumented, but populated in practice. Treated as optional so a
    /// response that stops carrying it degrades to an untitled page rather
    /// than failing to deserialize.
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    raw_content: Option<String>,
}

#[derive(Deserialize)]
struct TavilyFailedResult {
    #[serde(default)]
    url: String,
}

/// Turn one `/extract` payload into a bounded extraction, or a typed failure.
///
/// The endpoint answers HTTP 200 even when every URL failed, splitting the
/// outcome across `results` and `failed_results`, so the requested URL is
/// looked up by key in both. A missing or too-thin page is a failure rather
/// than an extraction with nothing in it.
fn normalize_extraction(
    requested: &str,
    payload: TavilyExtractResponse,
) -> Result<WebExtractResponse, WebSearchError> {
    let provider = WebSearchProviderKind::Tavily;
    let failed = || WebSearchError::PageNotExtracted { provider };
    if payload
        .failed_results
        .iter()
        .any(|entry| same_page_url(&entry.url, requested))
    {
        return Err(failed());
    }
    let result = payload
        .results
        .into_iter()
        .find(|result| same_page_url(&result.url, requested))
        .ok_or_else(failed)?;
    let content = result.raw_content.unwrap_or_default();
    let content = content.trim();
    let word_count = count_words(content);
    if word_count < MIN_EXTRACT_WORDS {
        return Err(failed());
    }
    let url = admit_fetch_url(&result.url).map_or_else(|_| requested.to_owned(), String::from);
    // The published contract lists no title, but responses carry one, so it is
    // read when present and left empty otherwise — which is what the contract
    // already means by "the page did not provide one". Nothing is derived from
    // the content to fill the gap: a heading lifted out of the page would be a
    // guess standing where a fact belongs.
    let title = result.title.unwrap_or_default();
    WebExtractResponse::new(
        ExtractionMethod::Provider(provider),
        url,
        title.trim(),
        content,
        word_count,
        // Tavily applies no length cap of its own, so anything shortened here
        // is shortened by the output budget, which sets this itself.
        false,
    )
}

fn parse_provider_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
        .or_else(|| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()
                .and_then(|date| date.and_hms_opt(0, 0, 0))
                .map(|value| value.and_utc())
        })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::{HttpResponse, WebSearchCredentials};
    use openwave_core::{AgentError, SecretProvider};

    #[derive(Clone)]
    struct FakeHttpClient {
        request: Arc<Mutex<Option<HttpRequest>>>,
        response: HttpResponse,
    }

    struct StaticSecrets;

    #[async_trait]
    impl SecretProvider for StaticSecrets {
        async fn get_secret(&self, key: &str) -> openwave_core::Result<Option<String>> {
            Ok(
                (key == WebSearchProviderKind::Tavily.credential_key())
                    .then(|| "tavily-key".into()),
            )
        }

        async fn set_secret(&self, _key: &str, _value: &str) -> openwave_core::Result<()> {
            Err(AgentError::Secret("read only test secrets".into()))
        }

        async fn delete_secret(&self, _key: &str) -> openwave_core::Result<()> {
            Err(AgentError::Secret("read only test secrets".into()))
        }
    }

    #[async_trait]
    impl HttpClient for FakeHttpClient {
        async fn post_json(&self, request: HttpRequest) -> Result<HttpResponse, WebSearchError> {
            *self.request.lock().unwrap() = Some(request);
            Ok(self.response.clone())
        }

        async fn get(
            &self,
            _request: crate::HttpGetRequest,
        ) -> Result<HttpResponse, WebSearchError> {
            unreachable!("the Tavily adapter posts JSON on both endpoints")
        }
    }

    #[tokio::test]
    async fn maps_tavily_response_and_keeps_credential_out_of_headers() {
        let credential = WebSearchCredentials::load(&StaticSecrets, WebSearchProviderKind::Tavily)
            .await
            .unwrap()
            .unwrap();
        let request = Arc::new(Mutex::new(None));
        let provider = TavilyProvider::new(
            FakeHttpClient {
                request: Arc::clone(&request),
                response: HttpResponse {
                    status: 200,
                    body: br#"{"results":[{"url":"https://example.com/t","title":"Tavily","content":"bounded content","score":0.5,"published_date":"2026-01-01"}]}"#.to_vec(),
                },
            },
            credential,
        )
        .unwrap();
        let response = provider
            .search(WebSearchRequest::new("openwave", 1).unwrap())
            .await
            .unwrap();

        assert_eq!(
            response.results[0].published_at.unwrap().to_rfc3339(),
            "2026-01-01T00:00:00+00:00"
        );
        let sent = request.lock().unwrap();
        let sent = sent.as_ref().unwrap();
        assert_eq!(sent.url, WebSearchProviderKind::Tavily.search_url());
        assert_eq!(sent.body["api_key"], "tavily-key");
        assert!(sent.headers.iter().all(|(_, value)| value != "tavily-key"));
    }

    const EXTRACT_URL: &str = "https://example.com/article";

    async fn extract(
        status: u16,
        body: serde_json::Value,
    ) -> (
        Result<WebExtractResponse, WebSearchError>,
        Option<HttpRequest>,
    ) {
        let credential = WebSearchCredentials::load(&StaticSecrets, WebSearchProviderKind::Tavily)
            .await
            .unwrap()
            .unwrap();
        let request = Arc::new(Mutex::new(None));
        let provider = TavilyProvider::new(
            FakeHttpClient {
                request: Arc::clone(&request),
                response: HttpResponse {
                    status,
                    body: serde_json::to_vec(&body).unwrap(),
                },
            },
            credential,
        )
        .unwrap();
        let result = provider
            .extract(WebExtractRequest::new(EXTRACT_URL).unwrap())
            .await;
        let sent = request.lock().unwrap().clone();
        (result, sent)
    }

    #[tokio::test]
    async fn extract_requests_the_whole_page_and_normalizes_it_within_budget() {
        let raw_content = "openwave ".repeat(8_000);
        let (result, sent) = extract(
            200,
            serde_json::json!({
                "results": [{
                    "url": EXTRACT_URL,
                    "title": " Example Domain ",
                    "raw_content": raw_content,
                }],
                "failed_results": [],
                "response_time": 1.23,
                "usage": { "credits": 1 },
            }),
        )
        .await;

        let response = result.unwrap();
        assert_eq!(
            response.extraction_method,
            ExtractionMethod::Provider(WebSearchProviderKind::Tavily)
        );
        assert_eq!(response.url, EXTRACT_URL);
        // The title is undocumented but present in real responses, so it is
        // reported rather than discarded.
        assert_eq!(response.title, "Example Domain");
        assert_eq!(response.word_count, 8_000);
        // Tavily caps nothing itself, so the output budget is what shortened
        // this page, and it says so.
        assert!(response.truncated);
        assert!(
            serde_json::to_vec(&response).unwrap().len() <= crate::MAX_EXTRACT_OUTPUT_BYTES,
            "extraction exceeded its serialized output budget"
        );

        let sent = sent.unwrap();
        assert_eq!(
            Some(sent.url.as_str()),
            WebSearchProviderKind::Tavily.extract_url()
        );
        assert_eq!(sent.body["urls"], serde_json::json!([EXTRACT_URL]));
        assert_eq!(sent.body["format"], "markdown");
        // A query would turn `raw_content` into reranked chunks instead of the
        // page, so it must never be sent from the extraction path.
        assert!(sent.body.get("query").is_none());
        assert_eq!(
            sent.headers[0],
            ("authorization".into(), "Bearer tavily-key".into())
        );
    }

    #[tokio::test]
    async fn extract_maps_per_url_failure_and_distinctive_statuses_to_typed_errors() {
        let other = "https://example.com/other";
        let page = "openwave ".repeat(40);
        let cases = [
            // The whole point: a wholly failed batch is still HTTP 200, with
            // the reason parked in `failed_results`.
            (
                serde_json::json!({
                    "results": [],
                    "failed_results": [{ "url": EXTRACT_URL, "error": "unreachable host" }],
                }),
                "per-URL failure inside HTTP 200",
            ),
            // A success for some other URL must not be adopted by position.
            (
                serde_json::json!({
                    "results": [{ "url": other, "raw_content": page }],
                    "failed_results": [],
                }),
                "result for a different URL",
            ),
            (
                serde_json::json!({
                    "results": [{ "url": EXTRACT_URL, "raw_content": "Enable JavaScript." }],
                    "failed_results": [],
                }),
                "content below the readable floor",
            ),
        ];
        for (body, case) in cases {
            let (result, _) = extract(200, body).await;
            assert!(
                matches!(
                    result,
                    Err(WebSearchError::PageNotExtracted {
                        provider: WebSearchProviderKind::Tavily
                    })
                ),
                "{case} did not produce a typed per-URL failure"
            );
        }

        let empty = || serde_json::json!({});
        assert!(matches!(
            extract(401, empty()).await.0,
            Err(WebSearchError::CredentialRejected(_))
        ));
        // 432 (plan allowance) and 433 (pay-as-you-go balance) are Tavily's own
        // statuses and are not rate limits: retrying cannot clear either.
        for status in [432, 433] {
            assert!(
                matches!(
                    extract(status, empty()).await.0,
                    Err(WebSearchError::QuotaExhausted(_))
                ),
                "HTTP {status} was not mapped to an exhausted quota"
            );
        }
        assert!(matches!(
            extract(429, empty()).await.0,
            Err(WebSearchError::RateLimited(_))
        ));
        assert!(matches!(
            extract(503, empty()).await.0,
            Err(WebSearchError::HttpStatus { status: 503, .. })
        ));
    }
}
