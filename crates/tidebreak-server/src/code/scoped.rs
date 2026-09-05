//! The request-facing code-mode runtime, bound to one authenticated principal.
//!
//! This is [`crate::scoped_store::ScopedStore`]'s counterpart for `/code/*`.
//! Route handlers do not touch [`CodeRuntime`] directly: they extract a
//! [`ScopedCode`], and every query it makes carries the requesting
//! principal's [`OwnerId`]. The unscoped runtime handle never escapes this
//! type, so route code cannot express a query that crosses owners — another
//! owner's repository, workspace, turn, event, or approval is
//! indistinguishable from one that does not exist (decisions 47 and 48).
//!
//! A session is the one thing that resolves for more than its owner, and
//! decision 0086 says on what terms. Resolution happens here, in
//! [`ScopedCode::session_access`], and there is exactly one of it: reads
//! resolve for the owner, for any access row, or on `deployment` visibility;
//! submit, queue, steer, interrupt, and approval decisions need `contribute`
//! or ownership; reap, permission mode, model, settings, delete, and access
//! management stay with the owner. A session the caller holds no claim on
//! still answers "not found", the same shape as one that never existed.
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
use crate::code::types::{
    CodeCloneDefaults, CodeCloneJobSnapshot, CodeDeliveryActionResult,
    CodeDeliveryPullRequestActionBody, CodeDeliveryPullRequestDetail, CodeDeliveryPullRequestQuery,
    CodeDeliveryPullRequestTarget, CodeDeliveryPullRequestsPage, CodeDeliveryRepositoriesSnapshot,
    CodeDeliveryRunActionBody, CodeDeliveryRunDetail, CodeDeliveryRunQuery, CodeDeliveryRunTarget,
    CodeDeliveryRunsPage, CodeHarnessInstallSnapshot, CodeRepoSources,
    ResolveCodeDeliveryRepositoriesBody,
};
use crate::error::ServerError;
use crate::principal::AuthContext;
use crate::state::AppState;

/// Code mode as one authenticated principal may see it.
///
/// Constructed only from server state plus a verified [`AuthContext`], so the
/// owner inside is always the one the auth middleware resolved. Fields stay
/// private and no method returns the inner runtime handle.
#[derive(Clone)]
pub struct ScopedCode {
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

    /// What this principal may do with this session (decision 0086).
    ///
    /// The single resolution step every session-scoped method below goes
    /// through. A session that does not exist and one this principal holds no
    /// claim on both answer "not found".
    async fn session_access(
        &self,
        id: SessionId,
    ) -> Result<tidebreak_core::db::code::ResolvedSessionAccess, ServerError> {
        tidebreak_core::db::code::resolve_session_access(&self.runtime.db, &self.owner, id)
            .await?
            .ok_or_else(|| ServerError::not_found("code session not found"))
    }

    /// The owner to run a read under. Reads resolve for the owner, for any
    /// access row, and on `deployment` visibility.
    ///
    /// The row still belongs to its owner, so the query underneath keeps
    /// carrying that key: a granted reader borrows the owner's scope for this
    /// one session, and gains nothing anywhere else.
    async fn session_owner_for_read(&self, id: SessionId) -> Result<OwnerId, ServerError> {
        Ok(self.session_access(id).await?.session.owner)
    }

    /// The owner to run a write under. Submit, queue, steer, interrupt, and
    /// approval decisions need `contribute` or ownership.
    async fn session_owner_for_contribute(&self, id: SessionId) -> Result<OwnerId, ServerError> {
        let access = self.session_access(id).await?;
        if !access.owner && access.level != tidebreak_core::SessionAccessLevel::Contribute {
            return Err(ServerError::not_found("code session not found"));
        }
        Ok(access.session.owner)
    }

    /// Refuse anyone but the owner. Reap, permission mode, model, settings,
    /// delete, and access management never leave the owner.
    async fn require_session_owner(&self, id: SessionId) -> Result<Session, ServerError> {
        let access = self.session_access(id).await?;
        if !access.owner {
            return Err(ServerError::not_found("code session not found"));
        }
        Ok(access.session)
    }

