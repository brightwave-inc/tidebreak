//! One print-mode child per session.
//!
//! The child runs `--input-format stream-json` with its stdin held open, so a
//! turn is one user line written into a process that is already warm. The
//! stream's own `result` line ends the turn; the child stays up for the next
//! one. Record 57 has the measurements that forced this.

use std::io;
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::watch;
use tokio::sync::Mutex as AsyncMutex;
use tracing::warn;

use crate::browser_channel::apply_child_env_tokio;
use crate::child::{turn_outcome, ChildPid};
use crate::claude::parse::ClaudeStreamParser;
use crate::launch::{validate_launch_plan_with, BypassPolicy, LaunchPlan};
use crate::{
    spawn_process_tree, ApprovalDecision, BrowserChannelSpec, HarnessApprovalRef, HarnessError,
    HarnessEvent, HarnessSession, ProcessTreeChild, SessionSpec, StreamBudget, StreamLineBuffer,
    TurnInput, TurnOutcome,
};
use tidebreak_core::CodePermissionMode;

const INTERRUPT_GRACE: Duration = Duration::from_secs(2);
const MAX_STDERR_BYTES: usize = 64 * 1_024;
/// How long a dying child gets to finish writing its stderr before the turn
/// reports why it died.
const STDERR_SETTLE: Duration = Duration::from_millis(250);

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

/// The stdout half of a live child, plus the parser reading it.
///
/// `run_turn` is the only reader, and it holds this for the length of a turn.
struct ChildReader {
    stdout: ChildStdout,
    lines: StreamLineBuffer,
    /// One parser per child: its `session_started` guard is what keeps a
    /// repeated `system/init` from minting a second session for the same
    /// process.
    parser: ClaudeStreamParser,
    /// Parser count already added to the session total.
    flushed_unrecognized: u64,
}

/// One live `claude` child and the three handles a session needs on it.
///
/// The locks are separate on purpose: `interrupt` writes to stdin while
/// `run_turn` holds the reader, and either may need to stop the process.
struct EngineChannel {
    stdin: AsyncMutex<ChildStdin>,
    reader: AsyncMutex<ChildReader>,
    child: AsyncMutex<Option<ProcessTreeChild>>,
    /// Exit status of a child that was already reaped by `interrupt` or by
    /// retirement, so the turn in flight can still report how it ended.
    reaped: Mutex<Option<ExitStatus>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Resolved model this child was launched with. `--model` is a launch
    /// flag, so a turn that asks for a different one needs a fresh child.
    model: Option<String>,
}

impl EngineChannel {
    /// Whether the process behind this channel is gone.
    async fn has_exited(&self) -> bool {
        let mut slot = self.child.lock().await;
        match slot.as_mut() {
            Some(child) => !matches!(child.try_wait(), Ok(None)),
            // Already reaped by an escalated interrupt.
            None => true,
        }
    }

    /// Stderr written since the last turn read it.
    fn take_stderr(&self) -> String {
        let taken = std::mem::take(&mut *self.stderr.lock().expect("claude child stderr"));
        String::from_utf8_lossy(&taken).into_owned()
    }

    /// Let the drain task finish a dying child's last words, then take them.
    async fn take_final_stderr(&self) -> String {
        let task = self.stderr_task.lock().expect("claude stderr task").take();
        if let Some(task) = task {
            let _ = tokio::time::timeout(STDERR_SETTLE, task).await;
        }
        self.take_stderr()
    }

    /// Reap the process, recording its exit for the turn to report.
    async fn stop(&self, grace: Option<Duration>) -> Option<ExitStatus> {
        let mut slot = self.child.lock().await;
        let status = match slot.as_mut() {
            Some(child) => match grace {
                Some(grace) => child.interrupt(grace).await.ok(),
                None => child.terminate().await.ok(),
            },
            None => None,
        };
        *slot = None;
        if status.is_some() {
            *self.reaped.lock().expect("claude child exit") = status;
        }
        status
    }

    /// How the process ended, whoever reaped it.
    async fn exit_status(&self) -> Option<ExitStatus> {
        let mut slot = self.child.lock().await;
        match slot.take() {
            Some(mut child) => child.wait().await.ok(),
            None => self.reaped.lock().expect("claude child exit").take(),
        }
    }
}

/// How the reader left the stream.
struct TurnRead {
    /// A `result` line closed the turn.
    saw_terminal: bool,
    /// The child's stdout closed, so the process is gone.
    eof: bool,
}

