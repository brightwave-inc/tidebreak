use std::fmt;

use async_trait::async_trait;
use serde_json::Value;
use url::Url;

use crate::WebSearchError;

/// Largest JSON response an adapter may retain in memory or parse.
pub const MAX_HTTP_RESPONSE_BYTES: usize = 1_000_000;

#[derive(Clone, PartialEq)]
pub struct HttpRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str(),
                    if is_sensitive_name(name) {
                        "***"
                    } else {
                        value
                    },
                )
            })
            .collect();
        formatter
            .debug_struct("HttpRequest")
            .field("url", &redacted_url(&self.url))
            .field("headers", &headers)
            .field("body", &redacted_json(&self.body))
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// Reject a custom HTTP implementation's oversized body before an adapter
    /// asks `serde_json` to allocate for it.
    pub fn ensure_bounded(&self) -> Result<(), WebSearchError> {
        if self.body.len() > MAX_HTTP_RESPONSE_BYTES {
            return Err(WebSearchError::Transport(
                "web search response exceeded byte limit".into(),
            ));
        }
        Ok(())
    }
}

/// Minimal outbound HTTP seam. A host can provide proxy, allow-list, test, or
/// auditing policy here without exposing that machinery to a model tool.
#[async_trait]
pub trait HttpClient: Send + Sync {
    async fn post_json(&self, request: HttpRequest) -> Result<HttpResponse, WebSearchError>;
}

fn is_sensitive_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase().replace('_', "-");
    name.contains("auth")
        || name.contains("token")
        || name.contains("secret")
        || name.contains("credential")
        || name.contains("password")
        || name.contains("cookie")
        || name.contains("session")
        || name.contains("key")
}

fn redacted_url(url: &str) -> String {
    let Ok(mut parsed) = Url::parse(url) else {
        // An invalid URL cannot be dispatched by the concrete client. Avoid
        // echoing it because it may still contain an accidental credential.
        return "<invalid url>".into();
    };
    // URL userinfo is another credential channel. It is never useful in a
    // diagnostic and must not survive Debug formatting.
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    let pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .map(|(name, value)| {
            (
                name.to_string(),
                if is_sensitive_name(&name) {
                    "***".into()
                } else {
                    value.to_string()
                },
            )
        })
        .collect();
    if !pairs.is_empty() {
        let mut query = parsed.query_pairs_mut();
        query.clear();
        query.extend_pairs(pairs.iter().map(|(name, value)| (&**name, &**value)));
    }
    parsed.into()
}

fn redacted_json(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(redacted_json).collect()),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(name, value)| {
                    (
                        name.clone(),
                        if is_sensitive_name(name) {
                            Value::String("***".into())
                        } else {
                            redacted_json(value)
                        },
                    )
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(feature = "http")]
#[derive(Clone, Debug)]
pub struct ReqwestHttpClient {
    client: reqwest::Client,
}

#[cfg(feature = "http")]
impl ReqwestHttpClient {
    /// Build a client with one bounded end-to-end request timeout.
    ///
    /// The caller owns the policy for the value. Redirects stay disabled for
    /// every timeout so credentials are never replayed to another origin.
    pub fn with_timeout(timeout: std::time::Duration) -> Result<Self, WebSearchError> {
        if timeout.is_zero() {
            return Err(WebSearchError::Transport(
                "web search timeout must be greater than zero".into(),
            ));
        }
        let client = reqwest::Client::builder()
            // Providers authenticate the initial request. Never follow a
            // 307/308 (or any) redirect that could replay that credential to a
            // different origin.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(timeout.min(std::time::Duration::from_secs(10)))
            .timeout(timeout)
            .build()
            .map_err(|error| WebSearchError::Transport(error.to_string()))?;
        Ok(Self { client })
    }

    pub fn new() -> Result<Self, WebSearchError> {
        Self::with_timeout(std::time::Duration::from_secs(20))
    }
}

#[cfg(feature = "http")]
#[async_trait]
impl HttpClient for ReqwestHttpClient {
    async fn post_json(&self, request: HttpRequest) -> Result<HttpResponse, WebSearchError> {
        use futures::StreamExt;

        let mut builder = self.client.post(request.url).json(&request.body);
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        let response = builder
            .send()
            .await
            .map_err(|error| WebSearchError::Transport(error.to_string()))?;
        let status = response.status().as_u16();
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| WebSearchError::Transport(error.to_string()))?;
            if body.len().saturating_add(chunk.len()) > MAX_HTTP_RESPONSE_BYTES {
                return Err(WebSearchError::Transport(
                    "web search response exceeded byte limit".into(),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(HttpResponse { status, body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_debug_redacts_keys_in_headers_body_and_url() {
        let request = HttpRequest {
            url: "https://url-user:url-password@example.com/search?api_key=url-secret&query=public"
                .into(),
            headers: vec![
                ("x-api-key".into(), "exa-secret".into()),
                ("Authorization".into(), "Bearer auth-secret".into()),
                ("x-auth".into(), "short-auth-secret".into()),
                ("content-type".into(), "application/json".into()),
            ],
            body: serde_json::json!({
                "api_key": "tavily-secret",
                "nested": {
                    "access_token": "nested-secret",
                    "credential": "credential-secret",
                    "query": "public",
                },
            }),
        };

        let debug = format!("{request:?}");
        for secret in [
            "url-secret",
            "url-user",
            "url-password",
            "exa-secret",
            "auth-secret",
            "short-auth-secret",
            "tavily-secret",
            "nested-secret",
            "credential-secret",
        ] {
            assert!(!debug.contains(secret), "debug leaked {secret}: {debug}");
        }
        assert!(debug.contains("***"));
        assert!(debug.contains("public"));
    }

    #[cfg(feature = "http")]
    #[tokio::test]
    async fn reqwest_client_does_not_follow_cross_host_redirects_with_credentials() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_address = target.local_addr().unwrap();
        let source = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_address = source.local_addr().unwrap();
        let source_task = tokio::spawn(async move {
            let (mut stream, _) = source.accept().await.unwrap();
            let mut buffer = [0_u8; 2048];
            let bytes_read = stream.read(&mut buffer).await.unwrap();
            assert!(bytes_read > 0);
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{target_address}/steal\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });

        let client = ReqwestHttpClient::new().unwrap();
        let response = client
            .post_json(HttpRequest {
                url: format!("http://{source_address}/search"),
                headers: vec![("x-api-key".into(), "not-for-the-redirect-target".into())],
                body: serde_json::json!({ "query": "openwave" }),
            })
            .await
            .unwrap();

        assert_eq!(response.status, 307);
        source_task.await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), target.accept())
                .await
                .is_err()
        );
    }
}
