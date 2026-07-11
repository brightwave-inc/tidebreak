//! OpenWave's in-process HTTP/WebSocket surface.
//!
//! Every client — the desktop webview, the CLI — drives the agent through this
//! one local API rather than linking the loop directly, so all surfaces share a
//! single wiring of `Config`, `Store`, and (next slice) the agent. The server
//! binds to an ephemeral **loopback** port and mints a per-launch **bearer
//! token**: only the local process it was handed to can reach it.
//!
//! The surface runs turns end to end: the chat CRUD routes, `POST
//! /chats/{id}/messages` to start a turn (one per chat at a time), and
//! `WS /chats/{id}/events` to watch it — journaled events are replayed on connect
//! and then streamed live (snapshot → replay → live).

mod approvals;
mod auth;
mod bus;
mod document_auditor;
mod document_worker;
mod error;
mod extract;
mod hub;
mod provider;
mod providers;
mod resolver;
mod routes;
mod state;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::http::{header, Method};
use axum::routing::{get, post};
use axum::Router;
use tokio::net::TcpListener;
use tower_http::cors::{AllowOrigin, CorsLayer};

use openwave_core::{
    AgentConfig, AgentError, Config, DbStore, KeychainSecretProvider, ListDir, Profile, ReadFile,
    Result, SecretProvider, Store, ToolRegistry, WriteFile,
};
use openwave_retrieval::{
    Embedder, HashEmbedder, LanceVectorStore, OpenAiEmbedder, ParserRegistry, PlainTextParser,
    Retriever, SearchTool, TextChunker, VectorStore,
};

use resolver::KeyedResolver;

pub use error::ServerError;
pub use state::AppState;

/// Build the router: unauthenticated health check plus the token-guarded API.
pub fn app(state: AppState) -> Router {
    // `route_layer` applies the token check to matched API routes only, so an
    // unknown path still answers `404` (not `401`), and `/healthz` stays open.
    let api = Router::new()
        .route(
            "/settings",
            get(routes::get_settings).put(routes::put_settings),
        )
        .route(
            "/projects",
            post(routes::create_project).get(routes::list_projects),
        )
        .route("/projects/{id}", get(routes::get_project))
        .route(
            "/projects/{project_id}/documents",
            post(routes::ingest_project_document).get(routes::list_project_documents),
        )
        .route(
            "/projects/{project_id}/documents/{document_id}",
            get(routes::get_project_document).delete(routes::delete_project_document),
        )
        .route(
            "/projects/{project_id}/documents/{document_id}/retry",
            post(routes::retry_project_document),
        )
        .route(
            "/projects/{project_id}/search",
            post(routes::search_project_documents),
        )
        .route("/models", get(routes::list_models))
        .route("/providers", get(routes::list_providers))
        .route(
            "/providers/{kind}",
            axum::routing::put(routes::put_provider),
        )
        .route(
            "/providers/{kind}/credential",
            axum::routing::delete(routes::delete_provider_credential),
        )
        .route(
            "/documents",
            post(routes::ingest_document).get(routes::list_documents),
        )
        .route(
            "/documents/{id}",
            get(routes::get_document).delete(routes::delete_document),
        )
        .route("/documents/{id}/retry", post(routes::retry_document))
        .route("/search", post(routes::search_documents))
        .route("/chats", post(routes::create_chat).get(routes::list_chats))
        .route(
            "/chats/{id}",
            get(routes::get_chat).patch(routes::patch_chat),
        )
        .route(
            "/settings/api-key",
            axum::routing::put(routes::put_api_key).delete(routes::delete_api_key),
        )
        .route("/chats/{id}/messages", post(routes::post_message))
        .route("/chats/{id}/cancel", post(routes::post_cancel))
        .route("/chats/{id}/steer", post(routes::post_steer))
        .route(
            "/chats/{id}/approvals/{call_id}",
            post(routes::post_approval),
        )
        .route("/chats/{id}/events", get(routes::chat_events))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ))
        .with_state(state);

    // Loopback-only + bearer token is the real gate. CORS mirrors the request
    // Origin so the Tauri webview (and a browser on `vite` during UI work) can
    // call the API from a different localhost port.
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::mirror_request())
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE, header::ACCEPT]);

    Router::new()
        .route("/healthz", get(healthz))
        .merge(api)
        .layer(cors)
}

/// Liveness probe — no auth, no state.
async fn healthz() -> &'static str {
    "ok"
}

/// A bound server: the loopback address and per-launch token are known, so the
/// spawning client can be told where to connect before the accept loop starts.
pub struct Server {
    local_addr: SocketAddr,
    token: Arc<str>,
    listener: TcpListener,
    router: Router,
    _document_auditor: AbortTask,
    _document_worker: AbortTask,
}

struct AbortTask(tokio::task::JoinHandle<()>);

impl Drop for AbortTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Server {
    /// The loopback address the server is listening on.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// The bearer token clients must present.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Run the accept loop until the process exits.
    pub async fn serve(self) -> Result<()> {
        axum::serve(self.listener, self.router)
            .await
            .map_err(|e| AgentError::msg(format!("server error: {e}")))
    }
}

/// Default model when none is configured via settings or per-chat. Overridable
/// with `OPENWAVE_MODEL`.
const DEFAULT_MODEL: &str = "claude-opus-4-8";

/// Wire the store from `config` and bind the API to an ephemeral loopback port.
pub async fn bind(config: Config) -> Result<Server> {
    let store = connect_store(&config).await?;
    let secrets: Arc<dyn SecretProvider> = Arc::new(KeychainSecretProvider::new());
    // Pre-providers installs may only have an env/legacy key — enable Anthropic
    // so `KeyedResolver`'s enabled check doesn't fail-closed on upgrade.
    providers::migrate_legacy_anthropic(&*store, &*secrets).await?;
    let resolver = Arc::new(KeyedResolver::new(store.clone(), secrets.clone()));
    let embedder = resolve_embedder(&*store, &*secrets).await;
    let vector_store = connect_vector_store(&config, embedder.dimensions()).await?;
    let (retrieval, tools, agent_config) = agent_deps(embedder, vector_store);
    let state = AppState::new(
        config,
        store,
        resolver,
        secrets,
        tools,
        retrieval,
        agent_config,
    );
    let token = state.token.clone();
    let document_worker = document_worker::DocumentWorker::new(
        state.store.clone(),
        state.retrieval.clone(),
        state.document_job_wake.clone(),
        state.document_writes.clone(),
        document_worker::DocumentWorkerConfig::default(),
    );
    let document_auditor = document_auditor::DocumentAuditor::new(
        state.store.clone(),
        state.retrieval.clone(),
        state.document_writes.clone(),
        state.document_job_wake.clone(),
        document_auditor::DocumentAuditorConfig::default(),
    );
    let router = app(state);

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|e| AgentError::config(format!("failed to bind loopback: {e}")))?;
    let local_addr = listener
        .local_addr()
        .map_err(|e| AgentError::config(format!("no local address: {e}")))?;

    let document_auditor = tokio::spawn(document_auditor.run());
    let document_worker = tokio::spawn(document_worker.run());

    Ok(Server {
        local_addr,
        token,
        listener,
        router,
        _document_auditor: AbortTask(document_auditor),
        _document_worker: AbortTask(document_worker),
    })
}

/// Assemble the agent dependencies for a real launch: the retrieval pipeline, the
/// tool set, and the per-turn tuning. The model **provider** is not built here — it
/// is resolved per turn by the [`KeyedResolver`] (a composite router over enabled
/// providers; see [`resolver`]), so configuring a provider at runtime takes effect
/// without a restart. The model *name* comes from `OPENWAVE_MODEL` (or the built-in
/// default) and can be overridden at runtime via `PUT /settings` or per-chat.
fn agent_deps(
    embedder: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
) -> (Arc<Retriever>, Arc<ToolRegistry>, AgentConfig) {
    let (retrieval, search) = build_retrieval(embedder, store);
    let tools = Arc::new(
        ToolRegistry::new()
            .with(Box::new(ReadFile))
            .with(Box::new(ListDir))
            .with(Box::new(WriteFile))
            .with(search),
    );
    let model = std::env::var("OPENWAVE_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    let agent_config = AgentConfig {
        model,
        ..AgentConfig::default()
    };
    (retrieval, tools, agent_config)
}

/// The embeddings model used when an OpenAI credential is configured. Its native
/// output width is [`EMBED_DIMS`]; kept fixed here (a configurable embeddings model
/// is a later slice) so the declared dimensionality always matches the model.
const EMBED_MODEL: &str = "text-embedding-3-small";
/// Native output dimensionality of [`EMBED_MODEL`].
const EMBED_DIMS: usize = 1536;

/// Choose the embedder for this launch.
///
/// Use real semantic embeddings via [`OpenAiEmbedder`] when the OpenAI provider is
/// both **enabled** and has a key configured (stored credential or `OPENAI_API_KEY`)
/// — documents are then embedded through OpenAI's API, consistent with the
/// bring-your-own-key egress model. Gating on `enabled` (the same flag that gates
/// chat routing) means a user who disabled OpenAI doesn't silently get document
/// text egressed for embeddings. Otherwise fall back to the offline, lexical
/// [`HashEmbedder`], so search works with no credentials (just less well).
///
/// Chosen once at startup: the vector index is dimension-bound to the embedder, so
/// enabling OpenAI (or adding a key) takes effect on restart — where a change in
/// embedding width rebuilds the persistent index (see [`LanceVectorStore::connect`]).
async fn resolve_embedder(store: &dyn Store, secrets: &dyn SecretProvider) -> Arc<dyn Embedder> {
    let enabled = providers::read_config(store, providers::ProviderKind::Openai)
        .await
        .map(|config| config.enabled)
        .unwrap_or(false);
    let key = if enabled {
        providers::resolve_api_key(secrets, providers::ProviderKind::Openai).await
    } else {
        None
    };
    match key {
        Some(key) => Arc::new(OpenAiEmbedder::new(key, EMBED_MODEL, EMBED_DIMS)),
        None => Arc::new(HashEmbedder::default()),
    }
}

/// Build the retrieval pipeline and the `search` tool over a shared embedder and
/// vector store.
///
/// The [`Retriever`] (used to ingest and to serve `POST /search`) and the returned
/// [`SearchTool`] (registered for the agent) hold the **same** embedder and store,
/// so a document ingested through the API is immediately visible to the agent's
/// search. The caller passes the store so production can persist to LanceDB while
/// tests use an in-memory one; either way it must be sized to `embedder.dimensions()`.
fn build_retrieval(
    embedder: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
) -> (Arc<Retriever>, Box<SearchTool>) {
    let retrieval = Arc::new(Retriever::new(
        Box::new(ParserRegistry::new().with_parser(PlainTextParser::new())),
        Box::new(TextChunker::default()),
        embedder.clone(),
        store.clone(),
    ));
    let search = Box::new(SearchTool::new(embedder, store));
    (retrieval, search)
}

/// Open the persistent vector store for this launch: a LanceDB dataset under
/// `data_dir/vectors`, kept separate from the SQLite operational database (the
/// index is derived, rebuildable data with a different lifecycle). Sized to the
/// embedder's dimensionality; a change in that width rebuilds the index (see
/// [`LanceVectorStore::connect`]).
async fn connect_vector_store(config: &Config, dims: usize) -> Result<Arc<dyn VectorStore>> {
    let dir = config.data_dir.join("vectors");
    let uri = dir
        .to_str()
        .ok_or_else(|| AgentError::config("vector store path is not valid UTF-8"))?;
    let store = LanceVectorStore::connect(uri, dims)
        .await
        .map_err(|e| AgentError::config(format!("failed to open vector store: {e}")))?;
    Ok(Arc::new(store))
}

