//! One print-mode child per session.
//!
//! The child runs `--input-format stream-json` with its stdin held open, so a
//! turn is one user line written into a process that is already warm. The
//! stream's own `result` line ends the turn; the child stays up for the next
//! one. Record 57 has the measurements that forced this.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::Hasher;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::watch;
use tokio::sync::{oneshot, Mutex as AsyncMutex};
use tokio::time::{timeout, timeout_at, Instant};
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
use tidebreak_core::{PermissionMode, ReasoningEffort};

#[cfg(not(test))]
const INTERRUPT_GRACE: Duration = Duration::from_secs(2);
#[cfg(test)]
const INTERRUPT_GRACE: Duration = Duration::from_millis(50);
#[cfg(not(test))]
const CONTROL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
const CONTROL_RESPONSE_TIMEOUT: Duration = Duration::from_millis(250);
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
pub(crate) fn permission_mode_flags(mode: PermissionMode) -> Vec<String> {
    match mode {
        PermissionMode::Plan => vec!["--permission-mode".into(), "plan".into()],
        PermissionMode::Ask => vec!["--permission-mode".into(), "manual".into()],
        PermissionMode::Auto => vec!["--permission-mode".into(), "acceptEdits".into()],
        PermissionMode::Allow => vec![
            "--dangerously-skip-permissions".into(),
            "--allow-dangerously-skip-permissions".into(),
        ],
    }
}

/// The keyword that turns ultracode on.
///
/// 2.1.234 exposes no flag for it: the engine scans a human-typed prompt for
/// the word and, when dynamic workflows are available, spends the turn on
/// multi-agent orchestration. A build where they are not just reads a stray
/// word, so this degrades to plain `xhigh` on its own.
pub(crate) const ULTRACODE_KEYWORD: &str = "ultracode";

/// The level a turn actually runs at, already degraded to the engine's ladder.
#[must_use]
pub(crate) fn resolve_effort(effort: Option<ReasoningEffort>) -> Option<ReasoningEffort> {
    effort.and_then(|level| level.clamp_to(crate::claude::EFFORT_LADDER))
}

/// `--effort` for a level. `Ultra` is ultracode, which the engine spells as
/// `xhigh` plus [`ULTRACODE_KEYWORD`] in the prompt — see [`turn_text`].
#[must_use]
pub(crate) fn effort_flags(effort: Option<ReasoningEffort>) -> Vec<String> {
    let Some(level) = resolve_effort(effort) else {
        return Vec::new();
    };
    let token = match level {
        ReasoningEffort::Ultra => ReasoningEffort::XHigh.as_str(),
        other => other.as_str(),
    };
    vec!["--effort".into(), token.to_owned()]
}

/// The single `--settings` flag for Tidebreak-owned Claude settings.
///
/// `plansDirectory` keeps Claude's Plan-mode notes outside both the worktree
/// and the user's default `~/.claude/plans` directory. `fastMode` shares this
/// object because Claude accepts one inline JSON settings value.
///
/// The model check is here rather than at the route that stores the bit,
/// because this is the first point that knows which model the turn actually
/// runs on: a session armed on Opus and then switched to Sonnet still carries
/// `fast_mode`, and Anthropic rejects `speed` outside the ids it serves.
/// Dropping the flag degrades that turn to standard speed, which is the same
/// degrade-don't-refuse rule effort follows — and it is the honest direction,
/// since the alternative claims a premium the model would never run.
pub(crate) fn settings_flags(
    plans_directory: &Path,
    fast_mode: bool,
    model: Option<&str>,
) -> Result<Vec<String>, HarnessError> {
    let plans_directory = plans_directory
        .to_str()
        .ok_or_else(|| HarnessError::Other("Claude plans directory must be valid UTF-8".into()))?;
    let mut settings = serde_json::Map::from_iter([(
        "plansDirectory".to_owned(),
        serde_json::Value::String(plans_directory.to_owned()),
    )]);
    if fast_mode && model.is_some_and(crate::claude::model_serves_fast_mode) {
        settings.insert("fastMode".to_owned(), serde_json::Value::Bool(true));
    }
    Ok(vec![
        "--settings".into(),
        serde_json::Value::Object(settings).to_string(),
    ])
}

/// One file-system entry under Claude's default plan directory.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlanEntry {
    kind: u8,
    len: u64,
    modified_nanos: u128,
    content_hash: u64,
}

/// Snapshot of the default plan directory before a Plan-mode turn.
struct PlanWriteGuard {
    root: PathBuf,
    before: BTreeMap<PathBuf, PlanEntry>,
}

impl PlanWriteGuard {
    fn capture(root: PathBuf) -> io::Result<Self> {
        let before = snapshot_directory(&root)?;
        Ok(Self { root, before })
    }

    fn changed_path(&self) -> io::Result<Option<PathBuf>> {
        let after = snapshot_directory(&self.root)?;
        let paths = self
            .before
            .keys()
            .chain(after.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let changed = |path: &PathBuf| self.before.get(path) != after.get(path);
        Ok(paths
            .iter()
            .find(|path| !path.as_os_str().is_empty() && changed(path))
            .cloned()
            .or_else(|| paths.into_iter().find(changed))
            .map(|path| self.root.join(path)))
    }
}

fn snapshot_directory(root: &Path) -> io::Result<BTreeMap<PathBuf, PlanEntry>> {
    let mut entries = BTreeMap::new();
    if !root.exists() {
        return Ok(entries);
    }
    snapshot_directory_at(root, root, &mut entries)?;
    Ok(entries)
}

fn snapshot_directory_at(
    root: &Path,
    path: &Path,
    entries: &mut BTreeMap<PathBuf, PlanEntry>,
) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    let relative = path.strip_prefix(root).unwrap_or(path).to_path_buf();
    let file_type = metadata.file_type();
    let kind = if file_type.is_dir() {
        1
    } else if file_type.is_file() {
        2
    } else if file_type.is_symlink() {
        3
    } else {
        4
    };
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    let content_hash = if file_type.is_file() {
        hash_file(path)?
    } else if file_type.is_symlink() {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        hasher.write(std::fs::read_link(path)?.as_os_str().as_encoded_bytes());
        hasher.finish()
    } else {
        0
    };
    entries.insert(
        relative,
        PlanEntry {
            kind,
            len: metadata.len(),
            modified_nanos,
            content_hash,
        },
    );
    if file_type.is_dir() {
        let mut children = std::fs::read_dir(path)?.collect::<io::Result<Vec<_>>>()?;
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            snapshot_directory_at(root, &child.path(), entries)?;
        }
    }
    Ok(())
}

