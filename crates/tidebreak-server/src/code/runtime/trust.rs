//! Repository trust: the decision, the engine config a checkout carries, and
//! moving live workers onto a changed decision.

use super::*;

use crate::code::repo_trust::{project_config_for, read_repo_trust, write_repo_trust};
use crate::code::types::{
    CodeProjectConfigEffect, CodeProjectConfigEffectKind, CodeProjectConfigFile,
    CodeRepoTrustSnapshot,
};
use tidebreak_harness::project_config::{scan_project_config, ProjectConfigEffect};
use tidebreak_harness::ProjectConfig;

/// How often a deferred trust resync re-reads a running session.
const DEFERRED_TRUST_POLL: Duration = Duration::from_secs(2);
/// How long a deferred trust resync waits for a turn to end.
const DEFERRED_TRUST_DEADLINE: Duration = Duration::from_secs(2 * 60 * 60);

impl CodeRuntime {
    /// The trust decision for a repository and the engine config its main
    /// checkout carries.
    pub async fn repo_trust(
        &self,
        owner: &OwnerId,
        id: RepoId,
    ) -> Result<CodeRepoTrustSnapshot, ServerError> {
        let repo = self.get_repo(owner, id).await?;
        let root = PathBuf::from(&repo.root_path);
        Ok(CodeRepoTrustSnapshot {
            repo_id: repo.id,
            trust: read_repo_trust(&root).await,
            files: scan_checkout(root, None).await,
        })
    }

    /// The trust decision for a workspace's repository and the engine config
    /// its worktree carries: what a session in that workspace would load.
    pub async fn workspace_trust(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<CodeRepoTrustSnapshot, ServerError> {
        let workspace = self.get_workspace(owner, id).await?;
        if workspace.is_remote() {
            return Err(ServerError::conflict_kind(
                "workspace_remote",
                "a sandbox workspace keeps its checkout in the sandbox, not on this machine",
            ));
        }
        let repo = self.get_repo(owner, workspace.repo_id).await?;
        let root = PathBuf::from(&repo.root_path);
        Ok(CodeRepoTrustSnapshot {
            repo_id: repo.id,
            trust: read_repo_trust(&root).await,
            files: scan_checkout(PathBuf::from(&workspace.worktree_path), Some(root)).await,
        })
    }

    /// Record a trust decision for a repository, then move the live workers
    /// of its sessions onto it.
    pub async fn set_repo_trust(
        self: &Arc<Self>,
        owner: &OwnerId,
        id: RepoId,
        trusted: bool,
    ) -> Result<CodeRepoTrustSnapshot, ServerError> {
        let repo = self.get_repo(owner, id).await?;
        write_repo_trust(Path::new(&repo.root_path), trusted)
            .await
            .map_err(|message| ServerError::conflict_kind("repo_trust_not_saved", message))?;
        self.resync_workers_to_repo_trust(id).await;
        self.repo_trust(owner, id).await
    }

    /// What an engine launched in this workspace may load: the repository's
    /// own engine config only when the user trusts the repository.
    pub(super) async fn workspace_project_config(
        &self,
        workspace: &CodeWorkspace,
    ) -> ProjectConfig {
        match get_repo(&self.db, &workspace.owner, workspace.repo_id).await {
            Ok(Some(repo)) => project_config_for(read_repo_trust(Path::new(&repo.root_path)).await),
            Ok(None) => ProjectConfig::Skip,
            Err(error) => {
                tracing::warn!(
                    workspace = %workspace.id,
                    %error,
                    "could not read the workspace's repository; its engine config stays off"
                );
                ProjectConfig::Skip
            }
        }
    }

    /// Move every live worker of `repo_id`'s sessions onto the repository's
    /// current trust decision.
    ///
    /// A worker composes its engine launch once, when it attaches, so a
    /// decision recorded later reaches a session only through a fresh worker.
    /// An idle worker is stopped and attached again at once. A worker with a
    /// turn in flight finishes that turn first, and a watcher moves it once
    /// the turn ends. Returns the sessions that moved on this pass.
    pub(super) async fn resync_workers_to_repo_trust(
        self: &Arc<Self>,
        repo_id: RepoId,
    ) -> Vec<SessionId> {
        let live: Vec<(SessionId, i64, OwnerId, ProjectConfig)> = self
            .workers
            .lock()
            .expect("code workers")
            .iter()
            .map(|(id, handle)| {
                (
                    *id,
                    handle.spawn_epoch,
                    handle.sink.owner().clone(),
                    handle.project_config,
                )
            })
            .collect();
        let mut moved = Vec::new();
        for (id, spawn_epoch, owner, launched_with) in live {
            let _recovery_guard = self.session_recovery_lock(id).lock_owned().await;
            let Ok(session) = self.get_session(&owner, id).await else {
                continue;
            };
            if session.spawn_epoch != spawn_epoch
                || matches!(
                    session.lifecycle,
                    SessionLifecycle::Fenced | SessionLifecycle::Ended
                )
            {
                continue;
            }
            let Ok(Some(workspace)) = self.session_workspace(&session).await else {
                continue;
            };
            if workspace.repo_id != repo_id
                || self.workspace_project_config(&workspace).await == launched_with
            {
                continue;
            }
            if session.lifecycle == SessionLifecycle::Running {
                self.defer_trust_resync(repo_id, id, owner);
                continue;
            }
            let Some(handle) = self.take_worker_for_epoch(id, spawn_epoch) else {
                continue;
            };
            self.revoke_worker_channels(id);
            if !Self::shut_down_worker(id, handle.clone()).await {
                self.workers
                    .lock()
                    .expect("code workers")
                    .insert(id, handle);
                continue;
            }
            let respawned = match self.get_session(&owner, id).await {
                Ok(session) => self.attach_and_spawn_worker(session).await,
                Err(error) => Err(error),
            };
            match respawned {
                Ok(_) => moved.push(id),
                Err(error) => tracing::warn!(
                    session = %id,
                    error = ?error,
                    "could not restart the session under the repository's trust decision"
                ),
            }
        }
        moved
    }

    /// Finish a trust resync once the session's turn in flight ends. One
    /// watcher per session: a second decision while one is pending changes
    /// what the resync reads, not whether it runs.
    fn defer_trust_resync(self: &Arc<Self>, repo_id: RepoId, id: SessionId, owner: OwnerId) {
        if !self
            .deferred_trust_resyncs
            .lock()
            .expect("deferred trust resyncs")
            .insert(id)
        {
            return;
        }
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            let deadline = TokioInstant::now() + DEFERRED_TRUST_DEADLINE;
            let settled = loop {
                tokio::time::sleep(DEFERRED_TRUST_POLL).await;
                match runtime.get_session(&owner, id).await {
                    Ok(session) if session.lifecycle != SessionLifecycle::Running => break true,
                    Ok(_) if TokioInstant::now() < deadline => continue,
                    _ => break false,
                }
            };
            runtime
                .deferred_trust_resyncs
                .lock()
                .expect("deferred trust resyncs")
                .remove(&id);
            if settled {
                runtime.resync_workers_to_repo_trust(repo_id).await;
            } else {
                tracing::warn!(
                    session = %id,
                    "gave up restarting the session under the repository's trust decision"
                );
            }
        });
    }
}

