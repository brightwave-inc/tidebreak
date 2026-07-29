//! A [`DocumentParser`](crate::DocumentParser) that ingests raster image files
//! through the local, PDFium-backed [`liteparse`] engine.
//!
//! Gated behind the `parse-image` feature (which implies `parse-liteparse`).
//! `liteparse` converts an image into a single-page PDF that wraps the pixels as
//! an image XObject, then parses that PDF through PDFium. The conversion is done
//! in-process in pure Rust (via the `image` crate); despite `liteparse` labelling
//! this its "ImageMagick" path, no ImageMagick binary is invoked, so nothing
//! extra needs to be installed for the conversion itself.
//!
//! What the image path does *not* do — with OCR disabled, as it is here to avoid
//! a Tesseract runtime dependency — is read the text off the pixels. A converted
//! raster image has no PDF text layer, so `liteparse` returns only page
//! scaffolding, not the words in the image; a photograph or scanned page is
//! therefore stored and listed but effectively not searchable. That mirrors how
//! the sibling PDF and Office parsers already handle text-less documents. Making
//! image text searchable is a later slice (OCR); this parser claims the image
//! media types now so those uploads route here with a stable fingerprint instead
//! of falling through to the generic [`FallbackParser`](crate::FallbackParser).
//!
//! PDFium is the one runtime dependency. When its library cannot be loaded this
//! parser degrades to an empty document rather than crashing — `liteparse`
//! otherwise panics the first time it touches PDFium. The
//! [`parse_when_available`](LiteParseImageParser::parse_when_available) seam
//! makes that graceful-degradation branch deterministically testable offline,
//! regardless of whether PDFium is installed in the test environment.
//!
//! Conversion or parse failures degrade the same way. Because an OCR-off image
//! carries no recoverable text, a corrupt or undecodable image is stored as an
//! empty document (not searchable) instead of failing the ingest — the "import
//! anything, index what we can" contract the
//! [`FallbackParser`](crate::FallbackParser) applies to binary uploads. This is
//! the one deliberate difference from the PDF and Office parsers, which fail
//! closed on a `liteparse` error because those formats do carry real text worth
//! surfacing a failure over.

use async_trait::async_trait;
use liteparse::types::PdfInput;
use liteparse::{LiteParse, LiteParseConfig, OutputFormat};

use crate::error::{Result, RetrievalError};
use crate::parse::{DocumentParser, ParsedDocument};

/// The raster image media types this parser claims. Vector images (`image/svg+xml`)
/// are XML text and stay with the always-on `PlainTextParser`.
const IMAGE_MEDIA_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/gif",
    "image/tiff",
    "image/bmp",
];

/// Stable identity of this parser's canonical-text behavior. Bump the trailing
/// version tag whenever an implementation or configuration change can alter the
/// extracted text, so the durable pipeline reparses affected documents. The
/// fingerprint describes the intended pipeline (notably OCR-off) and is
/// deliberately independent of whether PDFium happens to be installed at runtime.
const IMAGE_FINGERPRINT: &str = "liteparse:v2.8:image:pdfium:markdown:no-ocr:positioned:v3";

/// Ingests raster images by converting them to a single-page PDF and parsing it
/// with `liteparse`. With OCR off it produces canonical text only when the image
/// carries an embedded text layer, which raster images do not — so today it
/// stores the document without searchable text.
#[derive(Debug, Clone, Copy, Default)]
pub struct LiteParseImageParser;

impl LiteParseImageParser {
    /// Construct the parser.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// The base media type, lowercased and stripped of any `; charset=…` suffix.
    /// MIME types are case-insensitive (RFC 6838).
    fn base_media_type(media_type: &str) -> String {
        media_type
            .split(';')
            .next()
            .unwrap_or(media_type)
            .trim()
            .to_ascii_lowercase()
    }

    /// The fixed parser configuration: Markdown output, OCR off, no progress.
    fn config() -> LiteParseConfig {
        LiteParseConfig {
            output_format: OutputFormat::Markdown,
            ocr_enabled: false,
            // A missing text layer is a legitimate (if unhelpful) document, not a
            // hard failure — keep whatever native text was recovered.
            ocr_failure_fatal: false,
            quiet: true,
            ..Default::default()
        }
    }

    /// Parse an already-supported image, given whether the PDFium runtime library
    /// could be loaded on this machine.
    ///
    /// Split out from [`DocumentParser::parse`] so the graceful-degradation
    /// policy — the branch taken when PDFium is absent — is exercised
    /// deterministically in tests regardless of what is installed in the test
    /// environment. When `pdfium_available` is `false` this returns an empty
    /// document without touching `liteparse` or PDFium, so the ingest is stored
    /// (not searchable) instead of panicking. Unlike the PDF and Office parsers,
    /// which fail closed when PDFium is missing, an OCR-off image yields no text
    /// whether or not PDFium is present, so degrading to empty here loses nothing
    /// and keeps the lean pipeline usable without PDFium.
    ///
    /// A conversion or parse failure degrades to an empty document for the same
    /// reason: an OCR-off image carries no recoverable text, so a corrupt or
    /// undecodable image is stored (not searchable) rather than failing the
    /// ingest.
    async fn parse_when_available(
        &self,
        raw: &[u8],
        pdfium_available: bool,
    ) -> Result<ParsedDocument> {
        if !pdfium_available {
            return Ok(ParsedDocument::default());
        }

        let parsed = LiteParse::new(Self::config())
            .parse_input(PdfInput::Bytes(raw.to_vec()))
            .await
            .map(crate::liteparse_regions::parsed_document_from)
            // Keep the ingest alive: an image liteparse cannot convert or parse
            // has no text to lose (OCR is off), so store it empty instead of
            // surfacing an error and marking the document failed.
            .unwrap_or_default();
        Ok(parsed)
    }
}

