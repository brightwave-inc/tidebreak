//! The document-parser seam: raw bytes in, plain text out.
//!
//! A [`DocumentParser`] turns a source file's bytes into the canonical plain text
//! that chunking and embedding operate on. The always-on [`PlainTextParser`]
//! handles `text/*` (plain, markdown) with zero dependencies. Rich formats
//! (PDF/office/images) will arrive later as a feature-gated parser behind this
//! same trait, so the pipeline never has to care which parser produced the text.
//!
//! The trait is synchronous: naive text decoding is CPU-only, and a future
//! parser that shells out to native tooling can wrap itself in `spawn_blocking`
//! at its own boundary rather than forcing every caller onto an async path.

use crate::error::{Result, RetrievalError};

/// The plain text extracted from a source document, plus any parser-supplied
/// metadata. Kept as a struct (not a bare `String`) so richer parsers can attach
/// structure — page maps, headings, bounding boxes — without a breaking change.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedDocument {
    /// The canonical plain-text representation. Chunk spans index into this.
    pub text: String,
}

impl ParsedDocument {
    /// Wrap already-extracted text.
    pub fn from_text(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

/// Turns a document's raw bytes into plain text.
///
/// Object-safe so parsers can be held as `Box<dyn DocumentParser>` and swapped by
/// configuration (naive today, liteparse-backed later).
pub trait DocumentParser: Send + Sync {
    /// Whether this parser can handle the given media (MIME) type.
    fn supports(&self, media_type: &str) -> bool;

    /// Parse `raw` bytes of the given media type into plain text.
    fn parse(&self, raw: &[u8], media_type: &str) -> Result<ParsedDocument>;
}

/// The zero-dependency fallback parser: decodes bytes as UTF-8 text.
///
/// Accepts `text/*` media types (covers `text/plain` and `text/markdown` — the
/// latter is already human-readable, so it passes through untouched). Invalid
/// UTF-8 is repaired lossily rather than rejected, so a stray bad byte never
/// fails an otherwise-fine document.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlainTextParser;

impl PlainTextParser {
    /// Construct the parser.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl DocumentParser for PlainTextParser {
    fn supports(&self, media_type: &str) -> bool {
        // Match the top-level `text` type, ignoring any `; charset=...` suffix.
        // MIME types are case-insensitive (RFC 6838), so compare in lowercase.
        let base = media_type.split(';').next().unwrap_or(media_type).trim();
        base.is_empty() || base.to_ascii_lowercase().starts_with("text/")
    }

    fn parse(&self, raw: &[u8], media_type: &str) -> Result<ParsedDocument> {
        if !self.supports(media_type) {
            return Err(RetrievalError::parse(format!(
                "PlainTextParser does not support media type `{media_type}`"
            )));
        }
        // Lossy decode: replace invalid sequences instead of failing the ingest.
        let text = String::from_utf8_lossy(raw).into_owned();
        Ok(ParsedDocument::from_text(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_text_types_and_empty_but_not_binary() {
        let p = PlainTextParser::new();
        assert!(p.supports("text/plain"));
        assert!(p.supports("text/markdown"));
        assert!(p.supports("text/markdown; charset=utf-8"));
        assert!(p.supports("TEXT/PLAIN")); // MIME types are case-insensitive
        assert!(p.supports("Text/Markdown"));
        assert!(p.supports("")); // unknown/omitted => treat as text
        assert!(!p.supports("application/pdf"));
        assert!(!p.supports("image/png"));
    }

    #[test]
    fn parses_utf8_bytes_verbatim() {
        let p = PlainTextParser::new();
        let parsed = p
            .parse("# Title\n\nbody".as_bytes(), "text/markdown")
            .unwrap();
        assert_eq!(parsed.text, "# Title\n\nbody");
    }

    #[test]
    fn repairs_invalid_utf8_lossily() {
        let p = PlainTextParser::new();
        let parsed = p.parse(&[b'a', 0xFF, b'b'], "text/plain").unwrap();
        // The lone 0xFF becomes U+FFFD; surrounding text survives.
        assert!(parsed.text.starts_with('a'));
        assert!(parsed.text.ends_with('b'));
        assert!(parsed.text.contains('\u{FFFD}'));
    }

    #[test]
    fn rejects_unsupported_media_type() {
        let p = PlainTextParser::new();
        assert!(p.parse(b"%PDF-1.7", "application/pdf").is_err());
    }
}
