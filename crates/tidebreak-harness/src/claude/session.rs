//! One print-mode child per turn.

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::time::timeout;
use tracing::warn;

use crate::claude::parse::ClaudeStreamParser;
use crate::launch::{validate_launch_plan, LaunchPlan};
use crate::{
    filter_child_env, ApprovalDecision, HarnessApprovalRef, HarnessError, HarnessEvent,
    HarnessSession, SessionSpec, StreamBudget, StreamLineBuffer, TurnInput,
};
use tidebreak_core::CodePermissionMode;

const INTERRUPT_GRACE: Duration = Duration::from_secs(2);
const MAX_STDERR_BYTES: usize = 64 * 1_024;

/// Live Claude Code session: one child per [`HarnessSession::run_turn`].
pub struct ClaudeSession {
    spec: SessionSpec,
    resume_ref: Option<String>,
    child: Option<Child>,
}

impl ClaudeSession {
    pub(super) fn new(spec: SessionSpec) -> Self {
        let resume_ref = spec.resume_ref.clone();
        Self {
            spec,
            resume_ref,
            child: None,
        }
    }

    fn compose_plan(&self) -> Result<LaunchPlan, HarnessError> {
        // Prompt travels on stdin (`claude -p` with no prompt argument) so a
        // user message cannot trip the bypass-flag denylist.
        let mut argv = vec![
            self.spec.binary.to_string_lossy().into_owned(),
            "-p".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
            "--include-partial-messages".into(),
        ];
        match self.spec.permission_mode {
            CodePermissionMode::Plan => {
                argv.push("--permission-mode".into());
                argv.push("plan".into());
            }
            CodePermissionMode::Ask => {
                // Engine default. Fixtures record this as `permissionMode: default`.
                // `--help` does not list `default` as a flag value, so we omit it
                // rather than guess.
            }
            CodePermissionMode::Auto => {
                argv.push("--permission-mode".into());
                argv.push("acceptEdits".into());
            }
        }
        if let Some(resume) = &self.resume_ref {
            argv.push("--resume".into());
            argv.push(resume.clone());
        }
        argv.extend(self.spec.extra_argv.iter().cloned());
        let mut env = self.spec.extra_env.clone();
        env.retain(|(key, _)| !key.to_ascii_uppercase().starts_with("TIDEBREAK_") && key != "PWD");
        let plan = LaunchPlan {
            argv,
            cwd: self.spec.worktree.clone(),
            env,
        };
        validate_launch_plan(&plan)?;
        Ok(plan)
    }
}

#[async_trait]
impl HarnessSession for ClaudeSession {
    async fn run_turn(&mut self, input: TurnInput) -> Result<(), HarnessError> {
        let plan = self.compose_plan()?;
        let mut command = Command::new(&plan.argv[0]);
        command
            .args(&plan.argv[1..])
            .current_dir(&plan.cwd)
            .stdin(Stdio::piped())
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
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| HarnessError::Other("engine child has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarnessError::Other("engine child has no stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| HarnessError::Other("engine child has no stderr".into()))?;
        self.child = Some(child);

        let prompt = input.text.clone();
        let stdin_task = tokio::spawn(async move {
            let _ = stdin.write_all(prompt.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
        let stderr_task = tokio::spawn(async move { drain_capped(stderr, MAX_STDERR_BYTES).await });

        let mut parser = ClaudeStreamParser::new();
        let budget = StreamBudget::default();
        let mut lines = StreamLineBuffer::new();
        let mut reader = stdout;
        let mut chunk = vec![0_u8; budget.chunk_size];
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
                            emit_parsed(&self.spec, &mut parser, &mut self.resume_ref, &line).await;
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
            emit_parsed(&self.spec, &mut parser, &mut self.resume_ref, &pending).await;
        }

        let _ = stdin_task.await;
        let stderr = stderr_task.await.unwrap_or_default();
        if !stderr.is_empty() {
            warn!(bytes = stderr.len(), "engine stderr (capped)");
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.wait().await;
        }
        Ok(())
    }

    async fn decide(
        &mut self,
        _approval: HarnessApprovalRef,
        _decision: ApprovalDecision,
    ) -> Result<(), HarnessError> {
        Err(HarnessError::Other(
            "structured approvals are Unknown for Claude Code 2.1.233; \
             the permission-prompt channel was not captured"
                .into(),
        ))
    }

    async fn interrupt(&mut self) -> Result<(), HarnessError> {
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        if let Some(id) = child.id() {
            signal_interrupt(id);
        }
        match timeout(INTERRUPT_GRACE, child.wait()).await {
            Ok(Ok(_)) => {
                self.child = None;
                Ok(())
            }
            Ok(Err(err)) => Err(err.into()),
            Err(_) => {
                warn!("engine did not exit after SIGINT; killing");
                child.kill().await?;
                self.child = None;
                Ok(())
            }
        }
    }

    fn resume_ref(&self) -> Option<String> {
        self.resume_ref.clone()
    }

    fn child_pid(&self) -> Option<i64> {
        self.child
            .as_ref()
            .and_then(tokio::process::Child::id)
            .map(i64::from)
    }

    async fn shutdown(mut self: Box<Self>) -> Result<(), HarnessError> {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
        }
        Ok(())
    }
}

async fn emit_parsed(
    spec: &SessionSpec,
    parser: &mut ClaudeStreamParser,
    resume_ref: &mut Option<String>,
    line: &str,
) {
    for event in parser.push_line(line) {
        if let HarnessEvent::SessionStarted {
            resume_ref: Some(resume),
            ..
        } = &event
        {
            *resume_ref = Some(resume.clone());
        }
        spec.sink.emit(event).await;
    }
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
