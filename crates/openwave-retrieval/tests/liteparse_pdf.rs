//! End-to-end check that the `parse-liteparse` parser extracts text from a real
//! PDF via the local PDFium-backed engine. Runs only when the feature is on.
#![cfg(feature = "parse-liteparse")]

use openwave_retrieval::{DocumentParser, LiteParsePdfParser};

/// A minimal, valid single-page PDF whose text layer contains a known string.
const MINIMAL_PDF: &[u8] = include_bytes!("fixtures/minimal.pdf");

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
