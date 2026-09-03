//! Process-wide code-mode runtime: adapters, workers, worktrees, recovery.
//!
//! This module holds the [`CodeRuntime`] handle, its construction and
//! configuration, and the shared error mapping. Each concern lives in its
//! own file and extends the same `impl CodeRuntime`:
//!
//! - [`adapters`]: harness adapters, probes, and the managed Node runtime.
//! - [`recover`]: startup recovery of sessions and workspaces.
//! - [`repos`]: registered repositories.
//! - [`workspaces`]: workspace lifecycle, storage, and file reads.
//! - [`workspace_delivery`]: commits, pushes, pull requests, and branch rules.
//! - [`remote`]: remote and external sessions.
//! - [`sessions`]: session rows, turn history, and debugging reads.
//! - [`turns`]: turn submission, the queue, steering, and update quiesce.
//! - [`settings`]: permission mode, effort, and model capability checks.
//! - [`workers`]: per-session worker spawn and shutdown.
//! - [`sweeps`]: triggers and the background sweeps.
//! - [`approvals`]: approval records and decisions.

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
    complete_workspace_archive, complete_workspace_release, confirm_permission_mode_change,
    delete_trigger, delete_workspace, discard_permission_mode_change, fence_permission_mode_change,
    get_approval, get_open_turn, get_repo, get_repo_by_root_path, get_session, get_workspace,
    insert_repo, insert_session, insert_workspace, list_approvals, list_events, list_fork_events,
    list_pending_permission_mode_changes, list_repos, list_sessions, list_sessions_all_owners,
    list_sessions_for_workspace, list_triggers_for_repo, list_turns, list_workspaces,
    list_workspaces_by_status_all_owners, mark_repo_removed, queued_turn_head,
    replace_session_execution_settings, save_repo, save_workspace,
    set_active_workspace_pull_request, settle_approval_claim, update_trigger_enabled,
    ClaimedApprovalSettlement, PermissionModeChangeIntent, SessionExecutionSettings,
    MAX_REPLAY_EVENTS,
};
use tidebreak_core::{
    Approval, ApprovalDecisionKind, ApprovalId, ApprovalState, Attention, AttentionSource,
    CapLevel, CodeRepo, CodeTrigger, CodeTriggerAction, CodeTriggerCondition, CodeTriggerId,
    CodeWorkspace, CodeWorkspaceStatus, DbStore, Diffstat, Event, FenceReason, HarnessKind,
    OwnerId, PermissionMode, PullRequestDigest, QueuedTurn, QuickAction, ReasoningEffort, RepoId,
    Session, SessionId, SessionKind, SessionLifecycle, Turn, TurnId, WorkspaceId,
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
    self, archive_blockers, branch_name, branch_name_from_slug, create_worktree, prune_worktrees,
    remove_worktree, run_archive_script, run_setup_script, slugify, validate_repo_path,
    worktree_dir, WorktreeError,
};
use crate::error::ServerError;
use crate::managed_policy::ManagedPolicy;
use crate::routes::code::types::{CodeDeliveryPullRequestTarget, CodeGitHubRepositoryTarget};

mod adapters;
mod approvals;
mod recover;
mod remote;
mod repos;
mod sessions;
mod settings;
mod sweeps;
mod turns;
mod workers;
mod workspace_delivery;
mod workspaces;

use settings::{normalize_model, refuse_ceiling_with_no_offered_mode, refuse_unhonored_mode};

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

