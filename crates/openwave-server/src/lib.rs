//! OpenWave's in-process HTTP/WebSocket surface.
//!
//! Every client — the desktop webview, the CLI — drives the agent through this
//! one local API rather than linking the loop directly, so all surfaces share a
//! single wiring of `Config`, `Store`, and (next slice) the agent. The server
//! binds to an ephemeral **loopback** port and mints a per-launch **bearer
//! token**. Trusted native operations require a second per-launch credential
//! that renderer-facing clients are never given.
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
mod desktop_schema;
mod document_auditor;
mod document_stage;
mod document_worker;
mod error;
mod event_projection;
mod extract;
mod provider;
mod providers;
mod resolver;
mod routes;
mod sandbox_agent_run_worker;
mod sandbox_web_search_worker;
mod state;
mod turn_worker;
/// Host-owned, inert web-search configuration and provider selection.
pub mod web_search;

use std::fs::{OpenOptions, TryLockError};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::http::{header, Method};
use axum::routing::{get, post};
use axum::Router;
use tokio::net::TcpListener;
use tower_http::cors::{AllowOrigin, CorsLayer};
use uuid::Uuid;

#[cfg(test)]
use openwave_core::DbStore;
use openwave_core::{
    list_connected_folders_tool_spec, list_folder_tool_spec, read_connected_file_tool_spec,
    request_folder_access_tool_spec, validate_list_connected_folders_arguments,
    validate_list_folder_arguments, validate_read_connected_file_arguments,
    validate_request_folder_access_arguments, AgentConfig, AgentError, Config,
    KeychainSecretProvider, ListDir, Profile, ReadFile, Result, SecretProvider, Store,
    ToolRegistry, WriteFile,
};
use openwave_retrieval::{
    Embedder, HashEmbedder, LanceVectorStore, OpenAiEmbedder, ParserRegistry, PlainTextParser,
    Retriever, SearchTool, TextChunker, VectorStore,
};

use resolver::KeyedResolver;

pub use error::ServerError;
pub use state::AppState;

const MAX_RAW_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_WEB_SEARCH_CREDENTIAL_BODY_BYTES: usize = 16 * 1024;

/// Build the router: unauthenticated health check plus the token-guarded API.
pub fn app(state: AppState) -> Router {
    // `route_layer` applies the token check to matched API routes only, so an
    // unknown path still answers `404` (not `401`), and `/healthz` stays open.
    let document_api = Router::new()
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
        .route("/search", post(routes::search_documents));

    let client_executor_api = Router::new()
        .route(
            "/sandbox-file-reads/pending",
            get(routes::list_pending_delegated_file_reads),
        )
        .route(
            "/sandbox-file-reads/{call_id}/claim",
            post(routes::claim_delegated_file_read),
        )
        .route(
            "/sandbox-file-reads/{call_id}/heartbeat",
            post(routes::heartbeat_delegated_file_read),
        )
        .route(
            "/sandbox-file-reads/{call_id}/resolve",
            post(routes::resolve_delegated_file_read),
        )
        .route(
            "/chats/{id}/client-executions/pending/raw",
            get(routes::list_pending_client_executions_raw),
        )
        .route(
            "/chats/{id}/client-executions/{call_id}/claim",
            post(routes::claim_client_execution),
        )
        .route(
            "/chats/{id}/client-executions/{call_id}/heartbeat",
            post(routes::heartbeat_client_execution),
        )
        .route(
            "/chats/{id}/client-executions/{call_id}/resolve",
            post(routes::resolve_client_execution),
        );
    // A native embedding gives the renderer only the primary bearer, so its
    // canonical document surface joins the native-only router. A headless
    // embedding has no separate renderer trust boundary and deliberately keeps
    // the same API on its primary bearer for CLI/API compatibility.
    let (client_executor_api, public_document_api) = if state.root_attachment_routes_enabled {
        let client_executor_api = client_executor_api
            .route(
                "/chats/{chat_id}/root-attachment-changes/{change_id}/begin",
                post(routes::begin_root_attachment_change),
            )
            .route(
                "/root-attachment-changes/pending",
                get(routes::list_pending_root_attachment_changes),
            )
            .route(
                "/root-attachment-changes/{change_id}/finish",
                post(routes::finish_root_attachment_change),
            )
            .merge(document_api);
        (client_executor_api, Router::new())
    } else {
        (client_executor_api, document_api)
    };
    let client_executor_api = client_executor_api.route_layer(
        axum::middleware::from_fn_with_state(state.clone(), auth::require_client_executor_token),
    );

    let api = Router::new()
        .route(
            "/settings",
            get(routes::get_settings).put(routes::put_settings),
        )
        .route(
            "/projects",
            post(routes::create_project)
                .get(routes::list_projects)
                .layer(DefaultBodyLimit::max(
                    routes::MAX_PROJECT_METADATA_BODY_BYTES,
                )),
        )
        .route(
            "/projects/{id}",
            get(routes::get_project)
                .patch(routes::patch_project)
                .delete(routes::delete_project)
                .layer(DefaultBodyLimit::max(
                    routes::MAX_PROJECT_METADATA_BODY_BYTES,
                )),
        )
        .route("/models", get(routes::list_models))
        .route(
            "/web-search",
            get(routes::get_web_search_config).put(routes::put_web_search_config),
        )
        .route(
            "/web-search/credentials",
            get(routes::get_web_search_credentials),
        )
        .route(
            "/web-search/credentials/{provider}",
            axum::routing::put(routes::put_web_search_credential)
                .delete(routes::delete_web_search_credential)
                .layer(DefaultBodyLimit::max(MAX_WEB_SEARCH_CREDENTIAL_BODY_BYTES)),
        )
        .route("/providers", get(routes::list_providers))
        .route(
            "/providers/{kind}",
            axum::routing::put(routes::put_provider),
        )
        .route(
            "/providers/{kind}/credential",
            axum::routing::delete(routes::delete_provider_credential),
        )
        .merge(public_document_api)
        .route("/chats", post(routes::create_chat).get(routes::list_chats))
        .route(
            "/chats/{id}",
            get(routes::get_chat)
                .patch(routes::patch_chat)
                .delete(routes::delete_chat),
        )
        .route("/chats/{id}/messages", get(routes::list_chat_messages))
        .route("/chats/{id}/agent-runs", get(routes::list_agent_runs))
        .route(
            "/chats/{chat_id}/agent-runs/{run_id}/cancel",
            post(routes::post_agent_run_cancel),
        )
        .route(
            "/settings/api-key",
            axum::routing::put(routes::put_api_key).delete(routes::delete_api_key),
        )
        .route("/chats/{id}/messages", post(routes::post_message))
        .route("/chats/{id}/cancel", post(routes::post_cancel))
        .route("/chats/{id}/steer", post(routes::post_steer))
        .route(
            "/chats/{id}/client-executions/pending",
            get(routes::list_pending_folder_access_requests),
        )
        .route("/chats/{id}/approvals", get(routes::list_pending_approvals))
        .route(
            "/chats/{id}/approvals/{call_id}",
            post(routes::post_approval),
        )
        .route("/chats/{id}/events", get(routes::chat_events))
        .merge(client_executor_api)
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
    client_executor_token: Arc<str>,
    store: Arc<dyn Store>,
    listener: TcpListener,
    router: Router,
    _document_auditor: AbortTask,
    _document_worker: AbortTask,
    _turn_worker: AbortTask,
    _sandbox_agent_run_worker: AbortTask,
    _sandbox_web_search_worker: AbortTask,
    _blob_retirement_worker: AbortTask,
    _blob_orphan_auditor: AbortTask,
    _instance_lock: InstanceLock,
}

