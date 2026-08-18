//! One print-mode child per turn (`--prompt-file` + `--output-format streaming-json`).

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::watch;
use tokio::sync::Mutex as AsyncMutex;
use tracing::warn;

use crate::child::{turn_outcome, ChildPid};
use crate::grok::parse::GrokStreamParser;
use crate::launch::{validate_launch_plan_with, BypassPolicy, LaunchPlan};
use crate::{
    filter_child_env, spawn_process_tree, ApprovalDecision, HarnessApprovalRef, HarnessError,
    HarnessEvent, HarnessSession, ProcessTreeChild, SessionSpec, StreamBudget, StreamLineBuffer,
    TurnInput, TurnOutcome,
};
use tidebreak_core::CodePermissionMode;

const INTERRUPT_GRACE: Duration = Duration::from_secs(2);
const MAX_STDERR_BYTES: usize = 64 * 1_024;

/// Live Grok CLI session: one child per [`HarnessSession::run_turn`].
pub struct GrokSession {
    spec: SessionSpec,
    resume_ref: Mutex<Option<String>>,
    version: String,
    child: AsyncMutex<Option<ProcessTreeChild>>,
    pid: ChildPid,
    /// Exit status of a child [`HarnessSession::interrupt`] already reaped.
    reaped: Mutex<Option<ExitStatus>>,
    /// Unrecognized events summed across every turn's parser: each turn is a
    /// fresh child, so the per-turn count alone would reset on every prompt.
    unrecognized: AtomicU64,
}

impl GrokSession {
    pub(super) fn new(spec: SessionSpec, version: String) -> Self {
        let resume_ref = spec.resume_ref.clone();
        Self {
            spec,
            resume_ref: Mutex::new(resume_ref),
            version,
            child: AsyncMutex::new(None),
            pid: ChildPid::new(),
            reaped: Mutex::new(None),
            unrecognized: AtomicU64::new(0),
        }
    }

    fn compose_plan(
        &self,
        prompt_file: &Path,
        turn_model: Option<&str>,
    ) -> Result<LaunchPlan, HarnessError> {
        compose_print_plan(PrintLaunch {
            binary: &self.spec.binary,
            extra_argv: &self.spec.extra_argv,
            cwd: &self.spec.worktree,
            extra_env: &self.spec.extra_env,
            resume_ref: self.resume_ref.lock().expect("grok resume").as_deref(),
            prompt_file,
            mode: self.spec.permission_mode,
            model: turn_model.or(self.spec.model.as_deref()),
        })
    }
}

/// Inputs for one Grok print-mode launch.
pub(crate) struct PrintLaunch<'a> {
    pub binary: &'a Path,
    pub extra_argv: &'a [String],
    pub cwd: &'a Path,
    pub extra_env: &'a [(String, String)],
    pub resume_ref: Option<&'a str>,
    pub prompt_file: &'a Path,
    pub mode: CodePermissionMode,
    pub model: Option<&'a str>,
}

/// 1.0.4 honors Auto and Allow.
///
/// Auto composes no permission flags: the default print-mode posture
/// executes routine work unprompted (re-probed 2026-08-17 — a write tool
/// ran without asking), which is exactly an unsupervised workspace-write
/// auto. Allow composes `--always-approve`, the engine's documented
/// allow-everything flag. Ask needs a structured approval channel and
/// headless `streaming-json` has none (`grok agent stdio` ACP was probed;
/// no request/response pair was captured). Plan's `--permission-mode plan`
/// and `--sandbox read-only` both wrote files in captured and re-probed
/// turns. Composing `--deny` rules as a stand-in for Plan would
/// approximate a posture the engine's own plan/read-only flags did not
/// honor.
pub(crate) fn refuse_unhonored_mode(mode: CodePermissionMode) -> Result<(), HarnessError> {
    match mode {
        CodePermissionMode::Auto | CodePermissionMode::Allow => Ok(()),
        CodePermissionMode::Plan | CodePermissionMode::Ask => {
            Err(HarnessError::PermissionModeUnsupported(mode))
        }
    }
}