/// Result of `POST /sessions/{id}/turns`.
pub(crate) enum SubmitTurnOutcome {
    /// The session was idle; the turn ran to a terminal event.
    Ran(Box<Turn>),
    /// The session or its workspace was busy; the message parked as a
    /// durable queue row (decision 69).
    Queued(Box<QueuedTurn>),
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
    NewTurn(Box<Turn>),
    /// The session was busy; the message sits as a durable queue row.
    Queued(Box<QueuedTurn>),
    /// The row the first delivery caused was retracted before it could
    /// run; the replay has nothing to point at.
    Dropped,
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
    pub(in crate::code) host: HostEnv,
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
    workers: Mutex<HashMap<SessionId, WorkerHandle>>,
    /// Sessions whose worker must move to the selected engine binary once
    /// their turn in flight ends. See `resync_workers_to_selected_binaries`.
    deferred_resyncs: Mutex<HashSet<SessionId>>,
    /// Flips true while the process quiesces for a restart-to-update. Every
    /// session worker subscribes: the flag holds queue drains, refuses new
    /// turn starts at the worktree boundary, and parks idle engine children
    /// immediately. See `crate::update_quiesce`.
    update_quiesce: watch::Sender<bool>,
    /// Serializes writer admission with workspace lifecycle transitions.
    workspace_lifecycles: Mutex<HashMap<WorkspaceId, Arc<tokio::sync::Mutex<()>>>>,
    /// Serializes branch and folder allocation per repository.
    workspace_creations: Mutex<HashMap<RepoId, Arc<tokio::sync::Mutex<()>>>>,
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
    /// The registry's last answer per engine, for the doctor's update rows.
    pub(super) harness_releases: super::harness_release::KnownReleases,
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
    memory_capture: Mutex<Option<Arc<dyn super::memory_capture::TurnMemoryCapture>>>,
    #[cfg(test)]
    archive_shutdown_timeout: AtomicBool,
    #[cfg(test)]
    fail_next_workspace_release_metadata: AtomicBool,
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
            deferred_resyncs: Mutex::new(HashSet::new()),
            update_quiesce: watch::channel(false).0,
            workspace_lifecycles: Mutex::new(HashMap::new()),
            workspace_creations: Mutex::new(HashMap::new()),
            worktree_turns: Mutex::new(HashMap::new()),
            hot_prs: super::pr_refresh::HotPullRequests::default(),
            delivery_nudges: DeliveryNudgeDebounce::default(),
            host_gate: super::pr_fetch::HostGate::default(),
            branch_rules: Mutex::new(HashMap::new()),
            delivery_cache: DeliveryCache::default(),
            clone_jobs: CloneJobs::default(),
            harness_installs: HarnessInstallJobs::default(),
            harness_releases: super::harness_release::KnownReleases::default(),
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
            memory_capture: Mutex::new(None),
            #[cfg(test)]
            archive_shutdown_timeout: AtomicBool::new(false),
            #[cfg(test)]
            fail_next_workspace_release_metadata: AtomicBool::new(false),
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
            deferred_resyncs: Mutex::new(HashSet::new()),
            update_quiesce: watch::channel(false).0,
            workspace_lifecycles: Mutex::new(HashMap::new()),
            workspace_creations: Mutex::new(HashMap::new()),
            worktree_turns: Mutex::new(HashMap::new()),
            hot_prs: super::pr_refresh::HotPullRequests::default(),
            delivery_nudges: DeliveryNudgeDebounce::default(),
            host_gate: super::pr_fetch::HostGate::default(),
            branch_rules: Mutex::new(HashMap::new()),
            delivery_cache: DeliveryCache::default(),
            clone_jobs: CloneJobs::default(),
            harness_installs: HarnessInstallJobs::default(),
            harness_releases: super::harness_release::KnownReleases::default(),
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
            memory_capture: Mutex::new(None),
            #[cfg(test)]
            archive_shutdown_timeout: AtomicBool::new(false),
            #[cfg(test)]
            fail_next_workspace_release_metadata: AtomicBool::new(false),
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
    pub(crate) fn fail_next_workspace_release_metadata(&self) {
        self.fail_next_workspace_release_metadata
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

    /// Install the hook that captures memory after each completed turn.
    pub(crate) fn install_memory_capture(
        &self,
        capture: Arc<dyn super::memory_capture::TurnMemoryCapture>,
    ) {
        *self
            .memory_capture
            .lock()
            .expect("code memory capture hook") = Some(capture);
    }

    fn memory_capture_hook(&self) -> Option<Arc<dyn super::memory_capture::TurnMemoryCapture>> {
        self.memory_capture
            .lock()
            .expect("code memory capture hook")
            .clone()
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
        missing @ WorktreeError::MissingBaseRef { .. } => {
            ServerError::bad_request_kind("missing_base_ref", missing.to_string())
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
