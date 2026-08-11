//! `POST /apps/{id}/invoke` — out-of-turn invocation of a local app's pinned
//! capabilities: declared REST operations of `rest_api` connected apps,
//! connected folders, and operations of gateway connected apps.
//!
//! The first tool execution outside a model turn: the sandboxed app frame
//! posts a call to the trusted renderer, the renderer forwards it here on its
//! bearer, and the server executes the pinned operation through the governed
//! REST executor — minus the turn. Enforcement is entirely server-side and
//! fails closed, in a fixed order: the app must exist with a current
//! revision, the requested capability must be pinned in that revision's
//! manifest bindings, and a live app grant must cover the call — including
//! that every granted connected app's current definition still matches the
//! fingerprint recorded at consent.
//!
//! A gateway binding is the one surface that leaves the machine: it is
//! relayed to the gateway's shared-app invoke route as the signed-in user, and
//! the gateway re-enforces entitlement, its own manifest pin, and the viewer's
//! credential live on every call. The local ladder runs first regardless, so
//! an ungranted or stale call never reaches the network.
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
use openwave_core::AgentError;

use crate::connected_apps::{
    current_fingerprints, current_rest_definitions, GatewayDispatchError, GatewayOperationRequest,
};
use crate::connectors::GatewayInvokeOutcome;
use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::rest_executor::{RestExecuteError, RestOperationRequest};
use crate::state::AppState;

/// Body bound for an invoke request: a capability name plus its opaque
/// arguments. Far below the governed REST executor's request-body cap, and
/// exactly that cap.
pub(crate) const MAX_APP_INVOKE_BODY_BYTES: usize = 256 * 1024;

/// Body of `POST /apps/{id}/invoke` — one of the invocable surfaces.
///
/// Exactly one of three: `operation_id` (with optional `parameters`/`body`)
/// for a declared REST operation of a local `rest_api` record, `gateway_app`
/// plus `operation_id` (with optional `path_parameters`/`query`/`body`) for a
/// gateway connected app's operation, or `folder` (with `op` and its fields)
/// for a connected folder. The passthrough halves (`parameters`,
/// `path_parameters`, `query`, `body`) are opaque JSON authored inside the
/// sandboxed app frame; the server hands them to the executor or the gateway
/// verbatim and the renderer never interprets them, so — like
/// [`super::McpAppPayload`] — this request has a hand-written TS twin rather
/// than a generated wire type: the generator's precision guard rightly refuses
/// `any`-shaped fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppInvokeRequest {
    /// Catalog `operationId` of the pinned operation to execute — of a local
    /// `rest_api` record, or of `gateway_app` when that is present.
    pub operation_id: Option<String>,
    /// Declared parameter values for a local REST operation, name → JSON
    /// scalar.
    pub parameters: Option<serde_json::Value>,
    /// JSON request body, only when the operation declares one.
    pub body: Option<serde_json::Value>,
    /// The gateway's connected-app id, for a gateway operation. Crosses to
    /// the gateway as `connected_app_id`, the name its own invoke route uses.
    pub gateway_app: Option<String>,
    /// Path-template values for a gateway operation.
    pub path_parameters: Option<serde_json::Value>,
    /// Query values for a gateway operation.
    pub query: Option<serde_json::Value>,
    /// Root id of the pinned connected folder, for a folder operation.
    pub folder: Option<openwave_core::id::HostRootId>,
    /// Folder operation: `list`, `read`, or `write`.
    pub op: Option<String>,
    /// Folder-relative path; empty or absent means the folder root for
    /// `list`, and is invalid for `read` and `write`.
    pub path: Option<String>,
    /// Base64 content for a folder `write`.
    pub content_base64: Option<String>,
    /// Whether a folder `write` may replace an existing file; absent means a
    /// create that refuses to overwrite.
    pub replace: Option<bool>,
}

