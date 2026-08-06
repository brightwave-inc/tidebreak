//! HTTP and WebSocket route handlers.
//!
//! Document lifecycle and search handlers live in the dedicated `document`
//! submodule; settings, providers, projects, chats, and event streaming remain
//! here.

use axum::body::Body;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use cap_std::ambient_authority;
use cap_std::fs::Dir;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::Path as FsPath;
use tokio::sync::broadcast::error::RecvError;

use openwave_core::id::{AppId, AppRevisionId};
use openwave_core::local_app::app_revision_relative_path;
use openwave_core::{
    AcceptTurnOutcome, AcceptTurnSteerOutcome, AgentError, AgentEvent, AgentRun,
    AgentRunExecutionLocation, AgentRunStatus, AgentRunTier, ApprovalDecision, CallId, Chat,
    ChatId, DeleteChatOutcome, DeleteProjectOutcome, DocumentId, Message as StoredMessage,
    MessageId, PermissionMode, Project, ProjectId, ReasoningEffort,
    RequestAgentRunCancellationOutcome, RequestTurnCancellationOutcome, Role, SandboxToolCall,
    SandboxToolCallStatus, SecretProvider, SequencedEvent, Store, ToolCallExecution,
    ToolCallRecord, ToolCallStatus, TurnId, TurnSteer, TurnSteerId,
};

use crate::auth::{offered_handshake_subprotocol, WS_HANDSHAKE_SUBPROTOCOL};
use crate::code_execution::{
    self, CodeExecutionConfigInfo, CodeExecutionConfigUpdate, CodeExecutionCredentialReadiness,
    CodeExecutionCredentialsInfo,
};
use crate::error::ServerError;
use crate::event_projection::{RendererChatFrame, RendererChatMetadata, RendererSequencedEvent};
use crate::exec_write_snapshot::{
    list_file_change_summaries, render_file_change_preview, undo_one_file_change,
    undo_turn_file_changes, ExecFileChangeSummary, ExecFilePreviewError, ExecFilePreviewRequest,
    ExecFilePreviewRevision, ExecFileUndoOutcome, ExecTurnUndoOutcome,
};
use crate::extract::{Json, Path, Query};
use crate::mcp_config::{McpServersConfig, McpServersInfo};
use crate::model_roles::{self, ModelRole};
use crate::providers::{self, ProviderCredential, ProviderInfo, ProviderKind, ProviderUpdate};
use crate::scoped_store::ScopedStore;
use crate::state::{AppState, SandboxSteerRefusal};
use crate::view_frames::ViewFrameSource;
use crate::voice_transcription::{self, VoiceTranscriptionInfo, VoiceTranscriptionUpdate};
use crate::web_search::{
    self, WebSearchConfigInfo, WebSearchConfigUpdate, WebSearchCredentialReadiness,
    WebSearchCredentialsInfo,
};

pub(crate) const MAX_ACTIVE_BACKGROUND_AGENTS_SETTING: &str = "agents.max_active_background_agents";

mod app_grant;
mod app_invoke;
mod app_library;
pub(crate) mod client_execution;
mod connected_apps;
mod delegated_file_execution;
mod document;
pub(crate) mod image_attachment;
mod inbox;
mod plans;
mod plugins;
mod root_attachment;
mod user_questions;
pub use app_grant::*;
pub use app_invoke::*;
pub use app_library::*;
pub use client_execution::*;
pub use connected_apps::*;
pub use delegated_file_execution::*;
pub use document::*;
pub use image_attachment::*;
pub use inbox::*;
pub use plans::*;
pub use plugins::*;
pub use root_attachment::*;
pub use user_questions::*;

/// The policy every stored-bytes response carries.
///
/// Both byte-serving routes hand back content that originated outside OpenWave
/// — a reader's file, or an image an agent produced — from the API's own
/// origin, so a response a browser ever renders must be unable to reach back
/// into that origin. `sandbox` drops the response into an opaque origin with
/// scripting off, and `default-src 'none'` denies it every subresource and
/// every outbound request.
///
/// It is shared rather than duplicated so the two routes cannot drift into
/// serving comparable bytes under different rules.
pub(crate) const SERVED_BYTES_CONTENT_POLICY: &str =
    "default-src 'none'; sandbox; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

/// `GET /mcp/servers` — renderer-safe definitions and current connection health.
pub async fn get_mcp_servers(
    State(state): State<AppState>,
) -> Result<Json<McpServersInfo>, ServerError> {
    Ok(Json(state.mcp.info().await))
}

/// `PUT /mcp/servers` — atomically validate, connect, persist, and publish a
/// complete replacement set. A failed candidate never changes active tools.
///
/// On a managed profile the manual transports are locked: a body that adds or
/// edits a `command` or `url` server is refused with the same stable
/// `managed_profile` kind the provider lockdown uses, before anything is
/// validated or connected — so a refused write leaves the configuration
/// exactly as it was. Gateway-endpoint mounts remain the sanctioned path, and
/// manual servers already on file may still ride along a save unchanged (they
/// run forced-disabled) or be removed. An org that deploys the
/// `AllowLocalMcpServers` policy key narrows the lockdown to remote (`url`)
/// servers, leaving local stdio servers to the user.
pub async fn put_mcp_servers(
    State(state): State<AppState>,
    Json(body): Json<McpServersConfig>,
) -> Result<Json<McpServersInfo>, ServerError> {
    // Resolved outside the runtime's mutation lock: a policy that flips
    // between here and the commit skips the admission check, but the commit
    // itself re-reads the lockdown under that lock, so such a definition
    // persists inert and never connects. The residue is a millisecond-wide
    // cosmetic entry in durable config, not an execution bypass.
    let policy = crate::managed_policy::resolve(&*state.store, &*state.os_policy).await?;
    // Once validation/startup begins, finish the durable/live commit even if
    // the HTTP client disconnects and drops this handler future.
    let runtime = state.mcp.clone();
    let lockdown = crate::mcp_config::ManualLockdown::for_policy(&policy);
    let mutation = tokio::spawn(async move { runtime.replace_under_policy(body, lockdown).await });
    let outcome = mutation
        .await
        .map_err(|_| ServerError::internal("MCP settings update task failed"))?
        .map_err(mcp_request_error)?;
    match outcome {
        crate::mcp_config::McpReplaceOutcome::Replaced(info) => Ok(Json(info)),
        crate::mcp_config::McpReplaceOutcome::RefusedManual(refused) => {
            Err(providers::managed_profile_refusal(format!(
                "this profile is managed by a model gateway; manual MCP servers are locked \
                 ({}). Mount gateway-managed endpoints from the Model Gateway settings instead.",
                refused.join(", ")
            )))
        }
    }
}

/// `GET /chats/{chat_id}/calls/{call_id}/mcp-app-payload` — the completed
/// call's result, packaged for its declared MCP Apps view.
///
/// Only calls whose output carried a validated view declaration answer here,
/// and the payload is handed to the renderer as an opaque envelope for the
/// sandboxed frame — the transcript presentation itself never reads it.
pub async fn get_mcp_app_payload(
    store: ScopedStore,
    Path((chat_id, call_id)): Path<(ChatId, CallId)>,
) -> Result<Json<McpAppPayload>, ServerError> {
    store.require_chat(chat_id).await?;
    let events = store.list_events(chat_id, 0).await?;
    mcp_app_payload_from_events(&events, call_id)
        .map(Json)
        .ok_or_else(|| ServerError::not_found("no MCP App payload for this call"))
}

/// The MCP `CallToolResult` fields the view consumes, plus the call's
/// model-authored arguments for the `tool-input` notification.
///
/// Deliberately not a generated wire type: `arguments` and
/// `structured_content` are opaque passthrough JSON for the sandboxed view,
/// and the generator's precision guard rightly refuses `any`-shaped fields.
/// The renderer never reads them; the hand-written TS twin documents the
/// same opacity.
#[derive(Debug, PartialEq, Serialize)]
pub struct McpAppPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<serde_json::Value>,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    structured_content: Option<serde_json::Value>,
    is_error: bool,
}

fn mcp_app_payload_from_events(
    events: &[SequencedEvent],
    call_id: CallId,
) -> Option<McpAppPayload> {
    let mut fragments = String::new();
    let mut completed = None;
    for event in events {
        match &event.event {
            openwave_core::AgentEvent::ToolCallArgsDelta {
                call_id: id,
                fragment,
            } if *id == call_id => fragments.push_str(fragment),
            openwave_core::AgentEvent::ToolCallCompleted {
                call_id: id,
                output,
                ..
            } if *id == call_id => completed = Some(output),
            _ => {}
        }
    }
    // Mirror the preview filter: an error output never renders a card, so
    // it serves no payload either.
    let output = completed.filter(|output| output.ui_view.is_some() && !output.is_error)?;
    Some(McpAppPayload {
        arguments: serde_json::from_str(&fragments).ok(),
        content: output.content.clone(),
        structured_content: output.data.clone(),
        is_error: output.is_error,
    })
}

/// `GET /policy` — the resolved managed-mode policy. Read-only by design:
/// provisioning has no renderer-writable route, which is what keeps the
/// state sticky. A shell-registered pending pairing rides along — on an
/// unmanaged profile awaiting its first sign-in, or on a provisioned one
/// where the shell's confirmed re-pair flow parked it — so the gate can
/// present the sign-in that would commit it. Runtime state, merged here and
/// never part of the durable resolution.
pub async fn get_policy(
    State(state): State<AppState>,
) -> Result<Json<crate::managed_policy::ManagedPolicy>, ServerError> {
    Ok(Json(policy_with_pending(&state).await?))
}

async fn policy_with_pending(
    state: &AppState,
) -> Result<crate::managed_policy::ManagedPolicy, ServerError> {
    let mut policy = crate::managed_policy::resolve(&*state.store, &*state.os_policy).await?;
    policy.pending_gateway_url = state.gateway.pending_pairing_url().await;
    Ok(policy)
}

/// `GET /gateway/status` — renderer-safe model-gateway connection state.
pub async fn get_gateway_status(
    State(state): State<AppState>,
) -> Result<Json<crate::gateway_runtime::GatewayStatus>, ServerError> {
    Ok(Json(state.gateway.status().await?))
}

/// `POST /gateway/sign-in` — start the browser sign-in and return the URL the
/// renderer should open. The exchange completes in the background; poll
/// `GET /gateway/status` for the outcome.
pub async fn post_gateway_sign_in(
    State(state): State<AppState>,
) -> Result<Json<GatewaySignInStarted>, ServerError> {
    let authorization_url = state.gateway.begin_sign_in(state.mcp.clone()).await?;
    Ok(Json(GatewaySignInStarted { authorization_url }))
}

#[derive(Serialize)]
pub struct GatewaySignInStarted {
    authorization_url: String,
}

/// `POST /gateway/pairing/dismiss` — decline the pending deep-link pairing.
/// Renderer-reachable, deliberately: declining changes nothing durable, so
/// the failure direction is safe — a compromised renderer could only cancel
/// a pairing prompt, never create or approve one. Returns the policy the
/// gate should now render.
pub async fn post_gateway_pairing_dismiss(
    State(state): State<AppState>,
) -> Result<Json<crate::managed_policy::ManagedPolicy>, ServerError> {
    state.gateway.dismiss_pending_pairing().await;
    Ok(Json(policy_with_pending(&state).await?))
}

/// `POST /gateway/sign-out` — revoke at the gateway (best-effort) and clear
/// local session state and the synced model snapshot.
pub async fn post_gateway_sign_out(
    State(state): State<AppState>,
) -> Result<Json<crate::gateway_runtime::GatewayStatus>, ServerError> {
    state.gateway.sign_out().await?;
    Ok(Json(state.gateway.status().await?))
}

/// `GET /gateway/apps` — the signed-in user's entitled connected apps,
/// fetched live from the gateway. Fails when no gateway is configured or no
/// session is stored; the renderer only asks while signed in.
pub async fn get_gateway_apps(
    State(state): State<AppState>,
) -> Result<Json<crate::gateway_runtime::GatewayApps>, ServerError> {
    Ok(Json(state.gateway.apps().await?))
}

/// `POST /gateway/models/sync` — the settings page's explicit "sync with the
/// gateway": refetch the entitled models into the provider's model set and
/// reconcile the entitled MCP endpoint mounts, the same pair the periodic
/// sync tick performs. An explicit click reports failure honestly rather
/// than degrading quietly the way the background tick does — the user asked
/// for a sync and deserves to know it didn't complete.
pub async fn post_gateway_models_sync(
    State(state): State<AppState>,
) -> Result<Json<crate::gateway_runtime::GatewayStatus>, ServerError> {
    state.gateway.sync_models().await?;
    state.gateway.reconcile_endpoint_mounts(&state.mcp).await?;
    Ok(Json(state.gateway.status().await?))
}

/// `POST /mcp/servers/{name}/view-session` — trade the API bearer for a
/// single-use frame token addressing one prefetched MCP Apps view.
///
/// The frame itself cannot send an `Authorization` header, so the
/// authenticated renderer mints a short-lived capability here and points the
/// iframe at [`get_mcp_view_frame`].
pub async fn post_mcp_view_session(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<McpViewSessionRequest>,
) -> Result<Json<McpViewSession>, ServerError> {
    if state.mcp.ui_view_document(&name, &body.uri).await.is_none() {
        return Err(ServerError::not_found("no such MCP App view"));
    }
    let token = state
        .view_frames
        .mint(ViewFrameSource::McpView {
            server: name,
            uri: body.uri,
        })
        .await
        .ok_or_else(|| ServerError::not_found("no such MCP App view"))?;
    Ok(Json(McpViewSession {
        frame_path: format!("/mcp/view-frames/{token}"),
    }))
}

#[derive(Deserialize, ts_rs::TS)]
pub struct McpViewSessionRequest {
    uri: String,
}

/// Where the sandboxed iframe should load one view from, valid once.
#[derive(Serialize, ts_rs::TS)]
pub struct McpViewSession {
    frame_path: String,
}

/// `GET /mcp/view-frames/{token}` — redeem a frame token for its document.
///
/// Deliberately outside the bearer-guarded router (an iframe carries no
/// headers); reachable only with an unguessable single-use token that expires
/// in a minute. `frame-ancestors` is intentionally absent: the embedding
/// origin differs between dev and packaged builds, and access is gated by
/// token secrecy plus single use, not by who may embed. The response carries its **own** Content-Security-Policy —
/// an http-served document never inherits the app's policy the way a
/// `blob:`/`srcdoc` document would, which is precisely why the frame is
/// served instead of minted in the renderer: the view's inline script runs
/// under this explicit policy, network egress stays shut, and the app CSP
/// remains as strict as before.
pub async fn get_mcp_view_frame(
    State(state): State<AppState>,
    Path(token): Path<uuid::Uuid>,
) -> Response {
    let Some(ViewFrameSource::McpView { server, uri }) = state.view_frames.take(token).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(document) = state.mcp.ui_view_document(&server, &uri).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    view_frame_response(document.html)
}

/// The one response shape every sandboxed view frame is served with,
/// whatever its source: the document's own strict Content-Security-Policy
/// (inline script and style may run; every network direction is shut) plus
/// the no-sniff/no-referrer/no-store envelope.
///
/// The policy asserts `sandbox allow-scripts` itself rather than relying on
/// the embedder's `sandbox` attribute. Both embedders set that attribute, and
/// a test pins that they do — but the attribute is one edit away from being
/// dropped, and the whole isolation of a served app document rests on it: with
/// `allow-same-origin`, the document shares the API server's origin and can
/// read `server_info` (which carries the API bearer). Stating it in the
/// response makes the opaque origin a property of the document, not of
/// whoever embeds it. `allow-scripts` and nothing else: the document's own
/// inline script is the point, and forms, popups, top-level navigation, and
/// downloads are not.
fn view_frame_response(body: impl Into<axum::body::Body>) -> Response {
    (
        [
            ("content-type", "text/html; charset=utf-8"),
            (
                "content-security-policy",
                concat!(
                    "sandbox allow-scripts; ",
                    "default-src 'none'; script-src 'unsafe-inline'; ",
                    "style-src 'unsafe-inline'; img-src data:; font-src data:; ",
                    "connect-src 'none'; form-action 'none'; base-uri 'none'"
                ),
            ),
            ("x-content-type-options", "nosniff"),
            ("referrer-policy", "no-referrer"),
            ("cache-control", "no-store"),
        ],
        body.into(),
    )
        .into_response()
}

/// `POST /apps/{id}/view-session` — trade the API bearer for a single-use
/// frame token addressing one stored local-app revision.
///
/// The same capability trade as [`post_mcp_view_session`], for the same
/// reason: the sandboxed iframe cannot carry the bearer. Serves the app's
/// current revision unless the body pins one, which must belong to the app.
/// A soft-deleted app mints nothing until it is restored — deletion removes
/// the open affordance, not just the library row.
pub async fn post_app_view_session(
    State(state): State<AppState>,
    Path(id): Path<AppId>,
    Json(body): Json<AppViewSessionRequest>,
) -> Result<Json<AppViewSession>, ServerError> {
    let app = state
        .store
        .get_app(id)
        .await?
        .filter(|app| app.deleted_at.is_none())
        .ok_or_else(|| ServerError::not_found(format!("app {id} not found")))?;
    let revision_id = match body.revision {
        Some(revision_id) => {
            state
                .store
                .get_app_revision(revision_id)
                .await?
                .filter(|revision| revision.app_id == id)
                .ok_or_else(|| {
                    ServerError::not_found(format!("app revision {revision_id} not found"))
                })?
                .id
        }
        None => app.current_revision,
    };
    let token = state
        .view_frames
        .mint(ViewFrameSource::AppRevision {
            app_id: id,
            revision_id,
        })
        .await
        .ok_or_else(|| {
            ServerError::conflict("too many outstanding view frames; retry in a moment")
        })?;
    Ok(Json(AppViewSession {
        frame_path: format!("/apps/view-frames/{token}"),
    }))
}

#[derive(Deserialize, ts_rs::TS)]
pub struct AppViewSessionRequest {
    /// Revision to serve; the app's current revision when omitted.
    revision: Option<AppRevisionId>,
}

/// Where the sandboxed iframe should load one app revision from, valid once.
#[derive(Serialize, ts_rs::TS)]
pub struct AppViewSession {
    frame_path: String,
}

