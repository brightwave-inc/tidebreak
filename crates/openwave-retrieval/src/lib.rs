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
//! - [`VectorStore`] — store embedded chunks, query by similarity. Default:
//!   [`InMemoryVectorStore`], a brute-force cosine scan. Persistent, filtered, and
//!   hybrid backends (sqlite-vec, pgvector, Qdrant) land later behind the trait.
//!
//! [`Retriever`] wires all four into `ingest` and `search`. Everything a chunk or
//! citation carries a [`ByteSpan`] — a precise byte range into the source text —
//! so every answer points back to exactly where it came from.
//!
//! ```
//! use std::sync::Arc;
//! use openwave_retrieval::{
//!     Retriever, PlainTextParser, TextChunker, HashEmbedder, InMemoryVectorStore,
//!     DocumentSource,
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
//! let hits = retriever.search("largest planet", 3).await?;
//! for hit in hits {
//!     println!("[{:.3}] {}", hit.score, hit.snippet);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! The agent-facing [`SearchTool`] (behind the default `tool` feature) adapts the
//! query side to `openwave-core`'s `Tool` contract, so the agent loop can search a
//! shared index. Turn the feature off to use the pipeline as a standalone library
//! with no dependency on the core crate.
//!
//! For real semantic embeddings, the `embed-openai` feature adds [`OpenAiEmbedder`],
//! an [`Embedder`] backed by the OpenAI-compatible `/embeddings` endpoint — a
//! drop-in for [`HashEmbedder`] behind the same seam.
//!
//! Not yet wired: embedding reranking, persistent/hybrid vector backends,
//! rich-format parsing, per-chat/project index scoping, and choosing the embedder
//! from server configuration. Each lands as its own slice behind these seams.

mod chunk;
mod document;
mod embed;
mod error;
mod id;
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
#[cfg(feature = "embed-openai")]
pub use openai_embed::OpenAiEmbedder;
pub use parse::{DocumentParser, ParsedDocument, PlainTextParser};
pub use retriever::{IngestOutcome, Retriever};
#[cfg(feature = "tool")]
pub use search::SearchTool;
pub use vector::{InMemoryVectorStore, VectorRecord, VectorStore};
