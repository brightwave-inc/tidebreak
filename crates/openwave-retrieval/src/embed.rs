//! The embedding seam: text in, vectors out.
//!
//! Real embedders (OpenAI, Cohere, local ONNX) are asymmetric — they encode a
//! document and a query differently (Cohere's `search_document` vs
//! `search_query` input types are the canonical example). The [`Embedder`] trait
//! bakes that split in from the start so wiring a real provider later doesn't
//! reshape the seam.
//!
//! [`HashEmbedder`] is the always-available, zero-dependency implementation: a
//! deterministic hashing-trick encoder. It needs no network and no model weights,
//! which makes it the offline default and the backbone of the test suite. It is
//! *not* semantic — it captures lexical overlap only — so production deployments
//! swap in a real provider behind this same trait.

use async_trait::async_trait;

use crate::error::Result;

/// A dense embedding vector.
#[derive(Debug, Clone, PartialEq)]
pub struct Embedding(pub Vec<f32>);

impl Embedding {
    /// The vector's dimensionality.
    #[must_use]
    pub fn dim(&self) -> usize {
        self.0.len()
    }

    /// Cosine similarity against another embedding, in `[-1, 1]`.
    ///
    /// Returns `0.0` if either vector has zero magnitude (e.g. an empty text) so
    /// callers never see a `NaN`. Vectors of differing length also yield `0.0`.
    #[must_use]
    pub fn cosine_similarity(&self, other: &Embedding) -> f32 {
        if self.0.len() != other.0.len() {
            return 0.0;
        }
        let mut dot = 0.0f32;
        let mut norm_a = 0.0f32;
        let mut norm_b = 0.0f32;
        for (a, b) in self.0.iter().zip(&other.0) {
            dot += a * b;
            norm_a += a * a;
            norm_b += b * b;
        }
        if norm_a == 0.0 || norm_b == 0.0 {
            return 0.0;
        }
        dot / (norm_a.sqrt() * norm_b.sqrt())
    }
}

/// Turns text into dense vectors.
///
/// Object-safe (`Box<dyn Embedder>` / `Arc<dyn Embedder>`) and async, matching
/// `openwave-core`'s `ModelProvider` shape — real providers make network calls.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// The dimensionality every vector this embedder returns will have.
    fn dimensions(&self) -> usize;

    /// Whether embedding is guaranteed to stay in-process without network or
    /// external-service access.
    ///
    /// Defaults to `false` so new provider-backed implementations fail closed at
    /// approval boundaries until they explicitly prove they are local.
    fn is_local(&self) -> bool {
        false
    }

    /// Stable identity for document-vector behavior used in index watermarks.
    ///
    /// Custom embedders should override this when two configurations with the
    /// same width can produce incompatible vectors. The value must remain stable
    /// for this embedder instance's lifetime; runtime reconfiguration requires a
    /// new instance.
    fn fingerprint(&self) -> String {
        format!(
            "custom-embedder:type={}:dimensions={}",
            std::any::type_name::<Self>(),
            self.dimensions()
        )
    }

    /// Embed a batch of documents for indexing.
    async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Embedding>>;

    /// Embed a single query for search.
    ///
    /// Defaults to routing through [`Embedder::embed_documents`]; asymmetric
    /// providers override this to use their query-side encoding.
    async fn embed_query(&self, text: &str) -> Result<Embedding> {
        let mut out = self
            .embed_documents(std::slice::from_ref(&text.to_string()))
            .await?;
        Ok(out
            .pop()
            .unwrap_or_else(|| Embedding(vec![0.0; self.dimensions()])))
    }
}

/// A deterministic, dependency-free embedder using the hashing trick.
///
/// Tokenizes on non-alphanumeric boundaries, folds each lowercased token into one
/// of `dims` buckets via FNV-1a (with a sign bit so collisions can cancel), then
/// L2-normalizes. Texts that share tokens get similar vectors; unrelated texts get
/// near-orthogonal ones. Deterministic across runs and platforms because the hash
/// is fixed — no reliance on the standard library's randomized hasher.
#[derive(Debug, Clone, Copy)]
pub struct HashEmbedder {
    dims: usize,
}

