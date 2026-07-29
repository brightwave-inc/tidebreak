//! Turning a `liteparse` result into canonical text plus its page map.
//!
//! Shared by the PDF, Office and image parsers, which all reach canonical text
//! through the same `liteparse` pipeline and so all recover pages the same way.
//!
//! The mapping is exact rather than inferred. `liteparse` builds its full text
//! by joining each page's own rendered Markdown with a fixed separator and
//! hands back both halves — the joined text and the per-page pieces. Walking
//! the pieces in order therefore reproduces the byte offsets of the join, so a
//! page's [`SourceRegion`] is the span the page's text actually occupies. No
//! substring search, no fuzzy matching: if the accounting does not add up to
//! exactly the text we were given, we emit no regions at all rather than
//! citations that point at the wrong page.

use liteparse::ParseResult;
use openwave_core::{ByteSpan, SourceLocation, SourceRegion};

use crate::parse::ParsedDocument;

/// The separator `liteparse` joins per-page Markdown with. Mirrors the constant
/// in its Markdown output path; the accounting check below is what catches this
/// drifting on a version bump.
const PAGE_SEPARATOR: &str = "\n\n-----\n\n";

/// Convert a `liteparse` result into canonical text carrying one region per page.
pub(crate) fn parsed_document_from(result: ParseResult) -> ParsedDocument {
    let regions = page_regions(
        &result.text,
        result
            .pages
            .iter()
            .map(|page| (page.page_number, page.markdown.as_str())),
    );
    ParsedDocument::from_text(result.text).with_source_regions(regions)
}

/// Compute one [`SourceRegion`] per page from the pieces `text` was joined from.
///
/// Takes the pieces rather than a `ParseResult` so the offset arithmetic — the
/// part that can silently produce wrong citations — is testable without PDFium.
fn page_regions<'a>(
    text: &str,
    pages: impl Iterator<Item = (usize, &'a str)>,
) -> Vec<SourceRegion> {
    let mut regions = Vec::new();
    let mut offset = 0usize;
    for (page_number, markdown) in pages {
        let end = offset + markdown.len();
        // A page number is one-based upstream; a zero would mean liteparse
        // changed that contract, and a page we cannot name is one we cannot
        // cite. Drop the whole map rather than guess at a renumbering.
        let Some(number) = u32::try_from(page_number)
            .ok()
            .and_then(std::num::NonZeroU32::new)
        else {
            return Vec::new();
        };
        // Bail on any disagreement with the text we were handed: a page piece
        // that is not literally at the offset the join implies means the join
        // contract changed, and every span after it would be wrong.
        if end > text.len() || &text[offset..end] != markdown {
            return Vec::new();
        }
        // Skip empty pages: a region must be nonempty, and a gap where a blank
        // page sits is exactly what the region map is allowed to express.
        if !markdown.is_empty() {
            regions.push(SourceRegion {
                span: ByteSpan::new(offset, end),
                location: SourceLocation::Page {
                    number,
                    // Page granularity for now — `liteparse` exposes per-line
                    // boxes, but they are not offset-mapped into the emitted
                    // Markdown, so resolving a span to a rectangle needs a
                    // matching pass this does not attempt.
                    bounds: None,
                },
            });
        }
        offset = end + PAGE_SEPARATOR.len();
    }
    // The final page contributes no trailing separator, so the running offset
    // should have overshot the text by exactly one separator. Anything else
    // means we did not account for the whole document.
    if offset != text.len() + PAGE_SEPARATOR.len() && !(text.is_empty() && offset == 0) {
        return Vec::new();
    }
    regions
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Join pages the way liteparse does, so the fixture and the code under
    /// test cannot drift apart within a case.
    fn joined(pages: &[&str]) -> String {
        pages.join(PAGE_SEPARATOR)
    }

    fn numbered<'a>(pages: &'a [&'a str]) -> impl Iterator<Item = (usize, &'a str)> {
        pages.iter().enumerate().map(|(i, p)| (i + 1, *p))
    }

    /// `SourceLocation` is `#[non_exhaustive]`, so destructuring it outside its
    /// own crate needs a match even though it has one variant today.
    fn page_of(region: &SourceRegion) -> (u32, Option<openwave_core::PageBounds>) {
        match region.location {
            SourceLocation::Page { number, bounds } => (number.get(), bounds),
            #[allow(unreachable_patterns)]
            _ => panic!("expected a page location"),
        }
    }

    #[test]
    fn regions_span_each_page_in_the_joined_text() {
        let pages = ["# One\n\nalpha", "beta gamma", "## Three\n\ndelta"];
        let text = joined(&pages);
        let regions = page_regions(&text, numbered(&pages));

        assert_eq!(regions.len(), 3);
        for (index, region) in regions.iter().enumerate() {
            // The point of the whole map: the span must slice back to the page.
            assert_eq!(&text[region.span.start..region.span.end], pages[index]);
            let (number, bounds) = page_of(region);
            assert_eq!(number, index as u32 + 1);
            assert_eq!(bounds, None);
        }
    }

    #[test]
    fn blank_pages_become_gaps_and_keep_later_pages_aligned() {
        let pages = ["alpha", "", "gamma"];
        let text = joined(&pages);
        let regions = page_regions(&text, numbered(&pages));

        // The empty page yields no region, but still shifts page three's span.
        assert_eq!(regions.len(), 2);
        assert_eq!(&text[regions[1].span.start..regions[1].span.end], "gamma");
        assert_eq!(page_of(&regions[1]).0, 3);
    }

    #[test]
    fn a_page_map_that_does_not_reconstruct_the_text_is_dropped() {
        // Stands in for liteparse changing its join contract on a version bump:
        // wrong spans would silently cite the wrong page, so emit nothing.
        let text = joined(&["alpha", "beta"]);
        assert!(page_regions(&text, numbered(&["alpha", "different"])).is_empty());
        assert!(page_regions(&text, numbered(&["alpha"])).is_empty());
        assert!(page_regions(&text, numbered(&["alpha", "beta", "extra"])).is_empty());
    }

    #[test]
    fn multibyte_pages_produce_char_boundary_spans() {
        let pages = ["café ☕", "naïve"];
        let text = joined(&pages);
        let regions = page_regions(&text, numbered(&pages));

        assert_eq!(regions.len(), 2);
        for region in &regions {
            assert!(text.is_char_boundary(region.span.start));
            assert!(text.is_char_boundary(region.span.end));
        }
        assert_eq!(&text[regions[1].span.start..regions[1].span.end], "naïve");
    }

    #[test]
    fn regions_validate_against_the_canonical_text() {
        let pages = ["alpha", "beta"];
        let text = joined(&pages);
        // The pipeline validates parser output before storing it; page maps
        // must satisfy the same ordering and boundary rules as any other.
        openwave_core::validate_source_regions(&text, &page_regions(&text, numbered(&pages)))
            .expect("page regions must be valid source regions");
    }

    #[test]
    fn an_empty_document_yields_no_regions() {
        assert!(page_regions("", std::iter::empty()).is_empty());
    }
}
