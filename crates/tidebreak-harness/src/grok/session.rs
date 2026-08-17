//! One print-mode child per turn (`--prompt-file` + `--output-format streaming-json`).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout;
use tracing::warn;

use crate::grok::parse::GrokStreamParser;
use crate::launch::{validate_launch_plan, LaunchPlan};
use crate::{
    filter_child_env, ApprovalDecision, HarnessApprovalRef, HarnessError, HarnessEvent,
    HarnessSession, SessionSpec, StreamBudget, StreamLineBuffer, TurnInput,
};
use tidebreak_core::CodePermissionMode;

const INTERRUPT_GRACE: Duration = Duration::from_secs(2);
const MAX_STDERR_BYTES: usize = 64 * 1_024;

/// Live Grok CLI session: one child per [`HarnessSession::run_turn`].
pub struct GrokSession {
    spec: SessionSpec,
    resume_ref: Mutex<Option<String>>,
    version: String,
    child: AsyncMutex<Option<Child>>,
    child_pid: AtomicU32,
}

impl GrokSession {
    pub(super) fn new(spec: SessionSpec, version: String) -> Self {
        let resume_ref = spec.resume_ref.clone();
        Self {
            spec,
            resume_ref: Mutex::new(resume_ref),
            version,
            child: AsyncMutex::new(None),
            child_pid: AtomicU32::new(0),
        }
    }

    fn compose_plan(&self, prompt_file: &Path) -> Result<LaunchPlan, HarnessError> {
        compose_print_plan(
            &self.spec.binary,
            &self.spec.extra_argv,
            &self.spec.worktree,
            &self.spec.extra_env,
            self.resume_ref.lock().expect("grok resume").as_deref(),
            prompt_file,
        )
    }
}

/// 1.0.4 honors Auto only, as its default headless posture.
///
/// Auto composes no permission flags: the default print-mode posture
/// executes routine work unprompted (re-probed 2026-08-17 — a write tool
/// ran without asking), which is exactly an unsupervised workspace-write
/// auto. Ask needs a structured approval channel and headless
/// `streaming-json` has none (`grok agent stdio` ACP was probed; no
/// request/response pair was captured). Plan's `--permission-mode plan`
/// and `--sandbox read-only` both wrote files in captured and re-probed
/// turns. Composing `--always-approve` / `--yolo` / `bypassPermissions`
/// is forbidden. Composing `--deny` rules as a stand-in for Plan would
/// approximate a posture the engine's own plan/read-only flags did not
/// honor.
pub(crate) fn refuse_unhonored_mode(mode: CodePermissionMode) -> Result<(), HarnessError> {
    match mode {
        CodePermissionMode::Auto => Ok(()),
        CodePermissionMode::Plan | CodePermissionMode::Ask => {
            Err(HarnessError::PermissionModeUnsupported(mode))
        }
    }
}

/// Argv for one print-mode child. The prompt lives in `prompt_file`, never
/// on argv. Callers must already have refused an unhonored permission mode.
pub(crate) fn compose_print_plan(
    binary: &Path,
    extra_argv: &[String],
    cwd: &Path,
    extra_env: &[(String, String)],
    resume_ref: Option<&str>,
    prompt_file: &Path,
) -> Result<LaunchPlan, HarnessError> {
    let mut argv = vec![
        binary.to_string_lossy().into_owned(),
        "--prompt-file".into(),
        prompt_file.to_string_lossy().into_owned(),
        "--output-format".into(),
        "streaming-json".into(),
        "--cwd".into(),
        cwd.to_string_lossy().into_owned(),
        "--no-auto-update".into(),
    ];
    if let Some(resume) = resume_ref {
        argv.push("--resume".into());
        argv.push(resume.to_owned());
    }
    argv.extend(extra_argv.iter().cloned());
    let mut env = extra_env.to_vec();
    env.retain(|(key, _)| !key.to_ascii_uppercase().starts_with("TIDEBREAK_") && key != "PWD");
    let plan = LaunchPlan {
        argv,
        cwd: cwd.to_path_buf(),
        env,
    };
    validate_launch_plan(&plan)?;
    Ok(plan)
}

#[async_trait]
impl HarnessSession for GrokSession {
    async fn run_turn(&self, input: TurnInput) -> Result<(), HarnessError> {
        refuse_unhonored_mode(self.spec.permission_mode)?;
        let prompt_file = write_prompt_file(&input.text)?;
        let plan = match self.compose_plan(&prompt_file) {
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
        let pid = self.child_pid.load(Ordering::SeqCst);
        if pid != 0 {
            signal_interrupt(pid);
        }
        let mut slot = self.child.lock().await;
        let Some(child) = slot.as_mut() else {
            return Ok(());
        };
        match timeout(INTERRUPT_GRACE, child.wait()).await {
            Ok(Ok(_)) => {
                *slot = None;
                self.child_pid.store(0, Ordering::SeqCst);
                Ok(())
            }
            Ok(Err(err)) => Err(err.into()),
            Err(_) => {
                warn!("engine did not exit after SIGINT; killing");
                child.kill().await?;
                *slot = None;
                self.child_pid.store(0, Ordering::SeqCst);
                Ok(())
            }
        }
    }

    fn resume_ref(&self) -> Option<String> {
        self.resume_ref.lock().expect("grok resume").clone()
    }

    fn child_pid(&self) -> Option<i64> {
        match self.child_pid.load(Ordering::SeqCst) {
            0 => None,
            pid => Some(i64::from(pid)),
        }
    }

    async fn shutdown(self: Box<Self>) -> Result<(), HarnessError> {
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.kill().await;
        }
        Ok(())
    }
}

impl GrokSession {
    async fn spawn_and_read(&self, plan: &LaunchPlan) -> Result<(), HarnessError> {
        let mut command = Command::new(&plan.argv[0]);
        command
            .args(&plan.argv[1..])
            .current_dir(&plan.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env_clear();
        for (key, value) in filter_child_env(self.spec.env.iter().cloned()) {
            command.env(key, value);
        }
        for (key, value) in &plan.env {
            command.env(key, value);
        }
        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarnessError::Other("engine child has no stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| HarnessError::Other("engine child has no stderr".into()))?;
        self.child_pid
            .store(child.id().unwrap_or(0), Ordering::SeqCst);
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
        if !saw_terminal {
            self.spec.sink.emit(HarnessEvent::TurnInterrupted).await;
        }

        let stderr = stderr_task.await.unwrap_or_default();
        if !stderr.is_empty() {
            warn!(bytes = stderr.len(), "engine stderr (capped)");
        }
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.wait().await;
        }
        self.child_pid.store(0, Ordering::SeqCst);
        Ok(())
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

fn signal_interrupt(pid: u32) {
    #[cfg(unix)]
    {
        let pid = pid as i32;
        // SAFETY: the pid was recorded from a child we spawned this session.
        let _ = unsafe { libc::kill(pid, libc::SIGINT) };
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}
