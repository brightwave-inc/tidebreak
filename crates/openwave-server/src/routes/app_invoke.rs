//! `POST /apps/{id}/invoke` — out-of-turn invocation of a local app's pinned
//! MCP tools.
//!
//! The first tool execution outside a model turn: the sandboxed app frame
//! posts a call to the trusted renderer, the renderer forwards it here on its
//! bearer, and the server performs the same dispatch a turn would — registry
//! snapshot, `Tool::execute` — minus the turn. Enforcement is entirely
//! server-side and fails closed, in a fixed order: the app must exist with a
//! current revision, the requested tool must be pinned in that revision's
//! manifest bindings, and a live app grant must cover the call. The grant
//! store does not exist yet, so the grant gate refuses every invoke today and
//! the route ships dark.
//!
//! Chat approval gates, permission modes, and plan mode deliberately do not
//! apply: there is no chat. The app grant is the whole policy.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use openwave_core::id::AppId;
use openwave_core::local_app::{AppRecord, AppRevision};
use openwave_core::{AgentError, ChatId, ToolCtx};

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::state::AppState;

/// Body bound for an invoke request: a mounted tool name plus its opaque
/// arguments. Generous for a tool call, far below the MCP client's own 2 MiB
/// JSON-RPC frame bound so an admitted request can always be forwarded.
pub(crate) const MAX_APP_INVOKE_BODY_BYTES: usize = 256 * 1024;

/// Body of `POST /apps/{id}/invoke`.
///
/// `arguments` is opaque passthrough JSON authored inside the sandboxed app
/// frame. The server hands it to the mounted tool verbatim and the renderer
/// never interprets it, so — like [`super::McpAppPayload`] — this request has
/// a hand-written TS twin rather than a generated wire type: the generator's
/// precision guard rightly refuses `any`-shaped fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppInvokeRequest {
    /// Full mounted name (`mcp__{server}__{tool}`) of the pinned tool to run.
    pub tool: String,
    /// Opaque arguments for the tool, passed through untouched.
    #[serde(default)]
    pub arguments: serde_json::Value,
}

/// Result of a granted invoke, packaged for the sandboxed app frame.
///
/// Deliberately not a generated wire type, for the same reason as the
/// request: `structured_content` is opaque passthrough the renderer forwards
/// to the frame without reading. Both halves have already crossed the MCP
/// client's result clamp, so nothing here exceeds the 1 MiB call-result bound.
#[derive(Debug, PartialEq, Serialize)]
pub struct AppInvokeResult {
    /// The call result's text content, clamped by the MCP client.
    pub content: String,
    /// The call's structured content, absent when the server sent none or the
    /// clamp dropped an oversized payload (the text then carries a marker).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<serde_json::Value>,
    /// Whether the external tool reported its own failure.
    pub is_error: bool,
}

/// The stable machine-readable refusals of `POST /apps/{id}/invoke`.
///
/// Unlike the passthrough payloads, the refusal envelope is host-authored and
/// the renderer must branch on it — `consent_required` is the arm that will
/// open the grant sheet — so the kind is a closed generated union rather than
/// a free-form string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum AppInvokeRefusalKind {
    /// No live app with this id.
    AppNotFound,
    /// The requested tool is not pinned in the current revision's manifest.
    NotPinned,
    /// The pinned tool is not covered by a live app grant.
    ConsentRequired,
    /// The pinned name does not resolve to a mounted MCP tool right now.
    UnknownTool,
}

/// The typed refusal body — the `{ kind, message }` shape every route error
/// carries, with the kind closed so a client never string-matches prose.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct AppInvokeRefusal {
    pub kind: AppInvokeRefusalKind,
    pub message: String,
}

impl AppInvokeRefusal {
    fn status(&self) -> StatusCode {
        match self.kind {
            AppInvokeRefusalKind::AppNotFound => StatusCode::NOT_FOUND,
            AppInvokeRefusalKind::NotPinned | AppInvokeRefusalKind::ConsentRequired => {
                StatusCode::FORBIDDEN
            }
            // The manifest pin exists but nothing mounted answers to it — a
            // conflict with current MCP state, not a bad request.
            AppInvokeRefusalKind::UnknownTool => StatusCode::CONFLICT,
        }
    }
}

/// Invoke failure: a typed refusal, or an ordinary server-side failure.
#[derive(Debug)]
pub enum AppInvokeError {
    Refused(AppInvokeRefusal),
    Failed(ServerError),
}

impl AppInvokeError {
    fn refused(kind: AppInvokeRefusalKind, message: impl Into<String>) -> Self {
        Self::Refused(AppInvokeRefusal {
            kind,
            message: message.into(),
        })
    }
}

impl From<AgentError> for AppInvokeError {
    fn from(error: AgentError) -> Self {
        Self::Failed(error.into())
    }
}

