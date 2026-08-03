//! `POST /apps/{id}/invoke` — out-of-turn invocation of a local app's pinned
//! capabilities: mounted MCP tools and declared REST operations.
//!
//! The first tool execution outside a model turn: the sandboxed app frame
//! posts a call to the trusted renderer, the renderer forwards it here on its
//! bearer, and the server performs the same dispatch a turn would — registry
//! snapshot and `Tool::execute` for a mounted MCP tool, the governed REST
//! executor for a declared operation — minus the turn. Enforcement is
//! entirely server-side and fails closed, in a fixed order: the app must
//! exist with a current revision, the requested capability must be pinned in
//! that revision's manifest bindings, and a live app grant must cover the
//! call — including that every granted connected app's current definition
//! still matches the fingerprint recorded at consent.
//!
//! Chat approval gates, permission modes, and plan mode deliberately do not
//! apply: there is no chat. The app grant is the whole policy.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use openwave_core::id::AppId;
use openwave_core::local_app::{AppBinding, AppGrantBinding, AppRecord, AppRevision};
use openwave_core::{AgentError, ChatId, ToolCtx};

use crate::connected_apps::{current_app_fingerprints, current_rest_definitions};
use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::rest_executor::{RestExecuteError, RestOperationRequest};
use crate::state::AppState;

/// Body bound for an invoke request: a capability name plus its opaque
/// arguments. Generous for a tool call, far below the MCP client's own 2 MiB
/// JSON-RPC frame bound so an admitted request can always be forwarded, and
/// exactly the governed REST executor's request-body cap.
pub(crate) const MAX_APP_INVOKE_BODY_BYTES: usize = 256 * 1024;

/// Body of `POST /apps/{id}/invoke` — one of the two invocable surfaces.
///
/// Either `tool` (with optional `arguments`) for a mounted MCP tool, or
/// `operation_id` (with optional `parameters`/`body`) for a declared REST
/// operation — never both, never neither. The passthrough halves
/// (`arguments`, `parameters`, `body`) are opaque JSON authored inside the
/// sandboxed app frame; the server hands them to the executor verbatim and
/// the renderer never interprets them, so — like [`super::McpAppPayload`] —
/// this request has a hand-written TS twin rather than a generated wire type:
/// the generator's precision guard rightly refuses `any`-shaped fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppInvokeRequest {
    /// Full mounted name (`mcp__{server}__{tool}`) of the pinned tool to run.
    pub tool: Option<String>,
    /// Opaque arguments for the tool, passed through untouched.
    pub arguments: Option<serde_json::Value>,
    /// Catalog `operationId` of the pinned REST operation to execute.
    pub operation_id: Option<String>,
    /// Declared parameter values for the operation, name → JSON scalar.
    pub parameters: Option<serde_json::Value>,
    /// JSON request body, only when the operation declares one.
    pub body: Option<serde_json::Value>,
}

/// Result of a granted MCP-tool invoke, packaged for the sandboxed app frame.
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

/// Result of a granted REST-operation invoke, packaged for the sandboxed app
/// frame — [`AppInvokeResult`]'s sibling for the `rest_api` surface, and a
/// hand-written TS twin for the same reason.
///
/// An executed operation crosses as opaque passthrough: whatever status the
/// API answered (including 4xx/5xx and unfollowed redirects) with
/// `is_error: false`, the raw body base64-encoded so binary responses
/// survive JSON. A refused or failed execution (validation, egress, or
/// transport) is `is_error: true` with the executor's closed, host-authored
/// refusal text — never a 500 and never internals.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct AppRestInvokeResult {
    /// HTTP status the operation answered with; absent when execution failed
    /// before a response existed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Raw `Content-Type` header value, when the response carried one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Base64 of the raw response body (at most the executor's 4 MiB cap);
    /// absent when execution failed before a response existed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_base64: Option<String>,
    /// Whether execution failed before an HTTP response existed.
    pub is_error: bool,
    /// The executor's refusal text when `is_error` — closed vocabulary, no
    /// resolved addresses, no credential material.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    /// The requested capability is not pinned in the current revision's
    /// manifest.
    NotPinned,
    /// The pinned capability is not covered by a live app grant.
    ConsentRequired,
    /// The pinned name does not resolve to a mounted MCP tool or a declared
    /// catalog operation right now.
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
            // The manifest pin exists but nothing configured answers to it —
            // a conflict with current state, not a bad request.
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

