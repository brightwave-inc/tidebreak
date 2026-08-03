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

mod approval_judge;
mod approvals;
mod auth;
mod blob_orphan_auditor;
mod blob_retirement_worker;
mod bus;
mod chat_titling;
/// Host-owned code-execution provider selection and policy.
pub mod code_execution;
mod connected_apps;
mod desktop_schema;
mod document_decode;
mod durable_oplog;
mod error;
mod event_projection;
mod exec_write_snapshot;
mod extract;
mod foreground_prompt;
mod gateway_runtime;
mod managed_policy;
mod mcp_config;
mod model_registry;
mod model_roles;
/// OpenAPI ingest into the bounded operation catalog a `rest_api` connected
/// app stores and the governed REST executor validates against.
pub mod openapi_catalog;
mod pairing;
mod principal;
mod provider;
mod providers;
mod resolver;
/// Governed executor performing one declared operation of a `rest_api`
/// connected app: catalog validation before any I/O, pinned bounded egress,
/// request-time credential injection.
pub mod rest_executor;
mod retry;
mod routes;
mod sandbox_admission;
mod sandbox_agent_run_worker;
pub mod sandbox_container_run;
mod sandbox_container_run_worker;
/// A [`SandboxBackend`](openwave_sandbox_protocol::SandboxBackend) over the Docker
/// CLI: container provision, loopback addressing, idempotent teardown, and a
/// correlation-tag orphan sweep.
pub mod sandbox_docker;
mod sandbox_web_search_worker;
mod scoped_model_token;
mod scoped_store;
/// Rewriting stored credentials so the running binary owns their keychain items.
pub mod secret_rehome;
mod source_tools;
mod state;
mod turn_worker;
mod view_frames;
/// Host-owned, inert web-search configuration and provider selection.
pub mod web_search;
mod wire_types;

use std::fs::{OpenOptions, TryLockError};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::http::{header, Method};
use axum::routing::{delete, get, post};
use axum::Router;
use tokio::net::TcpListener;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use uuid::Uuid;

use openwave_code_execution::ExecTool;
#[cfg(test)]
use openwave_core::DbStore;
use openwave_core::{
    ask_user_questions_tool_spec, exit_plan_mode_tool_spec, import_connected_file_tool_spec,
    list_connected_folders_tool_spec, list_folder_tool_spec, read_connected_file_tool_spec,
    request_folder_access_tool_spec, validate_ask_user_questions_arguments,
    validate_exit_plan_mode_arguments, validate_import_connected_file_arguments,
    validate_list_connected_folders_arguments, validate_list_folder_arguments,
    validate_read_connected_file_arguments, validate_request_folder_access_arguments,
    validate_write_output_to_connected_folder_arguments,
    write_output_to_connected_folder_tool_spec, AgentConfig, AgentError, ApprovalClass, BlobStore,
    CachingSecretProvider, Config, CreateAppTool, FsBlobStore, KeychainSecretProvider, ListDir,
    Profile, ReadFile, Result, SecretProvider, Store, Tool, ToolRegistry, WriteFile,
};
use resolver::KeyedResolver;

pub use durable_oplog::DurableOperationStore;
pub use error::ServerError;
pub use pairing::{
    register_pending_pairing, register_replacing_pairing, PairingError, PairingHandle,
    PendingRegistration,
};
pub use state::AppState;

pub(crate) const MAX_RAW_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_WEB_SEARCH_CREDENTIAL_BODY_BYTES: usize = 16 * 1024;
const MAX_CODE_EXECUTION_CONFIG_BODY_BYTES: usize = 1_024;
const MAX_CODE_EXECUTION_CREDENTIAL_BODY_BYTES: usize = 16 * 1024;

