//! Periodic destruction of a finished background run's host scratch workspace.
//!
//! Every background run gets its own workspace on disk — `scratch_root/agent-run-<id>`,
//! plus a matching env-home directory under the local exec provider — and, on a
//! remote provider, its own provider-side sandbox session. Nothing ever removed
//! either: host scratch used to be bounded by the number of conversations, but
//! it now grows with delegation volume, and each run's provider session
//! outlives the run.
//!
//! This sweeps periodically rather than hooking a run's settlement path
//! directly. A settlement hook would need to fire from every place a run
//! reaches a terminal state — `submit_agent_run_result`,
//! `submit_agent_run_submission`, `submit_agent_run_folder_access_proposal`,
//! `fail_agent_run`, `finish_agent_run_cancellation` — and would still need a
//! grace period to be safe: a run's own worker heartbeats the exec lease while
//! a command is in flight, and cancellation is a two-step handshake
//! (`Cancelling` until the running worker acknowledges quiescence), so
//! "terminal" is momentarily meetable from more than one caller. A periodic
//! sweep only ever reads durable state, checks the same terminal predicate the
//! run's own worker uses, and waits out a grace period past `finished_at`
//! before touching anything — it cannot race an attempt that is still running
//! or about to retry, and it needs no new call site wired into the run
//! lifecycle at all. It also comes with a bounded worst case: after a host
//! crash, a stuck `Running` lease is self-healed into `Failed` or a fresh
//! attempt the next time the run scheduler claims work (see
//! `Store::claim_agent_run`), and this sweep picks up the resulting terminal
//! run on its next pass — no separate orphan detector is needed for that case.
//!
//! Chat deletion is the one intentional exception to "wait for the reaper":
//! deleting a conversation already refuses while any of its runs is
//! non-terminal, and the run rows are removed in the same transaction, so the
//! deletion path destroys those workspaces immediately for privacy. This sweep
//! remains the backstop when that best-effort erase does not finish.
//!
//! A run's *published outputs* live under `scratch_root/<chat_id>/outputs/…`,
//! never under the run's own workspace — that split (and the bug it fixed) is
//! in `ConfiguredExecProvider::publish_output_directory`. Destroying
//! `scratch_root/agent-run-<id>` therefore never touches a published output;
//! the two live under different directory names by construction.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use tidebreak_code_execution::{ExecutionWorkspaceId, WorkspaceLifecycle};
use tidebreak_core::{AgentError, AgentRunId, Result, Store};
use tokio::sync::Mutex;

use crate::code_execution::ConfiguredExecProvider;

/// Directory-name prefix for a background run's workspace; must match
/// `sandbox_exec_worker::agent_run_workspace`.
const WORKSPACE_PREFIX: &str = "agent-run-";

#[derive(Debug, Clone, Copy)]
pub(crate) struct AgentRunScratchReaperConfig {
    /// How long to wait past a run's terminal transition (or, for a directory
    /// with no matching run row, past its own mtime) before destroying its
    /// workspace. Absorbs any straggling async work — a heartbeat in flight, a
    /// clock skew — around the moment a run settles.
    grace_period: Duration,
    /// Delay between sweeps once a sweep found nothing left to reap.
    interval: Duration,
    /// Delay between sweeps while a discovered batch is still draining.
    drain_delay: Duration,
    /// Delay after a sweep that hit an error reading the filesystem or store.
    failure_delay: Duration,
    /// Directories checked against the store per sweep.
    batch_size: usize,
}

