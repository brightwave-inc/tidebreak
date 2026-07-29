//! The core retrieval domain types: documents, chunks, spans, and citations.
//!
//! The load-bearing idea here is the **byte span**. Every chunk records the byte
//! range it occupies in its parent document's text, and citations carry that same
//! span. Keeping offsets on everything means the chunk text can later live apart
//! from the vector index (offsets in the store, text rehydrated from the source),
//! and it gives every answer a precise, verifiable pointer back into the source.

pub use openwave_core::{ByteSpan, PageBounds, SourceLocation, SourceRegion, PAGE_BOUNDS_SCALE};
use openwave_core::{CellAddress, ChatId, DocumentGeneration, ProjectId, RetrievalEvidenceInput};
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
    ///
    /// Never returns more regions than a piece of evidence is allowed to carry.
    /// A parser that resolves positions within a page emits many small regions,
    /// and a span over dense text — a page of short table cells — can overlap
    /// more of them than [`RetrievalEvidenceInput::MAX_SOURCE_REGIONS`] permits,
    /// which would make the evidence invalid rather than merely coarse. When
    /// that happens the regions are coalesced by page: the page a passage came
    /// from is the part worth keeping, and the geometry within it is the part
    /// worth losing.
    ///
    /// A structured source coalesces the same way one level up the tree: a
    /// passage crossing four hundred cells of a table is at the table, and the
    /// enclosing node is what survives when the individual ones cannot. A
    /// workbook coalesces to the rectangle its cells occupy on each sheet, which
    /// is the shape a spreadsheet passage is read as anyway.
    #[must_use]
    pub fn source_regions_for(&self, span: ByteSpan) -> Vec<SourceRegion> {
        let clipped: Vec<SourceRegion> = self
            .source_regions
            .iter()
            .filter_map(|region| {
                let start = region.span.start.max(span.start);
                let end = region.span.end.min(span.end);
                (start < end).then(|| SourceRegion {
                    span: ByteSpan::new(start, end),
                    location: region.location.clone(),
                })
            })
            .collect();
        if clipped.len() <= RetrievalEvidenceInput::MAX_SOURCE_REGIONS {
            return clipped;
        }
        if clipped
            .iter()
            .all(|region| region.location.structured_path().is_some())
        {
            return coalesce_by_ancestor(clipped);
        }
        if clipped
            .iter()
            .all(|region| region.location.spreadsheet_cells().is_some())
        {
            return coalesce_by_sheet(clipped);
        }
        coalesce_by_page(clipped)
    }
}

/// Merge consecutive regions on the same sheet into one region for the
/// rectangle they occupy, dropping the individual cells.
///
/// A passage over a grid is read as a block, not as three hundred separate
/// values, so the rectangle enclosing them loses nothing a reader was using —
/// and unlike a page, a range keeps saying exactly where on the sheet the
/// passage was. Regions arrive ordered, so a run on one sheet is a contiguous
/// stretch of the passage; a passage that crosses into another sheet keeps that
/// sheet's cells as their own region.
fn coalesce_by_sheet(regions: Vec<SourceRegion>) -> Vec<SourceRegion> {
    let mut merged: Vec<SourceRegion> = Vec::new();
    for region in regions {
        let Some(cells) = region.location.spreadsheet_cells() else {
            merged.push(region);
            continue;
        };
        let extended = merged.last_mut().and_then(|last| {
            let previous = last.location.spreadsheet_cells()?;
            (previous.sheet_index == cells.sheet_index).then_some(last)
        });
        let Some(last) = extended else {
            merged.push(region);
            continue;
        };
        let SourceLocation::SpreadsheetCells {
            start_cell,
            end_cell,
            ..
        } = &mut last.location
        else {
            unreachable!("the location was just read as spreadsheet cells")
        };
        let corners = [
            start_cell.as_str(),
            end_cell.as_deref().unwrap_or(start_cell),
            cells.start_cell,
            cells.end_cell.unwrap_or(cells.start_cell),
        ]
        .into_iter()
        .filter_map(CellAddress::parse)
        .collect::<Vec<_>>();
        // A cell that does not read as A1 cannot widen a rectangle, and neither
        // can a rectangle whose corners will not write back as A1. Either way
        // the regions keep the cells they arrived with rather than a guess, and
        // only their spans merge.
        let widened = corners.first().zip(corners.last()).and_then(|(a, b)| {
            let top_left = corners.iter().fold(*a, |corner, cell| CellAddress {
                column: corner.column.min(cell.column),
                row: corner.row.min(cell.row),
            });
            let bottom_right = corners.iter().fold(*b, |corner, cell| CellAddress {
                column: corner.column.max(cell.column),
                row: corner.row.max(cell.row),
            });
            let start = top_left.to_a1()?;
            let end = if top_left == bottom_right {
                None
            } else {
                Some(bottom_right.to_a1()?)
            };
            Some((start, end))
        });
        if let Some((start, end)) = widened {
            *start_cell = start;
            *end_cell = end;
        }
        last.span = ByteSpan::new(last.span.start, region.span.end);
    }
    merged
}

