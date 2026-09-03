//! The request-facing code-mode runtime, bound to one authenticated principal.
//!
//! This is [`crate::scoped_store::ScopedStore`]'s counterpart for `/code/*`.
//! Route handlers do not touch [`CodeRuntime`] directly: they extract a
//! [`ScopedCode`], and every query it makes carries the requesting
//! principal's [`OwnerId`]. The unscoped runtime handle never escapes this
//! type, so route code cannot express a query that crosses owners — another
//! owner's repository, workspace, session, turn, event, or approval is
//! indistinguishable from one that does not exist (decisions 47 and 48).
//!
//! System paths — boot recovery, the stall sweep, session workers, the
//! capability-token approval bridge — are not requests. They keep the
//! unscoped handle on [`AppState`] and use the `_all_owners` store functions,
//! which say so in their names.
//!
//! The extractor fails closed like [`AuthContext`] itself: on a route the auth
//! middleware does not cover it answers `401`, never a defaulted owner.

use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;

use tidebreak_core::code::{QueuedTurn, SequencedEvent};
use tidebreak_core::{
    Approval, ApprovalId, ApprovalState, CodeRepo, CodeTrigger, CodeTriggerAction,
    CodeTriggerCondition, CodeTriggerId, CodeWorkspace, Diffstat, HarnessKind, OwnerId,
    PermissionMode, ReasoningEffort, RepoId, Session, SessionId, Turn, TurnId, WorkspaceId,
};

use super::checkpoint::ChangedFile;
use super::clone::CloneRequest;
use super::delivery;
use super::gh::{self, ActionOutcome, CommitOutcome, PushOutcome, WorkspaceGitStatus};
use super::runtime::{CodeRuntime, NewSessionSettings, RepoRegistration, SubmitTurnOutcome};
use super::worktree;
use crate::error::ServerError;
use crate::principal::AuthContext;
use crate::routes::code::types::{
    CodeCloneDefaults, CodeCloneJobSnapshot, CodeDeliveryActionResult,
    CodeDeliveryPullRequestActionBody, CodeDeliveryPullRequestDetail, CodeDeliveryPullRequestQuery,
    CodeDeliveryPullRequestTarget, CodeDeliveryPullRequestsPage, CodeDeliveryRepositoriesSnapshot,
    CodeDeliveryRunActionBody, CodeDeliveryRunDetail, CodeDeliveryRunQuery, CodeDeliveryRunTarget,
    CodeDeliveryRunsPage, CodeHarnessInstallSnapshot, CodeRepoSources,
    ResolveCodeDeliveryRepositoriesBody,
};
use crate::state::AppState;

/// Code mode as one authenticated principal may see it.
///
/// Constructed only from server state plus a verified [`AuthContext`], so the
/// owner inside is always the one the auth middleware resolved. Fields stay
/// private and no method returns the inner runtime handle.
#[derive(Clone)]
pub(crate) struct ScopedCode {
    runtime: Arc<CodeRuntime>,
    owner: OwnerId,
    allow_unscoped_delivery: bool,
}

impl ScopedCode {
    /// Bind the process's code runtime to the request's principal.
    fn new(state: &AppState, auth: &AuthContext) -> Result<Self, ServerError> {
        let runtime = state
            .code
            .clone()
            .ok_or_else(|| ServerError::internal("code mode is not configured on this server"))?;
        Ok(Self {
            runtime,
            owner: auth.principal.owner_id(),
            allow_unscoped_delivery: auth.principal.is_admin(),
        })
    }

    /// Bind an already-resolved runtime to a principal.
    ///
    /// For callers that have decided for themselves what to do when code mode
    /// is not configured — the inbox lists chats either way, so it must not
    /// take the extractor's all-or-nothing rejection.
    pub(crate) fn for_owner(
        runtime: std::sync::Arc<super::runtime::CodeRuntime>,
        owner: OwnerId,
    ) -> Self {
        Self {
            runtime,
            owner,
            allow_unscoped_delivery: false,
        }
    }

    /// The principal's durable owner key, for the seams that take it directly:
    /// the live buses, and background naming.
    pub(crate) fn owner(&self) -> &OwnerId {
        &self.owner
    }

