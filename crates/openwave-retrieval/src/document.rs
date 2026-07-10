//! The core retrieval domain types: documents, chunks, spans, and citations.
//!
//! The load-bearing idea here is the **byte span**. Every chunk records the byte
//! range it occupies in its parent document's text, and citations carry that same
//! span. Keeping offsets on everything means the chunk text can later live apart
//! from the vector index (offsets in the store, text rehydrated from the source),
//! and it gives every answer a precise, verifiable pointer back into the source.

use openwave_core::ProjectId;
use serde::{Deserialize, Serialize};

use crate::id::{ChunkId, DocumentId};

/// A half-open byte range `[start, end)` into a document's text.
///
/// Byte offsets (not char offsets) because that is what Rust string slicing
/// speaks; both ends always fall on UTF-8 character boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteSpan {
    /// Inclusive start byte offset.
    pub start: usize,
    /// Exclusive end byte offset.
    pub end: usize,
}

impl ByteSpan {
    /// Construct a span, panicking in debug if `start > end`.
    #[must_use]
    pub fn new(start: usize, end: usize) -> Self {
        debug_assert!(start <= end, "span start {start} must not exceed end {end}");
        Self { start, end }
    }

    /// The number of bytes the span covers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// Whether the span covers no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// Where an ingested document came from — its provenance for citations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DocumentSource {
    /// A path or URL the document was read from (shown to users in citations).
    Uri {
        /// The originating path or URL.
        uri: String,
    },
    /// Content supplied inline with no external origin.
    Inline,
}

impl DocumentSource {
    /// Convenience constructor for a URI source.
    pub fn uri(uri: impl Into<String>) -> Self {
        Self::Uri { uri: uri.into() }
    }
}

/// An ingested source document: its provenance, media type, and extracted text.
///
/// `text` is the canonical plain-text representation produced by a
/// [`crate::parse::DocumentParser`]. Chunk spans index into *this* string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    /// Stable identifier for this document.
    pub id: DocumentId,
    /// Project corpus this document belongs to, or `None` for the unscoped corpus.
    pub project_id: Option<ProjectId>,
    /// Where the document came from.
    pub source: DocumentSource,
    /// The document's media (MIME) type, e.g. `text/plain` or `text/markdown`.
    pub media_type: String,
    /// The extracted plain text. Chunk [`ByteSpan`]s are offsets into this.
    pub text: String,
}

impl Document {
    /// Assemble a document from freshly-parsed text, minting a new id.
    #[must_use]
    pub fn new(
        source: DocumentSource,
        media_type: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self::with_id(DocumentId::new(), source, media_type, text)
    }

    /// Assemble a document with a caller-supplied id.
    ///
    /// Used by ingestion to give URI-sourced documents a stable, derived id so
    /// re-ingestion is idempotent (see [`DocumentId::derive`]).
    #[must_use]
    pub fn with_id(
        id: DocumentId,
        source: DocumentSource,
        media_type: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            id,
            project_id: None,
            source,
            media_type: media_type.into(),
            text: text.into(),
        }
    }

    /// Assemble a project-scoped document from freshly parsed text.
    #[must_use]
    pub fn new_scoped(
        project_id: ProjectId,
        source: DocumentSource,
        media_type: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self::with_id_scoped(DocumentId::new(), project_id, source, media_type, text)
    }

    /// Assemble a project-scoped document with a caller-supplied id.
    #[must_use]
    pub fn with_id_scoped(
        id: DocumentId,
        project_id: ProjectId,
        source: DocumentSource,
        media_type: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            id,
            project_id: Some(project_id),
            source,
            media_type: media_type.into(),
            text: text.into(),
        }
    }

    /// Borrow the slice of `text` a span refers to.
    ///
    /// Returns `None` if the span falls outside the text or off a char boundary,
    /// so a stale citation can't panic the process.
    #[must_use]
    pub fn slice(&self, span: ByteSpan) -> Option<&str> {
        self.text.get(span.start..span.end)
    }
}

