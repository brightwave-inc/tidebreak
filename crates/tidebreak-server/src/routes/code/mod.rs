//! `/code/*` routes: repos, workspaces, sessions, doctor, event stream.
//!
//! App-token routes here are owner-scoped through `ScopedCode`. Three route
//! families use narrower bearer capabilities instead. The `/code/browser/*`
//! routes in [`browser`] and `/code/llm/*` routes in [`llm`] derive the owner
//! from a session-private capability. The `/external/code/*` routes in
//! [`external`] derive the owner from an adapter grant and require that grant
//! to bind every requested session.
//!
//! Routes that change what the machine *is* — installing pinned harness
//! binaries, and the clone-parent and worktree-root directories every
//! principal shares — are registered on the deployment-plane router in
//! `crate::lib` instead, behind `require_admin`.

pub(crate) use super::settings::double_option;

mod analytics;
mod approvals;
mod browser;
mod delivery;
mod external;
mod git;
mod grants;
mod harnesses;
mod llm;
mod repos;
mod session_events;
mod sessions;
mod terminals;
mod triggers;
pub(crate) mod types;
mod updates;
mod usage;
mod workspaces;

pub(crate) use crate::code::approval_bridge::approval_prompt;
pub(crate) use analytics::analytics;
pub(crate) use approvals::{decide_approval, list_approvals};
pub(crate) use browser::{
    browser_act, browser_list, browser_navigate, browser_screenshot, browser_snapshot, browser_wait,
};
pub(crate) use delivery::{
    act_on_pull_request as act_on_delivery_pull_request, act_on_run as act_on_delivery_run,
    discover_repositories as discover_delivery_repositories,
    pull_request_detail as delivery_pull_request_detail,
    query_pull_requests as query_delivery_pull_requests, query_runs as query_delivery_runs,
    resolve_repositories as resolve_delivery_repositories, run_detail as delivery_run_detail,
};
pub(crate) use external::{
    external_events, external_get_or_create, external_interrupt, external_messages, external_reap,
    external_rotate,
};
pub(crate) use git::{
    commit_workspace, create_pull_request, get_workspace_pr, get_workspace_pr_comments,
    list_workspace_pull_requests, mark_workspace_pr_ready, merge_workspace_pr, push_workspace,
    refresh_workspace_pr, run_workspace_action, start_workspace_watch, stop_workspace_watch,
    write_workspace_check_logs,
};
pub(crate) use grants::{
    connect_approve, connect_complete, connect_start, connect_status, connect_view, list_grants,
    revoke_grant, revoke_workspace_grants,
};
pub(crate) use harnesses::{
    install_harness, list_harness_models, list_harnesses, refresh_harnesses,
};
pub(crate) use llm::{
    harness_llm_anthropic_messages, harness_llm_openai_models, harness_llm_openai_responses,
    MAX_HARNESS_LLM_BODY_BYTES,
};
pub(crate) use repos::{
    clone_defaults, create_repo, delete_repo, get_clone_job, get_repo, list_github_repositories,
    list_repos, patch_repo, repo_sources, start_clone,
};
pub(crate) use session_events::session_events;
pub(crate) use sessions::{
    create_session, delete_queued_turn, fork_session, get_session_debug, get_session_image,
    interrupt_session, list_queued_turns, list_session_turns, list_workspace_sessions,
    patch_queued_turn, post_queue_send_now, publish_session_image, put_queue_paused, reap_session,
    set_attention, set_session_fast_mode, set_session_permission_mode,
    set_session_reasoning_effort, steer_session, submit_turn,
};
pub(crate) use terminals::{
    close_terminal, close_workspace_terminals, create_terminal, list_terminals, read_terminal,
    resize_terminal, write_terminal,
};
pub(crate) use triggers::{
    create_repo_trigger, delete_repo_trigger, list_repo_triggers, update_repo_trigger,
};
#[allow(unused_imports)]
pub(crate) use types::{
    CodeActionSnapshot, CodeAnalyticsDay, CodeAnalyticsHarness, CodeAnalyticsModel,
    CodeAnalyticsPricingCoverage, CodeAnalyticsRange, CodeAnalyticsRepository,
    CodeAnalyticsSnapshot, CodeAnalyticsTotals, CodeApprovalDecisionBody, CodeApprovalSnapshot,
    CodeCheckLog, CodeCheckLogError, CodeCheckLogsSnapshot, CodeCloneDefaults,
    CodeCloneJobSnapshot, CodeCommitSnapshot, CodeConnectPage, CodeDeliveryActionResult,
    CodeDeliveryPullRequestActionBody, CodeDeliveryPullRequestDetail, CodeDeliveryPullRequestFile,
    CodeDeliveryPullRequestQuery, CodeDeliveryPullRequestTarget, CodeDeliveryPullRequestsPage,
    CodeDeliveryRepositoriesSnapshot, CodeDeliveryRunActionBody, CodeDeliveryRunDetail,
    CodeDeliveryRunQuery, CodeDeliveryRunTarget, CodeDeliveryRunsPage, CodeFileChange,
    CodeForkBody, CodeForkTranscript, CodeGithubRepositories, CodeGithubRepository,
    CodeGrantSnapshot, CodeHarnessInstallSnapshot, CodePrCommentsSnapshot, CodePushSnapshot,
    CodeRepoSnapshot, CodeRepoSource, CodeRepoSources, CodeRepoStorageSnapshot, CodeSessionDebug,
    CodeSessionDigest, CodeSessionSnapshot, CodeStorageAction, CodeStorageSnapshot,
    CodeTerminalActivityNotice, CodeTerminalRead, CodeTerminalSnapshot, CodeTriggerSnapshot,
    CodeTurnSnapshot, CodeUpdateNotice, CodeWorkspaceBlob, CodeWorkspaceDiff, CodeWorkspaceFiles,
    CodeWorkspaceHistorySearchMatch, CodeWorkspaceHistorySearchSource, CodeWorkspacePrSnapshot,
    CodeWorkspacePullRequests, CodeWorkspaceSearch, CodeWorkspaceSearchMatch,
    CodeWorkspaceSnapshot, CodeWorkspaceStorageSnapshot, CodeWorkspaceTree, CodeWorktreeRoot,
    CreateCodeTriggerBody, HarnessDoctorReport, HarnessModelList, MergeCodePrBody, QueuedCodeTurn,
    ResolveCodeDeliveryRepositoriesBody, SequencedCodeEventFrame, SetCodeWorktreeRootBody,
    UpdateCodeTriggerBody,
};
pub(crate) use updates::code_updates;
pub(crate) use usage::subscription_usage;
pub(crate) use workspaces::{
    archive_workspace, create_workspace, get_workspace, get_workspace_blob, get_workspace_diff,
    get_worktree_root, list_storage, list_workspace_files, list_workspace_tree, list_workspaces,
    patch_workspace, release_workspace, restore_workspace, retry_workspace_setup, search_workspace,
    set_worktree_root,
};

// App-token handlers do not reach `AppState.code` directly. They extract a
// `crate::code::ScopedCode`, which binds the process runtime to the requesting
// principal and refuses when code mode is not configured. Capability handlers
// validate their narrower bearer and derive the owner before reading data. A
// new route is therefore scoped by construction rather than by remembering to
// filter (decision 6's "enforcement is a router property, not a handler habit",
// applied to data scoping).
