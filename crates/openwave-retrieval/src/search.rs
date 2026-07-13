//! The agent-facing `search` tool.
//!
//! [`SearchTool`] adapts the retrieval pipeline to `openwave-core`'s [`Tool`]
//! contract so the agent loop can call it like any other capability. It holds the
//! query-side seams — an [`Embedder`], a [`VectorStore`], and optionally a model
//! reranker — so it stays decoupled from ingestion (parser/chunker). Point it at
//! the *same* `Arc<dyn VectorStore>` the ingest side writes to and the agent
//! searches a live index.
//!
//! It is `ReadOnly` when embedding and optional reranking are guaranteed local.
//! A provider-backed query is `Sensitive` because query text crosses a network or
//! external-service boundary. Recoverable problems (empty query, an
//! embedder/store hiccup) come back as [`ToolOutput::error`] for the model to see
//! and adapt to, per the tool convention; only genuinely unexpected faults would
//! be an `Err`.
//!
//! Enabled by the `tool` feature.

use std::sync::Arc;

use async_trait::async_trait;
use openwave_core::{ApprovalClass, Result, Tool, ToolCtx, ToolOutput, ToolSpec};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::document::{Citation, SourceLocation};
use crate::embed::Embedder;
use crate::rerank::{rerank_candidates, Reranker};
use crate::selection::{candidate_limit, select};
use crate::vector::{SearchScope, VectorStore};

/// A `Tool` that searches an embedded index and returns grounded citations.
pub struct SearchTool {
    embedder: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
    reranker: Option<Arc<dyn Reranker>>,
}

impl SearchTool {
    /// Passages returned when the caller doesn't specify `k`.
    pub const DEFAULT_K: usize = 5;

    /// Build a search tool over a shared embedder and vector store.
    #[must_use]
    pub fn new(embedder: Arc<dyn Embedder>, store: Arc<dyn VectorStore>) -> Self {
        Self {
            embedder,
            store,
            reranker: None,
        }
    }

    /// Configure an optional model reranker for broad retrieval candidates.
    #[must_use]
    pub fn with_reranker(mut self, reranker: Arc<dyn Reranker>) -> Self {
        self.reranker = Some(reranker);
        self
    }
}

/// Arguments for the `search` tool.
#[derive(Deserialize)]
struct SearchArgs {
    /// The natural-language query to search for.
    query: String,
    /// How many passages to return (optional; clamped to the shared maximum).
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
            "\n\n{}. [score {:.3}] document {} (bytes {}..{}){}{}\n{}",
            i + 1,
            c.score,
            c.document_id,
            c.span.start,
            c.span.end,
            if c.heading_path.is_empty() {
                String::new()
            } else {
                format!("\nSection: {}", c.heading_path.join(" > "))
            },
            render_pages(c),
            c.snippet.trim()
        ));
    }
    out
}

