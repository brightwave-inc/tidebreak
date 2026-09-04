//! Tidebreak's in-process HTTP and WebSocket route surface.

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Request};
use axum::http::{header, Method};
use axum::routing::{delete, get, patch, post};
use axum::Router;
use tidebreak_core::{Config, Result};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use uuid::Uuid;

use tidebreak_server_core as core;

pub(crate) use core::{
    agent_control_tools, agent_run_scratch_reaper, approvals, auth, chat_titling, chatgpt_runtime,
    code, connected_apps, diagnostics, document_decode, engine, error, event_projection,
    exec_write_snapshot, extract, gateway_drafts, gateway_runtime, image_attachment,
    managed_policy, mcp_config, mcp_curated, memory_sweep, model_registry, model_roles,
    obo_gateway, openapi_discovery, plugin_install, plugin_state, principal, providers,
    runtime_settings, scoped_memory, scoped_store, state, ui_bundle, view_frames, workspace_config,
};
#[cfg(test)]
pub(crate) use core::{
    agent_deps, bus, configured_blob_store, connect_store, desktop_connect_options,
    host_connect_options, memory_capture, memory_tool, plugin_mcp, provider, resolver, retry,
    sandbox_agent_run_worker, sandbox_runtime, sandbox_task_plan_worker, scripted_harness,
    task_plan_tool, InstanceLock, HOST_MAX_CONNECTIONS,
};
pub use core::{
    code_execution, connectors, consent, deprovision_provisioned_gateway, deprovision_target,
    ensure_home_dir, host_folders, listen_endpoint, logging, media_type, openapi_catalog,
    output_files, register_pending_pairing, register_replacing_pairing, rehome_configured_secrets,
    rest_executor, sandbox_container_run, sandbox_docker, secret_rehome, voice_transcription,
    web_search, AppState, BrowserChannelBinding, BrowserRuntime, BrowserRuntimeError,
    BrowserRuntimeScope, DeprovisionTarget, DurableOperationStore, LocalVoiceError,
    LocalVoiceRunner, LocalVoiceState, LocalVoiceStatus, PairingError, PairingHandle,
    PendingRegistration, Server, ServerError, UpdateQuiesce,
};

pub mod routes;
#[cfg(test)]
mod tests;
pub mod wire;
#[cfg(test)]
mod wire_code_fixtures;
mod wire_types;

