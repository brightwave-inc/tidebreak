//! Closed source-reference grammar for durable assistant citations.
//!
//! Search tools expose opaque references to the model. The agent removes those
//! references from final text and hands only their typed identities to storage;
//! renderer clients never see the grammar or its underlying call identity.

use std::collections::{BTreeSet, HashSet};

use crate::RetrievalEvidenceInput;
use crate::{AssistantCitationId, DocumentId, MessageId, PageBounds, SourceLocation, SourceRegion};

const SOURCE_REFERENCE_PREFIX: &str = "[[ow-source:";
const SOURCE_REFERENCE_SUFFIX: &str = "]]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceReferenceCandidate {
    Possible,
    Complete,
    Invalid,
}

/// Most distinct evidence rows one assistant message may cite.
pub const MAX_ASSISTANT_CITATIONS: usize = RetrievalEvidenceInput::MAX_RESULTS;
pub const MAX_CITATION_EXCERPT_CHARS: usize = 600;
pub const MAX_CITATION_HEADING_CHARS: usize = 160;
pub const MAX_CITATION_PAGES: usize = 8;
/// Most highlight rectangles one citation carries to the renderer.
///
/// Larger than [`MAX_CITATION_PAGES`] because a passage is drawn line by line:
/// a handful of pages can easily be twenty rectangles. Overflow costs the
/// citation nothing beyond precision — the pages it spans are still listed.
pub const MAX_CITATION_BOUNDS: usize = 32;

/// One opaque reference selected by a model from a search result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AssistantCitationReference {
    pub source_token: uuid::Uuid,
}

/// Final assistant text after reserved references have been removed, plus the
/// first-use ordering of references that can be resolved by durable storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAssistantCitations {
    pub content: String,
    pub references: Vec<AssistantCitationReference>,
}

/// Renderer-safe historical citation projected from immutable evidence.
///
/// The source identity and canonical-text span travel with the excerpt because
/// a citation is a position in a document, not just a quotation of one: without
/// them a reader can only be shown the words again, never where they came from.
/// Neither is a capability — the document id already addresses the source panel,
/// and the span only means anything against text the same client can already
/// read.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
pub struct AssistantCitationSnapshot {
    pub id: AssistantCitationId,
    #[serde(skip)]
    pub message_id: MessageId,
    pub ordinal: u16,
    /// The cited source, addressable as a document panel.
    pub document_id: DocumentId,
    /// Half-open byte range of the cited passage in that document's canonical
    /// text, which is the text the extracted-text view renders.
    pub span: CitationSpan,
    pub excerpt: String,
    pub heading: Option<String>,
    pub pages: Vec<u32>,
    /// Where on those pages the passage sits, for sources whose parser resolved
    /// it that finely. Empty for page-granular sources; `pages` is the complete
    /// answer either way.
    pub bounds: Vec<CitationPageBounds>,
}

/// One highlight rectangle of a citation, on a named page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
pub struct CitationPageBounds {
    /// One-based page the rectangle falls on.
    pub page: u32,
    /// The rectangle, in that page's normalized coordinate space.
    pub bounds: PageBounds,
}

/// A citation's byte range, projected for the renderer.
///
/// [`crate::ByteSpan`] is `usize`, which is a host-width detail rather than part
/// of a wire contract; canonical text is bounded well inside `u32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
pub struct CitationSpan {
    /// Inclusive start byte offset.
    pub start: u32,
    /// Exclusive end byte offset.
    pub end: u32,
}

