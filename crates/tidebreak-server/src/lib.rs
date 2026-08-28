//! Tidebreak's in-process HTTP/WebSocket surface.
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
//!
//! The loopback bind is the default, not the only option: a self-host
//! deployment that must be reachable from outside its machine sets
//! `TIDEBREAK_LISTEN_ADDR`. The desktop profile refuses one — see
//! [`Config::bind_addr`].

mod agent_control_tools;
mod agent_run_scratch_reaper;
mod approval_judge;
mod approvals;
mod auth;
mod blob_orphan_auditor;
mod blob_retirement_worker;
mod bus;
mod chat_titling;
mod chatgpt_runtime;
mod code;
/// Host-owned code-execution provider selection and policy.
pub mod code_execution;
mod connected_apps;
pub mod connectors;
/// The unified consent read model: standing tool grants and host-broker
/// capability grants as one renderer-facing statement shape.
pub mod consent;
mod desktop_schema;
mod diagnostics;
mod document_decode;
mod durable_oplog;
mod error;
mod event_projection;
mod exec_write_snapshot;
mod extract;
mod foreground_prompt;
mod gateway_drafts;
mod gateway_runtime;
pub mod host_folders;
/// Per-launch `{data_dir}/listen.json` so a CLI can attach without argv tokens.
pub mod listen_endpoint;
pub mod logging;
mod managed_policy;
mod mcp_config;
mod mcp_curated;
/// Trusted decision about what imported bytes actually are, made from the
/// bytes rather than from whoever named them.
pub mod media_type;
mod model_registry;
mod model_roles;
mod obo_gateway;
/// OpenAPI ingest into the bounded operation catalog a `rest_api` connected
/// app stores and the governed REST executor validates against.
pub mod openapi_catalog;
/// Reading a conversation output's immutable revision bytes out of private
/// scratch — shared by the HTTP routes and the desktop's native save dialog.
pub mod output_files;
mod pairing;
mod plugin_install;
mod plugin_mcp;
mod plugin_state;
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
/// A [`SandboxBackend`](tidebreak_sandbox_protocol::SandboxBackend) over the Docker
/// CLI: container provision, loopback addressing, idempotent teardown, and a
/// correlation-tag orphan sweep.
pub mod sandbox_docker;
mod sandbox_exec_worker;
mod sandbox_task_plan_worker;
mod sandbox_web_search_worker;
mod scoped_model_token;
mod scoped_store;
#[cfg(any(test, feature = "scripted-harness"))]
mod scripted_harness;
#[cfg(feature = "scripted-provider")]
mod scripted_provider;
/// Rewriting stored credentials so the running binary owns their keychain items.
pub mod secret_rehome;
mod source_tools;
mod state;
mod task_plan_tool;
mod turn_worker;
mod view_frames;
pub mod voice_transcription;
/// Host-owned, inert web-search configuration and provider selection.
pub mod web_search;
mod wire_types;

use std::fs::{OpenOptions, TryLockError};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::http::{header, Method};
use axum::routing::{delete, get, patch, post};
use axum::Router;
use tokio::net::TcpListener;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use uuid::Uuid;

use resolver::KeyedResolver;
use tidebreak_code_execution::ExecTool;
use tidebreak_core::{
    ask_user_questions_tool_spec, browser_act_tool_spec, browser_list_tool_spec,
    browser_navigate_tool_spec, browser_screenshot_tool_spec, browser_snapshot_tool_spec,
    browser_wait_tool_spec, computer_capture_screen_tool_spec, computer_click_tool_spec,
    computer_focus_window_tool_spec, computer_key_press_tool_spec, computer_list_windows_tool_spec,
    computer_read_app_content_tool_spec, computer_return_to_tidebreak_tool_spec,
    computer_scroll_tool_spec, computer_type_text_tool_spec, computer_wait_tool_spec,
    exit_plan_mode_tool_spec, import_connected_file_tool_spec, list_connected_folders_tool_spec,
    list_folder_tool_spec, read_connected_file_tool_spec, request_folder_access_tool_spec,
    validate_ask_user_questions_arguments, validate_browser_act_arguments,
    validate_browser_list_arguments, validate_browser_navigate_arguments,
    validate_browser_screenshot_arguments, validate_browser_snapshot_arguments,
    validate_browser_wait_arguments, validate_computer_capture_screen_arguments,
    validate_computer_click_arguments, validate_computer_focus_window_arguments,
    validate_computer_key_press_arguments, validate_computer_list_windows_arguments,
    validate_computer_read_app_content_arguments, validate_computer_return_to_tidebreak_arguments,
    validate_computer_scroll_arguments, validate_computer_type_text_arguments,
    validate_computer_wait_arguments, validate_exit_plan_mode_arguments,
    validate_import_connected_file_arguments, validate_list_connected_folders_arguments,
    validate_list_folder_arguments, validate_read_connected_file_arguments,
    validate_request_folder_access_arguments, validate_write_output_to_connected_folder_arguments,
    write_output_to_connected_folder_tool_spec, AgentConfig, AgentError, ApprovalClass, BlobStore,
    BundledSecretProvider, CachingSecretProvider, Config, CreateAppTool, DbStore, FsBlobStore,
    KeychainSecretProvider, ListDir, Profile, ReadFile, Result, SecretProvider, Store, Tool,
    ToolRegistry, WriteFile,
};

/// Public contract for desktop browser adapters. The desktop implements
/// [`BrowserRuntime`] behind an `Arc` and installs it with
/// [`bind`] or one of its desktop variants.
pub use crate::code::browser_runtime::{BrowserRuntime, BrowserRuntimeError, BrowserRuntimeScope};

/// Bind-time pairing of the native browser runtime and the trusted bridge
/// executable.
///
/// Both halves must be present for the server to mint browser channels. The
/// desktop constructs this when it has a [`BrowserRuntime`] and has resolved
/// the absolute path to the `tidebreak` CLI sidecar that sits beside the
/// host broker. When either half is absent, no browser tools are advertised
/// or injected — sessions work exactly as before the browser channel existed.
///
/// `bridge_command` must be an absolute path. The desktop sibling resolver
/// owns existence, file-type, and executable checks; the server boundary
/// validates absoluteness as defense in depth.
#[derive(Clone)]
pub struct BrowserChannelBinding {
    /// The native browser adapter.
    pub runtime: Arc<dyn BrowserRuntime>,
    /// Absolute path to the trusted bridge executable.
    pub bridge_command: PathBuf,
}

impl BrowserChannelBinding {
    /// Construct a binding from both required halves.
    ///
    /// `bridge_command` must be absolute; the desktop sibling resolver must
    /// have already verified existence and executability.
    #[must_use]
    pub fn new(runtime: Arc<dyn BrowserRuntime>, bridge_command: PathBuf) -> Self {
        Self {
            runtime,
            bridge_command,
        }
    }

    /// Return the native runtime.
    #[must_use]
    pub fn runtime(&self) -> &Arc<dyn BrowserRuntime> {
        &self.runtime
    }