/// Live Claude Code session: one child for the session lifetime.
pub struct ClaudeSession {
    spec: SessionSpec,
    resume_ref: Mutex<Option<String>>,
    channel: AsyncMutex<Option<Arc<EngineChannel>>>,
    pid: ChildPid,
    /// Unrecognized events summed across every child this session has run.
    /// The parser dies with its child, so the total lives out here.
    unrecognized: AtomicU64,
    /// Stops asked for during the turn in flight. The first is a control
    /// request the engine answers; a second stops the process.
    interrupts_this_turn: AtomicU64,
    /// Whether a turn is running right now. Only a running turn may escalate a
    /// stop into taking the process.
    turn_in_flight: AtomicBool,
    /// Monotonic id for control requests, so a late `control_response` is
    /// never confused with the current one.
    next_control_id: AtomicU64,
}

/// Clears the in-flight flag however `run_turn` leaves.
struct TurnGuard<'a>(&'a AtomicBool);

impl Drop for TurnGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl ClaudeSession {
    pub(super) fn new(spec: SessionSpec) -> Self {
        let resume_ref = spec.resume_ref.clone();
        Self {
            spec,
            resume_ref: Mutex::new(resume_ref),
            channel: AsyncMutex::new(None),
            pid: ChildPid::new(),
            unrecognized: AtomicU64::new(0),
            interrupts_this_turn: AtomicU64::new(0),
            turn_in_flight: AtomicBool::new(false),
            next_control_id: AtomicU64::new(1),
        }
    }

    /// The model a turn actually runs on.
    fn resolved_model(&self, turn_model: Option<&str>) -> Option<String> {
        turn_model.or(self.spec.model.as_deref()).map(str::to_owned)
    }

    fn compose_plan_for(&self, turn_model: Option<&str>) -> Result<LaunchPlan, HarnessError> {
        // Prompt travels on stdin (`claude -p` with no prompt argument) so a
        // user message cannot trip the bypass-flag denylist. Every turn is a
        // stream-json user line on a stdin that stays open, which is what
        // keeps one child serving the whole session (decision 0057). Images
        // ride the same pipe as stream-json user content (decision 0046).
        let mut argv = vec![
            self.spec.binary.to_string_lossy().into_owned(),
            "-p".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
            "--include-partial-messages".into(),
            "--input-format".into(),
            "stream-json".into(),
        ];
        argv.extend(permission_mode_flags(self.spec.permission_mode));
        if let Some(model) = self.resolved_model(turn_model) {
            argv.push("--model".into());
            argv.push(model);
        }
        if let Some(flags) = crate::claude::browser::launch_args_for_mcp_channels(
            self.spec.approval.as_ref(),
            self.spec.browser.as_ref(),
        )? {
            argv.extend(flags);
        }
        if let Some(resume) = self.resume_ref.lock().expect("claude resume").clone() {
            argv.push("--resume".into());
            argv.push(resume);
        }
        argv.extend(self.spec.extra_argv.iter().cloned());
        let mut env = self.spec.extra_env.clone();
        env.retain(|(key, _)| !BrowserChannelSpec::is_reserved_env_key(key) && key != "PWD");
        let plan = LaunchPlan {
            argv,
            cwd: self.spec.worktree.clone(),
            env,
        };
        validate_launch_plan_with(&plan, bypass_policy(self.spec.permission_mode))?;
        Ok(plan)
    }

