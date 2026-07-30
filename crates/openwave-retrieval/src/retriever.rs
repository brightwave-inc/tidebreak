//! The canonical document parsing pipeline.

use crate::error::Result;
use crate::parse::{DocumentParser, ParsedDocument};

/// Owns the parser registry used by synchronous document ingestion.
pub struct Retriever {
    parser: Box<dyn DocumentParser>,
}

impl Retriever {
    /// Assemble the parsing pipeline.
    #[must_use]
    pub fn new(parser: Box<dyn DocumentParser>) -> Self {
        Self { parser }
    }

    /// Parse source bytes into canonical text.
    pub async fn parse_document(&self, media_type: &str, raw: &[u8]) -> Result<ParsedDocument> {
        self.parser.parse(raw, media_type).await
    }
}
