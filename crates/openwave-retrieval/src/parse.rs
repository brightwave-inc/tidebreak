//! The document-parser seam: raw bytes in, plain text out.
//!
//! A [`DocumentParser`] turns a source file's bytes into the canonical plain text
//! that chunking and embedding operate on. The always-on [`PlainTextParser`]
//! handles `text/*` (plain, markdown) with zero dependencies. Rich formats
//! (PDF/office/images) will arrive later as a feature-gated parser behind this
//! same trait, so the pipeline never has to care which parser produced the text.
//!
//! Parsing is async so native or remote implementations can yield while doing
//! I/O. CPU-heavy parsers must still isolate blocking work at their own boundary;
//! production rich parsing belongs in the durable document worker.

use crate::document::SourceRegion;
use crate::error::{Result, RetrievalError};

/// Ordered collection of parsers that dispatches by media type.
///
/// The first parser whose [`DocumentParser::supports`] method returns `true`
/// handles the document. Put narrow rich-format parsers before broad fallbacks.
/// This lets applications enable PDF/Office parsers without coupling the ingest
/// pipeline to a particular parsing library.
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

#[async_trait::async_trait]
impl DocumentParser for ParserRegistry {
    fn fingerprint_for(&self, media_type: &str) -> Option<String> {
        self.selected_parser(media_type)?
            .fingerprint_for(media_type)
    }

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

/// The plain text extracted from a source document, plus any parser-supplied
/// metadata. Kept as a struct (not a bare `String`) so richer parsers can attach
/// structure — page maps, headings, bounding boxes — without a breaking change.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedDocument {
    /// The canonical plain-text representation. Chunk spans index into this.
    pub text: String,
    /// Mappings from canonical text to locations in the original source.
    pub source_regions: Vec<SourceRegion>,
}

impl ParsedDocument {
    /// Wrap already-extracted text.
    pub fn from_text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            source_regions: Vec::new(),
        }
    }

    /// Attach parser-produced source regions.
    #[must_use]
    pub fn with_source_regions(mut self, source_regions: Vec<SourceRegion>) -> Self {
        self.source_regions = source_regions;
        self
    }
}

/// Turns a document's raw bytes into plain text.
///
/// Object-safe so parsers can be held as `Box<dyn DocumentParser>` and swapped by
/// configuration (naive today, liteparse-backed later).
#[async_trait::async_trait]
pub trait DocumentParser: Send + Sync {
    /// Stable identity for canonical-text behavior for `media_type`.
    ///
    /// Returns `None` exactly when [`supports`](Self::supports) returns `false`.
    /// Custom parsers should override this whenever an implementation change can
    /// alter canonical text. The value must remain stable for this parser
    /// instance's lifetime; runtime reconfiguration requires a new instance.
    fn fingerprint_for(&self, media_type: &str) -> Option<String> {
        self.supports(media_type)
            .then(|| format!("custom-parser:type={}", std::any::type_name::<Self>()))
    }

    /// Whether this parser can handle the given media (MIME) type.
    fn supports(&self, media_type: &str) -> bool;

    /// The media type of the canonical text this parser produces for
    /// `media_type`.
    ///
    /// Defaults to the source's own type, which is right for parsers that pass
    /// text through. Parsers that *convert* must override it: a PDF's canonical
    /// text is Markdown, and downstream stages that treat canonical text
    /// according to its format — chunking at headings, for one — have no other
    /// way to know that. Getting this wrong is silent: the text is still
    /// indexed, just handled as though it had no structure.
    fn canonical_media_type(&self, media_type: &str) -> String {
        media_type.to_string()
    }

    /// Parse `raw` bytes of the given media type into plain text.
    async fn parse(&self, raw: &[u8], media_type: &str) -> Result<ParsedDocument>;
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

#[async_trait::async_trait]
impl DocumentParser for PlainTextParser {
    fn fingerprint_for(&self, media_type: &str) -> Option<String> {
        self.supports(media_type)
            .then(|| "plain-text-lossy-v1".to_string())
    }

