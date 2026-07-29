//! Making one extracted page a citable source.
//!
//! A page a model fetched is evidence like any other, and until it is one it
//! cannot be anchored: a claim drawn from a fetched page has no span, no
//! highlight, and nothing a reader can open. This module is the bridge. The
//! extraction is handed to a host [`ExtractedPageSink`], which stores it as an
//! ordinary conversation source, and the passages of the stored text come back
//! as [`RetrievalEvidenceInput`] rows under the same closed grammar
//! `read_source` and `search` teach. Nothing about the citation path is
//! web-specific downstream of here.
//!
//! Two rules keep the spans honest, and both are enforced by construction
//! rather than by care:
//!
//! * A citation addresses the text that was **stored**, never the text that was
//!   fetched. The sink writes [`WebExtractResponse::content`] verbatim as the
//!   source's canonical text, so byte offsets into one are byte offsets into
//!   the other. A sink that cannot promise that must fail instead.
//! * A citation never crosses a discontinuity. Bounded extraction can drop the
//!   middle of a page and say so with
//!   [`EXTRACT_TRUNCATION_MARKER`](crate::types::EXTRACT_TRUNCATION_MARKER);
//!   text either side of that marker was never adjacent. Passages are cut at
//!   the marker before anything can quote across it, so no single reference can
//!   span material that the page did not have together.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use openwave_core::{
    citation_authoring_instruction, format_citation_reference, AssistantCitationReference,
    ByteSpan, ChatId, ChunkId, CitationFormat, DocumentGeneration, DocumentId, EvidenceLocation,
    RetrievalEvidenceInput, RetrievalEvidenceSource,
};

use crate::types::EXTRACT_TRUNCATION_MARKER;
use crate::WebExtractResponse;

/// Most citable passages one extraction is cut into.
///
/// A ceiling on pathology rather than a working limit, and it is deliberately
/// slack: an extraction is bounded to
/// [`MAX_EXTRACT_OUTPUT_BYTES`](crate::MAX_EXTRACT_OUTPUT_BYTES) and cut into
/// windows of [`RetrievalEvidenceInput::MAX_SNIPPET_BYTES`], which is two
/// windows for each of the at most two runs a truncated page has. Nothing a
/// real page produces comes near this, so the tail of a real page is never
/// dropped for want of a reference.
pub const MAX_EXTRACTED_PAGE_PASSAGES: usize = 8;

/// Where one stored extraction landed.
///
/// The sink returns the identity and the exact generation it wrote, because
/// evidence is generation-fenced: a citation made against text that has since
/// been replaced must be recognizable as stale rather than silently resolve
/// against different words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredExtractedPage {
    pub document_id: DocumentId,
    pub generation: DocumentGeneration,
}

/// Why one extracted page could not be kept as a source.
///
/// Deliberately opaque: a storage diagnostic is host state, and the model is
/// told only that the page could not be made citable — which is all it can act
/// on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the extracted page could not be stored as a source")]
pub struct ExtractedPageSinkError;

/// A durable home for extracted pages.
///
/// Implementations **must** store [`WebExtractResponse::content`] byte for byte
/// as the document's canonical text. Every span this module produces indexes
/// into that string, and a sink that normalized, re-wrapped, or re-parsed it
/// would leave citations pointing at text nobody wrote.
///
/// Implementations must also carry the extraction's provenance — its URL, its
/// title, the time it was fetched, and which engine produced it — onto the
/// stored source. For a page there is no original to re-derive any of it from
/// later: whatever is not recorded here is lost.
#[async_trait]
pub trait ExtractedPageSink: Send + Sync {
    async fn store_page(
        &self,
        chat_id: ChatId,
        page: &WebExtractResponse,
        fetched_at: DateTime<Utc>,
    ) -> Result<StoredExtractedPage, ExtractedPageSinkError>;
}

/// One contiguous run of stored text a model may cite as a unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtractedPassage {
    pub(crate) span: ByteSpan,
    pub(crate) reference: AssistantCitationReference,
}

/// Cut the stored text into the runs a citation may address.
///
/// The cuts are the truncation markers, and only those: within a run the bytes
/// were adjacent on the page, so a span anywhere inside one is a span the page
/// actually had. A run longer than one evidence snippet is windowed rather than
/// clipped, so the tail of a long article stays citable instead of silently
/// falling off the end of the only reference on offer.
pub(crate) fn extracted_passages(content: &str, max_snippet_bytes: usize) -> Vec<ExtractedPassage> {
    let mut passages = Vec::new();
    let mut offset = 0;
    for run in content.split(EXTRACT_TRUNCATION_MARKER) {
        let start = offset;
        offset += run.len() + EXTRACT_TRUNCATION_MARKER.len();
        let mut cursor = start;
        let end = start + run.len();
        while cursor < end {
            if passages.len() == MAX_EXTRACTED_PAGE_PASSAGES {
                return passages;
            }
            let window = window_end(content, cursor, end.min(cursor + max_snippet_bytes));
            // A window that cannot advance would loop forever; it can only
            // happen if a single character exceeds the snippet budget, which no
            // real budget allows.
            if window <= cursor {
                break;
            }
            passages.push(ExtractedPassage {
                span: ByteSpan::new(cursor, window),
                reference: AssistantCitationReference {
                    source_token: uuid::Uuid::new_v4(),
                },
            });
            cursor = window;
        }
    }
    passages
}