#[async_trait]
impl DocumentParser for LiteParseImageParser {
    fn fingerprint_for(&self, media_type: &str) -> Option<String> {
        self.supports(media_type)
            .then(|| IMAGE_FINGERPRINT.to_string())
    }

    fn supports(&self, media_type: &str) -> bool {
        IMAGE_MEDIA_TYPES.contains(&Self::base_media_type(media_type).as_str())
    }

    async fn parse(&self, raw: &[u8], media_type: &str) -> Result<ParsedDocument> {
        if !self.supports(media_type) {
            return Err(RetrievalError::parse(format!(
                "LiteParseImageParser does not support media type `{media_type}`"
            )));
        }
        // Probe the PDFium runtime before handing bytes to liteparse: it panics
        // (`load_default().expect(..)`) the first time it touches PDFium, which
        // would abort the parse task. A successful probe also primes liteparse's
        // own lazy load (shared, pinned `liteparse-pdfium-sys`); it is memoized
        // and cheap to repeat. When PDFium is absent we degrade to empty rather
        // than error — see `parse_when_available`.
        let pdfium_available = liteparse_pdfium_sys::dynamic::load_default().is_ok();
        self.parse_when_available(raw, pdfium_available).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_image_types_ignoring_case_and_charset() {
        let parser = LiteParseImageParser::new();
        assert!(parser.supports("image/png"));
        assert!(parser.supports("image/jpeg"));
        assert!(parser.supports("image/webp"));
        assert!(parser.supports("image/gif"));
        assert!(parser.supports("image/tiff"));
        assert!(parser.supports("image/bmp"));
        // Case-insensitive and charset-tolerant.
        assert!(parser.supports("IMAGE/PNG"));
        assert!(parser.supports("image/jpeg; charset=binary"));
    }

    #[test]
    fn does_not_support_pdf_office_text_or_svg() {
        let parser = LiteParseImageParser::new();
        assert!(!parser.supports("application/pdf"));
        assert!(!parser
            .supports("application/vnd.openxmlformats-officedocument.wordprocessingml.document"));
        assert!(!parser.supports("text/plain"));
        // SVG is XML text, handled by the plain-text parser, not this one.
        assert!(!parser.supports("image/svg+xml"));
        assert!(!parser.supports("application/octet-stream"));
        assert!(!parser.supports(""));
    }

    #[test]
    fn fingerprint_is_present_for_images_and_absent_otherwise() {
        let parser = LiteParseImageParser::new();
        assert_eq!(
            parser.fingerprint_for("image/png").as_deref(),
            Some(IMAGE_FINGERPRINT)
        );
        assert_eq!(parser.fingerprint_for("application/pdf"), None);
        assert_eq!(parser.fingerprint_for("text/plain"), None);
    }

    #[tokio::test]
    async fn rejects_unsupported_media_type_without_invoking_liteparse() {
        let parser = LiteParseImageParser::new();
        let error = parser
            .parse(b"not image bytes", "text/plain")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("does not support"));
    }

    #[tokio::test]
    async fn missing_pdfium_degrades_to_empty_document() {
        // When the PDFium runtime is absent, ingest must not fail or crash: the
        // document is stored with no searchable text rather than erroring.
        // Exercised directly so the graceful path is covered even on machines
        // that do have PDFium installed.
        let parser = LiteParseImageParser::new();
        let parsed = parser
            .parse_when_available(MINIMAL_PNG, false)
            .await
            .expect("absent PDFium runtime must degrade, not error");
        assert_eq!(parsed, ParsedDocument::default());
        assert!(parsed.text.is_empty());
        assert!(parsed.source_regions.is_empty());
    }

    /// Real end-to-end image ingest: convert a 1×1 PNG to PDF and parse it via
    /// PDFium. Ignored by default because it needs the PDFium runtime library,
    /// which is not guaranteed in CI. The point is that the image → PDF → PDFium
    /// pipeline runs to completion without panicking or erroring; with OCR off a
    /// raster image carries no real text, so `liteparse` returns only page
    /// scaffolding (an empty Markdown code fence) rather than searchable content.
    /// Run locally with:
    /// `cargo test -p openwave-retrieval --features parse-image -- --ignored`.
    #[tokio::test]
    #[ignore = "requires the PDFium runtime library"]
    async fn end_to_end_png_ingest_succeeds_without_crashing() {
        let parser = LiteParseImageParser::new();
        let parsed = parser
            .parse(MINIMAL_PNG, "image/png")
            .await
            .expect("PNG ingest should succeed with PDFium present");
        // No parser-supplied source regions on the text-extraction path. The
        // ingest completing (the `expect` above) without panicking is the real
        // assertion; with OCR off the extracted `text` is only page scaffolding.
        assert!(parsed.source_regions.is_empty());
    }

    /// A valid 1×1 red PNG (signature + IHDR + a zlib-compressed IDAT + IEND).
    const MINIMAL_PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2,
        0, 0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 0, 0,
        3, 1, 1, 0, 201, 254, 146, 239, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
}