    /// Return the absolute bridge executable path.
    #[must_use]
    pub fn bridge_command(&self) -> &std::path::Path {
        &self.bridge_command
    }
}
pub use durable_oplog::DurableOperationStore;
pub use error::ServerError;
pub use pairing::{
    deprovision_provisioned_gateway, deprovision_target, register_pending_pairing,
    register_replacing_pairing, DeprovisionTarget, PairingError, PairingHandle,
    PendingRegistration,
};
pub use state::{AppState, LocalVoiceError, LocalVoiceRunner, LocalVoiceState, LocalVoiceStatus};

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
            "/projects/{project_id}/documents/promote",
            post(routes::promote_document_to_project),
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
        );

    // Caller-held bytes are narrower than native client execution. On a
    // desktop embedding they require the scoped capability published through
    // listen.json (or the native executor credential); on a headless embed the
    // primary bearer remains sufficient for CLI/API compatibility.
    let chat_publication_api = Router::new()
        .route(
            "/chats/{chat_id}/documents/raw",
            post(routes::ingest_raw_chat_document)
                .layer(DefaultBodyLimit::max(MAX_RAW_DOCUMENT_BYTES)),
        )
        .route(
            "/chats/{chat_id}/attachments/images",
            post(routes::publish_chat_image_attachment)
                .layer(DefaultBodyLimit::max(routes::MAX_IMAGE_ATTACHMENT_BYTES)),
        )
        .route(
            "/code/sessions/{id}/attachments/images",
            post(routes::code::publish_session_image)
                .layer(DefaultBodyLimit::max(routes::MAX_IMAGE_ATTACHMENT_BYTES)),
        );

    let client_executor_api = Router::new()
        .route(
            "/native/mcp/servers",
            axum::routing::put(routes::put_mcp_servers)
                .layer(DefaultBodyLimit::max(mcp_config::MAX_CONFIG_BODY_BYTES)),
        )
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
    let (client_executor_api, public_document_api, scoped_publication_api) =
        if state.root_attachment_routes_enabled {
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
            let scoped_publication_api =
                chat_publication_api.route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    auth::require_local_import_capability,
                ));
            (client_executor_api, Router::new(), scoped_publication_api)
        } else {
            (
                client_executor_api,
                document_api.merge(chat_publication_api),
                Router::new(),
            )
        };
    let client_executor_api = client_executor_api.route_layer(
        axum::middleware::from_fn_with_state(state.clone(), auth::require_client_executor_token),
    );

    // The deployment plane: everything that changes what this deployment *is*
    // or touches its shared secrets — MCP servers (host processes), provider
    // and search and execution credentials (writes, deletes, and the presence
    // reads that reveal their metadata), model roles, global settings writes,
    // plugin install/enable, and connected-app sign-in/sign-out.
    //
    // Membership is decided here rather than in the handlers, so a new config
    // route is gated by where it is registered. Reads that only tell a client
    // what the deployment can do stay on the member plane below, including the
    // `GET` halves of paths whose `PUT` lives here.
    //
    // See `docs/decisions/0006-self-host-deployment-plane-authorization.md`.
    let deployment_api = Router::new()
        .route("/settings", axum::routing::put(routes::put_settings))
        .route(
            // The legacy single-key Anthropic shim writes into the same shared
            // secret store the typed provider credentials do.
            "/settings/api-key",
            axum::routing::put(routes::put_api_key).delete(routes::delete_api_key),
        )
        .route(
            "/models/roles/{role}",
            axum::routing::put(routes::put_model_role),
        )
        .route(
            "/web-search",
            axum::routing::put(routes::put_web_search_config),
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
        .route(
            "/code-execution",
            axum::routing::put(routes::put_code_execution_config)
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
            axum::routing::put(routes::put_mcp_servers)
                .layer(DefaultBodyLimit::max(mcp_config::MAX_CONFIG_BODY_BYTES)),
        )
        .route(
            "/mcp/servers/{name}/reconnect",
            post(routes::post_mcp_server_reconnect),
        )
        .route(
            "/plugins/install",
            post(routes::post_plugin_install).layer(DefaultBodyLimit::max(
                plugin_install::MAX_PLUGIN_INSTALL_BODY_BYTES,
            )),
        )
        .route(
            "/plugins/enabled",
            axum::routing::put(routes::put_plugins_enabled)
                .layer(DefaultBodyLimit::max(routes::MAX_PLUGIN_ENABLE_BODY_BYTES)),
        )
        .route(
            "/connected-apps/rest/{id}",
            axum::routing::put(routes::put_rest_connected_app)
                .delete(routes::delete_rest_connected_app)
                .layer(DefaultBodyLimit::max(
                    routes::MAX_REST_CONNECTED_APP_BODY_BYTES,
                )),
        )
        .route(
            "/connected-apps/rest/spec-preview",
            post(routes::post_rest_spec_preview).layer(DefaultBodyLimit::max(
                routes::MAX_REST_CONNECTED_APP_BODY_BYTES,
            )),
        )
        .route("/gateway/sign-in", post(routes::post_gateway_sign_in))
        .route("/gateway/sign-out", post(routes::post_gateway_sign_out))
        .route(
            "/gateway/models/sync",
            post(routes::post_gateway_models_sync),
        )
        .route(
            "/providers/{kind}",
            axum::routing::put(routes::put_provider),
        )
        .route(
            "/providers/{kind}/credential",
            axum::routing::delete(routes::delete_provider_credential),
        )
        .route(
            "/providers/openai/chatgpt/sign-in",
            post(routes::post_openai_chatgpt_sign_in),
        )
        .route(
            "/providers/openai/chatgpt/sign-out",
            post(routes::post_openai_chatgpt_sign_out),
        )
        .route(
            "/voice-transcription",
            axum::routing::put(routes::put_voice_transcription)
                .layer(DefaultBodyLimit::max(voice_transcription::MAX_AUDIO_BYTES)),
        )
        .route(
            "/voice-transcription/install",
            post(routes::post_voice_transcription_install),
        )
        // Code mode's deployment-plane routes. Installing a pinned harness
        // writes a binary to this machine, and the clone parent and worktree
        // root are shared settings that decide where every principal's
        // checkouts and worktrees land on its disk — all of them change what
        // the deployment *is*, so none belongs to a member (decisions 6 and 48
        // step 1). Creating workspaces under those directories stays on the
        // member plane: that produces owner-scoped rows and owner-keyed
        // checkouts.
        .route(
            "/code/harnesses/refresh",
            post(routes::code::refresh_harnesses),
        )
        .route(
            "/code/harnesses/{kind}/install",
            post(routes::code::install_harness),
        )
        .route(
            "/code/repos/clone-defaults",
            get(routes::code::clone_defaults),
        )
        .route(
            "/code/worktree-root",
            get(routes::code::get_worktree_root).put(routes::code::set_worktree_root),
        )
        .route("/diagnostics/snapshot", get(diagnostics::get_snapshot))
        .route("/diagnostics/metrics", get(diagnostics::get_metrics))
        .route("/diagnostics/export", get(diagnostics::get_export))
        .route_layer(axum::middleware::from_fn(auth::require_admin));

    // The engine-facing browser channel. Authenticated per request by the
    // session-scoped capability bearer (see `routes::code::browser`), so this
    // router is kept out of `require_token` below; the token never appears in
    // a path or query, which is why navigate and snapshot are POST.
    let browser_api = Router::new()
        .route("/code/browser/list", get(routes::code::browser_list))
        .route(
            "/code/browser/navigate",
            post(routes::code::browser_navigate),
        )
        .route(
            "/code/browser/snapshot",
            post(routes::code::browser_snapshot),
        )
        .route("/code/browser/wait", post(routes::code::browser_wait))
        .route(
            "/code/browser/screenshot",
            post(routes::code::browser_screenshot),
        )
        .route("/code/browser/act", post(routes::code::browser_act))
        .with_state(state.clone());

    // The engine-facing inference relay (decision 71). Authenticated per
    // request by the session-scoped relay key, so it stays outside
    // `require_token` like the browser channel above.
    let harness_llm_api = Router::new()
        .route(
            "/code/llm/anthropic/v1/messages",
            post(routes::code::harness_llm_anthropic_messages),
        )
        .route(
            "/code/llm/openai/v1/models",
            get(routes::code::harness_llm_openai_models),
        )
        .route(
            "/code/llm/openai/v1/responses",
            post(routes::code::harness_llm_openai_responses),
        )
        .layer(DefaultBodyLimit::max(
            routes::code::MAX_HARNESS_LLM_BODY_BYTES,
        ))
        .with_state(state.clone());

    let api = Router::new()
        .route("/settings", get(routes::get_settings))
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
        .route("/web-search", get(routes::get_web_search_config))
        .route(
            "/code-execution",
            get(routes::get_code_execution_config)
                .layer(DefaultBodyLimit::max(MAX_CODE_EXECUTION_CONFIG_BODY_BYTES)),
        )
        .route("/mcp/servers", get(routes::get_mcp_servers))
        .route("/connected-apps", get(routes::get_connected_apps))
        // The installed skill/plugin catalog and its enable flags.
        .route("/plugins", get(routes::get_plugins))
        .route(
            "/plugins/skills/{name}/instructions",
            get(routes::get_skill_instructions),
        )
        .route("/plugins/prompts/{name}/body", get(routes::get_prompt_body))
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
        .route(
            "/apps/{id}/gateway-page",
            post(routes::post_app_gateway_page),
        )
        .route("/apps", get(routes::get_app_library))
        .route(
            "/apps/{id}",
            get(routes::get_app_detail).delete(routes::delete_app),
        )
        .route("/policy", get(routes::get_policy))
        .route("/gateway/status", get(routes::get_gateway_status))
        .route("/gateway/apps", get(routes::get_gateway_apps))
        .route("/gateway/machine", get(routes::get_gateway_machine))
        .route(
            "/gateway/pairing/dismiss",
            post(routes::post_gateway_pairing_dismiss),
        )
        .route("/providers", get(routes::list_providers))
        .route(
            "/voice-transcription",
            get(routes::get_voice_transcription)
                .post(routes::post_voice_transcription)
                .layer(DefaultBodyLimit::max(voice_transcription::MAX_AUDIO_BYTES)),
        )
        .route(
            "/providers/openai/chatgpt/status",
            get(routes::get_openai_chatgpt_status),
        )
        .merge(public_document_api)
        .merge(scoped_publication_api)
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
        .route("/inbox", get(routes::list_inbox))
        .route("/notifications", get(routes::list_notifications))
        .route(
            "/notifications/unread-count",
            get(routes::unread_notification_count),
        )
        .route(
            "/notifications/read",
            axum::routing::post(routes::mark_notifications_read),
        )
        .route(
            "/notifications/read-all",
            axum::routing::post(routes::mark_all_notifications_read),
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
            "/chats/{chat_id}/agent-runs/{run_id}/task-plan",
            get(routes::get_agent_run_task_plan),
        )
        .route(
            "/chats/{chat_id}/agent-runs/{run_id}/progress",
            get(routes::list_agent_run_progress),
        )
        .route(
            "/chats/{chat_id}/agent-runs/{run_id}/cancel",
            post(routes::post_agent_run_cancel),
        )
        .route(
            "/chats/{chat_id}/agent-runs/{run_id}/resume",
            post(routes::post_agent_run_resume),
        )
        .route("/chats/{chat_id}/queued", get(routes::list_queued_turns))
        .route(
            "/chats/{chat_id}/queued/{turn_id}",
            axum::routing::patch(routes::patch_queued_turn).delete(routes::delete_queued_turn),
        )
        .route(
            "/chats/{chat_id}/queue-paused",
            axum::routing::put(routes::put_queue_paused),
        )
        .route(
            "/chats/{chat_id}/queued/send-now",
            post(routes::post_queue_send_now),
        )
        .route(
            "/chats/{chat_id}/agent-runs/{run_id}/steer",
            post(routes::post_agent_run_steer),
        )
        .route("/chats/{id}/messages", post(routes::post_message))
        .route("/chats/{id}/cancel", post(routes::post_cancel))
        .route("/chats/{id}/steer", post(routes::post_steer))
        .route(
            "/chats/{id}/compact",
            post(routes::post_compact)
                .layer(DefaultBodyLimit::max(routes::MAX_COMPACT_CHAT_BODY_BYTES)),
        )
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
        .route("/chats/{id}/task-plan", get(routes::get_task_plan))
        // Conversation outputs. Everything but writing the bytes to a chosen
        // path is here, so the desktop and a headless client read, edit, and
        // export the same outputs through the same implementation.
        .route("/chats/{chat_id}/outputs", get(routes::list_chat_outputs))
        .route(
            "/chats/{chat_id}/outputs/{output_id}",
            get(routes::get_chat_output).delete(routes::delete_chat_output),
        )
        .route(
            "/chats/{chat_id}/outputs/{output_id}/content",
            get(routes::get_chat_output_content),
        )
        .route(
            "/chats/{chat_id}/outputs/{output_id}/restore",
            post(routes::restore_chat_output),
        )
        .route(
            "/chats/{chat_id}/outputs/{output_id}/revisions",
            get(routes::list_chat_output_revisions)
                .post(routes::save_chat_output_revision)
                .layer(DefaultBodyLimit::max(
                    routes::MAX_OUTPUT_REVISION_BODY_BYTES,
                )),
        )
        .route(
            "/chats/{chat_id}/outputs/{output_id}/revisions/{revision_id}",
            get(routes::get_chat_output_revision),
        )
        .route(
            "/chats/{chat_id}/outputs/{output_id}/revisions/{revision_id}/restore",
            post(routes::restore_chat_output_revision),
        )
        .route("/chats/{id}/approvals", get(routes::list_pending_approvals))
        .route(
            "/chats/{id}/approvals/{call_id}",
            post(routes::post_approval),
        )
        .route("/grants", get(routes::list_standing_grants))
        .route("/consent/statements", get(routes::list_consent_statements))
        .route(
            "/grants/{call_id}",
            axum::routing::delete(routes::delete_standing_grant),
        )
        .route("/chats/{id}/events", get(routes::chat_events))
        .route(
            "/code/repos",
            post(routes::code::create_repo).get(routes::code::list_repos),
        )
        .route("/code/repos/sources", get(routes::code::repo_sources))
        .route(
            "/code/repos/github",
            get(routes::code::list_github_repositories),
        )
        .route("/code/repos/clone", post(routes::code::start_clone))
        .route("/code/repos/clone/{job}", get(routes::code::get_clone_job))
        .route(
            "/code/repos/{id}",
            get(routes::code::get_repo)
                .patch(routes::code::patch_repo)
                .delete(routes::code::delete_repo),
        )
        .route("/code/harnesses", get(routes::code::list_harnesses))
        .route("/code/analytics", get(routes::code::analytics))
        .route("/code/usage", get(routes::code::subscription_usage))
        .route(
            "/code/delivery/repositories",
            get(routes::code::discover_delivery_repositories),
        )
        .route(
            "/code/delivery/repositories/resolve",
            post(routes::code::resolve_delivery_repositories),
        )
        .route(
            "/code/delivery/pull-requests/query",
            post(routes::code::query_delivery_pull_requests),
        )
        .route(
            "/code/delivery/pull-requests/detail",
            post(routes::code::delivery_pull_request_detail),
        )
        .route(
            "/code/delivery/pull-requests/action",
            post(routes::code::act_on_delivery_pull_request),
        )
        .route(
            "/code/delivery/runs/query",
            post(routes::code::query_delivery_runs),
        )
        .route(
            "/code/delivery/runs/detail",
            post(routes::code::delivery_run_detail),
        )
        .route(
            "/code/delivery/runs/action",
            post(routes::code::act_on_delivery_run),
        )
        .route(
            "/code/harnesses/{kind}/models",
            get(routes::code::list_harness_models),
        )
        .route(
            "/code/workspaces",
            post(routes::code::create_workspace).get(routes::code::list_workspaces),
        )
        .route("/code/storage", get(routes::code::list_storage))
        .route(
            "/code/workspaces/{id}",
            get(routes::code::get_workspace).patch(routes::code::patch_workspace),
        )
        .route(
            "/code/workspaces/{id}/archive",
            post(routes::code::archive_workspace),
        )
        .route(
            "/code/workspaces/{id}/release",
            post(routes::code::release_workspace),
        )
        .route(
            "/code/workspaces/{id}/restore",
            post(routes::code::restore_workspace),
        )
        .route(
            "/code/workspaces/{id}/retry-setup",
            post(routes::code::retry_workspace_setup),
        )
        .route(
            "/code/workspaces/{id}/files",
            get(routes::code::list_workspace_files),
        )
        .route(
            "/code/workspaces/{id}/tree",
            get(routes::code::list_workspace_tree),
        )
        .route(
            "/code/workspaces/{id}/search",
            get(routes::code::search_workspace),
        )
        .route(
            "/code/workspaces/{id}/blob",
            get(routes::code::get_workspace_blob),
        )
        .route(
            "/code/workspaces/{id}/diff",
            get(routes::code::get_workspace_diff),
        )
        .route(
            "/code/workspaces/{id}/git/commit",
            post(routes::code::commit_workspace),
        )
        .route(
            "/code/workspaces/{id}/git/push",
            post(routes::code::push_workspace),
        )
        .route(
            "/code/workspaces/{id}/git/pr",
            post(routes::code::create_pull_request),
        )
        .route(
            "/code/workspaces/{id}/pr",
            get(routes::code::get_workspace_pr),
        )
        .route(
            "/code/workspaces/{id}/pull-requests",
            get(routes::code::list_workspace_pull_requests),
        )
        .route(
            "/code/workspaces/{id}/pr/refresh",
            post(routes::code::refresh_workspace_pr),
        )
        .route(
            "/code/workspaces/{id}/pr/comments",
            get(routes::code::get_workspace_pr_comments),
        )
        .route(
            "/code/workspaces/{id}/pr/check-logs",
            post(routes::code::write_workspace_check_logs),
        )
        .route(
            "/code/workspaces/{id}/pr/merge",
            post(routes::code::merge_workspace_pr),
        )
        .route(
            "/code/workspaces/{id}/pr/ready",
            post(routes::code::mark_workspace_pr_ready),
        )
        .route(
            "/code/repos/{id}/triggers",
            get(routes::code::list_repo_triggers).post(routes::code::create_repo_trigger),
        )
        .route(
            "/code/repos/{id}/triggers/{trigger_id}",
            patch(routes::code::update_repo_trigger).delete(routes::code::delete_repo_trigger),
        )
        .route(
            "/code/workspaces/{id}/watch",
            post(routes::code::start_workspace_watch).delete(routes::code::stop_workspace_watch),
        )
        .route(
            "/code/workspaces/{id}/actions/{name}",
            post(routes::code::run_workspace_action),
        )
        .route(
            "/code/workspaces/{id}/sessions",
            post(routes::code::create_session).get(routes::code::list_workspace_sessions),
        )
        .route(
            "/code/sessions/{id}/turns",
            post(routes::code::submit_turn).get(routes::code::list_session_turns),
        )
        .route(
            "/code/sessions/{id}/queued",
            get(routes::code::list_queued_turns),
        )
        .route(
            "/code/sessions/{id}/queued/{queued_id}",
            axum::routing::patch(routes::code::patch_queued_turn)
                .delete(routes::code::delete_queued_turn),
        )
        .route(
            "/code/sessions/{id}/queue-paused",
            axum::routing::put(routes::code::put_queue_paused),
        )
        .route(
            "/code/sessions/{id}/queued/send-now",
            post(routes::code::post_queue_send_now),
        )
        .route(
            "/code/sessions/{id}/attachments/images/{blob_id}",
            get(routes::code::get_session_image),
        )
        .route(
            "/code/sessions/{id}/steer",
            post(routes::code::steer_session),
        )
        .route(
            "/code/sessions/{id}/interrupt",
            post(routes::code::interrupt_session),
        )
        .route("/code/sessions/{id}/reap", post(routes::code::reap_session))
        .route(
            "/code/sessions/{id}/mode",
            post(routes::code::set_session_permission_mode),
        )
        .route(
            "/code/sessions/{id}/effort",
            post(routes::code::set_session_reasoning_effort),
        )
        .route(
            "/code/sessions/{id}/fast-mode",
            post(routes::code::set_session_fast_mode),
        )
        .route("/code/sessions/{id}/fork", post(routes::code::fork_session))
        .route(
            "/code/sessions/{id}/debug",
            get(routes::code::get_session_debug),
        )
        .route(
            "/code/sessions/{id}/attention",
            post(routes::code::set_attention),
        )
        .route(
            "/code/sessions/{id}/events",
            get(routes::code::session_events),
        )
        .route("/code/updates", get(routes::code::code_updates))
        .route(
            "/code/workspaces/{id}/terminals",
            post(routes::code::create_terminal)
                .get(routes::code::list_terminals)
                .delete(routes::code::close_workspace_terminals),
        )
        .route(
            "/code/workspaces/{id}/terminals/{tid}",
            axum::routing::delete(routes::code::close_terminal),
        )
        .route(
            "/code/workspaces/{id}/terminals/{tid}/read",
            get(routes::code::read_terminal),
        )
        .route(
            "/code/workspaces/{id}/terminals/{tid}/write",
            post(routes::code::write_terminal),
        )
        .route(
            "/code/workspaces/{id}/terminals/{tid}/resize",
            post(routes::code::resize_terminal),
        )
        .route("/code/approvals", get(routes::code::list_approvals))
        .route(
            "/code/approvals/{id}/decision",
            post(routes::code::decide_approval),
        )
        .merge(client_executor_api)
        // Merged before the bearer layer, so `require_token` wraps outside
        // `require_admin`: an unauthenticated request to a deployment-plane
        // route is a 401 from the bearer check, and only an authenticated
        // member reaches the role check's 403.
        .merge(deployment_api)
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ))
        .with_state(state.clone())
        // The engine-facing browser channel authenticates each request with
        // the per-session capability bearer its capfile carries, never the
        // launch token, so its routes merge after `route_layer` wrapped the
        // routes above and stay outside `require_token`. They still sit
        // inside `require_app_origin` and CORS with the rest of the API.
        // The inference relay authenticates the same way with its own
        // per-session key.
        .merge(browser_api)
        .merge(harness_llm_api);
    let frame_state = state.clone();
    let auth_discovery = Router::new()
        .route("/auth/discovery", get(auth::discovery))
        .with_state(state.clone());

    // Loopback-only + bearer token is the real gate. CORS names the origins
    // this app actually loads its frontend from rather than mirroring whatever
    // asked, so a page on the public web cannot read a response even holding a
    // leaked bearer. The same predicate backs the `Origin` middleware below,
    // which covers what CORS does not: WebSocket upgrades.
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _parts| {
            auth::origin_value_is_this_app(origin)
        }))
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
        .route(
            "/code/mcp/approval-prompt",
            post(routes::code::approval_prompt),
        )
        .with_state(frame_state);

    Router::new()
        .merge(view_frames)
        .merge(auth_discovery)
        .merge(api)
        // Inside CORS, so a foreign preflight is answered by the CORS layer's
        // own rejection rather than by a bare 403.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_app_origin,
        ))
        .layer(cors)
        .layer(axum::middleware::from_fn_with_state(
            state,
            diagnostics::observe_http_request,
        ))
        // A liveness probe with no auth, added after both layers so it carries
        // neither: nothing reads it cross-origin, and answering preflights for
        // it only helps a page confirm the app is on a guessed port.
        .route("/healthz", get(healthz))
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
    code_execution: Arc<code_execution::ConfiguredExecProvider>,
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
    _sandbox_task_plan_worker: AbortTask,
    _sandbox_exec_worker: AbortTask,
    _agent_run_scratch_reaper: AbortTask,
    _blob_retirement_worker: AbortTask,
    _blob_orphan_auditor: AbortTask,
    _approval_judge_worker: AbortTask,
    _mcp_supervisor: AbortTask,
    _gateway_model_sync: AbortTask,
    _instance_lock: InstanceLock,
    /// Removes `{data_dir}/listen.json` when this server drops.
    _listen_endpoint: listen_endpoint::ListenEndpointGuard,
}