impl IntoResponse for AppInvokeError {
    fn into_response(self) -> Response {
        match self {
            Self::Refused(refusal) => (refusal.status(), axum::Json(refusal)).into_response(),
            Self::Failed(error) => error.into_response(),
        }
    }
}

/// `POST /apps/{id}/invoke` — execute one of the app's pinned mounted MCP
/// tools outside any model turn.
///
/// Enforcement order is fixed and fails closed: the app record first, then
/// the manifest pin, then the grant gate, and only then dispatch. Every check
/// runs server-side against stored state, so nothing the renderer asserts can
/// widen what a call reaches.
pub async fn post_app_invoke(
    State(state): State<AppState>,
    Path(app_id): Path<AppId>,
    Json(request): Json<AppInvokeRequest>,
) -> Result<Json<AppInvokeResult>, AppInvokeError> {
    let (app, revision) = current_app_revision(&state, app_id).await?;
    require_pinned(&revision, &request.tool)?;
    require_app_grant(&state, &app, &revision, &request.tool).await?;
    dispatch_mounted_tool(&state, &request.tool, request.arguments)
        .await
        .map(Json)
}

/// Resolve a live app and its current revision, or refuse as absent.
///
/// A deleted app refuses identically to a missing one: soft-deletion removes
/// the app from every renderer surface, so it must remove it from this one.
async fn current_app_revision(
    state: &AppState,
    app_id: AppId,
) -> Result<(AppRecord, AppRevision), AppInvokeError> {
    let absent = || {
        AppInvokeError::refused(
            AppInvokeRefusalKind::AppNotFound,
            format!("no app {app_id}"),
        )
    };
    let app = state.store.get_app(app_id).await?.ok_or_else(absent)?;
    if app.deleted_at.is_some() {
        return Err(absent());
    }
    // A live app always points at a stored revision; if the record is ever
    // inconsistent, fail closed as absent rather than dispatch unpinned.
    let revision = state
        .store
        .get_app_revision(app.current_revision)
        .await?
        .ok_or_else(absent)?;
    Ok((app, revision))
}

/// Refuse any tool the current revision's manifest does not pin.
fn require_pinned(revision: &AppRevision, tool: &str) -> Result<(), AppInvokeError> {
    let pinned = revision
        .manifest
        .bindings
        .iter()
        .any(|binding| binding.tools.iter().any(|pinned| pinned == tool));
    if pinned {
        return Ok(());
    }
    Err(AppInvokeError::refused(
        AppInvokeRefusalKind::NotPinned,
        format!("{tool:?} is not pinned in this app's current manifest"),
    ))
}

/// The consent gate, evaluated after the pin check and before dispatch.
///
/// The grant store does not exist yet, so this refuses every invoke and the
/// route ships dark. The grants slice replaces only this function's body —
/// checking the live grant's `(server, tools)` cover and the bound server's
/// definition fingerprint — while the route's enforcement order stays fixed.
async fn require_app_grant(
    _state: &AppState,
    _app: &AppRecord,
    _revision: &AppRevision,
    tool: &str,
) -> Result<(), AppInvokeError> {
    Err(AppInvokeError::refused(
        AppInvokeRefusalKind::ConsentRequired,
        format!("no live app grant covers {tool:?}"),
    ))
}

/// Resolve a pinned name against the current MCP snapshot and execute it.
///
/// This route invokes mounted MCP tools only. The name must carry the mounted
/// `mcp__{server}__{tool}` shape — which no built-in server tool does — and
/// must resolve to a server-executed registration; a client-executed contract
/// has no executor here and refuses. The execution context is deliberately
/// inert: no chat owns this call and no scratch is attached, so even though
/// `McpTool::execute` ignores its context, nothing dispatched from here could
/// reach a workspace capability. Results cross back through the MCP client's
/// own 1 MiB call-result clamp.
pub(crate) async fn dispatch_mounted_tool(
    state: &AppState,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<AppInvokeResult, AppInvokeError> {
    let unknown = || {
        AppInvokeError::refused(
            AppInvokeRefusalKind::UnknownTool,
            format!("{tool_name:?} is not a mounted MCP tool"),
        )
    };
    if !is_mounted_mcp_name(tool_name) {
        return Err(unknown());
    }
    let registry = state.mcp.snapshot();
    let tool = registry.get(tool_name).ok_or_else(unknown)?;
    let ctx = ToolCtx::without_private_scratch(ChatId::new(), None);
    let output = tool.execute(&ctx, arguments).await?;
    Ok(AppInvokeResult {
        content: output.content,
        structured_content: output.data,
        is_error: output.is_error,
    })
}

/// Whether a name has the mounted `mcp__{server}__{tool}` shape.
///
/// The same grammar the manifest validator enforces on pins; repeated here so
/// dispatch is independently safe when driven without the pin check.
fn is_mounted_mcp_name(name: &str) -> bool {
    name.strip_prefix("mcp__").is_some_and(|rest| {
        rest.split_once("__")
            .is_some_and(|(server, tool)| !server.is_empty() && !tool.is_empty())
    })
}