/// `GET /apps/view-frames/{token}` — redeem a frame token for one stored
/// app revision's bundle.
///
/// The exact contract of [`get_mcp_view_frame`] — unauthenticated, reached
/// only by an unguessable single-use token, served under the same strict
/// policy — differing only in where the document comes from: app bundles are
/// write-once bytes under the profile data directory, loaded by durable
/// identity instead of re-resolved against a live MCP session.
pub async fn get_app_view_frame(
    State(state): State<AppState>,
    Path(token): Path<uuid::Uuid>,
) -> Response {
    let Some(ViewFrameSource::AppRevision {
        app_id,
        revision_id,
    }) = state.view_frames.take(token).await
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let path = state
        .config
        .data_dir
        .join(app_revision_relative_path(app_id, revision_id));
    match tokio::fs::read(&path).await {
        Ok(bundle) => view_frame_response(bundle),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `POST /mcp/servers/{name}/reconnect` — explicitly establish a fresh session,
/// rediscover tools, and publish them for subsequent turns.
pub async fn post_mcp_server_reconnect(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<McpServersInfo>, ServerError> {
    // A dropped response must not strand the published state at
    // `reconnecting`; Tokio tasks continue after their JoinHandle is dropped.
    let runtime = state.mcp.clone();
    let mutation = tokio::spawn(async move { runtime.reconnect(&name).await });
    Ok(Json(
        mutation
            .await
            .map_err(|_| ServerError::internal("MCP reconnect task failed"))?
            .map_err(mcp_request_error)?,
    ))
}

fn mcp_request_error(error: AgentError) -> ServerError {
    match error {
        AgentError::Config(message) => ServerError::bad_request(message),
        other => other.into(),
    }
}

/// Product-facing project names stay compact across desktop and API clients.
pub const MAX_PROJECT_TITLE_CHARS: usize = 120;
/// The same bound for conversation names, whether a user typed one or the
/// product derived one. A sidebar row is a sidebar row either way.
pub const MAX_CHAT_TITLE_CHARS: usize = MAX_PROJECT_TITLE_CHARS;
/// Project metadata requests need only a compact JSON object.
pub const MAX_PROJECT_METADATA_BODY_BYTES: usize = 1_024;

/// Runtime settings a client can read. The API key itself is never returned —
/// it lives in the `SecretProvider`, not the store — only whether one is set.
#[derive(Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct Settings {
    /// The model turns run against, or `None` to use the server's default.
    #[serde(default)]
    pub model: Option<String>,
    /// Whether a model API key is configured (never the key itself).
    pub has_api_key: bool,
    /// The sticky new-chat defaults, so a composer for a chat that does not
    /// exist yet can show what `POST /chats` will seed.
    #[serde(default)]
    pub chat_defaults: StickyChatDefaults,
    /// Maximum nonterminal spawned agents allowed in one chat.
    pub max_active_background_agents: u32,
}

/// The reader's last explicit per-chat choices — what an unspecified field of
/// `POST /chats` seeds. A `None` field has no recorded choice and keeps the
/// hard default (configured model, `ask`, open network).
///
/// The permission mode is reported clamped to any managed ceiling, so what a
/// picker displays is what creation will actually seed.
#[derive(Debug, Default, Serialize, Deserialize, ts_rs::TS)]
pub struct StickyChatDefaults {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    pub permission_mode: Option<PermissionMode>,
    #[serde(default)]
    pub network_policy: Option<openwave_core::NetworkPolicy>,
}

/// Body of `PUT /settings`. Each field is a *double* option so an absent key is
/// distinguished from an explicit `null`: absent leaves the value unchanged,
/// `null` resets it to the server default, and a value sets it.
#[derive(Debug, Deserialize)]
pub struct SettingsUpdate {
    #[serde(default, deserialize_with = "double_option")]
    pub model: Option<Option<String>>,
    #[serde(default)]
    pub max_active_background_agents: Option<u32>,
}

/// Deserialize a present field (including JSON `null`) as `Some(..)`; `#[serde(default)]`
/// supplies `None` when the field is absent.
fn double_option<'de, D, T>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    serde::Deserialize::deserialize(deserializer).map(Some)
}

/// `GET /settings` — the current runtime settings.
pub async fn get_settings(State(state): State<AppState>) -> Result<Json<Settings>, ServerError> {
    Ok(Json(Settings {
        model: read_model(&*state.store).await?,
        has_api_key: has_api_key(&*state.secrets).await,
        chat_defaults: read_sticky_chat_defaults(&state).await?,
        max_active_background_agents: read_max_active_background_agents(&*state.store).await?,
    }))
}

/// `PUT /settings` — update runtime settings, returning the new state. Only the
/// fields present in the body are touched.
pub async fn put_settings(
    State(state): State<AppState>,
    Json(mut body): Json<SettingsUpdate>,
) -> Result<Json<Settings>, ServerError> {
    if let Some(Some(model)) = body.model.as_mut() {
        *model = validate_model_selection(&state, model, false).await?;
    }
    match body.model {
        // Absent: leave the model unchanged.
        None => {}
        // Explicit null: reset to the server default.
        Some(None) => {
            model_roles::write_selection(&*state.store, ModelRole::Chat, None).await?;
        }
        // A value: reject empty (it would break every turn), else set it.
        Some(Some(model)) => {
            if model.is_empty() {
                return Err(ServerError::bad_request("model must not be empty"));
            }
            model_roles::write_selection(&*state.store, ModelRole::Chat, Some(&model)).await?;
        }
    }
    if let Some(limit) = body.max_active_background_agents {
        if limit == 0 || limit > AgentRun::MAX_CONCURRENCY_LIMIT {
            return Err(ServerError::bad_request(format!(
                "max_active_background_agents must be in 1..={}",
                AgentRun::MAX_CONCURRENCY_LIMIT
            )));
        }
        state
            .store
            .set_setting(
                MAX_ACTIVE_BACKGROUND_AGENTS_SETTING,
                &serde_json::json!(limit),
            )
            .await?;
    }
    Ok(Json(Settings {
        model: read_model(&*state.store).await?,
        has_api_key: has_api_key(&*state.secrets).await,
        chat_defaults: read_sticky_chat_defaults(&state).await?,
        max_active_background_agents: read_max_active_background_agents(&*state.store).await?,
    }))
}

pub(crate) async fn read_max_active_background_agents(
    store: &dyn Store,
) -> openwave_core::Result<u32> {
    Ok(store
        .get_setting(MAX_ACTIVE_BACKGROUND_AGENTS_SETTING)
        .await?
        .and_then(|value| serde_json::from_value::<u32>(value).ok())
        .filter(|limit| *limit > 0 && *limit <= AgentRun::MAX_CONCURRENCY_LIMIT)
        .unwrap_or(AgentRun::DEFAULT_MAX_ACTIVE_BACKGROUND_AGENTS))
}

/// `GET /web-search` — read host-owned web-search selection and readiness.
/// No model tool is registered by this endpoint.
pub async fn get_web_search_config(
    State(state): State<AppState>,
) -> Result<Json<WebSearchConfigInfo>, ServerError> {
    Ok(Json(
        web_search::config_info(&*state.store, &*state.secrets).await?,
    ))
}

/// `PUT /web-search` — select a fixed provider and bounded timeout. Provider
/// credentials remain in the OS keychain under fixed provider-owned names.
pub async fn put_web_search_config(
    State(state): State<AppState>,
    Json(body): Json<WebSearchConfigUpdate>,
) -> Result<Json<WebSearchConfigInfo>, ServerError> {
    Ok(Json(
        web_search::update_config(&*state.store, &*state.secrets, body).await?,
    ))
}

/// `GET /code-execution` — read host-owned provider selection, timeout policy,
/// and readiness. No executable or provider endpoint is accepted here.
pub async fn get_code_execution_config(
    State(state): State<AppState>,
) -> Result<Json<CodeExecutionConfigInfo>, ServerError> {
    Ok(Json(
        code_execution::config_info(&state.config, &*state.store, &*state.secrets).await?,
    ))
}

/// `PUT /code-execution` — select a fixed provider and bounded host timeout.
pub async fn put_code_execution_config(
    State(state): State<AppState>,
    Json(body): Json<CodeExecutionConfigUpdate>,
) -> Result<Json<CodeExecutionConfigInfo>, ServerError> {
    Ok(Json(
        code_execution::update_config(&state.config, &*state.store, &*state.secrets, body).await?,
    ))
}

/// `GET /code-execution/credentials` — readiness for the fixed E2B and Daytona
/// credential slots. Local execution needs no credential and is absent here.
pub async fn get_code_execution_credentials(
    State(state): State<AppState>,
) -> Json<CodeExecutionCredentialsInfo> {
    Json(code_execution::credentials_info(&*state.secrets).await)
}

const MAX_CODE_EXECUTION_CREDENTIAL_BYTES: usize = 8 * 1024;

/// Body of `PUT /code-execution/credentials/{provider}`. Debug output always
/// redacts the credential.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeExecutionCredentialUpdate {
    pub api_key: String,
}

impl std::fmt::Debug for CodeExecutionCredentialUpdate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodeExecutionCredentialUpdate")
            .field("api_key", &"***")
            .finish()
    }
}

/// Store a managed provider key in its fixed slot without changing selection.
pub async fn put_code_execution_credential(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(body): Json<CodeExecutionCredentialUpdate>,
) -> Result<Json<CodeExecutionCredentialReadiness>, ServerError> {
    let provider = code_execution::credential_provider(&provider)?;
    if body.api_key.len() > MAX_CODE_EXECUTION_CREDENTIAL_BYTES {
        return Err(ServerError::bad_request(format!(
            "{provider} api_key must be at most {MAX_CODE_EXECUTION_CREDENTIAL_BYTES} bytes"
        )));
    }
    let api_key = body.api_key.trim();
    if api_key.is_empty() {
        return Err(ServerError::bad_request(format!(
            "{provider} api_key must not be empty"
        )));
    }
    Ok(Json(
        code_execution::write_credential(&*state.secrets, provider, api_key).await?,
    ))
}

/// Remove only the requested provider's credential; selection remains unchanged.
pub async fn delete_code_execution_credential(
    State(state): State<AppState>,
    Path(provider): Path<String>,
) -> Result<Json<CodeExecutionCredentialReadiness>, ServerError> {
    let provider = code_execution::credential_provider(&provider)?;
    Ok(Json(
        code_execution::delete_credential(&*state.secrets, provider).await?,
    ))
}

/// `POST /chats/{chat_id}/turns/{turn_id}/file-changes/undo` — restore the
/// prior bytes journaled for one turn without clobbering later edits.
pub async fn post_undo_turn_file_changes(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, turn_id)): Path<(ChatId, TurnId)>,
) -> Result<Json<ExecTurnUndoOutcome>, ServerError> {
    store.require_chat(chat_id).await?;
    let outcome = undo_turn_file_changes(&*state.store, &*state.blobs, chat_id, turn_id).await?;
    if outcome.files.is_empty() {
        return Err(ServerError::not_found(format!(
            "turn {turn_id} has no retained file changes in chat {chat_id}"
        )));
    }
    Ok(Json(outcome))
}

/// `POST /chats/{chat_id}/turns/{turn_id}/file-changes/{snapshot_id}/undo` —
/// restore one file from the turn without touching its siblings.
pub async fn post_undo_one_file_change(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, turn_id, snapshot_id)): Path<(ChatId, TurnId, uuid::Uuid)>,
) -> Result<Json<ExecFileUndoOutcome>, ServerError> {
    store.require_chat(chat_id).await?;
    undo_one_file_change(&*state.store, &*state.blobs, chat_id, turn_id, snapshot_id)
        .await?
        .map(Json)
        .ok_or_else(|| ServerError::not_found("no retained file change for this turn"))
}

/// `GET /chats/{chat_id}/turns/{turn_id}/file-changes/{snapshot_id}/preview/{revision}`
/// — render one authorized journal revision without exposing source bytes,
/// paths, or a reusable document identity.
pub async fn get_file_change_preview(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, turn_id, snapshot_id, revision)): Path<(
        ChatId,
        TurnId,
        uuid::Uuid,
        ExecFilePreviewRevision,
    )>,
) -> Result<Response, ServerError> {
    store.require_chat(chat_id).await?;
    let _permit = state
        .file_preview_permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ServerError::too_many_requests_kind(
                "file_preview_busy",
                "Document preview rendering is busy; try again shortly.",
            )
        })?;
    let rendered = render_file_change_preview(
        &*state.store,
        &*state.blobs,
        ExecFilePreviewRequest {
            chat_id,
            turn_id,
            snapshot_id,
            revision,
            scripts_dir: state.config.exec_scripts_dir.as_deref(),
            temp_root: &state.config.data_dir.join("file-preview-temp"),
        },
    )
    .await
    .map_err(file_preview_error)?;
    let byte_len = rendered.bytes.len();
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, rendered.media_type.as_str())
        .header(header::CONTENT_LENGTH, byte_len.to_string())
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CONTENT_SECURITY_POLICY, SERVED_BYTES_CONTENT_POLICY)
        .header(header::REFERRER_POLICY, "no-referrer")
        .header(header::CONTENT_DISPOSITION, "inline")
        .header("x-openwave-preview-width", rendered.width.to_string())
        .header("x-openwave-preview-height", rendered.height.to_string())
        .body(Body::from(rendered.bytes))
        .map_err(|_| ServerError::internal("failed to build document preview response"))
}

fn file_preview_error(error: ExecFilePreviewError) -> ServerError {
    match error {
        ExecFilePreviewError::NotFound => {
            ServerError::not_found("No file change with that identity exists in this turn.")
        }
        ExecFilePreviewError::Unsupported => ServerError::unsupported_media_type_kind(
            "file_preview_unsupported",
            "No visual preview is available for this file type.",
        ),
        ExecFilePreviewError::Empty => ServerError::unprocessable_kind(
            "file_preview_empty",
            "This side of the change has no file.",
        ),
        ExecFilePreviewError::Stale => ServerError::conflict_kind(
            "file_preview_stale",
            "The file changed again; its after preview is no longer available.",
        ),
        ExecFilePreviewError::TooLarge => ServerError::unprocessable_kind(
            "file_preview_too_large",
            "This revision is too large to preview.",
        ),
        ExecFilePreviewError::Unavailable => ServerError::unprocessable_kind(
            "file_preview_unavailable",
            "This revision is no longer available to preview.",
        ),
        ExecFilePreviewError::RenderFailed => ServerError::unprocessable_kind(
            "file_preview_failed",
            "OpenWave could not render this revision on this device.",
        ),
    }
}

/// Maximum API-key size accepted by the local credential endpoint. This is
/// far beyond ordinary provider keys while keeping accidental pasted blobs out
/// of the OS keychain.
const MAX_WEB_SEARCH_CREDENTIAL_BYTES: usize = 8 * 1024;

/// Body of `PUT /web-search/credentials/{provider}`. The custom `Debug`
/// implementation redacts the only sensitive field.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebSearchCredentialUpdate {
    pub api_key: String,
}

impl std::fmt::Debug for WebSearchCredentialUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSearchCredentialUpdate")
            .field("api_key", &"***")
            .finish()
    }
}

/// `GET /web-search/credentials` — readiness for the fixed Exa and Tavily
/// credential slots. This route never returns the stored keys.
pub async fn get_web_search_credentials(
    State(state): State<AppState>,
) -> Result<Json<WebSearchCredentialsInfo>, ServerError> {
    Ok(Json(web_search::credentials_info(&*state.secrets).await?))
}

/// `PUT /web-search/credentials/{provider}` — store a key in one fixed
/// provider slot. It does not change provider selection or timeout policy.
pub async fn put_web_search_credential(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(body): Json<WebSearchCredentialUpdate>,
) -> Result<Json<WebSearchCredentialReadiness>, ServerError> {
    let provider = web_search::credential_provider(&provider)?;
    if body.api_key.len() > MAX_WEB_SEARCH_CREDENTIAL_BYTES {
        return Err(ServerError::bad_request(format!(
            "web search api_key must be at most {MAX_WEB_SEARCH_CREDENTIAL_BYTES} bytes"
        )));
    }
    let api_key = body.api_key.trim();
    if api_key.is_empty() {
        return Err(ServerError::bad_request(
            "web search api_key must not be empty",
        ));
    }
    Ok(Json(
        web_search::write_credential(&*state.secrets, provider, api_key).await?,
    ))
}

/// `DELETE /web-search/credentials/{provider}` — remove only that fixed
/// provider key. It does not change provider selection or timeout policy.
pub async fn delete_web_search_credential(
    State(state): State<AppState>,
    Path(provider): Path<String>,
) -> Result<Json<WebSearchCredentialReadiness>, ServerError> {
    let provider = web_search::credential_provider(&provider)?;
    Ok(Json(
        web_search::delete_credential(&*state.secrets, provider).await?,
    ))
}

/// The configured chat model, if any — the `chat` role's explicit selection.
async fn read_model(store: &dyn Store) -> openwave_core::Result<Option<String>> {
    model_roles::read_selection(store, ModelRole::Chat).await
}

/// The `chat` role's model with no conversation in hand: the global selection,
/// else the model this process launched with.
///
/// The boot default is process state, which is why the chat role has no
/// registry-backed default list of its own the way `utility` does.
async fn chat_role_model(store: &dyn Store, boot_default: &str) -> openwave_core::Result<String> {
    Ok(read_model(store)
        .await?
        .unwrap_or_else(|| boot_default.to_owned()))
}

/// Resolve which model a new execution in `chat` should use.
///
/// The order is the chat's override, then the global `model` setting, then the
/// boot default. A foreground turn freezes the result when its message is
/// accepted; a sandbox child inherits its origin turn's frozen selection and
/// only falls back here when it was admitted before that was recorded.
pub(crate) async fn resolve_chat_model(
    store: &dyn Store,
    chat: &openwave_core::Chat,
    boot_default: &str,
) -> openwave_core::Result<String> {
    match chat.model.clone() {
        Some(model) => Ok(model),
        None => chat_role_model(store, boot_default).await,
    }
}

