//! The vector-store seam: upsert embedded chunks, then query by hybrid relevance.
//!
//! [`VectorStore`] is the interface every backend implements. [`InMemoryVectorStore`]
//! is the reference backend — a brute-force cosine scan behind a lock. It has no
//! persistence and is O(n) per query, which is exactly right for tests, small
//! local corpora, and pinning down the semantics that persistent backends
//! (sqlite-vec, pgvector, Qdrant) must reproduce.

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;

use crate::document::{Chunk, DocumentSource, ScoredChunk};
use crate::embed::Embedding;
use crate::error::{Result, RetrievalError};
use crate::id::DocumentId;
use openwave_core::{ChatId, DocumentGeneration, ProjectId};

/// Corpus boundary applied to every retrieval query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    /// Only legacy documents not assigned to a conversation or project.
    Unscoped,
    /// Only legacy documents assigned to this project.
    Project(ProjectId),
    /// Only documents owned by this conversation.
    Chat(ChatId),
}

/// Default semantic quality floor for dense retrieval candidates.
pub const DEFAULT_MIN_DENSE_SIMILARITY: f32 = 0.2;

/// Policy applied by vector backends to one search query.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchOptions {
    /// Corpus boundary for the query.
    pub scope: SearchScope,
    /// Inclusive cosine-similarity floor for the dense branch.
    pub min_dense_similarity: f32,
}

impl SearchOptions {
    /// Build the default search policy for a corpus.
    #[must_use]
    pub const fn new(scope: SearchScope) -> Self {
        Self {
            scope,
            min_dense_similarity: DEFAULT_MIN_DENSE_SIMILARITY,
        }
    }

    /// Override the inclusive dense similarity floor.
    #[must_use]
    pub const fn with_min_dense_similarity(mut self, min_dense_similarity: f32) -> Self {
        self.min_dense_similarity = min_dense_similarity;
        self
    }

    /// Validate that the dense cutoff is finite and within the supported range.
    pub fn validate(self) -> Result<()> {
        if !self.min_dense_similarity.is_finite()
            || !(0.0..=1.0).contains(&self.min_dense_similarity)
        {
            return Err(RetrievalError::vector_store(
                "minimum dense similarity must be finite and within [0, 1]",
            ));
        }
        Ok(())
    }
}

impl SearchScope {
    fn includes(self, chat_id: Option<ChatId>, project_id: Option<ProjectId>) -> bool {
        match self {
            Self::Unscoped => chat_id.is_none() && project_id.is_none(),
            Self::Project(expected) => chat_id.is_none() && project_id == Some(expected),
            Self::Chat(expected) => chat_id == Some(expected),
        }
    }
}

/// A chunk together with its embedding, ready to store.
#[derive(Debug, Clone)]
pub struct VectorRecord {
    /// Conversation that owns this vector for current product retrieval.
    pub chat_id: Option<ChatId>,
    /// Legacy project corpus this vector belongs to.
    pub project_id: Option<ProjectId>,
    /// Immutable source provenance captured with this indexed generation.
    pub source: DocumentSource,
    /// Exact generation owning this record, absent only on the legacy unversioned path.
    pub generation: Option<DocumentGeneration>,
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

/// Newest durable publication state for one document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentGenerationState {
    /// The generation is currently searchable.
    Active(DocumentGeneration),
    /// The generation is durably staged but still invisible to search.
    Staged(DocumentGeneration),
}

impl DocumentGenerationState {
    /// Exact generation carried by this state.
    #[must_use]
    pub fn generation(self) -> DocumentGeneration {
        match self {
            Self::Active(generation) | Self::Staged(generation) => generation,
        }
    }
}

/// Stores embedded chunks and retrieves them by fused lexical and dense relevance.
///
/// Object-safe and async so backends can do I/O. Implementations are held behind
/// `Arc<dyn VectorStore>`.
#[async_trait]
pub trait VectorStore: Send + Sync {
    /// Whether queries are guaranteed to stay in-process without network or
    /// external-service access.
    ///
    /// Defaults to `false` so new remote-capable backends fail closed at
    /// approval boundaries until they explicitly prove they are local.
    fn is_local(&self) -> bool {
        false
    }

