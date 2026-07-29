//! A [`DocumentParser`](crate::DocumentParser) that extracts Markdown canonical
//! text from Office documents by converting them to PDF with LibreOffice and
//! then parsing the PDF through the local, PDFium-backed [`liteparse`] engine.
//!
//! Gated behind the `parse-office` feature (which implies `parse-liteparse`).
//! LibreOffice is an **optional** system tool: `liteparse` shells out to it to
//! turn Word/Excel/PowerPoint/OpenDocument files into PDF. When LibreOffice is
//! not installed, this parser degrades gracefully — it yields an empty document
//! (stored and listed, but with no searchable text) instead of failing the
//! ingest, the same end state the [`FallbackParser`](crate::FallbackParser)
//! produces for undecodable binary. That keeps the lean pipeline usable on
//! machines without LibreOffice while making Office uploads searchable wherever
//! it is present.
//!
//! Conversion goes through `liteparse::parse_input`, which detects the format
//! from the bytes, drives LibreOffice, and parses the resulting PDF. OCR stays
//! disabled, so conversion needs no OCR runtime; a converted document with no
//! text layer simply yields little text rather than erroring.

use async_trait::async_trait;
use liteparse::types::PdfInput;
use liteparse::{LiteParse, LiteParseConfig, OutputFormat};

use crate::error::{Result, RetrievalError};
use crate::parse::{DocumentParser, ParsedDocument};

/// The Office media types this parser claims: Word, Excel, and PowerPoint in
/// both their legacy binary and OOXML forms, the OpenDocument equivalents, and
/// RTF. CSV/TSV are `text/*` and stay with the always-on `PlainTextParser`.
const OFFICE_MEDIA_TYPES: &[&str] = &[
    // Word processing
    "application/msword",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.oasis.opendocument.text",
    "application/rtf",
    // Spreadsheets
    "application/vnd.ms-excel",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.oasis.opendocument.spreadsheet",
    // Presentations
    "application/vnd.ms-powerpoint",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/vnd.oasis.opendocument.presentation",
];

/// Stable identity of this parser's canonical-text behavior. Bump the trailing
/// version tag whenever an implementation or configuration change can alter the
/// extracted text, so the durable pipeline reparses affected documents. The
/// fingerprint describes the intended pipeline and is deliberately independent
/// of whether LibreOffice happens to be installed at runtime.
const OFFICE_FINGERPRINT: &str = "liteparse:v2.8:office:libreoffice:markdown:no-ocr:pages:v2";

/// Extracts Markdown canonical text from Office documents via LibreOffice +
/// `liteparse`.
#[derive(Debug, Clone, Copy, Default)]
pub struct LiteParseOfficeParser;

impl LiteParseOfficeParser {
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

    /// Parse an already-supported document, given whether the LibreOffice
    /// conversion tool was found on this machine.
    ///
    /// Split out from [`DocumentParser::parse`] so the graceful-degradation
    /// policy — the branch taken when LibreOffice is absent — is exercised
    /// deterministically in tests regardless of what is installed in the test
    /// environment. When `libreoffice_available` is `false` this returns an
    /// empty document without touching `liteparse` or PDFium.
    async fn parse_when_available(
        &self,
        raw: &[u8],
        libreoffice_available: bool,
    ) -> Result<ParsedDocument> {
        if !libreoffice_available {
            // Degrade gracefully: no conversion tool, so there is no text to
            // extract. Store the document without failing the ingest.
            return Ok(ParsedDocument::default());
        }

        // Conversion produces a PDF that liteparse then parses via PDFium. Fail
        // closed if the PDFium runtime is unavailable, mirroring
        // `LiteParsePdfParser`: liteparse otherwise panics
        // (`load_default().expect(..)`) the first time it touches PDFium, which
        // would abort the parse task instead of marking the document failed.
        liteparse_pdfium_sys::dynamic::load_default().map_err(|error| {
            RetrievalError::parse(format!(
                "Office parsing is unavailable: the PDFium runtime library could not be loaded ({error})"
            ))
        })?;

        let result = LiteParse::new(Self::config())
            .parse_input(PdfInput::Bytes(raw.to_vec()))
            .await
            .map_err(|error| {
                RetrievalError::parse(format!(
                    "liteparse could not parse the office document: {error}"
                ))
            })?;
        Ok(crate::liteparse_regions::parsed_document_from(result))
    }
}

