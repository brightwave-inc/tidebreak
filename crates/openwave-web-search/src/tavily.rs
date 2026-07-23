//! Tavily's direct `/search` adapter.

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::json;

use crate::{
    HttpClient, HttpRequest, WebSearchCredential, WebSearchError, WebSearchProvider,
    WebSearchProviderKind, WebSearchRequest, WebSearchResponse, WebSearchResult,
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
            Ok(HttpResponse {
                status: 200,
                body: br#"{"results":[{"url":"https://example.com/t","title":"Tavily","content":"bounded content","score":0.5,"published_date":"2026-01-01"}]}"#.to_vec(),
            })
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
}
