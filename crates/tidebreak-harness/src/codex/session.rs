//! Long-lived `codex app-server --stdio` child.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout;
use tracing::warn;

use crate::codex::parse::CodexStreamParser;
use crate::launch::{validate_launch_plan, LaunchPlan};
use crate::{
    filter_child_env, ApprovalDecision, HarnessApprovalRef, HarnessError, HarnessEvent,
    HarnessSession, SessionSpec, StreamBudget, StreamLineBuffer, TurnInput,
};
use tidebreak_core::CodePermissionMode;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_STDERR_BYTES: usize = 64 * 1_024;

/// Live Codex session: one app-server child for the session lifetime.
pub struct CodexSession {
    spec: SessionSpec,
    resume_ref: Mutex<Option<String>>,
    child: AsyncMutex<Option<Child>>,
    child_pid: AtomicU32,
    stdin: Option<Arc<AsyncMutex<ChildStdin>>>,
    stdout: Option<Arc<AsyncMutex<StdoutReader>>>,
    parser: Mutex<CodexStreamParser>,
    next_id: AtomicI64,
    pending_approvals: Mutex<HashMap<String, Value>>,
    current_turn_id: Mutex<Option<String>>,
}

struct StdoutReader {
    stdout: ChildStdout,
    lines: StreamLineBuffer,
}

impl CodexSession {
    pub(super) fn new(spec: SessionSpec) -> Self {
        let resume_ref = spec.resume_ref.clone();
        Self {
            spec,
            resume_ref: Mutex::new(resume_ref),
            child: AsyncMutex::new(None),
            child_pid: AtomicU32::new(0),
            stdin: None,
            stdout: None,
            parser: Mutex::new(CodexStreamParser::new()),
            next_id: AtomicI64::new(1),
            pending_approvals: Mutex::new(HashMap::new()),
            current_turn_id: Mutex::new(None),
        }
    }

    fn next_rpc_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    async fn write_message(&self, message: &Value) -> Result<(), HarnessError> {
        let Some(stdin) = &self.stdin else {
            return Err(HarnessError::Other("engine child has no stdin".into()));
        };
        let mut line = serde_json::to_vec(message)
            .map_err(|err| HarnessError::Other(format!("serialize rpc: {err}")))?;
        line.push(b'\n');
        let mut guard = stdin.lock().await;
        guard.write_all(&line).await?;
        guard.flush().await?;
        Ok(())
    }

    async fn request(&self, method: &str, params: Value) -> Result<i64, HarnessError> {
        let id = self.next_rpc_id();
        self.parser
            .lock()
            .expect("codex parser")
            .note_outbound(&json!(id), method);
        self.write_message(&json!({ "id": id, "method": method, "params": params }))
            .await?;
        Ok(id)
    }

    async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), HarnessError> {
        let message = match params {
            Some(params) => json!({ "method": method, "params": params }),
            None => json!({ "method": method }),
        };
        self.write_message(&message).await
    }
}

