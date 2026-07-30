//! The document-parser seam: raw bytes in, plain text out.
//!
//! A [`DocumentParser`] turns a source file's bytes into canonical text. The
//! always-on [`PlainTextParser`] handles textual formats with zero dependencies.
//! Binary formats fall through to an empty canonical document until a future
//! execution-based ingestion path replaces the retired in-process adapters.
//!
//! Parsing is async so native or remote implementations can yield while doing
//! I/O. CPU-heavy parsers must still isolate blocking work at their own boundary;
//! production rich parsing belongs outside this in-process compatibility seam.

use crate::error::{Result, RetrievalError};

/// Ordered collection of parsers that dispatches by media type.
///
/// The first parser whose [`DocumentParser::supports`] method returns `true`
/// handles the document. Put narrow format parsers before broad fallbacks.
#[derive(Default)]
pub struct ParserRegistry {
    parsers: Vec<Box<dyn DocumentParser>>,
}

impl ParserRegistry {
    /// Construct an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a parser at the lowest dispatch priority.
    pub fn push(&mut self, parser: Box<dyn DocumentParser>) {
        self.parsers.push(parser);
    }

    /// Append a parser and return the registry for fluent construction.
    #[must_use]
    pub fn with_parser(mut self, parser: impl DocumentParser + 'static) -> Self {
        self.push(Box::new(parser));
        self
    }

    fn selected_parser(&self, media_type: &str) -> Option<&dyn DocumentParser> {
        self.parsers
            .iter()
            .find(|parser| parser.supports(media_type))
            .map(Box::as_ref)
    }
}

/// Assemble the production document parsers, narrowest first.
///
/// [`PlainTextParser`] claims `text/*`, JSON, and XML; [`FallbackParser`] claims
/// everything else so any upload remains storable. Unknown UTF-8 content stays
/// readable, while binary PDF, Office, workbook, and image bytes produce an
/// empty canonical document.
#[must_use]
pub fn document_parser_registry() -> ParserRegistry {
    ParserRegistry::new()
        .with_parser(PlainTextParser::new())
        .with_parser(FallbackParser::new())
}

#[async_trait::async_trait]
impl DocumentParser for ParserRegistry {
    fn supports(&self, media_type: &str) -> bool {
        self.selected_parser(media_type).is_some()
    }

    fn canonical_media_type(&self, media_type: &str) -> String {
        self.selected_parser(media_type).map_or_else(
            || media_type.to_string(),
            |parser| parser.canonical_media_type(media_type),
        )
    }

    async fn parse(&self, raw: &[u8], media_type: &str) -> Result<ParsedDocument> {
        let parser = self.selected_parser(media_type).ok_or_else(|| {
            RetrievalError::parse(format!(
                "no registered document parser supports media type `{media_type}`"
            ))
        })?;
        parser.parse(raw, media_type).await
    }
}

/// The plain text extracted from a source document.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedDocument {
    /// The canonical plain-text representation.
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
/// configuration.
#[async_trait::async_trait]
pub trait DocumentParser: Send + Sync {
    /// Whether this parser can handle the given media (MIME) type.
    fn supports(&self, media_type: &str) -> bool;

    /// The media type of the canonical text this parser produces for
    /// `media_type`.
    ///
    /// Defaults to the source's own type, which is right for parsers that pass
    /// text through. Parsers that convert must override it so downstream readers
    /// interpret the canonical text according to the format actually produced.
    fn canonical_media_type(&self, media_type: &str) -> String {
        media_type.to_string()
    }

    /// Parse `raw` bytes of the given media type into plain text.
    async fn parse(&self, raw: &[u8], media_type: &str) -> Result<ParsedDocument>;
}

/// The zero-dependency fallback parser: decodes bytes as UTF-8 text.
///
/// Accepts `text/*`, JSON, and XML media types. Invalid UTF-8 is repaired
/// lossily rather than rejected, so a stray bad byte never fails an
/// otherwise-fine document.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlainTextParser;

impl PlainTextParser {
    /// Construct the parser.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl DocumentParser for PlainTextParser {
    fn supports(&self, media_type: &str) -> bool {
        // Match the top-level `text` type, ignoring any `; charset=...` suffix.
        // MIME types are case-insensitive (RFC 6838), so compare in lowercase.
        let base = media_type.split(';').next().unwrap_or(media_type).trim();
        let base = base.to_ascii_lowercase();
        base.is_empty()
            || base.starts_with("text/")
            || base == "application/json"
            || base == "application/xml"
            || base.ends_with("+json")
            || (!base.starts_with("image/") && base.ends_with("+xml"))
    }

