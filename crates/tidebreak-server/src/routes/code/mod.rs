//! `/code/*` routes: repos, workspaces, sessions, doctor, event stream.
//!
//! Every route here is owner-scoped through `ScopedCode`, with two
//! exceptions. The routes that change what the machine *is* — installing
//! pinned harness binaries, and the clone-parent and worktree-root
//! directories every principal shares — are registered on the
//! deployment-plane router in `crate::lib` instead, behind `require_admin`.
//! And the `/code/browser/*` routes in [`browser`] authenticate with the
//! per-session capability bearer rather than the launch token, so they are
//! registered outside `require_token` and derive their owner from the
//! browser token registry.

mod approvals;
mod browser;
mod delivery;
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
pub(crate) use browser::{
    browser_list, browser_navigate, browser_screenshot, browser_snapshot, browser_wait,
};
pub(crate) use delivery::{
    act_on_pull_request as act_on_delivery_pull_request, act_on_run as act_on_delivery_run,
    discover_repositories as discover_delivery_repositories,
    pull_request_detail as delivery_pull_request_detail,
    query_pull_requests as query_delivery_pull_requests, query_runs as query_delivery_runs,
    resolve_repositories as resolve_delivery_repositories, run_detail as delivery_run_detail,
};
pub(crate) use git::{
    commit_workspace, create_pull_request, get_workspace_pr, get_workspace_pr_comments,
    mark_workspace_pr_ready, merge_workspace_pr, push_workspace, refresh_workspace_pr,
    run_workspace_action, start_workspace_watch, stop_workspace_watch,
};
pub(crate) use harnesses::{
    install_harness, list_harness_models, list_harnesses, refresh_harnesses,
};
pub(crate) use repos::{
    clone_defaults, create_repo, delete_repo, get_clone_job, get_repo, list_repos, patch_repo,
    start_clone,
};
pub(crate) use session_events::session_events;
pub(crate) use sessions::{
    create_session, fork_session, get_session_debug, get_session_image, interrupt_session,
    list_session_turns, list_workspace_sessions, publish_session_image, reap_session,
    set_attention, set_session_permission_mode, steer_session, submit_turn,
};
pub(crate) use terminals::{
    close_terminal, close_workspace_terminals, create_terminal, list_terminals, read_terminal,
    resize_terminal, write_terminal,
};
#[allow(unused_imports)]
pub(crate) use types::{
    CodeActionSnapshot, CodeApprovalDecisionBody, CodeApprovalSnapshot, CodeCloneDefaults,
    CodeCloneJobSnapshot, CodeCommitSnapshot, CodeDeliveryActionResult,
    CodeDeliveryPullRequestActionBody, CodeDeliveryPullRequestDetail, CodeDeliveryPullRequestFile,
    CodeDeliveryPullRequestQuery, CodeDeliveryPullRequestTarget, CodeDeliveryPullRequestsPage,
    CodeDeliveryRepositoriesSnapshot, CodeDeliveryRunActionBody, CodeDeliveryRunDetail,
    CodeDeliveryRunQuery, CodeDeliveryRunTarget, CodeDeliveryRunsPage, CodeFileChange,
    CodeForkTranscript, CodeHarnessInstallSnapshot, CodePrCommentsSnapshot, CodePushSnapshot,
    CodeRepoSnapshot, CodeSessionDebug, CodeSessionDigest, CodeSessionSnapshot,
    CodeTerminalActivityNotice, CodeTerminalRead, CodeTerminalSnapshot, CodeTurnSnapshot,
    CodeUpdateNotice, CodeWorkspaceBlob, CodeWorkspaceDiff, CodeWorkspaceFiles,
    CodeWorkspacePrSnapshot, CodeWorkspaceSearch, CodeWorkspaceSearchMatch, CodeWorkspaceSnapshot,
    CodeWorkspaceTree, CodeWorktreeRoot, HarnessDoctorReport, HarnessModelList, MergeCodePrBody,
    QueuedCodeTurn, ResolveCodeDeliveryRepositoriesBody, SequencedCodeEventFrame,
    SetCodeWorktreeRootBody,
};
pub(crate) use updates::code_updates;
pub(crate) use usage::subscription_usage;
pub(crate) use workspaces::{
    archive_workspace, create_workspace, get_workspace, get_workspace_blob, get_workspace_diff,
    get_worktree_root, list_workspace_files, list_workspace_tree, list_workspaces, patch_workspace,
    release_workspace, restore_workspace, search_workspace, set_worktree_root,
};

// Nothing here reaches `AppState.code` directly. Every handler in this module
// extracts a `crate::code::ScopedCode`, which binds the process runtime to the
// requesting principal and refuses when code mode is not configured — so a new
// `/code/*` route is owner-scoped by construction rather than by remembering
// to filter (decision 6's "enforcement is a router property, not a handler
// habit", applied to data scoping).