/// Resolve, canonicalize, and availability-check a model selection before it
/// crosses a persistence boundary. Custom embedders with an injected provider
/// retain their free-form model contract; the production configured resolver
/// always enforces the typed registry.
async fn validate_model_selection(
    state: &AppState,
    value: &str,
    allow_legacy_custom: bool,
) -> Result<String, ServerError> {
    if value.is_empty() {
        return Err(ServerError::bad_request("model must not be empty"));
    }
    if !state.resolver.enforces_model_registry() {
        return Ok(value.to_owned());
    }
    let Some(policy) =
        providers::resolve_model_policy(&*state.store, value, allow_legacy_custom).await?
    else {
        return Err(ServerError::bad_request_kind(
            "unknown_model",
            format!(
                "model `{value}` is not registered for that provider; configure it under OpenAI-compatible models first"
            ),
        ));
    };
    let managed = crate::managed_policy::resolve(&*state.store, &*state.os_policy).await?;
    if !providers::provider_is_usable(&*state.store, &*state.secrets, policy.provider, &managed)
        .await?
    {
        return Err(ServerError::conflict_kind(
            "model_provider_unavailable",
            format!(
                "provider `{}` for model `{}` is disabled, unconfigured, or missing a credential",
                policy.provider, policy.id
            ),
        ));
    }
    Ok(policy.key)
}

/// Whether any model provider credential is configured — stored or via the
/// env fallbacks the resolver also honors. Prefer `GET /providers` for
/// per-kind detail; this field is the legacy "is anything ready?" signal.
async fn has_api_key(secrets: &dyn SecretProvider) -> bool {
    for &kind in ProviderKind::ALL {
        if providers::has_credential(secrets, kind).await {
            return true;
        }
    }
    false
}

/// Refuse selecting a permission mode above the managed ceiling.
///
/// The picker-facing half of the lockdown: the authoritative clamp lives at
/// the turn gate, which also catches chats whose stored mode predates the
/// policy. Selecting a mode at or below the ceiling stays open — the policy
/// names a maximum, not a fixed mode.
async fn refuse_permission_mode_over_ceiling(
    state: &AppState,
    requested: Option<PermissionMode>,
) -> Result<(), ServerError> {
    let Some(mode) = requested else {
        return Ok(());
    };
    let policy = crate::managed_policy::resolve(&*state.store, &*state.os_policy).await?;
    if !policy.permits_permission_mode(mode) {
        return Err(ServerError::conflict_kind(
            "permission_mode_locked",
            format!(
                "permission mode `{}` exceeds the maximum this managed profile allows (`{}`)",
                mode.as_str(),
                policy
                    .permission_mode_ceiling
                    .unwrap_or(PermissionMode::Allow)
                    .as_str()
            ),
        ));
    }
    Ok(())
}

/// Refuse a BYOK credential write on a managed profile.
///
/// The gateway session is a managed profile's only model credential; stored
/// BYOK keys are frozen while the policy holds — inert, not deleted, so an
/// unmanaged profile is byte-for-byte unaffected.
async fn refuse_credential_writes_when_managed(state: &AppState) -> Result<(), ServerError> {
    let policy = crate::managed_policy::resolve(&*state.store, &*state.os_policy).await?;
    if policy.managed {
        return Err(providers::managed_profile_refusal(
            "this profile is managed by a model gateway; provider API keys are locked",
        ));
    }
    Ok(())
}

/// Body of `PUT /settings/api-key`.
///
/// Legacy shim: writes the Anthropic credential in the typed blob shape and
/// enables the Anthropic provider. Prefer `PUT /providers/anthropic`.
#[derive(Deserialize)]
pub struct ApiKey {
    /// The provider API key to store. Written to the `SecretProvider` (the OS
    /// keychain on desktop), never to the database, and never read back out.
    pub api_key: String,
}

// Redact the key so it can't leak through a `{:?}`.
impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKey").field("api_key", &"***").finish()
    }
}