/// Result of a granted REST-operation invoke, packaged for the sandboxed app
/// frame — a hand-written TS twin rather than a generated wire type, for the
/// same passthrough reason as the request.
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
    /// The pinned name does not resolve to a declared catalog operation or an
    /// available connected folder right now.
    UnknownTool,
    /// The manifest pins a gateway operation but nothing at the gateway
    /// answers for it: no session, no registered draft, or a deployment that
    /// does not serve shared-app invokes. The message says which.
    GatewayUnavailable,
    /// The gateway reached the bound app and had no credential for this
    /// viewer. Only the viewer can fix it, and only at the gateway — so the
    /// frame renders a connect prompt rather than an error.
    GatewayAuthorizationRequired,
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
            AppInvokeRefusalKind::NotPinned
            | AppInvokeRefusalKind::ConsentRequired
            // The call was authorized here and refused there, for want of a
            // credential only the viewer can supply: forbidden, not a
            // conflict with local state.
            | AppInvokeRefusalKind::GatewayAuthorizationRequired => StatusCode::FORBIDDEN,
            // The manifest pin exists but nothing configured answers to it —
            // a conflict with current state, not a bad request.
            AppInvokeRefusalKind::UnknownTool | AppInvokeRefusalKind::GatewayUnavailable => {
                StatusCode::CONFLICT
            }
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
    Operation(RestOperationRequest),
    Gateway(GatewayOperationRequest),
    Folder {
        folder: openwave_core::id::HostRootId,
        path: String,
        op: FolderOp,
    },
}

/// One folder operation, as admitted by the request reader.
enum FolderOp {
    List,
    Read,
    Write {
        content_base64: String,
        replace: bool,
    },
}

impl FolderOp {
    fn writes(&self) -> bool {
        matches!(self, Self::Write { .. })
    }
}

