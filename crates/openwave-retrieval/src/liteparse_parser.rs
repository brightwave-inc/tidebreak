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
//! `liteparse::parse_input` is PDF-centric; Office and image formats route
//! through separate LibreOffice/ImageMagick conversion in the upstream CLI and
//! are intentionally out of scope for this parser, which claims only
//! `application/pdf`.

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
const LITEPARSE_FINGERPRINT: &str = "liteparse:v2.8:pdf:markdown:no-ocr:v1";

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

    async fn parse(&self, raw: &[u8], media_type: &str) -> Result<ParsedDocument> {
        if !self.supports(media_type) {
            return Err(RetrievalError::parse(format!(
                "LiteParsePdfParser does not support media type `{media_type}`"
            )));
        }
        let result = LiteParse::new(Self::config())
            .parse_input(PdfInput::Bytes(raw.to_vec()))
            .await
            .map_err(|error| {
                RetrievalError::parse(format!("liteparse could not parse the PDF: {error}"))
            })?;
        Ok(ParsedDocument::from_text(result.text))
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

    #[tokio::test]
    async fn rejects_unsupported_media_type_without_invoking_liteparse() {
        let parser = LiteParsePdfParser::new();
        let error = parser.parse(b"not a pdf", "text/plain").await.unwrap_err();
        assert!(error.to_string().contains("does not support"));
    }
}