    /// Start a child for this session, resuming whatever ref the session holds.
    fn spawn_child(&self, turn_model: Option<&str>) -> Result<Arc<EngineChannel>, HarnessError> {
        let plan = self.compose_plan_for(turn_model)?;
        let mut command = Command::new(&plan.argv[0]);
        command
            .args(&plan.argv[1..])
            .current_dir(&plan.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_child_env_tokio(
            &mut command,
            self.spec.env.iter().cloned(),
            &plan.env,
            self.spec.browser.as_ref(),
        );
        let mut child = spawn_process_tree(&mut command)?;
        let stdin = child
            .take_stdin()
            .ok_or_else(|| HarnessError::Other("engine child has no stdin".into()))?;
        let stdout = child
            .take_stdout()
            .ok_or_else(|| HarnessError::Other("engine child has no stdout".into()))?;
        let stderr = child
            .take_stderr()
            .ok_or_else(|| HarnessError::Other("engine child has no stderr".into()))?;
        // Publish before the first await: the pid is what crash recovery
        // probes, and the window it matters in opens here.
        self.pid.set(child.id());

        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink = captured.clone();
        // Nothing reads stderr between turns, so a child that chatters there
        // would fill its pipe and stall. Drain it for the child's whole life
        // and keep the tail for whichever turn has to explain a death.
        let stderr_task =
            tokio::spawn(async move { drain_capped(stderr, MAX_STDERR_BYTES, &sink).await });

        Ok(Arc::new(EngineChannel {
            stdin: AsyncMutex::new(stdin),
            reader: AsyncMutex::new(ChildReader {
                stdout,
                lines: StreamLineBuffer::new(),
                parser: ClaudeStreamParser::new(),
                flushed_unrecognized: 0,
            }),
            child: AsyncMutex::new(Some(child)),
            reaped: Mutex::new(None),
            stderr: captured,
            stderr_task: Mutex::new(Some(stderr_task)),
            model: self.resolved_model(turn_model),
        }))
    }

    /// The channel this turn runs on, and whether it was just spawned.
    ///
    /// A child that has exited, or that was launched on a different model, is
    /// retired here: the replacement resumes the session, so the turn the user
    /// asked for still lands on their transcript.
    async fn ensure_channel(
        &self,
        turn_model: Option<&str>,
    ) -> Result<(Arc<EngineChannel>, bool), HarnessError> {
        let mut slot = self.channel.lock().await;
        if let Some(channel) = slot.as_ref() {
            let same_model = channel.model == self.resolved_model(turn_model);
            // Probing reaps a child that has already exited, so never wait on
            // it again afterwards.
            let exited = channel.has_exited().await;
            if same_model && !exited {
                return Ok((channel.clone(), false));
            }
            if let Some(channel) = slot.take() {
                if !exited {
                    channel.stop(None).await;
                }
            }
            self.pid.clear();
        }
        let channel = self.spawn_child(turn_model)?;
        *slot = Some(channel.clone());
        Ok((channel, true))
    }

    /// Drop the current channel, stopping the process if it is still up.
    async fn retire_channel(&self) {
        let taken = self.channel.lock().await.take();
        if let Some(channel) = taken {
            channel.stop(None).await;
        }
        self.pid.clear();
    }

    async fn write_line(&self, channel: &EngineChannel, line: &[u8]) -> io::Result<()> {
        let mut stdin = channel.stdin.lock().await;
        stdin.write_all(line).await?;
        stdin.flush().await
    }

    /// Read the stream until the turn's own terminal event, or until the
    /// child's stdout closes.
    async fn read_turn(&self, channel: &EngineChannel) -> Result<TurnRead, HarnessError> {
        let mut guard = channel.reader.lock().await;
        let reader = &mut *guard;
        let budget = StreamBudget::default();
        let mut chunk = vec![0_u8; budget.chunk_size];
        let mut saw_terminal = false;
        let mut eof = false;
        let mut failed = None;
        loop {
            let mut chunks_this_tick = 0;
            while chunks_this_tick < budget.max_chunks_per_tick {
                match reader.stdout.read(&mut chunk).await {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(n) => {
                        let tick = reader.lines.push(&chunk[..n], budget);
                        if tick.overflow_chunks > 0 {
                            warn!(
                                overflow_chunks = tick.overflow_chunks,
                                "engine stdout exceeded the parse budget"
                            );
                        }
                        // Every line of the tick is drained even once the turn
                        // has ended: the engine writes lifecycle frames after
                        // its `result`, and a half-consumed tick would lose
                        // them. Reading past the tick would block instead —
                        // the child has nothing more to say until the next
                        // prompt.
                        for line in tick.lines {
                            saw_terminal |= emit_parsed(
                                &self.spec,
                                &mut reader.parser,
                                &self.resume_ref,
                                &line,
                            )
                            .await;
                        }
                        if saw_terminal {
                            break;
                        }
                    }
                    Err(err) => {
                        failed = Some(HarnessError::from(err));
                        break;
                    }
                }
                chunks_this_tick += 1;
            }
            // The child is long-lived, so the turn ends on the stream's own
            // terminal event. Leaving the loop here is what keeps the process
            // running for the next prompt.
            if saw_terminal || eof || failed.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
        if eof && !reader.lines.pending().is_empty() {
            let pending = reader.lines.pending().to_owned();
            saw_terminal |=
                emit_parsed(&self.spec, &mut reader.parser, &self.resume_ref, &pending).await;
        }
        let total = reader.parser.unrecognized();
        self.unrecognized
            .fetch_add(total - reader.flushed_unrecognized, Ordering::SeqCst);
        reader.flushed_unrecognized = total;
        match failed {
            Some(err) => Err(err),
            None => Ok(TurnRead { saw_terminal, eof }),
        }
    }
}

#[async_trait]
impl HarnessSession for ClaudeSession {
    async fn run_turn(&self, input: TurnInput) -> Result<TurnOutcome, HarnessError> {
        self.interrupts_this_turn.store(0, Ordering::SeqCst);
        self.turn_in_flight.store(true, Ordering::SeqCst);
        let _in_flight = TurnGuard(&self.turn_in_flight);
        let prompt = encode_turn_stdin(&input);
        let mut retried = false;
        let channel = loop {
            let (channel, fresh) = self.ensure_channel(input.model.as_deref()).await?;
            match self.write_line(&channel, &prompt).await {
                Ok(()) => break channel,
                // A child that died between turns leaves a pipe that only
                // answers on write. Respawning resumes the session, so the
                // user's message is delivered rather than lost. A child that
                // was just spawned and already refuses stdin is a real
                // failure, not a stale handle.
                Err(err) if !fresh && !retried => {
                    retried = true;
                    warn!(%err, "engine child refused the turn; respawning");
                    self.retire_channel().await;
                }
                Err(err) => {
                    self.retire_channel().await;
                    return Err(err.into());
                }
            }
        };

        let read = match self.read_turn(&channel).await {
            Ok(read) => read,
            Err(err) => {
                // The stream is unusable. Retire the child so the next turn
                // starts a fresh one and resumes.
                self.retire_channel().await;
                return Err(err);
            }
        };

        if !read.eof {
            let stderr = channel.take_stderr();
            if !stderr.is_empty() {
                warn!(bytes = stderr.len(), "engine stderr (capped)");
            }
            // The child is still up and the stream closed the turn itself.
            return Ok(turn_outcome(None, read.saw_terminal, &stderr));
        }

        // Stdout closed: the process is gone. Report how it ended and drop it,
        // so the next turn respawns and resumes from the session's ref.
        let status = channel.exit_status().await;
        let stderr = channel.take_final_stderr().await;
        if !stderr.is_empty() {
            warn!(bytes = stderr.len(), "engine stderr (capped)");
        }
        self.retire_channel().await;
        Ok(turn_outcome(status, read.saw_terminal, &stderr))
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

    /// Stop the running turn without ending the session.
    ///
    /// The first stop is a `control_request`: the engine aborts the turn and
    /// answers with a `result` carrying `terminal_reason: aborted_streaming`,
    /// which the parser reads as `TurnInterrupted`. The child stays up, so the
    /// next prompt costs nothing to start. A second stop for the same running
    /// turn — or a stdin that will not take the request — falls back to
    /// stopping the process. That still leaves the session usable: the next
    /// turn respawns and resumes.
    ///
    /// A stop that arrives with no turn running never takes the process. The
    /// per-turn adapter had no child at all between turns, and a session-long
    /// child must not be worse to stop into.
    async fn interrupt(&self) -> Result<(), HarnessError> {
        let Some(channel) = self.channel.lock().await.clone() else {
            return Ok(());
        };
        let asked = self.interrupts_this_turn.fetch_add(1, Ordering::SeqCst);
        let escalate = asked > 0 && self.turn_in_flight.load(Ordering::SeqCst);
        if !escalate {
            let request_id = format!(
                "tb-interrupt-{}",
                self.next_control_id.fetch_add(1, Ordering::SeqCst)
            );
            let mut line = serde_json::to_vec(&serde_json::json!({
                "type": "control_request",
                "request_id": request_id,
                "request": { "subtype": "interrupt" },
            }))
            .map_err(|err| HarnessError::Other(format!("encode interrupt: {err}")))?;
            line.push(b'\n');
            match self.write_line(&channel, &line).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    warn!(%err, "engine child refused a stop request; stopping the process")
                }
            }
        }
        let taken = self.channel.lock().await.take();
        if let Some(channel) = taken {
            channel.stop(Some(INTERRUPT_GRACE)).await;
        }
        self.pid.clear();
        Ok(())
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
        let taken = self.channel.lock().await.take();
        if let Some(channel) = taken {
            channel.stop(None).await;
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

async fn drain_capped<R>(mut reader: R, cap: usize, into: &Mutex<Vec<u8>>)
where
    R: AsyncReadExt + Unpin,
{
    let mut buf = [0_u8; 4_096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut out = into.lock().expect("claude child stderr");
                if out.len() < cap {
                    let room = cap - out.len();
                    out.extend_from_slice(&buf[..n.min(room)]);
                }
            }
        }
    }
}

/// One stream-json user line per turn, on a stdin that stays open.
pub(crate) fn encode_turn_stdin(input: &TurnInput) -> Vec<u8> {
    let mut content = Vec::new();
    if !input.text.is_empty() || input.images.is_empty() {
        content.push(serde_json::json!({
            "type": "text",
            "text": input.text,
        }));
    }
    for image in &input.images {
        content.push(serde_json::json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": image.media_type,
                "data": base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    &image.bytes,
                ),
            },
        }));
    }
    let mut encoded = serde_json::to_vec(&serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": content,
        },
    }))
    .unwrap_or_else(|_| input.text.as_bytes().to_vec());
    encoded.push(b'\n');
    encoded
}

