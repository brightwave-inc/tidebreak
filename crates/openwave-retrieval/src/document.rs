//! The core retrieval domain types: documents, chunks, spans, and citations.
//!
//! The load-bearing idea here is the **byte span**. Every chunk records the byte
//! range it occupies in its parent document's text, and citations carry that same
//! span. Keeping offsets on everything means the chunk text can later live apart
//! from the vector index (offsets in the store, text rehydrated from the source),
//! and it gives every answer a precise, verifiable pointer back into the source.

pub use openwave_core::{ByteSpan, SourceLocation, SourceRegion};
use openwave_core::{ChatId, DocumentGeneration, ProjectId};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

use crate::error::{Result, RetrievalError};
use crate::id::{ChunkId, DocumentId};

/// Where an ingested document came from — its provenance for citations.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DocumentSource {
    /// A path or URL the document was read from (shown to users in citations).
    Uri {
        /// The originating path or URL.
        uri: String,
    },
    /// Content supplied inline with no external origin.
    #[default]
    Inline,
}

impl DocumentSource {
    /// Convenience constructor for a URI source.
    pub fn uri(uri: impl Into<String>) -> Self {
        Self::Uri { uri: uri.into() }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if let Self::Uri { uri } = self {
            if uri.is_empty()
                || uri.len() > openwave_core::RetrievalEvidenceInput::MAX_SOURCE_URI_BYTES
                || uri.contains('\0')
            {
                return Err(RetrievalError::vector_store(
                    "document source URI exceeds retrieval evidence bounds",
                ));
            }
        }
        Ok(())
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
    /// Conversation that owns this document for current product retrieval.
    pub chat_id: Option<ChatId>,
    /// Legacy project corpus this document belongs to.
    pub project_id: Option<ProjectId>,
    /// Where the document came from.
    pub source: DocumentSource,
    /// The document's media (MIME) type, e.g. `text/plain` or `text/markdown`.
    pub media_type: String,
    /// The extracted plain text. Chunk [`ByteSpan`]s are offsets into this.
    pub text: String,
    /// Parser-produced mappings from canonical text to the original source.
    pub source_regions: Vec<SourceRegion>,
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
            chat_id: None,
            project_id: None,
            source,
            media_type: media_type.into(),
            text: text.into(),
            source_regions: Vec::new(),
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
            chat_id: None,
            project_id: Some(project_id),
            source,
            media_type: media_type.into(),
            text: text.into(),
            source_regions: Vec::new(),
        }
    }

    /// Assemble a conversation-scoped document with a caller-supplied id.
    #[must_use]
    pub fn with_id_for_chat(
        id: DocumentId,
        chat_id: ChatId,
        source: DocumentSource,
        media_type: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            id,
            chat_id: Some(chat_id),
            project_id: None,
            source,
            media_type: media_type.into(),
            text: text.into(),
            source_regions: Vec::new(),
        }
    }

    /// Attach validated parser-produced source regions.
    #[must_use]
    pub fn with_source_regions(mut self, source_regions: Vec<SourceRegion>) -> Self {
        self.source_regions = source_regions;
        self
    }

    /// Borrow the slice of `text` a span refers to.
    ///
    /// Returns `None` if the span falls outside the text or off a char boundary,
    /// so a stale citation can't panic the process.
    #[must_use]
    pub fn slice(&self, span: ByteSpan) -> Option<&str> {
        self.text.get(span.start..span.end)
    }

    /// Return source regions intersecting `span`, clipped to its boundaries.
    #[must_use]
    pub fn source_regions_for(&self, span: ByteSpan) -> Vec<SourceRegion> {
        self.source_regions
            .iter()
            .filter_map(|region| {
                let start = region.span.start.max(span.start);
                let end = region.span.end.min(span.end);
                (start < end).then(|| SourceRegion {
                    span: ByteSpan::new(start, end),
                    location: region.location.clone(),
                })
            })
            .collect()
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
    /// Markdown heading hierarchy that contains this chunk, outermost first.
    /// Empty for non-Markdown documents and Markdown preambles.
    pub heading_path: Vec<String>,
    /// Original source locations represented by this chunk.
    pub source_regions: Vec<SourceRegion>,
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
            heading_path: Vec::new(),
            source_regions: Vec::new(),
            span,
        }
    }

    /// Build a chunk with derived structural context while preserving source text.
    #[must_use]
    pub fn with_heading_path(
        document_id: DocumentId,
        ordinal: usize,
        span: ByteSpan,
        text: impl Into<String>,
        heading_path: Vec<String>,
    ) -> Self {
        let mut chunk = Self::new(document_id, ordinal, span, text);
        chunk.heading_path = heading_path;
        chunk
    }

    /// Canonical text used for embedding, lexical ranking, and model reranking.
    ///
    /// Citation text remains the exact source slice in [`Self::text`].
    /// Every chunk in a Markdown section receives the same breadcrumb prefix,
    /// including the first chunk whose exact source text still contains the raw
    /// heading line. This deliberate duplication keeps context uniform until a
    /// richer parser separates heading blocks from body blocks.
    #[must_use]
    pub fn retrieval_text(&self) -> Cow<'_, str> {
        if self.heading_path.is_empty() {
            Cow::Borrowed(&self.text)
        } else {
            Cow::Owned(format!(
                "{}\n\n{}",
                self.heading_path.join(" > "),
                self.text
            ))
        }
    }

    pub(crate) fn validate_source_regions(&self) -> Result<()> {
        let heading_bytes = self
            .heading_path
            .iter()
            .try_fold(0_usize, |total, heading| total.checked_add(heading.len()));
        if self.id != ChunkId::derive(self.document_id, self.span.start, self.span.end)
            || self.span.len() != self.text.len()
            || self.text.contains('\0')
            || i64::try_from(self.span.start).is_err()
            || i64::try_from(self.span.end).is_err()
            || self.text.len() > openwave_core::RetrievalEvidenceInput::MAX_SNIPPET_BYTES
            || self.heading_path.len() > openwave_core::RetrievalEvidenceInput::MAX_HEADING_SEGMENTS
            || heading_bytes.is_none_or(|bytes| {
                bytes > openwave_core::RetrievalEvidenceInput::MAX_HEADING_BYTES
            })
            || self
                .heading_path
                .iter()
                .any(|heading| heading.contains('\0'))
            || self.source_regions.len() > openwave_core::RetrievalEvidenceInput::MAX_SOURCE_REGIONS
        {
            return Err(RetrievalError::vector_store(
                "chunk exceeds retrieval evidence bounds",
            ));
        }
        let mut previous_end = self.span.start;
        for region in &self.source_regions {
            if region.span.is_empty()
                || region.span.start < self.span.start
                || region.span.end > self.span.end
                || region.span.start < previous_end
            {
                return Err(RetrievalError::vector_store(
                    "chunk source regions must be ordered within the chunk span",
                ));
            }
            let local_start = region.span.start - self.span.start;
            let local_end = region.span.end - self.span.start;
            if !self.text.is_char_boundary(local_start) || !self.text.is_char_boundary(local_end) {
                return Err(RetrievalError::vector_store(
                    "chunk source region offsets must be UTF-8 character boundaries",
                ));
            }
            previous_end = region.span.end;
        }
        Ok(())
    }
}

