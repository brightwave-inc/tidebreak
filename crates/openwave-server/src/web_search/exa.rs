//! Exa's direct `/search` and `/contents` adapter.

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;

use super::types::{count_words, same_page_url};
use crate::web_search::{
    admit_fetch_url, ExtractionMethod, HttpClient, HttpRequest, WebExtractRequest,
    WebExtractResponse, WebSearchCredential, WebSearchError, WebSearchProvider,
    WebSearchProviderKind, WebSearchRequest, WebSearchResponse, WebSearchResult, MIN_EXTRACT_WORDS,
};

/// Upper bound Exa accepts for `text.maxCharacters`, and what this adapter
/// always asks for.
///
/// Bounding the page at the source means the vendor never ships a whole book
/// over the wire for us to throw away, and even all-4-byte characters at this
/// length stay inside the serialized extraction budget — the trim ladder in
/// [`WebExtractResponse`] is a backstop here, not the primary control.
const EXTRACT_MAX_CHARACTERS: usize = 10_000;

const _: () = assert!(EXTRACT_MAX_CHARACTERS * 4 <= crate::web_search::MAX_EXTRACT_OUTPUT_BYTES);

/// How stale a cached crawl may be before Exa is asked to fetch the page live.
///
/// This is the supported replacement for the deprecated `livecrawl` mode
/// selector. A day-old cache is fine for the pages an agent reads; anything
/// older is re-crawled rather than quietly answered from an archive.
const EXTRACT_MAX_AGE_HOURS: u32 = 24;

/// Exa adapter backed by an injected HTTP client.
///
/// It has no default constructor because the host must explicitly decide which
/// HTTP policy and credential custody to use.
#[derive(Clone, Debug)]
pub struct ExaProvider<C> {
    client: C,
    credential: WebSearchCredential,
}

impl<C> ExaProvider<C> {
    pub fn new(client: C, credential: WebSearchCredential) -> Result<Self, WebSearchError> {
        if credential.kind() != WebSearchProviderKind::Exa {
            return Err(WebSearchError::NotConfigured(WebSearchProviderKind::Exa));
        }
        Ok(Self { client, credential })
    }
}