/// Build the router: unauthenticated health check plus the token-guarded API.
pub fn app(state: AppState) -> Router {
    // `route_layer` applies the token check to matched API routes only, so an
    // unknown path still answers `404` (not `401`), and `/healthz` stays open.
    let document_api = Router::new()
        .route(
            "/chats/{chat_id}/documents",
            post(routes::ingest_chat_document).get(routes::list_chat_documents),
        )
        .route(
            "/chats/{chat_id}/documents/raw",
            post(routes::ingest_raw_chat_document)
                .layer(DefaultBodyLimit::max(MAX_RAW_DOCUMENT_BYTES)),
        )
        .route(
            "/chats/{chat_id}/documents/raw-stream",
            // `DefaultBodyLimit` only binds extractors that buffer the body,
            // and this handler takes the raw `Body` so it can write straight
            // to the blob store — hence the transport-level limit instead.
            // The cap is the same 16 MiB the buffered routes use: the handler
            // reads the finished blob back to decode it, so streaming saves
            // the upload from being buffered twice, not from being large.
            post(routes::ingest_streamed_raw_chat_document)
                .layer(RequestBodyLimitLayer::new(MAX_RAW_DOCUMENT_BYTES)),
        )
        .route(
            "/chats/{chat_id}/documents/{document_id}",
            delete(routes::delete_chat_document),
        )
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
            "/projects/{project_id}/documents/{document_id}/file-content",
            get(routes::get_project_document_file_content),
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
        .route(
            "/documents/{id}/file-content",
            get(routes::get_document_file_content),
        )
        // Image attachments sit on the same trust boundary as raw document
        // ingest — both take bytes off the user's disk for one conversation —
        // so they follow it into whichever router that boundary lands on.
        .route(
            "/chats/{chat_id}/attachments/images",
            post(routes::publish_chat_image_attachment)
                .layer(DefaultBodyLimit::max(routes::MAX_IMAGE_ATTACHMENT_BYTES)),
        );

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
    // The reader's half of the document surface. `ChatDocumentDetail` omits the
    // catalog's `uri` and index bookkeeping, so unlike the full-fidelity routes
    // below there is nothing here to withhold from an untrusted client — and a
    // renderer-shaped client, this webview or a web one later, holds only the
    // primary bearer and is the thing that draws the document.
    let renderer_document_api = Router::new()
        .route(
            "/chats/{chat_id}/documents/{document_id}",
            get(routes::get_chat_document),
        )
        .route(
            "/chats/{chat_id}/documents/{document_id}/file-content",
            get(routes::get_chat_document_file_content),
        );

    // A native embedding gives the renderer only the primary bearer, so its
    // full-fidelity document surface joins the native-only router. A headless
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
            "/models/roles/{role}",
            axum::routing::put(routes::put_model_role),
        )
        .route(
            "/web-search",
            get(routes::get_web_search_config).put(routes::put_web_search_config),
        )
        .route(
            "/code-execution",
            get(routes::get_code_execution_config)
                .put(routes::put_code_execution_config)
                .layer(DefaultBodyLimit::max(MAX_CODE_EXECUTION_CONFIG_BODY_BYTES)),
        )
        .route(
            "/code-execution/credentials",
            get(routes::get_code_execution_credentials),
        )
        .route(
            "/code-execution/credentials/{provider}",
            axum::routing::put(routes::put_code_execution_credential)
                .delete(routes::delete_code_execution_credential)
                .layer(DefaultBodyLimit::max(
                    MAX_CODE_EXECUTION_CREDENTIAL_BODY_BYTES,
                )),
        )
        .route(
            "/mcp/servers",
            get(routes::get_mcp_servers)
                .put(routes::put_mcp_servers)
                .layer(DefaultBodyLimit::max(mcp_config::MAX_CONFIG_BODY_BYTES)),
        )
        .route(
            "/mcp/servers/{name}/reconnect",
            post(routes::post_mcp_server_reconnect),
        )
        .route(
            "/mcp/servers/{name}/view-session",
            post(routes::post_mcp_view_session),
        )
        .route(
            "/apps/{id}/view-session",
            post(routes::post_app_view_session),
        )
        .route(
            "/chats/{chat_id}/calls/{call_id}/mcp-app-payload",
            get(routes::get_mcp_app_payload),
        )
        .route(
            "/apps/{id}/invoke",
            post(routes::post_app_invoke)
                .layer(DefaultBodyLimit::max(routes::MAX_APP_INVOKE_BODY_BYTES)),
        )
        .route(
            "/apps/{id}/grant",
            get(routes::get_app_grant_state)
                .post(routes::post_app_grant)
                .delete(routes::delete_app_grant),
        )
        .route("/apps", get(routes::get_app_library))
        .route(
            "/apps/{id}",
            get(routes::get_app_detail).delete(routes::delete_app),
        )
        .route("/policy", get(routes::get_policy))
        .route("/gateway/status", get(routes::get_gateway_status))
        .route("/gateway/apps", get(routes::get_gateway_apps))
        .route("/gateway/sign-in", post(routes::post_gateway_sign_in))
        .route("/gateway/sign-out", post(routes::post_gateway_sign_out))
        .route(
            "/gateway/pairing/dismiss",
            post(routes::post_gateway_pairing_dismiss),
        )
        .route(
            "/gateway/models/sync",
            post(routes::post_gateway_models_sync),
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
        .merge(renderer_document_api)
        // The transcript must fetch pixels with its bearer rather than putting
        // a token in an image URL. Unlike image publication, this is renderer
        // presentation of an image already durably attached to the chat.
        .route(
            "/chats/{chat_id}/attachments/images/{attachment_id}",
            get(routes::get_chat_image_attachment),
        )
        .route("/chats", post(routes::create_chat).get(routes::list_chats))
        .route(
            "/chats/pending-prompts",
            get(routes::list_pending_chat_prompts),
        )
        .route(
            "/chats/{id}",
            get(routes::get_chat)
                .patch(routes::patch_chat)
                .delete(routes::delete_chat),
        )
        .route("/chats/{id}/messages", get(routes::list_chat_messages))
        .route("/chats/{id}/agent-runs", get(routes::list_agent_runs))
        .route(
            "/chats/{chat_id}/agent-runs/{run_id}/activity",
            get(routes::list_agent_run_activity),
        )
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
            "/chats/{chat_id}/turns/{turn_id}/file-changes/undo",
            post(routes::post_undo_turn_file_changes),
        )
        .route(
            "/chats/{chat_id}/turns/{turn_id}/file-changes/{snapshot_id}/undo",
            post(routes::post_undo_one_file_change),
        )
        .route(
            "/chats/{chat_id}/turns/{turn_id}/file-changes/{snapshot_id}/preview/{revision}",
            get(routes::get_file_change_preview),
        )
        .route(
            "/chats/{id}/client-executions/pending",
            get(routes::list_pending_folder_access_requests),
        )
        .route(
            "/chats/{id}/output-writebacks/pending",
            get(routes::list_pending_output_writebacks),
        )
        .route(
            "/chats/{id}/questions/pending",
            get(routes::list_pending_user_questions),
        )
        .route(
            "/chats/{id}/questions/{call_id}/answer",
            post(routes::answer_user_questions).layer(DefaultBodyLimit::max(
                routes::MAX_USER_QUESTION_ANSWER_BODY_BYTES,
            )),
        )
        .route(
            "/chats/{id}/plans/pending",
            get(routes::list_pending_plan_approvals),
        )
        .route(
            "/chats/{id}/plans/{call_id}/decision",
            post(routes::decide_plan)
                .layer(DefaultBodyLimit::max(routes::MAX_PLAN_DECISION_BODY_BYTES)),
        )
        .route("/chats/{id}/approvals", get(routes::list_pending_approvals))
        .route(
            "/chats/{id}/approvals/{call_id}",
            post(routes::post_approval),
        )
        .route("/grants", get(routes::list_standing_grants))
        .route(
            "/grants/{call_id}",
            axum::routing::delete(routes::delete_standing_grant),
        )
        .route("/chats/{id}/events", get(routes::chat_events))
        .merge(client_executor_api)
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ))
        .with_state(state.clone());
    let frame_state = state;

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
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::ACCEPT,
            header::IF_RANGE,
            header::RANGE,
        ])
        .expose_headers([
            header::ACCEPT_RANGES,
            header::CONTENT_LENGTH,
            header::CONTENT_RANGE,
        ]);

    // Reached by capability (single-use token), not by bearer: iframes send
    // no headers. See `routes::get_mcp_view_frame` and
    // `routes::get_app_view_frame`.
    let view_frames = Router::new()
        .route("/mcp/view-frames/{token}", get(routes::get_mcp_view_frame))
        .route("/apps/view-frames/{token}", get(routes::get_app_view_frame))
        .with_state(frame_state);

    Router::new()
        .route("/healthz", get(healthz))
        .merge(view_frames)
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
    /// The live exec staging registry, handed to native embedders so the host
    /// folder tools answer from the same per-turn copy exec writes into.
    code_execution: Arc<code_execution::ConfiguredCodeExecutionProvider>,
    /// The live MCP runtime, handed to pairing so a profile that becomes
    /// managed mid-session takes its manual servers down immediately.
    mcp: Arc<mcp_config::McpRuntime>,
    /// The one gateway runtime, handed to pairing so a registered pending
    /// pairing lands in the same slot the sign-in surface reads.
    gateway: Arc<gateway_runtime::GatewayRuntime>,
    listener: TcpListener,
    router: Router,
    _turn_worker: AbortTask,
    _sandbox_agent_run_worker: AbortTask,
    _sandbox_container_run_worker: Option<AbortTask>,
    _sandbox_web_search_worker: AbortTask,
    _blob_retirement_worker: AbortTask,
    _blob_orphan_auditor: AbortTask,
    _approval_judge_worker: AbortTask,
    _mcp_supervisor: AbortTask,
    _gateway_model_sync: AbortTask,
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

    /// Where this server stages a turn's exec writes for granted folders.
    ///
    /// Native embedders execute the host folder tools themselves, and those
    /// tools must not show the model the pre-turn folder while exec is working
    /// in a staged copy of it. See [`code_execution::StagedFolders`].
    pub fn staged_folders(&self) -> Arc<dyn code_execution::StagedFolders> {
        self.code_execution.clone()
    }

    /// The handles the native deep-link pairing flow needs.
    ///
    /// Pairing is exported for native embedders only, and it has live effects
    /// beyond the store — see [`register_pending_pairing`].
    pub fn pairing_handle(&self) -> PairingHandle {
        PairingHandle::new(self.store.clone(), self.mcp.clone(), self.gateway.clone())
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
const DEFAULT_MODEL: &str = "claude-opus-5";

/// Wire the store from `config` and bind the API to an ephemeral loopback port.
///
/// This generic embedding does not expose durable root-attachment mutations,
/// because it has no restart-stable native executor identity.
pub async fn bind(config: Config) -> Result<Server> {
    bind_inner(
        config,
        None,
        mcp_config::ConfiguredMcpServers::default(),
        None,
    )
    .await
}

/// Bind the API and mount external MCP servers from `OPENWAVE_MCP_CONFIG`.
///
/// This is the product boot path used by the CLI. Custom embedders can continue
/// to use [`bind`] when process-environment configuration is undesirable.
pub async fn bind_configured(config: Config) -> Result<Server> {
    let mcp_servers = mcp_config::ConfiguredMcpServers::from_env()?;
    bind_inner(config, None, mcp_servers, None).await
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
    bind_inner(
        config,
        Some(client_executor_id),
        mcp_config::ConfiguredMcpServers::default(),
        None,
    )
    .await
}

/// Desktop counterpart to [`bind_configured`], retaining the stable native
/// executor identity used by host-owned continuations.
pub async fn bind_configured_with_desktop_executor(
    config: Config,
    client_executor_id: Uuid,
) -> Result<Server> {
    if client_executor_id.is_nil() {
        return Err(AgentError::config("client executor id must not be nil"));
    }
    let mcp_servers = mcp_config::ConfiguredMcpServers::from_env()?;
    bind_inner(config, Some(client_executor_id), mcp_servers, None).await
}

/// Desktop binding with the native bridge that resolves current connected
/// folders into per-invocation local sandbox grants.
pub async fn bind_configured_with_desktop_executor_and_folder_grants(
    config: Config,
    client_executor_id: Uuid,
    folder_grant_resolver: Arc<dyn code_execution::ExecFolderGrantResolver>,
) -> Result<Server> {
    if client_executor_id.is_nil() {
        return Err(AgentError::config("client executor id must not be nil"));
    }
    let mcp_servers = mcp_config::ConfiguredMcpServers::from_env()?;
    bind_inner(
        config,
        Some(client_executor_id),
        mcp_servers,
        Some(folder_grant_resolver),
    )
    .await
}

/// The secret store the configured profile keeps its credentials in.
///
/// Wrapped in a [`CachingSecretProvider`] so a key costs one keychain read per
/// process rather than one per turn: [`resolver::ConfiguredResolver`] rebuilds
/// its route set on every turn, and each candidate route reads its provider's
/// credential to decide whether it exists.
fn secret_provider(config: &Config) -> Arc<dyn SecretProvider> {
    let keychain: Arc<dyn SecretProvider> = Arc::new(match &config.keychain_service {
        Some(service) => KeychainSecretProvider::with_service(service),
        None => KeychainSecretProvider::new(),
    });
    Arc::new(CachingSecretProvider::new(keychain))
}

/// Re-home the configured profile's credentials — see [`secret_rehome`]. Does
/// not open the data directory, so it runs without the daemon's instance lock.
pub async fn rehome_configured_secrets(
    config: &Config,
) -> Vec<(String, secret_rehome::RehomeOutcome)> {
    secret_rehome::rehome_secrets(&*secret_provider(config)).await
}

async fn bind_inner(
    config: Config,
    client_executor_id: Option<Uuid>,
    mcp_servers: mcp_config::ConfiguredMcpServers,
    folder_grant_resolver: Option<Arc<dyn code_execution::ExecFolderGrantResolver>>,
) -> Result<Server> {
    // Desktop live delivery remains process-local. Turns, steering, and tool
    // approvals are durable, while one process still owns the complete data
    // directory and its worker set.
    let instance_lock = InstanceLock::acquire(&config)?;
    let sandbox_container_admission = sandbox_admission::resolve(&config);
    let sandbox_spawn_execution_location = sandbox_container_admission.execution_location;
    let store = connect_store(&config).await?;
    let secrets = secret_provider(&config);
    // The product boot path is where this platform's OS-managed (MDM) policy
    // reader gets selected; directly assembled AppState stays hermetic. This
    // is the one instance shared by the boot policy read, the legacy-key
    // migration guard, the resolver, the gateway runtime, and the request
    // handlers, so they can never disagree on the resolved policy.
    let os_policy: Arc<dyn managed_policy::OsPolicySource> =
        managed_policy::platform_source(&config);
    // The legacy Anthropic auto-enable is gated on one policy read. A resolution
    // `Err` is
    // deliberately swallowed as "not allowed": an unreadable policy fails
    // closed to no BYOK arming while boot still proceeds, so the profile can
    // surface the error and be repaired instead of bricking.
    let boot_policy = managed_policy::resolve(&*store, &*os_policy).await;
    let byok_boot_allowed = matches!(&boot_policy, Ok(policy) if !policy.managed);
    // Pre-providers installs may only have an env/legacy key — enable Anthropic
    // so `KeyedResolver`'s enabled check doesn't fail-closed on upgrade. Never
    // on a managed profile: auto-enabling a BYOK provider would fight the
    // lockdown.
    if byok_boot_allowed {
        providers::migrate_legacy_anthropic(&*store, &*secrets).await?;
    }
    // The additive gateway configuration is retired: carry a managed row's
    // model snapshot forward once, name the remedy for a legacy unmanaged
    // one, and revoke any stored session the resolved policy no longer
    // stands behind — one an unmanaged profile can no longer reach, or one
    // an MDM re-point orphaned at a superseded deployment. Skipped when
    // policy is unreadable — fail closed, and the legacy state stays
    // untouched for when the policy is repaired.
    if let Ok(policy) = &boot_policy {
        providers::retire_legacy_gateway_row(&*store, policy).await?;
        gateway_runtime::retire_superseded_gateway_session(secrets.clone(), policy).await?;
    }
    let gateway =
        gateway_runtime::GatewayRuntime::new(store.clone(), secrets.clone(), os_policy.clone());
    let resolver = Arc::new(KeyedResolver::new(
        store.clone(),
        secrets.clone(),
        gateway.clone(),
        os_policy.clone(),
    ));
    let blobs: Arc<dyn BlobStore> = Arc::new(FsBlobStore::new(config.data_dir.join("blobs")));
    // The same lock root `AppState` uses. `BlobWriteGuard` rendezvouses through
    // permanent lock files, so a second handle over the directory excludes
    // against the first rather than shadowing it.
    let exec_blob_writes = Arc::new(state::BlobWriteGuard::new(
        config.data_dir.join("blob-locks"),
    ));
    let code_execution = Arc::new(
        code_execution::ConfiguredCodeExecutionProvider::new(
            store.clone(),
            secrets.clone(),
            config.data_dir.join("scratch"),
        )
        .with_blobs(blobs.clone())
        .with_blob_write_locks(exec_blob_writes)
        .with_document_scripts(config.exec_scripts_dir.clone())
        .with_skills(config.exec_skills_dir.clone())
        .with_folder_grant_resolver(folder_grant_resolver),
    );
    let foreground_web_search =
        Box::new(web_search::foreground_tool(store.clone(), secrets.clone()));
    let web_extract = Box::new(web_search::foreground_extract_tool(
        store.clone(),
        secrets.clone(),
    ));
    let (tools, agent_config) = agent_deps(
        code_execution.clone(),
        foreground_web_search,
        web_extract,
        store.clone(),
        config.data_dir.clone(),
    );
    let tools = Arc::new(tools);
    let mut state = match client_executor_id {
        Some(client_executor_id) => AppState::new_with_client_executor_id(
            config,
            store,
            resolver,
            secrets,
            tools,
            agent_config,
            client_executor_id,
        )?,
        None => AppState::new(config, store, resolver, secrets, tools, agent_config),
    };
    state.blobs = blobs;
    // The resolver and the /gateway routes must share ONE runtime: refresh
    // rotation is serialized per GatewayConnection instance, and two
    // instances over the same keychain entry can race a stale refresh token
    // into the gateway's reuse detection (a spurious full sign-out).
    state.gateway = gateway;
    state.os_policy = os_policy;
    state.mcp.initialize(mcp_servers).await?;
    let token = state.token.clone();
    let client_executor_token = state.client_executor_token.clone();
    let blob_retirement_worker = blob_retirement_worker::BlobRetirementWorker::new(
        state.store.clone(),
        state.blobs.clone(),
        state.blob_retirement_wake.clone(),
        state.blob_writes.clone(),
        blob_retirement_worker::BlobRetirementWorkerConfig::default(),
    );
    let approval_judge_worker = approval_judge::ApprovalJudgeWorker::new(
        state.store.clone(),
        state.resolver.clone(),
        state.secrets.clone(),
        state.os_policy.clone(),
        state.approvals.clone(),
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
        state.secrets.clone(),
        state.os_policy.clone(),
        state.tools.clone(),
        state.approvals.clone(),
        state.events.clone(),
        state.active_turns.clone(),
        state.turn_job_wake.clone(),
        state.agent_run_wake.clone(),
        state.agent_config.clone(),
        Some(state.config.data_dir.join("scratch")),
        turn_worker::TurnWorkerConfig {
            sandbox_spawn_execution_location,
            ..turn_worker::TurnWorkerConfig::default()
        },
    )
    .with_blobs(state.blobs.clone())
    .with_mcp_runtime(state.mcp.clone())
    .with_exec_folder_context(code_execution.clone());
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
    let sandbox_container_run_worker = {
        let enabled = sandbox_container_admission.enabled();
        sandbox_container_run_worker::SandboxContainerRunWorker::new(
            state.store.clone(),
            sandbox_container_admission.backend,
            state.resolver.clone(),
            state.agent_run_wake.clone(),
            enabled,
            sandbox_container_run::SandboxContainerRunConfig::default(),
            sandbox_container_run_worker::SandboxContainerRunWorkerConfig::default(),
        )
    };
    let server_store = state.store.clone();
    let mcp_runtime = state.mcp.clone();
    let gateway_runtime = state.gateway.clone();
    let router = app(state);

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|e| AgentError::config(format!("failed to bind loopback: {e}")))?;
    let local_addr = listener
        .local_addr()
        .map_err(|e| AgentError::config(format!("no local address: {e}")))?;

    let turn_worker = tokio::spawn(turn_worker.run());
    let sandbox_agent_run_worker = tokio::spawn(sandbox_agent_run_worker.run());
    let sandbox_container_run_worker =
        sandbox_container_run_worker.map(|worker| tokio::spawn(worker.run()));
    let sandbox_web_search_worker = tokio::spawn(sandbox_web_search_worker.run());
    let blob_retirement_worker = tokio::spawn(blob_retirement_worker.run());
    let blob_orphan_auditor = tokio::spawn(blob_orphan_auditor.run());
    let approval_judge_worker = tokio::spawn(approval_judge_worker.run());
    let mcp_supervisor = tokio::spawn(mcp_runtime.clone().supervise());
    let gateway_model_sync = tokio::spawn(gateway_runtime.clone().sync_models_periodically());

    Ok(Server {
        local_addr,
        token,
        client_executor_token,
        store: server_store,
        code_execution,
        mcp: mcp_runtime,
        gateway: gateway_runtime,
        listener,
        router,
        _turn_worker: AbortTask(turn_worker),
        _sandbox_agent_run_worker: AbortTask(sandbox_agent_run_worker),
        _sandbox_container_run_worker: sandbox_container_run_worker.map(AbortTask),
        _sandbox_web_search_worker: AbortTask(sandbox_web_search_worker),
        _blob_retirement_worker: AbortTask(blob_retirement_worker),
        _blob_orphan_auditor: AbortTask(blob_orphan_auditor),
        _approval_judge_worker: AbortTask(approval_judge_worker),
        _mcp_supervisor: AbortTask(mcp_supervisor),
        _gateway_model_sync: AbortTask(gateway_model_sync),
        _instance_lock: instance_lock,
    })
}

