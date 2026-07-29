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
        formatter
            .debug_struct("HttpRequest")
            .field("url", &redacted_url(&self.url))
            .field("headers", &redacted_pairs(&self.headers))
            .field("body", &redacted_json(&self.body))
            .finish()
    }
}

/// Header or query pairs with every credential-shaped name masked.
fn redacted_pairs(pairs: &[(String, String)]) -> Vec<(&str, &str)> {
    pairs
        .iter()
        .map(|(name, value)| {
            (
                name.as_str(),
                if is_sensitive_name(name) {
                    "***"
                } else {
                    value.as_str()
                },
            )
        })
        .collect()
}

/// One outbound `GET` with its query string supplied as decoded pairs.
///
/// The pairs are kept apart from `url` on purpose: the concrete client binds
/// the request to its provider's authority by inspecting `url` alone, and a
/// caller that had to pre-encode its own query string could otherwise smuggle
/// an authority past that check.
#[derive(Clone, PartialEq)]
pub struct HttpGetRequest {
    pub url: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
}

impl fmt::Debug for HttpGetRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpGetRequest")
            .field("url", &redacted_url(&self.url))
            .field("query", &redacted_pairs(&self.query))
            .field("headers", &redacted_pairs(&self.headers))
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
///
/// Both verbs are required rather than defaulted. Vendors split evenly between
/// JSON bodies and query strings, so a seam that could only `POST` would push
/// the difference into the adapters; and a defaulted `get` that failed at
/// runtime would let a host look implemented while half the backends could not
/// egress through it.
#[async_trait]
pub trait HttpClient: Send + Sync {
    async fn post_json(&self, request: HttpRequest) -> Result<HttpResponse, WebSearchError>;

    /// Dispatch one `GET`, appending `query` to `url` as a query string.
    async fn get(&self, request: HttpGetRequest) -> Result<HttpResponse, WebSearchError>;
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

/// The exact origin one transport is bound to: scheme, host, and explicit port.
///
/// Every hosted provider pins a fixed HTTPS domain through
/// [`Self::fixed`]. A self-hosted instance has no single address to pin, so its
/// origin comes from validated host configuration through [`Self::parse`]. The
/// binding is the same either way — one origin, decided before the client
/// exists, never reachable from a model argument or a tool input.
///
/// [`Self::parse`] is also the one place a non-HTTPS or private destination
/// becomes reachable, and only because the operator typed the address into
/// their own settings. That is a different trust class from the URLs the native
/// extractor fetches, which the model or a fetched page chose; `fetch_policy`
/// governs those and is deliberately untouched by this.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboundOrigin {
    /// Exactly `scheme://host[:port]`, as it must appear at the start of every
    /// request URL this client dispatches.
    origin: String,
    scheme: String,
    host: String,
    port: Option<u16>,
}

impl OutboundOrigin {
    /// The fixed HTTPS domain a provider's endpoints live on.
    ///
    /// `None` for a provider whose address is host configuration.
    #[must_use]
    pub fn fixed(provider: crate::WebSearchProviderKind) -> Option<Self> {
        provider.outbound_domain().map(|domain| Self {
            origin: format!("https://{domain}"),
            scheme: "https".into(),
            host: domain.into(),
            port: None,
        })
    }

    /// The origin of an already validated `http`/`https` base URL.
    pub fn parse(value: &str) -> Result<Self, WebSearchError> {
        let parsed = Url::parse(value).map_err(|_| WebSearchError::OutboundNotAllowed)?;
        let (Some(host), scheme @ ("http" | "https")) = (parsed.host_str(), parsed.scheme()) else {
            return Err(WebSearchError::OutboundNotAllowed);
        };
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(WebSearchError::OutboundNotAllowed);
        }
        let host = host.to_ascii_lowercase();
        let port = parsed.port();
        let authority = match port {
            Some(port) => format!("{host}:{port}"),
            None => host.clone(),
        };
        Ok(Self {
            origin: format!("{scheme}://{authority}"),
            scheme: scheme.to_owned(),
            host,
            port,
        })
    }

    /// The `scheme://host[:port]` prefix every request URL must start with.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.origin
    }
}

#[cfg(feature = "http")]
#[derive(Clone, Debug)]
pub struct ReqwestHttpClient {
    client: reqwest::Client,
    origin: OutboundOrigin,
}

#[cfg(feature = "http")]
impl ReqwestHttpClient {
    /// Build a client bound to one origin with a bounded end-to-end timeout.
    ///
    /// That origin is the only place this client may dial. Redirects stay
    /// disabled so credentials are never replayed to another origin.
    pub fn with_timeout(
        origin: OutboundOrigin,
        timeout: std::time::Duration,
    ) -> Result<Self, WebSearchError> {
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
        Ok(Self { client, origin })
    }

    pub fn new(origin: OutboundOrigin) -> Result<Self, WebSearchError> {
        Self::with_timeout(origin, std::time::Duration::from_secs(20))
    }
}