fn hash_file(path: &Path) -> io::Result<u64> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.write(&buffer[..read]);
    }
    Ok(hasher.finish())
}

fn ensure_private_directory(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The prompt as the engine receives it: the user's text, plus the ultracode
/// keyword on its own line when the turn asked for that level.
///
/// Appending is the whole mechanism. A prompt that already says the word is
/// left alone, so a user who typed it does not get it twice.
#[must_use]
pub(crate) fn turn_text(input: &TurnInput) -> String {
    if resolve_effort(input.reasoning_effort) != Some(ReasoningEffort::Ultra)
        || crate::text::contains_word(&input.text, ULTRACODE_KEYWORD)
    {
        return input.text.clone();
    }
    if input.text.trim().is_empty() {
        return ULTRACODE_KEYWORD.to_owned();
    }
    format!("{}\n\n{ULTRACODE_KEYWORD}", input.text)
}

/// Claude Code's own token for a mode on the `set_permission_mode` control
/// request, when the mode can be reached without relaunching.
///
/// `Allow` is absent on purpose. Its posture is
/// `--dangerously-skip-permissions`, which the engine only accepts when the
/// child was launched with `--allow-dangerously-skip-permissions` — and
/// composing that flag on a session that did not choose Allow is exactly what
/// decision 0033 forbids. Moving to or from Allow relaunches instead.
#[must_use]
pub(crate) fn live_mode_token(mode: PermissionMode) -> Option<&'static str> {
    match mode {
        PermissionMode::Plan => Some("plan"),
        PermissionMode::Ask => Some("manual"),
        PermissionMode::Auto => Some("acceptEdits"),
        PermissionMode::Allow => None,
    }
}