/// The claim one process makes on a data directory for as long as it serves it.
///
/// An OS advisory lock on a file in the directory, held open for the process's
/// lifetime. The kernel drops it when the file descriptor closes, which happens
/// on a clean exit and on a crash alike — so a stale `tidebreak.lock` left on
/// disk by a killed process never bricks the directory the way a PID file
/// would. The file's contents are irrelevant; only the lock is.
struct InstanceLock {
    _file: std::fs::File,
}

impl InstanceLock {
    fn acquire(config: &Config) -> Result<Self> {
        std::fs::create_dir_all(&config.data_dir)
            .map_err(|error| AgentError::config(format!("failed to create data dir: {error}")))?;
        let path = config.data_dir.join("tidebreak.lock");
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
            // Two servers over one directory would race the database with
            // nothing but SQLite's own locking between them, so this refuses
            // instead. The second process's way in is to be a client of the
            // first rather than a second server.
            Err(TryLockError::WouldBlock) => Err(AgentError::config(format!(
                "another Tidebreak process is already running on the data directory {}. \
                 Attach with the CLI's --attach (reads {}/listen.json), or \
                 --server <url> with TIDEBREAK_SERVER_TOKEN; quit the running one, \
                 or point TIDEBREAK_DATA_DIR somewhere else.",
                config.data_dir.display(),
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
/// with `TIDEBREAK_MODEL`.
const DEFAULT_MODEL: &str = "gpt-5.6-sol";

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
        None,
        None,
        None,
        None,
        None,
        None,
        false,
    )
    .await
}

/// Bind the API and mount external MCP servers from `TIDEBREAK_MCP_CONFIG`.
///
/// This is the product boot path used by the CLI. Custom embedders can continue
/// to use [`bind`] when process-environment configuration is undesirable.
pub async fn bind_configured(config: Config) -> Result<Server> {
    let mcp_servers = mcp_config::ConfiguredMcpServers::from_env()?;
    bind_inner(
        config,
        None,
        mcp_servers,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        false,
    )
    .await
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
        None,
        None,
        None,
        None,
        None,
        None,
        false,
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
    bind_inner(
        config,
        Some(client_executor_id),
        mcp_servers,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        false,
    )
    .await
}

/// Desktop binding with the native bridges only the product app can provide:
/// the resolver that turns connected folders into per-invocation local
/// sandbox grants, the office-to-PDF converter that renders office
/// outputs for the model's visual QA loop, and the host-folder surface
/// local-app folder bindings dispatch through.
pub async fn bind_configured_with_desktop_executor_and_folder_grants(
    config: Config,
    client_executor_id: Uuid,
    folder_grant_resolver: Arc<dyn code_execution::ExecFolderGrantResolver>,
    office_converter: Option<Arc<dyn tidebreak_code_execution::HostOfficeConverter>>,
    host_tool_broker: Option<Arc<dyn tidebreak_code_execution::HostToolBroker>>,
    local_voice: Option<Arc<dyn LocalVoiceRunner>>,
    host_folders: Option<Arc<dyn host_folders::HostFolders>>,
) -> Result<Server> {
    bind_configured_with_desktop_executor_and_folder_grants_and_browser_binding(
        config,
        client_executor_id,
        folder_grant_resolver,
        office_converter,
        host_tool_broker,
        local_voice,
        host_folders,
        None,
    )
    .await
}

/// Desktop binding with the native host bridges plus a browser runtime that
/// must be available before code-session recovery starts.
///
/// This function is preserved for source compatibility. A caller that
/// supplies only a runtime — without the bridge executable — gets no
/// browser tools: the bridge command is required too. The runtime remains
/// installed for the native route seam, but session creation leaves
/// `SessionSpec::browser` unset until both halves are present.
/// New callers should use
/// [`bind_configured_with_desktop_executor_and_folder_grants_and_browser_binding`]
/// directly.
#[allow(clippy::too_many_arguments)]
pub async fn bind_configured_with_desktop_executor_and_folder_grants_and_browser_runtime(
    config: Config,
    client_executor_id: Uuid,
    folder_grant_resolver: Arc<dyn code_execution::ExecFolderGrantResolver>,
    office_converter: Option<Arc<dyn tidebreak_code_execution::HostOfficeConverter>>,
    host_tool_broker: Option<Arc<dyn tidebreak_code_execution::HostToolBroker>>,
    local_voice: Option<Arc<dyn LocalVoiceRunner>>,
    host_folders: Option<Arc<dyn host_folders::HostFolders>>,
    browser_runtime: Option<Arc<dyn code::browser_runtime::BrowserRuntime>>,
) -> Result<Server> {
    bind_configured_with_desktop_executor_and_folder_grants_and_browser_parts(
        config,
        client_executor_id,
        folder_grant_resolver,
        office_converter,
        host_tool_broker,
        local_voice,
        host_folders,
        browser_runtime,
        None,
        false,
    )
    .await
}

/// Desktop binding with the native host bridges plus a browser channel
/// binding (runtime + bridge executable) that must be available before
/// code-session recovery starts.
///
/// When `binding` is `None`, no code-session browser tools are advertised or
/// injected.
/// When both halves are present (the binding always carries both, by
/// construction), session creation mints a session-private capability
/// file and injects the bridge executable path into engine config.
#[allow(clippy::too_many_arguments)]
pub async fn bind_configured_with_desktop_executor_and_folder_grants_and_browser_binding(
    config: Config,
    client_executor_id: Uuid,
    folder_grant_resolver: Arc<dyn code_execution::ExecFolderGrantResolver>,
    office_converter: Option<Arc<dyn tidebreak_code_execution::HostOfficeConverter>>,
    host_tool_broker: Option<Arc<dyn tidebreak_code_execution::HostToolBroker>>,
    local_voice: Option<Arc<dyn LocalVoiceRunner>>,
    host_folders: Option<Arc<dyn host_folders::HostFolders>>,
    binding: Option<BrowserChannelBinding>,
) -> Result<Server> {
    let (browser_runtime, browser_bridge_command) = match binding {
        Some(binding) => (Some(binding.runtime), Some(binding.bridge_command)),
        None => (None, None),
    };
    bind_configured_with_desktop_executor_and_folder_grants_and_browser_parts(
        config,
        client_executor_id,
        folder_grant_resolver,
        office_converter,
        host_tool_broker,
        local_voice,
        host_folders,
        browser_runtime,
        browser_bridge_command,
        false,
    )
    .await
}

/// Desktop binding that also installs the durable foreground browser
/// executor. This explicit entry point is the only production path that turns
/// on foreground browser tool registration; a code browser binding alone does
/// not advertise those tools.
#[allow(clippy::too_many_arguments)]
pub async fn bind_configured_with_desktop_foreground_browser_executor(
    config: Config,
    client_executor_id: Uuid,
    folder_grant_resolver: Arc<dyn code_execution::ExecFolderGrantResolver>,
    office_converter: Option<Arc<dyn tidebreak_code_execution::HostOfficeConverter>>,
    host_tool_broker: Option<Arc<dyn tidebreak_code_execution::HostToolBroker>>,
    local_voice: Option<Arc<dyn LocalVoiceRunner>>,
    host_folders: Option<Arc<dyn host_folders::HostFolders>>,
    binding: Option<BrowserChannelBinding>,
) -> Result<Server> {
    let (browser_runtime, browser_bridge_command) = match binding {
        Some(binding) => (Some(binding.runtime), Some(binding.bridge_command)),
        None => (None, None),
    };
    bind_configured_with_desktop_executor_and_folder_grants_and_browser_parts(
        config,
        client_executor_id,
        folder_grant_resolver,
        office_converter,
        host_tool_broker,
        local_voice,
        host_folders,
        browser_runtime,
        browser_bridge_command,
        true,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn bind_configured_with_desktop_executor_and_folder_grants_and_browser_parts(
    config: Config,
    client_executor_id: Uuid,
    folder_grant_resolver: Arc<dyn code_execution::ExecFolderGrantResolver>,
    office_converter: Option<Arc<dyn tidebreak_code_execution::HostOfficeConverter>>,
    host_tool_broker: Option<Arc<dyn tidebreak_code_execution::HostToolBroker>>,
    local_voice: Option<Arc<dyn LocalVoiceRunner>>,
    host_folders: Option<Arc<dyn host_folders::HostFolders>>,
    browser_runtime: Option<Arc<dyn code::browser_runtime::BrowserRuntime>>,
    browser_bridge_command: Option<PathBuf>,
    foreground_browser_executor: bool,
) -> Result<Server> {
    if client_executor_id.is_nil() {
        return Err(AgentError::config("client executor id must not be nil"));
    }
    if let Some(bridge) = browser_bridge_command
        .as_ref()
        .filter(|bridge| !bridge.is_absolute())
    {
        return Err(AgentError::config(format!(
            "browser bridge command must be an absolute path: {}",
            bridge.display()
        )));
    }
    let mcp_servers = mcp_config::ConfiguredMcpServers::from_env()?;
    bind_inner(
        config,
        Some(client_executor_id),
        mcp_servers,
        Some(folder_grant_resolver),
        office_converter,
        host_tool_broker,
        local_voice,
        host_folders,
        browser_runtime,
        browser_bridge_command,
        foreground_browser_executor,
    )
    .await
}

/// The secret store the configured profile keeps its credentials in.
///
/// Two decorators over the OS keychain, and the order matters.
/// [`BundledSecretProvider`] puts every key in one stored item, so an app
/// update costs one access prompt rather than one per credential.
/// [`CachingSecretProvider`] sits above it so a key costs one read of that
/// item per process rather than one per turn: [`resolver::ConfiguredResolver`]
/// rebuilds its route set on every turn, and each candidate route reads its
/// provider's credential to decide whether it exists.
///
/// The gateway session key is the one exception to memoized misses: a session
/// can land in the keychain without this cache observing the write — a
/// boot-time status read is in flight while sign-in completes, resolving to
/// the old `NoEntry` after the write's invalidation already ran — and a
/// cached `None` would then hide the session from the sync loop until
/// restart. Its misses re-ask the store instead; an absent item answers
/// `NoEntry` without an ACL prompt, so the slow-path rereads are cheap.
fn secret_provider(config: &Config) -> ProfileSecrets {
    let keychain: Arc<dyn SecretProvider> = Arc::new(match &config.keychain_service {
        Some(service) => KeychainSecretProvider::with_service(service),
        None => KeychainSecretProvider::new(),
    });
    let bundle = Arc::new(BundledSecretProvider::new(keychain));
    let provider = Arc::new(
        CachingSecretProvider::new(bundle.clone())
            .with_miss_passthrough([crate::connectors::GATEWAY_SECRET_KEY]),
    );
    ProfileSecrets { bundle, provider }
}

/// The profile's credential store, at the two layers callers need.
struct ProfileSecrets {
    /// The single stored item. Held apart from `provider` because re-homing
    /// and migration act on the item itself, which is below what the
    /// `SecretProvider` contract can express.
    bundle: Arc<BundledSecretProvider>,
    /// What every consumer uses: the bundle behind the per-process read cache.
    provider: Arc<dyn SecretProvider>,
}

/// Re-home the configured profile's credentials — see [`secret_rehome`].
///
/// Opens the profile's store to enumerate the per-record connected-app
/// credential keys (a static list cannot name them), but takes no instance
/// lock, so it still runs beside or without the daemon.
pub async fn rehome_configured_secrets(
    config: &Config,
) -> Result<Vec<(String, secret_rehome::RehomeOutcome)>> {
    let store = connect_store(config).await?;
    secret_rehome::rehome_secrets(&*store, &secret_provider(config).bundle).await
}

// Every parameter is one optional native bridge an embedding may supply;
// bundling them into a struct would only rename the arity.
#[allow(clippy::too_many_arguments)]
async fn bind_inner(
    config: Config,
    client_executor_id: Option<Uuid>,
    mcp_servers: mcp_config::ConfiguredMcpServers,
    folder_grant_resolver: Option<Arc<dyn code_execution::ExecFolderGrantResolver>>,
    office_converter: Option<Arc<dyn tidebreak_code_execution::HostOfficeConverter>>,
    host_tool_broker: Option<Arc<dyn tidebreak_code_execution::HostToolBroker>>,
    local_voice: Option<Arc<dyn LocalVoiceRunner>>,
    host_folders: Option<Arc<dyn host_folders::HostFolders>>,
    browser_runtime: Option<Arc<dyn code::browser_runtime::BrowserRuntime>>,
    browser_bridge_command: Option<PathBuf>,
    foreground_browser_executor: bool,
) -> Result<Server> {
    // Resolved first, before the instance lock or the store: a desktop profile
    // handed `TIDEBREAK_LISTEN_ADDR` refuses the boot rather than binding a
    // routable interface, and that refusal should cost nothing and leave
    // nothing behind. See [`Config::bind_addr`].
    let bind_addr = config.bind_addr()?;
    // Desktop live delivery remains process-local. Turns, steering, and tool
    // approvals are durable, while one process still owns the complete data
    // directory and its worker set.
    let instance_lock = InstanceLock::acquire(&config)?;
    let sandbox_container_admission = sandbox_admission::resolve(&config);
    let sandbox_spawn_execution_location = sandbox_container_admission.execution_location;
    let db = connect_db(&config).await?;
    let store: Arc<dyn Store> = db.clone();
    let ProfileSecrets {
        bundle: secret_bundle,
        provider: secrets,
    } = secret_provider(&config);
    // An app update replaces this binary, and macOS pins a keychain item's
    // access to the creating binary's signature — so the first boot of a new
    // binary re-homes the credential bundle before any consumer reads from
    // it. Inline, not spawned: a concurrent pass could interleave with token
    // refresh and strand a session (see the function), and the pass is cheap —
    // one read+rewrite of one item, once per binary, with later boots of the
    // same binary skipping it entirely.
    // Best-effort: a failure here must not take boot down with it.
    if let Err(error) = secret_rehome::rehome_once_per_binary(&*store, &secret_bundle).await {
        tracing::warn!("could not re-home stored credentials: {error}");
    }
    // The product boot path is where this platform's OS-managed (MDM) policy
    // reader gets selected; directly assembled AppState stays hermetic. This
    // is the one instance shared by the boot policy read, the legacy-key
    // migration guard, the resolver, the gateway runtime, and the request
    // handlers, so they can never disagree on the resolved policy.
    let os_policy: Arc<dyn managed_policy::OsPolicySource> =
        managed_policy::platform_source(&config);
    // The provisioned policy's durable home is the sidecar file in the data
    // directory, not the SQLite profile: a profile below the migration pin is
    // deleted and rebuilt, and the policy (and with it the gateway session's
    // authorization) must survive that. One instance, shared the same way.
    let provisioned_policy: Arc<dyn managed_policy::ProvisionedPolicySource> = Arc::new(
        managed_policy::ProvisionedPolicyFile::in_data_dir(&config.data_dir),
    );
    // One-time upgrade import: a pairing recorded by an earlier build lives
    // in the settings table. Copy it to the file before the first policy
    // read — and fail the boot if the copy fails, because the retire step
    // below would read the unmigrated profile as unmanaged and clear the
    // very session the import exists to preserve.
    managed_policy::import_legacy_setting(&*provisioned_policy, &*store).await?;
    // Legacy credential auto-enable is gated on one policy read. A resolution
    // `Err` is
    // deliberately swallowed as "not allowed": an unreadable policy fails
    // closed to no BYOK arming while boot still proceeds, so the profile can
    // surface the error and be repaired instead of bricking.
    let boot_policy = managed_policy::resolve(&*provisioned_policy, &*os_policy);
    let byok_boot_allowed = matches!(&boot_policy, Ok(policy) if !policy.managed);
    // Credentials can outlive provider settings across an update: legacy
    // Anthropic keys and ChatGPT OAuth sessions should retain the enabled state
    // that saving/signing in originally established. Never do this on a managed
    // profile: auto-enabling a BYOK provider would fight the lockdown.
    if byok_boot_allowed {
        providers::migrate_legacy_provider_enablement(&*store, &*secrets).await?;
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
    let gateway = gateway_runtime::GatewayRuntime::new(
        store.clone(),
        secrets.clone(),
        provisioned_policy.clone(),
        os_policy.clone(),
    );
    let chatgpt = Arc::new(chatgpt_runtime::ChatGptRuntime::new(
        store.clone(),
        secrets.clone(),
    )?);
    // One instance, shared by the resolver that builds each caller's routes,
    // the authentication middleware that records each caller's live token,
    // and every surface that reads a caller's own entitlements (decision 62).
    let on_behalf_of_gateway = obo_gateway::OboGateway::from_config(&config)?;
    let resolver = Arc::new(
        KeyedResolver::new(
            store.clone(),
            secrets.clone(),
            gateway.clone(),
            chatgpt.clone(),
            provisioned_policy.clone(),
            os_policy.clone(),
        )
        .with_on_behalf_of_gateway(on_behalf_of_gateway.clone()),
    );
    // Under the `scripted-provider` feature only — absent from every released
    // binary — a scripted provider stands in for configured routing so a test
    // in another crate can drive a real turn. See [`scripted_provider`].
    let resolver: Arc<dyn resolver::ProviderResolver> = resolver;
    #[cfg(feature = "scripted-provider")]
    let resolver = scripted_provider::resolver_from_env()?.unwrap_or(resolver);
    let blobs: Arc<dyn BlobStore> = Arc::new(FsBlobStore::new(config.data_dir.join("blobs")));
    // The same lock root `AppState` uses. `BlobWriteGuard` rendezvouses through
    // permanent lock files, so a second handle over the directory excludes
    // against the first rather than shadowing it.
    let exec_blob_writes = Arc::new(state::BlobWriteGuard::new(
        config.data_dir.join("blob-locks"),
    ));
    let code_host_tool_broker = host_tool_broker.clone();
    let code_execution = Arc::new(
        code_execution::ConfiguredExecProvider::new(
            store.clone(),
            secrets.clone(),
            config.data_dir.join("scratch"),
        )
        .with_blobs(blobs.clone())
        .with_blob_write_locks(exec_blob_writes)
        .with_document_scripts(config.exec_scripts_dir.clone())
        .with_skills(config.exec_skills_dir.clone())
        .with_prompts(config.exec_prompts_dir.clone())
        .with_plugins(config.exec_plugins_dir.clone())
        .with_user_skills(Some(config.user_skills_dir()))
        .with_user_prompts(Some(config.user_prompts_dir()))
        .with_user_plugins(Some(config.user_plugins_dir()))
        .with_folder_grant_resolver(folder_grant_resolver)
        .with_office_converter(office_converter)
        .with_host_tool_broker(host_tool_broker),
    );
    let foreground_web_search = web_search::foreground_tool(
        store.clone(),
        secrets.clone(),
        resolver.clone(),
        boot_default_model(),
    );
    let web_extract = web_search::foreground_extract_tool(store.clone(), secrets.clone());
    // Computer use exists only where there is a display to capture and a
    // trusted client to drive it: the desktop profile on macOS, where the
    // bundled broker has a real computer-use backend, AND the user has not
    // turned the capability off in settings. Anywhere else the tools are
    // simply not registered, so the model never sees them and a self-host or
    // background surface can never hold them.
    let computer_use_enabled = store
        .get_setting(crate::routes::COMPUTER_USE_ENABLED_SETTING)
        .await
        .ok()
        .flatten()
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    let computer_use =
        computer_use_enabled && config.profile == Profile::Desktop && cfg!(target_os = "macos");
    // Foreground browser tools exist only when the native desktop explicitly
    // installs their durable executor. Supplying the code-session runtime is
    // not enough, and non-desktop or non-macOS profiles stay fail-closed even
    // if an embedding passes the flag incorrectly.
    let foreground_browser = foreground_browser_executor
        && config.profile == Profile::Desktop
        && cfg!(target_os = "macos");
    let foreground_browser_semantic_actions = foreground_browser
        && browser_runtime
            .as_ref()
            .is_some_and(|runtime| runtime.supports_semantic_actions());
    // Tool execution and every sandbox worker must share the same local
    // cancellation handles. The tool registry is assembled before AppState,
    // so create those handles here and install the same Arcs into state after
    // its durable dependencies have been assembled.
    let agent_run_wake = Arc::new(tokio::sync::Notify::new());
    let sandbox_attempts = Arc::new(state::SandboxAttemptGuard::default());
    let sandbox_steering = Arc::new(state::SandboxSteerGuard::default());
    let cancellation_acceleration = agent_control_tools::SandboxCancellationAcceleration::new(
        store.clone(),
        sandbox_attempts.clone(),
        sandbox_steering.clone(),
        agent_run_wake.clone(),
    );
    let (tools, agent_config) = agent_deps_with_cancellation_acceleration(
        code_execution.clone(),
        foreground_web_search,
        web_extract,
        store.clone(),
        config.data_dir.clone(),
        host_folders.clone(),
        gateway.clone(),
        computer_use,
        foreground_browser,
        foreground_browser_semantic_actions,
        cancellation_acceleration,
    );
    let tools = Arc::new(tools);
    // The resolver, the /gateway routes, and MCP dispatch must share ONE
    // runtime, so it is injected at assembly rather than patched in after:
    // attestation contexts live in a per-instance registry (a second
    // instance splits a chat's inference tokens and MCP call bearers across
    // two contexts, and the gateway refuses every attested tools/call —
    // #1441), and refresh rotation is serialized per GatewayConnection
    // instance (two instances over the same keychain entry can race a stale
    // refresh token into the gateway's reuse detection, a spurious full
    // sign-out).
    let mut state = AppState::with_gateway_runtime(
        config,
        store,
        resolver,
        secrets,
        tools,
        agent_config,
        client_executor_id.unwrap_or_else(Uuid::new_v4),
        gateway,
        chatgpt,
        provisioned_policy,
        os_policy,
    )?
    .with_on_behalf_of_gateway(on_behalf_of_gateway);
    state.agent_run_wake = agent_run_wake;
    state.sandbox_attempts = sandbox_attempts;
    state.sandbox_steering = sandbox_steering;
    // Without a restart-stable native executor identity, durable
    // root-attachment mutations stay off — matching `AppState::new`.
    state.root_attachment_routes_enabled = client_executor_id.is_some();
    state.blobs = blobs;
    // The plugin management routes list what the provider actually loaded, so
    // they read the same instance staging and prompt composition use.
    state.code_execution = Some(code_execution.clone());
    // The bus is built with the app state, after the exec provider it belongs
    // to; handing it over here is what lets a first-run image pull reach the
    // chat that is waiting on it.
    code_execution.attach_event_bus(state.events.clone());
    if let Some(local_voice) = local_voice {
        state.set_local_voice_runner(local_voice);
    }
    // The host-folder surface local-app folder bindings dispatch through;
    // absent everywhere but the desktop, where folder bindings then refuse
    // to grant (docs/folder-bindings.md). The MCP runtime gets the same
    // handle so the create_app roster can list approved folders.
    if let Some(host) = &host_folders {
        state.mcp.set_host_folders(host.clone());
    }
    state.host_folders = host_folders;
    let runtime = code::CodeRuntime::new(
        db,
        state.config.data_dir.clone(),
        state.config.code_worktree_root_default.clone(),
        code_host_tool_broker,
        browser_runtime,
        browser_bridge_command,
        // On a gateway-authenticated hosted machine, git operations borrow
        // per-caller forge credentials from the same on-behalf-of handle the
        // router exchanges through (decision 63).
        state
            .on_behalf_of_gateway
            .clone()
            .map(|gateway| gateway as Arc<dyn obo_gateway::GitCredentialLender>),
        // And engine inference rides the caller's gateway grant through the
        // relay, since the hosted image has no provider credentials of its
        // own (decision 71).
        state
            .on_behalf_of_gateway
            .clone()
            .map(|gateway| Arc::new(code::harness_llm::HarnessLlmRelay::new(gateway))),
    );
    // A self-host machine owns its filesystem. Clones land under the data
    // directory unless an operator set a destination (decision 70).
    #[allow(unused_mut)]
    let mut runtime = if state.config.profile == Profile::SelfHost {
        runtime.with_clone_parent_default(state.config.data_dir.join("code").join("src"))
    } else {
        runtime
    };
    #[cfg(feature = "scripted-harness")]
    scripted_harness::install_from_env(&mut runtime.adapters)?;
    // Remote sessions need both halves: the configured runtime endpoint and
    // a gateway to mint owner-scoped tokens through. Half a configuration is
    // a boot error, not a silently local deployment.
    let runtime = match (
        state.config.runtime_endpoint.clone(),
        state.config.runtime_profile.clone(),
    ) {
        (Some(endpoint), Some(profile)) => {
            let Some(gateway) = state.on_behalf_of_gateway.clone() else {
                return Err(AgentError::config(
                    "TIDEBREAK_RUNTIME_ENDPOINT requires gateway authentication (TIDEBREAK_AUTH_GATEWAY_URL): sandboxes are provisioned as their owner",
                ));
            };
            let provisioner = code::remote::gateway::GatewayProvisioner::new(
                gateway.gateway_base_url(),
                &endpoint,
                gateway.runtime_tokens(&endpoint),
            )
            .map_err(|error| AgentError::config(format!("sandbox runtime client: {error}")))?;
            runtime.with_remote_sessions(code::remote::service::RemoteSessions::new(
                Arc::new(provisioner),
                code::remote::service::default_settings(profile),
            ))
        }
        _ => runtime,
    };
    let code = Arc::new(runtime);
    // Recovery runs after the bind, below: the workers it re-attaches need the
    // bound loopback address to reach their approval endpoint.
    state.code = Some(code.clone());
    // Before `initialize`: a boot-file or persisted replacement derives the
    // plugin slice in the same pass, so bundled servers come up with
    // everything else instead of after a second reconcile.
    state
        .mcp
        .set_plugin_catalog(Arc::new(plugin_mcp::InstalledPluginMcpCatalog::new(
            code_execution.clone(),
            state.store.clone(),
            state.config.plugin_data_dir(),
        )));
    state.mcp.initialize(mcp_servers).await?;
    // A no-op when `initialize` already derived the slice; the safety net for
    // the paths that return early (a managed profile ignoring its boot file).
    state.mcp.reconcile_plugin_servers().await;
    let token = state.token.clone();
    let client_executor_token = state.client_executor_token.clone();
    let local_import_token = state.local_import_token.clone();
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
        state.provisioned_policy.clone(),
        state.os_policy.clone(),
        state.approvals.clone(),
    )
    .with_on_behalf_of_gateway(state.on_behalf_of_gateway.clone());
    // Installed rather than constructed with the runtime: a recap runs on the
    // utility role, and the model handles that resolve it belong to the app
    // state the runtime is built before. See `code::recap`.
    code.install_recap(Arc::new(
        code::recap::TurnRecapper::new(
            code.db.clone(),
            code.bus.clone(),
            state.store.clone(),
            state.resolver.clone(),
            state.secrets.clone(),
            state.provisioned_policy.clone(),
            state.os_policy.clone(),
        )
        .with_on_behalf_of_gateway(state.on_behalf_of_gateway.clone()),
    ));
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
        state.provisioned_policy.clone(),
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
    .with_on_behalf_of_gateway(state.on_behalf_of_gateway.clone())
    .with_blobs(state.blobs.clone())
    .with_blob_write_locks(state.blob_writes.clone())
    .with_mcp_runtime(state.mcp.clone())
    .with_exec_folder_context(code_execution.clone())
    .with_diagnostics(state.diagnostics.clone());
    let sandbox_worker_config = sandbox_agent_run_worker::SandboxAgentRunWorkerConfig::default()
        .with_delegated_file_executor(client_executor_id.is_some());
    let sandbox_agent_run_worker = sandbox_agent_run_worker::SandboxAgentRunWorker::with_attempts(
        state.store.clone(),
        state.secrets.clone(),
        state.resolver.clone(),
        state.agent_run_wake.clone(),
        state.turn_job_wake.clone(),
        state.events.clone(),
        state.sandbox_attempts.clone(),
        state.agent_config.clone(),
        Some(state.config.data_dir.join("scratch")),
        Some(code_execution.clone()),
        sandbox_worker_config,
    );
    let sandbox_web_search_worker =
        sandbox_web_search_worker::SandboxWebSearchWorker::with_attempts(
            state.store.clone(),
            state.secrets.clone(),
            state.resolver.clone(),
            state.agent_config.model.clone(),
            state.agent_run_wake.clone(),
            state.sandbox_attempts.clone(),
            sandbox_web_search_worker::SandboxWebSearchWorkerConfig::default(),
        );
    let sandbox_task_plan_worker = sandbox_task_plan_worker::SandboxTaskPlanWorker::new(
        state.store.clone(),
        state.agent_run_wake.clone(),
        sandbox_task_plan_worker::SandboxTaskPlanWorkerConfig::default(),
    );
    let sandbox_exec_worker = sandbox_exec_worker::SandboxExecWorker::with_attempts(
        state.store.clone(),
        code_execution.clone(),
        state.agent_run_wake.clone(),
        state.sandbox_attempts.clone(),
        sandbox_exec_worker::SandboxExecWorkerConfig::default(),
    );
    let agent_run_scratch_reaper = agent_run_scratch_reaper::AgentRunScratchReaper::new(
        state.store.clone(),
        code_execution.clone(),
        state.config.data_dir.join("scratch"),
        agent_run_scratch_reaper::AgentRunScratchReaperConfig::default(),
    );
    let sandbox_container_run_worker = {
        let enabled = sandbox_container_admission.enabled();
        sandbox_container_run_worker::SandboxContainerRunWorker::new(
            state.store.clone(),
            sandbox_container_admission.backend,
            state.resolver.clone(),
            state.agent_run_wake.clone(),
            state.sandbox_steering.clone(),
            enabled,
            sandbox_container_run::SandboxContainerRunConfig::default(),
            sandbox_container_run_worker::SandboxContainerRunWorkerConfig::default(),
        )
    };

    // Queued-message promotion: a light sweep, wake-driven with a slow floor.
    // Try-based on the idempotent turn acceptance, so it needs no lease of its
    // own — see `routes::promote_queued_turns`.
    let queued_turn_promoter = {
        let state = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(750));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if let Err(error) = routes::promote_queued_turns(&state).await {
                    eprintln!("tidebreak: queued-turn promotion failed: {error:?}");
                }
            }
        })
    };
    drop(queued_turn_promoter);
    let server_store = state.store.clone();
    let data_dir = state.config.data_dir.clone();
    let mcp_runtime = state.mcp.clone();
    let gateway_runtime = state.gateway.clone();
    let router = app(state);

    let listener = TcpListener::bind(bind_addr)
        .await
        .map_err(|e| AgentError::config(format!("failed to bind {bind_addr}: {e}")))?;
    let local_addr = listener
        .local_addr()
        .map_err(|e| AgentError::config(format!("no local address: {e}")))?;
    // Publish the loopback base and take the code-mode recovery pass, then run
    // it in the background. It has to come after the bind — a session
    // re-attached before the address is known comes back with no approval
    // endpoint — and it should not gate launch, because it relaunches an
    // engine child per restored session. The trade is a brief window where a
    // restored session is listed but its worker is not attached yet, and a
    // turn submitted into it is refused with `session_worker_missing`; before,
    // the same wait was spent with the port closed and the app unusable.
    let code_recovery = code.start(format!("http://{local_addr}"));
    tokio::spawn(async move {
        if let Err(error) = code_recovery.await {
            tracing::warn!("code-mode recovery: {}", error.message());
        }
    });
    // After the accept address exists so first paint is not competing with
    // pip or host-tool downloads. Built-in plugins still start warming on
    // the first open; nothing waits on this pass.
    code_execution.spawn_dependency_provisioning();
    // Publish before workers start answering so an attach racing boot sees a
    // file that matches the bound address. It carries the primary bearer and
    // the narrow local-import capability, never the executor credential. See
    // decisions 0009 and 0016.
    let listen_endpoint = listen_endpoint::ListenEndpointGuard::publish(
        data_dir,
        &format!("http://{local_addr}"),
        token.as_ref(),
        local_import_token.as_ref(),
    )?;

    let turn_worker = tokio::spawn(turn_worker.run());
    let sandbox_agent_run_worker = tokio::spawn(sandbox_agent_run_worker.run());
    let sandbox_container_run_worker =
        sandbox_container_run_worker.map(|worker| tokio::spawn(worker.run()));
    let sandbox_web_search_worker = tokio::spawn(sandbox_web_search_worker.run());
    let sandbox_task_plan_worker = tokio::spawn(sandbox_task_plan_worker.run());
    let sandbox_exec_worker = tokio::spawn(sandbox_exec_worker.run());
    let agent_run_scratch_reaper = tokio::spawn(agent_run_scratch_reaper.run());
    let blob_retirement_worker = tokio::spawn(blob_retirement_worker.run());
    let blob_orphan_auditor = tokio::spawn(blob_orphan_auditor.run());
    let approval_judge_worker = tokio::spawn(approval_judge_worker.run());
    let mcp_supervisor = tokio::spawn(mcp_runtime.clone().supervise());
    let gateway_model_sync = tokio::spawn(
        gateway_runtime
            .clone()
            .sync_models_periodically(mcp_runtime.clone()),
    );

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
        _sandbox_task_plan_worker: AbortTask(sandbox_task_plan_worker),
        _sandbox_exec_worker: AbortTask(sandbox_exec_worker),
        _agent_run_scratch_reaper: AbortTask(agent_run_scratch_reaper),
        _blob_retirement_worker: AbortTask(blob_retirement_worker),
        _blob_orphan_auditor: AbortTask(blob_orphan_auditor),
        _approval_judge_worker: AbortTask(approval_judge_worker),
        _mcp_supervisor: AbortTask(mcp_supervisor),
        _gateway_model_sync: AbortTask(gateway_model_sync),
        _instance_lock: instance_lock,
        _listen_endpoint: listen_endpoint,
    })
}