    /// Bind an already-resolved runtime to a principal.
    ///
    /// For callers that have decided for themselves what to do when code mode
    /// is not configured — the inbox lists chats either way, so it must not
    /// take the extractor's all-or-nothing rejection.
    pub fn for_owner(runtime: std::sync::Arc<super::runtime::CodeRuntime>, owner: OwnerId) -> Self {
        Self {
            runtime,
            owner,
            allow_unscoped_delivery: false,
        }
    }

    /// The principal's durable owner key, for the seams that take it directly:
    /// the live buses, and background naming.
    pub fn owner(&self) -> &OwnerId {
        &self.owner
    }

    /// Whose scope this session's event socket runs under, and whether the
    /// caller owns it. The socket needs both: the owner key to read the
    /// journal, and the caller's own standing so a granted reader's stream can
    /// be severed when their row goes.
    pub async fn event_stream_access(&self, id: SessionId) -> Result<(OwnerId, bool), ServerError> {
        let access = self.session_access(id).await?;
        Ok((access.session.owner, access.owner))
    }

    // ------------------------------------------------------------------
    // Repositories. Per owner, like everything else here: two users may
    // register or clone the same remote and neither sees the other's row.
    // ------------------------------------------------------------------

    pub async fn register_repo(
        &self,
        root_path: std::path::PathBuf,
        metadata: RepoRegistration,
    ) -> Result<CodeRepo, ServerError> {
        self.runtime
            .register_repo(&self.owner, root_path, metadata)
            .await
    }

    pub async fn list_repos(&self) -> Result<Vec<CodeRepo>, ServerError> {
        self.runtime.list_repos(&self.owner).await
    }

    pub async fn get_repo(&self, id: RepoId) -> Result<CodeRepo, ServerError> {
        self.runtime.get_repo(&self.owner, id).await
    }

    pub async fn save_repo(&self, repo: &CodeRepo) -> Result<(), ServerError> {
        self.runtime.save_repo(repo).await
    }

    pub async fn remove_repo(&self, id: RepoId, reclaim_checkout: bool) -> Result<(), ServerError> {
        self.runtime
            .remove_repo(&self.owner, id, reclaim_checkout)
            .await
    }

    pub async fn clone_defaults(&self) -> Result<CodeCloneDefaults, ServerError> {
        self.runtime.clone_defaults().await
    }

    pub async fn repo_sources(&self) -> Result<CodeRepoSources, ServerError> {
        self.runtime.repo_sources(&self.owner).await
    }

    pub async fn list_github_repositories(
        &self,
    ) -> Result<crate::code::types::CodeGithubRepositories, ServerError> {
        self.runtime.list_github_repositories(&self.owner).await
    }

    pub async fn start_clone(
        &self,
        request: CloneRequest,
    ) -> Result<CodeCloneJobSnapshot, ServerError> {
        self.runtime.start_clone(&self.owner, request).await
    }

    pub fn get_clone_job(&self, id: uuid::Uuid) -> Result<CodeCloneJobSnapshot, ServerError> {
        self.runtime.get_clone_job(&self.owner, id)
    }

    // ------------------------------------------------------------------
    // Install-wide GitHub delivery views. Remote state is live and cached;
    // workspace correlation remains scoped to this owner.
    // ------------------------------------------------------------------

    pub async fn discover_delivery_repositories(
        &self,
        refresh: bool,
    ) -> Result<CodeDeliveryRepositoriesSnapshot, ServerError> {
        delivery::discover_repositories(&self.runtime, &self.owner, refresh).await
    }

