//! The vector-store seam: upsert embedded chunks, then query by nearest vector.
//!
//! [`VectorStore`] is the interface every backend implements. [`InMemoryVectorStore`]
//! is the reference backend — a brute-force cosine scan behind a lock. It has no
//! persistence and is O(n) per query, which is exactly right for tests, small
//! local corpora, and pinning down the semantics that persistent backends
//! (sqlite-vec, pgvector, Qdrant) must reproduce. Those, plus metadata filtering
//! and hybrid dense+sparse search, arrive as feature-gated backends behind this
//! trait later.

use std::sync::RwLock;

use async_trait::async_trait;

use crate::document::{Chunk, ScoredChunk};
use crate::embed::Embedding;
use crate::error::{Result, RetrievalError};

/// A chunk together with its embedding, ready to store.
#[derive(Debug, Clone)]
pub struct VectorRecord {
    /// The chunk being indexed.
    pub chunk: Chunk,
    /// Its embedding. Must match the store's dimensionality.
    pub embedding: Embedding,
}

/// Stores embedded chunks and retrieves them by vector similarity.
///
/// Object-safe and async so backends can do I/O. Implementations are held behind
/// `Arc<dyn VectorStore>`.
#[async_trait]
pub trait VectorStore: Send + Sync {
    /// Insert or replace records, keyed by chunk id.
    ///
    /// Re-upserting a chunk with the same id overwrites it, so re-ingesting a
    /// document (which yields the same derived chunk ids) is idempotent rather
    /// than duplicating vectors.
    async fn upsert(&self, records: Vec<VectorRecord>) -> Result<()>;

    /// Return the `k` chunks most similar to `query`, highest score first.
    ///
    /// Fewer than `k` come back when the store holds fewer records. `k == 0`
    /// yields an empty result.
    async fn query(&self, query: &Embedding, k: usize) -> Result<Vec<ScoredChunk>>;

    /// The number of records currently stored.
    async fn len(&self) -> Result<usize>;

    /// Whether the store holds no records.
    async fn is_empty(&self) -> Result<bool> {
        Ok(self.len().await? == 0)
    }
}

/// A brute-force, in-memory [`VectorStore`] using cosine similarity.
pub struct InMemoryVectorStore {
    dims: usize,
    records: RwLock<Vec<VectorRecord>>,
}

impl InMemoryVectorStore {
    /// Create an empty store that accepts vectors of exactly `dims` dimensions.
    #[must_use]
    pub fn new(dims: usize) -> Self {
        Self {
            dims,
            records: RwLock::new(Vec::new()),
        }
    }

    /// The dimensionality this store enforces.
    #[must_use]
    pub fn dimensions(&self) -> usize {
        self.dims
    }

    fn check_dims(&self, embedding: &Embedding) -> Result<()> {
        if embedding.dim() != self.dims {
            return Err(RetrievalError::DimensionMismatch {
                expected: self.dims,
                actual: embedding.dim(),
            });
        }
        Ok(())
    }
}

#[async_trait]
impl VectorStore for InMemoryVectorStore {
    async fn upsert(&self, records: Vec<VectorRecord>) -> Result<()> {
        for record in &records {
            self.check_dims(&record.embedding)?;
        }
        let mut store = self
            .records
            .write()
            .map_err(|_| RetrievalError::vector_store("in-memory store lock poisoned"))?;
        for record in records {
            if let Some(existing) = store.iter_mut().find(|r| r.chunk.id == record.chunk.id) {
                *existing = record;
            } else {
                store.push(record);
            }
        }
        Ok(())
    }

    async fn query(&self, query: &Embedding, k: usize) -> Result<Vec<ScoredChunk>> {
        self.check_dims(query)?;
        if k == 0 {
            return Ok(Vec::new());
        }
        let store = self
            .records
            .read()
            .map_err(|_| RetrievalError::vector_store("in-memory store lock poisoned"))?;

        let mut scored: Vec<ScoredChunk> = store
            .iter()
            .map(|r| ScoredChunk {
                chunk: r.chunk.clone(),
                score: query.cosine_similarity(&r.embedding),
            })
            .collect();

        // Highest score first. `total_cmp` gives a total order over f32 (no NaN
        // surprises); ties fall back to chunk id for a stable, reproducible order.
        scored.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.chunk.id.0.cmp(&b.chunk.id.0))
        });
        scored.truncate(k);
        Ok(scored)
    }

    async fn len(&self) -> Result<usize> {
        let store = self
            .records
            .read()
            .map_err(|_| RetrievalError::vector_store("in-memory store lock poisoned"))?;
        Ok(store.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::ByteSpan;
    use crate::id::DocumentId;

    fn record(doc: DocumentId, ordinal: usize, text: &str, vector: Vec<f32>) -> VectorRecord {
        let span = ByteSpan::new(ordinal * 100, ordinal * 100 + text.len());
        VectorRecord {
            chunk: Chunk::new(doc, ordinal, span, text),
            embedding: Embedding(vector),
        }
    }

    #[tokio::test]
    async fn rejects_wrong_dimensionality_on_upsert_and_query() {
        let store = InMemoryVectorStore::new(3);
        let doc = DocumentId::new();
        let err = store
            .upsert(vec![record(doc, 0, "a", vec![1.0, 0.0])])
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            RetrievalError::DimensionMismatch {
                expected: 3,
                actual: 2
            }
        ));
        assert!(store.query(&Embedding(vec![1.0, 0.0]), 5).await.is_err());
    }

    #[tokio::test]
    async fn returns_nearest_first_and_respects_k() {
        let store = InMemoryVectorStore::new(2);
        let doc = DocumentId::new();
        store
            .upsert(vec![
                record(doc, 0, "east", vec![1.0, 0.0]),
                record(doc, 1, "north", vec![0.0, 1.0]),
                record(doc, 2, "north-east", vec![1.0, 1.0]),
            ])
            .await
            .unwrap();

        // A query pointing east should rank "east" first, then the diagonal.
        let hits = store.query(&Embedding(vec![1.0, 0.0]), 2).await.unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].chunk.text, "east");
        assert_eq!(hits[1].chunk.text, "north-east");
        assert!(hits[0].score >= hits[1].score);
    }

    #[tokio::test]
    async fn k_zero_and_empty_store_return_nothing() {
        let store = InMemoryVectorStore::new(2);
        assert!(store.is_empty().await.unwrap());
        assert!(store
            .query(&Embedding(vec![1.0, 0.0]), 0)
            .await
            .unwrap()
            .is_empty());
        assert!(store
            .query(&Embedding(vec![1.0, 0.0]), 5)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn upsert_is_idempotent_by_chunk_id() {
        let store = InMemoryVectorStore::new(2);
        let doc = DocumentId::new();
        // Same document + span => same derived chunk id => replace, not append.
        store
            .upsert(vec![record(doc, 0, "hello", vec![1.0, 0.0])])
            .await
            .unwrap();
        store
            .upsert(vec![record(doc, 0, "hello", vec![0.0, 1.0])])
            .await
            .unwrap();
        assert_eq!(store.len().await.unwrap(), 1);
        // The second upsert's vector won.
        let hits = store.query(&Embedding(vec![0.0, 1.0]), 1).await.unwrap();
        assert!((hits[0].score - 1.0).abs() < 1e-6);
    }
}