#[must_use]
pub(crate) fn bypass_policy(mode: PermissionMode) -> BypassPolicy {
    match mode {
        PermissionMode::Allow => BypassPolicy::Permitted,
        PermissionMode::Plan | PermissionMode::Ask | PermissionMode::Auto => {
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
    /// Resolved effort this child was launched with. `--effort` is a launch
    /// flag too, and 2.1.234 has no control request that moves it.
    effort: Option<ReasoningEffort>,
    /// Whether this child was launched in fast mode. `fastMode` rides
    /// `--settings`, so it is a launch flag like the two above.
    fast_mode: bool,
    /// The mode this child is running under.
    ///
    /// Starts as what argv composed and moves with an accepted
    /// `set_permission_mode`, which is what keeps [`ClaudeSession::ensure_channel`]
    /// from retiring a child that already took the new mode. Only the launch
    /// flags decide the bypass posture, and a live switch never crosses it —
    /// see [`live_mode_token`] — so this can move without argv being wrong.
    mode: Mutex<PermissionMode>,
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
        if let Some(grace) = grace {
            return self.interrupt_tree(grace).await.ok().flatten();
        }
        let mut slot = self.child.lock().await;
        let status = match slot.as_mut() {
            Some(child) => child.terminate().await.ok(),
            None => None,
        };
        *slot = None;
        if status.is_some() {
            *self.reaped.lock().expect("claude child exit") = status;
        }
        status
    }

    async fn interrupt_tree(&self, grace: Duration) -> io::Result<Option<ExitStatus>> {
        let child = self.child.lock().await.take();
        let Some(mut child) = child else {
            return Ok(None);
        };
        let status = child.interrupt(grace).await?;
        *self.reaped.lock().expect("claude child exit") = Some(status);
        Ok(Some(status))
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
    /// Tidebreak-owned destination for Claude's Plan-mode files.
    plans_directory: PathBuf,
    /// The session's current permission mode, which a live switch moves.
    /// `spec.permission_mode` is only what it started on.
    permission_mode: Mutex<PermissionMode>,
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
    pending_interrupt: Mutex<Option<PendingClaudeInterrupt>>,
}

struct PendingClaudeInterrupt {
    request_id: String,
    reply: Option<oneshot::Sender<Result<(), HarnessError>>>,
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
        let permission_mode = spec.permission_mode;
        let plans_directory = std::env::temp_dir()
            .join("tidebreak-claude-plans")
            .join(uuid::Uuid::new_v4().to_string());
        Self {
            spec,
            plans_directory,
            permission_mode: Mutex::new(permission_mode),
            resume_ref: Mutex::new(resume_ref),
            channel: AsyncMutex::new(None),
            pid: ChildPid::new(),
            unrecognized: AtomicU64::new(0),
            interrupts_this_turn: AtomicU64::new(0),
            turn_in_flight: AtomicBool::new(false),
            next_control_id: AtomicU64::new(1),
            pending_interrupt: Mutex::new(None),
        }
    }

    /// The model a turn actually runs on.
    fn resolved_model(&self, turn_model: Option<&str>) -> Option<String> {
        turn_model.or(self.spec.model.as_deref()).map(str::to_owned)
    }

    /// The effort a turn actually runs at, already degraded to the ladder.
    fn resolved_effort(&self, turn_effort: Option<ReasoningEffort>) -> Option<ReasoningEffort> {
        resolve_effort(turn_effort.or(self.spec.reasoning_effort))
    }

    /// Whether a turn actually runs fast: armed, and on a model that serves it.
    ///
    /// The session can stay armed across a model switch, so this is what the
    /// child is really launched with and what [`Self::ensure_channel`] must
    /// compare against — otherwise switching between a serving and a
    /// non-serving model would reuse a child composed for the other one.
    fn resolved_fast_mode(&self, turn_model: Option<&str>) -> bool {
        self.spec.fast_mode
            && self
                .resolved_model(turn_model)
                .as_deref()
                .is_some_and(crate::claude::model_serves_fast_mode)
    }

    /// The mode in force right now.
    fn permission_mode(&self) -> PermissionMode {
        *self.permission_mode.lock().expect("claude permission mode")
    }

    fn default_plans_directory(&self) -> Option<PathBuf> {
        let extra_home = self
            .spec
            .extra_env
            .iter()
            .rev()
            .find(|(key, _)| key.eq_ignore_ascii_case("HOME"))
            .map(|(_, value)| PathBuf::from(value.as_str()));
        let probed_home = self
            .spec
            .env
            .iter()
            .rev()
            .find(|(key, _)| key.eq_ignore_ascii_case("HOME"))
            .map(|(_, value)| PathBuf::from(value.as_os_str()));
        extra_home
            .or(probed_home)
            .map(|home| home.join(".claude").join("plans"))
    }

    fn plan_write_guard(&self) -> Result<Option<PlanWriteGuard>, HarnessError> {
        if self.permission_mode() != PermissionMode::Plan {
            return Ok(None);
        }
        self.default_plans_directory()
            .map(PlanWriteGuard::capture)
            .transpose()
            .map_err(HarnessError::from)
    }

    fn compose_plan_for(
        &self,
        turn_model: Option<&str>,
        turn_effort: Option<ReasoningEffort>,
    ) -> Result<LaunchPlan, HarnessError> {
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
        argv.extend(permission_mode_flags(self.permission_mode()));
        if let Some(model) = self.resolved_model(turn_model) {
            argv.push("--model".into());
            argv.push(model);
        }
        argv.extend(effort_flags(self.resolved_effort(turn_effort)));
        ensure_private_directory(&self.plans_directory)?;
        argv.extend(settings_flags(
            &self.plans_directory,
            self.spec.fast_mode,
            self.resolved_model(turn_model).as_deref(),
        )?);
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
        for root in &self.spec.allowed_read_roots {
            if !root.is_absolute() {
                return Err(HarnessError::AllowedReadRootNotAbsolute(
                    root.to_string_lossy().into_owned(),
                ));
            }
            argv.push("--add-dir".into());
            argv.push(root.to_string_lossy().into_owned());
        }
        argv.extend(self.spec.extra_argv.iter().cloned());
        let mut env = self.spec.extra_env.clone();
        env.retain(|(key, _)| !BrowserChannelSpec::is_reserved_env_key(key) && key != "PWD");
        let plan = LaunchPlan {
            argv,
            cwd: self.spec.worktree.clone(),
            env,
        };
        validate_launch_plan_with(&plan, bypass_policy(self.permission_mode()))?;
        Ok(plan)
    }

    /// Start a child for this session, resuming whatever ref the session holds.
    fn spawn_child(
        &self,
        turn_model: Option<&str>,
        turn_effort: Option<ReasoningEffort>,
    ) -> Result<Arc<EngineChannel>, HarnessError> {
        let plan = self.compose_plan_for(turn_model, turn_effort)?;
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
            effort: self.resolved_effort(turn_effort),
            fast_mode: self.resolved_fast_mode(turn_model),
            mode: Mutex::new(self.permission_mode()),
        }))
    }

    /// The channel this turn runs on, and whether it was just spawned.
    ///
    /// A child that has exited, or that was launched on flags this turn no
    /// longer matches, is retired here: the replacement resumes the session, so
    /// the turn the user asked for still lands on their transcript.
    ///
    /// Model, effort, fast mode, and the bypass posture are all launch flags.
    /// A mode switch the control request already handled leaves the child's own
    /// mode agreeing with the session, so only a move to or from `Allow`
    /// respawns for that one.
    async fn ensure_channel(
        &self,
        turn_model: Option<&str>,
        turn_effort: Option<ReasoningEffort>,
    ) -> Result<(Arc<EngineChannel>, bool), HarnessError> {
        let mut slot = self.channel.lock().await;
        if let Some(channel) = slot.as_ref() {
            let same_flags = channel.model == self.resolved_model(turn_model)
                && channel.effort == self.resolved_effort(turn_effort)
                && channel.fast_mode == self.resolved_fast_mode(turn_model)
                && *channel.mode.lock().expect("claude child mode") == self.permission_mode();
            // Probing reaps a child that has already exited, so never wait on
            // it again afterwards.
            let exited = channel.has_exited().await;
            if same_flags && !exited {
                return Ok((channel.clone(), false));
            }
            if let Some(channel) = slot.take() {
                if !exited {
                    channel.stop(None).await;
                }
            }
            self.pid.clear();
        }
        let channel = self.spawn_child(turn_model, turn_effort)?;
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

    fn permission_mode_acknowledgement(
        line: &str,
        request_id: &str,
    ) -> Option<Result<(), HarnessError>> {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return None;
        };
        if value.get("type").and_then(serde_json::Value::as_str) != Some("control_response") {
            return None;
        }
        let response = value.get("response")?;
        let response_request_id = response
            .get("request_id")
            .and_then(serde_json::Value::as_str)?;
        if response_request_id != request_id {
            return None;
        }
        match response.get("subtype").and_then(serde_json::Value::as_str) {
            Some("success") => Some(Ok(())),
            Some("error") => {
                let detail = response
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("the engine rejected the request");
                Some(Err(HarnessError::PermissionModeSwitchFailed(detail.into())))
            }
            _ => Some(Err(HarnessError::PermissionModeSwitchFailed(
                "the engine returned a malformed acknowledgement".into(),
            ))),
        }
    }

    /// Read until the engine confirms this exact mode request.
    ///
    /// Permission-mode changes only reach the worker between turns, so this
    /// method is the sole stdout reader while it waits. Any unrelated frames
    /// still pass through the normal parser instead of disappearing.
    async fn wait_for_permission_mode_acknowledgement(
        &self,
        channel: &EngineChannel,
        request_id: &str,
    ) -> Result<(), HarnessError> {
        let mut guard = channel.reader.lock().await;
        let reader = &mut *guard;
        let budget = StreamBudget::default();
        let mut chunk = vec![0_u8; budget.chunk_size];
        let deadline = Instant::now() + CONTROL_RESPONSE_TIMEOUT;

        loop {
            let read = timeout_at(deadline, reader.stdout.read(&mut chunk)).await;
            let count = match read {
                Ok(Ok(0)) => {
                    return Err(HarnessError::PermissionModeSwitchFailed(
                        "the engine exited before acknowledging the request".into(),
                    ));
                }
                Ok(Ok(count)) => count,
                Ok(Err(error)) => {
                    return Err(HarnessError::PermissionModeSwitchFailed(format!(
                        "could not read the engine acknowledgement: {error}"
                    )));
                }
                Err(_) => {
                    return Err(HarnessError::PermissionModeSwitchFailed(
                        "timed out waiting for the engine acknowledgement".into(),
                    ));
                }
            };

            let tick = reader.lines.push(&chunk[..count], budget);
            if tick.overflow_chunks > 0 {
                warn!(
                    overflow_chunks = tick.overflow_chunks,
                    "engine stdout exceeded the parse budget while awaiting a mode change"
                );
            }
            let mut acknowledgement = None;
            for line in tick.lines {
                if let Some(result) = Self::permission_mode_acknowledgement(&line, request_id) {
                    acknowledgement.get_or_insert(result);
                    continue;
                }
                emit_parsed(self, &mut reader.parser, &self.resume_ref, &line).await;
            }
            if let Some(result) = acknowledgement {
                return result;
            }
            tokio::task::yield_now().await;
        }
    }

    fn register_interrupt(
        &self,
        request_id: String,
    ) -> oneshot::Receiver<Result<(), HarnessError>> {
        let (reply, receiver) = oneshot::channel();
        *self.pending_interrupt.lock().expect("claude interrupt") = Some(PendingClaudeInterrupt {
            request_id,
            reply: Some(reply),
        });
        receiver
    }

    fn cancel_interrupt(&self, request_id: &str, detail: &str) {
        let pending = {
            let mut slot = self.pending_interrupt.lock().expect("claude interrupt");
            if slot
                .as_ref()
                .is_none_or(|pending| pending.request_id != request_id)
            {
                return;
            }
            slot.take()
        };
        if let Some(mut pending) = pending {
            if let Some(reply) = pending.reply.take() {
                let _ = reply.send(Err(HarnessError::Other(detail.into())));
            }
        }
    }

    fn fail_pending_interrupt(&self, detail: &str) {
        let request_id = self
            .pending_interrupt
            .lock()
            .expect("claude interrupt")
            .as_ref()
            .map(|pending| pending.request_id.clone());
        if let Some(request_id) = request_id {
            self.cancel_interrupt(&request_id, detail);
        }
    }

    fn observe_control_response(&self, line: &str) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return;
        };
        if value.get("type").and_then(serde_json::Value::as_str) != Some("control_response") {
            return;
        }
        let Some(request_id) = value
            .pointer("/response/request_id")
            .and_then(serde_json::Value::as_str)
        else {
            return;
        };
        let pending = {
            let mut slot = self.pending_interrupt.lock().expect("claude interrupt");
            if slot
                .as_ref()
                .is_none_or(|pending| pending.request_id != request_id)
            {
                return;
            }
            slot.take()
        };
        let Some(mut pending) = pending else {
            return;
        };
        let result = match value
            .pointer("/response/subtype")
            .and_then(serde_json::Value::as_str)
        {
            Some("success") => Ok(()),
            Some("error") => {
                let detail = value
                    .pointer("/response/error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("the engine rejected the interrupt");
                Err(HarnessError::Other(detail.into()))
            }
            _ => Err(HarnessError::Other(
                "the engine returned a malformed interrupt response".into(),
            )),
        };
        if let Some(reply) = pending.reply.take() {
            let _ = reply.send(result);
        }
    }

    fn resolve_interrupt_for_terminal(&self, interrupted: bool) {
        let pending = self
            .pending_interrupt
            .lock()
            .expect("claude interrupt")
            .take();
        if let Some(mut pending) = pending {
            if let Some(reply) = pending.reply.take() {
                let result = if interrupted {
                    Ok(())
                } else {
                    Err(HarnessError::Other(
                        "the turn ended before the interrupt was acknowledged".into(),
                    ))
                };
                let _ = reply.send(result);
            }
        }
    }

    async fn interrupt_process_tree(&self) -> Result<(), HarnessError> {
        self.fail_pending_interrupt("the native interrupt did not complete");
        let taken = self.channel.lock().await.take();
        self.pid.clear();
        if let Some(channel) = taken {
            channel
                .interrupt_tree(INTERRUPT_GRACE)
                .await
                .map_err(|err| {
                    HarnessError::Other(format!("the process-tree interrupt failed: {err}"))
                })?;
        }
        Ok(())
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
                            saw_terminal |=
                                emit_parsed(self, &mut reader.parser, &self.resume_ref, &line)
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
            saw_terminal |= emit_parsed(self, &mut reader.parser, &self.resume_ref, &pending).await;
        }
        if eof {
            self.fail_pending_interrupt(
                "engine stdout closed before the interrupt was acknowledged",
            );
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

    async fn run_turn_inner(&self, input: TurnInput) -> Result<TurnOutcome, HarnessError> {
        self.fail_pending_interrupt("the prior turn ended before the interrupt was acknowledged");
        self.interrupts_this_turn.store(0, Ordering::SeqCst);
        self.turn_in_flight.store(true, Ordering::SeqCst);
        let _in_flight = TurnGuard(&self.turn_in_flight);
        let prompt = encode_turn_stdin(&input);
        let mut retried = false;
        let channel = loop {
            let (channel, fresh) = self
                .ensure_channel(input.model.as_deref(), input.reasoning_effort)
                .await?;
            match self.write_line(&channel, &prompt).await {
                Ok(()) => break channel,
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
                self.retire_channel().await;
                return Err(err);
            }
        };

        if !read.eof {
            let stderr = channel.take_stderr();
            if !stderr.is_empty() {
                warn!(bytes = stderr.len(), "engine stderr (capped)");
            }
            return Ok(turn_outcome(None, read.saw_terminal, &stderr));
        }

        let status = channel.exit_status().await;
        let stderr = channel.take_final_stderr().await;
        if !stderr.is_empty() {
            warn!(bytes = stderr.len(), "engine stderr (capped)");
        }
        self.retire_channel().await;
        Ok(turn_outcome(status, read.saw_terminal, &stderr))
    }
}