#[cfg(test)]
mod encode_tests {
    use super::*;
    use crate::TurnImage;

    #[test]
    fn text_rides_one_stream_json_user_line() {
        let encoded = encode_turn_stdin(&TurnInput {
            text: "hello".into(),
            model: None,
            images: Vec::new(),
        });
        let line = String::from_utf8(encoded).unwrap();
        assert!(
            line.ends_with('\n'),
            "stdin stays open, so the line must end"
        );
        let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(value["type"], "user");
        assert_eq!(value["message"]["content"][0]["text"], "hello");
    }

    #[test]
    fn images_ride_stream_json_user_content() {
        let encoded = encode_turn_stdin(&TurnInput {
            text: "look".into(),
            model: None,
            images: vec![TurnImage {
                media_type: "image/png".into(),
                bytes: b"pixels".to_vec(),
            }],
        });
        let line = String::from_utf8(encoded).unwrap();
        assert!(line.ends_with('\n'));
        let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(value["type"], "user");
        assert_eq!(value["message"]["content"][0]["text"], "look");
        assert_eq!(value["message"]["content"][1]["type"], "image");
        assert_eq!(
            value["message"]["content"][1]["source"]["media_type"],
            "image/png"
        );
        assert_eq!(
            value["message"]["content"][1]["source"]["data"],
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"pixels")
        );
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::ApprovalChannelSpec;
    use crate::HarnessEventSink;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    struct Discard;

