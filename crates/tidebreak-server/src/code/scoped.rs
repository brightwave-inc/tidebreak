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

use tidebreak_core::{
    CodeApproval, CodeApprovalId, CodeApprovalState, CodePermissionMode, CodeRepo, CodeSession,
    CodeSessionId, CodeTurn, CodeTurnId, CodeWorkspace, Diffstat, HarnessKind, OwnerId, RepoId,
    SequencedCodeEvent, WorkspaceId,
};
use tidebreak_harness::ApprovalDecision;

use super::checkpoint::ChangedFile;
use super::clone::CloneRequest;
use super::gh::{self, ActionOutcome, CommitOutcome, PushOutcome, WorkspaceGitStatus};
use super::runtime::{CodeRuntime, RepoRegistration, SubmitTurnOutcome};
use super::worktree;
use crate::error::ServerError;
use crate::principal::AuthContext;
use crate::routes::code::types::{CodeCloneDefaults, CodeCloneJobSnapshot};
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
        })
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

    pub(crate) async fn delete_repo(&self, id: RepoId) -> Result<(), ServerError> {
        self.runtime.delete_repo(&self.owner, id).await
    }

    pub(crate) async fn clone_defaults(&self) -> Result<CodeCloneDefaults, ServerError> {
        self.runtime.clone_defaults().await
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
        self.runtime.get_clone_job(id)
    }

    // ------------------------------------------------------------------
    // Workspaces.
    // ------------------------------------------------------------------

    pub(crate) async fn create_workspace(
        &self,
        repo_id: RepoId,
        title: Option<String>,
        base_ref: Option<String>,
    ) -> Result<CodeWorkspace, ServerError> {
        self.runtime
            .create_workspace(&self.owner, repo_id, title, base_ref)
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
    ) -> Result<CodeWorkspace, ServerError> {
        self.runtime.archive_workspace(&self.owner, id, force).await
    }

    pub(crate) async fn restore_workspace(
        &self,
        id: WorkspaceId,
    ) -> Result<CodeWorkspace, ServerError> {
        self.runtime.restore_workspace(&self.owner, id).await
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
        turn_id: Option<CodeTurnId>,
    ) -> Result<(Vec<ChangedFile>, bool, Diffstat, Option<CodeTurnId>), ServerError> {
        self.runtime.workspace_files(&self.owner, id, turn_id).await
    }

    pub(crate) async fn workspace_diff(
        &self,
        id: WorkspaceId,
        turn_id: Option<CodeTurnId>,
        file: Option<&str>,
    ) -> Result<(String, bool, Diffstat, Option<CodeTurnId>), ServerError> {
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

    pub(crate) async fn workspace_pr(
        &self,
        id: WorkspaceId,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        self.runtime.workspace_pr(&self.owner, id).await
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
    ) -> Result<tidebreak_core::CodeWatch, ServerError> {
        self.runtime.start_watch(&self.owner, id).await
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

    pub(crate) async fn merge_workspace_pr(
        &self,
        id: WorkspaceId,
        method: gh::MergeMethod,
        auto: bool,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        self.runtime
            .merge_workspace_pr(&self.owner, id, method, auto)
            .await
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
        permission_mode: CodePermissionMode,
        model: Option<String>,
    ) -> Result<CodeSession, ServerError> {
        self.runtime
            .create_session(&self.owner, workspace_id, harness, permission_mode, model)
            .await
    }

    pub(crate) async fn get_session(&self, id: CodeSessionId) -> Result<CodeSession, ServerError> {
        self.runtime.get_session(&self.owner, id).await
    }

    pub(crate) async fn list_workspace_sessions(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<CodeSession>, ServerError> {
        self.runtime
            .list_workspace_sessions(&self.owner, workspace_id)
            .await
    }

    pub(crate) async fn list_session_turns(
        &self,
        id: CodeSessionId,
    ) -> Result<Vec<CodeTurn>, ServerError> {
        self.runtime.list_session_turns(&self.owner, id).await
    }

    pub(crate) async fn session_debug(
        &self,
        id: CodeSessionId,
    ) -> Result<(CodeSession, Vec<CodeTurn>, Vec<SequencedCodeEvent>), ServerError> {
        self.runtime.session_debug(&self.owner, id).await
    }

    pub(crate) async fn resolve_turn_attachments(
        &self,
        requested: &[(uuid::Uuid, String)],
    ) -> Result<Vec<tidebreak_core::CodeTurnAttachment>, ServerError> {
        self.runtime.resolve_turn_attachments(requested).await
    }

    pub(crate) async fn submit_turn(
        &self,
        id: CodeSessionId,
        message: String,
        model: Option<String>,
        attachments: Vec<tidebreak_core::CodeTurnAttachment>,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        self.runtime
            .submit_turn(&self.owner, id, message, model, attachments)
            .await
    }

    pub(crate) async fn interrupt(&self, id: CodeSessionId) -> Result<(), ServerError> {
        // Authorize the session first: without this the worker registry would
        // answer for a session id whatever owner holds it.
        let _ = self.get_session(id).await?;
        self.runtime.interrupt(id).await
    }

    pub(crate) async fn steer(
        &self,
        id: CodeSessionId,
        expected_turn_id: CodeTurnId,
        message: String,
    ) -> Result<(), ServerError> {
        self.runtime
            .steer(&self.owner, id, expected_turn_id, message)
            .await
    }

    pub(crate) async fn reap(&self, id: CodeSessionId) -> Result<CodeSession, ServerError> {
        self.runtime.reap(&self.owner, id).await
    }

    pub(crate) async fn set_attention(
        &self,
        id: CodeSessionId,
        clear: bool,
        note: Option<String>,
    ) -> Result<CodeSession, ServerError> {
        self.runtime
            .set_attention(&self.owner, id, clear, note)
            .await
    }

    // ------------------------------------------------------------------
    // Approvals.
    // ------------------------------------------------------------------

    pub(crate) async fn list_approvals(
        &self,
        state: Option<CodeApprovalState>,
        session_id: Option<CodeSessionId>,
    ) -> Result<Vec<CodeApproval>, ServerError> {
        self.runtime
            .list_approvals(&self.owner, state, session_id)
            .await
    }

    pub(crate) async fn decide_approval(
        &self,
        id: CodeApprovalId,
        decision: ApprovalDecision,
    ) -> Result<CodeApproval, ServerError> {
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

    pub(crate) async fn list_sessions(&self) -> Result<Vec<CodeSession>, ServerError> {
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

    #[cfg(not(test))]
    pub(crate) async fn refresh_pinned_harnesses(&self) {
        self.runtime.refresh_pinned_harnesses().await;
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