/// Read the request's surface, refusing a body that names several or none.
///
/// The three surfaces take three disjoint field sets, and each refuses the
/// others' outright rather than ignoring them: a caller that sent `query` to a
/// local operation, or `parameters` to a gateway one, meant something the
/// server would not have done, and silently dropping the field is how a call
/// ends up wider or narrower than its author believed.
fn requested_surface(request: AppInvokeRequest) -> Result<InvokeSurface, AppInvokeError> {
    let invalid = |message: &str| {
        AppInvokeError::Failed(ServerError::unprocessable_kind(
            "invalid_invoke_request",
            message,
        ))
    };
    let folder_fields = request.op.is_some()
        || request.path.is_some()
        || request.content_base64.is_some()
        || request.replace.is_some();
    match (request.operation_id, request.folder, request.gateway_app) {
        (Some(operation_id), None, Some(gateway_app)) => {
            if folder_fields || request.parameters.is_some() {
                return Err(invalid(
                    "a gateway_app invoke takes operation_id, path_parameters, query, \
                     and body and nothing else",
                ));
            }
            Ok(InvokeSurface::Gateway(GatewayOperationRequest {
                gateway_app,
                operation_id,
                path_parameters: request.path_parameters,
                query: request.query,
                body: request.body,
            }))
        }
        (Some(operation_id), None, None) => {
            // `path_parameters` and `query` are the gateway's vocabulary. A
            // local operation takes its values through `parameters`, which the
            // catalog validates against declared parameter locations, so
            // admitting the gateway spellings here would widen the local
            // surface with fields nothing enforces.
            if folder_fields || request.path_parameters.is_some() || request.query.is_some() {
                return Err(invalid(
                    "an operation_id invoke takes parameters and body and nothing else",
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
        (None, Some(folder), None) => {
            if request.parameters.is_some()
                || request.body.is_some()
                || request.path_parameters.is_some()
                || request.query.is_some()
            {
                return Err(invalid(
                    "a folder invoke takes op, path, content_base64, and replace \
                     and nothing else",
                ));
            }
            let path = request.path.unwrap_or_default();
            let op = match request.op.as_deref() {
                Some("list") => {
                    if request.content_base64.is_some() || request.replace.is_some() {
                        return Err(invalid("a folder list takes op, folder, and path only"));
                    }
                    FolderOp::List
                }
                Some("read") => {
                    if request.content_base64.is_some() || request.replace.is_some() {
                        return Err(invalid("a folder read takes op, folder, and path only"));
                    }
                    if path.is_empty() {
                        return Err(invalid("a folder read needs a path"));
                    }
                    FolderOp::Read
                }
                Some("write") => {
                    let Some(content_base64) = request.content_base64 else {
                        return Err(invalid("a folder write needs content_base64"));
                    };
                    if path.is_empty() {
                        return Err(invalid("a folder write needs a path"));
                    }
                    FolderOp::Write {
                        content_base64,
                        replace: request.replace.unwrap_or(false),
                    }
                }
                _ => return Err(invalid("op must be one of list, read, or write")),
            };
            Ok(InvokeSurface::Folder { folder, path, op })
        }
        (None, None, Some(_)) => Err(invalid("a gateway_app invoke needs an operation_id")),
        _ => Err(invalid(
            "exactly one of operation_id (optionally with gateway_app) or folder \
             must be provided",
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
        InvokeSurface::Gateway(request) => {
            require_pinned_gateway_operation(
                &revision,
                &request.gateway_app,
                &request.operation_id,
            )?;
            let current = require_app_grant(
                &state,
                &app,
                &revision,
                &Pinned::GatewayOperation {
                    gateway_app: &request.gateway_app,
                    operation_id: &request.operation_id,
                },
            )
            .await?;
            // The consent sheet's label for the app, when the currency read
            // that just passed carried one — every refusal below names the app
            // the viewer would recognize, not the gateway's opaque id.
            let display_name = current
                .gateway_apps
                .get(&request.gateway_app)
                .map_or(request.gateway_app.as_str(), |app| app.name.as_str())
                .to_owned();
            dispatch_gateway_operation(&state, app_id, &request, &display_name)
                .await
                .map(|result| Json(result).into_response())
        }
        InvokeSurface::Folder { folder, path, op } => {
            require_pinned_folder(&revision, folder, op.writes())?;
            require_app_grant(
                &state,
                &app,
                &revision,
                &Pinned::Folder {
                    folder,
                    writes: op.writes(),
                },
            )
            .await?;
            dispatch_folder_op(&state, app_id, folder, &path, op)
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
            // A gateway binding pins operation ids too, but of an app only
            // the gateway can execute. This surface dispatches to a local
            // `rest_api` record, so a gateway pin must never admit a call to
            // it — the relay is its own surface.
            AppBinding::Folder(_) | AppBinding::GatewayOperations(_) => false,
        });
    if pinned {
        return Ok(());
    }
    Err(AppInvokeError::refused(
        AppInvokeRefusalKind::NotPinned,
        format!("operation {operation_id:?} is not pinned in this app's current manifest"),
    ))
}

/// Refuse any folder operation the current revision's manifest does not pin
/// at the required access level: a write needs a `read_write` binding, and a
/// `read` binding never pins one.
fn require_pinned_folder(
    revision: &AppRevision,
    folder: openwave_core::id::HostRootId,
    writes: bool,
) -> Result<(), AppInvokeError> {
    let pinned = revision
        .manifest
        .bindings
        .iter()
        .any(|binding| match binding {
            AppBinding::Folder(binding) => {
                binding.folder == folder
                    && (!writes
                        || binding.access == openwave_core::local_app::FolderAccess::ReadWrite)
            }
            AppBinding::Operations(_) | AppBinding::GatewayOperations(_) => false,
        });
    if pinned {
        return Ok(());
    }
    Err(AppInvokeError::refused(
        AppInvokeRefusalKind::NotPinned,
        if writes {
            format!("folder {folder} is not pinned for writing in this app's current manifest")
        } else {
            format!("folder {folder} is not pinned in this app's current manifest")
        },
    ))
}

/// Refuse any gateway operation the current revision's manifest does not pin
/// under that exact gateway app.
///
/// The pin is the pair, never the operation id alone: two gateway apps may
/// well declare the same id, and a manifest that binds one of them must not
/// admit a call to the other.
fn require_pinned_gateway_operation(
    revision: &AppRevision,
    gateway_app: &str,
    operation_id: &str,
) -> Result<(), AppInvokeError> {
    let pinned = revision
        .manifest
        .bindings
        .iter()
        .any(|binding| match binding {
            AppBinding::GatewayOperations(binding) => {
                binding.gateway_app == gateway_app
                    && binding
                        .operation_ids
                        .iter()
                        .any(|pinned| pinned == operation_id)
            }
            AppBinding::Operations(_) | AppBinding::Folder(_) => false,
        });
    if pinned {
        return Ok(());
    }
    Err(AppInvokeError::refused(
        AppInvokeRefusalKind::NotPinned,
        format!(
            "operation {operation_id:?} of gateway app {gateway_app} is not pinned in \
             this app's current manifest"
        ),
    ))
}

/// The invoked capability, for the grant gate.
enum Pinned<'a> {
    Operation(&'a str),
    GatewayOperation {
        gateway_app: &'a str,
        operation_id: &'a str,
    },
    Folder {
        folder: openwave_core::id::HostRootId,
        writes: bool,
    },
}

impl Pinned<'_> {
    fn description(&self) -> String {
        match self {
            Self::Operation(operation_id) => format!("operation {operation_id:?}"),
            Self::GatewayOperation {
                gateway_app,
                operation_id,
            } => format!("operation {operation_id:?} of gateway app {gateway_app}"),
            Self::Folder { folder, writes } => {
                if *writes {
                    format!("writing folder {folder}")
                } else {
                    format!("folder {folder}")
                }
            }
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
///    describes different capabilities. A gateway grant pins the deployment,
///    the app id, and the catalog hash, so a re-paired profile or a
///    re-ingested app re-prompts before anything is relayed.
///
/// The currency read is returned so callers can label a refusal with what the
/// gateway currently calls an app, without a second live read.
async fn require_app_grant(
    state: &AppState,
    app: &AppRecord,
    revision: &AppRevision,
    pinned: &Pinned<'_>,
) -> Result<crate::connected_apps::CurrentFingerprints, AppInvokeError> {
    let consent_required =
        |message: String| AppInvokeError::refused(AppInvokeRefusalKind::ConsentRequired, message);
    let Some(grant) = state.store.get_app_grant(app.id).await? else {
        return Err(consent_required(format!(
            "no live app grant covers {}",
            pinned.description()
        )));
    };
    // The pin check has already passed. For an app-keyed capability, exactly
    // one current-manifest binding names it and its connected app is the
    // record the grant must cover the capability under; for a folder
    // capability, the grant must hold the same root at an access level that
    // covers the operation — a `read_write` grant covers reads, a `read`
    // grant never covers a write.
    let covered = match pinned {
        Pinned::Folder { folder, writes } => grant.bindings.iter().any(|binding| {
            matches!(
                binding,
                AppGrantBinding::Folder(granted)
                    if granted.folder == *folder
                        && (!writes
                            || granted.access
                                == openwave_core::local_app::FolderAccess::ReadWrite)
            )
        }),
        // A gateway capability is keyed by the gateway's app id, which the
        // grant carries directly — there is no local record to resolve it
        // through, so the pair is matched as bound.
        Pinned::GatewayOperation {
            gateway_app,
            operation_id,
        } => grant.bindings.iter().any(|binding| {
            matches!(
                binding,
                AppGrantBinding::GatewayOperations(granted)
                    if granted.gateway_app == *gateway_app
                        && granted
                            .operation_ids
                            .iter()
                            .any(|granted| granted == operation_id)
            )
        }),
        Pinned::Operation(_) => {
            let connected_app = revision
                .manifest
                .bindings
                .iter()
                .find(|binding| match (binding, pinned) {
                    (AppBinding::Operations(binding), Pinned::Operation(operation_id)) => binding
                        .operation_ids
                        .iter()
                        .any(|held| held == operation_id),
                    _ => false,
                })
                .and_then(AppBinding::app);
            connected_app.is_some_and(|connected_app| {
                grant
                    .bindings
                    .iter()
                    .any(|binding| match (binding, pinned) {
                        (AppGrantBinding::Operations(binding), Pinned::Operation(operation_id)) => {
                            binding.app == connected_app
                                && binding
                                    .operation_ids
                                    .iter()
                                    .any(|granted| granted == operation_id)
                        }
                        _ => false,
                    })
            })
        }
    };
    if !covered {
        return Err(consent_required(format!(
            "the app grant does not cover {}",
            pinned.description()
        )));
    }
    let current = current_fingerprints(
        state,
        &crate::connected_apps::gateway_apps_granted_by(&grant.bindings),
    )
    .await?;
    for binding in &grant.bindings {
        // Every granted binding must still pin what its target carries now:
        // a reconfigured connected app or a disconnected folder invalidates
        // the whole grant, failing closed to re-consent.
        if !current.grant_binding_current(binding) {
            return Err(consent_required(match binding {
                AppGrantBinding::Operations(binding) => format!(
                    "connected app {} was reconfigured after consent; the grant is \
                     stale",
                    binding.app
                ),
                AppGrantBinding::Folder(_) => {
                    "a granted folder was disconnected after consent; the grant is stale".to_owned()
                }
                AppGrantBinding::GatewayOperations(binding) => format!(
                    "gateway app {} no longer reads as it did at consent; the grant \
                     is stale",
                    binding.gateway_app
                ),
            }));
        }
    }
    Ok(current)
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

/// Relay one granted gateway operation through the dispatch seam.
///
/// Everything local has already passed: the manifest pins the pair, the grant
/// covers it, and the gateway's catalog still reads as it did at consent. What
/// is left is the gateway's own live enforcement — entitlement, its manifest,
/// and the viewer's credential — so its answers map by who can act on them.
/// An executed call is passthrough, identical to the local REST path. A typed
/// `authorization_required` is a refusal the *viewer* resolves, at the gateway
/// and nowhere else, so it crosses machine-readably rather than as prose. Any
/// other gateway refusal is the app's to present, as an `is_error` result in
/// the gateway's own words. And a relay that could not happen at all is
/// `gateway_unavailable`, whose message says which of the two reasons it was.
async fn dispatch_gateway_operation(
    state: &AppState,
    app_id: AppId,
    request: &GatewayOperationRequest,
    display_name: &str,
) -> Result<AppRestInvokeResult, AppInvokeError> {
    let failure = |error: String| AppRestInvokeResult {
        status: None,
        content_type: None,
        body_base64: None,
        is_error: true,
        error: Some(error),
    };
    match state.gateway_dispatch.dispatch(app_id, request).await {
        Ok(GatewayInvokeOutcome::Executed {
            status,
            content_type,
            body_base64,
        }) => Ok(AppRestInvokeResult {
            status: Some(status),
            content_type,
            body_base64: Some(body_base64),
            is_error: false,
            error: None,
        }),
        Ok(GatewayInvokeOutcome::AuthorizationRequired { message }) => {
            Err(AppInvokeError::refused(
                AppInvokeRefusalKind::GatewayAuthorizationRequired,
                format!("connect {display_name} at your model gateway to continue: {message}"),
            ))
        }
        // Consent the relay could not heal: it re-states the author's consent
        // and calls again exactly once, so reaching here means the gateway
        // refused twice. That is the app's to present, in the gateway's own
        // words, like any other refusal.
        Ok(GatewayInvokeOutcome::ConsentRequired { message })
        | Ok(GatewayInvokeOutcome::Refused { message }) => Ok(failure(message)),
        Err(GatewayDispatchError::NoSession) => Err(AppInvokeError::refused(
            AppInvokeRefusalKind::GatewayUnavailable,
            format!(
                "{display_name} is served by your model gateway, and this profile has no \
                 gateway session to call it with"
            ),
        )),
        Err(GatewayDispatchError::NotRegistered) => Err(AppInvokeError::refused(
            AppInvokeRefusalKind::GatewayUnavailable,
            format!(
                "this app is not registered at your model gateway, so {display_name} \
                 cannot be called yet"
            ),
        )),
        Err(GatewayDispatchError::Unreachable(error)) => Ok(failure(error)),
    }
}

/// Result of a granted folder invoke, packaged for the sandboxed app frame —
/// the folder sibling of [`AppRestInvokeResult`], and a hand-written TS twin
/// for the same reason.
///
/// Exactly one payload half is present per operation: `entries` for a list,
/// `content_base64` for a read, `replaced` for a write. A refused or failed
/// operation is `is_error: true` with the seam's closed failure vocabulary —
/// never a path, never an OS error, never a 500.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct AppFolderInvokeResult {
    /// Directory entries, for a `list`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<AppFolderEntry>>,
    /// Base64 of the file's bytes (at most the host's binary read bound),
    /// for a `read`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_base64: Option<String>,
    /// Whether an existing file was replaced, for a `write`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaced: Option<bool>,
    /// Whether the operation failed.
    pub is_error: bool,
    /// The closed failure text when `is_error`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One listed entry under a granted folder.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct AppFolderEntry {
    pub name: String,
    pub directory: bool,
}

impl AppFolderInvokeResult {
    fn failure(error: crate::host_folders::FolderOpError) -> Self {
        Self {
            entries: None,
            content_base64: None,
            replaced: None,
            is_error: true,
            error: Some(error.to_string()),
        }
    }

    fn success() -> Self {
        Self {
            entries: None,
            content_base64: None,
            replaced: None,
            is_error: false,
            error: None,
        }
    }
}

/// Execute one granted folder operation through the host-folder seam.
///
/// An embedding without the seam refuses with `unknown_tool`: the manifest
/// pin exists but nothing configured answers to it — the same reading as a
/// pinned name no server mounts. Operation failures cross as `is_error`
/// results in the seam's closed vocabulary, exactly like REST executor
/// failures.
async fn dispatch_folder_op(
    state: &AppState,
    app_id: AppId,
    folder: openwave_core::id::HostRootId,
    path: &str,
    op: FolderOp,
) -> Result<AppFolderInvokeResult, AppInvokeError> {
    let Some(host) = &state.host_folders else {
        return Err(AppInvokeError::refused(
            AppInvokeRefusalKind::UnknownTool,
            "connected folders are not available in this embedding",
        ));
    };
    Ok(match op {
        FolderOp::List => match host.list_folder(app_id, folder, path).await {
            Ok(entries) => AppFolderInvokeResult {
                entries: Some(
                    entries
                        .into_iter()
                        .map(|entry| AppFolderEntry {
                            name: entry.name,
                            directory: entry.directory,
                        })
                        .collect(),
                ),
                ..AppFolderInvokeResult::success()
            },
            Err(error) => AppFolderInvokeResult::failure(error),
        },
        FolderOp::Read => match host.read_file(app_id, folder, path).await {
            Ok(bytes) => AppFolderInvokeResult {
                content_base64: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
                ..AppFolderInvokeResult::success()
            },
            Err(error) => AppFolderInvokeResult::failure(error),
        },
        FolderOp::Write {
            content_base64,
            replace,
        } => {
            let Ok(content) = base64::engine::general_purpose::STANDARD.decode(content_base64)
            else {
                return Ok(AppFolderInvokeResult::failure(
                    crate::host_folders::FolderOpError::Failed,
                ));
            };
            match host
                .write_file(app_id, folder, path, &content, replace)
                .await
            {
                Ok(receipt) => AppFolderInvokeResult {
                    replaced: Some(receipt.replaced),
                    ..AppFolderInvokeResult::success()
                },
                Err(error) => AppFolderInvokeResult::failure(error),
            }
        }
    })
}