#[async_trait]
impl HarnessSession for ClaudeSession {
    async fn run_turn(&self, input: TurnInput) -> Result<TurnOutcome, HarnessError> {
        let guard = self.plan_write_guard()?;
        let outcome = self.run_turn_inner(input).await;
        if let Some(path) = guard
            .as_ref()
            .map(PlanWriteGuard::changed_path)
            .transpose()?
            .flatten()
        {
            self.retire_channel().await;
            return Err(HarnessError::PlanWriteOutsideWorktree(
                path.to_string_lossy().into_owned(),
            ));
        }
        outcome
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
        channel.completer.complete(&approval, decision).await
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
        if !self.turn_in_flight.load(Ordering::SeqCst) {
            return Ok(());
        }
        let asked = self.interrupts_this_turn.fetch_add(1, Ordering::SeqCst);
        if asked > 0 {
            return self.interrupt_process_tree().await;
        }

        let request_id = format!(
            "tb-interrupt-{}",
            self.next_control_id.fetch_add(1, Ordering::SeqCst)
        );
        let receiver = self.register_interrupt(request_id.clone());
        let mut line = serde_json::to_vec(&serde_json::json!({
            "type": "control_request",
            "request_id": request_id,
            "request": { "subtype": "interrupt" },
        }))
        .map_err(|err| HarnessError::Other(format!("encode interrupt: {err}")))?;
        line.push(b'\n');
        if let Err(err) = self.write_line(&channel, &line).await {
            self.cancel_interrupt(&request_id, "the engine refused the interrupt request");
            warn!(%err, "engine child refused a stop request; stopping the process");
            return self.interrupt_process_tree().await;
        }

        match timeout(CONTROL_RESPONSE_TIMEOUT, receiver).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(err))) => {
                warn!(%err, "engine rejected a stop request; stopping the process");
                self.interrupt_process_tree().await
            }
            Ok(Err(_)) => self.interrupt_process_tree().await,
            Err(_) => {
                self.cancel_interrupt(
                    &request_id,
                    "timed out waiting for the engine to acknowledge the interrupt",
                );
                self.interrupt_process_tree().await
            }
        }
    }

    /// Re-posture a live child with the `set_permission_mode` control request.
    ///
    /// Cheap where it works: the child keeps its context, so the next turn
    /// starts as fast as any other. It does not work for `Allow` — see
    /// [`live_mode_token`] — and with no child up there is nothing to tell, so
    /// the next launch composes the recorded mode.
    async fn set_permission_mode(&self, mode: PermissionMode) -> Result<(), HarnessError> {
        let current = self.permission_mode();
        if current == mode {
            return Ok(());
        }
        let (Some(token), Some(_)) = (live_mode_token(mode), live_mode_token(current)) else {
            return Err(HarnessError::PermissionModeSwitchUnsupported);
        };
        // No child means no argv to disagree with: recording the mode is the
        // whole switch, and the next spawn composes it.
        let Some(channel) = self.channel.lock().await.clone() else {
            *self.permission_mode.lock().expect("claude permission mode") = mode;
            return Ok(());
        };
        let request_id = format!(
            "tb-set-mode-{}",
            self.next_control_id.fetch_add(1, Ordering::SeqCst)
        );
        let mut line = serde_json::to_vec(&serde_json::json!({
            "type": "control_request",
            "request_id": request_id,
            "request": { "subtype": "set_permission_mode", "mode": token },
        }))
        .map_err(|err| HarnessError::Other(format!("encode set_permission_mode: {err}")))?;
        line.push(b'\n');
        if let Err(error) = self.write_line(&channel, &line).await {
            self.retire_channel().await;
            return Err(HarnessError::PermissionModeSwitchFailed(format!(
                "could not write the engine request: {error}"
            )));
        }
        if let Err(error) = self
            .wait_for_permission_mode_acknowledgement(&channel, &request_id)
            .await
        {
            // A lost or malformed acknowledgement cannot prove whether the
            // engine applied the request. Retire the child so the next turn
            // launches under the prior mode that Tidebreak still reports.
            self.retire_channel().await;
            return Err(error);
        }
        *self.permission_mode.lock().expect("claude permission mode") = mode;
        // The engine confirmed the request, so it is no longer running the
        // mode its argv named. Without this the next turn would read the
        // disagreement as a stale child and respawn the one thing the switch
        // just avoided.
        *channel.mode.lock().expect("claude child mode") = mode;
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

    /// Release the idle child (decision 0064). The next turn takes the
    /// respawn path [`Self::ensure_channel`] already owns, so it resumes the
    /// engine session exactly like a dead-child replacement.
    async fn park(&self) -> Result<(), HarnessError> {
        self.retire_channel().await;
        Ok(())
    }

    async fn shutdown(self: Box<Self>) -> Result<(), HarnessError> {
        let taken = self.channel.lock().await.take();
        if let Some(channel) = taken {
            channel.stop(None).await;
        }
        let _ = std::fs::remove_dir_all(&self.plans_directory);
        Ok(())
    }
}

