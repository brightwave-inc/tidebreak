//! The end-to-end pipeline: parse → chunk → embed → store, and query → cite.
//!
//! [`Retriever`] wires the retrieval seams together behind two methods,
//! [`Retriever::ingest`] and [`Retriever::search`]. It owns a parser and chunker,
//! shares an embedder and vector store, and can optionally rerank broad search
//! candidates before final result selection.

use std::sync::Arc;

use crate::chunk::Chunker;
use crate::document::{Citation, Document, DocumentSource};
use crate::embed::Embedder;
use crate::error::{Result, RetrievalError};
use crate::id::DocumentId;
use crate::parse::DocumentParser;
use crate::rerank::{rerank_candidates, Reranker};
use crate::selection::{candidate_limit, result_limit, select};
use crate::vector::{GenerationStageOutcome, SearchScope, VectorRecord, VectorStore};
use openwave_core::DocumentGeneration;

/// The outcome of ingesting one document.
#[derive(Debug, Clone)]
pub struct IngestOutcome {
    /// The parsed document, including its canonical text. Callers that persist a
    /// text-of-record (to rehydrate citation snippets from spans later) keep this.
    pub document: Document,
    /// How many chunks were embedded and stored.
    pub chunks: usize,
}

/// Outcome of preparing and conditionally staging one exact document generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationIndexOutcome {
    /// Number of newly prepared chunks, or `None` when preflight avoided work.
    pub chunks: Option<usize>,
    /// Whether the generation was staged, already present, or fenced as stale.
    pub stage: GenerationStageOutcome,
}

/// Ties parsing, chunking, embedding, and vector storage into one pipeline.
pub struct Retriever {
    parser: Box<dyn DocumentParser>,
    chunker: Box<dyn Chunker>,
    embedder: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
    reranker: Option<Arc<dyn Reranker>>,
}

impl Retriever {
    /// Assemble a retriever from its four seams.
    #[must_use]
    pub fn new(
        parser: Box<dyn DocumentParser>,
        chunker: Box<dyn Chunker>,
        embedder: Arc<dyn Embedder>,
        store: Arc<dyn VectorStore>,
    ) -> Self {
        Self {
            parser,
            chunker,
            embedder,
            store,
            reranker: None,
        }
    }

    /// Configure an optional model reranker for broad retrieval candidates.
    #[must_use]
    pub fn with_reranker(mut self, reranker: Arc<dyn Reranker>) -> Self {
        self.reranker = Some(reranker);
        self
    }

    /// Borrow the shared vector store (e.g. to hand a search tool the same index).
    #[must_use]
    pub fn store(&self) -> &Arc<dyn VectorStore> {
        &self.store
    }

    /// Stable identity for canonical parsing behavior.
    ///
    /// Canonical text cannot be regenerated from this identity alone; parser
    /// upgrades require retained original bytes.
    pub fn canonical_fingerprint_for(&self, media_type: &str) -> Result<String> {
        self.parser.fingerprint_for(media_type).ok_or_else(|| {
            RetrievalError::parse(format!(
                "no document parser supports media type `{media_type}`"
            ))
        })
    }

    /// Stable identity for the chunker and embedder configuration that produced
    /// this retriever's derived index rows.
    #[must_use]
    pub fn index_fingerprint(&self) -> String {
        format!(
            "chunker={};embedder={}",
            self.chunker.fingerprint(),
            self.embedder.fingerprint()
        )
    }

    /// Verify that the vector backend contains the expected number of chunks for
    /// this canonical document under the active chunker.
    pub async fn index_is_complete(&self, document: &Document) -> Result<bool> {
        validate_document_source_regions(document)?;
        let expected = self.chunker.chunk(document)?.len();
        Ok(self.store.document_len(document.id).await? == Some(expected))
    }

    /// Parse source bytes into the canonical document persisted by callers.
    ///
    /// URI sources receive a stable derived id; inline sources receive a fresh
    /// id for each parse.
    pub async fn parse_document(
        &self,
        source: DocumentSource,
        media_type: &str,
        raw: &[u8],
    ) -> Result<Document> {
        let parsed = self.parser.parse(raw, media_type).await?;
        openwave_core::validate_source_regions(&parsed.text, &parsed.source_regions)
            .map_err(RetrievalError::parse)?;
        let id = match &source {
            DocumentSource::Uri { uri } => DocumentId::derive(uri),
            _ => DocumentId::new(),
        };
        Ok(Document::with_id(id, source, media_type, parsed.text)
            .with_source_regions(parsed.source_regions))
    }

