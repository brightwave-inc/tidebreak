//! HTTP and WebSocket route handlers.
//!
//! Document lifecycle and search handlers live in the dedicated `document`
//! submodule; settings, providers, projects, chats, and event streaming remain
//! here.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use cap_std::ambient_authority;
use cap_std::fs::Dir;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::Path as FsPath;
use tokio::sync::broadcast::error::RecvError;

use openwave_core::{
    AcceptTurnOutcome, AcceptTurnSteerOutcome, AgentError, AgentRun, AgentRunExecution,
    AgentRunStatus, ApprovalDecision, CallId, Chat, ChatId, DeleteChatOutcome,
    DeleteProjectOutcome, Message as StoredMessage, MessageId, Project, ProjectId, ReasoningEffort,
    RequestAgentRunCancellationOutcome, RequestTurnCancellationOutcome, Role, SandboxToolCall,
    SandboxToolCallStatus, SecretProvider, SequencedEvent, Store, ToolCallExecution,
    ToolCallRecord, ToolCallStatus, TurnId, TurnSteer, TurnSteerId,
};

use crate::auth::{offered_handshake_subprotocol, WS_HANDSHAKE_SUBPROTOCOL};
use crate::code_execution::{
    self, CodeExecutionConfigInfo, CodeExecutionConfigUpdate, CodeExecutionCredentialReadiness,
};
use crate::error::ServerError;
use crate::event_projection::{RendererChatFrame, RendererChatMetadata, RendererSequencedEvent};
use crate::extract::{Json, Path, Query};
use crate::mcp_config::{McpServersConfig, McpServersInfo};
use crate::model_roles::{self, ModelRole};
use crate::providers::{self, ProviderCredential, ProviderInfo, ProviderKind, ProviderUpdate};
use crate::state::AppState;
use crate::web_search::{
    self, WebSearchConfigInfo, WebSearchConfigUpdate, WebSearchCredentialReadiness,
    WebSearchCredentialsInfo,
};

pub(crate) mod client_execution;
mod delegated_file_execution;
mod document;
pub(crate) mod image_attachment;
mod root_attachment;
mod user_questions;
pub use client_execution::*;
pub use delegated_file_execution::*;
pub use document::*;
pub use image_attachment::*;
pub use root_attachment::*;
pub use user_questions::*;

/// `GET /mcp/servers` — renderer-safe definitions and current connection health.
pub async fn get_mcp_servers(
    State(state): State<AppState>,
) -> Result<Json<McpServersInfo>, ServerError> {
    Ok(Json(state.mcp.info().await))
}

/// `PUT /mcp/servers` — atomically validate, connect, persist, and publish a
/// complete replacement set. A failed candidate never changes active tools.
pub async fn put_mcp_servers(
    State(state): State<AppState>,
    Json(body): Json<McpServersConfig>,
) -> Result<Json<McpServersInfo>, ServerError> {
    // Once validation/startup begins, finish the durable/live commit even if
    // the HTTP client disconnects and drops this handler future.
    let runtime = state.mcp.clone();
    let mutation = tokio::spawn(async move { runtime.replace(body).await });
    Ok(Json(
        mutation
            .await
            .map_err(|_| ServerError::internal("MCP settings update task failed"))?
            .map_err(mcp_request_error)?,
    ))
}

