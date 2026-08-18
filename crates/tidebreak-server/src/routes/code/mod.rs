//! `/code/*` routes: repos, workspaces, sessions, doctor, event stream.

mod approvals;
mod git;
mod harnesses;
mod repos;
mod session_events;
mod sessions;
mod terminals;
mod types;
mod updates;
mod workspaces;

pub(crate) use crate::code::approval_bridge::approval_prompt;
pub(crate) use approvals::{decide_approval, list_approvals};
pub(crate) use git::{
    commit_workspace, create_pull_request, get_workspace_pr, get_workspace_pr_comments,
    merge_workspace_pr, push_workspace, refresh_workspace_pr, run_workspace_action,
};
pub(crate) use harnesses::{list_harness_models, list_harnesses, refresh_harnesses};
pub(crate) use repos::{
    clone_defaults, create_repo, delete_repo, get_clone_job, get_repo, list_repos, patch_repo,
    start_clone,
};
pub(crate) use session_events::session_events;
pub(crate) use sessions::{
    create_session, interrupt_session, list_session_turns, list_workspace_sessions, reap_session,
    set_attention, steer_session, submit_turn,
};
pub(crate) use terminals::{
    close_terminal, close_workspace_terminals, create_terminal, list_terminals, read_terminal,
    resize_terminal, write_terminal,
};
#[allow(unused_imports)]
pub(crate) use types::{
    CodeActionSnapshot, CodeApprovalDecisionBody, CodeApprovalSnapshot, CodeCloneDefaults,
    CodeCloneJobSnapshot, CodeCommitSnapshot, CodeFileChange, CodePrCommentsSnapshot,
    CodePushSnapshot, CodeRepoSnapshot, CodeSessionDigest, CodeSessionSnapshot,
    CodeTerminalActivityNotice, CodeTerminalRead, CodeTerminalSnapshot, CodeTurnSnapshot,
    CodeUpdateNotice, CodeWorkspaceDiff, CodeWorkspaceFiles, CodeWorkspacePrSnapshot,
    CodeWorkspaceSnapshot, CodeWorkspaceTree, HarnessDoctorReport, HarnessModelList,
    MergeCodePrBody, QueuedCodeTurn, SequencedCodeEventFrame,
};
pub(crate) use updates::code_updates;
pub(crate) use workspaces::{
    archive_workspace, create_workspace, get_workspace, get_workspace_diff, list_workspace_files,
    list_workspace_tree, list_workspaces, patch_workspace,
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