    // ------------------------------------------------------------------
    // Repositories. Per owner, like everything else here: two users may
    // register or clone the same remote and neither sees the other's row.
    // ------------------------------------------------------------------

    pub(crate) async fn register_repo(
        &self,
        root_path: std::path::PathBuf,
        metadata: RepoRegistration,
    ) -> Result<CodeRepo, ServerError> {
        self.runtime
            .register_repo(&self.owner, root_path, metadata)
            .await
    }

    pub(crate) async fn list_repos(&self) -> Result<Vec<CodeRepo>, ServerError> {
        self.runtime.list_repos(&self.owner).await
    }

    pub(crate) async fn get_repo(&self, id: RepoId) -> Result<CodeRepo, ServerError> {
        self.runtime.get_repo(&self.owner, id).await
    }

    pub(crate) async fn save_repo(&self, repo: &CodeRepo) -> Result<(), ServerError> {
        self.runtime.save_repo(repo).await
    }

    pub(crate) async fn remove_repo(
        &self,
        id: RepoId,
        reclaim_checkout: bool,
    ) -> Result<(), ServerError> {
        self.runtime
            .remove_repo(&self.owner, id, reclaim_checkout)
            .await
    }

    pub(crate) async fn clone_defaults(&self) -> Result<CodeCloneDefaults, ServerError> {
        self.runtime.clone_defaults().await
    }

    pub(crate) async fn repo_sources(&self) -> Result<CodeRepoSources, ServerError> {
        self.runtime.repo_sources(&self.owner).await
    }

    pub(crate) async fn list_github_repositories(
        &self,
    ) -> Result<crate::routes::code::CodeGithubRepositories, ServerError> {
        self.runtime.list_github_repositories(&self.owner).await
    }

    pub(crate) async fn start_clone(
        &self,
        request: CloneRequest,
    ) -> Result<CodeCloneJobSnapshot, ServerError> {
        self.runtime.start_clone(&self.owner, request).await
    }

    pub(crate) fn get_clone_job(
        &self,
        id: uuid::Uuid,
    ) -> Result<CodeCloneJobSnapshot, ServerError> {
        self.runtime.get_clone_job(&self.owner, id)
    }

    // ------------------------------------------------------------------
    // Install-wide GitHub delivery views. Remote state is live and cached;
    // workspace correlation remains scoped to this owner.
    // ------------------------------------------------------------------

    pub(crate) async fn discover_delivery_repositories(
        &self,
        refresh: bool,
    ) -> Result<CodeDeliveryRepositoriesSnapshot, ServerError> {
        delivery::discover_repositories(&self.runtime, &self.owner, refresh).await
    }

    pub(crate) async fn resolve_delivery_repositories(
        &self,
        body: ResolveCodeDeliveryRepositoriesBody,
    ) -> Result<CodeDeliveryRepositoriesSnapshot, ServerError> {
        delivery::resolve_repositories(
            &self.runtime,
            &self.owner,
            self.allow_unscoped_delivery,
            body,
        )
        .await
    }

    pub(crate) async fn query_delivery_pull_requests(
        &self,
        query: CodeDeliveryPullRequestQuery,
    ) -> Result<CodeDeliveryPullRequestsPage, ServerError> {
        delivery::query_pull_requests(
            &self.runtime,
            &self.owner,
            self.allow_unscoped_delivery,
            query,
        )
        .await
    }

    pub(crate) async fn delivery_pull_request_detail(
        &self,
        target: CodeDeliveryPullRequestTarget,
    ) -> Result<CodeDeliveryPullRequestDetail, ServerError> {
        delivery::pull_request_detail(
            &self.runtime,
            &self.owner,
            self.allow_unscoped_delivery,
            target,
        )
        .await
    }

    pub(crate) async fn act_on_delivery_pull_request(
        &self,
        body: CodeDeliveryPullRequestActionBody,
    ) -> Result<CodeDeliveryActionResult, ServerError> {
        delivery::act_on_pull_request(
            &self.runtime,
            &self.owner,
            self.allow_unscoped_delivery,
            body,
        )
        .await
    }