/// `GET /chats/{chat_id}/calls/{call_id}/mcp-app-payload` — the completed
/// call's result, packaged for its declared MCP Apps view.
///
/// Only calls whose output carried a validated view declaration answer here,
/// and the payload is handed to the renderer as an opaque envelope for the
/// sandboxed frame — the transcript presentation itself never reads it.
pub async fn get_mcp_app_payload(
    State(state): State<AppState>,
    Path((chat_id, call_id)): Path<(ChatId, CallId)>,
) -> Result<Json<McpAppPayload>, ServerError> {
    let events = state.store.list_events(chat_id, 0).await?;
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
/// state sticky.
pub async fn get_policy(
    State(state): State<AppState>,
) -> Result<Json<crate::managed_policy::ManagedPolicy>, ServerError> {
    Ok(Json(
        crate::managed_policy::resolve(&*state.store, &*state.os_policy).await?,
    ))
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
    let authorization_url = state.gateway.begin_sign_in().await?;
    Ok(Json(GatewaySignInStarted { authorization_url }))
}

#[derive(Serialize)]
pub struct GatewaySignInStarted {
    authorization_url: String,
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

/// `POST /gateway/models/sync` — refetch the entitled models into the
/// provider's model set.
pub async fn post_gateway_models_sync(
    State(state): State<AppState>,
) -> Result<Json<crate::gateway_runtime::GatewayStatus>, ServerError> {
    state.gateway.sync_models().await?;
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
    let token = state
        .mcp
        .mint_view_frame(&name, &body.uri)
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
    let Some(document) = state.mcp.take_view_frame(token).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            ("content-type", "text/html; charset=utf-8"),
            (
                "content-security-policy",
                concat!(
                    "default-src 'none'; script-src 'unsafe-inline'; ",
                    "style-src 'unsafe-inline'; img-src data:; font-src data:; ",
                    "connect-src 'none'; form-action 'none'; base-uri 'none'"
                ),
            ),
            ("x-content-type-options", "nosniff"),
            ("referrer-policy", "no-referrer"),
            ("cache-control", "no-store"),
        ],
        document.html,
    )
        .into_response()
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
}

/// Body of `PUT /settings`. Each field is a *double* option so an absent key is
/// distinguished from an explicit `null`: absent leaves the value unchanged,
/// `null` resets it to the server default, and a value sets it.
#[derive(Debug, Deserialize)]
pub struct SettingsUpdate {
    #[serde(default, deserialize_with = "double_option")]
    pub model: Option<Option<String>>,
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
    Ok(Json(Settings {
        model: read_model(&*state.store).await?,
        has_api_key: has_api_key(&*state.secrets).await,
    }))
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
        code_execution::config_info(&*state.store, &*state.secrets).await?,
    ))
}

/// `PUT /code-execution` — select a fixed provider and bounded host timeout.
pub async fn put_code_execution_config(
    State(state): State<AppState>,
    Json(body): Json<CodeExecutionConfigUpdate>,
) -> Result<Json<CodeExecutionConfigInfo>, ServerError> {
    Ok(Json(
        code_execution::update_config(&*state.store, &*state.secrets, body).await?,
    ))
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
    let provider = parse_web_search_provider(&provider)?;
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
    let provider = parse_web_search_provider(&provider)?;
    Ok(Json(
        web_search::delete_credential(&*state.secrets, provider).await?,
    ))
}

