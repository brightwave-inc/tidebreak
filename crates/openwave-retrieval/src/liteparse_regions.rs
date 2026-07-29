//! Turning a `liteparse` result into canonical text plus its source map.
//!
//! Shared by the PDF, Office and image parsers, which all reach canonical text
//! through the same `liteparse` pipeline and so all recover their pages the
//! same way.
//!
//! Two mappings at different confidences, which is why they are built
//! differently:
//!
//! **Pages are exact.** `liteparse` builds its full text by joining each page's
//! own rendered Markdown with a fixed separator and hands back both halves, so
//! walking the pieces in order reproduces the byte offsets of the join. If the
//! accounting does not add up to exactly the text we were given, we emit no
//! regions at all rather than citations that point at the wrong page.
//!
//! **Positions within a page are recovered.** `liteparse` knows where each line
//! sits on the page but does not offset-map its lines into the Markdown it
//! emits — the emitter rewrites bullets, rules and tables — so a line's span
//! has to be found by scanning the page's Markdown with a forward-only cursor.
//! Lines that do not turn up (systematically: rewritten bullets, rules,
//! reflowed table rows) simply get no rectangle. Every page is tiled either
//! way, so text a scan could not place still resolves to its page, and only
//! the finer geometry is lost.

use liteparse::types::Rect;
use liteparse::ParseResult;
use openwave_core::{ByteSpan, PageBounds, SourceLocation, SourceRegion, PAGE_BOUNDS_SCALE};

use crate::parse::ParsedDocument;

/// The separator `liteparse` joins per-page Markdown with. Mirrors the constant
/// in its Markdown output path; the accounting check below is what catches this
/// drifting on a version bump.
const PAGE_SEPARATOR: &str = "\n\n-----\n\n";

/// Most positioned regions to keep for a single page.
///
/// Bounds the region map stored alongside the document: without a limit a long
/// document of dense tables carries tens of thousands of rectangles, each of
/// which is read and rewritten on every chunking pass. A page that overruns
/// this keeps its page-level region and loses its geometry — pages that dense
/// are ones where a per-line highlight would be noise anyway.
const MAX_POSITIONED_REGIONS_PER_PAGE: usize = 256;

/// One page's contribution to canonical text, with the lines found on it.
struct PagePiece<'a> {
    number: usize,
    markdown: &'a str,
    lines: Vec<PositionedLine<'a>>,
}

/// A line of page text and where it sits, once normalized to the page box.
struct PositionedLine<'a> {
    text: &'a str,
    bounds: PageBounds,
}

/// Convert a `liteparse` result into canonical text carrying its source map.
pub(crate) fn parsed_document_from(result: ParseResult) -> ParsedDocument {
    let pieces: Vec<_> = result
        .pages
        .iter()
        .map(|page| PagePiece {
            number: page.page_number,
            markdown: page.markdown.as_str(),
            lines: page
                .projected_lines
                .iter()
                .filter_map(|line| {
                    Some(PositionedLine {
                        text: line.text.as_str(),
                        // A line we cannot place is not an error; it just does
                        // not contribute geometry.
                        bounds: normalized_bounds(&line.bbox, page.page_width, page.page_height)?,
                    })
                })
                .collect(),
        })
        .collect();
    let regions = source_regions(&result.text, &pieces);
    ParsedDocument::from_text(result.text).with_source_regions(regions)
}

/// Express a rectangle in page-relative coordinates.
///
/// Returns `None` for anything that would not survive the trip — a page with no
/// extent, non-finite coordinates, or a rectangle that normalizes to no area.
fn normalized_bounds(rect: &Rect, page_width: f32, page_height: f32) -> Option<PageBounds> {
    let left = fraction_of(rect.x, page_width)?;
    let top = fraction_of(rect.y, page_height)?;
    let right = fraction_of(rect.x + rect.width, page_width)?;
    let bottom = fraction_of(rect.y + rect.height, page_height)?;
    // Derive extent from the clamped edges so a rectangle overhanging the page
    // is trimmed to it rather than rejected or left pointing off-page.
    let bounds = PageBounds {
        left,
        top,
        width: right.checked_sub(left)?,
        height: bottom.checked_sub(top)?,
    };
    bounds.is_valid().then_some(bounds)
}

/// Position along one axis, as a fraction of the page's extent on that axis.
fn fraction_of(value: f32, extent: f32) -> Option<u16> {
    (value.is_finite() && extent.is_finite() && extent > 0.0).then(|| {
        // Clamped to the page, so the scaled value is always in `u16` range.
        let fraction = (value / extent).clamp(0.0, 1.0);
        (fraction * f32::from(PAGE_BOUNDS_SCALE)).round() as u16
    })
}

