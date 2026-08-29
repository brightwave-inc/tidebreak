//! HashiCorp Vault KV v2 custody for self-host credentials.

use std::io::Read as _;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt as _;
use reqwest::header::{HeaderValue, ACCEPT, CONTENT_TYPE};
use reqwest::{Method, StatusCode, Url};
use serde::{Deserialize, Serialize};
use tidebreak_core::{AgentError, Result, SecretProvider, VaultSecretConfig};

const USER_AGENT: &str = "tidebreak-vault/1";
const TOKEN_FILE_LIMIT: u64 = 16 * 1024;
const SECRET_VALUE_LIMIT: usize = 1024 * 1024;
const REQUEST_BODY_LIMIT: usize = 2 * 1024 * 1024;
const RESPONSE_BODY_LIMIT: usize = 2 * 1024 * 1024;
const SECRET_KEY_LIMIT: usize = 512;

const VAULT_TOKEN_HEADER: &str = "x-vault-token";
const VAULT_NAMESPACE_HEADER: &str = "x-vault-namespace";

/// A [`SecretProvider`] that stores one item per Vault KV v2 secret path.
pub(crate) struct VaultSecretProvider {
    base_url: Url,
    client: reqwest::Client,
    mount: Vec<String>,
    namespace: Option<HeaderValue>,
    path: Vec<String>,
    token_file: PathBuf,
}

impl VaultSecretProvider {
    /// Validate the operator configuration and build the no-redirect client.
    pub(crate) fn new(config: &VaultSecretConfig) -> Result<Self> {
        let base_url = validate_base_url(&config.address)?;
        let mount = validate_secret_path("TIDEBREAK_VAULT_MOUNT", &config.mount)?;
        let path = validate_secret_path("TIDEBREAK_VAULT_PATH", &config.path)?;
        if config.token_file.as_os_str().is_empty() {
            return Err(AgentError::config(
                "TIDEBREAK_VAULT_TOKEN_FILE must name a mounted token file",
            ));
        }
        let namespace = config
            .namespace
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(HeaderValue::from_str)
            .transpose()
            .map_err(|_| {
                AgentError::config(
                    "TIDEBREAK_VAULT_NAMESPACE contains characters that are invalid in an HTTP header",
                )
            })?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(base_url.scheme() == "https")
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .user_agent(USER_AGENT)
            .build()
            .map_err(|_| AgentError::config("could not initialize the Vault HTTP client"))?;
        Ok(Self {
            base_url,
            client,
            mount,
            namespace,
            path,
            token_file: config.token_file.clone(),
        })
    }

    fn secret_url(&self, key: &str) -> Result<Url> {
        if key.is_empty() || key.len() > SECRET_KEY_LIMIT || matches!(key, "." | "..") {
            return Err(AgentError::Secret(format!(
                "the credential key must contain 1 to {SECRET_KEY_LIMIT} bytes"
            )));
        }
        let mut url = self.base_url.clone();
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| AgentError::config("TIDEBREAK_VAULT_ADDR cannot be used as a base URL"))?;
        segments.pop_if_empty();
        segments.push("v1");
        for segment in &self.mount {
            segments.push(segment);
        }
        segments.push("data");
        for segment in &self.path {
            segments.push(segment);
        }
        segments.push(key);
        drop(segments);
        Ok(url)
    }

    async fn request(
        &self,
        method: Method,
        key: &str,
        body: Option<Vec<u8>>,
    ) -> Result<reqwest::Response> {
        let token = self.read_token().await?;
        let mut request = self
            .client
            .request(method, self.secret_url(key)?)
            .header(VAULT_TOKEN_HEADER, token)
            .header(ACCEPT, "application/json");
        if let Some(namespace) = &self.namespace {
            request = request.header(VAULT_NAMESPACE_HEADER, namespace.clone());
        }
        if let Some(body) = body {
            request = request.header(CONTENT_TYPE, "application/json").body(body);
        }
        request.send().await.map_err(|error| {
            if error.is_timeout() {
                AgentError::Secret("the Vault request timed out".into())
            } else {
                AgentError::Secret("the Vault request failed".into())
            }
        })
    }

    async fn read_token(&self) -> Result<HeaderValue> {
        let path = self.token_file.clone();
        let bytes = tokio::task::spawn_blocking(move || {
            let file = std::fs::File::open(path)?;
            let mut bytes = Vec::new();
            file.take(TOKEN_FILE_LIMIT + 1).read_to_end(&mut bytes)?;
            Ok::<_, std::io::Error>(bytes)
        })
        .await
        .map_err(|_| AgentError::Secret("could not read the configured Vault token file".into()))?
        .map_err(|_| AgentError::Secret("could not read the configured Vault token file".into()))?;
        if bytes.len() as u64 > TOKEN_FILE_LIMIT {
            return Err(AgentError::Secret(
                "the configured Vault token file exceeds the size limit".into(),
            ));
        }
        let token = std::str::from_utf8(&bytes)
            .map_err(|_| AgentError::Secret("the configured Vault token is not UTF-8".into()))?
            .trim();
        if token.is_empty() {
            return Err(AgentError::Secret(
                "the configured Vault token file is empty".into(),
            ));
        }
        HeaderValue::from_str(token).map_err(|_| {
            AgentError::Secret("the configured Vault token is not a valid HTTP header value".into())
        })
    }
}

