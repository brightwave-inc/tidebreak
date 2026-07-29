//! Errors produced by document parsing and ingestion.

use thiserror::Error;

/// Shorthand for results across the retrieval crate.
pub type Result<T, E = RetrievalError> = std::result::Result<T, E>;

/// Anything that can go wrong while producing canonical document text.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RetrievalError {
    /// A document could not be parsed into canonical text.
    #[error("parse error: {0}")]
    Parse(String),

    /// A catch-all for ingestion contexts that do not warrant a parser error.
    #[error("{0}")]
    Message(String),
}

impl RetrievalError {
    /// Build a [`RetrievalError::Parse`] from anything string-like.
    pub fn parse(message: impl Into<String>) -> Self {
        Self::Parse(message.into())
    }

    /// Build a [`RetrievalError::Message`] from anything string-like.
    pub fn msg(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }
}
