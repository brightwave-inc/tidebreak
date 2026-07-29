//! OpenWave document ingestion: turn source bytes into canonical text with
//! stable parser fingerprints and source-region mappings.
//!
//! [`Retriever`] owns the selected [`DocumentParser`] and exposes the two
//! operations the durable document pipeline needs:
//!
//! - identify the parser behavior for a media type;
//! - parse retained source bytes into a validated [`ParsedDocument`].
//!
//! The production [`document_parser_registry`] combines the always-available
//! text and structured-text parsers with feature-gated PDF, Office, image, and
//! spreadsheet adapters.

mod document;
mod error;
mod id;
#[cfg(feature = "parse-image")]
mod liteparse_image_parser;
#[cfg(feature = "parse-office")]
mod liteparse_office_parser;
#[cfg(feature = "parse-liteparse")]
mod liteparse_parser;
#[cfg(feature = "parse-liteparse")]
mod liteparse_regions;
mod parse;
mod retriever;
#[cfg(feature = "parse-spreadsheet")]
mod spreadsheet_parser;
mod structure;

pub use document::{ByteSpan, PageBounds, SourceLocation, SourceRegion, PAGE_BOUNDS_SCALE};
pub use error::{Result, RetrievalError};
pub use id::DocumentId;
#[cfg(feature = "parse-image")]
pub use liteparse_image_parser::LiteParseImageParser;
#[cfg(feature = "parse-office")]
pub use liteparse_office_parser::LiteParseOfficeParser;
#[cfg(feature = "parse-liteparse")]
pub use liteparse_parser::LiteParsePdfParser;
pub use parse::{
    document_parser_registry, DocumentParser, FallbackParser, ParsedDocument, ParserRegistry,
    PlainTextParser, StructuredTextParser,
};
pub use retriever::Retriever;
#[cfg(feature = "parse-spreadsheet")]
pub use spreadsheet_parser::SpreadsheetParser;
