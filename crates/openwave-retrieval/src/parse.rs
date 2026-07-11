//! The document-parser seam: raw bytes in, plain text out.
//!
//! A [`DocumentParser`] turns a source file's bytes into the canonical plain text
//! that chunking and embedding operate on. The always-on [`PlainTextParser`]
//! handles `text/*` (plain, markdown) with zero dependencies. Rich formats
//! (PDF/office/images) will arrive later as a feature-gated parser behind this
//! same trait, so the pipeline never has to care which parser produced the text.
//!
//! The trait is synchronous while the only implementation is CPU-only text
//! decoding. Rich parsers must not perform expensive native or remote work on an
//! async executor thread; the parser contract and durable worker boundary will
//! become async before one is wired into production.

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
}

impl DocumentParser for ParserRegistry {
    fn fingerprint(&self) -> String {
        // Length prefixes make the ordered composition unambiguous even when a
        // child fingerprint contains punctuation used by other fingerprints.
        let mut fingerprint = String::from("parser-registry-v1");
        for parser in &self.parsers {
            let child = parser.fingerprint();
            fingerprint.push(':');
            fingerprint.push_str(&child.len().to_string());
            fingerprint.push(':');
            fingerprint.push_str(&child);
        }
        let digest = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, fingerprint.as_bytes());
        format!("parser-registry-v1:{digest}")
    }

    fn supports(&self, media_type: &str) -> bool {
        self.parsers
            .iter()
            .any(|parser| parser.supports(media_type))
    }

    fn parse(&self, raw: &[u8], media_type: &str) -> Result<ParsedDocument> {
        let parser = self
            .parsers
            .iter()
            .find(|parser| parser.supports(media_type))
            .ok_or_else(|| {
                RetrievalError::parse(format!(
                    "no registered document parser supports media type `{media_type}`"
                ))
            })?;
        parser.parse(raw, media_type)
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
pub trait DocumentParser: Send + Sync {
    /// Stable identity for canonical-text behavior used in index watermarks.
    ///
    /// Custom parsers should override this whenever an implementation change can
    /// alter canonical text. The value must remain stable for this parser
    /// instance's lifetime; runtime reconfiguration requires a new instance.
    fn fingerprint(&self) -> String {
        format!("custom-parser:type={}", std::any::type_name::<Self>())
    }

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
    fn fingerprint(&self) -> String {
        "plain-text-lossy-v1".to_string()
    }

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

    struct PrefixParser {
        media_type: &'static str,
        prefix: &'static str,
    }

    impl DocumentParser for PrefixParser {
        fn fingerprint(&self) -> String {
            format!("prefix:{}:{}", self.media_type, self.prefix)
        }

        fn supports(&self, media_type: &str) -> bool {
            media_type == self.media_type
        }

        fn parse(&self, raw: &[u8], _media_type: &str) -> Result<ParsedDocument> {
            Ok(ParsedDocument::from_text(format!(
                "{}{}",
                self.prefix,
                String::from_utf8_lossy(raw)
            )))
        }
    }

    #[test]
    fn registry_dispatches_to_first_supporting_parser() {
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
            registry.parse(b"pdf", "application/pdf").unwrap().text,
            "first:pdf"
        );
        assert_eq!(registry.parse(b"text", "text/plain").unwrap().text, "text");
    }

    #[test]
    fn registry_rejects_unsupported_media_without_invoking_a_parser() {
        let registry = ParserRegistry::new().with_parser(PlainTextParser::new());
        assert!(!registry.supports("application/pdf"));
        let error = registry.parse(b"%PDF", "application/pdf").unwrap_err();
        assert!(error.to_string().contains("no registered document parser"));
    }

    #[test]
    fn registry_fingerprint_captures_ordered_children_unambiguously() {
        let first = ParserRegistry::new()
            .with_parser(PrefixParser {
                media_type: "a",
                prefix: "b:c",
            })
            .with_parser(PrefixParser {
                media_type: "d",
                prefix: "e",
            });
        let reversed = ParserRegistry::new()
            .with_parser(PrefixParser {
                media_type: "d",
                prefix: "e",
            })
            .with_parser(PrefixParser {
                media_type: "a",
                prefix: "b:c",
            });

        assert_ne!(first.fingerprint(), reversed.fingerprint());
        assert_eq!(first.fingerprint(), first.fingerprint());
        assert_eq!(first.fingerprint().len(), 55);
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
