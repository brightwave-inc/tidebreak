//! Durable execution of a background run's exec checkpoints.
//!
//! A background run cannot execute anything itself: it parks the command as a
//! checkpoint and releases its lease, and this lane runs the command under its
//! own executor lease against the run's private workspace. The workspace is
//! named by the run, holds nothing but what the run's own earlier commands
//! wrote, and carries no folder authority — a delegated run must not reach the
//! user's files. Files the command leaves in `output/` are published to the
//! parent conversation as outputs named by their own filenames.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tidebreak_code_execution::{
    ExecError, ExecRequest, ExecResponse, ExecutionId, ExecutionWorkspaceId, OutputArtifactScan,
    OutputArtifactStatus,
};
use tidebreak_core::{
    AgentError, AgentRunId, CallId, ChatId, ClaimSandboxToolCallOutcome, Result, SandboxExecArgs,
    SandboxToolCall, Store, ToolCallResolution, SANDBOX_EXEC_TOOL,
};
use tokio::sync::Notify;

use crate::code_execution::ConfiguredExecProvider;
use crate::retry::LaneBackoff;
use crate::state::SandboxAttemptGuard;

const CANDIDATE_BATCH_SIZE: u64 = 16;
/// Per-stream budget for a command's captured output inside one receipt.
///
/// The whole checkpoint chain is replayed into every model request the run
/// makes, so a receipt is charged against the run's context on each step, not
/// once. The provider already caps capture far higher; this is the tighter
/// bound that keeps a chain of commands affordable.
const MAX_RECEIPT_STREAM_BYTES: usize = 3_000;
const TRUNCATION_MARKER: &str = "\n…[truncated]";

/// One command to run for one durably claimed checkpoint.
#[derive(Debug, Clone)]
pub(crate) struct SandboxExecJob {
    pub(crate) call_id: CallId,
    pub(crate) run_id: AgentRunId,
    pub(crate) chat_id: ChatId,
    pub(crate) arguments: SandboxExecArgs,
}

/// Deliberately coarse host exec failures. A provider or configuration detail
/// must never become a checkpoint receipt or model context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SandboxExecFailure {
    /// The request or the host configuration cannot support exec at all;
    /// retrying is pointless and the receipt is terminal.
    Rejected { code: &'static str, message: String },
    /// A backend or storage fault that may not recur.
    Transient,
}

#[async_trait]
pub(crate) trait SandboxExecRunner: Send + Sync {
    /// Run the command and publish whatever it left in `output/`.
    ///
    /// The scan runs whatever the exit code was: a later failing step must not
    /// hide a file the command already durably wrote.
    async fn run(
        &self,
        job: SandboxExecJob,
    ) -> std::result::Result<(ExecResponse, OutputArtifactScan), SandboxExecFailure>;
}

/// The configured host provider, confined to one background run's workspace.
struct HostSandboxExec {
    provider: Arc<ConfiguredExecProvider>,
}

#[async_trait]
impl SandboxExecRunner for HostSandboxExec {
    async fn run(
        &self,
        job: SandboxExecJob,
    ) -> std::result::Result<(ExecResponse, OutputArtifactScan), SandboxExecFailure> {
        let workspace = agent_run_workspace(job.run_id).map_err(classify)?;
        let execution = ExecutionId::parse(job.call_id.to_string()).map_err(classify)?;
        // `files` and `folder_grants` stay empty: the run's workspace is the
        // only filesystem it has, and nothing outside it may be staged in.
        let request = ExecRequest::new(
            execution,
            workspace.clone(),
            job.arguments.command,
            job.arguments.args,
            job.arguments.cwd,
        )
        .map_err(classify)?;
        let response = self
            .provider
            .execute_for_agent_run(job.chat_id, request)
            .await
            .map_err(classify)?;
        let outputs = match self
            .provider
            .collect_agent_run_outputs(&workspace, job.chat_id, job.call_id, job.run_id)
            .await
        {
            Ok(outputs) => outputs,
            // The command ran; a scan that could not record its files says so
            // in the receipt rather than losing the command's result. The host's
            // own diagnosis stays here — the model gets the coarse fact, which
            // is all it can act on.
            Err(error) => {
                tracing::warn!(%error, "agent-run output scan failed");
                OutputArtifactScan {
                    entries: Vec::new(),
                    notes: vec![
                        "outputs could not be recorded for this command; files under output/ \
                         are not published yet"
                            .to_owned(),
                    ],
                }
            }
        };
        Ok((response, outputs))
    }
}