/// Which surface one admitted request names.
enum InvokeSurface {
    Tool {
        tool: String,
        arguments: serde_json::Value,
    },
    Operation(RestOperationRequest),
}

/// Read the request's surface, refusing a body that names both or neither.
fn requested_surface(request: AppInvokeRequest) -> Result<InvokeSurface, AppInvokeError> {
    let invalid = |message: &str| {
        AppInvokeError::Failed(ServerError::unprocessable_kind(
            "invalid_invoke_request",
            message,
        ))
    };
    match (request.tool, request.operation_id) {
        (Some(tool), None) => {
            if request.parameters.is_some() || request.body.is_some() {
                return Err(invalid(
                    "parameters and body belong to operation_id invokes; a tool \
                     invoke takes arguments",
                ));
            }
            Ok(InvokeSurface::Tool {
                tool,
                arguments: request.arguments.unwrap_or_default(),
            })
        }
        (None, Some(operation_id)) => {
            if request.arguments.is_some() {
                return Err(invalid(
                    "arguments belong to tool invokes; an operation_id invoke \
                     takes parameters and body",
                ));
            }
            Ok(InvokeSurface::Operation(RestOperationRequest {
                operation_id,
                parameters: request
                    .parameters
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
                body: request.body,
            }))
        }
        (Some(_), Some(_)) | (None, None) => Err(invalid(
            "exactly one of tool or operation_id must be provided",
        )),
    }
}

