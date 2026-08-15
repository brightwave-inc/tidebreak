//! One print-mode child per turn.

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::timeout;
use tracing::warn;

use crate::claude::parse::ClaudeStreamParser;
use crate::launch::{validate_launch_plan, LaunchPlan};
use crate::{
    passthrough_env, ApprovalDecision, HarnessApprovalRef, HarnessError, HarnessEvent,
    HarnessSession, SessionSpec, TurnInput,
};
use tidebreak_core::CodePermissionMode;

const INTERRUPT_GRACE: Duration = Duration::from_secs(2);

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

    fn compose_plan(&self, input: &TurnInput) -> Result<LaunchPlan, HarnessError> {
        let mut argv = vec![
            self.spec.binary.to_string_lossy().into_owned(),
            "-p".into(),
            input.text.clone(),
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
        let plan = self.compose_plan(&input)?;
        let mut command = Command::new(&plan.argv[0]);
        command
            .args(&plan.argv[1..])
            .current_dir(&plan.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env_clear();
        for (key, value) in passthrough_env() {
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
        self.child = Some(child);

        let mut parser = ClaudeStreamParser::new();
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await? {
            for event in parser.push_line(&line) {
                if let HarnessEvent::SessionStarted {
                    resume_ref: Some(resume),
                    ..
                } = &event
                {
                    self.resume_ref = Some(resume.clone());
                }
                self.spec.sink.emit(event).await;
            }
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

    async fn shutdown(mut self: Box<Self>) -> Result<(), HarnessError> {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
        }
        Ok(())
    }
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

/// Unused stdin helper kept so a future streamed-JSON input path has a home.
#[allow(dead_code)]
async fn write_prompt(stdin: &mut tokio::process::ChildStdin, text: &str) -> std::io::Result<()> {
    stdin.write_all(text.as_bytes()).await?;
    stdin.write_all(b"\n").await?;
    stdin.shutdown().await
}
