//! `/code/*` routes: repos, workspaces, sessions, doctor, event stream.

mod harnesses;
mod repos;
mod session_events;
mod sessions;
mod terminals;
mod types;
mod workspaces;

pub(crate) use harnesses::{list_harnesses, refresh_harnesses};
pub(crate) use repos::{create_repo, delete_repo, get_repo, list_repos, patch_repo};
pub(crate) use session_events::session_events;
pub(crate) use sessions::{
    create_session, interrupt_session, list_session_turns, list_workspace_sessions, reap_session,
    steer_session, submit_turn,
};
pub(crate) use terminals::{
    close_terminal, close_workspace_terminals, create_terminal, list_terminals, read_terminal,
    resize_terminal, write_terminal,
};
#[allow(unused_imports)]
pub(crate) use types::{
    CodeFileChange, CodeRepoSnapshot, CodeSessionSnapshot, CodeTerminalActivityNotice,
    CodeTerminalRead, CodeTerminalSnapshot, CodeTurnSnapshot, CodeWorkspaceDiff,
    CodeWorkspaceFiles, CodeWorkspaceSnapshot, HarnessDoctorReport, QueuedCodeTurn,
    SequencedCodeEventFrame,
};
pub(crate) use workspaces::{
    archive_workspace, create_workspace, get_workspace, get_workspace_diff, list_workspace_files,
    list_workspaces, patch_workspace,
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
