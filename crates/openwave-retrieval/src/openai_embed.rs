//! A real embedding provider: the OpenAI-compatible `/embeddings` endpoint.
//!
//! [`OpenAiEmbedder`] speaks the widely-supported OpenAI embeddings protocol, so
//! the same adapter covers OpenAI itself and any OpenAI-compatible gateway (point
//! `base_url` at its `/v1` root). It's the production counterpart to the offline
//! [`crate::HashEmbedder`]: same [`Embedder`] seam, real semantic vectors.
//!
//! The request/response handling is split into pure helpers ([`build_request_body`]
//! and [`parse_response`]) so the wire format is unit-tested without a network —
//! the same approach `openwave-router` takes for its provider adapters. The live
//! HTTP path isn't exercised in CI.
//!
//! Enabled by the `embed-openai` feature.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::embed::{Embedder, Embedding};
use crate::error::{Result, RetrievalError};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
/// Cap on a single embeddings request, so a hung call can't wedge ingest forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// An [`Embedder`] backed by an OpenAI-compatible `/embeddings` endpoint.
///
/// `dimensions` is the size every returned vector is validated against, so it
/// always matches what a [`crate::VectorStore`] expects — declare the model's
/// output size here. By default the request does **not** send a `dimensions`
/// parameter (many models and gateways `400` on it); call
/// [`OpenAiEmbedder::project_dimensions`] to ask a `text-embedding-3` model to
/// project its output down to `dimensions`.
///
/// So the two must agree: if `dimensions` differs from what the model actually
/// returns and you haven't called `project_dimensions`, every embed fails the
/// response's dimensionality check at call time (not at construction).
#[derive(Clone)]
pub struct OpenAiEmbedder {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
    dimensions: usize,
    /// Whether to send the `dimensions` request parameter (v3 projection).
    project_dimensions: bool,
}

impl OpenAiEmbedder {
    /// Build an embedder hitting OpenAI's embeddings API with the given model and
    /// its output dimensionality (e.g. `"text-embedding-3-small"`, `1536`).
    #[must_use]
    pub fn new(api_key: impl Into<String>, model: impl Into<String>, dimensions: usize) -> Self {
        // A build failure (e.g. TLS init) falls back to the default client rather
        // than panicking at construction; the timeout is best-effort.
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            model: model.into(),
            dimensions,
            project_dimensions: false,
        }
    }

    /// Point at a custom OpenAI-compatible gateway (its `/v1` root; `/embeddings`
    /// is appended).
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Send the `dimensions` request parameter, asking the model to project its
    /// output to the configured size. Only for `text-embedding-3` models — other
    /// models and many gateways reject the parameter. Off by default.
    #[must_use]
    pub fn project_dimensions(mut self) -> Self {
        self.project_dimensions = true;
        self
    }
}

#[async_trait]
impl Embedder for OpenAiEmbedder {
    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn fingerprint(&self) -> String {
        format!(
            "openai-compatible-v1:endpoint={}:model={}:dimensions={}:projection={}",
            endpoint_fingerprint(&self.base_url),
            self.model,
            self.dimensions,
            self.project_dimensions
        )
    }

    async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Embedding>> {
        // Don't make a request for nothing — the API rejects an empty input.
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let url = format!("{}/embeddings", self.base_url.trim_end_matches('/'));
        let sent_dimensions = self.project_dimensions.then_some(self.dimensions);
        let body = build_request_body(&self.model, texts, sent_dimensions);

        let response = self
            .client
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(embed_err)?;

        let status = response.status();
        let bytes = response.bytes().await.map_err(embed_err)?;
        if !status.is_success() {
            return Err(safe_http_error(status.as_u16(), &bytes));
        }
        parse_response(&bytes, texts.len(), self.dimensions)
    }
}

/// Collision-resistant identity of the endpoint's non-secret routing fields.
fn endpoint_fingerprint(base_url: &str) -> String {
    let canonical = reqwest::Url::parse(base_url).map_or_else(
        |_| {
            base_url
                .split(['?', '#'])
                .next()
                .unwrap_or_default()
                .to_string()
        },
        |mut url| {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        },
    );
    let digest = Sha256::digest(canonical.trim_end_matches('/').as_bytes());
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

/// Build the JSON request body for an embeddings call. `dimensions` is included
/// only when the caller opted into projection (v3 models); omitting it keeps the
/// request compatible with models and gateways that reject the parameter.
fn build_request_body(model: &str, texts: &[String], dimensions: Option<usize>) -> Value {
    let mut body = json!({
        "model": model,
        "input": texts,
        "encoding_format": "float",
    });
    if let Some(dimensions) = dimensions {
        body["dimensions"] = json!(dimensions);
    }
    body
}

/// Turn a non-2xx response into an error carrying the status plus the API's
/// structured `error.type`/`error.code` — never the free-text `message`, which a
/// gateway can echo request fragments or key material into.
fn safe_http_error(status: u16, body: &[u8]) -> RetrievalError {
    #[derive(Deserialize)]
    struct ErrorBody {
        error: ErrorDetail,
    }
    #[derive(Deserialize)]
    struct ErrorDetail {
        #[serde(default, rename = "type")]
        kind: Option<String>,
        #[serde(default)]
        code: Option<String>,
    }

    let detail = serde_json::from_slice::<ErrorBody>(body)
        .ok()
        .map(|b| b.error)
        .and_then(|e| e.code.or(e.kind));
    match detail {
        Some(detail) => RetrievalError::embed(format!(
            "embeddings request failed with HTTP {status} ({detail})"
        )),
        None => RetrievalError::embed(format!("embeddings request failed with HTTP {status}")),
    }
}

/// The subset of the embeddings response we consume.
#[derive(Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    index: usize,
    embedding: Vec<f32>,
}