#[cfg(feature = "http")]
impl ReqwestHttpClient {
    /// Send one already authority-checked request and read its body under the
    /// hard byte cap.
    async fn dispatch(builder: reqwest::RequestBuilder) -> Result<HttpResponse, WebSearchError> {
        use futures::StreamExt;

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

#[cfg(feature = "http")]
#[async_trait]
impl HttpClient for ReqwestHttpClient {
    async fn post_json(&self, request: HttpRequest) -> Result<HttpResponse, WebSearchError> {
        validate_outbound_url(&request.url, &self.origin)?;
        let mut builder = self.client.post(request.url).json(&request.body);
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        Self::dispatch(builder).await
    }

    async fn get(&self, request: HttpGetRequest) -> Result<HttpResponse, WebSearchError> {
        // The authority is decided by `url` alone, and `query` is appended by
        // the client afterwards, so no query pair can move the destination.
        validate_outbound_url(&request.url, &self.origin)?;
        let mut builder = self.client.get(request.url).query(&request.query);
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        Self::dispatch(builder).await
    }
}

/// Refuse any request URL that does not sit exactly on the bound origin.
///
/// The check is deliberately made twice over. The raw prefix comparison rejects
/// anything the parser might normalize away — an added default port, a trailing
/// dot on the host, userinfo, a look-alike suffix — and the parsed comparison
/// rejects anything the raw form could disguise.
#[cfg(feature = "http")]
fn validate_outbound_url(value: &str, origin: &OutboundOrigin) -> Result<(), WebSearchError> {
    let parsed = Url::parse(value).map_err(|_| WebSearchError::OutboundNotAllowed)?;
    let boundary_is_clean = value
        .strip_prefix(&origin.origin)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '?', '#']));
    if !boundary_is_clean
        || parsed.scheme() != origin.scheme
        || parsed.host_str() != Some(origin.host.as_str())
        || parsed.port() != origin.port
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(WebSearchError::OutboundNotAllowed);
    }
    Ok(())
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

        let get = HttpGetRequest {
            url: "https://url-user:url-password@example.com/search?api_key=url-secret".into(),
            query: vec![
                ("q".into(), "public".into()),
                ("subscription_token".into(), "query-secret".into()),
            ],
            headers: vec![("X-Subscription-Token".into(), "brave-secret".into())],
        };

        let debug = format!("{request:?}{get:?}");
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
            "query-secret",
            "brave-secret",
        ] {
            assert!(!debug.contains(secret), "debug leaked {secret}: {debug}");
        }
        assert!(debug.contains("***"));
        assert!(debug.contains("public"));
    }

    #[cfg(feature = "http")]
    #[tokio::test]
    async fn reqwest_client_rejects_requests_outside_its_provider_domain() {
        let client = ReqwestHttpClient::new(
            OutboundOrigin::fixed(crate::WebSearchProviderKind::Exa).unwrap(),
        )
        .unwrap();
        for url in [
            "http://api.exa.ai/search",
            "https://api.tavily.com/search",
            "https://api.exa.ai:443/search",
            "https://api.exa.ai.evil.example/search",
            "https://api.exa.ai./search",
            "https://user:password@api.exa.ai/search",
        ] {
            let error = client
                .post_json(HttpRequest {
                    url: url.into(),
                    headers: vec![("x-api-key".into(), "must-not-egress".into())],
                    body: serde_json::json!({ "query": "openwave" }),
                })
                .await
                .unwrap_err();
            assert!(
                matches!(error, WebSearchError::OutboundNotAllowed),
                "unexpected POST result for {url}: {error}"
            );

            // The GET verb is bound by exactly the same rule; a seam that
            // checked only one of them would be no boundary at all.
            let error = client
                .get(HttpGetRequest {
                    url: url.into(),
                    query: vec![("q".into(), "openwave".into())],
                    headers: vec![("x-subscription-token".into(), "must-not-egress".into())],
                })
                .await
                .unwrap_err();
            assert!(
                matches!(error, WebSearchError::OutboundNotAllowed),
                "unexpected GET result for {url}: {error}"
            );
        }
    }

    #[cfg(feature = "http")]
    #[test]
    fn provider_endpoints_satisfy_their_fixed_domain_policy() {
        for provider in crate::WebSearchProviderKind::ALL {
            let Some(origin) = OutboundOrigin::fixed(provider) else {
                // A self-hosted provider has no fixed domain to check; its
                // origin is validated where the operator configures it.
                continue;
            };
            for endpoint in provider
                .search_url()
                .into_iter()
                .chain(provider.extract_url())
            {
                assert!(
                    validate_outbound_url(endpoint, &origin).is_ok(),
                    "{endpoint} is not on {provider}'s fixed outbound domain"
                );
            }
        }
    }

    /// A configured origin is still exactly one origin. Loopback and an
    /// explicit port are reachable — that is the point of a self-hosted
    /// instance — but nothing else on the host is.
    #[cfg(feature = "http")]
    #[test]
    fn a_configured_origin_binds_as_tightly_as_a_fixed_one() {
        let origin = OutboundOrigin::parse("http://localhost:8888").unwrap();
        assert!(validate_outbound_url("http://localhost:8888/search", &origin).is_ok());
        for url in [
            "https://localhost:8888/search",
            "http://localhost:8889/search",
            "http://localhost/search",
            "http://localhost.evil.example:8888/search",
            "http://user:password@localhost:8888/search",
            "http://127.0.0.1:8888/search",
        ] {
            assert!(
                matches!(
                    validate_outbound_url(url, &origin),
                    Err(WebSearchError::OutboundNotAllowed)
                ),
                "{url} was accepted against {}",
                origin.as_str()
            );
        }

        // Userinfo never survives into an origin in the first place.
        assert!(OutboundOrigin::parse("http://user:password@localhost:8888").is_err());
        assert!(OutboundOrigin::parse("ftp://localhost").is_err());
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

        let client = ReqwestHttpClient::new(
            OutboundOrigin::fixed(crate::WebSearchProviderKind::Exa).unwrap(),
        )
        .unwrap();
        let response = client
            .client
            .post(format!("http://{source_address}/search"))
            .header("x-api-key", "not-for-the-redirect-target")
            .json(&serde_json::json!({ "query": "openwave" }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status().as_u16(), 307);
        source_task.await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), target.accept())
                .await
                .is_err()
        );
    }
}
