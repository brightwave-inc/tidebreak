//! The vector-store seam: upsert embedded chunks, then query by nearest vector.
//!
//! [`VectorStore`] is the interface every backend implements. [`InMemoryVectorStore`]
//! is the reference backend — a brute-force cosine scan behind a lock. It has no
//! persistence and is O(n) per query, which is exactly right for tests, small
//! local corpora, and pinning down the semantics that persistent backends
//! (sqlite-vec, pgvector, Qdrant) must reproduce. Those, plus metadata filtering
//! and hybrid dense+sparse search, arrive as feature-gated backends behind this
//! trait later.

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;

use crate::document::{Chunk, ScoredChunk};
use crate::embed::Embedding;
use crate::error::{Result, RetrievalError};
use crate::id::DocumentId;
use openwave_core::DocumentGeneration;

/// A chunk together with its embedding, ready to store.
#[derive(Debug, Clone)]
pub struct VectorRecord {
    /// The chunk being indexed.
    pub chunk: Chunk,
    /// Its embedding. Must match the store's dimensionality.
    pub embedding: Embedding,
}

/// Result of conditionally staging one derived document generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationStageOutcome {
    /// The requested generation was durably staged but is not searchable yet.
    Staged,
    /// The exact generation was already staged or active.
    AlreadyPresent,
    /// A newer generation fence already exists, so this writer is stale.
    Rejected {
        /// Newest staged or active generation that fenced the request.
        current: DocumentGeneration,
    },
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
    /// than duplicating vectors. Implementations that support generation-aware
    /// publication reject this legacy path after a document has entered that
    /// protocol, because an unversioned write cannot preserve its generation
    /// fence.
    async fn upsert(&self, records: Vec<VectorRecord>) -> Result<()>;

    /// Return the `k` chunks most similar to `query`, highest score first.
    ///
    /// Fewer than `k` come back when the store holds fewer records. `k == 0`
    /// yields an empty result.
    async fn query(&self, query: &Embedding, k: usize) -> Result<Vec<ScoredChunk>>;

    /// Atomically replace every chunk of `document_id` with `records`: remove the
    /// document's existing chunks and store the new ones as one operation.
    ///
    /// This is what lets re-ingesting *changed* content be a true replacement. It
    /// must be atomic with respect to other store operations — two concurrent
    /// re-ingests of the same document must not interleave a partial delete and
    /// insert and leave a mix of versions. Passing an empty `records` clears the
    /// document. Every record must belong to `document_id`; implementations reject
    /// cross-document records and validate each embedding's dimensionality, as
    /// [`VectorStore::upsert`] does, before mutating the store. Like `upsert`,
    /// this legacy path is rejected once the document is generation-managed.
    async fn replace_document(
        &self,
        document_id: DocumentId,
        records: Vec<VectorRecord>,
    ) -> Result<()>;

    /// Stage a complete derived generation without making it searchable.
    ///
    /// Implementations retain a generation marker even when `records` is empty.
    /// Lower revisions are rejected, equal revisions require the exact token,
    /// and staging a newer generation fences activation of every older writer.
    async fn stage_document_generation(
        &self,
        _document_id: DocumentId,
        _generation: DocumentGeneration,
        _records: Vec<VectorRecord>,
    ) -> Result<GenerationStageOutcome> {
        Err(RetrievalError::vector_store(
            "generation-aware staging is not implemented by this vector store",
        ))
    }

    /// Atomically make the exact staged generation searchable.
    ///
    /// Returns `false` when the requested generation is neither the newest exact
    /// stage nor the already-active generation.
    async fn activate_document_generation(
        &self,
        _document_id: DocumentId,
        _generation: DocumentGeneration,
    ) -> Result<bool> {
        Err(RetrievalError::vector_store(
            "generation-aware activation is not implemented by this vector store",
        ))
    }

    /// Return the active searchable generation, including an empty tombstone.
    async fn active_document_generation(
        &self,
        _document_id: DocumentId,
    ) -> Result<Option<DocumentGeneration>> {
        Err(RetrievalError::vector_store(
            "generation-aware publication is not implemented by this vector store",
        ))
    }

    /// Count physically stored chunks for one document when the backend can do
    /// so efficiently.
    ///
    /// `None` means coverage cannot be verified; repair callers should then act
    /// conservatively rather than treating a catalog watermark as proof that
    /// derived rows still exist.
    async fn document_len(&self, _document_id: DocumentId) -> Result<Option<usize>> {
        Ok(None)
    }

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
    state: RwLock<InMemoryVectorState>,
}