    #[async_trait]
    impl HarnessEventSink for Discard {
        async fn emit(&self, _event: HarnessEvent) {}
    }

    #[derive(Default)]
    struct Recorder {
        events: Mutex<Vec<HarnessEvent>>,
    }

    #[async_trait]
    impl HarnessEventSink for Recorder {
        async fn emit(&self, event: HarnessEvent) {
            self.events.lock().expect("recorded events").push(event);
        }
    }

    impl Recorder {
        fn snapshot(&self) -> Vec<HarnessEvent> {
            self.events.lock().expect("recorded events").clone()
        }
    }

    struct NoopCompleter;

    #[async_trait]
    impl crate::ApprovalCompleter for NoopCompleter {
        async fn complete(
            &self,
            _call_id: &str,
            _decision: crate::ApprovalDecision,
        ) -> Result<(), crate::HarnessError> {
            Ok(())
        }
    }

    fn session_with(
        binary: PathBuf,
        worktree: &Path,
        sink: Arc<dyn HarnessEventSink>,
    ) -> ClaudeSession {
        ClaudeSession::new(SessionSpec {
            worktree: worktree.to_path_buf(),
            permission_mode: CodePermissionMode::Plan,
            model: None,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            env: Vec::new(),
            approval: None,
            binary,
            sink,
            browser: None,
        })
    }

    fn write_engine(dir: &Path, body: &str) -> PathBuf {
        let binary = dir.join("engine.sh");
        std::fs::write(&binary, body).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        binary
    }

    fn turn(text: &str) -> TurnInput {
        TurnInput {
            text: text.into(),
            model: None,
            images: Vec::new(),
        }
    }