/// One chunk of a document: a contiguous slice of its text, ready to embed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    /// Derived id, stable for a given `(document, span)`.
    pub id: ChunkId,
    /// The document this chunk belongs to.
    pub document_id: DocumentId,
    /// Zero-based position of this chunk within the document.
    pub ordinal: usize,
    /// The chunk's text.
    pub text: String,
    /// The byte range this chunk occupies in the document text.
    pub span: ByteSpan,
}

impl Chunk {
    /// Build a chunk, deriving its id from the document and span.
    #[must_use]
    pub fn new(
        document_id: DocumentId,
        ordinal: usize,
        span: ByteSpan,
        text: impl Into<String>,
    ) -> Self {
        Self {
            id: ChunkId::derive(document_id, span.start, span.end),
            document_id,
            ordinal,
            text: text.into(),
            span,
        }
    }
}

/// A chunk paired with the relevance score a search assigned it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredChunk {
    /// The matched chunk.
    pub chunk: Chunk,
    /// Similarity score; higher is more relevant. For cosine, in `[-1, 1]`.
    pub score: f32,
}

/// A retrieval result framed as a citation: a scored pointer back into a source.
///
/// This is what a search returns and what an answer grounds itself on. It carries
/// the document, the exact byte span, a text snippet, and the score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Citation {
    /// The cited document.
    pub document_id: DocumentId,
    /// The cited chunk.
    pub chunk_id: ChunkId,
    /// The exact byte range cited within the document text.
    pub span: ByteSpan,
    /// The cited text.
    pub snippet: String,
    /// The relevance score that surfaced this citation.
    pub score: f32,
}

impl From<ScoredChunk> for Citation {
    fn from(scored: ScoredChunk) -> Self {
        Self {
            document_id: scored.chunk.document_id,
            chunk_id: scored.chunk.id,
            span: scored.chunk.span,
            snippet: scored.chunk.text,
            score: scored.score,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_len_and_emptiness() {
        assert_eq!(ByteSpan::new(4, 10).len(), 6);
        assert!(ByteSpan::new(5, 5).is_empty());
        assert!(!ByteSpan::new(5, 6).is_empty());
    }

    #[test]
    fn document_slice_returns_the_spanned_text() {
        let doc = Document::new(DocumentSource::Inline, "text/plain", "hello world");
        assert_eq!(doc.slice(ByteSpan::new(0, 5)), Some("hello"));
        assert_eq!(doc.slice(ByteSpan::new(6, 11)), Some("world"));
    }

    #[test]
    fn document_slice_is_bounds_safe() {
        let doc = Document::new(DocumentSource::Inline, "text/plain", "hi");
        assert_eq!(doc.slice(ByteSpan::new(0, 99)), None);
    }

    #[test]
    fn chunk_id_derives_from_document_and_span() {
        let doc = DocumentId::new();
        let span = ByteSpan::new(3, 8);
        let chunk = Chunk::new(doc, 0, span, "abcde");
        assert_eq!(chunk.id, ChunkId::derive(doc, 3, 8));
    }

    #[test]
    fn citation_carries_span_and_snippet_from_scored_chunk() {
        let doc = DocumentId::new();
        let chunk = Chunk::new(doc, 2, ByteSpan::new(0, 3), "abc");
        let citation: Citation = ScoredChunk {
            chunk: chunk.clone(),
            score: 0.9,
        }
        .into();
        assert_eq!(citation.document_id, doc);
        assert_eq!(citation.chunk_id, chunk.id);
        assert_eq!(citation.span, ByteSpan::new(0, 3));
        assert_eq!(citation.snippet, "abc");
        assert_eq!(citation.score, 0.9);
    }

    #[test]
    fn document_source_serializes_tagged() {
        let json = serde_json::to_string(&DocumentSource::uri("file:///a.txt")).unwrap();
        assert_eq!(json, r#"{"kind":"uri","uri":"file:///a.txt"}"#);
        let inline = serde_json::to_string(&DocumentSource::Inline).unwrap();
        assert_eq!(inline, r#"{"kind":"inline"}"#);
    }
}