/// `POST /apps/{id}/invoke` — execute one of the app's pinned capabilities
/// outside any model turn.
///
/// Enforcement order is fixed and fails closed: the app record first, then
/// the manifest pin, then the grant gate, and only then dispatch. Every check
/// runs server-side against stored state, so nothing the renderer asserts can
/// widen what a call reaches.
pub async fn post_app_invoke(
    State(state): State<AppState>,
    Path(app_id): Path<AppId>,
    Json(request): Json<AppInvokeRequest>,
) -> Result<Response, AppInvokeError> {
    let surface = requested_surface(request)?;
    let (app, revision) = current_app_revision(&state, app_id).await?;
    match surface {
        InvokeSurface::Tool { tool, arguments } => {
            require_pinned_tool(&revision, &tool)?;
            require_app_grant(&state, &app, &revision, &Pinned::Tool(&tool)).await?;
            dispatch_mounted_tool(&state, &tool, arguments)
                .await
                .map(|result| Json(result).into_response())
        }
        InvokeSurface::Operation(request) => {
            require_pinned_operation(&revision, &request.operation_id)?;
            require_app_grant(
                &state,
                &app,
                &revision,
                &Pinned::Operation(&request.operation_id),
            )
            .await?;
            dispatch_rest_operation(&state, &revision, &request)
                .await
                .map(|result| Json(result).into_response())
        }
    }
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
fn require_pinned_tool(revision: &AppRevision, tool: &str) -> Result<(), AppInvokeError> {
    let pinned = revision
        .manifest
        .bindings
        .iter()
        .any(|binding| match binding {
            AppBinding::Tools(binding) => binding.tools.iter().any(|pinned| pinned == tool),
            AppBinding::Operations(_) => false,
        });
    if pinned {
        return Ok(());
    }
    Err(AppInvokeError::refused(
        AppInvokeRefusalKind::NotPinned,
        format!("{tool:?} is not pinned in this app's current manifest"),
    ))
}

/// Refuse any operation the current revision's manifest does not pin.
fn require_pinned_operation(
    revision: &AppRevision,
    operation_id: &str,
) -> Result<(), AppInvokeError> {
    let pinned = revision
        .manifest
        .bindings
        .iter()
        .any(|binding| match binding {
            AppBinding::Operations(binding) => binding
                .operation_ids
                .iter()
                .any(|pinned| pinned == operation_id),
            AppBinding::Tools(_) => false,
        });
    if pinned {
        return Ok(());
    }
    Err(AppInvokeError::refused(
        AppInvokeRefusalKind::NotPinned,
        format!("operation {operation_id:?} is not pinned in this app's current manifest"),
    ))
}

/// The invoked capability, for the grant gate.
enum Pinned<'a> {
    Tool(&'a str),
    Operation(&'a str),
}

impl Pinned<'_> {
    fn description(&self) -> String {
        match self {
            Self::Tool(tool) => format!("{tool:?}"),
            Self::Operation(operation_id) => format!("operation {operation_id:?}"),
        }
    }
}

/// The consent gate, evaluated after the pin check and before dispatch.
///
/// Three checks, all live and all fail-closed to `consent_required` — a
/// missing or stale grant is a re-prompt, never an error:
///
/// 1. A grant exists for the app.
/// 2. It covers the invoked `(connected app, capability)` pair as the
///    *current* revision's manifest binds it, in the same vocabulary — so a
///    revision that widens the manifest exceeds the grant by construction,
///    with no special-casing.
/// 3. Every granted connected app's current definition fingerprint equals the
///    granted one. A reconfigured or deleted record invalidates the whole
///    grant: consent named a definition, not a name, and must never outlive
///    it. The `mcp_server` fingerprint covers the namespace and the
///    `rest_api` fingerprint the base URL, document hash, and credential
///    reference, so a repointed record can never keep a grant that now
///    describes different capabilities.
async fn require_app_grant(
    state: &AppState,
    app: &AppRecord,
    revision: &AppRevision,
    pinned: &Pinned<'_>,
) -> Result<(), AppInvokeError> {
    let consent_required =
        |message: String| AppInvokeError::refused(AppInvokeRefusalKind::ConsentRequired, message);
    let Some(grant) = state.store.get_app_grant(app.id).await? else {
        return Err(consent_required(format!(
            "no live app grant covers {}",
            pinned.description()
        )));
    };
    // The pin check has already passed, so exactly one current-manifest
    // binding names this capability; its connected app is the record the
    // grant must cover the capability under.
    let connected_app = revision
        .manifest
        .bindings
        .iter()
        .find(|binding| match (binding, pinned) {
            (AppBinding::Tools(binding), Pinned::Tool(tool)) => {
                binding.tools.iter().any(|held| held == tool)
            }
            (AppBinding::Operations(binding), Pinned::Operation(operation_id)) => binding
                .operation_ids
                .iter()
                .any(|held| held == operation_id),
            _ => false,
        })
        .map(AppBinding::app);
    let covered = connected_app.is_some_and(|connected_app| {
        grant
            .bindings
            .iter()
            .any(|binding| match (binding, pinned) {
                (AppGrantBinding::Tools(binding), Pinned::Tool(tool)) => {
                    binding.app == connected_app
                        && binding.tools.iter().any(|granted| granted == tool)
                }
                (AppGrantBinding::Operations(binding), Pinned::Operation(operation_id)) => {
                    binding.app == connected_app
                        && binding
                            .operation_ids
                            .iter()
                            .any(|granted| granted == operation_id)
                }
                _ => false,
            })
    });
    if !covered {
        return Err(consent_required(format!(
            "the app grant does not cover {}",
            pinned.description()
        )));
    }
    let current = current_app_fingerprints(state).await?;
    for binding in &grant.bindings {
        let matches = current
            .get(&binding.app())
            .is_some_and(|app| app.fingerprint == binding.fingerprint());
        if !matches {
            return Err(consent_required(format!(
                "connected app {} was reconfigured after consent; the grant is stale",
                binding.app()
            )));
        }
    }
    Ok(())
}

/// Resolve a pinned name against the current MCP snapshot and execute it.
///
/// This path invokes mounted MCP tools only. The name must carry the mounted
/// `mcp__{server}__{tool}` shape — which no built-in server tool does — and
/// must resolve to a server-executed registration; a client-executed contract
/// has no executor here and refuses. The execution context is deliberately
/// inert: no chat owns this call and no scratch is attached, so nothing
/// dispatched from here could reach a workspace capability. The synthetic
/// chat id is process-stable rather than per-call: gateway tools resolve a
/// per-chat call credential from it, and a fresh id per invoke would mint a
/// fresh gateway attestation context per invoke — pure token churn, since an
/// app invoke has no model emission to attest and gateway-attested tools
/// refuse it regardless (the recorded Direct-endpoints-only limitation).
/// Results cross back through the MCP client's own 1 MiB call-result clamp.
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
    static APP_INVOKE_CHAT: std::sync::LazyLock<ChatId> = std::sync::LazyLock::new(ChatId::new);
    let ctx = ToolCtx::without_private_scratch(*APP_INVOKE_CHAT, None);
    let output = tool.execute(&ctx, arguments).await?;
    Ok(AppInvokeResult {
        content: output.content,
        structured_content: output.data,
        is_error: output.is_error,
    })
}

