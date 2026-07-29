//! Closed source-reference grammar for durable assistant citations.
//!
//! Search tools expose opaque references to the model, which cites a passage by
//! wrapping its own phrasing in a `:cit[…]{ref=…}` directive. The agent
//! resolves those references and rewrites each directive to its durable
//! identity — `:cit[…]{citation_id=…}` — so the cited phrase stays where the
//! model put it. Renderer clients never see the opaque token or the call
//! identity behind it.

use std::collections::BTreeSet;

use crate::RetrievalEvidenceInput;
use crate::{
    AssistantCitationId, DocumentId, EvidenceLocation, MessageId, PageBounds, SourceLocation,
    SourceRegion, StructuredPathType,
};

const SOURCE_REFERENCE_PREFIX: &str = "[[ow-source:";
const SOURCE_REFERENCE_SUFFIX: &str = "]]";
const SOURCE_TOKEN_LEN: usize = 32;

/// Opens the inline citation directive, ahead of the cited phrase.
const CITATION_DIRECTIVE_PREFIX: &str = ":cit[";
/// Closes the cited phrase and opens the model-facing reference attribute.
const CITATION_REFERENCE_ATTRIBUTE: &str = "]{ref=";
/// Closes the cited phrase and opens the durable, renderer-facing attribute.
const CITATION_ID_ATTRIBUTE: &str = "]{citation_id=";
const CITATION_ATTRIBUTE_SUFFIX: &str = "}";

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
    /// Where the passage sits in its source, in the terms that source is
    /// addressed by.
    pub location: CitationLocation,
}

/// Where a citation points, projected per evidence kind.
///
/// The discriminant is the renderer's instruction for how to open the passage:
/// pages and rectangles address a paginated document, a cell range addresses a
/// sheet, and a path addresses a node. Only document content is produced today.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CitationLocation {
    /// A passage of canonical text, at `pages` of the source it was parsed
    /// from.
    DocumentContent {
        pages: Vec<u32>,
        /// Where on those pages the passage sits, for sources whose parser
        /// resolved it that finely. Empty for page-granular sources; `pages`
        /// is the complete answer either way.
        bounds: Vec<CitationPageBounds>,
    },
    /// A cell or rectangular range on one sheet of a workbook.
    SpreadsheetCellRange {
        start_cell: String,
        end_cell: Option<String>,
        sheet_index: i32,
        sheet_name: String,
    },
    /// A node of a structured document, addressed by path.
    StructuredPath {
        path: String,
        path_type: StructuredPathType,
    },
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

/// Project the renderer's view of where a citation points, from the immutable
/// location of the evidence it was made from.
///
/// Only document content is reshaped: its regions become the pages and
/// rectangles a viewer paints. The other kinds already address their source in
/// the terms a reader opens it by, so they travel across unchanged.
pub(crate) fn project_citation_location(location: &EvidenceLocation) -> CitationLocation {
    match location {
        EvidenceLocation::DocumentContent { source_regions, .. } => {
            let (pages, bounds) = project_citation_pages(source_regions);
            CitationLocation::DocumentContent { pages, bounds }
        }
        EvidenceLocation::SpreadsheetCellRange {
            start_cell,
            end_cell,
            sheet_index,
            sheet_name,
        } => CitationLocation::SpreadsheetCellRange {
            start_cell: start_cell.clone(),
            end_cell: end_cell.clone(),
            sheet_index: *sheet_index,
            sheet_name: sheet_name.clone(),
        },
        EvidenceLocation::StructuredPath { path, path_type } => CitationLocation::StructuredPath {
            path: path.clone(),
            path_type: *path_type,
        },
    }
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
        // A region that names something other than a page has no page to
        // paint; document-content evidence never carries one, and a location
        // this projection does not recognize is left out rather than guessed at.
        let SourceLocation::Page { number, bounds } = region.location else {
            continue;
        };
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

/// Produce the exact citation directive a search result asks the model to
/// author around the phrasing that a passage supports.
#[must_use]
pub fn format_citation_directive(phrase: &str, reference: AssistantCitationReference) -> String {
    format!(
        "{CITATION_DIRECTIVE_PREFIX}{phrase}{CITATION_REFERENCE_ATTRIBUTE}{}{CITATION_ATTRIBUTE_SUFFIX}",
        reference.source_token.simple()
    )
}

/// How a turn asks the model to cite the sources it read.
///
/// Only the authoring instruction changes. Both forms resolve through the same
/// grammar and land in the same durable shape — an ordered reference list, with
/// inline directives as an optional layer on top — so one conversation can hold
/// messages authored under either, and each keeps rendering as it was written.
///
/// Persisted per chat as the token from [`Self::as_str`], with an absent value
/// meaning "follow the global default".
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
pub enum CitationFormat {
    /// Anchor each claim: the model wraps its own phrasing in a citation
    /// directive, so a reader sees which words a source backs.
    #[default]
    Inline,
    /// Answer plainly and cite by bare reference: nothing is anchored in the
    /// prose, and the sources surface only in the row at the foot of the
    /// message.
    SourcesAttached,
}

impl CitationFormat {
    /// Every format, in the order a picker lists them.
    pub const ALL: &'static [Self] = &[Self::Inline, Self::SourcesAttached];

    /// The wire/storage token for this format.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::SourcesAttached => "sources_attached",
        }
    }

    /// Parse a stored/wire token back into a format.
    ///
    /// Deliberately returns `Option` (an unknown token falls back to the
    /// default rather than failing a turn), so this is not the `FromStr` trait.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|format| format.as_str() == value)
    }
}