/// Emits one line's events, reporting whether any of them ended the turn.
async fn emit_parsed(
    session: &ClaudeSession,
    parser: &mut ClaudeStreamParser,
    resume_ref: &Mutex<Option<String>>,
    line: &str,
) -> bool {
    session.observe_control_response(line);
    let mut terminal = false;
    let mut interrupted = false;
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
        interrupted |= matches!(event, HarnessEvent::TurnInterrupted);
        session.spec.sink.emit(event).await;
    }
    if terminal {
        session.resolve_interrupt_for_terminal(interrupted);
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
    let text = turn_text(input);
    let mut content = Vec::new();
    if !text.is_empty() || input.images.is_empty() {
        content.push(serde_json::json!({
            "type": "text",
            "text": text,
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
            reasoning_effort: None,
            fast_mode: false,
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
            reasoning_effort: None,
            fast_mode: false,
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
            _approval: &crate::HarnessApprovalRef,
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
        session_with_mode(binary, worktree, sink, PermissionMode::Plan)
    }

    fn session_with_mode(
        binary: PathBuf,
        worktree: &Path,
        sink: Arc<dyn HarnessEventSink>,
        permission_mode: PermissionMode,
    ) -> ClaudeSession {
        ClaudeSession::new(SessionSpec {
            worktree: worktree.to_path_buf(),
            allowed_read_roots: Vec::new(),
            permission_mode,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
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
            reasoning_effort: None,
            fast_mode: false,
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

    async fn run_interrupt_case(
        mode: &str,
    ) -> (
        Result<TurnOutcome, HarnessError>,
        Result<(), HarnessError>,
        bool,
        bool,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let binary = write_engine(
            dir.path(),
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *control_request*)
      printf '%s\n' "$line" >>"$FAKE_CLAUDE_INTERRUPTS"
      case "$FAKE_CLAUDE_INTERRUPT_MODE" in
        wrong_id)
          printf '{"type":"control_response","response":{"subtype":"success","request_id":"wrong-id","response":{}}}\n'
          ;;
        error)
          printf '{"type":"control_response","response":{"subtype":"error","request_id":"tb-interrupt-1","error":"turn is no longer active"}}\n'
          ;;
        none)
          :
          ;;
        exit)
          exit 0
          ;;
      esac
      ;;
    *)
      printf '{"type":"system","subtype":"init","session_id":"sess-1","claude_code_version":"2.1.238"}\n'
      touch "$FAKE_CLAUDE_STARTED"
      ;;
  esac
done
"#,
        );
        let mut session = session_with(binary, dir.path(), Arc::new(Discard));
        session.spec.extra_env.extend([
            ("FAKE_CLAUDE_INTERRUPT_MODE".into(), mode.to_owned()),
            (
                "FAKE_CLAUDE_INTERRUPTS".into(),
                dir.path()
                    .join("interrupts.ndjson")
                    .to_string_lossy()
                    .into_owned(),
            ),
            (
                "FAKE_CLAUDE_STARTED".into(),
                dir.path().join("started").to_string_lossy().into_owned(),
            ),
        ]);
        let session = Arc::new(session);
        let running = tokio::spawn({
            let session = Arc::clone(&session);
            async move { session.run_turn(turn("keep working")).await }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !dir.path().join("started").exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fake turn did not start");

        let stopped = session.interrupt().await;
        let outcome = tokio::time::timeout(Duration::from_secs(2), running)
            .await
            .expect("fake turn did not finish")
            .expect("turn task panicked");
        let child_alive = session.child_pid().is_some();
        let request_written = std::fs::read_to_string(dir.path().join("interrupts.ndjson"))
            .is_ok_and(|input| input.lines().count() == 1);
        session.park().await.unwrap();
        (outcome, stopped, child_alive, request_written)
    }

    /// The engine takes `low..max` on `--effort`. `Ultra` is ultracode, which
    /// it spells as `xhigh` plus the keyword — there is no flag.
    #[test]
    fn effort_flags_map_the_ladder_and_spell_ultra_as_xhigh() {
        let flag = |level: Option<ReasoningEffort>| effort_flags(level).join(" ");
        assert_eq!(flag(None), "");
        assert_eq!(flag(Some(ReasoningEffort::Low)), "--effort low");
        assert_eq!(flag(Some(ReasoningEffort::XHigh)), "--effort xhigh");
        assert_eq!(flag(Some(ReasoningEffort::Max)), "--effort max");
        assert_eq!(flag(Some(ReasoningEffort::Ultra)), "--effort xhigh");
        // `none` is an OpenAI rung the engine has no equivalent for, so it
        // degrades to the lowest level `--effort` does take.
        assert_eq!(flag(Some(ReasoningEffort::None)), "--effort low");
    }

    #[test]
    fn ultra_appends_the_ultracode_keyword_and_nothing_else_does() {
        let with = |text: &str, level: Option<ReasoningEffort>| {
            turn_text(&TurnInput {
                text: text.into(),
                model: None,
                reasoning_effort: level,
                fast_mode: false,
                images: Vec::new(),
            })
        };
        assert_eq!(with("fix the bug", None), "fix the bug");
        assert_eq!(
            with("fix the bug", Some(ReasoningEffort::Max)),
            "fix the bug"
        );
        assert_eq!(
            with("fix the bug", Some(ReasoningEffort::Ultra)),
            "fix the bug\n\nultracode"
        );
        // Already asked for by name: do not say it twice.
        assert_eq!(
            with("ultracode this", Some(ReasoningEffort::Ultra)),
            "ultracode this"
        );
        // An image-only turn still carries the keyword.
        assert_eq!(with("", Some(ReasoningEffort::Ultra)), "ultracode");
    }

    #[test]
    fn effort_rides_argv_and_ultra_composes_xhigh() {
        let dir = tempfile::tempdir().unwrap();
        let session = ClaudeSession::new(SessionSpec {
            worktree: dir.path().to_path_buf(),
            allowed_read_roots: Vec::new(),
            permission_mode: PermissionMode::Ask,
            model: None,
            reasoning_effort: Some(ReasoningEffort::Ultra),
            fast_mode: false,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            env: Vec::new(),
            approval: None,
            binary: PathBuf::from("/usr/bin/claude"),
            sink: Arc::new(Discard),
            browser: None,
        });
        let plan = session.compose_plan_for(None, None).unwrap();
        let index = plan.argv.iter().position(|arg| arg == "--effort").unwrap();
        assert_eq!(plan.argv[index + 1], "xhigh");
        // A turn-level level wins over the session's.
        let plan = session
            .compose_plan_for(None, Some(ReasoningEffort::Low))
            .unwrap();
        let index = plan.argv.iter().position(|arg| arg == "--effort").unwrap();
        assert_eq!(plan.argv[index + 1], "low");
    }

    #[test]
    fn tidebreak_settings_redirect_plans_and_merge_fast_mode() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = session_with_mode(
            PathBuf::from("/usr/bin/claude"),
            dir.path(),
            Arc::new(Discard),
            PermissionMode::Plan,
        );
        session.spec.model = Some("claude-opus-5".into());
        session.spec.fast_mode = true;

        let plan = session.compose_plan_for(None, None).unwrap();
        let settings_indexes = plan
            .argv
            .iter()
            .enumerate()
            .filter_map(|(index, arg)| (arg == "--settings").then_some(index))
            .collect::<Vec<_>>();
        assert_eq!(settings_indexes.len(), 1);
        let settings: serde_json::Value =
            serde_json::from_str(&plan.argv[settings_indexes[0] + 1]).unwrap();
        assert_eq!(settings["fastMode"], true);
        assert_eq!(
            settings["plansDirectory"],
            session.plans_directory.to_string_lossy().as_ref()
        );
        assert!(!session.plans_directory.starts_with(dir.path()));
    }

    /// The control request reaches `plan`, `manual`, and `acceptEdits`.
    /// `Allow` is the bypass flag, which only a fresh child can carry.
    #[test]
    fn a_live_switch_covers_every_mode_except_the_bypass() {
        assert_eq!(live_mode_token(PermissionMode::Plan), Some("plan"));
        assert_eq!(live_mode_token(PermissionMode::Ask), Some("manual"));
        assert_eq!(live_mode_token(PermissionMode::Auto), Some("acceptEdits"));
        assert_eq!(live_mode_token(PermissionMode::Allow), None);
    }

    #[test]
    fn allowed_read_roots_precede_extra_argv_in_every_permission_mode() {
        let dir = tempfile::tempdir().unwrap();
        let roots = [dir.path().join("forks"), dir.path().join("attachments")];

        for mode in [
            PermissionMode::Plan,
            PermissionMode::Ask,
            PermissionMode::Auto,
            PermissionMode::Allow,
        ] {
            let mut session = session_with_mode(
                PathBuf::from("/usr/bin/claude"),
                dir.path(),
                Arc::new(Discard),
                mode,
            );
            session.spec.allowed_read_roots = roots.to_vec();
            session.spec.extra_argv = vec!["--append-system-prompt".into(), "extra".into()];

            let plan = session.compose_plan_for(None, None).unwrap();
            let add_dir_indexes: Vec<usize> = plan
                .argv
                .iter()
                .enumerate()
                .filter_map(|(index, arg)| (arg == "--add-dir").then_some(index))
                .collect();
            let extra_index = plan
                .argv
                .iter()
                .position(|arg| arg == "--append-system-prompt")
                .unwrap();

            assert_eq!(add_dir_indexes.len(), roots.len(), "mode: {mode:?}");
            for (index, root) in add_dir_indexes.into_iter().zip(&roots) {
                assert_eq!(plan.argv[index + 1], root.to_string_lossy());
                assert!(index < extra_index, "mode: {mode:?}");
            }
        }
    }

    #[test]
    fn allowed_read_roots_must_be_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = session_with(
            PathBuf::from("/usr/bin/claude"),
            dir.path(),
            Arc::new(Discard),
        );
        session.spec.allowed_read_roots = vec![PathBuf::from("relative/private")];

        let err = session.compose_plan_for(None, None).unwrap_err();
        assert!(matches!(
            err,
            HarnessError::AllowedReadRootNotAbsolute(root) if root == "relative/private"
        ));
    }

    #[test]
    fn extra_argv_bypass_policy_still_tracks_permission_mode() {
        let dir = tempfile::tempdir().unwrap();

        for mode in [
            PermissionMode::Plan,
            PermissionMode::Ask,
            PermissionMode::Auto,
        ] {
            let mut session = session_with_mode(
                PathBuf::from("/usr/bin/claude"),
                dir.path(),
                Arc::new(Discard),
                mode,
            );
            session.spec.extra_argv = vec!["--dangerously-skip-permissions".into()];

            assert!(matches!(
                session.compose_plan_for(None, None),
                Err(HarnessError::LaunchRejected(_))
            ));
        }

        let mut allow = session_with_mode(
            PathBuf::from("/usr/bin/claude"),
            dir.path(),
            Arc::new(Discard),
            PermissionMode::Allow,
        );
        allow.spec.extra_argv = vec!["--dangerously-skip-permissions".into()];
        allow.compose_plan_for(None, None).unwrap();
    }

    /// With no child up there is nothing to tell, so the mode is recorded and
    /// the next launch composes it. Moving to `Allow` is refused whatever the
    /// child's state, because its flags are decided at launch.
    #[tokio::test]
    async fn a_switch_without_a_child_is_recorded_for_the_next_launch() {
        let dir = tempfile::tempdir().unwrap();
        let session = session_with(
            PathBuf::from("/usr/bin/claude"),
            dir.path(),
            Arc::new(Discard),
        );
        session
            .set_permission_mode(PermissionMode::Auto)
            .await
            .unwrap();
        let plan = session.compose_plan_for(None, None).unwrap();
        let index = plan
            .argv
            .iter()
            .position(|arg| arg == "--permission-mode")
            .unwrap();
        assert_eq!(plan.argv[index + 1], "acceptEdits");

        assert!(matches!(
            session.set_permission_mode(PermissionMode::Allow).await,
            Err(HarnessError::PermissionModeSwitchUnsupported)
        ));
        // Refused means unchanged: the session must not be left claiming a
        // posture its argv would not compose.
        assert_eq!(session.permission_mode(), PermissionMode::Auto);
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
            allowed_read_roots: Vec::new(),
            permission_mode: PermissionMode::Plan,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            env: Vec::new(),
            approval: Some(approval),
            binary: dir.path().join("claude"),
            sink: Arc::new(Discard),
            browser: Some(browser),
        });
        let plan = session.compose_plan_for(None, None).unwrap();
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
            allowed_read_roots: Vec::new(),
            permission_mode: PermissionMode::Plan,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            env: Vec::new(),
            approval: None,
            binary: dir.path().join("claude"),
            sink: Arc::new(Discard),
            browser: None,
        });
        let plan = session.compose_plan_for(None, None).unwrap();
        let index = plan
            .argv
            .iter()
            .position(|arg| arg == "--input-format")
            .expect("a session-long child reads stream-json input on every turn");
        assert_eq!(plan.argv[index + 1], "stream-json");
    }

    /// The point of the control request: a mode switch between turns keeps the
    /// child, so the next turn does not pay for a respawn and a resume.
    #[tokio::test]
    async fn a_live_mode_switch_keeps_the_child_and_the_next_turn_lands_on_it() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox.ndjson");
        let binary = write_engine(
            dir.path(),
            &format!(
                r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> {inbox}
  case "$line" in
    *control_request*)
      printf '{{"type":"control_response","response":{{"subtype":"success","request_id":"wrong-id","response":{{}}}}}}\n'
      printf '{{"type":"control_response","response":{{"subtype":"success","request_id":"tb-set-mode-1","response":{{}}}}}}\n'
      continue
      ;;
  esac
  printf '{{"type":"system","subtype":"init","session_id":"sess-1","claude_code_version":"2.1.238"}}\n'
  printf '{{"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed","session_id":"sess-1","usage":{{"input_tokens":1,"output_tokens":1}}}}\n'
done
"#,
                inbox = inbox.display()
            ),
        );
        let session = session_with(binary, dir.path(), Arc::new(Discard));

        session.run_turn(turn("first")).await.unwrap();
        let pid = session.child_pid().expect("the child outlives its turn");

        session
            .set_permission_mode(PermissionMode::Auto)
            .await
            .unwrap();
        session.run_turn(turn("second")).await.unwrap();

        assert_eq!(
            session.child_pid(),
            Some(pid),
            "the switch must not respawn the child"
        );
        let sent = read_lines(&inbox);
        let modes: Vec<serde_json::Value> = sent
            .iter()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|value| value["type"] == "control_request")
            .collect();
        assert_eq!(modes.len(), 1, "one switch, one control request: {sent:?}");
        assert_eq!(modes[0]["request"]["subtype"], "set_permission_mode");
        assert_eq!(modes[0]["request"]["mode"], "acceptEdits");
    }

    /// A write is not acceptance. Every failed acknowledgement path keeps
    /// the old mode and retires the child, so the next launch cannot inherit
    /// an unconfirmed posture.
    #[tokio::test]
    async fn a_live_mode_switch_keeps_the_prior_mode_without_a_positive_acknowledgement() {
        for (behavior, expected) in [
            ("error", "permission changes are locked"),
            ("malformed", "malformed acknowledgement"),
            ("none", "timed out"),
            ("exit", "exited before acknowledging"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let binary = write_engine(
                dir.path(),
                r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *control_request*)
      case "$FAKE_CLAUDE_MODE_ACK" in
        error)
          printf '{"type":"control_response","response":{"subtype":"error","request_id":"tb-set-mode-1","error":"permission changes are locked"}}\n'
          ;;
        malformed)
          printf '{"type":"control_response","response":{"request_id":"tb-set-mode-1"}}\n'
          ;;
        none)
          :
          ;;
        exit)
          exit 0
          ;;
      esac
      ;;
    *)
      printf '{"type":"system","subtype":"init","session_id":"sess-1","claude_code_version":"2.1.238"}\n'
      printf '{"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed","session_id":"sess-1","usage":{"input_tokens":1,"output_tokens":1}}\n'
      ;;
  esac
