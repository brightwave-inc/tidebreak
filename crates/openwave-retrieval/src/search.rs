//! The agent-facing `search` tool.
//!
//! [`SearchTool`] adapts the retrieval pipeline to `openwave-core`'s [`Tool`]
//! contract so the agent loop can call it like any other capability. It holds the
//! two seams a query needs — an [`Embedder`] and a [`VectorStore`] — and nothing
//! else, so it stays decoupled from ingestion (parser/chunker). Point it at the
//! *same* `Arc<dyn VectorStore>` the ingest side writes to and the agent searches
//! a live index.
//!
//! It is `ReadOnly` (never mutates), so the agent may call it without an approval
//! prompt. Recoverable problems (empty query, an embedder/store hiccup) come back
//! as [`ToolOutput::error`] for the model to see and adapt to, per the tool
//! convention; only genuinely unexpected faults would be an `Err`.
//!
//! Enabled by the `tool` feature.

use std::sync::Arc;

use async_trait::async_trait;
use openwave_core::{ApprovalClass, Result, Tool, ToolCtx, ToolOutput, ToolSpec};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::document::Citation;
use crate::embed::Embedder;
use crate::vector::VectorStore;

/// A `Tool` that searches an embedded index and returns grounded citations.
pub struct SearchTool {
    embedder: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
}

impl SearchTool {
    /// Passages returned when the caller doesn't specify `k`.
    pub const DEFAULT_K: usize = 5;
    /// Ceiling on `k`, so a large request can't fan out unboundedly.
    pub const MAX_K: usize = 50;

    /// Build a search tool over a shared embedder and vector store.
    #[must_use]
    pub fn new(embedder: Arc<dyn Embedder>, store: Arc<dyn VectorStore>) -> Self {
        Self { embedder, store }
    }
}

/// Arguments for the `search` tool.
#[derive(Deserialize)]
struct SearchArgs {
    /// The natural-language query to search for.
    query: String,
    /// How many passages to return (optional; clamped to `[1, MAX_K]`).
    #[serde(default)]
    k: Option<usize>,
}

/// Parse tool args, turning a bad shape into a model-facing error output.
fn parse_args<T: for<'de> Deserialize<'de>>(args: Value) -> std::result::Result<T, ToolOutput> {
    serde_json::from_value(args).map_err(|e| ToolOutput::error(format!("invalid arguments: {e}")))
}

/// Render citations into a compact, model-readable listing.
fn render(citations: &[Citation]) -> String {
    let plural = if citations.len() == 1 { "" } else { "s" };
    let mut out = format!("Found {} passage{plural}:", citations.len());
    for (i, c) in citations.iter().enumerate() {
        out.push_str(&format!(
            "\n\n{}. [score {:.3}] document {} (bytes {}..{})\n{}",
            i + 1,
            c.score,
            c.document_id,
            c.span.start,
            c.span.end,
            c.snippet.trim()
        ));
    }
    out
}

