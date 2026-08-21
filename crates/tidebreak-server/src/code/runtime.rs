//! Process-wide code-mode runtime: adapters, workers, worktrees, recovery.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::sync::oneshot;

use tidebreak_core::db::code::{
    delete_repo, delete_workspace, get_approval, get_open_turn, get_repo, get_repo_by_root_path,
    get_session, get_workspace, insert_repo, insert_session, insert_workspace, list_approvals,
    list_events, list_repos, list_repos_all_owners, list_sessions, list_sessions_all_owners,
    list_sessions_for_workspace, list_turns, list_workspaces, save_approval, save_repo,
    save_session, save_workspace,
};
use tidebreak_core::{
    ApprovalDecisionKind, Attention, AttentionSource, CapLevel, CodeApproval, CodeApprovalId,
    CodeApprovalState, CodeEvent, CodePermissionMode, CodeRepo, CodeSession, CodeSessionId,
    CodeSessionKind, CodeSessionLifecycle, CodeTurn, CodeTurnId, CodeWorkspace,
    CodeWorkspaceStatus, DbStore, Diffstat, FenceReason, HarnessKind, OwnerId, RepoId, WorkspaceId,
};
use tidebreak_harness::{
    builtin_registry, AdapterRegistry, ApprovalChannelSpec, ApprovalDecision, HarnessAdapter,
    HarnessError, HarnessEvent, HarnessEventSink, HarnessProbe, HostEnv, SessionSpec,
};

use super::approval_bridge::ApprovalBridge;
use super::browser_channel::{BrowserSubject, BrowserTokenRegistry};
use super::bus::CodeEventBus;
use super::checkpoint::{
    delete_workspace_refs, list_changed_files, produce_diff, resolve_diff_range,
    sweep_orphaned_refs, ChangedFile, CheckpointError, DiffBounds,
};
use super::clone::CloneJobs;
use super::delivery::DeliveryCache;
use super::fork;
use super::gh::{
    self, ActionOutcome, CommitOutcome, GhError, PrDigestCache, PushOutcome, WorkspaceGitStatus,
};
use super::harness_install::HarnessInstallJobs;
use super::recovery::{self, RecoveryAction};
use super::session_worker::{
    attach_engine, journal_event, queue_follow_up, spawn_session_worker, WorkerCommand,
    WorkerError, WorkerHandle,
};
#[cfg(windows)]
use super::worktree::repo_paths_equivalent;
use super::worktree::{
    self, archive_blockers, branch_name, create_worktree, prune_worktrees, remove_worktree,
    run_archive_script, run_setup_script, slugify, validate_repo_path, worktree_dir, WorktreeError,
};
use crate::error::ServerError;

const MANAGED_NODE_WAIT_TIMEOUT: Duration = Duration::from_secs(300);
const MANAGED_NODE_STARTUP_GRACE: Duration = Duration::from_secs(2);
const MANAGED_NODE_POLL_INTERVAL: Duration = Duration::from_millis(100);

async fn wait_for_managed_node(
    broker: &dyn tidebreak_code_execution::HostToolBroker,
    retry: bool,
    wait_timeout: Duration,
    startup_grace: Duration,
) -> Result<PathBuf, String> {
    use tidebreak_code_execution::{HostDep, HostToolStatus};

    if retry {
        broker.retry(HostDep::Node);
    } else {
        broker.ensure(HostDep::Node);
    }
    let started = Instant::now();
    loop {
        match broker.status(HostDep::Node).await {
            HostToolStatus::Available => {
                return broker.managed_root(HostDep::Node).await.ok_or_else(|| {
                    "managed Node became available without a verified runtime root".to_owned()
                });
            }
            HostToolStatus::Installing => {}
            HostToolStatus::Unavailable(_) if retry && started.elapsed() < startup_grace => {}
            HostToolStatus::Unavailable(reason)
                if started.elapsed() < startup_grace
                    && reason.to_ascii_lowercase().contains("not installed yet") => {}
            HostToolStatus::Unavailable(reason) => return Err(reason),
        }
        if started.elapsed() >= wait_timeout {
            return Err("timed out waiting for the managed Node runtime".to_owned());
        }
        tokio::time::sleep(MANAGED_NODE_POLL_INTERVAL).await;
    }
}

/// Whether a memoized probe still describes the pinned binary on disk.
///
/// A probe observes exactly one file, and the pin version is a segment of
/// that file's path (`{data_dir}/tools/harnesses/{kind}/{version}/…`), so one
/// path comparison covers both halves of what the probe depends on: the
/// resolved binary and the pin it came from. A probe that found nothing
/// describes no install and is always stale.
fn probe_describes(cached: Option<&HarnessProbe>, installed: &Path) -> bool {
    cached.is_some_and(|probe| probe.found && probe.binary_path.as_deref() == Some(installed))
}

/// Optional metadata on `POST /code/repos`. Every field left `None` takes
/// the value [`CodeRuntime::register_repo`] derives from the checkout.
#[derive(Debug, Default)]
pub(crate) struct RepoRegistration {
    pub display_name: Option<String>,
    pub default_base_ref: Option<String>,
    pub branch_prefix: Option<String>,
    pub setup_script: Option<String>,
    pub archive_script: Option<String>,
}

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
    /// Visible worktree root this embedding names, if any. The stored
    /// `code_worktree_root` setting overrides it; see [`super::worktree_root`].
    pub(crate) worktree_root_default: Option<PathBuf>,
    pub blobs: Arc<dyn tidebreak_core::BlobStore>,
    pub approvals: Arc<ApprovalBridge>,
    pub(crate) browser_tokens: BrowserTokenRegistry,
    /// The desktop browser adapter, installed before recovery starts. Absent
    /// in headless deployments and tests that do not register one.
    browser_runtime: Option<Arc<dyn crate::code::browser_runtime::BrowserRuntime>>,
    /// Absolute path to the trusted bridge executable (the `tidebreak` CLI
    /// sidecar). Absent in headless deployments and tests that do not
    /// register one. When both `browser_runtime` and this are `Some`,
    /// session creation mints a [`SessionSpec`] with `browser: Some`; when
    /// either is `None`, `browser` stays `None` and no browser tools are
    /// advertised or injected.
    browser_bridge_command: Option<PathBuf>,

    host: HostEnv,
    host_tool_broker: Option<Arc<dyn tidebreak_code_execution::HostToolBroker>>,
    loopback_base: Mutex<Option<String>>,
    /// Memoized harness probes, one per kind. See [`CodeRuntime::probe`].
    probes: Mutex<HashMap<HarnessKind, HarnessProbe>>,
    /// Last pin-install failure per kind. Cleared on a successful install.
    pin_install_errors: Mutex<HashMap<HarnessKind, String>>,
    workers: Mutex<HashMap<CodeSessionId, WorkerHandle>>,
    /// One turn at a time per worktree, shared by every session in it.
    ///
    /// Sessions in a workspace share one checkout, so their turns cannot
    /// overlap: two harnesses editing the same files, and two checkpoints
    /// racing for `.git/index.lock`, is corruption rather than concurrency.
    /// A worker takes the workspace's lock for the length of a turn, so a
    /// sibling's turn starts after this one ends. See record 55.
    worktree_turns: Mutex<HashMap<WorkspaceId, Arc<tokio::sync::Mutex<()>>>>,
    pr_cache: PrDigestCache,
    pub(crate) delivery_cache: DeliveryCache,
    pub(crate) clone_jobs: CloneJobs,
    /// Warm harness installs started ahead of a session create.
    pub(super) harness_installs: HarnessInstallJobs,
    #[cfg(test)]
    pub(crate) gh_search_path: Mutex<Option<String>>,
    stall_sweep: Mutex<Option<super::attention::StallSweepGuard>>,
    stall_started: AtomicBool,
    watch_sweep: Mutex<Option<super::watch::WatchSweepGuard>>,
    watch_started: AtomicBool,
    /// Workspaces with a background naming call in flight.
    ///
    /// One call per workspace at a time; a second trigger is dropped rather
    /// than queued, because the next turn on a still-unnamed workspace retries
    /// anyway (`super::titling`).
    pub(super) titling_in_flight: Mutex<std::collections::HashSet<tidebreak_core::WorkspaceId>>,
}