    pub(crate) async fn query_delivery_runs(
        &self,
        query: CodeDeliveryRunQuery,
    ) -> Result<CodeDeliveryRunsPage, ServerError> {
        delivery::query_runs(
            &self.runtime,
            &self.owner,
            self.allow_unscoped_delivery,
            query,
        )
        .await
    }

    pub(crate) async fn delivery_run_detail(
        &self,
        target: CodeDeliveryRunTarget,
    ) -> Result<CodeDeliveryRunDetail, ServerError> {
        delivery::run_detail(
            &self.runtime,
            &self.owner,
            self.allow_unscoped_delivery,
            target,
        )
        .await
    }

    pub(crate) async fn act_on_delivery_run(
        &self,
        body: CodeDeliveryRunActionBody,
    ) -> Result<CodeDeliveryActionResult, ServerError> {
        delivery::act_on_run(
            &self.runtime,
            &self.owner,
            self.allow_unscoped_delivery,
            body,
        )
        .await
    }

    pub(crate) async fn worktree_root(
        &self,
    ) -> Result<crate::routes::code::CodeWorktreeRoot, ServerError> {
        self.runtime.worktree_root_snapshot().await
    }

    pub(crate) async fn set_worktree_root(
        &self,
        root: Option<&str>,
    ) -> Result<crate::routes::code::CodeWorktreeRoot, ServerError> {
        self.runtime.set_worktree_root(root).await
    }

    // ------------------------------------------------------------------
    // Workspaces.
    // ------------------------------------------------------------------

    pub(crate) async fn create_workspace(
        &self,
        repo_id: RepoId,
        title: Option<String>,
        suggested_title: Option<String>,
        base_ref: Option<String>,
    ) -> Result<CodeWorkspace, ServerError> {
        self.runtime
            .create_workspace(&self.owner, repo_id, title, suggested_title, base_ref)
            .await
    }

    pub(crate) async fn create_remote_workspace(
        &self,
        repo_id: RepoId,
        title: Option<String>,
    ) -> Result<CodeWorkspace, ServerError> {
        self.runtime
            .create_remote_workspace(&self.owner, repo_id, title)
            .await
    }

    pub(crate) async fn list_workspaces(
        &self,
        repo_id: Option<RepoId>,
    ) -> Result<Vec<CodeWorkspace>, ServerError> {
        self.runtime.list_workspaces(&self.owner, repo_id).await
    }

    pub(crate) async fn get_workspace(
        &self,
        id: WorkspaceId,
    ) -> Result<CodeWorkspace, ServerError> {
        self.runtime.get_workspace(&self.owner, id).await
    }

    pub(crate) async fn save_workspace(
        &self,
        workspace: &CodeWorkspace,
    ) -> Result<(), ServerError> {
        self.runtime.save_workspace(workspace).await
    }

    pub(crate) async fn archive_workspace(
        &self,
        id: WorkspaceId,
        force: bool,
        terminals: &crate::code::terminal::TerminalHub,
    ) -> Result<CodeWorkspace, ServerError> {
        self.runtime
            .archive_workspace(&self.owner, id, force, terminals)
            .await
    }

