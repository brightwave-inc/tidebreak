//! Route handlers extracted from the parent `routes` module.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use tidebreak_core::id::{AppId, AppRevisionId};
use tidebreak_core::local_app::app_revision_relative_path;
use tidebreak_core::{AgentError, CallId, ChatId, SequencedEvent};

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::mcp_config::{McpServersConfig, McpServersInfo};
use crate::providers::{self};
use crate::scoped_store::ScopedStore;
use crate::state::AppState;
use crate::view_frames::ViewFrameSource;

/// `GET /mcp/servers` — renderer-safe definitions and current connection health.
pub async fn get_mcp_servers(
    State(state): State<AppState>,
) -> Result<Json<McpServersInfo>, ServerError> {
    Ok(Json(state.mcp.info().await))
}

/// `PUT /mcp/servers` — atomically validate, connect, persist, and publish a
/// complete replacement set. A failed candidate never changes active tools.
///
/// The body describes the *user-configured* set only. Plugin-sourced servers
/// are derived from the installed plugin tree and its enable flags, so a body
/// naming one is refused outright rather than quietly ignored — the reader
/// sees them in the same list and would otherwise reasonably expect an edit to
/// land. Omitting them is not a delete: the runtime rebuilds that slice
/// itself, and a plugin's servers go away when the plugin is switched off or
/// uninstalled.
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
    if let Some(server) = body.servers.iter().find(|server| server.plugin.is_some()) {
        return Err(ServerError::bad_request_kind(
            "mcp_plugin_server_read_only",
            format!(
                "MCP server {:?} comes from a plugin and cannot be edited here; \
                 manage it from the plugin that provides it",
                server.name
            ),
        ));
    }
    // Resolved outside the runtime's mutation lock: a policy that flips
    // between here and the commit skips the admission check, but the commit
    // itself re-reads the lockdown under that lock, so such a definition
    // persists inert and never connects. The residue is a millisecond-wide
    // cosmetic entry in durable config, not an execution bypass.
    let policy = crate::managed_policy::resolve(&*state.provisioned_policy, &*state.os_policy)?;
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
            tidebreak_core::AgentEvent::ToolCallArgsDelta {
                call_id: id,
                fragment,
            } if *id == call_id => fragments.push_str(fragment),
            tidebreak_core::AgentEvent::ToolCallCompleted {
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
    let mut policy = crate::managed_policy::resolve(&*state.provisioned_policy, &*state.os_policy)?;
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
    // The `create_app` roster's gateway section is read through the session
    // that just ended; republish so the door stops offering bindings nothing
    // can resolve any more.
    state.mcp.refresh_connected_app_roster().await;
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

#[cfg(test)]
mod mcp_app_payload_tests {
    use tidebreak_core::{AgentEvent, ToolOutput, ToolUiView};

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
