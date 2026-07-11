//! OpenWave retrieval — ingest documents, embed them, search by meaning, and get
//! back grounded citations.
//!
//! The crate is built from four seams, each a trait with a working default:
//!
//! - [`DocumentParser`] — raw bytes → plain text. Default: [`PlainTextParser`]
//!   (zero-dependency `text/*`). Rich formats (PDF/office/images) land later as a
//!   feature-gated parser behind this trait.
//! - [`Chunker`] — text → overlapping, span-tracked [`Chunk`]s. Default:
//!   [`TextChunker`], a boundary-aware sliding window. Hierarchical and
//!   sentence-shingle strategies can compose behind the trait later.
//! - [`Embedder`] — text → dense vectors, with separate document/query encodings.
//!   Default: [`HashEmbedder`], a deterministic offline hashing encoder for tests
//!   and local use; real providers (OpenAI/Cohere/ONNX) slot in behind the trait.
//! - [`VectorStore`] — store embedded chunks, query by lexical and dense relevance.
//!   Default: [`InMemoryVectorStore`], dependency-free BM25 plus a brute-force
//!   cosine scan, fused with reciprocal rank fusion. [`LanceVectorStore`] adds
//!   the durable embedded implementation behind the `vec-lance` feature.
//!
//! [`Retriever`] wires all four into `ingest` and `search`. Everything a chunk or
//! citation carries a [`ByteSpan`] — a precise byte range into the source text —
//! so every answer points back to exactly where it came from.
//!
//! ```
//! use std::sync::Arc;
//! use openwave_retrieval::{
//!     Retriever, PlainTextParser, TextChunker, HashEmbedder, InMemoryVectorStore,
//!     DocumentSource, SearchScope,
//! };
//!
//! # async fn demo() -> openwave_retrieval::Result<()> {
//! let dims = 256;
//! let retriever = Retriever::new(
//!     Box::new(PlainTextParser::new()),
//!     Box::new(TextChunker::default()),
//!     Arc::new(HashEmbedder::new(dims)),
//!     Arc::new(InMemoryVectorStore::new(dims)),
//! );
//!
//! retriever
//!     .ingest(DocumentSource::uri("notes.txt"), "text/plain", b"Jupiter is a gas giant.")
//!     .await?;
//! let hits = retriever
//!     .search(SearchScope::Unscoped, "largest planet", 3)
//!     .await?;
//! for hit in hits {
//!     println!("[{:.3}] {}", hit.score, hit.snippet);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! The agent-facing [`SearchTool`] (behind the default `tool` feature) adapts the
//! query side to `openwave-core`'s `Tool` contract, so the agent loop can search a
//! shared index. Turn the feature off to omit that adapter. The crate retains a
//! lean, default-feature-free dependency on `openwave-core` because core owns the
//! persisted [`DocumentId`] shared by the catalog and index.
//!
//! For real semantic embeddings, the `embed-openai` feature adds [`OpenAiEmbedder`],
//! an [`Embedder`] backed by the OpenAI-compatible `/embeddings` endpoint — a
//! drop-in for [`HashEmbedder`] behind the same seam.
//!
//! Not yet wired: model reranking, rich-format parsing, and ANN index management.
//! Each lands as its own slice behind these seams.

mod chunk;
mod document;
mod embed;
mod error;
mod hybrid;
mod id;
#[cfg(feature = "vec-lance")]
mod lance_store;
#[cfg(feature = "embed-openai")]
mod openai_embed;
mod parse;
mod retriever;
#[cfg(feature = "tool")]
mod search;
mod vector;

pub use chunk::{Chunker, TextChunker};
pub use document::{ByteSpan, Chunk, Citation, Document, DocumentSource, ScoredChunk};
pub use embed::{Embedder, Embedding, HashEmbedder};
pub use error::{Result, RetrievalError};
pub use id::{ChunkId, DocumentId};
#[cfg(feature = "vec-lance")]
pub use lance_store::LanceVectorStore;
#[cfg(feature = "embed-openai")]
pub use openai_embed::OpenAiEmbedder;
pub use openwave_core::{DocumentGeneration, ProjectId};
pub use parse::{DocumentParser, ParsedDocument, PlainTextParser};
pub use retriever::{GenerationIndexOutcome, IngestOutcome, Retriever};
#[cfg(feature = "tool")]
pub use search::SearchTool;
pub use vector::{
    DocumentGenerationState, GenerationStageOutcome, InMemoryVectorStore, SearchOptions,
    SearchScope, VectorRecord, VectorStore, DEFAULT_MIN_DENSE_SIMILARITY,
};