fn parse_web_search_provider(
    value: &str,
) -> std::result::Result<openwave_web_search::WebSearchProviderKind, ServerError> {
    match value {
        "exa" => Ok(openwave_web_search::WebSearchProviderKind::Exa),
        "tavily" => Ok(openwave_web_search::WebSearchProviderKind::Tavily),
        _ => Err(ServerError::not_found(format!(
            "unknown web search provider kind: {value}"
        ))),
    }
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

/// `GET /providers` — every known provider kind and its current config.
pub async fn list_providers(
    State(state): State<AppState>,
) -> Result<Json<ProvidersList>, ServerError> {
    Ok(Json(ProvidersList {
        providers: providers::list_providers(&*state.store, &*state.secrets).await?,
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
    let policy = crate::managed_policy::resolve(&*state.store, &*state.os_policy).await?;
    let info =
        providers::update_provider(&*state.store, &*state.secrets, kind, body, &policy).await?;
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
async fn resolved_role_key(
    state: &AppState,
    role: ModelRole,
    selection: Option<&str>,
) -> Result<Option<String>, ServerError> {
    match role {
        // The chat role goes through the same seam a new execution does, minus
        // the per-chat override there is no chat here to read — so the label a
        // client shows for "default" is what the next turn actually gets. Its
        // last resort is the boot default, which no role's list can name.
        ModelRole::Chat => {
            let fallback = match selection {
                Some(selection) => selection.to_owned(),
                None => chat_role_model(&*state.store, &state.agent_config.model).await?,
            };
            Ok(
                providers::resolve_model_policy(&*state.store, &fallback, true)
                    .await?
                    .map(|policy| policy.key),
            )
        }
        _ => Ok(model_roles::resolve(&*state.store, &*state.secrets, role)
            .await?
            .map(|policy| policy.key)),
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
    State(state): State<AppState>,
    Json(body): Json<CreateProject>,
) -> Result<impl IntoResponse, ServerError> {
    let project = Project {
        id: ProjectId::new(),
        title: normalize_project_title(body.title)?,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    state.store.create_project(&project).await?;
    Ok((StatusCode::CREATED, Json(project)))
}

/// `PATCH /projects/{id}` — update bounded human-facing project metadata.
pub async fn patch_project(
    State(state): State<AppState>,
    Path(id): Path<ProjectId>,
    Json(body): Json<ProjectUpdate>,
) -> Result<Json<Project>, ServerError> {
    let title = body.title.map(normalize_project_title).transpose()?;
    if let Some(title) = title {
        if !state.store.update_project_title(id, title).await? {
            return Err(ServerError::not_found(format!("project {id} not found")));
        }
    }
    state
        .store
        .get_project(id)
        .await?
        .map(Json)
        .ok_or_else(|| ServerError::not_found(format!("project {id} not found")))
}

/// `GET /projects` — list projects, most-recently-created first.
pub async fn list_projects(
    State(state): State<AppState>,
) -> Result<Json<Vec<Project>>, ServerError> {
    Ok(Json(state.store.list_projects().await?))
}

/// `GET /projects/{id}` — fetch one project, or `404`.
pub async fn get_project(
    State(state): State<AppState>,
    Path(id): Path<ProjectId>,
) -> Result<Json<Project>, ServerError> {
    state
        .store
        .get_project(id)
        .await?
        .map(Json)
        .ok_or_else(|| ServerError::not_found(format!("project {id} not found")))
}

/// `DELETE /projects/{id}` — remove an empty project. Owned conversations,
/// documents, and root defaults must be removed through their explicit
/// lifecycle APIs first; this boundary never cascades them.
pub async fn delete_project(
    State(state): State<AppState>,
    Path(id): Path<ProjectId>,
) -> Result<StatusCode, ServerError> {
    match state.store.delete_project(id).await? {
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
    /// Optional model for this chat; omitted to use the configured default.
    #[serde(default)]
    pub model: Option<String>,
    /// Optional reasoning-effort override for this chat; honored only by models
    /// that expose the control.
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// `POST /chats` — create a chat and return it (`201 Created`).
pub async fn create_chat(
    State(state): State<AppState>,
    Json(mut body): Json<CreateChat>,
) -> Result<impl IntoResponse, ServerError> {
    // Return a product-facing 400 for an unknown project. The Store and schema
    // independently enforce the same membership invariant inside insertion.
    if let Some(project_id) = body.project_id {
        state
            .store
            .get_project(project_id)
            .await?
            .ok_or_else(|| ServerError::not_found(format!("project {project_id} not found")))?;
    }
    if let Some(model) = body.model.as_mut() {
        *model = validate_model_selection(&state, model, false).await?;
    }
    let chat = Chat {
        id: ChatId::new(),
        project_id: body.project_id,
        title: normalize_chat_title(body.title)?,
        model: body.model,
        reasoning_effort: body.reasoning_effort,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    let chat = state.store.create_chat_with_project_defaults(&chat).await?;
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
}

/// `PATCH /chats/{id}` — update the human-facing title and/or model selection.
pub async fn patch_chat(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
    Json(mut body): Json<ChatUpdate>,
) -> Result<Json<Chat>, ServerError> {
    // Validate every supplied field before touching durable state. This keeps a
    // mixed request all-or-nothing from the user's point of view.
    if let Some(Some(model)) = body.model.as_mut() {
        *model = validate_model_selection(&state, model, false).await?;
    }
    let title = body.title.map(normalize_chat_title).transpose()?;

    let mut chat = state
        .store
        .get_chat(id)
        .await?
        .ok_or_else(|| ServerError::not_found(format!("chat {id} not found")))?;

    if !state
        .store
        .update_chat_metadata(id, title.clone(), body.model.clone(), body.reasoning_effort)
        .await?
    {
        return Err(ServerError::not_found(format!("chat {id} not found")));
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
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub refusal: Option<crate::event_projection::RendererRefusal>,
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

/// One visible transcript plus the durable journal watermark that produced it.
/// The renderer uses the watermark to subscribe only to future events, avoiding
/// duplicate text when reopening a completed conversation.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct ChatTranscript {
    pub messages: Vec<ChatMessageSnapshot>,
    /// Finished tool activity from terminal turns, projected through a fixed
    /// renderer-safe allowlist. Canonical tool records never cross this API.
    pub tool_activity: Vec<openwave_core::ChatToolActivitySnapshot>,
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
}

impl TranscriptRole {
    /// `None` for roles the transcript does not show.
    fn for_transcript(role: Role) -> Option<Self> {
        match role {
            Role::User => Some(Self::User),
            Role::Assistant => Some(Self::Assistant),
            Role::System | Role::Tool => None,
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
            refusal: None,
        })
    }
}

/// `GET /chats/{id}/messages` — replay the visible durable transcript in
/// commit order. The existence check prevents a missing chat from looking like
/// an empty conversation.
pub async fn list_chat_messages(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
) -> Result<Json<ChatTranscript>, ServerError> {
    let transcript = state
        .store
        .get_chat_transcript(id)
        .await?
        .ok_or_else(|| ServerError::not_found(format!("chat {id} not found")))?;
    let mut citations_by_message = std::collections::HashMap::new();
    for citation in transcript.citations {
        citations_by_message
            .entry(citation.message_id)
            .or_insert_with(Vec::new)
            .push(citation);
    }
    let mut image_attachments_by_message = std::collections::HashMap::new();
    for attachment in transcript.message_attachments {
        image_attachments_by_message
            .entry(attachment.message_id)
            .or_insert_with(Vec::new)
            .push(TranscriptImageAttachment::from(attachment));
    }
    let mut refusals_by_message = transcript
        .refusals
        .into_iter()
        .map(|snapshot| {
            (
                snapshot.message_id,
                crate::event_projection::RendererRefusal::from(&snapshot.refusal),
            )
        })
        .collect::<std::collections::HashMap<_, _>>();
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
            }
            snapshot.refusal = refusals_by_message.remove(&snapshot.id);
            Some(snapshot)
        })
        .collect();
    Ok(Json(ChatTranscript {
        messages,
        tool_activity: transcript.tool_activity,
        last_event_seq: transcript.last_event_seq,
    }))
}

/// `GET /chats` — list chats, most-recently-created first.
pub async fn list_chats(State(state): State<AppState>) -> Result<Json<Vec<Chat>>, ServerError> {
    Ok(Json(state.store.list_chats().await?))
}

/// `GET /chats/{id}` — fetch one chat, or `404`.
pub async fn get_chat(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
) -> Result<Json<Chat>, ServerError> {
    state
        .store
        .get_chat(id)
        .await?
        .map(Json)
        .ok_or_else(|| ServerError::not_found(format!("chat {id} not found")))
}

/// `DELETE /chats/{id}` — remove a quiesced conversation and its product
/// history. Rooted or active conversations deliberately return a conflict: the
/// caller must first finish cancellation and durable broker detachment.
pub async fn delete_chat(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
) -> Result<StatusCode, ServerError> {
    match state.store.delete_chat(id).await? {
        DeleteChatOutcome::Deleted => {
            state.document_job_wake.notify_one();
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
            "reconcile connected-folder changes before deleting this conversation",
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
/// Worker lease tokens, delegated inputs, scheduling budgets, and other
/// executor-facing fields intentionally remain inside the server/store boundary.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct AgentRunSnapshot {
    pub id: openwave_core::AgentRunId,
    pub parent_id: Option<openwave_core::AgentRunId>,
    pub execution: AgentRunExecution,
    pub status: AgentRunStatus,
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
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
    // This is an OpenWave call id, not a provider call identity. It lets a
    // transcript observer attach this durable status to its exact spawning
    // step without exposing delegated input or executor data.
    pub spawn_call_id: Option<openwave_core::CallId>,
}

impl AgentRunSnapshot {
    fn from_run(run: AgentRun, activity: Option<AgentActivitySnapshot>) -> Self {
        Self {
            id: run.id,
            parent_id: run.parent_id,
            spawn_call_id: run.spawn_call_id,
            execution: run.execution,
            status: run.status,
            started_at: run.started_at,
            finished_at: run.finished_at,
            last_error_code: run.last_error_code,
            activity,
            created_at: run.created_at,
            updated_at: run.updated_at,
        }
    }
}

/// Fixed, renderer-safe names for supported live work.
///
/// Adding a durable tool does not automatically expose it to a renderer: it
/// must be deliberately admitted here with a safe label.
#[derive(Debug, Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivityKind {
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
        "web_search" => AgentActivityKind::WebSearch,
        openwave_core::SANDBOX_READ_DELEGATED_FILE_TOOL => AgentActivityKind::ReadDelegatedFile,
        // Unknown tool names are executor data, not a renderer API contract.
        _ => return None,
    };
    let status = match call.status {
        SandboxToolCallStatus::Accepted => AgentActivityStatus::Waiting,
        SandboxToolCallStatus::Claimed => AgentActivityStatus::Running,
        SandboxToolCallStatus::Completed
        | SandboxToolCallStatus::Failed
        | SandboxToolCallStatus::Cancelled => return None,
        _ => return None,
    };
    Some(AgentActivitySnapshot { kind, status })
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
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
) -> Result<Json<Vec<AgentRunSnapshot>>, ServerError> {
    state
        .store
        .get_chat(id)
        .await?
        .ok_or_else(|| ServerError::not_found(format!("chat {id} not found")))?;
    let runs = state.store.list_agent_runs(id).await?;
    // This read model needs only live client checkpoints. Loading the complete
    // tool-call transcript here would needlessly deserialize historical model
    // arguments, results, and local diagnostics just to render current work.
    let client_calls = state.store.list_pending_client_tool_calls(id).await?;
    let now = Utc::now();
    let mut snapshots = Vec::with_capacity(runs.len());
    for run in runs {
        let activity = if run.execution == AgentRunExecution::Sandbox {
            let calls = state
                .store
                .list_sandbox_tool_calls_for_agent_run(run.id)
                .await?;
            sandbox_activity(&calls)
        } else if run.execution == AgentRunExecution::Foreground {
            foreground_activity(&client_calls, now)
        } else {
            None
        };
        snapshots.push(AgentRunSnapshot::from_run(run, activity));
    }
    Ok(Json(snapshots))
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
    Path((chat_id, run_id)): Path<(ChatId, openwave_core::AgentRunId)>,
) -> Result<(StatusCode, Json<AgentRunCancellationSnapshot>), ServerError> {
    if state.store.get_chat(chat_id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {chat_id} not found")));
    }
    let Some(run) = state.store.get_agent_run(run_id).await? else {
        return Err(ServerError::conflict("sandbox run is not cancellable"));
    };
    if run.chat_id != chat_id || run.execution != AgentRunExecution::Sandbox {
        return Err(ServerError::conflict("sandbox run is not cancellable"));
    }

    let mut outcome = None;
    for _ in 0..8 {
        if let Some(resolved) = state.store.request_agent_run_cancellation(run_id).await? {
            outcome = Some(resolved);
            break;
        }
        let Some(current) = state.store.get_agent_run(run_id).await? else {
            return Err(ServerError::conflict("sandbox run is not cancellable"));
        };
        if current.chat_id != chat_id || current.execution != AgentRunExecution::Sandbox {
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
                    state.sandbox_attempts.cancel_search(
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
        if run.execution != AgentRunExecution::Sandbox
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
            execution: ToolCallExecution::Client,
            status: ToolCallStatus::Pending,
            result: None,
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
    let chat = state
        .store
        .get_chat(id)
        .await?
        .ok_or_else(|| ServerError::not_found(format!("chat {id} not found")))?;

    // An ambiguous HTTP retry names only its turn and content, not the resolved
    // model snapshot. Reuse the first acceptance's immutable model so a settings
    // change between attempts cannot turn the same request into a conflict.
    let model = if let Some(existing) = state.store.get_turn_run(body.turn_id).await? {
        if existing.chat_id != id {
            return Err(ServerError::conflict(format!(
                "turn {} was already accepted by another chat",
                body.turn_id
            )));
        }
        existing.model
    } else {
        let selected = resolve_chat_model(&*state.store, &chat, &state.agent_config.model).await?;
        validate_model_selection(&state, &selected, true).await?
    };
    let images = resolve_message_attachments(&state, &body.attachments).await?;
    if !images.is_empty() {
        require_image_capable_model(&state, &model).await?;
    }
    match state
        .store
        .accept_turn_with_attachments(body.turn_id, id, &model, &body.content, &images)
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
            let Some(existing) = state.store.get_turn_run(body.turn_id).await? else {
                return Err(ServerError::conflict(format!(
                    "turn {} was accepted with conflicting request data",
                    body.turn_id
                )));
            };
            if existing.chat_id == id
                && matches!(
                    state
                        .store
                        .accept_turn_with_attachments(
                            body.turn_id,
                            id,
                            &existing.model,
                            &body.content,
                            &images,
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
    if state.store.get_chat(id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {id} not found")));
    }
    match state
        .store
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
    Path(id): Path<ChatId>,
    Json(body): Json<CancelBody>,
) -> Result<StatusCode, ServerError> {
    // Distinguish "unknown chat" (404) from "known chat, nothing running" (409).
    if state.store.get_chat(id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {id} not found")));
    }
    if !state
        .store
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
        if let Some(resolution) = state
            .store
            .request_turn_cancellation_and_append_event(body.turn_id, Utc::now())
            .await?
        {
            break resolution;
        }
        // A heartbeat can advance `updated_at` after this request captures its
        // operational timestamp. Retry the same empty command with fresh time;
        // the store serializes it against the heartbeat and terminal decisions.
        if !state
            .store
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
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalGrantRung {
    /// Exactly the action the card showed.
    ExactAction,
    /// This executable, with any arguments.
    AnyArgsForCommand,
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
}

impl PendingApprovalSnapshot {
    fn from_approval(approval: openwave_core::ToolApproval) -> Self {
        let kind = approval.kind;
        Self {
            call_id: approval.call_id,
            turn_id: approval.turn_id,
            action: openwave_core::RendererToolName::from(approval.tool_name.as_str()),
            approval: kind,
            class: approval.class,
            preview: approval.preview,
            can_approve: kind.is_approvable(),
            can_remember: kind.is_standing_grantable(),
        }
    }
}

/// `GET /chats/{id}/approvals` — recover a bounded page of pending cards.
pub(crate) async fn list_pending_approvals(
    State(state): State<AppState>,
    Path(chat_id): Path<ChatId>,
    Query(query): Query<PendingApprovalsQuery>,
) -> Result<Json<Vec<PendingApprovalSnapshot>>, ServerError> {
    if !(1..=100).contains(&query.limit) {
        return Err(ServerError::bad_request(
            "approval limit must be between 1 and 100",
        ));
    }
    if state.store.get_chat(chat_id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {chat_id} not found")));
    }
    let approvals = state
        .store
        .list_pending_tool_call_approvals(chat_id, query.limit)
        .await?;
    Ok(Json(
        approvals
            .into_iter()
            .map(PendingApprovalSnapshot::from_approval)
            .collect(),
    ))
}

/// `POST /chats/{id}/approvals/{call_id}` — decide a parked Sensitive tool call.
///
/// `204` on success. `404` if the chat or call isn't pending. The turn stays
/// holding its slot until it finishes after the decision.
pub async fn post_approval(
    State(state): State<AppState>,
    Path((chat_id, call_id)): Path<(ChatId, CallId)>,
    Json(body): Json<ApprovalBody>,
) -> Result<StatusCode, ServerError> {
    // Confirm the chat exists so a typo'd id doesn't look like "not pending".
    if state.store.get_chat(chat_id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {chat_id} not found")));
    }
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
                .unwrap_or_else(|| "user denied approval".into()),
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
    Path(id): Path<ChatId>,
    Query(query): Query<EventsQuery>,
    headers: axum::http::HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ServerError> {
    let Some(chat) = state.store.get_chat(id).await? else {
        return Err(ServerError::not_found(format!("chat {id} not found")));
    };
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
                    if send_event(&mut socket, &event).await.is_err() {
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
    for event in events {
        *last_seq = event.seq;
        send_event(socket, &event).await.map_err(|_| ())?;
    }
    Ok(())
}

/// Send one journaled event as a frame.
async fn send_event(socket: &mut WebSocket, event: &SequencedEvent) -> Result<(), axum::Error> {
    send_frame(
        socket,
        &RendererChatFrame::Event(RendererSequencedEvent::from(event)),
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