#[async_trait]
impl Tool for SearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search".into(),
            description: "Search the indexed documents for passages relevant to a query. \
                          Returns ranked, grounded citations (document id, byte span, and text)."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The natural-language query to search for."
                    },
                    "k": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": Self::MAX_K,
                        "description": "How many passages to return (default 5)."
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let args: SearchArgs = match parse_args(args) {
            Ok(args) => args,
            Err(output) => return Ok(output),
        };
        // Trim once and use the same value for both the guard and the embedder,
        // so a padded query can't slip past validation yet embed differently on a
        // real (non-hashing) provider.
        let query = args.query.trim();
        if query.is_empty() {
            return Ok(ToolOutput::error("query must not be empty"));
        }
        let k = args.k.unwrap_or(Self::DEFAULT_K).clamp(1, Self::MAX_K);

        let embedding = match self.embedder.embed_query(query).await {
            Ok(embedding) => embedding,
            Err(err) => return Ok(ToolOutput::error(format!("embedding failed: {err}"))),
        };
        let hits = match self.store.query(&embedding, k).await {
            Ok(hits) => hits,
            Err(err) => return Ok(ToolOutput::error(format!("search failed: {err}"))),
        };

        let citations: Vec<Citation> = hits.into_iter().map(Citation::from).collect();
        if citations.is_empty() {
            return Ok(ToolOutput::text("No matching passages found."));
        }
        let content = render(&citations);
        let data = serde_json::to_value(&citations)?;
        Ok(ToolOutput::text(content).with_data(data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use openwave_core::ChatId;

    use crate::chunk::{Chunker, TextChunker};
    use crate::document::{Document, DocumentSource};
    use crate::embed::HashEmbedder;
    use crate::vector::{InMemoryVectorStore, VectorRecord};

    const DIMS: usize = 512;

    /// Build a tool over a store pre-loaded with the given one-sentence-per-line text.
    async fn tool_with(text: &str) -> SearchTool {
        let embedder = Arc::new(HashEmbedder::new(DIMS));
        let store = Arc::new(InMemoryVectorStore::new(DIMS));

        let doc = Document::new(
            DocumentSource::uri("file:///corpus.txt"),
            "text/plain",
            text,
        );
        let chunks = TextChunker::new(90, 0).chunk(&doc).unwrap();
        let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
        let embeddings = embedder.embed_documents(&texts).await.unwrap();
        let records: Vec<VectorRecord> = chunks
            .into_iter()
            .zip(embeddings)
            .map(|(chunk, embedding)| VectorRecord { chunk, embedding })
            .collect();
        store.upsert(records).await.unwrap();

        SearchTool::new(embedder, store)
    }

    fn ctx() -> ToolCtx {
        ToolCtx {
            chat_id: ChatId::new(),
            workspace_dir: PathBuf::from("/tmp/unused"),
        }
    }

    #[test]
    fn advertises_a_read_only_search_spec() {
        let store = Arc::new(InMemoryVectorStore::new(DIMS));
        let tool = SearchTool::new(Arc::new(HashEmbedder::new(DIMS)), store);
        assert_eq!(tool.spec().name, "search");
        assert_eq!(tool.approval_class(), ApprovalClass::ReadOnly);
        assert!(tool.spec().input_schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "query"));
    }

    #[tokio::test]
    async fn returns_ranked_citations_as_text_and_structured_data() {
        let tool = tool_with(
            "Mars is the fourth planet from the Sun, the Red Planet.\n\
             Jupiter is the largest planet in the Solar System, a gas giant.\n\
             The Great Barrier Reef is the world's largest coral reef system.",
        )
        .await;

        let out = tool
            .execute(
                &ctx(),
                json!({ "query": "largest gas giant planet", "k": 1 }),
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(
            out.content.contains("Jupiter"),
            "content was: {}",
            out.content
        );

        // Structured payload is an array of citations the client can render.
        let data = out.data.expect("expected structured citations");
        let arr = data.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert!(arr[0]["snippet"].as_str().unwrap().contains("Jupiter"));
        assert!(arr[0]["document_id"].is_string());
        assert!(arr[0]["span"]["start"].is_number());
    }

    #[tokio::test]
    async fn k_defaults_and_clamps_to_the_ceiling() {
        let tool = tool_with("alpha beta gamma\ndelta epsilon zeta\neta theta iota").await;

        // No k => default, capped by however many chunks exist.
        let out = tool
            .execute(&ctx(), json!({ "query": "alpha" }))
            .await
            .unwrap();
        let n = out.data.unwrap().as_array().unwrap().len();
        assert!((1..=SearchTool::DEFAULT_K).contains(&n));

        // An over-large k is clamped, never rejected.
        let out = tool
            .execute(&ctx(), json!({ "query": "alpha", "k": 9999 }))
            .await
            .unwrap();
        assert!(!out.is_error);

        // k = 0 is clamped up to 1, not treated as "return nothing".
        let out = tool
            .execute(&ctx(), json!({ "query": "alpha", "k": 0 }))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(out.data.unwrap().as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn empty_query_is_a_model_facing_error() {
        let tool = tool_with("some indexed content here").await;
        let out = tool
            .execute(&ctx(), json!({ "query": "   " }))
            .await
            .unwrap();
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn missing_query_is_a_model_facing_error() {
        let tool = tool_with("some indexed content here").await;
        let out = tool.execute(&ctx(), json!({ "k": 3 })).await.unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("invalid arguments"));
    }

    #[tokio::test]
    async fn empty_index_reports_no_matches() {
        let tool = SearchTool::new(
            Arc::new(HashEmbedder::new(DIMS)),
            Arc::new(InMemoryVectorStore::new(DIMS)),
        );
        let out = tool
            .execute(&ctx(), json!({ "query": "anything" }))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("No matching passages"));
    }
}