    fn read_lines(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn compose_plan_for_merges_mcp_channels_into_one_config_flag() {
        let dir = tempfile::tempdir().unwrap();
        let approval = ApprovalChannelSpec {
            mcp_endpoint_url: "http://127.0.0.1:9999/code/mcp/approval-prompt".into(),
            token: "session-token".into(),
            completer: Arc::new(NoopCompleter),
        };
        let browser = BrowserChannelSpec::new(
            PathBuf::from("/tmp/session-browser-cap.json"),
            PathBuf::from("/usr/local/bin/tidebreak"),
        );
        let session = ClaudeSession::new(SessionSpec {
            worktree: dir.path().to_path_buf(),
            permission_mode: CodePermissionMode::Plan,
            model: None,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            env: Vec::new(),
            approval: Some(approval),
            binary: dir.path().join("claude"),
            sink: Arc::new(Discard),
            browser: Some(browser),
        });
        let plan = session.compose_plan_for(None).unwrap();
        assert_eq!(
            plan.argv
                .iter()
                .filter(|arg| *arg == "--mcp-config")
                .count(),
            1,
            "compose must use the merged helper exactly once"
        );
        let config_index = plan
            .argv
            .iter()
            .position(|arg| arg == "--mcp-config")
            .unwrap();
        let config: serde_json::Value = serde_json::from_str(&plan.argv[config_index + 1]).unwrap();
        assert!(
            config["mcpServers"].get("tb-approvals").is_some(),
            "merged config keeps the approval HTTP server"
        );
        assert!(
            config["mcpServers"].get("tb-browser").is_some(),
            "merged config adds the browser stdio server"
        );
        assert_eq!(
            plan.argv
                .iter()
                .filter(|arg| *arg == "--permission-prompt-tool")
                .count(),
            1,
            "both channels keep exactly one permission-prompt-tool flag"
        );
    }

    /// Stream-json input used to be an image-turn flag. It is the session's
    /// whole delivery channel now, so it must be on for every launch.
    #[test]
    fn every_turn_reads_stream_json_from_a_stdin_that_stays_open() {
        let dir = tempfile::tempdir().unwrap();
        let session = ClaudeSession::new(SessionSpec {
            worktree: dir.path().to_path_buf(),
            permission_mode: CodePermissionMode::Plan,
            model: None,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            env: Vec::new(),
            approval: None,
            binary: dir.path().join("claude"),
            sink: Arc::new(Discard),
            browser: None,
        });
        let plan = session.compose_plan_for(None).unwrap();
        let index = plan
            .argv
            .iter()
            .position(|arg| arg == "--input-format")
            .expect("a session-long child reads stream-json input on every turn");
        assert_eq!(plan.argv[index + 1], "stream-json");
    }

    /// A failing engine is indistinguishable from a finished one on stdout
    /// alone: both reach EOF. The exit status and stderr are the only signal
    /// that the turn did not really complete, so they must leave the adapter.
    #[tokio::test]
    async fn a_failed_child_reports_its_exit_and_stderr_and_exposes_its_pid_while_it_runs() {
        let dir = tempfile::tempdir().unwrap();
        let binary = write_engine(
            dir.path(),
            "#!/bin/sh\nsleep 0.5\necho 'auth expired' >&2\nexit 3\n",
        );
        let session = session_with(binary, dir.path(), Arc::new(Discard));
        assert!(
            session.child_pid_changes().is_some(),
            "an adapter that owns a child must stream its pid"
        );

        let run = session.run_turn(turn("hello"));
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

    /// The whole point of record 57: two turns, one process, and a turn that
    /// ends on the stream's `result` rather than on the child exiting.
    #[tokio::test]
    async fn two_turns_run_on_one_child_that_never_exits() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox.ndjson");
        let binary = write_engine(
            dir.path(),
            &format!(
                r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> {inbox}
  printf '{{"type":"system","subtype":"init","session_id":"sess-1","claude_code_version":"2.1.238"}}\n'
  printf '{{"type":"stream_event","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"ok"}}}}}}\n'
  printf '{{"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed","session_id":"sess-1","usage":{{"input_tokens":1,"output_tokens":2}}}}\n'
done
"#,
                inbox = inbox.display()
            ),
        );
        let sink = Arc::new(Recorder::default());
        let session = session_with(binary, dir.path(), sink.clone());

        assert!(matches!(
            session.run_turn(turn("first")).await.unwrap(),
            TurnOutcome::Clean
        ));
        let pid = session
            .child_pid()
            .expect("the child outlives the turn it just answered");
        assert!(
            // SAFETY: signal 0 only probes for the process; it delivers nothing.
            unsafe { libc::kill(pid as libc::pid_t, 0) } == 0,
            "the process must still be running between turns"
        );

        assert!(matches!(
            session.run_turn(turn("second")).await.unwrap(),
            TurnOutcome::Clean
        ));
        assert_eq!(
            session.child_pid(),
            Some(pid),
            "the second turn must land on the same child"
        );

        let sent = read_lines(&inbox);
        assert_eq!(sent.len(), 2, "one user line per turn: {sent:?}");
        for (line, expected) in sent.iter().zip(["first", "second"]) {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(value["type"], "user");
            assert_eq!(value["message"]["content"][0]["text"], expected);
        }

        let events = sink.snapshot();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, HarnessEvent::SessionStarted { .. }))
                .count(),
            1,
            "one child means one session_started, however often init repeats"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, HarnessEvent::TurnCompleted { .. }))
                .count(),
            2,
            "each turn still ends on its own result"
        );
        assert_eq!(session.resume_ref().as_deref(), Some("sess-1"));
    }

    /// A stop ends the turn and leaves the session able to run the next one.
    #[tokio::test]
    async fn an_interrupt_ends_the_turn_and_leaves_the_session_usable() {
        let dir = tempfile::tempdir().unwrap();
        let started = dir.path().join("started");
        let binary = write_engine(
            dir.path(),
            &format!(
                r#"#!/bin/sh
turns=0
while IFS= read -r line; do
  case "$line" in
    *control_request*)
      printf '{{"type":"control_response","response":{{"subtype":"success","request_id":"tb-interrupt-1","response":{{"still_queued":[]}}}}}}\n'
      printf '{{"type":"result","subtype":"error_during_execution","is_error":true,"terminal_reason":"aborted_streaming","session_id":"sess-1"}}\n'
      ;;
    *)
      turns=$((turns+1))
      printf '{{"type":"system","subtype":"init","session_id":"sess-1","claude_code_version":"2.1.238"}}\n'
      printf '{{"type":"stream_event","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"working"}}}}}}\n'
      touch {started}
      if [ "$turns" -gt 1 ]; then
        printf '{{"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed","session_id":"sess-1","usage":{{"input_tokens":1,"output_tokens":1}}}}\n'
      fi
      ;;
  esac
done
"#,
                started = started.display()
            ),
        );
        let sink = Arc::new(Recorder::default());
        let session = session_with(binary, dir.path(), sink.clone());

        let run = session.run_turn(turn("write me a novel"));
        let stop = async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            session.interrupt().await
        };
        let (outcome, stopped) = tokio::join!(run, stop);
        stopped.expect("a stop request is accepted");
        assert!(
            matches!(outcome.unwrap(), TurnOutcome::Clean),
            "the engine closed the turn itself, so nothing is incomplete"
        );
        let pid = session
            .child_pid()
            .expect("a stopped turn must not take the session's child with it");
        assert!(sink
            .snapshot()
            .iter()
            .any(|event| matches!(event, HarnessEvent::TurnInterrupted)));

        assert!(matches!(
            session.run_turn(turn("carry on")).await.unwrap(),
            TurnOutcome::Clean
        ));
        assert_eq!(
            session.child_pid(),
            Some(pid),
            "the next turn runs on the same child"
        );
        assert!(sink
            .snapshot()
            .iter()
            .any(|event| matches!(event, HarnessEvent::TurnCompleted { .. })));
    }

    /// A second stop for the same turn does not wait on an engine that is not
    /// answering. It takes the process, and the session survives that too.
    #[tokio::test]
    async fn a_second_stop_takes_the_process_and_the_session_still_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let argv_log = dir.path().join("argv.log");
        let started = dir.path().join("started");
        let binary = write_engine(
            dir.path(),
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> {argv_log}
while IFS= read -r line; do
  case "$line" in
    *control_request*) : ;;
    *)
      printf '{{"type":"system","subtype":"init","session_id":"sess-1","claude_code_version":"2.1.238"}}\n'
      touch {started}
      if [ -f {started}.again ]; then
        printf '{{"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed","session_id":"sess-1","usage":{{}}}}\n'
      fi
      ;;
  esac
