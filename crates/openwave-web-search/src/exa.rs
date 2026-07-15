//! Exa's direct `/search` adapter.

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;

use crate::{
    HttpClient, HttpRequest, WebSearchCredential, WebSearchError, WebSearchProvider,
    WebSearchProviderKind, WebSearchRequest, WebSearchResponse, WebSearchResult,
};

const EXA_SEARCH_URL: &str = "https://api.exa.ai/search";

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
                url: EXA_SEARCH_URL.into(),
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
}

fn request_body(request: &WebSearchRequest) -> serde_json::Value {
    let mut body = json!({
        "query": request.query,
        "type": "auto",
        "numResults": request.max_results,
        "contents": { "text": { "maxCharacters": crate::MAX_RESULT_CONTENT_CHARS } },
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::{HttpResponse, WebSearchCredentials};
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
            Ok((key == WebSearchProviderKind::Exa.credential_key()).then(|| "exa-key".into()))
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
            self.seen.lock().unwrap().push(request);
            Ok(self.response.clone())
        }
    }

    #[tokio::test]
    async fn maps_exa_response_and_sends_bounded_direct_request() {
        let credential = WebSearchCredentials::load(&StaticSecrets, WebSearchProviderKind::Exa)
            .await
            .unwrap()
            .unwrap();
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
        assert_eq!(sent[0].url, EXA_SEARCH_URL);
        assert_eq!(sent[0].body["numResults"], 2);
        assert_eq!(sent[0].body["includeDomains"][0], "docs.example.com");
        assert_eq!(sent[0].headers[0], ("x-api-key".into(), "exa-key".into()));
    }
}