/// Argv for one print-mode child. The prompt lives in `prompt_file`, never
/// on argv. Callers must already have refused an unhonored permission mode.
pub(crate) fn compose_print_plan(launch: PrintLaunch<'_>) -> Result<LaunchPlan, HarnessError> {
    let mut argv = vec![
        launch.binary.to_string_lossy().into_owned(),
        "--prompt-file".into(),
        launch.prompt_file.to_string_lossy().into_owned(),
        "--output-format".into(),
        "streaming-json".into(),
        "--cwd".into(),
        launch.cwd.to_string_lossy().into_owned(),
        "--no-auto-update".into(),
    ];
    if launch.mode == CodePermissionMode::Allow {
        argv.push("--always-approve".into());
    }
    if let Some(model) = launch.model {
        argv.push("--model".into());
        argv.push(model.to_owned());
    }
    if let Some(resume) = launch.resume_ref {
        argv.push("--resume".into());
        argv.push(resume.to_owned());
    }
    argv.extend(launch.extra_argv.iter().cloned());
    let mut env = launch.extra_env.to_vec();
    env.retain(|(key, _)| !key.to_ascii_uppercase().starts_with("TIDEBREAK_") && key != "PWD");
    let policy = match launch.mode {
        CodePermissionMode::Allow => BypassPolicy::Permitted,
        CodePermissionMode::Plan | CodePermissionMode::Ask | CodePermissionMode::Auto => {
            BypassPolicy::Forbidden
        }
    };
    let plan = LaunchPlan {
        argv,
        cwd: launch.cwd.to_path_buf(),
        env,
    };
    validate_launch_plan_with(&plan, policy)?;
    Ok(plan)
}

#[async_trait]
impl HarnessSession for GrokSession {
    async fn run_turn(&self, input: TurnInput) -> Result<TurnOutcome, HarnessError> {
        refuse_unhonored_mode(self.spec.permission_mode)?;
        let prompt_file = write_prompt_file(&input.text)?;
        let plan = match self.compose_plan(&prompt_file, input.model.as_deref()) {
            Ok(plan) => plan,
            Err(err) => {
                let _ = std::fs::remove_file(&prompt_file);
                return Err(err);
            }
        };
        let result = self.spawn_and_read(&plan).await;
        let _ = std::fs::remove_file(&prompt_file);
        result
    }

    async fn decide(
        &self,
        _approval: HarnessApprovalRef,
        _decision: ApprovalDecision,
    ) -> Result<(), HarnessError> {
        Err(HarnessError::Other(
            "this engine has no structured approval channel".into(),
        ))
    }

    async fn interrupt(&self) -> Result<(), HarnessError> {
        let mut slot = self.child.lock().await;
        let Some(child) = slot.as_mut() else {
            return Ok(());
        };
        let status = child.interrupt(INTERRUPT_GRACE).await?;
        self.finish_child(slot, Some(status));
        Ok(())
    }

    fn resume_ref(&self) -> Option<String> {
        self.resume_ref.lock().expect("grok resume").clone()
    }

    fn child_pid(&self) -> Option<i64> {
        self.pid.get()
    }

    fn child_pid_changes(&self) -> Option<watch::Receiver<Option<i64>>> {
        Some(self.pid.subscribe())
    }

    fn unrecognized_events(&self) -> u64 {
        self.unrecognized.load(Ordering::SeqCst)
    }

    async fn shutdown(self: Box<Self>) -> Result<(), HarnessError> {
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.terminate().await;
        }
        Ok(())
    }
}

impl GrokSession {
    /// Drop a child this session is done with, leaving its exit status for
    /// `spawn_and_read` to report.
    fn finish_child(
        &self,
        mut slot: tokio::sync::MutexGuard<'_, Option<ProcessTreeChild>>,
        status: Option<ExitStatus>,
    ) {
        *slot = None;
        self.pid.clear();
        *self.reaped.lock().expect("grok child exit") = status;
    }