/// Assemble the tools and per-turn tuning for a real launch.
///
/// The model **provider** is not built here — it is resolved per turn by the
/// [`KeyedResolver`] (a composite router over enabled providers; see
/// [`resolver`]), so configuring a provider at runtime takes effect without a
/// restart. The model *name* comes from `TIDEBREAK_MODEL` (or the built-in
/// default) and can be overridden at runtime via `PUT /settings` or per-chat.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn agent_deps(
    code_execution: Arc<dyn tidebreak_code_execution::ExecProvider>,
    web_search: Box<dyn Tool>,
    web_extract: Box<dyn Tool>,
    source_store: Arc<dyn Store>,
    profile_data_dir: std::path::PathBuf,
    host_folders: Option<Arc<dyn host_folders::HostFolders>>,
    gateway: Arc<gateway_runtime::GatewayRuntime>,
    // Whether the computer-use tools register at all: desktop profile on
    // macOS, decided by the caller. When false the contracts stay
    // unregistered, so no turn surface can advertise or checkpoint them.
    computer_use: bool,
    // Whether the foreground browser observation tools register. Always
    // false in production until a desktop foreground browser executor
    // explicitly opts in. The code-mode BrowserRuntime is not sufficient.
    foreground_browser: bool,
    // Whether the installed foreground browser runtime can synthesize trusted
    // semantic input. This flag has no effect unless foreground_browser is on.
    foreground_browser_semantic_actions: bool,
) -> (ToolRegistry, AgentConfig) {
    let cancellation_acceleration = agent_control_tools::SandboxCancellationAcceleration::new(
        source_store.clone(),
        Arc::new(state::SandboxAttemptGuard::default()),
        Arc::new(state::SandboxSteerGuard::default()),
        Arc::new(tokio::sync::Notify::new()),
    );
    agent_deps_with_cancellation_acceleration(
        code_execution,
        web_search,
        web_extract,
        source_store,
        profile_data_dir,
        host_folders,
        gateway,
        computer_use,
        foreground_browser,
        foreground_browser_semantic_actions,
        cancellation_acceleration,
    )
}