/// Assemble the tools and per-turn tuning for a real launch.
///
/// The model **provider** is not built here — it is resolved per turn by the
/// [`KeyedResolver`] (a composite router over enabled providers; see
/// [`resolver`]), so configuring a provider at runtime takes effect without a
/// restart. The model *name* comes from `OPENWAVE_MODEL` (or the built-in
/// default) and can be overridden at runtime via `PUT /settings` or per-chat.
fn agent_deps(
    code_execution: Arc<dyn openwave_code_execution::CodeExecutionProvider>,
    web_search: Box<dyn Tool>,
    web_extract: Box<dyn Tool>,
    source_store: Arc<dyn Store>,
    profile_data_dir: std::path::PathBuf,
) -> (ToolRegistry, AgentConfig) {
    let mut tools = ToolRegistry::new()
        .with(Box::new(ReadFile))
        .with(Box::new(ListDir))
        .with(Box::new(WriteFile))
        .with(Box::new(ExecTool::new(code_execution)))
        .with(Box::new(source_tools::ListSourcesTool::new(
            source_store.clone(),
        )))
        .with(Box::new(source_tools::ReadSourceTool::new(
            source_store.clone(),
        )))
        .with(Box::new(source_tools::ReadToolResultTool::new(
            source_store.clone(),
        )))
        .with(Box::new(CreateAppTool::new(source_store, profile_data_dir)))
        .with(web_search)
        .with(web_extract);
    tools.register_validated_client(
        request_folder_access_tool_spec(),
        ApprovalClass::ReadOnly,
        validate_request_folder_access_arguments,
    );
    tools.register_validated_client(
        list_connected_folders_tool_spec(),
        ApprovalClass::ReadOnly,
        validate_list_connected_folders_arguments,
    );
    tools.register_validated_client(
        list_folder_tool_spec(),
        ApprovalClass::ReadOnly,
        validate_list_folder_arguments,
    );
    tools.register_validated_client(
        read_connected_file_tool_spec(),
        ApprovalClass::ReadOnly,
        validate_read_connected_file_arguments,
    );
    // Importing copies a connected file into the chat's sources: durable chat
    // state, so it counts as a workspace mutation even though the connected
    // folder itself is only read.
    tools.register_validated_client(
        import_connected_file_tool_spec(),
        ApprovalClass::Workspace,
        validate_import_connected_file_arguments,
    );
    // The spec advertises the model-facing filename shape; the agent resolves
    // that filename into the canonical id-bearing arguments before the call is
    // checkpointed, so the validator checks the canonical durable form.
    tools.register_validated_client(
        write_output_to_connected_folder_tool_spec(),
        ApprovalClass::Workspace,
        validate_write_output_to_connected_folder_arguments,
    );
    tools.register_validated_foreground_client(
        ask_user_questions_tool_spec(),
        ApprovalClass::ReadOnly,
        validate_ask_user_questions_arguments,
    );
    tools.register_validated_foreground_client(
        exit_plan_mode_tool_spec(),
        ApprovalClass::ReadOnly,
        validate_exit_plan_mode_arguments,
    );
    // Foreground spawn checkpoints child acceptance and immediately resumes;
    // an explicit ordered wait parks only when results are needed. The bounded
    // sandbox worker below never receives either orchestration definition.
    tools.register_foreground_agent_orchestration();
    let model = std::env::var("OPENWAVE_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    let agent_config = AgentConfig {
        model,
        ..AgentConfig::default()
    };
    (tools, agent_config)
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