/// `PUT /settings/api-key` — store the Anthropic API key. `204 No Content`.
pub async fn put_api_key(
    State(state): State<AppState>,
    Json(body): Json<ApiKey>,
) -> Result<StatusCode, ServerError> {
    refuse_credential_writes_when_managed(&state).await?;
    if body.api_key.is_empty() {
        return Err(ServerError::bad_request("api_key must not be empty"));
    }
    // Write the typed credential and enable Anthropic so the new providers
    // surface and the legacy shim stay equivalent.
    providers::write_credential(
        &*state.secrets,
        ProviderKind::Anthropic,
        &ProviderCredential::api_key(&body.api_key),
    )
    .await?;
    let mut config = providers::read_config(&*state.store, ProviderKind::Anthropic).await?;
    config.enabled = true;
    providers::write_config(&*state.store, ProviderKind::Anthropic, &config).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /settings/api-key` — remove the stored Anthropic API key. `204`.
///
/// Clears only the stored key. If the daemon was launched with an
/// `ANTHROPIC_API_KEY` in its environment, that fallback still applies — so
/// `has_api_key` may stay `true` and turns keep resolving a provider after a
/// delete. The environment is a deploy-time default the API doesn't override.
pub async fn delete_api_key(State(state): State<AppState>) -> Result<StatusCode, ServerError> {
    refuse_credential_writes_when_managed(&state).await?;
    providers::delete_credential(&*state.secrets, ProviderKind::Anthropic).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Response for `GET /providers`.
#[derive(Debug, Serialize)]
pub struct ProvidersList {
    pub providers: Vec<ProviderInfo>,
}

/// `GET /providers` — every known provider kind and its current config. The
/// model gateway appears only on a managed profile, projected from policy.
pub async fn list_providers(
    State(state): State<AppState>,
) -> Result<Json<ProvidersList>, ServerError> {
    let policy = crate::managed_policy::resolve(&*state.store, &*state.os_policy).await?;
    Ok(Json(ProvidersList {
        providers: providers::list_providers(&*state.store, &*state.secrets, &policy).await?,
    }))
}

/// `PUT /providers/{kind}` — update a provider's config and/or credential.
pub async fn put_provider(
    State(state): State<AppState>,
    Path(kind): Path<String>,
    Json(body): Json<ProviderUpdate>,
) -> Result<Json<ProviderInfo>, ServerError> {
    let kind = ProviderKind::parse(&kind)
        .ok_or_else(|| ServerError::not_found(format!("unknown provider kind: {kind}")))?;
    let info = providers::update_provider(
        &*state.store,
        &*state.secrets,
        kind,
        body,
        &*state.os_policy,
    )
    .await?;
    Ok(Json(info))
}

/// `DELETE /providers/{kind}/credential` — remove the stored credential. `204`.
pub async fn delete_provider_credential(
    State(state): State<AppState>,
    Path(kind): Path<String>,
) -> Result<StatusCode, ServerError> {
    let kind = ProviderKind::parse(&kind)
        .ok_or_else(|| ServerError::not_found(format!("unknown provider kind: {kind}")))?;
    refuse_credential_writes_when_managed(&state).await?;
    providers::delete_credential(&*state.secrets, kind).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_voice_transcription(
    State(state): State<AppState>,
) -> Result<Json<VoiceTranscriptionInfo>, ServerError> {
    Ok(Json(
        voice_transcription::info(&*state.store, &*state.secrets, &*state.local_voice).await?,
    ))
}

pub async fn put_voice_transcription(
    State(state): State<AppState>,
    Json(body): Json<VoiceTranscriptionUpdate>,
) -> Result<Json<VoiceTranscriptionInfo>, ServerError> {
    Ok(Json(
        voice_transcription::update(&*state.store, &*state.secrets, &*state.local_voice, body)
            .await?,
    ))
}

pub async fn post_voice_transcription(
    State(state): State<AppState>,
    headers: HeaderMap,
    audio: Bytes,
) -> Result<Json<serde_json::Value>, ServerError> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ServerError::bad_request("voice recording content type is required"))?;
    let text = voice_transcription::transcribe(
        &*state.store,
        &*state.secrets,
        &*state.local_voice,
        content_type,
        audio,
    )
    .await?;
    Ok(Json(serde_json::json!({ "text": text })))
}

pub async fn post_voice_transcription_install(
    State(state): State<AppState>,
    Json(request): Json<voice_transcription::LocalVoiceInstall>,
) -> Result<Json<voice_transcription::LocalVoiceInfo>, ServerError> {
    Ok(Json(
        voice_transcription::install_local(&*state.store, &*state.local_voice, request).await?,
    ))
}

/// A selectable model in the catalog.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct ModelInfo {
    /// Stable provider-qualified selection key used by settings and chats.
    pub key: String,
    /// The identifier passed to the provider and stored as `chat.model`.
    pub id: String,
    /// Human-readable label for the selector (e.g. `"Claude Opus 4.8"`).
    pub display_name: String,
    /// The provider that serves the model.
    pub provider: ProviderKind,
    /// The vendor whose curated model this row is, when that differs from the
    /// provider serving it — a gateway-served model whose id exactly matches a
    /// curated one. For presentation only (icon and branding); routing still
    /// uses `provider`, and a client falls back to it when this is null.
    pub vendor: Option<ProviderKind>,
    /// How thoroughly OpenWave has exercised this provider/model combination.
    pub verification: crate::model_registry::VerificationTier,
    /// Whether the provider is enabled, configured, and credentialed.
    pub available: bool,
    /// Approximate context window in tokens.
    pub context_window: u32,
    /// Maximum model output in tokens.
    pub max_output_tokens: u32,
    /// Input modalities accepted by the model.
    pub input_modalities: Vec<crate::model_registry::InputModality>,
    /// Whether the model can produce an internal reasoning stream.
    pub supports_reasoning: bool,
    /// The reasoning-effort levels this model accepts, ascending. Empty when
    /// the model exposes no effort control, which is what a client checks
    /// before offering the selector at all.
    ///
    /// Carries the enum rather than plain strings so the generated TypeScript
    /// is the same union a chat's stored effort has, and a client cannot offer
    /// a level it could not then set.
    pub reasoning_efforts: Vec<ReasoningEffort>,
    /// Whether the model accepts image input alongside text.
    pub multimodal: bool,
}

/// One named model role and what it resolves to right now.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct ModelRoleInfo {
    /// The role this row describes.
    pub role: ModelRole,
    /// The catalog key the user selected for this role, or `None` when the role
    /// is left automatic.
    pub selection: Option<String>,
    /// The catalog key this role resolves to right now, selection or not.
    ///
    /// A selector that offers "automatic" as a choice can only say what that
    /// choice means if the server says which model it lands on. `None` when the
    /// role resolves to nothing the catalog can name, which leaves the client
    /// with nothing to promise rather than a guess — and, for `utility`, means
    /// the work that depends on it is skipped.
    pub resolved_key: Option<String>,
}

/// Response for `GET /models`.
#[derive(Debug, Serialize)]
pub struct ModelCatalog {
    /// The models a client can select from.
    pub models: Vec<ModelInfo>,
    /// Every named role, its selection, and what it currently resolves to.
    pub roles: Vec<ModelRoleInfo>,
}

/// `GET /models` — the catalog a chat's model selector chooses from.
///
/// All typed registry rows plus current availability. Clients may explain
/// unavailable rows, but must never offer them as usable selections.
pub async fn list_models(State(state): State<AppState>) -> Result<Json<ModelCatalog>, ServerError> {
    let mut roles = Vec::with_capacity(ModelRole::ALL.len());
    for &role in ModelRole::ALL {
        let selection = model_roles::read_selection(&*state.store, role).await?;
        let resolved_key = resolved_role_key(&state, role, selection.as_deref()).await?;
        roles.push(ModelRoleInfo {
            role,
            selection,
            resolved_key,
        });
    }
    let policy = crate::managed_policy::resolve(&*state.store, &*state.os_policy).await?;
    let models = providers::catalog_models(&*state.store, &*state.secrets, &policy)
        .await?
        .into_iter()
        .map(|entry| ModelInfo {
            key: entry.policy.key,
            id: entry.policy.id,
            display_name: entry.policy.display_name,
            provider: entry.policy.provider,
            vendor: entry.policy.vendor,
            verification: entry.policy.verification,
            available: entry.available,
            context_window: entry.policy.context_window,
            max_output_tokens: entry.policy.max_output_tokens,
            input_modalities: entry.policy.input_modalities.clone(),
            supports_reasoning: entry.policy.supports_reasoning,
            reasoning_efforts: entry.policy.reasoning_efforts.clone(),
            multimodal: entry
                .policy
                .input_modalities
                .contains(&crate::model_registry::InputModality::Image),
        })
        .collect();
    Ok(Json(ModelCatalog { models, roles }))
}

/// Body of `PUT /models/roles/{role}`. An explicit `null` selection returns the
/// role to automatic resolution.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRoleUpdate {
    /// The catalog key to pin this role to, or `null` for automatic.
    #[serde(default)]
    pub selection: Option<String>,
}

/// `PUT /models/roles/{role}` — pin a role to one model, or clear it back to
/// automatic resolution.
///
/// A selection must be a registered model whose provider is currently usable, so
/// a role cannot be pinned to something that could not run. For `chat` this
/// writes the same setting as `PUT /settings`.
pub async fn put_model_role(
    State(state): State<AppState>,
    Path(role): Path<String>,
    Json(body): Json<ModelRoleUpdate>,
) -> Result<Json<ModelRoleInfo>, ServerError> {
    let role = ModelRole::parse(&role)
        .ok_or_else(|| ServerError::not_found(format!("unknown model role: {role}")))?;
    let selection = match body.selection {
        Some(selection) => Some(validate_model_selection(&state, &selection, false).await?),
        None => None,
    };
    model_roles::write_selection(&*state.store, role, selection.as_deref()).await?;
    let resolved_key = resolved_role_key(&state, role, selection.as_deref()).await?;
    Ok(Json(ModelRoleInfo {
        role,
        selection,
        resolved_key,
    }))
}

/// The catalog key `role` resolves to right now, given its stored `selection`.
///
/// This is the server's one answer for "what does this role's default mean" —
/// the settings page and the composer both label their automatic choice with
/// it, which is why the managed re-route below lives here rather than in a
/// client.
async fn resolved_role_key(
    state: &AppState,
    role: ModelRole,
    selection: Option<&str>,
) -> Result<Option<String>, ServerError> {
    match role {
        // The chat role goes through the same seam a new execution does, minus
        // the per-chat override there is no chat here to read — so the label a
        // client shows for "default" is what the next turn actually gets: the
        // accept path below freezes its model through the same
        // `effective_chat_policy`, managed re-route included. Its last resort
        // is the boot default, which no role's list can name.
        ModelRole::Chat => {
            let fallback = match selection {
                Some(selection) => selection.to_owned(),
                None => chat_role_model(&*state.store, &state.agent_config.model).await?,
            };
            let managed = crate::managed_policy::resolve(&*state.store, &*state.os_policy).await?;
            Ok(model_roles::effective_chat_policy(
                &*state.store,
                &*state.secrets,
                &managed,
                &fallback,
            )
            .await?
            .map(|policy| policy.key))
        }
        _ => Ok(
            model_roles::resolve(&*state.store, &*state.secrets, &*state.os_policy, role)
                .await?
                .map(|policy| policy.key),
        ),
    }
}

/// Body of `POST /projects`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateProject {
    /// Optional human-facing title.
    #[serde(default)]
    pub title: Option<String>,
}

/// Body of `PATCH /projects/{id}`. An explicit `null` clears the title, while
/// an absent field leaves it unchanged.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectUpdate {
    #[serde(default, deserialize_with = "double_option")]
    pub title: Option<Option<String>>,
}

fn normalize_project_title(title: Option<String>) -> Result<Option<String>, ServerError> {
    let title = title
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if title
        .as_ref()
        .is_some_and(|value| value.chars().count() > MAX_PROJECT_TITLE_CHARS)
    {
        return Err(ServerError::bad_request(format!(
            "project title must not exceed {MAX_PROJECT_TITLE_CHARS} characters"
        )));
    }
    Ok(title)
}

/// Trim a client-supplied conversation title and hold it to the stored bound.
///
/// An empty title is the same as no title: the sidebar renders "New chat" for
/// both, and storing `Some("")` would also read as "already named" to the
/// derived-title path and suppress it forever.
fn normalize_chat_title(title: Option<String>) -> Result<Option<String>, ServerError> {
    let title = title
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if title
        .as_ref()
        .is_some_and(|value| value.chars().count() > MAX_CHAT_TITLE_CHARS)
    {
        return Err(ServerError::bad_request(format!(
            "chat title must not exceed {MAX_CHAT_TITLE_CHARS} characters"
        )));
    }
    Ok(title)
}

/// `POST /projects` — create a project and return it (`201 Created`).
pub async fn create_project(
    store: ScopedStore,
    Json(body): Json<CreateProject>,
) -> Result<impl IntoResponse, ServerError> {
    let project = Project {
        id: ProjectId::new(),
        title: normalize_project_title(body.title)?,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_project(&project).await?;
    Ok((StatusCode::CREATED, Json(project)))
}

/// `PATCH /projects/{id}` — update bounded human-facing project metadata.
pub async fn patch_project(
    store: ScopedStore,
    Path(id): Path<ProjectId>,
    Json(body): Json<ProjectUpdate>,
) -> Result<Json<Project>, ServerError> {
    let title = body.title.map(normalize_project_title).transpose()?;
    if let Some(title) = title {
        if !store.update_project_title(id, title).await? {
            return Err(ServerError::not_found(format!("project {id} not found")));
        }
    }
    store
        .get_project(id)
        .await?
        .map(Json)
        .ok_or_else(|| ServerError::not_found(format!("project {id} not found")))
}

/// `GET /projects` — list projects, most-recently-created first.
pub async fn list_projects(store: ScopedStore) -> Result<Json<Vec<Project>>, ServerError> {
    Ok(Json(store.list_projects().await?))
}

/// `GET /projects/{id}` — fetch one project, or `404`.
pub async fn get_project(
    store: ScopedStore,
    Path(id): Path<ProjectId>,
) -> Result<Json<Project>, ServerError> {
    Ok(Json(store.require_project(id).await?))
}

/// `DELETE /projects/{id}` — remove an empty project. Owned conversations,
/// documents, and root defaults must be removed through their explicit
/// lifecycle APIs first; this boundary never cascades them.
pub async fn delete_project(
    store: ScopedStore,
    Path(id): Path<ProjectId>,
) -> Result<StatusCode, ServerError> {
    match store.delete_project(id).await? {
        DeleteProjectOutcome::Deleted => Ok(StatusCode::NO_CONTENT),
        DeleteProjectOutcome::NotFound => {
            Err(ServerError::not_found(format!("project {id} not found")))
        }
        DeleteProjectOutcome::NotEmpty => Err(ServerError::conflict_kind(
            "project_not_empty",
            "remove the project's conversations, documents, and connected folders before deleting it",
        )),
    }
}

/// Body of `POST /chats`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateChat {
    /// Optional human-facing title.
    #[serde(default)]
    pub title: Option<String>,
    /// Optional project to file this chat under; omitted for a loose chat.
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    /// Optional model for this chat; omitted seeds the sticky default, else
    /// the configured default.
    #[serde(default)]
    pub model: Option<String>,
    /// Optional reasoning-effort override for this chat; honored only by models
    /// that expose the control. Omitted seeds the sticky default.
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Optional permission mode for this chat; omitted seeds the sticky
    /// default (clamped to any managed ceiling), else `ask`.
    #[serde(default)]
    pub permission_mode: Option<PermissionMode>,
    /// Code-execution network access for this conversation workspace.
    /// Omitted seeds the sticky default, else open public-internet access.
    #[serde(default)]
    pub network_policy: Option<openwave_core::NetworkPolicy>,
}

/// Settings keys holding the sticky new-chat defaults: the reader's last
/// explicit per-chat choice at these routes, replayed into the next chat.
///
/// Deployment-scoped like every other setting. `model` deliberately gets its
/// own key rather than reusing the global `model` selection: seeding is a
/// creation-time copy into the new chat, so picking a model in one chat never
/// retargets existing chats that ride the configured default.
const STICKY_MODEL_KEY: &str = "chat_default.model";
const STICKY_REASONING_EFFORT_KEY: &str = "chat_default.reasoning_effort";
const STICKY_PERMISSION_MODE_KEY: &str = "chat_default.permission_mode";
const STICKY_NETWORK_POLICY_KEY: &str = "chat_default.network_policy";

/// Read one sticky new-chat default. A stored value this build no longer
/// recognizes reads as unset rather than failing the create.
async fn read_sticky_default<T: serde::de::DeserializeOwned>(
    store: &dyn Store,
    key: &str,
) -> openwave_core::Result<Option<T>> {
    Ok(store
        .get_setting(key)
        .await?
        .and_then(|value| serde_json::from_value(value).ok()))
}

/// Record (or clear, with `None`) one sticky new-chat default.
async fn write_sticky_default<T: Serialize>(
    store: &dyn Store,
    key: &str,
    value: Option<&T>,
) -> Result<(), ServerError> {
    let value = match value {
        Some(value) => serde_json::to_value(value)
            .map_err(|_| ServerError::internal("could not encode a sticky chat default"))?,
        None => serde_json::Value::Null,
    };
    Ok(store.set_setting(key, &value).await?)
}

/// Read every sticky new-chat default, the permission mode clamped to any
/// managed ceiling — what `POST /chats` will seed, and therefore what a
/// composer should display before the chat exists.
async fn read_sticky_chat_defaults(state: &AppState) -> Result<StickyChatDefaults, ServerError> {
    let store = &*state.store;
    let permission_mode = match read_sticky_default(store, STICKY_PERMISSION_MODE_KEY).await? {
        // The managed ceiling clamps a sticky mode recorded before the
        // policy arrived: a remembered `allow` under an `ask` ceiling seeds
        // (and reads back) `ask`, mirroring the turn gate's treatment of
        // stored over-ceiling modes.
        Some(mode) => crate::managed_policy::resolve(store, &*state.os_policy)
            .await?
            .clamp_permission_mode(Some(mode)),
        None => None,
    };
    Ok(StickyChatDefaults {
        model: read_sticky_default(store, STICKY_MODEL_KEY).await?,
        reasoning_effort: read_sticky_default(store, STICKY_REASONING_EFFORT_KEY).await?,
        permission_mode,
        network_policy: read_sticky_default(store, STICKY_NETWORK_POLICY_KEY).await?,
    })
}

/// `POST /chats` — create a chat and return it (`201 Created`).
///
/// Fields the request leaves unspecified seed from the sticky defaults — the
/// reader's last explicit choice at these same routes — so a new chat starts
/// the way the reader configured the previous one instead of resetting to the
/// hard defaults. A brand-new install has no sticky state and keeps today's
/// defaults (`ask`, open network, configured model).
pub async fn create_chat(
    State(state): State<AppState>,
    store: ScopedStore,
    Json(mut body): Json<CreateChat>,
) -> Result<impl IntoResponse, ServerError> {
    // Return a product-facing 400 for an unknown project. The Store and schema
    // independently enforce the same membership invariant inside insertion.
    if let Some(project_id) = body.project_id {
        store.require_project(project_id).await?;
    }
    if let Some(model) = body.model.as_mut() {
        *model = validate_model_selection(&state, model, false).await?;
    }
    refuse_permission_mode_over_ceiling(&state, body.permission_mode).await?;
    if let Some(policy) = body.network_policy.as_mut() {
        crate::code_execution::normalize_network_policy(policy)?;
    }
    // An explicit choice at creation is as much "the last-chosen mode" as one
    // made mid-chat — the home composer's pickers land here, never at PATCH —
    // so record it the same way. Absent fields never clear a sticky default;
    // only an explicit PATCH `null` does.
    if let Some(model) = &body.model {
        write_sticky_default(&*state.store, STICKY_MODEL_KEY, Some(model)).await?;
    }
    if let Some(effort) = &body.reasoning_effort {
        write_sticky_default(&*state.store, STICKY_REASONING_EFFORT_KEY, Some(effort)).await?;
    }
    if let Some(mode) = &body.permission_mode {
        write_sticky_default(&*state.store, STICKY_PERMISSION_MODE_KEY, Some(mode)).await?;
    }
    if let Some(policy) = &body.network_policy {
        write_sticky_default(&*state.store, STICKY_NETWORK_POLICY_KEY, Some(policy)).await?;
    }
    let model = match body.model {
        Some(model) => Some(model),
        // A sticky selection that no longer validates — deregistered model,
        // disabled or uncredentialed provider — falls back to the configured
        // default instead of failing the create or pinning a dead model.
        None => match read_sticky_default::<String>(&*state.store, STICKY_MODEL_KEY).await? {
            Some(sticky) => validate_model_selection(&state, &sticky, false).await.ok(),
            None => None,
        },
    };
    let reasoning_effort = match body.reasoning_effort {
        Some(effort) => Some(effort),
        None => read_sticky_default(&*state.store, STICKY_REASONING_EFFORT_KEY).await?,
    };
    let permission_mode = match body.permission_mode {
        Some(mode) => Some(mode),
        // The managed ceiling clamps a sticky mode recorded before the policy
        // arrived: a remembered `allow` under an `ask` ceiling seeds `ask`,
        // mirroring how the turn gate treats stored over-ceiling modes.
        None => match read_sticky_default(&*state.store, STICKY_PERMISSION_MODE_KEY).await? {
            Some(sticky) => crate::managed_policy::resolve(&*state.store, &*state.os_policy)
                .await?
                .clamp_permission_mode(Some(sticky)),
            None => None,
        },
    };
    let network_policy = match body.network_policy {
        Some(policy) => policy,
        None => {
            let mut sticky = read_sticky_default(&*state.store, STICKY_NETWORK_POLICY_KEY)
                .await?
                .unwrap_or_default();
            // Stored values were normalized at write; a stale one that no
            // longer passes falls back to the product default rather than
            // failing the create.
            if crate::code_execution::normalize_network_policy(&mut sticky).is_err() {
                sticky = openwave_core::NetworkPolicy::default();
            }
            sticky
        }
    };
    let chat = Chat {
        id: ChatId::new(),
        project_id: body.project_id,
        title: normalize_chat_title(body.title)?,
        model,
        reasoning_effort,
        permission_mode,
        network_policy,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    let chat = store.create_chat_with_project_defaults(&chat).await?;
    Ok((StatusCode::CREATED, Json(chat)))
}

/// Body of `PATCH /chats/{id}`. A double option (like `PUT /settings`): absent
/// leaves the model unchanged, `null` clears it (fall back to the default), and a
/// value sets it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatUpdate {
    /// An explicit `null` clears the title. Non-empty titles are trimmed before
    /// persistence so sidebar labels remain stable across clients.
    #[serde(default, deserialize_with = "double_option")]
    pub title: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub model: Option<Option<String>>,
    /// An explicit `null` clears the reasoning-effort override; a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub reasoning_effort: Option<Option<ReasoningEffort>>,
    /// An explicit `null` clears the permission mode (back to `ask`); a value
    /// sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub permission_mode: Option<Option<PermissionMode>>,
    /// Replace the code-execution network policy. Omitted leaves it unchanged.
    #[serde(default)]
    pub network_policy: Option<openwave_core::NetworkPolicy>,
}

/// `PATCH /chats/{id}` — update the human-facing title and/or model selection.
pub async fn patch_chat(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(id): Path<ChatId>,
    Json(mut body): Json<ChatUpdate>,
) -> Result<Json<Chat>, ServerError> {
    // Validate every supplied field before touching durable state. This keeps a
    // mixed request all-or-nothing from the user's point of view.
    if let Some(Some(model)) = body.model.as_mut() {
        *model = validate_model_selection(&state, model, false).await?;
    }
    if let Some(policy) = body.network_policy.as_mut() {
        crate::code_execution::normalize_network_policy(policy)?;
    }
    // A `null` (clear back to the default) is always allowed: the ceiling
    // caps what the reader may select, and the turn gate clamps whatever the
    // default resolves to.
    refuse_permission_mode_over_ceiling(&state, body.permission_mode.flatten()).await?;
    let title = body.title.map(normalize_chat_title).transpose()?;

    let mut chat = store.require_chat(id).await?;

    if !store
        .update_chat_metadata(
            id,
            title.clone(),
            body.model.clone(),
            body.reasoning_effort,
            body.permission_mode,
            body.network_policy.clone(),
        )
        .await?
    {
        return Err(ServerError::not_found(format!("chat {id} not found")));
    }
    // Each explicit choice here becomes the sticky default a new chat seeds
    // from; an explicit clear (`null`) clears the sticky default the same way,
    // back to the hard default. Recorded server-side so every client benefits.
    if let Some(model) = &body.model {
        write_sticky_default(&*state.store, STICKY_MODEL_KEY, model.as_ref()).await?;
    }
    if let Some(effort) = &body.reasoning_effort {
        write_sticky_default(&*state.store, STICKY_REASONING_EFFORT_KEY, effort.as_ref()).await?;
    }
    if let Some(mode) = &body.permission_mode {
        write_sticky_default(&*state.store, STICKY_PERMISSION_MODE_KEY, mode.as_ref()).await?;
    }
    if let Some(policy) = &body.network_policy {
        write_sticky_default(&*state.store, STICKY_NETWORK_POLICY_KEY, Some(policy)).await?;
    }
    if let Some(title) = title {
        chat.title = title;
    }
    if let Some(model) = body.model {
        chat.model = model;
    }
    if let Some(reasoning_effort) = body.reasoning_effort {
        chat.reasoning_effort = reasoning_effort;
    }
    if let Some(permission_mode) = body.permission_mode {
        chat.permission_mode = permission_mode;
    }
    if let Some(network_policy) = body.network_policy {
        chat.network_policy = network_policy;
    }
    Ok(Json(chat))
}

/// A renderer-safe durable transcript entry. Internal routing and tool state
/// deliberately remain behind the server boundary.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct ChatMessageSnapshot {
    pub id: MessageId,
    pub role: TranscriptRole,
    pub content: String,
    pub created_at: chrono::DateTime<Utc>,
    pub citations: Vec<openwave_core::AssistantCitationSnapshot>,
    /// Images submitted with this user message. These are durable identity and
    /// geometry only; image bytes remain behind a chat-scoped authenticated
    /// endpoint and never enter the transcript payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub image_attachments: Option<Vec<TranscriptImageAttachment>>,
    /// Files submitted with this user message. Their bytes remain behind the
    /// existing chat-scoped document endpoints.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub file_attachments: Option<Vec<TranscriptFileAttachment>>,
}

/// One renderer-safe image identity attached to a historical user message.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct TranscriptImageAttachment {
    /// Content-addressed opaque attachment identity, not a host path.
    pub attachment_id: uuid::Uuid,
    /// Sniffed IANA media type from the trusted image ingest boundary.
    pub media_type: String,
    /// Header-derived dimensions, bounded at image publication.
    pub width: u32,
    pub height: u32,
}

impl From<openwave_core::MessageAttachment> for TranscriptImageAttachment {
    fn from(attachment: openwave_core::MessageAttachment) -> Self {
        Self {
            attachment_id: attachment.image.blob_id,
            media_type: attachment.image.media_type.as_str().to_owned(),
            width: attachment.image.width,
            height: attachment.image.height,
        }
    }
}

/// One renderer-safe source document attached to a historical user message.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct TranscriptFileAttachment {
    pub document_id: DocumentId,
    pub name: String,
    pub media_type: String,
}

impl From<openwave_core::MessageDocumentAttachment> for TranscriptFileAttachment {
    fn from(attachment: openwave_core::MessageDocumentAttachment) -> Self {
        Self {
            document_id: attachment.document_id,
            name: attachment.title.unwrap_or_else(|| "Attachment".to_owned()),
            media_type: attachment.media_type,
        }
    }
}

/// One terminal turn's renderer-safe status and visible streamed content.
///
/// A completed turn points at its authoritative assistant message. Failed and
/// cancelled turns have no message, but remain first-class transcript entries
/// carrying the partial prose and reasoning the reader already saw live.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct ChatTerminalTurnSnapshot {
    pub turn_id: TurnId,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message_id: Option<MessageId>,
    pub status: ChatTerminalTurnStatus,
    pub partial_content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub refusal: Option<crate::event_projection::RendererRefusal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub failure_category: Option<crate::event_projection::TurnFailureCategory>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub failure_model: Option<crate::event_projection::RendererModelIdentity>,
    pub file_changes: Vec<ExecFileChangeSummary>,
    /// Skills the user explicitly invoked for this turn, in submitted order.
    /// Absent for the ordinary turn that invoked none.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub invoked_skills: Option<Vec<String>>,
    pub finished_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ChatTerminalTurnStatus {
    Completed,
    Failed,
    Cancelled,
}

impl From<openwave_core::ChatTerminalTurnSnapshot> for ChatTerminalTurnSnapshot {
    fn from(snapshot: openwave_core::ChatTerminalTurnSnapshot) -> Self {
        let status = match snapshot.status {
            openwave_core::ChatTerminalTurnStatus::Completed => ChatTerminalTurnStatus::Completed,
            openwave_core::ChatTerminalTurnStatus::Failed => ChatTerminalTurnStatus::Failed,
            openwave_core::ChatTerminalTurnStatus::Cancelled => ChatTerminalTurnStatus::Cancelled,
        };
        let failure_category = matches!(status, ChatTerminalTurnStatus::Failed).then(|| {
            crate::event_projection::TurnFailureCategory::from_kind(
                snapshot.failure_kind.as_deref().unwrap_or_default(),
            )
        });
        let failure_model =
            failure_category.and_then(|_| crate::event_projection::model_identity(&snapshot.model));
        Self {
            turn_id: snapshot.turn_id,
            message_id: snapshot.message_id,
            status,
            partial_content: snapshot.partial_content,
            reasoning: (!snapshot.reasoning.trim().is_empty()).then_some(snapshot.reasoning),
            refusal: snapshot.refusal.as_ref().map(Into::into),
            failure_category,
            failure_model,
            file_changes: Vec::new(),
            invoked_skills: (!snapshot.invoked_skills.is_empty())
                .then_some(snapshot.invoked_skills),
            finished_at: snapshot.finished_at,
        }
    }
}

/// One visible transcript plus the durable journal watermark that produced it.
/// The renderer uses the watermark to subscribe only to future events, avoiding
/// duplicate text when reopening a completed conversation.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct ChatTranscript {
    pub messages: Vec<ChatMessageSnapshot>,
    /// Finished tool activity from terminal turns, projected through a fixed
    /// renderer-safe allowlist. Canonical tool records never cross this API.
    pub tool_activity: Vec<openwave_core::ChatToolActivitySnapshot>,
    /// Status and streamed presentation for every terminal turn. This owns
    /// terminal metadata even when no assistant message was committed.
    pub terminal_turns: Vec<ChatTerminalTurnSnapshot>,
    pub last_event_seq: i64,
}

/// The roles a visible transcript entry can have.
///
/// Narrower than [`Role`] on purpose. The transcript shows the conversation, not
/// the model's plumbing, so `System` and `Tool` never appear — and that was
/// previously guaranteed only by a `matches!` filter at the one call site, while
/// the snapshot's own type still admitted all four. The renderer mirrored the
/// narrow version and branched on `assistant` with no third arm, so a `system`
/// entry reaching it would have rendered as a user message.
///
/// Encoding it here makes the guarantee the type's rather than the caller's, and
/// makes a new [`Role`] variant a decision in [`Self::for_transcript`] instead of
/// something that silently appears in the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptRole {
    User,
    Assistant,
    /// A durable host-authored note — e.g. "User restored output 'report.md'…"
    /// — written between turns so the model's next turn learns what happened.
    /// Shown as a subtle inline notice, never as a user or assistant bubble.
    System,
}

impl TranscriptRole {
    /// `None` for roles the transcript does not show.
    fn for_transcript(role: Role) -> Option<Self> {
        match role {
            Role::User => Some(Self::User),
            Role::Assistant => Some(Self::Assistant),
            Role::System => Some(Self::System),
            Role::Tool => None,
        }
    }
}

impl ChatMessageSnapshot {
    /// `None` when the message is not part of the visible conversation.
    ///
    /// Replaces a separate `matches!` filter followed by an infallible
    /// conversion: the two could disagree, and only the filter was enforcing the
    /// narrowing the type claimed.
    fn for_transcript(message: StoredMessage) -> Option<Self> {
        Some(Self {
            id: message.id,
            role: TranscriptRole::for_transcript(message.role)?,
            content: message.content,
            created_at: message.created_at,
            citations: Vec::new(),
            image_attachments: None,
            file_attachments: None,
        })
    }
}

/// `GET /chats/{id}/messages` — replay the visible durable transcript in
/// commit order. The existence check prevents a missing chat from looking like
/// an empty conversation.
pub async fn list_chat_messages(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(id): Path<ChatId>,
) -> Result<Json<ChatTranscript>, ServerError> {
    let transcript = store
        .get_chat_transcript(id)
        .await?
        .ok_or_else(|| ServerError::not_found(format!("chat {id} not found")))?;
    let mut citations_by_message = std::collections::HashMap::new();
    for citation in transcript.citations {
        citations_by_message
            .entry(citation.message_id)
            .or_insert_with(Vec::new)
            .push(citation.citation);
    }
    let mut image_attachments_by_message = std::collections::HashMap::new();
    for attachment in transcript.message_attachments {
        image_attachments_by_message
            .entry(attachment.message_id)
            .or_insert_with(Vec::new)
            .push(TranscriptImageAttachment::from(attachment));
    }
    let mut file_attachments_by_message = std::collections::HashMap::new();
    for attachment in transcript.message_document_attachments {
        file_attachments_by_message
            .entry(attachment.message_id)
            .or_insert_with(Vec::new)
            .push(TranscriptFileAttachment::from(attachment));
    }
    let messages = transcript
        .messages
        .into_iter()
        .filter_map(|message| {
            let mut snapshot = ChatMessageSnapshot::for_transcript(message)?;
            snapshot.citations = citations_by_message
                .remove(&snapshot.id)
                .unwrap_or_default();
            if snapshot.role == TranscriptRole::User {
                let image_attachments = image_attachments_by_message
                    .remove(&snapshot.id)
                    .unwrap_or_default();
                snapshot.image_attachments =
                    (!image_attachments.is_empty()).then_some(image_attachments);
                let file_attachments = file_attachments_by_message
                    .remove(&snapshot.id)
                    .unwrap_or_default();
                snapshot.file_attachments =
                    (!file_attachments.is_empty()).then_some(file_attachments);
            }
            Some(snapshot)
        })
        .collect();
    let mut file_changes_by_turn =
        match list_file_change_summaries(&*state.store, &*state.blobs, id).await {
            Ok(summaries) => summaries,
            Err(error) => {
                tracing::warn!(
                    chat = %id,
                    %error,
                    "could not load connected-folder change summaries"
                );
                std::collections::HashMap::new()
            }
        };
    Ok(Json(ChatTranscript {
        messages,
        tool_activity: transcript.tool_activity,
        terminal_turns: transcript
            .terminal_turns
            .into_iter()
            .map(|turn| {
                let mut snapshot = ChatTerminalTurnSnapshot::from(turn);
                snapshot.file_changes = file_changes_by_turn
                    .remove(&snapshot.turn_id)
                    .unwrap_or_default();
                snapshot
            })
            .collect(),
        last_event_seq: transcript.last_event_seq,
    }))
}

/// `GET /chats` — list chats, most-recently-created first.
pub async fn list_chats(store: ScopedStore) -> Result<Json<Vec<Chat>>, ServerError> {
    Ok(Json(store.list_chats().await?))
}

/// `GET /chats/{id}` — fetch one chat, or `404`.
pub async fn get_chat(
    store: ScopedStore,
    Path(id): Path<ChatId>,
) -> Result<Json<Chat>, ServerError> {
    Ok(Json(store.require_chat(id).await?))
}

/// `DELETE /chats/{id}` — remove a quiesced conversation and its product
/// history. Rooted or active conversations deliberately return a conflict: the
/// caller must first finish cancellation and durable broker detachment.
pub async fn delete_chat(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(id): Path<ChatId>,
) -> Result<StatusCode, ServerError> {
    match store.delete_chat(id).await? {
        DeleteChatOutcome::Deleted => {
            state.blob_retirement_wake.notify_one();
            let scratch_root = state.config.data_dir.join("scratch");
            let cleanup =
                tokio::task::spawn_blocking(move || remove_private_chat_scratch(&scratch_root, id))
                    .await;
            match cleanup {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    eprintln!("openwave: could not remove private scratch for chat {id}: {error}");
                }
                Err(error) => {
                    eprintln!(
                        "openwave: private scratch cleanup task stopped for chat {id}: {error}"
                    );
                }
            }
            Ok(StatusCode::NO_CONTENT)
        }
        DeleteChatOutcome::NotFound => Err(ServerError::not_found(format!("chat {id} not found"))),
        DeleteChatOutcome::ActiveWork => Err(ServerError::conflict_kind(
            "chat_active",
            "finish or cancel the active work before deleting this conversation",
        )),
        DeleteChatOutcome::RootsAttached => Err(ServerError::conflict_kind(
            "chat_roots_attached",
            "detach connected folders before deleting this conversation",
        )),
        DeleteChatOutcome::RootAttachmentStateUnresolved => Err(ServerError::conflict_kind(
            "chat_root_attachment_unresolved",
            "a connected-folder change is still finishing; try deleting again in a moment",
        )),
    }
}

/// Remove a deleted chat's private scratch without following a replacement
/// root or chat-directory symlink. Database deletion remains authoritative, so
/// callers log cleanup failure rather than turning a committed delete into an
/// ambiguous HTTP failure.
fn remove_private_chat_scratch(root: &FsPath, id: ChatId) -> std::io::Result<()> {
    let root_metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "private scratch root is not a regular directory",
        ));
    }
    let directory = Dir::open_ambient_dir(root, ambient_authority())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let opened = directory.dir_metadata()?;
        if root_metadata.dev() != cap_std::fs::MetadataExt::dev(&opened)
            || root_metadata.ino() != cap_std::fs::MetadataExt::ino(&opened)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "private scratch root changed while it was opened",
            ));
        }
    }
    let chat_name = id.to_string();
    match directory.symlink_metadata(&chat_name) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            directory.remove_dir_all(chat_name)
        }
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "private chat scratch is not a regular directory",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Renderer-safe state for one agent run.
///
/// Worker lease tokens, scheduling budgets, and other executor-facing fields
/// intentionally remain inside the server/store boundary.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct AgentRunSnapshot {
    pub id: openwave_core::AgentRunId,
    pub parent_id: Option<openwave_core::AgentRunId>,
    pub tier: AgentRunTier,
    pub execution_location: AgentRunExecutionLocation,
    pub status: AgentRunStatus,
    /// The exact bounded task delegated by the visible spawn step.
    pub task: Option<String>,
    pub started_at: Option<chrono::DateTime<Utc>>,
    pub finished_at: Option<chrono::DateTime<Utc>>,
    /// Stable, bounded classification suitable for renderer display.
    pub last_error_code: Option<String>,
    /// The currently checkpointed, renderer-safe activity, if any.
    ///
    /// This is intentionally a small fixed vocabulary. It never exposes tool
    /// arguments, results, provider call identities, executor leases, or raw
    /// executor diagnostics.
    pub activity: Option<AgentActivitySnapshot>,
    /// Files a background run submitted as its deliverables, in its own order.
    ///
    /// A background run produces outputs by writing files and submitting them
    /// by name; nothing here is host-authored, and a run that submitted nothing
    /// carries an empty list.
    pub submitted_outputs: Vec<SubmittedOutputSnapshot>,
    /// Bounded terminal display text returned to the parent, if settled.
    pub terminal_text: Option<String>,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
    // This is an OpenWave call id, not a provider call identity. It lets a
    // transcript observer attach this durable status to its exact spawning
    // step without exposing delegated input or executor data.
    pub spawn_call_id: Option<openwave_core::CallId>,
}

impl AgentRunSnapshot {
    fn from_run(
        run: AgentRun,
        activity: Option<AgentActivitySnapshot>,
        terminal_text: Option<String>,
        submitted_outputs: Vec<SubmittedOutputSnapshot>,
    ) -> Self {
        Self {
            id: run.id,
            parent_id: run.parent_id,
            spawn_call_id: run.spawn_call_id,
            tier: run.tier,
            execution_location: run.execution_location,
            status: run.status,
            task: run.input,
            started_at: run.started_at,
            finished_at: run.finished_at,
            last_error_code: run.last_error_code,
            activity,
            submitted_outputs,
            terminal_text,
            created_at: run.created_at,
            updated_at: run.updated_at,
        }
    }
}

/// One file a background run submitted, as the renderer sees it.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
pub struct SubmittedOutputSnapshot {
    pub output_id: openwave_core::OutputId,
    /// The name the run gave the file, which is the output's name.
    pub filename: String,
}

/// Fixed, renderer-safe names for supported live work.
///
/// Adding a durable tool does not automatically expose it to a renderer: it
/// must be deliberately admitted here with a safe label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivityKind {
    Exec,
    WebSearch,
    ReadDelegatedFile,
    ListConnectedFolders,
    ListFolder,
    ReadConnectedFile,
    ImportConnectedFile,
}

/// Coarse checkpoint lifecycle suitable for display.
///
/// This intentionally does not mirror all durable executor states; only live
/// work is represented, and terminal checkpoints produce no activity.
#[derive(Debug, Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivityStatus {
    Waiting,
    Running,
}

/// Renderer-safe projection of one live supported checkpoint.
#[derive(Debug, Clone, Copy, Serialize, ts_rs::TS)]
pub struct AgentActivitySnapshot {
    pub kind: AgentActivityKind,
    pub status: AgentActivityStatus,
}

fn sandbox_activity(calls: &[SandboxToolCall]) -> Option<AgentActivitySnapshot> {
    // A sandbox can have at most one live checkpoint. Inspecting the latest
    // durable live checkpoint also prevents an older, completed activity from
    // lingering in the UI after the run has advanced.
    let call = calls.iter().rev().find(|call| !call.status.is_terminal())?;
    let kind = match call.name.as_str() {
        openwave_core::SANDBOX_EXEC_TOOL => AgentActivityKind::Exec,
        "web_search" => AgentActivityKind::WebSearch,
        openwave_core::SANDBOX_READ_DELEGATED_FILE_TOOL => AgentActivityKind::ReadDelegatedFile,
        // Unknown tool names are executor data, not a renderer API contract.
        _ => return None,
    };
    let status = match call.status {
        SandboxToolCallStatus::Accepted | SandboxToolCallStatus::RetryWait => {
            AgentActivityStatus::Waiting
        }
        SandboxToolCallStatus::Claimed => AgentActivityStatus::Running,
        SandboxToolCallStatus::Completed
        | SandboxToolCallStatus::Failed
        | SandboxToolCallStatus::Cancelled => return None,
        _ => return None,
    };
    Some(AgentActivitySnapshot { kind, status })
}

/// Coarse, renderer-safe lifecycle for one historical activity entry.
///
/// Unlike [`AgentActivityStatus`], which only names live work, this also
/// admits the three terminal outcomes so a settled step can be shown in an
/// ordered timeline. It carries no failure detail: a failed step is only
/// "failed", never why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivityOutcome {
    Waiting,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// One renderer-safe entry in a background run's ordered activity history.
///
/// Built on read from durable sandbox tool calls and their immutable receipts.
/// `detail` admits bounded model-authored command/argument/query text, which may
/// repeat anything the child already saw and is not covered by the host-field
/// non-disclosure guarantee. Stored result text is copied in one place only: a
/// settled `exec` step carries its receipt's bounded tail, because that text is
/// the command's own output from a private workspace and is what makes a failed
/// step readable. Web-search and delegated-file results stay server-side. The
/// other host-derived values are the numeric exit code parsed from a receipt's
/// first line and the delegated file's leaf name. Full broker paths and root
/// identities, provider identities, executor leases, and diagnostics are never
/// copied.
///
/// No separate activity-history shape is persisted. The optional field keeps
/// the wire additive for older clients and lets calls without derivable detail
/// retain the original `{kind, outcome, at}` shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct AgentActivityHistoryItem {
    pub kind: AgentActivityKind,
    pub outcome: AgentActivityOutcome,
    pub at: chrono::DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub detail: Option<openwave_core::AgentActivityDetail>,
}

/// Project a background run's durable sandbox tool calls into an ordered,
/// renderer-safe activity history.
///
/// The store returns calls in durable creation order, so the projection keeps
/// that order. Tool names outside the admitted vocabulary are executor data,
/// not a renderer contract, and are skipped rather than leaked as raw labels.
/// `delegated_file` is the run's one admission-delegated file identity, when it
/// had one; only its base name may reach a `read_delegated_file` entry. The
/// receipt map contains only terminal exec receipts, used to recover the typed
/// exit code from their first line and a bounded tail of what the command
/// printed.
fn sandbox_activity_history(
    calls: &[SandboxToolCall],
    delegated_file: Option<&str>,
    receipts: &std::collections::HashMap<CallId, openwave_core::SandboxToolCallReceipt>,
) -> Vec<AgentActivityHistoryItem> {
    calls
        .iter()
        .filter_map(|call| sandbox_activity_history_item(call, delegated_file, receipts))
        .collect()
}

fn sandbox_activity_history_item(
    call: &SandboxToolCall,
    delegated_file: Option<&str>,
    receipts: &std::collections::HashMap<CallId, openwave_core::SandboxToolCallReceipt>,
) -> Option<AgentActivityHistoryItem> {
    let kind = match call.name.as_str() {
        openwave_core::SANDBOX_EXEC_TOOL => AgentActivityKind::Exec,
        "web_search" => AgentActivityKind::WebSearch,
        openwave_core::SANDBOX_READ_DELEGATED_FILE_TOOL => AgentActivityKind::ReadDelegatedFile,
        // Unknown tool names are executor data, not a renderer API contract.
        _ => return None,
    };
    // A terminal step is dated by when it resolved; a live step by when it was
    // admitted. `resolved_at` is always present once terminal, but fall back to
    // the creation time rather than dropping a settled step from the timeline.
    let (outcome, at) = match call.status {
        SandboxToolCallStatus::Accepted | SandboxToolCallStatus::RetryWait => {
            (AgentActivityOutcome::Waiting, call.created_at)
        }
        SandboxToolCallStatus::Claimed => (AgentActivityOutcome::Running, call.created_at),
        SandboxToolCallStatus::Completed => (
            AgentActivityOutcome::Completed,
            call.resolved_at.unwrap_or(call.created_at),
        ),
        SandboxToolCallStatus::Failed => (
            AgentActivityOutcome::Failed,
            call.resolved_at.unwrap_or(call.created_at),
        ),
        SandboxToolCallStatus::Cancelled => (
            AgentActivityOutcome::Cancelled,
            call.resolved_at.unwrap_or(call.created_at),
        ),
        // `SandboxToolCallStatus` is non-exhaustive; an unrecognized future
        // state is executor data, not a renderer contract.
        _ => return None,
    };
    // The delegated read is argument-free, so its headline comes from the
    // admission rather than from model-authored arguments. Every other detail
    // is a bounded projection of the durable call and optional receipt.
    let detail = match call.name.as_str() {
        openwave_core::SANDBOX_READ_DELEGATED_FILE_TOOL => {
            delegated_file.and_then(openwave_core::AgentActivityDetail::delegated_file)
        }
        _ => openwave_core::AgentActivityDetail::build(&call.name, &call.arguments).map(|detail| {
            match receipts.get(&call.id) {
                Some(receipt) => detail.with_exec_result(&receipt.result),
                None => detail,
            }
        }),
    };
    Some(AgentActivityHistoryItem {
        kind,
        outcome,
        at,
        detail,
    })
}

fn foreground_activity(
    calls: &[ToolCallRecord],
    now: chrono::DateTime<Utc>,
) -> Option<AgentActivitySnapshot> {
    // A foreground turn can park on exactly one client tool call. Looking at
    // the latest live supported call means a completed folder operation never
    // lingers after its continuation advances.
    let call = calls.iter().rev().find(|call| {
        call.execution == ToolCallExecution::Client && call.status == ToolCallStatus::Pending
    })?;
    let kind = match call.name.as_str() {
        "list_connected_folders" => AgentActivityKind::ListConnectedFolders,
        "list_folder" => AgentActivityKind::ListFolder,
        "read_connected_file" => AgentActivityKind::ReadConnectedFile,
        "import_connected_file" => AgentActivityKind::ImportConnectedFile,
        // Unknown client tools are executor data, not a renderer API contract.
        _ => return None,
    };
    let status = if call
        .client_lease_expires_at
        .is_some_and(|expires_at| expires_at > now)
    {
        AgentActivityStatus::Running
    } else {
        AgentActivityStatus::Waiting
    };
    Some(AgentActivitySnapshot { kind, status })
}

/// `GET /chats/{id}/agent-runs` — list renderer-safe execution state.
pub async fn list_agent_runs(
    store: ScopedStore,
    Path(id): Path<ChatId>,
) -> Result<Json<Vec<AgentRunSnapshot>>, ServerError> {
    store.require_chat(id).await?;
    let runs = store.list_agent_runs(id).await?;
    // This read model needs only live client checkpoints. Loading the complete
    // tool-call transcript here would needlessly deserialize historical model
    // arguments, results, and local diagnostics just to render current work.
    let client_calls = store.list_pending_client_tool_calls(id).await?;
    let now = Utc::now();
    let mut snapshots = Vec::with_capacity(runs.len());
    for run in runs {
        let mut submitted_outputs = Vec::new();
        let terminal_text = match run.status {
            AgentRunStatus::Completed | AgentRunStatus::Cancelled => {
                match store.get_agent_run_result(run.id).await? {
                    Some(result) => match &result.payload {
                        // A submission's names belong to the structured field,
                        // where the reader can open each one. Repeating them in
                        // the prose block would say the same thing twice, so
                        // the text here is the run's summary alone.
                        openwave_core::AgentRunResultPayload::Submission { outputs, summary } => {
                            submitted_outputs.extend(outputs.iter().map(|output| {
                                SubmittedOutputSnapshot {
                                    output_id: output.output_id,
                                    filename: output.filename.clone(),
                                }
                            }));
                            Some(summary.clone())
                        }
                        _ => Some(result.text),
                    },
                    None => None,
                }
            }
            AgentRunStatus::Failed => run
                .last_error_code
                .as_deref()
                .map(|code| format!("Sandbox task failed ({code})")),
            _ => None,
        };
        let activity = if run.tier == AgentRunTier::Background {
            let calls = store.list_sandbox_tool_calls_for_agent_run(run.id).await?;
            sandbox_activity(&calls)
        } else if run.tier == AgentRunTier::Foreground {
            foreground_activity(&client_calls, now)
        } else {
            None
        };
        snapshots.push(AgentRunSnapshot::from_run(
            run,
            activity,
            terminal_text,
            submitted_outputs,
        ));
    }
    Ok(Json(snapshots))
}

/// `GET /chats/{id}/agent-runs/{run_id}/activity` — ordered, renderer-safe
/// activity history for one background run.
///
/// This is the durable companion to the live `activity` field on a run
/// snapshot: where that field names only the single current checkpoint, this
/// returns every admitted step in order, each with a coarse terminal outcome
/// and timestamp. Each entry may add a bounded typed headline — the command,
/// exit status, and output tail a settled exec recorded, the query a web search
/// asked, or the base name of the run's one delegated file. Command, argument,
/// and query text is model-authored and may repeat information the child
/// already saw. The exec output tail is the one stored result the projection
/// copies: it is the command's own text from a private workspace. Web-search
/// and delegated-file results and host-only fields are not copied, apart from
/// the typed exit code and admitted leaf name. A missing, wrong-chat, or
/// foreground run returns `404` rather than revealing whether an unrelated run
/// identifier exists.
pub async fn list_agent_run_activity(
    store: ScopedStore,
    Path((chat_id, run_id)): Path<(ChatId, openwave_core::AgentRunId)>,
) -> Result<Json<Vec<AgentActivityHistoryItem>>, ServerError> {
    store.require_chat(chat_id).await?;
    let run = store
        .get_agent_run(run_id)
        .await?
        .filter(|run| run.chat_id == chat_id && run.tier == AgentRunTier::Background);
    let Some(run) = run else {
        return Err(ServerError::not_found(format!(
            "agent run {run_id} not found"
        )));
    };
    let calls = store.list_sandbox_tool_calls_for_agent_run(run.id).await?;
    // The run's one delegated file identity is needed only for its argument-free
    // read call. A missing admission leaves that entry in the original
    // detail-free shape rather than dropping the history.
    let delegated_file = if calls
        .iter()
        .any(|call| call.name == openwave_core::SANDBOX_READ_DELEGATED_FILE_TOOL)
    {
        store
            .get_sandbox_agent_admission(run.id)
            .await?
            .and_then(|admission| admission.resource)
            .map(|resource| resource.relative_path)
    } else {
        None
    };
    // Exit status and the printed tail live on the immutable receipts, not the
    // call rows. A missing receipt — a live step, or a call settled before
    // receipts were kept — leaves the detail without them rather than failing.
    let mut receipts = std::collections::HashMap::new();
    for call in &calls {
        if call.name == openwave_core::SANDBOX_EXEC_TOOL && call.status.is_terminal() {
            if let Some(receipt) = store.get_sandbox_tool_call_receipt(call.id).await? {
                receipts.insert(call.id, receipt);
            }
        }
    }
    Ok(Json(sandbox_activity_history(
        &calls,
        delegated_file.as_deref(),
        &receipts,
    )))
}

/// One line of live progress a background run published, as the renderer sees
/// it.
///
/// The text is the run's own bounded narration — the same class of prose the
/// terminal `terminal_text` already carries, published while the run is still
/// working instead of only at the end. It is model-authored and may repeat
/// information the run already saw. Stored tool records and host-owned fields
/// are not copied directly into it. Typed activity headlines are projected
/// separately.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
pub struct AgentRunProgressLine {
    /// Monotonic per-run ordering. Pass the page's `next_sequence` back as
    /// `after_sequence` to read only what has arrived since.
    pub sequence: i64,
    pub text: String,
    pub at: chrono::DateTime<Utc>,
}

/// One resumable page of a background run's live progress.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct AgentRunProgressPage {
    pub entries: Vec<AgentRunProgressLine>,
    /// The cursor to resume from: the highest sequence in this page, or the
    /// requested cursor when the page is empty. A reader that polls with this
    /// value never re-reads a line it already has.
    pub next_sequence: i64,
}

/// Query for `GET /chats/{chat_id}/agent-runs/{run_id}/progress`.
#[derive(Debug, Deserialize)]
pub struct AgentRunProgressQuery {
    /// Return only lines strictly newer than this sequence; `0` (the default)
    /// starts from the oldest line retention still holds.
    #[serde(default)]
    pub after_sequence: i64,
    /// Maximum lines to return, clamped to the read model's own bound.
    pub limit: Option<u64>,
}

/// `GET /chats/{chat_id}/agent-runs/{run_id}/progress` — the resumable live
/// progress stream for one background run.
///
/// The run snapshot says what state a child is in and the activity projections
/// say which step it is on; neither says what the child is actually doing. This
/// is that: the ordered lines the run itself published, readable while it is
/// still running rather than only once it submits a result. Because each line
/// carries a monotonic sequence, an observer polls with the cursor it last saw
/// and receives only what is new.
///
/// Read-only, and bound to the exact chat: a missing, wrong-chat, or foreground
/// run returns `404` rather than revealing whether an unrelated run identifier
/// exists.
pub async fn list_agent_run_progress(
    store: ScopedStore,
    Path((chat_id, run_id)): Path<(ChatId, openwave_core::AgentRunId)>,
    Query(query): Query<AgentRunProgressQuery>,
) -> Result<Json<AgentRunProgressPage>, ServerError> {
    store.require_chat(chat_id).await?;
    let run = store
        .get_agent_run(run_id)
        .await?
        .filter(|run| run.chat_id == chat_id && run.tier == AgentRunTier::Background);
    if run.is_none() {
        return Err(ServerError::not_found(format!(
            "agent run {run_id} not found"
        )));
    }
    let after_sequence = query.after_sequence.max(0);
    let limit = query
        .limit
        .unwrap_or(openwave_core::AgentRunProgressEntry::DEFAULT_PAGE);
    let entries = store
        .list_agent_run_progress(run_id, after_sequence, limit)
        .await?;
    let next_sequence = entries
        .last()
        .map_or(after_sequence, |entry| entry.sequence);
    Ok(Json(AgentRunProgressPage {
        entries: entries
            .into_iter()
            .map(|entry| AgentRunProgressLine {
                sequence: entry.sequence,
                text: entry.text,
                at: entry.created_at,
            })
            .collect(),
        next_sequence,
    }))
}

/// Closed renderer-safe acknowledgement for sandbox cancellation.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct AgentRunCancellationSnapshot {
    pub id: openwave_core::AgentRunId,
    pub status: AgentRunCancellationStatus,
}

#[derive(Debug, Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunCancellationStatus {
    Cancelling,
    Cancelled,
}

/// `POST /chats/{chat_id}/agent-runs/{run_id}/cancel` — durably request
/// cancellation of one sandbox child.
///
/// The durable transition commits before any process-local signal is sent.
/// Exact retries of cancelling or cancelled work remain accepted. Foreground,
/// wrong-chat, successful, and failed runs are rejected without exposing
/// executor details.
pub async fn post_agent_run_cancel(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, run_id)): Path<(ChatId, openwave_core::AgentRunId)>,
) -> Result<(StatusCode, Json<AgentRunCancellationSnapshot>), ServerError> {
    store.require_chat(chat_id).await?;
    let Some(run) = store.get_agent_run(run_id).await? else {
        return Err(ServerError::conflict("sandbox run is not cancellable"));
    };
    if run.chat_id != chat_id || run.tier != AgentRunTier::Background {
        return Err(ServerError::conflict("sandbox run is not cancellable"));
    }

    let mut outcome = None;
    for _ in 0..8 {
        if let Some(resolved) = store.request_agent_run_cancellation(run_id).await? {
            outcome = Some(resolved);
            break;
        }
        let Some(current) = store.get_agent_run(run_id).await? else {
            return Err(ServerError::conflict("sandbox run is not cancellable"));
        };
        if current.chat_id != chat_id || current.tier != AgentRunTier::Background {
            return Err(ServerError::conflict("sandbox run is not cancellable"));
        }
        tokio::task::yield_now().await;
    }
    let Some(outcome) = outcome else {
        return Err(ServerError::conflict(
            "sandbox run cancellation could not be serialized",
        ));
    };

    let (run, status) = match outcome {
        RequestAgentRunCancellationOutcome::Requested(run) => {
            let status = AgentRunCancellationStatus::Cancelling;
            (run, status)
        }
        RequestAgentRunCancellationOutcome::Cancelled(run) => {
            (run, AgentRunCancellationStatus::Cancelled)
        }
        RequestAgentRunCancellationOutcome::Existing(run)
            if run.status == AgentRunStatus::Cancelled =>
        {
            (run, AgentRunCancellationStatus::Cancelled)
        }
        RequestAgentRunCancellationOutcome::Existing(run)
            if run.status == AgentRunStatus::Cancelling =>
        {
            (run, AgentRunCancellationStatus::Cancelling)
        }
        RequestAgentRunCancellationOutcome::AlreadyTerminal(_)
        | RequestAgentRunCancellationOutcome::Existing(_) => {
            return Err(ServerError::conflict("sandbox run is not cancellable"));
        }
    };

    signal_sandbox_run_after_commit(&state, run.id).await;
    state.agent_run_wake.notify_one();
    Ok((
        StatusCode::ACCEPTED,
        Json(AgentRunCancellationSnapshot { id: run.id, status }),
    ))
}

/// Body of `POST /chats/{chat_id}/agent-runs/{run_id}/steer`.
#[derive(Debug, Deserialize)]
pub struct AgentRunSteerBody {
    /// The instruction to hand the running sandbox agent.
    pub content: String,
}

/// `POST /chats/{chat_id}/agent-runs/{run_id}/steer` — hand one mid-run
/// instruction to a sandbox-resident child that is running right now.
///
/// Unlike turn steering, this is **attached-only and not durable**: the
/// instruction travels over the connection the container driver is holding, and
/// the sandbox folds it into its next model step. A run this process holds no
/// connection to is refused with `409` and nothing is queued, so a caller is
/// never told an instruction was accepted that no agent will ever read.
/// `202 Accepted` means a live connection took it. Foreground, wrong-chat, and
/// terminal runs are rejected without exposing executor details.
pub async fn post_agent_run_steer(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, run_id)): Path<(ChatId, openwave_core::AgentRunId)>,
    Json(body): Json<AgentRunSteerBody>,
) -> Result<StatusCode, ServerError> {
    let content = body.content.trim().to_owned();
    if content.is_empty()
        || content.contains('\0')
        || content.len() > openwave_sandbox_protocol::steer::MAX_STEER_BYTES
    {
        return Err(ServerError::bad_request(
            "steering content must be non-empty, contain no NUL characters, and fit the size limit",
        ));
    }
    store.require_chat(chat_id).await?;
    let run = store
        .get_agent_run(run_id)
        .await?
        .filter(|run| run.chat_id == chat_id && run.tier == AgentRunTier::Background);
    let Some(run) = run else {
        return Err(ServerError::not_found(format!(
            "agent run {run_id} not found"
        )));
    };
    if run.status != AgentRunStatus::Running {
        return Err(ServerError::conflict("sandbox run is not steerable"));
    }
    match state.sandbox_steering.steer(run_id, content) {
        Ok(()) => Ok(StatusCode::ACCEPTED),
        Err(SandboxSteerRefusal::NotAttached) => Err(ServerError::conflict(
            "sandbox run is not attached; steering is not queued",
        )),
        Err(SandboxSteerRefusal::Backlogged) => Err(ServerError::conflict(
            "sandbox run has not consumed its pending steering yet",
        )),
    }
}

/// Best-effort local acceleration after a sandbox cancellation has committed.
///
/// The immutable receipts provide exact attempt identities. Missing receipts,
/// transient read failures, and absent local workers are harmless because the
/// durable state machine remains authoritative and its workers will eventually
/// observe the cancellation through heartbeats, lease expiry, or terminal
/// write fencing.
async fn signal_sandbox_run_after_commit(state: &AppState, run_id: openwave_core::AgentRunId) {
    if let Ok(Some(signal)) = state.store.get_agent_run_cancellation_signal(run_id).await {
        state
            .sandbox_attempts
            .cancel_model(run_id, signal.lease_token);
    }
    // Cancelling a waiting run atomically terminalizes its live tool call and
    // records the exact executor lease. Never infer that lease from mutable
    // call state or signal every call belonging to a run.
    if let Ok(calls) = state
        .store
        .list_sandbox_tool_calls_for_agent_run(run_id)
        .await
    {
        for call in calls {
            if call.status != SandboxToolCallStatus::Cancelled {
                continue;
            }
            if let Ok(Some(receipt)) = state.store.get_sandbox_tool_call_receipt(call.id).await {
                if receipt.status == SandboxToolCallStatus::Cancelled {
                    state.sandbox_attempts.cancel_checkpoint(
                        call.id,
                        run_id,
                        receipt.executor_lease_token,
                    );
                }
            }
        }
    }
}

/// Signal only children durably owned by the cancelled origin turn.
async fn signal_origin_sandbox_runs_after_commit(
    state: &AppState,
    chat_id: ChatId,
    origin_turn_id: TurnId,
) {
    let Ok(runs) = state.store.list_agent_runs(chat_id).await else {
        return;
    };
    for run in runs {
        if run.tier != AgentRunTier::Background
            || !matches!(
                run.status,
                AgentRunStatus::Cancelling | AgentRunStatus::Cancelled
            )
        {
            continue;
        }
        let Ok(Some(admission)) = state.store.get_sandbox_agent_admission(run.id).await else {
            continue;
        };
        if admission.origin_turn_id == origin_turn_id {
            signal_sandbox_run_after_commit(state, run.id).await;
        }
    }
}

#[cfg(test)]
mod activity_tests {
    use super::*;

    fn client_call(name: &str, lease_expires_at: Option<chrono::DateTime<Utc>>) -> ToolCallRecord {
        ToolCallRecord {
            id: CallId::new(),
            chat_id: ChatId::new(),
            turn_id: TurnId::new(),
            provider_id: "provider-call-identity".into(),
            name: name.into(),
            arguments: serde_json::json!({
                "root_id": "5b3e9987-5ebf-4bb0-bc6f-0c041b156027",
                "path": "taxes/2026/private-return.txt",
                "grant": "private-grant"
            }),
            raw_arguments: None,
            execution: ToolCallExecution::Client,
            status: ToolCallStatus::Pending,
            result: None,
            result_preview: None,
            error_code: Some("private-error-code".into()),
            error_detail: Some("private error detail".into()),
            client_executor_id: Some(uuid::Uuid::new_v4()),
            client_lease_expires_at: lease_expires_at,
            created_at: Utc::now(),
            resolved_at: None,
        }
    }

    #[test]
    fn foreground_folder_activity_has_a_closed_safe_vocabulary() {
        let now = Utc::now();
        for (name, kind) in [
            ("list_connected_folders", "list_connected_folders"),
            ("list_folder", "list_folder"),
            ("read_connected_file", "read_connected_file"),
        ] {
            let activity = foreground_activity(
                &[client_call(name, Some(now + chrono::Duration::minutes(1)))],
                now,
            )
            .expect("supported foreground folder work is visible");
            assert_eq!(
                serde_json::to_value(activity).unwrap(),
                serde_json::json!({"kind": kind, "status": "running"})
            );
        }

        let waiting = foreground_activity(&[client_call("list_folder", None)], now)
            .expect("an unclaimed folder operation is visible");
        assert_eq!(
            serde_json::to_value(waiting).unwrap(),
            serde_json::json!({"kind": "list_folder", "status": "waiting"})
        );

        assert!(foreground_activity(&[client_call("unknown_client_tool", None)], now).is_none());

        let rendered = serde_json::to_string(
            &foreground_activity(&[client_call("read_connected_file", None)], now).unwrap(),
        )
        .unwrap();
        for forbidden in [
            "5b3e9987-5ebf-4bb0-bc6f-0c041b156027",
            "taxes/2026/private-return.txt",
            "private-grant",
            "provider-call-identity",
            "private-error-code",
            "private error detail",
        ] {
            assert!(!rendered.contains(forbidden), "activity leaked {forbidden}");
        }
    }
}

/// Body of `POST /chats/{id}/messages`.
#[derive(Debug, Deserialize)]
pub struct PostMessage {
    /// Stable client-generated identity for acceptance and ambiguous retries.
    pub turn_id: TurnId,
    /// The user's input for this turn.
    pub content: String,
    /// Ids returned by the image attachment publish endpoint, in display order.
    ///
    /// Only identity crosses the wire. The server re-derives every attachment's
    /// format and dimensions from the stored bytes, so a caller cannot describe
    /// an image as something it is not.
    #[serde(default)]
    pub attachments: Vec<uuid::Uuid>,
    /// Chat-owned document ids returned by the file ingest endpoint.
    #[serde(default)]
    pub file_attachments: Vec<DocumentId>,
    /// Skills the user explicitly invoked for this turn, by name.
    ///
    /// Absent or empty is the ordinary send, where the model routes on the
    /// prompt's skill catalog itself. Every name must be a currently enabled
    /// skill; one that is not refuses the turn rather than being dropped.
    #[serde(default)]
    pub invoked_skills: Vec<String>,
}

/// Refuse a turn that invokes a skill the install cannot actually run.
///
/// The catalog the user picked from and the catalog this turn will stage are
/// read at different moments, and a skill can be disabled or uninstalled in
/// between. Honouring the rest of the list would send the turn with an
/// instruction to read a manifest that is not there, so an unknown or disabled
/// name refuses the whole submission before any model call — the same posture
/// as a turn carrying images a model cannot see. The refusal names the skill
/// so a client can drop it and resubmit.
async fn require_invocable_skills(state: &AppState, invoked: &[String]) -> Result<(), ServerError> {
    if invoked.is_empty() {
        return Ok(());
    }
    if invoked.len() > openwave_core::TurnRun::MAX_INVOKED_SKILLS {
        return Err(ServerError::bad_request_kind(
            "too_many_invoked_skills",
            format!(
                "a turn may invoke at most {} skills",
                openwave_core::TurnRun::MAX_INVOKED_SKILLS
            ),
        ));
    }
    let mut distinct = std::collections::HashSet::with_capacity(invoked.len());
    for name in invoked {
        if !distinct.insert(name.as_str()) {
            return Err(ServerError::bad_request_kind(
                "duplicate_invoked_skill",
                format!("skill `{name}` was invoked more than once"),
            ));
        }
    }
    let available = match state.code_execution.as_ref() {
        Some(exec) => exec.skill_catalog().await,
        None => Vec::new(),
    };
    for name in invoked {
        if !available.iter().any(|skill| skill.name == *name) {
            return Err(ServerError::bad_request_kind(
                "invoked_skill_unavailable",
                format!("skill `{name}` is not installed or is not enabled"),
            ));
        }
    }
    Ok(())
}

/// Resolve published attachment ids into authoritative image identity.
///
/// The bytes are inspected again here rather than trusting what the publish
/// response said, because nothing durable connects the two requests and the
/// metadata persisted with the turn must describe the bytes that actually exist.
async fn resolve_message_attachments(
    state: &AppState,
    ids: &[uuid::Uuid],
) -> Result<Vec<openwave_core::ImageRef>, ServerError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    if ids.len() > openwave_core::MAX_MESSAGE_ATTACHMENTS {
        return Err(ServerError::bad_request_kind(
            "too_many_image_attachments",
            format!(
                "a message may carry at most {} image attachments",
                openwave_core::MAX_MESSAGE_ATTACHMENTS
            ),
        ));
    }
    let mut images = Vec::with_capacity(ids.len());
    for &id in ids {
        let missing = || {
            ServerError::bad_request_kind(
                "image_attachment_not_found",
                format!("image attachment {id} has not been published"),
            )
        };
        let bytes = state.blobs.get(id).await?.ok_or_else(missing)?;
        let image = image_attachment::inspect_image_bytes(&bytes)?;
        // Attachment ids are content addresses. Bytes that do not hash back to
        // the requested id are some other blob, so the reference is unresolved
        // rather than merely mismatched.
        if image.blob_id != id {
            return Err(missing());
        }
        images.push(image);
    }
    Ok(images)
}

async fn resolve_file_attachments(
    store: &ScopedStore,
    chat_id: ChatId,
    ids: &[DocumentId],
) -> Result<Vec<DocumentId>, ServerError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    if ids.len() > openwave_core::MAX_MESSAGE_ATTACHMENTS {
        return Err(ServerError::bad_request_kind(
            "too_many_attachments",
            format!(
                "a message may carry at most {} attachments",
                openwave_core::MAX_MESSAGE_ATTACHMENTS
            ),
        ));
    }
    let mut distinct = std::collections::HashSet::with_capacity(ids.len());
    for &id in ids {
        if !distinct.insert(id) {
            return Err(ServerError::bad_request_kind(
                "duplicate_file_attachment",
                format!("file attachment {id} was submitted more than once"),
            ));
        }
        let document = store.get_document(id).await?.ok_or_else(|| {
            ServerError::bad_request_kind(
                "file_attachment_not_found",
                format!("file attachment {id} has not been imported"),
            )
        })?;
        let source_blob = document.source_blob.as_ref();
        if document.chat_id != Some(chat_id)
            || document.project_id.is_some()
            || source_blob.is_none()
        {
            return Err(ServerError::bad_request_kind(
                "file_attachment_not_found",
                format!("file attachment {id} has not been imported for chat {chat_id}"),
            ));
        }
        if source_blob.is_some_and(|blob| blob.byte_len > openwave_core::MAX_IMAGE_BYTES) {
            return Err(ServerError::bad_request_kind(
                "file_attachment_too_large",
                "files attached to a message must be 16 MB or smaller",
            ));
        }
    }
    Ok(ids.to_vec())
}

/// Refuse a turn whose model cannot see the images it carries.
///
/// Stripping the images would leave the model answering confidently about
/// something it never received, and silently switching models would change the
/// answer's author behind the user's back. Refusing is the only option that
/// leaves the user in control, so the error is machine-readable and the client
/// can offer to change the model or drop the attachments.
async fn require_image_capable_model(state: &AppState, model: &str) -> Result<(), ServerError> {
    if !state.resolver.enforces_model_registry() {
        return Ok(());
    }
    let Some(policy) = providers::resolve_model_policy(&*state.store, model, true).await? else {
        return Err(ServerError::bad_request_kind(
            "unknown_model",
            format!("model `{model}` is not registered for that provider"),
        ));
    };
    require_image_input(&policy)
}

/// The capability decision itself, separated so it can be exercised against a
/// constructed policy rather than only against whatever the registry happens to
/// advertise today.
fn require_image_input(policy: &providers::ResolvedModelPolicy) -> Result<(), ServerError> {
    if policy
        .input_modalities
        .contains(&crate::model_registry::InputModality::Image)
    {
        return Ok(());
    }
    Err(ServerError::conflict_kind(
        "model_image_input_unsupported",
        format!(
            "model `{}` does not accept image input; choose a model that does, or send the message without images",
            policy.display_name
        ),
    ))
}

#[cfg(test)]
mod image_capability_tests {
    use super::*;
    use crate::model_registry::{InputModality, ModelSpec};

    const TEXT_ONLY: ModelSpec = ModelSpec {
        id: "text-only-model",
        display_name: "Text Only Model",
        provider: ProviderKind::Anthropic,
        verification: crate::model_registry::VerificationTier::Unverified,
        context_window: 200_000,
        max_output_tokens: 64_000,
        input_modalities: &[InputModality::Text],
        supports_reasoning: false,
        reasoning_efforts: &[],
    };

    #[test]
    fn advertised_image_input_is_the_only_thing_that_admits_a_turn_with_images() {
        let native_model = crate::model_registry::find("claude-opus-5")
            .expect("the default curated Anthropic model is registered");
        assert!(
            require_image_input(&providers::ResolvedModelPolicy::curated(native_model)).is_ok()
        );

        let refused = require_image_input(&providers::ResolvedModelPolicy::curated(&TEXT_ONLY))
            .expect_err("a text-only model must refuse a turn carrying images");
        assert_eq!(refused.kind(), "model_image_input_unsupported");
    }
}

/// `POST /chats/{id}/messages` — durably accept a message and queue its turn.
///
/// Returns `202 Accepted` after the input and queued turn commit; a supervised
/// worker claims it asynchronously and journals events for replay/live delivery.
/// Repeating an exact `turn_id` and payload is idempotent. `404` if the chat
/// doesn't exist, `409` if the identity names different input or another turn
/// already owns the chat's single durable live slot.
///
/// Published image attachments may be referenced by id. They commit with the
/// message, and a retry that names different images is an identity conflict
/// rather than a silent acceptance of the first submission's images. A turn
/// carrying images against a model that does not accept image input is refused
/// with `model_image_input_unsupported`.
pub async fn post_message(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(id): Path<ChatId>,
    Json(body): Json<PostMessage>,
) -> Result<StatusCode, ServerError> {
    if body.turn_id.0.is_nil() {
        return Err(ServerError::bad_request("turn_id must not be nil"));
    }
    if body.content.trim().is_empty() || body.content.contains('\0') {
        return Err(ServerError::bad_request(
            "message content must be non-empty and contain no NUL characters",
        ));
    }
    let chat = store.require_chat(id).await?;

    // An ambiguous HTTP retry names only its turn and content, not the resolved
    // model snapshot. Reuse the first acceptance's immutable model so a settings
    // change between attempts cannot turn the same request into a conflict.
    let model = if let Some(existing) = store.get_turn_run(body.turn_id).await? {
        if existing.chat_id != id {
            return Err(ServerError::conflict(format!(
                "turn {} was already accepted by another chat",
                body.turn_id
            )));
        }
        existing.model
    } else {
        let selected = resolve_chat_model(&*state.store, &chat, &state.agent_config.model).await?;
        // The managed re-route the roles read applies, on exactly the domain
        // that read labels: the role default, and only for a managed profile.
        // Unmanaged sends pass through untouched — free-form ids included —
        // and a per-chat override is the user's explicit pick, so a dead one
        // gets the honest validation refusal below rather than a silent swap
        // out from under the label the pill still shows; the picker offers
        // only gateway models to fix it. A managed profile with nothing
        // entitled also keeps the raw selection, refused with the real reason.
        let managed = crate::managed_policy::resolve(&*state.store, &*state.os_policy).await?;
        let selected = if managed.managed && chat.model.is_none() {
            model_roles::effective_chat_policy(&*state.store, &*state.secrets, &managed, &selected)
                .await?
                .map_or(selected, |policy| policy.key)
        } else {
            selected
        };
        validate_model_selection(&state, &selected, true).await?
    };
    require_invocable_skills(&state, &body.invoked_skills).await?;
    let images = resolve_message_attachments(&state, &body.attachments).await?;
    let documents = resolve_file_attachments(&store, id, &body.file_attachments).await?;
    if images.len().saturating_add(documents.len()) > openwave_core::MAX_MESSAGE_ATTACHMENTS {
        return Err(ServerError::bad_request_kind(
            "too_many_attachments",
            format!(
                "a message may carry at most {} attachments",
                openwave_core::MAX_MESSAGE_ATTACHMENTS
            ),
        ));
    }
    if !images.is_empty() {
        require_image_capable_model(&state, &model).await?;
    }
    match store
        .accept_turn_with_attachments(
            body.turn_id,
            id,
            &model,
            &body.content,
            &images,
            &documents,
            &body.invoked_skills,
        )
        .await?
    {
        AcceptTurnOutcome::Accepted(_) | AcceptTurnOutcome::Existing(_) => {
            state.turn_job_wake.notify_one();
            Ok(StatusCode::ACCEPTED)
        }
        AcceptTurnOutcome::IdentityConflict => {
            // Concurrent identical requests can resolve different mutable model
            // defaults before either commits. Retry against the winner's immutable
            // model so the wire identity remains `(chat, turn_id, content)`.
            let Some(existing) = store.get_turn_run(body.turn_id).await? else {
                return Err(ServerError::conflict(format!(
                    "turn {} was accepted with conflicting request data",
                    body.turn_id
                )));
            };
            if existing.chat_id == id
                && matches!(
                    store
                        .accept_turn_with_attachments(
                            body.turn_id,
                            id,
                            &existing.model,
                            &body.content,
                            &images,
                            &documents,
                            &body.invoked_skills,
                        )
                        .await?,
                    AcceptTurnOutcome::Existing(_)
                )
            {
                state.turn_job_wake.notify_one();
                Ok(StatusCode::ACCEPTED)
            } else {
                Err(ServerError::conflict(format!(
                    "turn {} was already accepted with different input",
                    body.turn_id
                )))
            }
        }
        AcceptTurnOutcome::ChatBusy(active) => Err(ServerError::conflict(format!(
            "chat {id} already has active turn {}",
            active.id
        ))),
    }
}

/// Body of `POST /chats/{id}/steer`.
#[derive(Debug, Deserialize)]
pub struct SteerBody {
    /// Stable client-generated identity for admission and ambiguous retries.
    pub steer_id: TurnSteerId,
    /// Exact durable turn to steer.
    pub turn_id: TurnId,
    /// User text to inject into the running turn.
    pub content: String,
    /// When true, preempt the provider stream immediately; otherwise the message
    /// waits for the next step boundary.
    #[serde(default)]
    pub interrupt: bool,
}

/// `POST /chats/{id}/steer` — durably enqueue an instruction for an active turn.
///
/// `202 Accepted` only after the exact instruction commits. A local notification
/// can reduce delivery latency, but the claimed worker always applies pending
/// instructions from the database. Repeating the same identity and payload is
/// idempotent. `404` if the chat doesn't exist, `409` for conflicting identity or
/// unavailable turn, and `400` for malformed input.
pub async fn post_steer(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(id): Path<ChatId>,
    Json(body): Json<SteerBody>,
) -> Result<StatusCode, ServerError> {
    if body.steer_id.0.is_nil() {
        return Err(ServerError::bad_request("steer_id must not be nil"));
    }
    if body.content.trim().is_empty()
        || body.content.contains('\0')
        || body.content.chars().count() > TurnSteer::MAX_CONTENT_LEN
    {
        return Err(ServerError::bad_request(
            "steer content must be non-empty, contain no NUL characters, and fit the size limit",
        ));
    }
    store.require_chat(id).await?;
    match store
        .accept_turn_steer(
            body.steer_id,
            body.turn_id,
            id,
            &body.content,
            body.interrupt,
        )
        .await?
    {
        AcceptTurnSteerOutcome::Accepted(_) | AcceptTurnSteerOutcome::Existing(_) => {
            state
                .active_turns
                .signal_steer(id, body.turn_id, body.interrupt);
            state.turn_job_wake.notify_one();
            Ok(StatusCode::ACCEPTED)
        }
        AcceptTurnSteerOutcome::IdentityConflict => Err(ServerError::conflict(format!(
            "steer {} was already used by different request data",
            body.steer_id
        ))),
        AcceptTurnSteerOutcome::TurnUnavailable => Err(ServerError::conflict(format!(
            "turn {} is not accepting steering instructions",
            body.turn_id
        ))),
    }
}

/// `POST /chats/{id}/cancel` — durably cancel one exact turn for a chat.
///
/// Queued work becomes terminal atomically; running work enters `cancelling`
/// until its exact worker acknowledges quiescence. `202 Accepted` is idempotent
/// for cancelling/cancelled retries. `404` if the chat doesn't exist, `409` if
/// the turn does not belong to the chat or can no longer accept cancellation.
#[derive(Debug, Deserialize)]
pub struct CancelBody {
    /// Exact durable turn to cancel.
    pub turn_id: TurnId,
}

pub async fn post_cancel(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(id): Path<ChatId>,
    Json(body): Json<CancelBody>,
) -> Result<StatusCode, ServerError> {
    // Distinguish "unknown chat" (404) from "known chat, nothing running" (409).
    store.require_chat(id).await?;
    if !store
        .get_turn_run(body.turn_id)
        .await?
        .is_some_and(|turn| turn.chat_id == id)
    {
        return Err(ServerError::conflict(format!(
            "turn {} does not belong to chat {id}",
            body.turn_id
        )));
    }
    let resolution = loop {
        if let Some(resolution) = store
            .request_turn_cancellation_and_append_event(body.turn_id, Utc::now())
            .await?
        {
            break resolution;
        }
        // A heartbeat can advance `updated_at` after this request captures its
        // operational timestamp. Retry the same empty command with fresh time;
        // the store serializes it against the heartbeat and terminal decisions.
        if !store
            .get_turn_run(body.turn_id)
            .await?
            .is_some_and(|turn| turn.chat_id == id)
        {
            return Err(ServerError::conflict(format!(
                "turn {} is not cancellable",
                body.turn_id
            )));
        }
        tokio::task::yield_now().await;
    };
    if matches!(
        resolution.outcome,
        RequestTurnCancellationOutcome::AlreadyTerminal(_)
    ) {
        return Err(ServerError::conflict(format!(
            "turn {} already finished before cancellation",
            body.turn_id
        )));
    }
    if let Some(event) = resolution.terminal_event {
        let _ = state.events.sender(id).send(event);
    }
    state.active_turns.cancel(id, body.turn_id);
    // The turn transaction has already committed its child cancellation
    // cascade. Exact local handles now reduce provider shutdown latency without
    // taking part in the durable decision.
    signal_origin_sandbox_runs_after_commit(&state, id, body.turn_id).await;
    state.turn_job_wake.notify_one();
    // A parked-parent cancellation can fence a queued or running sandbox
    // child. Wake its worker promptly; durable claims remain the source of
    // truth if this notification is lost.
    state.agent_run_wake.notify_one();
    Ok(StatusCode::ACCEPTED)
}

/// Body of `POST /chats/{id}/approvals/{call_id}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalBody {
    /// `approve` or `reject`.
    pub decision: ApprovalChoice,
    /// Optional reject reason (invalid on approve).
    #[serde(default)]
    pub reason: Option<String>,
    /// How much of an approval to remember for this chat. Absent means this
    /// call only.
    #[serde(default)]
    pub grant: Option<ApprovalGrantRung>,
}

/// How wide a standing grant the human chose, narrowest first.
///
/// The renderer names a rung; the server builds the concrete grant from the
/// arguments the call is parked on. A grant can therefore only ever describe
/// the action that was actually under review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalGrantRung {
    /// Exactly the action the card showed.
    ExactAction,
    /// A leading run of the command's argv tokens, with any arguments after
    /// it — "any `cargo test`", not just "any `cargo`".
    ///
    /// The renderer names how many tokens it was offered rather than the
    /// tokens themselves. The server derives the ladder from the parked
    /// call's own arguments and honors the length only if it appears there,
    /// so a client cannot invent a prefix the card never showed.
    CommandPrefix { tokens: usize },
    /// A leading run of a workspace write's path segments — the file itself,
    /// or the directory that holds it.
    ///
    /// Named by segment count on the same terms as [`Self::CommandPrefix`]:
    /// the concrete place comes from the parked call, never from the client.
    PathPrefix { segments: usize },
    /// Every call to this tool.
    WholeTool,
}