done
"#,
            );
            let mut session = session_with(binary, dir.path(), Arc::new(Discard));
            session
                .spec
                .extra_env
                .push(("FAKE_CLAUDE_MODE_ACK".into(), behavior.into()));

            session.run_turn(turn("first")).await.unwrap();
            let result = session.set_permission_mode(PermissionMode::Auto).await;
            let Err(HarnessError::PermissionModeSwitchFailed(detail)) = result else {
                panic!("{behavior} must return a typed mode-switch failure: {result:?}");
            };
            assert!(detail.contains(expected), "{behavior}: {detail}");
            assert_eq!(
                session.permission_mode(),
                PermissionMode::Plan,
                "{behavior} must keep the prior session mode"
            );
            assert_eq!(
                session.child_pid(),
                None,
                "{behavior} must retire an ambiguous child"
            );
            let plan = session.compose_plan_for(None, None).unwrap();
            let mode = plan
                .argv
                .windows(2)
                .find(|pair| pair[0] == "--permission-mode")
                .map(|pair| pair[1].as_str());
            assert_eq!(
                mode,
                Some("plan"),
                "{behavior} must compose the next child under the prior mode"
            );
        }
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

    #[tokio::test]
    async fn plan_mode_fails_if_the_engine_writes_to_the_default_home_directory() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let worktree = dir.path().join("worktree");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        let binary = write_engine(
            dir.path(),
            r#"#!/bin/sh
while IFS= read -r line; do
  mkdir -p "$HOME/.claude/plans"
  printf 'escaped\n' > "$HOME/.claude/plans/escaped.md"
  printf '{"type":"system","subtype":"init","session_id":"sess-plan","claude_code_version":"2.1.245"}\n'
  printf '{"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed","session_id":"sess-plan","usage":{"input_tokens":1,"output_tokens":1}}\n'
done
"#,
        );
        let mut session = session_with(binary, &worktree, Arc::new(Discard));
        session.spec.env = vec![("HOME".into(), home.as_os_str().to_owned())];

        let error = session.run_turn(turn("plan this")).await.unwrap_err();
        assert!(matches!(
            error,
            HarnessError::PlanWriteOutsideWorktree(path)
                if path.ends_with(".claude/plans/escaped.md")
        ));
        assert_eq!(session.child_pid(), None, "the violating child is retired");
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

    #[tokio::test]
    async fn a_wrong_claude_interrupt_id_times_out_and_stops_the_process_tree() {
        let (outcome, stopped, child_alive, request_written) = run_interrupt_case("wrong_id").await;
        stopped.unwrap();
        assert!(!matches!(outcome.unwrap(), TurnOutcome::Clean));
        assert!(!child_alive);
        assert!(request_written);
    }

    #[tokio::test]
    async fn a_claude_interrupt_error_stops_the_process_tree() {
        let (outcome, stopped, child_alive, request_written) = run_interrupt_case("error").await;
        stopped.unwrap();
        assert!(!matches!(outcome.unwrap(), TurnOutcome::Clean));
        assert!(!child_alive);
        assert!(request_written);
    }

    #[tokio::test]
    async fn a_missing_claude_interrupt_response_stops_the_process_tree() {
        let (outcome, stopped, child_alive, request_written) = run_interrupt_case("none").await;
        stopped.unwrap();
        assert!(!matches!(outcome.unwrap(), TurnOutcome::Clean));
        assert!(!child_alive);
        assert!(request_written);
    }

    #[tokio::test]
    async fn a_claude_child_exit_during_interrupt_runs_the_process_fallback() {
        let (outcome, stopped, child_alive, request_written) = run_interrupt_case("exit").await;
        stopped.unwrap();
        assert!(!matches!(outcome.unwrap(), TurnOutcome::Clean));
        assert!(!child_alive);
        assert!(request_written);
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

    /// Decision 0064: parking releases the idle child, and the wake turn
    /// takes the same respawn-and-resume path a dead child does.
    #[tokio::test]
    async fn a_parked_child_is_respawned_and_resumed_on_the_next_turn() {
        let dir = tempfile::tempdir().unwrap();
        let argv_log = dir.path().join("argv.log");
        let binary = write_engine(
            dir.path(),
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> {argv_log}
while IFS= read -r line; do
  printf '{{"type":"system","subtype":"init","session_id":"sess-9","claude_code_version":"2.1.238"}}\n'
  printf '{{"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed","session_id":"sess-9","usage":{{"input_tokens":1,"output_tokens":1}}}}\n'
done
"#,
                argv_log = argv_log.display()
            ),
        );
        let session = session_with(binary, dir.path(), Arc::new(Discard));

        assert!(matches!(
            session.run_turn(turn("one")).await.unwrap(),
            TurnOutcome::Clean
        ));
        let parked_pid = session.child_pid().expect("the child outlives its turn");

        session.park().await.unwrap();
        assert_eq!(session.child_pid(), None, "the parked child is gone");

        assert!(matches!(
            session.run_turn(turn("two")).await.unwrap(),
            TurnOutcome::Clean
        ));
        let woken_pid = session.child_pid().expect("the wake turn spawned a child");
        assert_ne!(parked_pid, woken_pid, "a new process answered the wake");
        let launches = read_lines(&argv_log);
        assert_eq!(launches.len(), 2, "the wake respawns: {launches:?}");
        assert!(
            launches[1].contains("--resume sess-9"),
            "the replacement resumes the session: {}",
            launches[1]
        );
    }
}
