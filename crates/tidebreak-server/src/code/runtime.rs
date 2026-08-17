//! Process-wide code-mode runtime: adapters, workers, worktrees, recovery.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use tokio::sync::oneshot;

use tidebreak_core::db::code::{
    delete_repo, delete_workspace, get_approval, get_open_turn, get_repo, get_repo_by_root_path,
    get_session, get_workspace, insert_repo, insert_session, insert_workspace, list_approvals,
    list_repos, list_sessions, list_sessions_for_workspace, list_turns, list_workspaces,
    save_approval, save_repo, save_workspace,
};
use tidebreak_core::{
    ApprovalDecisionKind, Attention, AttentionSource, CapLevel, CodeApproval, CodeApprovalId,
    CodeApprovalState, CodeEvent, CodePermissionMode, CodeRepo, CodeSession, CodeSessionId,
    CodeSessionLifecycle, CodeTurn, CodeTurnId, CodeWorkspace, CodeWorkspaceStatus, DbStore,
    Diffstat, HarnessKind, RepoId, WorkspaceId,
};
use tidebreak_harness::{
    builtin_registry, AdapterRegistry, ApprovalChannelSpec, ApprovalDecision, HarnessAdapter,
    HarnessEvent, HarnessEventSink, HostEnv, SessionSpec,
};

use super::approval_bridge::ApprovalBridge;
use super::bus::CodeEventBus;
use super::checkpoint::{
    delete_workspace_refs, list_changed_files, produce_diff, resolve_diff_range,
    sweep_orphaned_refs, ChangedFile, CheckpointError, DiffBounds,
};
use super::clone::CloneJobs;
use super::gh::{
    self, ActionOutcome, CommitOutcome, GhError, PrDigestCache, PushOutcome, WorkspaceGitStatus,
};
use super::recovery::{self, RecoveryAction};
use super::session_worker::{
    attach_engine, journal_event, queue_follow_up, spawn_session_worker, WorkerCommand,
    WorkerError, WorkerHandle,
};
use super::worktree::{
    self, archive_blockers, branch_name, create_worktree, prune_worktrees, remove_worktree,
    run_setup_script, slugify, validate_repo_path, worktree_dir, WorktreeError,
};
use crate::error::ServerError;

/// Result of `POST /code/sessions/{id}/turns`.
pub(crate) enum SubmitTurnOutcome {
    /// The session was idle; the turn ran to a terminal event.
    Ran(Box<CodeTurn>),
    /// The session was running; the message occupies the single follow-up slot.
    Queued,
}

/// Shared code-mode services for the process.
pub(crate) struct CodeRuntime {
    pub db: Arc<DbStore>,
    pub bus: Arc<CodeEventBus>,
    pub adapters: AdapterRegistry,
    pub data_dir: PathBuf,
    pub approvals: Arc<ApprovalBridge>,
    host: HostEnv,
    loopback_base: Mutex<Option<String>>,
    workers: Mutex<HashMap<CodeSessionId, WorkerHandle>>,
    pr_cache: PrDigestCache,
    pub(crate) clone_jobs: CloneJobs,
    #[cfg(test)]
    pub(crate) gh_search_path: Mutex<Option<String>>,
    stall_sweep: Mutex<Option<super::attention::StallSweepGuard>>,
    stall_started: AtomicBool,
    /// Workspaces with a background naming call in flight.
    ///
    /// One call per workspace at a time; a second trigger is dropped rather
    /// than queued, because the next turn on a still-unnamed workspace retries
    /// anyway (`super::titling`).
    pub(super) titling_in_flight: Mutex<std::collections::HashSet<tidebreak_core::WorkspaceId>>,
}

