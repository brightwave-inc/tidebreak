//! `/code/browser/*` routes: the engine-facing browser channel.
//!
//! These routes are called by the engine child, not the renderer, and they
//! authenticate with the per-session capability bearer minted by
//! [`crate::code::browser_channel::BrowserTokenRegistry`] — never the
//! per-launch app token. They are therefore registered outside
//! `require_token` (see `crate::app`), and each handler resolves its own
//! `Authorization` header.
//!
//! ## Security properties
//!
//! * The token travels only in the `Authorization` header — never in a URL
//!   path, query string, or response body — and no error message echoes it.
//! * Owner, workspace, and session identity come exclusively from the token
//!   registry's subject. The body types deny unknown fields, so a payload
//!   naming `workspace_id`, `session_id`, or `owner_id` is a `400`, not an
//!   escalation.
//! * The subject's session must exist, belong to the subject's workspace,
//!   and not be ended or fenced; the workspace must exist and be active.
//! * Without an attached [`BrowserRuntime`] every route answers `501`.

use std::sync::Arc;

use axum::http::header::AUTHORIZATION;
use axum::http::HeaderMap;

use tidebreak_core::{
    db, BrowserListResult, BrowserNavigateArgs, BrowserNavigateResult,
    BrowserPageSnapshot, BrowserSnapshotArgs, CodeSessionLifecycle, CodeWorkspaceStatus,
};

use crate::code::browser_channel::BrowserSubject;
use crate::code::browser_runtime::{BrowserRuntime, BrowserRuntimeScope};
use crate::error::ServerError;
use crate::extract::Json;
use crate::state::AppState;

pub async fn browser_list(
    axum::extract::State(state): axum::extract::State<AppState>,
    headers: HeaderMap,
) -> Result<Json<BrowserListResult>, ServerError> {
    let subject = authorize(&state, &headers).await?;
    let runtime = attached_runtime(&state)?;
    runtime
        .as_ref()
        .list(&BrowserRuntimeScope::from(subject))
        .await
        .map(Json)
}

pub async fn browser_navigate(
    axum::extract::State(state): axum::extract::State<AppState>,
    headers: HeaderMap,
    Json(args): Json<BrowserNavigateArgs>,
) -> Result<Json<BrowserNavigateResult>, ServerError> {
    let subject = authorize(&state, &headers).await?;
    require_well_formed(args.is_well_formed())?;
    let runtime = attached_runtime(&state)?;
    runtime
        .as_ref()
        .navigate(&BrowserRuntimeScope::from(subject), &args)
        .await
        .map(Json)
}

pub async fn browser_snapshot(
    axum::extract::State(state): axum::extract::State<AppState>,
    headers: HeaderMap,
    Json(args): Json<BrowserSnapshotArgs>,
) -> Result<Json<BrowserPageSnapshot>, ServerError> {
    let subject = authorize(&state, &headers).await?;
    require_well_formed(args.is_well_formed())?;
    let runtime = attached_runtime(&state)?;
    runtime
        .as_ref()
        .snapshot(&BrowserRuntimeScope::from(subject), &args)
        .await
        .map(Json)
}

// ── shared refusal ladder ───────────────────────────────────────────────────

/// Resolve the capability bearer to a live, in-scope [`BrowserSubject`].
async fn authorize(state: &AppState, headers: &HeaderMap) -> Result<BrowserSubject, ServerError> {
    let token = bearer_token(headers)
        .ok_or_else(|| ServerError::unauthorized("missing browser capability token"))?;
    let code = state
        .code
        .clone()
        .ok_or_else(|| ServerError::internal("code mode is not configured on this server"))?;
    let subject = code
        .browser_tokens
        .subject_for_token(token)
        .ok_or_else(|| ServerError::unauthorized("unknown or revoked browser capability token"))?;

    let session = db::code::get_session(&code.db, &subject.owner, subject.session)
        .await?
        .ok_or_else(|| ServerError::forbidden("the browser session has ended"))?;
    if matches!(
        session.lifecycle,
        CodeSessionLifecycle::Ended | CodeSessionLifecycle::Fenced
    ) {
        return Err(ServerError::forbidden("the browser session has ended"));
    }
    if session.workspace_id != subject.workspace {
        return Err(ServerError::not_found("browser target not found"));
    }

    let workspace = db::code::get_workspace(&code.db, &subject.owner, subject.workspace)
        .await?
        .ok_or_else(|| ServerError::not_found("browser target not found"))?;
    if workspace.status != CodeWorkspaceStatus::Active {
        return Err(ServerError::forbidden(
            "the browser session's workspace is not active",
        ));
    }

    Ok(subject)
}

/// The embedding's browser runtime, or `501` where none is attached.
fn attached_runtime(state: &AppState) -> Result<Arc<dyn BrowserRuntime>, ServerError> {
    let code = state
        .code
        .as_ref()
        .ok_or_else(|| ServerError::internal("code mode is not configured on this server"))?;
    code.browser_runtime()
        .ok_or_else(|| ServerError::not_implemented("this server has no in-app browser runtime"))
}

fn require_well_formed(well_formed: bool) -> Result<(), ServerError> {
    if well_formed {
        Ok(())
    } else {
        Err(ServerError::unprocessable_kind(
            "invalid_browser_arguments",
            "browser arguments are not well-formed",
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