struct InstanceLock {
    _file: std::fs::File,
}

impl InstanceLock {
    fn acquire(config: &Config) -> Result<Self> {
        std::fs::create_dir_all(&config.data_dir)
            .map_err(|error| AgentError::config(format!("failed to create data dir: {error}")))?;
        let path = config.data_dir.join("openwave.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| {
                AgentError::config(format!("failed to open {}: {error}", path.display()))
            })?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(AgentError::config(format!(
                "another OpenWave server already owns {}",
                config.data_dir.display()
            ))),
            Err(TryLockError::Error(error)) => Err(AgentError::config(format!(
                "failed to lock {}: {error}",
                path.display()
            ))),
        }
    }
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

    /// The second per-launch credential for trusted native-only operations.
    pub fn client_executor_token(&self) -> &str {
        &self.client_executor_token
    }

    /// The authoritative durable store used by this server instance.
    ///
    /// Native embedders use this to resolve renderer-supplied entity IDs back
    /// to server-owned records before granting host capabilities.
    pub fn store(&self) -> Arc<dyn Store> {
        self.store.clone()
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
///
/// This generic embedding does not expose durable root-attachment mutations,
/// because it has no restart-stable native executor identity.
pub async fn bind(config: Config) -> Result<Server> {
    bind_inner(config, None).await
}

/// Bind the API with a stable app-private native executor identity.
///
/// The desktop persists this identity outside renderer-visible state so pending
/// attachment work remains recoverable across launches.
pub async fn bind_with_desktop_executor(
    config: Config,
    client_executor_id: Uuid,
) -> Result<Server> {
    if client_executor_id.is_nil() {
        return Err(AgentError::config("client executor id must not be nil"));
    }
    bind_inner(config, Some(client_executor_id)).await
}

async fn bind_inner(config: Config, client_executor_id: Option<Uuid>) -> Result<Server> {
    // Desktop live delivery remains process-local. Turns, steering, and tool
    // approvals are durable, while one process still owns the complete data
    // directory and its worker set.
    let instance_lock = InstanceLock::acquire(&config)?;
    let store = connect_store(&config).await?;
    let secrets: Arc<dyn SecretProvider> = Arc::new(KeychainSecretProvider::new());
    // Pre-providers installs may only have an env/legacy key — enable Anthropic
    // so `KeyedResolver`'s enabled check doesn't fail-closed on upgrade.
    providers::migrate_legacy_anthropic(&*store, &*secrets).await?;
    let resolver = Arc::new(KeyedResolver::new(store.clone(), secrets.clone()));
    let embedder = resolve_embedder(&*store, &*secrets).await;
    let vector_store = connect_vector_store(&config, embedder.dimensions()).await?;
    let (retrieval, tools, agent_config) = agent_deps(embedder, vector_store);
    let state = match client_executor_id {
        Some(client_executor_id) => AppState::new_with_client_executor_id(
            config,
            store,
            resolver,
            secrets,
            tools,
            retrieval,
            agent_config,
            client_executor_id,
        )?,
        None => AppState::new(
            config,
            store,
            resolver,
            secrets,
            tools,
            retrieval,
            agent_config,
        ),
    };
    let token = state.token.clone();
    let client_executor_token = state.client_executor_token.clone();
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
    let turn_worker = turn_worker::TurnWorker::new(
        state.store.clone(),
        state.resolver.clone(),
        state.tools.clone(),
        state.approvals.clone(),
        state.events.clone(),
        state.active_turns.clone(),
        state.turn_job_wake.clone(),
        state.agent_run_wake.clone(),
        state.agent_config.clone(),
        Some(state.config.data_dir.join("scratch")),
        turn_worker::TurnWorkerConfig::default(),
    );
    let sandbox_worker_config = sandbox_agent_run_worker::SandboxAgentRunWorkerConfig::default()
        .with_delegated_file_executor(client_executor_id.is_some());
    let sandbox_agent_run_worker = sandbox_agent_run_worker::SandboxAgentRunWorker::with_attempts(
        state.store.clone(),
        state.resolver.clone(),
        state.agent_run_wake.clone(),
        state.turn_job_wake.clone(),
        state.events.clone(),
        state.sandbox_attempts.clone(),
        state.agent_config.clone(),
        Some(state.config.data_dir.join("scratch")),
        sandbox_worker_config,
    );
    let sandbox_web_search_worker =
        sandbox_web_search_worker::SandboxWebSearchWorker::with_attempts(
            state.store.clone(),
            state.secrets.clone(),
            state.agent_run_wake.clone(),
            state.sandbox_attempts.clone(),
            sandbox_web_search_worker::SandboxWebSearchWorkerConfig::default(),
        );
    let server_store = state.store.clone();
    let router = app(state);

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|e| AgentError::config(format!("failed to bind loopback: {e}")))?;
    let local_addr = listener
        .local_addr()
        .map_err(|e| AgentError::config(format!("no local address: {e}")))?;

    let document_auditor = tokio::spawn(document_auditor.run());
    let document_worker = tokio::spawn(document_worker.run());
    let turn_worker = tokio::spawn(turn_worker.run());
    let sandbox_agent_run_worker = tokio::spawn(sandbox_agent_run_worker.run());
    let sandbox_web_search_worker = tokio::spawn(sandbox_web_search_worker.run());
    let blob_retirement_worker = tokio::spawn(blob_retirement_worker.run());
    let blob_orphan_auditor = tokio::spawn(blob_orphan_auditor.run());

    Ok(Server {
        local_addr,
        token,
        client_executor_token,
        store: server_store,
        listener,
        router,
        _document_auditor: AbortTask(document_auditor),
        _document_worker: AbortTask(document_worker),
        _turn_worker: AbortTask(turn_worker),
        _sandbox_agent_run_worker: AbortTask(sandbox_agent_run_worker),
        _sandbox_web_search_worker: AbortTask(sandbox_web_search_worker),
        _blob_retirement_worker: AbortTask(blob_retirement_worker),
        _blob_orphan_auditor: AbortTask(blob_orphan_auditor),
        _instance_lock: instance_lock,
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
    let mut tools = ToolRegistry::new()
        .with(Box::new(ReadFile))
        .with(Box::new(ListDir))
        .with(Box::new(WriteFile))
        .with(search);
    tools.register_validated_client(
        request_folder_access_tool_spec(),
        validate_request_folder_access_arguments,
    );
    tools.register_validated_client(
        list_connected_folders_tool_spec(),
        validate_list_connected_folders_arguments,
    );
    tools.register_validated_client(list_folder_tool_spec(), validate_list_folder_arguments);
    tools.register_validated_client(
        read_connected_file_tool_spec(),
        validate_read_connected_file_arguments,
    );
    // Foreground spawn checkpoints child acceptance and immediately resumes;
    // an explicit ordered wait parks only when results are needed. The bounded
    // sandbox worker below never receives either orchestration definition.
    tools.register_foreground_agent_orchestration();
    let tools = Arc::new(tools);
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
            let store = desktop_schema::connect(config).await?;
            Ok(Arc::new(store))
        }
        _ => Err(AgentError::config(
            "only the desktop profile is supported for now",
        )),
    }
}

#[cfg(test)]
mod tests;
