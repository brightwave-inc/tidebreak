//! OpenWave document ingestion: turn source bytes into canonical text with
//! stable parser fingerprints and source-region mappings.
//!
//! [`Retriever`] owns the selected [`DocumentParser`] and exposes the two
//! operations the durable document pipeline needs:
//!
//! - identify the parser behavior for a media type;
//! - parse retained source bytes into a validated [`ParsedDocument`].
//!
//! The production [`document_parser_registry`] combines the structured-text,
//! plain-text, and binary-safe fallback parsers.

mod document;
mod error;
mod id;
mod parse;
mod retriever;
mod structure;

pub use document::{ByteSpan, SourceLocation, SourceRegion};
pub use error::{Result, RetrievalError};
pub use id::DocumentId;
pub use parse::{
    document_parser_registry, DocumentParser, FallbackParser, ParsedDocument, ParserRegistry,
    PlainTextParser, StructuredTextParser,
};
pub use retriever::Retriever;