/// Argv for the long-lived app-server child. Prompt never appears here.
pub(crate) fn compose_app_server_plan(
    binary: &std::path::Path,
    extra_argv: &[String],
    cwd: &std::path::Path,
    extra_env: &[(String, String)],
) -> Result<LaunchPlan, HarnessError> {
    let mut argv = vec![
        binary.to_string_lossy().into_owned(),
        "app-server".into(),
        "--stdio".into(),
    ];
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

/// `thread/start` sandbox + approvalPolicy for a permission mode.
#[must_use]
pub(crate) fn thread_start_policy(mode: CodePermissionMode) -> (&'static str, &'static str) {
    match mode {
        CodePermissionMode::Plan => ("read-only", "untrusted"),
        CodePermissionMode::Ask => ("workspace-write", "untrusted"),
        CodePermissionMode::Auto => ("workspace-write", "on-request"),
    }
}

/// Spawn the app-server child and complete initialize + thread/start|resume.
pub(super) async fn attach(spec: SessionSpec) -> Result<CodexSession, HarnessError> {
    let mut session = CodexSession::new(spec);
    let plan = compose_app_server_plan(
        &session.spec.binary,
        &session.spec.extra_argv,
        &session.spec.worktree,
        &session.spec.extra_env,
    )?;
    let mut command = Command::new(&plan.argv[0]);
    command
        .args(&plan.argv[1..])
        .current_dir(&plan.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env_clear();
    for (key, value) in filter_child_env(session.spec.env.iter().cloned()) {
        command.env(key, value);
    }
    for (key, value) in &plan.env {
        command.env(key, value);
    }
    let mut child = command.spawn()?;
    let stdin = child
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
    session.stdin = Some(Arc::new(AsyncMutex::new(stdin)));
    session.stdout = Some(Arc::new(AsyncMutex::new(StdoutReader {
        stdout,
        lines: StreamLineBuffer::new(),
    })));
    session
        .child_pid
        .store(child.id().unwrap_or(0), Ordering::SeqCst);
    *session.child.lock().await = Some(child);
    tokio::spawn(async move {
        let _ = drain_capped(stderr, MAX_STDERR_BYTES).await;
    });

    let init_id = session
        .request(
            "initialize",
            json!({
                "clientInfo": { "name": "tidebreak-harness", "version": "0.0.0" },
                "capabilities": { "experimentalApi": false }
            }),
        )
        .await?;
    session.read_until_rpc(init_id).await?;
    session.notify("initialized", None).await?;

    let (method, params) =
        if let Some(resume) = session.resume_ref.lock().expect("codex resume").clone() {
            ("thread/resume", json!({ "threadId": resume }))
        } else {
            let (sandbox, approval) = thread_start_policy(session.spec.permission_mode);
            (
                "thread/start",
                json!({
                    "cwd": session.spec.worktree,
                    "approvalPolicy": approval,
                    "sandbox": sandbox,
                }),
            )
        };
    let thread_req = session.request(method, params).await?;
    session.read_until_rpc(thread_req).await?;
    Ok(session)
}

impl CodexSession {
    async fn read_until_rpc(&self, rpc_id: i64) -> Result<(), HarnessError> {
        let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
        loop {
            if tokio::time::Instant::now() > deadline {
                return Err(HarnessError::Other(format!(
                    "timed out waiting for rpc id {rpc_id}"
                )));
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let lines = timeout(remaining, self.read_lines()).await;
            let lines = match lines {
                Ok(Ok(lines)) => lines,
                Ok(Err(err)) => return Err(err),
                Err(_) => {
                    return Err(HarnessError::Other(format!(
                        "timed out waiting for rpc id {rpc_id}"
                    )));
                }
            };
            let mut seen = false;
            for line in lines {
                if line_is_rpc_id(&line, rpc_id) {
                    seen = true;
                }
                self.emit_parsed(&line).await;
            }
            if seen {
                return Ok(());
            }
        }
    }

    async fn read_until_terminal_turn(&self) -> Result<(), HarnessError> {
        loop {
            let lines = self.read_lines().await?;
            if lines.is_empty() {
                return Err(HarnessError::Other(
                    "engine stdout closed before the turn finished".into(),
                ));
            }
            let mut terminal = false;
            for line in lines {
                for event in self.emit_parsed(&line).await {
                    if matches!(
                        event,
                        HarnessEvent::TurnCompleted { .. }
                            | HarnessEvent::TurnFailed { .. }
                            | HarnessEvent::TurnInterrupted
                    ) {
                        terminal = true;
                    }
                }
            }
            if terminal {
                return Ok(());
            }
        }
    }

    async fn read_lines(&self) -> Result<Vec<String>, HarnessError> {
        let Some(stdout) = &self.stdout else {
            return Err(HarnessError::Other("engine child has no stdout".into()));
        };
        let mut reader = stdout.lock().await;
        let budget = StreamBudget::default();
        let mut chunk = vec![0_u8; budget.chunk_size];
        loop {
            match reader.stdout.read(&mut chunk).await? {
                0 => return Ok(Vec::new()),
                n => {
                    let tick = reader.lines.push(&chunk[..n], budget);
                    if tick.overflow_chunks > 0 {
                        warn!(
                            overflow_chunks = tick.overflow_chunks,
                            "engine stdout exceeded the parse budget"
                        );
                    }
                    if !tick.lines.is_empty() {
                        return Ok(tick.lines);
                    }
                }
            }
        }
    }

    async fn emit_parsed(&self, line: &str) -> Vec<HarnessEvent> {
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            if let Some(turn_id) = value
                .pointer("/result/turn/id")
                .or_else(|| value.pointer("/params/turn/id"))
                .or_else(|| value.pointer("/params/turnId"))
                .and_then(Value::as_str)
            {
                *self.current_turn_id.lock().expect("codex turn") = Some(turn_id.to_owned());
            }
        }
        let events = self.parser.lock().expect("codex parser").push_line(line);
        for event in &events {
            if let HarnessEvent::SessionStarted {
                resume_ref: Some(resume),
                ..
            } = event
            {
                *self.resume_ref.lock().expect("codex resume") = Some(resume.clone());
            }
            if let HarnessEvent::ApprovalRequested { harness_ref, .. } = event {
                if let Some(id) = self
                    .parser
                    .lock()
                    .expect("codex parser")
                    .pending_approval_rpc_id(&harness_ref.call_id)
                {
                    self.pending_approvals
                        .lock()
                        .expect("codex approvals")
                        .insert(harness_ref.call_id.clone(), id.clone());
                }
            }
            self.spec.sink.emit(event.clone()).await;
        }
        events
    }
}

#[async_trait]
impl HarnessSession for CodexSession {
    async fn run_turn(&self, input: TurnInput) -> Result<(), HarnessError> {
        if self.child_pid.load(Ordering::SeqCst) == 0 {
            return Err(HarnessError::Other("engine child is not running".into()));
        }
        let Some(thread_id) = self.resume_ref.lock().expect("codex resume").clone() else {
            return Err(HarnessError::Other("thread has no resume ref".into()));
        };
        let id = self
            .request(
                "turn/start",
                json!({
                    "threadId": thread_id,
                    "input": [{ "type": "text", "text": input.text }],
                }),
            )
            .await?;
        let _ = id;
        self.read_until_terminal_turn().await
    }

    async fn decide(
        &self,
        approval: HarnessApprovalRef,
        decision: ApprovalDecision,
    ) -> Result<(), HarnessError> {
        let rpc_id = self
            .pending_approvals
            .lock()
            .expect("codex approvals")
            .remove(&approval.call_id)
            .or_else(|| {
                self.parser
                    .lock()
                    .expect("codex parser")
                    .take_pending_approval(&approval.call_id)
            })
            .ok_or_else(|| {
                HarnessError::Other(format!(
                    "no parked approval with call_id {}",
                    approval.call_id
                ))
            })?;
        // Captured channel carries accept/decline only — no rejection string.
        let token = match decision {
            ApprovalDecision::Approve => "accept",
            ApprovalDecision::Deny { .. } => "decline",
        };
        self.write_message(&json!({ "id": rpc_id, "result": { "decision": token } }))
            .await?;
        self.spec
            .sink
            .emit(HarnessEvent::ApprovalResolved {
                harness_ref: approval,
                decision,
            })
            .await;
        Ok(())
    }

    async fn interrupt(&self) -> Result<(), HarnessError> {
        let thread_id = self.resume_ref.lock().expect("codex resume").clone();
        let turn_id = self.current_turn_id.lock().expect("codex turn").clone();
        if let (Some(thread_id), Some(turn_id)) = (thread_id, turn_id) {
            let id = self
                .request(
                    "turn/interrupt",
                    json!({ "threadId": thread_id, "turnId": turn_id }),
                )
                .await;
            if id.is_ok() {
                return Ok(());
            }
        }
        let pid = self.child_pid.load(Ordering::SeqCst);
        if pid != 0 {
            signal_interrupt(pid);
        }
        let mut slot = self.child.lock().await;
        let Some(child) = slot.as_mut() else {
            return Ok(());
        };
        match timeout(Duration::from_secs(2), child.wait()).await {
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
        self.resume_ref.lock().expect("codex resume").clone()
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

fn line_is_rpc_id(line: &str, rpc_id: i64) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    match value.get("id") {
        Some(Value::Number(n)) => n.as_i64() == Some(rpc_id),
        Some(Value::String(s)) => s.parse::<i64>().ok() == Some(rpc_id),
        _ => false,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn app_server_plan_is_clean() {
        let plan = compose_app_server_plan(
            std::path::Path::new("/usr/bin/codex"),
            &[],
            std::path::Path::new("/workspace"),
            &[],
        )
        .unwrap();
        assert_eq!(plan.argv, ["/usr/bin/codex", "app-server", "--stdio"]);
        validate_launch_plan(&plan).unwrap();
    }

    #[test]
    fn extra_bypass_flag_is_rejected() {
        let err = compose_app_server_plan(
            std::path::Path::new("/usr/bin/codex"),
            &["--dangerously-bypass-approvals-and-sandbox".into()],
            std::path::Path::new("/workspace"),
            &[],
        )
        .unwrap_err();
        assert!(matches!(err, HarnessError::LaunchRejected(_)));
    }

    #[test]
    fn permission_mode_mapping_matches_0033() {
        assert_eq!(
            thread_start_policy(CodePermissionMode::Plan),
            ("read-only", "untrusted")
        );
        assert_eq!(
            thread_start_policy(CodePermissionMode::Ask),
            ("workspace-write", "untrusted")
        );
        assert_eq!(
            thread_start_policy(CodePermissionMode::Auto),
            ("workspace-write", "on-request")
        );
        let _ = PathBuf::from("/workspace");
    }
}