    /// Insert or replace records, keyed by chunk id.
    ///
    /// Re-upserting a chunk with the same id overwrites it, so re-ingesting a
    /// document (which yields the same derived chunk ids) is idempotent rather
    /// than duplicating vectors. Implementations that support generation-aware
    /// publication reject this legacy path after a document has entered that
    /// protocol, because an unversioned write cannot preserve its generation
    /// fence.
    async fn upsert(&self, records: Vec<VectorRecord>) -> Result<()>;

    /// Return the `k` chunks most relevant using the default quality policy.
    ///
    /// Non-empty text participates in lexical ranking; empty text requests the
    /// dense-only mode.
    /// Fewer than `k` come back when the store holds fewer records. `k == 0`
    /// yields an empty result.
    async fn query(
        &self,
        query_text: &str,
        query: &Embedding,
        k: usize,
        scope: SearchScope,
    ) -> Result<Vec<ScoredChunk>> {
        self.query_with_options(query_text, query, k, SearchOptions::new(scope))
            .await
    }

    /// Return the `k` chunks most relevant under an explicit quality policy.
    ///
    /// Implementations must validate `options` before honoring `k == 0`, apply
    /// corpus scope before branch limits, and treat `min_dense_similarity` as an
    /// inclusive cosine-similarity cutoff on the dense branch. The cutoff is
    /// applied before fusion and must not remove lexical-only matches. Empty
    /// query text requests dense-only results with cosine-similarity scores.
    /// Fused-score ties among candidates delivered by the backend branches use
    /// chunk id as the deterministic final order. Native branch selection when
    /// exact scores tie at that branch's `k` boundary is backend-defined.
    async fn query_with_options(
        &self,
        query_text: &str,
        query: &Embedding,
        k: usize,
        options: SearchOptions,
    ) -> Result<Vec<ScoredChunk>>;

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

