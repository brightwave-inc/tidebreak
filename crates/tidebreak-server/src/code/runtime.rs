//! Process-wide code-mode runtime: adapters, workers, worktrees, recovery.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::sync::{oneshot, watch};
use tokio::time::Instant as TokioInstant;

use tidebreak_core::db::code::{
    abandon_pending_approval, arm_trigger, begin_permission_mode_change,
    cancel_permission_mode_change, claim_approval, compare_and_set_workspace_status,
    complete_workspace_archive, confirm_permission_mode_change, delete_trigger, delete_workspace,
    discard_permission_mode_change, fence_permission_mode_change, get_approval, get_open_turn,
    get_repo, get_repo_by_root_path, get_session, get_workspace, insert_repo, insert_session,
    insert_workspace, list_approvals, list_events, list_fork_events,
    list_pending_permission_mode_changes, list_repos, list_sessions, list_sessions_all_owners,
    list_sessions_for_workspace, list_triggers_for_repo, list_turns, list_workspaces,
    list_workspaces_by_status_all_owners, mark_repo_removed, queued_turn_head,
    replace_session_execution_settings, save_repo, save_workspace,
    set_active_workspace_pull_request, set_workspace_branch_if, settle_approval_claim,
    update_trigger_enabled, ClaimedApprovalSettlement, CodeSessionExecutionSettings,
    PermissionModeChangeIntent, MAX_REPLAY_EVENTS,
};
use tidebreak_core::{
    ApprovalDecisionKind, Attention, AttentionSource, CapLevel, CodeApproval, CodeApprovalId,
    CodeApprovalState, CodeEvent, CodeQueuedTurn, CodeRepo, CodeSession, CodeSessionId,
    CodeSessionKind, CodeSessionLifecycle, CodeTrigger, CodeTriggerAction, CodeTriggerCondition,
    CodeTriggerId, CodeTurn, CodeTurnId, CodeWorkspace, CodeWorkspaceStatus, DbStore, Diffstat,
    FenceReason, HarnessKind, OwnerId, PermissionMode, PullRequestDigest, QuickAction,
    ReasoningEffort, RepoId, WorkspaceId,
};
use tidebreak_harness::{
    builtin_registry, AdapterRegistry, ApprovalChannelSpec, ApprovalDecision, HarnessAdapter,
    HarnessApprovalCapability, HarnessApprovalRef, HarnessError, HarnessEventSink, HarnessProbe,
    HostEnv, SessionSpec,
};

use super::approval_bridge::ApprovalBridge;
use super::browser_channel::{BrowserSubject, BrowserTokenRegistry};
use super::bus::CodeEventBus;
use super::checkpoint::{
    delete_workspace_refs, list_changed_files, produce_diff, record_session_baseline,
    resolve_diff_range, ChangedFile, CheckpointError, DiffBounds,
};
use super::ci_logs;
use super::clone::CloneJobs;
use super::delivery::DeliveryCache;
use super::fork;
use super::gh::{self, ActionOutcome, CommitOutcome, GhError, PushOutcome, WorkspaceGitStatus};
use super::harness_install::HarnessInstallJobs;
use super::naming_settings;
use super::recovery::{self, RecoveryAction};
use super::session_worker::{
    attach_engine, spawn_session_worker, wake_queue, AttachmentStore, ExecutionSettingsSettlement,
    PermissionModeSettlement, TriggerDeliveryClaim, WorkerCommand, WorkerError, WorkerHandle,
};
#[cfg(windows)]
use super::worktree::repo_paths_equivalent;
use super::worktree::{
    self, archive_blockers, branch_name, create_worktree, directory_bytes, prune_worktrees,
    remove_worktree, rename_local_only_branch, run_archive_script, run_setup_script, slugify,
    unique_branch_bytes, validate_repo_path, worktree_dir, WorktreeError,
};
use crate::error::ServerError;
use crate::managed_policy::ManagedPolicy;
use crate::routes::code::types::{
    CodeDeliveryPullRequestTarget, CodeGitHubRepositoryTarget, CodeRepoStorageSnapshot,
    CodeStorageAction, CodeStorageSnapshot, CodeWorkspaceStorageSnapshot,
};

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
    /// Set only by the clone path: the remote the checkout came from, which
    /// is what makes the directory Tidebreak's to reclaim later.
    pub cloned_from: Option<String>,
    pub display_name: Option<String>,
    pub default_base_ref: Option<String>,
    pub branch_prefix: Option<String>,
    pub setup_script: Option<String>,
    pub archive_script: Option<String>,
    /// Named commands workspaces of this repo offer. Validated by the caller.
    pub quick_actions: Vec<QuickAction>,
}

/// Result of `POST /code/sessions/{id}/turns`.
pub(crate) enum SubmitTurnOutcome {
    /// The session was idle; the turn ran to a terminal event.
    Ran(Box<CodeTurn>),
    /// The session or its workspace was busy; the message parked as a
    /// durable queue row (decision 69).
    Queued(Box<CodeQueuedTurn>),
    /// The durable trigger delivery behind this submit was already accepted
    /// by an earlier attempt; nothing new was written.
    AlreadyDelivered,
}

/// Result of one external message delivery (`docs/slack-sessions.md`,
/// stage 2). A replayed delivery answers with the same shape the first one
/// earned, derived from the row's current state.
#[derive(Debug)]
pub(crate) enum ExternalMessageOutcome {
    /// The message became a running turn.
    NewTurn(Box<CodeTurn>),
    /// The session was busy; the message sits as a durable queue row.
    Queued(Box<CodeQueuedTurn>),
    /// The row the first delivery caused was retracted before it could
    /// run; the replay has nothing to point at.
    Dropped,
}

enum LivePermissionModeOutcome {
    Unavailable,
    RelaunchRequired,
    Acknowledged(LivePermissionModeChange),
}

struct LivePermissionModeChange {
    settlement: oneshot::Sender<PermissionModeSettlement>,
    handle: WorkerHandle,
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
    /// Where this embedding places clones when no `code_clone_parent_dir`
    /// is stored. The self-host profile sets `{data_dir}/code/src` so a
    /// hosted machine never asks a caller to name a path they cannot see
    /// (decision 70).
    pub(crate) clone_parent_default: Option<PathBuf>,
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
    /// Per-caller git-forge lending on a gateway-authenticated hosted
    /// machine (decision 63). `None` everywhere else: a machine with its own
    /// `git`/`gh` configuration authenticates however the operator says.
    git_credentials: Option<Arc<dyn crate::obo_gateway::GitCredentialLender>>,
    /// Per-session engine inference relay on a gateway-authenticated hosted
    /// machine (decision 71). `None` everywhere else: a machine whose engines
    /// carry their own provider credentials keeps using them.
    harness_llm: Option<Arc<super::harness_llm::HarnessLlmRelay>>,
    /// Managed-policy-aware gateway catalog on a local machine. Hosted
    /// machines use `harness_llm` because their catalog belongs to the caller.
    gateway_runtime: Option<Arc<crate::gateway_runtime::GatewayRuntime>>,
    /// The configured remote-session context, on a deployment with a sandbox
    /// runtime endpoint (`docs/slack-sessions.md`). `None` everywhere else;
    /// remote workspaces then refuse turns rather than half-running.
    remote: Option<Arc<super::remote::service::RemoteSessions>>,
    /// Live fan-out for adapter-grant revocations, so an event stream
    /// holding a revoked grant drops immediately (docs/slack-sessions.md).
    pub(crate) grant_revocations: Arc<super::grants::GrantRevocations>,
    loopback_base: Mutex<Option<String>>,
    /// Memoized harness probes, one per kind. See [`CodeRuntime::probe`].
    probes: Mutex<HashMap<HarnessKind, HarnessProbe>>,
    /// Last pin-install failure per kind. Cleared on a successful install.
    pin_install_errors: Mutex<HashMap<HarnessKind, String>>,
    workers: Mutex<HashMap<CodeSessionId, WorkerHandle>>,
    /// Flips true while the process quiesces for a restart-to-update. Every
    /// session worker subscribes: the flag holds queue drains, refuses new
    /// turn starts at the worktree boundary, and parks idle engine children
    /// immediately. See `crate::update_quiesce`.
    update_quiesce: watch::Sender<bool>,
    /// Serializes writer admission with workspace lifecycle transitions.
    workspace_lifecycles: Mutex<HashMap<WorkspaceId, Arc<tokio::sync::Mutex<()>>>>,
    /// One turn at a time per worktree, shared by every session in it.
    ///
    /// Sessions in a workspace share one checkout, so their turns cannot
    /// overlap: two harnesses editing the same files, and two checkpoints
    /// racing for `.git/index.lock`, is corruption rather than concurrency.
    /// A worker takes the workspace's lock for the length of a turn, so a
    /// sibling's turn starts after this one ends. See record 55.
    worktree_turns: Mutex<HashMap<WorkspaceId, Arc<tokio::sync::Mutex<()>>>>,
    /// Workspaces whose digest was requested recently, with the owner each
    /// belongs to: the hot tier the refresher walks (decision 66). Shared by
    /// handle so the post-turn fact detector marks its own pushes hot.
    hot_prs: super::pr_refresh::HotPullRequests,
    /// Per-owner delivery nudge debounce, including one pending trailing edge.
    delivery_nudges: DeliveryNudgeDebounce,
    /// Paces and parks every conditional GitHub read (decision 66).
    pub(crate) host_gate: super::pr_fetch::HostGate,
    /// Base-branch rules, cached per branch: whether a merge queue runs
    /// decides if the timeline read is worth paying, and the required
    /// approval count anchors the review-decision word (decision 66).
    branch_rules: Mutex<HashMap<String, CachedBranchRules>>,
    pub(crate) delivery_cache: DeliveryCache,
    pub(crate) clone_jobs: CloneJobs,
    /// Warm harness installs started ahead of a session create.
    pub(super) harness_installs: HarnessInstallJobs,
    #[cfg(test)]
    pub(crate) gh_search_path: Mutex<Option<String>>,
    /// Forge REST base override, so tests point decision 65's reads at a
    /// loopback fake while the lending gate still names github.com.
    #[cfg(test)]
    forge_api_base: Mutex<Option<String>>,
    stall_sweep: Mutex<Option<super::attention::StallSweepGuard>>,
    stall_started: AtomicBool,
    watch_sweep: Mutex<Option<super::watch::WatchSweepGuard>>,
    watch_started: AtomicBool,
    trigger_sweep: Mutex<Option<super::trigger::TriggerSweepGuard>>,
    trigger_started: AtomicBool,
    reconcile_sweep: Mutex<Option<super::reconcile::ReconcileSweepGuard>>,
    reconcile_started: AtomicBool,
    pr_refresh_sweep: Mutex<Option<super::pr_refresh::PrRefreshGuard>>,
    pr_refresh_started: AtomicBool,
    remote_sweep: Mutex<Option<super::remote::service::RemoteSweepGuard>>,
    remote_started: AtomicBool,
    /// Workspaces with a background naming call in flight.
    ///
    /// One call per workspace at a time; a second trigger is dropped rather
    /// than queued, because the next turn on a still-unnamed workspace retries
    /// anyway (`super::titling`).
    pub(super) titling_in_flight: Mutex<std::collections::HashSet<tidebreak_core::WorkspaceId>>,
    /// Derives a recap for each completed turn (`super::recap`).
    ///
    /// Installed after construction rather than taken in `new`, because it
    /// reads the model handles the app state owns and this runtime is built
    /// first. `None` until then, and in deployments that install none.
    recap: Mutex<Option<Arc<dyn super::recap::TurnRecap>>>,
    /// Derives a lucid rewrite of each completed turn's closing message
    /// (`super::rewrite`). Installed the same way as recap.
    rewrite: Mutex<Option<Arc<dyn super::rewrite::TurnRewrite>>>,
    #[cfg(test)]
    archive_shutdown_timeout: AtomicBool,
    #[cfg(test)]
    fail_next_workspace_final_save: AtomicBool,
}

/// How long one base branch's rules answer stands before the next read.
/// Rules change on the order of releases, not pushes.
const BRANCH_RULES_TTL: Duration = Duration::from_secs(3600);

/// Floor between two delivery nudges to one owner (decision 66): a sweep
/// updating many rows collapses to one re-read on the other side.
const DELIVERY_NUDGE_DEBOUNCE: Duration = Duration::from_secs(3);

#[derive(Default)]
struct DeliveryNudgeDebounce {
    owners: Arc<Mutex<HashMap<OwnerId, DeliveryNudgeState>>>,
}

/// The exact merge target the runtime accepted plus the refreshed workspace
/// status after the host action.
pub(crate) struct WorkspaceMergeOutcome {
    pub target: CodeDeliveryPullRequestTarget,
    pub accepted_head_sha: String,
    pub status: WorkspaceGitStatus,
}

struct DeliveryNudgeState {
    sent_at: TokioInstant,
    trailing_at: Option<TokioInstant>,
}

impl DeliveryNudgeDebounce {
    fn publish(&self, bus: &Arc<CodeEventBus>, owner: &OwnerId) {
        let now = TokioInstant::now();
        let trailing_at = {
            let mut owners = self.owners.lock().expect("delivery nudges");
            match owners.get_mut(owner) {
                Some(state) if now.duration_since(state.sent_at) < DELIVERY_NUDGE_DEBOUNCE => {
                    if state.trailing_at.is_some() {
                        return;
                    }
                    let deadline = state.sent_at + DELIVERY_NUDGE_DEBOUNCE;
                    state.trailing_at = Some(deadline);
                    Some(deadline)
                }
                Some(state) => {
                    state.sent_at = now;
                    state.trailing_at = None;
                    None
                }
                None => {
                    owners.insert(
                        owner.clone(),
                        DeliveryNudgeState {
                            sent_at: now,
                            trailing_at: None,
                        },
                    );
                    None
                }
            }
        };
        let Some(trailing_at) = trailing_at else {
            bus.publish_update(owner, super::bus::CodeLiveUpdate::Delivery);
            return;
        };

        let owner = owner.clone();
        let owners = Arc::downgrade(&self.owners);
        let bus = Arc::downgrade(bus);
        tokio::spawn(async move {
            tokio::time::sleep_until(trailing_at).await;
            let (Some(owners), Some(bus)) = (owners.upgrade(), bus.upgrade()) else {
                return;
            };
            let publish = {
                let mut owners = owners.lock().expect("delivery nudges");
                let Some(state) = owners.get_mut(&owner) else {
                    return;
                };
                if state.trailing_at != Some(trailing_at) {
                    false
                } else {
                    state.sent_at = TokioInstant::now();
                    state.trailing_at = None;
                    true
                }
            };
            if publish {
                bus.publish_update(&owner, super::bus::CodeLiveUpdate::Delivery);
            }
        });
    }
}

/// One cached branch-rules answer. `rules: None` records a host that has no
/// rules endpoint, so a known 404 is not re-read every refresh.
struct CachedBranchRules {
    fetched_at: Instant,
    rules: Option<super::pr_fetch::BranchRules>,
}

/// What a new session starts on, beyond the engine it is bound to.
///
/// One value rather than three parameters: a caller sets all of them together,
/// and the routes that move them mid-conversation move them one at a time
/// against the same session row.
#[derive(Debug, Clone, Default)]
pub(crate) struct NewSessionSettings {
    pub permission_mode: PermissionMode,
    pub model: Option<String>,
    /// `None` leaves the engine's own default in force.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Whether the session starts armed for the engine's fast mode.
    pub fast_mode: bool,
    /// The managed permission-mode ceiling in force at create, when one is
    /// asserted. Copied from resolved policy so this path can refuse a
    /// ceiling that permits no mode the engine offers, after probe, on the
    /// same ceiling the route already clamps against.
    pub permission_mode_ceiling: Option<PermissionMode>,
}

struct SelectedModelCapabilities {
    reasoning_efforts: Vec<ReasoningEffort>,
    /// The selected row's own ladder, when the engine listed that row.
    listed_model_reasoning_efforts: Option<Vec<ReasoningEffort>>,
    reasoning_known: bool,
    fast_mode: bool,
    fast_mode_known: bool,
}

impl SelectedModelCapabilities {
    fn supports_reasoning(&self, effort: ReasoningEffort) -> bool {
        self.reasoning_efforts.contains(&effort)
    }

    fn deactivate_unsupported(&self, settings: &mut CodeSessionExecutionSettings) {
        if self.reasoning_known
            && settings
                .reasoning_effort
                .is_some_and(|effort| !self.supports_reasoning(effort))
        {
            settings.reasoning_effort = None;
        }
        if self.fast_mode_known && settings.fast_mode && !self.fast_mode {
            settings.fast_mode = false;
        }
    }
}