    async fn spawn_and_read(&self, plan: &LaunchPlan) -> Result<TurnOutcome, HarnessError> {
        let mut command = Command::new(&plan.argv[0]);
        command
            .args(&plan.argv[1..])
            .current_dir(&plan.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear();
        for (key, value) in filter_child_env(self.spec.env.iter().cloned()) {
            command.env(key, value);
        }
        for (key, value) in &plan.env {
            command.env(key, value);
        }
        let mut child = spawn_process_tree(&mut command)?;
        let stdout = child
            .take_stdout()
            .ok_or_else(|| HarnessError::Other("engine child has no stdout".into()))?;
        let stderr = child
            .take_stderr()
            .ok_or_else(|| HarnessError::Other("engine child has no stderr".into()))?;
        self.reaped.lock().expect("grok child exit").take();
        // Publish before the first await: the pid is what crash recovery
        // probes, and the window it matters in opens here.
        self.pid.set(child.id());
        *self.child.lock().await = Some(child);

        let stderr_task = tokio::spawn(async move { drain_capped(stderr, MAX_STDERR_BYTES).await });

        let mut parser = GrokStreamParser::new();
        parser.set_version(self.version.clone());
        let budget = StreamBudget::default();
        let mut lines = StreamLineBuffer::new();
        let mut reader = stdout;
        let mut chunk = vec![0_u8; budget.chunk_size];
        let mut saw_terminal = false;
        loop {
            let mut chunks_this_tick = 0;
            let mut eof = false;
            while chunks_this_tick < budget.max_chunks_per_tick {
                match reader.read(&mut chunk).await? {
                    0 => {
                        eof = true;
                        break;
                    }
                    n => {
                        let tick = lines.push(&chunk[..n], budget);
                        if tick.overflow_chunks > 0 {
                            warn!(
                                overflow_chunks = tick.overflow_chunks,
                                "engine stdout exceeded the parse budget"
                            );
                        }
                        for line in tick.lines {
                            if emit_parsed(&self.spec, &mut parser, &self.resume_ref, &line).await {
                                saw_terminal = true;
                            }
                        }
                    }
                }
                chunks_this_tick += 1;
            }
            if eof {
                break;
            }
            tokio::task::yield_now().await;
        }
        if !lines.pending().is_empty() {
            let pending = lines.pending().to_owned();
            if emit_parsed(&self.spec, &mut parser, &self.resume_ref, &pending).await {
                saw_terminal = true;
            }
        }
        self.unrecognized
            .fetch_add(parser.unrecognized(), Ordering::SeqCst);

        let stderr = stderr_task.await.unwrap_or_default();
        if !stderr.is_empty() {
            warn!(bytes = stderr.len(), "engine stderr (capped)");
        }
        // `interrupt` may already have reaped the child; it leaves the status
        // behind so a stopped turn is still distinguishable from a finished one.
        let status = match self.child.lock().await.take() {
            Some(mut child) => child.wait().await.ok(),
            None => self.reaped.lock().expect("grok child exit").take(),
        };
        self.pid.clear();
        Ok(turn_outcome(status, saw_terminal, &stderr))
    }
}

fn write_prompt_file(text: &str) -> Result<PathBuf, HarnessError> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("tidebreak-grok-prompt-{stamp}.txt"));
    std::fs::write(&path, text)?;
    Ok(path)
}

async fn emit_parsed(
    spec: &SessionSpec,
    parser: &mut GrokStreamParser,
    resume_ref: &Mutex<Option<String>>,
    line: &str,
) -> bool {
    let mut terminal = false;
    for event in parser.push_line(line) {
        if let HarnessEvent::SessionStarted {
            resume_ref: Some(resume),
            ..
        } = &event
        {
            *resume_ref.lock().expect("grok resume") = Some(resume.clone());
        }
        terminal |= matches!(
            event,
            HarnessEvent::TurnCompleted { .. }
                | HarnessEvent::TurnFailed { .. }
                | HarnessEvent::TurnInterrupted
        );
        spec.sink.emit(event).await;
    }
    terminal
}

async fn drain_capped<R>(mut reader: R, cap: usize) -> String
where
    R: AsyncReadExt + Unpin,
{
    let mut out = Vec::new();
    let mut buf = [0_u8; 4_096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if out.len() < cap {
                    let room = cap - out.len();
                    out.extend_from_slice(&buf[..n.min(room)]);
                }
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
