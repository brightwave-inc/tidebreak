//! `/code/*` routes: repos, workspaces, sessions, doctor, event stream.
//!
//! Every route here is owner-scoped through `ScopedCode`. The routes that
//! change what the machine *is* — installing pinned harness binaries, and the
//! clone-parent-directory setting every principal shares — are registered on
//! the deployment-plane router in `crate::lib` instead, behind `require_admin`.

mod approvals;
mod git;
mod harnesses;
mod repos;
mod session_events;
mod sessions;
mod terminals;
pub(crate) mod types;
mod updates;
mod usage;
mod workspaces;

pub(crate) use crate::code::approval_bridge::approval_prompt;
pub(crate) use approvals::{decide_approval, list_approvals};
pub(crate) use git::{
    commit_workspace, create_pull_request, get_workspace_pr, get_workspace_pr_comments,
    merge_workspace_pr, push_workspace, refresh_workspace_pr, run_workspace_action,
    start_workspace_watch, stop_workspace_watch,
};
pub(crate) use harnesses::{list_harness_models, list_harnesses, refresh_harnesses};
pub(crate) use repos::{
    clone_defaults, create_repo, delete_repo, get_clone_job, get_repo, list_repos, patch_repo,
    start_clone,
};
pub(crate) use session_events::session_events;
pub(crate) use sessions::{
    create_session, get_session_debug, get_session_image, interrupt_session, list_session_turns,
    list_workspace_sessions, publish_session_image, reap_session, set_attention, steer_session,
    submit_turn,
};
pub(crate) use terminals::{
    close_terminal, close_workspace_terminals, create_terminal, list_terminals, read_terminal,
    resize_terminal, write_terminal,
};
#[allow(unused_imports)]
pub(crate) use types::{
    CodeActionSnapshot, CodeApprovalDecisionBody, CodeApprovalSnapshot, CodeCloneDefaults,
    CodeCloneJobSnapshot, CodeCommitSnapshot, CodeFileChange, CodePrCommentsSnapshot,
    CodePushSnapshot, CodeRepoSnapshot, CodeSessionDebug, CodeSessionDigest, CodeSessionSnapshot,
    CodeTerminalActivityNotice, CodeTerminalRead, CodeTerminalSnapshot, CodeTurnSnapshot,
    CodeUpdateNotice, CodeWorkspaceBlob, CodeWorkspaceDiff, CodeWorkspaceFiles,
    CodeWorkspacePrSnapshot, CodeWorkspaceSearch, CodeWorkspaceSearchMatch, CodeWorkspaceSnapshot,
    CodeWorkspaceTree, HarnessDoctorReport, HarnessModelList, MergeCodePrBody, QueuedCodeTurn,
    SequencedCodeEventFrame,
};
pub(crate) use updates::code_updates;
pub(crate) use usage::subscription_usage;
pub(crate) use workspaces::{
    archive_workspace, create_workspace, get_workspace, get_workspace_blob, get_workspace_diff,
    list_workspace_files, list_workspace_tree, list_workspaces, patch_workspace, search_workspace,
};

// Nothing here reaches `AppState.code` directly. Every handler in this module
// extracts a `crate::code::ScopedCode`, which binds the process runtime to the
// requesting principal and refuses when code mode is not configured — so a new
// `/code/*` route is owner-scoped by construction rather than by remembering
// to filter (decision 6's "enforcement is a router property, not a handler
// habit", applied to data scoping).