/// Wire form of an approval decision.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalChoice {
    Approve,
    Reject,
}

/// Bounded query for restart/reconnect approval recovery.
#[derive(Debug, Deserialize)]
pub(crate) struct PendingApprovalsQuery {
    #[serde(default = "default_pending_approvals_limit")]
    pub limit: u64,
}

fn default_pending_approvals_limit() -> u64 {
    50
}

/// Closed renderer-safe pending approval projection. Canonical arguments,
/// model-authored summaries, and unknown tool names never cross this boundary;
/// only a tool's own closed preview of the action under review does.
#[derive(Debug, Serialize, ts_rs::TS)]
pub(crate) struct PendingApprovalSnapshot {
    pub call_id: CallId,
    pub turn_id: TurnId,
    pub action: openwave_core::RendererToolName,
    pub approval: openwave_core::ToolApprovalKind,
    pub class: openwave_core::ApprovalClass,
    /// Absent, not null, when the tool projects no action.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub preview: Option<openwave_core::ToolActionPreview>,
    pub can_approve: bool,
    pub can_remember: bool,
    /// Complete standing-grant ladder for this exact call, narrowest first.
    ///
    /// Empty means only one-shot approval is available. The renderer receives
    /// the whole ladder because command policy may refuse exact and whole-tool
    /// grants as well as prefixes.
    pub grant_rungs: Vec<ApprovalGrantRung>,
    /// Where the Auto-mode judge stands, when one was engaged. Absent means
    /// no judge ever owned this card.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub auto_judge_status: Option<openwave_core::AutoJudgeStatus>,
}