fn render_pages(citation: &Citation) -> String {
    let mut pages = Vec::new();
    for region in &citation.source_regions {
        if let SourceLocation::Page { number } = &region.location {
            let page = number.get();
            if pages.last() != Some(&page) {
                pages.push(page);
            }
        }
    }
    match pages.as_slice() {
        [] => String::new(),
        [page] => format!("\nPage: {page}"),
        pages => format!(
            "\nPages: {}",
            pages
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

#[async_trait]
impl Tool for SearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search".into(),
            description: "Search the indexed documents for passages relevant to a query. \
                          Returns ranked, grounded citations (document id, byte span, source \
                          pages when available, and text)."
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
                        "maximum": crate::MAX_SEARCH_RESULTS,
                        "description": "How many passages to return (default 5)."
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        let reranker_is_local = self
            .reranker
            .as_ref()
            .is_none_or(|reranker| reranker.is_local());
        if self.embedder.is_local() && self.store.is_local() && reranker_is_local {
            ApprovalClass::ReadOnly
        } else {
            ApprovalClass::Sensitive
        }
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
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
        let k = args
            .k
            .unwrap_or(Self::DEFAULT_K)
            .clamp(1, crate::MAX_SEARCH_RESULTS);

        let embedding = match self.embedder.embed_query(query).await {
            Ok(embedding) => embedding,
            Err(err) => return Ok(ToolOutput::error(format!("embedding failed: {err}"))),
        };
        let scope = match ctx.project_id {
            Some(project_id) => SearchScope::Project(project_id),
            None => SearchScope::Unscoped,
        };
        let candidates = match self
            .store
            .query(query, &embedding, candidate_limit(k), scope)
            .await
        {
            Ok(hits) => hits,
            Err(err) => return Ok(ToolOutput::error(format!("search failed: {err}"))),
        };
        let candidates = match rerank_candidates(self.reranker.as_deref(), query, candidates).await
        {
            Ok(candidates) => candidates,
            Err(_) => return Ok(ToolOutput::error("reranking failed")),
        };
        let hits = select(candidates, k);

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
    use std::sync::Mutex;

    use openwave_core::{ChatId, ProjectId};

    use crate::chunk::{Chunker, TextChunker};
    use crate::document::{
        ByteSpan, Chunk, Document, DocumentSource, ScoredChunk, SourceLocation, SourceRegion,
    };
    use crate::embed::HashEmbedder;
    use crate::id::DocumentId;
    use crate::vector::{InMemoryVectorStore, SearchOptions, VectorRecord};

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
            .map(|(chunk, embedding)| VectorRecord {
                project_id: None,
                chunk,
                embedding,
            })
            .collect();
        store.upsert(records).await.unwrap();

        SearchTool::new(embedder, store)
    }

    fn ctx() -> ToolCtx {
        ctx_for(None)
    }

    fn ctx_for(project_id: Option<ProjectId>) -> ToolCtx {
        ToolCtx::new(ChatId::new(), project_id, PathBuf::from("/tmp/unused"))
    }

    struct SpyVectorStore {
        candidates: Vec<ScoredChunk>,
        calls: Mutex<Vec<(usize, SearchScope)>>,
    }

    struct FailingReranker;

    struct FixedReranker(Vec<f32>);

    struct RemoteEmbedder(HashEmbedder);

    #[async_trait]
    impl Embedder for RemoteEmbedder {
        fn dimensions(&self) -> usize {
            self.0.dimensions()
        }

        async fn embed_documents(&self, texts: &[String]) -> crate::Result<Vec<crate::Embedding>> {
            self.0.embed_documents(texts).await
        }
    }

    #[async_trait]
    impl Reranker for FailingReranker {
        async fn rerank(
            &self,
            _query: &str,
            _candidates: &[ScoredChunk],
        ) -> crate::Result<Vec<f32>> {
            Err(crate::RetrievalError::rerank("provider unavailable"))
        }
    }

    #[async_trait]
    impl Reranker for FixedReranker {
        async fn rerank(
            &self,
            _query: &str,
            _candidates: &[ScoredChunk],
        ) -> crate::Result<Vec<f32>> {
            Ok(self.0.clone())
        }
    }

    #[async_trait]
    impl VectorStore for SpyVectorStore {
        async fn upsert(&self, _records: Vec<VectorRecord>) -> crate::Result<()> {
            Ok(())
        }

        async fn query_with_options(
            &self,
            _query_text: &str,
            _query: &crate::Embedding,
            k: usize,
            options: SearchOptions,
        ) -> crate::Result<Vec<ScoredChunk>> {
            self.calls.lock().unwrap().push((k, options.scope));
            Ok(self.candidates.iter().take(k).cloned().collect())
        }

        async fn replace_document(
            &self,
            _document_id: DocumentId,
            _records: Vec<VectorRecord>,
        ) -> crate::Result<()> {
            Ok(())
        }

        async fn len(&self) -> crate::Result<usize> {
            Ok(self.candidates.len())
        }
    }

    fn scored(
        document_id: DocumentId,
        ordinal: usize,
        start: usize,
        end: usize,
        text: &str,
    ) -> ScoredChunk {
        ScoredChunk {
            chunk: Chunk::new(document_id, ordinal, ByteSpan::new(start, end), text),
            score: 1.0 - ordinal as f32 / 10.0,
        }
    }

    #[test]
    fn search_approval_tracks_query_egress() {
        let store = Arc::new(InMemoryVectorStore::new(DIMS));
        let local = SearchTool::new(Arc::new(HashEmbedder::new(DIMS)), store.clone());
        assert_eq!(local.spec().name, "search");
        assert_eq!(local.approval_class(), ApprovalClass::ReadOnly);
        assert!(local.spec().input_schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "query"));

        let remote = SearchTool::new(
            Arc::new(RemoteEmbedder(HashEmbedder::new(DIMS))),
            store.clone(),
        );
        assert_eq!(remote.approval_class(), ApprovalClass::Sensitive);

        let remote_store = SearchTool::new(
            Arc::new(HashEmbedder::new(DIMS)),
            Arc::new(SpyVectorStore {
                candidates: Vec::new(),
                calls: Mutex::new(Vec::new()),
            }),
        );
        assert_eq!(remote_store.approval_class(), ApprovalClass::Sensitive);

        // Unknown/provider-backed rerankers also fail closed even with a local
        // embedder because they receive the query and candidate text.
        let reranked = SearchTool::new(Arc::new(HashEmbedder::new(DIMS)), store)
            .with_reranker(Arc::new(FixedReranker(Vec::new())));
        assert_eq!(reranked.approval_class(), ApprovalClass::Sensitive);
    }

    #[test]
    fn render_shows_section_context_without_changing_the_snippet() {
        let document_id = DocumentId::new();
        let chunk = Chunk::with_heading_path(
            document_id,
            0,
            ByteSpan::new(10, 14),
            "body",
            vec!["Guide".into(), "Setup".into()],
        );
        let citation = Citation::from(ScoredChunk { chunk, score: 0.5 });

        let output = render(std::slice::from_ref(&citation));

        assert!(output.contains("Section: Guide > Setup"));
        assert!(output.ends_with("body"));
        assert_eq!(citation.snippet, "body");
        assert_eq!(citation.heading_path, ["Guide", "Setup"]);
    }

    #[test]
    fn render_shows_single_and_multi_page_provenance() {
        let document_id = DocumentId::new();
        let page = |start, end, number| SourceRegion {
            span: ByteSpan::new(start, end),
            location: SourceLocation::Page {
                number: std::num::NonZeroU32::new(number).unwrap(),
            },
        };
        let mut single = Chunk::new(document_id, 0, ByteSpan::new(0, 4), "body");
        single.source_regions = vec![page(0, 4, 7)];
        let single = Citation::from(ScoredChunk {
            chunk: single,
            score: 0.5,
        });
        assert!(render(&[single]).contains("\nPage: 7\nbody"));

        let mut multi = Chunk::new(document_id, 0, ByteSpan::new(0, 4), "body");
        multi.source_regions = vec![page(0, 2, 7), page(2, 4, 8)];
        let multi = Citation::from(ScoredChunk {
            chunk: multi,
            score: 0.5,
        });
        assert!(render(&[multi]).contains("\nPages: 7, 8\nbody"));
    }

    #[test]
    fn render_omits_section_line_for_an_empty_breadcrumb() {
        let citation = Citation::from(ScoredChunk {
            chunk: Chunk::new(DocumentId::new(), 0, ByteSpan::new(0, 4), "body"),
            score: 0.5,
        });

        let output = render(&[citation]);

        assert!(!output.contains("Section:"));
        assert!(output.ends_with("body"));
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
    async fn search_scope_is_inherited_from_tool_context_not_model_arguments() {
        let embedder = Arc::new(HashEmbedder::new(DIMS));
        let store = Arc::new(InMemoryVectorStore::new(DIMS));
        let project_a = ProjectId::new();
        let project_b = ProjectId::new();
        let documents = [
            Document::new(
                DocumentSource::Inline,
                "text/plain",
                "unscoped lighthouse fact",
            ),
            Document::new_scoped(
                project_a,
                DocumentSource::Inline,
                "text/plain",
                "project alpha lighthouse fact",
            ),
            Document::new_scoped(
                project_b,
                DocumentSource::Inline,
                "text/plain",
                "project beta lighthouse fact",
            ),
        ];
        for document in documents {
            let chunks = TextChunker::new(90, 0).chunk(&document).unwrap();
            let texts: Vec<_> = chunks.iter().map(|chunk| chunk.text.clone()).collect();
            let embeddings = embedder.embed_documents(&texts).await.unwrap();
            store
                .upsert(
                    chunks
                        .into_iter()
                        .zip(embeddings)
                        .map(|(chunk, embedding)| VectorRecord {
                            project_id: document.project_id,
                            chunk,
                            embedding,
                        })
                        .collect(),
                )
                .await
                .unwrap();
        }
        let tool = SearchTool::new(embedder, store);

        let loose = tool
            .execute(&ctx_for(None), json!({"query": "lighthouse fact", "k": 10}))
            .await
            .unwrap();
        assert!(loose.content.contains("unscoped lighthouse"));
        assert!(!loose.content.contains("project alpha"));
        assert!(!loose.content.contains("project beta"));

        let alpha = tool
            .execute(
                &ctx_for(Some(project_a)),
                json!({"query": "lighthouse fact", "k": 10}),
            )
            .await
            .unwrap();
        assert!(alpha.content.contains("project alpha"));
        assert!(!alpha.content.contains("unscoped lighthouse"));
        assert!(!alpha.content.contains("project beta"));
    }

    #[tokio::test]
    async fn requests_four_times_k_and_backfills_using_trusted_context_scope() {
        let first = DocumentId::new();
        let second = DocumentId::new();
        let project_id = ProjectId::new();
        let forged_project_id = ProjectId::new();
        let store = Arc::new(SpyVectorStore {
            candidates: vec![
                scored(first, 0, 0, 100, "primary"),
                scored(first, 1, 20, 80, "redundant"),
                scored(second, 2, 0, 20, "backfill"),
            ],
            calls: Mutex::new(Vec::new()),
        });
        let tool = SearchTool::new(Arc::new(HashEmbedder::new(DIMS)), store.clone());

        let output = tool
            .execute(
                &ctx_for(Some(project_id)),
                json!({
                    "query": "relevant material",
                    "k": 2,
                    "project_id": forged_project_id
                }),
            )
            .await
            .unwrap();

        assert!(!output.is_error);
        assert!(output.content.contains("primary"));
        assert!(!output.content.contains("redundant"));
        assert!(output.content.contains("backfill"));
        assert_eq!(
            store.calls.lock().unwrap().as_slice(),
            &[(8, SearchScope::Project(project_id))]
        );
    }

    #[tokio::test]
    async fn reranker_failure_is_model_facing() {
        let store = Arc::new(SpyVectorStore {
            candidates: vec![
                scored(DocumentId::new(), 0, 0, 5, "first"),
                scored(DocumentId::new(), 1, 10, 15, "second"),
            ],
            calls: Mutex::new(Vec::new()),
        });
        let tool = SearchTool::new(Arc::new(HashEmbedder::new(DIMS)), store)
            .with_reranker(Arc::new(FailingReranker));

        let output = tool
            .execute(&ctx(), json!({"query": "query", "k": 2}))
            .await
            .unwrap();

        assert!(output.is_error);
        assert_eq!(output.content, "reranking failed");
        assert!(!output.content.contains("provider unavailable"));
    }

    #[tokio::test]
    async fn reranker_reorders_results_and_replaces_tool_scores() {
        let store = Arc::new(SpyVectorStore {
            candidates: vec![
                scored(DocumentId::new(), 0, 0, 5, "backend first"),
                scored(DocumentId::new(), 1, 10, 15, "reranked first"),
            ],
            calls: Mutex::new(Vec::new()),
        });
        let tool = SearchTool::new(Arc::new(HashEmbedder::new(DIMS)), store)
            .with_reranker(Arc::new(FixedReranker(vec![0.1, 0.9])));

        let output = tool
            .execute(&ctx(), json!({"query": "query", "k": 2}))
            .await
            .unwrap();

        assert!(!output.is_error);
        let citations: Vec<Citation> = serde_json::from_value(output.data.unwrap()).unwrap();
        assert_eq!(citations[0].snippet, "reranked first");
        assert_eq!(citations[0].score, 0.9);
        assert_eq!(citations[1].snippet, "backend first");
        assert_eq!(citations[1].score, 0.1);
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