/// A chunk paired with the relevance score a search assigned it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredChunk {
    /// The matched chunk.
    pub chunk: Chunk,
    /// Source provenance captured in the same indexed row as this chunk.
    #[serde(skip)]
    pub source: DocumentSource,
    /// Exact searchable generation, absent only for legacy unversioned stores.
    #[serde(skip)]
    pub generation: Option<DocumentGeneration>,
    /// Relevance score; initially assigned by the backend and overwritten by an
    /// optional reranker before final selection. Higher is more relevant within
    /// one result set; reranker scores are not comparable across queries or
    /// configurations.
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
    /// Source provenance captured when this generation was indexed.
    #[serde(skip)]
    pub source: DocumentSource,
    /// Exact indexed generation; agent search rejects unversioned results.
    #[serde(skip)]
    pub generation: Option<DocumentGeneration>,
    /// The cited chunk.
    pub chunk_id: ChunkId,
    /// The exact byte range cited within the document text.
    pub span: ByteSpan,
    /// The cited text.
    pub snippet: String,
    /// Markdown heading hierarchy containing the cited source span.
    pub heading_path: Vec<String>,
    /// Original source locations represented by the cited span.
    pub source_regions: Vec<SourceRegion>,
    /// The final relevance score that surfaced this citation. When reranking is
    /// configured, this is the reranker score rather than the backend score.
    pub score: f32,
}

impl From<ScoredChunk> for Citation {
    fn from(scored: ScoredChunk) -> Self {
        Self {
            document_id: scored.chunk.document_id,
            source: scored.source,
            generation: scored.generation,
            chunk_id: scored.chunk.id,
            span: scored.chunk.span,
            snippet: scored.chunk.text,
            heading_path: scored.chunk.heading_path,
            source_regions: scored.chunk.source_regions,
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
    fn retrieval_text_adds_context_without_changing_source_text() {
        let doc = DocumentId::new();
        let plain = Chunk::new(doc, 0, ByteSpan::new(0, 4), "body");
        assert!(matches!(plain.retrieval_text(), Cow::Borrowed("body")));

        let contextual = Chunk::with_heading_path(
            doc,
            0,
            ByteSpan::new(0, 4),
            "body",
            vec!["Guide".into(), "Setup".into()],
        );
        assert_eq!(contextual.text, "body");
        assert_eq!(contextual.retrieval_text(), "Guide > Setup\n\nbody");
    }

    #[test]
    fn citation_carries_span_and_snippet_from_scored_chunk() {
        let doc = DocumentId::new();
        let chunk = Chunk::new(doc, 2, ByteSpan::new(0, 3), "abc");
        let citation: Citation = ScoredChunk {
            chunk: chunk.clone(),
            source: DocumentSource::Inline,
            generation: None,
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
