//! The canonical document parsing pipeline.

use crate::error::{Result, RetrievalError};
use crate::parse::{DocumentParser, ParsedDocument};

/// Owns the parser registry used by the durable document worker.
pub struct Retriever {
    parser: Box<dyn DocumentParser>,
}

impl Retriever {
    /// Assemble the parsing pipeline.
    #[must_use]
    pub fn new(parser: Box<dyn DocumentParser>) -> Self {
        Self { parser }
    }

    /// Stable identity for canonical parsing behavior.
    ///
    /// Canonical text cannot be regenerated from this identity alone; parser
    /// upgrades require retained original bytes.
    pub fn canonical_fingerprint_for(&self, media_type: &str) -> Result<String> {
        self.parser.fingerprint_for(media_type).ok_or_else(|| {
            RetrievalError::parse(format!(
                "no document parser supports media type `{media_type}`"
            ))
        })
    }

    /// Parse source bytes into validated canonical text and source locations.
    pub async fn parse_document(&self, media_type: &str, raw: &[u8]) -> Result<ParsedDocument> {
        let parsed = self.parser.parse(raw, media_type).await?;
        openwave_core::validate_source_regions(&parsed.text, &parsed.source_regions)
            .map_err(RetrievalError::parse)?;
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ByteSpan, SourceLocation, SourceRegion};
    use std::num::NonZeroU32;

    struct InvalidRegionParser;

    #[async_trait::async_trait]
    impl DocumentParser for InvalidRegionParser {
        fn supports(&self, _media_type: &str) -> bool {
            true
        }

        async fn parse(&self, _raw: &[u8], _media_type: &str) -> Result<ParsedDocument> {
            Ok(
                ParsedDocument::from_text("short").with_source_regions(vec![SourceRegion {
                    span: ByteSpan::new(0, 99),
                    location: SourceLocation::Page {
                        number: NonZeroU32::new(1).expect("nonzero page"),
                    },
                }]),
            )
        }
    }

    #[tokio::test]
    async fn rejects_parser_regions_outside_canonical_text() {
        let retriever = Retriever::new(Box::new(InvalidRegionParser));
        let error = retriever
            .parse_document("application/test", b"ignored")
            .await
            .expect_err("invalid parser regions must not reach persistence");
        assert!(error.to_string().contains("source region"));
    }
}
