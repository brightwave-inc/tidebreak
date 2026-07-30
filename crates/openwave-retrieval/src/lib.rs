//! OpenWave document ingestion: turn source bytes into canonical text.
//!
//! [`Retriever`] owns the selected [`DocumentParser`] and exposes the two
//! operation synchronous ingestion needs: parse source bytes into a
//! [`ParsedDocument`].
//!
//! The production [`document_parser_registry`] combines plain-text and
//! binary-safe fallback parsers.

mod error;
mod id;
mod parse;
mod retriever;

pub use error::{Result, RetrievalError};
pub use id::DocumentId;
pub use parse::{
    document_parser_registry, DocumentParser, FallbackParser, ParsedDocument, ParserRegistry,
    PlainTextParser,
};
pub use retriever::Retriever;