#[async_trait]
impl SecretProvider for VaultSecretProvider {
    async fn get_secret(&self, key: &str) -> Result<Option<String>> {
        let response = self.request(Method::GET, key, None).await?;
        let status = response.status();
        let body = read_bounded(response).await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        require_success("read", status)?;
        let response: VaultReadResponse = serde_json::from_slice(&body).map_err(|_| {
            AgentError::Secret("Vault returned an unreadable credential response".into())
        })?;
        if response.data.data.value.len() > SECRET_VALUE_LIMIT {
            return Err(AgentError::Secret(
                "Vault returned a credential that exceeds the size limit".into(),
            ));
        }
        Ok(Some(response.data.data.value))
    }

    async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
        if value.len() > SECRET_VALUE_LIMIT {
            return Err(AgentError::Secret(
                "the credential exceeds the Vault storage size limit".into(),
            ));
        }
        let body = serde_json::to_vec(&VaultWriteRequest {
            data: VaultWriteData { value },
        })
        .map_err(|_| AgentError::Secret("could not encode the Vault credential request".into()))?;
        if body.len() > REQUEST_BODY_LIMIT {
            return Err(AgentError::Secret(
                "the encoded Vault credential request exceeds the size limit".into(),
            ));
        }
        let response = self.request(Method::POST, key, Some(body)).await?;
        let status = response.status();
        let _ = read_bounded(response).await?;
        require_success("write", status)
    }

    async fn delete_secret(&self, key: &str) -> Result<()> {
        let response = self.request(Method::DELETE, key, None).await?;
        let status = response.status();
        let _ = read_bounded(response).await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(());
        }
        require_success("delete", status)
    }
}

/// A self-host deployment without Vault can read environment fallbacks but
/// cannot persist deployment credentials.
pub(crate) struct UnavailableSelfHostSecretProvider;

#[async_trait]
impl SecretProvider for UnavailableSelfHostSecretProvider {
    async fn get_secret(&self, _key: &str) -> Result<Option<String>> {
        Ok(None)
    }

    async fn set_secret(&self, _key: &str, _value: &str) -> Result<()> {
        Err(vault_setup_error())
    }

    async fn delete_secret(&self, _key: &str) -> Result<()> {
        Err(vault_setup_error())
    }
}

fn vault_setup_error() -> AgentError {
    AgentError::config(
        "stored credentials are unavailable for this self-host deployment; set TIDEBREAK_VAULT_ADDR and TIDEBREAK_VAULT_TOKEN_FILE to enable Vault KV v2 custody"
            .to_owned(),
    )
}

#[derive(Deserialize)]
struct VaultReadResponse {
    data: VaultReadOuterData,
}

#[derive(Deserialize)]
struct VaultReadOuterData {
    data: VaultReadData,
}

#[derive(Deserialize)]
struct VaultReadData {
    value: String,
}

#[derive(Serialize)]
struct VaultWriteRequest<'a> {
    data: VaultWriteData<'a>,
}

#[derive(Serialize)]
struct VaultWriteData<'a> {
    value: &'a str,
}