impl Default for AgentRunScratchReaperConfig {
    fn default() -> Self {
        Self {
            grace_period: Duration::from_secs(60 * 60),
            interval: Duration::from_secs(15 * 60),
            drain_delay: Duration::from_millis(50),
            failure_delay: Duration::from_secs(60),
            batch_size: 64,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct AgentRunScratchReapReport {
    pub scanned: usize,
    pub reaped: usize,
    pub skipped: usize,
    pub remaining: usize,
    pub failures: Vec<AgentRunScratchReapFailure>,
}

#[derive(Debug)]
pub(crate) struct AgentRunScratchReapFailure {
    pub run_id: Option<AgentRunId>,
    pub error: String,
}

#[derive(Clone)]
pub(crate) struct AgentRunScratchReaper {
    store: Arc<dyn Store>,
    code_execution: Arc<ConfiguredExecProvider>,
    scratch_root: Arc<PathBuf>,
    sweep: Arc<Mutex<()>>,
    inventory: Arc<Mutex<VecDeque<AgentRunId>>>,
    config: AgentRunScratchReaperConfig,
}

struct ScanResult {
    scanned: usize,
    candidates: VecDeque<AgentRunId>,
    failures: Vec<AgentRunScratchReapFailure>,
}

impl AgentRunScratchReaper {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        code_execution: Arc<ConfiguredExecProvider>,
        scratch_root: impl Into<PathBuf>,
        config: AgentRunScratchReaperConfig,
    ) -> Self {
        assert!(config.batch_size > 0);
        Self {
            store,
            code_execution,
            scratch_root: Arc::new(scratch_root.into()),
            sweep: Arc::new(Mutex::new(())),
            inventory: Arc::new(Mutex::new(VecDeque::new())),
            config,
        }
    }

    pub(crate) async fn run(self) {
        loop {
            let delay = match self.sweep_once().await {
                Ok(report) => {
                    if report.reaped > 0 || !report.failures.is_empty() {
                        tracing::info!(
                            "tidebreak: agent-run scratch sweep scanned {}, reaped {}, skipped {}, remaining {}, errors {}",
                            report.scanned,
                            report.reaped,
                            report.skipped,
                            report.remaining,
                            report.failures.len()
                        );
                    }
                    for failure in &report.failures {
                        match failure.run_id {
                            Some(run_id) => tracing::error!(
                                "tidebreak: agent run {run_id} scratch reap failed: {}",
                                failure.error
                            ),
                            None => tracing::error!(
                                "tidebreak: agent-run scratch scan failed: {}",
                                failure.error
                            ),
                        }
                    }
                    next_delay(self.config, &report)
                }
                Err(error) => {
                    tracing::error!("tidebreak: agent-run scratch sweep failed: {error}");
                    self.config.failure_delay
                }
            };
            tokio::time::sleep(delay).await;
        }
    }

    pub(crate) async fn sweep_once(&self) -> Result<AgentRunScratchReapReport> {
        let _sweep = self.sweep.lock().await;
        let cutoff = SystemTime::now()
            .checked_sub(self.config.grace_period)
            .ok_or_else(|| AgentError::msg("agent-run scratch grace period exceeds system time"))?;
        let mut inventory = self.inventory.lock().await;
        let scan = if inventory.is_empty() {
            let root = Arc::clone(&self.scratch_root);
            let scan = tokio::task::spawn_blocking(move || scan_candidates(&root))
                .await
                .map_err(|error| {
                    AgentError::Store(format!("agent-run scratch scan task failed: {error}"))
                })??;
            inventory.extend(scan.candidates.iter().copied());
            Some(scan)
        } else {
            None
        };
        let candidates = (0..self.config.batch_size)
            .filter_map(|_| inventory.pop_front())
            .collect::<Vec<_>>();
        drop(inventory);
        let mut report = AgentRunScratchReapReport {
            scanned: scan.as_ref().map_or(0, |scan| scan.scanned),
            failures: scan.map_or_else(Vec::new, |scan| scan.failures),
            ..AgentRunScratchReapReport::default()
        };
        for run_id in candidates {
            match self.reap_one(run_id, cutoff).await {
                Ok(true) => report.reaped += 1,
                Ok(false) => report.skipped += 1,
                Err(error) => report.failures.push(AgentRunScratchReapFailure {
                    run_id: Some(run_id),
                    error: error.to_string(),
                }),
            }
        }
        report.remaining = self.inventory.lock().await.len();
        Ok(report)
    }

    /// Decide whether one candidate workspace is safe to destroy, and destroy
    /// it if so. Returns whether it was reaped.
    async fn reap_one(&self, run_id: AgentRunId, cutoff: SystemTime) -> Result<bool> {
        match self.store.get_agent_run(run_id).await? {
            Some(run) => {
                // Anything short of terminal — including `RetryWait`, which
                // reuses this same workspace on its next attempt — must never
                // be touched.
                if !run.status.is_terminal() {
                    return Ok(false);
                }
                // A store defect that terminalized a run without stamping
                // `finished_at` is not this sweep's problem to guess at; leave
                // the workspace for a human to notice rather than destroy on
                // a missing signal.
                let Some(finished_at) = run.finished_at else {
                    return Ok(false);
                };
                if !past_cutoff(finished_at, cutoff) {
                    return Ok(false);
                }
            }
            // No matching run row. Every run row is created durably before its
            // workspace ever exists, so a missing row is either a directory this
            // process does not recognize — leftover from an older naming scheme
            // or a foreign process — or a workspace whose run was erased with
            // its conversation and whose immediate cleanup did not finish (host
            // crash, destroy failure). Still gate on the directory's own age so
            // a workspace whose row insert has not yet become visible is never
            // raced, and so a just-deleted chat's in-flight destroy is not
            // double-driven by this sweep.
            None => {
                if !self.directory_is_old_enough(run_id, cutoff).await? {
                    return Ok(false);
                }
            }
        }
        self.destroy(run_id).await?;
        Ok(true)
    }

    async fn directory_is_old_enough(
        &self,
        run_id: AgentRunId,
        cutoff: SystemTime,
    ) -> Result<bool> {
        let path = self.scratch_root.join(workspace_dir_name(run_id));
        tokio::task::spawn_blocking(move || directory_mtime_before(&path, cutoff))
            .await
            .map_err(|error| {
                AgentError::Store(format!("agent-run scratch metadata task failed: {error}"))
            })?
    }

    /// Destroy one run's workspace: the provider-side session or sandbox (a
    /// remote provider's own quota release, and — for the local provider — the
    /// on-disk workspace and its env-home directory too), plus the host-side
    /// mirror directory every provider stages files through, which a remote
    /// provider's own `destroy_workspace` does not touch.
    async fn destroy(&self, run_id: AgentRunId) -> Result<()> {
        destroy_agent_run_workspace(&self.code_execution, self.scratch_root.as_path(), run_id).await
    }
}

/// Destroy one background run's workspace and host scratch directory.
///
/// Shared by the periodic reaper and the chat-deletion path, which erases a
/// deleted conversation's former runs immediately rather than waiting for the
/// next sweep. Safe to call when the workspace is already gone.
pub(crate) async fn destroy_agent_run_workspace(
    code_execution: &ConfiguredExecProvider,
    scratch_root: &Path,
    run_id: AgentRunId,
) -> Result<()> {
    let workspace = ExecutionWorkspaceId::parse(workspace_dir_name(run_id))
        .map_err(|error| AgentError::msg(format!("invalid agent-run workspace id: {error}")))?;
    if let Some(configured) = code_execution
        .workspace()
        .await
        .map_err(|error| AgentError::msg(error.to_string()))?
    {
        configured
            .destroy_workspace(&workspace)
            .await
            .map_err(|error| AgentError::msg(error.to_string()))?;
    }
    let path = scratch_root.join(workspace.as_str());
    tokio::task::spawn_blocking(move || remove_dir_all_if_present(&path))
        .await
        .map_err(|error| {
            AgentError::Store(format!("agent-run scratch removal task failed: {error}"))
        })?
}

fn next_delay(config: AgentRunScratchReaperConfig, report: &AgentRunScratchReapReport) -> Duration {
    if report.remaining == 0 {
        config.interval
    } else if report.failures.is_empty() {
        config.drain_delay
    } else {
        config.failure_delay
    }
}

fn workspace_dir_name(run_id: AgentRunId) -> String {
    format!("{WORKSPACE_PREFIX}{run_id}")
}

fn past_cutoff(finished_at: DateTime<Utc>, cutoff: SystemTime) -> bool {
    let cutoff: DateTime<Utc> = cutoff.into();
    finished_at <= cutoff
}

fn scan_candidates(root: &Path) -> Result<ScanResult> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ScanResult {
                scanned: 0,
                candidates: VecDeque::new(),
                failures: Vec::new(),
            });
        }
        Err(error) => return Err(scan_error("read scratch directory", error)),
    };
    let mut scanned = 0;
    let mut candidates = Vec::new();
    let mut failures = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                failures.push(AgentRunScratchReapFailure {
                    run_id: None,
                    error: scan_error("read scratch directory entry", error).to_string(),
                });
                continue;
            }
        };
        let Some(run_id) = run_id_from_name(&entry.file_name()) else {
            continue;
        };
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => {
                scanned += 1;
                candidates.push(run_id);
            }
            Ok(_) => {}
            Err(error) => failures.push(AgentRunScratchReapFailure {
                run_id: Some(run_id),
                error: scan_error("read scratch entry type", error).to_string(),
            }),
        }
    }
    candidates.sort_unstable_by_key(|id| id.as_uuid().as_u128());
    Ok(ScanResult {
        scanned,
        candidates: candidates.into(),
        failures,
    })
}