impl PendingApprovalSnapshot {
    fn from_approval(approval: openwave_core::ToolApproval) -> Self {
        let kind = approval.kind;
        let grant_rungs =
            approval_grant_rungs(kind, approval.preview.as_ref(), approval.action_is_exact);
        Self {
            call_id: approval.call_id,
            turn_id: approval.turn_id,
            action: openwave_core::RendererToolName::from(approval.tool_name.as_str()),
            approval: kind,
            class: approval.class,
            preview: approval.preview,
            can_approve: kind.is_approvable(),
            can_remember: !grant_rungs.is_empty(),
            grant_rungs,
            auto_judge_status: approval.auto_judge_status,
        }
    }
}

/// Renderer names for the complete standing-grant ladder of one approval.
pub(crate) fn approval_grant_rungs(
    kind: openwave_core::ToolApprovalKind,
    action: Option<&openwave_core::ToolActionPreview>,
    action_is_exact: bool,
) -> Vec<ApprovalGrantRung> {
    let mut scopes = match action {
        Some(action) => openwave_core::GrantScope::ladder_for_action(action),
        None => vec![openwave_core::GrantScope::WholeTool],
    };
    // A rung appears only when granting it would mint: the kind admits only
    // the rungs that describe its own action, so a workspace edit offers its
    // place rungs and an ungrantable kind offers nothing.
    scopes.retain(|scope| kind.grantable_at(scope));
    grant_rungs_from_scopes(&scopes, action_is_exact)
}

