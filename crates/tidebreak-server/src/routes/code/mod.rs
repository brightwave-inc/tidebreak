//! `/code/*` routes: repos, workspaces, sessions, doctor, event stream.

mod harnesses;
mod repos;
mod session_events;
mod sessions;
mod types;
mod workspaces;

pub(crate) use harnesses::{list_harnesses, refresh_harnesses};
pub(crate) use repos::{create_repo, delete_repo, get_repo, list_repos, patch_repo};
pub(crate) use session_events::session_events;
pub(crate) use sessions::{create_session, interrupt_session, reap_session, submit_turn};
#[allow(unused_imports)]
pub(crate) use types::{
    CodeRepoSnapshot, CodeSessionSnapshot, CodeTurnSnapshot, CodeWorkspaceSnapshot,
    HarnessDoctorReport, SequencedCodeEventFrame,
};
pub(crate) use workspaces::{
    archive_workspace, create_workspace, get_workspace, list_workspaces, patch_workspace,
};

use crate::code::CodeRuntime;
use crate::error::ServerError;
use crate::state::AppState;

pub(crate) fn require_code(state: &AppState) -> Result<&CodeRuntime, ServerError> {
    state
        .code
        .as_deref()
        .ok_or_else(|| ServerError::internal("code mode is not configured on this server"))
}