fn directory_mtime_before(path: &Path, cutoff: SystemTime) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(metadata
            .modified()
            .map_err(|error| scan_error("read scratch modification time", error))?
            <= cutoff),
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(scan_error("read scratch metadata", error)),
    }
}

fn remove_dir_all_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(scan_error("remove agent-run scratch directory", error)),
    }
}

fn run_id_from_name(name: &OsStr) -> Option<AgentRunId> {
    let name = name.to_str()?;
    let id: AgentRunId = name.strip_prefix(WORKSPACE_PREFIX)?.parse().ok()?;
    (name == workspace_dir_name(id)).then_some(id)
}

fn scan_error(action: &str, error: std::io::Error) -> AgentError {
    AgentError::Store(format!("failed to {action}: {error}"))
}

#[cfg(test)]
mod tests {
    use std::fs::{self, FileTimes};

    use async_trait::async_trait;
    use tidebreak_core::{
        AdmitSandboxAgentRunOutcome, AgentError, CallId, Chat, ChatId, DbStore, SecretProvider,
        TurnId,
    };

    use super::*;

    struct NoSecrets;

    #[async_trait]
    impl SecretProvider for NoSecrets {
        async fn get_secret(&self, _key: &str) -> Result<Option<String>> {
            Ok(None)
        }