done
"#,
                argv_log = argv_log.display(),
                started = started.display()
            ),
        );
        let session = session_with(binary, dir.path(), Arc::new(Discard));

        let run = session.run_turn(turn("ignore me"));
        let stop = async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            session.interrupt().await.unwrap();
            session.interrupt().await.unwrap();
        };
        let (outcome, ()) = tokio::join!(run, stop);
        match outcome.unwrap() {
            TurnOutcome::Incomplete { .. } => {}
            other => panic!("a stopped process cannot report a clean turn: {other:?}"),
        }
        assert_eq!(session.child_pid(), None, "the process is gone");

        std::fs::write(format!("{}.again", started.display()), "").unwrap();
        assert!(matches!(
            session.run_turn(turn("again")).await.unwrap(),
            TurnOutcome::Clean
        ));
        let launches = read_lines(&argv_log);
        assert_eq!(launches.len(), 2, "the next turn respawns: {launches:?}");
        assert!(
            launches[1].contains("--resume sess-1"),
            "the replacement resumes the session: {}",
            launches[1]
        );
    }

    /// A stop aimed at a session that is not running a turn must not cost the
    /// session its warm child. The per-turn adapter had no child to take here.
    #[tokio::test]
    async fn stops_between_turns_leave_the_child_alone() {
        let dir = tempfile::tempdir().unwrap();
        let binary = write_engine(
            dir.path(),
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *control_request*)
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"x","response":{"still_queued":[]}}}\n'
      ;;
    *)
      printf '{"type":"system","subtype":"init","session_id":"sess-1","claude_code_version":"2.1.238"}\n'
      printf '{"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed","session_id":"sess-1","usage":{"input_tokens":1,"output_tokens":1}}\n'
      ;;
  esac
