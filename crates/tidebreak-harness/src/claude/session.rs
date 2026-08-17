//! One print-mode child per turn.

use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout;
use tracing::warn;

use crate::child::{signal_interrupt, turn_outcome, ChildPid};
use crate::claude::parse::ClaudeStreamParser;
use crate::launch::{validate_launch_plan_with, BypassPolicy, LaunchPlan};
use crate::{
    filter_child_env, ApprovalDecision, HarnessApprovalRef, HarnessError, HarnessEvent,
    HarnessSession, SessionSpec, StreamBudget, StreamLineBuffer, TurnInput, TurnOutcome,
};
use tidebreak_core::CodePermissionMode;

const INTERRUPT_GRACE: Duration = Duration::from_secs(2);
const MAX_STDERR_BYTES: usize = 64 * 1_024;

/// Per-mode flag mapping captured on 2.1.233:
///   Plan  → --permission-mode plan        (mutations refused)
///   Ask   → --permission-mode manual      (every tool parks on the prompt tool)
///   Auto  → --permission-mode acceptEdits (workspace writes proceed; sensitive still parks)
///   Allow → --dangerously-skip-permissions (engine permission system off)
/// `--permission-mode auto` is the engine's classifier, not Auto.
/// `--allow-dangerously-skip-permissions` is required for print mode to honor
/// the skip flag.
#[must_use]
pub(crate) fn permission_mode_flags(mode: CodePermissionMode) -> Vec<String> {
    match mode {
        CodePermissionMode::Plan => vec!["--permission-mode".into(), "plan".into()],
        CodePermissionMode::Ask => vec!["--permission-mode".into(), "manual".into()],
        CodePermissionMode::Auto => vec!["--permission-mode".into(), "acceptEdits".into()],
        CodePermissionMode::Allow => vec![
            "--dangerously-skip-permissions".into(),
            "--allow-dangerously-skip-permissions".into(),
        ],
    }
}

#[must_use]
pub(crate) fn bypass_policy(mode: CodePermissionMode) -> BypassPolicy {
    match mode {
        CodePermissionMode::Allow => BypassPolicy::Permitted,
        CodePermissionMode::Plan | CodePermissionMode::Ask | CodePermissionMode::Auto => {
            BypassPolicy::Forbidden
        }
    }
}

/// Live Claude Code session: one child per [`HarnessSession::run_turn`].
pub struct ClaudeSession {
    spec: SessionSpec,
    resume_ref: Mutex<Option<String>>,
    child: AsyncMutex<Option<Child>>,
    pid: ChildPid,
    /// Exit status of a child [`HarnessSession::interrupt`] already reaped, so
    /// `run_turn` can still report how the turn ended.
    reaped: Mutex<Option<ExitStatus>>,
    /// Unrecognized events summed across every turn's parser: each turn is a
    /// fresh child, so the per-turn count alone would reset on every prompt.
    unrecognized: AtomicU64,
}

impl ClaudeSession {
    pub(super) fn new(spec: SessionSpec) -> Self {
        let resume_ref = spec.resume_ref.clone();
        Self {
            spec,
            resume_ref: Mutex::new(resume_ref),
            child: AsyncMutex::new(None),
            pid: ChildPid::new(),
            reaped: Mutex::new(None),
            unrecognized: AtomicU64::new(0),
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
        argv.extend(permission_mode_flags(self.spec.permission_mode));
        if let Some(channel) = &self.spec.approval {
            if let Some(flags) = crate::claude::approvals::launch_args_for_approval_channel(channel)
            {
                argv.extend(flags);
            }
        }
        if let Some(resume) = self.resume_ref.lock().expect("claude resume").clone() {
            argv.push("--resume".into());
            argv.push(resume);
        }
        argv.extend(self.spec.extra_argv.iter().cloned());
        let mut env = self.spec.extra_env.clone();
        env.retain(|(key, _)| !key.to_ascii_uppercase().starts_with("TIDEBREAK_") && key != "PWD");
        let plan = LaunchPlan {
            argv,
            cwd: self.spec.worktree.clone(),
            env,
        };
        validate_launch_plan_with(&plan, bypass_policy(self.spec.permission_mode))?;
        Ok(plan)
    }

    /// Drop a child this session is done with, leaving its exit status for
    /// `run_turn` to report.
    fn finish_child(
        &self,
        mut slot: tokio::sync::MutexGuard<'_, Option<Child>>,
        status: Option<ExitStatus>,
    ) {
        *slot = None;
        self.pid.clear();
        *self.reaped.lock().expect("claude child exit") = status;
    }
}

#[async_trait]
impl HarnessSession for ClaudeSession {
    async fn run_turn(&self, input: TurnInput) -> Result<TurnOutcome, HarnessError> {
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
        self.reaped.lock().expect("claude child exit").take();
        // Publish before the first await: the pid is what crash recovery
        // probes, and the window it matters in opens here.
        self.pid.set(child.id());
        *self.child.lock().await = Some(child);

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
                            saw_terminal |=
                                emit_parsed(&self.spec, &mut parser, &self.resume_ref, &line).await;
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
            saw_terminal |= emit_parsed(&self.spec, &mut parser, &self.resume_ref, &pending).await;
        }
        self.unrecognized
            .fetch_add(parser.unrecognized(), Ordering::SeqCst);

