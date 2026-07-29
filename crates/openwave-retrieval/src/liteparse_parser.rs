//! A [`DocumentParser`](crate::DocumentParser) that extracts Markdown canonical
//! text from PDFs using the local, PDFium-backed [`liteparse`] engine.
//!
//! Gated behind the `parse-liteparse` feature because it pulls a large
//! dependency tree. OCR is disabled (the crate is built with
//! `default-features = false`), so text-layer PDFs parse fully offline with no
//! system OCR dependency. Image-only / scanned pages yield little or no text
//! rather than failing — the retrieval pipeline treats a text-sparse document as
//! a valid, if unhelpful, ingest instead of an error.
//!
//! This parser claims only `application/pdf`. Office formats route through
//! LibreOffice conversion in the sibling `LiteParseOfficeParser` (feature
//! `parse-office`); images remain out of scope for now.

use async_trait::async_trait;
use liteparse::types::PdfInput;
use liteparse::{LiteParse, LiteParseConfig, OutputFormat};

use crate::error::{Result, RetrievalError};
use crate::parse::{DocumentParser, ParsedDocument};

/// The one media type this parser claims.
const PDF_MEDIA_TYPE: &str = "application/pdf";

/// Stable identity of this parser's canonical-text behavior. Bump the trailing
/// version tag whenever an implementation or configuration change can alter the
/// extracted text, so the durable pipeline reparses affected documents.
const LITEPARSE_FINGERPRINT: &str = "liteparse:v2.8:pdf:markdown:no-ocr:pages:v2";

/// Extracts Markdown canonical text from `application/pdf` bytes via `liteparse`.
#[derive(Debug, Clone, Copy, Default)]
pub struct LiteParsePdfParser;

impl LiteParsePdfParser {
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
}

#[async_trait]
impl DocumentParser for LiteParsePdfParser {
    fn fingerprint_for(&self, media_type: &str) -> Option<String> {
        self.supports(media_type)
            .then(|| LITEPARSE_FINGERPRINT.to_string())
    }

    fn supports(&self, media_type: &str) -> bool {
        Self::base_media_type(media_type) == PDF_MEDIA_TYPE
    }

    /// `liteparse` is configured for Markdown output, so a parsed
    /// PDF carries Markdown regardless of the source's own type.
    fn canonical_media_type(&self, _media_type: &str) -> String {
        "text/markdown".to_string()
    }

    async fn parse(&self, raw: &[u8], media_type: &str) -> Result<ParsedDocument> {
        if !self.supports(media_type) {
            return Err(RetrievalError::parse(format!(
                "LiteParsePdfParser does not support media type `{media_type}`"
            )));
        }
        // Fail closed if the PDFium runtime library is unavailable. liteparse
        // otherwise panics (`load_default().expect(..)`) the first time it
        // touches PDFium, which would abort the parse task instead of marking
        // the document failed. This probe shares liteparse's runtime binding
        // (same pinned `liteparse-pdfium-sys`), so a success here also primes
        // liteparse's own lazy load; it is memoized and cheap to repeat.
        liteparse_pdfium_sys::dynamic::load_default().map_err(|error| {
            RetrievalError::parse(format!(
                "PDF parsing is unavailable: the PDFium runtime library could not be loaded ({error})"
            ))
        })?;
        let result = LiteParse::new(Self::config())
            .parse_input(PdfInput::Bytes(raw.to_vec()))
            .await
            .map_err(|error| {
                RetrievalError::parse(format!("liteparse could not parse the PDF: {error}"))
            })?;
        Ok(crate::liteparse_regions::parsed_document_from(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_only_pdf_ignoring_case_and_charset() {
        let parser = LiteParsePdfParser::new();
        assert!(parser.supports("application/pdf"));
        assert!(parser.supports("APPLICATION/PDF"));
        assert!(parser.supports("application/pdf; charset=binary"));
        assert!(!parser.supports("text/plain"));
        assert!(!parser
            .supports("application/vnd.openxmlformats-officedocument.wordprocessingml.document"));
        assert!(!parser.supports("image/png"));
        assert!(!parser.supports(""));
    }

    #[test]
    fn fingerprint_is_present_for_pdf_and_absent_otherwise() {
        let parser = LiteParsePdfParser::new();
        assert_eq!(
            parser.fingerprint_for("application/pdf").as_deref(),
            Some(LITEPARSE_FINGERPRINT)
        );
        assert_eq!(parser.fingerprint_for("text/plain"), None);
    }

    #[test]
    fn canonical_text_is_declared_markdown_not_the_source_type() {
        // The chunker partitions Markdown at headings, and can only know to do
        // that for a PDF if the parser says what it emitted.
        let parser = LiteParsePdfParser::new();
        assert_eq!(
            parser.canonical_media_type("application/pdf"),
            "text/markdown"
        );
    }

    #[tokio::test]
    async fn rejects_unsupported_media_type_without_invoking_liteparse() {
        let parser = LiteParsePdfParser::new();
        let error = parser.parse(b"not a pdf", "text/plain").await.unwrap_err();
        assert!(error.to_string().contains("does not support"));
    }
}
