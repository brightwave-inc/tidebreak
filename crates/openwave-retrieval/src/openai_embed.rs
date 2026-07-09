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

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::embed::{Embedder, Embedding};
use crate::error::{Result, RetrievalError};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// An [`Embedder`] backed by an OpenAI-compatible `/embeddings` endpoint.
///
/// `dimensions` is sent as the request's `dimensions` parameter (supported by the
/// `text-embedding-3` family, which can project to a smaller size) and is the size
/// every returned vector is validated against — so it always matches what a
/// [`crate::VectorStore`] is configured to expect.
#[derive(Clone)]
pub struct OpenAiEmbedder {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
    dimensions: usize,
}

impl OpenAiEmbedder {
    /// Build an embedder hitting OpenAI's embeddings API with the given model and
    /// output dimensionality (e.g. `"text-embedding-3-small"`, `1536`).
    #[must_use]
    pub fn new(api_key: impl Into<String>, model: impl Into<String>, dimensions: usize) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            model: model.into(),
            dimensions,
        }
    }

    /// Point at a custom OpenAI-compatible gateway (its `/v1` root; `/embeddings`
    /// is appended).
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

#[async_trait]
impl Embedder for OpenAiEmbedder {
    fn dimensions(&self) -> usize {
        self.dimensions
    }

    async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Embedding>> {
        // Don't make a request for nothing — the API rejects an empty input.
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let url = format!("{}/embeddings", self.base_url.trim_end_matches('/'));
        let body = build_request_body(&self.model, texts, self.dimensions);

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
            // Never forward the raw body — a gateway may echo key material or
            // request fragments, and the error string can reach a client.
            return Err(RetrievalError::embed(format!(
                "embeddings request failed with HTTP {}",
                status.as_u16()
            )));
        }
        parse_response(&bytes, texts.len(), self.dimensions)
    }
}

/// Build the JSON request body for an embeddings call.
fn build_request_body(model: &str, texts: &[String], dimensions: usize) -> Value {
    json!({
        "model": model,
        "input": texts,
        "encoding_format": "float",
        "dimensions": dimensions,
    })
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
    fn request_body_has_the_expected_shape() {
        let texts = vec!["hello".to_string(), "world".to_string()];
        let body = build_request_body("text-embedding-3-small", &texts, 1536);
        assert_eq!(body["model"], "text-embedding-3-small");
        assert_eq!(body["encoding_format"], "float");
        assert_eq!(body["dimensions"], 1536);
        assert_eq!(body["input"], json!(["hello", "world"]));
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
}