/// The exact reference a result hands the model to copy under `format`.
///
/// Inline gets the directive that carries the phrasing it supports; the
/// sources-attached form gets the bare marker, which the parser strips from the
/// answer and keeps only in the reference order.
#[must_use]
pub fn format_citation_reference(
    format: CitationFormat,
    phrase: &str,
    reference: AssistantCitationReference,
) -> String {
    match format {
        CitationFormat::Inline => format_citation_directive(phrase, reference),
        CitationFormat::SourcesAttached => format_source_reference(reference),
    }
}

/// The sentence a tool result uses to teach `format`, where `subject` names
/// what the result offers — a "passage", a "range".
#[must_use]
pub fn citation_authoring_instruction(format: CitationFormat, subject: &str) -> String {
    match format {
        CitationFormat::Inline => format!(
            "To cite a {subject}, wrap the wording it supports in that {subject}'s citation \
             directive: your phrasing goes in the brackets and may paraphrase the {subject}, \
             and the reference is copied exactly."
        ),
        CitationFormat::SourcesAttached => format!(
            "To cite a {subject}, write the answer plainly and put that {subject}'s reference \
             immediately after the sentence it supports: wrap no phrasing, and copy the \
             reference exactly. The sources you cite are listed at the end of the message."
        ),
    }
}

/// Resolve reserved references without interpreting Markdown.
///
/// A citation directive whose token is well formed keeps its cited phrase and
/// is rewritten to the durable identity the phrase will be stored under; a bare
/// reserved token is removed from user-visible text, as it always has been.
/// Exact lowercase-hex tokens become typed references; unknown tokens can be
/// ignored by storage. Duplicates retain their first position — and their first
/// citation identity — and the result is bounded independently of provider
/// output size.
///
/// `message_id` is the identity the parsed content will be stored under, since
/// a citation's identity is derived from its message and its first-use ordinal.
#[must_use]
pub fn parse_assistant_citations(text: &str, message_id: MessageId) -> ParsedAssistantCitations {
    let mut content = String::with_capacity(text.len());
    let mut references: Vec<AssistantCitationReference> = Vec::new();
    let mut remainder = text;

    loop {
        // Whichever form opens first; the two openings cannot start at the same
        // offset because their first bytes differ.
        let (start, directive_first) = match (
            remainder.find(SOURCE_REFERENCE_PREFIX),
            remainder.find(CITATION_DIRECTIVE_PREFIX),
        ) {
            (Some(marker), Some(directive)) => (marker.min(directive), directive < marker),
            (Some(marker), None) => (marker, false),
            (None, Some(directive)) => (directive, true),
            (None, None) => break,
        };
        content.push_str(&remainder[..start]);
        let candidate = &remainder[start..];
        if directive_first {
            let Some(directive) = split_citation_directive(candidate) else {
                // Not a directive after all: release the opening and rescan from
                // inside it, so directive-like prose is preserved and a bare
                // reference embedded in the would-be phrase is still stripped.
                content.push_str(CITATION_DIRECTIVE_PREFIX);
                remainder = &candidate[CITATION_DIRECTIVE_PREFIX.len()..];
                continue;
            };
            match first_use_ordinal(&mut references, directive.reference) {
                // A directive with nothing to mark is just a bare marker.
                Some(_) if directive.phrase.is_empty() => {}
                Some(ordinal) => content.push_str(&format_bound_citation_directive(
                    directive.phrase,
                    AssistantCitationId::derive(message_id, ordinal),
                )),
                // Past the citation bound the prose is still the model's; only
                // the citation is dropped.
                None => content.push_str(directive.phrase),
            }
            remainder = directive.rest;
            continue;
        }
        let token = &candidate[SOURCE_REFERENCE_PREFIX.len()..];
        let Some(payload) = token.get(..SOURCE_TOKEN_LEN) else {
            content.push_str(SOURCE_REFERENCE_PREFIX);
            remainder = token;
            continue;
        };
        let Some(after_payload) = token.get(SOURCE_TOKEN_LEN..) else {
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
        // A bare reference marks no phrase, so it is only recorded.
        let _ = first_use_ordinal(&mut references, reference);
        remainder = &after_payload[SOURCE_REFERENCE_SUFFIX.len()..];
    }
    content.push_str(remainder);

    ParsedAssistantCitations {
        content,
        references,
    }
}

/// Re-derive the citation identities embedded in parsed content for a different
/// message identity.
///
/// A citation is identified by its message and ordinal, so content persisted
/// under an identity other than the one it was parsed for would otherwise carry
/// ids the stored message does not own.
pub(crate) fn rebind_citation_ids(
    content: &str,
    from: MessageId,
    to: MessageId,
    citations: usize,
) -> String {
    let mut rebound = content.to_owned();
    if from == to {
        return rebound;
    }
    for ordinal in 1..=citations.min(MAX_ASSISTANT_CITATIONS) {
        let ordinal = u16::try_from(ordinal).expect("citation limit fits u16");
        rebound = rebound.replace(
            &AssistantCitationId::derive(from, ordinal).to_string(),
            &AssistantCitationId::derive(to, ordinal).to_string(),
        );
    }
    rebound
}

struct CitationDirective<'a> {
    phrase: &'a str,
    reference: AssistantCitationReference,
    rest: &'a str,
}