impl CodeRuntime {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        db: Arc<DbStore>,
        data_dir: PathBuf,
        worktree_root_default: Option<PathBuf>,
        host_tool_broker: Option<Arc<dyn tidebreak_code_execution::HostToolBroker>>,
        browser_runtime: Option<Arc<dyn crate::code::browser_runtime::BrowserRuntime>>,
        browser_bridge_command: Option<PathBuf>,
        git_credentials: Option<Arc<dyn crate::obo_gateway::GitCredentialLender>>,
        harness_llm: Option<Arc<super::harness_llm::HarnessLlmRelay>>,
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
            clone_parent_default: None,
            approvals: ApprovalBridge::new(),
            browser_tokens,
            browser_runtime,
            browser_bridge_command,
            host: HostEnv {
                data_dir: Some(data_dir),
                ..HostEnv::from_process()
            },
            host_tool_broker,
            git_credentials,
            harness_llm,
            gateway_runtime: None,
            remote: None,
            grant_revocations: Arc::new(super::grants::GrantRevocations::default()),
            loopback_base: Mutex::new(None),
            probes: Mutex::new(HashMap::new()),
            pin_install_errors: Mutex::new(HashMap::new()),
            workers: Mutex::new(HashMap::new()),
            update_quiesce: watch::channel(false).0,
            workspace_lifecycles: Mutex::new(HashMap::new()),
            worktree_turns: Mutex::new(HashMap::new()),
            hot_prs: super::pr_refresh::HotPullRequests::default(),
            delivery_nudges: DeliveryNudgeDebounce::default(),
            host_gate: super::pr_fetch::HostGate::default(),
            branch_rules: Mutex::new(HashMap::new()),
            delivery_cache: DeliveryCache::default(),
            clone_jobs: CloneJobs::default(),
            harness_installs: HarnessInstallJobs::default(),
            #[cfg(test)]
            gh_search_path: Mutex::new(None),
            #[cfg(test)]
            forge_api_base: Mutex::new(None),
            stall_sweep: Mutex::new(None),
            stall_started: AtomicBool::new(false),
            watch_sweep: Mutex::new(None),
            watch_started: AtomicBool::new(false),
            trigger_sweep: Mutex::new(None),
            trigger_started: AtomicBool::new(false),
            reconcile_sweep: Mutex::new(None),
            reconcile_started: AtomicBool::new(false),
            pr_refresh_sweep: Mutex::new(None),
            pr_refresh_started: AtomicBool::new(false),
            remote_sweep: Mutex::new(None),
            remote_started: AtomicBool::new(false),
            titling_in_flight: Mutex::new(std::collections::HashSet::new()),
            recap: Mutex::new(None),
            rewrite: Mutex::new(None),
            #[cfg(test)]
            archive_shutdown_timeout: AtomicBool::new(false),
            #[cfg(test)]
            fail_next_workspace_final_save: AtomicBool::new(false),
        }
    }

    /// Bound after listen so Claude can be pointed at the loopback MCP route.
    fn set_loopback_base(&self, base: String) {
        self.browser_tokens.set_loopback_base(&base);
        *self.loopback_base.lock().expect("loopback base") =
            Some(base.trim_end_matches('/').into());
    }

    /// The per-caller git-forge lender, on a machine that has one.
    pub(crate) fn git_credentials(
        &self,
    ) -> Option<&Arc<dyn crate::obo_gateway::GitCredentialLender>> {
        self.git_credentials.as_ref()
    }

    /// The engine inference relay, on a machine that has one.
    pub(crate) fn harness_llm(&self) -> Option<Arc<super::harness_llm::HarnessLlmRelay>> {
        self.harness_llm.clone()
    }

    /// The gateway model snapshot that applies to this caller. Hosted
    /// machines read the caller's catalog; local machines use the synced
    /// deployment snapshot.
    pub(crate) async fn gateway_model_snapshot(
        &self,
        owner: &OwnerId,
    ) -> Option<crate::providers::GatewayModelSnapshot> {
        if let Some(relay) = self.harness_llm() {
            return relay.catalog(owner).await.ok().flatten();
        }
        let gateway = self.gateway_runtime.as_ref()?;
        gateway.model_snapshot().await.ok().flatten()
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
            runtime.ensure_trigger_sweep();
            runtime.ensure_reconcile_sweep();
            runtime.ensure_pr_refresh_sweep();
            runtime.ensure_remote_sweep();
            actions
        })
    }

    #[cfg(test)]
    pub(crate) fn with_registry(
        db: Arc<DbStore>,
        data_dir: PathBuf,
        adapters: AdapterRegistry,
    ) -> Self {
        Self::with_registry_and_browser_runtime(db, data_dir, adapters, None, None)
    }

    #[cfg(test)]
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
            clone_parent_default: None,
            approvals: ApprovalBridge::new(),
            browser_tokens,
            browser_runtime,
            browser_bridge_command,
            host: HostEnv::from_process(),
            host_tool_broker: None,
            git_credentials: None,
            harness_llm: None,
            gateway_runtime: None,
            remote: None,
            grant_revocations: Arc::new(super::grants::GrantRevocations::default()),
            loopback_base: Mutex::new(None),
            probes: Mutex::new(HashMap::new()),
            pin_install_errors: Mutex::new(HashMap::new()),
            workers: Mutex::new(HashMap::new()),
            update_quiesce: watch::channel(false).0,
            workspace_lifecycles: Mutex::new(HashMap::new()),
            worktree_turns: Mutex::new(HashMap::new()),
            hot_prs: super::pr_refresh::HotPullRequests::default(),
            delivery_nudges: DeliveryNudgeDebounce::default(),
            host_gate: super::pr_fetch::HostGate::default(),
            branch_rules: Mutex::new(HashMap::new()),
            delivery_cache: DeliveryCache::default(),
            clone_jobs: CloneJobs::default(),
            harness_installs: HarnessInstallJobs::default(),
            #[cfg(test)]
            gh_search_path: Mutex::new(None),
            #[cfg(test)]
            forge_api_base: Mutex::new(None),
            stall_sweep: Mutex::new(None),
            stall_started: AtomicBool::new(false),
            watch_sweep: Mutex::new(None),
            watch_started: AtomicBool::new(false),
            trigger_sweep: Mutex::new(None),
            trigger_started: AtomicBool::new(false),
            reconcile_sweep: Mutex::new(None),
            reconcile_started: AtomicBool::new(false),
            pr_refresh_sweep: Mutex::new(None),
            pr_refresh_started: AtomicBool::new(false),
            remote_sweep: Mutex::new(None),
            remote_started: AtomicBool::new(false),
            titling_in_flight: Mutex::new(std::collections::HashSet::new()),
            recap: Mutex::new(None),
            rewrite: Mutex::new(None),
            #[cfg(test)]
            archive_shutdown_timeout: AtomicBool::new(false),
            #[cfg(test)]
            fail_next_workspace_final_save: AtomicBool::new(false),
        }
    }

    /// Lend gateway git credentials from tests, in place of the on-behalf-of
    /// handle a hosted deployment wires in.
    #[cfg(test)]
    pub(crate) fn with_git_credentials(
        mut self,
        lender: Arc<dyn crate::obo_gateway::GitCredentialLender>,
    ) -> Self {
        self.git_credentials = Some(lender);
        self
    }

    /// Wire the engine inference relay from tests, standing in for the
    /// on-behalf-of handle a hosted deployment constructs.
    #[cfg(test)]
    pub(crate) fn with_harness_llm(
        mut self,
        relay: Arc<super::harness_llm::HarnessLlmRelay>,
    ) -> Self {
        self.harness_llm = Some(relay);
        self
    }

    /// Wire the managed-policy-aware gateway catalog used by local engines.
    pub(crate) fn with_gateway_runtime(
        mut self,
        gateway: Arc<crate::gateway_runtime::GatewayRuntime>,
    ) -> Self {
        self.gateway_runtime = Some(gateway);
        self
    }

    pub(crate) fn with_clone_parent_default(mut self, parent: PathBuf) -> Self {
        self.clone_parent_default = Some(parent);
        self
    }

    /// Enable remote sessions with a configured transport and settings.
    pub(crate) fn with_remote_sessions(
        mut self,
        remote: Arc<super::remote::service::RemoteSessions>,
    ) -> Self {
        self.remote = Some(remote);
        self
    }

    /// The remote-session context, when this deployment configured one.
    pub(crate) fn remote_sessions(&self) -> Option<Arc<super::remote::service::RemoteSessions>> {
        self.remote.clone()
    }

    #[cfg(test)]
    pub(crate) fn set_gh_search_path(&self, path: Option<String>) {
        *self.gh_search_path.lock().expect("gh search path") = path;
    }

    #[cfg(test)]
    pub(crate) fn set_forge_api_base(&self, base: Option<String>) {
        *self.forge_api_base.lock().expect("forge api base") = base;
    }

    #[cfg(test)]
    pub(crate) fn set_archive_shutdown_timeout(&self, enabled: bool) {
        self.archive_shutdown_timeout
            .store(enabled, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_workspace_final_save(&self) {
        self.fail_next_workspace_final_save
            .store(true, Ordering::SeqCst);
    }

    /// The REST base decision 65's pull-request operations drive: the
    /// forge's own, or a test override.
    pub(crate) fn forge_api_base_for(&self, host: &str) -> String {
        #[cfg(test)]
        {
            if let Some(base) = self.forge_api_base.lock().expect("forge api base").clone() {
                return base;
            }
        }
        super::forge_rest::default_api_base(host)
    }

    /// Return the installed browser adapter, if any.
    pub(crate) fn browser_runtime(
        &self,
    ) -> Option<Arc<dyn crate::code::browser_runtime::BrowserRuntime>> {
        self.browser_runtime.clone()
    }

    /// Install the hook that recaps each completed turn (`super::recap`).
    pub(crate) fn install_recap(&self, recap: Arc<dyn super::recap::TurnRecap>) {
        *self.recap.lock().expect("code recap hook") = Some(recap);
    }

    /// The installed recap hook, if any. Cloned per session sink so a later
    /// install never has to reach sinks that are already running.
    fn recap_hook(&self) -> Option<Arc<dyn super::recap::TurnRecap>> {
        self.recap.lock().expect("code recap hook").clone()
    }

    /// Install the hook that rewrites each completed turn (`super::rewrite`).
    pub(crate) fn install_rewrite(&self, rewrite: Arc<dyn super::rewrite::TurnRewrite>) {
        *self.rewrite.lock().expect("code rewrite hook") = Some(rewrite);
    }

    fn rewrite_hook(&self) -> Option<Arc<dyn super::rewrite::TurnRewrite>> {
        self.rewrite.lock().expect("code rewrite hook").clone()
    }

    /// Revoke the session browser token and permanently tombstone its native
    /// adapter authority. Idempotent — safe to call multiple times.
    ///
    /// Terminal paths pass the database-backed session scope explicitly so
    /// the adapter is still tombstoned when a prior launch failure already
    /// removed the transient bearer token and capfile.
    fn revoke_browser_session(&self, session: &CodeSession) {
        self.browser_tokens.revoke(session.id);
        self.approvals.revoke_session(session.id);
        if let Some(relay) = &self.harness_llm {
            relay.revoke(session.id);
        }
        if let Some(runtime) = &self.browser_runtime {
            let scope = crate::code::browser_runtime::BrowserRuntimeScope {
                owner: session.owner.clone(),
                workspace: session.workspace_id,
                session: session.id,
            };
            runtime.revoke_session(&scope);
        }
    }

    /// Revoke only the outgoing worker's transient browser and approval channels.
    ///
    /// Reap and launch-failure paths replace a worker while preserving the
    /// same logical code session. Its native browser capability therefore
    /// stays live and can be reused by the fresh channel. Only terminal end
    /// paths call [`Self::revoke_browser_session`] and plant the adapter's
    /// enduring tombstone.
    fn revoke_worker_channels(&self, session_id: CodeSessionId) {
        self.browser_tokens.revoke(session_id);
        self.approvals.revoke_session(session_id);
        if let Some(relay) = &self.harness_llm {
            relay.revoke(session_id);
        }
    }

    pub(crate) async fn recover(&self) -> Result<Vec<RecoveryAction>, ServerError> {
        self.ensure_stall_sweep();
        let mut actions = Vec::new();
        let mut recovery_owners = list_sessions_all_owners(&self.db)
            .await?
            .into_iter()
            .map(|session| session.owner)
            .collect::<Vec<_>>();
        recovery_owners.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        recovery_owners.dedup();
        for owner in &recovery_owners {
            for pending in list_pending_permission_mode_changes(&self.db, owner).await? {
                let reason = FenceReason::ProbeAmbiguous {
                    detail: format!(
                        "permission mode change from {} to {} stopped before revision {} committed",
                        pending.intent.previous_mode,
                        pending.intent.requested_mode,
                        pending.intent.revision
                    ),
                };
                if let Some(fenced) =
                    fence_permission_mode_change(&self.db, owner, &pending.intent, &reason).await?
                {
                    super::attention::emit_digest(&self.db, &self.bus, &fenced).await;
                    actions.push(RecoveryAction::Fenced {
                        session: fenced.id.to_string(),
                    });
                } else {
                    let _ =
                        discard_permission_mode_change(&self.db, owner, &pending.intent).await?;
                }
            }
        }
        actions.extend(
            recovery::recover_running_sessions(&self.db, &self.bus)
                .await
                .map_err(ServerError::from)?,
        );
        for workspace in
            list_workspaces_by_status_all_owners(&self.db, CodeWorkspaceStatus::Archiving).await?
        {
            let sessions =
                list_sessions_for_workspace(&self.db, &workspace.owner, workspace.id).await?;
            if sessions.iter().any(|session| {
                matches!(
                    session.lifecycle,
                    CodeSessionLifecycle::Running | CodeSessionLifecycle::Fenced
                )
            }) {
                tracing::warn!(
                    workspace = %workspace.id,
                    "code-mode: archive recovery kept lifecycle exclusion because a worker may still exist"
                );
                continue;
            }
            if !std::path::Path::new(&workspace.worktree_path).exists() {
                let repo = self.get_repo(&workspace.owner, workspace.repo_id).await?;
                match self.finalize_removed_workspace(workspace, &repo).await {
                    Ok(workspace) => self.forget_workspace_turn_lock(workspace.id),
                    Err(error) => {
                        tracing::warn!(
                            "code-mode: archive recovery could not finalize a missing checkout: {}",
                            error.message()
                        );
                    }
                }
                continue;
            }
            if !compare_and_set_workspace_status(
                &self.db,
                &workspace.owner,
                workspace.id,
                CodeWorkspaceStatus::Archiving,
                CodeWorkspaceStatus::Active,
            )
            .await?
            {
                tracing::warn!(
                    workspace = %workspace.id,
                    "code-mode: archive recovery lost its lifecycle compare-and-set"
                );
            }
        }
        let recovered_sessions = list_sessions_all_owners(&self.db).await?;
        for session in &recovered_sessions {
            super::approval_sweep::abandon_for_restart(
                &self.db,
                &self.bus,
                &session.owner,
                session.id,
                session.spawn_epoch,
            )
            .await;
            self.refresh_approval_attention(&session.owner, session.id)
                .await;
        }
        // Do not sweep repository-wide checkpoint refs here. Another
        // Tidebreak profile can manage the same repository from a separate
        // database, so this process cannot identify global orphans safely.
        // Recovery only mutates rows. Re-attach a worker for every session
        // that is still usable so submit_turn is not stuck after a restart.
        // Concurrently: each attach launches an engine child, so a serial pass
        // charged app launch the sum of every restored session.
        // Remote sessions have no local worker to re-attach: their engine
        // lives in a sandbox, and spawning a host harness against the empty
        // worktree path would fail loudly for a session that is healthy.
        let mut remote_workspaces = std::collections::HashSet::new();
        let mut checked_workspaces = std::collections::HashSet::new();
        for session in &recovered_sessions {
            if !checked_workspaces.insert(session.workspace_id) {
                continue;
            }
            if let Ok(workspace) = self
                .get_workspace(&session.owner, session.workspace_id)
                .await
            {
                if workspace.is_remote() {
                    remote_workspaces.insert(session.workspace_id);
                }
            }
        }
        let resumable: Vec<CodeSession> = recovered_sessions
            .into_iter()
            .filter(|session| {
                !matches!(
                    session.lifecycle,
                    CodeSessionLifecycle::Ended | CodeSessionLifecycle::Fenced
                ) && !remote_workspaces.contains(&session.workspace_id)
                    && !self
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
        self.probe_uncached(adapter).await
    }

    /// The probe for session create: a cached signed-out observation that
    /// would refuse is stale.
    ///
    /// Signing in or adding a provider override does not invalidate the
    /// doctor's cache. Re-using that answer here would refuse a repaired
    /// machine — the false refusal this path exists to avoid. A signed-in
    /// or unverified cache, or a signed-out cache the relay or an override
    /// already carries, still hits.
    async fn probe_for_session_create(&self, adapter: &dyn HarnessAdapter) -> HarnessProbe {
        let kind = adapter.kind();
        let cached = self
            .probes
            .lock()
            .expect("harness probes")
            .get(&kind)
            .cloned();
        if let Some(probe) = cached {
            let would_refuse =
                Self::signed_out_harness_refusal(self.harness_llm.is_some(), kind, &probe).is_err();
            if !would_refuse {
                return probe;
            }
            self.probes.lock().expect("harness probes").remove(&kind);
        }
        self.probe(adapter).await
    }

    async fn probe_uncached(&self, adapter: &dyn HarnessAdapter) -> HarnessProbe {
        let kind = adapter.kind();
        let mut host = self.host.clone();
        host.managed_node_root = match self.host_tool_broker.as_deref() {
            Some(broker) => {
                broker
                    .managed_root(tidebreak_code_execution::HostDep::Node)
                    .await
            }
            None => tidebreak_managed_node::managed_node_root(&self.data_dir),
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
            None => tidebreak_managed_node::managed_node_root(&self.data_dir)
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
            cloned_from,
            display_name,
            default_base_ref,
            branch_prefix,
            setup_script,
            archive_script,
            quick_actions,
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
        let default_branch_prefix = self.default_branch_prefix(owner).await;
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
                .unwrap_or(default_branch_prefix),
            setup_script,
            archive_script,
            quick_actions,
            created_at: Utc::now(),
            removed_at: None,
            cloned_from,
            origin_host: None,
            origin_owner: None,
            origin_name: None,
        };
        insert_repo(&self.db, &repo).await?;
        self.delivery_cache.invalidate_owner(owner);
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

    /// Refuse work that materializes a checkout for a removed registration.
    ///
    /// Reading a removed repository stays allowed: its archived workspaces and
    /// their transcripts resolve through it, which is the reason the row is
    /// kept at all.
    fn refuse_removed_repo(repo: &CodeRepo) -> Result<(), ServerError> {
        if repo.removed_at.is_some() {
            return Err(ServerError::conflict_kind(
                "repo_removed",
                "this repository registration was removed; register it again to start new work",
            ));
        }
        Ok(())
    }

    /// Remove a repository registration, keeping every archived workspace and
    /// transcript that hangs off it.
    ///
    /// The row survives on purpose. Hard-deleting it strands that history: on
    /// SQLite the workspace foreign key is not enforced, so the rows stay
    /// behind with nothing to reach them through, and on PostgreSQL it is
    /// enforced, so the delete fails against the very archived workspaces this
    /// path requires. Reclaiming the checkout on disk is a separate act with
    /// its own confirmation.
    pub(crate) async fn remove_repo(
        &self,
        owner: &OwnerId,
        id: RepoId,
        reclaim_checkout: bool,
    ) -> Result<(), ServerError> {
        let repo = self.get_repo(owner, id).await?;
        let workspaces = list_workspaces(&self.db, owner, Some(id)).await?;
        if workspaces.iter().any(|workspace| {
            !matches!(
                workspace.status,
                CodeWorkspaceStatus::Archived | CodeWorkspaceStatus::Released
            )
        }) {
            return Err(ServerError::conflict_kind(
                "repo_has_workspaces",
                "archive every workspace before removing the repository",
            ));
        }
        if reclaim_checkout {
            // Only a checkout Tidebreak cloned is Tidebreak's to delete. A
            // registration names a directory the user already had, and the
            // clone parent is a setting that moves, so there is no path test
            // that stays honest — the recorded origin is the only safe
            // signal.
            if repo.cloned_from.is_none() {
                return Err(ServerError::conflict_kind(
                    "checkout_not_reclaimable",
                    "Tidebreak did not clone this repository, so it will not delete the directory; \
                     remove the registration and delete the checkout yourself",
                ));
            }
            let root = PathBuf::from(&repo.root_path);
            // Re-validate rather than trusting the stored path: a row written
            // long ago must not turn into a recursive delete of whatever
            // occupies that path now.
            match validate_repo_path(&root).await {
                // `root_path` was stored as the canonical toplevel, so a
                // repository still rooted there resolves back to the same
                // path. Anything else — a nested checkout, a different repo
                // moved in — compares unequal and is left alone.
                Ok(validated) if validated.toplevel == root => {
                    tokio::fs::remove_dir_all(&root).await.map_err(|error| {
                        ServerError::internal(format!(
                            "could not remove the cloned checkout {}: {error}",
                            root.display()
                        ))
                    })?;
                    tracing::info!(repo = %repo.id, "code-mode: reclaimed a cloned checkout");
                }
                _ => {
                    return Err(ServerError::conflict_kind(
                        "checkout_not_a_repository",
                        format!(
                            "{} is no longer the git repository Tidebreak cloned; \
                             it was left alone",
                            root.display()
                        ),
                    ));
                }
            }
        }
        if !mark_repo_removed(&self.db, owner, id, Utc::now()).await? {
            return Err(ServerError::not_found(format!("repo {id} not found")));
        }
        self.delivery_cache.invalidate_owner(owner);
        Ok(())
    }

    /// Create a workspace whose checkout lives in a sandbox, not on this
    /// machine. A per-workspace `remote:<id>` worktree marker records that
    /// state ([`CodeWorkspace::is_remote`]); nothing here touches the
    /// filesystem.
    ///
    /// The authenticated remote-workspace route exposes this owner-scoped
    /// runtime path.
    pub(crate) async fn create_remote_workspace(
        &self,
        owner: &OwnerId,
        repo_id: RepoId,
        title: Option<String>,
    ) -> Result<CodeWorkspace, ServerError> {
        if self.remote.is_none() {
            return Err(ServerError::conflict_kind(
                "remote_disabled",
                "this deployment has no sandbox runtime configured",
            ));
        }
        let repo = self.get_repo(owner, repo_id).await?;
        Self::refuse_removed_repo(&repo)?;
        if repo.origin_host.is_none() || repo.origin_owner.is_none() || repo.origin_name.is_none() {
            return Err(ServerError::conflict_kind(
                "repo_origin_unknown",
                "the repository records no origin, so a sandbox cannot clone it",
            ));
        }
        let workspace = self.build_remote_workspace(owner, &repo, title).await?;
        insert_workspace(&self.db, &workspace).await?;
        Ok(workspace)
    }

    /// Validate and shape a remote workspace value without inserting it, so
    /// a caller can commit it atomically with the rows that depend on it.
    async fn build_remote_workspace(
        &self,
        owner: &OwnerId,
        repo: &CodeRepo,
        title: Option<String>,
    ) -> Result<CodeWorkspace, ServerError> {
        let title = title
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        let id = WorkspaceId::new();
        let branch = branch_name(&repo.branch_prefix, &title, id.0.as_u128());
        let existing = list_workspaces(&self.db, owner, Some(repo.id)).await?;
        if existing
            .iter()
            .any(|workspace| workspace.branch_name == branch)
        {
            return Err(ServerError::conflict_kind(
                "branch_collision",
                format!("branch {branch} already exists on this repository"),
            ));
        }
        Ok(CodeWorkspace {
            id,
            owner: owner.clone(),
            repo_id: repo.id,
            title,
            worktree_path: CodeWorkspace::remote_worktree_marker(id),
            branch_name: branch,
            base_ref: repo.default_base_ref.clone(),
            status: CodeWorkspaceStatus::Active,
            pr: None,
            created_at: Utc::now(),
            archived_at: None,
            released_at: None,
            released_tip: None,
            bundle_bytes: None,
        })
    }

    /// Shape a remote session value bound to `workspace`, uninserted.
    fn remote_session_value(
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        harness: HarnessKind,
        settings: NewSessionSettings,
    ) -> CodeSession {
        CodeSession {
            id: CodeSessionId::new(),
            owner: owner.clone(),
            workspace_id,
            kind: CodeSessionKind::Interactive,
            harness_kind: harness,
            harness_version: None,
            harness_resume_ref: None,
            permission_mode: settings.permission_mode,
            model: normalize_model(settings.model),
            reasoning_effort: settings.reasoning_effort,
            fast_mode: false,
            lifecycle: CodeSessionLifecycle::Idle,
            fence_reason: None,
            child_pid: None,
            child_process_identity: None,
            spawn_epoch: 1,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: Utc::now(),
        }
    }

    /// Bind an external conversation to a session, creating the remote
    /// workspace, session, and binding together on first contact
    /// (docs/slack-sessions.md, stage 2).
    ///
    /// Idempotent across the channel's retries: a bound conversation
    /// answers with its session, an ended one answers `Ended` rather than
    /// resurrecting, and a binding under another grant refuses. Two racing
    /// creates converge on one session through the binding's unique
    /// conversation key.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn external_get_or_create(
        &self,
        owner: &OwnerId,
        grant_id: tidebreak_core::CodeGrantId,
        channel_kind: &str,
        external_key: &str,
        repo_id: RepoId,
        title: Option<String>,
        harness: HarnessKind,
        settings: NewSessionSettings,
    ) -> Result<tidebreak_core::ExternalSessionResolution, ServerError> {
        if self.remote.is_none() {
            return Err(ServerError::conflict_kind(
                "remote_disabled",
                "this deployment has no sandbox runtime configured",
            ));
        }
        if channel_kind.trim().is_empty() || external_key.trim().is_empty() {
            return Err(ServerError::conflict_kind(
                "binding_key_invalid",
                "a binding needs a channel kind and a conversation key",
            ));
        }
        // The fast path costs one read and builds nothing.
        if let Some(binding) = tidebreak_core::db::code::get_external_binding(
            &self.db,
            owner,
            channel_kind,
            external_key,
        )
        .await?
        {
            if binding.grant_id != grant_id {
                return Ok(tidebreak_core::ExternalSessionResolution::GrantMismatch);
            }
            let session = self.get_session(owner, binding.session_id).await?;
            if session.lifecycle == CodeSessionLifecycle::Ended {
                return Ok(tidebreak_core::ExternalSessionResolution::Ended {
                    session_id: binding.session_id,
                });
            }
            return Ok(tidebreak_core::ExternalSessionResolution::Existing(
                Box::new(binding),
            ));
        }
        let repo = self.get_repo(owner, repo_id).await?;
        Self::refuse_removed_repo(&repo)?;
        if repo.origin_host.is_none() || repo.origin_owner.is_none() || repo.origin_name.is_none() {
            return Err(ServerError::conflict_kind(
                "repo_origin_unknown",
                "the repository records no origin, so a sandbox cannot clone it",
            ));
        }
        let workspace = self.build_remote_workspace(owner, &repo, title).await?;
        let session = Self::remote_session_value(owner, workspace.id, harness, settings);
        Ok(tidebreak_core::db::code::resolve_external_session(
            &self.db,
            owner,
            grant_id,
            channel_kind,
            external_key,
            &workspace,
            &session,
        )
        .await?)
    }

    /// Create a session on a remote workspace: a row and nothing else. No
    /// local harness is probed or spawned — the sandbox carries the engine,
    /// and the first turn provisions it.
    ///
    /// The authenticated remote-session route exposes this owner-scoped
    /// runtime path.
    pub(crate) async fn create_remote_session(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        harness: HarnessKind,
        settings: NewSessionSettings,
    ) -> Result<CodeSession, ServerError> {
        if self.remote.is_none() {
            return Err(ServerError::conflict_kind(
                "remote_disabled",
                "this deployment has no sandbox runtime configured",
            ));
        }
        let lifecycle = self.workspace_lifecycle_lock(workspace_id);
        let _lifecycle_guard = lifecycle.lock().await;
        let workspace = self.get_workspace(owner, workspace_id).await?;
        if !workspace.is_remote() {
            return Err(ServerError::conflict_kind(
                "workspace_not_remote",
                "this workspace has a local checkout; create a local session on it",
            ));
        }
        if workspace.status != CodeWorkspaceStatus::Active {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                format!("workspace is {}", workspace.status.as_str()),
            ));
        }
        let session = Self::remote_session_value(owner, workspace_id, harness, settings);
        insert_session(&self.db, &session).await?;
        Ok(session)
    }

    pub(crate) async fn create_workspace(
        &self,
        owner: &OwnerId,
        repo_id: RepoId,
        title: Option<String>,
        base_ref: Option<String>,
    ) -> Result<CodeWorkspace, ServerError> {
        let repo = self.get_repo(owner, repo_id).await?;
        Self::refuse_removed_repo(&repo)?;
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
            released_at: None,
            released_tip: None,
            bundle_bytes: None,
        };
        insert_workspace(&self.db, &workspace).await?;
        let operation =
            match create_worktree(std::path::Path::new(&repo.root_path), &path, &branch, &base)
                .await
            {
                Ok(operation) => operation,
                Err(err) => {
                    let _ = delete_workspace(&self.db, owner, id).await;
                    return Err(map_worktree(err));
                }
            };
        // Before the setup script, which may itself commit: from here on,
        // anything this workspace commits should already carry the right
        // name.
        self.name_workspace_author(owner, &path).await;
        match run_setup_script(
            &path,
            std::path::Path::new(&repo.root_path),
            &workspace.title,
            repo.setup_script.as_deref(),
        )
        .await
        {
            Ok(()) => {
                workspace.status = CodeWorkspaceStatus::Active;
                match self.save_workspace_final(&workspace).await {
                    Ok(true) => operation.complete().await,
                    Ok(false) => {
                        operation.rollback().await;
                        return Err(ServerError::not_found(format!(
                            "workspace {} not found",
                            workspace.id
                        )));
                    }
                    Err(error) => {
                        operation.rollback().await;
                        return Err(error);
                    }
                }
                gh::run_auto_create_actions(&path, &repo.quick_actions).await;
                Ok(workspace)
            }
            Err(err) => {
                workspace.status = CodeWorkspaceStatus::SetupFailed;
                match self.save_workspace_final(&workspace).await {
                    Ok(true) => operation.complete().await,
                    Ok(false) => {
                        operation.rollback().await;
                        return Err(ServerError::not_found(format!(
                            "workspace {} not found",
                            workspace.id
                        )));
                    }
                    Err(error) => {
                        operation.rollback().await;
                        return Err(error);
                    }
                }
                Err(ServerError::unprocessable_kind(
                    "setup_failed",
                    err.to_string(),
                ))
            }
        }
    }

    async fn save_workspace_final(&self, workspace: &CodeWorkspace) -> Result<bool, ServerError> {
        #[cfg(test)]
        if self
            .fail_next_workspace_final_save
            .swap(false, Ordering::SeqCst)
        {
            return Err(ServerError::internal(
                "injected workspace lifecycle persistence failure",
            ));
        }
        Ok(save_workspace(&self.db, workspace).await?)
    }

    pub(crate) async fn list_workspaces(
        &self,
        owner: &OwnerId,
        repo_id: Option<RepoId>,
    ) -> Result<Vec<CodeWorkspace>, ServerError> {
        Ok(list_workspaces(&self.db, owner, repo_id).await?)
    }

    /// Bytes each repo and workspace currently occupy, and what the next
    /// reclaim tier would free.
    pub(crate) async fn storage_snapshot(
        &self,
        owner: &OwnerId,
    ) -> Result<CodeStorageSnapshot, ServerError> {
        let repos = list_repos(&self.db, owner).await?;
        let workspaces = list_workspaces(&self.db, owner, None).await?;
        let mut out = Vec::with_capacity(repos.len());
        for repo in repos {
            let members: Vec<&CodeWorkspace> = workspaces
                .iter()
                .filter(|workspace| workspace.repo_id == repo.id)
                .collect();
            let live = members.iter().any(|workspace| {
                !matches!(
                    workspace.status,
                    CodeWorkspaceStatus::Archived | CodeWorkspaceStatus::Released
                )
            });
            let clone_path = std::path::Path::new(&repo.root_path);
            let clone_bytes = if clone_path.exists() {
                i64::try_from(directory_bytes(clone_path).await).unwrap_or(i64::MAX)
            } else {
                0
            };
            let mut rows = Vec::with_capacity(members.len());
            for workspace in members {
                rows.push(self.workspace_storage(&repo, workspace).await);
            }
            out.push(CodeRepoStorageSnapshot {
                id: repo.id,
                display_name: repo.display_name,
                clone_bytes,
                clone_reclaimable: repo.cloned_from.is_some() && !live,
                workspaces: rows,
            });
        }
        Ok(CodeStorageSnapshot { repos: out })
    }

    async fn workspace_storage(
        &self,
        repo: &CodeRepo,
        workspace: &CodeWorkspace,
    ) -> CodeWorkspaceStorageSnapshot {
        let worktree = std::path::Path::new(&workspace.worktree_path);
        let worktree_bytes = if worktree.exists() {
            i64::try_from(directory_bytes(worktree).await).unwrap_or(i64::MAX)
        } else {
            0
        };
        let repo_root = std::path::Path::new(&repo.root_path);
        let branch_bytes = if workspace.status == CodeWorkspaceStatus::Archived {
            i64::try_from(
                unique_branch_bytes(repo_root, &workspace.base_ref, &workspace.branch_name).await,
            )
            .unwrap_or(i64::MAX)
        } else {
            0
        };
        let bundle_bytes = workspace.bundle_bytes.unwrap_or(0);
        let (on_disk_bytes, next_action, next_reclaim_bytes) = match workspace.status {
            CodeWorkspaceStatus::Released => (bundle_bytes, None, 0),
            CodeWorkspaceStatus::Archived => {
                (branch_bytes, Some(CodeStorageAction::Release), branch_bytes)
            }
            CodeWorkspaceStatus::Active => (
                worktree_bytes,
                Some(CodeStorageAction::Archive),
                worktree_bytes,
            ),
            CodeWorkspaceStatus::Creating
            | CodeWorkspaceStatus::SetupFailed
            | CodeWorkspaceStatus::Archiving => (worktree_bytes, None, 0),
        };
        CodeWorkspaceStorageSnapshot {
            id: workspace.id,
            title: workspace.title.clone(),
            status: workspace.status,
            on_disk_bytes,
            next_action,
            next_reclaim_bytes,
        }
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
        let lifecycle = self.workspace_lifecycle_lock(workspace.id);
        let _lifecycle_guard = lifecycle.lock().await;
        let current = self.get_workspace(&workspace.owner, workspace.id).await?;
        if current.status != workspace.status {
            return Err(ServerError::conflict_kind(
                "workspace_lifecycle_changed",
                format!(
                    "workspace became {} before the update was saved",
                    current.status.as_str()
                ),
            ));
        }
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

    /// Rename the untouched placeholder branch after background titling names
    /// the workspace. Every guard fails closed and leaves the original branch.
    pub(crate) async fn rename_generated_workspace_branch(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
        title: &str,
    ) -> Result<bool, ServerError> {
        if !naming_settings::auto_rename_branches(&*self.db, owner).await? {
            return Ok(false);
        }
        let turn = self.worktree_turn_lock(id);
        let _turn_guard = turn.lock().await;
        let lifecycle = self.workspace_lifecycle_lock(id);
        let _lifecycle_guard = lifecycle.lock().await;
        let workspace = self.get_workspace(owner, id).await?;
        if workspace.is_remote()
            || workspace.status != CodeWorkspaceStatus::Active
            || workspace.pr.is_some()
            || workspace.title != title
        {
            return Ok(false);
        }
        let repo = self.get_repo(owner, workspace.repo_id).await?;
        let expected = branch_name(&repo.branch_prefix, "", id.0.as_u128());
        if workspace.branch_name != expected {
            return Ok(false);
        }
        let next = branch_name(&repo.branch_prefix, title, id.0.as_u128());
        if next == expected {
            return Ok(false);
        }
        let path = std::path::Path::new(&workspace.worktree_path);
        if !rename_local_only_branch(path, &expected, &next)
            .await
            .map_err(map_worktree)?
        {
            return Ok(false);
        }
        if !set_workspace_branch_if(&self.db, owner, id, title, &expected, &next).await? {
            if let Err(error) = rename_local_only_branch(path, &next, &expected).await {
                tracing::error!(
                    workspace = %id,
                    error = %error,
                    "could not restore a branch after its workspace update lost the race"
                );
            }
            return Ok(false);
        }
        super::attention::emit_workspace_digests(&self.db, &self.bus, owner, id).await;
        Ok(true)
    }

    pub(crate) async fn archive_workspace(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
        force: bool,
        terminals: &super::terminal::TerminalHub,
    ) -> Result<CodeWorkspace, ServerError> {
        let mut workspace = self.get_workspace(owner, id).await?;
        if workspace.status == CodeWorkspaceStatus::Archived {
            return Ok(workspace);
        }
        let path = std::path::PathBuf::from(&workspace.worktree_path);
        if workspace.status == CodeWorkspaceStatus::Archiving && !path.exists() {
            let sessions = list_sessions_for_workspace(&self.db, owner, id).await?;
            if sessions.iter().any(|session| {
                matches!(
                    session.lifecycle,
                    CodeSessionLifecycle::Running | CodeSessionLifecycle::Fenced
                )
            }) {
                return Err(ServerError::conflict_kind(
                    "workspace_lifecycle_busy",
                    "a workspace worker may still be running",
                ));
            }
            let repo = self.get_repo(owner, workspace.repo_id).await?;
            let archived = self.finalize_removed_workspace(workspace, &repo).await?;
            self.forget_workspace_turn_lock(archived.id);
            return Ok(archived);
        }
        if workspace.status != CodeWorkspaceStatus::Active {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                format!("workspace is {}", workspace.status.as_str()),
            ));
        }
        let repo = self.get_repo(owner, workspace.repo_id).await?;
        // Blockers first: a refused archive must leave the workspace exactly as
        // it was, and running the hook script is not "exactly as it was".
        self.refuse_running_sessions(owner, id, force).await?;
        if path.exists() && !force {
            if let Some(block) = archive_blockers(&path, &workspace.base_ref)
                .await
                .map_err(map_worktree)?
            {
                return Err(ServerError::conflict_kind(
                    block.as_str(),
                    "workspace has uncommitted or unpushed work; pass force to discard it",
                ));
            }
        }
        {
            let lifecycle = self.workspace_lifecycle_lock(id);
            let _lifecycle_guard = lifecycle.lock().await;
            workspace = self.get_workspace(owner, id).await?;
            if workspace.status != CodeWorkspaceStatus::Active {
                return Err(ServerError::conflict_kind(
                    "workspace_lifecycle_busy",
                    format!("workspace is {}", workspace.status.as_str()),
                ));
            }
            self.refuse_running_sessions(owner, id, force).await?;
            if !compare_and_set_workspace_status(
                &self.db,
                owner,
                id,
                CodeWorkspaceStatus::Active,
                CodeWorkspaceStatus::Archiving,
            )
            .await?
            {
                return Err(ServerError::conflict_kind(
                    "workspace_lifecycle_busy",
                    "another workspace lifecycle operation started first",
                ));
            }
        }
        workspace.status = CodeWorkspaceStatus::Archiving;

        let archived = self
            .archive_workspace_exclusive(owner, workspace, repo, force, terminals)
            .await;
        if archived
            .as_ref()
            .is_err_and(|error| archive_failure_can_reopen(error) && path.exists())
            && !compare_and_set_workspace_status(
                &self.db,
                owner,
                id,
                CodeWorkspaceStatus::Archiving,
                CodeWorkspaceStatus::Active,
            )
            .await?
        {
            tracing::warn!(
                workspace = %id,
                "code-mode: failed to restore Active after a refused archive"
            );
        }
        archived
    }

    async fn archive_workspace_exclusive(
        &self,
        owner: &OwnerId,
        workspace: CodeWorkspace,
        repo: CodeRepo,
        force: bool,
        terminals: &super::terminal::TerminalHub,
    ) -> Result<CodeWorkspace, ServerError> {
        if !terminals.close_workspace_and_wait(workspace.id).await {
            return Err(ServerError::conflict_kind(
                "terminal_shutdown_timeout",
                "a workspace terminal did not stop; the checkout was preserved",
            ));
        }
        let workers_stopped = self.end_workspace_sessions(owner, workspace.id).await?;
        #[cfg(test)]
        let workers_stopped =
            workers_stopped && !self.archive_shutdown_timeout.load(Ordering::SeqCst);
        if !workers_stopped {
            return Err(ServerError::conflict_kind(
                "workspace_shutdown_timeout",
                "a workspace worker did not stop; the checkout was preserved",
            ));
        }

        if workspace.is_remote() {
            let archived_at = Utc::now();
            if !complete_workspace_archive(&self.db, owner, workspace.id, archived_at).await? {
                let current = self.get_workspace(owner, workspace.id).await?;
                if current.status == CodeWorkspaceStatus::Archived {
                    return Ok(current);
                }
                return Err(ServerError::conflict_kind(
                    "workspace_lifecycle_changed",
                    "the workspace row changed before archive completed",
                ));
            }
            let mut workspace = workspace;
            workspace.status = CodeWorkspaceStatus::Archived;
            workspace.archived_at = Some(archived_at);
            super::attention::emit_workspace_digests(&self.db, &self.bus, owner, workspace.id)
                .await;
            return Ok(workspace);
        }

        let turn = self.worktree_turn_lock(workspace.id);
        let _turn_guard = turn.lock().await;
        let current = self.get_workspace(owner, workspace.id).await?;
        if current.status != CodeWorkspaceStatus::Archiving {
            return Err(ServerError::conflict_kind(
                "workspace_lifecycle_changed",
                format!(
                    "workspace became {} during archive",
                    current.status.as_str()
                ),
            ));
        }

        let path = std::path::Path::new(&workspace.worktree_path);
        if path.exists() {
            // Decision 0032: the archive script obeys the same
            // failure-preserves rule as setup. Lifecycle exclusion starts
            // before the hook so no in-process writer can overlap it.
            if let Err(err) = run_archive_script(
                path,
                std::path::Path::new(&repo.root_path),
                &workspace.title,
                repo.archive_script.as_deref(),
            )
            .await
            {
                return Err(ServerError::unprocessable_kind(
                    "archive_script_failed",
                    err.to_string(),
                ));
            }
            if !force {
                if let Some(block) = archive_blockers(path, &workspace.base_ref)
                    .await
                    .map_err(map_worktree)?
                {
                    return Err(ServerError::conflict_kind(
                        block.as_str(),
                        "workspace changed during archive; the checkout was preserved",
                    ));
                }
            }
        }

        remove_worktree(std::path::Path::new(&repo.root_path), path)
            .await
            .map_err(map_worktree)?;
        let archived = self.finalize_removed_workspace(workspace, &repo).await?;
        drop(_turn_guard);
        self.forget_workspace_turn_lock(archived.id);
        Ok(archived)
    }

    async fn finalize_removed_workspace(
        &self,
        mut workspace: CodeWorkspace,
        repo: &CodeRepo,
    ) -> Result<CodeWorkspace, ServerError> {
        let _ = prune_worktrees(std::path::Path::new(&repo.root_path)).await;
        if let Err(error) =
            delete_workspace_refs(std::path::Path::new(&repo.root_path), workspace.id).await
        {
            tracing::warn!(
                workspace = %workspace.id,
                "code-mode: could not delete checkpoint refs on archive: {error}"
            );
        }
        let archived_at = workspace.archived_at.unwrap_or_else(Utc::now);
        if !complete_workspace_archive(&self.db, &workspace.owner, workspace.id, archived_at)
            .await?
        {
            let current = self.get_workspace(&workspace.owner, workspace.id).await?;
            if current.status == CodeWorkspaceStatus::Archived {
                return Ok(current);
            }
            return Err(ServerError::conflict_kind(
                "workspace_lifecycle_changed",
                "the workspace row changed before archive completed",
            ));
        }
        workspace.status = CodeWorkspaceStatus::Archived;
        workspace.archived_at = Some(archived_at);
        super::attention::emit_workspace_digests(
            &self.db,
            &self.bus,
            &workspace.owner,
            workspace.id,
        )
        .await;
        Ok(workspace)
    }

    fn forget_workspace_turn_lock(&self, workspace_id: WorkspaceId) {
        self.worktree_turns
            .lock()
            .expect("worktree turn locks")
            .remove(&workspace_id);
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
    /// Release an archived workspace: drop its branch, keep its commits.
    ///
    /// The deepest reclaim tier. Archive already removed the checkout, which
    /// is where the bytes are; what is left is the branch and the objects it
    /// holds alive. Bundling `base..branch` captures the work this workspace
    /// did — usually kilobytes against a checkout measured in gigabytes — so
    /// dropping the ref frees nearly everything and a restore still rebuilds
    /// exactly. The transcript is not touched at any tier.
    /// Drop a restored workspace's release bookkeeping.
    ///
    /// The bundle stays until the row carrying these cleared fields is durable.
    fn clear_release(workspace: &mut CodeWorkspace) {
        if workspace.released_at.is_none() && workspace.bundle_bytes.is_none() {
            return;
        }
        workspace.released_at = None;
        workspace.released_tip = None;
        workspace.bundle_bytes = None;
    }

    /// Remove a restored workspace's bundle after its final row is durable.
    fn remove_release_bundle(workspace: &CodeWorkspace, data_dir: &std::path::Path) {
        let bundle = worktree::bundle_path(data_dir, &workspace.id.0);
        if let Err(error) = std::fs::remove_file(&bundle) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    workspace = %workspace.id,
                    "code-mode: could not remove restored bundle {}: {error}",
                    bundle.display()
                );
            }
        }
    }

    pub(crate) async fn release_workspace(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
        force: bool,
    ) -> Result<CodeWorkspace, ServerError> {
        let mut workspace = self.get_workspace(owner, id).await?;
        if workspace.status == CodeWorkspaceStatus::Released {
            return Ok(workspace);
        }
        if workspace.status != CodeWorkspaceStatus::Archived {
            return Err(ServerError::conflict_kind(
                "workspace_not_archived",
                format!(
                    "archive the workspace before releasing it; it is {}",
                    workspace.status.as_str()
                ),
            ));
        }
        let repo = self.get_repo(owner, workspace.repo_id).await?;
        let repo_root = std::path::Path::new(&repo.root_path);

        // A branch that archive already lost is nothing to bundle. Record the
        // release anyway: the tier describes what is on disk, and refusing
        // here would strand the row in a state no action can leave.
        if !worktree::branch_exists(repo_root, &workspace.branch_name)
            .await
            .map_err(map_worktree)?
        {
            workspace.status = CodeWorkspaceStatus::Released;
            workspace.released_at = Some(Utc::now());
            save_workspace(&self.db, &workspace).await?;
            return Ok(workspace);
        }

        if !force
            && worktree::release_is_unmerged(repo_root, &workspace.base_ref, &workspace.branch_name)
                .await
                .map_err(map_worktree)?
        {
            return Err(ServerError::conflict_kind(
                "branch_unmerged",
                "this branch has commits the base does not; pass force to bundle and drop it",
            ));
        }

        let tip = worktree::branch_tip(repo_root, &workspace.branch_name)
            .await
            .map_err(map_worktree)?;
        let bundle = worktree::bundle_path(&self.data_dir, &workspace.id.0);
        let bytes = worktree::create_bundle(
            repo_root,
            &workspace.base_ref,
            &workspace.branch_name,
            &bundle,
        )
        .await
        .map_err(map_worktree)?;

        // Drop the ref only once the bundle is on disk and measured. The
        // ordering is the whole safety property: a failure above leaves an
        // archived workspace with its branch, which is exactly where it was.
        worktree::delete_branch(repo_root, &workspace.branch_name)
            .await
            .map_err(map_worktree)?;

        workspace.status = CodeWorkspaceStatus::Released;
        workspace.released_at = Some(Utc::now());
        workspace.released_tip = Some(tip);
        workspace.bundle_bytes = Some(i64::try_from(bytes).unwrap_or(i64::MAX));
        save_workspace(&self.db, &workspace).await?;
        super::attention::emit_workspace_digests(&self.db, &self.bus, owner, workspace.id).await;
        Ok(workspace)
    }

    pub(crate) async fn restore_workspace(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<CodeWorkspace, ServerError> {
        let lifecycle = self.workspace_lifecycle_lock(id);
        let _lifecycle_guard = lifecycle.lock().await;
        let mut workspace = self.get_workspace(owner, id).await?;
        if workspace.status == CodeWorkspaceStatus::Active {
            return Ok(workspace);
        }
        let released = workspace.status == CodeWorkspaceStatus::Released;
        if !released && workspace.status != CodeWorkspaceStatus::Archived {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                format!("workspace is {}", workspace.status.as_str()),
            ));
        }
        let repo = self.get_repo(owner, workspace.repo_id).await?;
        Self::refuse_removed_repo(&repo)?;
        let repo_root = std::path::Path::new(&repo.root_path);
        let path = std::path::Path::new(&workspace.worktree_path);
        let operation = if released {
            let released_tip = workspace.released_tip.as_deref().ok_or_else(|| {
                ServerError::conflict_kind(
                    "released_tip_missing",
                    "the released workspace has no recorded commit; its bundle was preserved",
                )
            })?;
            let bundle = worktree::bundle_path(&self.data_dir, &workspace.id.0);
            worktree::restore_released_worktree(
                repo_root,
                path,
                &workspace.branch_name,
                &bundle,
                released_tip,
            )
            .await
            .map_err(map_worktree)?
        } else {
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
            worktree::restore_worktree(repo_root, path, &workspace.branch_name)
                .await
                .map_err(map_worktree)?
        };
        // Mirror create's tail exactly: setup decides between Active and
        // SetupFailed, and a failing script preserves the checkout
        // (Decision 0032's failure-preserves rule). One vocabulary for both
        // paths — a reader debugging "setup_failed" should not need to know
        // whether the workspace was created or restored.
        let setup = run_setup_script(
            path,
            std::path::Path::new(&repo.root_path),
            &workspace.title,
            repo.setup_script.as_deref(),
        )
        .await;
        workspace.status = if setup.is_ok() {
            CodeWorkspaceStatus::Active
        } else {
            CodeWorkspaceStatus::SetupFailed
        };
        workspace.archived_at = None;
        if released {
            Self::clear_release(&mut workspace);
        }
        match self.save_workspace_final(&workspace).await {
            Ok(true) => {}
            Ok(false) => {
                operation.rollback().await;
                return Err(ServerError::not_found(format!(
                    "workspace {} not found",
                    workspace.id
                )));
            }
            Err(error) => {
                operation.rollback().await;
                return Err(error);
            }
        }
        operation.complete().await;
        if released {
            Self::remove_release_bundle(&workspace, &self.data_dir);
        }
        match setup {
            Ok(()) => Ok(workspace),
            Err(error) => Err(ServerError::unprocessable_kind(
                "setup_failed",
                error.to_string(),
            )),
        }
    }

    /// Re-run the setup script on a worktree that already exists.
    ///
    /// A failed setup keeps the checkout (Decision 0032), but every other
    /// route refuses a `setup_failed` workspace, so the state has no exit
    /// short of archiving the work. This is that exit: fix the script, run it
    /// again, and the workspace goes Active without cutting a second worktree.
    ///
    /// Both outcomes match create's tail — Active on success, still
    /// `SetupFailed` and a 422 `setup_failed` on failure.
    pub(crate) async fn retry_workspace_setup(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<CodeWorkspace, ServerError> {
        let lifecycle = self.workspace_lifecycle_lock(id);
        let _lifecycle_guard = lifecycle.lock().await;
        let mut workspace = self.get_workspace(owner, id).await?;
        if workspace.status == CodeWorkspaceStatus::Active {
            return Ok(workspace);
        }
        if workspace.status != CodeWorkspaceStatus::SetupFailed {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                format!("workspace is {}", workspace.status.as_str()),
            ));
        }
        let path = std::path::PathBuf::from(&workspace.worktree_path);
        if !path.exists() {
            return Err(ServerError::conflict_kind(
                "worktree_missing",
                "the worktree is gone; archive this workspace and restore it instead",
            ));
        }
        let repo = self.get_repo(owner, workspace.repo_id).await?;
        Self::refuse_removed_repo(&repo)?;
        match run_setup_script(
            &path,
            std::path::Path::new(&repo.root_path),
            &workspace.title,
            repo.setup_script.as_deref(),
        )
        .await
        {
            Ok(()) => {
                workspace.status = CodeWorkspaceStatus::Active;
                if !self.save_workspace_final(&workspace).await? {
                    return Err(ServerError::not_found(format!(
                        "workspace {} not found",
                        workspace.id
                    )));
                }
                gh::run_auto_create_actions(&path, &repo.quick_actions).await;
                Ok(workspace)
            }
            Err(error) => Err(ServerError::unprocessable_kind(
                "setup_failed",
                error.to_string(),
            )),
        }
    }

    pub(crate) async fn commit_workspace(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
        message: Option<String>,
    ) -> Result<CommitOutcome, ServerError> {
        let turn = self.worktree_turn_lock(id);
        let _turn_guard = turn.lock().await;
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
        let turn = self.worktree_turn_lock(id);
        let _turn_guard = turn.lock().await;
        let workspace = self.require_live_workspace(owner, id).await?;
        let worktree = std::path::PathBuf::from(&workspace.worktree_path);
        let credential = self.borrow_git_credential(owner, &worktree).await?;
        let outcome = gh::push_branch(&worktree, &workspace.branch_name, credential.as_ref())
            .await
            .map_err(map_gh)?;
        // The delivery lists hold the pre-push row.
        self.delivery_cache.invalidate();
        // Best-effort contributed fact (decision 77): a user push to a branch
        // that is a pull request's head is the same act the detector mints
        // for. Failures are silent; the reconcile sweep corrects. On a hosted
        // machine the read rides the forge REST API with the same credential
        // the push just used (decision 65) — one borrow, one operation.
        if let Ok(target) = super::delivery::repository_target_from_path(&worktree).await {
            let values = match credential.as_ref() {
                Some(credential) => super::forge_rest::list_pull_requests_for_head(
                    &self.forge_api_base_for(&target.host),
                    &target,
                    credential,
                    &workspace.branch_name,
                )
                .await
                .ok(),
                None => {
                    let gh_path = self.gh_search_path_owned();
                    gh::list_pull_requests_for_head_raw(
                        &target.host,
                        &target.owner,
                        &target.name,
                        &workspace.branch_name,
                        gh_path.as_deref(),
                    )
                    .await
                    .ok()
                }
            };
            if let Some(value) = values.as_ref().and_then(|values| values.first()) {
                super::pr_facts::record_confirmed_fact(
                    &self.db,
                    owner,
                    workspace.id,
                    None,
                    None,
                    &target,
                    value,
                    tidebreak_core::CodePullRequestRelation::Contributed,
                    tidebreak_core::CodePullRequestDiscovery::Command,
                )
                .await;
            }
        }
        // A push dirties the row: refresh it now (decision 66), so the next
        // reader sees checks pending on the new head rather than the
        // pre-push snapshot. The fetcher's checks read is keyed to the new
        // head by construction.
        self.refresh_workspace_pr_row(owner, id).await;
        Ok(outcome)
    }

    /// Name the caller on this workspace's commits (decision 65), on a
    /// machine that lends gateway git identities and only when the gateway
    /// states the caller's own account acts.
    ///
    /// Best-effort by design: a caller who has not connected, a bot-attributed
    /// deployment, and a machine with its own credentials all leave the
    /// checkout exactly as it is, and the commit path reports its own
    /// failures. An identity the gateway states without a commit email is
    /// incomplete and configures nothing — half an identity would be worse
    /// than the checkout's own.
    async fn name_workspace_author(&self, owner: &OwnerId, worktree: &std::path::Path) {
        let Some(lender) = self.git_credentials() else {
            return;
        };
        let Ok(identity) = lender.git_forge_identity(owner).await else {
            return;
        };
        let crate::obo_gateway::GitForgeAttribution::Person {
            login,
            display_name,
            commit_email,
        } = identity.attribution
        else {
            return;
        };
        let Some(email) = commit_email else {
            return;
        };
        let name = display_name.unwrap_or(login);
        if let Err(error) = gh::configure_workspace_identity(worktree, &name, &email).await {
            tracing::debug!(error, "the workspace git identity was not configured");
        }
    }

    /// The forge login used for account-prefixed branches, when one is known.
    pub(crate) async fn branch_account_name(&self, owner: &OwnerId) -> Option<String> {
        #[cfg(test)]
        // Tests must not inherit the developer machine's `gh` login.
        self.git_credentials()?;
        if let Some(lender) = self.git_credentials() {
            let identity = lender.git_forge_identity(owner).await.ok()?;
            return match identity.attribution {
                crate::obo_gateway::GitForgeAttribution::Person { login, .. } => Some(login),
                crate::obo_gateway::GitForgeAttribution::Bot { bot_login } => {
                    bot_login.or(Some(identity.app_name))
                }
            };
        }
        let search_path = self.gh_search_path_owned();
        gh::observe_gh(search_path.as_deref()).await.viewer_login
    }

    async fn default_branch_prefix(&self, owner: &OwnerId) -> String {
        let account = self.branch_account_name(owner).await;
        naming_settings::read(&*self.db, owner, account.as_deref())
            .await
            .map(|settings| settings.effective_branch_prefix)
            .unwrap_or_else(|_| "tidebreak/".to_owned())
    }

    /// Borrow a repository-scoped forge credential for one git operation in
    /// `worktree`, on a machine that lends them (decision 63). `Ok(None)` is
    /// every machine that does not — and every checkout whose origin
    /// [`forge_lending_target`] rules out: those operations carry no
    /// credential today and keep working exactly as they do.
    ///
    /// A refusal from the gateway fails the operation with its reason rather
    /// than falling back to an uncredentialed attempt — the attempt would
    /// fail with a worse message, and a fallback would blur which identity
    /// acted.
    async fn borrow_git_credential(
        &self,
        owner: &OwnerId,
        worktree: &std::path::Path,
    ) -> Result<Option<crate::obo_gateway::GitCredential>, ServerError> {
        let Some(lender) = self.git_credentials() else {
            return Ok(None);
        };
        let Some(target) = forge_lending_target(worktree).await else {
            return Ok(None);
        };
        let repository = format!("{}/{}", target.owner, target.name);
        match lender.git_credential(owner, &repository).await {
            Ok(credential) => Ok(Some(credential)),
            Err(refusal) => Err(ServerError::unprocessable_kind(
                "git_forge_refused",
                super::clone::git_forge_refusal_message(&refusal),
            )),
        }
    }

    /// The REST context for a pull-request operation in `worktree`
    /// (decision 65): the forge repository the checkout names plus one
    /// borrowed credential. `Ok(None)` is every machine with its own
    /// credentials and every checkout outside the lending gate — those keep
    /// `gh` exactly as it is. A gateway refusal fails the operation with its
    /// reason, exactly as a push does.
    async fn forge_rest_context(
        &self,
        owner: &OwnerId,
        worktree: &std::path::Path,
    ) -> Result<
        Option<(
            crate::routes::code::types::CodeGitHubRepositoryTarget,
            crate::obo_gateway::GitCredential,
        )>,
        ServerError,
    > {
        if self.git_credentials().is_none() {
            return Ok(None);
        }
        let Some(target) = forge_lending_target(worktree).await else {
            return Ok(None);
        };
        let credential = self
            .borrow_git_credential(owner, worktree)
            .await?
            .ok_or_else(|| {
                ServerError::unprocessable_kind(
                    "git_forge_refused",
                    "this checkout's origin is not a lendable forge repository",
                )
            })?;
        Ok(Some((target, credential)))
    }

    pub(crate) async fn workspace_pr(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        let mut workspace = self.get_workspace(owner, id).await?;
        // Being asked is the attention signal (decision 66): the request
        // path reads local git plus the stored row, and the hot refresher
        // this mark feeds is what keeps the row current while anyone reads.
        self.mark_workspace_pr_hot(owner, workspace.id);
        let gh_path = self.gh_search_path_owned();
        let mut status = gh::workspace_git_status(
            std::path::Path::new(&workspace.worktree_path),
            &workspace.title,
            &workspace.branch_name,
            &workspace.base_ref,
            workspace.pr.clone(),
            gh_path.as_deref(),
        )
        .await
        .map_err(map_gh)?;
        // On a hosted machine, say whose identity a push would act as
        // (decisions 63 and 65) — only for a checkout the machine would
        // actually lend an identity to, so the sentence is never wider than
        // the lending. Probed per caller and held fresh by the lender; a
        // refusal simply leaves the field empty — the push itself reports
        // refusals with their reasons.
        if let Some(lender) = self.git_credentials() {
            let worktree = std::path::Path::new(&workspace.worktree_path);
            if forge_lending_target(worktree).await.is_some() {
                if let Ok(identity) = lender.git_forge_identity(owner).await {
                    match identity.attribution {
                        crate::obo_gateway::GitForgeAttribution::Person { login, .. } => {
                            status.pushes_as = Some(login);
                            status.pushes_as_self = Some(true);
                        }
                        crate::obo_gateway::GitForgeAttribution::Bot { bot_login } => {
                            status.pushes_as = Some(bot_login.unwrap_or(identity.app_name));
                        }
                    }
                }
            }
        }
        if status.pr != workspace.pr {
            workspace.pr = status.pr.clone();
            self.save_workspace(&workspace).await?;
            // A digest that moved is a fresh host observation: write it onto
            // the fact row's live tier and fan the change out (decision 66).
            if let Some(digest) = &status.pr {
                self.record_pull_request_live_state(owner, Some(workspace.id), digest)
                    .await;
            }
        }
        Ok(status)
    }

    /// Force a fresh host read now — the user asked, or a mutation just
    /// moved the pull request — then answer with the refreshed row.
    pub(crate) async fn refresh_workspace_pr(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        self.refresh_workspace_pr_row(owner, id).await;
        self.workspace_pr(owner, id).await
    }

    /// One conditional refresh of the workspace's pull-request row: fetch,
    /// write the row (which fans real change out to every other holder),
    /// and take the new digest as this workspace's column. Quiet on every
    /// failure — the caller's row keeps whatever it had, and the next tick
    /// or sweep corrects.
    ///
    /// The fetch rides an authenticated `gh` where one exists; a
    /// gateway-hosted machine has none (decision 65), so there the same
    /// refresh drives the forge REST API with a borrowed credential —
    /// same gate, same stored ETags, same 304-shaped traffic.
    pub(crate) async fn refresh_workspace_pr_row(&self, owner: &OwnerId, id: WorkspaceId) {
        let Ok(workspace) = self.get_workspace(owner, id).await else {
            return;
        };
        if workspace.status != CodeWorkspaceStatus::Active {
            return;
        }
        let gh_path = self.gh_search_path_owned();
        let worktree = std::path::PathBuf::from(&workspace.worktree_path);
        let digest = match gh::authenticated_gh_binary(gh_path.as_deref()).await {
            Some(binary) => {
                let transport = super::pr_fetch::FetchTransport::Gh {
                    cwd: &worktree,
                    binary: &binary,
                };
                self.fetched_workspace_digest(owner, &workspace, transport)
                    .await
            }
            None => {
                let Ok(Some((target, credential))) =
                    self.forge_rest_context(owner, &worktree).await
                else {
                    return;
                };
                let api_base = self.forge_api_base_for(&target.host);
                let transport = super::pr_fetch::FetchTransport::Rest {
                    api_base: &api_base,
                    credential: &credential,
                };
                self.fetched_workspace_digest(owner, &workspace, transport)
                    .await
            }
        };
        let Some(digest) = digest else {
            return;
        };
        if workspace.pr.as_ref() != Some(&digest) {
            match set_active_workspace_pull_request(&self.db, owner, workspace.id, &digest).await {
                Ok(true) => {
                    super::attention::emit_workspace_digests(
                        &self.db,
                        &self.bus,
                        owner,
                        workspace.id,
                    )
                    .await;
                }
                Ok(false) => {}
                Err(err) => {
                    tracing::debug!(error = %err, "code-mode: workspace digest write failed");
                }
            }
        }
    }

    /// Keep this workspace on the hot refresh tier.
    fn mark_workspace_pr_hot(&self, owner: &OwnerId, id: WorkspaceId) {
        self.hot_prs.mark(owner, id);
    }

    /// The hot tier itself, for a writer that outlives no runtime reference:
    /// the post-turn fact detector marks the workspace whose head it just
    /// watched move (issue 2799).
    pub(super) fn hot_pull_requests(&self) -> super::pr_refresh::HotPullRequests {
        self.hot_prs.clone()
    }

    /// One delivery nudge on the updates channel, debounced per owner
    /// (decision 66): a sweep that moves several rows costs one re-read,
    /// not one per row.
    pub(crate) fn nudge_delivery_update(&self, owner: &OwnerId) {
        self.delivery_nudges.publish(&self.bus, owner);
    }

    /// The workspaces the hot refresher walks this tick.
    pub(super) fn hot_pull_request_workspaces(&self) -> Vec<(OwnerId, WorkspaceId)> {
        self.hot_prs.live()
    }

    /// Start the hot pull-request refresher once (decision 66).
    pub(super) fn ensure_pr_refresh_sweep(self: &Arc<Self>) {
        if self.pr_refresh_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let guard = super::pr_refresh::PrRefreshGuard::spawn(Arc::downgrade(self));
        *self.pr_refresh_sweep.lock().expect("pr refresh sweep") = Some(guard);
    }

    /// The repository identity a workspace's pull request lives on: the
    /// registered origin when the reconcile sweep has confirmed one, the
    /// worktree's own remote otherwise.
    async fn workspace_repository_target(
        &self,
        owner: &OwnerId,
        workspace: &CodeWorkspace,
    ) -> Option<crate::routes::code::types::CodeGitHubRepositoryTarget> {
        let repo = self.get_repo(owner, workspace.repo_id).await.ok()?;
        if let (Some(host), Some(repo_owner), Some(name)) = (
            repo.origin_host.clone(),
            repo.origin_owner.clone(),
            repo.origin_name.clone(),
        ) {
            return Some(crate::routes::code::types::CodeGitHubRepositoryTarget {
                host,
                owner: repo_owner,
                name,
            });
        }
        super::delivery::repository_target_from_local(&repo)
            .await
            .ok()
    }

    /// The workspace's digest through the conditional fetcher (decision 66),
    /// over whichever transport the caller resolved — `gh` or the hosted
    /// forge REST API.
    ///
    /// Identity comes from the stored digest's URL, or a head lookup when
    /// the workspace knows no pull request yet. Each endpoint sends the
    /// row's stored ETag: a 304 answers from the row for free, and a 200
    /// carries new state. The result lands on the row — live tier, fanout,
    /// and the ETags for next time. `None` leaves the caller's persisted
    /// digest standing: no pull request, a parked host, a failed read, or a
    /// conditional read whose row moved under it.
    async fn fetched_workspace_digest(
        &self,
        owner: &OwnerId,
        workspace: &CodeWorkspace,
        transport: super::pr_fetch::FetchTransport<'_>,
    ) -> Option<PullRequestDigest> {
        use super::pr_fetch::{self, EndpointRead};

        let gate = &self.host_gate;
        let stored_identity = workspace
            .pr
            .as_ref()
            .and_then(|pr| pr.url.as_deref())
            .and_then(super::pr_facts::pull_request_identity_from_url);
        let (host, repo_owner, repo_name, number) = match stored_identity {
            Some(identity) => identity,
            None => {
                let target = self.workspace_repository_target(owner, workspace).await?;
                let found = match pr_fetch::read_pull_request_for_head(
                    gate,
                    transport,
                    &target.host,
                    &target.owner,
                    &target.name,
                    &workspace.branch_name,
                )
                .await
                {
                    Ok(found) => found?,
                    Err(failure) => {
                        tracing::debug!(error = %failure, "code-mode: pull-request lookup skipped");
                        return None;
                    }
                };
                (target.host, target.owner, target.name, found.number)
            }
        };
        let stored = tidebreak_core::db::code::get_pull_request_fetch_state(
            &self.db,
            owner,
            &host,
            &repo_owner,
            &repo_name,
            number,
        )
        .await
        .ok()
        .flatten();
        let (stored_fact, mut pull_etag, mut checks_etag, mut reviews_etag) = match stored {
            Some(state) => (
                Some(state.fact),
                state.pull_etag,
                state.checks_etag,
                state.reviews_etag,
            ),
            None => (None, None, None, None),
        };
        let sent_pull_etag = pull_etag.clone();
        let (pull, fresh_pull) = match pr_fetch::read_pull_request(
            gate,
            transport,
            &host,
            &repo_owner,
            &repo_name,
            number,
            pull_etag.as_deref(),
        )
        .await
        {
            Ok(EndpointRead::Fresh { value, etag }) => {
                pull_etag = etag;
                (value, true)
            }
            Ok(EndpointRead::NotModified) => {
                (pr_fetch::rest_pull_from_fact(stored_fact.as_ref()?), false)
            }
            Ok(EndpointRead::Missing) => return None,
            Err(failure) => {
                tracing::debug!(error = %failure, "code-mode: pull-request read skipped");
                return None;
            }
        };
        let stored_live = stored_fact.as_ref().and_then(|fact| fact.live.as_ref());
        let checks = match pull.head_sha.as_deref() {
            Some(sha) => {
                // A checks ETag names one head's answer; a moved head sends
                // an unconditional read.
                let same_head = stored_fact
                    .as_ref()
                    .and_then(|fact| fact.head_sha.as_deref())
                    == Some(sha);
                let conditional = if same_head {
                    checks_etag.as_deref()
                } else {
                    None
                };
                match pr_fetch::read_check_runs(
                    gate,
                    transport,
                    &host,
                    &repo_owner,
                    &repo_name,
                    sha,
                    conditional,
                )
                .await
                {
                    Ok(EndpointRead::Fresh { value, etag }) => {
                        checks_etag = etag;
                        value
                    }
                    Ok(EndpointRead::NotModified) => stored_live
                        .and_then(|live| live.checks.clone())
                        .unwrap_or_default(),
                    Ok(EndpointRead::Missing) => Vec::new(),
                    Err(failure) => {
                        tracing::debug!(error = %failure, "code-mode: check-runs read skipped");
                        checks_etag = None;
                        stored_live
                            .and_then(|live| live.checks.clone())
                            .unwrap_or_default()
                    }
                }
            }
            None => Vec::new(),
        };
        let open = pull.state == "open";
        let rules = if open {
            self.branch_rules_for(
                transport,
                &host,
                &repo_owner,
                &repo_name,
                pull.base_branch.as_deref(),
            )
            .await
        } else {
            None
        };
        let review_decision = if open {
            match pr_fetch::read_reviews(
                gate,
                transport,
                &host,
                &repo_owner,
                &repo_name,
                number,
                reviews_etag.as_deref(),
            )
            .await
            {
                Ok(EndpointRead::Fresh { value, etag }) => {
                    reviews_etag = etag;
                    pr_fetch::derive_review_decision(rules, &value)
                }
                Ok(EndpointRead::NotModified) => {
                    stored_live.and_then(|live| live.review_decision.clone())
                }
                Ok(EndpointRead::Missing) => None,
                Err(failure) => {
                    tracing::debug!(error = %failure, "code-mode: reviews read skipped");
                    reviews_etag = None;
                    stored_live.and_then(|live| live.review_decision.clone())
                }
            }
        } else {
            None
        };
        let in_merge_queue = if open {
            match rules {
                // Rules that name no queue spare the timeline read; a queue
                // — or a host that cannot answer the rules endpoint — pays
                // it.
                Some(rules) if !rules.has_merge_queue => Some(false),
                _ => {
                    pr_fetch::read_merge_queue_membership(
                        gate,
                        transport,
                        &host,
                        &repo_owner,
                        &repo_name,
                        number,
                    )
                    .await
                }
            }
        } else {
            Some(false)
        };
        let fresh_fact = if fresh_pull {
            stored_fact.clone().map(|mut fact| {
                pr_fetch::apply_fresh_pull_to_fact(&mut fact, &pull, Utc::now());
                fact
            })
        } else {
            None
        };
        let condition = if fresh_pull {
            tidebreak_core::db::code::PullRequestFetchCondition::Unconditional
        } else {
            tidebreak_core::db::code::PullRequestFetchCondition::PullEtag(sent_pull_etag.as_deref())
        };

        let digest = pr_fetch::digest_from_parts(&pull, &checks, review_decision, in_merge_queue);
        // The validator gates every write this read produces, not just the
        // fetch state (issue 2799). A 304 reconstructs its digest from the
        // stored snapshot, so a row that moved under the read leaves that
        // reconstruction describing a pull request that no longer exists.
        // Write the transport hints first: if the row refuses them, the
        // digest never reaches the live tier, the workspace column, or the
        // caller's broadcast.
        let accepted = match tidebreak_core::db::code::set_pull_request_fetch_state(
            &self.db,
            owner,
            &host,
            &repo_owner,
            &repo_name,
            number,
            fresh_fact.as_ref(),
            condition,
            pull_etag.as_deref(),
            checks_etag.as_deref(),
            reviews_etag.as_deref(),
        )
        .await
        {
            Ok(accepted) => accepted,
            Err(err) => {
                tracing::debug!(error = %err, "code-mode: fetch-state write failed");
                false
            }
        };
        if !fresh_pull && !accepted {
            tracing::debug!(
                host = %host,
                number = number,
                "code-mode: a conditional pull-request read lost its validator; dropping it"
            );
            return None;
        }
        self.record_pull_request_live_state(owner, Some(workspace.id), &digest)
            .await;
        Some(digest)
    }

    /// The base branch's rules, cached per branch for [`BRANCH_RULES_TTL`].
    async fn branch_rules_for(
        &self,
        transport: super::pr_fetch::FetchTransport<'_>,
        host: &str,
        repo_owner: &str,
        repo_name: &str,
        branch: Option<&str>,
    ) -> Option<super::pr_fetch::BranchRules> {
        let branch = branch?;
        let key = format!(
            "{}/{}/{}/{}",
            host.to_ascii_lowercase(),
            repo_owner.to_ascii_lowercase(),
            repo_name.to_ascii_lowercase(),
            branch
        );
        {
            let cache = self.branch_rules.lock().expect("branch rules");
            if let Some(entry) = cache.get(&key) {
                if entry.fetched_at.elapsed() <= BRANCH_RULES_TTL {
                    return entry.rules;
                }
            }
        }
        let rules = match super::pr_fetch::read_branch_rules(
            &self.host_gate,
            transport,
            host,
            repo_owner,
            repo_name,
            branch,
        )
        .await
        {
            Ok(super::pr_fetch::EndpointRead::Fresh { value, .. }) => Some(value),
            // A host with no rules endpoint answers for the cache period
            // too: hammering a known 404 helps nobody.
            Ok(_) => None,
            // A park or a transport failure states nothing; ask again next
            // time.
            Err(_) => return None,
        };
        self.branch_rules.lock().expect("branch rules").insert(
            key,
            CachedBranchRules {
                fetched_at: Instant::now(),
                rules,
            },
        );
        rules
    }

    /// After a pull-request state change on the delivery surface, make every
    /// live workspace holding that pull request read fresh: drop each one's
    /// digest cache entry and take the normal status path, which persists
    /// the digest and broadcasts the change (decision 66). Matching is by
    /// the digest's own URL, so a same-numbered pull request in another
    /// repository stays untouched. Detached and best-effort: the action's
    /// response never waits on it, and a failed re-read leaves the next
    /// sweep to correct.
    pub(crate) fn refresh_workspaces_for_pull_request(
        self: &Arc<Self>,
        owner: &OwnerId,
        pull_request_url: &str,
    ) {
        let runtime = Arc::clone(self);
        let owner = owner.clone();
        let url = pull_request_url.to_owned();
        tokio::spawn(async move {
            let workspaces = match list_workspaces(&runtime.db, &owner, None).await {
                Ok(workspaces) => workspaces,
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "code-mode: could not list workspaces after a delivery action"
                    );
                    return;
                }
            };
            for workspace in workspaces {
                if workspace.status != CodeWorkspaceStatus::Active {
                    continue;
                }
                let holds = workspace
                    .pr
                    .as_ref()
                    .and_then(|pr| pr.url.as_deref())
                    .is_some_and(|value| value.eq_ignore_ascii_case(&url));
                if !holds {
                    continue;
                }
                if let Err(err) = runtime.refresh_workspace_pr(&owner, workspace.id).await {
                    tracing::warn!(
                        workspace = %workspace.id,
                        error = %err.message(),
                        "code-mode: workspace digest refresh after a delivery action failed"
                    );
                }
            }
        });
    }

    /// Write a freshly observed digest onto its decision-62 fact row and fan
    /// real change out (decision 66): every other live workspace holding the
    /// same pull request takes the digest as a write-through copy — column
    /// and digest cache both — and broadcasts, so one host read updates
    /// every surface without a second one. Best-effort: a missing fact row
    /// or a store failure leaves the sweeps to correct.
    pub(crate) async fn record_pull_request_live_state(
        &self,
        owner: &OwnerId,
        source: Option<WorkspaceId>,
        digest: &PullRequestDigest,
    ) {
        let Some(url) = digest.url.as_deref() else {
            return;
        };
        let Some((host, repo_owner, repo_name, number)) =
            super::pr_facts::pull_request_identity_from_url(url)
        else {
            return;
        };
        if number != digest.number {
            return;
        }
        let live = tidebreak_core::CodePullRequestLiveState::from_digest(digest, Utc::now());
        let changed = match tidebreak_core::db::code::set_pull_request_live_state(
            &self.db,
            owner,
            &host,
            &repo_owner,
            &repo_name,
            number,
            &live,
        )
        .await
        {
            Ok(Some((_, changed))) => changed,
            // No fact row yet: the detector or the reconcile sweep mints it,
            // and the next digest change lands on it.
            Ok(None) => return,
            Err(err) => {
                tracing::debug!(error = %err, "code-mode: live-tier write failed");
                return;
            }
        };
        if !changed {
            return;
        }
        // One delivery nudge per real change (decision 66): the delivery
        // page and notification monitor re-read on receipt instead of on
        // their own timers.
        self.nudge_delivery_update(owner);
        let workspaces = match list_workspaces(&self.db, owner, None).await {
            Ok(workspaces) => workspaces,
            Err(_) => return,
        };
        for workspace in workspaces {
            if source == Some(workspace.id) || workspace.status != CodeWorkspaceStatus::Active {
                continue;
            }
            let holds = workspace
                .pr
                .as_ref()
                .and_then(|pr| pr.url.as_deref())
                .is_some_and(|value| value.eq_ignore_ascii_case(url));
            if !holds || workspace.pr.as_ref() == Some(digest) {
                continue;
            }
            match set_active_workspace_pull_request(&self.db, owner, workspace.id, digest).await {
                Ok(true) => {
                    super::attention::emit_workspace_digests(
                        &self.db,
                        &self.bus,
                        owner,
                        workspace.id,
                    )
                    .await;
                }
                Ok(false) => {}
                Err(err) => {
                    tracing::warn!(
                        workspace = %workspace.id,
                        error = %err,
                        "code-mode: pull-request write-through failed"
                    );
                }
            }
        }
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

    /// Every pull request attributed to the workspace, from the durable fact
    /// store (decision 77): open first, then newest activity. No host read.
    pub(crate) async fn workspace_pull_requests(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<
        Vec<(
            tidebreak_core::CodePullRequestFact,
            tidebreak_core::CodePullRequestRelation,
        )>,
        ServerError,
    > {
        // The read authorizes through the workspace row: another owner's
        // workspace is indistinguishable from a missing one.
        let _ = self.get_workspace(owner, id).await?;
        let mut facts =
            tidebreak_core::db::code::list_attributed_facts_for_workspace(&self.db, owner, id)
                .await?;
        facts.sort_by(|(left, _), (right, _)| {
            let left_open = left.state == tidebreak_core::CodePullRequestState::Open;
            let right_open = right.state == tidebreak_core::CodePullRequestState::Open;
            right_open
                .cmp(&left_open)
                .then_with(|| right.updated_at.cmp(&left.updated_at))
        });
        Ok(facts)
    }

    /// Merge the exact pull request head the desktop confirmed.
    ///
    /// The workspace turn lock covers every mutable local and host preflight
    /// plus the repository-qualified merge invocation. The final helper also
    /// sends `--match-head-commit`, so a force push after the live read fails
    /// at GitHub instead of changing what this request lands.
    pub(crate) async fn merge_workspace_pr(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
        target: CodeDeliveryPullRequestTarget,
        expected_head_sha: String,
        method: gh::MergeMethod,
        auto: bool,
    ) -> Result<WorkspaceMergeOutcome, ServerError> {
        validate_workspace_merge_request(&target, &expected_head_sha)?;
        let turn = self.worktree_turn_lock(id);
        let _turn_guard = turn.lock().await;
        let workspace = self.require_live_workspace(owner, id).await?;
        let worktree = std::path::Path::new(&workspace.worktree_path);
        let local = gh::inspect_workspace_merge_local_state(worktree)
            .await
            .map_err(map_gh)?;
        let current_branch = local.current_branch.ok_or_else(|| {
            ServerError::conflict_kind(
                "workspace_branch_changed",
                format!(
                    "the workspace is detached; check out {} and refresh before merging",
                    workspace.branch_name
                ),
            )
        })?;
        if current_branch != workspace.branch_name {
            return Err(ServerError::conflict_kind(
                "workspace_branch_changed",
                format!(
                    "the workspace branch changed from {} to {current_branch}; refresh before merging",
                    workspace.branch_name
                ),
            ));
        }
        if local.dirty {
            return Err(ServerError::conflict_kind(
                "workspace_dirty",
                "the workspace now has uncommitted changes; review them before merging",
            ));
        }
        let upstream = local.upstream.ok_or_else(|| {
            ServerError::conflict_kind(
                "workspace_upstream_missing",
                "the workspace branch no longer has an upstream; push it and refresh before merging",
            )
        })?;
        let expected_upstream = format!("origin/{}", workspace.branch_name);
        if upstream != expected_upstream {
            return Err(ServerError::conflict_kind(
                "workspace_branch_changed",
                format!(
                    "the workspace branch now tracks {upstream} instead of {expected_upstream}; refresh before merging"
                ),
            ));
        }
        if local.ahead_of_upstream > 0 {
            return Err(ServerError::conflict_kind(
                "workspace_unpushed",
                format!(
                    "the workspace now has {} unpushed commit{}; push and refresh before merging",
                    local.ahead_of_upstream,
                    if local.ahead_of_upstream == 1 {
                        ""
                    } else {
                        "s"
                    }
                ),
            ));
        }

        let local_target = super::delivery::repository_target_from_path(worktree)
            .await
            .map_err(|message| {
                ServerError::conflict_kind(
                    "workspace_repository_changed",
                    format!("the workspace repository could not be verified: {message}"),
                )
            })?;
        if !same_repository(&local_target, &target.repository) {
            return Err(ServerError::conflict_kind(
                "workspace_repository_changed",
                format!(
                    "the workspace repository changed from {} to {}; refresh before merging",
                    repository_label(&target.repository),
                    repository_label(&local_target)
                ),
            ));
        }

        let gh_path = self.gh_search_path_owned();
        let live = gh::view_workspace_pull_request(worktree, gh_path.as_deref())
            .await
            .map_err(map_gh)?;
        if !same_repository(&live.target, &target.repository) || live.number != target.number {
            return Err(ServerError::conflict_kind(
                "pr_target_changed",
                format!(
                    "the workspace now resolves to {}#{} instead of {}#{}; refresh before merging",
                    repository_label(&live.target),
                    live.number,
                    repository_label(&target.repository),
                    target.number
                ),
            ));
        }
        if live.head_branch != workspace.branch_name {
            return Err(ServerError::conflict_kind(
                "pr_target_changed",
                format!(
                    "pull request #{} now uses branch {} instead of {}; refresh before merging",
                    target.number, live.head_branch, workspace.branch_name
                ),
            ));
        }
        if live.state != "open" {
            return Err(ServerError::conflict_kind(
                "pr_not_mergeable",
                format!(
                    "pull request #{} is {}; refresh before merging",
                    target.number, live.state
                ),
            ));
        }
        if local.head_sha != expected_head_sha {
            return Err(pr_head_changed(&expected_head_sha, &local.head_sha));
        }
        if live.head_sha != expected_head_sha {
            return Err(pr_head_changed(&expected_head_sha, &live.head_sha));
        }

        gh::merge_pull_request_target(
            &target.repository.host,
            &target.repository.owner,
            &target.repository.name,
            target.number,
            method,
            auto,
            false,
            &expected_head_sha,
            gh_path.as_deref(),
        )
        .await
        .map_err(map_gh)?;
        drop(_turn_guard);
        // A merge dirties the row (decision 66); the delivery lists hold the
        // pre-merge row.
        self.delivery_cache.invalidate();
        let status = self.refresh_workspace_pr(owner, id).await?;
        Ok(WorkspaceMergeOutcome {
            target,
            accepted_head_sha: expected_head_sha,
            status,
        })
    }

    /// Take the workspace's pull request out of draft and return a fresh
    /// status. Decision 42 keeps pull-request state changes on a user-initiated
    /// endpoint rather than on any agent or automation path, so this is the
    /// only route to `gh pr ready` for a workspace.
    pub(crate) async fn mark_workspace_pr_ready(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        let turn = self.worktree_turn_lock(id);
        let _turn_guard = turn.lock().await;
        let workspace = self.require_live_workspace(owner, id).await?;
        let gh_path = self.gh_search_path_owned();
        gh::mark_workspace_pull_request_ready(
            std::path::Path::new(&workspace.worktree_path),
            gh_path.as_deref(),
        )
        .await
        .map_err(map_gh)?;
        self.delivery_cache.invalidate();
        self.refresh_workspace_pr(owner, id).await
    }

    pub(crate) async fn create_workspace_pr(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
        title: Option<String>,
        body: Option<String>,
    ) -> Result<WorkspaceGitStatus, ServerError> {
        let turn = self.worktree_turn_lock(id);
        let _turn_guard = turn.lock().await;
        let mut workspace = self.require_live_workspace(owner, id).await?;
        let worktree = std::path::PathBuf::from(&workspace.worktree_path);
        // On a hosted machine the pull request rides the forge REST API with
        // a borrowed credential (decision 65) and lands as the caller; the
        // authored fact comes straight from the creation answer, with no
        // second host read. Everywhere else `gh` does exactly what it always
        // has (decision 34), including its own best-effort fact read below.
        let (digest, rest_fact) = match self.forge_rest_context(owner, &worktree).await? {
            Some((target, credential)) => {
                let api_base = self.forge_api_base_for(&target.host);
                let (digest, fact) = gh::create_pull_request_rest(
                    &worktree,
                    &workspace.title,
                    &workspace.branch_name,
                    &workspace.base_ref,
                    title.as_deref(),
                    body.as_deref(),
                    &api_base,
                    &target,
                    &credential,
                )
                .await
                .map_err(map_gh)?;
                (digest, Some((target, fact)))
            }
            None => {
                let gh_path = self.gh_search_path_owned();
                let digest = gh::create_pull_request(
                    &worktree,
                    &workspace.title,
                    &workspace.branch_name,
                    &workspace.base_ref,
                    title.as_deref(),
                    body.as_deref(),
                    gh_path.as_deref(),
                )
                .await
                .map_err(map_gh)?;
                (digest, None)
            }
        };
        self.delivery_cache.invalidate();
        let created_number = digest.number;
        workspace.pr = Some(digest);
        self.save_workspace(&workspace).await?;
        // Best-effort authored fact (decision 77). The digest just came from
        // the host; the REST path already holds the full row, and the `gh`
        // path re-reads it repository-qualified for full identity and
        // timestamps. Failures are silent; the reconcile sweep corrects.
        if let Some((target, fact)) = rest_fact {
            super::pr_facts::record_confirmed_fact(
                &self.db,
                owner,
                workspace.id,
                None,
                None,
                &target,
                &fact,
                tidebreak_core::CodePullRequestRelation::Authored,
                tidebreak_core::CodePullRequestDiscovery::Command,
            )
            .await;
        } else if let Ok(target) = super::delivery::repository_target_from_path(&worktree).await {
            let gh_path = self.gh_search_path_owned();
            if let Ok(value) = gh::view_pull_request_raw(
                &target.host,
                &target.owner,
                &target.name,
                created_number,
                gh_path.as_deref(),
            )
            .await
            {
                super::pr_facts::record_confirmed_fact(
                    &self.db,
                    owner,
                    workspace.id,
                    None,
                    None,
                    &target,
                    &value,
                    tidebreak_core::CodePullRequestRelation::Authored,
                    tidebreak_core::CodePullRequestDiscovery::Command,
                )
                .await;
            }
        }
        // Creation dirties the row (decision 66): the response carries the
        // fetched digest — checks pending on the fresh pull request — not
        // the light creation stub.
        self.refresh_workspace_pr(owner, id).await
    }

    pub(crate) async fn run_workspace_action(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
        name: &str,
    ) -> Result<ActionOutcome, ServerError> {
        let turn = self.worktree_turn_lock(id);
        let _turn_guard = turn.lock().await;
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
        if workspace.status != CodeWorkspaceStatus::Active {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                format!("workspace is {}", workspace.status.as_str()),
            ));
        }
        if workspace.is_remote() {
            return Err(ServerError::conflict_kind(
                "workspace_remote",
                "this workspace's engine runs in a remote sandbox; there is no host worktree",
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
        settings: NewSessionSettings,
    ) -> Result<CodeSession, ServerError> {
        self.create_session_of_kind(
            owner,
            workspace_id,
            CodeSessionKind::Interactive,
            harness,
            settings,
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
        NewSessionSettings {
            permission_mode,
            model,
            reasoning_effort,
            fast_mode,
            permission_mode_ceiling,
        }: NewSessionSettings,
    ) -> Result<CodeSession, ServerError> {
        let lifecycle = self.workspace_lifecycle_lock(workspace_id);
        let _lifecycle_guard = lifecycle.lock().await;
        let workspace = self.get_workspace(owner, workspace_id).await?;
        if workspace.status != CodeWorkspaceStatus::Active {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                format!("workspace is {}", workspace.status.as_str()),
            ));
        }
        if workspace.is_remote() {
            return Err(ServerError::conflict_kind(
                "workspace_remote",
                "this workspace's engine runs in a remote sandbox; create a remote session on it",
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
            // Skip when a CLI e2e has replaced this kind with the scripted
            // engine: that binary has no pin and must not try to download one.
            let skip_pin = {
                #[cfg(feature = "scripted-harness")]
                {
                    crate::scripted_harness::env_is_set()
                }
                #[cfg(not(feature = "scripted-harness"))]
                {
                    false
                }
            };
            if !skip_pin {
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
        }
        let probe = self.probe_for_session_create(adapter.as_ref()).await;
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
        refuse_ceiling_with_no_offered_mode(permission_mode_ceiling, harness, &caps)?;
        if let Some(ceiling) = permission_mode_ceiling {
            if permission_mode > ceiling {
                return Err(ServerError::conflict_kind(
                    "permission_mode_locked",
                    format!(
                        "permission mode `{}` exceeds the maximum this managed profile allows (`{}`)",
                        permission_mode.as_str(),
                        ceiling.as_str()
                    ),
                ));
            }
        }
        refuse_unhonored_mode(harness, permission_mode, &caps)?;
        if probe.binary_path.is_none() {
            return Err(ServerError::unprocessable_kind(
                "harness_not_found",
                format!("{harness} has no path"),
            ));
        }
        self.refuse_signed_out_harness(harness, &probe)?;
        let execution_settings = CodeSessionExecutionSettings {
            model: normalize_model(model),
            reasoning_effort,
            fast_mode,
        };
        if execution_settings.reasoning_effort.is_some() || execution_settings.fast_mode {
            let selected = self
                .selected_model_capabilities_for_owner(
                    owner,
                    adapter.as_ref(),
                    &probe,
                    execution_settings.model.as_deref(),
                )
                .await;
            Self::validate_execution_settings(harness, &execution_settings, &selected)?;
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
            model: execution_settings.model,
            reasoning_effort: execution_settings.reasoning_effort,
            fast_mode: execution_settings.fast_mode,
            lifecycle: CodeSessionLifecycle::Created,
            fence_reason: None,
            child_pid: None,
            child_process_identity: None,
            spawn_epoch: 0,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: Utc::now(),
        };
        insert_session(&self.db, &session).await?;
        // Pin where this session starts, before it can take a turn. Sessions
        // share the worktree (record 55), so without a baseline the first
        // turn's diff is the whole worktree against the base branch — a
        // sibling's edits included.
        if let Err(err) = record_session_baseline(
            Path::new(&workspace.worktree_path),
            workspace.id,
            session.id,
        )
        .await
        {
            tracing::warn!(
                session = %session.id,
                workspace = %workspace.id,
                error = %err,
                "could not record the session baseline; its first turn diffs against the base ref"
            );
        }
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

    /// Resolve requested attachments for one session, or refuse.
    ///
    /// Takes the owner and session because publication is per-session
    /// authority. Resolving without them — as this did — could only check that
    /// the bytes existed somewhere, which is not the same question.
    pub(crate) async fn resolve_turn_attachments(
        &self,
        owner: &OwnerId,
        session_id: CodeSessionId,
        requested: &[(uuid::Uuid, String)],
    ) -> Result<Vec<tidebreak_core::ImageRef>, ServerError> {
        if requested.len() > tidebreak_core::context::MAX_HYDRATED_IMAGES {
            return Err(ServerError::bad_request(format!(
                "a turn may carry at most {} image attachments",
                tidebreak_core::context::MAX_HYDRATED_IMAGES
            )));
        }
        let mut resolved = Vec::with_capacity(requested.len());
        let mut resolved_bytes = 0_u64;
        for (blob_id, media_type) in requested {
            if blob_id.is_nil() {
                return Err(ServerError::bad_request(
                    "attachment blob_id must not be nil",
                ));
            }
            let media_type = parse_turn_media_type(media_type).ok_or_else(|| {
                ServerError::bad_request(format!("unsupported attachment media type {media_type}"))
            })?;
            // Publication is the authority, not the blob's existence. The blob
            // store is content-addressed and owner-blind, so checking only
            // that the bytes are present would let any known id be bound into
            // this session and read back through its own image route. An
            // unpublished id is refused as not-found, so the failure cannot
            // confirm the blob exists somewhere else.
            let unpublished = || {
                ServerError::bad_request(format!(
                    "attachment blob {blob_id} was not published to session {session_id}"
                ))
            };
            let published = tidebreak_core::db::code::get_published_session_image(
                &self.db, owner, session_id, *blob_id,
            )
            .await?
            .ok_or_else(unpublished)?;
            if published.media_type != media_type {
                return Err(ServerError::bad_request(format!(
                    "attachment blob {blob_id} was published as {}",
                    published.media_type
                )));
            }
            // Re-derive from the bytes, the way chat's resolution does: an id
            // is a content address, so bytes that no longer hash back to it,
            // or that no longer match the reserved descriptor, are unresolved
            // rather than merely mismatched.
            let bytes = self
                .blobs
                .get(*blob_id)
                .await
                .map_err(|err| ServerError::internal(format!("blob read: {err}")))?
                .ok_or_else(unpublished)?;
            let image = crate::routes::inspect_image_bytes(&bytes)?;
            if image.blob_id != *blob_id || image != published {
                return Err(unpublished());
            }
            resolved_bytes = resolved_bytes.saturating_add(image.byte_len);
            if resolved_bytes
                > u64::try_from(tidebreak_core::context::MAX_HYDRATED_IMAGE_BYTES)
                    .expect("the image byte limit fits in u64")
            {
                return Err(ServerError::bad_request(format!(
                    "turn image attachments may total at most {} bytes",
                    tidebreak_core::context::MAX_HYDRATED_IMAGE_BYTES
                )));
            }
            resolved.push(image);
        }
        Ok(resolved)
    }

    /// Begin an update quiesce: session workers hold their queue drains, no
    /// new turn starts, and idle engine children park immediately. Turns
    /// already in flight run to their boundary; [`Self::await_update_quiesce`]
    /// is how the caller waits for that. See `crate::update_quiesce`.
    pub(crate) fn begin_update_quiesce(&self) {
        self.update_quiesce.send_replace(true);
        self.wake_all_workers();
    }

    /// Reopen turn admission after an update that did not install. Parked
    /// children stay parked; the next turn respawns and resumes them exactly
    /// as an idle park does (decision 0064).
    pub(crate) fn end_update_quiesce(&self) {
        self.update_quiesce.send_replace(false);
        self.wake_all_workers();
    }

    pub(crate) fn update_quiesce_active(&self) -> bool {
        *self.update_quiesce.borrow()
    }

    fn wake_all_workers(&self) {
        for handle in self.workers.lock().expect("code workers").values() {
            wake_queue(handle);
        }
    }

    /// Wait until no local session is mid-turn and no engine child is live,
    /// or fail with a sentence the updater can show as-is. Remote sessions
    /// run their engine in a sandbox and survive a restart on their own, so
    /// they never appear here — the worker map only holds local sessions.
    pub(crate) async fn await_update_quiesce(&self, deadline: Duration) -> Result<(), String> {
        let deadline_at = Instant::now() + deadline;
        // Turn starts are fenced by the worktree lock, not by the database:
        // a starting turn holds its workspace's lock from before it re-reads
        // the quiesce flag until the turn ends, and the flag is already up.
        // Acquiring and releasing every lock therefore proves each workspace
        // is past any start that raced the flag — whoever takes a lock after
        // this sees the flag and refuses. Without this pass, a poll of the
        // stored lifecycle could observe Idle in the window between a turn
        // winning its lock and persisting Running, and the update would exit
        // over an engine turn that was about to start.
        let worktree_turns: Vec<Arc<tokio::sync::Mutex<()>>> = self
            .worktree_turns
            .lock()
            .expect("code worktree turn locks")
            .values()
            .cloned()
            .collect();
        for worktree_turn in worktree_turns {
            let remaining = deadline_at.saturating_duration_since(Instant::now());
            match tokio::time::timeout(remaining, worktree_turn.lock()).await {
                Ok(guard) => drop(guard),
                Err(_) => {
                    return Err(
                        "A code session is still working on a turn. Try again once it \
                                finishes — the update stays ready."
                            .to_owned(),
                    );
                }
            }
        }
        // Past every turn boundary; now wait for the workers to park their
        // engine children and for the stored rows to agree.
        loop {
            let ids: Vec<CodeSessionId> = self
                .workers
                .lock()
                .expect("code workers")
                .keys()
                .copied()
                .collect();
            let mut busy = 0usize;
            for id in ids {
                match tidebreak_core::db::code::get_session_all_owners(&self.db, id).await {
                    Ok(Some(session)) => {
                        if session.lifecycle == CodeSessionLifecycle::Running
                            || session.child_pid.is_some()
                        {
                            busy += 1;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        return Err(format!(
                            "could not read code sessions while preparing the update: {error}"
                        ));
                    }
                }
            }
            if busy == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline_at {
                return Err(if busy == 1 {
                    "A code session is still working on a turn. Try again once it finishes — \
                     the update stays ready."
                        .to_owned()
                } else {
                    format!(
                        "{busy} code sessions are still working on turns. Try again once they \
                         finish — the update stays ready."
                    )
                });
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    pub(crate) async fn submit_turn(
        &self,
        owner: &OwnerId,
        id: CodeSessionId,
        message: String,
        model: Option<String>,
        reasoning_effort: Option<Option<ReasoningEffort>>,
        attachments: Vec<tidebreak_core::ImageRef>,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        self.submit_turn_inner(
            owner,
            id,
            message,
            model,
            reasoning_effort,
            attachments,
            None,
            true,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn submit_turn_inner(
        &self,
        owner: &OwnerId,
        id: CodeSessionId,
        message: String,
        model: Option<String>,
        reasoning_effort: Option<Option<ReasoningEffort>>,
        attachments: Vec<tidebreak_core::ImageRef>,
        trigger_delivery: Option<TriggerDeliveryClaim>,
        queue_if_busy: bool,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        if let Some(claim) = trigger_delivery {
            if tidebreak_core::db::code::trigger_delivery_accepted(
                &self.db,
                owner,
                claim.delivery_id,
            )
            .await?
            {
                return Ok(SubmitTurnOutcome::AlreadyDelivered);
            }
        }
        let mut session = self.get_session(owner, id).await?;
        let workspace = self.get_workspace(owner, session.workspace_id).await?;
        if workspace.status != CodeWorkspaceStatus::Active {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                format!("workspace is {}", workspace.status.as_str()),
            ));
        }
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
        if workspace.is_remote() {
            // The sandbox path: no local worker, no worktree lock, no
            // harness probe. Everything below this branch assumes a checkout
            // on this machine.
            return self
                .submit_remote_turn(
                    owner,
                    session,
                    &workspace,
                    message,
                    model,
                    reasoning_effort,
                    attachments,
                    trigger_delivery,
                    queue_if_busy,
                )
                .await;
        }
        // No capability gate on attachments. An engine that states image input
        // is handed the bytes on its own protocol; every other one is handed a
        // private file and an absolute path in the prompt. The worker picks
        // between the two.
        // Both stick: a composer choice is the session's from here on, exactly
        // as the engines' own pickers behave. The outer `Option` on effort is
        // what lets a turn say "back to the engine default" rather than "no
        // opinion" — the inner `None` is a real choice.
        let requested_model = normalize_model(model);
        let model_changed = requested_model
            .as_deref()
            .is_some_and(|model| session.model.as_deref() != Some(model));
        let requested_effort = reasoning_effort;
        let mut next = CodeSessionExecutionSettings::from(&session);
        if let Some(model) = requested_model {
            next.model = Some(model);
        }
        if let Some(effort) = reasoning_effort {
            next.reasoning_effort = effort;
        }
        if model_changed
            || requested_effort.is_some()
            || next.reasoning_effort.is_some()
            || next.fast_mode
        {
            let adapter = self.adapter(session.harness_kind)?;
            let probe = self.probe(adapter.as_ref()).await;
            let selected = self
                .selected_model_capabilities_for_owner(
                    owner,
                    adapter.as_ref(),
                    &probe,
                    next.model.as_deref(),
                )
                .await;
            if requested_effort.is_some() {
                let requested = CodeSessionExecutionSettings {
                    model: next.model.clone(),
                    reasoning_effort: next.reasoning_effort,
                    fast_mode: false,
                };
                Self::validate_execution_settings(session.harness_kind, &requested, &selected)?;
            }
            selected.deactivate_unsupported(&mut next);
        }
        if next != CodeSessionExecutionSettings::from(&session) {
            session = self
                .commit_execution_settings(&session, &next, "the turn could reserve them")
                .await?;
        }
        let handle = self.require_worker(id)?;
        if handle.spawn_epoch != session.spawn_epoch {
            return Err(ServerError::conflict_kind(
                "session_worker_changed",
                "the session worker changed before the turn could start",
            ));
        }
        // A sibling fenced for an unaccounted engine — an orphan from a
        // previous boot, an ambiguous pid, a lost resume — may still be alive
        // in this checkout, outside every lock this process holds. The turn
        // lock cannot see it, so nothing in the workspace writes until it is
        // reaped (record 55). A sibling fenced for repeated turn failures is
        // not that: its engine answered every time, so it does not stop us.
        if let Some(reason) = self.workspace_fence_reason(owner, &session).await? {
            return Err(ServerError::conflict_kind("workspace_fenced", reason));
        }
        // Queue-default (0009, 0065): a send while a turn is in flight parks
        // as a durable queue row. This does not consult mid_turn_steering —
        // that cap gates the separate /steer route only. A backlog parks the
        // send even with no open turn: rows ahead of this message must run
        // first (FIFO), and the worker may already be holding the head while
        // it waits on a sibling's worktree turn.
        let in_flight = session.lifecycle == CodeSessionLifecycle::Running
            || get_open_turn(&self.db, owner, id).await?.is_some();
        let backlog = !tidebreak_core::db::code::list_queued_turns(&self.db, owner, id)
            .await?
            .is_empty();
        // An update quiesce parks the send too: the row survives the restart
        // and drains after the relaunch, so nothing typed during the short
        // install window is lost or refused.
        if in_flight || backlog || self.update_quiesce_active() {
            if !queue_if_busy {
                return Err(ServerError::conflict_kind(
                    "trigger_turn_busy",
                    "the trigger turn was not accepted because the session became busy",
                ));
            }
            return self
                .park_follow_up(owner, &handle, &session, message, attachments)
                .await;
        }
        let (reply, rx) = oneshot::channel();
        handle
            .commands
            .send(WorkerCommand::RunTurn {
                message: message.clone(),
                attachments: attachments.clone(),
                trigger_delivery,
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
                if !queue_if_busy {
                    return Err(ServerError::conflict_kind(
                        "trigger_turn_busy",
                        "the trigger turn was not accepted because the workspace became busy",
                    ));
                }
                return self
                    .park_follow_up(owner, &handle, &session, message, attachments)
                    .await;
            }
            // The quiesce flag flipped while this send was in flight. Park it
            // the same way: the durable row runs after the relaunch.
            Err(WorkerError::UpdateQuiesced) => {
                if !queue_if_busy {
                    return Err(ServerError::conflict_kind(
                        "trigger_turn_busy",
                        "the trigger turn was not accepted because the app is restarting to update",
                    ));
                }
                return self
                    .park_follow_up(owner, &handle, &session, message, attachments)
                    .await;
            }
            Err(WorkerError::TriggerDeliveryAccepted) => {
                return Ok(SubmitTurnOutcome::AlreadyDelivered);
            }
            Err(err) => return Err(map_worker(err)),
        };
        Ok(SubmitTurnOutcome::Ran(Box::new(turn)))
    }

    /// Submit a turn created by one durable trigger delivery.
    pub(crate) async fn submit_trigger_turn(
        &self,
        owner: &OwnerId,
        id: CodeSessionId,
        message: String,
        delivery_id: tidebreak_core::CodeTriggerDeliveryId,
        lease_token: uuid::Uuid,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        self.submit_turn_inner(
            owner,
            id,
            message,
            None,
            None,
            Vec::new(),
            Some(TriggerDeliveryClaim {
                delivery_id,
                lease_token,
            }),
            false,
        )
        .await
    }

    /// Submit one turn to a remote session's sandbox (`docs/slack-sessions.md`).
    #[allow(clippy::too_many_arguments)]
    async fn submit_remote_turn(
        &self,
        owner: &OwnerId,
        mut session: CodeSession,
        workspace: &CodeWorkspace,
        message: String,
        model: Option<String>,
        reasoning_effort: Option<Option<ReasoningEffort>>,
        attachments: Vec<tidebreak_core::ImageRef>,
        trigger_delivery: Option<TriggerDeliveryClaim>,
        queue_if_busy: bool,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        let Some(remote) = self.remote_sessions() else {
            return Err(ServerError::conflict_kind(
                "remote_disabled",
                "this deployment has no sandbox runtime configured",
            ));
        };
        if trigger_delivery.is_some() {
            // Trigger delivery is at-most-once. The runtime's spawn and inbox
            // calls accept no idempotency key and expose no replay result, so
            // retrying an ambiguous response could run one trigger twice.
            return Err(ServerError::conflict_kind(
                "remote_triggers_unsupported",
                "remote trigger turns are disabled because sandbox spawn and inbox calls have no idempotency key; submit the turn manually",
            ));
        }
        if !attachments.is_empty() {
            // Remote messages carry text only. Until the runtime provides a
            // bounded, owner-scoped file transfer, attachment bytes have no
            // safe path into the sandbox.
            return Err(ServerError::conflict_kind(
                "remote_attachments_unsupported",
                "remote sessions cannot stage attachment bytes because the sandbox message contract carries text only; send the turn without attachments",
            ));
        }
        // Settings stick without local capability validation: the sandbox
        // engine reads them off the spawn, and no local harness answers for
        // a remote one.
        let mut next = CodeSessionExecutionSettings::from(&session);
        if let Some(model) = normalize_model(model) {
            next.model = Some(model);
        }
        if let Some(effort) = reasoning_effort {
            next.reasoning_effort = effort;
        }
        if next != CodeSessionExecutionSettings::from(&session) {
            session = replace_session_execution_settings(&self.db, owner, &session, &next)
                .await?
                .ok_or_else(|| {
                    ServerError::conflict_kind(
                        "session_settings_changed",
                        "the session settings changed before the turn could reserve them",
                    )
                })?;
        }
        // Queue-default, exactly as the local path: a busy session parks the
        // send as a durable row the remote sweep promotes at the next idle.
        let in_flight = session.lifecycle == CodeSessionLifecycle::Running
            || get_open_turn(&self.db, owner, session.id).await?.is_some();
        let backlog = !tidebreak_core::db::code::list_queued_turns(&self.db, owner, session.id)
            .await?
            .is_empty();
        if in_flight || backlog {
            if !queue_if_busy {
                return Err(ServerError::conflict_kind(
                    "trigger_turn_busy",
                    "the turn was not accepted because the session is busy",
                ));
            }
            return self.park_remote_follow_up(owner, &session, message).await;
        }
        let repo = self.get_repo(owner, workspace.repo_id).await?;
        let driver = remote.driver(&self.db, self.bus.as_ref());
        let outcome = driver
            .submit_turn(&mut session, workspace, &repo, &message)
            .await?;
        // A provisioned or delivered turn has events to drain and a parked
        // one has a head to promote; either way the sweep should look now,
        // not at its next floor.
        remote.wake_sweep();
        self.relay_remote_outcome(owner, &session, outcome, message, queue_if_busy)
            .await
    }

    /// Translate a driver outcome into the submit answer the routes speak.
    async fn relay_remote_outcome(
        &self,
        owner: &OwnerId,
        session: &CodeSession,
        outcome: super::remote::driver::RemoteTurnOutcome,
        message: String,
        queue_if_busy: bool,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        use super::remote::driver::RemoteTurnOutcome as Outcome;
        match outcome {
            Outcome::Delivered { turn } | Outcome::Reincarnated { turn, .. } => {
                Ok(SubmitTurnOutcome::Ran(turn))
            }
            Outcome::TurnInFlight | Outcome::ReincarnationInFlight | Outcome::FlushPending => {
                if !queue_if_busy {
                    return Err(ServerError::conflict_kind(
                        "trigger_turn_busy",
                        "the turn was not accepted because the session is busy",
                    ));
                }
                self.park_remote_follow_up(owner, session, message).await
            }
            Outcome::CapExhausted { running } => Err(ServerError::conflict_kind(
                "sandbox_cap_exhausted",
                format!(
                    "the sandbox cap is full: {} session(s) hold the slots",
                    running.len()
                ),
            )),
            Outcome::SpendExhausted {
                spent_microusd,
                ceiling_microusd,
            } => Err(ServerError::conflict_kind(
                "session_spend_exhausted",
                format!(
                    "this session has spent {spent_microusd} of its {ceiling_microusd} micro-USD ceiling and takes no more turns"
                ),
            )),
            Outcome::SignInRequired => Err(ServerError::conflict_kind(
                "sign_in_required",
                "sign in to the sandbox environment, then retry",
            )),
        }
    }

    /// Park a message for a remote session. The remote sweep promotes the
    /// head at the next idle; there is no worker to nudge.
    async fn park_remote_follow_up(
        &self,
        owner: &OwnerId,
        session: &CodeSession,
        message: String,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        let queued = tidebreak_core::db::code::list_queued_turns(&self.db, owner, session.id)
            .await
            .map_err(ServerError::from)?;
        if queued.len() >= CodeQueuedTurn::MAX_PER_SESSION {
            return Err(ServerError::conflict_kind(
                "queue_full",
                format!(
                    "this session may queue at most {} messages",
                    CodeQueuedTurn::MAX_PER_SESSION
                ),
            ));
        }
        let now = chrono::Utc::now();
        let row = tidebreak_core::db::code::enqueue_queued_turn(
            &self.db,
            owner,
            &CodeQueuedTurn {
                id: CodeTurnId::new(),
                session_id: session.id,
                message,
                attachments: Vec::new(),
                position: 0,
                created_at: now,
                updated_at: now,
            },
        )
        .await
        .map_err(ServerError::from)?;
        if let Some(remote) = self.remote_sessions() {
            remote.wake_sweep();
        }
        Ok(SubmitTurnOutcome::Queued(Box::new(row)))
    }

    /// Promote the queue head of every idle remote session that has one.
    /// Called from the remote sweep; local sessions drain their own queues
    /// through their workers and are skipped here.
    pub(crate) async fn promote_remote_queue_heads(&self) -> Result<(), ServerError> {
        if self.remote.is_none() {
            return Ok(());
        }
        // Only sessions holding a queue can have a head to promote, so the
        // pass reads those rather than every session on the machine.
        for (owner, session_id) in
            tidebreak_core::db::code::sessions_with_queued_turns_all_owners(&self.db).await?
        {
            let Some(session) = get_session(&self.db, &owner, session_id).await? else {
                continue;
            };
            self.try_promote_remote_head(session).await?;
        }
        Ok(())
    }

    /// Promote one idle remote session's queue head, when it has one and
    /// nothing holds promotion. Shared by the sweep and the external
    /// messages path, which tries the head immediately after enqueueing
    /// rather than waiting out a sweep tick.
    async fn try_promote_remote_head(&self, mut session: CodeSession) -> Result<(), ServerError> {
        let Some(remote) = self.remote_sessions() else {
            return Ok(());
        };
        if session.lifecycle != CodeSessionLifecycle::Idle {
            return Ok(());
        }
        let Ok(workspace) = self
            .get_workspace(&session.owner, session.workspace_id)
            .await
        else {
            return Ok(());
        };
        if !workspace.is_remote() {
            return Ok(());
        }
        if tidebreak_core::db::code::queue_paused(&self.db, &session.owner, session.id).await? {
            return Ok(());
        }
        if remote.promotion_held(session.id) {
            return Ok(());
        }
        let Some(head) = queued_turn_head(&self.db, &session.owner, session.id).await? else {
            return Ok(());
        };
        let Ok(repo) = self.get_repo(&session.owner, workspace.repo_id).await else {
            return Ok(());
        };
        let driver = remote.driver(&self.db, self.bus.as_ref());
        let message = head.message.clone();
        use super::remote::driver::RemoteTurnOutcome as Outcome;
        match driver
            .submit_turn_from(&mut session, &workspace, &repo, &message, Some(&head))
            .await
        {
            // The claim was the atomic promotion; nothing to delete. A
            // reincarnation has a fresh sandbox to pump, so the sweep
            // looks again now rather than at its next floor.
            Ok(Outcome::Delivered { .. }) | Ok(Outcome::Reincarnated { .. }) => {
                remote.clear_promotion_hold(session.id);
                remote.wake_sweep();
            }
            // Permanent for this session: nothing exposes a way to raise
            // the ceiling, and every retry would re-journal the refusal
            // and re-cancel the sandbox. Pause the queue so the tray
            // shows why nothing moves; unpausing retries deliberately.
            Ok(Outcome::SpendExhausted { .. }) => {
                let _ = tidebreak_core::db::code::set_queue_paused(
                    &self.db,
                    &session.owner,
                    session.id,
                    true,
                )
                .await;
            }
            // Transient machine-side refusals: hold retries so the
            // notice and attention do not repeat every sweep tick. The
            // hold expiring retries on its own once the slot may be
            // free or the owner has signed in.
            Ok(Outcome::CapExhausted { .. }) | Ok(Outcome::SignInRequired) => {
                remote.hold_promotion(session.id);
            }
            // Busy shapes: the row stays queued for the next idle.
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    session = %session.id,
                    %error,
                    "promoting a queued remote message failed; the row stays queued"
                );
                remote.hold_promotion(session.id);
            }
        }
        Ok(())
    }

    /// Take one external message for a bound session
    /// (`docs/slack-sessions.md`, stage 2).
    ///
    /// Idempotent across the channel's retries: the event id commits with
    /// the queue row it causes, and a replay derives its answer from that
    /// row's current state — still queued, promoted into a turn, or
    /// retracted — without writing a second row. An idle session promotes
    /// the head immediately; a busy one queues durably.
    pub(crate) async fn external_submit_message(
        &self,
        owner: &OwnerId,
        grant_id: tidebreak_core::CodeGrantId,
        session_id: CodeSessionId,
        message: String,
        event_id: &str,
        channel_ts: &str,
    ) -> Result<ExternalMessageOutcome, ServerError> {
        if self.remote.is_none() {
            return Err(ServerError::conflict_kind(
                "remote_disabled",
                "this deployment has no sandbox runtime configured",
            ));
        }
        if !tidebreak_core::db::code::session_bound_to_grant(&self.db, owner, session_id, grant_id)
            .await?
        {
            return Err(ServerError::conflict_kind(
                "grant_scope",
                "this grant holds no binding to that session",
            ));
        }
        let session = self.get_session(owner, session_id).await?;
        match session.lifecycle {
            CodeSessionLifecycle::Ended => {
                return Err(ServerError::conflict_kind(
                    "session_ended",
                    "the bound session has ended; the conversation is closed",
                ));
            }
            CodeSessionLifecycle::Fenced => {
                return Err(ServerError::conflict_kind(
                    "session_fenced",
                    "the bound session is fenced pending a reap",
                ));
            }
            _ => {}
        }
        let record = tidebreak_core::db::code::record_external_message(
            &self.db, owner, session_id, event_id, channel_ts, &message,
        )
        .await?;
        let (turn_id, fresh) = match &record {
            tidebreak_core::ExternalMessageRecord::Recorded(row) => (row.id, true),
            tidebreak_core::ExternalMessageRecord::Replay { turn_id } => (*turn_id, false),
        };
        if fresh {
            // Best effort: a refusal leaves the row queued for the sweep.
            let session = self.get_session(owner, session_id).await?;
            if let Err(error) = self.try_promote_remote_head(session).await {
                tracing::warn!(
                    session = %session_id,
                    ?error,
                    "promoting an external message failed; the row stays queued"
                );
            }
        }
        if let Some(row) = tidebreak_core::db::code::list_queued_turns(&self.db, owner, session_id)
            .await?
            .into_iter()
            .find(|row| row.id == turn_id)
        {
            return Ok(ExternalMessageOutcome::Queued(Box::new(row)));
        }
        if let Some(turn) = tidebreak_core::db::code::get_turn(&self.db, owner, turn_id).await? {
            return Ok(ExternalMessageOutcome::NewTurn(Box::new(turn)));
        }
        // The row the first delivery caused was retracted before it ran.
        Ok(ExternalMessageOutcome::Dropped)
    }

    /// Park a message as a durable queue row (decision 69).
    ///
    /// The row id is minted here and becomes the promoted turn's id. The cap
    /// is checked before the insert so an overfull queue answers with a typed
    /// conflict rather than a store error; the store re-checks under the
    /// session write lock, so a racing pair can overshoot by at most one row.
    async fn park_follow_up(
        &self,
        owner: &OwnerId,
        handle: &WorkerHandle,
        session: &CodeSession,
        message: String,
        attachments: Vec<tidebreak_core::ImageRef>,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        let queued = tidebreak_core::db::code::list_queued_turns(&self.db, owner, session.id)
            .await
            .map_err(ServerError::from)?;
        if queued.len() >= CodeQueuedTurn::MAX_PER_SESSION {
            return Err(ServerError::conflict_kind(
                "queue_full",
                format!(
                    "this session may queue at most {} messages",
                    CodeQueuedTurn::MAX_PER_SESSION
                ),
            ));
        }
        let now = chrono::Utc::now();
        let row = tidebreak_core::db::code::enqueue_queued_turn(
            &self.db,
            owner,
            &CodeQueuedTurn {
                id: CodeTurnId::new(),
                session_id: session.id,
                message,
                attachments,
                position: 0,
                created_at: now,
                updated_at: now,
            },
        )
        .await
        .map_err(ServerError::from)?;
        wake_queue(handle);
        Ok(SubmitTurnOutcome::Queued(Box::new(row)))
    }

    /// The session's queued messages plus whether promotion is paused.
    pub(crate) async fn list_queued_turns(
        &self,
        owner: &OwnerId,
        id: CodeSessionId,
    ) -> Result<(Vec<CodeQueuedTurn>, bool), ServerError> {
        let _ = self.get_session(owner, id).await?;
        let queued = tidebreak_core::db::code::list_queued_turns(&self.db, owner, id).await?;
        let paused = tidebreak_core::db::code::queue_paused(&self.db, owner, id).await?;
        Ok((queued, paused))
    }

    /// Edit or reorder one queued message. `None` when the row is gone.
    pub(crate) async fn update_queued_turn(
        &self,
        owner: &OwnerId,
        id: CodeSessionId,
        queued_id: CodeTurnId,
        message: Option<&str>,
        position: Option<i32>,
    ) -> Result<Option<CodeQueuedTurn>, ServerError> {
        let _ = self.get_session(owner, id).await?;
        Ok(tidebreak_core::db::code::update_queued_turn(
            &self.db, owner, id, queued_id, message, position,
        )
        .await?)
    }

    pub(crate) async fn interrupt(&self, id: CodeSessionId) -> Result<(), ServerError> {
        // Interrupt stops only the active turn. The worker and logical code
        // session continue, so its browser capfile and native capability must
        // remain live for later turns.
        if self.workers.lock().expect("code workers").contains_key(&id) {
            let handle = self.require_worker(id)?;
            let (reply, rx) = oneshot::channel();
            handle
                .commands
                .send(WorkerCommand::Interrupt { reply })
                .await
                .map_err(|_| ServerError::internal("session worker is gone"))?;
            return rx
                .await
                .map_err(|_| ServerError::internal("session worker dropped the interrupt"))?
                .map_err(map_worker);
        }
        // No host worker: a remote session's engine lives in a sandbox.
        let sessions = list_sessions_all_owners(&self.db).await?;
        let Some(session) = sessions.into_iter().find(|session| session.id == id) else {
            return Err(ServerError::conflict_kind(
                "session_worker_missing",
                "session worker is not running",
            ));
        };
        let workspace = self
            .get_workspace(&session.owner, session.workspace_id)
            .await?;
        if !workspace.is_remote() {
            return Err(ServerError::conflict_kind(
                "session_worker_missing",
                "session worker is not running",
            ));
        }
        self.interrupt_remote(&session).await
    }

    async fn interrupt_remote(&self, session: &CodeSession) -> Result<(), ServerError> {
        let Some(remote) = self.remote_sessions() else {
            return Err(ServerError::conflict_kind(
                "remote_disabled",
                "this deployment has no sandbox runtime configured",
            ));
        };
        let Some(row) =
            tidebreak_core::db::code::latest_incarnation(&self.db, &session.owner, session.id)
                .await?
        else {
            return Err(ServerError::conflict_kind(
                "no_active_turn",
                "there is no active turn to interrupt",
            ));
        };
        if row.state != tidebreak_core::IncarnationState::Active {
            return Err(ServerError::conflict_kind(
                "no_active_turn",
                "there is no active turn to interrupt",
            ));
        }
        let Some(sandbox_id) = row.sandbox_id.as_deref() else {
            return Err(ServerError::conflict_kind(
                "no_active_turn",
                "there is no active turn to interrupt",
            ));
        };
        let message = super::remote::wire::SandboxMessage {
            body: "stop".to_owned(),
            interrupt: true,
        };
        match remote
            .provisioner
            .send(&session.owner, sandbox_id, &message)
            .await
        {
            Ok(_) => Ok(()),
            Err(super::remote::RemoteSandboxError::SignInRequired(_)) => {
                Err(ServerError::conflict_kind(
                    "sign_in_required",
                    "sign in to the sandbox environment, then retry",
                ))
            }
            Err(error) => Err(ServerError::internal(error.to_string())),
        }
    }

    /// Best-effort stop of a remote session's sandbox. Used when the session
    /// row is ending, so the environment does not keep spending.
    async fn cancel_remote_sandbox(&self, session: &CodeSession) {
        let Some(remote) = self.remote_sessions() else {
            return;
        };
        let Ok(Some(row)) =
            tidebreak_core::db::code::latest_incarnation(&self.db, &session.owner, session.id)
                .await
        else {
            return;
        };
        if let Some(sandbox_id) = row.sandbox_id.as_deref() {
            if let Err(error) = remote.provisioner.cancel(&session.owner, sandbox_id).await {
                tracing::warn!(
                    session = %session.id,
                    %error,
                    "could not cancel a remote sandbox while ending the session"
                );
            }
        }
        if row.state != tidebreak_core::IncarnationState::Stopped {
            let _ = tidebreak_core::db::code::stop_incarnation(
                &self.db,
                &session.owner,
                row.id,
                Some("session_ended"),
            )
            .await;
        }
    }

    /// Retract one queued message. `false` when the row is gone.
    pub(crate) async fn delete_queued_turn(
        &self,
        owner: &OwnerId,
        id: CodeSessionId,
        queued_id: CodeTurnId,
    ) -> Result<bool, ServerError> {
        let _ = self.get_session(owner, id).await?;
        Ok(tidebreak_core::db::code::delete_queued_turn(&self.db, owner, id, queued_id).await?)
    }

    /// Pause or release the session's queue. A release wakes the worker so a
    /// waiting head starts without a new send.
    pub(crate) async fn set_queue_paused(
        &self,
        owner: &OwnerId,
        id: CodeSessionId,
        paused: bool,
    ) -> Result<(), ServerError> {
        let _ = self.get_session(owner, id).await?;
        tidebreak_core::db::code::set_queue_paused(&self.db, owner, id, paused).await?;
        if !paused {
            self.wake_session_queue(id);
        }
        Ok(())
    }

    /// Clear the session's queue pause so the worker's next drain starts the
    /// head row. The tray composes send-now client-side exactly as chat does:
    /// pause, move the row first, stop the live turn, then this.
    pub(crate) async fn send_queued_now(
        &self,
        owner: &OwnerId,
        id: CodeSessionId,
    ) -> Result<(), ServerError> {
        let _ = self.get_session(owner, id).await?;
        tidebreak_core::db::code::set_queue_paused(&self.db, owner, id, false).await?;
        self.wake_session_queue(id);
        Ok(())
    }

    /// Nudge a live worker to re-read its queue. A session with no worker has
    /// nothing to wake; its next spawn drains the queue first thing.
    fn wake_session_queue(&self, id: CodeSessionId) {
        if let Ok(handle) = self.require_worker(id) {
            wake_queue(&handle);
        }
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
            .find(|other| {
                other.id != session.id
                    && other.lifecycle == CodeSessionLifecycle::Fenced
                    // Only a fence that implies an unaccounted engine process
                    // stops the workspace. A sibling fenced for repeated turn
                    // failures answered every time; its worktree is not at
                    // risk, so this session keeps working.
                    && other
                        .fence_reason
                        .as_ref()
                        .is_none_or(FenceReason::blocks_workspace)
            })
            .map(|fenced| {
                format!(
                    "another session in this workspace is fenced until it is reaped ({})",
                    fenced.id
                )
            }))
    }

    pub(crate) async fn steer(
        &self,
        owner: &OwnerId,
        id: CodeSessionId,
        expected_turn_id: CodeTurnId,
        message: String,
    ) -> Result<(), ServerError> {
        self.steer_inner(owner, id, expected_turn_id, message, None)
            .await
    }

    async fn steer_inner(
        &self,
        owner: &OwnerId,
        id: CodeSessionId,
        expected_turn_id: CodeTurnId,
        message: String,
        trigger_delivery: Option<TriggerDeliveryClaim>,
    ) -> Result<(), ServerError> {
        if let Some(claim) = trigger_delivery {
            if tidebreak_core::db::code::trigger_delivery_accepted(
                &self.db,
                owner,
                claim.delivery_id,
            )
            .await?
            {
                return Ok(());
            }
        }
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
        let mut accepted_trigger_delivery = None;
        if let Some(claim) = trigger_delivery {
            let accepted = tidebreak_core::db::code::accept_trigger_delivery(
                &self.db,
                owner,
                claim.delivery_id,
                claim.lease_token,
                tidebreak_core::CodeTriggerDeliverySink::Steer,
                id,
                Some(expected_turn_id),
                Utc::now(),
            )
            .await?;
            if !accepted {
                return Ok(());
            }
            accepted_trigger_delivery = Some(claim.delivery_id);
        }
        let (reply, rx) = oneshot::channel();
        let result = async {
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
        .await;
        if let Some(delivery_id) = accepted_trigger_delivery {
            // Engine steering has no durable enqueue boundary. The receipt is
            // therefore the at-most-once acceptance point: after it commits,
            // a send failure or ambiguous engine response must not retry and
            // risk applying the same instruction twice.
            if let Err(error) = result {
                tracing::warn!(
                    delivery = %delivery_id,
                    session = %id,
                    turn = %expected_turn_id,
                    error = %error.message(),
                    "trigger steering failed after durable acceptance"
                );
            }
            return Ok(());
        }
        result
    }

    /// Steer an active turn for one durable trigger delivery.
    pub(crate) async fn steer_trigger(
        &self,
        owner: &OwnerId,
        id: CodeSessionId,
        expected_turn_id: CodeTurnId,
        message: String,
        delivery_id: tidebreak_core::CodeTriggerDeliveryId,
        lease_token: uuid::Uuid,
    ) -> Result<(), ServerError> {
        self.steer_inner(
            owner,
            id,
            expected_turn_id,
            message,
            Some(TriggerDeliveryClaim {
                delivery_id,
                lease_token,
            }),
        )
        .await
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
        let workspace = self.get_workspace(owner, session.workspace_id).await?;
        if workspace.is_remote() {
            // No worker to shut down and nothing to relaunch: the driver
            // cancels whatever the environment still holds, closes the
            // incarnation, and resolves the fence. The next turn
            // reincarnates on demand.
            let Some(remote) = self.remote_sessions() else {
                return Err(ServerError::conflict_kind(
                    "remote_disabled",
                    "this deployment has no sandbox runtime configured",
                ));
            };
            let driver = remote.driver(&self.db, self.bus.as_ref());
            return driver.reap(session).await.map_err(|error| match error {
                recovery::ReapSessionError::Store(error) => ServerError::from(error),
                other => ServerError::conflict_kind("session_not_reaped", other.to_string()),
            });
        }
        let handle = self.workers.lock().expect("code workers").remove(&id);
        let decision_gate = handle
            .as_ref()
            .map(|handle| handle.approval_decisions.clone());
        let _decision_guard = match decision_gate {
            Some(gate) => Some(gate.lock_owned().await),
            None => None,
        };
        self.revoke_worker_channels(id);
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
            .map_err(|error| match error {
                recovery::ReapSessionError::Store(error) => ServerError::from(error),
                other => ServerError::conflict_kind("session_not_reaped", other.to_string()),
            })?;
        self.attach_and_spawn_worker(session).await
    }

    /// Change a session's permission mode through one durable intent.
    ///
    /// A live worker stays inside the mode-change command after native
    /// acknowledgement. It cannot accept another turn until the exact owner,
    /// lifecycle, worker epoch, prior mode, and revision confirm in storage.
    /// If confirmation fails, the worker stops before this method returns.
    ///
    /// Refused while a turn is running. A turn that began under one posture
    /// must not have it changed underneath it — that is the whole point of
    /// the posture.
    pub(crate) async fn set_permission_mode(
        &self,
        owner: &OwnerId,
        id: CodeSessionId,
        mode: PermissionMode,
    ) -> Result<CodeSession, ServerError> {
        let session = self.get_session(owner, id).await?;
        if session.permission_mode == mode {
            return Ok(session);
        }
        match session.lifecycle {
            CodeSessionLifecycle::Running => {
                return Err(ServerError::conflict_kind(
                    "turn_running",
                    "finish or interrupt the running turn before changing the permission mode",
                ));
            }
            CodeSessionLifecycle::Ended => {
                return Err(ServerError::conflict_kind(
                    "session_ended",
                    "this session has ended; start a new one to pick a different mode",
                ));
            }
            _ => {}
        }

        let workspace = self.get_workspace(owner, session.workspace_id).await?;
        if workspace.is_remote() {
            // The sandbox carries the engine. Persist the mode on the row and
            // do not relaunch a host harness against the empty worktree.
            let mut session = session;
            session.permission_mode = mode;
            super::attention::persist_session(&self.db, &self.bus, &session).await?;
            return Ok(session);
        }

        // Refuse a mode this engine cannot honor here, not at the next turn,
        // and with the same rule session creation applies (decision 0038).
        let adapter = self.adapter(session.harness_kind)?;
        let probe = self.probe(adapter.as_ref()).await;
        let caps = adapter.capabilities(&probe);
        refuse_unhonored_mode(session.harness_kind, mode, &caps)?;

        let intent = begin_permission_mode_change(&self.db, owner, &session, mode)
            .await?
            .ok_or_else(|| {
                ServerError::conflict_kind(
                    "permission_mode_changed",
                    "the session changed before the permission mode could be reserved",
                )
            })?;

        let live = match self
            .repostured_in_place(id, intent.worker_epoch, mode)
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                match cancel_permission_mode_change(&self.db, owner, &intent).await {
                    Ok(true) => return Err(error),
                    Ok(false) => {
                        self.retire_permission_mode_worker(&intent).await;
                        let _ = discard_permission_mode_change(&self.db, owner, &intent).await;
                    }
                    Err(cancel_error) => {
                        self.retire_permission_mode_worker(&intent).await;
                        tracing::warn!(
                            session = %id,
                            error = %cancel_error,
                            "could not cancel a failed permission-mode intent"
                        );
                    }
                }
                return Err(error);
            }
        };

        if let LivePermissionModeOutcome::Acknowledged(change) = live {
            return match confirm_permission_mode_change(&self.db, owner, &intent).await {
                Ok(true) => {
                    if change
                        .settlement
                        .send(PermissionModeSettlement::Confirmed)
                        .is_err()
                    {
                        self.retire_permission_mode_worker(&intent).await;
                        let session = self.get_session(owner, id).await?;
                        return self.attach_and_spawn_worker(session).await;
                    }
                    let session = self.get_session(owner, id).await?;
                    self.note_permission_mode(
                        owner,
                        &session,
                        intent.previous_mode,
                        mode,
                        intent.revision,
                    )
                    .await;
                    Ok(session)
                }
                Ok(false) => {
                    let fenced = self
                        .stop_and_fence_permission_mode_change(owner, &intent, change)
                        .await?;
                    if fenced.is_some() {
                        Err(ServerError::conflict_kind(
                            "permission_mode_unconfirmed",
                            "the engine accepted the permission mode, but the durable session changed before confirmation; reap the fenced session before another turn",
                        ))
                    } else {
                        Err(ServerError::conflict_kind(
                            "permission_mode_changed",
                            "the engine accepted the permission mode, but a newer session state superseded it",
                        ))
                    }
                }
                Err(error) => {
                    if let Err(fence_error) = self
                        .stop_and_fence_permission_mode_change(owner, &intent, change)
                        .await
                    {
                        tracing::warn!(
                            session = %id,
                            error = %fence_error.message(),
                            "could not persist the permission-mode failure fence"
                        );
                    }
                    Err(ServerError::from(error))
                }
            };
        }

        // A relaunch is the fallback, and it only works where rebuilding the
        // launch plan carries the mode. An engine that fixed its posture when
        // it created the session, and cannot re-apply it on resume, would come
        // back running the old one while the record claimed the new one.
        if !adapter.relaunch_composes_permission_mode() && session.harness_resume_ref.is_some() {
            let cancelled = matches!(
                cancel_permission_mode_change(&self.db, owner, &intent).await,
                Ok(true)
            );
            if !cancelled {
                self.retire_permission_mode_worker(&intent).await;
                let reason = permission_mode_fence_reason(&intent);
                if let Some(fenced) =
                    fence_permission_mode_change(&self.db, owner, &intent, &reason).await?
                {
                    super::attention::emit_digest(&self.db, &self.bus, &fenced).await;
                } else {
                    let _ = discard_permission_mode_change(&self.db, owner, &intent).await?;
                }
            }
            return Err(ServerError::conflict_kind(
                "permission_mode_fixed",
                format!(
                    "{} fixes its permission mode when the session starts; start a new session to pick a different one",
                    session.harness_kind
                ),
            ));
        }

        let handle = self.take_worker_for_epoch(id, intent.worker_epoch);
        let decision_gate = handle
            .as_ref()
            .map(|handle| handle.approval_decisions.clone());
        let _decision_guard = match decision_gate {
            Some(gate) => Some(gate.lock_owned().await),
            None => None,
        };
        if handle.is_some() {
            self.revoke_worker_channels(id);
        }
        if let Some(handle) = handle {
            if !Self::shut_down_worker(id, handle).await {
                let reason = permission_mode_fence_reason(&intent);
                let fenced =
                    fence_permission_mode_change(&self.db, owner, &intent, &reason).await?;
                if let Some(fenced) = fenced {
                    super::attention::emit_digest(&self.db, &self.bus, &fenced).await;
                } else {
                    let _ = discard_permission_mode_change(&self.db, owner, &intent).await?;
                }
                return Err(ServerError::conflict_kind(
                    "permission_mode_unconfirmed",
                    "the session worker did not stop while changing permission mode; reap the fenced session before another turn",
                ));
            }
        }

        super::approval_sweep::abandon_for_restart(
            &self.db,
            &self.bus,
            owner,
            intent.session_id,
            intent.worker_epoch,
        )
        .await;
        if !confirm_permission_mode_change(&self.db, owner, &intent).await? {
            let _ = discard_permission_mode_change(&self.db, owner, &intent).await;
            return Err(ServerError::conflict_kind(
                "permission_mode_changed",
                "the session changed while its worker stopped for the permission mode update",
            ));
        }
        let session = self.get_session(owner, id).await?;
        self.note_permission_mode(owner, &session, intent.previous_mode, mode, intent.revision)
            .await;

        self.attach_and_spawn_worker(session).await
    }

    /// Ask the live engine to take a mode, then hold its worker for settlement.
    async fn repostured_in_place(
        &self,
        id: CodeSessionId,
        expected_spawn_epoch: i64,
        mode: PermissionMode,
    ) -> Result<LivePermissionModeOutcome, ServerError> {
        let Ok(handle) = self.require_worker(id) else {
            return Ok(LivePermissionModeOutcome::Unavailable);
        };
        if handle.spawn_epoch != expected_spawn_epoch {
            return Ok(LivePermissionModeOutcome::Unavailable);
        }
        let (reply, rx) = oneshot::channel();
        let (settlement, settle) = oneshot::channel();
        if handle
            .commands
            .send(WorkerCommand::SetPermissionMode {
                mode,
                settlement: settle,
                reply,
            })
            .await
            .is_err()
        {
            return Ok(LivePermissionModeOutcome::Unavailable);
        }
        match rx.await {
            Ok(Ok(())) => Ok(LivePermissionModeOutcome::Acknowledged(
                LivePermissionModeChange { settlement, handle },
            )),
            Ok(Err(WorkerError::RelaunchRequired(_))) => {
                Ok(LivePermissionModeOutcome::RelaunchRequired)
            }
            Ok(Err(err)) => Err(map_worker(err)),
            // The worker went away mid-request. Relaunching is the repair.
            Err(_) => Ok(LivePermissionModeOutcome::Unavailable),
        }
    }

    async fn stop_and_fence_permission_mode_change(
        &self,
        owner: &OwnerId,
        intent: &PermissionModeChangeIntent,
        change: LivePermissionModeChange,
    ) -> Result<Option<CodeSession>, ServerError> {
        let LivePermissionModeChange { settlement, handle } = change;
        let _ = settlement.send(PermissionModeSettlement::Abort);
        let handle = if let Some(registered) =
            self.take_worker_for_epoch(intent.session_id, intent.worker_epoch)
        {
            self.revoke_worker_channels(intent.session_id);
            drop(handle);
            registered
        } else {
            handle
        };
        let _ = Self::shut_down_worker(intent.session_id, handle).await;
        let reason = permission_mode_fence_reason(intent);
        let fenced = fence_permission_mode_change(&self.db, owner, intent, &reason).await?;
        if let Some(fenced) = &fenced {
            super::attention::emit_digest(&self.db, &self.bus, fenced).await;
        } else {
            let _ = discard_permission_mode_change(&self.db, owner, intent).await;
        }
        Ok(fenced)
    }

    async fn retire_permission_mode_worker(&self, intent: &PermissionModeChangeIntent) {
        let Some(handle) = self.take_worker_for_epoch(intent.session_id, intent.worker_epoch)
        else {
            return;
        };
        self.revoke_worker_channels(intent.session_id);
        let _ = Self::shut_down_worker(intent.session_id, handle).await;
    }

    async fn commit_execution_settings(
        &self,
        expected: &CodeSession,
        next: &CodeSessionExecutionSettings,
        action: &'static str,
    ) -> Result<CodeSession, ServerError> {
        let applies_to_future_turn = expected.lifecycle == CodeSessionLifecycle::Running
            || get_open_turn(&self.db, &expected.owner, expected.id)
                .await?
                .is_some();
        if applies_to_future_turn {
            return replace_session_execution_settings(&self.db, &expected.owner, expected, next)
                .await?
                .ok_or_else(|| {
                    ServerError::conflict_kind(
                        "session_settings_changed",
                        format!("the session settings changed before {action}"),
                    )
                });
        }

        let handle = self.require_worker(expected.id)?;
        if handle.spawn_epoch != expected.spawn_epoch {
            return Err(ServerError::conflict_kind(
                "session_worker_changed",
                "the session worker changed before the settings update",
            ));
        }
        let (reply, response) = oneshot::channel();
        let (settlement, release) = oneshot::channel();
        handle
            .commands
            .send(WorkerCommand::SetExecutionSettings {
                settings: next.clone(),
                settlement: release,
                reply,
            })
            .await
            .map_err(|_| {
                ServerError::conflict_kind(
                    "session_worker_missing",
                    "the session worker stopped before the settings update",
                )
            })?;
        response
            .await
            .map_err(|_| {
                ServerError::conflict_kind(
                    "session_worker_missing",
                    "the session worker stopped before reserving the settings update",
                )
            })?
            .map_err(map_worker)?;

        let updated =
            match replace_session_execution_settings(&self.db, &expected.owner, expected, next)
                .await
            {
                Ok(Some(updated)) => updated,
                Ok(None) => {
                    let _ = settlement.send(ExecutionSettingsSettlement::Abort);
                    return Err(ServerError::conflict_kind(
                        "session_settings_changed",
                        format!("the session settings changed before {action}"),
                    ));
                }
                Err(error) => {
                    let _ = settlement.send(ExecutionSettingsSettlement::Abort);
                    return Err(ServerError::from(error));
                }
            };
        if settlement
            .send(ExecutionSettingsSettlement::Confirmed)
            .is_err()
        {
            if let Some(handle) = self.take_worker_for_epoch(expected.id, expected.spawn_epoch) {
                self.revoke_worker_channels(expected.id);
                let _ = Self::shut_down_worker(expected.id, handle).await;
            }
            return self.attach_and_spawn_worker(updated).await;
        }
        Ok(updated)
    }

    fn take_worker_for_epoch(&self, id: CodeSessionId, spawn_epoch: i64) -> Option<WorkerHandle> {
        let mut workers = self.workers.lock().expect("code workers");
        let exact = workers
            .get(&id)
            .is_some_and(|handle| handle.spawn_epoch == spawn_epoch);
        exact.then(|| workers.remove(&id)).flatten()
    }

    /// Journal a mode change so the transcript says when the posture moved.
    async fn note_permission_mode(
        &self,
        owner: &OwnerId,
        session: &CodeSession,
        previous: PermissionMode,
        mode: PermissionMode,
        revision: i64,
    ) {
        let _ = super::session_worker::journal_event(
            &self.db,
            &self.bus,
            owner,
            session.id,
            session.spawn_epoch,
            CodeEvent::HarnessNotice {
                level: tidebreak_core::HarnessNoticeLevel::Info,
                message: format!(
                    "permission mode changed from {previous} to {mode} at revision {revision}"
                ),
            },
        )
        .await;
    }

    /// Change a session's reasoning effort. `None` hands the level back to the
    /// engine's own default.
    ///
    /// No relaunch and no engine call: every adapter reads the effort off the
    /// turn, so persisting it is the whole switch. Refused mid-turn all the
    /// same — a level that changed under a running turn would report a session
    /// setting the turn did not run at.
    pub(crate) async fn set_reasoning_effort(
        &self,
        owner: &OwnerId,
        id: CodeSessionId,
        effort: Option<ReasoningEffort>,
    ) -> Result<CodeSession, ServerError> {
        let session = self.get_session(owner, id).await?;
        match session.lifecycle {
            CodeSessionLifecycle::Running => {
                return Err(ServerError::conflict_kind(
                    "turn_running",
                    "finish or interrupt the running turn before changing the reasoning effort",
                ));
            }
            CodeSessionLifecycle::Ended => {
                return Err(ServerError::conflict_kind(
                    "session_ended",
                    "this session has ended; start a new one to pick a different effort",
                ));
            }
            _ => {}
        }
        if get_open_turn(&self.db, owner, id).await?.is_some() {
            return Err(ServerError::conflict_kind(
                "turn_running",
                "finish or interrupt the running turn before changing the reasoning effort",
            ));
        }
        let adapter = self.adapter(session.harness_kind)?;
        let probe = self.probe(adapter.as_ref()).await;
        let selected = self
            .selected_model_capabilities_for_owner(
                owner,
                adapter.as_ref(),
                &probe,
                session.model.as_deref(),
            )
            .await;
        let mut next = CodeSessionExecutionSettings::from(&session);
        selected.deactivate_unsupported(&mut next);
        next.reasoning_effort = effort;
        Self::validate_execution_settings(session.harness_kind, &next, &selected)?;
        if next == CodeSessionExecutionSettings::from(&session) {
            return Ok(session);
        }
        self.commit_execution_settings(&session, &next, "the reasoning effort could be saved")
            .await
    }

    /// Arm or disarm the engine's fast mode for a session.
    ///
    /// Refused mid-turn and after the session ends, on the rule the effort
    /// route already applies. Enabling is also refused unless the selected
    /// model advertises the tier, so the session snapshot never claims that a
    /// standard-speed turn is fast.
    pub(crate) async fn set_fast_mode(
        &self,
        owner: &OwnerId,
        id: CodeSessionId,
        fast_mode: bool,
    ) -> Result<CodeSession, ServerError> {
        let session = self.get_session(owner, id).await?;
        match session.lifecycle {
            CodeSessionLifecycle::Running => {
                return Err(ServerError::conflict_kind(
                    "turn_running",
                    "finish or interrupt the running turn before changing fast mode",
                ));
            }
            CodeSessionLifecycle::Ended => {
                return Err(ServerError::conflict_kind(
                    "session_ended",
                    "this session has ended; start a new one to run it in fast mode",
                ));
            }
            _ => {}
        }
        if get_open_turn(&self.db, owner, id).await?.is_some() {
            return Err(ServerError::conflict_kind(
                "turn_running",
                "finish or interrupt the running turn before changing fast mode",
            ));
        }
        let adapter = self.adapter(session.harness_kind)?;
        let probe = self.probe(adapter.as_ref()).await;
        let selected = self
            .selected_model_capabilities_for_owner(
                owner,
                adapter.as_ref(),
                &probe,
                session.model.as_deref(),
            )
            .await;
        let mut next = CodeSessionExecutionSettings::from(&session);
        selected.deactivate_unsupported(&mut next);
        next.fast_mode = fast_mode;
        Self::validate_execution_settings(session.harness_kind, &next, &selected)?;
        if next == CodeSessionExecutionSettings::from(&session) {
            return Ok(session);
        }
        self.commit_execution_settings(&session, &next, "fast mode could be saved")
            .await
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

    async fn selected_model_capabilities(
        adapter: &dyn HarnessAdapter,
        probe: &HarnessProbe,
        selected: Option<&str>,
    ) -> SelectedModelCapabilities {
        let caps = adapter.capabilities(probe);
        let listed = adapter.list_models(probe).await;
        // Empty is inconclusive: adapters also return it when model listing
        // fails. Only catalog evidence may disable an already stored setting.
        let catalog_known = !listed.is_empty();
        let model = listed.iter().find(|model| match selected {
            Some(selected) => model.id == selected,
            None => model.default,
        });
        let (reasoning_efforts, reasoning_known) = if caps.reasoning_levels == CapLevel::Supported {
            match (selected, model) {
                (_, Some(model)) => (model.reasoning_efforts.clone(), true),
                (None, None) => {
                    let mut levels = adapter.reasoning_efforts(probe);
                    if levels.is_empty() {
                        levels = listed
                            .iter()
                            .flat_map(|model| model.reasoning_efforts.iter().copied())
                            .collect::<std::collections::BTreeSet<_>>()
                            .into_iter()
                            .collect();
                    }
                    let reasoning_known = catalog_known || !levels.is_empty();
                    (levels, reasoning_known)
                }
                (Some(_), None) => (Vec::new(), catalog_known),
            }
        } else {
            (Vec::new(), true)
        };
        SelectedModelCapabilities {
            reasoning_efforts,
            listed_model_reasoning_efforts: model.map(|model| model.reasoning_efforts.clone()),
            reasoning_known,
            fast_mode: model.is_some_and(|model| model.fast_mode),
            fast_mode_known: model.is_some() || catalog_known,
        }
    }

    async fn selected_model_capabilities_for_owner(
        &self,
        owner: &OwnerId,
        adapter: &dyn HarnessAdapter,
        probe: &HarnessProbe,
        selected: Option<&str>,
    ) -> SelectedModelCapabilities {
        let mut capabilities = Self::selected_model_capabilities(adapter, probe, selected).await;
        let Some(selected) = selected else {
            return capabilities;
        };
        if adapter.capabilities(probe).reasoning_levels != CapLevel::Supported {
            return capabilities;
        }
        let Some(snapshot) = self.gateway_model_snapshot(owner).await else {
            return capabilities;
        };
        let Some(model_efforts) =
            crate::providers::gateway_reasoning_efforts_for_model(&snapshot, selected)
        else {
            return capabilities;
        };
        let engine_efforts = adapter.reasoning_efforts(probe);
        capabilities.reasoning_efforts = crate::providers::effective_gateway_reasoning_efforts(
            self.harness_llm.is_some(),
            capabilities.listed_model_reasoning_efforts.as_deref(),
            &engine_efforts,
            model_efforts,
        );
        capabilities.reasoning_known = true;
        tracing::debug!(
            harness = %adapter.kind(),
            model = selected,
            efforts = ?capabilities.reasoning_efforts,
            "using the gateway model's reasoning effort ladder"
        );
        capabilities
    }

    fn validate_execution_settings(
        harness: HarnessKind,
        settings: &CodeSessionExecutionSettings,
        selected: &SelectedModelCapabilities,
    ) -> Result<(), ServerError> {
        let model = settings.model.as_deref().unwrap_or("the default model");
        if let Some(effort) = settings.reasoning_effort {
            if !selected.supports_reasoning(effort) {
                return Err(ServerError::unprocessable_kind(
                    "reasoning_effort_unsupported",
                    format!(
                        "{harness} model {model} does not support reasoning effort {}",
                        effort.as_str()
                    ),
                ));
            }
        }
        if settings.fast_mode && !selected.fast_mode {
            return Err(ServerError::unprocessable_kind(
                "fast_mode_unsupported",
                format!("{harness} model {model} does not support fast mode"),
            ));
        }
        Ok(())
    }

    /// Refuse to mint a session that will 401 on its first turn (issue 2653).
    ///
    /// Fires only on a definitive signed-out observation with no other auth
    /// mode in sight: the relay carries covered engines as the caller
    /// (decision 71), and an API key or gateway endpoint override in the
    /// environment or engine config authenticates without any vendor login
    /// (issue 2749). An unobserved sign-in state (`None`) stays allowed — a
    /// false refusal on a working machine is worse than the first-turn
    /// failure this replaces, which the worker at least maps to the same
    /// legible message.
    fn refuse_signed_out_harness(
        &self,
        harness: HarnessKind,
        probe: &HarnessProbe,
    ) -> Result<(), ServerError> {
        Self::signed_out_harness_refusal(self.harness_llm.is_some(), harness, probe)
    }

    fn signed_out_harness_refusal(
        relay_active: bool,
        harness: HarnessKind,
        probe: &HarnessProbe,
    ) -> Result<(), ServerError> {
        if probe.authenticated != Some(false) {
            return Ok(());
        }
        if relay_active && super::harness_llm::relay_covered(harness) {
            return Ok(());
        }
        if tidebreak_harness::auth_override_present(harness, &probe.env) {
            return Ok(());
        }
        let label = super::harness_label(harness);
        Err(ServerError::unprocessable_kind(
            "harness_not_authenticated",
            format!(
                "{label} is not signed in on this machine. \
                 Sign in to {label} in your own terminal, then start the session again."
            ),
        ))
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

    /// Triggers armed on one repository.
    pub(crate) async fn list_triggers(
        &self,
        owner: &OwnerId,
        repo_id: RepoId,
    ) -> Result<Vec<CodeTrigger>, ServerError> {
        // Refuses an unknown repository rather than returning an empty list,
        // so a stale id reads as an error and not as "none armed".
        self.get_repo(owner, repo_id).await?;
        Ok(list_triggers_for_repo(&self.db, owner, repo_id).await?)
    }

    /// Arm a trigger on a repository.
    ///
    /// One row per `(repository, condition)`. A later arm sets its action and
    /// enables it in one upsert.
    pub(crate) async fn create_trigger(
        &self,
        owner: &OwnerId,
        repo_id: RepoId,
        condition: CodeTriggerCondition,
        action: CodeTriggerAction,
    ) -> Result<CodeTrigger, ServerError> {
        self.get_repo(owner, repo_id).await?;
        let now = Utc::now();
        let trigger = CodeTrigger {
            id: CodeTriggerId::new(),
            owner: owner.clone(),
            repo_id,
            condition,
            action,
            enabled: true,
            created_at: now,
            updated_at: now,
        };
        arm_trigger(&self.db, owner, &trigger).await?;
        // Read back rather than returning what we just built. Two arms of the
        // same condition racing each other both see no row and both mint an
        // id; only one is stored, and the loser would otherwise answer 201
        // with an id that GET, PATCH and DELETE cannot find.
        list_triggers_for_repo(&self.db, owner, repo_id)
            .await?
            .into_iter()
            .find(|trigger| trigger.condition == condition)
            .ok_or_else(|| ServerError::internal("the trigger vanished after it was saved"))
    }

    /// Switch a trigger on or off, keeping the row so the scoping survives.
    pub(crate) async fn set_trigger_enabled(
        &self,
        owner: &OwnerId,
        repo_id: RepoId,
        id: CodeTriggerId,
        enabled: bool,
    ) -> Result<CodeTrigger, ServerError> {
        if !update_trigger_enabled(&self.db, owner, repo_id, id, enabled, Utc::now()).await? {
            return Err(ServerError::not_found("trigger not found"));
        }
        list_triggers_for_repo(&self.db, owner, repo_id)
            .await?
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| ServerError::not_found("trigger not found"))
    }

    /// Remove a repository-scoped trigger and its recorded fire fingerprints.
    pub(crate) async fn delete_trigger(
        &self,
        owner: &OwnerId,
        repo_id: RepoId,
        id: CodeTriggerId,
    ) -> Result<(), ServerError> {
        if delete_trigger(&self.db, owner, repo_id, id).await? {
            Ok(())
        } else {
            Err(ServerError::not_found("trigger not found"))
        }
    }

    /// Start the trigger sweep once. Same weak-handle shape as the watch
    /// sweep, on its own interval so the two do not read GitHub together.
    pub(super) fn ensure_trigger_sweep(self: &Arc<Self>) {
        if self.trigger_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let guard = super::trigger::TriggerSweepGuard::spawn(Arc::downgrade(self));
        *self.trigger_sweep.lock().expect("trigger sweep") = Some(guard);
    }

    /// Start the pull-request reconcile sweep once (decision 77). Same
    /// weak-handle shape, on a third coprime interval.
    pub(super) fn ensure_reconcile_sweep(self: &Arc<Self>) {
        if self.reconcile_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let guard = super::reconcile::ReconcileSweepGuard::spawn(Arc::downgrade(self));
        *self.reconcile_sweep.lock().expect("reconcile sweep") = Some(guard);
    }

    /// Start the remote-session sweep once, on deployments that configured
    /// remote execution. A no-op everywhere else.
    pub(super) fn ensure_remote_sweep(self: &Arc<Self>) {
        if self.remote.is_none() || self.remote_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let guard = super::remote::service::RemoteSweepGuard::spawn(Arc::downgrade(self));
        *self.remote_sweep.lock().expect("remote sweep") = Some(guard);
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
        if let Ok(workspace) = self.get_workspace(owner, session.workspace_id).await {
            if workspace.is_remote() {
                self.cancel_remote_sandbox(&session).await;
            }
        }
        let handle = self
            .workers
            .lock()
            .expect("code workers")
            .remove(&session.id);
        let decision_gate = handle
            .as_ref()
            .map(|handle| handle.approval_decisions.clone());
        let _decision_guard = match decision_gate {
            Some(gate) => Some(gate.lock_owned().await),
            None => None,
        };
        self.revoke_browser_session(&session);
        session.lifecycle = CodeSessionLifecycle::Ended;
        session.child_pid = None;
        session.child_process_identity = None;
        session.fence_reason = None;
        super::attention::persist_session(&self.db, &self.bus, &session).await?;
        let stopped = match handle {
            Some(handle) => Self::shut_down_worker(session.id, handle).await,
            None => true,
        };
        let mut current = self.get_session(owner, session.id).await?;
        current.lifecycle = CodeSessionLifecycle::Ended;
        current.child_pid = None;
        current.child_process_identity = None;
        current.fence_reason = None;
        if !super::attention::persist_session(&self.db, &self.bus, &current).await? {
            return Err(ServerError::conflict_kind(
                "session_not_ended",
                "the session did not stay ended after the worker stopped",
            ));
        }
        if stopped {
            super::approval_sweep::abandon_for_restart(
                &self.db,
                &self.bus,
                owner,
                current.id,
                current.spawn_epoch,
            )
            .await;
        } else {
            super::approval_sweep::abandon_for_ended_session(
                &self.db,
                &self.bus,
                owner,
                current.id,
                current.spawn_epoch,
            )
            .await;
        }
        // Nothing will ever promote an ended session's queued rows; retract
        // them so the queue does not read as pending work forever.
        if let Err(error) =
            tidebreak_core::db::code::delete_session_queued_turns(&self.db, owner, current.id).await
        {
            tracing::warn!(
                session = %current.id,
                error = %error,
                "could not clear the ended session's queued turns"
            );
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

    /// The external bindings behind `session_ids`, for provenance display.
    /// Sessions the desktop created have none and are simply absent.
    pub(crate) async fn external_bindings_for_sessions(
        &self,
        owner: &OwnerId,
        session_ids: &[CodeSessionId],
    ) -> Result<Vec<tidebreak_core::CodeExternalBinding>, ServerError> {
        Ok(
            tidebreak_core::db::code::list_external_bindings_for_sessions(
                &self.db,
                owner,
                session_ids,
            )
            .await?,
        )
    }

    pub(crate) async fn list_session_turns(
        &self,
        owner: &OwnerId,
        session_id: CodeSessionId,
    ) -> Result<Vec<CodeTurn>, ServerError> {
        let _ = self.get_session(owner, session_id).await?;
        Ok(list_turns(&self.db, owner, session_id).await?)
    }

    pub(crate) async fn list_turn_metrics(
        &self,
        owner: &OwnerId,
    ) -> Result<Vec<tidebreak_core::db::code::CodeTurnMetric>, ServerError> {
        Ok(tidebreak_core::db::code::list_turn_metrics(&self.db, owner).await?)
    }

    pub(crate) async fn list_pull_request_facts(
        &self,
        owner: &OwnerId,
    ) -> Result<Vec<tidebreak_core::CodePullRequestFact>, ServerError> {
        Ok(tidebreak_core::db::code::list_pull_request_facts(&self.db, owner).await?)
    }

    pub(crate) async fn list_pull_request_attributions(
        &self,
        owner: &OwnerId,
    ) -> Result<Vec<tidebreak_core::CodePullRequestAttribution>, ServerError> {
        Ok(tidebreak_core::db::code::list_pull_request_attributions(&self.db, owner).await?)
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
        let events = list_events(&self.db, owner, session_id, 0, MAX_REPLAY_EVENTS).await?;
        Ok((session, turns, events.events))
    }

    /// Write one fork of this session — the condensed transcript plus a full
    /// record per turn — into private storage, for a child agent to read.
    ///
    /// `at_turn` forks at the end of that turn; `None` forks at the newest.
    /// The caller creates the child and names the absolute path in its first
    /// message. Git cannot index the transcript or its attachments.
    pub(crate) async fn fork_transcript(
        &self,
        owner: &OwnerId,
        session_id: CodeSessionId,
        at_turn: Option<tidebreak_core::CodeTurnId>,
    ) -> Result<fork::WrittenTranscript, ServerError> {
        let session = self.get_session(owner, session_id).await?;
        let workspace = self
            .require_live_workspace(owner, session.workspace_id)
            .await?;
        let private_root =
            super::scratch::workspace_root(&self.data_dir, workspace.id).map_err(|err| {
                ServerError::internal(format!("could not open private storage: {err}"))
            })?;

        // A session-level fork promises all accepted work through the newest
        // turn. Reserve the checkout long enough to take that snapshot so a
        // turn cannot start or finish halfway through preparation. An
        // explicit earlier turn is already a stable seam and stays available
        // while later work runs.
        let turn_lock = at_turn
            .is_none()
            .then(|| self.worktree_turn_lock(workspace.id));
        let _turn_guard = turn_lock
            .as_ref()
            .map(|lock| {
                lock.try_lock().map_err(|_| {
                    ServerError::conflict_kind(
                        "fork_turn_unsettled",
                        "a turn is still changing this workspace; wait for it to finish, or fork from an earlier completed turn",
                    )
                })
            })
            .transpose()?;

        let turns = list_turns(&self.db, owner, session_id).await?;
        let pending_approval_turns: HashSet<CodeTurnId> = list_approvals(
            &self.db,
            owner,
            Some(CodeApprovalState::Pending),
            Some(session_id),
        )
        .await?
        .into_iter()
        .map(|approval| approval.turn_id)
        .collect();
        let has_queued_follow_up = at_turn.is_none()
            && queued_turn_head(&self.db, owner, session_id)
                .await?
                .is_some();
        let Some(prepared_cut) = fork::cut_at(&turns, at_turn) else {
            return Err(ServerError::bad_request(
                "that turn is not part of this session",
            ));
        };
        let Some(boundary) = prepared_cut.turns.last() else {
            let error = fork::ForkBoundaryError::NoTurns;
            return Err(ServerError::conflict_kind(error.kind(), error.message()));
        };
        let replay =
            list_fork_events(&self.db, owner, session_id, boundary.id, MAX_REPLAY_EVENTS).await?;
        let cut = fork::cut_at_settled_boundary(
            &turns,
            replay.boundary_status,
            &pending_approval_turns,
            has_queued_follow_up,
            at_turn,
        )
        .map_err(|error| match error {
            fork::ForkBoundaryError::UnknownTurn => ServerError::bad_request(error.message()),
            _ => ServerError::conflict_kind(error.kind(), error.message()),
        })?;
        drop(_turn_guard);

        fork::write_transcript(
            &private_root,
            self.blobs.as_ref(),
            &session,
            cut,
            &replay.events,
            &replay.complete_turns,
        )
        .await
        .map_err(|err| ServerError::internal(format!("could not write the fork transcript: {err}")))
    }

    /// Download the workspace pull request's failing job logs into private
    /// storage, and report where they landed.
    ///
    /// The fix-errors action calls this before it sends its prompt, so the
    /// agent opens a file instead of working out which job failed and asking
    /// GitHub for it. The digest is read fresh: fixing against the logs of a
    /// head that has already been superseded is worse than not attaching any.
    pub(crate) async fn workspace_check_logs(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
    ) -> Result<(Option<String>, ci_logs::WrittenCheckLogs), ServerError> {
        let workspace = self.require_live_workspace(owner, workspace_id).await?;
        let status = self.refresh_workspace_pr(owner, workspace_id).await?;
        let Some(pr) = status.pr else {
            return Err(ServerError::not_found(
                "no pull request found for this workspace",
            ));
        };
        let gh = gh::observe_gh(self.gh_search_path_owned().as_deref()).await;
        let binary = gh::require_gh_binary(&gh).map_err(map_gh)?;
        let private_root =
            super::scratch::workspace_root(&self.data_dir, workspace.id).map_err(|err| {
                ServerError::internal(format!("could not open private storage: {err}"))
            })?;
        let written = ci_logs::write_failing_check_logs(
            &private_root,
            &binary,
            pr.checks.as_deref().unwrap_or(&[]),
            pr.head_sha.as_deref(),
        )
        .await
        .map_err(|err| ServerError::internal(format!("could not write the check logs: {err}")))?;
        Ok((pr.head_sha, written))
    }

    pub(crate) async fn workspace_tree(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        query: &str,
        limit: Option<u32>,
    ) -> Result<(Vec<String>, bool), ServerError> {
        let workspace = self.get_workspace(owner, workspace_id).await?;
        if workspace.is_remote() {
            return Err(ServerError::conflict_kind(
                "workspace_remote",
                "this workspace's engine runs in a remote sandbox; there is no host worktree",
            ));
        }
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
        if workspace.is_remote() {
            return Err(ServerError::conflict_kind(
                "workspace_remote",
                "this workspace's engine runs in a remote sandbox; there is no host worktree",
            ));
        }
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
        if workspace.is_remote() {
            return Err(ServerError::conflict_kind(
                "workspace_remote",
                "this workspace's engine runs in a remote sandbox; there is no host worktree",
            ));
        }
        let (worktree, from, to, turn) = resolve_diff_range(&self.db, &workspace, turn_id)
            .await
            .map_err(map_checkpoint)?;
        let produced = produce_diff(&worktree, &from, &to, file, DiffBounds::default())
            .await
            .map_err(map_checkpoint)?;
        Ok((produced.diff, produced.truncated, produced.stat, turn))
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
            let decision_gate = handle
                .as_ref()
                .map(|handle| handle.approval_decisions.clone());
            let _decision_guard = match decision_gate {
                Some(gate) => Some(gate.lock_owned().await),
                None => None,
            };
            self.revoke_browser_session(&session);
            // Mark the row ended before asking the worker to stop. A worker
            // interrupted mid-turn re-reads the row on its way round the loop
            // and leaves on its own when it finds the session ended, so one
            // `Shutdown` is enough however busy it was.
            session.lifecycle = CodeSessionLifecycle::Ended;
            session.child_pid = None;
            session.child_process_identity = None;
            session.fence_reason = None;
            super::attention::persist_session(&self.db, &self.bus, &session).await?;
            let stopped = match handle {
                Some(handle) => Self::shut_down_worker(session.id, handle).await,
                None => true,
            };
            all_stopped &= stopped;
            // The outgoing worker still holds this epoch, so a persist during
            // the wait can overwrite Ended. Re-assert from a fresh load.
            let mut current = self.get_session(owner, session.id).await?;
            current.lifecycle = CodeSessionLifecycle::Ended;
            current.child_pid = None;
            current.child_process_identity = None;
            current.fence_reason = None;
            if !super::attention::persist_session(&self.db, &self.bus, &current).await? {
                return Err(ServerError::conflict_kind(
                    "session_not_ended",
                    "the session did not stay ended after the worker stopped",
                ));
            }
            if stopped {
                super::approval_sweep::abandon_for_restart(
                    &self.db,
                    &self.bus,
                    owner,
                    current.id,
                    current.spawn_epoch,
                )
                .await;
            } else {
                super::approval_sweep::abandon_for_ended_session(
                    &self.db,
                    &self.bus,
                    owner,
                    current.id,
                    current.spawn_epoch,
                )
                .await;
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
        let mut session = session;
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
        if session.reasoning_effort.is_some() || session.fast_mode {
            let selected = self
                .selected_model_capabilities_for_owner(
                    &session.owner,
                    adapter.as_ref(),
                    &probe,
                    session.model.as_deref(),
                )
                .await;
            let mut next = CodeSessionExecutionSettings::from(&session);
            selected.deactivate_unsupported(&mut next);
            if next != CodeSessionExecutionSettings::from(&session) {
                session =
                    replace_session_execution_settings(&self.db, &session.owner, &session, &next)
                        .await?
                        .ok_or_else(|| {
                            ServerError::conflict_kind(
                                "session_settings_changed",
                                "the session settings changed before its worker could attach",
                            )
                        })?;
            }
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
            session.harness_kind,
            self.harness_llm.is_some() && super::harness_llm::relay_covered(session.harness_kind),
            None,
            attached.subagents.clone(),
            self.gh_search_path_owned(),
            self.recap_hook(),
            self.rewrite_hook(),
            self.hot_pull_requests(),
        );
        let approval = self.approval_channel(
            &attached.owner,
            attached.id,
            attached.spawn_epoch,
            session.permission_mode,
        );

        // Mint a browser channel only when both halves are present: the
        // native BrowserRuntime (the desktop adapter) and the trusted
        // bridge executable (the CLI sidecar). If either is absent, browser
        // stays None — no browser tools are advertised or injected, and the
        // session works exactly as before the browser channel existed.
        let browser = match (
            self.browser_runtime.as_ref(),
            self.browser_bridge_command.as_ref(),
        ) {
            (Some(runtime), Some(bridge)) => {
                let browser_subject = BrowserSubject {
                    owner: session.owner.clone(),
                    workspace: session.workspace_id,
                    session: session.id,
                };
                Some(
                    self.browser_tokens
                        .issue_with_semantic_actions(
                            browser_subject,
                            bridge,
                            runtime.supports_semantic_actions(),
                        )
                        .map_err(ServerError::internal)?,
                )
            }
            _ => None,
        };

        let private_root =
            super::scratch::workspace_root(&self.data_dir, workspace.id).map_err(|err| {
                ServerError::internal(format!("could not open private storage: {err}"))
            })?;

        // On a gateway-authenticated machine, point the engine's own
        // inference at this server's relay (decision 71): a per-session key
        // stands in for provider credentials the hosted image does not have.
        let (extra_argv, extra_env, relay_key_env) = match self.harness_llm.as_ref() {
            Some(relay) => {
                let base = self
                    .loopback_base
                    .lock()
                    .expect("loopback base")
                    .clone()
                    .ok_or_else(|| {
                        ServerError::internal("harness LLM relay: loopback base not set")
                    })?;
                let key = relay.issue(super::harness_llm::HarnessLlmSubject {
                    owner: session.owner.clone(),
                    session: session.id,
                });
                let (argv, env) =
                    super::harness_llm::spawn_wiring(session.harness_kind, &base, &key);
                (
                    argv,
                    env,
                    Some(super::harness_llm::RELAY_KEY_ENV.to_owned()),
                )
            }
            None => (Vec::new(), Vec::new(), None),
        };

        let spec = SessionSpec {
            worktree: PathBuf::from(&workspace.worktree_path),
            allowed_read_roots: vec![private_root.path().to_path_buf()],
            permission_mode: session.permission_mode,
            model: session.model.clone(),
            reasoning_effort: session.reasoning_effort,
            fast_mode: session.fast_mode,
            resume_ref: session.harness_resume_ref.clone(),
            extra_argv,
            extra_env,
            relay_key_env,
            env: probe.env.clone(),
            approval,
            binary: Some(binary),
            sink: sink.clone() as Arc<dyn HarnessEventSink>,
            browser,
        };
        let mut attached = attached;
        let engine = match adapter.launch(spec).await {
            Ok(engine) => engine,
            Err(HarnessError::ResumeLost(detail)) => {
                self.revoke_worker_channels(session.id);
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
                self.revoke_worker_channels(session.id);
                return Err(ServerError::internal(format!(
                    "failed to launch engine session: {err}"
                )));
            }
        };
        attached.child_pid = engine.child_pid();
        attached.child_process_identity = attached.child_pid.and_then(|pid| {
            tidebreak_harness::spawned_process_identity(pid).or_else(|| {
                tidebreak_harness::current_process_identity(pid)
                    .ok()
                    .flatten()
            })
        });
        if let Some(resume) = engine.resume_ref().or(session.harness_resume_ref.clone()) {
            attached.harness_resume_ref = Some(resume);
        }
        super::attention::persist_session(&self.db, &self.bus, &attached).await?;
        let handle = spawn_session_worker(
            attached.clone(),
            engine,
            sink,
            AttachmentStore {
                blobs: Some(self.blobs.clone()),
                private_root,
                // Only an engine that states image input takes the bytes on
                // its own protocol. The rest receive absolute private paths.
                engine_reads_images: adapter.capabilities(&probe).image_input
                    == CapLevel::Supported,
            },
            self.worktree_turn_lock(attached.workspace_id),
            self.update_quiesce.subscribe(),
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

    pub(crate) fn workspace_write_lock(
        &self,
        workspace_id: WorkspaceId,
    ) -> Arc<tokio::sync::Mutex<()>> {
        self.workspace_lifecycle_lock(workspace_id)
    }

    fn workspace_lifecycle_lock(&self, workspace_id: WorkspaceId) -> Arc<tokio::sync::Mutex<()>> {
        self.workspace_lifecycles
            .lock()
            .expect("workspace lifecycle locks")
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
                approval_decisions: handle.approval_decisions.clone(),
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
        owner: &OwnerId,
        session_id: CodeSessionId,
        spawn_epoch: i64,
        mode: PermissionMode,
    ) -> Option<ApprovalChannelSpec> {
        self.approvals.revoke_session(session_id);
        if !matches!(mode, PermissionMode::Ask | PermissionMode::Auto) {
            return None;
        }
        let base = self.loopback_base.lock().expect("loopback base").clone()?;
        let token = self.approvals.issue_token(owner, session_id, spawn_epoch);
        Some(ApprovalChannelSpec {
            mcp_endpoint_url: format!("{base}/code/mcp/approval-prompt"),
            token,
            completer: self.approvals.clone(),
        })
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

    pub(crate) async fn record_external_approval(
        &self,
        session_id: CodeSessionId,
        approval_id: CodeApprovalId,
        approval: &HarnessApprovalRef,
        raw: &serde_json::Value,
    ) -> Result<CodeApproval, ServerError> {
        let capability = approval.capability.as_ref().ok_or_else(|| {
            ServerError::internal("external approval is missing its server capability")
        })?;
        let handle = self.require_worker(session_id)?;
        if handle.spawn_epoch != capability.spawn_epoch {
            return Err(ServerError::conflict_kind(
                "approval_worker_replaced",
                "the worker that requested this approval is no longer attached",
            ));
        }
        handle
            .sink
            .record_external_approval(approval_id, approval, raw)
            .await
            .map_err(map_worker)
    }

    pub(crate) async fn abandon_external_approval(
        &self,
        session_id: CodeSessionId,
        approval_id: CodeApprovalId,
    ) -> Result<(), ServerError> {
        let session = tidebreak_core::db::code::get_session_all_owners(&self.db, session_id)
            .await?
            .ok_or_else(|| ServerError::not_found(format!("session {session_id} not found")))?;
        let Some(approval) = get_approval(&self.db, &session.owner, approval_id).await? else {
            return Ok(());
        };
        if approval.session_id != session_id {
            return Err(ServerError::internal(format!(
                "approval {approval_id} belongs to a different session"
            )));
        }
        let Some(worker_epoch) = approval.worker_epoch else {
            return Err(ServerError::internal(format!(
                "approval {approval_id} has no worker epoch"
            )));
        };
        if let Some(settlement) = abandon_pending_approval(
            &self.db,
            &session.owner,
            approval_id,
            session_id,
            worker_epoch,
            Utc::now(),
        )
        .await?
        {
            self.bus.publish(session_id, settlement.event);
            self.refresh_approval_attention(&session.owner, session_id)
                .await;
        }
        Ok(())
    }

    async fn refresh_approval_attention(&self, owner: &OwnerId, session_id: CodeSessionId) {
        let Ok(Some(session)) = get_session(&self.db, owner, session_id).await else {
            return;
        };
        let Ok(next) = super::attention::compute_attention(
            &self.db,
            &self.bus,
            &session,
            super::attention::ComputeOpts::default(),
        )
        .await
        else {
            return;
        };
        let _ =
            super::attention::apply_attention(&self.db, &self.bus, owner, session_id, next, false)
                .await;
    }

    fn native_approval_ref(
        owner: &OwnerId,
        approval: &CodeApproval,
    ) -> Result<HarnessApprovalRef, ServerError> {
        let call_id = approval.native_call_id.clone().ok_or_else(|| {
            ServerError::internal(format!("approval {} has no native call ID", approval.id))
        })?;
        if approval
            .harness_raw
            .get("call_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|stored| stored != call_id)
        {
            return Err(ServerError::internal(format!(
                "approval {} has conflicting native call IDs",
                approval.id
            )));
        }
        let worker_epoch = approval.worker_epoch.ok_or_else(|| {
            ServerError::internal(format!("approval {} has no worker epoch", approval.id))
        })?;
        let (Some(token), Some(request_sha256)) = (
            approval.server_capability.clone(),
            approval.request_sha256.clone(),
        ) else {
            if approval.server_capability.is_none() && approval.request_sha256.is_none() {
                return Ok(HarnessApprovalRef::engine(call_id));
            }
            return Err(ServerError::internal(format!(
                "approval {} has an incomplete server capability",
                approval.id
            )));
        };
        Ok(HarnessApprovalRef {
            call_id,
            capability: Some(HarnessApprovalCapability {
                token,
                owner_id: owner.to_string(),
                approval_id: approval.id.to_string(),
                session_id: approval.session_id.to_string(),
                turn_id: approval.turn_id.to_string(),
                spawn_epoch: worker_epoch,
                request_sha256,
            }),
        })
    }

    async fn abandon_claim_after_delivery_failure(
        &self,
        owner: &OwnerId,
        session_id: CodeSessionId,
        approval_id: CodeApprovalId,
        worker_epoch: i64,
        claim: uuid::Uuid,
    ) -> Result<(), ServerError> {
        if let Some(settlement) = settle_approval_claim(
            &self.db,
            owner,
            ClaimedApprovalSettlement {
                approval_id,
                session_id,
                worker_epoch,
                claim,
                decision: ApprovalDecisionKind::Abandoned,
                decided_at: Utc::now(),
            },
        )
        .await?
        {
            self.bus.publish(session_id, settlement.event);
            self.refresh_approval_attention(owner, session_id).await;
        }
        Ok(())
    }

    pub(crate) async fn decide_approval(
        &self,
        owner: &OwnerId,
        id: CodeApprovalId,
        request: ApprovalDecisionRequest,
    ) -> Result<CodeApproval, ServerError> {
        let initial = self.get_approval(owner, id).await?;
        if !initial.state.is_pending() {
            return Err(ServerError::conflict_kind(
                "approval_not_pending",
                format!(
                    "approval {id} is no longer awaiting a decision: it is {}",
                    initial.state.as_str()
                ),
            ));
        }
        if initial.decision_claim.is_some() {
            return Err(ServerError::conflict_kind(
                "approval_decision_in_progress",
                format!("approval {id} already has a decision in progress"),
            ));
        }
        let handle = self.require_worker(initial.session_id)?;
        let _decision_guard = handle.approval_decisions.clone().lock_owned().await;
        // Shutdown can remove the handle before this task acquires the gate.
        // Re-read every durable precondition after the gate is ours.
        let approval = self.get_approval(owner, id).await?;
        if !approval.state.is_pending() {
            return Err(ServerError::conflict_kind(
                "approval_not_pending",
                format!(
                    "approval {id} is no longer awaiting a decision: it is {}",
                    approval.state.as_str()
                ),
            ));
        }
        if approval.decision_claim.is_some() {
            return Err(ServerError::conflict_kind(
                "approval_decision_in_progress",
                format!("approval {id} already has a decision in progress"),
            ));
        }
        let worker_epoch = approval
            .worker_epoch
            .ok_or_else(|| ServerError::internal(format!("approval {id} has no worker epoch")))?;
        let session = self.get_session(owner, approval.session_id).await?;
        if session.lifecycle != CodeSessionLifecycle::Running {
            return Err(ServerError::conflict_kind(
                "approval_worker_inactive",
                format!(
                    "approval {id} cannot be decided while session {} is {}",
                    session.id,
                    session.lifecycle.as_str()
                ),
            ));
        }
        if session.spawn_epoch != worker_epoch || handle.spawn_epoch != worker_epoch {
            return Err(ServerError::conflict_kind(
                "approval_worker_replaced",
                "the worker that requested this approval is no longer attached",
            ));
        }
        let adapter = self.adapter(session.harness_kind)?;
        let probe = self.probe(adapter.as_ref()).await;
        let decision = resolve_decision_request(&approval, &adapter.capabilities(&probe), request)?;
        let native_ref = Self::native_approval_ref(owner, &approval)?;
        let claim = uuid::Uuid::new_v4();
        let Some(_) = claim_approval(
            &self.db,
            owner,
            id,
            approval.session_id,
            worker_epoch,
            claim,
            Utc::now(),
        )
        .await?
        else {
            let current = self.get_approval(owner, id).await?;
            let kind = if current.state.is_pending() && current.decision_claim.is_some() {
                "approval_decision_in_progress"
            } else {
                "approval_not_pending"
            };
            return Err(ServerError::conflict_kind(
                kind,
                format!("approval {id} no longer accepts this decision"),
            ));
        };
        let (reply, rx) = oneshot::channel();
        if handle
            .commands
            .send(crate::code::session_worker::WorkerCommand::Decide {
                approval: native_ref,
                decision: Box::new(decision.clone()),
                reply,
            })
            .await
            .is_err()
        {
            self.abandon_claim_after_delivery_failure(
                owner,
                approval.session_id,
                id,
                worker_epoch,
                claim,
            )
            .await?;
            return Err(ServerError::conflict_kind(
                "approval_delivery_failed",
                "the session worker stopped before it received the decision",
            ));
        }
        let native_result = match rx.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(WorkerError::ApprovalDeliveryUnknown(message))) => {
                return Err(ServerError::conflict_kind(
                    "approval_delivery_unknown",
                    message,
                ));
            }
            Ok(Err(error)) => Err(map_worker(error)),
            Err(_) => {
                return Err(ServerError::conflict_kind(
                    "approval_delivery_unknown",
                    "the session worker stopped before it acknowledged the decision; the approval stays claimed until recovery",
                ));
            }
        };
        if let Err(error) = native_result {
            self.abandon_claim_after_delivery_failure(
                owner,
                approval.session_id,
                id,
                worker_epoch,
                claim,
            )
            .await?;
            return Err(error);
        }
        let event_decision = tidebreak_core::ApprovalDecisionKind::from(decision.clone());
        let Some(settlement) = settle_approval_claim(
            &self.db,
            owner,
            ClaimedApprovalSettlement {
                approval_id: id,
                session_id: approval.session_id,
                worker_epoch,
                claim,
                decision: event_decision,
                decided_at: Utc::now(),
            },
        )
        .await?
        else {
            return Err(ServerError::internal(format!(
                "approval {id} lost its durable decision claim after native acknowledgement"
            )));
        };
        self.bus.publish(approval.session_id, settlement.event);
        self.refresh_approval_attention(owner, approval.session_id)
            .await;
        Ok(settlement.approval)
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

fn offered_permission_modes(caps: &tidebreak_core::HarnessCaps) -> Vec<PermissionMode> {
    PermissionMode::ALL
        .iter()
        .copied()
        .filter(|&mode| honors_permission_mode(mode, caps))
        .collect()
}

fn honors_permission_mode(mode: PermissionMode, caps: &tidebreak_core::HarnessCaps) -> bool {
    // Each mode stands on its own capability flag (decision 0038): Auto is
    // never derived from the approval channel, so an engine whose only
    // honest posture is unsupervised Auto can still be driven.
    match mode {
        PermissionMode::Plan => caps.plan_mode == CapLevel::Supported,
        PermissionMode::Ask => caps.structured_approvals == CapLevel::Supported,
        PermissionMode::Auto => caps.auto_mode == CapLevel::Supported,
        PermissionMode::Allow => caps.allow_mode == CapLevel::Supported,
    }
}

fn refuse_ceiling_with_no_offered_mode(
    ceiling: Option<PermissionMode>,
    harness: HarnessKind,
    caps: &tidebreak_core::HarnessCaps,
) -> Result<(), ServerError> {
    let Some(ceiling) = ceiling else {
        return Ok(());
    };
    if !ManagedPolicy::permission_mode_ceiling_excludes_all(
        Some(ceiling),
        offered_permission_modes(caps),
    ) {
        return Ok(());
    }
    Err(ServerError::conflict_kind(
        "permission_mode_locked",
        format!(
            "{harness} offers no permission mode at or below the managed ceiling (`{}`)",
            ceiling.as_str()
        ),
    ))
}

fn refuse_unhonored_mode(
    harness: HarnessKind,
    mode: PermissionMode,
    caps: &tidebreak_core::HarnessCaps,
) -> Result<(), ServerError> {
    if honors_permission_mode(mode, caps) {
        return Ok(());
    }
    let reason = match mode {
        PermissionMode::Plan => format!("{harness} cannot honor plan mode"),
        PermissionMode::Ask => format!(
            "{harness} cannot honor {mode}: structured approvals are {}",
            caps.structured_approvals.as_str()
        ),
        PermissionMode::Auto => format!(
            "{harness} cannot honor {mode}: an auto posture is {}",
            caps.auto_mode.as_str()
        ),
        PermissionMode::Allow => format!(
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
            if let Some(message) = message.strip_prefix(gh::PR_HEAD_CHANGED_PREFIX) {
                ServerError::conflict_kind("pr_head_changed", message)
            } else if let Some(message) = message.strip_prefix(gh::GH_UNAVAILABLE_PREFIX) {
                ServerError::conflict_kind("gh_unavailable", message)
            } else if message.contains("no quick action") {
                ServerError::not_found(message)
            } else {
                ServerError::bad_request_kind("git", message)
            }
        }
        GhError::Internal(message) => ServerError::internal(message),
    }
}

fn validate_workspace_merge_request(
    target: &CodeDeliveryPullRequestTarget,
    expected_head_sha: &str,
) -> Result<(), ServerError> {
    if target.number == 0
        || target.repository.host.trim().is_empty()
        || target.repository.owner.trim().is_empty()
        || target.repository.name.trim().is_empty()
        || expected_head_sha.trim().is_empty()
    {
        return Err(ServerError::bad_request_kind(
            "workspace_merge_target",
            "repository, pull request number, and expected head commit are required",
        ));
    }
    Ok(())
}

fn same_repository(left: &CodeGitHubRepositoryTarget, right: &CodeGitHubRepositoryTarget) -> bool {
    left.host.eq_ignore_ascii_case(&right.host)
        && left.owner.eq_ignore_ascii_case(&right.owner)
        && left.name.eq_ignore_ascii_case(&right.name)
}

fn repository_label(target: &CodeGitHubRepositoryTarget) -> String {
    if target.host.eq_ignore_ascii_case("github.com") {
        format!("{}/{}", target.owner, target.name)
    } else {
        format!("{}/{}/{}", target.host, target.owner, target.name)
    }
}

fn pr_head_changed(expected: &str, current: &str) -> ServerError {
    ServerError::conflict_kind(
        "pr_head_changed",
        format!(
            "pull request head changed from {} to {}; refresh before merging",
            short_sha(expected),
            short_sha(current)
        ),
    )
}

fn short_sha(sha: &str) -> &str {
    sha.get(..sha.len().min(8)).unwrap_or(sha)
}

/// The origin a hosted machine may lend the forge's App identity to: a
/// parseable forge repository on the forge's own host, and nothing else
/// (decision 63).
///
/// The host gate is a security boundary, not a convenience. The origin URL
/// is workspace state an agent can rewrite, and the parser accepts any
/// `host/owner/repo` shape — without the gate, the next push would mint a
/// live installation token and offer it to whatever host `origin` names.
/// Only `owner/name` ever travels to the gateway, and the one-shot helper
/// re-checks the same host at `get`, so both halves refuse independently.
async fn forge_lending_target(
    worktree: &std::path::Path,
) -> Option<crate::routes::code::types::CodeGitHubRepositoryTarget> {
    let target = super::delivery::repository_target_from_path(worktree)
        .await
        .ok()?;
    target
        .host
        .eq_ignore_ascii_case(gh::GIT_CREDENTIAL_FORGE_HOST)
        .then_some(target)
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
        WorktreeError::ArchiveUncertain(message) => {
            ServerError::conflict_kind("archive_inspection_uncertain", message)
        }
        WorktreeError::Conflict { kind, message } => ServerError::conflict_kind(kind, message),
    }
}

fn archive_failure_can_reopen(error: &ServerError) -> bool {
    matches!(
        error.kind(),
        "archive_script_failed"
            | "archive_inspection_uncertain"
            | "ignored_content"
            | "uncommitted"
            | "unpushed"
            | "uncommitted_and_unpushed"
    )
}

fn permission_mode_fence_reason(intent: &PermissionModeChangeIntent) -> FenceReason {
    FenceReason::ProbeAmbiguous {
        detail: format!(
            "permission mode change from {} to {} reached the engine but revision {} did not commit",
            intent.previous_mode, intent.requested_mode, intent.revision
        ),
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
        WorkerError::ApprovalDeliveryFailed(message) => {
            ServerError::conflict_kind("approval_delivery_failed", message)
        }
        WorkerError::ApprovalDeliveryUnknown(message) => {
            ServerError::conflict_kind("approval_delivery_unknown", message)
        }
        // `set_permission_mode` intercepts this and relaunches, so reaching
        // here means a caller asked the engine to re-posture without a
        // fallback. Say what the caller has to do rather than 500.
        WorkerError::RelaunchRequired(message) => {
            ServerError::conflict_kind("relaunch_required", message)
        }
        WorkerError::Failed(message) => ServerError::internal(message),
        // Only the drain loop drives promoted queue rows, and it re-reads on
        // staleness and pauses on a stop. A caller seeing either took a path
        // that does not exist.
        WorkerError::QueuedTurnStale => {
            ServerError::internal("the queued turn changed before it could start".to_owned())
        }
        WorkerError::QueuedTurnStopped => {
            ServerError::internal("the queued turn was stopped before it could start".to_owned())
        }
        WorkerError::TriggerDeliveryAccepted => ServerError::conflict_kind(
            "trigger_delivery_accepted",
            "trigger delivery was already accepted",
        ),
        // `submit_turn` intercepts this and parks the message instead, so
        // reaching here means a caller took the turn path without handling
        // contention. Answer as a conflict rather than a 500.
        WorkerError::WorktreeBusy => ServerError::conflict_kind(
            "workspace_busy",
            "another session in this workspace is mid-turn",
        ),
        // Also intercepted and parked by `submit_turn`. A caller that still
        // sees it raced the restart itself; a conflict reads better than a
        // 500 from a process that is about to relaunch.
        WorkerError::UpdateQuiesced => ServerError::conflict_kind(
            "update_quiesced",
            "Tidebreak is restarting for an update; the turn starts after the relaunch",
        ),
    }
}

impl From<WorktreeError> for ServerError {
    fn from(err: WorktreeError) -> Self {
        map_worktree(err)
    }
}

#[cfg(test)]
mod delivery_nudge_tests {
    use super::super::bus::CodeLiveUpdate;
    use super::*;
    use tokio::sync::broadcast::error::TryRecvError;

    #[tokio::test(start_paused = true)]
    async fn suppressed_changes_share_one_trailing_nudge_per_owner() {
        let nudges = DeliveryNudgeDebounce::default();
        let bus = Arc::new(CodeEventBus::default());
        let alice = OwnerId::new("alice").unwrap();
        let bob = OwnerId::new("bob").unwrap();
        let mut alice_updates = bus.subscribe_updates(&alice);
        let mut bob_updates = bus.subscribe_updates(&bob);

        nudges.publish(&bus, &alice);
        assert_eq!(alice_updates.try_recv().unwrap(), CodeLiveUpdate::Delivery);

        nudges.publish(&bus, &alice);
        nudges.publish(&bus, &alice);
        assert!(matches!(alice_updates.try_recv(), Err(TryRecvError::Empty)));

        nudges.publish(&bus, &bob);
        assert_eq!(bob_updates.try_recv().unwrap(), CodeLiveUpdate::Delivery);

        tokio::time::advance(DELIVERY_NUDGE_DEBOUNCE).await;
        tokio::task::yield_now().await;
        assert_eq!(alice_updates.try_recv().unwrap(), CodeLiveUpdate::Delivery);
        assert!(matches!(alice_updates.try_recv(), Err(TryRecvError::Empty)));
        assert!(matches!(bob_updates.try_recv(), Err(TryRecvError::Empty)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_pending_trailing_nudge_stops_with_its_runtime_state() {
        let bus = Arc::new(CodeEventBus::default());
        let owner = OwnerId::new("alice").unwrap();
        let mut updates = bus.subscribe_updates(&owner);
        let nudges = DeliveryNudgeDebounce::default();

        nudges.publish(&bus, &owner);
        assert_eq!(updates.try_recv().unwrap(), CodeLiveUpdate::Delivery);
        nudges.publish(&bus, &owner);
        drop(nudges);

        tokio::time::advance(DELIVERY_NUDGE_DEBOUNCE).await;
        tokio::task::yield_now().await;
        assert!(matches!(updates.try_recv(), Err(TryRecvError::Empty)));
    }
}

#[cfg(test)]
mod selected_model_capabilities_tests {
    use super::*;
    use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
    use tidebreak_harness::ListedHarnessModel;

    #[derive(Default)]
    struct NoSecrets;

    #[async_trait::async_trait]
    impl tidebreak_core::SecretProvider for NoSecrets {
        async fn get_secret(&self, _key: &str) -> tidebreak_core::Result<Option<String>> {
            Ok(None)
        }

        async fn set_secret(&self, _key: &str, _value: &str) -> tidebreak_core::Result<()> {
            Ok(())
        }

        async fn delete_secret(&self, _key: &str) -> tidebreak_core::Result<()> {
            Ok(())
        }
    }

    async fn runtime_with_gateway_snapshot(
        gateway_url: &str,
        policy_url: &str,
    ) -> (CodeRuntime, tempfile::TempDir) {
        let directory = tempfile::tempdir().expect("data dir");
        let db = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("code.db").display()
            ))
            .await
            .expect("db"),
        );
        crate::providers::write_gateway_snapshot(
            &*db,
            &crate::providers::GatewayModelSnapshot {
                gateway_url: gateway_url.into(),
                installation_id: None,
                models: Vec::new(),
                model_protocols: Default::default(),
                model_reasoning_efforts: std::collections::BTreeMap::from([(
                    "glm-5.3".into(),
                    vec![
                        ReasoningEffort::Low,
                        ReasoningEffort::High,
                        ReasoningEffort::Max,
                    ],
                )]),
                member_catalog: Some("v1".into()),
                catalog_etag: None,
            },
        )
        .await
        .expect("gateway snapshot");
        let provisioned = crate::managed_policy::MemoryProvisionedPolicy::new();
        crate::managed_policy::provision(&*provisioned, policy_url).expect("managed policy");
        let gateway = crate::gateway_runtime::GatewayRuntime::new(
            db.clone(),
            Arc::new(NoSecrets),
            provisioned,
            Arc::new(crate::managed_policy::NoOsPolicy),
        );
        let runtime =
            CodeRuntime::with_registry(db, directory.path().to_path_buf(), AdapterRegistry::new())
                .with_gateway_runtime(gateway);
        (runtime, directory)
    }

    fn scripted_probe() -> HarnessProbe {
        HarnessProbe {
            found: true,
            binary_path: Some(PathBuf::from("/scripted")),
            version: Some("1.0.0".into()),
            authenticated: Some(true),
            stderr: String::new(),
            env: Vec::new(),
            commands: Vec::new(),
        }
    }

    #[tokio::test]
    async fn an_unavailable_catalog_does_not_clear_committed_settings() {
        let adapter =
            ScriptedAdapter::new(plain_text_script()).with_reasoning_levels(CapLevel::Supported);
        let probe = HarnessProbe {
            found: true,
            binary_path: Some(PathBuf::from("/scripted")),
            version: Some("1.0.0".into()),
            authenticated: Some(true),
            stderr: String::new(),
            env: Vec::new(),
            commands: Vec::new(),
        };
        let capabilities =
            CodeRuntime::selected_model_capabilities(&adapter, &probe, Some("configured")).await;
        let mut settings = CodeSessionExecutionSettings {
            model: Some("configured".into()),
            reasoning_effort: Some(ReasoningEffort::High),
            fast_mode: true,
        };

        capabilities.deactivate_unsupported(&mut settings);

        assert_eq!(settings.reasoning_effort, Some(ReasoningEffort::High));
        assert!(settings.fast_mode);
    }

    #[tokio::test]
    async fn a_gateway_model_uses_the_model_and_engine_effort_intersection() {
        let (runtime, _directory) =
            runtime_with_gateway_snapshot("https://gateway.example/", "https://gateway.example/")
                .await;
        let adapter =
            ScriptedAdapter::new(plain_text_script()).with_reasoning_levels(CapLevel::Supported);

        let capabilities = runtime
            .selected_model_capabilities_for_owner(
                &OwnerId::new("alice").unwrap(),
                &adapter,
                &scripted_probe(),
                Some("model-gateway-model-gateway/glm-5.3"),
            )
            .await;

        assert_eq!(
            capabilities.reasoning_efforts,
            vec![
                ReasoningEffort::Low,
                ReasoningEffort::High,
                ReasoningEffort::Max,
            ]
        );
        assert!(capabilities.reasoning_known);
    }

    #[tokio::test]
    async fn a_codex_rows_ladder_wins_over_the_engine_wide_ladder() {
        let (runtime, _directory) =
            runtime_with_gateway_snapshot("https://gateway.example/", "https://gateway.example/")
                .await;
        let adapter = ScriptedAdapter::new(plain_text_script())
            .with_kind(HarnessKind::Codex)
            .with_reasoning_levels(CapLevel::Supported)
            .with_models(vec![ListedHarnessModel {
                id: "model-gateway-model-gateway/glm-5.3".into(),
                label: "GLM 5.3".into(),
                default: true,
                reasoning_efforts: vec![
                    ReasoningEffort::Low,
                    ReasoningEffort::High,
                    ReasoningEffort::Ultra,
                ],
                fast_mode: false,
            }]);

        let capabilities = runtime
            .selected_model_capabilities_for_owner(
                &OwnerId::new("alice").unwrap(),
                &adapter,
                &scripted_probe(),
                Some("model-gateway-model-gateway/glm-5.3"),
            )
            .await;

        assert_eq!(
            capabilities.reasoning_efforts,
            vec![ReasoningEffort::Low, ReasoningEffort::High]
        );
    }

    #[tokio::test]
    async fn a_snapshot_from_another_gateway_does_not_change_engine_efforts() {
        let (runtime, _directory) = runtime_with_gateway_snapshot(
            "https://old.gateway.example/",
            "https://gateway.example/",
        )
        .await;
        let adapter =
            ScriptedAdapter::new(plain_text_script()).with_reasoning_levels(CapLevel::Supported);

        let capabilities = runtime
            .selected_model_capabilities_for_owner(
                &OwnerId::new("alice").unwrap(),
                &adapter,
                &scripted_probe(),
                Some("model-gateway-model-gateway/glm-5.3"),
            )
            .await;

        assert!(capabilities.reasoning_efforts.is_empty());
        assert!(!capabilities.reasoning_known);
    }

    #[test]
    fn an_authoritative_catalog_clears_unsupported_settings() {
        let capabilities = SelectedModelCapabilities {
            reasoning_efforts: vec![ReasoningEffort::Low],
            listed_model_reasoning_efforts: Some(vec![ReasoningEffort::Low]),
            reasoning_known: true,
            fast_mode: false,
            fast_mode_known: true,
        };
        let mut settings = CodeSessionExecutionSettings {
            model: Some("steady".into()),
            reasoning_effort: Some(ReasoningEffort::High),
            fast_mode: true,
        };

        capabilities.deactivate_unsupported(&mut settings);

        assert_eq!(settings.reasoning_effort, None);
        assert!(!settings.fast_mode);
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
mod signed_out_refusal_tests {
    use super::*;

    fn probe(
        authenticated: Option<bool>,
        env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> HarnessProbe {
        HarnessProbe {
            found: true,
            binary_path: Some(PathBuf::from("/scripted/engine")),
            version: Some("1.0.0".into()),
            authenticated,
            stderr: String::new(),
            env,
            commands: Vec::new(),
        }
    }

    #[test]
    fn a_definitive_signed_out_observation_refuses_with_the_typed_kind() {
        let err = CodeRuntime::signed_out_harness_refusal(
            false,
            HarnessKind::Codex,
            &probe(Some(false), Vec::new()),
        )
        .unwrap_err();
        assert_eq!(err.kind(), "harness_not_authenticated");
        assert!(
            err.message()
                .contains("Codex CLI is not signed in on this machine."),
            "{}",
            err.message()
        );
    }

    #[test]
    fn an_unverified_or_signed_in_observation_allows_create() {
        for authenticated in [None, Some(true)] {
            assert!(CodeRuntime::signed_out_harness_refusal(
                false,
                HarnessKind::ClaudeCode,
                &probe(authenticated, Vec::new()),
            )
            .is_ok());
        }
    }

    #[test]
    fn the_relay_carries_covered_engines_past_a_signed_out_probe() {
        // The #2742 guarantee: a hosted machine with the relay creates freely.
        assert!(CodeRuntime::signed_out_harness_refusal(
            true,
            HarnessKind::Codex,
            &probe(Some(false), Vec::new()),
        )
        .is_ok());
    }

    #[test]
    fn a_credential_override_in_the_environment_allows_create() {
        // A gateway-managed machine authenticates with no vendor login
        // (issue 2749); its overrides must beat the signed-out observation.
        let env = vec![(
            std::ffi::OsString::from("OPENAI_BASE_URL"),
            std::ffi::OsString::from("https://gateway.example/v1"),
        )];
        assert!(CodeRuntime::signed_out_harness_refusal(
            false,
            HarnessKind::Codex,
            &probe(Some(false), env),
        )
        .is_ok());
    }

    #[test]
    fn engines_without_a_verified_override_surface_never_refuse() {
        // opencode honors provider keys and config shapes the probe cannot
        // cheaply rule out, so a signed-out observation alone must not block.
        assert!(CodeRuntime::signed_out_harness_refusal(
            false,
            HarnessKind::Opencode,
            &probe(Some(false), Vec::new()),
        )
        .is_ok());
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
        use tidebreak_managed_node::{
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

/// One decision as the wire requested it, before the server resolves it
/// against the approval row and the engine's declared capabilities.
///
/// A grant is named by index into the rungs the approval's kind offered, so
/// a client can only pick a scope the card showed (the same rule chat's
/// grant rungs follow); the server resolves the concrete scope here.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ApprovalDecisionRequest {
    Approve,
    Deny {
        feedback: Option<String>,
    },
    ApproveWithGrant {
        grant_index: u32,
    },
    Answers {
        answers: Vec<tidebreak_core::UserQuestionAnswer>,
    },
    PlanDecision {
        approve: bool,
        feedback: Option<String>,
    },
}

/// Resolve a requested decision into the engine-channel decision, refusing
/// anything the approval's kind or the engine's capability vector cannot
/// carry. One approval surface, capability-gated decisions (decision 0048).
fn resolve_decision_request(
    approval: &CodeApproval,
    caps: &tidebreak_core::HarnessCaps,
    request: ApprovalDecisionRequest,
) -> Result<ApprovalDecision, ServerError> {
    use tidebreak_core::CodeApprovalKind as Kind;
    let structured_mismatch = |wanted: &str| {
        ServerError::unprocessable_kind(
            "approval_decision_mismatch",
            format!("this approval takes {wanted}"),
        )
    };
    match request {
        ApprovalDecisionRequest::Approve => match &approval.kind {
            // Structured kinds have structured decisions; a bare approve on
            // them would drop the payload the engine is waiting for.
            Kind::Questions { .. } => Err(structured_mismatch("answers")),
            Kind::Plan { .. } => Err(structured_mismatch("a plan decision")),
            _ => Ok(ApprovalDecision::Approve),
        },
        // Denying is always expressible: it is the fail-closed path.
        ApprovalDecisionRequest::Deny { feedback } => Ok(ApprovalDecision::Deny { feedback }),
        ApprovalDecisionRequest::ApproveWithGrant { grant_index } => {
            if caps.standing_grants != CapLevel::Supported {
                return Err(ServerError::unprocessable_kind(
                    "standing_grants_unavailable",
                    "this engine keeps no standing grants",
                ));
            }
            let Kind::ToolUse { offered_grants, .. } = &approval.kind else {
                return Err(structured_mismatch("approve or deny"));
            };
            let scope = offered_grants
                .get(usize::try_from(grant_index).unwrap_or(usize::MAX))
                .cloned()
                .ok_or_else(|| {
                    ServerError::unprocessable_kind(
                        "grant_rung_unknown",
                        format!("this approval offered no grant rung {grant_index}"),
                    )
                })?;
            Ok(ApprovalDecision::ApproveWithGrant { scope })
        }
        ApprovalDecisionRequest::Answers { answers } => {
            if caps.user_questions != CapLevel::Supported {
                return Err(ServerError::unprocessable_kind(
                    "user_questions_unavailable",
                    "this engine takes no structured answers",
                ));
            }
            let Kind::Questions { questions } = &approval.kind else {
                return Err(structured_mismatch("approve or deny"));
            };
            let asked: std::collections::HashSet<&str> = questions
                .iter()
                .map(|question| question.id.as_str())
                .collect();
            let mut seen = std::collections::HashSet::new();
            let well_formed = answers.iter().all(|answer| {
                answer.shape_is_well_formed()
                    && asked.contains(answer.question_id.as_str())
                    && seen.insert(answer.question_id.as_str())
            });
            if !well_formed || answers.is_empty() {
                return Err(ServerError::unprocessable_kind(
                    "answers_invalid",
                    "the answers do not match the questions this approval asked",
                ));
            }
            Ok(ApprovalDecision::Answers { answers })
        }
        ApprovalDecisionRequest::PlanDecision { approve, feedback } => {
            let Kind::Plan { .. } = &approval.kind else {
                return Err(structured_mismatch("approve or deny"));
            };
            Ok(ApprovalDecision::PlanDecision { approve, feedback })
        }
    }
}