    pub async fn resolve_delivery_repositories(
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

    pub async fn query_delivery_pull_requests(
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

    pub async fn delivery_pull_request_detail(
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

    pub async fn act_on_delivery_pull_request(
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

    pub async fn query_delivery_runs(
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

    pub async fn delivery_run_detail(
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

    pub async fn act_on_delivery_run(
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

    pub async fn worktree_root(&self) -> Result<crate::code::types::CodeWorktreeRoot, ServerError> {
        self.runtime.worktree_root_snapshot().await
    }

    pub async fn set_worktree_root(
        &self,
        root: Option<&str>,
    ) -> Result<crate::code::types::CodeWorktreeRoot, ServerError> {
        self.runtime.set_worktree_root(root).await
    }

    // ------------------------------------------------------------------
    // Workspaces.
    // ------------------------------------------------------------------

    pub async fn create_workspace(
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

    pub async fn create_remote_workspace(
        &self,
        repo_id: RepoId,
        title: Option<String>,
    ) -> Result<CodeWorkspace, ServerError> {
        self.runtime
            .create_remote_workspace(&self.owner, repo_id, title)
            .await
    }

    pub async fn list_workspaces(
        &self,
        repo_id: Option<RepoId>,
    ) -> Result<Vec<CodeWorkspace>, ServerError> {
        self.runtime.list_workspaces(&self.owner, repo_id).await
    }

    pub async fn get_workspace(&self, id: WorkspaceId) -> Result<CodeWorkspace, ServerError> {
        self.runtime.get_workspace(&self.owner, id).await
    }

    pub async fn save_workspace(&self, workspace: &CodeWorkspace) -> Result<(), ServerError> {
        self.runtime.save_workspace(workspace).await
    }

    pub async fn archive_workspace(
        &self,
        id: WorkspaceId,
        force: bool,
        terminals: &crate::code::terminal::TerminalHub,
    ) -> Result<CodeWorkspace, ServerError> {
        self.runtime
            .archive_workspace(&self.owner, id, force, terminals)
            .await
    }

    pub fn workspace_write_lock(&self, id: WorkspaceId) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        self.runtime.workspace_write_lock(id)
    }

    pub async fn restore_workspace(&self, id: WorkspaceId) -> Result<CodeWorkspace, ServerError> {
        self.runtime.restore_workspace(&self.owner, id).await
    }

    pub async fn retry_workspace_setup(
        &self,
        id: WorkspaceId,
    ) -> Result<CodeWorkspace, ServerError> {
        self.runtime.retry_workspace_setup(&self.owner, id).await
    }

    pub async fn workspace_tree(
        &self,
        id: WorkspaceId,
        query: &str,
        limit: Option<u32>,
    ) -> Result<(Vec<String>, bool), ServerError> {
        self.runtime
            .workspace_tree(&self.owner, id, query, limit)
            .await
    }

    pub async fn workspace_search(
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

    pub async fn workspace_transcript_search(
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

    pub async fn workspace_blob(
        &self,
        id: WorkspaceId,
        path: &str,
    ) -> Result<worktree::WorktreeBlob, ServerError> {
        self.runtime.workspace_blob(&self.owner, id, path).await
    }

    pub async fn workspace_files(
        &self,
        id: WorkspaceId,
        turn_id: Option<TurnId>,
    ) -> Result<(Vec<ChangedFile>, bool, Diffstat, Option<TurnId>), ServerError> {
        self.runtime.workspace_files(&self.owner, id, turn_id).await
    }

    pub async fn workspace_diff(
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

    pub async fn commit_workspace(
        &self,
        id: WorkspaceId,
        message: Option<String>,
    ) -> Result<CommitOutcome, ServerError> {
        self.runtime
            .commit_workspace(&self.owner, id, message)
            .await
    }

    pub async fn push_workspace(&self, id: WorkspaceId) -> Result<PushOutcome, ServerError> {
        self.runtime.push_workspace(&self.owner, id).await
    }

    pub async fn list_triggers(&self, repo_id: RepoId) -> Result<Vec<CodeTrigger>, ServerError> {
        self.runtime.list_triggers(&self.owner, repo_id).await
    }

    pub async fn create_trigger(
        &self,
        repo_id: RepoId,
        condition: CodeTriggerCondition,
        action: CodeTriggerAction,
    ) -> Result<CodeTrigger, ServerError> {
        self.runtime
            .create_trigger(&self.owner, repo_id, condition, action)
            .await
    }

    pub async fn set_trigger_enabled(
        &self,
        repo_id: RepoId,
        id: CodeTriggerId,
        enabled: bool,
    ) -> Result<CodeTrigger, ServerError> {
        self.runtime
            .set_trigger_enabled(&self.owner, repo_id, id, enabled)
            .await
    }

    pub async fn delete_trigger(
        &self,
        repo_id: RepoId,
        id: CodeTriggerId,
    ) -> Result<(), ServerError> {
        self.runtime.delete_trigger(&self.owner, repo_id, id).await
    }

    pub async fn workspace_pr(&self, id: WorkspaceId) -> Result<WorkspaceGitStatus, ServerError> {
        self.runtime.workspace_pr(&self.owner, id).await
    }

    pub async fn workspace_pull_requests(
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

    pub async fn refresh_workspace_pr(
        &self,
        id: WorkspaceId,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        self.runtime.refresh_workspace_pr(&self.owner, id).await
    }

    pub async fn start_watch(
        &self,
        id: WorkspaceId,
        permission_mode_ceiling: Option<tidebreak_core::PermissionMode>,
    ) -> Result<tidebreak_core::CodeWatch, ServerError> {
        self.runtime
            .start_watch(&self.owner, id, permission_mode_ceiling)
            .await
    }

    pub async fn stop_watch(
        &self,
        id: WorkspaceId,
    ) -> Result<tidebreak_core::CodeWatch, ServerError> {
        self.runtime.stop_watch(&self.owner, id).await
    }

    pub async fn latest_watch(
        &self,
        id: WorkspaceId,
    ) -> Result<Option<tidebreak_core::CodeWatch>, ServerError> {
        self.runtime.latest_watch(&self.owner, id).await
    }

    pub async fn workspace_pr_comments(
        &self,
        id: WorkspaceId,
    ) -> Result<gh::PrComments, ServerError> {
        self.runtime.workspace_pr_comments(&self.owner, id).await
    }

    pub async fn workspace_check_logs(
        &self,
        id: WorkspaceId,
    ) -> Result<(Option<String>, super::ci_logs::WrittenCheckLogs), ServerError> {
        self.runtime.workspace_check_logs(&self.owner, id).await
    }

    pub async fn merge_workspace_pr(
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

    pub async fn mark_workspace_pr_ready(
        &self,
        id: WorkspaceId,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        self.runtime.mark_workspace_pr_ready(&self.owner, id).await
    }

    pub async fn create_workspace_pr(
        &self,
        id: WorkspaceId,
        title: Option<String>,
        body: Option<String>,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        self.runtime
            .create_workspace_pr(&self.owner, id, title, body)
            .await
    }

    pub async fn run_workspace_action(
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

    pub async fn create_session(
        &self,
        workspace_id: WorkspaceId,
        harness: HarnessKind,
        settings: NewSessionSettings,
    ) -> Result<Session, ServerError> {
        self.runtime
            .create_session(&self.owner, workspace_id, harness, settings)
            .await
    }

    pub async fn create_internal_session(
        &self,
        settings: NewSessionSettings,
    ) -> Result<Session, ServerError> {
        self.runtime
            .create_internal_session(&self.owner, settings)
            .await
    }

    pub async fn create_remote_session(
        &self,
        workspace_id: WorkspaceId,
        harness: HarnessKind,
        settings: NewSessionSettings,
    ) -> Result<Session, ServerError> {
        self.runtime
            .create_remote_session(&self.owner, workspace_id, harness, settings)
            .await
    }

    pub async fn get_session(&self, id: SessionId) -> Result<Session, ServerError> {
        Ok(self.session_access(id).await?.session)
    }

    /// One session's access list. Owner-only (decision 0086).
    pub async fn list_session_access(
        &self,
        id: SessionId,
    ) -> Result<Vec<tidebreak_core::db::code::SessionAccess>, ServerError> {
        self.require_session_owner(id).await?;
        Ok(
            tidebreak_core::db::code::list_session_access(&self.runtime.db, &self.owner, id)
                .await?,
        )
    }

    /// Add or raise one subject's access. Owner-only, and idempotent: granting
    /// a subject that already holds a row rewrites its level.
    pub async fn grant_session_access(
        &self,
        id: SessionId,
        subject: &str,
        level: tidebreak_core::SessionAccessLevel,
    ) -> Result<tidebreak_core::db::code::SessionAccess, ServerError> {
        let session = self.require_session_owner(id).await?;
        if !tidebreak_core::db::code::valid_access_subject(subject) {
            return Err(ServerError::bad_request_kind(
                "invalid_access_subject",
                "a subject is `principal:<key>` or `external:<channel kind>:<id>`",
            ));
        }
        let row = tidebreak_core::db::code::grant_session_access(
            &self.runtime.db,
            &self.owner,
            id,
            subject,
            level,
            chrono::Utc::now(),
        )
        .await?
        .ok_or_else(|| ServerError::not_found("code session not found"))?;
        self.announce_access_change(&session, &[]).await;
        Ok(row)
    }

    /// Drop one subject's access. Owner-only. The principals the row resolved
    /// for are read before the delete, so the reader that just lost it is told
    /// and can close what it was watching.
    pub async fn revoke_session_access(
        &self,
        id: SessionId,
        subject: &str,
    ) -> Result<bool, ServerError> {
        let session = self.require_session_owner(id).await?;
        let before = tidebreak_core::db::code::session_readers_all_owners(&self.runtime.db, id)
            .await
            .unwrap_or_default();
        let revoked = tidebreak_core::db::code::revoke_session_access(
            &self.runtime.db,
            &self.owner,
            id,
            subject,
        )
        .await?;
        if revoked {
            self.announce_access_change(&session, &before).await;
        }
        Ok(revoked)
    }

    /// Set who may discover the session without a row. Owner-only, and never
    /// a grant of writes.
    pub async fn set_session_visibility(
        &self,
        id: SessionId,
        visibility: tidebreak_core::SessionVisibility,
    ) -> Result<Session, ServerError> {
        let before = self.require_session_owner(id).await?;
        let session = tidebreak_core::db::code::set_session_visibility(
            &self.runtime.db,
            &self.owner,
            id,
            visibility,
        )
        .await?
        .ok_or_else(|| ServerError::not_found("code session not found"))?;
        // Narrowing to `private` has to reach the readers the old visibility
        // admitted, which is why the announcement runs against both.
        self.announce_access_change(&before, &[]).await;
        self.announce_access_change(&session, &[]).await;
        Ok(session)
    }

    /// Publish the access change and a fresh digest to everyone it touches.
    async fn announce_access_change(&self, session: &Session, also: &[OwnerId]) {
        super::attention::emit_access_changed(&self.runtime.db, &self.runtime.bus, session, also)
            .await;
        super::attention::emit_digest(&self.runtime.db, &self.runtime.bus, session).await;
    }

    pub async fn list_internal_sessions(&self) -> Result<Vec<Session>, ServerError> {
        self.runtime.list_internal_sessions(&self.owner).await
    }

    pub async fn list_workspace_sessions(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<Session>, ServerError> {
        self.runtime
            .list_workspace_sessions(&self.owner, workspace_id)
            .await
    }

    pub async fn external_bindings_for_sessions(
        &self,
        session_ids: &[SessionId],
    ) -> Result<Vec<tidebreak_core::CodeExternalBinding>, ServerError> {
        self.runtime
            .external_bindings_for_sessions(&self.owner, session_ids)
            .await
    }

    pub async fn list_session_turns(&self, id: SessionId) -> Result<Vec<Turn>, ServerError> {
        let owner = self.session_owner_for_read(id).await?;
        self.runtime.list_session_turns(&owner, id).await
    }

    pub async fn list_turn_metrics(
        &self,
    ) -> Result<Vec<tidebreak_core::db::code::TurnMetric>, ServerError> {
        self.runtime.list_turn_metrics(&self.owner).await
    }

    pub async fn list_pull_request_facts(
        &self,
    ) -> Result<Vec<tidebreak_core::CodePullRequestFact>, ServerError> {
        self.runtime.list_pull_request_facts(&self.owner).await
    }

    pub async fn list_pull_request_attributions(
        &self,
    ) -> Result<Vec<tidebreak_core::CodePullRequestAttribution>, ServerError> {
        self.runtime
            .list_pull_request_attributions(&self.owner)
            .await
    }

    pub async fn fork_transcript(
        &self,
        id: SessionId,
        at_turn: Option<tidebreak_core::TurnId>,
    ) -> Result<super::fork::WrittenTranscript, ServerError> {
        self.runtime.fork_transcript(&self.owner, id, at_turn).await
    }

    pub async fn session_debug(
        &self,
        id: SessionId,
    ) -> Result<(Session, Vec<Turn>, Vec<SequencedEvent>), ServerError> {
        self.runtime.session_debug(&self.owner, id).await
    }

    pub async fn resolve_turn_attachments(
        &self,
        session_id: SessionId,
        requested: &[(uuid::Uuid, String)],
    ) -> Result<Vec<tidebreak_core::ImageRef>, ServerError> {
        let owner = self.session_owner_for_contribute(session_id).await?;
        self.runtime
            .resolve_turn_attachments(&owner, session_id, requested)
            .await
    }

    pub async fn submit_turn(
        &self,
        id: SessionId,
        message: String,
        model: Option<String>,
        reasoning_effort: Option<Option<ReasoningEffort>>,
        attachments: Vec<tidebreak_core::ImageRef>,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        let owner = self.session_owner_for_contribute(id).await?;
        // The turn records who sent it, not whose session it ran under
        // (decision 0086). On a shared session those differ.
        let actor = tidebreak_core::TurnActor::principal(&self.owner);
        self.runtime
            .submit_turn(
                &owner,
                id,
                message,
                model,
                reasoning_effort,
                attachments,
                Some(actor),
            )
            .await
    }

    pub async fn list_queued_turns(
        &self,
        id: SessionId,
    ) -> Result<(Vec<QueuedTurn>, bool), ServerError> {
        let owner = self.session_owner_for_read(id).await?;
        self.runtime.list_queued_turns(&owner, id).await
    }

    pub async fn update_queued_turn(
        &self,
        id: SessionId,
        queued_id: TurnId,
        message: Option<&str>,
        position: Option<i32>,
    ) -> Result<Option<QueuedTurn>, ServerError> {
        let owner = self.session_owner_for_contribute(id).await?;
        self.runtime
            .update_queued_turn(&owner, id, queued_id, message, position)
            .await
    }

    pub async fn delete_queued_turn(
        &self,
        id: SessionId,
        queued_id: TurnId,
    ) -> Result<bool, ServerError> {
        let owner = self.session_owner_for_contribute(id).await?;
        self.runtime.delete_queued_turn(&owner, id, queued_id).await
    }

    pub async fn set_queue_paused(&self, id: SessionId, paused: bool) -> Result<(), ServerError> {
        let owner = self.session_owner_for_contribute(id).await?;
        self.runtime.set_queue_paused(&owner, id, paused).await
    }

    pub async fn send_queued_now(&self, id: SessionId) -> Result<(), ServerError> {
        let owner = self.session_owner_for_contribute(id).await?;
        self.runtime.send_queued_now(&owner, id).await
    }

    pub async fn set_reasoning_effort(
        &self,
        id: SessionId,
        effort: Option<ReasoningEffort>,
    ) -> Result<Session, ServerError> {
        self.require_session_owner(id).await?;
        self.runtime
            .set_reasoning_effort(&self.owner, id, effort)
            .await
    }

    pub async fn set_fast_mode(
        &self,
        id: SessionId,
        fast_mode: bool,
    ) -> Result<Session, ServerError> {
        self.require_session_owner(id).await?;
        self.runtime.set_fast_mode(&self.owner, id, fast_mode).await
    }

    pub async fn interrupt(&self, id: SessionId) -> Result<(), ServerError> {
        let _ = self.session_owner_for_contribute(id).await?;
        self.runtime.interrupt(id).await
    }

    pub async fn steer(
        &self,
        id: SessionId,
        expected_turn_id: TurnId,
        message: String,
    ) -> Result<(), ServerError> {
        let owner = self.session_owner_for_contribute(id).await?;
        self.runtime
            .steer(&owner, id, expected_turn_id, message)
            .await
    }

    pub async fn reap(&self, id: SessionId) -> Result<Session, ServerError> {
        self.require_session_owner(id).await?;
        self.runtime.reap(&self.owner, id).await
    }

    // ------------------------------------------------------------------
    // Adapter grants and the connect handshake (docs/slack-sessions.md).
    // ------------------------------------------------------------------

    pub async fn list_adapter_grants(
        &self,
    ) -> Result<Vec<tidebreak_core::CodeExternalGrant>, ServerError> {
        self.runtime.list_adapter_grants(&self.owner).await
    }

    pub async fn list_adapter_grant_profiles(
        &self,
    ) -> Result<Vec<tidebreak_core::CodeGrantProfile>, ServerError> {
        self.runtime.list_adapter_grant_profiles(&self.owner).await
    }

    pub async fn revoke_adapter_grant(
        &self,
        id: tidebreak_core::CodeGrantId,
        reason: &str,
    ) -> Result<Option<tidebreak_core::CodeExternalGrant>, ServerError> {
        self.runtime
            .revoke_adapter_grant(&self.owner, id, reason)
            .await
    }

    pub async fn revoke_workspace_grants(
        &self,
        channel_kind: &str,
        workspace_identity: &str,
        reason: &str,
    ) -> Result<Vec<tidebreak_core::CodeExternalGrant>, ServerError> {
        self.runtime
            .revoke_workspace_grants(&self.owner, channel_kind, workspace_identity, reason)
            .await
    }

    pub async fn view_connect_handshake(
        &self,
        nonce: &str,
    ) -> Result<Option<(tidebreak_core::CodeConnectHandshake, String)>, ServerError> {
        self.runtime
            .view_connect_handshake(&self.owner, nonce)
            .await
    }

    pub async fn approve_connect_handshake(
        &self,
        nonce: &str,
        csrf: &str,
        lease: Option<&crate::auth::GatewayAuthLease>,
    ) -> Result<Option<tidebreak_core::CodeConnectHandshake>, ServerError> {
        self.runtime
            .approve_connect_handshake(&self.owner, nonce, csrf, lease)
            .await
    }

    pub async fn set_permission_mode(
        &self,
        id: SessionId,
        mode: PermissionMode,
    ) -> Result<Session, ServerError> {
        self.require_session_owner(id).await?;
        self.runtime
            .set_permission_mode(&self.owner, id, mode)
            .await
    }

    pub async fn set_attention(
        &self,
        id: SessionId,
        clear: bool,
        note: Option<String>,
    ) -> Result<Session, ServerError> {
        self.require_session_owner(id).await?;
        self.runtime
            .set_attention(&self.owner, id, clear, note)
            .await
    }

    // ------------------------------------------------------------------
    // Approvals.
    // ------------------------------------------------------------------

    pub async fn list_approvals(
        &self,
        state: Option<ApprovalState>,
        session_id: Option<SessionId>,
    ) -> Result<Vec<Approval>, ServerError> {
        let owner = match session_id {
            Some(id) => self.session_owner_for_read(id).await?,
            None => self.owner.clone(),
        };
        self.runtime.list_approvals(&owner, state, session_id).await
    }

    pub async fn decide_approval(
        &self,
        id: ApprovalId,
        decision: super::runtime::ApprovalDecisionRequest,
    ) -> Result<Approval, ServerError> {
        let approval = tidebreak_core::db::code::get_approval_all_owners(&self.runtime.db, id)
            .await?
            .ok_or_else(|| ServerError::not_found(format!("approval {id} not found")))?;
        let owner = self
            .session_owner_for_contribute(approval.session_id)
            .await?;
        let actor = tidebreak_core::TurnActor {
            principal: Some(self.owner.to_string()),
            display: None,
            channel_kind: None,
            external_identity: None,
        };
        self.runtime
            .decide_approval(&owner, id, decision, Some(actor))
            .await
    }

    // ------------------------------------------------------------------
    // Harness discovery. Probes describe the machine's binaries rather than
    // any owner's rows, so they are the same answer for every principal; the
    // per-harness unrecognized-event counts beside them are summed over the
    // principal's own sessions.
    // ------------------------------------------------------------------

    /// Reserve a validated image for a session the principal may drive.
    ///
    /// Publication is a write: it is what a later turn attachment is checked
    /// against, so it takes `contribute` like the turn that will carry the
    /// image, and the row is written under the session's owner, whose
    /// session it is (decision 0086). A viewer is refused as not found, the
    /// way every other write answers them.
    pub async fn publish_session_image(
        &self,
        session_id: SessionId,
        image: &tidebreak_core::ImageRef,
    ) -> Result<bool, ServerError> {
        let owner = self.session_owner_for_contribute(session_id).await?;
        Ok(tidebreak_core::db::code::publish_session_image(
            &self.runtime.db,
            &owner,
            session_id,
            image,
            chrono::Utc::now(),
        )
        .await?)
    }

    /// Refuse a principal who may not drive this session, before a route
    /// does work on their behalf. The same answer as
    /// [`Self::publish_session_image`] gives, settled before the bytes are
    /// stored rather than after.
    pub async fn ensure_session_contributor(
        &self,
        session_id: SessionId,
    ) -> Result<(), ServerError> {
        self.session_owner_for_contribute(session_id)
            .await
            .map(|_| ())
    }

    pub async fn list_sessions(&self) -> Result<Vec<Session>, ServerError> {
        self.runtime.list_sessions(&self.owner).await
    }

    pub fn adapters(&self) -> &tidebreak_harness::AdapterRegistry {
        &self.runtime.adapters
    }

    pub fn adapter(
        &self,
        kind: HarnessKind,
    ) -> Result<Arc<dyn tidebreak_harness::HarnessAdapter>, ServerError> {
        self.runtime.adapter(kind)
    }

    pub async fn probe(
        &self,
        adapter: &dyn tidebreak_harness::HarnessAdapter,
    ) -> tidebreak_harness::HarnessProbe {
        self.runtime.probe(adapter).await
    }

    pub fn pin_install_error(&self, kind: HarnessKind) -> Option<String> {
        self.runtime.pin_install_error(kind)
    }

    pub fn invalidate_probes(&self) {
        self.runtime.invalidate_probes();
    }

    /// Whether the on-behalf-of inference relay is active (decision 71):
    /// true only on a gateway-authenticated hosted machine, whose engines
    /// carry no provider credentials of their own. The doctor reads this to
    /// decide whether the local sign-in probe answers the right question.
    pub fn harness_llm_relay_active(&self) -> bool {
        self.runtime.harness_llm().is_some()
    }

    /// The engine inference relay, on a machine that has one. The hosted
    /// model listing reads the caller's gateway catalog through it.
    pub fn harness_llm(&self) -> Option<Arc<super::harness_llm::HarnessLlmRelay>> {
        self.runtime.harness_llm()
    }

    pub async fn gateway_model_snapshot(&self) -> Option<crate::providers::GatewayModelSnapshot> {
        self.runtime.gateway_model_snapshot(&self.owner).await
    }

    /// Warm the pinned install of one engine. See
    /// [`CodeRuntime::start_harness_install`].
    ///
    /// Progress reaches this principal's `/updates` socket; the binary it
    /// writes belongs to the machine, which is why the route sits on the
    /// deployment plane beside the doctor's refresh.
    pub async fn start_harness_install(
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
    pub async fn check_harness_updates(&self) -> Result<(), String> {
        self.runtime.check_harness_updates().await
    }

    /// The update channel this machine is on.
    pub async fn harness_update_channel(&self) -> tidebreak_core::HarnessUpdateChannel {
        self.runtime.harness_update_channel().await
    }

    /// Where one engine stands against its pin and the registry.
    pub async fn harness_release_status(
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