impl CodeRuntime {
    pub(crate) fn new(db: Arc<DbStore>, data_dir: PathBuf) -> Self {
        Self {
            db,
            bus: Arc::new(CodeEventBus::default()),
            adapters: builtin_registry(),
            data_dir,
            approvals: ApprovalBridge::new(),
            host: HostEnv::from_process(),
            loopback_base: Mutex::new(None),
            workers: Mutex::new(HashMap::new()),
            pr_cache: PrDigestCache::default(),
            clone_jobs: CloneJobs::default(),
            #[cfg(test)]
            gh_search_path: Mutex::new(None),
            stall_sweep: Mutex::new(None),
            stall_started: AtomicBool::new(false),
            titling_in_flight: Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// Bound after listen so Claude can be pointed at the loopback MCP route.
    pub(crate) fn set_loopback_base(&self, base: String) {
        *self.loopback_base.lock().expect("loopback base") =
            Some(base.trim_end_matches('/').into());
    }

    #[cfg(any(test, feature = "scripted-harness"))]
    pub(crate) fn with_registry(
        db: Arc<DbStore>,
        data_dir: PathBuf,
        adapters: AdapterRegistry,
    ) -> Self {
        Self {
            db,
            bus: Arc::new(CodeEventBus::default()),
            adapters,
            data_dir,
            approvals: ApprovalBridge::new(),
            host: HostEnv::from_process(),
            loopback_base: Mutex::new(None),
            workers: Mutex::new(HashMap::new()),
            pr_cache: PrDigestCache::default(),
            clone_jobs: CloneJobs::default(),
            #[cfg(test)]
            gh_search_path: Mutex::new(None),
            stall_sweep: Mutex::new(None),
            stall_started: AtomicBool::new(false),
            titling_in_flight: Mutex::new(std::collections::HashSet::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_gh_search_path(&self, path: Option<String>) {
        *self.gh_search_path.lock().expect("gh search path") = path;
    }

    pub(crate) async fn recover(&self) -> Result<Vec<RecoveryAction>, ServerError> {
        self.ensure_stall_sweep();
        let actions = recovery::recover_running_sessions(&self.db, &self.bus)
            .await
            .map_err(ServerError::from)?;
        if let Err(error) = self.sweep_orphaned_checkpoints().await {
            tracing::warn!(
                "code-mode: checkpoint ref sweep failed: {}",
                error.message()
            );
        }
        // Recovery only mutates rows. Re-attach a worker for every session
        // that is still usable so submit_turn is not stuck after a restart.
        for session in list_sessions(&self.db).await? {
            if matches!(
                session.lifecycle,
                CodeSessionLifecycle::Ended | CodeSessionLifecycle::Fenced
            ) {
                continue;
            }
            if self
                .workers
                .lock()
                .expect("code workers")
                .contains_key(&session.id)
            {
                continue;
            }
            if let Err(error) = self.attach_and_spawn_worker(session).await {
                tracing::warn!(
                    "code-mode: could not resume a recovered session worker: {}",
                    error.message()
                );
            }
        }
        Ok(actions)
    }

    pub(crate) fn adapter(
        &self,
        kind: HarnessKind,
    ) -> Result<Arc<dyn HarnessAdapter>, ServerError> {
        self.adapters.get(kind).ok_or_else(|| {
            ServerError::bad_request_kind(
                "harness_unavailable",
                format!("no adapter is registered for {kind}"),
            )
        })
    }

    pub(crate) async fn register_repo(
        &self,
        root_path: PathBuf,
        display_name: Option<String>,
        default_base_ref: Option<String>,
        branch_prefix: Option<String>,
        setup_script: Option<String>,
        archive_script: Option<String>,
    ) -> Result<CodeRepo, ServerError> {
        let validated = validate_repo_path(&root_path).await.map_err(map_worktree)?;
        let toplevel = validated.toplevel.display().to_string();
        if let Some(existing) = get_repo_by_root_path(&self.db, &toplevel).await? {
            return Err(ServerError::conflict_kind(
                "repo_already_registered",
                format!(
                    "repository {} is already registered as {}",
                    toplevel, existing.id
                ),
            ));
        }
        // Nested registrations of the same toplevel are already collapsed by
        // canonicalize + unique root_path. A path inside another registered
        // repo would resolve to the same toplevel.
        let name = display_name
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                validated
                    .toplevel
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "repo".into())
            });
        let repo = CodeRepo {
            id: RepoId::new(),
            root_path: toplevel,
            display_name: name,
            default_base_ref: default_base_ref
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "main".into()),
            branch_prefix: branch_prefix
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "tidebreak/".into()),
            setup_script,
            archive_script,
            quick_actions: Vec::new(),
            created_at: Utc::now(),
        };
        insert_repo(&self.db, &repo).await?;
        Ok(repo)
    }

    pub(crate) async fn list_repos(&self) -> Result<Vec<CodeRepo>, ServerError> {
        Ok(list_repos(&self.db).await?)
    }

    pub(crate) async fn get_repo(&self, id: RepoId) -> Result<CodeRepo, ServerError> {
        get_repo(&self.db, id)
            .await?
            .ok_or_else(|| ServerError::not_found(format!("repo {id} not found")))
    }

    pub(crate) async fn save_repo(&self, repo: &CodeRepo) -> Result<(), ServerError> {
        if !save_repo(&self.db, repo).await? {
            return Err(ServerError::not_found(format!(
                "repo {} not found",
                repo.id
            )));
        }
        Ok(())
    }

    pub(crate) async fn delete_repo(&self, id: RepoId) -> Result<(), ServerError> {
        let workspaces = list_workspaces(&self.db, Some(id)).await?;
        if workspaces
            .iter()
            .any(|workspace| workspace.status != CodeWorkspaceStatus::Archived)
        {
            return Err(ServerError::conflict_kind(
                "repo_has_workspaces",
                "archive every workspace before deleting the repository",
            ));
        }
        if !delete_repo(&self.db, id).await? {
            return Err(ServerError::not_found(format!("repo {id} not found")));
        }
        Ok(())
    }