    pub(crate) fn workspace_write_lock(
        &self,
        id: WorkspaceId,
    ) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        self.runtime.workspace_write_lock(id)
    }

    pub(crate) async fn restore_workspace(
        &self,
        id: WorkspaceId,
    ) -> Result<CodeWorkspace, ServerError> {
        self.runtime.restore_workspace(&self.owner, id).await
    }

    pub(crate) async fn retry_workspace_setup(
        &self,
        id: WorkspaceId,
    ) -> Result<CodeWorkspace, ServerError> {
        self.runtime.retry_workspace_setup(&self.owner, id).await
    }

    pub(crate) async fn workspace_tree(
        &self,
        id: WorkspaceId,
        query: &str,
        limit: Option<u32>,
    ) -> Result<(Vec<String>, bool), ServerError> {
        self.runtime
            .workspace_tree(&self.owner, id, query, limit)
            .await
    }

    pub(crate) async fn workspace_search(
        &self,
        id: WorkspaceId,
        query: &str,
        include: &str,
        exclude: &str,
        limit: Option<u32>,
    ) -> Result<(Vec<worktree::WorktreeSearchMatch>, bool), ServerError> {
        self.runtime
            .workspace_search(&self.owner, id, query, include, exclude, limit)
            .await
    }

    pub(crate) async fn workspace_transcript_search(
        &self,
        id: WorkspaceId,
        query: &str,
        limit: Option<u32>,
    ) -> Result<tidebreak_core::db::code::CodeTranscriptSearchPage, ServerError> {
        let workspace = self.runtime.get_workspace(&self.owner, id).await?;
        let requested = limit.unwrap_or(tidebreak_core::db::code::DEFAULT_TRANSCRIPT_SEARCH_LIMIT);
        let limit = if requested == 0 {
            tidebreak_core::db::code::DEFAULT_TRANSCRIPT_SEARCH_LIMIT
        } else {
            requested.min(tidebreak_core::db::code::MAX_TRANSCRIPT_SEARCH_LIMIT)
        };
        Ok(tidebreak_core::db::code::search_repo_transcripts(
            &self.runtime.db,
            &self.owner,
            workspace.repo_id,
            query,
            u64::from(limit),
        )
        .await?)
    }

    pub(crate) async fn workspace_blob(
        &self,
        id: WorkspaceId,
        path: &str,
    ) -> Result<worktree::WorktreeBlob, ServerError> {
        self.runtime.workspace_blob(&self.owner, id, path).await
    }

    pub(crate) async fn workspace_files(
        &self,
        id: WorkspaceId,
        turn_id: Option<TurnId>,
    ) -> Result<(Vec<ChangedFile>, bool, Diffstat, Option<TurnId>), ServerError> {
        self.runtime.workspace_files(&self.owner, id, turn_id).await
    }

    pub(crate) async fn workspace_diff(
        &self,
        id: WorkspaceId,
        turn_id: Option<TurnId>,
        file: Option<&str>,
    ) -> Result<(String, bool, Diffstat, Option<TurnId>), ServerError> {
        self.runtime
            .workspace_diff(&self.owner, id, turn_id, file)
            .await
    }

    // ------------------------------------------------------------------
    // Git surfaces on a workspace.
    // ------------------------------------------------------------------

    pub(crate) async fn commit_workspace(
        &self,
        id: WorkspaceId,
        message: Option<String>,
    ) -> Result<CommitOutcome, ServerError> {
        self.runtime
            .commit_workspace(&self.owner, id, message)
            .await
    }

    pub(crate) async fn push_workspace(&self, id: WorkspaceId) -> Result<PushOutcome, ServerError> {
        self.runtime.push_workspace(&self.owner, id).await
    }

    pub(crate) async fn list_triggers(
        &self,
        repo_id: RepoId,
    ) -> Result<Vec<CodeTrigger>, ServerError> {
        self.runtime.list_triggers(&self.owner, repo_id).await
    }

    pub(crate) async fn create_trigger(
        &self,
        repo_id: RepoId,
        condition: CodeTriggerCondition,
        action: CodeTriggerAction,
    ) -> Result<CodeTrigger, ServerError> {
        self.runtime
            .create_trigger(&self.owner, repo_id, condition, action)
            .await
    }

    pub(crate) async fn set_trigger_enabled(
        &self,
        repo_id: RepoId,
        id: CodeTriggerId,
        enabled: bool,
    ) -> Result<CodeTrigger, ServerError> {
        self.runtime
            .set_trigger_enabled(&self.owner, repo_id, id, enabled)
            .await
    }

    pub(crate) async fn delete_trigger(
        &self,
        repo_id: RepoId,
        id: CodeTriggerId,
    ) -> Result<(), ServerError> {
        self.runtime.delete_trigger(&self.owner, repo_id, id).await
    }

    pub(crate) async fn workspace_pr(
        &self,
        id: WorkspaceId,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        self.runtime.workspace_pr(&self.owner, id).await
    }

    pub(crate) async fn workspace_pull_requests(
        &self,
        id: WorkspaceId,
    ) -> Result<
        Vec<(
            tidebreak_core::CodePullRequestFact,
            tidebreak_core::CodePullRequestRelation,
        )>,
        ServerError,
    > {
        self.runtime.workspace_pull_requests(&self.owner, id).await
    }

    pub(crate) async fn refresh_workspace_pr(
        &self,
        id: WorkspaceId,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        self.runtime.refresh_workspace_pr(&self.owner, id).await
    }

    pub(crate) async fn start_watch(
        &self,
        id: WorkspaceId,
        permission_mode_ceiling: Option<tidebreak_core::PermissionMode>,
    ) -> Result<tidebreak_core::CodeWatch, ServerError> {
        self.runtime
            .start_watch(&self.owner, id, permission_mode_ceiling)
            .await
    }

    pub(crate) async fn stop_watch(
        &self,
        id: WorkspaceId,
    ) -> Result<tidebreak_core::CodeWatch, ServerError> {
        self.runtime.stop_watch(&self.owner, id).await
    }

    pub(crate) async fn latest_watch(
        &self,
        id: WorkspaceId,
    ) -> Result<Option<tidebreak_core::CodeWatch>, ServerError> {
        self.runtime.latest_watch(&self.owner, id).await
    }

    pub(crate) async fn workspace_pr_comments(
        &self,
        id: WorkspaceId,
    ) -> Result<gh::PrComments, ServerError> {
        self.runtime.workspace_pr_comments(&self.owner, id).await
    }

    pub(crate) async fn workspace_check_logs(
        &self,
        id: WorkspaceId,
    ) -> Result<(Option<String>, super::ci_logs::WrittenCheckLogs), ServerError> {
        self.runtime.workspace_check_logs(&self.owner, id).await
    }

    pub(crate) async fn merge_workspace_pr(
        &self,
        id: WorkspaceId,
        target: CodeDeliveryPullRequestTarget,
        expected_head_sha: String,
        method: gh::MergeMethod,
        auto: bool,
    ) -> Result<super::runtime::WorkspaceMergeOutcome, ServerError> {
        self.runtime
            .merge_workspace_pr(&self.owner, id, target, expected_head_sha, method, auto)
            .await
    }

    pub(crate) async fn mark_workspace_pr_ready(
        &self,
        id: WorkspaceId,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        self.runtime.mark_workspace_pr_ready(&self.owner, id).await
    }

    pub(crate) async fn create_workspace_pr(
        &self,
        id: WorkspaceId,
        title: Option<String>,
        body: Option<String>,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        self.runtime
            .create_workspace_pr(&self.owner, id, title, body)
            .await
    }

    pub(crate) async fn run_workspace_action(
        &self,
        id: WorkspaceId,
        name: &str,
    ) -> Result<ActionOutcome, ServerError> {
        self.runtime
            .run_workspace_action(&self.owner, id, name)
            .await
    }

    // ------------------------------------------------------------------
    // Sessions, turns, and the journal.
    // ------------------------------------------------------------------

    pub(crate) async fn create_session(
        &self,
        workspace_id: WorkspaceId,
        harness: HarnessKind,
        settings: NewSessionSettings,
    ) -> Result<Session, ServerError> {
        self.runtime
            .create_session(&self.owner, workspace_id, harness, settings)
            .await
    }

    pub(crate) async fn create_internal_session(
        &self,
        settings: NewSessionSettings,
    ) -> Result<Session, ServerError> {
        self.runtime
            .create_internal_session(&self.owner, settings)
            .await
    }

    pub(crate) async fn create_remote_session(
        &self,
        workspace_id: WorkspaceId,
        harness: HarnessKind,
        settings: NewSessionSettings,
    ) -> Result<Session, ServerError> {
        self.runtime
            .create_remote_session(&self.owner, workspace_id, harness, settings)
            .await
    }

    pub(crate) async fn get_session(&self, id: SessionId) -> Result<Session, ServerError> {
        self.runtime.get_session(&self.owner, id).await
    }

    pub(crate) async fn list_internal_sessions(&self) -> Result<Vec<Session>, ServerError> {
        self.runtime.list_internal_sessions(&self.owner).await
    }

    pub(crate) async fn list_workspace_sessions(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<Session>, ServerError> {
        self.runtime
            .list_workspace_sessions(&self.owner, workspace_id)
            .await
    }

    pub(crate) async fn external_bindings_for_sessions(
        &self,
        session_ids: &[SessionId],
    ) -> Result<Vec<tidebreak_core::CodeExternalBinding>, ServerError> {
        self.runtime
            .external_bindings_for_sessions(&self.owner, session_ids)
            .await
    }

    pub(crate) async fn list_session_turns(&self, id: SessionId) -> Result<Vec<Turn>, ServerError> {
        self.runtime.list_session_turns(&self.owner, id).await
    }

    pub(crate) async fn list_turn_metrics(
        &self,
    ) -> Result<Vec<tidebreak_core::db::code::TurnMetric>, ServerError> {
        self.runtime.list_turn_metrics(&self.owner).await
    }

    pub(crate) async fn list_pull_request_facts(
        &self,
    ) -> Result<Vec<tidebreak_core::CodePullRequestFact>, ServerError> {
        self.runtime.list_pull_request_facts(&self.owner).await
    }

    pub(crate) async fn list_pull_request_attributions(
        &self,
    ) -> Result<Vec<tidebreak_core::CodePullRequestAttribution>, ServerError> {
        self.runtime
            .list_pull_request_attributions(&self.owner)
            .await
    }

    pub(crate) async fn fork_transcript(
        &self,
        id: SessionId,
        at_turn: Option<tidebreak_core::TurnId>,
    ) -> Result<super::fork::WrittenTranscript, ServerError> {
        self.runtime.fork_transcript(&self.owner, id, at_turn).await
    }

    pub(crate) async fn session_debug(
        &self,
        id: SessionId,
    ) -> Result<(Session, Vec<Turn>, Vec<SequencedEvent>), ServerError> {
        self.runtime.session_debug(&self.owner, id).await
    }

    pub(crate) async fn resolve_turn_attachments(
        &self,
        session_id: SessionId,
        requested: &[(uuid::Uuid, String)],
    ) -> Result<Vec<tidebreak_core::ImageRef>, ServerError> {
        self.runtime
            .resolve_turn_attachments(&self.owner, session_id, requested)
            .await
    }

    pub(crate) async fn submit_turn(
        &self,
        id: SessionId,
        message: String,
        model: Option<String>,
        reasoning_effort: Option<Option<ReasoningEffort>>,
        attachments: Vec<tidebreak_core::ImageRef>,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        self.runtime
            .submit_turn(
                &self.owner,
                id,
                message,
                model,
                reasoning_effort,
                attachments,
            )
            .await
    }

    pub(crate) async fn list_queued_turns(
        &self,
        id: SessionId,
    ) -> Result<(Vec<QueuedTurn>, bool), ServerError> {
        self.runtime.list_queued_turns(&self.owner, id).await
    }

    pub(crate) async fn update_queued_turn(
        &self,
        id: SessionId,
        queued_id: TurnId,
        message: Option<&str>,
        position: Option<i32>,
    ) -> Result<Option<QueuedTurn>, ServerError> {
        self.runtime
            .update_queued_turn(&self.owner, id, queued_id, message, position)
            .await
    }

    pub(crate) async fn delete_queued_turn(
        &self,
        id: SessionId,
        queued_id: TurnId,
    ) -> Result<bool, ServerError> {
        self.runtime
            .delete_queued_turn(&self.owner, id, queued_id)
            .await
    }

    pub(crate) async fn set_queue_paused(
        &self,
        id: SessionId,
        paused: bool,
    ) -> Result<(), ServerError> {
        self.runtime.set_queue_paused(&self.owner, id, paused).await
    }

    pub(crate) async fn send_queued_now(&self, id: SessionId) -> Result<(), ServerError> {
        self.runtime.send_queued_now(&self.owner, id).await
    }

    pub(crate) async fn set_reasoning_effort(
        &self,
        id: SessionId,
        effort: Option<ReasoningEffort>,
    ) -> Result<Session, ServerError> {
        self.runtime
            .set_reasoning_effort(&self.owner, id, effort)
            .await
    }

    pub(crate) async fn set_fast_mode(
        &self,
        id: SessionId,
        fast_mode: bool,
    ) -> Result<Session, ServerError> {
        self.runtime.set_fast_mode(&self.owner, id, fast_mode).await
    }

    pub(crate) async fn interrupt(&self, id: SessionId) -> Result<(), ServerError> {
        // Authorize the session first: without this the worker registry would
        // answer for a session id whatever owner holds it.
        let _ = self.get_session(id).await?;
        self.runtime.interrupt(id).await
    }

    pub(crate) async fn steer(
        &self,
        id: SessionId,
        expected_turn_id: TurnId,
        message: String,
    ) -> Result<(), ServerError> {
        self.runtime
            .steer(&self.owner, id, expected_turn_id, message)
            .await
    }

    pub(crate) async fn reap(&self, id: SessionId) -> Result<Session, ServerError> {
        self.runtime.reap(&self.owner, id).await
    }

    // ------------------------------------------------------------------
    // Adapter grants and the connect handshake (docs/slack-sessions.md).
    // ------------------------------------------------------------------

    pub(crate) async fn list_adapter_grants(
        &self,
    ) -> Result<Vec<tidebreak_core::CodeExternalGrant>, ServerError> {
        self.runtime.list_adapter_grants(&self.owner).await
    }

    pub(crate) async fn list_adapter_grant_profiles(
        &self,
    ) -> Result<Vec<tidebreak_core::CodeGrantProfile>, ServerError> {
        self.runtime.list_adapter_grant_profiles(&self.owner).await
    }

    pub(crate) async fn revoke_adapter_grant(
        &self,
        id: tidebreak_core::CodeGrantId,
        reason: &str,
    ) -> Result<Option<tidebreak_core::CodeExternalGrant>, ServerError> {
        self.runtime
            .revoke_adapter_grant(&self.owner, id, reason)
            .await
    }

    pub(crate) async fn revoke_workspace_grants(
        &self,
        channel_kind: &str,
        workspace_identity: &str,
        reason: &str,
    ) -> Result<Vec<tidebreak_core::CodeExternalGrant>, ServerError> {
        self.runtime
            .revoke_workspace_grants(&self.owner, channel_kind, workspace_identity, reason)
            .await
    }

    pub(crate) async fn view_connect_handshake(
        &self,
        nonce: &str,
    ) -> Result<Option<(tidebreak_core::CodeConnectHandshake, String)>, ServerError> {
        self.runtime
            .view_connect_handshake(&self.owner, nonce)
            .await
    }

    pub(crate) async fn approve_connect_handshake(
        &self,
        nonce: &str,
        csrf: &str,
    ) -> Result<Option<tidebreak_core::CodeConnectHandshake>, ServerError> {
        self.runtime
            .approve_connect_handshake(&self.owner, nonce, csrf)
            .await
    }

    pub(crate) async fn set_permission_mode(
        &self,
        id: SessionId,
        mode: PermissionMode,
    ) -> Result<Session, ServerError> {
        self.runtime
            .set_permission_mode(&self.owner, id, mode)
            .await
    }

    pub(crate) async fn set_attention(
        &self,
        id: SessionId,
        clear: bool,
        note: Option<String>,
    ) -> Result<Session, ServerError> {
        self.runtime
            .set_attention(&self.owner, id, clear, note)
            .await
    }

    // ------------------------------------------------------------------
    // Approvals.
    // ------------------------------------------------------------------

    pub(crate) async fn list_approvals(
        &self,
        state: Option<ApprovalState>,
        session_id: Option<SessionId>,
    ) -> Result<Vec<Approval>, ServerError> {
        self.runtime
            .list_approvals(&self.owner, state, session_id)
            .await
    }

    pub(crate) async fn decide_approval(
        &self,
        id: ApprovalId,
        decision: super::runtime::ApprovalDecisionRequest,
    ) -> Result<Approval, ServerError> {
        self.runtime
            .decide_approval(&self.owner, id, decision)
            .await
    }

    // ------------------------------------------------------------------
    // Harness discovery. Probes describe the machine's binaries rather than
    // any owner's rows, so they are the same answer for every principal; the
    // per-harness unrecognized-event counts beside them are summed over the
    // principal's own sessions.
    // ------------------------------------------------------------------

    /// Reserve a validated image for one of the principal's sessions.
    pub(crate) async fn publish_session_image(
        &self,
        session_id: SessionId,
        image: &tidebreak_core::ImageRef,
    ) -> Result<bool, ServerError> {
        Ok(tidebreak_core::db::code::publish_session_image(
            &self.runtime.db,
            &self.owner,
            session_id,
            image,
            chrono::Utc::now(),
        )
        .await?)
    }

    pub(crate) async fn list_sessions(&self) -> Result<Vec<Session>, ServerError> {
        self.runtime.list_sessions(&self.owner).await
    }

    pub(crate) fn adapters(&self) -> &tidebreak_harness::AdapterRegistry {
        &self.runtime.adapters
    }

    pub(crate) fn adapter(
        &self,
        kind: HarnessKind,
    ) -> Result<Arc<dyn tidebreak_harness::HarnessAdapter>, ServerError> {
        self.runtime.adapter(kind)
    }

    pub(crate) async fn probe(
        &self,
        adapter: &dyn tidebreak_harness::HarnessAdapter,
    ) -> tidebreak_harness::HarnessProbe {
        self.runtime.probe(adapter).await
    }

    pub(crate) fn pin_install_error(&self, kind: HarnessKind) -> Option<String> {
        self.runtime.pin_install_error(kind)
    }

    pub(crate) fn invalidate_probes(&self) {
        self.runtime.invalidate_probes();
    }

    /// Whether the on-behalf-of inference relay is active (decision 71):
    /// true only on a gateway-authenticated hosted machine, whose engines
    /// carry no provider credentials of their own. The doctor reads this to
    /// decide whether the local sign-in probe answers the right question.
    pub(crate) fn harness_llm_relay_active(&self) -> bool {
        self.runtime.harness_llm().is_some()
    }

    /// The engine inference relay, on a machine that has one. The hosted
    /// model listing reads the caller's gateway catalog through it.
    pub(crate) fn harness_llm(&self) -> Option<Arc<super::harness_llm::HarnessLlmRelay>> {
        self.runtime.harness_llm()
    }

    pub(crate) async fn gateway_model_snapshot(
        &self,
    ) -> Option<crate::providers::GatewayModelSnapshot> {
        self.runtime.gateway_model_snapshot(&self.owner).await
    }

    /// Warm the pinned install of one engine. See
    /// [`CodeRuntime::start_harness_install`].
    ///
    /// Progress reaches this principal's `/updates` socket; the binary it
    /// writes belongs to the machine, which is why the route sits on the
    /// deployment plane beside the doctor's refresh.
    pub(crate) async fn start_harness_install(
        &self,
        kind: HarnessKind,
        deliberate: bool,
    ) -> Result<CodeHarnessInstallSnapshot, ServerError> {
        self.runtime
            .start_harness_install(&self.owner, kind, deliberate)
            .await
    }

    /// Ask the registry for every engine's newest release. See
    /// [`CodeRuntime::check_harness_updates`].
    pub(crate) async fn check_harness_updates(&self) -> Result<(), String> {
        self.runtime.check_harness_updates().await
    }

    /// The update channel this machine is on.
    pub(crate) async fn harness_update_channel(&self) -> tidebreak_core::HarnessUpdateChannel {
        self.runtime.harness_update_channel().await
    }

    /// Where one engine stands against its pin and the registry.
    pub(crate) async fn harness_release_status(
        &self,
        kind: HarnessKind,
    ) -> super::harness_release::HarnessReleaseStatus {
        self.runtime.harness_release_status(kind).await
    }
}

impl FromRequestParts<AppState> for ScopedCode {
    type Rejection = ServerError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let auth = AuthContext::from_request_parts(parts, state)
            .await
            .map_err(|_| {
                ServerError::unauthorized("this request has no authenticated principal")
            })?;
        Self::new(state, &auth)
    }
}