fn validate_base_url(raw: &str) -> Result<Url> {
    let mut url = Url::parse(raw.trim())
        .map_err(|_| AgentError::config("TIDEBREAK_VAULT_ADDR is not a valid URL"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AgentError::config(
            "TIDEBREAK_VAULT_ADDR must not contain credentials, a query, or a fragment",
        ));
    }
    if url.host_str().is_none() {
        return Err(AgentError::config(
            "TIDEBREAK_VAULT_ADDR must contain a host",
        ));
    }
    let literal_loopback = url.host_str().is_some_and(|host| {
        host.trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
    });
    if url.scheme() != "https" && !(url.scheme() == "http" && literal_loopback) {
        return Err(AgentError::config(
            "TIDEBREAK_VAULT_ADDR must use https; http is allowed only for a literal loopback address",
        ));
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

fn validate_secret_path(name: &str, raw: &str) -> Result<Vec<String>> {
    let trimmed = raw.trim();
    let segments = trimmed.split('/').collect::<Vec<_>>();
    if trimmed.is_empty()
        || segments
            .iter()
            .any(|segment| segment.is_empty() || matches!(*segment, "." | ".."))
    {
        return Err(AgentError::config(format!(
            "{name} must contain non-empty path segments and no `.` or `..` segment"
        )));
    }
    Ok(segments.into_iter().map(str::to_owned).collect())
}

async fn read_bounded(response: reqwest::Response) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > RESPONSE_BODY_LIMIT as u64)
    {
        return Err(AgentError::Secret(
            "the Vault response exceeds the size limit".into(),
        ));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|_| AgentError::Secret("the Vault response could not be read".into()))?;
        if body.len().saturating_add(chunk.len()) > RESPONSE_BODY_LIMIT {
            return Err(AgentError::Secret(
                "the Vault response exceeds the size limit".into(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn require_success(operation: &str, status: StatusCode) -> Result<()> {
    if status.is_success() {
        return Ok(());
    }
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        return Err(AgentError::Secret(
            "Vault refused the configured token".into(),
        ));
    }
    Err(AgentError::Secret(format!(
        "the Vault credential {operation} failed with HTTP {}",
        status.as_u16()
    )))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::body::Bytes;
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, Response};
    use axum::response::IntoResponse as _;
    use axum::routing::get;
    use axum::Router;
    use serde_json::json;
    use tempfile::TempDir;
    use tokio::net::TcpListener;

    use super::*;

    #[derive(Clone, Copy)]
    enum ReadBehavior {
        Malformed,
        Oversized,
        Redirect,
    }

    #[derive(Default)]
    struct FixtureState {
        items: Mutex<HashMap<String, String>>,
        namespaces: Mutex<Vec<Option<String>>>,
        next_read: Mutex<Option<ReadBehavior>>,
        request_count: Mutex<usize>,
        required_token: Mutex<String>,
        tokens: Mutex<Vec<String>>,
    }

    impl FixtureState {
        fn authorization_refusal(&self, headers: &HeaderMap) -> Option<Response<axum::body::Body>> {
            *self.request_count.lock().unwrap() += 1;
            let token = headers
                .get(VAULT_TOKEN_HEADER)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            self.tokens.lock().unwrap().push(token.clone());
            self.namespaces.lock().unwrap().push(
                headers
                    .get(VAULT_NAMESPACE_HEADER)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned),
            );
            if token != *self.required_token.lock().unwrap() {
                return Some((StatusCode::FORBIDDEN, "denied").into_response());
            }
            None
        }
    }

    struct Fixture {
        address: String,
        handle: tokio::task::JoinHandle<()>,
        state: Arc<FixtureState>,
    }

    impl Fixture {
        async fn start(token: &str) -> Self {
            let state = Arc::new(FixtureState {
                required_token: Mutex::new(token.to_owned()),
                ..FixtureState::default()
            });
            let router = Router::new()
                .route("/v1/{*path}", get(read).post(write).delete(remove))
                .with_state(state.clone());
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = format!("http://{}", listener.local_addr().unwrap());
            let handle = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            Self {
                address,
                handle,
                state,
            }
        }

        fn require_token(&self, token: &str) {
            *self.state.required_token.lock().unwrap() = token.to_owned();
        }

        fn next_read(&self, behavior: ReadBehavior) {
            *self.state.next_read.lock().unwrap() = Some(behavior);
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.handle.abort();
        }
    }

    async fn read(
        State(state): State<Arc<FixtureState>>,
        Path(path): Path<String>,
        headers: HeaderMap,
    ) -> Response<axum::body::Body> {
        if let Some(response) = state.authorization_refusal(&headers) {
            return response;
        }
        match state.next_read.lock().unwrap().take() {
            Some(ReadBehavior::Malformed) => {
                return (StatusCode::OK, "{not-json").into_response();
            }
            Some(ReadBehavior::Oversized) => {
                return (StatusCode::OK, "x".repeat(RESPONSE_BODY_LIMIT + 1)).into_response();
            }
            Some(ReadBehavior::Redirect) => {
                return Response::builder()
                    .status(StatusCode::TEMPORARY_REDIRECT)
                    .header(reqwest::header::LOCATION, "/followed")
                    .body(axum::body::Body::empty())
                    .unwrap();
            }
            None => {}
        }
        match state.items.lock().unwrap().get(&path).cloned() {
            Some(value) => axum::Json(json!({
                "data": {
                    "data": { "value": value },
                    "metadata": { "version": 1 }
                }
            }))
            .into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }

    async fn write(
        State(state): State<Arc<FixtureState>>,
        Path(path): Path<String>,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response<axum::body::Body> {
        if let Some(response) = state.authorization_refusal(&headers) {
            return response;
        }
        let value = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|body| body.pointer("/data/value")?.as_str().map(str::to_owned));
        let Some(value) = value else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        state.items.lock().unwrap().insert(path, value);
        StatusCode::NO_CONTENT.into_response()
    }

    async fn remove(
        State(state): State<Arc<FixtureState>>,
        Path(path): Path<String>,
        headers: HeaderMap,
    ) -> Response<axum::body::Body> {
        if let Some(response) = state.authorization_refusal(&headers) {
            return response;
        }
        state.items.lock().unwrap().remove(&path);
        StatusCode::NO_CONTENT.into_response()
    }

    fn token_file(token: &str) -> (TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("vault-token");
        std::fs::write(&path, format!("{token}\n")).unwrap();
        (directory, path)
    }

    fn provider(
        fixture: &Fixture,
        token_file: PathBuf,
        namespace: Option<&str>,
    ) -> VaultSecretProvider {
        VaultSecretProvider::new(&VaultSecretConfig {
            address: fixture.address.clone(),
            token_file,
            mount: "secret".into(),
            path: "tidebreak/production".into(),
            namespace: namespace.map(str::to_owned),
        })
        .unwrap()
    }

    #[tokio::test]
    async fn missing_secret_returns_none() {
        let fixture = Fixture::start("token-one").await;
        let (_directory, token_file) = token_file("token-one");
        let provider = provider(&fixture, token_file, None);

        assert_eq!(provider.get_secret("missing").await.unwrap(), None);
    }

    #[tokio::test]
    async fn secret_can_be_created_and_overwritten() {
        let fixture = Fixture::start("token-one").await;
        let (_directory, token_file) = token_file("token-one");
        let provider = provider(&fixture, token_file, None);

        provider.set_secret("provider.test", "first").await.unwrap();
        assert!(fixture
            .state
            .items
            .lock()
            .unwrap()
            .contains_key("secret/data/tidebreak/production/provider.test"));
        assert_eq!(
            provider
                .get_secret("provider.test")
                .await
                .unwrap()
                .as_deref(),
            Some("first")
        );
        provider
            .set_secret("provider.test", "second")
            .await
            .unwrap();
        assert_eq!(
            provider
                .get_secret("provider.test")
                .await
                .unwrap()
                .as_deref(),
            Some("second")
        );
    }

    #[tokio::test]
    async fn secret_can_be_deleted() {
        let fixture = Fixture::start("token-one").await;
        let (_directory, token_file) = token_file("token-one");
        let provider = provider(&fixture, token_file, None);

        provider.set_secret("provider.test", "value").await.unwrap();
        provider.delete_secret("provider.test").await.unwrap();
        assert_eq!(provider.get_secret("provider.test").await.unwrap(), None);
        provider.delete_secret("provider.test").await.unwrap();
    }

    #[tokio::test]
    async fn authentication_failure_redacts_token_and_value() {
        let fixture = Fixture::start("expected-token").await;
        let (_directory, token_file) = token_file("wrong-token");
        let provider = provider(&fixture, token_file, None);

        let error = provider
            .set_secret("provider.test", "stored-secret-value")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("refused"));
        assert!(!error.contains("wrong-token"));
        assert!(!error.contains("stored-secret-value"));
    }

    #[tokio::test]
    async fn malformed_and_oversized_responses_are_rejected() {
        let fixture = Fixture::start("token-one").await;
        let (_directory, token_file) = token_file("token-one");
        let provider = provider(&fixture, token_file, None);

        fixture.next_read(ReadBehavior::Malformed);
        let malformed = provider
            .get_secret("provider.test")
            .await
            .unwrap_err()
            .to_string();
        assert!(malformed.contains("unreadable"));

        fixture.next_read(ReadBehavior::Oversized);
        let oversized = provider
            .get_secret("provider.test")
            .await
            .unwrap_err()
            .to_string();
        assert!(oversized.contains("size limit"));
    }

    #[tokio::test]
    async fn token_file_is_read_for_every_request() {
        let fixture = Fixture::start("token-one").await;
        let (_directory, token_file) = token_file("token-one");
        let provider = provider(&fixture, token_file.clone(), None);

        assert_eq!(provider.get_secret("missing").await.unwrap(), None);
        fixture.require_token("token-two");
        std::fs::write(&token_file, "token-two\n").unwrap();
        provider.set_secret("provider.test", "value").await.unwrap();

        assert_eq!(
            *fixture.state.tokens.lock().unwrap(),
            vec!["token-one".to_owned(), "token-two".to_owned()]
        );
    }

    #[tokio::test]
    async fn namespace_is_sent_on_each_request() {
        let fixture = Fixture::start("token-one").await;
        let (_directory, token_file) = token_file("token-one");
        let provider = provider(&fixture, token_file, Some("platform/team-a"));

        assert_eq!(provider.get_secret("missing").await.unwrap(), None);
        assert_eq!(
            *fixture.state.namespaces.lock().unwrap(),
            vec![Some("platform/team-a".to_owned())]
        );
    }

    #[tokio::test]
    async fn redirects_are_not_followed() {
        let fixture = Fixture::start("token-one").await;
        let (_directory, token_file) = token_file("token-one");
        let provider = provider(&fixture, token_file, None);
        fixture.next_read(ReadBehavior::Redirect);

        let error = provider
            .get_secret("provider.test")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("307"));
        assert_eq!(*fixture.state.request_count.lock().unwrap(), 1);
    }

    #[test]
    fn vault_address_validation_requires_https_or_literal_loopback() {
        let (_, token_file) = token_file("token-one");
        let config = |address: &str| VaultSecretConfig {
            address: address.into(),
            token_file: token_file.clone(),
            mount: "secret".into(),
            path: "tidebreak".into(),
            namespace: None,
        };

        assert!(VaultSecretProvider::new(&config("https://vault.example.test")).is_ok());
        assert!(VaultSecretProvider::new(&config("http://127.0.0.1:8200")).is_ok());
        assert!(VaultSecretProvider::new(&config("http://[::1]:8200")).is_ok());
        for address in [
            "http://localhost:8200",
            "http://vault.example.test",
            "https://name:secret@vault.example.test",
            "https://vault.example.test?token=secret",
            "https://vault.example.test/#fragment",
        ] {
            assert!(
                VaultSecretProvider::new(&config(address)).is_err(),
                "accepted {address}"
            );
        }
    }

    #[tokio::test]
    async fn unavailable_provider_allows_fallback_reads_but_rejects_changes() {
        let provider = UnavailableSelfHostSecretProvider;

        assert_eq!(provider.get_secret("provider.test").await.unwrap(), None);
        for error in [
            provider
                .set_secret("provider.test", "value")
                .await
                .unwrap_err(),
            provider.delete_secret("provider.test").await.unwrap_err(),
        ] {
            let message = error.to_string();
            assert!(message.contains("TIDEBREAK_VAULT_ADDR"));
            assert!(message.contains("TIDEBREAK_VAULT_TOKEN_FILE"));
        }
    }
}