    pub(crate) async fn create_workspace(
        &self,
        repo_id: RepoId,
        title: Option<String>,
        base_ref: Option<String>,
    ) -> Result<CodeWorkspace, ServerError> {
        let repo = self.get_repo(repo_id).await?;
        let title = title
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        let id = WorkspaceId::new();
        let branch = branch_name(&repo.branch_prefix, &title, id.0.as_u128());
        let existing = list_workspaces(&self.db, Some(repo_id)).await?;
        if existing
            .iter()
            .any(|workspace| workspace.branch_name == branch)
        {
            return Err(ServerError::conflict_kind(
                "branch_collision",
                format!("branch {branch} already exists on this repository"),
            ));
        }
        let repo_slug = {
            let from_name = slugify(&repo.display_name);
            if from_name.is_empty() {
                slugify(&repo.root_path)
            } else {
                from_name
            }
        };
        let workspace_slug = {
            let from_title = slugify(&title);
            if from_title.is_empty() {
                worktree::two_word_name(id.0.as_u128())
            } else {
                from_title
            }
        };
        let path = worktree_dir(&self.data_dir, repo_id, id, &repo_slug, &workspace_slug);
        let display_title = if title.is_empty() {
            workspace_slug.clone()
        } else {
            title
        };
        let base = base_ref
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| repo.default_base_ref.clone());
        let mut workspace = CodeWorkspace {
            id,
            repo_id,
            title: display_title,
            worktree_path: path.display().to_string(),
            branch_name: branch.clone(),
            base_ref: base.clone(),
            status: CodeWorkspaceStatus::Creating,
            pr: None,
            created_at: Utc::now(),
            archived_at: None,
        };
        insert_workspace(&self.db, &workspace).await?;
        match create_worktree(std::path::Path::new(&repo.root_path), &path, &branch, &base).await {
            Ok(()) => {}
            Err(err) => {
                let _ = delete_workspace(&self.db, id).await;
                return Err(map_worktree(err));
            }
        }
        match run_setup_script(&path, repo.setup_script.as_deref()).await {
            Ok(()) => {
                workspace.status = CodeWorkspaceStatus::Active;
                save_workspace(&self.db, &workspace).await?;
                gh::run_auto_create_actions(&path, &repo.quick_actions).await;
                Ok(workspace)
            }
            Err(err) => {
                workspace.status = CodeWorkspaceStatus::SetupFailed;
                save_workspace(&self.db, &workspace).await?;
                Err(ServerError::unprocessable_kind(
                    "setup_failed",
                    err.to_string(),
                ))
            }
        }
    }

    pub(crate) async fn list_workspaces(
        &self,
        repo_id: Option<RepoId>,
    ) -> Result<Vec<CodeWorkspace>, ServerError> {
        Ok(list_workspaces(&self.db, repo_id).await?)
    }

    pub(crate) async fn get_workspace(
        &self,
        id: WorkspaceId,
    ) -> Result<CodeWorkspace, ServerError> {
        get_workspace(&self.db, id)
            .await?
            .ok_or_else(|| ServerError::not_found(format!("workspace {id} not found")))
    }

    pub(crate) async fn save_workspace(
        &self,
        workspace: &CodeWorkspace,
    ) -> Result<(), ServerError> {
        if !save_workspace(&self.db, workspace).await? {
            return Err(ServerError::not_found(format!(
                "workspace {} not found",
                workspace.id
            )));
        }
        super::attention::emit_workspace_digests(&self.db, &self.bus, workspace.id).await;
        Ok(())
    }

    pub(crate) async fn archive_workspace(
        &self,
        id: WorkspaceId,
        force: bool,
    ) -> Result<CodeWorkspace, ServerError> {
        let mut workspace = self.get_workspace(id).await?;
        if workspace.status == CodeWorkspaceStatus::Archived {
            return Ok(workspace);
        }
        let repo = self.get_repo(workspace.repo_id).await?;
        if let Some(script) = repo.archive_script.as_deref() {
            if let Err(err) = super::setup_script::run_workspace_script(
                std::path::Path::new(&workspace.worktree_path),
                script,
            )
            .await
            {
                return Err(ServerError::unprocessable_kind(
                    "archive_script_failed",
                    err,
                ));
            }
        }
        self.refuse_running_sessions(id, force).await?;
        let path = std::path::Path::new(&workspace.worktree_path);
        if path.exists() {
            if let Some(block) = archive_blockers(path, &workspace.base_ref)
                .await
                .map_err(map_worktree)?
            {
                if !force {
                    return Err(ServerError::conflict_kind(
                        block.as_str(),
                        "workspace has uncommitted or unpushed work; pass force to discard it",
                    ));
                }
            }
        }
        self.end_workspace_sessions(id).await?;
        remove_worktree(std::path::Path::new(&repo.root_path), path)
            .await
            .map_err(map_worktree)?;
        let _ = prune_worktrees(std::path::Path::new(&repo.root_path)).await;
        if let Err(error) =
            delete_workspace_refs(std::path::Path::new(&repo.root_path), workspace.id).await
        {
            tracing::warn!(
                workspace = %workspace.id,
                "code-mode: could not delete checkpoint refs on archive: {error}"
            );
        }
        if let Err(error) = self
            .sweep_repo_checkpoint_refs(std::path::Path::new(&repo.root_path), repo.id)
            .await
        {
            tracing::warn!(
                "code-mode: checkpoint ref sweep failed: {}",
                error.message()
            );
        }
        workspace.status = CodeWorkspaceStatus::Archived;
        workspace.archived_at = Some(Utc::now());
        save_workspace(&self.db, &workspace).await?;
        Ok(workspace)
    }

    pub(crate) async fn commit_workspace(
        &self,
        id: WorkspaceId,
        message: Option<String>,
    ) -> Result<CommitOutcome, ServerError> {
        let workspace = self.require_live_workspace(id).await?;
        gh::commit_all(
            std::path::Path::new(&workspace.worktree_path),
            &workspace.title,
            message.as_deref(),
        )
        .await
        .map_err(map_gh)
    }

    pub(crate) async fn push_workspace(&self, id: WorkspaceId) -> Result<PushOutcome, ServerError> {
        let workspace = self.require_live_workspace(id).await?;
        gh::push_branch(
            std::path::Path::new(&workspace.worktree_path),
            &workspace.branch_name,
        )
        .await
        .map_err(map_gh)
    }

    pub(crate) async fn workspace_pr(
        &self,
        id: WorkspaceId,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        let mut workspace = self.get_workspace(id).await?;
        let gh_path = self.gh_search_path_owned();
        let status = gh::workspace_git_status(
            std::path::Path::new(&workspace.worktree_path),
            workspace.id,
            &workspace.title,
            &workspace.branch_name,
            &workspace.base_ref,
            workspace.pr.clone(),
            &self.pr_cache,
            gh_path.as_deref(),
        )
        .await
        .map_err(map_gh)?;
        if status.pr != workspace.pr {
            workspace.pr = status.pr.clone();
            self.save_workspace(&workspace).await?;
        }
        Ok(status)
    }

    pub(crate) async fn create_workspace_pr(
        &self,
        id: WorkspaceId,
        title: Option<String>,
        body: Option<String>,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        let mut workspace = self.require_live_workspace(id).await?;
        let gh_path = self.gh_search_path_owned();
        let digest = gh::create_pull_request(
            std::path::Path::new(&workspace.worktree_path),
            workspace.id,
            &workspace.title,
            &workspace.branch_name,
            &workspace.base_ref,
            title.as_deref(),
            body.as_deref(),
            &self.pr_cache,
            gh_path.as_deref(),
        )
        .await
        .map_err(map_gh)?;
        workspace.pr = Some(digest);
        self.save_workspace(&workspace).await?;
        self.workspace_pr(id).await
    }

    pub(crate) async fn run_workspace_action(
        &self,
        id: WorkspaceId,
        name: &str,
    ) -> Result<ActionOutcome, ServerError> {
        let workspace = self.require_live_workspace(id).await?;
        let repo = self.get_repo(workspace.repo_id).await?;
        gh::run_named_action(
            std::path::Path::new(&workspace.worktree_path),
            &repo.quick_actions,
            name,
        )
        .await
        .map_err(map_gh)
    }

    async fn require_live_workspace(&self, id: WorkspaceId) -> Result<CodeWorkspace, ServerError> {
        let workspace = self.get_workspace(id).await?;
        if workspace.status == CodeWorkspaceStatus::Archived {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                "workspace is archived",
            ));
        }
        if !std::path::Path::new(&workspace.worktree_path).exists() {
            return Err(ServerError::not_found("workspace worktree is gone"));
        }
        Ok(workspace)
    }

    fn gh_search_path_owned(&self) -> Option<String> {
        #[cfg(test)]
        {
            return self.gh_search_path.lock().expect("gh search path").clone();
        }
        #[cfg(not(test))]
        None
    }

    pub(crate) async fn create_session(
        &self,
        workspace_id: WorkspaceId,
        harness: HarnessKind,
        permission_mode: CodePermissionMode,
    ) -> Result<CodeSession, ServerError> {
        let workspace = self.get_workspace(workspace_id).await?;
        if workspace.status != CodeWorkspaceStatus::Active {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                format!("workspace is {}", workspace.status.as_str()),
            ));
        }
        let existing = list_sessions_for_workspace(&self.db, workspace_id).await?;
        if existing
            .iter()
            .any(|session| session.lifecycle != CodeSessionLifecycle::Ended)
        {
            return Err(ServerError::conflict_kind(
                "session_exists",
                "this workspace already has an active session",
            ));
        }
        let adapter = self.adapter(harness)?;
        let probe = adapter.probe(&self.host).await;
        if !probe.found {
            return Err(ServerError::unprocessable_kind(
                "harness_not_found",
                format!(
                    "{harness} is not installed{}",
                    if probe.stderr.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", probe.stderr)
                    }
                ),
            ));
        }
        let caps = adapter.capabilities(&probe);
        refuse_unhonored_mode(harness, permission_mode, &caps)?;
        if probe.binary_path.is_none() {
            return Err(ServerError::unprocessable_kind(
                "harness_not_found",
                format!("{harness} has no path"),
            ));
        }
        let session = CodeSession {
            id: CodeSessionId::new(),
            workspace_id,
            harness_kind: harness,
            harness_version: probe.version.clone(),
            harness_resume_ref: None,
            permission_mode,
            lifecycle: CodeSessionLifecycle::Created,
            fence_reason: None,
            child_pid: None,
            spawn_epoch: 0,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            created_at: Utc::now(),
        };
        insert_session(&self.db, &session).await?;
        self.attach_and_spawn_worker(session).await
    }

    pub(crate) async fn get_session(&self, id: CodeSessionId) -> Result<CodeSession, ServerError> {
        get_session(&self.db, id)
            .await?
            .ok_or_else(|| ServerError::not_found(format!("session {id} not found")))
    }

    pub(crate) async fn submit_turn(
        &self,
        id: CodeSessionId,
        message: String,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        let session = self.get_session(id).await?;
        if session.lifecycle == CodeSessionLifecycle::Fenced {
            return Err(ServerError::conflict_kind(
                "session_fenced",
                "session is fenced until it is reaped",
            ));
        }
        if session.lifecycle == CodeSessionLifecycle::Ended {
            return Err(ServerError::conflict_kind(
                "session_ended",
                "session has ended",
            ));
        }
        let handle = self.require_worker(id)?;
        // Queue-default (0009): a send while a turn is in flight parks one
        // follow-up. This does not consult mid_turn_steering — that cap
        // gates the separate /steer route only.
        let in_flight = session.lifecycle == CodeSessionLifecycle::Running
            || get_open_turn(&self.db, id).await?.is_some();
        if in_flight {
            if !queue_follow_up(&handle, message) {
                return Err(ServerError::conflict_kind(
                    "queue_full",
                    "a follow-up is already queued on this session",
                ));
            }
            return Ok(SubmitTurnOutcome::Queued);
        }
        let (reply, rx) = oneshot::channel();
        handle
            .commands
            .send(WorkerCommand::RunTurn { message, reply })
            .await
            .map_err(|_| ServerError::internal("session worker is gone"))?;
        let turn = rx
            .await
            .map_err(|_| ServerError::internal("session worker dropped the turn"))?
            .map_err(map_worker)?;
        Ok(SubmitTurnOutcome::Ran(Box::new(turn)))
    }

    pub(crate) async fn interrupt(&self, id: CodeSessionId) -> Result<(), ServerError> {
        let handle = self.require_worker(id)?;
        let (reply, rx) = oneshot::channel();
        handle
            .commands
            .send(WorkerCommand::Interrupt { reply })
            .await
            .map_err(|_| ServerError::internal("session worker is gone"))?;
        rx.await
            .map_err(|_| ServerError::internal("session worker dropped the interrupt"))?
            .map_err(map_worker)
    }

    pub(crate) async fn reap(&self, id: CodeSessionId) -> Result<CodeSession, ServerError> {
        let session = self.get_session(id).await?;
        if session.lifecycle != CodeSessionLifecycle::Fenced {
            return Err(ServerError::conflict_kind(
                "not_fenced",
                "only a fenced session can be reaped",
            ));
        }
        let handle = self.workers.lock().expect("code workers").remove(&id);
        if let Some(handle) = handle {
            let _ = handle.commands.send(WorkerCommand::Shutdown).await;
        }
        let session = recovery::reap_session(&self.db, &self.bus, session)
            .await
            .map_err(ServerError::from)?;
        self.attach_and_spawn_worker(session).await
    }

    pub(crate) async fn set_attention(
        &self,
        id: CodeSessionId,
        clear: bool,
        note: Option<String>,
    ) -> Result<CodeSession, ServerError> {
        let _ = self.get_session(id).await?;
        super::attention::user_set_attention(&self.db, &self.bus, id, clear, note)
            .await
            .map_err(ServerError::from)
    }

    pub(crate) async fn mark_session_viewed(&self, id: CodeSessionId) -> Result<(), ServerError> {
        super::attention::mark_viewed(&self.db, &self.bus, id)
            .await
            .map_err(ServerError::from)
    }

    fn ensure_stall_sweep(&self) {
        if self.stall_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let guard = super::attention::StallSweepGuard::spawn(self.db.clone(), self.bus.clone());
        *self.stall_sweep.lock().expect("stall sweep") = Some(guard);
    }

    pub(crate) async fn list_sessions(&self) -> Result<Vec<CodeSession>, ServerError> {
        Ok(list_sessions(&self.db).await?)
    }

    pub(crate) async fn list_workspace_sessions(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<CodeSession>, ServerError> {
        let _ = self.get_workspace(workspace_id).await?;
        Ok(list_sessions_for_workspace(&self.db, workspace_id).await?)
    }

    pub(crate) async fn list_session_turns(
        &self,
        session_id: CodeSessionId,
    ) -> Result<Vec<CodeTurn>, ServerError> {
        let _ = self.get_session(session_id).await?;
        Ok(list_turns(&self.db, session_id).await?)
    }

    pub(crate) async fn workspace_files(
        &self,
        workspace_id: WorkspaceId,
        turn_id: Option<CodeTurnId>,
    ) -> Result<(Vec<ChangedFile>, bool, Diffstat, Option<CodeTurnId>), ServerError> {
        let workspace = self.get_workspace(workspace_id).await?;
        let (worktree, from, to, turn) = resolve_diff_range(&self.db, &workspace, turn_id)
            .await
            .map_err(map_checkpoint)?;
        let listed = list_changed_files(&worktree, &from, &to, DiffBounds::default())
            .await
            .map_err(map_checkpoint)?;
        Ok((listed.files, listed.truncated, listed.stat, turn))
    }

    pub(crate) async fn workspace_diff(
        &self,
        workspace_id: WorkspaceId,
        turn_id: Option<CodeTurnId>,
        file: Option<&str>,
    ) -> Result<(String, bool, Diffstat, Option<CodeTurnId>), ServerError> {
        let workspace = self.get_workspace(workspace_id).await?;
        let (worktree, from, to, turn) = resolve_diff_range(&self.db, &workspace, turn_id)
            .await
            .map_err(map_checkpoint)?;
        let produced = produce_diff(&worktree, &from, &to, file, DiffBounds::default())
            .await
            .map_err(map_checkpoint)?;
        Ok((produced.diff, produced.truncated, produced.stat, turn))
    }

    async fn sweep_orphaned_checkpoints(&self) -> Result<(), ServerError> {
        for repo in list_repos(&self.db).await? {
            self.sweep_repo_checkpoint_refs(std::path::Path::new(&repo.root_path), repo.id)
                .await?;
        }
        Ok(())
    }

    async fn sweep_repo_checkpoint_refs(
        &self,
        repo_root: &std::path::Path,
        repo_id: RepoId,
    ) -> Result<(), ServerError> {
        let live: Vec<WorkspaceId> = list_workspaces(&self.db, Some(repo_id))
            .await?
            .into_iter()
            .filter(|workspace| workspace.status != CodeWorkspaceStatus::Archived)
            .map(|workspace| workspace.id)
            .collect();
        sweep_orphaned_refs(repo_root, &live)
            .await
            .map(|_| ())
            .map_err(map_checkpoint)
    }

    async fn refuse_running_sessions(
        &self,
        workspace_id: WorkspaceId,
        allow_running: bool,
    ) -> Result<(), ServerError> {
        if allow_running {
            return Ok(());
        }
        let sessions = list_sessions_for_workspace(&self.db, workspace_id).await?;
        if sessions
            .iter()
            .any(|session| session.lifecycle == CodeSessionLifecycle::Running)
        {
            return Err(ServerError::conflict_kind(
                "session_running",
                "a session is still running in this workspace; pass force to end it",
            ));
        }
        Ok(())
    }

    async fn end_workspace_sessions(&self, workspace_id: WorkspaceId) -> Result<(), ServerError> {
        let sessions = list_sessions_for_workspace(&self.db, workspace_id).await?;
        for mut session in sessions {
            if session.lifecycle == CodeSessionLifecycle::Ended {
                continue;
            }
            let handle = self
                .workers
                .lock()
                .expect("code workers")
                .remove(&session.id);
            if let Some(handle) = handle {
                let _ = handle.commands.send(WorkerCommand::Shutdown).await;
            }
            session.lifecycle = CodeSessionLifecycle::Ended;
            session.child_pid = None;
            session.fence_reason = None;
            super::attention::persist_session(&self.db, &self.bus, &session).await?;
        }
        Ok(())
    }

    async fn attach_and_spawn_worker(
        &self,
        session: CodeSession,
    ) -> Result<CodeSession, ServerError> {
        let workspace = self.get_workspace(session.workspace_id).await?;
        let adapter = self.adapter(session.harness_kind)?;
        let probe = adapter.probe(&self.host).await;
        if !probe.found {
            return Err(ServerError::unprocessable_kind(
                "harness_not_found",
                format!("{} is not installed", session.harness_kind),
            ));
        }
        let binary = probe.binary_path.clone().ok_or_else(|| {
            ServerError::unprocessable_kind(
                "harness_not_found",
                format!("{} has no path", session.harness_kind),
            )
        })?;
        let attached = attach_engine(
            &self.db,
            &self.bus,
            session.id,
            session.harness_kind,
            probe.version.clone().or(session.harness_version.clone()),
            None,
        )
        .await
        .map_err(map_worker)?;
        let sink = super::session_worker::sink_for(
            self.db.clone(),
            self.bus.clone(),
            session.id,
            attached.spawn_epoch,
            None,
        );
        let approval = self.approval_channel(session.id, session.permission_mode);
        let spec = SessionSpec {
            worktree: PathBuf::from(&workspace.worktree_path),
            permission_mode: session.permission_mode,
            resume_ref: session.harness_resume_ref.clone(),
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            env: probe.env.clone(),
            approval,
            binary,
            sink: sink.clone() as Arc<dyn HarnessEventSink>,
        };
        let engine = adapter.launch(spec).await.map_err(|err| {
            ServerError::internal(format!("failed to launch engine session: {err}"))
        })?;
        let mut attached = attached;
        attached.child_pid = engine.child_pid();
        if let Some(resume) = engine.resume_ref().or(session.harness_resume_ref.clone()) {
            attached.harness_resume_ref = Some(resume);
        }
        super::attention::persist_session(&self.db, &self.bus, &attached).await?;
        let handle = spawn_session_worker(
            self.db.clone(),
            self.bus.clone(),
            attached.clone(),
            engine,
            sink,
        );
        self.workers
            .lock()
            .expect("code workers")
            .insert(session.id, handle);
        let pending = list_approvals(
            &self.db,
            Some(CodeApprovalState::Pending),
            Some(attached.id),
        )
        .await?;
        if !pending.is_empty() {
            super::attention::replace_attention(
                &mut attached,
                Attention::needs_you("an approval is waiting", AttentionSource::Structured),
                false,
            );
            super::attention::persist_session(&self.db, &self.bus, &attached).await?;
        }
        Ok(attached)
    }

    fn require_worker(&self, id: CodeSessionId) -> Result<WorkerHandle, ServerError> {
        self.workers
            .lock()
            .expect("code workers")
            .get(&id)
            .map(|handle| WorkerHandle {
                spawn_epoch: handle.spawn_epoch,
                commands: handle.commands.clone(),
                pending: handle.pending.clone(),
                wake: handle.wake.clone(),
                sink: handle.sink.clone(),
            })
            .ok_or_else(|| {
                ServerError::conflict_kind(
                    "session_worker_missing",
                    "no live worker is attached to this session",
                )
            })
    }

    fn approval_channel(
        &self,
        session_id: CodeSessionId,
        mode: CodePermissionMode,
    ) -> Option<ApprovalChannelSpec> {
        if !matches!(mode, CodePermissionMode::Ask | CodePermissionMode::Auto) {
            return None;
        }
        let base = self.loopback_base.lock().expect("loopback base").clone()?;
        let token = self.approvals.issue_token(session_id);
        Some(ApprovalChannelSpec {
            mcp_endpoint_url: format!("{base}/code/mcp/approval-prompt"),
            token,
            completer: self.approvals.clone(),
        })
    }

    pub(crate) async fn ingest_harness_event(
        &self,
        session_id: CodeSessionId,
        event: HarnessEvent,
    ) -> Result<(), ServerError> {
        let handle = self.require_worker(session_id)?;
        handle.sink.emit(event).await;
        Ok(())
    }

    pub(crate) async fn list_approvals(
        &self,
        state: Option<CodeApprovalState>,
        session_id: Option<CodeSessionId>,
    ) -> Result<Vec<CodeApproval>, ServerError> {
        Ok(list_approvals(&self.db, state, session_id).await?)
    }

    pub(crate) async fn get_approval(
        &self,
        id: CodeApprovalId,
    ) -> Result<CodeApproval, ServerError> {
        get_approval(&self.db, id)
            .await?
            .ok_or_else(|| ServerError::not_found(format!("approval {id} not found")))
    }

    pub(crate) async fn decide_approval(
        &self,
        id: CodeApprovalId,
        decision: ApprovalDecision,
    ) -> Result<CodeApproval, ServerError> {
        let mut approval = self.get_approval(id).await?;
        if approval.state != CodeApprovalState::Pending {
            return Err(ServerError::conflict_kind(
                "approval_already_decided",
                format!("approval {id} is {}", approval.state.as_str()),
            ));
        }
        let call_id = approval
            .harness_raw
            .get("call_id")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_owned();
        if call_id.is_empty() {
            return Err(ServerError::internal(
                "approval row is missing a harness call_id",
            ));
        }
        let handle = self.require_worker(approval.session_id)?;
        let (reply, rx) = oneshot::channel();
        handle
            .commands
            .send(crate::code::session_worker::WorkerCommand::Decide {
                approval: tidebreak_harness::HarnessApprovalRef { call_id },
                decision: decision.clone(),
                reply,
            })
            .await
            .map_err(|_| ServerError::internal("session worker is gone"))?;
        rx.await
            .map_err(|_| ServerError::internal("session worker dropped the decision"))?
            .map_err(map_worker)?;
        let now = Utc::now();
        match &decision {
            ApprovalDecision::Approve => {
                approval.state = CodeApprovalState::Approved;
                approval.feedback = None;
            }
            ApprovalDecision::Deny { feedback } => {
                approval.state = CodeApprovalState::Denied;
                approval.feedback = feedback.clone();
            }
        }
        approval.decided_at = Some(now);
        if !save_approval(&self.db, &approval).await? {
            return Err(ServerError::not_found(format!("approval {id} not found")));
        }
        let session = self.get_session(approval.session_id).await?;
        journal_event(
            &self.db,
            &self.bus,
            approval.session_id,
            session.spawn_epoch,
            CodeEvent::ApprovalResolved {
                approval_id: approval.id,
                decision: match &decision {
                    ApprovalDecision::Approve => ApprovalDecisionKind::Approve,
                    ApprovalDecision::Deny { feedback } => ApprovalDecisionKind::Deny {
                        feedback: feedback.clone(),
                    },
                },
            },
        )
        .await
        .map_err(|err| ServerError::internal(err.to_string()))?;
        let still_pending = list_approvals(
            &self.db,
            Some(CodeApprovalState::Pending),
            Some(approval.session_id),
        )
        .await?;
        if still_pending.is_empty() {
            let _ = super::attention::apply_attention(
                &self.db,
                &self.bus,
                approval.session_id,
                Attention::working(AttentionSource::Lifecycle),
                false,
            )
            .await;
        }
        Ok(approval)
    }
}

