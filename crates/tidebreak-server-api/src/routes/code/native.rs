//! `/code/native/*` routes: the engine-facing native computer-use channel.
//!
//! These routes authenticate with the per-session capability bearer minted
//! by [`crate::code::native_channel::NativeTokenRegistry`] — never the
//! per-launch app token. They are registered outside `require_token` (see
//! `crate::app`), and each handler resolves its own `Authorization` header.
//!
//! The wire contract is [`ComputerUseCall`] in and [`ComputerUseResult`]
//! out. The subject — owner, workspace, session — is derived exclusively
//! from the token; request bodies carry no identity. Tool names and
//! argument schemas come from `tidebreak_core::computer_use`, so a new
//! primitive registered there is accepted here without a route change.

use std::sync::Arc;

use axum::http::header::AUTHORIZATION;
use axum::http::HeaderMap;

use tidebreak_core::computer_session::{ComputerUseCall, ComputerUseResult};
use tidebreak_core::computer_use::validate_computer_use_arguments;
use tidebreak_core::{db, CodeWorkspaceStatus, SessionLifecycle};

use crate::code::native_channel::NativeSubject;
use crate::code::native_runtime::{NativeRuntime, NativeRuntimeError, NativeRuntimeScope};
use crate::error::ServerError;
use crate::extract::Json;
use crate::state::AppState;

/// Execute one native computer-use operation for the token's session.
///
/// On [`NativeRuntimeError::Recovered`] the stored result for the same call
/// identity and arguments is answered instead, so a bridge that lost the
/// original response can safely re-send the identical call.
pub async fn native_execute(
    axum::extract::State(state): axum::extract::State<AppState>,
    headers: HeaderMap,
    Json(call): Json<ComputerUseCall>,
) -> Result<Json<ComputerUseResult>, ServerError> {
    let subject = authorize(&state, &headers).await?;
    require_well_formed(&call)?;
    let runtime = attached_runtime(&state)?;
    let scope = NativeRuntimeScope::from(subject);
    match runtime.execute(&scope, &call).await {
        Ok(result) => Ok(Json(result)),
        Err(NativeRuntimeError::Recovered) => match runtime.result_for_call(&scope, &call).await {
            Ok(Some(stored)) => Ok(Json(stored)),
            Ok(None) => Err(ServerError::internal(
                "the native runtime reported a recovered result it cannot produce",
            )),
            Err(error) => Err(map_runtime_error(error)),
        },
        Err(error) => Err(map_runtime_error(error)),
    }
}

/// Fetch the stored result for an exact prior call, without re-executing.
///
/// Bridges use this after an interrupted response, and after an
/// unknown-outcome refusal, to inspect what the host recorded before
/// proposing another action. `404` means no result is stored for that call.
pub async fn native_result(
    axum::extract::State(state): axum::extract::State<AppState>,
    headers: HeaderMap,
    Json(call): Json<ComputerUseCall>,
) -> Result<Json<ComputerUseResult>, ServerError> {
    let subject = authorize(&state, &headers).await?;
    require_well_formed(&call)?;
    let runtime = attached_runtime(&state)?;
    let scope = NativeRuntimeScope::from(subject);
    match runtime.result_for_call(&scope, &call).await {
        Ok(Some(stored)) => Ok(Json(stored)),
        Ok(None) => Err(ServerError::not_found(
            "no stored result for that native request",
        )),
        Err(error) => Err(map_runtime_error(error)),
    }
}

// ── shared refusal ladder ───────────────────────────────────────────────────

async fn authorize(state: &AppState, headers: &HeaderMap) -> Result<NativeSubject, ServerError> {
    let token = bearer_token(headers)
        .ok_or_else(|| ServerError::unauthorized("missing native capability token"))?;
    let code = state
        .code
        .clone()
        .ok_or_else(|| ServerError::internal("code mode is not configured on this server"))?;
    let subject = code
        .native_tokens
        .subject_for_token(token)
        .ok_or_else(|| ServerError::unauthorized("unknown or revoked native capability token"))?;

    let session = db::code::get_session(&code.db, &subject.owner, subject.session)
        .await?
        .ok_or_else(|| ServerError::forbidden("the native session has ended"))?;
    if matches!(
        session.lifecycle,
        SessionLifecycle::Ended | SessionLifecycle::Fenced
    ) {
        return Err(ServerError::forbidden("the native session has ended"));
    }
    if session.workspace_id != Some(subject.workspace) {
        return Err(ServerError::not_found("native target not found"));
    }

    let workspace = db::code::get_workspace(&code.db, &subject.owner, subject.workspace)
        .await?
        .ok_or_else(|| ServerError::not_found("native target not found"))?;
    if workspace.status != CodeWorkspaceStatus::Active {
        return Err(ServerError::forbidden(
            "the native session's workspace is not active",
        ));
    }

    Ok(subject)
}

fn attached_runtime(state: &AppState) -> Result<Arc<dyn NativeRuntime>, ServerError> {
    let code = state
        .code
        .as_ref()
        .ok_or_else(|| ServerError::internal("code mode is not configured on this server"))?;
    let runtime = code.native_runtime().ok_or_else(|| {
        ServerError::not_implemented("this server has no native computer-use runtime")
    })?;
    if !runtime.is_available() {
        return Err(ServerError::not_implemented(
            "native computer use is not available on this host",
        ));
    }
    Ok(runtime)
}

/// Refuse a call whose tool name or arguments the canonical registry does
/// not validate. Unknown names fail here too — the registry is the single
/// list of native primitives.
fn require_well_formed(call: &ComputerUseCall) -> Result<(), ServerError> {
    if validate_computer_use_arguments(&call.name, &call.arguments) {
        Ok(())
    } else {
        Err(ServerError::unprocessable_kind(
            "invalid_native_arguments",
            "native computer-use arguments are not well-formed",
        ))
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

fn map_runtime_error(error: NativeRuntimeError) -> ServerError {
    match error {
        NativeRuntimeError::SessionEnded => ServerError::forbidden("the native session has ended"),
        NativeRuntimeError::NotAuthorized(message) => ServerError::forbidden(message),
        NativeRuntimeError::Unsupported(operation) => {
            ServerError::not_implemented(format!("this host does not support {operation}"))
        }
        NativeRuntimeError::RequestConflict => ServerError::conflict_kind(
            "native_request_conflict",
            "that request id is already bound to a different call; mint a new request id",
        ),
        NativeRuntimeError::Recovered => ServerError::conflict_kind(
            "native_request_recovered",
            "that request already completed; fetch its stored result",
        ),
        NativeRuntimeError::UnknownOutcome => ServerError::conflict_kind(
            "native_unknown_outcome",
            "a previous action's effect is unknown; inspect the target before acting again",
        ),
        NativeRuntimeError::Failed(message) => {
            ServerError::internal(format!("native operation failed: {message}"))
        }
    }
}