impl CodeRuntime {
    pub(crate) fn new(
        db: Arc<DbStore>,
        data_dir: PathBuf,
        worktree_root_default: Option<PathBuf>,
        host_tool_broker: Option<Arc<dyn tidebreak_code_execution::HostToolBroker>>,
        browser_runtime: Option<Arc<dyn crate::code::browser_runtime::BrowserRuntime>>,
        browser_bridge_command: Option<PathBuf>,

    ) -> Self {
        let browser_tokens = BrowserTokenRegistry::new(&data_dir)
            // Panic on construction failure: the data dir is trusted/absolute
            // at this point (set from config and validated by the host). The
            // only failure is an OS-level path resolution error such as a
            // dangling CWD, which is a startup bug, not a runtime condition.
            .expect("browser capfile directory must be resolvable to an absolute path");
        #[cfg(test)]
        browser_tokens.set_loopback_base("http://127.0.0.1:0");
        Self {
            db,
            bus: Arc::new(CodeEventBus::default()),
            adapters: builtin_registry(),
            blobs: Arc::new(tidebreak_core::FsBlobStore::new(data_dir.join("blobs"))),
            data_dir: data_dir.clone(),
            worktree_root_default,
            approvals: ApprovalBridge::new(),
            browser_tokens,
            browser_runtime,
            browser_bridge_command,

            host: HostEnv {
                data_dir: Some(data_dir),
                ..HostEnv::from_process()
            },
            host_tool_broker,
            loopback_base: Mutex::new(None),
            probes: Mutex::new(HashMap::new()),
            pin_install_errors: Mutex::new(HashMap::new()),
            workers: Mutex::new(HashMap::new()),
            worktree_turns: Mutex::new(HashMap::new()),
            pr_cache: PrDigestCache::default(),
            delivery_cache: DeliveryCache::default(),
            clone_jobs: CloneJobs::default(),
            harness_installs: HarnessInstallJobs::default(),
            #[cfg(test)]
            gh_search_path: Mutex::new(None),
            stall_sweep: Mutex::new(None),
            stall_started: AtomicBool::new(false),
            watch_sweep: Mutex::new(None),
            watch_started: AtomicBool::new(false),
            titling_in_flight: Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// Bound after listen so Claude can be pointed at the loopback MCP route.
    fn set_loopback_base(&self, base: String) {
        self.browser_tokens.set_loopback_base(&base);
        *self.loopback_base.lock().expect("loopback base") =
            Some(base.trim_end_matches('/').into());
    }

    /// Boot: publish the bound loopback base now, and hand back the recovery
    /// pass to run.
    ///
    /// The two steps are one call because their order is load-bearing. Every
    /// worker recovery re-attaches mints its approval MCP endpoint from this
    /// base ([`Self::approval_channel`]), so recovering before the bound
    /// address is known restores Ask- and Auto-mode sessions with no approval
    /// channel at all — silently, since the channel is an `Option`. Recovery
    /// comes back as a future rather than running here so the boot path can
    /// keep it off the bind critical path; the base is published before that
    /// future exists, so a session created while recovery is still in flight
    /// is wired exactly like a recovered one.
    pub(crate) fn start(
        self: &Arc<Self>,
        loopback_base: String,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<RecoveryAction>, ServerError>> + Send>,
    > {
        self.set_loopback_base(loopback_base);
        // Synchronously delete stale capfiles before the returned future is
        // pollable so issue() cannot race an unpolled startup cleanup.
        let cleanup = self.browser_tokens.delete_all_stale_capfiles();
        let runtime = self.clone();
        Box::pin(async move {
            cleanup.map_err(ServerError::internal)?;
            let actions = runtime.recover().await;
            // After recovery so resumed watch sessions have workers to drive;
            // the sweep reads its work list from the `code_watch` table, so
            // active watches resume with no extra state.
            runtime.ensure_watch_sweep();
            actions
        })
    }

    #[cfg(any(test, feature = "scripted-harness"))]
    pub(crate) fn with_registry(
        db: Arc<DbStore>,
        data_dir: PathBuf,
        adapters: AdapterRegistry,
    ) -> Self {
        Self::with_registry_and_browser_runtime(db, data_dir, adapters, None, None)

    }

    #[cfg(any(test, feature = "scripted-harness"))]
    pub(crate) fn with_registry_and_browser_runtime(
        db: Arc<DbStore>,
        data_dir: PathBuf,
        adapters: AdapterRegistry,
        browser_runtime: Option<Arc<dyn crate::code::browser_runtime::BrowserRuntime>>,
        browser_bridge_command: Option<PathBuf>,

    ) -> Self {
        let browser_tokens = BrowserTokenRegistry::new(&data_dir)
            // Panic on construction failure: the data dir is trusted/absolute
            // at this point (set from config and validated by the host). The
            // only failure is an OS-level path resolution error such as a
            // dangling CWD, which is a startup bug, not a runtime condition.
            .expect("browser capfile directory must be resolvable to an absolute path");
        #[cfg(test)]
        browser_tokens.set_loopback_base("http://127.0.0.1:0");
        Self {
            db,
            bus: Arc::new(CodeEventBus::default()),
            adapters,
            blobs: Arc::new(tidebreak_core::FsBlobStore::new(data_dir.join("blobs"))),
            data_dir,
            worktree_root_default: None,
            approvals: ApprovalBridge::new(),
            browser_tokens,
            browser_runtime,
            browser_bridge_command,

            host: HostEnv::from_process(),
            host_tool_broker: None,
            loopback_base: Mutex::new(None),
            probes: Mutex::new(HashMap::new()),
            pin_install_errors: Mutex::new(HashMap::new()),
            workers: Mutex::new(HashMap::new()),
            worktree_turns: Mutex::new(HashMap::new()),
            pr_cache: PrDigestCache::default(),
            delivery_cache: DeliveryCache::default(),
            clone_jobs: CloneJobs::default(),
            harness_installs: HarnessInstallJobs::default(),
            #[cfg(test)]
            gh_search_path: Mutex::new(None),
            stall_sweep: Mutex::new(None),
            stall_started: AtomicBool::new(false),
            watch_sweep: Mutex::new(None),
            watch_started: AtomicBool::new(false),
            titling_in_flight: Mutex::new(std::collections::HashSet::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_gh_search_path(&self, path: Option<String>) {
        *self.gh_search_path.lock().expect("gh search path") = path;
    }

    /// Return the installed browser adapter, if any.
    pub(crate) fn browser_runtime(
        &self,
    ) -> Option<Arc<dyn crate::code::browser_runtime::BrowserRuntime>> {
        self.browser_runtime.clone()
    }

    /// Revoke the session browser token AND the desktop adapter
    /// capability. Idempotent — safe to call multiple times.
    ///
    /// The token registry is invalidated first and the adapter scope is
    /// derived from the returned [`BrowserSubject`] — no DB lookup needed.
    /// The adapter call happens after the registry lock is released.
    fn revoke_browser_session(&self, session_id: CodeSessionId) {
        let subject = self.browser_tokens.revoke(session_id);
        if let Some(subject) = subject {
            if let Some(runtime) = &self.browser_runtime {
                let scope = crate::code::browser_runtime::BrowserRuntimeScope::from(subject);
                runtime.revoke_session(&scope);
            }
        }
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
        // Concurrently: each attach launches an engine child, so a serial pass
        // charged app launch the sum of every restored session.
        let resumable: Vec<CodeSession> = list_sessions_all_owners(&self.db)
            .await?
            .into_iter()
            .filter(|session| {
                !matches!(
                    session.lifecycle,
                    CodeSessionLifecycle::Ended | CodeSessionLifecycle::Fenced
                ) && !self
                    .workers
                    .lock()
                    .expect("code workers")
                    .contains_key(&session.id)
            })
            .collect();
        futures::future::join_all(resumable.into_iter().map(|session| async move {
            if let Err(error) = self.attach_and_spawn_worker(session).await {
                tracing::warn!(
                    "code-mode: could not resume a recovered session worker: {}",
                    error.message()
                );
            }
        }))
        .await;
        Ok(actions)
    }

    /// The probe for `adapter`, resolved once and memoized.
    ///
    /// Decision 0034 makes discovery a cached read rather than a per-request
    /// one: a cold probe asks the user's shell in interactive login mode and
    /// then runs a version and an authentication observation, which is seconds
    /// of subprocess per harness. Re-probing is on demand — the doctor's
    /// refresh calls [`Self::invalidate_probes`] — so a harness installed
    /// while the app is running is picked up by the button that exists to say
    /// so, not by paying for it on every code-mode navigation.
    ///
    /// The cache fills lazily. Nothing warms it at boot: recovery already
    /// probes the kinds that have live sessions on its way to re-attaching
    /// them, and warming the rest would spend four login shells on harnesses
    /// this launch may never touch.
    pub(crate) async fn probe(&self, adapter: &dyn HarnessAdapter) -> HarnessProbe {
        let kind = adapter.kind();
        let cached = self
            .probes
            .lock()
            .expect("harness probes")
            .get(&kind)
            .cloned();
        if let Some(probe) = cached {
            return probe;
        }
        let mut host = self.host.clone();
        host.managed_node_root = match self.host_tool_broker.as_deref() {
            Some(broker) => {
                broker
                    .managed_root(tidebreak_code_execution::HostDep::Node)
                    .await
            }
            None => tidebreak_code_execution::managed_node::managed_node_root(&self.data_dir),
        };
        let probe = adapter.probe(&host).await;
        self.probes
            .lock()
            .expect("harness probes")
            .insert(kind, probe.clone());
        probe
    }

    /// Drop every memoized probe so the next read is cold. The doctor's
    /// refresh is the on-demand re-probe decision 0034 describes.
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn record_pin_install(&self, kind: HarnessKind, result: Result<(), String>) {
        let mut errors = self.pin_install_errors.lock().expect("pin install errors");
        match result {
            Ok(()) => {
                errors.remove(&kind);
            }
            Err(err) => {
                errors.insert(kind, err);
            }
        }
    }

    pub(crate) fn pin_install_error(&self, kind: HarnessKind) -> Option<String> {
        self.pin_install_errors
            .lock()
            .expect("pin install errors")
            .get(&kind)
            .cloned()
    }

    pub(crate) fn invalidate_probes(&self) {
        self.probes.lock().expect("harness probes").clear();
    }

    /// Drop the memoized probe for `kind` only when the install it was taken
    /// against is not the one now on disk.
    ///
    /// Session create used to invalidate unconditionally, which charged every
    /// create a cold probe — a login shell plus a Node CLI start — to observe
    /// a binary that had not moved since the last one.
    pub(super) fn invalidate_moved_probe(&self, kind: HarnessKind, installed: &Path) {
        let mut probes = self.probes.lock().expect("harness probes");
        if !probe_describes(probes.get(&kind), installed) {
            probes.remove(&kind);
        }
    }

    #[cfg_attr(test, allow(dead_code))]
    async fn managed_node_root(&self, retry: bool) -> Result<PathBuf, String> {
        match self.host_tool_broker.as_deref() {
            Some(broker) => {
                wait_for_managed_node(
                    broker,
                    retry,
                    MANAGED_NODE_WAIT_TIMEOUT,
                    MANAGED_NODE_STARTUP_GRACE,
                )
                .await
            }
            None => tidebreak_code_execution::managed_node::managed_node_root(&self.data_dir)
                .ok_or_else(|| {
                    "the verified managed Node runtime is not installed in this Tidebreak data directory"
                        .to_owned()
                }),
        }
    }

    #[cfg_attr(test, allow(dead_code))]
    pub(super) async fn ensure_pinned_harness(
        &self,
        kind: HarnessKind,
        retry_node: bool,
    ) -> Result<PathBuf, String> {
        let node_root = self.managed_node_root(retry_node).await?;
        tidebreak_harness::ensure_installed(&self.data_dir, kind, Some(&node_root)).await
    }

    #[cfg_attr(test, allow(dead_code))]
    pub(crate) async fn refresh_pinned_harnesses(&self) {
        let node_root = self.managed_node_root(true).await;
        let installs = HarnessKind::ALL.iter().map(|kind| {
            let node_root = node_root.as_ref();
            async move {
                let result = match node_root {
                    Ok(root) => tidebreak_harness::ensure_installed(
                        &self.data_dir,
                        *kind,
                        Some(root.as_path()),
                    )
                    .await
                    .map(|_| ()),
                    Err(err) => Err(err.clone()),
                };
                (*kind, result)
            }
        });
        for (kind, result) in futures::future::join_all(installs).await {
            self.record_pin_install(kind, result);
        }
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
        owner: &OwnerId,
        root_path: PathBuf,
        metadata: RepoRegistration,
    ) -> Result<CodeRepo, ServerError> {
        let RepoRegistration {
            display_name,
            default_base_ref,
            branch_prefix,
            setup_script,
            archive_script,
        } = metadata;
        let validated = validate_repo_path(&root_path).await.map_err(map_worktree)?;
        let toplevel = validated.toplevel.display().to_string();
        let exact = get_repo_by_root_path(&self.db, owner, &toplevel).await?;
        #[cfg(windows)]
        let existing = match exact {
            Some(repo) => Some(repo),
            None => list_repos(&self.db, owner).await?.into_iter().find(|repo| {
                repo_paths_equivalent(std::path::Path::new(&repo.root_path), &validated.toplevel)
            }),
        };
        #[cfg(not(windows))]
        let existing = exact;
        if let Some(existing) = existing {
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
            owner: owner.clone(),
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

    pub(crate) async fn list_repos(&self, owner: &OwnerId) -> Result<Vec<CodeRepo>, ServerError> {
        Ok(list_repos(&self.db, owner).await?)
    }

    pub(crate) async fn get_repo(
        &self,
        owner: &OwnerId,
        id: RepoId,
    ) -> Result<CodeRepo, ServerError> {
        get_repo(&self.db, owner, id)
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

    pub(crate) async fn delete_repo(&self, owner: &OwnerId, id: RepoId) -> Result<(), ServerError> {
        let workspaces = list_workspaces(&self.db, owner, Some(id)).await?;
        if workspaces
            .iter()
            .any(|workspace| workspace.status != CodeWorkspaceStatus::Archived)
        {
            return Err(ServerError::conflict_kind(
                "repo_has_workspaces",
                "archive every workspace before deleting the repository",
            ));
        }
        if !delete_repo(&self.db, owner, id).await? {
            return Err(ServerError::not_found(format!("repo {id} not found")));
        }
        Ok(())
    }

    pub(crate) async fn create_workspace(
        &self,
        owner: &OwnerId,
        repo_id: RepoId,
        title: Option<String>,
        base_ref: Option<String>,
    ) -> Result<CodeWorkspace, ServerError> {
        let repo = self.get_repo(owner, repo_id).await?;
        let title = title
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        let id = WorkspaceId::new();
        let branch = branch_name(&repo.branch_prefix, &title, id.0.as_u128());
        let existing = list_workspaces(&self.db, owner, Some(repo_id)).await?;
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
        // Resolved per creation, not cached: the root is a setting an operator
        // can change while the process runs, and it decides only where the
        // *next* worktree lands. Existing workspaces keep the absolute path on
        // their row (`super::worktree_root`).
        let root = self.owner_worktree_root(owner).await?;
        let path = worktree_dir(&root, id, &repo_slug, &workspace_slug);
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
            owner: owner.clone(),
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
                let _ = delete_workspace(&self.db, owner, id).await;
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
        owner: &OwnerId,
        repo_id: Option<RepoId>,
    ) -> Result<Vec<CodeWorkspace>, ServerError> {
        Ok(list_workspaces(&self.db, owner, repo_id).await?)
    }

    pub(crate) async fn get_workspace(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<CodeWorkspace, ServerError> {
        get_workspace(&self.db, owner, id)
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
        super::attention::emit_workspace_digests(
            &self.db,
            &self.bus,
            &workspace.owner,
            workspace.id,
        )
        .await;
        Ok(())
    }

    pub(crate) async fn archive_workspace(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
        force: bool,
    ) -> Result<CodeWorkspace, ServerError> {
        let mut workspace = self.get_workspace(owner, id).await?;
        if workspace.status == CodeWorkspaceStatus::Archived {
            return Ok(workspace);
        }
        let repo = self.get_repo(owner, workspace.repo_id).await?;
        // Blockers first: a refused archive must leave the workspace exactly as
        // it was, and running the hook script is not "exactly as it was".
        self.refuse_running_sessions(owner, id, force).await?;
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
            // Decision 0032: the archive script obeys the same
            // failure-preserves rule as setup. A script that backs up or
            // pushes state and exits non-zero must stop the archive, not have
            // its checkout removed underneath it. A worktree that is already
            // gone has nothing to run the script against.
            if let Err(err) = run_archive_script(path, repo.archive_script.as_deref()).await {
                return Err(ServerError::unprocessable_kind(
                    "archive_script_failed",
                    err.to_string(),
                ));
            }
        }
        let workers_stopped = self.end_workspace_sessions(owner, id).await?;
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
            .sweep_repo_checkpoint_refs(owner, std::path::Path::new(&repo.root_path), repo.id)
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
        // Drop the turn lock only once every worker confirmed it was gone.
        // The lock is an `Arc`, so forgetting it here does not disturb a
        // worker that outlived its shutdown grace — it hands the *next* one a
        // second lock over the same checkout, which is the one thing this is
        // supposed to make impossible. Keeping the entry costs a map slot per
        // archived workspace and keeps restore on the same lock.
        if workers_stopped {
            self.worktree_turns
                .lock()
                .expect("worktree turn locks")
                .remove(&workspace.id);
        } else {
            tracing::warn!(
                workspace = %workspace.id,
                "code-mode: keeping the workspace turn lock; a worker outlived its shutdown"
            );
        }
        Ok(workspace)
    }

    /// Reactivate an archived workspace at its own path, on its own branch.
    ///
    /// Archive keeps the branch, the session rows, and the journal; restore
    /// puts a checkout back under them. Nothing else changes: sessions stay
    /// Ended (a new one resumes via the stored harness ref), and the
    /// checkpoint refs archive deleted stay gone — per-turn diffs from before
    /// the archive cannot be reopened.
    ///
    /// `create_workspace`'s branch-collision check reads archived rows too,
    /// so this route is the sanctioned way to get an archived branch back
    /// into a workspace.
    pub(crate) async fn restore_workspace(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<CodeWorkspace, ServerError> {
        let mut workspace = self.get_workspace(owner, id).await?;
        if workspace.status == CodeWorkspaceStatus::Active {
            return Ok(workspace);
        }
        if workspace.status != CodeWorkspaceStatus::Archived {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                format!("workspace is {}", workspace.status.as_str()),
            ));
        }
        let repo = self.get_repo(owner, workspace.repo_id).await?;
        let repo_root = std::path::Path::new(&repo.root_path);
        if !worktree::branch_exists(repo_root, &workspace.branch_name)
            .await
            .map_err(map_worktree)?
        {
            return Err(ServerError::conflict_kind(
                "branch_missing",
                format!(
                    "branch {} no longer exists; create a new workspace instead",
                    workspace.branch_name
                ),
            ));
        }
        let path = std::path::Path::new(&workspace.worktree_path);
        if path.exists() {
            return Err(ServerError::conflict_kind(
                "worktree_path_occupied",
                format!("something already exists at {}", workspace.worktree_path),
            ));
        }
        worktree::restore_worktree(repo_root, path, &workspace.branch_name)
            .await
            .map_err(map_worktree)?;
        // Mirror create's tail exactly: setup decides between Active and
        // SetupFailed, and a failing script preserves the checkout
        // (Decision 0032's failure-preserves rule). One vocabulary for both
        // paths — a reader debugging "setup_failed" should not need to know
        // whether the workspace was created or restored.
        match run_setup_script(path, repo.setup_script.as_deref()).await {
            Ok(()) => {
                workspace.status = CodeWorkspaceStatus::Active;
                workspace.archived_at = None;
                save_workspace(&self.db, &workspace).await?;
                Ok(workspace)
            }
            Err(err) => {
                workspace.status = CodeWorkspaceStatus::SetupFailed;
                workspace.archived_at = None;
                save_workspace(&self.db, &workspace).await?;
                Err(ServerError::unprocessable_kind(
                    "setup_failed",
                    err.to_string(),
                ))
            }
        }
    }

    pub(crate) async fn commit_workspace(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
        message: Option<String>,
    ) -> Result<CommitOutcome, ServerError> {
        let workspace = self.require_live_workspace(owner, id).await?;
        gh::commit_all(
            std::path::Path::new(&workspace.worktree_path),
            &workspace.title,
            message.as_deref(),
        )
        .await
        .map_err(map_gh)
    }

    pub(crate) async fn push_workspace(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<PushOutcome, ServerError> {
        let workspace = self.require_live_workspace(owner, id).await?;
        gh::push_branch(
            std::path::Path::new(&workspace.worktree_path),
            &workspace.branch_name,
        )
        .await
        .map_err(map_gh)
    }

    pub(crate) async fn workspace_pr(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        let mut workspace = self.get_workspace(owner, id).await?;
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

    /// Force a fresh host read: drop the digest cache entry, then take the
    /// normal status path.
    pub(crate) async fn refresh_workspace_pr(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        self.pr_cache.invalidate(id);
        self.workspace_pr(owner, id).await
    }

    pub(crate) async fn workspace_pr_comments(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<gh::PrComments, ServerError> {
        let workspace = self.get_workspace(owner, id).await?;
        let gh_path = self.gh_search_path_owned();
        gh::load_pr_comments(
            std::path::Path::new(&workspace.worktree_path),
            gh_path.as_deref(),
        )
        .await
        .map_err(map_gh)
    }

    /// User-initiated merge (or auto-merge arming) of the workspace PR, then a
    /// fresh status read so the caller and the updates channel both see the
    /// result. This is the only route to `gh pr merge`.
    pub(crate) async fn merge_workspace_pr(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
        method: gh::MergeMethod,
        auto: bool,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        let workspace = self.require_live_workspace(owner, id).await?;
        let gh_path = self.gh_search_path_owned();
        gh::merge_pull_request(
            std::path::Path::new(&workspace.worktree_path),
            workspace.id,
            method,
            auto,
            &self.pr_cache,
            gh_path.as_deref(),
        )
        .await
        .map_err(map_gh)?;
        self.workspace_pr(owner, id).await
    }

    pub(crate) async fn create_workspace_pr(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
        title: Option<String>,
        body: Option<String>,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        let mut workspace = self.require_live_workspace(owner, id).await?;
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
        self.workspace_pr(owner, id).await
    }

    pub(crate) async fn run_workspace_action(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
        name: &str,
    ) -> Result<ActionOutcome, ServerError> {
        let workspace = self.require_live_workspace(owner, id).await?;
        let repo = self.get_repo(owner, workspace.repo_id).await?;
        gh::run_named_action(
            std::path::Path::new(&workspace.worktree_path),
            &repo.quick_actions,
            name,
        )
        .await
        .map_err(map_gh)
    }

    async fn require_live_workspace(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<CodeWorkspace, ServerError> {
        let workspace = self.get_workspace(owner, id).await?;
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

    pub(crate) fn gh_search_path_owned(&self) -> Option<String> {
        #[cfg(test)]
        {
            return self.gh_search_path.lock().expect("gh search path").clone();
        }
        #[cfg(not(test))]
        None
    }

    pub(crate) async fn create_session(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        harness: HarnessKind,
        permission_mode: CodePermissionMode,
        model: Option<String>,
    ) -> Result<CodeSession, ServerError> {
        self.create_session_of_kind(
            owner,
            workspace_id,
            CodeSessionKind::Interactive,
            harness,
            permission_mode,
            model,
        )
        .await
    }

    /// Shared create path for interactive sessions and watch sessions.
    ///
    /// A workspace holds any number of interactive sessions and at most one
    /// watch session, so the guard below covers watch only. The worktree they
    /// share is protected by the per-workspace turn lock rather than by a cap
    /// on conversations; see record 55.
    pub(crate) async fn create_session_of_kind(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        kind: CodeSessionKind,
        harness: HarnessKind,
        permission_mode: CodePermissionMode,
        model: Option<String>,
    ) -> Result<CodeSession, ServerError> {
        let workspace = self.get_workspace(owner, workspace_id).await?;
        if workspace.status != CodeWorkspaceStatus::Active {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                format!("workspace is {}", workspace.status.as_str()),
            ));
        }
        if kind == CodeSessionKind::Watch {
            let existing = list_sessions_for_workspace(&self.db, owner, workspace_id).await?;
            if existing.iter().any(|session| {
                session.lifecycle != CodeSessionLifecycle::Ended
                    && session.kind == CodeSessionKind::Watch
            }) {
                return Err(ServerError::conflict_kind(
                    "session_exists",
                    "this workspace already has an active watch session",
                ));
            }
        }
        let adapter = self.adapter(harness)?;
        #[cfg(not(test))]
        {
            // The warm install the dialog starts usually got here first, in
            // which case this is a marker read. It stays on the create path
            // regardless: correctness must not depend on the warm path having
            // run, and a pin installed here is serialized against that one.
            match self.ensure_pinned_harness(harness, false).await {
                Ok(binary) => {
                    self.record_pin_install(harness, Ok(()));
                    self.invalidate_moved_probe(harness, &binary);
                }
                Err(err) => {
                    self.record_pin_install(harness, Err(err.clone()));
                    return Err(ServerError::unprocessable_kind(
                        "harness_not_found",
                        format!("{harness} could not be installed: {err}"),
                    ));
                }
            }
        }
        let probe = self.probe(adapter.as_ref()).await;
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
            owner: owner.clone(),
            workspace_id,
            kind,
            harness_kind: harness,
            harness_version: probe.version.clone(),
            harness_resume_ref: None,
            permission_mode,
            model: normalize_model(model),
            lifecycle: CodeSessionLifecycle::Created,
            fence_reason: None,
            child_pid: None,
            spawn_epoch: 0,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: Utc::now(),
        };
        insert_session(&self.db, &session).await?;
        self.attach_and_spawn_worker(session).await
    }

    pub(crate) async fn get_session(
        &self,
        owner: &OwnerId,
        id: CodeSessionId,
    ) -> Result<CodeSession, ServerError> {
        get_session(&self.db, owner, id)
            .await?
            .ok_or_else(|| ServerError::not_found(format!("session {id} not found")))
    }

    pub(crate) async fn resolve_turn_attachments(
        &self,
        requested: &[(uuid::Uuid, String)],
    ) -> Result<Vec<tidebreak_core::CodeTurnAttachment>, ServerError> {
        if requested.len() > tidebreak_core::MAX_MESSAGE_ATTACHMENTS {
            return Err(ServerError::bad_request(format!(
                "a turn may carry at most {} image attachments",
                tidebreak_core::MAX_MESSAGE_ATTACHMENTS
            )));
        }
        let mut resolved = Vec::with_capacity(requested.len());
        for (blob_id, media_type) in requested {
            if blob_id.is_nil() {
                return Err(ServerError::bad_request(
                    "attachment blob_id must not be nil",
                ));
            }
            let media_type = parse_turn_media_type(media_type).ok_or_else(|| {
                ServerError::bad_request(format!("unsupported attachment media type {media_type}"))
            })?;
            let Some(meta) = self
                .blobs
                .metadata(*blob_id)
                .await
                .map_err(|err| ServerError::internal(format!("blob metadata: {err}")))?
            else {
                return Err(ServerError::bad_request(format!(
                    "attachment blob {blob_id} was not found"
                )));
            };
            if meta.byte_len == 0 {
                return Err(ServerError::bad_request("attachment blob is empty"));
            }
            if meta.byte_len > tidebreak_core::MAX_IMAGE_BYTES {
                return Err(ServerError::bad_request(
                    "attachment exceeds the maximum image size",
                ));
            }
            resolved.push(tidebreak_core::CodeTurnAttachment {
                blob_id: *blob_id,
                media_type,
                byte_len: meta.byte_len,
            });
        }
        Ok(resolved)
    }

    pub(crate) async fn submit_turn(
        &self,
        owner: &OwnerId,
        id: CodeSessionId,
        message: String,
        model: Option<String>,
        attachments: Vec<tidebreak_core::CodeTurnAttachment>,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        let mut session = self.get_session(owner, id).await?;
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
        if !attachments.is_empty() {
            let adapter = self.adapter(session.harness_kind)?;
            let probe = self.probe(adapter.as_ref()).await;
            let caps = adapter.capabilities(&probe);
            if caps.image_input != CapLevel::Supported {
                return Err(ServerError::unprocessable_kind(
                    "unsupported_attachment",
                    format!(
                        "{harness} does not support image attachments",
                        harness = session.harness_kind
                    ),
                ));
            }
        }
        let handle = self.require_worker(id)?;
        if let Some(model) = normalize_model(model) {
            session.model = Some(model);
            let _ = save_session(&self.db, &session).await?;
        }
        // A fenced sibling means an engine from a previous boot may still be
        // alive in this checkout, outside every lock this process holds. The
        // turn lock cannot see it, so nothing in the workspace writes until it
        // is reaped (record 55).
        if let Some(reason) = self.workspace_fence_reason(owner, &session).await? {
            return Err(ServerError::conflict_kind("workspace_fenced", reason));
        }
        // Queue-default (0009): a send while a turn is in flight parks one
        // follow-up. This does not consult mid_turn_steering — that cap
        // gates the separate /steer route only.
        let in_flight = session.lifecycle == CodeSessionLifecycle::Running
            || get_open_turn(&self.db, owner, id).await?.is_some();
        if in_flight {
            return self.park_follow_up(&handle, &session, message, attachments);
        }
        let (reply, rx) = oneshot::channel();
        handle
            .commands
            .send(WorkerCommand::RunTurn {
                message: message.clone(),
                model: session.model.clone(),
                attachments: attachments.clone(),
                reply,
            })
            .await
            .map_err(|_| ServerError::internal("session worker is gone"))?;
        let turn = match rx
            .await
            .map_err(|_| ServerError::internal("session worker dropped the turn"))?
        {
            Ok(turn) => turn,
            // A sibling holds the workspace's turn lock. Taking that lock is
            // the reservation, so this is the first moment either send can
            // know which one won — a check before the send would let two idle
            // siblings both believe the checkout was free. The loser parks
            // exactly as a busy session does, and the route answers now rather
            // than holding the connection open for someone else's turn.
            Err(WorkerError::WorktreeBusy) => {
                return self.park_follow_up(&handle, &session, message, attachments);
            }
            Err(err) => return Err(map_worker(err)),
        };
        Ok(SubmitTurnOutcome::Ran(Box::new(turn)))
    }

    /// Park a message in the session's single follow-up slot (record 9).
    fn park_follow_up(
        &self,
        handle: &WorkerHandle,
        session: &CodeSession,
        message: String,
        attachments: Vec<tidebreak_core::CodeTurnAttachment>,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        if !queue_follow_up(handle, message, session.model.clone(), attachments) {
            return Err(ServerError::conflict_kind(
                "queue_full",
                "a follow-up is already queued on this session",
            ));
        }
        Ok(SubmitTurnOutcome::Queued)
    }

    /// Why this session's workspace is closed to turns, if it is.
    ///
    /// The turn lock lives in this process, so it can only order the workers
    /// this process owns. A fenced session is the one case where that is not
    /// enough: it means a harness child outlived a restart and may still be
    /// writing to the checkout. Until it is reaped, no sibling may start a
    /// turn there — otherwise the single-writer rule holds only for the
    /// sessions we happen to know about.
    async fn workspace_fence_reason(
        &self,
        owner: &OwnerId,
        session: &CodeSession,
    ) -> Result<Option<String>, ServerError> {
        let siblings = list_sessions_for_workspace(&self.db, owner, session.workspace_id).await?;
        Ok(siblings
            .iter()
            .find(|other| other.id != session.id && other.lifecycle == CodeSessionLifecycle::Fenced)
            .map(|fenced| {
                format!(
                    "another session in this workspace is fenced until it is reaped ({})",
                    fenced.id
                )
            }))
    }

    pub(crate) async fn interrupt(&self, id: CodeSessionId) -> Result<(), ServerError> {
        // Invalidate the session browser token so in-flight browser route
        // calls are rejected immediately, then revoke the native scope.
        self.revoke_browser_session(id);
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

    pub(crate) async fn steer(
        &self,
        owner: &OwnerId,
        id: CodeSessionId,
        expected_turn_id: CodeTurnId,
        message: String,
    ) -> Result<(), ServerError> {
        let session = self.get_session(owner, id).await?;
        if session.lifecycle != CodeSessionLifecycle::Running {
            return Err(ServerError::conflict_kind(
                "no_active_turn",
                "there is no active turn to steer; the message was not queued",
            ));
        }
        let Some(active_turn) = get_open_turn(&self.db, owner, id).await? else {
            return Err(ServerError::conflict_kind(
                "no_active_turn",
                "there is no active turn to steer; the message was not queued",
            ));
        };
        if active_turn.id != expected_turn_id {
            return Err(ServerError::conflict_kind(
                "stale_turn",
                format!(
                    "turn {expected_turn_id} is no longer active; current turn is {}; the message was not queued",
                    active_turn.id
                ),
            ));
        }
        let adapter = self.adapter(session.harness_kind)?;
        let probe = self.probe(adapter.as_ref()).await;
        let level = adapter.capabilities(&probe).mid_turn_steering;
        if level != CapLevel::Supported {
            return Err(ServerError::unprocessable_kind(
                "steering_unavailable",
                format!(
                    "{harness} mid-turn steering is {level}; the message was not queued",
                    harness = session.harness_kind,
                    level = level.as_str(),
                ),
            ));
        }
        let handle = self.require_worker(id)?;
        let (reply, rx) = oneshot::channel();
        handle
            .commands
            .send(WorkerCommand::Steer {
                expected_turn_id,
                message,
                reply,
            })
            .await
            .map_err(|_| ServerError::internal("session worker is gone"))?;
        rx.await
            .map_err(|_| ServerError::internal("session worker dropped the steer"))?
            .map_err(map_worker)
    }

    pub(crate) async fn reap(
        &self,
        owner: &OwnerId,
        id: CodeSessionId,
    ) -> Result<CodeSession, ServerError> {
        let session = self.get_session(owner, id).await?;
        if session.lifecycle != CodeSessionLifecycle::Fenced {
            return Err(ServerError::conflict_kind(
                "not_fenced",
                "only a fenced session can be reaped",
            ));
        }
        let handle = self.workers.lock().expect("code workers").remove(&id);
        self.revoke_browser_session(id);
        let session = match handle {
            // The outgoing worker writes its own final state as it stops, and
            // the new spawn must not be started against a row it is still
            // moving. Wait for it, then reap the row as it stands now.
            Some(handle) => {
                Self::shut_down_worker(id, handle).await;
                self.get_session(owner, id).await?
            }
            None => session,
        };
        let session = recovery::reap_session(&self.db, &self.bus, session)
            .await
            .map_err(ServerError::from)?;
        self.attach_and_spawn_worker(session).await
    }

    pub(crate) async fn set_attention(
        &self,
        owner: &OwnerId,
        id: CodeSessionId,
        clear: bool,
        note: Option<String>,
    ) -> Result<CodeSession, ServerError> {
        let _ = self.get_session(owner, id).await?;
        super::attention::user_set_attention(&self.db, &self.bus, owner, id, clear, note)
            .await
            .map_err(ServerError::from)
    }

    pub(crate) async fn mark_session_viewed(
        &self,
        owner: &OwnerId,
        id: CodeSessionId,
    ) -> Result<(), ServerError> {
        super::attention::mark_viewed(&self.db, &self.bus, owner, id)
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

    /// Start the watch sweep once. The guard holds a weak runtime handle so
    /// this field never keeps its own runtime alive.
    pub(super) fn ensure_watch_sweep(self: &Arc<Self>) {
        if self.watch_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let guard = super::watch::WatchSweepGuard::spawn(Arc::downgrade(self));
        *self.watch_sweep.lock().expect("watch sweep") = Some(guard);
    }

    /// End one session: mark the row ended, stop its worker, and re-assert.
    ///
    /// The same steps [`Self::end_workspace_sessions`] takes per session,
    /// for callers that must end a single session (the watch path) without
    /// touching the workspace's other sessions.
    pub(super) async fn end_session_row(
        &self,
        owner: &OwnerId,
        session_id: CodeSessionId,
    ) -> Result<(), ServerError> {
        let Some(mut session) = get_session(&self.db, owner, session_id).await? else {
            return Ok(());
        };
        if session.lifecycle == CodeSessionLifecycle::Ended {
            return Ok(());
        }
        let handle = self
            .workers
            .lock()
            .expect("code workers")
            .remove(&session.id);
        self.revoke_browser_session(session.id);
        session.lifecycle = CodeSessionLifecycle::Ended;
        session.child_pid = None;
        session.fence_reason = None;
        super::attention::persist_session(&self.db, &self.bus, &session).await?;
        if let Some(handle) = handle {
            Self::shut_down_worker(session.id, handle).await;
        }
        let mut current = self.get_session(owner, session.id).await?;
        current.lifecycle = CodeSessionLifecycle::Ended;
        current.child_pid = None;
        current.fence_reason = None;
        if !super::attention::persist_session(&self.db, &self.bus, &current).await? {
            return Err(ServerError::conflict_kind(
                "session_not_ended",
                "the session did not stay ended after the worker stopped",
            ));
        }
        Ok(())
    }

    pub(crate) async fn list_sessions(
        &self,
        owner: &OwnerId,
    ) -> Result<Vec<CodeSession>, ServerError> {
        Ok(list_sessions(&self.db, owner).await?)
    }

    pub(crate) async fn list_workspace_sessions(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<CodeSession>, ServerError> {
        let _ = self.get_workspace(owner, workspace_id).await?;
        Ok(list_sessions_for_workspace(&self.db, owner, workspace_id).await?)
    }

    pub(crate) async fn list_session_turns(
        &self,
        owner: &OwnerId,
        session_id: CodeSessionId,
    ) -> Result<Vec<CodeTurn>, ServerError> {
        let _ = self.get_session(owner, session_id).await?;
        Ok(list_turns(&self.db, owner, session_id).await?)
    }

    pub(crate) async fn session_debug(
        &self,
        owner: &OwnerId,
        session_id: CodeSessionId,
    ) -> Result<
        (
            CodeSession,
            Vec<CodeTurn>,
            Vec<tidebreak_core::SequencedCodeEvent>,
        ),
        ServerError,
    > {
        let session = self.get_session(owner, session_id).await?;
        let turns = list_turns(&self.db, owner, session_id).await?;
        let events = list_events(&self.db, owner, session_id, 0).await?;
        Ok((session, turns, events))
    }

    /// Write this session's transcript into its worktree for a fork to read.
    ///
    /// The child is a sibling session in the same worktree, so the file only
    /// has to exist where the engine already works. Nothing is handed over
    /// here: the caller creates the child and names the path in its first
    /// message.
    pub(crate) async fn fork_transcript(
        &self,
        owner: &OwnerId,
        session_id: CodeSessionId,
    ) -> Result<fork::WrittenTranscript, ServerError> {
        let session = self.get_session(owner, session_id).await?;
        let workspace = self
            .require_live_workspace(owner, session.workspace_id)
            .await?;
        let turns = list_turns(&self.db, owner, session_id).await?;
        let events = list_events(&self.db, owner, session_id, 0).await?;
        fork::write_transcript(
            std::path::Path::new(&workspace.worktree_path),
            &session,
            &turns,
            &events,
        )
        .await
        .map_err(|err| ServerError::internal(format!("could not write the fork transcript: {err}")))
    }

    pub(crate) async fn workspace_tree(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        query: &str,
        limit: Option<u32>,
    ) -> Result<(Vec<String>, bool), ServerError> {
        let workspace = self.get_workspace(owner, workspace_id).await?;
        worktree::list_tree_paths(
            std::path::Path::new(&workspace.worktree_path),
            query,
            limit.unwrap_or(worktree::DEFAULT_TREE_LIMIT),
        )
        .await
        .map_err(map_worktree)
    }

    pub(crate) async fn workspace_search(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        query: &str,
        include: &str,
        exclude: &str,
        limit: Option<u32>,
    ) -> Result<(Vec<worktree::WorktreeSearchMatch>, bool), ServerError> {
        let workspace = self.require_live_workspace(owner, workspace_id).await?;
        worktree::search_worktree_contents(
            std::path::Path::new(&workspace.worktree_path),
            query,
            include,
            exclude,
            limit.unwrap_or(worktree::DEFAULT_SEARCH_LIMIT),
        )
        .await
        .map_err(map_worktree)
    }

    pub(crate) async fn workspace_files(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        turn_id: Option<CodeTurnId>,
    ) -> Result<(Vec<ChangedFile>, bool, Diffstat, Option<CodeTurnId>), ServerError> {
        let workspace = self.get_workspace(owner, workspace_id).await?;
        let (worktree, from, to, turn) = resolve_diff_range(&self.db, &workspace, turn_id)
            .await
            .map_err(map_checkpoint)?;
        let listed = list_changed_files(&worktree, &from, &to, DiffBounds::default())
            .await
            .map_err(map_checkpoint)?;
        Ok((listed.files, listed.truncated, listed.stat, turn))
    }

    pub(crate) async fn workspace_blob(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        path: &str,
    ) -> Result<worktree::WorktreeBlob, ServerError> {
        let workspace = self.require_live_workspace(owner, workspace_id).await?;
        worktree::read_worktree_file(std::path::Path::new(&workspace.worktree_path), path)
            .await
            .map_err(map_worktree)
    }

    pub(crate) async fn workspace_diff(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        turn_id: Option<CodeTurnId>,
        file: Option<&str>,
    ) -> Result<(String, bool, Diffstat, Option<CodeTurnId>), ServerError> {
        let workspace = self.get_workspace(owner, workspace_id).await?;
        let (worktree, from, to, turn) = resolve_diff_range(&self.db, &workspace, turn_id)
            .await
            .map_err(map_checkpoint)?;
        let produced = produce_diff(&worktree, &from, &to, file, DiffBounds::default())
            .await
            .map_err(map_checkpoint)?;
        Ok((produced.diff, produced.truncated, produced.stat, turn))
    }

    async fn sweep_orphaned_checkpoints(&self) -> Result<(), ServerError> {
        for repo in list_repos_all_owners(&self.db).await? {
            self.sweep_repo_checkpoint_refs(
                &repo.owner,
                std::path::Path::new(&repo.root_path),
                repo.id,
            )
            .await?;
        }
        Ok(())
    }

    async fn sweep_repo_checkpoint_refs(
        &self,
        owner: &OwnerId,
        repo_root: &std::path::Path,
        repo_id: RepoId,
    ) -> Result<(), ServerError> {
        let live: Vec<WorkspaceId> = list_workspaces(&self.db, owner, Some(repo_id))
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
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        allow_running: bool,
    ) -> Result<(), ServerError> {
        if allow_running {
            return Ok(());
        }
        let sessions = list_sessions_for_workspace(&self.db, owner, workspace_id).await?;
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

    /// End every session in a workspace.
    ///
    /// Returns whether every worker confirmed it stopped. A `false` means at
    /// least one is still running somewhere with its own handle on the
    /// workspace's turn lock.
    async fn end_workspace_sessions(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
    ) -> Result<bool, ServerError> {
        let mut all_stopped = true;
        let sessions = list_sessions_for_workspace(&self.db, owner, workspace_id).await?;
        for mut session in sessions {
            if session.lifecycle == CodeSessionLifecycle::Ended {
                continue;
            }
            let handle = self
                .workers
                .lock()
                .expect("code workers")
                .remove(&session.id);
            self.revoke_browser_session(session.id);
            // Mark the row ended before asking the worker to stop. A worker
            // interrupted mid-turn re-reads the row on its way round the loop
            // and leaves on its own when it finds the session ended, so one
            // `Shutdown` is enough however busy it was.
            session.lifecycle = CodeSessionLifecycle::Ended;
            session.child_pid = None;
            session.fence_reason = None;
            super::attention::persist_session(&self.db, &self.bus, &session).await?;
            if let Some(handle) = handle {
                all_stopped &= Self::shut_down_worker(session.id, handle).await;
            }
            // The outgoing worker still holds this epoch, so a persist during
            // the wait can overwrite Ended. Re-assert from a fresh load.
            let mut current = self.get_session(owner, session.id).await?;
            current.lifecycle = CodeSessionLifecycle::Ended;
            current.child_pid = None;
            current.fence_reason = None;
            if !super::attention::persist_session(&self.db, &self.bus, &current).await? {
                return Err(ServerError::conflict_kind(
                    "session_not_ended",
                    "the session did not stay ended after the worker stopped",
                ));
            }
        }
        Ok(all_stopped)
    }

    /// Ask a superseded worker to stop, and wait for its command receiver to
    /// close — that is the last thing the worker does. One `Shutdown` is
    /// enough; resending would only replay `interrupt` into an engine that is
    /// already stopping. The wait is bounded so a wedged engine cannot hold
    /// an archive or a reap open.
    ///
    /// Returns whether the worker confirmed it was gone. That answer matters
    /// to the caller: a worker that outlived its grace still holds this
    /// workspace's turn lock, so anything keyed on "the checkout is now
    /// unowned" has to stay put.
    async fn shut_down_worker(id: CodeSessionId, handle: WorkerHandle) -> bool {
        const GRACE: std::time::Duration = std::time::Duration::from_secs(5);
        let commands = handle.commands.clone();
        drop(handle);
        // Send and wait under one deadline: a worker that has stopped draining
        // its commands would otherwise park the send itself once the channel
        // filled up.
        let stopped = tokio::time::timeout(GRACE, async {
            let _ = commands.send(WorkerCommand::Shutdown).await;
            commands.closed().await;
        })
        .await
        .is_ok();
        if !stopped {
            tracing::warn!(
                session = %id,
                "code-mode: session worker did not stop in time; continuing without it"
            );
        }
        stopped
    }

    async fn attach_and_spawn_worker(
        &self,
        session: CodeSession,
    ) -> Result<CodeSession, ServerError> {
        let workspace = self
            .get_workspace(&session.owner, session.workspace_id)
            .await?;
        let adapter = self.adapter(session.harness_kind)?;
        // Cached, so the probe `create_session` already paid for is not paid
        // again on the way into the worker.
        let probe = self.probe(adapter.as_ref()).await;
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
            session.owner.clone(),
            session.id,
            attached.spawn_epoch,
            None,
            attached.subagents.clone(),
        );
        let approval = self.approval_channel(session.id, session.permission_mode);

        // Mint a browser channel only when both halves are present: the
        // native BrowserRuntime (the desktop adapter) and the trusted
        // bridge executable (the CLI sidecar). If either is absent, browser
        // stays None — no browser tools are advertised or injected, and the
        // session works exactly as before the browser channel existed.
        let browser = match (
            self.browser_runtime.as_ref(),
            self.browser_bridge_command.as_ref(),
        ) {
            (Some(_runtime), Some(bridge)) => {
                let browser_subject = BrowserSubject {
                    owner: session.owner.clone(),
                    workspace: session.workspace_id,
                    session: session.id,
                };
                Some(
                    self.browser_tokens
                        .issue(browser_subject, bridge)
                        .map_err(ServerError::internal)?,
                )
            }
            _ => None,
        };


        let spec = SessionSpec {
            worktree: PathBuf::from(&workspace.worktree_path),
            permission_mode: session.permission_mode,
            model: session.model.clone(),
            resume_ref: session.harness_resume_ref.clone(),
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            env: probe.env.clone(),
            approval,
            binary,
            sink: sink.clone() as Arc<dyn HarnessEventSink>,
            browser,

        };
        let mut attached = attached;
        let engine = match adapter.launch(spec).await {
            Ok(engine) => engine,
            Err(HarnessError::ResumeLost(detail)) => {
                self.revoke_browser_session(session.id);
                // The engine refused the stored resume ref. Fence with a
                // reason the UI can explain — the fence drops the dead ref, so
                // a reap re-attaches with a fresh engine session.
                recovery::fence_session(
                    &self.db,
                    &self.bus,
                    &mut attached,
                    FenceReason::ResumeLost {
                        detail: detail.clone(),
                    },
                )
                .await?;
                return Err(ServerError::conflict_kind(
                    "session_resume_lost",
                    format!("the engine no longer has this session: {detail}"),
                ));
            }
            Err(err) => {
                self.revoke_browser_session(session.id);
                return Err(ServerError::internal(format!(
                    "failed to launch engine session: {err}"
                )));
            }
        };
        attached.child_pid = engine.child_pid();
        if let Some(resume) = engine.resume_ref().or(session.harness_resume_ref.clone()) {
            attached.harness_resume_ref = Some(resume);
        }
        super::attention::persist_session(&self.db, &self.bus, &attached).await?;
        let handle = spawn_session_worker(
            attached.clone(),
            engine,
            sink,
            Some(self.blobs.clone()),
            self.worktree_turn_lock(attached.workspace_id),
        );
        self.workers
            .lock()
            .expect("code workers")
            .insert(session.id, handle);
        let pending = list_approvals(
            &self.db,
            &attached.owner,
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

    /// The turn lock for a workspace, minted on first use.
    ///
    /// Every session in the workspace hands the same `Arc` to its worker, so
    /// the lock outlives any one session and a worker recovered after a
    /// restart rejoins the same queue. See record 55.
    pub(super) fn worktree_turn_lock(
        &self,
        workspace_id: WorkspaceId,
    ) -> Arc<tokio::sync::Mutex<()>> {
        self.worktree_turns
            .lock()
            .expect("worktree turn locks")
            .entry(workspace_id)
            .or_default()
            .clone()
    }

    fn require_worker(&self, id: CodeSessionId) -> Result<WorkerHandle, ServerError> {
        self.workers
            .lock()
            .expect("code workers")
            .get(&id)
            .map(|handle| WorkerHandle {
                spawn_epoch: handle.spawn_epoch,
                commands: handle.commands.clone(),
                queue: handle.queue.clone(),
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
        owner: &OwnerId,
        state: Option<CodeApprovalState>,
        session_id: Option<CodeSessionId>,
    ) -> Result<Vec<CodeApproval>, ServerError> {
        Ok(list_approvals(&self.db, owner, state, session_id).await?)
    }

    pub(crate) async fn get_approval(
        &self,
        owner: &OwnerId,
        id: CodeApprovalId,
    ) -> Result<CodeApproval, ServerError> {
        get_approval(&self.db, owner, id)
            .await?
            .ok_or_else(|| ServerError::not_found(format!("approval {id} not found")))
    }

    pub(crate) async fn decide_approval(
        &self,
        owner: &OwnerId,
        id: CodeApprovalId,
        decision: ApprovalDecision,
    ) -> Result<CodeApproval, ServerError> {
        let mut approval = self.get_approval(owner, id).await?;
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
        if !save_approval(&self.db, owner, &approval).await? {
            return Err(ServerError::not_found(format!("approval {id} not found")));
        }
        let session = self.get_session(owner, approval.session_id).await?;
        journal_event(
            &self.db,
            &self.bus,
            owner,
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
            owner,
            Some(CodeApprovalState::Pending),
            Some(approval.session_id),
        )
        .await?;
        if still_pending.is_empty() {
            let _ = super::attention::apply_attention(
                &self.db,
                &self.bus,
                owner,
                approval.session_id,
                Attention::working(AttentionSource::Lifecycle),
                false,
            )
            .await;
        }
        Ok(approval)
    }
}

fn parse_turn_media_type(value: &str) -> Option<tidebreak_core::ImageMediaType> {
    if let Some(parsed) = tidebreak_core::ImageMediaType::parse(value) {
        return Some(parsed);
    }
    match value.trim().to_ascii_lowercase().as_str() {
        "png" => Some(tidebreak_core::ImageMediaType::Png),
        "jpeg" | "jpg" => Some(tidebreak_core::ImageMediaType::Jpeg),
        "webp" => Some(tidebreak_core::ImageMediaType::Webp),
        "gif" => Some(tidebreak_core::ImageMediaType::Gif),
        _ => None,
    }
}

fn normalize_model(model: Option<String>) -> Option<String> {
    model.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    })
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
        CodePermissionMode::Allow => caps.allow_mode == CapLevel::Supported,
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
        CodePermissionMode::Allow => format!(
            "{harness} cannot honor {mode}: an allow-all posture is {}",
            caps.allow_mode.as_str()
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
        GhError::MergeBlocked(message) => ServerError::conflict_kind("pr_not_mergeable", message),
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
        WorkerError::NoActiveTurn(message) => ServerError::conflict_kind("no_active_turn", message),
        WorkerError::StaleTurn(message) => ServerError::conflict_kind("stale_turn", message),
        WorkerError::SteeringUnavailable(message) => {
            ServerError::unprocessable_kind("steering_unavailable", message)
        }
        WorkerError::SteeringRejected(message) => {
            ServerError::conflict_kind("steering_rejected", message)
        }
        WorkerError::Failed(message) => ServerError::internal(message),
        // `submit_turn` intercepts this and parks the message instead, so
        // reaching here means a caller took the turn path without handling
        // contention. Answer as a conflict rather than a 500.
        WorkerError::WorktreeBusy => ServerError::conflict_kind(
            "workspace_busy",
            "another session in this workspace is mid-turn",
        ),
    }
}

impl From<WorktreeError> for ServerError {
    fn from(err: WorktreeError) -> Self {
        map_worktree(err)
    }
}

#[cfg(test)]
mod probe_freshness_tests {
    use super::*;

    fn probe_at(path: Option<&str>) -> HarnessProbe {
        HarnessProbe {
            found: path.is_some(),
            binary_path: path.map(PathBuf::from),
            version: Some("2.1.234".into()),
            authenticated: Some(true),
            stderr: String::new(),
            env: Vec::new(),
            commands: Vec::new(),
        }
    }

    #[test]
    fn a_probe_of_the_installed_binary_is_still_current() {
        let installed =
            Path::new("/data/tools/harnesses/claude_code/2.1.234/node_modules/.bin/claude");
        assert!(probe_describes(
            Some(&probe_at(installed.to_str())),
            installed
        ));
    }

    #[test]
    fn a_pin_bump_moves_the_path_and_stales_the_probe() {
        let previous = probe_at(Some(
            "/data/tools/harnesses/claude_code/2.1.233/node_modules/.bin/claude",
        ));
        assert!(!probe_describes(
            Some(&previous),
            Path::new("/data/tools/harnesses/claude_code/2.1.234/node_modules/.bin/claude")
        ));
    }

    #[test]
    fn no_probe_and_a_probe_that_found_nothing_are_both_stale() {
        let installed = Path::new("/data/tools/harnesses/codex/0.147.0/node_modules/.bin/codex");
        assert!(!probe_describes(None, installed));
        assert!(!probe_describes(Some(&probe_at(None)), installed));
    }
}

#[cfg(test)]
mod managed_node_wait_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tidebreak_code_execution::{HostDep, HostToolBroker, HostToolStatus};

    struct RecordingBroker {
        ensure_calls: AtomicUsize,
        retry_calls: AtomicUsize,
        root: PathBuf,
    }

    #[async_trait::async_trait]
    impl HostToolBroker for RecordingBroker {
        fn ensure(&self, tool: HostDep) {
            assert_eq!(tool, HostDep::Node);
            self.ensure_calls.fetch_add(1, Ordering::Relaxed);
        }

        fn retry(&self, tool: HostDep) {
            assert_eq!(tool, HostDep::Node);
            self.retry_calls.fetch_add(1, Ordering::Relaxed);
        }

        async fn status(&self, tool: HostDep) -> HostToolStatus {
            assert_eq!(tool, HostDep::Node);
            HostToolStatus::Available
        }

        async fn managed_root(&self, tool: HostDep) -> Option<PathBuf> {
            assert_eq!(tool, HostDep::Node);
            Some(self.root.clone())
        }
    }

    /// Reproduces the Doctor Refresh race: `retry` has been called, but the
    /// first `status` still reports the remembered failure from the previous
    /// attempt. That Unavailable must not fail the wait while startup grace
    /// is open.
    struct StaleFailureThenReadyBroker {
        retry_calls: AtomicUsize,
        polls: AtomicUsize,
        root: PathBuf,
    }

    #[async_trait::async_trait]
    impl HostToolBroker for StaleFailureThenReadyBroker {
        fn ensure(&self, tool: HostDep) {
            assert_eq!(tool, HostDep::Node);
        }

        fn retry(&self, tool: HostDep) {
            assert_eq!(tool, HostDep::Node);
            self.retry_calls.fetch_add(1, Ordering::Relaxed);
        }

        async fn status(&self, tool: HostDep) -> HostToolStatus {
            assert_eq!(tool, HostDep::Node);
            match self.polls.fetch_add(1, Ordering::Relaxed) {
                0 => HostToolStatus::Unavailable(
                    "the previous Node install failed: disk full".into(),
                ),
                1 => HostToolStatus::Installing,
                _ => HostToolStatus::Available,
            }
        }

        async fn managed_root(&self, tool: HostDep) -> Option<PathBuf> {
            assert_eq!(tool, HostDep::Node);
            Some(self.root.clone())
        }
    }

    #[tokio::test]
    async fn explicit_harness_refresh_retries_node_provisioning() {
        let broker = RecordingBroker {
            ensure_calls: AtomicUsize::new(0),
            retry_calls: AtomicUsize::new(0),
            root: PathBuf::from("/verified/node"),
        };
        let root = wait_for_managed_node(&broker, true, Duration::from_secs(1), Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(root, PathBuf::from("/verified/node"));
        assert_eq!(broker.ensure_calls.load(Ordering::Relaxed), 0);
        assert_eq!(broker.retry_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn retry_does_not_fail_on_a_stale_unavailable_during_startup_grace() {
        let broker = StaleFailureThenReadyBroker {
            retry_calls: AtomicUsize::new(0),
            polls: AtomicUsize::new(0),
            root: PathBuf::from("/verified/node"),
        };
        let root = wait_for_managed_node(
            &broker,
            true,
            Duration::from_secs(1),
            Duration::from_secs(2),
        )
        .await
        .expect("stale failure during grace is in-progress, not terminal");

        assert_eq!(root, PathBuf::from("/verified/node"));
        assert_eq!(broker.retry_calls.load(Ordering::Relaxed), 1);
        assert!(broker.polls.load(Ordering::Relaxed) >= 3);
    }

    #[cfg(any(
        all(
            target_os = "macos",
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        all(
            target_os = "linux",
            any(target_arch = "aarch64", target_arch = "x86_64")
        )
    ))]
    #[tokio::test]
    async fn headless_runtime_reuses_a_verified_node_for_an_existing_pinned_harness() {
        use std::os::unix::fs::PermissionsExt as _;
        use tidebreak_code_execution::managed_node::{
            current_managed_node_pin, managed_node_install_marker, managed_node_marker_path,
            managed_node_version_dir,
        };

        let data_dir = tempfile::tempdir().expect("data dir");
        let node_pin = current_managed_node_pin().expect("supported test platform");
        let node_root = managed_node_version_dir(data_dir.path());
        let node_bin = node_root.join("bin");
        std::fs::create_dir_all(&node_bin).expect("node bin");
        for name in ["node", "npm"] {
            let path = node_bin.join(name);
            std::fs::write(&path, b"#!/bin/sh\n").expect("node entrypoint");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("node entrypoint mode");
        }
        std::fs::write(
            managed_node_marker_path(&node_root),
            managed_node_install_marker(node_pin).expect("node marker"),
        )
        .expect("write node marker");

        let harness = HarnessKind::ClaudeCode;
        let harness_pin = tidebreak_harness::pin_for(harness).expect("harness pin");
        let harness_dir = tidebreak_harness::pin::install_dir(data_dir.path(), harness_pin);
        let harness_binary = harness_dir
            .join("node_modules")
            .join(".bin")
            .join(harness_pin.bin);
        std::fs::create_dir_all(harness_binary.parent().expect("harness parent"))
            .expect("harness bin");
        std::fs::write(&harness_binary, b"#!/usr/bin/env node\n").expect("harness binary");
        std::fs::set_permissions(&harness_binary, std::fs::Permissions::from_mode(0o755))
            .expect("harness mode");
        std::fs::write(
            harness_dir.join("installed.json"),
            serde_json::to_vec(&serde_json::json!({
                "package": harness_pin.package,
                "version": harness_pin.version,
            }))
            .expect("harness marker"),
        )
        .expect("write harness marker");

        let db = DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            data_dir.path().join("headless-code.db").display()
        ))
        .await
        .expect("db");
        let runtime = CodeRuntime::new(
            Arc::new(db),
            data_dir.path().to_path_buf(),
            None,
            None,
            None,
            None,

        );

        assert_eq!(
            runtime.managed_node_root(false).await.expect("node root"),
            node_root
        );
        assert_eq!(
            runtime
                .ensure_pinned_harness(harness, false)
                .await
                .expect("existing harness"),
            harness_binary
        );
    }
}