/// Build the document's source map from the pieces `text` was joined from.
///
/// Takes plain pieces rather than a `ParseResult` so the offset arithmetic —
/// the part that can silently produce wrong citations — is testable without
/// PDFium.
fn source_regions(text: &str, pages: &[PagePiece<'_>]) -> Vec<SourceRegion> {
    let mut regions = Vec::new();
    let mut offset = 0usize;
    for page in pages {
        let end = offset + page.markdown.len();
        // A page number is one-based upstream; a zero would mean liteparse
        // changed that contract, and a page we cannot name is one we cannot
        // cite. Drop the whole map rather than guess at a renumbering.
        let Some(number) = u32::try_from(page.number)
            .ok()
            .and_then(std::num::NonZeroU32::new)
        else {
            return Vec::new();
        };
        // Bail on any disagreement with the text we were handed: a page piece
        // that is not literally at the offset the join implies means the join
        // contract changed, and every span after it would be wrong.
        if end > text.len() || &text[offset..end] != page.markdown {
            return Vec::new();
        }
        // Skip empty pages: a region must be nonempty, and a gap where a blank
        // page sits is exactly what the region map is allowed to express.
        if !page.markdown.is_empty() {
            regions.extend(tile_page(offset, page, number));
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

/// Tile one page's span with regions, positioned where its lines were found.
///
/// The result covers the page's whole span with no gaps and no overlaps: the
/// lines the scan placed carry their rectangle, and everything between them is
/// covered by a region that names only the page.
fn tile_page(
    page_start: usize,
    page: &PagePiece<'_>,
    number: std::num::NonZeroU32,
) -> Vec<SourceRegion> {
    let markdown = page.markdown;
    let region = |start: usize, end: usize, bounds: Option<PageBounds>| SourceRegion {
        span: ByteSpan::new(page_start + start, page_start + end),
        location: SourceLocation::Page { number, bounds },
    };

    // Locate each line in the emitted Markdown with a forward-only cursor.
    // Forward-only is what keeps a repeated line ("Total", a page header) from
    // matching an earlier occurrence that another line already claimed.
    let mut placed: Vec<(usize, usize, PageBounds)> = Vec::new();
    let mut cursor = 0usize;
    for line in &page.lines {
        let needle = line.text.trim();
        if needle.is_empty() {
            continue;
        }
        let Some(at) = markdown[cursor..].find(needle) else {
            continue;
        };
        let start = cursor + at;
        placed.push((start, start + needle.len(), line.bounds));
        cursor = start + needle.len();
        if placed.len() == MAX_POSITIONED_REGIONS_PER_PAGE {
            // Too dense to be worth positioning: fall back to a bare page.
            return vec![region(0, markdown.len(), None)];
        }
    }

    // Fill the gaps so the page's span stays fully covered.
    let mut regions = Vec::with_capacity(placed.len() * 2 + 1);
    let mut filled = 0usize;
    for (start, end, bounds) in placed {
        if start > filled {
            regions.push(region(filled, start, None));
        }
        regions.push(region(start, end, Some(bounds)));
        filled = end;
    }
    if filled < markdown.len() {
        regions.push(region(filled, markdown.len(), None));
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

    fn bounds(left: u16, top: u16) -> PageBounds {
        PageBounds {
            left,
            top,
            width: 100,
            height: 100,
        }
    }

    /// Pages with no positioned lines — the page-map-only case.
    fn bare<'a>(pages: &'a [&'a str]) -> Vec<PagePiece<'a>> {
        pages
            .iter()
            .enumerate()
            .map(|(i, markdown)| PagePiece {
                number: i + 1,
                markdown,
                lines: Vec::new(),
            })
            .collect()
    }

    /// `SourceLocation` is `#[non_exhaustive]`, so destructuring it outside its
    /// own crate needs a match even though it has one variant today.
    fn page_of(region: &SourceRegion) -> (u32, Option<PageBounds>) {
        match region.location {
            SourceLocation::Page { number, bounds } => (number.get(), bounds),
            #[allow(unreachable_patterns)]
            _ => panic!("expected a page location"),
        }
    }

    /// Assert the map covers `text` exactly once: ordered, gapless, in bounds.
    /// Every case wants this, and a tiling bug is what would break it.
    fn assert_tiles(text: &str, regions: &[SourceRegion]) {
        openwave_core::validate_source_regions(text, regions).expect("regions must be valid");
        let mut covered = 0usize;
        for region in regions {
            assert_eq!(region.span.start, covered, "regions must leave no gap");
            covered = region.span.end;
        }
        assert_eq!(covered, text.len(), "regions must cover the whole text");
    }

    #[test]
    fn regions_span_each_page_in_the_joined_text() {
        let pages = ["# One\n\nalpha", "beta gamma", "## Three\n\ndelta"];
        let text = joined(&pages);
        let regions = source_regions(&text, &bare(&pages));

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
        let regions = source_regions(&text, &bare(&pages));

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
        assert!(source_regions(&text, &bare(&["alpha", "different"])).is_empty());
        assert!(source_regions(&text, &bare(&["alpha"])).is_empty());
        assert!(source_regions(&text, &bare(&["alpha", "beta", "extra"])).is_empty());
    }

    #[test]
    fn multibyte_pages_produce_char_boundary_spans() {
        let pages = ["café ☕", "naïve"];
        let text = joined(&pages);
        let regions = source_regions(&text, &bare(&pages));

        assert_eq!(regions.len(), 2);
        for region in &regions {
            assert!(text.is_char_boundary(region.span.start));
            assert!(text.is_char_boundary(region.span.end));
        }
        assert_eq!(&text[regions[1].span.start..regions[1].span.end], "naïve");
    }

    #[test]
    fn an_empty_document_yields_no_regions() {
        assert!(source_regions("", &[]).is_empty());
    }

    #[test]
    fn located_lines_carry_bounds_and_unmatched_text_still_carries_its_page() {
        // "middle" is not among the lines: it stands for the emitter's own
        // output (a rewritten bullet, a rule) that no line will ever match.
        let markdown = "alpha middle omega";
        let text = markdown.to_string();
        let regions = source_regions(
            &text,
            &[PagePiece {
                number: 1,
                markdown,
                lines: vec![
                    PositionedLine {
                        text: "alpha",
                        bounds: bounds(0, 0),
                    },
                    PositionedLine {
                        text: "omega",
                        bounds: bounds(0, 500),
                    },
                ],
            }],
        );

        assert_tiles(&text, &regions);
        let sliced: Vec<_> = regions
            .iter()
            .map(|r| (&text[r.span.start..r.span.end], page_of(r).1))
            .collect();
        assert_eq!(
            sliced,
            vec![
                ("alpha", Some(bounds(0, 0))),
                (" middle ", None),
                ("omega", Some(bounds(0, 500))),
            ]
        );
    }

    #[test]
    fn a_repeated_line_matches_forward_rather_than_reclaiming_an_earlier_one() {
        // Page headers and "Total" rows repeat. A backward match would give the
        // second occurrence the first one's rectangle — a highlight in the
        // wrong place on the page.
        let markdown = "Total x Total";
        let text = markdown.to_string();
        let regions = source_regions(
            &text,
            &[PagePiece {
                number: 1,
                markdown,
                lines: vec![
                    PositionedLine {
                        text: "Total",
                        bounds: bounds(0, 0),
                    },
                    PositionedLine {
                        text: "Total",
                        bounds: bounds(0, 900),
                    },
                ],
            }],
        );

        assert_tiles(&text, &regions);
        assert_eq!(regions.len(), 3);
        assert_eq!(regions[0].span, ByteSpan::new(0, 5));
        assert_eq!(page_of(&regions[0]).1, Some(bounds(0, 0)));
        // The second "Total" is the one at the end of the page, not the start.
        assert_eq!(regions[2].span, ByteSpan::new(8, 13));
        assert_eq!(page_of(&regions[2]).1, Some(bounds(0, 900)));
    }

    #[test]
    fn a_page_too_dense_to_position_keeps_its_page_and_drops_its_geometry() {
        let words: Vec<String> = (0..MAX_POSITIONED_REGIONS_PER_PAGE + 10)
            .map(|i| format!("w{i}"))
            .collect();
        let markdown = words.join(" ");
        let text = markdown.clone();
        let regions = source_regions(
            &text,
            &[PagePiece {
                number: 1,
                markdown: &markdown,
                lines: words
                    .iter()
                    .map(|w| PositionedLine {
                        text: w,
                        bounds: bounds(0, 0),
                    })
                    .collect(),
            }],
        );

        assert_tiles(&text, &regions);
        assert_eq!(regions.len(), 1);
        assert_eq!(page_of(&regions[0]), (1, None));
    }

    #[test]
    fn bounds_are_normalized_to_the_page_and_clamped_to_it() {
        let rect = Rect {
            x: 61.2,
            y: 79.2,
            width: 306.0,
            height: 79.2,
        };
        // A tenth in from the left, a tenth down, half the width, a tenth tall.
        assert_eq!(
            normalized_bounds(&rect, 612.0, 792.0),
            Some(PageBounds {
                left: 1_000,
                top: 1_000,
                width: 5_000,
                height: 1_000,
            })
        );

        // A rectangle overhanging the page is trimmed to it, not rejected: the
        // text is really there, and a highlight drawn off-page helps nobody.
        let overhang = Rect {
            x: 550.0,
            y: 0.0,
            width: 200.0,
            height: 79.2,
        };
        let trimmed = normalized_bounds(&overhang, 612.0, 792.0).expect("should trim to the page");
        assert!(trimmed.is_valid());
        assert_eq!(trimmed.left + trimmed.width, PAGE_BOUNDS_SCALE);
    }

    #[test]
    fn bounds_that_cannot_be_placed_are_dropped_rather_than_guessed() {
        let rect = Rect {
            x: 10.0,
            y: 10.0,
            width: 100.0,
            height: 100.0,
        };
        // A page with no extent gives nothing to normalize against.
        assert_eq!(normalized_bounds(&rect, 0.0, 792.0), None);
        assert_eq!(normalized_bounds(&rect, f32::NAN, 792.0), None);
        // A line with no area would be an invalid region, not a thin highlight.
        let flat = Rect {
            height: 0.0,
            ..rect
        };
        assert_eq!(normalized_bounds(&flat, 612.0, 792.0), None);
    }
}