    fn supports(&self, media_type: &str) -> bool {
        // Match the top-level `text` type, ignoring any `; charset=...` suffix.
        // MIME types are case-insensitive (RFC 6838), so compare in lowercase.
        let base = media_type.split(';').next().unwrap_or(media_type).trim();
        base.is_empty() || base.to_ascii_lowercase().starts_with("text/")
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

/// The parser for tree-shaped text: JSON, XML, and HTML.
///
/// Canonical text is the file itself, exactly as [`PlainTextParser`] would have
/// produced it — nothing is extracted, converted, or reordered, so spans keep
/// addressing the bytes a reader downloads. What this parser adds is the map
/// from those spans to the nodes they came from, which is what lets a citation
/// open the document as a tree rather than as a wall of characters.
///
/// A file that does not parse is still indexed: it yields the same text with an
/// empty map, which is indistinguishable downstream from a format that has no
/// structure to record.
#[derive(Debug, Clone, Copy, Default)]
pub struct StructuredTextParser;

impl StructuredTextParser {
    /// Construct the parser.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl DocumentParser for StructuredTextParser {
    fn fingerprint_for(&self, media_type: &str) -> Option<String> {
        crate::structure::structured_format(media_type).map(crate::structure::structure_fingerprint)
    }

    fn supports(&self, media_type: &str) -> bool {
        crate::structure::structured_format(media_type).is_some()
    }

    async fn parse(&self, raw: &[u8], media_type: &str) -> Result<ParsedDocument> {
        let format = crate::structure::structured_format(media_type).ok_or_else(|| {
            RetrievalError::parse(format!(
                "StructuredTextParser does not support media type `{media_type}`"
            ))
        })?;
        // Lossy for the same reason plain text is: one bad byte in a large
        // export should cost that byte, not the document.
        let text = String::from_utf8_lossy(raw).into_owned();
        let regions = crate::structure::structured_regions(&text, format);
        Ok(ParsedDocument::from_text(text).with_source_regions(regions))
    }
}

/// A last-resort parser that accepts **any** media type so no upload is rejected
/// for its format alone.
///
/// Register it after the specific parsers ([`PlainTextParser`], a PDF parser,
/// …): a document only reaches this fallback when nothing narrower claimed it.
/// Bytes that decode as valid UTF-8 become canonical text and are indexed —
/// text-like uploads with an unknown media type (`.log`, `.yaml`, `.ndjson`, a
/// bare `application/octet-stream`) stay searchable. Bytes that are not valid
/// UTF-8 are treated as binary: the document is still stored and listed, but its
/// canonical text is empty, so it produces no chunks and never pollutes search
/// with decoded noise. This mirrors "import anything, index what we can" — the
/// document remains available to work with even when it is not searchable.
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
    fn fingerprint_for(&self, _media_type: &str) -> Option<String> {
        Some("fallback:utf8-else-empty:v1".to_string())
    }

    fn supports(&self, _media_type: &str) -> bool {
        true
    }

    async fn parse(&self, raw: &[u8], _media_type: &str) -> Result<ParsedDocument> {
        // Index only genuinely textual bytes; binary content is retained but not
        // decoded into the search index.
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
        fn fingerprint_for(&self, media_type: &str) -> Option<String> {
            self.supports(media_type)
                .then(|| format!("prefix:{}:{}", self.media_type, self.prefix))
        }

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
            registry.fingerprint_for("application/pdf").as_deref(),
            Some("prefix:application/pdf:first:")
        );
        assert_eq!(
            registry.parse(b"text", "text/plain").await.unwrap().text,
            "text"
        );
        assert_eq!(
            registry.fingerprint_for("text/plain").as_deref(),
            Some("plain-text-lossy-v1")
        );
    }

    #[tokio::test]
    async fn registry_rejects_unsupported_media_without_invoking_a_parser() {
        let registry = ParserRegistry::new().with_parser(PlainTextParser::new());
        assert!(!registry.supports("application/pdf"));
        assert_eq!(registry.fingerprint_for("application/pdf"), None);
        let error = registry
            .parse(b"%PDF", "application/pdf")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no registered document parser"));
    }