fn refuse_unhonored_mode(
    harness: HarnessKind,
    mode: CodePermissionMode,
    caps: &tidebreak_core::HarnessCaps,
) -> Result<(), ServerError> {
    // Each mode stands on its own capability flag (decision 0038): Auto is
    // never derived from the approval channel, so an engine whose only
    // honest posture is unsupervised Auto can still be driven.
    let ok = match mode {
        CodePermissionMode::Plan => caps.plan_mode == CapLevel::Supported,
        CodePermissionMode::Ask => caps.structured_approvals == CapLevel::Supported,
        CodePermissionMode::Auto => caps.auto_mode == CapLevel::Supported,
    };
    if ok {
        return Ok(());
    }
    let reason = match mode {
        CodePermissionMode::Plan => format!("{harness} cannot honor plan mode"),
        CodePermissionMode::Ask => format!(
            "{harness} cannot honor {mode}: structured approvals are {}",
            caps.structured_approvals.as_str()
        ),
        CodePermissionMode::Auto => format!(
            "{harness} cannot honor {mode}: an auto posture is {}",
            caps.auto_mode.as_str()
        ),
    };
    Err(ServerError::unprocessable_kind(
        "permission_mode_unavailable",
        reason,
    ))
}

fn map_checkpoint(err: CheckpointError) -> ServerError {
    match err {
        CheckpointError::User(message) => {
            if message.contains("not found") || message.contains("gone") {
                ServerError::not_found(message)
            } else if message.contains("no checkpoint") {
                ServerError::conflict_kind("no_checkpoint", message)
            } else {
                ServerError::bad_request_kind("checkpoint", message)
            }
        }
        CheckpointError::Internal(message) => ServerError::internal(message),
    }
}