#[allow(clippy::too_many_arguments)]
fn agent_deps_with_cancellation_acceleration(
    code_execution: Arc<dyn tidebreak_code_execution::ExecProvider>,
    web_search: Box<dyn Tool>,
    web_extract: Box<dyn Tool>,
    source_store: Arc<dyn Store>,
    profile_data_dir: std::path::PathBuf,
    host_folders: Option<Arc<dyn host_folders::HostFolders>>,
    gateway: Arc<gateway_runtime::GatewayRuntime>,
    computer_use: bool,
    foreground_browser: bool,
    foreground_browser_semantic_actions: bool,
    cancellation_acceleration: agent_control_tools::SandboxCancellationAcceleration,
) -> (ToolRegistry, AgentConfig) {
    /// The host-folder seam folded into `create_app`'s authoring-time folder
    /// lookup: approved roots as (id, name) pairs, empty on any error —
    /// legibility, never the gate.
    struct HostFolderAuthoringSource(Arc<dyn host_folders::HostFolders>);

    #[async_trait::async_trait]
    impl tidebreak_core::local_app::ApprovedFolderSource for HostFolderAuthoringSource {
        async fn approved_folders(&self) -> Vec<(tidebreak_core::id::HostRootId, String)> {
            self.0
                .approved_roots()
                .await
                .map(|folders| {
                    folders
                        .into_iter()
                        .map(|folder| (folder.root_id, folder.display_name))
                        .collect()
                })
                .unwrap_or_default()
        }
    }

    /// The gateway session folded into `create_app`'s authoring-time gateway
    /// lookup, over the same live reads the roster and the consent surface
    /// use. `None` — no session, or a gateway that cannot answer — is what
    /// makes the door refuse gateway bindings with a sentence the user can
    /// act on.
    struct GatewayAuthoringSource(Arc<gateway_runtime::GatewayRuntime>);

    #[async_trait::async_trait]
    impl tidebreak_core::local_app::GatewayAppSource for GatewayAuthoringSource {
        async fn entitled_apps(
            &self,
        ) -> Option<Vec<tidebreak_core::local_app::GatewayAuthoringApp>> {
            Some(
                self.0
                    .app_roster()
                    .await?
                    .into_iter()
                    .map(|app| tidebreak_core::local_app::GatewayAuthoringApp {
                        id: app.id,
                        name: app.name,
                        operation_ids: app.operation_ids,
                    })
                    .collect(),
            )
        }
    }

    let create_app = {
        let tool = CreateAppTool::new(source_store.clone(), profile_data_dir)
            .with_gateway_apps(Arc::new(GatewayAuthoringSource(gateway)));
        match host_folders {
            Some(host) => tool.with_approved_folders(Arc::new(HostFolderAuthoringSource(host))),
            None => tool,
        }
    };
    let mut tools = ToolRegistry::new()
        .with(Box::new(ReadFile))
        .with(Box::new(ListDir))
        .with(Box::new(WriteFile))
        .with(Box::new(ExecTool::new(code_execution)))
        .with(Box::new(source_tools::ListDocumentsTool::new(
            source_store.clone(),
        )))
        .with(Box::new(source_tools::ReadDocumentTool::new(
            source_store.clone(),
        )))
        .with(Box::new(source_tools::ReadToolResultTool::new(
            source_store.clone(),
        )))
        .with(Box::new(create_app))
        .with(Box::new(agent_control_tools::ResumeAgentTool::new(
            source_store.clone(),
        )))
        .with(Box::new(
            agent_control_tools::CancelAgentTool::new(source_store.clone())
                .with_cancellation_acceleration(cancellation_acceleration),
        ))
        .with(Box::new(task_plan_tool::UpdateTaskPlanTool::new(
            source_store.clone(),
        )))
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
    if computer_use {
        register_computer_use_tools(&mut tools);
    }
    if foreground_browser {
        register_foreground_browser_tools(&mut tools, foreground_browser_semantic_actions);
    }
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
    let model = boot_default_model();
    let agent_config = AgentConfig {
        model,
        ..AgentConfig::default()
    };
    (tools, agent_config)
}