    /// Chunk, embed, and atomically replace one already-parsed document's index.
    ///
    /// Callers can persist `document` before awaiting external embedding I/O,
    /// then record an index watermark only after this succeeds.
    pub async fn index_document(&self, document: &Document) -> Result<usize> {
        let records = self.prepare_document_index(document).await?;
        let count = records.len();
        self.store.replace_document(document.id, records).await?;
        Ok(count)
    }

    /// Prepare and invisibly stage an exact authoritative document generation.
    ///
    /// Search continues to see the prior active generation until
    /// [`activate_document_generation`](Self::activate_document_generation)
    /// succeeds. Staging is safe to retry and may be rejected when a newer
    /// generation already fences this work.
    pub async fn stage_document_generation(
        &self,
        document: &Document,
        generation: DocumentGeneration,
    ) -> Result<GenerationIndexOutcome> {
        if let Some(current) = self.store.newest_document_generation(document.id).await? {
            let current = current.generation();
            match generation.content_revision.cmp(&current.content_revision) {
                std::cmp::Ordering::Less => {
                    return Ok(GenerationIndexOutcome {
                        chunks: None,
                        stage: GenerationStageOutcome::Rejected { current },
                    });
                }
                std::cmp::Ordering::Equal => {
                    if generation.revision_token != current.revision_token {
                        return Err(RetrievalError::vector_store(format!(
                            "document {} generation {} has conflicting revision tokens",
                            document.id, generation.content_revision
                        )));
                    }
                    return Ok(GenerationIndexOutcome {
                        chunks: None,
                        stage: GenerationStageOutcome::AlreadyPresent,
                    });
                }
                std::cmp::Ordering::Greater => {}
            }
        }
        let records = self.prepare_document_index(document).await?;
        let chunks = records.len();
        let stage = self
            .store
            .stage_document_generation(document.id, generation, records)
            .await?;
        Ok(GenerationIndexOutcome {
            chunks: Some(chunks),
            stage,
        })
    }

    /// Stage an empty generation marker used to retire a deleted document.
    pub async fn stage_document_tombstone(
        &self,
        document_id: DocumentId,
        generation: DocumentGeneration,
    ) -> Result<GenerationStageOutcome> {
        self.store
            .stage_document_generation(document_id, generation, Vec::new())
            .await
    }

    /// Atomically make one exact staged generation searchable.
    ///
    /// Empty staged generations remain durable fences while exposing no chunks.
    pub async fn activate_document_generation(
        &self,
        document_id: DocumentId,
        generation: DocumentGeneration,
    ) -> Result<bool> {
        self.store
            .activate_document_generation(document_id, generation)
            .await
    }

    async fn prepare_document_index(&self, document: &Document) -> Result<Vec<VectorRecord>> {
        validate_document_source_regions(document)?;
        let chunks = self.chunker.chunk(document)?;

        let records: Vec<VectorRecord> = if chunks.is_empty() {
            Vec::new()
        } else {
            let texts: Vec<String> = chunks
                .iter()
                .map(|chunk| chunk.retrieval_text().into_owned())
                .collect();
            let embeddings = self.embedder.embed_documents(&texts).await?;
            if embeddings.len() != chunks.len() {
                return Err(RetrievalError::embed(format!(
                    "embedder returned {} vectors for {} chunks",
                    embeddings.len(),
                    chunks.len()
                )));
            }
            chunks
                .into_iter()
                .zip(embeddings)
                .map(|(chunk, embedding)| VectorRecord {
                    project_id: document.project_id,
                    source: document.source.clone(),
                    generation: None,
                    chunk,
                    embedding,
                })
                .collect()
        };
        Ok(records)
    }

    /// Ingest one document: parse its bytes, chunk, embed, and store.
    ///
    /// A full, **atomic replace** for [`DocumentSource::Uri`] sources: the document
    /// id is derived from the URI, so re-ingesting the same URI targets the same
    /// document, and the store swaps its chunks in one operation (see
    /// [`VectorStore::replace_document`]). Re-ingesting identical content is
    /// idempotent; re-ingesting *changed* (even shorter, or now-empty) content
    /// leaves no stale chunks behind; and two concurrent re-ingests of the same URI
    /// resolve to one version rather than a mix. [`DocumentSource::Inline`] sources
    /// get a fresh id every call, so they never collide with a prior ingest. A
    /// document that chunks to nothing (empty or whitespace-only) stores nothing and
    /// reports zero chunks — after clearing any prior version.
    pub async fn ingest(
        &self,
        source: DocumentSource,
        media_type: &str,
        raw: &[u8],
    ) -> Result<IngestOutcome> {
        let document = self.parse_document(source, media_type, raw).await?;
        let count = self.index_document(&document).await?;

        Ok(IngestOutcome {
            document,
            chunks: count,
        })
    }