pub(crate) fn grant_rungs_from_scopes(
    scopes: &[openwave_core::GrantScope],
    action_is_exact: bool,
) -> Vec<ApprovalGrantRung> {
    scopes
        .iter()
        .filter_map(|scope| match scope {
            openwave_core::GrantScope::ExactAction(_) if action_is_exact => {
                Some(ApprovalGrantRung::ExactAction)
            }
            openwave_core::GrantScope::ExactAction(_) => None,
            openwave_core::GrantScope::CommandPrefix { tokens } => {
                Some(ApprovalGrantRung::CommandPrefix {
                    tokens: tokens.len(),
                })
            }
            openwave_core::GrantScope::PathSubtree { prefix } => {
                Some(ApprovalGrantRung::PathPrefix {
                    segments: prefix.split('/').count(),
                })
            }
            openwave_core::GrantScope::WholeTool => Some(ApprovalGrantRung::WholeTool),
            // Retained for old durable grants; the current ladder names the
            // same authority as a one-token command prefix.
            openwave_core::GrantScope::AnyArgsFor { .. } => {
                Some(ApprovalGrantRung::CommandPrefix { tokens: 1 })
            }
        })
        .collect()
}

/// `GET /chats/{id}/approvals` — recover a bounded page of pending cards.
pub(crate) async fn list_pending_approvals(
    store: ScopedStore,
    Path(chat_id): Path<ChatId>,
    Query(query): Query<PendingApprovalsQuery>,
) -> Result<Json<Vec<PendingApprovalSnapshot>>, ServerError> {
    if !(1..=100).contains(&query.limit) {
        return Err(ServerError::bad_request(
            "approval limit must be between 1 and 100",
        ));
    }
    store.require_chat(chat_id).await?;
    let approvals = store
        .list_pending_tool_call_approvals(chat_id, query.limit)
        .await?;
    Ok(Json(
        approvals
            .into_iter()
            .map(PendingApprovalSnapshot::from_approval)
            .collect(),
    ))
}