#[async_trait]
impl<C: HttpClient> WebSearchProvider for ExaProvider<C> {
    fn kind(&self) -> WebSearchProviderKind {
        WebSearchProviderKind::Exa
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
                    ("x-api-key".into(), self.credential.api_key().into()),
                    ("content-type".into(), "application/json".into()),
                ],
                body: request_body(&request),
            })
            .await?;
        response.ensure_bounded()?;
        if !(200..300).contains(&response.status) {
            return Err(WebSearchError::HttpStatus {
                provider: self.kind(),
                status: response.status,
            });
        }
        let payload: ExaSearchResponse = serde_json::from_slice(&response.body).map_err(|_| {
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
        // Revalidate at the egress boundary, exactly as `search` does: this
        // request may have been deserialized rather than freshly admitted.
        request.validate()?;
        let response = self
            .client
            .post_json(HttpRequest {
                // The authority is fixed by the provider kind and bound into
                // the transport before the credential is attached here.
                url: self
                    .kind()
                    .extract_url()
                    .ok_or(WebSearchError::ExtractNotSupported(self.kind()))?
                    .into(),
                headers: vec![
                    // `/contents` documents bearer auth; the legacy `x-api-key`
                    // header the search path still uses is not the shape new
                    // calls should be written against.
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
        let payload: ExaContentsResponse =
            serde_json::from_slice(&response.body).map_err(|_| {
                WebSearchError::InvalidResponse {
                    provider: self.kind(),
                }
            })?;
        normalize_extraction(request.url(), payload)
    }
}

fn request_body(request: &WebSearchRequest) -> serde_json::Value {
    let mut body = json!({
        "query": request.query,
        "type": "auto",
        "numResults": request.max_results,
        "contents": { "text": { "maxCharacters": crate::web_search::MAX_RESULT_CONTENT_CHARS } },
    });
    if !request.domains.is_empty() {
        body["includeDomains"] = json!(request
            .domains
            .iter()
            .map(|domain| domain.as_str())
            .collect::<Vec<_>>());
    }
    if let Some(start) = request.start_published_at {
        body["startPublishedDate"] = json!(start.to_rfc3339());
    }
    if let Some(end) = request.end_published_at {
        body["endPublishedDate"] = json!(end.to_rfc3339());
    }
    body
}

#[derive(Deserialize)]
struct ExaSearchResponse {
    results: Vec<ExaResult>,
}

#[derive(Deserialize)]
struct ExaResult {
    url: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    highlights: Vec<String>,
    #[serde(default)]
    score: Option<f64>,
    #[serde(default, rename = "publishedDate")]
    published_date: Option<String>,
    #[serde(default)]
    image: Option<String>,
}

fn normalize_result(result: ExaResult) -> Result<WebSearchResult, WebSearchError> {
    let snippet = result
        .highlights
        .into_iter()
        .find(|highlight| !highlight.trim().is_empty())
        .unwrap_or_else(|| result.text.clone());
    let published_at = result
        .published_date
        .as_deref()
        .and_then(parse_provider_timestamp);
    WebSearchResult::new(
        result.url,
        result.title,
        snippet,
        (!result.text.trim().is_empty()).then_some(result.text),
        result.score,
        published_at,
        result.image,
        BTreeMap::new(),
    )
}

fn parse_provider_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

/// One URL, text always requested explicitly.
///
/// `text` is sent as an object on every call rather than relying on the
/// endpoint's default: the default decides both whether text comes back at all
/// and how much of it, and neither is something this adapter should inherit
/// from a vendor release note. `includeHtmlTags` stays off so the payload is
/// the markdown the extraction contract promises.
fn extract_request_body(url: &str) -> serde_json::Value {
    json!({
        "urls": [url],
        "text": {
            "maxCharacters": EXTRACT_MAX_CHARACTERS,
            "includeHtmlTags": false,
        },
        "maxAgeHours": EXTRACT_MAX_AGE_HOURS,
    })
}

/// Project a request-level status onto the crate's typed errors.
///
/// Only the statuses that change what a caller should do are named. Everything
/// else — validation, server faults — stays a plain [`WebSearchError::HttpStatus`],
/// which the extraction tool treats as "this vendor call failed, try native".
fn extract_status_error(status: u16) -> WebSearchError {
    let provider = WebSearchProviderKind::Exa;
    match status {
        401 => WebSearchError::CredentialRejected(provider),
        // Exa reports an exhausted balance as Payment Required.
        402 => WebSearchError::QuotaExhausted(provider),
        429 => WebSearchError::RateLimited(provider),
        status => WebSearchError::HttpStatus { provider, status },
    }
}

#[derive(Deserialize)]
struct ExaContentsResponse {
    #[serde(default)]
    results: Vec<ExaContentResult>,
    /// Per-URL outcome, parallel to but not positionally aligned with
    /// `results`: a URL that failed appears here only.
    #[serde(default)]
    statuses: Vec<ExaContentStatus>,
}

#[derive(Deserialize)]
struct ExaContentResult {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

/// The `error` object carries a closed vendor tag (`CRAWL_NOT_FOUND`,
/// `CRAWL_TIMEOUT`, …). It is deliberately not read: the tag would only ever be
/// forwarded, and no vendor string belongs in a model context. `source` used to
/// appear here and no longer does, which unknown-field tolerance already covers.
#[derive(Deserialize)]
struct ExaContentStatus {
    #[serde(default)]
    id: String,
    #[serde(default)]
    status: String,
}

/// Turn one `/contents` payload into a bounded extraction, or a typed failure.
///
/// A failed URL comes back inside an HTTP 200 with an empty `results`, so the
/// per-URL status is consulted first and the result is then matched by key. An
/// absent or too-thin result is a failure, never an empty-content success.
fn normalize_extraction(
    requested: &str,
    payload: ExaContentsResponse,
) -> Result<WebExtractResponse, WebSearchError> {
    let provider = WebSearchProviderKind::Exa;
    let failed = || WebSearchError::PageNotExtracted { provider };
    if payload
        .statuses
        .iter()
        .any(|entry| same_page_url(&entry.id, requested) && entry.status != "success")
    {
        return Err(failed());
    }
    let result = payload
        .results
        .into_iter()
        .find(|result| {
            [result.id.as_deref(), result.url.as_deref()]
                .into_iter()
                .flatten()
                .any(|candidate| same_page_url(candidate, requested))
        })
        .ok_or_else(failed)?;
    let text = result.text.unwrap_or_default();
    let text = text.trim();
    let word_count = count_words(text);
    if word_count < MIN_EXTRACT_WORDS {
        return Err(failed());
    }
    // Exa bounds the text but does not say when it hit the bound. Content
    // sitting exactly on the cap is reported as truncated rather than passed
    // off as a whole page.
    let truncated = text.chars().count() >= EXTRACT_MAX_CHARACTERS;
    // The vendor may echo a resolved URL. Accept it only if it clears the same
    // admission policy every native redirect hop clears; otherwise keep the URL
    // the caller asked for.
    let url = result
        .url
        .as_deref()
        .and_then(|url| admit_fetch_url(url).ok())
        .map_or_else(|| requested.to_owned(), String::from);
    WebExtractResponse::new(
        ExtractionMethod::Provider(provider),
        url,
        result.title.unwrap_or_default(),
        text,
        word_count,
        truncated,
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::web_search::{HttpResponse, WebSearchCredentialState, WebSearchCredentials};
    use openwave_core::{AgentError, SecretProvider};

    #[derive(Clone)]
    struct FakeHttpClient {
        seen: Arc<Mutex<Vec<HttpRequest>>>,
        response: HttpResponse,
    }

    struct StaticSecrets;

    #[async_trait]
    impl SecretProvider for StaticSecrets {
        async fn get_secret(&self, key: &str) -> openwave_core::Result<Option<String>> {
            Ok(
                (Some(key) == WebSearchProviderKind::Exa.credential_key())
                    .then(|| "exa-key".into()),
            )
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
        match WebSearchCredentials::resolve(&StaticSecrets, WebSearchProviderKind::Exa).await {
            Ok(WebSearchCredentialState::Present(credential)) => credential,
            other => panic!("expected a stored Exa key, got {other:?}"),
        }
    }

    #[async_trait]
    impl HttpClient for FakeHttpClient {
        async fn post_json(&self, request: HttpRequest) -> Result<HttpResponse, WebSearchError> {
            self.seen.lock().unwrap().push(request);
            Ok(self.response.clone())
        }

        async fn get(
            &self,
            _request: crate::web_search::HttpGetRequest,
        ) -> Result<HttpResponse, WebSearchError> {
            unreachable!("the Exa adapter posts JSON on both endpoints")
        }
    }

    #[tokio::test]
    async fn maps_exa_response_and_sends_bounded_direct_request() {
        let credential = test_credential().await;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let client = FakeHttpClient {
            seen: Arc::clone(&seen),
            response: HttpResponse {
                status: 200,
                body: br#"{"results":[{"url":"https://example.com/a#part","title":"Example","text":"full text","highlights":["short summary"],"score":0.8,"publishedDate":"2026-01-01T00:00:00Z"},{"url":"not-a-url","title":"skip"}]}"#.to_vec(),
            },
        };
        let provider = ExaProvider::new(client, credential).unwrap();
        let request = WebSearchRequest::new("latest openwave", 2)
            .unwrap()
            .with_domains(vec!["docs.example.com".parse().unwrap()])
            .unwrap();
        let response = provider.search(request).await.unwrap();

        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].url, "https://example.com/a");
        assert_eq!(response.results[0].snippet, "short summary");
        let sent = seen.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(
            Some(sent[0].url.as_str()),
            WebSearchProviderKind::Exa.search_url()
        );
        assert_eq!(sent[0].body["numResults"], 2);
        assert_eq!(sent[0].body["includeDomains"][0], "docs.example.com");
        assert_eq!(sent[0].headers[0], ("x-api-key".into(), "exa-key".into()));
    }

    const EXTRACT_URL: &str = "https://example.com/article";

    async fn extract(
        status: u16,
        body: serde_json::Value,
    ) -> (Result<WebExtractResponse, WebSearchError>, Vec<HttpRequest>) {
        let credential = test_credential().await;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let provider = ExaProvider::new(
            FakeHttpClient {
                seen: Arc::clone(&seen),
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
        let sent = seen.lock().unwrap().clone();
        (result, sent)
    }

    #[tokio::test]
    async fn extract_asks_for_bounded_text_and_normalizes_the_page_within_budget() {
        // A page far larger than either bound, so both the vendor-side cap and
        // the serialized output budget have to do their jobs.
        let text = "openwave ".repeat(8_000);
        let (result, sent) = extract(
            200,
            serde_json::json!({
                "requestId": "request-fixture",
                "results": [{
                    "id": EXTRACT_URL,
                    "url": EXTRACT_URL,
                    "title": "Example article",
                    "text": text,
                    "publishedDate": "2026-01-01T00:00:00Z",
                    "author": "Example",
                }],
                "statuses": [{ "id": EXTRACT_URL, "status": "success" }],
                "costDollars": { "total": 0.001 },
            }),
        )
        .await;

        let response = result.unwrap();
        assert_eq!(
            response.extraction_method,
            ExtractionMethod::Provider(WebSearchProviderKind::Exa)
        );
        assert_eq!(response.url, EXTRACT_URL);
        assert_eq!(response.title, "Example article");
        // Words are counted over the whole extraction, before either trim.
        assert_eq!(response.word_count, 8_000);
        assert!(response.truncated);
        assert!(
            serde_json::to_vec(&response).unwrap().len()
                <= crate::web_search::MAX_EXTRACT_OUTPUT_BYTES,
            "extraction exceeded its serialized output budget"
        );

        assert_eq!(
            Some(sent[0].url.as_str()),
            WebSearchProviderKind::Exa.extract_url()
        );
        assert_eq!(sent[0].body["urls"], serde_json::json!([EXTRACT_URL]));
        // Text is always requested explicitly and always bounded at the source,
        // and freshness is expressed through the supported knob.
        assert_eq!(
            sent[0].body["text"]["maxCharacters"],
            EXTRACT_MAX_CHARACTERS
        );
        assert_eq!(sent[0].body["maxAgeHours"], EXTRACT_MAX_AGE_HOURS);
        assert!(sent[0].body.get("livecrawl").is_none());
        assert_eq!(
            sent[0].headers[0],
            ("authorization".into(), "Bearer exa-key".into())
        );
    }

    #[tokio::test]
    async fn extract_maps_per_url_failure_and_distinctive_statuses_to_typed_errors() {
        let other = "https://example.com/other";
        let page = "openwave ".repeat(40);
        let cases = [
            // The whole point: a failed URL comes back inside an HTTP 200 with
            // an empty `results` and the reason in `statuses`.
            (
                serde_json::json!({
                    "results": [],
                    "statuses": [{
                        "id": EXTRACT_URL,
                        "status": "error",
                        "error": { "tag": "CRAWL_NOT_FOUND", "httpStatusCode": 404 },
                    }],
                }),
                "per-URL failure inside HTTP 200",
            ),
            // A success for some other URL must not be adopted by position.
            (
                serde_json::json!({
                    "results": [{ "id": other, "url": other, "title": "Other", "text": page }],
                    "statuses": [{ "id": other, "status": "success" }],
                }),
                "result for a different URL",
            ),
            // A result too thin to be a page is nothing, not a blank success.
            (
                serde_json::json!({
                    "results": [{ "id": EXTRACT_URL, "url": EXTRACT_URL, "text": "Enable JavaScript." }],
                    "statuses": [{ "id": EXTRACT_URL, "status": "success" }],
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
                        provider: WebSearchProviderKind::Exa
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
        // Exa signals an exhausted balance as Payment Required, which no amount
        // of backing off resolves.
        assert!(matches!(
            extract(402, empty()).await.0,
            Err(WebSearchError::QuotaExhausted(_))
        ));
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
