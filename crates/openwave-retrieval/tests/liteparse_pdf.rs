//! End-to-end check that the `parse-liteparse` parser extracts text from a real
//! PDF via the local PDFium-backed engine. Runs only when the feature is on.
#![cfg(feature = "parse-liteparse")]

use openwave_retrieval::{DocumentParser, LiteParsePdfParser, SourceLocation};

/// A minimal, valid single-page PDF whose text layer contains a known string.
const MINIMAL_PDF: &[u8] = include_bytes!("fixtures/minimal.pdf");

/// Two pages, each with its own marker string, so a page map that always says
/// "page one" is distinguishable from one that actually tracks the source.
const TWO_PAGE_PDF: &[u8] = include_bytes!("fixtures/two-page.pdf");

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extracts_text_layer_from_a_real_pdf() {
    let parser = LiteParsePdfParser::new();
    let parsed = parser
        .parse(MINIMAL_PDF, "application/pdf")
        .await
        .expect("liteparse should parse a valid text-layer PDF");

    assert!(
        parsed.text.contains("liteparse fixture"),
        "expected the PDF's text layer in the canonical output, got: {:?}",
        parsed.text
    );
}

/// The assertion the page pathway existed for but could never make: a passage
/// resolves to the page it is actually printed on, through a real parse.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn maps_each_passage_to_the_page_it_appears_on() {
    let parser = LiteParsePdfParser::new();
    let parsed = parser
        .parse(TWO_PAGE_PDF, "application/pdf")
        .await
        .expect("liteparse should parse a valid two-page PDF");

    // Resolve a marker's offset in canonical text to a page the way the
    // retrieval pipeline does: find the region whose span contains it.
    let page_of = |marker: &str| {
        let at = parsed
            .text
            .find(marker)
            .unwrap_or_else(|| panic!("{marker:?} missing from canonical text: {:?}", parsed.text));
        parsed
            .source_regions
            .iter()
            .find(|region| region.span.start <= at && at < region.span.end)
            .map(|region| match region.location {
                SourceLocation::Page { number, .. } => number.get(),
                #[allow(unreachable_patterns)]
                _ => panic!("expected a page location"),
            })
            .unwrap_or_else(|| panic!("no source region covers {marker:?}"))
    };

    assert_eq!(page_of("Alpha"), 1);
    assert_eq!(page_of("Bravo"), 2);
}

/// The geometry half: a real parse should place text on the page, not just name
/// the page. Asserts the rectangle is plausible rather than exact — the precise
/// numbers are PDFium's text metrics, which are not ours to pin.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn positions_text_within_the_page_it_appears_on() {
    let parser = LiteParsePdfParser::new();
    let parsed = parser
        .parse(TWO_PAGE_PDF, "application/pdf")
        .await
        .expect("liteparse should parse a valid two-page PDF");

    let positioned: Vec<_> = parsed
        .source_regions
        .iter()
        .filter_map(|region| match region.location {
            SourceLocation::Page { number, bounds } => bounds.map(|bounds| (number.get(), bounds)),
            #[allow(unreachable_patterns)]
            _ => None,
        })
        .collect();

    assert!(
        !positioned.is_empty(),
        "expected at least one positioned region, got: {:?}",
        parsed.source_regions
    );
    for (page, bounds) in &positioned {
        assert!(*page >= 1 && *page <= 2, "unexpected page {page}");
        assert!(
            bounds.is_valid(),
            "bounds must lie within the page: {bounds:?}"
        );
    }
    // The fixture draws its text near the top of the page (y = 700 of 792), so
    // a region placed in the bottom half would mean the vertical axis is
    // flipped — the classic PDF-coordinates bug, and one a viewer would show.
    let (_, first) = positioned.first().expect("checked non-empty");
    assert!(
        first.top < openwave_retrieval::PAGE_BOUNDS_SCALE / 2,
        "text drawn near the page top should not land in the bottom half: {first:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn surfaces_an_error_for_bytes_that_are_not_a_pdf() {
    let parser = LiteParsePdfParser::new();
    let result = parser
        .parse(b"this is plainly not a pdf", "application/pdf")
        .await;
    assert!(
        result.is_err(),
        "non-PDF bytes claimed as application/pdf should fail, not silently succeed"
    );
}