/// One durable "don't ask again" the reader has made, with enough provenance
/// to recognize it later and withdraw it. Grant scopes are already closed
/// renderer-safe projections, so the snapshot carries them verbatim.
#[derive(Debug, Serialize, ts_rs::TS)]
pub(crate) struct StandingGrantSnapshot {
    /// The approval decision that created the grant — also the handle a
    /// revocation names.
    pub source_call_id: CallId,
    /// How far the grant reaches — one chat, or every chat in a project.
    pub level: openwave_core::GrantLevel,
    /// The name of whatever the level points at, for provenance. `None` when
    /// that chat or project is untitled.
    pub level_title: Option<String>,
    pub action: openwave_core::RendererToolName,
    pub approval: openwave_core::ToolApprovalKind,
    pub scope: openwave_core::GrantScope,
    pub granted_at: chrono::DateTime<Utc>,
}

/// `GET /grants` — the principal's standing grants, newest first, across all
/// of their chats.
///
/// The settings surface for "what the agent can do without asking": a grant
/// the reader cannot find is a one-way door, and this is where it is found.
/// Grants are owner-scoped through the chat or project their level points at
/// (#853), and the provenance titles resolve through the same principal's
/// chats and projects.
pub(crate) async fn list_standing_grants(
    store: ScopedStore,
) -> Result<Json<Vec<StandingGrantSnapshot>>, ServerError> {
    standing_grant_snapshots(&store).await.map(Json)
}

async fn standing_grant_snapshots(
    store: &ScopedStore,
) -> Result<Vec<StandingGrantSnapshot>, ServerError> {
    let grants = store.list_standing_tool_grants().await?;
    let chat_titles: std::collections::HashMap<ChatId, Option<String>> = store
        .list_chats()
        .await?
        .into_iter()
        .map(|chat| (chat.id, chat.title))
        .collect();
    let project_titles: std::collections::HashMap<ProjectId, Option<String>> = store
        .list_projects()
        .await?
        .into_iter()
        .map(|project| (project.id, project.title))
        .collect();
    Ok(grants
        .into_iter()
        .map(|record| {
            let level = record.grant.level();
            let level_title = match level {
                openwave_core::GrantLevel::Chat { chat_id } => {
                    chat_titles.get(&chat_id).cloned().flatten()
                }
                openwave_core::GrantLevel::Project { project_id } => {
                    project_titles.get(&project_id).cloned().flatten()
                }
            };
            StandingGrantSnapshot {
                source_call_id: record.source_call_id,
                level,
                level_title,
                action: openwave_core::RendererToolName::from(record.grant.tool_name()),
                approval: record.grant.kind(),
                scope: record.grant.scope().clone(),
                granted_at: record.grant.granted_at(),
            }
        })
        .collect())
}

/// `GET /consent/statements` — the server's rows of the unified consent read
/// model: every standing tool grant as one [`ConsentStatementSnapshot`].
///
/// The capability half of the union lives in the desktop's host broker and
/// joins these rows renderer-side; the server serves what its own store
/// holds, in the shared statement shape, so both halves render as one list.
pub(crate) async fn list_consent_statements(
    store: ScopedStore,
) -> Result<Json<Vec<crate::consent::ConsentStatementSnapshot>>, ServerError> {
    Ok(Json(
        standing_grant_snapshots(&store)
            .await?
            .into_iter()
            .map(|grant| crate::consent::ConsentStatementSnapshot {
                handle: crate::consent::ConsentHandle::ToolGrant {
                    call_id: grant.source_call_id,
                },
                level: grant.level,
                level_title: grant.level_title,
                verb: crate::consent::ConsentVerb::Tool {
                    action: grant.action,
                    approval: grant.approval,
                },
                resource: crate::consent::ConsentResource::ActionScope { scope: grant.scope },
                method: crate::consent::ConsentMethodSnapshot::ApprovalCard,
                granted_at: grant.granted_at,
            })
            .collect(),
    ))
}

/// `DELETE /grants/{call_id}` — withdraw a standing grant. Later matching
/// calls park on the approval card again. `204` on success, `404` when the
/// grant does not exist (already revoked, or never granted).
pub(crate) async fn delete_standing_grant(
    store: ScopedStore,
    Path(call_id): Path<CallId>,
) -> Result<StatusCode, ServerError> {
    if store.revoke_standing_tool_grant(call_id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ServerError::not_found(format!(
            "standing grant {call_id} not found"
        )))
    }
}

/// `POST /chats/{id}/approvals/{call_id}` — decide a parked Sensitive tool call.
///
/// `204` on success. `404` if the chat or call isn't pending. The turn stays
/// holding its slot until it finishes after the decision.
pub async fn post_approval(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, call_id)): Path<(ChatId, CallId)>,
    Json(body): Json<ApprovalBody>,
) -> Result<StatusCode, ServerError> {
    // Confirm the chat exists so a typo'd id doesn't look like "not pending".
    store.require_chat(chat_id).await?;
    let decision = match body.decision {
        ApprovalChoice::Approve => {
            if body.reason.is_some() {
                return Err(ServerError::bad_request(
                    "approval reason is only valid when rejecting",
                ));
            }
            ApprovalDecision::Approve
        }
        ApprovalChoice::Reject => ApprovalDecision::Reject {
            reason: body
                .reason
                .map(|reason| reason.trim().to_owned())
                .filter(|reason| !reason.is_empty())
                .unwrap_or_else(|| openwave_core::ToolApproval::DEFAULT_REJECT_REASON.into()),
        },
    };
    if body.grant.is_some() && !matches!(&decision, ApprovalDecision::Approve) {
        return Err(ServerError::bad_request(
            "only an approval can be remembered",
        ));
    }
    if decision
        .reason()
        .is_some_and(|reason| !openwave_core::ToolApproval::valid_reason(reason))
    {
        return Err(ServerError::bad_request(
            "approval reject reason is invalid",
        ));
    }
    match state
        .approvals
        .resolve_with_grant(chat_id, call_id, decision, body.grant)
        .await?
    {
        crate::approvals::ResolveApprovalOutcome::Resolved => Ok(StatusCode::NO_CONTENT),
        crate::approvals::ResolveApprovalOutcome::NotPending => Err(ServerError::not_found(
            format!("no pending approval for call {call_id}"),
        )),
        crate::approvals::ResolveApprovalOutcome::WrongChat => Err(ServerError::not_found(
            format!("no pending approval for call {call_id}"),
        )),
        crate::approvals::ResolveApprovalOutcome::NotApprovable => Err(ServerError::conflict_kind(
            "approval_action_not_presentable",
            "this action cannot be approved from the renderer",
        )),
        // Refusing beats widening: a rung the parked call cannot describe
        // would otherwise have to fall back to a broader grant than the human
        // was shown.
        crate::approvals::ResolveApprovalOutcome::GrantNotAvailable => Err(
            ServerError::bad_request("this action cannot be granted at that scope"),
        ),
        crate::approvals::ResolveApprovalOutcome::DecisionConflict => {
            Err(ServerError::conflict_kind(
                "approval_already_decided",
                "this approval was already decided differently",
            ))
        }
    }
}

/// Query for `GET /chats/{id}/events`.
#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    /// Resume after this journal sequence number; `0` (the default) replays from
    /// the start.
    #[serde(default)]
    pub after: i64,
}

/// `GET /chats/{id}/events` (WebSocket) — stream a chat's turn events.
///
/// On connect the client gets **snapshot → replay → live**: journaled events with
/// `seq > after` are replayed, then live events stream as they occur. Subscribing
/// to the live tail *before* replaying, dropping any live event whose `seq` was
/// already replayed, and replaying whenever the live cursor jumps means nothing
/// is missed or duplicated across the handoff or a worker-ownership race. `404`
/// if the chat doesn't exist.
///
/// Auth is checked by the bearer middleware. Browser clients authenticate via
/// `Sec-WebSocket-Protocol` (`openwave-token.<token>`). When the client offered
/// `openwave-v1`, this handler selects it in the upgrade response so the
/// browser accepts the handshake (RFC 6455).
pub async fn chat_events(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(id): Path<ChatId>,
    Query(query): Query<EventsQuery>,
    headers: axum::http::HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ServerError> {
    let chat = store.require_chat(id).await?;
    let upgrade = if offered_handshake_subprotocol(&headers) {
        upgrade.protocols([WS_HANDSHAKE_SUBPROTOCOL])
    } else {
        upgrade
    };
    Ok(upgrade.on_upgrade(move |socket| stream_events(socket, state, id, query.after, chat.title)))
}

/// Serve one client's event stream for `chat`: replay from the journal, then live.
async fn stream_events(
    mut socket: WebSocket,
    state: AppState,
    chat: ChatId,
    after: i64,
    title: Option<String>,
) {
    // Subscribe before replaying, so an event emitted during replay is buffered on
    // the live channel rather than lost in the gap between the two.
    let mut live = state.events.subscribe(chat);
    // Metadata rides the same socket but not the same order: it has no sequence
    // and nothing replays it.
    let mut metadata = state.events.subscribe_metadata(chat);

    // The name is state rather than an event, so the socket opens by stating it
    // and every reconnect restates it. Nothing retains a notice for a client that
    // was not listening yet, and the common case is exactly that: a new chat's
    // first turn can name it before the renderer finishes connecting. A client
    // that already knows this name does nothing with it.
    if let Some(title) = title {
        if send_frame(
            &mut socket,
            &RendererChatFrame::Metadata(RendererChatMetadata::Titled { title }),
        )
        .await
        .is_err()
        {
            return;
        }
    }

    // Replay everything the client hasn't seen yet from the durable journal.
    let mut last_seq = after;
    if replay_after(&mut socket, &*state.store, chat, &mut last_seq)
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            // Watch the socket so a client disconnect ends the task promptly.
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                _ => {}
            },
            notice = metadata.recv() => match notice {
                Ok(notice) => {
                    let frame = RendererChatFrame::Metadata((&notice).into());
                    if send_frame(&mut socket, &frame).await.is_err() {
                        break;
                    }
                }
                // Nothing to catch up on: the durable value is what a fresh read
                // returns, so a dropped notice costs a client nothing but motion.
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            },
            live_event = live.recv() => match live_event {
                Ok(event) => {
                    if event.seq <= last_seq {
                        continue; // already covered by replay
                    }
                    if event.seq > last_seq.saturating_add(1) {
                        // Durable commits can be published out of order across
                        // lease owners. Fill the gap from the journal before
                        // accepting this live tail; replay includes the current
                        // event because publication always follows commit.
                        if replay_after(&mut socket, &*state.store, chat, &mut last_seq)
                            .await
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    last_seq = event.seq;
                    let model = state
                        .store
                        .list_turn_runs(chat)
                        .await
                        .ok()
                        .and_then(|turns| turns.into_iter().find(|turn| !turn.status.is_terminal()))
                        .map(|turn| turn.model);
                    if send_event(&mut socket, &event, model.as_deref()).await.is_err() {
                        break;
                    }
                }
                // Fell behind the live buffer. Rather than drop the client, catch
                // up from the journal (durable truth) and resume live — the seq
                // dedup above absorbs any overlap. A long/fast turn can outrun the
                // 256-slot buffer, so this keeps an ordinary client connected.
                Err(RecvError::Lagged(_)) => {
                    if replay_after(&mut socket, &*state.store, chat, &mut last_seq)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            },
        }
    }
}

/// Send journaled events with `seq > *last_seq` to the socket, advancing
/// `*last_seq`. `Err(())` means the connection should end (send or store failure).
async fn replay_after(
    socket: &mut WebSocket,
    store: &dyn Store,
    chat: ChatId,
    last_seq: &mut i64,
) -> Result<(), ()> {
    let events = store.list_events(chat, *last_seq).await.map_err(|_| ())?;
    let turn_models = store
        .list_turn_runs(chat)
        .await
        .map_err(|_| ())?
        .into_iter()
        .map(|turn| (turn.id, turn.model))
        .collect::<std::collections::HashMap<_, _>>();
    let mut active_turn_id = None;
    for event in events {
        *last_seq = event.seq;
        if let AgentEvent::TurnStarted { turn_id } = &event.event {
            active_turn_id = Some(*turn_id);
        }
        let model = active_turn_id.and_then(|turn_id| turn_models.get(&turn_id));
        send_event(socket, &event, model.map(String::as_str))
            .await
            .map_err(|_| ())?;
    }
    Ok(())
}

/// Send one journaled event as a frame.
async fn send_event(
    socket: &mut WebSocket,
    event: &SequencedEvent,
    model: Option<&str>,
) -> Result<(), axum::Error> {
    send_frame(
        socket,
        &RendererChatFrame::Event(Box::new(
            RendererSequencedEvent::from(event).with_turn_model(model),
        )),
    )
    .await
}

/// Send one frame as JSON text. A frame that fails to serialize is skipped
/// rather than sent empty (which a client couldn't decode).
async fn send_frame(socket: &mut WebSocket, frame: &RendererChatFrame) -> Result<(), axum::Error> {
    let Ok(json) = serde_json::to_string(frame) else {
        return Ok(());
    };
    socket.send(Message::Text(json.into())).await
}

#[cfg(test)]
mod mcp_app_payload_tests {
    use openwave_core::{AgentEvent, ToolOutput, ToolUiView};

    use super::*;

    fn sequenced(seq: i64, event: AgentEvent) -> SequencedEvent {
        SequencedEvent { seq, event }
    }

    #[test]
    fn assembles_arguments_and_result_for_a_view_declaring_call() {
        let call = CallId::new();
        let other = CallId::new();
        let events = vec![
            sequenced(
                1,
                AgentEvent::ToolCallArgsDelta {
                    call_id: call,
                    fragment: "{\"operation\":".into(),
                },
            ),
            sequenced(
                2,
                AgentEvent::ToolCallArgsDelta {
                    call_id: other,
                    fragment: "{\"unrelated\":true}".into(),
                },
            ),
            sequenced(
                3,
                AgentEvent::ToolCallArgsDelta {
                    call_id: call,
                    fragment: "\"list\"}".into(),
                },
            ),
            sequenced(
                4,
                AgentEvent::ToolCallCompleted {
                    call_id: call,
                    output: ToolOutput::text("{\"status\":200}")
                        .with_data(serde_json::json!({"status": 200}))
                        .with_ui_view(ToolUiView {
                            server: "gateway".into(),
                            resource_uri: "ui://gateway/app.html".into(),
                        }),
                    action: None,
                    result: None,
                },
            ),
        ];

        let payload = mcp_app_payload_from_events(&events, call).expect("payload exists");
        assert_eq!(
            payload.arguments,
            Some(serde_json::json!({"operation": "list"}))
        );
        assert_eq!(payload.content, "{\"status\":200}");
        assert_eq!(
            payload.structured_content,
            Some(serde_json::json!({"status": 200}))
        );
        assert!(!payload.is_error);
    }

    #[test]
    fn calls_without_a_view_declaration_expose_no_payload() {
        let call = CallId::new();
        let events = vec![sequenced(
            1,
            AgentEvent::ToolCallCompleted {
                call_id: call,
                output: ToolOutput::text("plain result"),
                action: None,
                result: None,
            },
        )];
        assert!(mcp_app_payload_from_events(&events, call).is_none());
        assert!(mcp_app_payload_from_events(&events, CallId::new()).is_none());
    }
}