/// Fold a run of structured-path regions into at most the number of regions
/// evidence carries, naming each group by the deepest node its members share.
///
/// Regions arrive ordered, so a group is a contiguous stretch of the passage
/// and the node above them all is a true description of it. A group whose
/// members share no node at all — spanning two top-level keys, say — keeps the
/// first node it touches, which is at least somewhere the reader can be sent.
fn coalesce_by_ancestor(regions: Vec<SourceRegion>) -> Vec<SourceRegion> {
    let limit = RetrievalEvidenceInput::MAX_SOURCE_REGIONS;
    let group_size = regions.len().div_ceil(limit).max(1);
    regions
        .chunks(group_size)
        .filter_map(|group| {
            let first = group.first()?;
            let (first_path, path_type) = first.location.structured_path()?;
            let common = group
                .iter()
                .filter_map(|region| region.location.structured_path())
                .filter(|(_, kind)| *kind == path_type)
                .fold(first_path, |common, (path, _)| {
                    path_type.common_ancestor(common, path)
                });
            let path = if common.is_empty() {
                first_path
            } else {
                common
            };
            Some(SourceRegion {
                span: ByteSpan::new(first.span.start, group.last()?.span.end),
                location: SourceLocation::StructuredPath {
                    path: path.to_owned(),
                    path_type,
                },
            })
        })
        .collect()
}

/// Merge consecutive regions that name the same page into one region for that
/// page, dropping the geometry that distinguished them.
///
/// Regions arrive ordered and non-overlapping, so consecutive regions on a page
/// are contiguous or separated only by text that region map already assigned to
/// the same page — merging them spans exactly the text they covered between
/// them.
fn coalesce_by_page(regions: Vec<SourceRegion>) -> Vec<SourceRegion> {
    /// The page a region names, when it names one. `SourceLocation` is
    /// `#[non_exhaustive]`, so a location this crate does not recognize is
    /// possible in principle; such a region is passed through untouched rather
    /// than folded into a neighbour it may have nothing to do with.
    fn page_number(location: &SourceLocation) -> Option<std::num::NonZeroU32> {
        match location {
            SourceLocation::Page { number, .. } => Some(*number),
            _ => None,
        }
    }

    let mut merged: Vec<SourceRegion> = Vec::new();
    for region in regions {
        let page = page_number(&region.location);
        let extends_previous = page.is_some()
            && merged
                .last()
                .is_some_and(|previous| page_number(&previous.location) == page);
        if extends_previous {
            // Safe: `extends_previous` is false for an empty `merged`.
            let previous = merged.last_mut().expect("checked non-empty");
            previous.span = ByteSpan::new(previous.span.start, region.span.end);
        } else if let Some(number) = page {
            merged.push(SourceRegion {
                span: region.span,
                location: SourceLocation::Page {
                    number,
                    bounds: None,
                },
            });
        } else {
            merged.push(region);
        }
    }
    merged
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
    fn clipping_never_returns_more_regions_than_evidence_may_carry() {
        // A page of short cells, each positioned: far more regions than the
        // evidence cap. Exceeding it does not truncate the evidence, it makes
        // the evidence invalid — so the clip has to stay inside the cap itself.
        let count = RetrievalEvidenceInput::MAX_SOURCE_REGIONS * 3;
        let text = "x".repeat(count);
        let regions: Vec<SourceRegion> = (0..count)
            .map(|i| SourceRegion {
                span: ByteSpan::new(i, i + 1),
                location: SourceLocation::Page {
                    number: std::num::NonZeroU32::new(if i < count / 2 { 1 } else { 2 }).unwrap(),
                    bounds: Some(openwave_core::PageBounds {
                        left: 0,
                        top: 0,
                        width: 10,
                        height: 10,
                    }),
                },
            })
            .collect();
        let document = Document::new(DocumentSource::Inline, "application/pdf", text.clone())
            .with_source_regions(regions);

        let clipped = document.source_regions_for(ByteSpan::new(0, count));
        assert!(clipped.len() <= RetrievalEvidenceInput::MAX_SOURCE_REGIONS);
        // The pages survive the coalescing; only the geometry is given up.
        let pages: Vec<_> = clipped
            .iter()
            .map(|region| match region.location {
                SourceLocation::Page { number, bounds } => (number.get(), bounds),
                #[allow(unreachable_patterns)]
                _ => panic!("expected a page location"),
            })
            .collect();
        assert_eq!(pages, vec![(1, None), (2, None)]);
        // And the merged regions still tile the span they were clipped from.
        assert_eq!(clipped[0].span, ByteSpan::new(0, count / 2));
        assert_eq!(clipped[1].span, ByteSpan::new(count / 2, count));
    }

    #[test]
    fn clipping_keeps_geometry_when_it_fits() {
        let document = Document::new(DocumentSource::Inline, "application/pdf", "alpha beta")
            .with_source_regions(vec![SourceRegion {
                span: ByteSpan::new(0, 5),
                location: SourceLocation::Page {
                    number: std::num::NonZeroU32::new(1).unwrap(),
                    bounds: Some(openwave_core::PageBounds {
                        left: 1,
                        top: 2,
                        width: 3,
                        height: 4,
                    }),
                },
            }]);

        let clipped = document.source_regions_for(ByteSpan::new(0, 10));
        assert_eq!(clipped.len(), 1);
        assert!(matches!(
            clipped[0].location,
            SourceLocation::Page {
                bounds: Some(_),
                ..
            }
        ));
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