/// Project the pages and highlight rectangles a citation shows, from the
/// immutable source regions of the evidence it was made from.
///
/// Pages keep the regions' own order and stay first-seen distinct. Rectangles
/// are ordered by page and then by position down and across it, which is the
/// order a viewer paints them in, and identical rectangles collapse: regions
/// are per-span, so one visual line quoted twice is one highlight.
pub(crate) fn project_citation_pages(
    regions: &[SourceRegion],
) -> (Vec<u32>, Vec<CitationPageBounds>) {
    let mut pages = Vec::new();
    // Ordered and deduplicated by construction. The leading key is
    // (page, top, left); width and height only break ties between rectangles
    // that start at the same point.
    let mut rects = BTreeSet::new();
    for region in regions {
        let SourceLocation::Page { number, bounds } = region.location;
        let page = number.get();
        if pages.len() < MAX_CITATION_PAGES && !pages.contains(&page) {
            pages.push(page);
        }
        if let Some(bounds) = bounds {
            rects.insert((page, bounds.top, bounds.left, bounds.width, bounds.height));
        }
    }
    let bounds = rects
        .into_iter()
        .take(MAX_CITATION_BOUNDS)
        .map(|(page, top, left, width, height)| CitationPageBounds {
            page,
            bounds: PageBounds {
                left,
                top,
                width,
                height,
            },
        })
        .collect();
    (pages, bounds)
}

/// Produce the exact closed reference a search result gives to the model.
#[must_use]
pub fn format_source_reference(reference: AssistantCitationReference) -> String {
    format!(
        "{SOURCE_REFERENCE_PREFIX}{}{SOURCE_REFERENCE_SUFFIX}",
        reference.source_token.simple()
    )
}

/// Strip reserved references without interpreting Markdown.
///
/// Structurally complete reserved tokens are always removed from user-visible
/// text. Exact lowercase-hex tokens become typed references; unknown tokens can
/// be ignored by storage. Duplicates retain their first
/// position and the result is bounded independently of provider output size.
#[must_use]
pub fn parse_assistant_citations(text: &str) -> ParsedAssistantCitations {
    let mut content = String::with_capacity(text.len());
    let mut references = Vec::new();
    let mut seen = HashSet::new();
    let mut remainder = text;

    while let Some(start) = remainder.find(SOURCE_REFERENCE_PREFIX) {
        content.push_str(&remainder[..start]);
        let token = &remainder[start + SOURCE_REFERENCE_PREFIX.len()..];
        let Some(payload) = token.get(..32) else {
            content.push_str(SOURCE_REFERENCE_PREFIX);
            remainder = token;
            continue;
        };
        let Some(after_payload) = token.get(32..) else {
            unreachable!("a 32-byte token prefix has a remainder")
        };
        let Some(reference) = after_payload
            .starts_with(SOURCE_REFERENCE_SUFFIX)
            .then(|| parse_reference_payload(payload))
            .flatten()
        else {
            content.push_str(SOURCE_REFERENCE_PREFIX);
            remainder = token;
            continue;
        };
        if references.len() < MAX_ASSISTANT_CITATIONS && seen.insert(reference) {
            references.push(reference);
        }
        remainder = &after_payload[SOURCE_REFERENCE_SUFFIX.len()..];
    }
    content.push_str(remainder);

    ParsedAssistantCitations {
        content,
        references,
    }
}

pub(crate) fn classify_source_reference_candidate(candidate: &str) -> SourceReferenceCandidate {
    if candidate.len() < SOURCE_REFERENCE_PREFIX.len() {
        return if SOURCE_REFERENCE_PREFIX.starts_with(candidate) {
            SourceReferenceCandidate::Possible
        } else {
            SourceReferenceCandidate::Invalid
        };
    }
    if !candidate.starts_with(SOURCE_REFERENCE_PREFIX) {
        return SourceReferenceCandidate::Invalid;
    }
    let remainder = &candidate[SOURCE_REFERENCE_PREFIX.len()..];
    let payload_len = remainder.len().min(32);
    if !remainder.as_bytes()[..payload_len]
        .iter()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return SourceReferenceCandidate::Invalid;
    }
    if remainder.len() < 32 {
        return SourceReferenceCandidate::Possible;
    }
    let suffix = &remainder[32..];
    if suffix == SOURCE_REFERENCE_SUFFIX {
        SourceReferenceCandidate::Complete
    } else if SOURCE_REFERENCE_SUFFIX.starts_with(suffix) {
        SourceReferenceCandidate::Possible
    } else {
        SourceReferenceCandidate::Invalid
    }
}