    /// Return the newest staged-or-active generation for cheap retry preflight.
    ///
    /// This read is advisory: callers must still use
    /// [`stage_document_generation`](Self::stage_document_generation) for the
    /// mutation-time compare-and-set that closes races after preflight.
    async fn newest_document_generation(
        &self,
        _document_id: DocumentId,
    ) -> Result<Option<DocumentGenerationState>> {
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

/// An in-memory [`VectorStore`] using BM25, cosine similarity, and RRF.
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

    fn validate_record(&self, record: &VectorRecord) -> Result<()> {
        self.check_dims(&record.embedding)?;
        if record.chat_id.is_some() && record.project_id.is_some() {
            return Err(RetrievalError::vector_store(
                "vector record cannot belong to both a conversation and a project",
            ));
        }
        record.source.validate()?;
        record.chunk.validate_source_regions()
    }

    fn validate_document_records(
        &self,
        document_id: DocumentId,
        records: &[VectorRecord],
    ) -> Result<Option<(Option<ChatId>, Option<ProjectId>)>> {
        let scope = records
            .first()
            .map(|record| (record.chat_id, record.project_id));
        for record in records {
            self.validate_record(record)?;
            if record.chunk.document_id != document_id {
                return Err(RetrievalError::vector_store(format!(
                    "replacement record {} belongs to document {}, expected {document_id}",
                    record.chunk.id, record.chunk.document_id
                )));
            }
            if Some((record.chat_id, record.project_id)) != scope {
                return Err(RetrievalError::vector_store(format!(
                    "records for document {document_id} span multiple document corpora"
                )));
            }
        }
        Ok(scope)
    }

    fn ensure_scope_unchanged<'a>(
        document_id: DocumentId,
        requested: Option<(Option<ChatId>, Option<ProjectId>)>,
        existing: impl Iterator<Item = &'a VectorRecord>,
    ) -> Result<()> {
        let Some(requested) = requested else {
            return Ok(());
        };
        if existing
            .filter(|record| record.chunk.document_id == document_id)
            .any(|record| (record.chat_id, record.project_id) != requested)
        {
            return Err(RetrievalError::vector_store(format!(
                "document {document_id} cannot move between document corpora while it has indexed records"
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl VectorStore for InMemoryVectorStore {
    fn is_local(&self) -> bool {
        true
    }

    async fn upsert(&self, records: Vec<VectorRecord>) -> Result<()> {
        if records.iter().any(|record| record.generation.is_some()) {
            return Err(RetrievalError::vector_store(
                "legacy upsert cannot accept generation-stamped records",
            ));
        }
        let mut scopes = HashMap::new();
        for record in &records {
            self.validate_record(record)?;
            match scopes.entry(record.chunk.document_id) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert((record.chat_id, record.project_id));
                }
                std::collections::hash_map::Entry::Occupied(entry)
                    if *entry.get() != (record.chat_id, record.project_id) =>
                {
                    return Err(RetrievalError::vector_store(format!(
                        "records for document {} span multiple document corpora",
                        record.chunk.document_id
                    )));
                }
                std::collections::hash_map::Entry::Occupied(_) => {}
            }
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
        for (document_id, scope) in scopes {
            Self::ensure_scope_unchanged(
                document_id,
                Some(scope),
                store.unversioned_records.iter(),
            )?;
        }
        for record in records {
            upsert_unversioned(&mut store.unversioned_records, record);
        }
        Ok(())
    }

    async fn query_with_options(
        &self,
        query_text: &str,
        query: &Embedding,
        k: usize,
        options: SearchOptions,
    ) -> Result<Vec<ScoredChunk>> {
        options.validate()?;
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
        let visible = visible
            .filter(|record| options.scope.includes(record.chat_id, record.project_id))
            .collect::<Vec<_>>();
        if query_text.trim().is_empty() {
            Ok(crate::hybrid::dense(
                &visible,
                query,
                k,
                options.min_dense_similarity,
            ))
        } else {
            Ok(crate::hybrid::rank(
                &visible,
                query_text,
                query,
                k,
                options.min_dense_similarity,
            ))
        }
    }

    async fn replace_document(
        &self,
        document_id: DocumentId,
        records: Vec<VectorRecord>,
    ) -> Result<()> {
        if records.iter().any(|record| record.generation.is_some()) {
            return Err(RetrievalError::vector_store(
                "legacy replacement cannot accept generation-stamped records",
            ));
        }
        let scope = self.validate_document_records(document_id, &records)?;
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
        Self::ensure_scope_unchanged(document_id, scope, store.unversioned_records.iter())?;
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
        mut records: Vec<VectorRecord>,
    ) -> Result<GenerationStageOutcome> {
        if generation.content_revision < 1 || generation.revision_token.is_nil() {
            return Err(RetrievalError::vector_store(
                "document generation must have a positive revision and non-nil token",
            ));
        }
        for record in &mut records {
            match record.generation {
                Some(found) if found != generation => {
                    return Err(RetrievalError::vector_store(
                        "vector record generation does not match its publication fence",
                    ));
                }
                _ => record.generation = Some(generation),
            }
        }
        let scope = self.validate_document_records(document_id, &records)?;
        let records = dedupe_records(records);
        let mut state = self
            .state
            .write()
            .map_err(|_| RetrievalError::vector_store("in-memory store lock poisoned"))?;
        if !records.is_empty() {
            if let Some(publication) = state.publications.get(&document_id) {
                let existing = publication
                    .active
                    .as_ref()
                    .into_iter()
                    .flat_map(|generation| generation.records.iter())
                    .chain(
                        publication
                            .staged
                            .as_ref()
                            .into_iter()
                            .flat_map(|generation| generation.records.iter()),
                    );
                Self::ensure_scope_unchanged(document_id, scope, existing)?;
            }
        }
        if !records.is_empty() {
            Self::ensure_scope_unchanged(document_id, scope, state.unversioned_records.iter())?;
        }
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
        if generation.content_revision < 1 || generation.revision_token.is_nil() {
            return Err(RetrievalError::vector_store(
                "document generation must have a positive revision and non-nil token",
            ));
        }
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

    async fn newest_document_generation(
        &self,
        document_id: DocumentId,
    ) -> Result<Option<DocumentGenerationState>> {
        let state = self
            .state
            .read()
            .map_err(|_| RetrievalError::vector_store("in-memory store lock poisoned"))?;
        let Some(publication) = state.publications.get(&document_id) else {
            return Ok(None);
        };
        match (publication.active.as_ref(), publication.staged.as_ref()) {
            (None, None) => Ok(None),
            (Some(active), None) => Ok(Some(DocumentGenerationState::Active(active.generation))),
            (None, Some(staged)) => Ok(Some(DocumentGenerationState::Staged(staged.generation))),
            (Some(active), Some(staged)) => {
                if active.generation.content_revision == staged.generation.content_revision
                    && active.generation.revision_token != staged.generation.revision_token
                {
                    return Err(RetrievalError::vector_store(format!(
                        "document {document_id} has conflicting generation markers at revision {}",
                        active.generation.content_revision
                    )));
                }
                if staged.generation.content_revision >= active.generation.content_revision {
                    Ok(Some(DocumentGenerationState::Staged(staged.generation)))
                } else {
                    Ok(Some(DocumentGenerationState::Active(active.generation)))
                }
            }
        }
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
    use crate::document::{ByteSpan, SourceLocation, SourceRegion};
    use crate::id::DocumentId;

    fn record(doc: DocumentId, ordinal: usize, text: &str, vector: Vec<f32>) -> VectorRecord {
        let span = ByteSpan::new(ordinal * 100, ordinal * 100 + text.len());
        VectorRecord {
            chat_id: None,
            project_id: None,
            source: DocumentSource::Inline,
            generation: None,
            chunk: Chunk::new(doc, ordinal, span, text),
            embedding: Embedding(vector),
        }
    }

    fn scoped_record(
        doc: DocumentId,
        project_id: ProjectId,
        ordinal: usize,
        text: &str,
        vector: Vec<f32>,
    ) -> VectorRecord {
        VectorRecord {
            chat_id: None,
            project_id: Some(project_id),
            ..record(doc, ordinal, text, vector)
        }
    }

    fn chat_record(doc: DocumentId, chat_id: ChatId, text: &str, vector: Vec<f32>) -> VectorRecord {
        VectorRecord {
            chat_id: Some(chat_id),
            project_id: None,
            ..record(doc, 0, text, vector)
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
        assert!(store
            .query("", &Embedding(vec![1.0, 0.0]), 5, SearchScope::Unscoped)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn rejects_malformed_chunk_source_regions_before_mutation() {
        let store = InMemoryVectorStore::new(2);
        let doc = DocumentId::new();
        let mut invalid = record(doc, 0, "é", vec![1.0, 0.0]);
        invalid.chunk.source_regions = vec![SourceRegion {
            span: ByteSpan::new(1, 2),
            location: SourceLocation::Page {
                number: std::num::NonZeroU32::new(1).unwrap(),
                bounds: None,
            },
        }];

        assert!(invalid.chunk.validate_source_regions().is_err());
        assert!(store.upsert(vec![invalid]).await.is_err());
        assert!(store.is_empty().await.unwrap());
    }

    #[tokio::test]
    async fn rejects_malformed_chunk_ids_and_nil_generations_before_mutation() {
        let store = InMemoryVectorStore::new(2);
        let doc = DocumentId::new();
        let mut invalid = record(doc, 0, "fact", vec![1.0, 0.0]);
        invalid.chunk.id = crate::ChunkId::new();
        assert!(store.upsert(vec![invalid.clone()]).await.is_err());
        assert!(store
            .stage_document_generation(doc, generation(1), vec![invalid])
            .await
            .is_err());
        let nil = DocumentGeneration {
            content_revision: 1,
            revision_token: uuid::Uuid::nil(),
        };
        assert!(store
            .stage_document_generation(doc, nil, vec![record(doc, 0, "fact", vec![1.0, 0.0])])
            .await
            .is_err());
        assert!(store.activate_document_generation(doc, nil).await.is_err());
        assert!(store
            .upsert(vec![record(doc, 0, "nul\0text", vec![1.0, 0.0])])
            .await
            .is_err());
        if usize::BITS > 63 {
            let start = (i64::MAX as usize) + 1;
            let mut outside = record(doc, 0, "fact", vec![1.0, 0.0]);
            outside.chunk = Chunk::new(doc, 0, ByteSpan::new(start, start + 4), "fact");
            assert!(store.upsert(vec![outside]).await.is_err());
        }
        assert!(store.is_empty().await.unwrap());
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
        let hits = store
            .query("", &Embedding(vec![1.0, 0.0]), 2, SearchScope::Unscoped)
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].chunk.text, "east");
        assert_eq!(hits[1].chunk.text, "north-east");
        assert!(hits[0].score >= hits[1].score);
    }

    #[tokio::test]
    async fn dense_cutoff_precedes_fusion_but_lexical_matches_are_rescued() {
        let store = InMemoryVectorStore::new(2);
        store
            .upsert(vec![
                record(DocumentId::new(), 0, "needle both branches", vec![1.0, 0.0]),
                record(
                    DocumentId::new(),
                    0,
                    "needle lexical rescue",
                    vec![0.0, 1.0],
                ),
                record(DocumentId::new(), 0, "dense only", vec![0.8, 0.6]),
                record(DocumentId::new(), 0, "below cutoff", vec![-1.0, 0.0]),
            ])
            .await
            .unwrap();

        let hybrid = store
            .query(
                "needle",
                &Embedding(vec![1.0, 0.0]),
                10,
                SearchScope::Unscoped,
            )
            .await
            .unwrap();
        let texts = hybrid
            .iter()
            .map(|hit| hit.chunk.text.as_str())
            .collect::<Vec<_>>();
        assert!(texts.contains(&"needle both branches"));
        assert!(texts.contains(&"needle lexical rescue"));
        assert!(texts.contains(&"dense only"));
        assert!(!texts.contains(&"below cutoff"));
        let lexical = hybrid
            .iter()
            .find(|hit| hit.chunk.text == "needle lexical rescue")
            .unwrap();
        assert!(lexical.score < DEFAULT_MIN_DENSE_SIMILARITY);

        let dense = store
            .query("", &Embedding(vec![1.0, 0.0]), 10, SearchScope::Unscoped)
            .await
            .unwrap();
        assert_eq!(
            dense
                .iter()
                .map(|hit| hit.chunk.text.as_str())
                .collect::<Vec<_>>(),
            vec!["needle both branches", "dense only"]
        );
        assert!(dense
            .iter()
            .all(|hit| hit.score >= DEFAULT_MIN_DENSE_SIMILARITY));
    }

    #[tokio::test]
    async fn dense_cutoff_is_inclusive_and_validated_before_empty_limit() {
        let store = InMemoryVectorStore::new(2);
        store
            .upsert(vec![record(
                DocumentId::new(),
                0,
                "exact boundary",
                vec![1.0, 0.0],
            )])
            .await
            .unwrap();
        let exact = store
            .query_with_options(
                "",
                &Embedding(vec![1.0, 0.0]),
                1,
                SearchOptions::new(SearchScope::Unscoped).with_min_dense_similarity(1.0),
            )
            .await
            .unwrap();
        assert_eq!(exact.len(), 1);

        for invalid in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
            let error = store
                .query_with_options(
                    "",
                    &Embedding(vec![1.0, 0.0]),
                    0,
                    SearchOptions::new(SearchScope::Unscoped).with_min_dense_similarity(invalid),
                )
                .await
                .unwrap_err();
            assert!(error.to_string().contains("within [0, 1]"));
        }
    }

    #[tokio::test]
    async fn query_filters_by_corpus_before_scoring_and_top_k() {
        let store = InMemoryVectorStore::new(2);
        let project_a = ProjectId::new();
        let project_b = ProjectId::new();
        store
            .upsert(vec![
                scoped_record(
                    DocumentId::new(),
                    project_a,
                    0,
                    "weaker right-project hit",
                    vec![0.8, 0.6],
                ),
                scoped_record(
                    DocumentId::new(),
                    project_b,
                    0,
                    "stronger wrong-project hit",
                    vec![1.0, 0.0],
                ),
                record(
                    DocumentId::new(),
                    0,
                    "stronger unscoped hit",
                    vec![1.0, 0.0],
                ),
            ])
            .await
            .unwrap();

        let project_hits = store
            .query(
                "",
                &Embedding(vec![1.0, 0.0]),
                1,
                SearchScope::Project(project_a),
            )
            .await
            .unwrap();
        assert_eq!(project_hits.len(), 1);
        assert_eq!(project_hits[0].chunk.text, "weaker right-project hit");

        let unscoped_hits = store
            .query("", &Embedding(vec![1.0, 0.0]), 1, SearchScope::Unscoped)
            .await
            .unwrap();
        assert_eq!(unscoped_hits.len(), 1);
        assert_eq!(unscoped_hits[0].chunk.text, "stronger unscoped hit");
    }

    #[tokio::test]
    async fn chat_scope_filters_before_top_k_and_excludes_legacy_corpora() {
        let store = InMemoryVectorStore::new(2);
        let first = ChatId::new();
        let second = ChatId::new();
        store
            .upsert(vec![
                chat_record(
                    DocumentId::new(),
                    first,
                    "weaker owning-chat hit",
                    vec![0.8, 0.6],
                ),
                chat_record(
                    DocumentId::new(),
                    second,
                    "stronger other-chat hit",
                    vec![1.0, 0.0],
                ),
                record(
                    DocumentId::new(),
                    0,
                    "stronger legacy-unscoped hit",
                    vec![1.0, 0.0],
                ),
            ])
            .await
            .unwrap();

        let hits = store
            .query("", &Embedding(vec![1.0, 0.0]), 1, SearchScope::Chat(first))
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.text, "weaker owning-chat hit");

        let mut double_scoped = chat_record(DocumentId::new(), first, "invalid", vec![1.0, 0.0]);
        double_scoped.project_id = Some(ProjectId::new());
        assert!(store
            .upsert(vec![double_scoped])
            .await
            .unwrap_err()
            .to_string()
            .contains("both a conversation and a project"));
    }

    #[tokio::test]
    async fn hybrid_query_fuses_lexical_and_dense_candidates_within_scope() {
        let store = InMemoryVectorStore::new(2);
        let project = ProjectId::new();
        store
            .upsert(vec![
                record(
                    DocumentId::new(),
                    0,
                    "needleshard exact identifier",
                    vec![0.0, 1.0],
                ),
                record(
                    DocumentId::new(),
                    0,
                    "semantic dense candidate",
                    vec![1.0, 0.0],
                ),
                scoped_record(
                    DocumentId::new(),
                    project,
                    0,
                    "needleshard wrong corpus",
                    vec![1.0, 0.0],
                ),
            ])
            .await
            .unwrap();

        let hits = store
            .query(
                "needleshard",
                &Embedding(vec![1.0, 0.0]),
                2,
                SearchScope::Unscoped,
            )
            .await
            .unwrap();

        assert_eq!(hits.len(), 2);
        assert!(hits
            .iter()
            .any(|hit| hit.chunk.text == "needleshard exact identifier"));
        assert!(hits
            .iter()
            .any(|hit| hit.chunk.text == "semantic dense candidate"));
        assert!(hits
            .iter()
            .all(|hit| hit.chunk.text != "needleshard wrong corpus"));
    }

    #[tokio::test]
    async fn hybrid_tie_at_k_boundary_uses_chunk_id() {
        let store = InMemoryVectorStore::new(2);
        let dense = record(
            DocumentId::derive("tie-dense"),
            0,
            "dense candidate",
            vec![1.0, 0.0],
        );
        let lexical = record(
            DocumentId::derive("tie-lexical"),
            0,
            "lexicalneedle candidate",
            vec![0.0, 1.0],
        );
        let expected = dense.chunk.id.0.min(lexical.chunk.id.0);
        store.upsert(vec![dense, lexical]).await.unwrap();

        let hits = store
            .query(
                "lexicalneedle",
                &Embedding(vec![1.0, 0.0]),
                1,
                SearchScope::Unscoped,
            )
            .await
            .unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.id.0, expected);
        assert!((hits[0].score - (1.0 / 61.0)).abs() < 1e-6);
    }

    #[tokio::test]
    async fn document_writes_reject_mixed_project_metadata() {
        let store = InMemoryVectorStore::new(2);
        let doc = DocumentId::new();
        let project_a = ProjectId::new();
        let project_b = ProjectId::new();
        let records = vec![
            scoped_record(doc, project_a, 0, "a", vec![1.0, 0.0]),
            scoped_record(doc, project_b, 1, "b", vec![0.0, 1.0]),
        ];

        let replacement_error = store
            .replace_document(doc, records.clone())
            .await
            .unwrap_err();
        assert!(replacement_error
            .to_string()
            .contains("multiple document corpora"));
        let stage_error = store
            .stage_document_generation(doc, generation(1), records)
            .await
            .unwrap_err();
        assert!(stage_error
            .to_string()
            .contains("multiple document corpora"));
        assert!(store.is_empty().await.unwrap());
    }

    #[tokio::test]
    async fn upsert_rejects_mixed_or_incrementally_changed_project_metadata_atomically() {
        let store = InMemoryVectorStore::new(2);
        let doc = DocumentId::new();
        let project_a = ProjectId::new();
        let project_b = ProjectId::new();

        let mixed_error = store
            .upsert(vec![
                scoped_record(doc, project_a, 0, "a", vec![1.0, 0.0]),
                scoped_record(doc, project_b, 1, "b", vec![0.0, 1.0]),
            ])
            .await
            .unwrap_err();
        assert!(mixed_error
            .to_string()
            .contains("multiple document corpora"));
        assert!(store.is_empty().await.unwrap());

        store
            .upsert(vec![scoped_record(
                doc,
                project_a,
                0,
                "original",
                vec![1.0, 0.0],
            )])
            .await
            .unwrap();
        let move_error = store
            .upsert(vec![scoped_record(
                doc,
                project_b,
                1,
                "must not land",
                vec![0.0, 1.0],
            )])
            .await
            .unwrap_err();
        assert!(move_error.to_string().contains("cannot move"));
        assert_eq!(store.len().await.unwrap(), 1);
        let hits = store
            .query(
                "",
                &Embedding(vec![1.0, 0.0]),
                10,
                SearchScope::Project(project_a),
            )
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.text, "original");
        assert!(store
            .query(
                "",
                &Embedding(vec![0.0, 1.0]),
                10,
                SearchScope::Project(project_b),
            )
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn replacement_allows_scope_change_only_after_clearing_live_records() {
        let store = InMemoryVectorStore::new(2);
        let doc = DocumentId::new();
        let project_a = ProjectId::new();
        let project_b = ProjectId::new();
        store
            .replace_document(
                doc,
                vec![scoped_record(
                    doc,
                    project_a,
                    0,
                    "project a",
                    vec![1.0, 0.0],
                )],
            )
            .await
            .unwrap();

        assert!(store
            .replace_document(
                doc,
                vec![scoped_record(
                    doc,
                    project_b,
                    0,
                    "blocked move",
                    vec![0.0, 1.0],
                )],
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("cannot move"));
        assert_eq!(store.len().await.unwrap(), 1);

        store.replace_document(doc, Vec::new()).await.unwrap();
        store
            .replace_document(
                doc,
                vec![scoped_record(
                    doc,
                    project_b,
                    0,
                    "project b",
                    vec![0.0, 1.0],
                )],
            )
            .await
            .unwrap();
        assert!(store
            .query(
                "",
                &Embedding(vec![0.0, 1.0]),
                1,
                SearchScope::Project(project_b),
            )
            .await
            .unwrap()[0]
            .chunk
            .text
            .contains("project b"));
    }

    #[tokio::test]
    async fn staging_allows_scope_change_only_after_an_active_empty_tombstone() {
        let store = InMemoryVectorStore::new(2);
        let doc = DocumentId::new();
        let project_a = ProjectId::new();
        let project_b = ProjectId::new();
        let first = generation(1);
        let tombstone = generation(2);
        let recreated = generation(3);
        store
            .stage_document_generation(
                doc,
                first,
                vec![scoped_record(
                    doc,
                    project_a,
                    0,
                    "project a",
                    vec![1.0, 0.0],
                )],
            )
            .await
            .unwrap();

        assert!(store
            .stage_document_generation(
                doc,
                tombstone,
                vec![scoped_record(
                    doc,
                    project_b,
                    0,
                    "blocked while staged",
                    vec![0.0, 1.0],
                )],
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("cannot move"));
        assert!(store
            .activate_document_generation(doc, first)
            .await
            .unwrap());
        assert!(store
            .stage_document_generation(
                doc,
                tombstone,
                vec![scoped_record(
                    doc,
                    project_b,
                    0,
                    "blocked while active",
                    vec![0.0, 1.0],
                )],
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("cannot move"));

        store
            .stage_document_generation(doc, tombstone, Vec::new())
            .await
            .unwrap();
        assert!(store
            .activate_document_generation(doc, tombstone)
            .await
            .unwrap());
        store
            .stage_document_generation(
                doc,
                recreated,
                vec![scoped_record(
                    doc,
                    project_b,
                    0,
                    "project b",
                    vec![0.0, 1.0],
                )],
            )
            .await
            .unwrap();
        assert!(store
            .activate_document_generation(doc, recreated)
            .await
            .unwrap());
        assert_eq!(
            store
                .query(
                    "",
                    &Embedding(vec![0.0, 1.0]),
                    1,
                    SearchScope::Project(project_b),
                )
                .await
                .unwrap()[0]
                .chunk
                .text,
            "project b"
        );
    }

    #[tokio::test]
    async fn k_zero_and_empty_store_return_nothing() {
        let store = InMemoryVectorStore::new(2);
        assert!(store.is_empty().await.unwrap());
        assert!(store
            .query("", &Embedding(vec![1.0, 0.0]), 0, SearchScope::Unscoped)
            .await
            .unwrap()
            .is_empty());
        assert!(store
            .query("", &Embedding(vec![1.0, 0.0]), 5, SearchScope::Unscoped)
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
        let hits = store
            .query("", &Embedding(vec![0.0, 1.0]), 1, SearchScope::Unscoped)
            .await
            .unwrap();
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
        let hits = store
            .query("", &Embedding(vec![1.0, 0.0]), 5, SearchScope::Unscoped)
            .await
            .unwrap();
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
        let hits = store
            .query("", &Embedding(vec![0.0, 1.0]), 5, SearchScope::Unscoped)
            .await
            .unwrap();
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
        let hits = store
            .query("", &Embedding(vec![1.0, 0.0]), 10, SearchScope::Unscoped)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.text, "original");
    }

    #[tokio::test]
    async fn staged_generation_is_invisible_and_newer_stage_fences_activation() {
        let store = InMemoryVectorStore::new(2);
        let doc = DocumentId::new();
        let first = generation(1);
        let third = generation(3);
        let mut first_record = record(doc, 0, "first", vec![1.0, 0.0]);
        first_record.source = DocumentSource::uri("file:///first.txt");
        let mut third_record = record(doc, 0, "third", vec![0.0, 1.0]);
        third_record.source = DocumentSource::uri("file:///third.txt");

        assert_eq!(
            store
                .stage_document_generation(doc, first, vec![first_record],)
                .await
                .unwrap(),
            GenerationStageOutcome::Staged
        );
        assert_eq!(
            store.newest_document_generation(doc).await.unwrap(),
            Some(DocumentGenerationState::Staged(first))
        );
        assert!(store
            .query("", &Embedding(vec![1.0, 0.0]), 10, SearchScope::Unscoped)
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
            store.newest_document_generation(doc).await.unwrap(),
            Some(DocumentGenerationState::Active(first))
        );

        assert_eq!(
            store
                .stage_document_generation(doc, third, vec![third_record],)
                .await
                .unwrap(),
            GenerationStageOutcome::Staged
        );
        let before_activation = store
            .query("", &Embedding(vec![1.0, 0.0]), 10, SearchScope::Unscoped)
            .await
            .unwrap();
        assert_eq!(before_activation.len(), 1);
        assert_eq!(before_activation[0].chunk.text, "first");
        assert_eq!(before_activation[0].generation, Some(first));
        assert_eq!(
            before_activation[0].source,
            DocumentSource::uri("file:///first.txt")
        );
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

        let after_activation = store
            .query("", &Embedding(vec![0.0, 1.0]), 10, SearchScope::Unscoped)
            .await
            .unwrap();
        assert_eq!(after_activation.len(), 1);
        assert_eq!(after_activation[0].chunk.text, "third");
        assert_eq!(after_activation[0].generation, Some(third));
        assert_eq!(
            after_activation[0].source,
            DocumentSource::uri("file:///third.txt")
        );
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
    async fn tombstone_allows_a_later_generation_to_change_corpus() {
        let store = InMemoryVectorStore::new(2);
        let doc = DocumentId::new();
        let project_a = ProjectId::new();
        let project_b = ProjectId::new();
        for (generation, records) in [
            (
                generation(1),
                vec![scoped_record(
                    doc,
                    project_a,
                    0,
                    "old corpus",
                    vec![1.0, 0.0],
                )],
            ),
            (generation(2), Vec::new()),
            (
                generation(3),
                vec![scoped_record(
                    doc,
                    project_b,
                    0,
                    "new corpus",
                    vec![0.0, 1.0],
                )],
            ),
        ] {
            assert_eq!(
                store
                    .stage_document_generation(doc, generation, records)
                    .await
                    .unwrap(),
                GenerationStageOutcome::Staged
            );
            assert!(store
                .activate_document_generation(doc, generation)
                .await
                .unwrap());
        }
        assert_eq!(
            store
                .query(
                    "",
                    &Embedding(vec![0.0, 1.0]),
                    5,
                    SearchScope::Project(project_a)
                )
                .await
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            store
                .query(
                    "",
                    &Embedding(vec![0.0, 1.0]),
                    5,
                    SearchScope::Project(project_b)
                )
                .await
                .unwrap()
                .len(),
            1
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
        let hits = store
            .query("", &Embedding(vec![1.0, 0.0]), 10, SearchScope::Unscoped)
            .await
            .unwrap();
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