#[async_trait]
impl DocumentParser for LiteParseOfficeParser {
    fn fingerprint_for(&self, media_type: &str) -> Option<String> {
        self.supports(media_type)
            .then(|| OFFICE_FINGERPRINT.to_string())
    }

    fn supports(&self, media_type: &str) -> bool {
        OFFICE_MEDIA_TYPES.contains(&Self::base_media_type(media_type).as_str())
    }

    async fn parse(&self, raw: &[u8], media_type: &str) -> Result<ParsedDocument> {
        if !self.supports(media_type) {
            return Err(RetrievalError::parse(format!(
                "LiteParseOfficeParser does not support media type `{media_type}`"
            )));
        }
        let libreoffice_available = liteparse::conversion::find_libre_office_command()
            .await
            .is_some();
        self.parse_when_available(raw, libreoffice_available).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_office_types_ignoring_case_and_charset() {
        let parser = LiteParseOfficeParser::new();
        assert!(parser
            .supports("application/vnd.openxmlformats-officedocument.wordprocessingml.document"));
        assert!(
            parser.supports("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
        );
        assert!(parser
            .supports("application/vnd.openxmlformats-officedocument.presentationml.presentation"));
        assert!(parser.supports("application/msword"));
        assert!(parser.supports("application/vnd.ms-excel"));
        assert!(parser.supports("application/vnd.ms-powerpoint"));
        assert!(parser.supports("application/vnd.oasis.opendocument.text"));
        assert!(parser.supports("application/rtf"));
        // Case-insensitive and charset-tolerant.
        assert!(parser.supports("APPLICATION/MSWORD"));
        assert!(parser.supports("application/msword; charset=binary"));
    }

    #[test]
    fn does_not_support_pdf_text_or_images() {
        let parser = LiteParseOfficeParser::new();
        assert!(!parser.supports("application/pdf"));
        assert!(!parser.supports("text/plain"));
        assert!(!parser.supports("text/csv"));
        assert!(!parser.supports("image/png"));
        assert!(!parser.supports("application/octet-stream"));
        assert!(!parser.supports(""));
    }

    #[test]
    fn fingerprint_is_present_for_office_and_absent_otherwise() {
        let parser = LiteParseOfficeParser::new();
        assert_eq!(
            parser
                .fingerprint_for(
                    "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                )
                .as_deref(),
            Some(OFFICE_FINGERPRINT)
        );
        assert_eq!(parser.fingerprint_for("application/pdf"), None);
        assert_eq!(parser.fingerprint_for("text/plain"), None);
    }

    #[tokio::test]
    async fn rejects_unsupported_media_type_without_invoking_liteparse() {
        let parser = LiteParseOfficeParser::new();
        let error = parser
            .parse(b"not office bytes", "text/plain")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("does not support"));
    }

    #[tokio::test]
    async fn missing_libreoffice_degrades_to_empty_document() {
        // When the conversion tool is absent, ingest must not fail: the document
        // is stored with no searchable text rather than erroring or crashing.
        // Exercised directly so the graceful path is covered even on machines
        // that do have LibreOffice installed.
        let parser = LiteParseOfficeParser::new();
        let parsed = parser
            .parse_when_available(b"PK\x03\x04 fake docx bytes", false)
            .await
            .expect("absent conversion tool must degrade, not error");
        assert_eq!(parsed, ParsedDocument::default());
        assert!(parsed.text.is_empty());
        assert!(parsed.source_regions.is_empty());
    }

    /// Real end-to-end conversion: build a `.docx` in memory, run it through the
    /// LibreOffice → PDF → liteparse pipeline, and assert the source text comes
    /// back. Ignored by default because it needs both LibreOffice and the PDFium
    /// runtime, neither guaranteed in CI. Run locally with:
    /// `cargo test -p openwave-retrieval --features parse-office -- --ignored`.
    #[tokio::test]
    #[ignore = "requires LibreOffice and the PDFium runtime library"]
    async fn end_to_end_docx_conversion_extracts_text() {
        let needle = "OpenWaveOfficeIngestProbe";
        let docx = build_minimal_docx(needle);
        let parser = LiteParseOfficeParser::new();
        let parsed = parser
            .parse(
                &docx,
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            )
            .await
            .expect("docx conversion should succeed with LibreOffice + PDFium present");
        assert!(
            parsed.text.contains(needle),
            "expected converted text to contain {needle:?}, got: {:?}",
            parsed.text
        );
    }

    /// Assemble the smallest ZIP that LibreOffice accepts as a `.docx`: the
    /// content-types map, the package relationships, and a one-paragraph
    /// document body carrying `text`. Entries are stored (no compression) so the
    /// helper needs no zip crate.
    fn build_minimal_docx(text: &str) -> Vec<u8> {
        let document = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
<w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>"
        );
        let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
</Types>";
        let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
</Relationships>";
        stored_zip(&[
            ("[Content_Types].xml", content_types.as_bytes()),
            ("_rels/.rels", rels.as_bytes()),
            ("word/document.xml", document.as_bytes()),
        ])
    }

    /// Build a ZIP archive with stored (uncompressed) entries. Minimal but valid
    /// enough for LibreOffice to open — CRC-32 and sizes are filled in per entry.
    fn stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        fn crc32(data: &[u8]) -> u32 {
            let mut crc: u32 = 0xFFFF_FFFF;
            for &byte in data {
                crc ^= byte as u32;
                for _ in 0..8 {
                    let mask = (crc & 1).wrapping_neg();
                    crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
                }
            }
            !crc
        }

        let mut out = Vec::new();
        let mut central = Vec::new();
        let mut offsets = Vec::new();
        for (name, data) in entries {
            let nb = name.as_bytes();
            let crc = crc32(data);
            offsets.push(out.len() as u32);
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes()); // local file header sig
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
            out.extend_from_slice(&0u16.to_le_bytes()); // mod time
            out.extend_from_slice(&0u16.to_le_bytes()); // mod date
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // compressed size
            out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // uncompressed size
            out.extend_from_slice(&(nb.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra len
            out.extend_from_slice(nb);
            out.extend_from_slice(data);
        }
        for (i, (name, data)) in entries.iter().enumerate() {
            let nb = name.as_bytes();
            let crc = crc32(data);
            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes()); // central header sig
            central.extend_from_slice(&20u16.to_le_bytes()); // version made by
            central.extend_from_slice(&20u16.to_le_bytes()); // version needed
            central.extend_from_slice(&0u16.to_le_bytes()); // flags
            central.extend_from_slice(&0u16.to_le_bytes()); // method: stored
            central.extend_from_slice(&0u16.to_le_bytes()); // mod time
            central.extend_from_slice(&0u16.to_le_bytes()); // mod date
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(nb.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes()); // extra len
            central.extend_from_slice(&0u16.to_le_bytes()); // comment len
            central.extend_from_slice(&0u16.to_le_bytes()); // disk number
            central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            central.extend_from_slice(&offsets[i].to_le_bytes());
            central.extend_from_slice(nb);
        }
        let cd_offset = out.len() as u32;
        let cd_size = central.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes()); // EOCD sig
        out.extend_from_slice(&[0u8; 4]); // disk numbers
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        out
    }
}
