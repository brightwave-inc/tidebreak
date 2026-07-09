//! The end-to-end pipeline: parse → chunk → embed → store, and query → cite.
//!
//! [`Retriever`] wires the four seams together behind two methods, [`Retriever::ingest`]
//! and [`Retriever::search`]. It owns a parser and chunker and shares an embedder
//! and vector store (the store is typically shared with the rest of the process,
//! so a future `search` tool can query the same index this ingests into).

use std::sync::Arc;

use crate::chunk::Chunker;
use crate::document::{Citation, Document, DocumentSource};
use crate::embed::Embedder;
use crate::error::{Result, RetrievalError};
use crate::id::DocumentId;
use crate::parse::DocumentParser;
use crate::vector::{VectorRecord, VectorStore};

/// The outcome of ingesting one document.
#[derive(Debug, Clone)]
pub struct IngestOutcome {
    /// The parsed document, including its canonical text. Callers that persist a
    /// text-of-record (to rehydrate citation snippets from spans later) keep this.
    pub document: Document,
    /// How many chunks were embedded and stored.
    pub chunks: usize,
}

/// Ties parsing, chunking, embedding, and vector storage into one pipeline.
pub struct Retriever {
    parser: Box<dyn DocumentParser>,
    chunker: Box<dyn Chunker>,
    embedder: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
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
        }
    }

    /// Borrow the shared vector store (e.g. to hand a search tool the same index).
    #[must_use]
    pub fn store(&self) -> &Arc<dyn VectorStore> {
        &self.store
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
        let parsed = self.parser.parse(raw, media_type)?;
        // Stable id per URI so re-ingesting a file is idempotent; inline content
        // has no external identity, so it gets a fresh id each time.
        let id = match &source {
            DocumentSource::Uri { uri } => DocumentId::derive(uri),
            _ => DocumentId::new(),
        };
        let document = Document::with_id(id, source, media_type, parsed.text);

        let chunks = self.chunker.chunk(&document)?;

        // Embed first (the slow, awaited step), then hand the store a single
        // atomic replace. Doing the delete+insert as one store operation — rather
        // than a separate prune then upsert with an embed await in between — is
        // what keeps two concurrent re-ingests of the same document from
        // interleaving and leaving a mix of versions. Empty content yields an
        // empty record set, which clears any prior version of the document.
        let records: Vec<VectorRecord> = if chunks.is_empty() {
            Vec::new()
        } else {
            let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
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
                .map(|(chunk, embedding)| VectorRecord { chunk, embedding })
                .collect()
        };

        let count = records.len();
        self.store.replace_document(document.id, records).await?;

        Ok(IngestOutcome {
            document,
            chunks: count,
        })
    }

    /// Search the index for the `k` chunks most relevant to `query`, as citations.
    pub async fn search(&self, query: &str, k: usize) -> Result<Vec<Citation>> {
        let embedding = self.embedder.embed_query(query).await?;
        let hits = self.store.query(&embedding, k).await?;
        Ok(hits.into_iter().map(Citation::from).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::TextChunker;
    use crate::embed::HashEmbedder;
    use crate::parse::PlainTextParser;
    use crate::vector::InMemoryVectorStore;

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
            .search("which planet is the largest gas giant", 1)
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
    async fn empty_document_ingests_no_chunks() {
        let r = retriever();
        let outcome = r
            .ingest(DocumentSource::Inline, "text/plain", b"   \n  ")
            .await
            .unwrap();
        assert_eq!(outcome.chunks, 0);
        assert!(r.store().is_empty().await.unwrap());
        assert!(r.search("anything", 5).await.unwrap().is_empty());
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
            .map(|(chunk, embedding)| VectorRecord { chunk, embedding })
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
        let hits = r.search("three four five", 10).await.unwrap();
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
        let hits = r.search("gamma", 10).await.unwrap();
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
