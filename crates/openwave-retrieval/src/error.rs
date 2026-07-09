//! The crate-wide error type for retrieval.
//!
//! [`RetrievalError`] mirrors the shape of `openwave-core`'s `AgentError`: a lean,
//! `#[non_exhaustive]` enum whose variants grow as the code that produces them
//! lands (parsing, embedding, vector backends). Keeping it separate from the core
//! error keeps `openwave-retrieval` a standalone leaf crate for now.

use thiserror::Error;

/// Shorthand for results across the retrieval crate.
pub type Result<T, E = RetrievalError> = std::result::Result<T, E>;

/// Anything that can go wrong while ingesting, embedding, or searching.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RetrievalError {
    /// A document could not be parsed into text.
    #[error("parse error: {0}")]
    Parse(String),

    /// An embedding provider failed (network, model, or malformed response).
    #[error("embedding error: {0}")]
    Embed(String),

    /// A vector backend failed (I/O, query, or upsert).
    #[error("vector store error: {0}")]
    VectorStore(String),

    /// An embedding's dimensionality did not match what the store expects.
    #[error("dimension mismatch: expected {expected}, got {actual}")]
    #[non_exhaustive]
    DimensionMismatch {
        /// The dimensionality the store was configured with.
        expected: usize,
        /// The dimensionality of the offending embedding.
        actual: usize,
    },

    /// A catch-all for contexts that do not yet warrant their own variant.
    #[error("{0}")]
    Message(String),

    /// JSON (de)serialization failure.
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
}

impl RetrievalError {
    /// Build a [`RetrievalError::Parse`] from anything string-like.
    pub fn parse(message: impl Into<String>) -> Self {
        Self::Parse(message.into())
    }

    /// Build a [`RetrievalError::Embed`] from anything string-like.
    pub fn embed(message: impl Into<String>) -> Self {
        Self::Embed(message.into())
    }

    /// Build a [`RetrievalError::VectorStore`] from anything string-like.
    pub fn vector_store(message: impl Into<String>) -> Self {
        Self::VectorStore(message.into())
    }

    /// Build a [`RetrievalError::Message`] from anything string-like.
    pub fn msg(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimension_mismatch_reads_clearly() {
        let err = RetrievalError::DimensionMismatch {
            expected: 8,
            actual: 4,
        };
        assert_eq!(err.to_string(), "dimension mismatch: expected 8, got 4");
    }
}