/// The largest character boundary at or below `limit`, so a window never splits
/// a character.
fn window_end(content: &str, start: usize, limit: usize) -> usize {
    let mut end = limit;
    while end > start && !content.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// The evidence rows one stored extraction contributes to the turn.
///
/// The heading is the page's own title, which is the only name a reader has for
/// a source that was never a file. It is already sanitized by
/// [`WebExtractResponse::new`], so it cannot carry a bidirectional override into
/// a citation row.
pub(crate) fn extracted_evidence(
    page: &WebExtractResponse,
    stored: &StoredExtractedPage,
    passages: &[ExtractedPassage],
) -> Vec<RetrievalEvidenceInput> {
    let heading_path = if page.title.is_empty() {
        Vec::new()
    } else {
        vec![page.title.clone()]
    };
    let source = if page.url.len() <= RetrievalEvidenceInput::MAX_SOURCE_URI_BYTES {
        RetrievalEvidenceSource::Uri {
            uri: page.url.clone(),
        }
    } else {
        RetrievalEvidenceSource::Inline
    };
    passages
        .iter()
        .enumerate()
        .map(|(index, passage)| RetrievalEvidenceInput {
            rank: u16::try_from(index + 1).expect("passage limit fits u16"),
            source_token: passage.reference.source_token,
            document_id: stored.document_id,
            generation: stored.generation,
            chunk_id: ChunkId::derive(stored.document_id, passage.span.start, passage.span.end),
            span: passage.span,
            snippet: page.content[passage.span.start..passage.span.end].to_owned(),
            // A fetched page has no pages and no tree of its own: its stored
            // text is a rendering, not the document it came from, so the only
            // honest location is the span itself.
            location: EvidenceLocation::for_source_regions(heading_path.clone(), Vec::new()),
            source: source.clone(),
        })
        .collect()
}

/// The model-facing result of one extraction that became a source.
///
/// Prose rather than the JSON an inert fetch returned, because this is now a
/// source read: every other tool that hands out citation references — `search`,
/// `read_source` — frames its passages this way, and a citation directive
/// quoted inside a JSON string field is a directive the model has to unescape
/// before it can copy it.
pub(crate) fn extracted_page_result(
    page: &WebExtractResponse,
    document_id: DocumentId,
    passages: &[ExtractedPassage],
    format: CitationFormat,
) -> String {
    let mut content = String::with_capacity(page.content.len() + 1_024);
    content.push_str(&extraction_header(page));
    content.push_str(&format!("Document ID: {document_id}\n"));
    content.push_str(
        "This page is now a source in this conversation: read_source can reopen it and \
         search can match it.\n",
    );
    content.push_str(&citation_authoring_instruction(format, "passage"));
    content.push('\n');
    if passages.len() > 1 {
        content.push_str(
            "The passages below are listed in page order and each has its own reference. \
             Cite the passage a claim actually came from; a reference does not carry the \
             others.\n",
        );
    }
    for (index, passage) in passages.iter().enumerate() {
        let reference = format_citation_reference(format, "your phrasing", passage.reference);
        content.push_str(&format!(
            "\n--- Passage {} of {} · cite as: {reference} ---\n",
            index + 1,
            passages.len()
        ));
        content.push_str(&page.content[passage.span.start..passage.span.end]);
        content.push('\n');
    }
    content
}

/// The same result when the page could not be kept as a source.
///
/// The content is still worth returning — the fetch happened and the model can
/// read it — but nothing may be offered to cite, because there is no stored
/// text for a span to address. Saying so is the point: silence here would read
/// as a page that simply had nothing worth citing.
pub(crate) fn uncitable_page_result(page: &WebExtractResponse) -> String {
    format!(
        "{}This page was not kept as a source in this conversation, so it cannot be \
         cited. Attribute it by naming its URL instead.\n\n{}\n",
        extraction_header(page),
        page.content
    )
}

fn extraction_header(page: &WebExtractResponse) -> String {
    let title = if page.title.is_empty() {
        "Untitled page"
    } else {
        page.title.as_str()
    };
    let words = if page.truncated {
        format!("{} words, shortened to fit", page.word_count)
    } else {
        format!("{} words", page.word_count)
    };
    format!(
        "Fetched page: {title}\nURL: {}\nExtracted by: {}\nLength: {words}\n",
        page.url, page.extraction_method
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExtractionMethod;

    fn page(content: &str, truncated: bool) -> WebExtractResponse {
        WebExtractResponse::new(
            ExtractionMethod::Native,
            "https://example.com/article",
            "Ownership Explained",
            content,
            content.split_whitespace().count(),
            truncated,
        )
        .expect("the fixture URL is admissible")
    }

    /// The rule the whole module exists to guarantee: text either side of a
    /// truncation marker was never adjacent on the page, so no one reference may
    /// cover both. A span that crossed it would attribute a sentence to a page
    /// that never contained it next to the words before it.
    #[test]
    fn no_passage_spans_a_truncation_boundary() {
        let head = "alpha ".repeat(40);
        let tail = "omega ".repeat(40);
        let page = page(&format!("{head}{EXTRACT_TRUNCATION_MARKER}{tail}"), true);

        let passages = extracted_passages(&page.content, 32 * 1024);

        assert_eq!(passages.len(), 2);
        for passage in &passages {
            let quoted = &page.content[passage.span.start..passage.span.end];
            assert!(!quoted.contains(EXTRACT_TRUNCATION_MARKER.trim()));
            // Every passage quotes one side of the cut and nothing of the other.
            assert!(quoted.contains("alpha") != quoted.contains("omega"));
        }
        assert_eq!(
            &page.content[passages[0].span.start..passages[0].span.end],
            head
        );
        assert_eq!(
            &page.content[passages[1].span.start..passages[1].span.end],
            tail
        );
        // The marker itself belongs to neither passage.
        assert!(passages[0].span.end < passages[1].span.start);
    }

    /// A run longer than one snippet stays wholly reachable: the tail of a long
    /// article must be citable, not merely present.
    #[test]
    fn long_runs_are_windowed_rather_than_clipped() {
        let page = page(&"word ".repeat(100), false);

        let passages = extracted_passages(&page.content, 100);

        assert!(passages.len() > 1);
        assert_eq!(passages[0].span.start, 0);
        assert_eq!(
            passages
                .last()
                .expect("a windowed run has passages")
                .span
                .end,
            page.content.len()
        );
        // Contiguous and non-overlapping: every byte is citable exactly once.
        for pair in passages.windows(2) {
            assert_eq!(pair[0].span.end, pair[1].span.start);
        }
        // Distinct references, or the model could not tell the windows apart.
        let tokens: std::collections::HashSet<_> = passages
            .iter()
            .map(|passage| passage.reference.source_token)
            .collect();
        assert_eq!(tokens.len(), passages.len());
    }

    /// Multi-byte text must not be split mid-character, and the snippet a
    /// citation carries has to be exactly the bytes its span addresses.
    #[test]
    fn windows_land_on_character_boundaries_and_snippets_match_their_spans() {
        let page = page(&"🌊 é ".repeat(40), false);
        let stored = StoredExtractedPage {
            document_id: DocumentId::new(),
            generation: DocumentGeneration {
                content_revision: 1,
                revision_token: uuid::Uuid::new_v4(),
            },
        };

        let passages = extracted_passages(&page.content, 61);
        let evidence = extracted_evidence(&page, &stored, &passages);

        assert!(passages.len() > 1);
        for row in &evidence {
            assert_eq!(
                row.snippet,
                page.content[row.span.start..row.span.end],
                "the snippet must be the text its span addresses"
            );
            assert_eq!(row.document_id, stored.document_id);
            assert_eq!(row.generation, stored.generation);
        }
        assert_eq!(
            evidence
                .iter()
                .map(|row| row.snippet.as_str())
                .collect::<String>(),
            page.content
        );
    }

    /// The result must teach exactly one grammar, and must not offer a
    /// reference the turn's format did not ask for.
    #[test]
    fn the_result_teaches_only_the_turn_s_citation_format() {
        let page = page(&"word ".repeat(40), false);
        let passages = extracted_passages(&page.content, 32 * 1024);
        let document_id = DocumentId::new();

        let inline = extracted_page_result(&page, document_id, &passages, CitationFormat::Inline);
        assert!(inline.contains(&openwave_core::format_citation_directive(
            "your phrasing",
            passages[0].reference
        )));
        assert!(!inline.contains(&openwave_core::format_source_reference(
            passages[0].reference
        )));

        let attached = extracted_page_result(
            &page,
            document_id,
            &passages,
            CitationFormat::SourcesAttached,
        );
        assert!(attached.contains(&openwave_core::format_source_reference(
            passages[0].reference
        )));
        assert!(!attached.contains(":cit["));

        // Provenance a reader needs is on the result either way.
        for result in [&inline, &attached] {
            assert!(result.contains("https://example.com/article"));
            assert!(result.contains("Extracted by: native"));
            assert!(result.contains(&document_id.to_string()));
        }
    }

    /// A page that could not be stored must not be quietly presented as
    /// citable, and must offer nothing that looks like a reference.
    #[test]
    fn an_unstored_page_offers_no_reference_at_all() {
        let page = page(&"word ".repeat(40), false);

        let result = uncitable_page_result(&page);

        assert!(result.contains("cannot be cited"));
        assert!(!result.contains(":cit["));
        assert!(!result.contains("[[ow-source:"));
        assert!(result.contains(&page.content));
    }
}