/// Resolve the pinned operation's `rest_api` record and run the governed
/// executor against it.
///
/// The grant gate has already matched the record's live fingerprint, so a
/// missing or unparseable record here is a stale read raced with an edit —
/// refused as `consent_required`, the same verdict the sweep would reach. An
/// operation the current catalog no longer declares refuses as
/// `unknown_tool` (the fingerprint sweep makes this unreachable in practice:
/// a changed document moves the fingerprint first). Every other executor
/// refusal — validation, egress vetting, secret resolution, transport — comes
/// back as an `is_error` result, never a 500: those failures are the app's to
/// present, and their closed messages carry no internals.
async fn dispatch_rest_operation(
    state: &AppState,
    revision: &AppRevision,
    request: &RestOperationRequest,
) -> Result<AppRestInvokeResult, AppInvokeError> {
    let bound = revision
        .manifest
        .bindings
        .iter()
        .find_map(|binding| match binding {
            AppBinding::Operations(binding)
                if binding
                    .operation_ids
                    .iter()
                    .any(|pinned| pinned == &request.operation_id) =>
            {
                Some(binding.app)
            }
            _ => None,
        })
        .expect("the pin check admitted this operation");
    let definitions = current_rest_definitions(state).await?;
    let Some((_, _, definition)) = definitions.into_iter().find(|(id, _, _)| *id == bound) else {
        return Err(AppInvokeError::refused(
            AppInvokeRefusalKind::ConsentRequired,
            format!("connected app {bound} was reconfigured after consent; the grant is stale"),
        ));
    };
    match state
        .rest_dispatch
        .dispatch(&definition.target(), &definition.catalog, request)
        .await
    {
        Ok(response) => Ok(AppRestInvokeResult {
            status: Some(response.status),
            content_type: response.content_type,
            body_base64: Some(base64::engine::general_purpose::STANDARD.encode(&response.body)),
            is_error: false,
            error: None,
        }),
        Err(RestExecuteError::UnknownOperation { operation_id }) => Err(AppInvokeError::refused(
            AppInvokeRefusalKind::UnknownTool,
            format!("operation {operation_id:?} is not declared by the connected app"),
        )),
        Err(error) => Ok(AppRestInvokeResult {
            status: None,
            content_type: None,
            body_base64: None,
            is_error: true,
            error: Some(error.to_string()),
        }),
    }
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