impl HashEmbedder {
    /// A reasonable default dimensionality for the hashing-trick encoder.
    pub const DEFAULT_DIMS: usize = 256;

    /// Build a hashing embedder with the given dimensionality (clamped to ≥ 1).
    #[must_use]
    pub fn new(dims: usize) -> Self {
        Self { dims: dims.max(1) }
    }

    fn embed_one(&self, text: &str) -> Embedding {
        let mut v = vec![0.0f32; self.dims];
        for token in text.split(|c: char| !c.is_alphanumeric()) {
            if token.is_empty() {
                continue;
            }
            let lower = token.to_lowercase();
            let h = fnv1a(lower.as_bytes());
            let bucket = (h % self.dims as u64) as usize;
            // Top bit picks the sign, so distinct tokens hashing to the same
            // bucket can cancel rather than always reinforce.
            let sign = if h & (1 << 63) == 0 { 1.0 } else { -1.0 };
            v[bucket] += sign;
        }
        l2_normalize(&mut v);
        Embedding(v)
    }
}

impl Default for HashEmbedder {
    fn default() -> Self {
        Self::new(Self::DEFAULT_DIMS)
    }
}

#[async_trait]
impl Embedder for HashEmbedder {
    fn dimensions(&self) -> usize {
        self.dims
    }

    fn is_local(&self) -> bool {
        true
    }

    fn fingerprint(&self) -> String {
        format!("hash-fnv1a-v1:{}d", self.dims)
    }

    async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Embedding>> {
        Ok(texts.iter().map(|t| self.embed_one(t)).collect())
    }

    async fn embed_query(&self, text: &str) -> Result<Embedding> {
        Ok(self.embed_one(text))
    }
}

/// FNV-1a, 64-bit. Fixed constants => deterministic everywhere.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Scale a vector to unit length in place. Leaves an all-zero vector untouched.
fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn embeddings_have_the_declared_dimensionality() {
        let e = HashEmbedder::new(64);
        assert_eq!(e.dimensions(), 64);
        let out = e
            .embed_documents(&["hello world".to_string()])
            .await
            .unwrap();
        assert_eq!(out[0].dim(), 64);
    }

    #[tokio::test]
    async fn embedding_is_deterministic() {
        let e = HashEmbedder::default();
        let a = e.embed_query("the quick brown fox").await.unwrap();
        let b = e.embed_query("the quick brown fox").await.unwrap();
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn shared_tokens_score_higher_than_unrelated_text() {
        let e = HashEmbedder::new(512);
        let query = e.embed_query("annual revenue growth").await.unwrap();

        let related = e
            .embed_documents(&["revenue growth accelerated this year".to_string()])
            .await
            .unwrap()[0]
            .clone();
        let unrelated = e
            .embed_documents(&["the weather in paris was mild".to_string()])
            .await
            .unwrap()[0]
            .clone();

        assert!(
            query.cosine_similarity(&related) > query.cosine_similarity(&unrelated),
            "related text should be nearer the query than unrelated text"
        );
    }

    #[tokio::test]
    async fn identical_text_is_maximally_similar() {
        let e = HashEmbedder::new(256);
        let a = e.embed_query("same words here").await.unwrap();
        let b = e.embed_query("same words here").await.unwrap();
        // Normalized identical vectors => cosine ~ 1.
        assert!((a.cosine_similarity(&b) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_is_zero_for_empty_or_mismatched_vectors() {
        let zero = Embedding(vec![0.0; 4]);
        let some = Embedding(vec![1.0, 0.0, 0.0, 0.0]);
        assert_eq!(zero.cosine_similarity(&some), 0.0);
        assert_eq!(some.cosine_similarity(&Embedding(vec![1.0, 0.0])), 0.0);
    }
}