    #[test]
    fn registry_fingerprint_ignores_unrelated_parser_changes() {
        let plain_only = ParserRegistry::new().with_parser(PlainTextParser::new());
        let with_unrelated = ParserRegistry::new()
            .with_parser(PrefixParser {
                media_type: "application/pdf",
                prefix: "pdf:",
            })
            .with_parser(PlainTextParser::new());
        let with_two_unrelated = ParserRegistry::new()
            .with_parser(PrefixParser {
                media_type: "application/epub+zip",
                prefix: "epub:",
            })
            .with_parser(PrefixParser {
                media_type: "application/pdf",
                prefix: "pdf:",
            })
            .with_parser(PlainTextParser::new());
        let reversed_unrelated = ParserRegistry::new()
            .with_parser(PrefixParser {
                media_type: "application/pdf",
                prefix: "pdf:",
            })
            .with_parser(PrefixParser {
                media_type: "application/epub+zip",
                prefix: "epub:",
            })
            .with_parser(PlainTextParser::new());

        assert_eq!(
            plain_only.fingerprint_for("text/plain"),
            with_unrelated.fingerprint_for("text/plain")
        );
        assert_eq!(
            plain_only.fingerprint_for("text/plain"),
            with_two_unrelated.fingerprint_for("text/plain")
        );
        assert_eq!(
            with_two_unrelated.fingerprint_for("text/plain"),
            reversed_unrelated.fingerprint_for("text/plain")
        );
    }

    #[test]
    fn registry_fingerprint_changes_with_selected_parser_priority() {
        let first = ParserRegistry::new()
            .with_parser(PrefixParser {
                media_type: "application/pdf",
                prefix: "first:",
            })
            .with_parser(PrefixParser {
                media_type: "application/pdf",
                prefix: "second:",
            });
        let reversed = ParserRegistry::new()
            .with_parser(PrefixParser {
                media_type: "application/pdf",
                prefix: "second:",
            })
            .with_parser(PrefixParser {
                media_type: "application/pdf",
                prefix: "first:",
            });

        assert_eq!(
            first.fingerprint_for("application/pdf").as_deref(),
            Some("prefix:application/pdf:first:")
        );
        assert_eq!(
            reversed.fingerprint_for("application/pdf").as_deref(),
            Some("prefix:application/pdf:second:")
        );
    }

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

    #[tokio::test]
    async fn parses_utf8_bytes_verbatim() {
        let p = PlainTextParser::new();
        let parsed = p
            .parse("# Title\n\nbody".as_bytes(), "text/markdown")
            .await
            .unwrap();
        assert_eq!(parsed.text, "# Title\n\nbody");
    }

    #[tokio::test]
    async fn repairs_invalid_utf8_lossily() {
        let p = PlainTextParser::new();
        let parsed = p.parse(&[b'a', 0xFF, b'b'], "text/plain").await.unwrap();
        // The lone 0xFF becomes U+FFFD; surrounding text survives.
        assert!(parsed.text.starts_with('a'));
        assert!(parsed.text.ends_with('b'));
        assert!(parsed.text.contains('\u{FFFD}'));
    }

    #[tokio::test]
    async fn rejects_unsupported_media_type() {
        let p = PlainTextParser::new();
        assert!(p.parse(b"%PDF-1.7", "application/pdf").await.is_err());
    }

    #[test]
    fn fallback_supports_and_fingerprints_every_media_type() {
        let p = FallbackParser::new();
        for media in [
            "application/octet-stream",
            "image/png",
            "",
            "application/x-thing",
        ] {
            assert!(p.supports(media));
            assert_eq!(
                p.fingerprint_for(media).as_deref(),
                Some("fallback:utf8-else-empty:v1")
            );
        }
    }

    #[tokio::test]
    async fn fallback_indexes_utf8_but_stores_binary_as_empty() {
        let p = FallbackParser::new();
        // A text-like file with an unknown media type stays searchable.
        let text = p
            .parse(b"level=info msg=\"started\"", "application/octet-stream")
            .await
            .unwrap();
        assert_eq!(text.text, "level=info msg=\"started\"");
        // Binary bytes are retained but not decoded into canonical text.
        let binary = p
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
        // Specific parser still wins for text/*.
        assert_eq!(
            registry.fingerprint_for("text/plain").as_deref(),
            Some("plain-text-lossy-v1")
        );
        // Nothing is unsupported once the fallback is present.
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