/// Split a well-formed authoring directive off the front of `candidate`.
///
/// The first `]` closes the cited phrase: telling a bracket inside the phrase
/// from the directive's own would take a Markdown parse, so a phrase carrying
/// one degrades to literal prose instead of being guessed at.
fn split_citation_directive(candidate: &str) -> Option<CitationDirective<'_>> {
    let opened = candidate.strip_prefix(CITATION_DIRECTIVE_PREFIX)?;
    let (phrase, closed) = opened.split_at(opened.find(']')?);
    let attribute = closed.strip_prefix(CITATION_REFERENCE_ATTRIBUTE)?;
    let payload = attribute.get(..SOURCE_TOKEN_LEN)?;
    let rest = attribute
        .get(SOURCE_TOKEN_LEN..)?
        .strip_prefix(CITATION_ATTRIBUTE_SUFFIX)?;
    Some(CitationDirective {
        phrase,
        reference: parse_reference_payload(payload)?,
        rest,
    })
}

fn format_bound_citation_directive(phrase: &str, id: AssistantCitationId) -> String {
    format!(
        "{CITATION_DIRECTIVE_PREFIX}{phrase}{CITATION_ID_ATTRIBUTE}{id}{CITATION_ATTRIBUTE_SUFFIX}"
    )
}