done
"#,
        );
        let session = session_with(binary, dir.path(), Arc::new(Discard));

        assert!(matches!(
            session.run_turn(turn("one")).await.unwrap(),
            TurnOutcome::Clean
        ));
        let pid = session.child_pid().expect("the child outlives the turn");

        session.interrupt().await.unwrap();
        session.interrupt().await.unwrap();
        assert_eq!(
            session.child_pid(),
            Some(pid),
            "an idle stop must not take the process"
        );

        // The engine's answers to those stops are still in the pipe. They must
        // not be read as the next turn ending.
        assert!(matches!(
            session.run_turn(turn("two")).await.unwrap(),
            TurnOutcome::Clean
        ));
        assert_eq!(session.child_pid(), Some(pid));
    }

    /// A child that dies between turns is replaced, and the replacement picks
    /// the session up rather than starting a new one.
    #[tokio::test]
    async fn a_dead_child_is_respawned_and_resumed_on_the_next_turn() {
        let dir = tempfile::tempdir().unwrap();
        let argv_log = dir.path().join("argv.log");
        let binary = write_engine(
            dir.path(),
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> {argv_log}
IFS= read -r line
printf '{{"type":"system","subtype":"init","session_id":"sess-7","claude_code_version":"2.1.238"}}\n'
printf '{{"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed","session_id":"sess-7","usage":{{"input_tokens":1,"output_tokens":1}}}}\n'
exit 0
"#,
                argv_log = argv_log.display()
            ),
        );
        let sink = Arc::new(Recorder::default());
        let session = session_with(binary, dir.path(), sink.clone());

        assert!(matches!(
            session.run_turn(turn("one")).await.unwrap(),
            TurnOutcome::Clean
        ));
        // The child answered and then exited. Give it a moment to be reaped
        // so the next turn sees a dead process rather than a live one.
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(matches!(
            session.run_turn(turn("two")).await.unwrap(),
            TurnOutcome::Clean
        ));
        let launches = read_lines(&argv_log);
        assert_eq!(launches.len(), 2, "a dead child is replaced: {launches:?}");
        assert!(
            !launches[0].contains("--resume"),
            "the first child had nothing to resume: {}",
            launches[0]
        );
        assert!(
            launches[1].contains("--resume sess-7"),
            "the replacement resumes the session: {}",
            launches[1]
        );
        assert_eq!(
            sink.snapshot()
                .iter()
                .filter(|event| matches!(event, HarnessEvent::TurnCompleted { .. }))
                .count(),
            2,
            "both turns completed"
        );
    }
}