/// The engine config a checkout carries, read off the async runtime.
/// `main_checkout` is the repository's main checkout when `root` is a linked
/// worktree of it.
async fn scan_checkout(
    root: PathBuf,
    main_checkout: Option<PathBuf>,
) -> Vec<CodeProjectConfigFile> {
    let files =
        tokio::task::spawn_blocking(move || scan_project_config(&root, main_checkout.as_deref()))
            .await
            .unwrap_or_default();
    files
        .into_iter()
        .map(|file| CodeProjectConfigFile {
            path: file.path,
            engines: file.engines,
            effects: file
                .effects
                .into_iter()
                .map(|(effect, count)| CodeProjectConfigEffect {
                    kind: effect_kind(effect),
                    count,
                })
                .collect(),
        })
        .collect()
}

fn effect_kind(effect: ProjectConfigEffect) -> CodeProjectConfigEffectKind {
    match effect {
        ProjectConfigEffect::Hooks => CodeProjectConfigEffectKind::Hooks,
        ProjectConfigEffect::McpServers => CodeProjectConfigEffectKind::McpServers,
        ProjectConfigEffect::Plugins => CodeProjectConfigEffectKind::Plugins,
        ProjectConfigEffect::Packages => CodeProjectConfigEffectKind::Packages,
        ProjectConfigEffect::EnvironmentVariables => {
            CodeProjectConfigEffectKind::EnvironmentVariables
        }
        ProjectConfigEffect::PermissionRules => CodeProjectConfigEffectKind::PermissionRules,
        ProjectConfigEffect::HelperCommands => CodeProjectConfigEffectKind::HelperCommands,
        ProjectConfigEffect::CustomTools => CodeProjectConfigEffectKind::CustomTools,
        ProjectConfigEffect::Workflows => CodeProjectConfigEffectKind::Workflows,
        ProjectConfigEffect::ScheduledTasks => CodeProjectConfigEffectKind::ScheduledTasks,
        ProjectConfigEffect::Agents => CodeProjectConfigEffectKind::Agents,
        ProjectConfigEffect::Commands => CodeProjectConfigEffectKind::Commands,
        ProjectConfigEffect::Skills => CodeProjectConfigEffectKind::Skills,
        ProjectConfigEffect::Instructions => CodeProjectConfigEffectKind::Instructions,
        ProjectConfigEffect::Settings => CodeProjectConfigEffectKind::Settings,
    }
}