        let _ = stdin_task.await;
        let stderr = stderr_task.await.unwrap_or_default();
        if !stderr.is_empty() {
            warn!(bytes = stderr.len(), "engine stderr (capped)");
        }
        // `interrupt` may already have reaped the child; it leaves the status
        // behind so a stopped turn is still distinguishable from a finished one.
        let status = match self.child.lock().await.take() {
            Some(mut child) => child.wait().await.ok(),
            None => self.reaped.lock().expect("claude child exit").take(),
        };
        self.pid.clear();
        Ok(turn_outcome(status, saw_terminal, &stderr))
    }

    async fn decide(
        &self,
        approval: HarnessApprovalRef,
        decision: ApprovalDecision,
    ) -> Result<(), HarnessError> {
        let Some(channel) = &self.spec.approval else {
            return Err(HarnessError::Other(
                "this session has no approval channel".into(),
            ));
        };
        channel
            .completer
            .complete(&approval.call_id, decision)
            .await
    }

    async fn interrupt(&self) -> Result<(), HarnessError> {
        if let Some(pid) = self.pid.get() {
            signal_interrupt(pid);
        }
        let mut slot = self.child.lock().await;
        let Some(child) = slot.as_mut() else {
            return Ok(());
        };
        match timeout(INTERRUPT_GRACE, child.wait()).await {
            Ok(Ok(status)) => {
                self.finish_child(slot, Some(status));
                Ok(())
            }
            Ok(Err(err)) => Err(err.into()),
            Err(_) => {
                warn!("engine did not exit after SIGINT; killing");
                // `kill` already reaps; `try_wait` reads the status it left
                // without risking a second wait on a reaped child.
                child.kill().await?;
                let status = child.try_wait().ok().flatten();
                self.finish_child(slot, status);
                Ok(())
            }
        }
    }

    fn resume_ref(&self) -> Option<String> {
        self.resume_ref.lock().expect("claude resume").clone()
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
            let _ = child.kill().await;
        }
        Ok(())
    }
}

/// Emits one line's events, reporting whether any of them ended the turn.
async fn emit_parsed(
    spec: &SessionSpec,
    parser: &mut ClaudeStreamParser,
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
            *resume_ref.lock().expect("claude resume") = Some(resume.clone());
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::HarnessEventSink;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    struct Discard;

    #[async_trait]
    impl HarnessEventSink for Discard {
        async fn emit(&self, _event: HarnessEvent) {}
    }

    /// A failing engine is indistinguishable from a finished one on stdout
    /// alone: both reach EOF. The exit status and stderr are the only signal
    /// that the turn did not really complete, so they must leave the adapter.
    #[tokio::test]
    async fn a_failed_child_reports_its_exit_and_stderr_and_exposes_its_pid_while_it_runs() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("engine.sh");
        std::fs::write(
            &binary,
            "#!/bin/sh\nsleep 0.5\necho 'auth expired' >&2\nexit 3\n",
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let session = ClaudeSession::new(SessionSpec {
            worktree: dir.path().to_path_buf(),
            permission_mode: CodePermissionMode::Plan,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            env: Vec::new(),
            approval: None,
            binary,
            sink: Arc::new(Discard),
        });
        assert!(
            session.child_pid_changes().is_some(),
            "an adapter with a per-turn child must stream its pid"
        );

        let run = session.run_turn(TurnInput {
            text: "hello".into(),
        });
        let observe = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            session.child_pid()
        };
        let (outcome, mid_turn_pid) = tokio::join!(run, observe);
        assert!(
            mid_turn_pid.is_some(),
            "the pid must be readable while the turn is in flight"
        );
        match outcome.expect("the adapter reports the exit rather than failing") {
            TurnOutcome::Incomplete { detail } => {
                assert!(detail.contains("status 3"), "{detail}");
                assert!(detail.contains("auth expired"), "{detail}");
            }
            other => panic!("a child that exited 3 must not look clean: {other:?}"),
        }
        assert_eq!(session.child_pid(), None, "the pid is cleared on exit");
    }
}