    /// Search the index for the `k` chunks most relevant to `query`, as citations.
    pub async fn search(&self, scope: SearchScope, query: &str, k: usize) -> Result<Vec<Citation>> {
        let k = result_limit(k);
        let embedding = self.embedder.embed_query(query).await?;
        let candidates = self
            .store
            .query(query, &embedding, candidate_limit(k), scope)
            .await?;
        let candidates = rerank_candidates(self.reranker.as_deref(), query, candidates).await?;
        let hits = select(candidates, k);
        Ok(hits.into_iter().map(Citation::from).collect())
    }

    /// Remove a document and all its chunks from the index. Idempotent — deleting a
    /// document that was never ingested (or already removed) is a no-op.
    pub async fn delete(&self, document_id: DocumentId) -> Result<()> {
        self.store.replace_document(document_id, Vec::new()).await
    }
}

fn validate_document_source_regions(document: &Document) -> Result<()> {
    openwave_core::validate_source_regions(&document.text, &document.source_regions)
        .map_err(RetrievalError::parse)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;

    use super::*;
    use crate::chunk::TextChunker;
    use crate::document::{ByteSpan, Chunk, ScoredChunk, SourceLocation, SourceRegion};
    use crate::embed::HashEmbedder;
    use crate::parse::{DocumentParser, ParsedDocument, PlainTextParser};
    use crate::vector::{InMemoryVectorStore, SearchOptions};

    struct ControlledEmbedder {
        inner: HashEmbedder,
        calls: AtomicUsize,
        fail: AtomicBool,
    }

    struct CapturingEmbedder {
        inner: HashEmbedder,
        documents: Mutex<Vec<Vec<String>>>,
    }

    struct QuerySpyVectorStore {
        candidates: Vec<ScoredChunk>,
        limits: Mutex<Vec<usize>>,
    }

    struct SpyReranker {
        scores: Vec<f32>,
        candidate_counts: Mutex<Vec<usize>>,
    }

    struct ContextObservingReranker {
        observed: Mutex<Vec<Vec<String>>>,
    }

    struct StaticParser(ParsedDocument);

    #[async_trait::async_trait]
    impl DocumentParser for StaticParser {
        fn fingerprint_for(&self, media_type: &str) -> Option<String> {
            self.supports(media_type).then(|| "static-parser-v1".into())
        }

        fn supports(&self, _media_type: &str) -> bool {
            true
        }

        async fn parse(&self, _raw: &[u8], _media_type: &str) -> Result<ParsedDocument> {
            Ok(self.0.clone())
        }
    }

    #[async_trait::async_trait]
    impl Reranker for SpyReranker {
        async fn rerank(&self, _query: &str, candidates: &[ScoredChunk]) -> Result<Vec<f32>> {
            self.candidate_counts.lock().unwrap().push(candidates.len());
            Ok(self.scores.clone())
        }
    }

    #[async_trait::async_trait]
    impl Reranker for ContextObservingReranker {
        async fn rerank(&self, _query: &str, candidates: &[ScoredChunk]) -> Result<Vec<f32>> {
            self.observed.lock().unwrap().push(
                candidates
                    .iter()
                    .map(|candidate| candidate.chunk.retrieval_text().into_owned())
                    .collect(),
            );
            Ok(vec![1.0; candidates.len()])
        }
    }

    #[async_trait::async_trait]
    impl VectorStore for QuerySpyVectorStore {
        async fn upsert(&self, _records: Vec<VectorRecord>) -> Result<()> {
            Ok(())
        }

        async fn query_with_options(
            &self,
            _query_text: &str,
            _query: &crate::Embedding,
            k: usize,
            _options: SearchOptions,
        ) -> Result<Vec<ScoredChunk>> {
            self.limits.lock().unwrap().push(k);
            Ok(self.candidates.iter().take(k).cloned().collect())
        }

        async fn replace_document(
            &self,
            _document_id: DocumentId,
            _records: Vec<VectorRecord>,
        ) -> Result<()> {
            Ok(())
        }

        async fn len(&self) -> Result<usize> {
            Ok(self.candidates.len())
        }
    }

    fn scored(
        document_id: DocumentId,
        ordinal: usize,
        start: usize,
        end: usize,
        text: &str,
    ) -> ScoredChunk {
        ScoredChunk {
            chunk: Chunk::new(document_id, ordinal, ByteSpan::new(start, end), text),
            source: DocumentSource::Inline,
            generation: None,
            score: 1.0 - ordinal as f32 / 10.0,
        }
    }

    #[async_trait::async_trait]
    impl Embedder for ControlledEmbedder {
        fn dimensions(&self) -> usize {
            self.inner.dimensions()
        }

        async fn embed_documents(&self, texts: &[String]) -> Result<Vec<crate::Embedding>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err(RetrievalError::embed("injected embedding failure"));
            }
            self.inner.embed_documents(texts).await
        }

        async fn embed_query(&self, text: &str) -> Result<crate::Embedding> {
            self.inner.embed_query(text).await
        }
    }

    #[async_trait::async_trait]
    impl Embedder for CapturingEmbedder {
        fn dimensions(&self) -> usize {
            self.inner.dimensions()
        }

        async fn embed_documents(&self, texts: &[String]) -> Result<Vec<crate::Embedding>> {
            self.documents.lock().unwrap().push(texts.to_vec());
            self.inner.embed_documents(texts).await
        }

        async fn embed_query(&self, text: &str) -> Result<crate::Embedding> {
            self.inner.embed_query(text).await
        }
    }

    fn retriever() -> Retriever {
        let dims = 512;
        // A 90-char window with newline-preferring cuts keeps each one-per-line
        // sentence in the test corpus a whole chunk.
        Retriever::new(
            Box::new(PlainTextParser::new()),
            Box::new(TextChunker::new(90, 0)),
            Arc::new(HashEmbedder::new(dims)),
            Arc::new(InMemoryVectorStore::new(dims)),
        )
    }

    #[tokio::test]
    async fn ingest_then_search_returns_grounded_citations() {
        let r = retriever();
        let text = "\
Mars is the fourth planet from the Sun and is often called the Red Planet.
Jupiter is the largest planet in the Solar System, a gas giant.
The Great Barrier Reef is the world's largest coral reef system.";

        let outcome = r
            .ingest(
                DocumentSource::uri("file:///space.txt"),
                "text/plain",
                text.as_bytes(),
            )
            .await
            .unwrap();
        assert!(outcome.chunks >= 3);

        let hits = r
            .search(
                SearchScope::Unscoped,
                "which planet is the largest gas giant",
                1,
            )
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        // The top citation should be the Jupiter sentence, and its span must slice
        // back to exactly the cited snippet in the original document.
        assert!(hits[0].snippet.contains("Jupiter"));
        assert_eq!(
            outcome.document.slice(hits[0].span),
            Some(hits[0].snippet.as_str())
        );
        assert_eq!(hits[0].document_id, outcome.document.id);
    }

    #[tokio::test]
    async fn markdown_heading_context_is_embedded_but_citations_stay_exact() {
        let embedder = Arc::new(CapturingEmbedder {
            inner: HashEmbedder::new(512),
            documents: Mutex::new(Vec::new()),
        });
        let retriever = Retriever::new(
            Box::new(PlainTextParser::new()),
            Box::new(TextChunker::new(10_000, 0)),
            embedder.clone(),
            Arc::new(InMemoryVectorStore::new(512)),
        );
        let document = Document::new(
            DocumentSource::Inline,
            "text/markdown",
            "# Operator Guide\n## Needleshard\ninstallation details",
        );

        retriever.index_document(&document).await.unwrap();
        let embedded = embedder.documents.lock().unwrap().clone();
        assert_eq!(embedded.len(), 1);
        assert_eq!(embedded[0].len(), 2);
        assert!(embedded[0][1].starts_with("Operator Guide > Needleshard\n\n"));

        let citations = retriever
            .search(SearchScope::Unscoped, "needleshard", 1)
            .await
            .unwrap();
        assert_eq!(citations[0].heading_path, ["Operator Guide", "Needleshard"]);
        assert_eq!(
            document.slice(citations[0].span),
            Some(citations[0].snippet.as_str())
        );
        assert!(!citations[0].snippet.starts_with("Operator Guide >"));
    }

    #[tokio::test]
    async fn search_requests_extra_candidates_and_backfills_after_suppression() {
        let first = DocumentId::new();
        let second = DocumentId::new();
        let store = Arc::new(QuerySpyVectorStore {
            candidates: vec![
                scored(first, 0, 0, 100, "primary"),
                scored(first, 1, 20, 80, "redundant"),
                scored(second, 2, 0, 20, "backfill"),
            ],
            limits: Mutex::new(Vec::new()),
        });
        let retriever = Retriever::new(
            Box::new(PlainTextParser::new()),
            Box::new(TextChunker::new(90, 0)),
            Arc::new(HashEmbedder::new(512)),
            store.clone(),
        );

        let citations = retriever
            .search(SearchScope::Unscoped, "relevant material", 2)
            .await
            .unwrap();

        assert_eq!(store.limits.lock().unwrap().as_slice(), &[8]);
        assert_eq!(citations.len(), 2);
        assert_eq!(citations[0].snippet, "primary");
        assert_eq!(citations[1].snippet, "backfill");
    }

    #[tokio::test]
    async fn reranks_every_overfetched_candidate_before_selection() {
        let candidates: Vec<_> = (0..8)
            .map(|ordinal| {
                scored(
                    DocumentId::new(),
                    ordinal,
                    ordinal * 10,
                    ordinal * 10 + 5,
                    &format!("candidate {ordinal}"),
                )
            })
            .collect();
        let store = Arc::new(QuerySpyVectorStore {
            candidates,
            limits: Mutex::new(Vec::new()),
        });
        let reranker = Arc::new(SpyReranker {
            scores: (0..8).map(|index| index as f32).collect(),
            candidate_counts: Mutex::new(Vec::new()),
        });
        let retriever = Retriever::new(
            Box::new(PlainTextParser::new()),
            Box::new(TextChunker::new(90, 0)),
            Arc::new(HashEmbedder::new(512)),
            store.clone(),
        )
        .with_reranker(reranker.clone());

        let citations = retriever
            .search(SearchScope::Unscoped, "query", 2)
            .await
            .unwrap();

        assert_eq!(store.limits.lock().unwrap().as_slice(), &[8]);
        assert_eq!(reranker.candidate_counts.lock().unwrap().as_slice(), &[8]);
        assert_eq!(citations[0].snippet, "candidate 7");
        assert_eq!(citations[0].score, 7.0);
        assert_eq!(citations[1].snippet, "candidate 6");
        assert_eq!(citations[1].score, 6.0);
    }

    #[tokio::test]
    async fn reranker_observes_the_exact_contextual_candidate_text() {
        let mut candidate = scored(DocumentId::new(), 0, 0, 4, "body");
        candidate.chunk.heading_path = vec!["Guide".into(), "Setup".into()];
        let store = Arc::new(QuerySpyVectorStore {
            candidates: vec![candidate],
            limits: Mutex::new(Vec::new()),
        });
        let reranker = Arc::new(ContextObservingReranker {
            observed: Mutex::new(Vec::new()),
        });
        let retriever = Retriever::new(
            Box::new(PlainTextParser::new()),
            Box::new(TextChunker::new(90, 0)),
            Arc::new(HashEmbedder::new(512)),
            store,
        )
        .with_reranker(reranker.clone());

        let citations = retriever
            .search(SearchScope::Unscoped, "query", 1)
            .await
            .unwrap();

        assert_eq!(
            reranker.observed.lock().unwrap().as_slice(),
            &[vec!["Guide > Setup\n\nbody".to_string()]]
        );
        assert_eq!(citations[0].snippet, "body");
        assert_eq!(citations[0].heading_path, ["Guide", "Setup"]);
    }

    #[tokio::test]
    async fn malformed_reranker_output_fails_search() {
        let store = Arc::new(QuerySpyVectorStore {
            candidates: vec![
                scored(DocumentId::new(), 0, 0, 5, "first"),
                scored(DocumentId::new(), 1, 10, 15, "second"),
            ],
            limits: Mutex::new(Vec::new()),
        });
        let retriever = Retriever::new(
            Box::new(PlainTextParser::new()),
            Box::new(TextChunker::new(90, 0)),
            Arc::new(HashEmbedder::new(512)),
            store,
        )
        .with_reranker(Arc::new(SpyReranker {
            scores: vec![1.0],
            candidate_counts: Mutex::new(Vec::new()),
        }));

        let error = retriever
            .search(SearchScope::Unscoped, "query", 2)
            .await
            .unwrap_err();
        assert!(matches!(error, RetrievalError::Rerank(_)));
    }

    #[tokio::test]
    async fn search_caps_large_output_and_candidate_requests() {
        let candidates: Vec<_> = (0..201)
            .map(|ordinal| {
                scored(
                    DocumentId::new(),
                    ordinal,
                    ordinal * 20,
                    ordinal * 20 + 10,
                    "candidate",
                )
            })
            .collect();
        let store = Arc::new(QuerySpyVectorStore {
            candidates,
            limits: Mutex::new(Vec::new()),
        });
        let retriever = Retriever::new(
            Box::new(PlainTextParser::new()),
            Box::new(TextChunker::new(90, 0)),
            Arc::new(HashEmbedder::new(512)),
            store.clone(),
        );

        let citations = retriever
            .search(SearchScope::Unscoped, "relevant material", usize::MAX)
            .await
            .unwrap();
        let empty = retriever
            .search(SearchScope::Unscoped, "relevant material", 0)
            .await
            .unwrap();

        assert_eq!(store.limits.lock().unwrap().as_slice(), &[80, 0]);
        assert_eq!(citations.len(), crate::MAX_SEARCH_RESULTS);
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn parse_and_index_can_be_orchestrated_separately() {
        let r = retriever();
        let document = r
            .parse_document(
                DocumentSource::uri("file:///separate.txt"),
                "text/plain",
                b"persist this before embedding",
            )
            .await
            .unwrap();
        assert_eq!(document.id, DocumentId::derive("file:///separate.txt"));
        assert_eq!(document.text, "persist this before embedding");

        assert_eq!(r.index_document(&document).await.unwrap(), 1);
        let hits = r
            .search(SearchScope::Unscoped, "persist embedding", 1)
            .await
            .unwrap();
        assert_eq!(hits[0].document_id, document.id);
    }

    #[tokio::test]
    async fn parser_source_regions_are_validated_and_attached() {
        let parsed = ParsedDocument::from_text("page one\npage two").with_source_regions(vec![
            SourceRegion {
                span: ByteSpan::new(0, 8),
                location: SourceLocation::Page {
                    number: std::num::NonZeroU32::new(1).unwrap(),
                },
            },
            SourceRegion {
                span: ByteSpan::new(9, 17),
                location: SourceLocation::Page {
                    number: std::num::NonZeroU32::new(2).unwrap(),
                },
            },
        ]);
        let dims = 8;
        let retriever = Retriever::new(
            Box::new(StaticParser(parsed.clone())),
            Box::new(TextChunker::default()),
            Arc::new(HashEmbedder::new(dims)),
            Arc::new(InMemoryVectorStore::new(dims)),
        );
        let document = retriever
            .parse_document(DocumentSource::Inline, "application/pdf", b"ignored")
            .await
            .unwrap();
        assert_eq!(document.source_regions, parsed.source_regions);

        let invalid = ParsedDocument::from_text("é").with_source_regions(vec![SourceRegion {
            span: ByteSpan::new(1, 2),
            location: SourceLocation::Page {
                number: std::num::NonZeroU32::new(1).unwrap(),
            },
        }]);
        let retriever = Retriever::new(
            Box::new(StaticParser(invalid)),
            Box::new(TextChunker::default()),
            Arc::new(HashEmbedder::new(dims)),
            Arc::new(InMemoryVectorStore::new(dims)),
        );
        assert!(matches!(
            retriever
                .parse_document(DocumentSource::Inline, "application/pdf", b"ignored")
                .await,
            Err(RetrievalError::Parse(_))
        ));
    }

    #[tokio::test]
    async fn indexing_preserves_the_documents_project_scope() {
        let r = retriever();
        let project_id = openwave_core::ProjectId::new();
        let document = Document::new_scoped(
            project_id,
            DocumentSource::Inline,
            "text/plain",
            "project-only retrieval content",
        );
        assert_eq!(r.index_document(&document).await.unwrap(), 1);

        assert!(r
            .search(SearchScope::Unscoped, "retrieval content", 1)
            .await
            .unwrap()
            .is_empty());
        let hits = r
            .search(SearchScope::Project(project_id), "retrieval content", 1)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].document_id, document.id);
    }

    #[tokio::test]
    async fn generation_indexing_stages_activates_and_retires_atomically() {
        let r = retriever();
        let document = Document::new(
            DocumentSource::Inline,
            "text/plain",
            "generation-aware indexing",
        );
        let first = DocumentGeneration {
            content_revision: 1,
            revision_token: uuid::Uuid::from_u128(1),
        };
        let tombstone = DocumentGeneration {
            content_revision: 2,
            revision_token: uuid::Uuid::from_u128(2),
        };

        let staged = r.stage_document_generation(&document, first).await.unwrap();
        assert_eq!(staged.chunks, Some(1));
        assert_eq!(staged.stage, GenerationStageOutcome::Staged);
        assert!(r
            .search(SearchScope::Unscoped, "indexing", 5)
            .await
            .unwrap()
            .is_empty());
        assert!(r
            .activate_document_generation(document.id, first)
            .await
            .unwrap());
        assert_eq!(
            r.search(SearchScope::Unscoped, "indexing", 5)
                .await
                .unwrap()
                .len(),
            1
        );

        assert_eq!(
            r.stage_document_tombstone(document.id, tombstone)
                .await
                .unwrap(),
            GenerationStageOutcome::Staged
        );
        assert_eq!(
            r.search(SearchScope::Unscoped, "indexing", 5)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(r
            .activate_document_generation(document.id, tombstone)
            .await
            .unwrap());
        assert!(r
            .search(SearchScope::Unscoped, "indexing", 5)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            r.stage_document_generation(&document, first)
                .await
                .unwrap()
                .stage,
            GenerationStageOutcome::Rejected { current: tombstone }
        );
    }

    #[tokio::test]
    async fn generation_preflight_skips_embedding_for_resolved_or_fenced_work() {
        let embedder = Arc::new(ControlledEmbedder {
            inner: HashEmbedder::new(32),
            calls: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
        });
        let r = Retriever::new(
            Box::new(PlainTextParser::new()),
            Box::new(TextChunker::new(90, 0)),
            embedder.clone(),
            Arc::new(InMemoryVectorStore::new(32)),
        );
        let document = Document::new(DocumentSource::Inline, "text/plain", "preflight once");
        let current = DocumentGeneration {
            content_revision: 2,
            revision_token: uuid::Uuid::from_u128(2),
        };
        let stale = DocumentGeneration {
            content_revision: 1,
            revision_token: uuid::Uuid::from_u128(1),
        };
        r.stage_document_generation(&document, current)
            .await
            .unwrap();
        assert_eq!(embedder.calls.load(Ordering::SeqCst), 1);
        embedder.fail.store(true, Ordering::SeqCst);

        let exact_staged = r
            .stage_document_generation(&document, current)
            .await
            .unwrap();
        assert_eq!(exact_staged.chunks, None);
        assert_eq!(exact_staged.stage, GenerationStageOutcome::AlreadyPresent);
        assert_eq!(embedder.calls.load(Ordering::SeqCst), 1);

        assert!(r
            .activate_document_generation(document.id, current)
            .await
            .unwrap());
        let exact_active = r
            .stage_document_generation(&document, current)
            .await
            .unwrap();
        assert_eq!(exact_active.stage, GenerationStageOutcome::AlreadyPresent);
        assert_eq!(
            r.stage_document_generation(&document, stale)
                .await
                .unwrap()
                .stage,
            GenerationStageOutcome::Rejected { current }
        );
        let conflicting = DocumentGeneration {
            revision_token: uuid::Uuid::from_u128(200),
            ..current
        };
        assert!(r
            .stage_document_generation(&document, conflicting)
            .await
            .is_err());
        assert_eq!(embedder.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn index_fingerprint_captures_the_pipeline_configuration() {
        let r = retriever();
        assert_eq!(
            r.canonical_fingerprint_for("text/plain").unwrap(),
            "plain-text-lossy-v1"
        );
        assert!(r.canonical_fingerprint_for("application/pdf").is_err());
        assert_eq!(
            r.index_fingerprint(),
            "chunker=text-window-v3:markdown=atx-heading-context:max_chars=90:overlap=0;embedder=hash-fnv1a-v1:512d"
        );
    }

    #[tokio::test]
    async fn empty_document_ingests_no_chunks() {
        let r = retriever();
        let outcome = r
            .ingest(DocumentSource::Inline, "text/plain", b"   \n  ")
            .await
            .unwrap();
        assert_eq!(outcome.chunks, 0);
        assert!(r.store().is_empty().await.unwrap());
        assert!(r
            .search(SearchScope::Unscoped, "anything", 5)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn re_ingesting_same_document_id_is_idempotent() {
        let r = retriever();
        let doc = Document::new(
            DocumentSource::Inline,
            "text/plain",
            "alpha beta gamma delta epsilon",
        );
        let chunks = TextChunker::new(60, 12).chunk(&doc).unwrap();
        let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
        let embeddings = HashEmbedder::new(512)
            .embed_documents(&texts)
            .await
            .unwrap();
        let records: Vec<VectorRecord> = chunks
            .into_iter()
            .zip(embeddings)
            .map(|(chunk, embedding)| VectorRecord {
                project_id: None,
                source: doc.source.clone(),
                generation: None,
                chunk,
                embedding,
            })
            .collect();

        // Upsert the same derived-id records twice; the store must not grow.
        r.store().upsert(records.clone()).await.unwrap();
        let after_first = r.store().len().await.unwrap();
        r.store().upsert(records).await.unwrap();
        assert_eq!(r.store().len().await.unwrap(), after_first);
    }

    #[tokio::test]
    async fn re_ingesting_same_uri_is_idempotent_end_to_end() {
        let r = retriever();
        let text = "one two three four five six seven eight nine ten eleven twelve";
        let uri = || DocumentSource::uri("file:///notes.txt");

        let first = r
            .ingest(uri(), "text/plain", text.as_bytes())
            .await
            .unwrap();
        assert!(first.chunks > 0);
        let after_first = r.store().len().await.unwrap();

        // Same URI + same bytes => same document id => same chunk ids => the
        // pipeline replaces in place rather than duplicating.
        let second = r
            .ingest(uri(), "text/plain", text.as_bytes())
            .await
            .unwrap();
        assert_eq!(second.document.id, first.document.id);
        assert_eq!(r.store().len().await.unwrap(), after_first);

        // And a single search returns each chunk once, not doubled.
        let hits = r
            .search(SearchScope::Unscoped, "three four five", 10)
            .await
            .unwrap();
        let unique: std::collections::HashSet<_> = hits.iter().map(|h| h.chunk_id).collect();
        assert_eq!(unique.len(), hits.len());
    }

    #[tokio::test]
    async fn re_ingesting_shorter_content_prunes_stale_chunks() {
        let r = retriever();
        let uri = || DocumentSource::uri("file:///doc.txt");
        // Long enough (and multi-line) to split into several chunks.
        let long = "alpha alpha alpha alpha alpha alpha alpha\n\
                    beta beta beta beta beta beta beta\n\
                    gamma gamma gamma gamma gamma gamma";
        let first = r
            .ingest(uri(), "text/plain", long.as_bytes())
            .await
            .unwrap();
        assert!(first.chunks >= 2);

        // Re-ingest much shorter content at the same URI.
        let second = r
            .ingest(uri(), "text/plain", b"alpha alpha alpha")
            .await
            .unwrap();
        assert_eq!(second.document.id, first.document.id);
        // The store holds only the new chunks — the old beta/gamma ones are pruned.
        assert_eq!(r.store().len().await.unwrap(), second.chunks);
        let hits = r.search(SearchScope::Unscoped, "gamma", 10).await.unwrap();
        assert!(
            hits.iter().all(|h| !h.snippet.contains("gamma")),
            "stale chunks from the longer version must be gone"
        );
    }

    #[tokio::test]
    async fn re_ingesting_empty_content_clears_the_document() {
        let r = retriever();
        let uri = || DocumentSource::uri("file:///doc.txt");
        r.ingest(uri(), "text/plain", b"alpha beta gamma delta epsilon")
            .await
            .unwrap();
        assert!(r.store().len().await.unwrap() > 0);

        // Re-ingesting empty content at the same URI clears its chunks entirely.
        let out = r.ingest(uri(), "text/plain", b"   ").await.unwrap();
        assert_eq!(out.chunks, 0);
        assert!(r.store().is_empty().await.unwrap());
    }

    #[tokio::test]
    async fn delete_removes_a_documents_chunks_and_is_idempotent() {
        let r = retriever();
        let outcome = r
            .ingest(
                DocumentSource::uri("file:///doc.txt"),
                "text/plain",
                b"alpha beta gamma delta epsilon",
            )
            .await
            .unwrap();
        assert!(r.store().len().await.unwrap() > 0);

        r.delete(outcome.document.id).await.unwrap();
        assert!(r.store().is_empty().await.unwrap());
        // Deleting again (or an unknown document) is a no-op, not an error.
        r.delete(outcome.document.id).await.unwrap();
        r.delete(DocumentId::new()).await.unwrap();
    }

    #[tokio::test]
    async fn inline_documents_get_a_fresh_id_each_ingest() {
        let r = retriever();
        let a = r
            .ingest(DocumentSource::Inline, "text/plain", b"same inline bytes")
            .await
            .unwrap();
        let b = r
            .ingest(DocumentSource::Inline, "text/plain", b"same inline bytes")
            .await
            .unwrap();
        assert_ne!(a.document.id, b.document.id);
    }

    #[tokio::test]
    async fn rejects_unsupported_media_type() {
        let r = retriever();
        let err = r
            .ingest(DocumentSource::Inline, "application/pdf", b"%PDF-1.7")
            .await
            .unwrap_err();
        assert!(matches!(err, RetrievalError::Parse(_)));
    }
}