/// Workspace identity for one background run.
///
/// The run, never its parent conversation: sibling runs delegated in one
/// message execute concurrently and must not share a filesystem. They do share
/// the conversation's outputs catalog, so two runs that write `report.md`
/// produce two versions of one output.
fn agent_run_workspace(run_id: AgentRunId) -> std::result::Result<ExecutionWorkspaceId, ExecError> {
    ExecutionWorkspaceId::parse(format!("agent-run-{run_id}"))
}

fn classify(error: ExecError) -> SandboxExecFailure {
    match error {
        ExecError::NotConfigured => SandboxExecFailure::Rejected {
            code: "exec_not_configured",
            message: "Code execution is not configured for this host.".into(),
        },
        ExecError::InvalidRequest(_) => SandboxExecFailure::Rejected {
            code: "invalid_exec_arguments",
            message: "The command could not be run as requested.".into(),
        },
        _ => SandboxExecFailure::Transient,
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SandboxExecWorkerConfig {
    lease: Duration,
    heartbeat: Duration,
    idle_min: Duration,
    idle_cap: Duration,
    failure_delay: Duration,
    failure_delay_cap: Duration,
    retry_delay: Duration,
    max_concurrency: usize,
}

impl Default for SandboxExecWorkerConfig {
    fn default() -> Self {
        // A command is capped at two minutes, and a cold managed sandbox can
        // spend a while being built before it runs. The lease covers that with
        // room to spare and is renewed while the command runs, so an executor
        // that dies is still reclaimed promptly rather than at the far end of
        // the envelope. The database caps the lease at the run's deadline.
        Self {
            lease: Duration::from_secs(300),
            heartbeat: Duration::from_secs(20),
            idle_min: Duration::from_millis(250),
            idle_cap: Duration::from_secs(5),
            failure_delay: Duration::from_secs(1),
            failure_delay_cap: Duration::from_secs(30),
            retry_delay: Duration::from_secs(2),
            max_concurrency: 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxExecWorkerOutcome {
    Idle,
    Resolved(CallId),
    RetryScheduled(CallId),
    LeaseLost(CallId),
}

#[derive(Clone)]
pub(crate) struct SandboxExecWorker {
    store: Arc<dyn Store>,
    exec: Arc<dyn SandboxExecRunner>,
    wake: Arc<Notify>,
    attempts: Arc<SandboxAttemptGuard>,
    config: SandboxExecWorkerConfig,
}

impl SandboxExecWorker {
    pub(crate) fn with_attempts(
        store: Arc<dyn Store>,
        provider: Arc<ConfiguredExecProvider>,
        wake: Arc<Notify>,
        attempts: Arc<SandboxAttemptGuard>,
        config: SandboxExecWorkerConfig,
    ) -> Self {
        Self::with_runner_and_attempts(
            store,
            Arc::new(HostSandboxExec { provider }),
            wake,
            attempts,
            config,
        )
    }

    #[cfg(test)]
    fn with_runner(
        store: Arc<dyn Store>,
        exec: Arc<dyn SandboxExecRunner>,
        wake: Arc<Notify>,
        config: SandboxExecWorkerConfig,
    ) -> Self {
        Self::with_runner_and_attempts(
            store,
            exec,
            wake,
            Arc::new(SandboxAttemptGuard::default()),
            config,
        )
    }

    fn with_runner_and_attempts(
        store: Arc<dyn Store>,
        exec: Arc<dyn SandboxExecRunner>,
        wake: Arc<Notify>,
        attempts: Arc<SandboxAttemptGuard>,
        config: SandboxExecWorkerConfig,
    ) -> Self {
        assert!(!config.lease.is_zero());
        assert!(!config.heartbeat.is_zero());
        assert!(config.max_concurrency > 0);
        Self {
            store,
            exec,
            wake,
            attempts,
            config,
        }
    }

    pub(crate) async fn run(self) {
        let mut lanes = tokio::task::JoinSet::new();
        for _ in 0..self.config.max_concurrency {
            lanes.spawn(self.clone().run_lane());
        }
        let mut restart_backoff =
            LaneBackoff::new(self.config.failure_delay, self.config.failure_delay_cap);
        while let Some(result) = lanes.join_next().await {
            if let Err(error) = result {
                tracing::error!("tidebreak: sandbox exec worker lane stopped: {error}");
                tokio::time::sleep(restart_backoff.next_delay()).await;
            }
            lanes.spawn(self.clone().run_lane());
        }
    }

    async fn run_lane(self) {
        let mut idle_delay = self.config.idle_min;
        let mut failure_backoff =
            LaneBackoff::new(self.config.failure_delay, self.config.failure_delay_cap);
        loop {
            match self.run_once().await {
                Ok(SandboxExecWorkerOutcome::Idle) => {
                    failure_backoff.reset();
                    tokio::select! {
                        _ = tokio::time::sleep(idle_delay) => {}
                        _ = self.wake.notified() => {}
                    }
                    idle_delay = idle_delay.saturating_mul(2).min(self.config.idle_cap);
                }
                Ok(_) => {
                    failure_backoff.reset();
                    idle_delay = self.config.idle_min;
                }
                Err(error) => {
                    tracing::warn!("tidebreak: sandbox exec worker iteration failed: {error}");
                    let delay = failure_backoff.next_delay();
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = self.wake.notified() => {}
                    }
                }
            }
        }
    }

    /// Claim and resolve one exact persisted exec checkpoint.
    pub(crate) async fn run_once(&self) -> Result<SandboxExecWorkerOutcome> {
        for candidate in self
            .store
            .list_sandbox_tool_call_candidates_named(SANDBOX_EXEC_TOOL, CANDIDATE_BATCH_SIZE)
            .await?
        {
            let lease_token = uuid::Uuid::new_v4();
            let call = match self
                .store
                .claim_sandbox_tool_call_named(
                    candidate.id,
                    SANDBOX_EXEC_TOOL,
                    lease_token,
                    chrono_duration(self.config.lease)?,
                )
                .await?
            {
                ClaimSandboxToolCallOutcome::Claimed(call)
                | ClaimSandboxToolCallOutcome::Existing(call) => call,
                ClaimSandboxToolCallOutcome::Unavailable => continue,
            };
            self.wake.notify_one();
            return self.process(call, lease_token).await;
        }
        Ok(SandboxExecWorkerOutcome::Idle)
    }

    async fn process(
        &self,
        call: SandboxToolCall,
        lease_token: uuid::Uuid,
    ) -> Result<SandboxExecWorkerOutcome> {
        let Some(active_attempt) =
            self.attempts
                .register_checkpoint(call.id, call.agent_run_id, lease_token)
        else {
            return Ok(SandboxExecWorkerOutcome::LeaseLost(call.id));
        };
        let cancel = active_attempt.cancel_token();
        // Close cancel-before-register and prove this exact executor lease
        // before anything runs.
        let Some(_) = self
            .store
            .heartbeat_sandbox_tool_call(call.id, lease_token, chrono_duration(self.config.lease)?)
            .await?
        else {
            return Ok(SandboxExecWorkerOutcome::LeaseLost(call.id));
        };
        let resolution = match parse_exec_job(&call) {
            Err(resolution) => resolution,
            Ok(job) => {
                // The command holds the lease for as long as it runs, so the
                // lease is renewed underneath it. A renewal the database
                // refuses means this attempt no longer owns the checkpoint;
                // abandon it rather than write a receipt for it.
                let result = {
                    let mut running = std::pin::pin!(self.exec.run(job));
                    let mut heartbeat = tokio::time::interval(self.config.heartbeat);
                    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    heartbeat.tick().await;
                    loop {
                        tokio::select! {
                            result = &mut running => break Some(result),
                            _ = cancel.cancelled() => break None,
                            _ = heartbeat.tick() => {
                                if self
                                    .store
                                    .heartbeat_sandbox_tool_call(
                                        call.id,
                                        lease_token,
                                        chrono_duration(self.config.lease)?,
                                    )
                                    .await?
                                    .is_some()
                                {
                                    continue;
                                }
                                break None;
                            }
                        }
                    }
                };
                match result {
                    None => return Ok(SandboxExecWorkerOutcome::LeaseLost(call.id)),
                    Some(Ok((response, outputs))) => exec_resolution(&response, &outputs),
                    Some(Err(SandboxExecFailure::Rejected { code, message })) => {
                        failed_resolution(code, &message)
                    }
                    Some(Err(SandboxExecFailure::Transient)) => {
                        if call.retry_at.is_none() {
                            return self.schedule_retry(call.id, lease_token).await;
                        }
                        failed_resolution(
                            "exec_failed",
                            "The command could not be run in this task's workspace.",
                        )
                    }
                }
            }
        };
        match self
            .store
            .resolve_sandbox_tool_call(call.id, lease_token, &resolution)
            .await?
        {
            tidebreak_core::ResolveSandboxToolCallOutcome::Resolved
            | tidebreak_core::ResolveSandboxToolCallOutcome::Existing => {
                self.wake.notify_one();
                Ok(SandboxExecWorkerOutcome::Resolved(call.id))
            }
            tidebreak_core::ResolveSandboxToolCallOutcome::NotFound
            | tidebreak_core::ResolveSandboxToolCallOutcome::AlreadyTerminal
            | tidebreak_core::ResolveSandboxToolCallOutcome::LeaseLost => {
                Ok(SandboxExecWorkerOutcome::LeaseLost(call.id))
            }
        }
    }

    /// Park the call for its single bounded retry instead of writing a terminal
    /// failure receipt. The durable `retry_at` marker makes the second attempt
    /// terminal on any failure.
    async fn schedule_retry(
        &self,
        id: CallId,
        lease_token: uuid::Uuid,
    ) -> Result<SandboxExecWorkerOutcome> {
        match self
            .store
            .retry_sandbox_tool_call(id, lease_token, chrono_duration(self.config.retry_delay)?)
            .await?
        {
            tidebreak_core::RetrySandboxToolCallOutcome::Scheduled => {
                self.wake.notify_one();
                Ok(SandboxExecWorkerOutcome::RetryScheduled(id))
            }
            tidebreak_core::RetrySandboxToolCallOutcome::LeaseLost => {
                Ok(SandboxExecWorkerOutcome::LeaseLost(id))
            }
        }
    }
}

fn parse_exec_job(
    call: &SandboxToolCall,
) -> std::result::Result<SandboxExecJob, ToolCallResolution> {
    if call.name != SANDBOX_EXEC_TOOL {
        return Err(failed_resolution(
            "unsupported_sandbox_tool",
            "This sandbox tool is not available.",
        ));
    }
    let invalid = || {
        failed_resolution(
            "invalid_exec_arguments",
            "The command arguments are invalid.",
        )
    };
    let arguments =
        serde_json::from_value::<SandboxExecArgs>(call.arguments.clone()).map_err(|_| invalid())?;
    if !arguments.is_well_formed() {
        return Err(invalid());
    }
    Ok(SandboxExecJob {
        call_id: call.id,
        run_id: call.agent_run_id,
        chat_id: call.chat_id,
        arguments,
    })
}

/// Render one command's result for the model that asked for it.
///
/// Published files come first: they are the point of the command, and the run
/// finishes by naming them. A nonzero exit is not a lane failure — the model
/// asked for a command and is owed its exit code — but it is marked as an error
/// receipt so the model does not read a failed build as a success.
fn exec_resolution(response: &ExecResponse, outputs: &OutputArtifactScan) -> ToolCallResolution {
    let exit = response
        .exit_code
        .map_or_else(|| "signal".into(), |code| code.to_string());
    let mut result = format!("exit: {exit}\nduration_ms: {}", response.duration_ms);
    if response.timed_out {
        result.push_str(
            "\ntimed_out: true (the host killed this command at its time limit; its output may be \
             empty or incomplete — split the work into smaller commands rather than rerunning \
             this one unchanged)",
        );
    }
    let published: Vec<String> = outputs
        .entries
        .iter()
        .filter_map(|entry| match entry.status {
            OutputArtifactStatus::Created => Some(format!(
                "published {} (version {})",
                entry.filename, entry.ordinal
            )),
            OutputArtifactStatus::Updated => Some(format!(
                "republished {} (version {})",
                entry.filename, entry.ordinal
            )),
            // A file that still matches its published version is not news.
            OutputArtifactStatus::Unchanged => None,
        })
        .collect();
    if !published.is_empty() || !outputs.notes.is_empty() {
        result.push_str("\n\noutputs:");
        for line in published.iter().chain(&outputs.notes) {
            result.push('\n');
            result.push_str(line);
        }
    }
    if !response.stdout.is_empty() {
        result.push_str("\n\nstdout:\n");
        result.push_str(&clamp(&response.stdout));
    }
    if !response.stderr.is_empty() {
        result.push_str("\n\nstderr:\n");
        result.push_str(&clamp(&response.stderr));
    }
    if response.timed_out || response.exit_code != Some(0) {
        return ToolCallResolution::Failed {
            result,
            error_code: "exec_command_failed".into(),
            error_detail: None,
        };
    }
    ToolCallResolution::Completed { result }
}

/// Keep the tail of a captured stream — a failing command's diagnosis is
/// usually at the end, not the beginning — within the receipt's budget, and
/// strip interior NUL bytes.
///
/// A receipt carrying a NUL is refused when it is resolved, which would leave
/// the call parked and re-executed every lease period rather than failing. A
/// command that prints one (reading a binary it just wrote, say) is ordinary,
/// so the byte comes out here.
fn clamp(stream: &str) -> String {
    let stream = if stream.contains('\0') {
        std::borrow::Cow::Owned(stream.replace('\0', ""))
    } else {
        std::borrow::Cow::Borrowed(stream)
    };
    let stream = stream.as_ref();
    if stream.len() <= MAX_RECEIPT_STREAM_BYTES {
        return stream.to_owned();
    }
    let start = stream.len() - MAX_RECEIPT_STREAM_BYTES;
    let start = (start..stream.len())
        .find(|index| stream.is_char_boundary(*index))
        .unwrap_or(stream.len());
    format!("{TRUNCATION_MARKER}\n{}", &stream[start..])
}

fn failed_resolution(code: &str, result: &str) -> ToolCallResolution {
    ToolCallResolution::Failed {
        result: result.into(),
        error_code: code.into(),
        error_detail: None,
    }
}

fn chrono_duration(duration: Duration) -> Result<chrono::Duration> {
    chrono::Duration::from_std(duration)
        .map_err(|error| AgentError::msg(format!("invalid sandbox exec duration: {error}")))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use chrono::Utc;
    use tidebreak_code_execution::{ExecProviderKind, OutputArtifactEntry, OutputArtifactStatus};
    use tidebreak_core::{
        Chat, DbStore, ParkSandboxToolCallOutcome, SandboxToolCallRequest, SandboxToolCallStatus,
    };

    use super::*;

    /// One scripted result per attempt, so a lane's retry behaviour is observed
    /// rather than inferred.
    struct ScriptedExec {
        attempts: AtomicUsize,
        results:
            Mutex<Vec<std::result::Result<(ExecResponse, OutputArtifactScan), SandboxExecFailure>>>,
    }

    #[async_trait]
    impl SandboxExecRunner for ScriptedExec {
        async fn run(
            &self,
            _job: SandboxExecJob,
        ) -> std::result::Result<(ExecResponse, OutputArtifactScan), SandboxExecFailure> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            self.results.lock().unwrap().remove(0)
        }
    }

    fn scripted(
        results: Vec<std::result::Result<(ExecResponse, OutputArtifactScan), SandboxExecFailure>>,
    ) -> Arc<ScriptedExec> {
        Arc::new(ScriptedExec {
            attempts: AtomicUsize::new(0),
            results: Mutex::new(results),
        })
    }

    fn response(stdout: &str) -> ExecResponse {
        ExecResponse {
            provider: ExecProviderKind::Local,
            exit_code: Some(0),
            stdout: stdout.into(),
            stderr: String::new(),
            timed_out: false,
            output_truncated: false,
            duration_ms: 12,
            sync_notes: Vec::new(),
            degraded: None,
        }
    }

    async fn test_store() -> (Arc<DbStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("test.db").display()
            ))
            .await
            .unwrap(),
        );
        (store, dir)
    }

    /// Park one exec checkpoint the way a background run's own loop does.
    async fn checkpoint(store: &Arc<DbStore>, arguments: serde_json::Value) -> CallId {
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: Some("sandbox exec".into()),
            model: Some("model".into()),
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let turn_id = tidebreak_core::TurnId::new();
        store
            .accept_turn(turn_id, chat.id, "sandbox-test-model", "sandbox exec test")
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
            tidebreak_core::AdmitSandboxAgentRunOutcome::Accepted { child, .. } => child,
            outcome => panic!("unexpected sandbox admission: {outcome:?}"),
        };
        let worker_lease = uuid::Uuid::new_v4();
        assert_eq!(
            store
                .claim_agent_run(worker_lease, chrono::Duration::minutes(5), 1, 1)
                .await
                .unwrap()
                .unwrap()
                .id,
            run.id
        );
        let request = SandboxToolCallRequest {
            id: CallId::new(),
            agent_run_id: run.id,
            chat_id: chat.id,
            provider_id: "provider-call".into(),
            name: SANDBOX_EXEC_TOOL.into(),
            arguments,
        };
        assert!(matches!(
            store
                .park_agent_run_for_sandbox_tool_calls(
                    run.id,
                    worker_lease,
                    &[crate::tests::dispatchable(&request)]
                )
                .await
                .unwrap(),
            ParkSandboxToolCallOutcome::Parked { .. }
        ));
        request.id
    }

    fn worker(store: Arc<DbStore>, exec: Arc<dyn SandboxExecRunner>) -> SandboxExecWorker {
        SandboxExecWorker::with_runner(
            store,
            exec,
            Arc::new(Notify::new()),
            SandboxExecWorkerConfig::default(),
        )
    }

    #[tokio::test]
    async fn a_command_receipt_names_its_published_files_and_keeps_the_output_tail() {
        let (store, _dir) = test_store().await;
        let call = checkpoint(
            &store,
            serde_json::json!({"command":"python3","args":["report.py"]}),
        )
        .await;
        let stdout = format!("{}TAIL-MARKER", "x".repeat(MAX_RECEIPT_STREAM_BYTES));
        let exec = scripted(vec![Ok((
            response(&stdout),
            OutputArtifactScan {
                entries: vec![OutputArtifactEntry {
                    filename: "Quarterly review.md".into(),
                    output_id: tidebreak_core::OutputId::new().to_string(),
                    ordinal: 1,
                    status: OutputArtifactStatus::Created,
                }],
                notes: Vec::new(),
            },
        ))]);
        assert_eq!(
            worker(store.clone(), exec).run_once().await.unwrap(),
            SandboxExecWorkerOutcome::Resolved(call)
        );
        let receipt = store
            .get_sandbox_tool_call_receipt(call)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receipt.status, SandboxToolCallStatus::Completed);
        // The filename the command chose is what the model is told was
        // published — no host-invented title anywhere in the receipt.
        assert!(receipt
            .result
            .contains("published Quarterly review.md (version 1)"));
        assert!(receipt.result.contains("TAIL-MARKER"));
        assert!(receipt.result.contains(TRUNCATION_MARKER));
        assert!(receipt.result.len() < 2 * MAX_RECEIPT_STREAM_BYTES);
    }

    #[tokio::test]
    async fn a_transient_failure_retries_once_and_then_resolves_terminally() {
        let (store, _dir) = test_store().await;
        let call = checkpoint(&store, serde_json::json!({"command":"python3"})).await;
        let exec = scripted(vec![
            Err(SandboxExecFailure::Transient),
            Err(SandboxExecFailure::Transient),
        ]);
        let worker = SandboxExecWorker::with_runner(
            store.clone(),
            exec.clone(),
            Arc::new(Notify::new()),
            SandboxExecWorkerConfig {
                retry_delay: Duration::from_millis(1),
                ..SandboxExecWorkerConfig::default()
            },
        );
        assert_eq!(
            worker.run_once().await.unwrap(),
            SandboxExecWorkerOutcome::RetryScheduled(call)
        );
        assert!(store
            .get_sandbox_tool_call_receipt(call)
            .await
            .unwrap()
            .is_none());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            worker.run_once().await.unwrap(),
            SandboxExecWorkerOutcome::Resolved(call)
        );
        assert_eq!(exec.attempts.load(Ordering::SeqCst), 2);
        assert_eq!(
            store
                .get_sandbox_tool_call_receipt(call)
                .await
                .unwrap()
                .unwrap()
                .status,
            SandboxToolCallStatus::Failed
        );
    }

    #[tokio::test]
    async fn an_unconfigured_host_fails_the_command_without_spending_a_retry() {
        let (store, _dir) = test_store().await;
        let call = checkpoint(&store, serde_json::json!({"command":"python3"})).await;
        let exec = scripted(vec![Err(SandboxExecFailure::Rejected {
            code: "exec_not_configured",
            message: "Code execution is not configured for this host.".into(),
        })]);
        assert_eq!(
            worker(store.clone(), exec.clone())
                .run_once()
                .await
                .unwrap(),
            SandboxExecWorkerOutcome::Resolved(call)
        );
        assert_eq!(exec.attempts.load(Ordering::SeqCst), 1);
        let receipt = store
            .get_sandbox_tool_call_receipt(call)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receipt.status, SandboxToolCallStatus::Failed);
        assert_eq!(receipt.error_code.as_deref(), Some("exec_not_configured"));
    }
}