/// Open the durable store the profile selects.
///
/// Only the desktop profile (SQLite under `data_dir`) is wired today; the
/// self-host Postgres store lands with that profile's slice.
async fn connect_store(config: &Config) -> Result<Arc<dyn Store>> {
    match config.profile {
        Profile::Desktop => {
            std::fs::create_dir_all(&config.data_dir)
                .map_err(|e| AgentError::config(format!("failed to create data dir: {e}")))?;
            let store = DbStore::connect(&config.database_url()?).await?;
            Ok(Arc::new(store))
        }
        _ => Err(AgentError::config(
            "only the desktop profile is supported for now",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use axum::body::{to_bytes, Body};
    use axum::http::{header, Request, StatusCode};
    use futures::stream::{self, BoxStream, StreamExt};
    // Tests use the in-memory store; production wires LanceDB in `bind`.
    use openwave_core::{
        AgentErrorInfo, AgentEvent, ApprovalClass, Chat, ChatId, ChatRequest, Message,
        ModelProvider, Project, ProjectId, ProviderEvent, ProviderId, SecretProvider,
        SequencedEvent, StopReason, Tool, ToolCtx, ToolOutput, ToolSpec, Usage,
    };
    use openwave_retrieval::{
        Embedding, InMemoryVectorStore, RetrievalError, ScoredChunk, VectorRecord,
    };
    use resolver::ProviderResolver;
    use serde::de::DeserializeOwned;
    use tokio::sync::Notify;
    use tower::ServiceExt;

    /// A provider that answers with a one-line completion and no tool calls.
    struct FakeProvider;

    #[async_trait]
    impl ModelProvider for FakeProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("fake")
        }
        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta { text: "hi".into() },
                ProviderEvent::Usage(Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    ..Default::default()
                }),
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    /// A provider that records the model each request asked for, then answers
    /// like `FakeProvider`. Lets a test assert which model a turn ran against.
    #[derive(Clone, Default)]
    struct RecordingProvider {
        models: Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl ModelProvider for RecordingProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("recording")
        }
        async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            self.models.lock().unwrap().push(req.model);
            Ok(stream::iter(vec![ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            }])
            .boxed())
        }
    }

    /// A provider whose completion blocks on `gate` until the test releases it —
    /// so a turn stays active while the test checks concurrency behavior.
    struct GatedProvider {
        gate: Arc<Notify>,
    }

    #[async_trait]
    impl ModelProvider for GatedProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("gated")
        }
        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let gate = self.gate.clone();
            Ok(stream::once(async move {
                gate.notified().await;
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                }
            })
            .boxed())
        }
    }

    /// An embedder that pauses its first document batch, exposing same-document
    /// ingest interleavings to the route tests.
    struct FirstBatchGatedEmbedder {
        inner: HashEmbedder,
        calls: AtomicUsize,
        entered: Notify,
        release: Notify,
    }

    #[async_trait]
    impl Embedder for FirstBatchGatedEmbedder {
        fn dimensions(&self) -> usize {
            self.inner.dimensions()
        }

        fn fingerprint(&self) -> String {
            "test-gated-hash-v1".into()
        }

        async fn embed_documents(
            &self,
            texts: &[String],
        ) -> openwave_retrieval::Result<Vec<Embedding>> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.notify_one();
                self.release.notified().await;
            }
            self.inner.embed_documents(texts).await
        }

        async fn embed_query(&self, text: &str) -> openwave_retrieval::Result<Embedding> {
            self.inner.embed_query(text).await
        }
    }

    struct FailingEmbedder;

    #[async_trait]
    impl Embedder for FailingEmbedder {
        fn dimensions(&self) -> usize {
            8
        }

        async fn embed_documents(
            &self,
            _texts: &[String],
        ) -> openwave_retrieval::Result<Vec<Embedding>> {
            Err(RetrievalError::embed("injected embedding failure"))
        }
    }

    struct FailAfterFirstBatchEmbedder {
        inner: HashEmbedder,
        calls: AtomicUsize,
    }

    struct FailNextDeleteVectorStore {
        inner: InMemoryVectorStore,
        fail_delete: std::sync::atomic::AtomicBool,
    }

    impl FailNextDeleteVectorStore {
        fn new(dimensions: usize) -> Self {
            Self {
                inner: InMemoryVectorStore::new(dimensions),
                fail_delete: std::sync::atomic::AtomicBool::new(false),
            }
        }

        fn fail_next_delete(&self) {
            self.fail_delete.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl VectorStore for FailNextDeleteVectorStore {
        async fn upsert(&self, records: Vec<VectorRecord>) -> openwave_retrieval::Result<()> {
            self.inner.upsert(records).await
        }

        async fn query_with_options(
            &self,
            query_text: &str,
            query: &Embedding,
            k: usize,
            options: openwave_retrieval::SearchOptions,
        ) -> openwave_retrieval::Result<Vec<ScoredChunk>> {
            self.inner
                .query_with_options(query_text, query, k, options)
                .await
        }

        async fn replace_document(
            &self,
            document_id: openwave_core::DocumentId,
            records: Vec<VectorRecord>,
        ) -> openwave_retrieval::Result<()> {
            if records.is_empty() && self.fail_delete.swap(false, Ordering::SeqCst) {
                return Err(RetrievalError::vector_store("injected delete failure"));
            }
            self.inner.replace_document(document_id, records).await
        }

        async fn stage_document_generation(
            &self,
            document_id: openwave_core::DocumentId,
            generation: openwave_core::DocumentGeneration,
            records: Vec<VectorRecord>,
        ) -> openwave_retrieval::Result<openwave_retrieval::GenerationStageOutcome> {
            if records.is_empty() && self.fail_delete.swap(false, Ordering::SeqCst) {
                return Err(RetrievalError::vector_store("injected tombstone failure"));
            }
            self.inner
                .stage_document_generation(document_id, generation, records)
                .await
        }

        async fn activate_document_generation(
            &self,
            document_id: openwave_core::DocumentId,
            generation: openwave_core::DocumentGeneration,
        ) -> openwave_retrieval::Result<bool> {
            self.inner
                .activate_document_generation(document_id, generation)
                .await
        }

        async fn active_document_generation(
            &self,
            document_id: openwave_core::DocumentId,
        ) -> openwave_retrieval::Result<Option<openwave_core::DocumentGeneration>> {
            self.inner.active_document_generation(document_id).await
        }

        async fn newest_document_generation(
            &self,
            document_id: openwave_core::DocumentId,
        ) -> openwave_retrieval::Result<Option<openwave_retrieval::DocumentGenerationState>>
        {
            self.inner.newest_document_generation(document_id).await
        }

        async fn document_len(
            &self,
            document_id: openwave_core::DocumentId,
        ) -> openwave_retrieval::Result<Option<usize>> {
            self.inner.document_len(document_id).await
        }

        async fn len(&self) -> openwave_retrieval::Result<usize> {
            self.inner.len().await
        }
    }

    #[async_trait]
    impl Embedder for FailAfterFirstBatchEmbedder {
        fn dimensions(&self) -> usize {
            self.inner.dimensions()
        }

        fn fingerprint(&self) -> String {
            "test-fail-after-first-v1".into()
        }

        async fn embed_documents(
            &self,
            texts: &[String],
        ) -> openwave_retrieval::Result<Vec<Embedding>> {
            if self.calls.fetch_add(1, Ordering::SeqCst) > 0 {
                return Err(RetrievalError::embed("injected update failure"));
            }
            self.inner.embed_documents(texts).await
        }

        async fn embed_query(&self, text: &str) -> openwave_retrieval::Result<Embedding> {
            self.inner.embed_query(text).await
        }
    }

    /// A resolver that always hands back a fixed provider — lets a test inject a
    /// fake in place of the real credential-driven resolution.
    struct FixedResolver(Arc<dyn ModelProvider>);

    #[async_trait]
    impl ProviderResolver for FixedResolver {
        async fn resolve(&self) -> Arc<dyn ModelProvider> {
            self.0.clone()
        }
    }

    /// An in-memory `SecretProvider` for tests (no OS keychain).
    #[derive(Default)]
    struct MemSecrets(std::sync::Mutex<std::collections::HashMap<String, String>>);

    #[async_trait]
    impl SecretProvider for MemSecrets {
        async fn get_secret(&self, key: &str) -> Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }
        async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
            Ok(())
        }
        async fn delete_secret(&self, key: &str) -> Result<()> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
    }

    /// Store wrapper that pauses the first terminal event append. This exposes
    /// races between `run_turn` returning and the journal finishing.
    struct PauseTerminalStore {
        inner: Arc<dyn Store>,
        entered: Arc<Notify>,
        release: Arc<Notify>,
        blocked: std::sync::atomic::AtomicBool,
        fail_document_delete: std::sync::atomic::AtomicBool,
    }

    impl PauseTerminalStore {
        fn new(inner: Arc<dyn Store>, entered: Arc<Notify>, release: Arc<Notify>) -> Self {
            Self {
                inner,
                entered,
                release,
                blocked: std::sync::atomic::AtomicBool::new(false),
                fail_document_delete: std::sync::atomic::AtomicBool::new(false),
            }
        }

        fn fail_next_document_delete(&self) {
            self.fail_document_delete.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl Store for PauseTerminalStore {
        async fn create_project(&self, project: &Project) -> Result<()> {
            self.inner.create_project(project).await
        }
        async fn get_project(&self, id: ProjectId) -> Result<Option<Project>> {
            self.inner.get_project(id).await
        }
        async fn list_projects(&self) -> Result<Vec<Project>> {
            self.inner.list_projects().await
        }
        async fn create_document(&self, document: &openwave_core::DocumentRecord) -> Result<()> {
            self.inner.create_document(document).await
        }
        async fn get_document(
            &self,
            id: openwave_core::DocumentId,
        ) -> Result<Option<openwave_core::DocumentRecord>> {
            self.inner.get_document(id).await
        }
        async fn list_documents(
            &self,
            scope: openwave_core::DocumentScope,
        ) -> Result<Vec<openwave_core::DocumentRecord>> {
            self.inner.list_documents(scope).await
        }
        async fn list_document_summaries(
            &self,
            scope: openwave_core::DocumentScope,
            after: Option<openwave_core::DocumentListCursor>,
            limit: u64,
        ) -> Result<Vec<openwave_core::DocumentSummaryRecord>> {
            self.inner
                .list_document_summaries(scope, after, limit)
                .await
        }
        async fn get_document_generation(
            &self,
            id: openwave_core::DocumentId,
        ) -> Result<Option<openwave_core::DocumentGeneration>> {
            self.inner.get_document_generation(id).await
        }
        async fn list_pending_document_retirements(
            &self,
            after: Option<openwave_core::DocumentId>,
            limit: u64,
        ) -> Result<Vec<(openwave_core::DocumentId, openwave_core::DocumentGeneration)>> {
            self.inner
                .list_pending_document_retirements(after, limit)
                .await
        }
        async fn get_pending_document_retirement(
            &self,
            id: openwave_core::DocumentId,
        ) -> Result<Option<openwave_core::DocumentGeneration>> {
            self.inner.get_pending_document_retirement(id).await
        }
        async fn complete_document_retirement(
            &self,
            id: openwave_core::DocumentId,
            generation: openwave_core::DocumentGeneration,
        ) -> Result<bool> {
            self.inner
                .complete_document_retirement(id, generation)
                .await
        }
        async fn delete_document(
            &self,
            id: openwave_core::DocumentId,
        ) -> Result<openwave_core::DocumentGeneration> {
            if self.fail_document_delete.swap(false, Ordering::SeqCst) {
                return Err(AgentError::Store(
                    "injected document catalog delete failure".into(),
                ));
            }
            self.inner.delete_document(id).await
        }
        async fn upsert_document(
            &self,
            document: &openwave_core::DocumentUpsert,
        ) -> Result<openwave_core::DocumentRecord> {
            self.inner.upsert_document(document).await
        }
        async fn upsert_document_and_enqueue_index(
            &self,
            document: &openwave_core::DocumentUpsert,
            pipeline_fingerprint: &str,
            max_attempts: i32,
        ) -> Result<(openwave_core::DocumentRecord, openwave_core::DocumentJob)> {
            self.inner
                .upsert_document_and_enqueue_index(document, pipeline_fingerprint, max_attempts)
                .await
        }
        async fn get_document_job(
            &self,
            id: openwave_core::DocumentJobId,
        ) -> Result<Option<openwave_core::DocumentJob>> {
            self.inner.get_document_job(id).await
        }
        async fn list_document_jobs(
            &self,
            document_id: openwave_core::DocumentId,
        ) -> Result<Vec<openwave_core::DocumentJob>> {
            self.inner.list_document_jobs(document_id).await
        }
        async fn claim_document_job(
            &self,
            now: chrono::DateTime<chrono::Utc>,
            lease_expires_at: chrono::DateTime<chrono::Utc>,
        ) -> Result<Option<openwave_core::DocumentJob>> {
            self.inner.claim_document_job(now, lease_expires_at).await
        }
        async fn heartbeat_document_job(
            &self,
            id: openwave_core::DocumentJobId,
            lease_token: uuid::Uuid,
            now: chrono::DateTime<chrono::Utc>,
            lease_expires_at: chrono::DateTime<chrono::Utc>,
        ) -> Result<bool> {
            self.inner
                .heartbeat_document_job(id, lease_token, now, lease_expires_at)
                .await
        }
        async fn complete_document_index_job(
            &self,
            id: openwave_core::DocumentJobId,
            lease_token: uuid::Uuid,
            completed_at: chrono::DateTime<chrono::Utc>,
        ) -> Result<bool> {
            self.inner
                .complete_document_index_job(id, lease_token, completed_at)
                .await
        }
        async fn record_document_job_failure(
            &self,
            id: openwave_core::DocumentJobId,
            lease_token: uuid::Uuid,
            failed_at: chrono::DateTime<chrono::Utc>,
            retry_at: Option<chrono::DateTime<chrono::Utc>>,
            error_code: &str,
            error_detail: Option<&str>,
        ) -> Result<Option<openwave_core::DocumentJobStatus>> {
            self.inner
                .record_document_job_failure(
                    id,
                    lease_token,
                    failed_at,
                    retry_at,
                    error_code,
                    error_detail,
                )
                .await
        }
        async fn mark_document_indexed(
            &self,
            id: openwave_core::DocumentId,
            revision: i64,
            revision_token: uuid::Uuid,
            fingerprint: &str,
            indexed_at: chrono::DateTime<chrono::Utc>,
        ) -> Result<bool> {
            self.inner
                .mark_document_indexed(id, revision, revision_token, fingerprint, indexed_at)
                .await
        }
        async fn clear_document_index(
            &self,
            id: openwave_core::DocumentId,
            revision: i64,
            revision_token: uuid::Uuid,
        ) -> Result<bool> {
            self.inner
                .clear_document_index(id, revision, revision_token)
                .await
        }
        async fn create_chat(&self, chat: &Chat) -> Result<()> {
            self.inner.create_chat(chat).await
        }
        async fn get_chat(&self, id: ChatId) -> Result<Option<Chat>> {
            self.inner.get_chat(id).await
        }
        async fn list_chats(&self) -> Result<Vec<Chat>> {
            self.inner.list_chats().await
        }
        async fn set_chat_model(&self, id: ChatId, model: Option<String>) -> Result<()> {
            self.inner.set_chat_model(id, model).await
        }
        async fn append_message(&self, message: &Message) -> Result<()> {
            self.inner.append_message(message).await
        }
        async fn list_messages(&self, chat_id: ChatId) -> Result<Vec<Message>> {
            self.inner.list_messages(chat_id).await
        }
        async fn upsert_tool_call(&self, call: &openwave_core::ToolCallRecord) -> Result<()> {
            self.inner.upsert_tool_call(call).await
        }
        async fn list_tool_calls(
            &self,
            chat_id: ChatId,
        ) -> Result<Vec<openwave_core::ToolCallRecord>> {
            self.inner.list_tool_calls(chat_id).await
        }
        async fn get_setting(&self, key: &str) -> Result<Option<serde_json::Value>> {
            self.inner.get_setting(key).await
        }
        async fn set_setting(&self, key: &str, value: &serde_json::Value) -> Result<()> {
            self.inner.set_setting(key, value).await
        }
        async fn append_event(&self, chat_id: ChatId, event: &AgentEvent) -> Result<i64> {
            if matches!(
                event,
                AgentEvent::TurnCompleted { .. }
                    | AgentEvent::TurnFailed { .. }
                    | AgentEvent::TurnCancelled { .. }
            ) && self
                .blocked
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                self.entered.notify_waiters();
                self.release.notified().await;
            }
            self.inner.append_event(chat_id, event).await
        }
        async fn list_events(&self, chat_id: ChatId, after: i64) -> Result<Vec<SequencedEvent>> {
            self.inner.list_events(chat_id, after).await
        }
    }

    /// A router over a fresh temp SQLite store with the given provider; returns
    /// the router, token, the store (to inspect the journal), and the tempdir.
    async fn test_app_with(
        provider: Arc<dyn ModelProvider>,
    ) -> (Router, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
        let (retrieval, _search) = build_retrieval(
            Arc::new(HashEmbedder::default()),
            Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
        );
        test_app_with_retrieval(provider, retrieval).await
    }

    async fn test_app_with_retrieval(
        provider: Arc<dyn ModelProvider>,
        retrieval: Arc<Retriever>,
    ) -> (Router, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        test_app_from_parts(provider, retrieval, store, dir)
    }

    async fn test_app_with_worker() -> (
        Router,
        Arc<str>,
        Arc<dyn Store>,
        tempfile::TempDir,
        document_worker::DocumentWorker,
    ) {
        let (retrieval, _search) = build_retrieval(
            Arc::new(HashEmbedder::default()),
            Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
        );
        test_app_with_retrieval_and_worker(Arc::new(FakeProvider), retrieval).await
    }

    async fn test_app_with_retrieval_and_worker(
        provider: Arc<dyn ModelProvider>,
        retrieval: Arc<Retriever>,
    ) -> (
        Router,
        Arc<str>,
        Arc<dyn Store>,
        tempfile::TempDir,
        document_worker::DocumentWorker,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let state = AppState::new(
            Config::desktop(dir.path()),
            store.clone(),
            Arc::new(FixedResolver(provider)),
            Arc::new(MemSecrets::default()),
            Arc::new(ToolRegistry::new()),
            retrieval.clone(),
            AgentConfig {
                model: "fake".into(),
                ..AgentConfig::default()
            },
        );
        let token = state.token.clone();
        let worker = document_worker::DocumentWorker::new(
            store.clone(),
            retrieval,
            state.document_job_wake.clone(),
            state.document_writes.clone(),
            document_worker::DocumentWorkerConfig::default(),
        );
        (app(state), token, store, dir, worker)
    }

    fn test_app_from_parts(
        provider: Arc<dyn ModelProvider>,
        retrieval: Arc<Retriever>,
        store: Arc<dyn Store>,
        dir: tempfile::TempDir,
    ) -> (Router, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
        let state = AppState::new(
            Config::desktop(dir.path()),
            store.clone(),
            Arc::new(FixedResolver(provider)),
            Arc::new(MemSecrets::default()),
            Arc::new(ToolRegistry::new()),
            retrieval,
            AgentConfig {
                model: "fake".into(),
                ..AgentConfig::default()
            },
        );
        let token = state.token.clone();
        (app(state), token, store, dir)
    }

    async fn test_app() -> (Router, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
        test_app_with(Arc::new(FakeProvider)).await
    }

    #[tokio::test]
    async fn app_state_roots_blob_storage_under_the_data_directory() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let (retrieval, _search) = build_retrieval(
            Arc::new(HashEmbedder::default()),
            Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
        );
        let state = AppState::new(
            Config::desktop(dir.path()),
            store,
            Arc::new(FixedResolver(Arc::new(FakeProvider))),
            Arc::new(MemSecrets::default()),
            Arc::new(ToolRegistry::new()),
            retrieval,
            AgentConfig::default(),
        );
        let id = uuid::Uuid::new_v4().to_string();

        state
            .blobs
            .put(&id, b"source bytes".to_vec())
            .await
            .unwrap();

        assert_eq!(
            state.blobs.get(&id).await.unwrap().as_deref(),
            Some(&b"source bytes"[..])
        );
        assert!(dir
            .path()
            .join("blobs")
            .join(format!("{id}.blob"))
            .is_file());
    }

    async fn json_body<T: DeserializeOwned>(response: axum::response::Response) -> T {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Create a chat and return it.
    async fn make_chat(router: &Router, bearer: &str) -> Chat {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chats")
                    .header(header::AUTHORIZATION, bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"workspace_dir": "/tmp/ws"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        json_body(response).await
    }

    /// POST a message to a chat, returning the response status.
    async fn send_message(
        router: &Router,
        bearer: &str,
        chat: ChatId,
        content: &str,
    ) -> StatusCode {
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/chats/{chat}/messages"))
                    .header(header::AUTHORIZATION, bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"content": content}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    /// POST `/chats/{id}/cancel`, returning the response status.
    async fn cancel_turn(router: &Router, bearer: &str, chat: ChatId) -> StatusCode {
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/chats/{chat}/cancel"))
                    .header(header::AUTHORIZATION, bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    /// Poll the journal until the turn terminates (or time out), returning its
    /// events in sequence order.
    async fn wait_for_turn(store: &Arc<dyn Store>, chat: ChatId) -> Vec<SequencedEvent> {
        for _ in 0..200 {
            let events = store.list_events(chat, 0).await.unwrap();
            if events.iter().any(|e| {
                matches!(
                    e.event,
                    AgentEvent::TurnCompleted { .. }
                        | AgentEvent::TurnFailed { .. }
                        | AgentEvent::TurnCancelled { .. }
                )
            }) {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("turn did not finish within the timeout");
    }

    #[tokio::test]
    async fn cancel_stops_a_running_turn() {
        // A turn that blocks in the provider (a stand-in for a long model call),
        // so it stays running until we cancel it.
        let gate = Arc::new(Notify::new());
        let (router, token, store, _dir) =
            test_app_with(Arc::new(GatedProvider { gate: gate.clone() })).await;
        let bearer = format!("Bearer {token}");
        let chat = make_chat(&router, &bearer).await;

        assert_eq!(
            send_message(&router, &bearer, chat.id, "go").await,
            StatusCode::ACCEPTED
        );

        // The slot is claimed synchronously before the turn is spawned, so a
        // cancel arriving right after the 202 still finds the running turn.
        assert_eq!(
            cancel_turn(&router, &bearer, chat.id).await,
            StatusCode::ACCEPTED
        );

        // The turn preempts the blocked provider call and ends as cancelled —
        // note we never release `gate`, so only the cancel can end it.
        let events = wait_for_turn(&store, chat.id).await;
        assert!(matches!(
            events.last().map(|e| &e.event),
            Some(AgentEvent::TurnCancelled { .. })
        ));
    }

    #[tokio::test]
    async fn cancel_without_a_running_turn_is_a_conflict_and_unknown_chat_is_404() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let chat = make_chat(&router, &bearer).await;

        // Known chat, nothing running → 409.
        assert_eq!(
            cancel_turn(&router, &bearer, chat.id).await,
            StatusCode::CONFLICT
        );
        // Unknown chat → 404.
        assert_eq!(
            cancel_turn(&router, &bearer, ChatId::new()).await,
            StatusCode::NOT_FOUND
        );
    }

    /// POST `/chats/{id}/steer`, returning the response status.
    async fn steer_turn(
        router: &Router,
        bearer: &str,
        chat: ChatId,
        content: &str,
        interrupt: bool,
    ) -> StatusCode {
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/chats/{chat}/steer"))
                    .header(header::AUTHORIZATION, bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"content": content, "interrupt": interrupt}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn steer_without_a_running_turn_is_a_conflict_and_unknown_chat_is_404() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let chat = make_chat(&router, &bearer).await;

        assert_eq!(
            steer_turn(&router, &bearer, chat.id, "hi", false).await,
            StatusCode::CONFLICT
        );
        assert_eq!(
            steer_turn(&router, &bearer, ChatId::new(), "hi", false).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            steer_turn(&router, &bearer, chat.id, "  ", false).await,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn interrupt_steer_preempts_a_running_turn_and_continues() {
        // Stall after the first delta so steer can interrupt; then finish.
        struct StallThenFinish {
            calls: AtomicUsize,
        }
        #[async_trait]
        impl ModelProvider for StallThenFinish {
            fn id(&self) -> ProviderId {
                ProviderId::new("stall-then-finish")
            }
            async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
                if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    let head = stream::iter(vec![ProviderEvent::TextDelta {
                        text: "partial".into(),
                    }]);
                    return Ok(head.chain(stream::pending()).boxed());
                }
                Ok(stream::iter(vec![
                    ProviderEvent::TextDelta {
                        text: "after steer".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ])
                .boxed())
            }
        }

        let (router, token, store, _dir) = test_app_with(Arc::new(StallThenFinish {
            calls: AtomicUsize::new(0),
        }))
        .await;
        let bearer = format!("Bearer {token}");
        let chat = make_chat(&router, &bearer).await;

        assert_eq!(
            send_message(&router, &bearer, chat.id, "go").await,
            StatusCode::ACCEPTED
        );
        // Give the turn a moment to enter the stalled stream.
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            steer_turn(&router, &bearer, chat.id, "change course", true).await,
            StatusCode::ACCEPTED
        );

        let events = wait_for_turn(&store, chat.id).await;
        let stream_interrupted_at = events
            .iter()
            .position(|e| matches!(e.event, AgentEvent::StreamInterrupted));
        let user_steered_at = events.iter().position(|e| {
            matches!(
                &e.event,
                AgentEvent::UserSteered { content } if content == "change course"
            )
        });
        assert!(
            matches!((stream_interrupted_at, user_steered_at), (Some(a), Some(b)) if a < b),
            "interrupted stream is marked before steer is injected"
        );
        assert!(events.iter().any(|e| matches!(
            &e.event,
            AgentEvent::UserSteered { content } if content == "change course"
        )));
        assert!(matches!(
            events.last().map(|e| &e.event),
            Some(AgentEvent::TurnCompleted { .. })
        ));
        let mut visible_assistant = String::new();
        for event in events.iter().map(|e| &e.event) {
            match event {
                AgentEvent::TextDelta { text } => visible_assistant.push_str(text),
                AgentEvent::StreamInterrupted => visible_assistant.clear(),
                _ => {}
            }
        }
        assert_eq!(visible_assistant, "after steer");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.event, AgentEvent::TurnCancelled { .. })),
            "steer continues the turn"
        );
    }

    /// POST a JSON body to `uri`, returning the response.
    async fn post_json(
        router: &Router,
        bearer: &str,
        uri: &str,
        body: serde_json::Value,
    ) -> axum::response::Response {
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header(header::AUTHORIZATION, bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn ingest_then_search_finds_the_passage() {
        let (router, token, store, _dir, worker) = test_app_with_worker().await;
        let bearer = format!("Bearer {token}");

        let ingest = post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({
                "uri": "file:///solar.txt",
                "content": "Jupiter is the largest planet in the Solar System, a gas giant.",
            }),
        )
        .await;
        assert_eq!(ingest.status(), StatusCode::ACCEPTED);
        let ingest: serde_json::Value = json_body(ingest).await;
        assert!(ingest["document_id"].is_string());
        assert!(ingest["job_id"].is_string());
        assert_eq!(ingest["processing_status"], "queued");
        let document_id = ingest["document_id"].as_str().unwrap().parse().unwrap();
        let record = store
            .get_document(document_id)
            .await
            .unwrap()
            .expect("source record should be durable before the response");
        assert_eq!(
            record.canonical_text,
            "Jupiter is the largest planet in the Solar System, a gas giant."
        );
        assert_eq!(record.content_revision, 1);
        assert_eq!(record.indexed_revision, None);
        assert_eq!(
            record.processing_status,
            openwave_core::DocumentProcessingStatus::Queued
        );

        assert!(matches!(
            worker.run_once().await.unwrap(),
            document_worker::WorkerOutcome::Completed(_)
        ));

        // The worker's activated generation is searchable over the shared index.
        let search = post_json(
            &router,
            &bearer,
            "/search",
            serde_json::json!({ "query": "largest gas giant planet", "k": 1 }),
        )
        .await;
        assert_eq!(search.status(), StatusCode::OK);
        let results: serde_json::Value = json_body(search).await;
        let citations = results["citations"].as_array().unwrap();
        assert_eq!(citations.len(), 1);
        assert!(citations[0]["snippet"]
            .as_str()
            .unwrap()
            .contains("Jupiter"));
        assert_eq!(citations[0]["document_id"], ingest["document_id"]);
    }

    #[tokio::test]
    async fn project_document_routes_enforce_corpus_identity_and_ownership() {
        let (router, token, store, _dir, worker) = test_app_with_worker().await;
        let bearer = format!("Bearer {token}");
        let project_a = make_project(&router, &bearer).await;
        let project_b = make_project(&router, &bearer).await;
        let uri = "file:///shared-source.txt";

        let root: serde_json::Value = json_body(
            post_json(
                &router,
                &bearer,
                "/documents",
                serde_json::json!({"uri": uri, "content": "loose corpus zephyr"}),
            )
            .await,
        )
        .await;
        let a: serde_json::Value = json_body(
            post_json(
                &router,
                &bearer,
                &format!("/projects/{}/documents", project_a.id),
                serde_json::json!({"uri": uri, "content": "project alpha aurora"}),
            )
            .await,
        )
        .await;
        let b: serde_json::Value = json_body(
            post_json(
                &router,
                &bearer,
                &format!("/projects/{}/documents", project_b.id),
                serde_json::json!({"uri": uri, "content": "project beta nebula"}),
            )
            .await,
        )
        .await;

        assert_eq!(
            root["document_id"],
            openwave_core::DocumentId::derive(uri).to_string()
        );
        assert_eq!(
            a["document_id"],
            openwave_core::DocumentId::derive_for_project(project_a.id, uri).to_string()
        );
        assert_eq!(
            b["document_id"],
            openwave_core::DocumentId::derive_for_project(project_b.id, uri).to_string()
        );
        assert_ne!(root["document_id"], a["document_id"]);
        assert_ne!(a["document_id"], b["document_id"]);

        for _ in 0..3 {
            assert!(matches!(
                worker.run_once().await.unwrap(),
                document_worker::WorkerOutcome::Completed(_)
            ));
        }

        let request = |method: axum::http::Method, uri: String| {
            let router = router.clone();
            let bearer = bearer.clone();
            async move {
                router
                    .oneshot(
                        Request::builder()
                            .method(method)
                            .uri(uri)
                            .header(header::AUTHORIZATION, bearer)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap()
            }
        };

        let a_id = a["document_id"].as_str().unwrap();
        let b_id = b["document_id"].as_str().unwrap();
        let listing: serde_json::Value = json_body(
            request(
                axum::http::Method::GET,
                format!("/projects/{}/documents", project_a.id),
            )
            .await,
        )
        .await;
        assert_eq!(listing["documents"].as_array().unwrap().len(), 1);
        assert_eq!(listing["documents"][0]["document_id"], a["document_id"]);
        assert_eq!(
            listing["documents"][0]["project_id"],
            project_a.id.to_string()
        );

        assert_eq!(
            request(
                axum::http::Method::GET,
                format!("/projects/{}/documents/{b_id}", project_a.id),
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            request(
                axum::http::Method::DELETE,
                format!("/projects/{}/documents/{a_id}", project_b.id),
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            request(axum::http::Method::DELETE, format!("/documents/{a_id}"),)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            request(axum::http::Method::GET, format!("/documents/{a_id}"),)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            post_json(
                &router,
                &bearer,
                &format!("/documents/{a_id}/retry"),
                serde_json::Value::Null,
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            store
                .get_document(a_id.parse().unwrap())
                .await
                .unwrap()
                .unwrap()
                .project_id,
            Some(project_a.id)
        );

        let root_search: serde_json::Value = json_body(
            post_json(
                &router,
                &bearer,
                "/search",
                serde_json::json!({"query": "loose corpus zephyr", "k": 1}),
            )
            .await,
        )
        .await;
        assert_eq!(
            root_search["citations"][0]["document_id"],
            root["document_id"]
        );
        let a_search: serde_json::Value = json_body(
            post_json(
                &router,
                &bearer,
                &format!("/projects/{}/search", project_a.id),
                serde_json::json!({"query": "project beta nebula", "k": 1}),
            )
            .await,
        )
        .await;
        assert_eq!(a_search["citations"][0]["document_id"], a["document_id"]);

        let unknown = ProjectId::new();
        assert_eq!(
            post_json(
                &router,
                &bearer,
                &format!("/projects/{unknown}/documents"),
                serde_json::json!({"uri": uri, "content": "orphan"}),
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            request(
                axum::http::Method::DELETE,
                format!("/projects/{}/documents/{a_id}", project_a.id),
            )
            .await
            .status(),
            StatusCode::ACCEPTED
        );
    }

    #[tokio::test]
    async fn failed_indexing_leaves_authoritative_source_stale_for_retry() {
        let retrieval = Arc::new(Retriever::new(
            Box::new(PlainTextParser::new()),
            Box::new(TextChunker::default()),
            Arc::new(FailingEmbedder),
            Arc::new(InMemoryVectorStore::new(8)),
        ));
        let (router, token, store, _dir, worker) =
            test_app_with_retrieval_and_worker(Arc::new(FakeProvider), retrieval).await;
        let bearer = format!("Bearer {token}");

        let response = post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({
                "uri": "file:///retry.txt",
                "content": "authoritative even when embedding fails",
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert!(matches!(
            worker.run_once().await.unwrap(),
            document_worker::WorkerOutcome::RetryScheduled(_)
        ));

        let record = store
            .get_document(openwave_core::DocumentId::derive("file:///retry.txt"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            record.canonical_text,
            "authoritative even when embedding fails"
        );
        assert_eq!(record.content_revision, 1);
        assert_eq!(record.indexed_revision, None);
        assert_eq!(record.index_fingerprint, None);
        assert_eq!(
            record.processing_status,
            openwave_core::DocumentProcessingStatus::Queued
        );
    }

    #[tokio::test]
    async fn explicit_retry_revives_the_exact_terminal_job() {
        let (router, token, store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let ingested: serde_json::Value = json_body(
            post_json(
                &router,
                &bearer,
                "/documents",
                serde_json::json!({
                    "uri": "file:///manual-retry.txt",
                    "content": "retry the exact failed generation"
                }),
            )
            .await,
        )
        .await;
        let id: openwave_core::DocumentId =
            ingested["document_id"].as_str().unwrap().parse().unwrap();
        let job_id: openwave_core::DocumentJobId =
            ingested["job_id"].as_str().unwrap().parse().unwrap();
        let now = chrono::Utc::now();
        let claimed = store
            .claim_document_job(now, now + chrono::Duration::minutes(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, job_id);
        assert_eq!(
            store
                .record_document_job_failure(
                    job_id,
                    claimed.lease_token.unwrap(),
                    chrono::Utc::now(),
                    None,
                    "manual_test_failure",
                    None,
                )
                .await
                .unwrap(),
            Some(openwave_core::DocumentJobStatus::Failed)
        );

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/documents/{id}/retry"))
                    .header(header::AUTHORIZATION, bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let retried = store.get_document_job(job_id).await.unwrap().unwrap();
        assert_eq!(retried.status, openwave_core::DocumentJobStatus::Queued);
        assert_eq!(retried.attempt_count, 0);
        assert_eq!(retried.id, job_id);
    }

    #[tokio::test]
    async fn project_retry_revives_only_the_owned_terminal_job() {
        let (router, token, store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let project_a = make_project(&router, &bearer).await;
        let project_b = make_project(&router, &bearer).await;
        let ingested: serde_json::Value = json_body(
            post_json(
                &router,
                &bearer,
                &format!("/projects/{}/documents", project_a.id),
                serde_json::json!({
                    "uri": "file:///project-manual-retry.txt",
                    "content": "retry only within the owning project"
                }),
            )
            .await,
        )
        .await;
        let document_id: openwave_core::DocumentId =
            ingested["document_id"].as_str().unwrap().parse().unwrap();
        let job_id: openwave_core::DocumentJobId =
            ingested["job_id"].as_str().unwrap().parse().unwrap();
        let now = chrono::Utc::now();
        let claimed = store
            .claim_document_job(now, now + chrono::Duration::minutes(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, job_id);
        assert_eq!(
            store
                .record_document_job_failure(
                    job_id,
                    claimed.lease_token.unwrap(),
                    chrono::Utc::now(),
                    None,
                    "project_manual_test_failure",
                    None,
                )
                .await
                .unwrap(),
            Some(openwave_core::DocumentJobStatus::Failed)
        );

        assert_eq!(
            post_json(
                &router,
                &bearer,
                &format!("/documents/{document_id}/retry"),
                serde_json::Value::Null,
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            post_json(
                &router,
                &bearer,
                &format!("/projects/{}/documents/{document_id}/retry", project_b.id),
                serde_json::Value::Null,
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            store
                .get_document_job(job_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            openwave_core::DocumentJobStatus::Failed
        );

        let response = post_json(
            &router,
            &bearer,
            &format!("/projects/{}/documents/{document_id}/retry", project_a.id),
            serde_json::Value::Null,
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let response: serde_json::Value = json_body(response).await;
        assert_eq!(response["document_id"], document_id.to_string());
        assert_eq!(response["job_id"], job_id.to_string());
        let retried = store.get_document_job(job_id).await.unwrap().unwrap();
        assert_eq!(retried.status, openwave_core::DocumentJobStatus::Queued);
        assert_eq!(retried.attempt_count, 0);
        assert_eq!(retried.id, job_id);
    }

    #[tokio::test]
    async fn failed_update_keeps_the_prior_active_generation_searchable() {
        let embedder = Arc::new(FailAfterFirstBatchEmbedder {
            inner: HashEmbedder::default(),
            calls: AtomicUsize::new(0),
        });
        let retrieval = Arc::new(Retriever::new(
            Box::new(PlainTextParser::new()),
            Box::new(TextChunker::default()),
            embedder,
            Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
        ));
        let (router, token, store, _dir, worker) =
            test_app_with_retrieval_and_worker(Arc::new(FakeProvider), retrieval).await;
        let bearer = format!("Bearer {token}");
        let uri = "file:///updated.txt";

        assert_eq!(
            post_json(
                &router,
                &bearer,
                "/documents",
                serde_json::json!({"uri": uri, "content": "obsolete searchable phrase"}),
            )
            .await
            .status(),
            StatusCode::ACCEPTED
        );
        assert!(matches!(
            worker.run_once().await.unwrap(),
            document_worker::WorkerOutcome::Completed(_)
        ));
        assert_eq!(
            post_json(
                &router,
                &bearer,
                "/documents",
                serde_json::json!({"uri": uri, "content": "replacement failed to embed"}),
            )
            .await
            .status(),
            StatusCode::ACCEPTED
        );
        assert!(matches!(
            worker.run_once().await.unwrap(),
            document_worker::WorkerOutcome::RetryScheduled(_)
        ));

        let search: serde_json::Value = json_body(
            post_json(
                &router,
                &bearer,
                "/search",
                serde_json::json!({"query": "obsolete searchable phrase"}),
            )
            .await,
        )
        .await;
        assert_eq!(search["citations"].as_array().unwrap().len(), 1);
        assert!(search["citations"][0]["snippet"]
            .as_str()
            .unwrap()
            .contains("obsolete"));
        let record = store
            .get_document(openwave_core::DocumentId::derive(uri))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.canonical_text, "replacement failed to embed");
        assert_eq!(record.content_revision, 2);
        assert_eq!(record.indexed_revision, None);
    }

    #[tokio::test]
    async fn update_enqueues_without_calling_legacy_vector_retirement() {
        let vector_store = Arc::new(FailNextDeleteVectorStore::new(HashEmbedder::DEFAULT_DIMS));
        let retrieval = Arc::new(Retriever::new(
            Box::new(PlainTextParser::new()),
            Box::new(TextChunker::default()),
            Arc::new(HashEmbedder::default()),
            vector_store.clone(),
        ));
        let (router, token, store, _dir) =
            test_app_with_retrieval(Arc::new(FakeProvider), retrieval).await;
        let bearer = format!("Bearer {token}");
        let uri = "file:///retirement.txt";

        assert_eq!(
            post_json(
                &router,
                &bearer,
                "/documents",
                serde_json::json!({"uri": uri, "content": "still authoritative"}),
            )
            .await
            .status(),
            StatusCode::ACCEPTED
        );
        vector_store.fail_next_delete();
        assert_eq!(
            post_json(
                &router,
                &bearer,
                "/documents",
                serde_json::json!({"uri": uri, "content": "must not publish"}),
            )
            .await
            .status(),
            StatusCode::ACCEPTED
        );

        let record = store
            .get_document(openwave_core::DocumentId::derive(uri))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.canonical_text, "must not publish");
        assert_eq!(record.content_revision, 2);
        assert_eq!(record.indexed_revision, None);
        assert_eq!(record.index_fingerprint, None);
        assert_eq!(record.indexed_at, None);
    }

    #[tokio::test]
    async fn first_ingest_persists_source_without_attempting_vector_retirement() {
        let vector_store = Arc::new(FailNextDeleteVectorStore::new(HashEmbedder::DEFAULT_DIMS));
        vector_store.fail_next_delete();
        let retrieval = Arc::new(Retriever::new(
            Box::new(PlainTextParser::new()),
            Box::new(TextChunker::default()),
            Arc::new(HashEmbedder::default()),
            vector_store,
        ));
        let (router, token, store, _dir) =
            test_app_with_retrieval(Arc::new(FakeProvider), retrieval).await;
        let bearer = format!("Bearer {token}");
        let uri = "file:///first-source.txt";

        assert_eq!(
            post_json(
                &router,
                &bearer,
                "/documents",
                serde_json::json!({"uri": uri, "content": "source comes first"}),
            )
            .await
            .status(),
            StatusCode::ACCEPTED
        );
        let record = store
            .get_document(openwave_core::DocumentId::derive(uri))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.canonical_text, "source comes first");
        assert_eq!(record.indexed_revision, None);
        assert_eq!(
            record.processing_status,
            openwave_core::DocumentProcessingStatus::Queued
        );
    }

    #[tokio::test]
    async fn document_catalog_pages_metadata_and_keeps_project_content_private() {
        let (router, token, store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let ingested: serde_json::Value = json_body(
            post_json(
                &router,
                &bearer,
                "/documents",
                serde_json::json!({
                    "uri": "file:///catalog.txt",
                    "media_type": "text/markdown",
                    "content": "# Catalog\n\nDurable source",
                }),
            )
            .await,
        )
        .await;
        let id = ingested["document_id"].as_str().unwrap().to_owned();

        for suffix in ["second", "third"] {
            assert_eq!(
                post_json(
                    &router,
                    &bearer,
                    "/documents",
                    serde_json::json!({
                        "uri": format!("file:///{suffix}.txt"),
                        "content": format!("{suffix} document"),
                    }),
                )
                .await
                .status(),
                StatusCode::ACCEPTED
            );
        }

        let project = make_project(&router, &bearer).await;
        let project_document_id = openwave_core::DocumentId::new();
        let now = chrono::Utc::now();
        store
            .create_document(&openwave_core::DocumentRecord {
                id: project_document_id,
                project_id: Some(project.id),
                source_uri: Some("file:///project-secret.txt".into()),
                media_type: "text/plain".into(),
                title: None,
                source_blob: None,
                canonical_text: "project-only source".into(),
                canonical_fingerprint: None,
                source_regions: Vec::new(),
                content_revision: 1,
                revision_token: uuid::Uuid::new_v4(),
                processing_status: openwave_core::DocumentProcessingStatus::Queued,
                indexed_revision: None,
                index_fingerprint: None,
                created_at: now,
                updated_at: now,
                indexed_at: None,
            })
            .await
            .unwrap();

        let get = |uri: String| {
            let router = router.clone();
            let bearer = bearer.clone();
            async move {
                router
                    .oneshot(
                        Request::builder()
                            .uri(uri)
                            .header(header::AUTHORIZATION, bearer)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap()
            }
        };

        let first = get("/documents?limit=2".into()).await;
        assert_eq!(first.status(), StatusCode::OK);
        let first: serde_json::Value = json_body(first).await;
        let first_documents = first["documents"].as_array().unwrap();
        assert_eq!(first_documents.len(), 2);
        let cursor = first["next_cursor"].as_str().expect("a second page");
        assert!(first_documents.iter().all(|summary| {
            summary.get("content").is_none() && summary.get("revision_token").is_none()
        }));

        let second = get(format!("/documents?limit=2&cursor={cursor}")).await;
        assert_eq!(second.status(), StatusCode::OK);
        let second: serde_json::Value = json_body(second).await;
        let second_documents = second["documents"].as_array().unwrap();
        assert_eq!(second_documents.len(), 1);
        assert!(second["next_cursor"].is_null());

        let listed_ids: std::collections::HashSet<_> = first_documents
            .iter()
            .chain(second_documents)
            .map(|summary| summary["document_id"].as_str().unwrap())
            .collect();
        assert_eq!(listed_ids.len(), 3);
        assert!(listed_ids.contains(id.as_str()));
        assert!(!listed_ids.contains(project_document_id.to_string().as_str()));

        let catalog_summary = first_documents
            .iter()
            .chain(second_documents)
            .find(|summary| summary["document_id"] == id)
            .unwrap();
        assert_eq!(catalog_summary["uri"], "file:///catalog.txt");
        assert_eq!(catalog_summary["media_type"], "text/markdown");
        assert_eq!(catalog_summary["content_revision"], 1);
        assert_eq!(catalog_summary["processing_status"], "queued");
        assert!(catalog_summary["indexed_revision"].is_null());

        let detail = get(format!("/documents/{id}")).await;
        assert_eq!(detail.status(), StatusCode::OK);
        let detail: serde_json::Value = json_body(detail).await;
        assert_eq!(detail["content"], "# Catalog\n\nDurable source");
        assert_eq!(detail["document_id"], id);
        assert!(detail.get("revision_token").is_none());

        assert_eq!(
            get(format!("/documents/{project_document_id}"))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );

        assert_eq!(
            get("/documents?limit=0".into()).await.status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            get("/documents?cursor=garbage".into()).await.status(),
            StatusCode::BAD_REQUEST
        );

        assert_eq!(
            get(format!("/documents/{}", openwave_core::DocumentId::new()))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn document_catalog_cursor_preserves_nanosecond_ordering() {
        let (router, token, store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let mut expected = Vec::new();

        for nanos in [900, 800, 700] {
            let id = openwave_core::DocumentId::new();
            let created_at = chrono::DateTime::from_timestamp(1_700_000_000, nanos).unwrap();
            store
                .create_document(&openwave_core::DocumentRecord {
                    id,
                    project_id: None,
                    source_uri: Some(format!("file:///{nanos}.txt")),
                    media_type: "text/plain".into(),
                    title: None,
                    source_blob: None,
                    canonical_text: nanos.to_string(),
                    canonical_fingerprint: None,
                    source_regions: Vec::new(),
                    content_revision: 1,
                    revision_token: uuid::Uuid::new_v4(),
                    processing_status: openwave_core::DocumentProcessingStatus::Queued,
                    indexed_revision: None,
                    index_fingerprint: None,
                    created_at,
                    updated_at: created_at,
                    indexed_at: None,
                })
                .await
                .unwrap();
            expected.push(id.to_string());
        }

        let mut uri = "/documents?limit=1".to_owned();
        let mut actual = Vec::new();
        loop {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(&uri)
                        .header(header::AUTHORIZATION, &bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let page: serde_json::Value = json_body(response).await;
            let documents = page["documents"].as_array().unwrap();
            assert_eq!(documents.len(), 1);
            actual.push(documents[0]["document_id"].as_str().unwrap().to_owned());
            let Some(cursor) = page["next_cursor"].as_str() else {
                break;
            };
            uri = format!("/documents?limit=1&cursor={cursor}");
        }

        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn concurrent_same_document_ingests_publish_in_request_order() {
        let embedder = Arc::new(FirstBatchGatedEmbedder {
            inner: HashEmbedder::default(),
            calls: AtomicUsize::new(0),
            entered: Notify::new(),
            release: Notify::new(),
        });
        let retrieval = Arc::new(Retriever::new(
            Box::new(PlainTextParser::new()),
            Box::new(TextChunker::default()),
            embedder.clone(),
            Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
        ));
        let (router, token, store, _dir, worker) =
            test_app_with_retrieval_and_worker(Arc::new(FakeProvider), retrieval).await;
        let bearer = format!("Bearer {token}");

        let first = post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({
                "uri": "file:///concurrent.txt",
                "content": "first version",
            }),
        )
        .await;
        let second = post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({
                "uri": "file:///concurrent.txt",
                "content": "second version",
            }),
        )
        .await;
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        assert_eq!(second.status(), StatusCode::ACCEPTED);
        assert_eq!(embedder.calls.load(Ordering::SeqCst), 0);
        let record = store
            .get_document(openwave_core::DocumentId::derive("file:///concurrent.txt"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.canonical_text, "second version");
        assert_eq!(record.content_revision, 2);
        assert_eq!(record.indexed_revision, None);
        let jobs = store.list_document_jobs(record.id).await.unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].status, openwave_core::DocumentJobStatus::Cancelled);
        assert_eq!(jobs[1].status, openwave_core::DocumentJobStatus::Queued);

        let run = tokio::spawn(async move { worker.run_once().await.unwrap() });
        tokio::time::timeout(Duration::from_secs(1), embedder.entered.notified())
            .await
            .expect("worker did not reach embedding");
        assert_eq!(embedder.calls.load(Ordering::SeqCst), 1);
        embedder.release.notify_one();
        assert!(matches!(
            run.await.unwrap(),
            document_worker::WorkerOutcome::Completed(_)
        ));
        let record = store.get_document(record.id).await.unwrap().unwrap();
        assert_eq!(record.indexed_revision, Some(2));
    }

    #[tokio::test]
    async fn deleting_a_document_removes_it_from_the_index() {
        let (router, token, store, _dir, worker) = test_app_with_worker().await;
        let bearer = format!("Bearer {token}");
        let ingest: serde_json::Value = json_body(
            post_json(
                &router,
                &bearer,
                "/documents",
                serde_json::json!({ "uri": "file:///doc.txt", "content": "Jupiter is a gas giant." }),
            )
            .await,
        )
        .await;
        let id = ingest["document_id"].as_str().unwrap().to_string();
        assert!(matches!(
            worker.run_once().await.unwrap(),
            document_worker::WorkerOutcome::Completed(_)
        ));

        let delete = |id: String| {
            let router = router.clone();
            let bearer = bearer.clone();
            async move {
                router
                    .oneshot(
                        Request::builder()
                            .method("DELETE")
                            .uri(format!("/documents/{id}"))
                            .header(header::AUTHORIZATION, &bearer)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap()
                    .status()
            }
        };

        assert_eq!(delete(id.clone()).await, StatusCode::ACCEPTED);
        assert!(matches!(
            worker.run_once().await.unwrap(),
            document_worker::WorkerOutcome::Retired(_)
        ));
        // Gone from the index.
        let results: serde_json::Value = json_body(
            post_json(
                &router,
                &bearer,
                "/search",
                serde_json::json!({ "query": "gas giant" }),
            )
            .await,
        )
        .await;
        assert!(results["citations"].as_array().unwrap().is_empty());
        // Idempotent: deleting again is still accepted.
        assert_eq!(delete(id.clone()).await, StatusCode::ACCEPTED);
        assert_eq!(store.get_document(id.parse().unwrap()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn durable_worker_retries_a_failed_tombstone_publication() {
        let vector_store = Arc::new(FailNextDeleteVectorStore::new(HashEmbedder::DEFAULT_DIMS));
        let retrieval = Arc::new(Retriever::new(
            Box::new(PlainTextParser::new()),
            Box::new(TextChunker::default()),
            Arc::new(HashEmbedder::default()),
            vector_store.clone(),
        ));
        let (router, token, store, _dir, worker) =
            test_app_with_retrieval_and_worker(Arc::new(FakeProvider), retrieval).await;
        let bearer = format!("Bearer {token}");
        let ingest: serde_json::Value = json_body(
            post_json(
                &router,
                &bearer,
                "/documents",
                serde_json::json!({
                    "uri": "file:///retry-delete.txt",
                    "content": "retire this searchable source"
                }),
            )
            .await,
        )
        .await;
        assert!(matches!(
            worker.run_once().await.unwrap(),
            document_worker::WorkerOutcome::Completed(_)
        ));
        let id = ingest["document_id"].as_str().unwrap();
        vector_store.fail_next_delete();

        let delete = |id: &str| {
            let router = router.clone();
            let bearer = bearer.clone();
            let uri = format!("/documents/{id}");
            async move {
                router
                    .oneshot(
                        Request::builder()
                            .method("DELETE")
                            .uri(uri)
                            .header(header::AUTHORIZATION, bearer)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap()
                    .status()
            }
        };
        assert_eq!(delete(id).await, StatusCode::ACCEPTED);
        assert_eq!(store.get_document(id.parse().unwrap()).await.unwrap(), None);
        assert!(worker.run_once().await.is_err());
        let visible: serde_json::Value = json_body(
            post_json(
                &router,
                &bearer,
                "/search",
                serde_json::json!({"query": "searchable source"}),
            )
            .await,
        )
        .await;
        assert_eq!(visible["citations"].as_array().unwrap().len(), 1);

        assert!(matches!(
            worker.run_once().await.unwrap(),
            document_worker::WorkerOutcome::Retired(_)
        ));
        let cleared: serde_json::Value = json_body(
            post_json(
                &router,
                &bearer,
                "/search",
                serde_json::json!({"query": "searchable source"}),
            )
            .await,
        )
        .await;
        assert!(cleared["citations"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn re_ingesting_the_same_uri_is_idempotent() {
        let (router, token, _store, _dir, worker) = test_app_with_worker().await;
        let bearer = format!("Bearer {token}");
        let doc = serde_json::json!({
            "uri": "file:///notes.txt",
            "content": "one two three four five six seven eight nine ten",
        });

        let first: serde_json::Value =
            json_body(post_json(&router, &bearer, "/documents", doc.clone()).await).await;
        let second: serde_json::Value =
            json_body(post_json(&router, &bearer, "/documents", doc).await).await;
        // Same URI => same derived document id => replaced in place.
        assert_eq!(first["document_id"], second["document_id"]);
        assert!(matches!(
            worker.run_once().await.unwrap(),
            document_worker::WorkerOutcome::Completed(_)
        ));

        // A broad search still returns each chunk once, not doubled.
        let results: serde_json::Value = json_body(
            post_json(
                &router,
                &bearer,
                "/search",
                serde_json::json!({ "query": "three four five", "k": 50 }),
            )
            .await,
        )
        .await;
        let citations = results["citations"].as_array().unwrap();
        let ids: std::collections::HashSet<_> = citations
            .iter()
            .map(|c| c["chunk_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), citations.len());
    }

    #[tokio::test]
    async fn a_padded_uri_targets_the_same_document() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");

        // Surrounding whitespace must not change the derived document id, or
        // "re-ingest the same file" would silently create a second document.
        let padded: serde_json::Value = json_body(
            post_json(
                &router,
                &bearer,
                "/documents",
                serde_json::json!({ "uri": "  file:///a.txt  ", "content": "hello world" }),
            )
            .await,
        )
        .await;
        let clean: serde_json::Value = json_body(
            post_json(
                &router,
                &bearer,
                "/documents",
                serde_json::json!({ "uri": "file:///a.txt", "content": "hello world" }),
            )
            .await,
        )
        .await;
        assert_eq!(padded["document_id"], clean["document_id"]);
    }

    #[tokio::test]
    async fn ingest_rejects_empty_content_and_search_rejects_empty_query() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");

        let bad_ingest = post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({ "content": "   " }),
        )
        .await;
        assert_eq!(bad_ingest.status(), StatusCode::BAD_REQUEST);

        let bad_search = post_json(
            &router,
            &bearer,
            "/search",
            serde_json::json!({ "query": "  " }),
        )
        .await;
        assert_eq!(bad_search.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn ingest_rejects_unsupported_media_type() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let response = post_json(
            &router,
            &bearer,
            "/documents",
            serde_json::json!({ "content": "%PDF-1.7", "media_type": "application/pdf" }),
        )
        .await;
        // A parser that can't handle the media type is the caller's problem: 400.
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let info: AgentErrorInfo = json_body(response).await;
        assert_eq!(info.kind, "bad_request");
    }

    #[tokio::test]
    async fn search_on_an_empty_index_returns_no_citations() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let results: serde_json::Value = json_body(
            post_json(
                &router,
                &bearer,
                "/search",
                serde_json::json!({ "query": "anything" }),
            )
            .await,
        )
        .await;
        assert!(results["citations"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn root_search_never_returns_project_owned_vectors() {
        let vectors = Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS));
        vectors
            .upsert(vec![VectorRecord {
                project_id: Some(ProjectId::new()),
                chunk: openwave_retrieval::Chunk::new(
                    openwave_core::DocumentId::new(),
                    0,
                    openwave_retrieval::ByteSpan::new(0, 14),
                    "project secret",
                ),
                embedding: Embedding(vec![0.0; HashEmbedder::DEFAULT_DIMS]),
            }])
            .await
            .unwrap();
        let (retrieval, _search) = build_retrieval(Arc::new(HashEmbedder::default()), vectors);
        let (router, token, _store, _dir) =
            test_app_with_retrieval(Arc::new(FakeProvider), retrieval).await;
        let results: serde_json::Value = json_body(
            post_json(
                &router,
                &format!("Bearer {token}"),
                "/search",
                serde_json::json!({"query": "project secret"}),
            )
            .await,
        )
        .await;
        assert!(results["citations"].as_array().unwrap().is_empty());
    }

    #[test]
    fn agent_deps_registers_the_search_tool_alongside_the_file_tools() {
        let (_retrieval, tools, _config) = agent_deps(
            Arc::new(HashEmbedder::default()),
            Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
        );
        let names: Vec<String> = tools.specs().into_iter().map(|s| s.name).collect();
        assert!(
            names.iter().any(|n| n == "search"),
            "search tool registered"
        );
        assert!(
            names.iter().any(|n| n == "read_file"),
            "file tools still present"
        );
    }

    #[tokio::test]
    async fn catalog_delete_failure_leaves_source_stale_and_repairable() {
        let dir = tempfile::tempdir().unwrap();
        let inner: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("delete-failure.db").display()
            ))
            .await
            .unwrap(),
        );
        let store = Arc::new(PauseTerminalStore::new(
            inner,
            Arc::new(Notify::new()),
            Arc::new(Notify::new()),
        ));
        let retrieval = Arc::new(Retriever::new(
            Box::new(PlainTextParser::new()),
            Box::new(TextChunker::default()),
            Arc::new(HashEmbedder::default()),
            Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
        ));
        let (router, token, store_view, _dir) =
            test_app_from_parts(Arc::new(FakeProvider), retrieval, store.clone(), dir);
        let bearer = format!("Bearer {token}");
        let uri = "file:///delete-failure.txt";
        let ingested: serde_json::Value = json_body(
            post_json(
                &router,
                &bearer,
                "/documents",
                serde_json::json!({"uri": uri, "content": "rebuildable source"}),
            )
            .await,
        )
        .await;
        let id = ingested["document_id"].as_str().unwrap().to_string();

        store.fail_next_document_delete();
        let delete = |id: String| {
            let router = router.clone();
            let bearer = bearer.clone();
            async move {
                router
                    .oneshot(
                        Request::builder()
                            .method("DELETE")
                            .uri(format!("/documents/{id}"))
                            .header(header::AUTHORIZATION, bearer)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap()
            }
        };
        assert_eq!(
            delete(id.clone()).await.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        let record = store_view
            .get_document(id.parse().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.canonical_text, "rebuildable source");
        assert_eq!(record.indexed_revision, None);
        assert_eq!(record.index_fingerprint, None);
        assert_eq!(record.indexed_at, None);

        let search: serde_json::Value = json_body(
            post_json(
                &router,
                &bearer,
                "/search",
                serde_json::json!({"query": "rebuildable source"}),
            )
            .await,
        )
        .await;
        assert!(search["citations"].as_array().unwrap().is_empty());
        assert_eq!(delete(id).await.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn resolve_embedder_uses_openai_only_when_enabled_and_keyed() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let secrets = MemSecrets::default();
        providers::write_credential(
            &secrets,
            providers::ProviderKind::Openai,
            &providers::ProviderCredential::api_key("sk-openai-test"),
        )
        .await
        .unwrap();

        // Enabled + keyed → the real 1536-dim embedder. A stored credential takes
        // precedence over any env var, so this is deterministic; construction only,
        // no network call.
        providers::write_config(
            &*store,
            providers::ProviderKind::Openai,
            &providers::ProviderConfig {
                enabled: true,
                base_url: None,
            },
        )
        .await
        .unwrap();
        let online = resolve_embedder(&*store, &secrets).await;
        assert_eq!(online.dimensions(), EMBED_DIMS);
        assert_ne!(EMBED_DIMS, HashEmbedder::default().dimensions());

        // Disabled but keyed → the key is ignored (no silent egress), even though
        // it's present. Deterministic regardless of any ambient OPENAI_API_KEY,
        // since a disabled provider never consults the key at all.
        providers::write_config(
            &*store,
            providers::ProviderKind::Openai,
            &providers::ProviderConfig {
                enabled: false,
                base_url: None,
            },
        )
        .await
        .unwrap();
        let offline = resolve_embedder(&*store, &secrets).await;
        assert_eq!(offline.dimensions(), HashEmbedder::default().dimensions());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn connect_vector_store_opens_a_durable_lance_index_under_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());

        // Ingest into the store, then reopen from the same data_dir and confirm the
        // chunk survived — i.e. bind()'s production path really persists to disk.
        {
            let store = connect_vector_store(&config, 2).await.unwrap();
            let doc = openwave_retrieval::DocumentId::new();
            let chunk = openwave_retrieval::Chunk::new(
                doc,
                0,
                openwave_retrieval::ByteSpan::new(0, 4),
                "note",
            );
            store
                .upsert(vec![openwave_retrieval::VectorRecord {
                    project_id: None,
                    chunk,
                    embedding: openwave_retrieval::Embedding(vec![1.0, 0.0]),
                }])
                .await
                .unwrap();
            assert_eq!(store.len().await.unwrap(), 1);
        }
        assert!(
            dir.path().join("vectors").exists(),
            "lance dir created under data_dir"
        );
        let reopened = connect_vector_store(&config, 2).await.unwrap();
        assert_eq!(reopened.len().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn health_needs_no_token() {
        let (router, _token, _store, _dir) = test_app().await;
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn api_rejects_missing_and_wrong_tokens() {
        let (router, _token, _store, _dir) = test_app().await;
        let missing = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/chats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let wrong = router
            .oneshot(
                Request::builder()
                    .uri("/chats")
                    .header(header::AUTHORIZATION, "Bearer not-the-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn retrieval_routes_require_a_token() {
        let (router, _token, _store, _dir) = test_app().await;
        // Both retrieval routes sit behind the bearer-token layer, not out in the
        // open like /healthz — a request with no token is rejected before it runs.
        for uri in ["/documents", "/search"] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(uri)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(serde_json::json!({}).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{uri} must require a token"
            );
        }
    }

    #[tokio::test]
    async fn create_then_get_and_list() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");

        let created: Chat = {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/chats")
                        .header(header::AUTHORIZATION, &bearer)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            serde_json::json!({"workspace_dir": "/tmp/ws", "title": "hi"})
                                .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::CREATED);
            json_body(response).await
        };
        assert_eq!(created.title.as_deref(), Some("hi"));

        let fetched: Chat = {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/chats/{}", created.id))
                        .header(header::AUTHORIZATION, &bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            json_body(response).await
        };
        assert_eq!(fetched, created);

        let listed: Vec<Chat> = {
            let response = router
                .oneshot(
                    Request::builder()
                        .uri("/chats")
                        .header(header::AUTHORIZATION, &bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            json_body(response).await
        };
        assert_eq!(listed, vec![created]);
    }

    /// Create a project and return it.
    async fn make_project(router: &Router, bearer: &str) -> Project {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/projects")
                    .header(header::AUTHORIZATION, bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"workspace_dir": "/tmp/proj", "title": "p"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        json_body(response).await
    }

    #[tokio::test]
    async fn project_create_get_and_list() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let created = make_project(&router, &bearer).await;
        assert_eq!(created.title.as_deref(), Some("p"));

        let fetched: Project = {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/projects/{}", created.id))
                        .header(header::AUTHORIZATION, &bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            json_body(response).await
        };
        assert_eq!(fetched, created);

        let listed: Vec<Project> = {
            let response = router
                .oneshot(
                    Request::builder()
                        .uri("/projects")
                        .header(header::AUTHORIZATION, &bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            json_body(response).await
        };
        assert_eq!(listed, vec![created]);
    }

    #[tokio::test]
    async fn chat_can_be_filed_under_a_project() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let project = make_project(&router, &bearer).await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chats")
                    .header(header::AUTHORIZATION, &bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"workspace_dir": "/tmp/ws", "project_id": project.id})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let chat: Chat = json_body(response).await;
        assert_eq!(chat.project_id, Some(project.id));
    }

    #[tokio::test]
    async fn chat_referencing_an_unknown_project_is_rejected() {
        let (router, token, _store, _dir) = test_app().await;
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chats")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"workspace_dir": "/tmp/ws", "project_id": ProjectId::new()})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let info: AgentErrorInfo = json_body(response).await;
        assert_eq!(info.kind, "bad_request");
    }

    #[tokio::test]
    async fn models_catalog_is_served() {
        let (router, token, _store, _dir) = test_app().await;
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/models")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let catalog: serde_json::Value = json_body(response).await;
        let models = catalog["models"].as_array().unwrap();
        assert!(!models.is_empty());
        assert!(models.iter().any(|m| m["provider"] == "anthropic"));
    }

    #[tokio::test]
    async fn chat_created_with_a_model() {
        let (router, token, _store, _dir) = test_app().await;
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chats")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"workspace_dir": "/tmp/ws", "model": "claude-x"})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let chat: Chat = json_body(response).await;
        assert_eq!(chat.model.as_deref(), Some("claude-x"));
    }

    #[tokio::test]
    async fn chat_created_with_empty_model_is_rejected() {
        let (router, token, _store, _dir) = test_app().await;
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chats")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"workspace_dir": "/tmp/ws", "model": ""}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let info: AgentErrorInfo = json_body(response).await;
        assert_eq!(info.kind, "bad_request");
    }

    /// PATCH a chat's model with a raw JSON body, returning the response.
    async fn patch_chat(
        router: &Router,
        bearer: &str,
        chat: ChatId,
        body: serde_json::Value,
    ) -> axum::response::Response {
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/chats/{chat}"))
                    .header(header::AUTHORIZATION, bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn patch_chat_sets_and_clears_the_model() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let chat = make_chat(&router, &bearer).await;
        assert_eq!(chat.model, None);

        let set = patch_chat(
            &router,
            &bearer,
            chat.id,
            serde_json::json!({"model": "m1"}),
        )
        .await;
        assert_eq!(set.status(), StatusCode::OK);
        assert_eq!(json_body::<Chat>(set).await.model.as_deref(), Some("m1"));

        let cleared = patch_chat(
            &router,
            &bearer,
            chat.id,
            serde_json::json!({"model": null}),
        )
        .await;
        assert_eq!(cleared.status(), StatusCode::OK);
        assert_eq!(json_body::<Chat>(cleared).await.model, None);
    }

    #[tokio::test]
    async fn patch_chat_rejects_empty_model_and_unknown_chat() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let chat = make_chat(&router, &bearer).await;

        let empty = patch_chat(&router, &bearer, chat.id, serde_json::json!({"model": ""})).await;
        assert_eq!(empty.status(), StatusCode::BAD_REQUEST);

        let missing = patch_chat(
            &router,
            &bearer,
            ChatId::new(),
            serde_json::json!({"model": "m"}),
        )
        .await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn chat_model_takes_precedence_over_the_default() {
        let recorder = RecordingProvider::default();
        let (router, token, store, _dir) = test_app_with(Arc::new(recorder.clone())).await;
        let bearer = format!("Bearer {token}");

        // A global default is set, but the chat picks its own model — the chat wins.
        let set_default = put_settings(
            &router,
            &bearer,
            serde_json::json!({"model": "default-model"}),
        )
        .await;
        assert_eq!(set_default.status(), StatusCode::OK);
        let chat = make_chat(&router, &bearer).await;
        let patched = patch_chat(
            &router,
            &bearer,
            chat.id,
            serde_json::json!({"model": "chat-model"}),
        )
        .await;
        assert_eq!(patched.status(), StatusCode::OK);

        assert_eq!(
            send_message(&router, &bearer, chat.id, "hi").await,
            StatusCode::ACCEPTED
        );
        wait_for_turn(&store, chat.id).await;
        assert!(
            recorder
                .models
                .lock()
                .unwrap()
                .iter()
                .any(|m| m == "chat-model"),
            "the chat's own model should win over the global default"
        );
    }

    #[tokio::test]
    async fn settings_default_then_update_roundtrips() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");

        // Default: no model configured.
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/settings")
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let settings: serde_json::Value = json_body(response).await;
        assert!(settings["model"].is_null());

        // PUT a model, and it comes back.
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/settings")
                    .header(header::AUTHORIZATION, &bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"model": "claude-x"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let settings: serde_json::Value = json_body(response).await;
        assert_eq!(settings["model"], "claude-x");

        // GET reflects the update.
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/settings")
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let settings: serde_json::Value = json_body(response).await;
        assert_eq!(settings["model"], "claude-x");
    }

    /// PUT /settings with a raw JSON body, returning the response.
    async fn put_settings(
        router: &Router,
        bearer: &str,
        body: serde_json::Value,
    ) -> axum::response::Response {
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/settings")
                    .header(header::AUTHORIZATION, bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn put_empty_model_is_rejected() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let response = put_settings(&router, &bearer, serde_json::json!({"model": ""})).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let info: AgentErrorInfo = json_body(response).await;
        assert_eq!(info.kind, "bad_request");
    }

    #[tokio::test]
    async fn put_non_string_model_is_rejected() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        // A number where a string is expected fails extraction as a JSON 400.
        let response = put_settings(&router, &bearer, serde_json::json!({"model": 5})).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let info: AgentErrorInfo = json_body(response).await;
        assert_eq!(info.kind, "bad_request");
    }

    #[tokio::test]
    async fn explicit_null_model_clears_a_configured_one() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");

        // Set, then clear with an explicit null.
        let set = put_settings(&router, &bearer, serde_json::json!({"model": "claude-x"})).await;
        assert_eq!(set.status(), StatusCode::OK);
        let cleared = put_settings(&router, &bearer, serde_json::json!({"model": null})).await;
        assert_eq!(cleared.status(), StatusCode::OK);
        let settings: serde_json::Value = json_body(cleared).await;
        assert!(
            settings["model"].is_null(),
            "explicit null resets the model"
        );

        // An empty body leaves the (now-cleared) value unchanged.
        let untouched = put_settings(&router, &bearer, serde_json::json!({})).await;
        let settings: serde_json::Value = json_body(untouched).await;
        assert!(settings["model"].is_null());
    }

    /// `has_api_key` from `GET /settings`.
    async fn api_key_configured(router: &Router, bearer: &str) -> bool {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/settings")
                    .header(header::AUTHORIZATION, bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        json_body::<serde_json::Value>(response).await["has_api_key"]
            .as_bool()
            .unwrap()
    }

    #[tokio::test]
    async fn api_key_put_configures_it_and_delete_reverts() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");

        // Capture the env-dependent baseline so the test is deterministic wherever
        // it runs, then assert the transitions the API drives.
        let baseline = api_key_configured(&router, &bearer).await;

        let put = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/settings/api-key")
                    .header(header::AUTHORIZATION, &bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"api_key": "sk-test"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(put.status(), StatusCode::NO_CONTENT);
        assert!(api_key_configured(&router, &bearer).await);

        let delete = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/settings/api-key")
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(delete.status(), StatusCode::NO_CONTENT);
        assert_eq!(api_key_configured(&router, &bearer).await, baseline);
    }

    #[tokio::test]
    async fn put_empty_api_key_is_rejected() {
        let (router, token, _store, _dir) = test_app().await;
        let response = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/settings/api-key")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::json!({"api_key": ""}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let info: AgentErrorInfo = json_body(response).await;
        assert_eq!(info.kind, "bad_request");
    }

    #[tokio::test]
    async fn providers_list_and_put_roundtrip() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");

        let list = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/providers")
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::OK);
        let body: serde_json::Value = json_body(list).await;
        let providers = body["providers"].as_array().unwrap();
        assert!(providers.iter().any(|p| p["kind"] == "anthropic"));
        assert!(providers.iter().any(|p| p["kind"] == "openai"));
        assert!(providers.iter().any(|p| p["kind"] == "openai_compatible"));

        let put = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/providers/openai")
                    .header(header::AUTHORIZATION, &bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "enabled": true,
                            "credential": {"type": "api_key", "key": "sk-openai"}
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(put.status(), StatusCode::OK);
        let info: serde_json::Value = json_body(put).await;
        assert_eq!(info["kind"], "openai");
        assert_eq!(info["enabled"], true);
        assert_eq!(info["has_credential"], true);
        assert!(info.get("credential").is_none());

        // Credential never appears on the list either.
        let list = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/providers")
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body: serde_json::Value = json_body(list).await;
        let openai = body["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["kind"] == "openai")
            .unwrap();
        assert_eq!(openai["has_credential"], true);
        assert!(openai.get("credential").is_none());

        let delete = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/providers/openai/credential")
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(delete.status(), StatusCode::NO_CONTENT);

        let list = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/providers")
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body: serde_json::Value = json_body(list).await;
        let openai = body["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["kind"] == "openai")
            .unwrap();
        assert_eq!(openai["has_credential"], false);
    }

    #[tokio::test]
    async fn openai_compatible_requires_base_url_when_enabled() {
        let (router, token, _store, _dir) = test_app().await;
        let response = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/providers/openai_compatible")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::json!({"enabled": true}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn unknown_provider_kind_is_404() {
        let (router, token, _store, _dir) = test_app().await;
        let response = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/providers/not-a-provider")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::json!({"enabled": true}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn models_catalog_includes_enabled_credentialed_providers() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");

        let put = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/providers/openai")
                    .header(header::AUTHORIZATION, &bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "enabled": true,
                            "credential": {"type": "api_key", "key": "sk-openai"}
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(put.status(), StatusCode::OK);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/models")
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let catalog: serde_json::Value = json_body(response).await;
        let models = catalog["models"].as_array().unwrap();
        assert!(models.iter().any(|m| m["provider"] == "openai"));
    }

    #[tokio::test]
    async fn resolver_builds_a_router_from_enabled_providers() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}/test.db?mode=rwc",
                dir.path().display()
            ))
            .await
            .unwrap(),
        );
        let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
        providers::write_credential(
            &*secrets,
            providers::ProviderKind::Anthropic,
            &providers::ProviderCredential::api_key("sk-test"),
        )
        .await
        .unwrap();
        providers::write_config(
            &*store,
            providers::ProviderKind::Anthropic,
            &providers::ProviderConfig {
                enabled: true,
                base_url: None,
            },
        )
        .await
        .unwrap();

        let resolver = resolver::KeyedResolver::new(store.clone(), secrets.clone());
        let resolved = resolver.resolve().await;
        // Composite router — selection happens on stream from req.model.
        assert_eq!(resolved.id().0, "router");

        // Same route set ⇒ the cached provider is reused.
        let again = resolver.resolve().await;
        assert!(Arc::ptr_eq(&resolved, &again));

        // Changing the key rebuilds it.
        providers::write_credential(
            &*secrets,
            providers::ProviderKind::Anthropic,
            &providers::ProviderCredential::api_key("sk-different"),
        )
        .await
        .unwrap();
        let rebuilt = resolver.resolve().await;
        assert!(!Arc::ptr_eq(&resolved, &rebuilt));
        assert_eq!(rebuilt.id().0, "router");

        // Disabling Anthropic with no other providers fails closed.
        providers::write_config(
            &*store,
            providers::ProviderKind::Anthropic,
            &providers::ProviderConfig {
                enabled: false,
                base_url: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(resolver.resolve().await.id().0, "unconfigured");
    }

    #[tokio::test]
    async fn resolver_includes_openai_when_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}/test.db?mode=rwc",
                dir.path().display()
            ))
            .await
            .unwrap(),
        );
        let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
        providers::write_credential(
            &*secrets,
            providers::ProviderKind::Openai,
            &providers::ProviderCredential::api_key("sk-openai"),
        )
        .await
        .unwrap();
        providers::write_config(
            &*store,
            providers::ProviderKind::Openai,
            &providers::ProviderConfig {
                enabled: true,
                base_url: None,
            },
        )
        .await
        .unwrap();

        let routes = providers::collect_routes(&*store, &*secrets).await;
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].kind, openwave_router::RouteKind::Openai);

        let resolver = resolver::KeyedResolver::new(store, secrets);
        let provider = resolver.resolve().await;
        assert_eq!(provider.id().0, "router");

        // A curated openai model is selectable; an anthropic model is not
        // (no anthropic route, no openai_compatible fallback).
        let router = openwave_router::Router::build(routes);
        assert_eq!(
            router.select("gpt-4o"),
            Some(openwave_router::RouteKind::Openai)
        );
        assert_eq!(router.select("claude-opus-4-8"), None);
    }

    #[tokio::test]
    async fn openai_compatible_route_is_free_form_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}/test.db?mode=rwc",
                dir.path().display()
            ))
            .await
            .unwrap(),
        );
        let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
        providers::write_credential(
            &*secrets,
            providers::ProviderKind::OpenaiCompatible,
            &providers::ProviderCredential::api_key("sk-local"),
        )
        .await
        .unwrap();
        providers::write_config(
            &*store,
            providers::ProviderKind::OpenaiCompatible,
            &providers::ProviderConfig {
                enabled: true,
                base_url: Some("http://127.0.0.1:1234/v1".into()),
            },
        )
        .await
        .unwrap();

        let routes = providers::collect_routes(&*store, &*secrets).await;
        let router = openwave_router::Router::build(routes);
        assert_eq!(
            router.select("llama-3-local"),
            Some(openwave_router::RouteKind::OpenaiCompatible)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn configured_model_is_used_for_the_turn() {
        let recorder = RecordingProvider::default();
        let (router, token, store, _dir) = test_app_with(Arc::new(recorder.clone())).await;
        let bearer = format!("Bearer {token}");
        let chat = make_chat(&router, &bearer).await;

        // Configure the model, then run a turn.
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/settings")
                    .header(header::AUTHORIZATION, &bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"model": "claude-configured"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        assert_eq!(
            send_message(&router, &bearer, chat.id, "hi").await,
            StatusCode::ACCEPTED
        );
        wait_for_turn(&store, chat.id).await;

        assert!(
            recorder
                .models
                .lock()
                .unwrap()
                .iter()
                .any(|m| m == "claude-configured"),
            "the turn should run against the configured model"
        );
    }

    #[tokio::test]
    async fn relative_workspace_dir_is_rejected() {
        let (router, token, _store, _dir) = test_app().await;
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chats")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"workspace_dir": "relative/dir"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let info: AgentErrorInfo = json_body(response).await;
        assert_eq!(info.kind, "bad_request");
    }

    #[tokio::test]
    async fn unknown_chat_is_404() {
        let (router, token, _store, _dir) = test_app().await;
        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/chats/{}", ChatId::new()))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn post_message_runs_a_turn_and_journals_its_events() {
        let (router, token, store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let chat = make_chat(&router, &bearer).await;

        assert_eq!(
            send_message(&router, &bearer, chat.id, "hello").await,
            StatusCode::ACCEPTED
        );

        let events = wait_for_turn(&store, chat.id).await;
        assert!(matches!(events[0].event, AgentEvent::TurnStarted { .. }));
        assert!(events
            .iter()
            .any(|e| matches!(&e.event, AgentEvent::TextDelta { text } if text == "hi")));
        assert!(events
            .iter()
            .any(|e| matches!(e.event, AgentEvent::TurnCompleted { .. })));
    }

    #[tokio::test]
    async fn message_to_unknown_chat_is_404() {
        let (router, token, _store, _dir) = test_app().await;
        assert_eq!(
            send_message(&router, &format!("Bearer {token}"), ChatId::new(), "hi").await,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_turn_on_the_same_chat_is_refused() {
        // A gated provider keeps the first turn active (blocked on the gate) while
        // we submit a second one, which must be refused with 409.
        let gate = Arc::new(Notify::new());
        let (router, token, _store, _dir) =
            test_app_with(Arc::new(GatedProvider { gate: gate.clone() })).await;
        let bearer = format!("Bearer {token}");
        let chat = make_chat(&router, &bearer).await;

        // The handler claims the chat's slot synchronously before returning, so by
        // the time this 202 is observed the turn is holding the slot.
        assert_eq!(
            send_message(&router, &bearer, chat.id, "one").await,
            StatusCode::ACCEPTED
        );
        assert_eq!(
            send_message(&router, &bearer, chat.id, "two").await,
            StatusCode::CONFLICT
        );

        // Release the first turn so it can finish and free the slot.
        gate.notify_one();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn slot_frees_after_a_turn_completes() {
        let (router, token, store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let chat = make_chat(&router, &bearer).await;

        assert_eq!(
            send_message(&router, &bearer, chat.id, "one").await,
            StatusCode::ACCEPTED
        );
        wait_for_turn(&store, chat.id).await;

        // The turn finished, so its slot is released and a follow-up is accepted.
        assert_eq!(
            send_message(&router, &bearer, chat.id, "two").await,
            StatusCode::ACCEPTED
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn slot_stays_held_until_journal_drains() {
        let dir = tempfile::tempdir().unwrap();
        let inner: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let store: Arc<dyn Store> = Arc::new(PauseTerminalStore::new(
            inner,
            entered.clone(),
            release.clone(),
        ));
        let state = AppState::new(
            Config::desktop(dir.path()),
            store.clone(),
            Arc::new(FixedResolver(Arc::new(FakeProvider))),
            Arc::new(MemSecrets::default()),
            Arc::new(ToolRegistry::new()),
            build_retrieval(
                Arc::new(HashEmbedder::default()),
                Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
            )
            .0,
            AgentConfig {
                model: "fake".into(),
                ..AgentConfig::default()
            },
        );
        let token = state.token.clone();
        let router = app(state);
        let bearer = format!("Bearer {token}");
        let chat = make_chat(&router, &bearer).await;

        assert_eq!(
            send_message(&router, &bearer, chat.id, "one").await,
            StatusCode::ACCEPTED
        );
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .expect("turn reached blocked terminal journal append");

        assert_eq!(
            send_message(&router, &bearer, chat.id, "two").await,
            StatusCode::CONFLICT,
            "slot must remain held while terminal event is still being journaled"
        );
        assert_eq!(
            steer_turn(&router, &bearer, chat.id, "late", false).await,
            StatusCode::CONFLICT,
            "steer must not 202 after the agent finished (journal still draining)"
        );
        assert_eq!(
            cancel_turn(&router, &bearer, chat.id).await,
            StatusCode::CONFLICT,
            "cancel must not 202 after the agent finished (journal still draining)"
        );

        release.notify_waiters();
        wait_for_turn(&store, chat.id).await;
        assert_eq!(
            send_message(&router, &bearer, chat.id, "two").await,
            StatusCode::ACCEPTED
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn turn_fails_closed_with_no_provider_configured() {
        // The unconfigured provider errors without any network call; the turn must
        // end in TurnFailed, not hang or egress.
        let (router, token, store, _dir) =
            test_app_with(Arc::new(crate::provider::UnconfiguredProvider)).await;
        let bearer = format!("Bearer {token}");
        let chat = make_chat(&router, &bearer).await;

        assert_eq!(
            send_message(&router, &bearer, chat.id, "hello").await,
            StatusCode::ACCEPTED
        );
        let events = wait_for_turn(&store, chat.id).await;
        assert!(matches!(
            events.last().unwrap().event,
            AgentEvent::TurnFailed { .. }
        ));
    }

    /// Sensitive tool that records whether it ran.
    struct SensitiveProbe {
        ran: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl Tool for SensitiveProbe {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "probe".into(),
                description: "sensitive probe".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }
        fn approval_class(&self) -> ApprovalClass {
            ApprovalClass::Sensitive
        }
        async fn execute(
            &self,
            _ctx: &ToolCtx,
            _args: serde_json::Value,
        ) -> openwave_core::Result<ToolOutput> {
            self.ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ToolOutput::text("probed"))
        }
    }

    /// Provider that asks for `probe` once, then finishes.
    struct ProbeProvider {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl ModelProvider for ProbeProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("probe")
        }
        async fn stream(
            &self,
            _req: ChatRequest,
        ) -> openwave_core::Result<BoxStream<'static, ProviderEvent>> {
            let events = if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "call_probe".into(),
                        name: "probe".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: "{}".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ]
            } else {
                vec![
                    ProviderEvent::TextDelta {
                        text: "done".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ]
            };
            Ok(stream::iter(events).boxed())
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn approval_endpoint_unparks_a_sensitive_tool() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tools =
            Arc::new(ToolRegistry::new().with(Box::new(SensitiveProbe { ran: ran.clone() })));
        let state = AppState::new(
            Config::desktop(dir.path()),
            store.clone(),
            Arc::new(FixedResolver(Arc::new(ProbeProvider {
                calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }))),
            Arc::new(MemSecrets::default()),
            tools,
            build_retrieval(
                Arc::new(HashEmbedder::default()),
                Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
            )
            .0,
            AgentConfig {
                model: "fake".into(),
                ..AgentConfig::default()
            },
        );
        let token = state.token.clone();
        let router = app(state);
        let bearer = format!("Bearer {token}");
        let chat = make_chat(&router, &bearer).await;

        assert_eq!(
            send_message(&router, &bearer, chat.id, "probe it").await,
            StatusCode::ACCEPTED
        );

        // Wait until the turn parks on ApprovalRequired.
        let call_id = {
            let mut found = None;
            for _ in 0..200 {
                let events = store.list_events(chat.id, 0).await.unwrap();
                if let Some(id) = events.iter().find_map(|e| match &e.event {
                    AgentEvent::ApprovalRequired { call_id, .. } => Some(*call_id),
                    _ => None,
                }) {
                    found = Some(id);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            found.expect("turn should park on ApprovalRequired")
        };
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 0);

        // Approve via the HTTP endpoint.
        let decide = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/chats/{}/approvals/{call_id}", chat.id))
                    .header(header::AUTHORIZATION, &bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"decision": "approve"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(decide.status(), StatusCode::NO_CONTENT);

        let events = wait_for_turn(&store, chat.id).await;
        assert!(events
            .iter()
            .any(|e| matches!(e.event, AgentEvent::ApprovalDecided { approved: true, .. })));
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(matches!(
            events.last().unwrap().event,
            AgentEvent::TurnCompleted { .. }
        ));

        // A second decide for the same call is 404 (already resolved).
        let again = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/chats/{}/approvals/{call_id}", chat.id))
                    .header(header::AUTHORIZATION, &bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"decision": "approve"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(again.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn bind_yields_a_loopback_addr_and_token() {
        openwave_core::KeychainSecretProvider::use_mock();
        let dir = tempfile::tempdir().unwrap();
        let server = bind(Config::desktop(dir.path())).await.unwrap();
        assert!(server.local_addr().ip().is_loopback());
        assert!(!server.token().is_empty());
    }

    #[tokio::test]
    async fn malformed_requests_get_json_errors_not_plaintext() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");

        // A non-UUID path segment: 400 with a parseable `{ kind, message }` body,
        // not axum's default plain-text rejection.
        let bad_path = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/chats/not-a-uuid")
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_path.status(), StatusCode::BAD_REQUEST);
        let info: AgentErrorInfo = json_body(bad_path).await;
        assert_eq!(info.kind, "bad_request");

        // A body with no `Content-Type: application/json`: also a JSON 400.
        let no_content_type = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chats")
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::from(r#"{"workspace_dir":"/tmp/ws"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(no_content_type.status(), StatusCode::BAD_REQUEST);
        let info: AgentErrorInfo = json_body(no_content_type).await;
        assert_eq!(info.kind, "bad_request");
    }

    #[tokio::test]
    async fn self_host_profile_is_not_yet_supported() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            profile: Profile::SelfHost,
            data_dir: dir.path().to_path_buf(),
        };
        assert!(bind(config).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn serve_answers_over_a_real_socket() {
        openwave_core::KeychainSecretProvider::use_mock();
        let dir = tempfile::tempdir().unwrap();
        let server = bind(Config::desktop(dir.path())).await.unwrap();
        let addr = server.local_addr();
        let token = server.token().to_string();
        // The listener is already bound, so connections queue immediately; drive
        // the accept loop in the background for the duration of the test.
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        let client = reqwest::Client::new();
        let health = client
            .get(format!("http://{addr}/healthz"))
            .send()
            .await
            .unwrap();
        assert_eq!(health.status(), reqwest::StatusCode::OK);

        let unauthed = client
            .get(format!("http://{addr}/chats"))
            .send()
            .await
            .unwrap();
        assert_eq!(unauthed.status(), reqwest::StatusCode::UNAUTHORIZED);

        let authed = client
            .get(format!("http://{addr}/chats"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(authed.status(), reqwest::StatusCode::OK);
        assert_eq!(authed.json::<Vec<Chat>>().await.unwrap(), vec![]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cors_preflight_allows_localhost_origin() {
        openwave_core::KeychainSecretProvider::use_mock();
        let dir = tempfile::tempdir().unwrap();
        let server = bind(Config::desktop(dir.path())).await.unwrap();
        let addr = server.local_addr();
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        let client = reqwest::Client::new();
        let preflight = client
            .request(reqwest::Method::OPTIONS, format!("http://{addr}/chats"))
            .header(reqwest::header::ORIGIN, "http://localhost:1420")
            .header(reqwest::header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
            .header(
                reqwest::header::ACCESS_CONTROL_REQUEST_HEADERS,
                "authorization",
            )
            .send()
            .await
            .unwrap();
        assert_eq!(preflight.status(), reqwest::StatusCode::OK);
        let allow_origin = preflight
            .headers()
            .get(reqwest::header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok());
        assert_eq!(allow_origin, Some("http://localhost:1420"));
    }

    // --- WebSocket event stream ---

    use std::net::SocketAddr;

    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    /// Serve a router (with the given provider) over a real loopback socket.
    async fn serve_app_with(
        provider: Arc<dyn ModelProvider>,
    ) -> (SocketAddr, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
        let (router, token, store, dir) = test_app_with(provider).await;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        (addr, token, store, dir)
    }

    async fn make_chat_http(client: &reqwest::Client, addr: SocketAddr, token: &str) -> Chat {
        client
            .post(format!("http://{addr}/chats"))
            .bearer_auth(token)
            .json(&serde_json::json!({"workspace_dir": "/tmp/ws"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn send_message_http(
        client: &reqwest::Client,
        addr: SocketAddr,
        token: &str,
        chat: ChatId,
    ) {
        let response = client
            .post(format!("http://{addr}/chats/{chat}/messages"))
            .bearer_auth(token)
            .json(&serde_json::json!({"content": "hi"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    }

    /// Connect to a chat's event socket (authenticated) and read frames until
    /// `want` turns have ended (or a timeout), returning the decoded events in
    /// arrival order.
    async fn read_until_turns_end(
        addr: SocketAddr,
        token: &str,
        chat: ChatId,
        after: i64,
        want: usize,
    ) -> Vec<SequencedEvent> {
        let mut request = format!("ws://{addr}/chats/{chat}/events?after={after}")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("Authorization", format!("Bearer {token}").parse().unwrap());
        let (mut socket, _response) = connect_async(request).await.unwrap();

        let mut events = Vec::new();
        let mut completed = 0;
        let read = async {
            while let Some(frame) = socket.next().await {
                let WsMessage::Text(text) = frame.unwrap() else {
                    continue;
                };
                let event: SequencedEvent = serde_json::from_str(text.as_str()).unwrap();
                if matches!(
                    event.event,
                    AgentEvent::TurnCompleted { .. } | AgentEvent::TurnFailed { .. }
                ) {
                    completed += 1;
                }
                events.push(event);
                if completed >= want {
                    break;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(5), read)
            .await
            .expect("turns did not complete over the socket");
        events
    }

    /// Read one turn's worth of events over a fresh connection.
    async fn read_until_turn_end(
        addr: SocketAddr,
        token: &str,
        chat: ChatId,
        after: i64,
    ) -> Vec<SequencedEvent> {
        read_until_turns_end(addr, token, chat, after, 1).await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ws_replays_a_finished_turn_from_the_journal() {
        let (addr, token, store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
        let client = reqwest::Client::new();
        let chat = make_chat_http(&client, addr, &token).await;

        // Run the turn to completion, then connect — everything comes from replay.
        send_message_http(&client, addr, &token, chat.id).await;
        wait_for_turn(&store, chat.id).await;

        let events = read_until_turn_end(addr, &token, chat.id, 0).await;
        assert_eq!(events.first().unwrap().seq, 1, "replay starts at seq 1");
        assert!(matches!(events[0].event, AgentEvent::TurnStarted { .. }));
        assert!(events
            .iter()
            .any(|e| matches!(&e.event, AgentEvent::TextDelta { text } if text == "hi")));
        assert!(matches!(
            events.last().unwrap().event,
            AgentEvent::TurnCompleted { .. }
        ));
        // Sequence numbers are strictly increasing.
        assert!(events.windows(2).all(|w| w[0].seq < w[1].seq));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ws_streams_a_turn_started_after_connecting() {
        let (addr, token, _store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
        let client = reqwest::Client::new();
        let chat = make_chat_http(&client, addr, &token).await;

        // Connect first (journal empty), then trigger the turn — events arrive live.
        let reader = {
            let token = token.clone();
            tokio::spawn(async move { read_until_turn_end(addr, &token, chat.id, 0).await })
        };
        send_message_http(&client, addr, &token, chat.id).await;

        let events = reader.await.unwrap();
        assert!(matches!(events[0].event, AgentEvent::TurnStarted { .. }));
        assert!(matches!(
            events.last().unwrap().event,
            AgentEvent::TurnCompleted { .. }
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ws_after_cursor_replays_only_newer_events() {
        let (addr, token, store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
        let client = reqwest::Client::new();
        let chat = make_chat_http(&client, addr, &token).await;
        send_message_http(&client, addr, &token, chat.id).await;
        wait_for_turn(&store, chat.id).await;

        // Resume after seq 1: the first replayed event must be seq 2, and seq 1 is
        // not re-sent.
        let events = read_until_turn_end(addr, &token, chat.id, 1).await;
        assert_eq!(events.first().unwrap().seq, 2);
        assert!(events.iter().all(|e| e.seq > 1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ws_replays_one_turn_then_streams_the_next_live() {
        let (addr, token, store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
        let client = reqwest::Client::new();
        let chat = make_chat_http(&client, addr, &token).await;

        // Turn 1 runs to completion and is journaled.
        send_message_http(&client, addr, &token, chat.id).await;
        wait_for_turn(&store, chat.id).await;

        // Connect (replays turn 1) and keep reading; then run turn 2, whose events
        // arrive live on the same connection. Assert both turns come through in
        // one gap-free, duplicate-free, strictly-increasing stream.
        let reader = {
            let token = token.clone();
            tokio::spawn(async move { read_until_turns_end(addr, &token, chat.id, 0, 2).await })
        };
        // Let the reader connect, subscribe, and drain the replay before turn 2.
        tokio::time::sleep(Duration::from_millis(100)).await;
        send_message_http(&client, addr, &token, chat.id).await;

        let events = reader.await.unwrap();
        assert!(matches!(events[0].event, AgentEvent::TurnStarted { .. }));
        assert_eq!(events[0].seq, 1);
        assert!(events.windows(2).all(|w| w[0].seq < w[1].seq));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.event, AgentEvent::TurnCompleted { .. }))
                .count(),
            2,
            "both turns completed over one connection"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ws_bad_after_cursor_is_a_json_400() {
        let (addr, token, _store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
        let client = reqwest::Client::new();
        let chat = make_chat_http(&client, addr, &token).await;
        // A non-integer `after` fails extraction; it must answer the API-wide
        // `{ kind, message }` JSON, not axum's plain-text rejection.
        let response = client
            .get(format!("http://{addr}/chats/{}/events?after=abc", chat.id))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
        let info: AgentErrorInfo = response.json().await.unwrap();
        assert_eq!(info.kind, "bad_request");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ws_without_a_token_is_rejected() {
        let (addr, _token, _store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
        let chat = ChatId::new();
        let request = format!("ws://{addr}/chats/{chat}/events")
            .into_client_request()
            .unwrap();
        // No Authorization header: the handshake must fail (auth runs before upgrade).
        assert!(connect_async(request).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ws_subprotocol_auth_succeeds() {
        use crate::auth::{WS_HANDSHAKE_SUBPROTOCOL, WS_TOKEN_SUBPROTOCOL_PREFIX};

        let (addr, token, store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
        let client = reqwest::Client::new();
        let chat = make_chat_http(&client, addr, &token).await;
        send_message_http(&client, addr, &token, chat.id).await;
        wait_for_turn(&store, chat.id).await;

        // Authenticate with Sec-WebSocket-Protocol only — no Authorization header.
        let mut request = format!("ws://{addr}/chats/{}/events?after=0", chat.id)
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            format!("{WS_HANDSHAKE_SUBPROTOCOL}, {WS_TOKEN_SUBPROTOCOL_PREFIX}{token}")
                .parse()
                .unwrap(),
        );
        let (mut socket, response) = connect_async(request).await.unwrap();
        // Server must select the handshake subprotocol.
        let selected = response
            .headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|v| v.to_str().ok());
        assert_eq!(selected, Some(WS_HANDSHAKE_SUBPROTOCOL));

        let mut saw_completed = false;
        let read = async {
            while let Some(frame) = socket.next().await {
                let WsMessage::Text(text) = frame.unwrap() else {
                    continue;
                };
                let event: SequencedEvent = serde_json::from_str(text.as_str()).unwrap();
                if matches!(event.event, AgentEvent::TurnCompleted { .. }) {
                    saw_completed = true;
                    break;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(5), read)
            .await
            .expect("turn did not complete over subprotocol-authed socket");
        assert!(saw_completed);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ws_subprotocol_wrong_token_is_rejected() {
        use crate::auth::{WS_HANDSHAKE_SUBPROTOCOL, WS_TOKEN_SUBPROTOCOL_PREFIX};

        let (addr, _token, _store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
        let chat = ChatId::new();
        let mut request = format!("ws://{addr}/chats/{chat}/events")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            format!("{WS_HANDSHAKE_SUBPROTOCOL}, {WS_TOKEN_SUBPROTOCOL_PREFIX}not-the-token")
                .parse()
                .unwrap(),
        );
        assert!(connect_async(request).await.is_err());
    }
}