fn map_gh(err: GhError) -> ServerError {
    match err {
        GhError::NothingToCommit => ServerError::conflict_kind(
            "nothing_to_commit",
            "there is nothing to commit in this workspace",
        ),
        GhError::AuthFailed(message) => ServerError::conflict_kind("git_auth_failed", message),
        GhError::PushFailed(message) => ServerError::conflict_kind("git_push_failed", message),
        GhError::GhAbsent { instructions } => ServerError::conflict_kind("gh_absent", instructions),
        GhError::GhSignedOut { instructions } => {
            ServerError::conflict_kind("gh_signed_out", instructions)
        }
        GhError::User(message) => {
            if message.contains("no quick action") {
                ServerError::not_found(message)
            } else {
                ServerError::bad_request_kind("git", message)
            }
        }
        GhError::Internal(message) => ServerError::internal(message),
    }
}

fn map_worktree(err: WorktreeError) -> ServerError {
    match err {
        WorktreeError::User(message) => {
            if message.contains("already exists") {
                ServerError::conflict_kind("branch_collision", message)
            } else if message.contains("bare") {
                ServerError::bad_request_kind("bare_repo", message)
            } else if message.contains("not a git repository") {
                ServerError::bad_request_kind("not_a_repo", message)
            } else {
                ServerError::bad_request_kind("worktree", message)
            }
        }
        WorktreeError::Internal(message) => ServerError::internal(message),
    }
}

fn map_worker(err: WorkerError) -> ServerError {
    match err {
        WorkerError::Conflict(message) => ServerError::conflict_kind("conflict", message),
        WorkerError::Failed(message) => ServerError::internal(message),
    }
}

impl From<WorktreeError> for ServerError {
    fn from(err: WorktreeError) -> Self {
        map_worktree(err)
    }
}