        async fn set_secret(&self, _key: &str, _value: &str) -> Result<()> {
            Err(AgentError::Secret("read only test secrets".into()))
        }

        async fn delete_secret(&self, _key: &str) -> Result<()> {
            Ok(())
        }
    }

    fn config(grace_period: Duration, batch_size: usize) -> AgentRunScratchReaperConfig {
        AgentRunScratchReaperConfig {
            grace_period,
            interval: Duration::from_secs(15 * 60),
            drain_delay: Duration::from_millis(1),
            failure_delay: Duration::from_secs(1),
            batch_size,
        }
    }

    async fn store(dir: &tempfile::TempDir) -> Arc<DbStore> {
        Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("reaper.db").display()
            ))
            .await
            .unwrap(),
        )
    }

    fn provider(store: Arc<DbStore>, scratch_root: &Path) -> Arc<ConfiguredExecProvider> {
        Arc::new(ConfiguredExecProvider::new(
            store,
            Arc::new(NoSecrets),
            scratch_root,
        ))
    }

    fn reaper(
        store: Arc<DbStore>,
        scratch_root: &Path,
        grace_period: Duration,
        batch_size: usize,
    ) -> AgentRunScratchReaper {
        AgentRunScratchReaper::new(
            store.clone(),
            provider(store, scratch_root),
            scratch_root,
            config(grace_period, batch_size),
        )
    }

    /// Admit a background run and drive it straight to `Completed`, the way a
    /// run that finished normally would settle. Returns the run id.
    async fn completed_run(store: &Arc<DbStore>) -> AgentRunId {
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: Some("scratch reaper".into()),
            model: Some("model".into()),
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let turn_id = TurnId::new();
        store
            .accept_turn(turn_id, chat.id, "scratch-test-model", "scratch reap test")
            .await
            .unwrap();
        let turn_lease = uuid::Uuid::new_v4();
        let now = Utc::now();
        let turn = store
            .claim_turn_run(turn_lease, now, now + chrono::Duration::hours(1))
            .await
            .unwrap()
            .turn
            .unwrap();
        let run = match store
            .admit_sandbox_agent_run(
                turn.id,
                CallId::new(),
                "write a report",
                turn_lease,
                turn.steer_revision,
                1,
                Utc::now(),
            )
            .await
            .unwrap()
            .unwrap()
        {
            AdmitSandboxAgentRunOutcome::Accepted { child, .. } => child,
            outcome => panic!("unexpected sandbox admission: {outcome:?}"),
        };
        let lease = uuid::Uuid::new_v4();
        assert_eq!(
            store
                .claim_agent_run(lease, chrono::Duration::minutes(5), 8, 8)
                .await
                .unwrap()
                .unwrap()
                .id,
            run.id
        );
        store
            .submit_agent_run_result(run.id, lease, "done")
            .await
            .unwrap();
        run.id
    }

    fn age_dir(path: &Path, age: Duration) {
        let modified = SystemTime::now().checked_sub(age).unwrap();
        let file = fs::File::open(path).unwrap();
        file.set_times(FileTimes::new().set_modified(modified))
            .unwrap();
    }

    #[tokio::test]
    async fn a_completed_run_past_the_grace_period_is_reaped() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("scratch");
        let store = store(&dir).await;
        let run_id = completed_run(&store).await;
        let workspace = scratch.join(workspace_dir_name(run_id));
        fs::create_dir_all(workspace.join("output")).unwrap();
        fs::write(workspace.join("notes.txt"), b"scratch").unwrap();

        // A zero grace period still proves the terminal check: `finished_at`
        // was stamped strictly before this sweep runs, so it is always at or
        // before a cutoff of "now".
        let reaper = reaper(store.clone(), &scratch, Duration::ZERO, 16);
        let report = reaper.sweep_once().await.unwrap();

        assert_eq!(report.scanned, 1);
        assert_eq!(report.reaped, 1);
        assert_eq!(report.skipped, 0);
        assert!(report.failures.is_empty());
        assert!(!workspace.exists());
    }

    #[tokio::test]
    async fn a_run_still_running_or_within_its_grace_period_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("scratch");
        let store = store(&dir).await;

        // Still running: never touched, no matter how old its directory looks.
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let turn_id = TurnId::new();
        store
            .accept_turn(turn_id, chat.id, "model", "still running")
            .await
            .unwrap();
        let turn_lease = uuid::Uuid::new_v4();
        let now = Utc::now();
        let turn = store
            .claim_turn_run(turn_lease, now, now + chrono::Duration::hours(1))
            .await
            .unwrap()
            .turn
            .unwrap();
        let running = match store
            .admit_sandbox_agent_run(
                turn.id,
                CallId::new(),
                "still running",
                turn_lease,
                turn.steer_revision,
                1,
                Utc::now(),
            )
            .await
            .unwrap()
            .unwrap()
        {
            AdmitSandboxAgentRunOutcome::Accepted { child, .. } => child,
            outcome => panic!("unexpected sandbox admission: {outcome:?}"),
        };
        store
            .claim_agent_run(uuid::Uuid::new_v4(), chrono::Duration::minutes(5), 2, 2)
            .await
            .unwrap();
        let running_dir = scratch.join(workspace_dir_name(running.id));
        fs::create_dir_all(&running_dir).unwrap();
        age_dir(&running_dir, Duration::from_secs(3 * 60 * 60));

        // Completed, but inside its grace period: left alone this sweep.
        let recent = completed_run(&store).await;
        let recent_dir = scratch.join(workspace_dir_name(recent));
        fs::create_dir_all(&recent_dir).unwrap();

        let reaper = reaper(store.clone(), &scratch, Duration::from_secs(60 * 60), 16);
        let report = reaper.sweep_once().await.unwrap();

        assert_eq!(report.scanned, 2);
        assert_eq!(report.reaped, 0);
        assert_eq!(report.skipped, 2);
        assert!(running_dir.exists());
        assert!(recent_dir.exists());
    }

    /// The bytes a run published are the parent conversation's, not the run's
    /// own — see `ConfiguredExecProvider::publish_output_directory`.
    /// A reap of the run's workspace must never reach into the conversation's
    /// own scratch directory, even when both are old.
    #[tokio::test]
    async fn reaping_a_run_workspace_never_touches_the_conversation_scratch() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("scratch");
        let store = store(&dir).await;
        let run_id = completed_run(&store).await;
        let run = store.get_agent_run(run_id).await.unwrap().unwrap();
        let workspace = scratch.join(workspace_dir_name(run_id));
        fs::create_dir_all(&workspace).unwrap();

        let conversation_outputs = scratch.join(run.chat_id.to_string()).join("outputs");
        fs::create_dir_all(&conversation_outputs).unwrap();
        let published = conversation_outputs.join("report.md");
        fs::write(&published, b"published output bytes").unwrap();

        let reaper = reaper(store.clone(), &scratch, Duration::ZERO, 16);
        let report = reaper.sweep_once().await.unwrap();

        assert_eq!(report.reaped, 1);
        assert!(!workspace.exists());
        assert_eq!(
            fs::read(&published).unwrap(),
            b"published output bytes",
            "the run's workspace was destroyed but the conversation's published output survives"
        );
    }

    #[tokio::test]
    async fn an_old_directory_with_no_matching_run_is_reaped_on_its_own_age() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("scratch");
        fs::create_dir_all(&scratch).unwrap();
        let store = store(&dir).await;
        let foreign_id = AgentRunId::new();
        let path = scratch.join(workspace_dir_name(foreign_id));
        fs::create_dir_all(&path).unwrap();
        age_dir(&path, Duration::from_secs(3 * 60 * 60));

        let reaper = reaper(store, &scratch, Duration::from_secs(60 * 60), 16);
        let report = reaper.sweep_once().await.unwrap();

        assert_eq!(report.reaped, 1);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn a_young_directory_with_no_matching_run_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("scratch");
        fs::create_dir_all(&scratch).unwrap();
        let store = store(&dir).await;
        let foreign_id = AgentRunId::new();
        let path = scratch.join(workspace_dir_name(foreign_id));
        fs::create_dir_all(&path).unwrap();

        let reaper = reaper(store, &scratch, Duration::from_secs(60 * 60), 16);
        let report = reaper.sweep_once().await.unwrap();

        assert_eq!(report.reaped, 0);
        assert_eq!(report.skipped, 1);
        assert!(path.exists());
    }
}