/// The one-based position `reference` holds in first-use order, recording it if
/// this is its first use and the citation bound leaves room for it.
fn first_use_ordinal(
    references: &mut Vec<AssistantCitationReference>,
    reference: AssistantCitationReference,
) -> Option<u16> {
    let position = match references.iter().position(|seen| *seen == reference) {
        Some(position) => position,
        None if references.len() < MAX_ASSISTANT_CITATIONS => {
            references.push(reference);
            references.len() - 1
        }
        None => return None,
    };
    Some(u16::try_from(position + 1).expect("citation limit fits u16"))
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

    /// The stored form of the citation a directive with `reference` becomes,
    /// wrapping `phrase`, when it is the `ordinal`-th distinct citation of
    /// `message_id`.
    fn bound(phrase: &str, message_id: MessageId, ordinal: u16) -> String {
        format!(
            ":cit[{phrase}]{{citation_id={}}}",
            AssistantCitationId::derive(message_id, ordinal)
        )
    }

    #[test]
    fn directives_keep_their_phrase_and_carry_one_identity_per_evidence() {
        let message_id = MessageId::new();
        let first = AssistantCitationReference {
            source_token: uuid::Uuid::new_v4(),
        };
        let second = AssistantCitationReference {
            source_token: uuid::Uuid::new_v4(),
        };
        let text = format!(
            "{} and {}, plus {}.",
            format_citation_directive("the sky is blue", first),
            format_citation_directive("water is wet", second),
            format_citation_directive("still blue", first),
        );

        let parsed = parse_assistant_citations(&text, message_id);

        assert_eq!(
            parsed.content,
            format!(
                "{} and {}, plus {}.",
                bound("the sky is blue", message_id, 1),
                bound("water is wet", message_id, 2),
                bound("still blue", message_id, 1),
            )
        );
        assert_eq!(parsed.references, [first, second]);
        assert!(!parsed
            .content
            .contains(&first.source_token.simple().to_string()));
    }

    #[test]
    fn rebinding_matches_parsing_for_the_other_message() {
        let parsed_for = MessageId::new();
        let stored_under = MessageId::new();
        let first = AssistantCitationReference {
            source_token: uuid::Uuid::new_v4(),
        };
        let second = AssistantCitationReference {
            source_token: uuid::Uuid::new_v4(),
        };
        let text = format!(
            "Grounded {} and {}.",
            format_citation_directive("claim", first),
            format_citation_directive("other claim", second),
        );

        let parsed = parse_assistant_citations(&text, parsed_for);
        let rebound = rebind_citation_ids(
            &parsed.content,
            parsed_for,
            stored_under,
            parsed.references.len(),
        );

        assert_eq!(
            rebound,
            parse_assistant_citations(&text, stored_under).content
        );
    }

    #[test]
    fn malformed_directives_degrade_to_prose() {
        let message_id = MessageId::new();
        let reference = AssistantCitationReference {
            source_token: uuid::Uuid::new_v4(),
        };
        let token = reference.source_token.simple().to_string();

        // An unterminated phrase and an unparseable token are ordinary prose.
        let unparseable = ":cit[unterminated and :cit[bad]{ref=nope} too";
        let parsed = parse_assistant_citations(unparseable, message_id);
        assert_eq!(parsed.content, unparseable);
        assert!(parsed.references.is_empty());

        // A bracket inside the phrase closes it early, so the directive
        // degrades — and a bare marker caught inside it is still stripped.
        let nested = format!(
            ":cit[nested {} bracket]{{ref={token}}}",
            format_source_reference(reference)
        );
        let parsed = parse_assistant_citations(&nested, message_id);
        assert_eq!(
            parsed.content,
            format!(":cit[nested  bracket]{{ref={token}}}")
        );
        assert_eq!(parsed.references, [reference]);

        // A directive with no phrase to mark resolves like a bare marker.
        let empty = format!("Answer{}", format_citation_directive("", reference));
        let parsed = parse_assistant_citations(&empty, message_id);
        assert_eq!(parsed.content, "Answer");
        assert_eq!(parsed.references, [reference]);
    }

    #[test]
    fn directives_past_the_citation_bound_keep_only_their_phrase() {
        let message_id = MessageId::new();
        let references = (0..MAX_ASSISTANT_CITATIONS + 1)
            .map(|_| AssistantCitationReference {
                source_token: uuid::Uuid::new_v4(),
            })
            .collect::<Vec<_>>();
        let text = references
            .iter()
            .map(|reference| format_citation_directive("phrase", *reference))
            .collect::<String>();

        let parsed = parse_assistant_citations(&text, message_id);

        assert_eq!(parsed.references, references[..MAX_ASSISTANT_CITATIONS]);
        assert!(parsed.content.ends_with("phrase"));
        assert_eq!(
            parsed.content.matches(":cit[").count(),
            MAX_ASSISTANT_CITATIONS
        );
    }

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
        let parsed = parse_assistant_citations(&text, MessageId::new());
        assert_eq!(parsed.content, "Grounded  answer ");
        assert_eq!(parsed.references, [first, second]);
    }

    #[test]
    fn parser_strips_well_formed_unknown_tokens_and_preserves_malformed_text() {
        let parsed = parse_assistant_citations(
            " before [[ow-source:not-a-token]] middle \
             [[ow-source:00000000000000000000000000000000]] after \
             [[ow-source:still-being-written ",
            MessageId::new(),
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
        let parsed = parse_assistant_citations(&text, MessageId::new());
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
        let parsed = parse_assistant_citations(&text, MessageId::new());
        assert!(parsed.content.is_empty());
        assert_eq!(parsed.references, references[..MAX_ASSISTANT_CITATIONS]);
    }
}