pub(crate) const MAX_RAW_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_WEB_SEARCH_CREDENTIAL_BODY_BYTES: usize = 16 * 1024;
const MAX_CODE_EXECUTION_CONFIG_BODY_BYTES: usize = 1_024;
const MAX_CODE_EXECUTION_CREDENTIAL_BODY_BYTES: usize = 16 * 1024;
const MAX_EXTERNAL_CONNECT_BODY_BYTES: usize = 16 * 1024;

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
        )
        .route(
            "/sessions/{id}/attachments/images",
            post(routes::code::publish_session_image)
                .layer(DefaultBodyLimit::max(routes::MAX_IMAGE_ATTACHMENT_BYTES)),
        );

    let machine_client_executor_api = Router::new()
        .route(
            "/native/client-executions/pending",
            get(routes::list_all_pending_client_executions),
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
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_client_executor_token,
        ))
        .with_state(state.clone());

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
        .route(
            "/connected-apps/rest/spec-discovery",
            post(routes::post_rest_spec_discovery),
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
            "/code/harnesses/check-updates",
            post(routes::code::check_harness_updates),
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
        // A machine session's own git borrows the person's forge credential
        // here, under the same key; its body is a few description lines.
        .route(
            crate::code::harness_llm::GIT_CREDENTIAL_PATH,
            post(routes::code::harness_git_credential).layer(DefaultBodyLimit::max(
                routes::code::MAX_GIT_CREDENTIAL_BODY_BYTES,
            )),
        )
        .with_state(state.clone());

    // The channel-adapter surface (docs/slack-sessions.md, stage 2).
    // Authenticated per request by adapter grant tokens, so it stays outside
    // `require_token` like the inference relay above.
    let external_adapter_api = Router::new()
        .route(
            "/external/code/sessions",
            post(routes::code::external_get_or_create),
        )
        .route(
            "/external/code/sessions/{id}/messages",
            post(routes::code::external_messages),
        )
        .route(
            "/external/code/sessions/{id}/events",
            get(routes::code::external_events),
        )
        .route(
            "/external/code/sessions/{id}/interrupt",
            post(routes::code::external_interrupt),
        )
        .route(
            "/external/code/sessions/{id}/reap",
            post(routes::code::external_reap),
        )
        .route(
            "/external/grants/rotate",
            post(routes::code::external_rotate),
        )
        // The connect bootstrap: start requires the deployment's narrow
        // adapter service bearer. Status and completion require the separate
        // per-handshake confirmation capability returned only to that caller.
        // The owner's view and approve stay on the authenticated API.
        .route(
            "/external/connect",
            post(routes::code::connect_start)
                .layer(RequestBodyLimitLayer::new(MAX_EXTERNAL_CONNECT_BODY_BYTES)),
        )
        // The operator's pairing probe: bootstrap-authenticated, no writes.
        .route(
            "/external/connect/probe",
            axum::routing::get(routes::code::connect_probe),
        )
        .route(
            "/external/connect/{nonce}/status",
            get(routes::code::connect_status),
        )
        .route(
            "/external/connect/{nonce}/complete",
            post(routes::code::connect_complete),
        )
        .with_state(state.clone());

    let api = Router::new()
        .route("/settings", get(routes::get_settings))
        .route("/workspace-config", get(routes::export_workspace_config))
        .route(
            "/workspace-config/preview",
            post(routes::preview_workspace_config)
                .layer(DefaultBodyLimit::max(mcp_config::MAX_CONFIG_BODY_BYTES)),
        )
        .route(
            "/workspace-config/apply",
            post(routes::apply_workspace_config)
                .layer(DefaultBodyLimit::max(mcp_config::MAX_CONFIG_BODY_BYTES)),
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
        .route("/memory/capabilities", get(routes::capabilities))
        .route(
            "/memory/records",
            get(routes::list_records).post(routes::create_record),
        )
        .route(
            "/memory/records/{id}",
            get(routes::get_record)
                .patch(routes::update_record)
                .delete(routes::delete_record),
        )
        .route(
            "/memory/records/{id}/status",
            axum::routing::put(routes::set_record_status),
        )
        .route("/memory/records/{id}/revisions", get(routes::revisions))
        .route("/memory/search", get(routes::search))
        .route("/memory/digest", get(routes::digest))
        .route("/memory/sweep", get(routes::sweep_status))
        .route(
            "/memory/ingest",
            post(routes::ingest).layer(DefaultBodyLimit::max(routes::MAX_MEMORY_BODY_BYTES)),
        )
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
            "/sessions",
            post(routes::code::create_internal_session).get(routes::code::list_internal_sessions),
        )
        .route("/sessions/{id}", get(routes::code::get_session))
        .route(
            "/sessions/{id}/turns",
            post(routes::code::submit_turn).get(routes::code::list_session_turns),
        )
        .route(
            "/sessions/{id}/queued",
            get(routes::code::list_queued_turns),
        )
        .route(
            "/sessions/{id}/queued/{queued_id}",
            axum::routing::patch(routes::code::patch_queued_turn)
                .delete(routes::code::delete_queued_turn),
        )
        .route(
            "/sessions/{id}/queue-paused",
            axum::routing::put(routes::code::put_queue_paused),
        )
        .route(
            "/sessions/{id}/queued/send-now",
            post(routes::code::post_queue_send_now),
        )
        .route(
            "/sessions/{id}/attachments/images/{blob_id}",
            get(routes::code::get_session_image),
        )
        .route("/sessions/{id}/steer", post(routes::code::steer_session))
        .route(
            "/sessions/{id}/interrupt",
            post(routes::code::interrupt_session),
        )
        .route("/sessions/{id}/reap", post(routes::code::reap_session))
        .route(
            "/sessions/{id}/mode",
            post(routes::code::set_session_permission_mode),
        )
        .route(
            "/sessions/{id}/effort",
            post(routes::code::set_session_reasoning_effort),
        )
        .route(
            "/sessions/{id}/fast-mode",
            post(routes::code::set_session_fast_mode),
        )
        .route("/sessions/{id}/fork", post(routes::code::fork_session))
        .route("/sessions/{id}/debug", get(routes::code::get_session_debug))
        .route(
            "/sessions/{id}/attention",
            post(routes::code::set_attention),
        )
        .route("/sessions/{id}/events", get(routes::code::session_events))
        .route("/updates", get(routes::code::code_updates))
        .route("/approvals", get(routes::code::list_approvals))
        .route(
            "/approvals/{id}/decision",
            post(routes::code::decide_approval),
        )
        .route("/code/grants", get(routes::code::list_grants))
        .route("/code/grants/{id}/revoke", post(routes::code::revoke_grant))
        .route(
            "/code/grants/revoke-workspace",
            post(routes::code::revoke_workspace_grants),
        )
        .route("/external/connect/{nonce}", get(routes::code::connect_view))
        .route(
            "/external/connect/{nonce}/approve",
            post(routes::code::connect_approve),
        )
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
        .route(
            "/code/workspace-title",
            post(routes::code::propose_workspace_title),
        )
        .route(
            "/code/remote/workspaces",
            post(routes::code::create_remote_workspace),
        )
        .route(
            "/code/remote/workspaces/{id}/sessions",
            post(routes::code::create_remote_session),
        )
        .route(
            "/code/workspaces/{id}",
            get(routes::code::get_workspace).patch(routes::code::patch_workspace),
        )
        .route(
            "/code/workspaces/{id}/archive",
            post(routes::code::archive_workspace),
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
            "/code/sessions",
            post(routes::code::create_internal_session).get(routes::code::list_internal_sessions),
        )
        .route("/code/sessions/{id}", get(routes::code::get_session))
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
        .merge(harness_llm_api)
        .merge(external_adapter_api);
    let frame_state = state.clone();
    // Public like discovery, and for the same reason: a page has to reach
    // both before it holds a bearer. The handoff route answers only on a
    // gateway-authenticated machine.
    let auth_discovery = Router::new()
        .route("/auth/discovery", get(auth::discovery))
        .route("/auth/handoff", get(auth::handoff))
        .route("/auth/oidc/start", get(auth::oidc_start))
        .route("/auth/oidc/callback", get(auth::oidc_callback))
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

    let root = Router::new()
        .merge(view_frames)
        .merge(auth_discovery)
        .merge(machine_client_executor_api)
        .merge(api);
    // The renderer bundle, when this machine carries one, answers what no
    // route claimed. It sits with the API inside the origin and CORS layers
    // and outside `require_token`: a page has to load before it can hold a
    // bearer. Without a bundle the fallback stays axum's own `404`.
    let root = match state.config.ui_dist.clone() {
        Some(dist) => {
            let dist = Arc::new(dist);
            root.fallback(move |request: Request| {
                let dist = dist.clone();
                async move { ui_bundle::serve(dist, request).await }
            })
        }
        None => root,
    };
    root
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

fn queued_turn_promoter(
    state: AppState,
    floor: std::time::Duration,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    Box::pin(routes::run_queued_turn_promoter(state, floor))
}

#[doc(hidden)]
pub fn route_runtime() -> core::RouteRuntime {
    core::RouteRuntime::new(app, queued_turn_promoter)
}

pub async fn bind(config: Config) -> Result<Server> {
    core::bind(config, route_runtime()).await
}

pub async fn bind_configured(config: Config) -> Result<Server> {
    core::bind_configured(config, route_runtime()).await
}

pub async fn bind_with_desktop_executor(
    config: Config,
    client_executor_id: Uuid,
) -> Result<Server> {
    core::bind_with_desktop_executor(config, client_executor_id, route_runtime()).await
}

pub async fn bind_configured_with_desktop_executor(
    config: Config,
    client_executor_id: Uuid,
) -> Result<Server> {
    core::bind_configured_with_desktop_executor(config, client_executor_id, route_runtime()).await
}

pub async fn bind_configured_with_desktop_executor_and_folder_grants(
    config: Config,
    client_executor_id: Uuid,
    folder_grant_resolver: Arc<dyn code_execution::ExecFolderGrantResolver>,
    office_converter: Option<Arc<dyn tidebreak_code_execution::HostOfficeConverter>>,
    host_tool_broker: Option<Arc<dyn tidebreak_code_execution::HostToolBroker>>,
    local_voice: Option<Arc<dyn LocalVoiceRunner>>,
    host_folders: Option<Arc<dyn host_folders::HostFolders>>,
) -> Result<Server> {
    core::bind_configured_with_desktop_executor_and_folder_grants(
        config,
        client_executor_id,
        folder_grant_resolver,
        office_converter,
        host_tool_broker,
        local_voice,
        host_folders,
        route_runtime(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn bind_configured_with_desktop_executor_and_folder_grants_and_browser_runtime(
    config: Config,
    client_executor_id: Uuid,
    folder_grant_resolver: Arc<dyn code_execution::ExecFolderGrantResolver>,
    office_converter: Option<Arc<dyn tidebreak_code_execution::HostOfficeConverter>>,
    host_tool_broker: Option<Arc<dyn tidebreak_code_execution::HostToolBroker>>,
    local_voice: Option<Arc<dyn LocalVoiceRunner>>,
    host_folders: Option<Arc<dyn host_folders::HostFolders>>,
    browser_runtime: Option<Arc<dyn BrowserRuntime>>,
) -> Result<Server> {
    core::bind_configured_with_desktop_executor_and_folder_grants_and_browser_runtime(
        config,
        client_executor_id,
        folder_grant_resolver,
        office_converter,
        host_tool_broker,
        local_voice,
        host_folders,
        browser_runtime,
        route_runtime(),
    )
    .await
}

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
    core::bind_configured_with_desktop_executor_and_folder_grants_and_browser_binding(
        config,
        client_executor_id,
        folder_grant_resolver,
        office_converter,
        host_tool_broker,
        local_voice,
        host_folders,
        binding,
        route_runtime(),
    )
    .await
}

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
    core::bind_configured_with_desktop_foreground_browser_executor(
        config,
        client_executor_id,
        folder_grant_resolver,
        office_converter,
        host_tool_broker,
        local_voice,
        host_folders,
        binding,
        route_runtime(),
    )
    .await
}
