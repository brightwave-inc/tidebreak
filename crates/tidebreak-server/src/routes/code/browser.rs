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
    db, BrowserActArgs, BrowserActResult, BrowserListArgs, BrowserListResult, BrowserNavigateArgs,
    BrowserNavigateResult, BrowserPageSnapshot, BrowserScreenshotArgs, BrowserScreenshotResult,
    BrowserSnapshotArgs, BrowserWaitArgs, BrowserWaitResult, CodeSessionLifecycle,
    CodeWorkspaceStatus,
};

use crate::code::browser_channel::BrowserSubject;
use crate::code::browser_runtime::{BrowserRuntime, BrowserRuntimeError};
use crate::error::ServerError;
use crate::extract::Json;
use crate::state::AppState;

// ── shared refusal ladder ───────────────────────────────────────────────────

/// Resolve the capability bearer to a live, in-scope [`BrowserSubject`].
///
/// The ladder fails closed in order: no/unknown token → `401`; a subject
/// whose session is gone, ended, or fenced, or whose workspace is inactive →
/// `403`; a subject whose session and workspace disagree, or whose workspace
/// is gone → `404`, indistinguishable from a target that never existed.
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
    // A token minted for one workspace never reaches a session that moved
    // out of it; the mismatch reads exactly like a missing target.
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
    state
        .browser_runtime
        .clone()
        .ok_or_else(|| ServerError::not_implemented("this server has no in-app browser runtime"))
}

/// `422` for a parsed body that fails the shared browser contract's shape
/// checks (bad opaque id, non-HTTP(S) or credentialed URL, out-of-range
/// bounds).
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

fn map_runtime_error(error: BrowserRuntimeError) -> ServerError {
    match error {
        BrowserRuntimeError::UnknownBrowserId(id) => {
            ServerError::not_found(format!("browser {id} not found"))
        }
        BrowserRuntimeError::SessionEnded => {
            ServerError::forbidden("the browser session has ended")
        }
        BrowserRuntimeError::Unsupported(operation) => ServerError::not_implemented(format!(
            "this browser engine does not support {operation}"
        )),
        BrowserRuntimeError::StaleTarget => ServerError::conflict_kind(
            "stale_browser_target",
            "the page changed since that snapshot; take a new browser_snapshot",
        ),
        BrowserRuntimeError::Failed(message) => {
            ServerError::internal(format!("browser operation failed: {message}"))
        }
    }
}
// probe re-run marker