fn parse_reference_payload(payload: &str) -> Option<AssistantCitationReference> {
    if payload.len() != 32
        || !payload
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let source_token = uuid::Uuid::parse_str(payload).ok()?;
    Some(AssistantCitationReference { source_token })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_strips_and_deduplicates_exact_references_without_markdown() {
        let first = AssistantCitationReference {
            source_token: uuid::Uuid::new_v4(),
        };
        let second = AssistantCitationReference {
            source_token: uuid::Uuid::new_v4(),
        };
        let text = format!(
            "Grounded {first_ref} answer {second_ref}{first_ref}",
            first_ref = format_source_reference(first),
            second_ref = format_source_reference(second),
        );
        let parsed = parse_assistant_citations(&text);
        assert_eq!(parsed.content, "Grounded  answer ");
        assert_eq!(parsed.references, [first, second]);
    }

    #[test]
    fn parser_strips_well_formed_unknown_tokens_and_preserves_malformed_text() {
        let parsed = parse_assistant_citations(
            " before [[ow-source:not-a-token]] middle \
             [[ow-source:00000000000000000000000000000000]] after \
             [[ow-source:still-being-written ",
        );
        assert_eq!(
            parsed.references,
            [AssistantCitationReference {
                source_token: uuid::Uuid::nil(),
            }]
        );
        assert_eq!(
            parsed.content,
            " before [[ow-source:not-a-token]] middle  after [[ow-source:still-being-written "
        );
    }

    #[test]
    fn parser_preserves_whitespace_and_every_near_miss() {
        let valid = AssistantCitationReference {
            source_token: uuid::Uuid::new_v4(),
        };
        let valid_marker = format_source_reference(valid);
        let text = format!(
            "\n[[ow-source:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa]]\n\
             [[ow-source:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb]]\n\
             [[ow-source:CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC]]\n\
             [[ow-source:short ordinary paragraph and a later ]] marker\n\
             {valid_marker} tail 🌊\n"
        );
        let parsed = parse_assistant_citations(&text);
        assert_eq!(parsed.references, [valid]);
        assert_eq!(parsed.content, text.replace(&valid_marker, ""));
    }

    /// Evidence may carry many more regions than a highlight list should, and
    /// the renderer reads the list in order — so the overflow that gets dropped
    /// has to be the tail, not an arbitrary subset.
    #[test]
    fn page_bounds_are_bounded_from_the_front_of_the_reading_order() {
        let regions = (0..MAX_CITATION_BOUNDS + 4)
            .rev()
            .map(|index| SourceRegion {
                span: crate::ByteSpan::new(index, index + 1),
                location: SourceLocation::Page {
                    number: std::num::NonZeroU32::new(1).expect("page one is nonzero"),
                    bounds: Some(PageBounds {
                        left: 0,
                        top: u16::try_from(index).expect("test row fits u16"),
                        width: 1_000,
                        height: 1,
                    }),
                },
            })
            .collect::<Vec<_>>();
        let (_, bounds) = project_citation_pages(&regions);
        assert_eq!(bounds.len(), MAX_CITATION_BOUNDS);
        assert_eq!(bounds[0].bounds.top, 0);
        assert_eq!(
            bounds[MAX_CITATION_BOUNDS - 1].bounds.top,
            u16::try_from(MAX_CITATION_BOUNDS - 1).expect("the cap fits u16")
        );
    }

    #[test]
    fn parser_bounds_distinct_references_but_strips_all_tokens() {
        let references = (0..MAX_ASSISTANT_CITATIONS + 2)
            .map(|_| AssistantCitationReference {
                source_token: uuid::Uuid::new_v4(),
            })
            .collect::<Vec<_>>();
        let text = references
            .iter()
            .map(|reference| format_source_reference(*reference))
            .collect::<String>();
        let parsed = parse_assistant_citations(&text);
        assert!(parsed.content.is_empty());
        assert_eq!(parsed.references, references[..MAX_ASSISTANT_CITATIONS]);
    }
}