#[derive(Default)]
struct InMemoryVectorState {
    unversioned_records: Vec<VectorRecord>,
    publications: HashMap<DocumentId, DocumentPublication>,
}

#[derive(Default)]
struct DocumentPublication {
    active: Option<GenerationRecords>,
    staged: Option<GenerationRecords>,
}

struct GenerationRecords {
    generation: DocumentGeneration,
    records: Vec<VectorRecord>,
}

impl InMemoryVectorStore {
    /// Create an empty store that accepts vectors of exactly `dims` dimensions.
    #[must_use]
    pub fn new(dims: usize) -> Self {
        Self {
            dims,
            state: RwLock::new(InMemoryVectorState::default()),
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

    fn validate_document_records(
        &self,
        document_id: DocumentId,
        records: &[VectorRecord],
    ) -> Result<()> {
        for record in records {
            self.check_dims(&record.embedding)?;
            if record.chunk.document_id != document_id {
                return Err(RetrievalError::vector_store(format!(
                    "replacement record {} belongs to document {}, expected {document_id}",
                    record.chunk.id, record.chunk.document_id
                )));
            }
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
            .state
            .write()
            .map_err(|_| RetrievalError::vector_store("in-memory store lock poisoned"))?;
        if records
            .iter()
            .any(|record| store.publications.contains_key(&record.chunk.document_id))
        {
            return Err(RetrievalError::vector_store(
                "legacy upsert cannot modify a generation-managed document",
            ));
        }
        for record in records {
            upsert_unversioned(&mut store.unversioned_records, record);
        }
        Ok(())
    }

    async fn query(&self, query: &Embedding, k: usize) -> Result<Vec<ScoredChunk>> {
        self.check_dims(query)?;
        if k == 0 {
            return Ok(Vec::new());
        }
        let store = self
            .state
            .read()
            .map_err(|_| RetrievalError::vector_store("in-memory store lock poisoned"))?;

        let visible = store.unversioned_records.iter().chain(
            store
                .publications
                .values()
                .filter_map(|publication| publication.active.as_ref())
                .flat_map(|active| active.records.iter()),
        );
        let mut scored: Vec<ScoredChunk> = visible
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

    async fn replace_document(
        &self,
        document_id: DocumentId,
        records: Vec<VectorRecord>,
    ) -> Result<()> {
        self.validate_document_records(document_id, &records)?;
        // Delete + insert under a single write lock, so a concurrent ingest can't
        // observe or race a half-applied replacement.
        let mut store = self
            .state
            .write()
            .map_err(|_| RetrievalError::vector_store("in-memory store lock poisoned"))?;
        if store.publications.contains_key(&document_id) {
            return Err(RetrievalError::vector_store(
                "legacy replacement cannot modify a generation-managed document",
            ));
        }
        store
            .unversioned_records
            .retain(|r| r.chunk.document_id != document_id);
        // Insert with the same by-chunk-id dedupe `upsert` uses, so the two write
        // paths give symmetric guarantees: if a chunker ever emits two records with
        // the same derived id (identical span), the last wins rather than both
        // landing and producing duplicate citations.
        for record in records {
            if let Some(existing) = store
                .unversioned_records
                .iter_mut()
                .find(|r| r.chunk.id == record.chunk.id)
            {
                *existing = record;
            } else {
                store.unversioned_records.push(record);
            }
        }
        Ok(())
    }

    async fn stage_document_generation(
        &self,
        document_id: DocumentId,
        generation: DocumentGeneration,
        records: Vec<VectorRecord>,
    ) -> Result<GenerationStageOutcome> {
        if generation.content_revision < 1 {
            return Err(RetrievalError::vector_store(
                "document generation revision must be at least one",
            ));
        }
        self.validate_document_records(document_id, &records)?;
        let records = dedupe_records(records);
        let mut state = self
            .state
            .write()
            .map_err(|_| RetrievalError::vector_store("in-memory store lock poisoned"))?;
        let publication = state.publications.entry(document_id).or_default();
        let newest = publication
            .active
            .as_ref()
            .into_iter()
            .chain(publication.staged.as_ref())
            .max_by_key(|records| records.generation.content_revision);
        if let Some(newest) = newest {
            match generation
                .content_revision
                .cmp(&newest.generation.content_revision)
            {
                std::cmp::Ordering::Less => {
                    return Ok(GenerationStageOutcome::Rejected {
                        current: newest.generation,
                    });
                }
                std::cmp::Ordering::Equal => {
                    if generation.revision_token != newest.generation.revision_token {
                        return Err(RetrievalError::vector_store(format!(
                            "document {document_id} generation {} has conflicting revision tokens",
                            generation.content_revision
                        )));
                    }
                    return Ok(GenerationStageOutcome::AlreadyPresent);
                }
                std::cmp::Ordering::Greater => {}
            }
        }
        publication.staged = Some(GenerationRecords {
            generation,
            records,
        });
        Ok(GenerationStageOutcome::Staged)
    }

    async fn activate_document_generation(
        &self,
        document_id: DocumentId,
        generation: DocumentGeneration,
    ) -> Result<bool> {
        let mut state = self
            .state
            .write()
            .map_err(|_| RetrievalError::vector_store("in-memory store lock poisoned"))?;
        let Some(publication) = state.publications.get_mut(&document_id) else {
            return Ok(false);
        };
        if let Some(staged) = publication.staged.as_ref() {
            if staged.generation != generation {
                return Ok(false);
            }
            let staged = publication
                .staged
                .take()
                .expect("the exact staged generation was just checked");
            publication.active = Some(staged);
            state
                .unversioned_records
                .retain(|record| record.chunk.document_id != document_id);
            return Ok(true);
        }
        Ok(publication
            .active
            .as_ref()
            .is_some_and(|active| active.generation == generation))
    }

    async fn active_document_generation(
        &self,
        document_id: DocumentId,
    ) -> Result<Option<DocumentGeneration>> {
        let state = self
            .state
            .read()
            .map_err(|_| RetrievalError::vector_store("in-memory store lock poisoned"))?;
        Ok(state
            .publications
            .get(&document_id)
            .and_then(|publication| publication.active.as_ref())
            .map(|active| active.generation))
    }

    async fn document_len(&self, document_id: DocumentId) -> Result<Option<usize>> {
        let store = self
            .state
            .read()
            .map_err(|_| RetrievalError::vector_store("in-memory store lock poisoned"))?;
        if let Some(active) = store
            .publications
            .get(&document_id)
            .and_then(|publication| publication.active.as_ref())
        {
            return Ok(Some(active.records.len()));
        }
        Ok(Some(
            store
                .unversioned_records
                .iter()
                .filter(|record| record.chunk.document_id == document_id)
                .count(),
        ))
    }

    async fn len(&self) -> Result<usize> {
        let store = self
            .state
            .read()
            .map_err(|_| RetrievalError::vector_store("in-memory store lock poisoned"))?;
        Ok(store.unversioned_records.len()
            + store
                .publications
                .values()
                .filter_map(|publication| publication.active.as_ref())
                .map(|active| active.records.len())
                .sum::<usize>())
    }
}

fn dedupe_records(records: Vec<VectorRecord>) -> Vec<VectorRecord> {
    let mut by_id = HashMap::new();
    let mut deduped = Vec::with_capacity(records.len());
    for record in records {
        if let Some(&index) = by_id.get(&record.chunk.id) {
            deduped[index] = record;
        } else {
            by_id.insert(record.chunk.id, deduped.len());
            deduped.push(record);
        }
    }
    deduped
}

fn upsert_unversioned(records: &mut Vec<VectorRecord>, record: VectorRecord) {
    if let Some(existing) = records
        .iter_mut()
        .find(|existing| existing.chunk.id == record.chunk.id)
    {
        *existing = record;
    } else {
        records.push(record);
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

    fn generation(revision: i64) -> DocumentGeneration {
        DocumentGeneration {
            content_revision: revision,
            revision_token: uuid::Uuid::from_u128(revision as u128),
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
        assert_eq!(store.document_len(doc).await.unwrap(), Some(1));
        assert_eq!(
            store.document_len(DocumentId::new()).await.unwrap(),
            Some(0)
        );
        // The second upsert's vector won.
        let hits = store.query(&Embedding(vec![0.0, 1.0]), 1).await.unwrap();
        assert!((hits[0].score - 1.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn replace_document_swaps_only_that_documents_chunks() {
        let store = InMemoryVectorStore::new(2);
        let a = DocumentId::new();
        let b = DocumentId::new();
        store
            .upsert(vec![
                record(a, 0, "a0", vec![1.0, 0.0]),
                record(a, 1, "a1", vec![0.0, 1.0]),
                record(b, 0, "b0", vec![1.0, 1.0]),
            ])
            .await
            .unwrap();

        // Replace document a's two chunks with a single new one.
        store
            .replace_document(a, vec![record(a, 0, "a-new", vec![1.0, 0.0])])
            .await
            .unwrap();
        assert_eq!(store.len().await.unwrap(), 2); // one a, one b
        let hits = store.query(&Embedding(vec![1.0, 0.0]), 5).await.unwrap();
        assert!(hits.iter().any(|h| h.chunk.text == "a-new"));
        assert!(
            !hits.iter().any(|h| h.chunk.text == "a1"),
            "old a chunk gone"
        );
        // Document b is untouched.
        assert!(hits.iter().any(|h| h.chunk.document_id == b));
    }

    #[tokio::test]
    async fn replace_document_dedupes_records_by_chunk_id() {
        // Two records with the same derived chunk id (same document + span) must
        // collapse to one — the last wins — not both land as duplicate citations.
        let store = InMemoryVectorStore::new(2);
        let doc = DocumentId::new();
        let first = record(doc, 0, "old", vec![1.0, 0.0]);
        let second = record(doc, 0, "new", vec![0.0, 1.0]);
        assert_eq!(first.chunk.id, second.chunk.id, "same span => same id");

        store
            .replace_document(doc, vec![first, second])
            .await
            .unwrap();
        assert_eq!(store.len().await.unwrap(), 1);
        let hits = store.query(&Embedding(vec![0.0, 1.0]), 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.text, "new");
    }

    #[tokio::test]
    async fn replace_document_with_no_records_clears_it() {
        let store = InMemoryVectorStore::new(2);
        let a = DocumentId::new();
        store
            .upsert(vec![record(a, 0, "a0", vec![1.0, 0.0])])
            .await
            .unwrap();
        store.replace_document(a, vec![]).await.unwrap();
        assert!(store.is_empty().await.unwrap());
        // Replacing an absent document with nothing is a no-op.
        store
            .replace_document(DocumentId::new(), vec![])
            .await
            .unwrap();
        assert!(store.is_empty().await.unwrap());
    }

    #[tokio::test]
    async fn replace_document_validates_dimensionality() {
        let store = InMemoryVectorStore::new(2);
        let a = DocumentId::new();
        let err = store
            .replace_document(a, vec![record(a, 0, "bad", vec![1.0, 0.0, 0.0])])
            .await
            .unwrap_err();
        assert!(matches!(err, RetrievalError::DimensionMismatch { .. }));
    }

    #[tokio::test]
    async fn replace_document_rejects_records_from_another_document() {
        let store = InMemoryVectorStore::new(2);
        let existing = DocumentId::new();
        let wrong = DocumentId::new();
        store
            .upsert(vec![record(existing, 0, "original", vec![1.0, 0.0])])
            .await
            .unwrap();

        let err = store
            .replace_document(existing, vec![record(wrong, 0, "wrong", vec![0.0, 1.0])])
            .await
            .unwrap_err();

        assert!(err.to_string().contains("belongs to document"));
        let hits = store.query(&Embedding(vec![1.0, 0.0]), 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.text, "original");
    }

    #[tokio::test]
    async fn staged_generation_is_invisible_and_newer_stage_fences_activation() {
        let store = InMemoryVectorStore::new(2);
        let doc = DocumentId::new();
        let first = generation(1);
        let third = generation(3);

        assert_eq!(
            store
                .stage_document_generation(
                    doc,
                    first,
                    vec![record(doc, 0, "first", vec![1.0, 0.0])],
                )
                .await
                .unwrap(),
            GenerationStageOutcome::Staged
        );
        assert!(store
            .query(&Embedding(vec![1.0, 0.0]), 10)
            .await
            .unwrap()
            .is_empty());
        assert!(store
            .activate_document_generation(doc, first)
            .await
            .unwrap());
        assert_eq!(
            store.active_document_generation(doc).await.unwrap(),
            Some(first)
        );

        assert_eq!(
            store
                .stage_document_generation(
                    doc,
                    third,
                    vec![record(doc, 0, "third", vec![0.0, 1.0])],
                )
                .await
                .unwrap(),
            GenerationStageOutcome::Staged
        );
        let before_activation = store.query(&Embedding(vec![1.0, 0.0]), 10).await.unwrap();
        assert_eq!(before_activation.len(), 1);
        assert_eq!(before_activation[0].chunk.text, "first");
        assert!(!store
            .activate_document_generation(doc, first)
            .await
            .unwrap());
        assert!(store
            .activate_document_generation(doc, third)
            .await
            .unwrap());
        assert!(store
            .activate_document_generation(doc, third)
            .await
            .unwrap());

        let after_activation = store.query(&Embedding(vec![0.0, 1.0]), 10).await.unwrap();
        assert_eq!(after_activation.len(), 1);
        assert_eq!(after_activation[0].chunk.text, "third");
        assert_eq!(
            store.active_document_generation(doc).await.unwrap(),
            Some(third)
        );
    }

    #[tokio::test]
    async fn staging_is_monotonic_idempotent_and_token_exact() {
        let store = InMemoryVectorStore::new(2);
        let doc = DocumentId::new();
        let second = generation(2);
        let third = generation(3);

        assert_eq!(
            store
                .stage_document_generation(doc, third, Vec::new())
                .await
                .unwrap(),
            GenerationStageOutcome::Staged
        );
        assert_eq!(
            store
                .stage_document_generation(doc, third, Vec::new())
                .await
                .unwrap(),
            GenerationStageOutcome::AlreadyPresent
        );
        assert_eq!(
            store
                .stage_document_generation(doc, second, Vec::new())
                .await
                .unwrap(),
            GenerationStageOutcome::Rejected { current: third }
        );
        let conflicting = DocumentGeneration {
            revision_token: uuid::Uuid::from_u128(300),
            ..third
        };
        assert!(store
            .stage_document_generation(doc, conflicting, Vec::new())
            .await
            .is_err());
        assert!(!store
            .activate_document_generation(doc, second)
            .await
            .unwrap());
        assert!(store
            .activate_document_generation(doc, third)
            .await
            .unwrap());
        assert_eq!(
            store
                .stage_document_generation(doc, second, Vec::new())
                .await
                .unwrap(),
            GenerationStageOutcome::Rejected { current: third }
        );
    }

    #[tokio::test]
    async fn empty_generation_clears_chunks_and_retains_the_fence() {
        let store = InMemoryVectorStore::new(2);
        let doc = DocumentId::new();
        let live = generation(1);
        let tombstone = generation(2);
        store
            .stage_document_generation(doc, live, vec![record(doc, 0, "live", vec![1.0, 0.0])])
            .await
            .unwrap();
        assert!(store.activate_document_generation(doc, live).await.unwrap());

        store
            .stage_document_generation(doc, tombstone, Vec::new())
            .await
            .unwrap();
        assert!(store
            .activate_document_generation(doc, tombstone)
            .await
            .unwrap());
        assert!(store.is_empty().await.unwrap());
        assert_eq!(store.document_len(doc).await.unwrap(), Some(0));
        assert_eq!(
            store.active_document_generation(doc).await.unwrap(),
            Some(tombstone)
        );
        assert_eq!(
            store
                .stage_document_generation(doc, live, Vec::new())
                .await
                .unwrap(),
            GenerationStageOutcome::Rejected { current: tombstone }
        );
    }

    #[tokio::test]
    async fn staging_validation_does_not_mutate_publication_state() {
        let store = InMemoryVectorStore::new(2);
        let doc = DocumentId::new();
        let wrong = DocumentId::new();
        assert!(store
            .stage_document_generation(
                doc,
                generation(1),
                vec![record(wrong, 0, "wrong document", vec![1.0, 0.0])],
            )
            .await
            .is_err());
        assert!(store
            .stage_document_generation(
                doc,
                generation(1),
                vec![record(doc, 0, "wrong dimensions", vec![1.0])],
            )
            .await
            .is_err());
        assert_eq!(store.active_document_generation(doc).await.unwrap(), None);
        assert!(!store
            .activate_document_generation(doc, generation(1))
            .await
            .unwrap());
        assert!(store.is_empty().await.unwrap());
    }

    #[tokio::test]
    async fn legacy_writes_cannot_erase_active_or_staged_generation_fences() {
        let store = InMemoryVectorStore::new(2);
        let doc = DocumentId::new();
        let first = generation(1);
        let second = generation(2);
        let tombstone = generation(3);
        store
            .stage_document_generation(doc, first, vec![record(doc, 0, "active", vec![1.0, 0.0])])
            .await
            .unwrap();
        assert!(store
            .activate_document_generation(doc, first)
            .await
            .unwrap());
        assert_eq!(
            store
                .stage_document_generation(doc, tombstone, Vec::new())
                .await
                .unwrap(),
            GenerationStageOutcome::Staged
        );

        assert!(store
            .upsert(vec![record(doc, 0, "legacy", vec![1.0, 1.0])])
            .await
            .is_err());
        assert!(store
            .replace_document(doc, vec![record(doc, 0, "legacy", vec![1.0, 1.0])])
            .await
            .is_err());
        let hits = store.query(&Embedding(vec![1.0, 0.0]), 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.text, "active");
        assert!(store
            .activate_document_generation(doc, tombstone)
            .await
            .unwrap());
        assert!(store.is_empty().await.unwrap());
        assert!(store.replace_document(doc, Vec::new()).await.is_err());
        assert_eq!(
            store
                .stage_document_generation(doc, second, Vec::new())
                .await
                .unwrap(),
            GenerationStageOutcome::Rejected { current: tombstone }
        );
    }
}