/// The model this process launched with.
///
/// Read in two places — the boot agent config, and the web-search resolver's
/// last fallback when neither a chat nor the global `chat` role names a model —
/// so it lives in one.
fn boot_default_model() -> String {
    std::env::var("TIDEBREAK_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
}
/// Register the computer-use contracts as validated client tools.
///
/// Registered only when the caller determined the host can honor them (the
/// desktop profile on macOS). The tools are client-executed: the server parks
/// each call for the desktop to claim, the desktop authorizes against the host
/// broker's per-app grants, and the broker performs the work. Reads and capture
/// are `ReadOnly` — once their broker grant exists they never card per call,
/// and plan mode keeps them. The acting tools are `Sensitive`; they resolve to
/// [`tidebreak_core::ToolApprovalKind::ComputerMayControlApp`] through
/// `ToolApprovalKind::for_tool_name`, so plan mode refuses them and a control
/// call cards once per app instead of per action.
fn register_computer_use_tools(tools: &mut ToolRegistry) {
    // Pure reads and capture are ReadOnly — once their broker grant exists
    // they never card per call, and plan mode keeps them.
    for (spec, validate) in [
        (
            computer_list_windows_tool_spec(),
            validate_computer_list_windows_arguments as fn(&serde_json::Value) -> bool,
        ),
        (
            computer_capture_screen_tool_spec(),
            validate_computer_capture_screen_arguments,
        ),
        (
            computer_read_app_content_tool_spec(),
            validate_computer_read_app_content_arguments,
        ),
        (
            computer_return_to_tidebreak_tool_spec(),
            validate_computer_return_to_tidebreak_arguments,
        ),
        (computer_wait_tool_spec(), validate_computer_wait_arguments),
    ] {
        tools.register_validated_client(spec, ApprovalClass::ReadOnly, validate);
    }
    // The acting tools are Sensitive, resolving to `ComputerMayControlApp`
    // through `ToolApprovalKind::for_tool_name`. Scroll and focus act too —
    // they synthesize input, warp the cursor, and raise windows — so they are
    // not read-only and plan mode refuses them.
    for (spec, validate) in [
        (
            computer_click_tool_spec(),
            validate_computer_click_arguments as fn(&serde_json::Value) -> bool,
        ),
        (
            computer_type_text_tool_spec(),
            validate_computer_type_text_arguments,
        ),
        (
            computer_key_press_tool_spec(),
            validate_computer_key_press_arguments,
        ),
        (
            computer_scroll_tool_spec(),
            validate_computer_scroll_arguments,
        ),
        (
            computer_focus_window_tool_spec(),
            validate_computer_focus_window_arguments,
        ),
    ] {
        tools.register_validated_client(spec, ApprovalClass::Sensitive, validate);
    }
}

/// Register exactly five observation browser tools as validated client tools.
///
/// The server checkpoints each call for the desktop foreground browser
/// executor to claim, authorize, and dispatch through [`BrowserRegistry`].
/// The server itself never drives a browser — it only validates arguments
/// and parks the call. No semantic act tool is registered; that requires
/// native input synthesis and is gated on a separate capability check.
///
/// Registered only when an explicit foreground-browser availability flag is
/// true (the desktop can bind a browser surface for foreground chat). When
/// absent — the default in every production binding until the foreground
/// executor opts in — no browser tools are advertised, and no model surface
/// can see or checkpoint them. The code-mode browser runtime is not sufficient
/// to enable this gate.
fn register_foreground_browser_tools(tools: &mut ToolRegistry, semantic_actions: bool) {
    // Observation reads: list, snapshot, wait, screenshot. They never mutate
    // the workspace or escape the existing browser capability scope. Plan mode
    // keeps them.
    for (spec, validate) in [
        (
            browser_list_tool_spec(),
            validate_browser_list_arguments as fn(&serde_json::Value) -> bool,
        ),
        (
            browser_snapshot_tool_spec(),
            validate_browser_snapshot_arguments as fn(&serde_json::Value) -> bool,
        ),
        (
            browser_wait_tool_spec(),
            validate_browser_wait_arguments as fn(&serde_json::Value) -> bool,
        ),
        (
            browser_screenshot_tool_spec(),
            validate_browser_screenshot_arguments as fn(&serde_json::Value) -> bool,
        ),
    ] {
        tools.register_validated_client(spec, ApprovalClass::ReadOnly, validate);
    }
    // Navigate can cross origins and must use the existing sensitive-tool
    // posture until native browser consent reauthorizes it. Plan mode
    // refuses it.
    tools.register_validated_client(
        browser_navigate_tool_spec(),
        ApprovalClass::Sensitive,
        validate_browser_navigate_arguments,
    );
    if semantic_actions {
        tools.register_validated_client(
            browser_act_tool_spec(),
            ApprovalClass::Sensitive,
            validate_browser_act_arguments,
        );
    }
}

/// Open the durable store the profile selects.
///
/// Desktop opens SQLite under `data_dir`. Self-host opens the database named
/// by `TIDEBREAK_DATABASE_URL` (PostgreSQL; build with the `postgres` feature
/// to compile the driver in) — and only after the profile's principal-naming
/// authenticator loads. A shared store must never open behind an API that
/// cannot tell its callers apart, so a self-host config without a valid
/// token file refuses to boot here, before the store exists, on every path
/// that opens it (#853).
async fn connect_store(config: &Config) -> Result<Arc<dyn Store>> {
    Ok(connect_db(config).await?)
}

async fn connect_db(config: &Config) -> Result<Arc<DbStore>> {
    match config.profile {
        Profile::Desktop => {
            std::fs::create_dir_all(&config.data_dir)
                .map_err(|e| AgentError::config(format!("failed to create data dir: {e}")))?;
            let store = desktop_schema::connect(config).await?;
            Ok(Arc::new(store))
        }
        Profile::SelfHost => {
            // Validate and discard: boot proves a principal-naming
            // authenticator is configured before the shared store exists.
            // State assembly constructs the live verifier used by requests.
            auth::PrincipalAuthenticator::from_config(config)?;
            let store = tidebreak_core::DbStore::connect_with_options(host_connect_options(
                &config.database_url()?,
            ))
            .await?;
            Ok(Arc::new(store))
        }
        // An unknown future profile has chosen neither a store nor an
        // authenticator; fail closed rather than defaulting to either.
        _ => Err(AgentError::config(
            "no store backend is wired for this profile",
        )),
    }
}

/// How many pooled connections a host process opens.
///
/// Constraint: SeaORM gives a SQLite pool one connection unless it is told
/// otherwise, so a single slow write blocks every other request in the
/// process — including the four sequential store calls that create a
/// workspace, which is how a ~0.3s `git worktree add` turned into minutes
/// (#2316). WAL lets readers run beside the writer, so a modest pool is
/// enough for interactive requests to pass a stalled one. Keep it fixed: this
/// is a floor for responsiveness, not a tuning surface.
const HOST_MAX_CONNECTIONS: u32 = 8;

/// How long a request waits for a pooled connection before it fails.
///
/// Explicit rather than inherited: waits at exactly sqlx's 30s default are
/// what the starved pool looked like in the logs, and a caller that has
/// waited this long is better served by an error it can report than by a
/// dialog that never resolves.
const HOST_ACQUIRE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Build the connect options every host store opens with.
pub(crate) fn host_connect_options(url: &str) -> sea_orm::ConnectOptions {
    let mut options = sea_orm::ConnectOptions::new(url.to_owned());
    options
        .max_connections(HOST_MAX_CONNECTIONS)
        // Keep one connection warm so an idle profile does not pay reconnect
        // latency on the first interactive request.
        .min_connections(1)
        .acquire_timeout(HOST_ACQUIRE_TIMEOUT);
    options
}

#[cfg(test)]
mod tests;