/// Parse an embeddings response into vectors, one per input in order.
///
/// The API returns each vector tagged with its input `index`; this sorts by that
/// index (order isn't guaranteed), and validates that the count and each vector's
/// dimensionality match what was requested — so a malformed or truncated response
/// is an error rather than a silently-misaligned result.
fn parse_response(body: &[u8], expected: usize, dimensions: usize) -> Result<Vec<Embedding>> {
    let parsed: EmbeddingsResponse = serde_json::from_slice(body)
        .map_err(|e| RetrievalError::embed(format!("malformed embeddings response: {e}")))?;
    if parsed.data.len() != expected {
        return Err(RetrievalError::embed(format!(
            "expected {expected} embeddings, got {}",
            parsed.data.len()
        )));
    }
    let mut data = parsed.data;
    data.sort_by_key(|d| d.index);
    let mut out = Vec::with_capacity(expected);
    for (position, item) in data.into_iter().enumerate() {
        if item.index != position {
            return Err(RetrievalError::embed(format!(
                "embeddings response indices are not contiguous (saw {} at position {position})",
                item.index
            )));
        }
        if item.embedding.len() != dimensions {
            return Err(RetrievalError::embed(format!(
                "embedding {position} has {} dimensions, expected {dimensions}",
                item.embedding.len()
            )));
        }
        out.push(Embedding(item.embedding));
    }
    Ok(out)
}

fn embed_err(err: impl std::fmt::Display) -> RetrievalError {
    RetrievalError::embed(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_body_omits_dimensions_by_default() {
        let texts = vec!["hello".to_string(), "world".to_string()];
        let body = build_request_body("text-embedding-3-small", &texts, None);
        assert_eq!(body["model"], "text-embedding-3-small");
        assert_eq!(body["encoding_format"], "float");
        assert_eq!(body["input"], json!(["hello", "world"]));
        // Absent by default — many models/gateways 400 on an unexpected field.
        assert!(body.get("dimensions").is_none());
    }

    #[test]
    fn request_body_includes_dimensions_when_projecting() {
        let body = build_request_body("text-embedding-3-small", &["x".to_string()], Some(512));
        assert_eq!(body["dimensions"], 512);
    }

    #[test]
    fn safe_http_error_surfaces_code_not_free_text() {
        let body = json!({
            "error": {
                "message": "Incorrect API key sk-secret-leaked provided",
                "type": "invalid_request_error",
                "code": "invalid_api_key"
            }
        })
        .to_string();
        let err = safe_http_error(401, body.as_bytes()).to_string();
        assert!(err.contains("401"));
        assert!(err.contains("invalid_api_key"));
        // The free-text message (which echoed the key) must not leak through.
        assert!(!err.contains("sk-secret-leaked"));
    }

    #[test]
    fn safe_http_error_without_a_body_is_just_the_status() {
        let err = safe_http_error(500, b"gateway timeout, not json").to_string();
        assert!(err.contains("500"));
    }

    #[test]
    fn parses_and_orders_embeddings_by_index() {
        // Deliberately out of order: index 1 before index 0.
        let body = json!({
            "data": [
                { "index": 1, "embedding": [0.3, 0.4] },
                { "index": 0, "embedding": [0.1, 0.2] },
            ]
        })
        .to_string();
        let out = parse_response(body.as_bytes(), 2, 2).unwrap();
        assert_eq!(out[0], Embedding(vec![0.1, 0.2]));
        assert_eq!(out[1], Embedding(vec![0.3, 0.4]));
    }

    #[test]
    fn rejects_a_wrong_embedding_count() {
        let body = json!({ "data": [ { "index": 0, "embedding": [0.1, 0.2] } ] }).to_string();
        let err = parse_response(body.as_bytes(), 2, 2).unwrap_err();
        assert!(matches!(err, RetrievalError::Embed(_)));
    }

    #[test]
    fn rejects_a_wrong_dimensionality() {
        let body = json!({ "data": [ { "index": 0, "embedding": [0.1, 0.2, 0.3] } ] }).to_string();
        let err = parse_response(body.as_bytes(), 1, 2).unwrap_err();
        assert!(err.to_string().contains("expected 2"));
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(parse_response(b"not json", 1, 2).is_err());
    }

    #[tokio::test]
    async fn empty_input_makes_no_request_and_reports_configured_dims() {
        let embedder = OpenAiEmbedder::new("sk-test", "text-embedding-3-small", 1536)
            .with_base_url("http://127.0.0.1:1/v1");
        assert_eq!(embedder.dimensions(), 1536);
        // No network call for an empty batch, so this resolves without a server.
        assert!(embedder.embed_documents(&[]).await.unwrap().is_empty());
    }

    #[test]
    fn fingerprint_tracks_endpoint_without_persisting_it() {
        let default = OpenAiEmbedder::new("key", "model", 8).fingerprint();
        let custom = OpenAiEmbedder::new("key", "model", 8)
            .with_base_url("https://user:secret@example.test/v1?token=hidden")
            .fingerprint();
        let rotated_secret = OpenAiEmbedder::new("key", "model", 8)
            .with_base_url("https://other:rotated@example.test/v1?token=changed")
            .fingerprint();

        assert_ne!(default, custom);
        assert_eq!(custom, rotated_secret);
        assert!(!custom.contains("secret"));
        assert!(!custom.contains("hidden"));
        assert!(!custom.contains("example.test"));
    }
}