    async fn parse(&self, raw: &[u8], media_type: &str) -> Result<ParsedDocument> {
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

/// A last-resort parser that accepts **any** media type so no upload is rejected
/// for its format alone.
///
/// Register it after [`PlainTextParser`]: a document only reaches this fallback when nothing
/// narrower claimed it. Bytes that decode as valid UTF-8 become canonical text —
/// text-like uploads with an unknown media type (`.log`, `.yaml`, `.ndjson`, a
/// bare `application/octet-stream`) stay readable. Bytes that are not valid
/// UTF-8 are treated as binary: the document is still stored and listed, but its
/// canonical text is empty. The document remains available even when it cannot
/// be read as text.
#[derive(Debug, Clone, Copy, Default)]
pub struct FallbackParser;

impl FallbackParser {
    /// Construct the parser.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl DocumentParser for FallbackParser {
    fn supports(&self, _media_type: &str) -> bool {
        true
    }

    async fn parse(&self, raw: &[u8], _media_type: &str) -> Result<ParsedDocument> {
        // Decode only genuinely textual bytes; binary content is retained
        // without pretending it is readable text.
        let text = std::str::from_utf8(raw)
            .map(str::to_owned)
            .unwrap_or_default();
        Ok(ParsedDocument::from_text(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PrefixParser {
        media_type: &'static str,
        prefix: &'static str,
    }

    #[async_trait::async_trait]
    impl DocumentParser for PrefixParser {
        fn supports(&self, media_type: &str) -> bool {
            media_type == self.media_type
        }

        async fn parse(&self, raw: &[u8], _media_type: &str) -> Result<ParsedDocument> {
            Ok(ParsedDocument::from_text(format!(
                "{}{}",
                self.prefix,
                String::from_utf8_lossy(raw)
            )))
        }
    }

    #[tokio::test]
    async fn registry_dispatches_to_first_supporting_parser() {
        let registry = ParserRegistry::new()
            .with_parser(PrefixParser {
                media_type: "application/pdf",
                prefix: "first:",
            })
            .with_parser(PrefixParser {
                media_type: "application/pdf",
                prefix: "second:",
            })
            .with_parser(PlainTextParser::new());

        assert_eq!(
            registry
                .parse(b"pdf", "application/pdf")
                .await
                .unwrap()
                .text,
            "first:pdf"
        );
        assert_eq!(
            registry.parse(b"text", "text/plain").await.unwrap().text,
            "text"
        );
    }

    #[tokio::test]
    async fn registry_rejects_unsupported_media_without_invoking_a_parser() {
        let registry = ParserRegistry::new().with_parser(PlainTextParser::new());
        assert!(!registry.supports("application/pdf"));
        let error = registry
            .parse(b"%PDF", "application/pdf")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no registered document parser"));
    }

    #[test]
    fn plain_text_supports_text_and_structured_text_media() {
        let parser = PlainTextParser::new();
        for media_type in [
            "text/plain",
            "text/markdown; charset=utf-8",
            "TEXT/PLAIN",
            "",
            "application/json",
            "application/xml",
            "application/ld+json",
        ] {
            assert!(parser.supports(media_type), "{media_type}");
        }
        assert!(!parser.supports("application/pdf"));
        assert!(!parser.supports("image/png"));
    }

    #[tokio::test]
    async fn plain_text_decodes_utf8_lossily() {
        let parser = PlainTextParser::new();
        let parsed = parser
            .parse(&[b'a', 0xFF, b'b'], "text/plain")
            .await
            .unwrap();
        assert_eq!(parsed.text, "a\u{FFFD}b");
    }

    #[tokio::test]
    async fn fallback_keeps_utf8_readable_but_stores_binary_as_empty() {
        let parser = FallbackParser::new();
        for media_type in [
            "application/octet-stream",
            "image/png",
            "",
            "application/x-thing",
        ] {
            assert!(parser.supports(media_type));
        }
        let text = parser
            .parse(b"level=info msg=\"started\"", "application/octet-stream")
            .await
            .unwrap();
        assert_eq!(text.text, "level=info msg=\"started\"");
        let binary = parser
            .parse(&[0x00, 0xFF, 0x89, 0x50], "application/octet-stream")
            .await
            .unwrap();
        assert!(binary.text.is_empty());
    }

    #[tokio::test]
    async fn registry_with_fallback_accepts_any_media_type() {
        let registry = ParserRegistry::new()
            .with_parser(PlainTextParser::new())
            .with_parser(FallbackParser::new());
        assert!(registry.supports("application/vnd.custom"));
        assert_eq!(
            registry
                .parse(b"hello", "application/x-unknown")
                .await
                .unwrap()
                .text,
            "hello"
        );
    }
}
