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
mod blob_orphan_auditor;
mod blob_retirement_worker;
mod bus;
mod document_auditor;
mod document_stage;
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

use axum::extract::DefaultBodyLimit;
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

const MAX_RAW_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;

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
            "/projects/{project_id}/documents/raw",
            post(routes::ingest_raw_project_document)
                .layer(DefaultBodyLimit::max(MAX_RAW_DOCUMENT_BYTES)),
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
            "/documents/raw",
            post(routes::ingest_raw_document).layer(DefaultBodyLimit::max(MAX_RAW_DOCUMENT_BYTES)),
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
    _blob_retirement_worker: AbortTask,
    _blob_orphan_auditor: AbortTask,
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
        state.blobs.clone(),
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
    let blob_retirement_worker = blob_retirement_worker::BlobRetirementWorker::new(
        state.store.clone(),
        state.blobs.clone(),
        state.blob_retirement_wake.clone(),
        state.blob_writes.clone(),
        blob_retirement_worker::BlobRetirementWorkerConfig::default(),
    );
    let blob_orphan_auditor = blob_orphan_auditor::BlobOrphanAuditor::new(
        state.store.clone(),
        state.config.data_dir.join("blobs"),
        state.blob_writes.clone(),
        state.blob_retirement_wake.clone(),
        blob_orphan_auditor::BlobOrphanAuditorConfig::default(),
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
    let blob_retirement_worker = tokio::spawn(blob_retirement_worker.run());
    let blob_orphan_auditor = tokio::spawn(blob_orphan_auditor.run());

    Ok(Server {
        local_addr,
        token,
        listener,
        router,
        _document_auditor: AbortTask(document_auditor),
        _document_worker: AbortTask(document_worker),
        _blob_retirement_worker: AbortTask(blob_retirement_worker),
        _blob_orphan_auditor: AbortTask(blob_orphan_auditor),
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
mod tests;
