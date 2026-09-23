//! One print-mode child per session.
//!
//! The child runs `--input-format stream-json` with its stdin held open, so a
//! turn is one user line written into a process that is already warm. The
//! turn's own `result` line ends it; the child stays up for the next one.
//! Record 57 has the measurements that forced this. A task reads the child's
//! stdout for the child's whole life, because the engine also writes between
//! turns: see [`ReaderShared`].

use std::collections::{BTreeMap, BTreeSet, HashMap};
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
use tokio::time::timeout;
use tracing::warn;

use crate::child::{turn_outcome, ChildPid};
use crate::claude::parse::{ClaudeStreamParser, TurnMark};
use crate::launch::{validate_launch_plan_with, BypassPolicy, LaunchPlan};
use crate::{
    spawn_process_tree, ApprovalDecision, BrowserChannelSpec, HarnessApprovalRef, HarnessError,
    HarnessEvent, HarnessEventSink, HarnessSession, ProcessTreeChild, ProjectConfig, SessionSpec,
    StreamBudget, StreamLine, StreamLineBuffer, TurnInput, TurnOutcome,
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

/// Claude Code's switches for a repository the user has not trusted.
///
/// Print mode skips the engine's own workspace-trust prompt, so without these
/// a cloned repository's config runs as the user when the child starts.
/// Checked against 2.1.259:
///
/// - `--setting-sources user` leaves out the project and local sources: both
///   `.claude/settings*.json` files with their hooks, environment,
///   permissions, helper commands, and plugins; `.mcp.json`; `CLAUDE.md` and
///   the rules files; and the project's agents, commands, skills, output
///   styles, and workflows. `--settings` still applies.
/// - `--strict-mcp-config` keeps the MCP servers to the ones Tidebreak names
///   in `--mcp-config`.
#[must_use]
pub(crate) fn project_config_flags(project_config: ProjectConfig) -> Vec<String> {
    match project_config {
        ProjectConfig::Skip => vec![
            "--setting-sources".into(),
            "user".into(),
            "--strict-mcp-config".into(),
        ],
        ProjectConfig::Load => Vec::new(),
    }
}

/// The headless scheduler reads `.claude/scheduled_tasks.json` whatever the
/// setting sources say, so a launch in an untrusted repository turns it off.
const DISABLE_CRON_ENV: &str = "CLAUDE_CODE_DISABLE_CRON";

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
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The file in the session's private directory that holds the MCP config.
const MCP_CONFIG_FILE: &str = "mcp-config.json";

/// Replace `path` with `contents`, readable and writable by this user only.
///
/// The bytes land in a fresh owner-only file beside `path` and are renamed
/// over it, so a child that starts while a respawn rewrites the file never
/// reads a half-written document.
fn write_private_file(path: &Path, contents: &str) -> io::Result<()> {
    let directory = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "a private file needs a parent directory",
        )
    })?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(".mcp-config-").suffix(".json");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        builder.permissions(std::fs::Permissions::from_mode(0o600));
    }
    let mut file = builder.tempfile_in(directory)?;
    io::Write::write_all(file.as_file_mut(), contents.as_bytes())?;
    file.persist(path).map_err(|error| error.error)?;
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

/// A reply to one control request. `Err` carries the engine's reason.
type ControlReply = Result<(), String>;

/// A control request waiting on its `control_response`.
struct ControlWaiter {
    reply: oneshot::Sender<ControlReply>,
    /// A stop request. The turn's own end answers it too, because the engine
    /// can end the turn before it acknowledges the stop.
    stop: bool,
}

/// The user turn a child is running, as its stdout reader sees it.
struct PendingTurn {
    /// The client `uuid` on the turn's stdin line.
    uuid: String,
    /// The engine took the line off its queue: a `command_lifecycle` line
    /// said `started`, or a reply named the uuid. From here on, the next
    /// `result` ends this turn.
    started: bool,
    done: oneshot::Sender<TurnEnd>,
}

/// How a user turn left the stream.
struct TurnEnd {
    /// The turn's own `result` arrived.
    saw_terminal: bool,
    /// The child's stdout closed, so the process is gone.
    eof: bool,
    /// Reading the child's stdout failed.
    error: Option<io::Error>,
}

#[derive(Default)]
struct ReaderState {
    /// The user turn waiting on this child.
    turn: Option<PendingTurn>,
    /// This child reports `command_lifecycle` for the lines the session
    /// sends. 2.1.259 does (captured).
    lifecycle: bool,
    /// Stdout closed or failed. Nothing more will arrive.
    closed: bool,
    /// Control requests waiting on their `control_response`, by request id.
    controls: HashMap<String, ControlWaiter>,
}

/// What a child's stdout reader shares with the session.
///
/// One task reads the child's stdout for the child's whole life, between
/// turns as well as during them. The engine keeps writing between turns:
/// when a background task ends after a turn's `result`, it reports the end
/// and runs a turn of its own, with its own `result`. Reading only inside
/// turns left those lines in the pipe, and the next user turn took that
/// `result` as its own and ended early.
struct ReaderShared {
    sink: Arc<dyn HarnessEventSink>,
    resume_ref: Arc<Mutex<Option<String>>>,
    unrecognized: Arc<AtomicU64>,
    state: Mutex<ReaderState>,
}

/// Where one line's events go.
enum Route {
    /// The user turn in flight: every event, as the engine sent it.
    Turn,
    /// The line ends the user turn in flight.
    EndsTurn(PendingTurn),
    /// Activity nobody asked for: between turns, or a turn the engine
    /// started itself.
    Background,
}

impl ReaderShared {
    fn state(&self) -> std::sync::MutexGuard<'_, ReaderState> {
        self.state.lock().expect("claude reader state")
    }

    /// Wait for the end of the user turn whose stdin line carries `uuid`.
    /// `None` when stdout has already closed.
    fn begin_turn(&self, uuid: &str) -> Option<oneshot::Receiver<TurnEnd>> {
        let mut state = self.state();
        if state.closed {
            return None;
        }
        let (done, receiver) = oneshot::channel();
        state.turn = Some(PendingTurn {
            uuid: uuid.to_owned(),
            started: false,
            done,
        });
        Some(receiver)
    }

    /// Stop waiting for a turn whose line never reached the child.
    fn abandon_turn(&self) {
        self.state().turn = None;
    }

    fn is_closed(&self) -> bool {
        self.state().closed
    }

    /// Wait for the `control_response` that answers `request_id`. `None`
    /// when stdout has already closed.
    fn await_control(
        &self,
        request_id: &str,
        stop: bool,
    ) -> Option<oneshot::Receiver<ControlReply>> {
        let mut state = self.state();
        if state.closed {
            return None;
        }
        let (reply, receiver) = oneshot::channel();
        state
            .controls
            .insert(request_id.to_owned(), ControlWaiter { reply, stop });
        Some(receiver)
    }

    fn cancel_control(&self, request_id: &str) {
        self.state().controls.remove(request_id);
    }

    /// Answer every stop request still waiting.
    fn settle_stops(&self, reply: &ControlReply) {
        let waiters: Vec<ControlWaiter> = {
            let mut state = self.state();
            let ids: Vec<String> = state
                .controls
                .iter()
                .filter(|(_, waiter)| waiter.stop)
                .map(|(id, _)| id.clone())
                .collect();
            ids.iter()
                .filter_map(|id| state.controls.remove(id))
                .collect()
        };
        for waiter in waiters {
            let _ = waiter.reply.send(reply.clone());
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
        let Some(waiter) = self.state().controls.remove(request_id) else {
            return;
        };
        let reply = match value
            .pointer("/response/subtype")
            .and_then(serde_json::Value::as_str)
        {
            Some("success") => Ok(()),
            Some("error") => Err(value
                .pointer("/response/error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("the engine rejected the request")
                .to_owned()),
            _ => Err("the engine returned a malformed acknowledgement".to_owned()),
        };
        let _ = waiter.reply.send(reply);
    }

    /// Decide where a line goes, from what it says about its turn.
    ///
    /// The user's line gets a client `uuid`, and the engine reports its fate
    /// in `command_lifecycle` lines: `queued` when the engine reads it,
    /// `started` when a turn takes it. A `result` read before `started` ends
    /// a turn that was already running when the line arrived, so it never
    /// ends the user's turn. The first `result` after `started` does, even
    /// when a turn the engine started itself took the line between two tool
    /// calls and its `result` names no uuid (captured on 2.1.259).
    ///
    /// An engine that reports no lifecycle for the line falls back to the
    /// first `result` after the write, unless that `result` names only other
    /// lines.
    fn route(&self, mark: &TurnMark) -> Route {
        let mut state = self.state();
        let ReaderState {
            turn, lifecycle, ..
        } = &mut *state;
        let Some(pending) = turn.as_mut() else {
            return Route::Background;
        };
        if let Some((uuid, command_state)) = &mark.command {
            if *uuid == pending.uuid {
                *lifecycle = true;
                pending.started |= command_state == "started";
            }
        }
        pending.started |= mark.user_messages.contains(&pending.uuid);
        if mark.ends_turn {
            let ours = pending.started || (!*lifecycle && mark.user_messages.is_empty());
            if !ours {
                return Route::Background;
            }
            return Route::EndsTurn(turn.take().expect("the pending turn"));
        }
        if pending.started || !*lifecycle {
            Route::Turn
        } else {
            Route::Background
        }
    }

    /// Stdout closed or failed: nothing more will reach anyone waiting.
    fn close(&self, error: Option<io::Error>) {
        let (turn, controls) = {
            let mut state = self.state();
            state.closed = true;
            (state.turn.take(), std::mem::take(&mut state.controls))
        };
        for waiter in controls.into_values() {
            let _ = waiter.reply.send(Err(
                "the engine exited before acknowledging the request".into()
            ));
        }
        if let Some(turn) = turn {
            let _ = turn.done.send(TurnEnd {
                saw_terminal: false,
                eof: error.is_none(),
                error,
            });
        }
    }
}

/// Read one child's stdout until it closes.
async fn read_stdout(shared: Arc<ReaderShared>, mut stdout: ChildStdout) {
    let budget = StreamBudget::default();
    let mut lines = StreamLineBuffer::new();
    let mut parser = ClaudeStreamParser::new();
    let mut flushed = 0;
    let mut chunk = vec![0_u8; budget.chunk_size];
    let mut chunks_this_tick = 0;
    let error = loop {
        match stdout.read(&mut chunk).await {
            Ok(0) => break None,
            Ok(count) => {
                let tick = lines.push(&chunk[..count], budget);
                if tick.overflow_chunks > 0 {
                    warn!(
                        overflow_chunks = tick.overflow_chunks,
                        "engine stdout exceeded the parse budget"
                    );
                }
                for line in tick.lines {
                    read_line(&shared, &mut parser, &line, false).await;
                }
                flush_unrecognized(&shared, &parser, &mut flushed);
            }
            Err(error) => break Some(error),
        }
        chunks_this_tick += 1;
        if chunks_this_tick >= budget.max_chunks_per_tick {
            chunks_this_tick = 0;
            tokio::task::yield_now().await;
        }
    };
    if error.is_none() {
        if let Some(pending) = lines.pending_line() {
            read_line(&shared, &mut parser, &pending, true).await;
            flush_unrecognized(&shared, &parser, &mut flushed);
        }
    }
    shared.close(error);
}

/// Add the parser's new unrecognized events to the session total. The parser
/// dies with its child, so the total lives on the session.
fn flush_unrecognized(shared: &ReaderShared, parser: &ClaudeStreamParser, flushed: &mut u64) {
    let total = parser.unrecognized();
    shared
        .unrecognized
        .fetch_add(total - *flushed, Ordering::SeqCst);
    *flushed = total;
}

/// Parse one line and deliver its events where [`ReaderShared::route`] says.
///
/// `last` marks the unterminated line left in the buffer when stdout closed.
async fn read_line(
    shared: &ReaderShared,
    parser: &mut ClaudeStreamParser,
    line: &StreamLine,
    last: bool,
) {
    // A cut line is not JSON and answers no control request. The parser
    // recovers what event it was from the part that arrived.
    let events = if line.cut {
        parser.push_cut_line(&line.text)
    } else {
        shared.observe_control_response(&line.text);
        parser.push_line(&line.text)
    };
    let mark = parser.take_turn_mark();
    for event in &events {
        if let HarnessEvent::SessionStarted {
            resume_ref: Some(resume),
            ..
        } = event
        {
            *shared.resume_ref.lock().expect("claude resume") = Some(resume.clone());
        }
    }
    match shared.route(&mark) {
        Route::Turn => {
            for event in events {
                shared.sink.emit(event).await;
            }
        }
        Route::Background => {
            for event in events.into_iter().filter_map(background_event) {
                shared.sink.emit(event).await;
            }
        }
        Route::EndsTurn(turn) => {
            let interrupted = events
                .iter()
                .any(|event| matches!(event, HarnessEvent::TurnInterrupted));
            for event in events {
                shared.sink.emit(event).await;
            }
            shared.settle_stops(&if interrupted {
                Ok(())
            } else {
                Err("the turn ended before the interrupt was acknowledged".into())
            });
            let _ = turn.done.send(TurnEnd {
                saw_terminal: true,
                eof: last,
                error: None,
            });
        }
    }
}

/// An event from activity nobody asked for, the way the transcript keeps it.
///
/// The engine's own turn keeps its messages, tool calls, and notices, such as
/// the notice that a background task finished. Its streaming text is left
/// out, because the message that follows repeats it. Its end is not a turn
/// end the session reports: only a failure reaches the transcript, as a
/// notice.
fn background_event(event: HarnessEvent) -> Option<HarnessEvent> {
    match event {
        HarnessEvent::AssistantDelta { .. }
        | HarnessEvent::ReasoningDelta { .. }
        | HarnessEvent::UserSteered { .. }
        | HarnessEvent::TurnStarted
        | HarnessEvent::TurnCompleted { .. }
        | HarnessEvent::TurnInterrupted => None,
        HarnessEvent::TurnFailed { error } => Some(HarnessEvent::HarnessNotice {
            level: tidebreak_core::HarnessNoticeLevel::Warning,
            message: format!(
                "Claude Code could not finish a turn it started on its own: {}",
                error.message
            )
            .chars()
            .take(tidebreak_core::MAX_NOTICE_CHARS)
            .collect(),
        }),
        other => Some(other),
    }
}

/// One live `claude` child and the handles a session needs on it.
///
/// The locks are separate on purpose: `interrupt` writes to stdin while a
/// turn waits on the reader, and either may need to stop the process.
struct EngineChannel {
    stdin: AsyncMutex<ChildStdin>,
    /// State the stdout reader shares with the session.
    reader: Arc<ReaderShared>,
    /// The task reading the child's stdout.
    reader_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
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
    /// Whether the process behind this channel is gone, or its stdout is.
    async fn has_exited(&self) -> bool {
        if self.reader.is_closed() {
            return true;
        }
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
        // Nothing waits on a retired child's output.
        self.stop_reading();
        status
    }

    fn stop_reading(&self) {
        if let Some(task) = self.reader_task.lock().expect("claude reader task").take() {
            task.abort();
        }
    }

    async fn interrupt_tree(&self, grace: Duration) -> io::Result<Option<ExitStatus>> {
        let child = self.child.lock().await.take();
        let Some(mut child) = child else {
            return Ok(None);
        };
        let status = match child.interrupt(grace).await {
            Ok(status) => status,
            Err(error) => {
                let status = child.terminate().await.ok();
                *self.reaped.lock().expect("claude child exit") = status;
                return Err(error);
            }
        };
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

impl Drop for EngineChannel {
    fn drop(&mut self) {
        self.stop_reading();
    }
}

/// Live Claude Code session: one child for the session lifetime.
pub struct ClaudeSession {
    spec: SessionSpec,
    /// Tidebreak-owned directory only this session's user can open. It holds
    /// the Plan-mode files and the MCP config, which carries bearer tokens.
    private_directory: PathBuf,
    /// Tidebreak-owned destination for Claude's Plan-mode files.
    plans_directory: PathBuf,
    /// The session's current permission mode, which a live switch moves.
    /// `spec.permission_mode` is only what it started on.
    permission_mode: Mutex<PermissionMode>,
    /// Shared with each child's stdout reader, which records the session id
    /// the engine reports.
    resume_ref: Arc<Mutex<Option<String>>>,
    channel: AsyncMutex<Option<Arc<EngineChannel>>>,
    pid: ChildPid,
    /// Unrecognized events summed across every child this session has run.
    /// The parser dies with its child, so the total lives out here.
    unrecognized: Arc<AtomicU64>,
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

/// Claude Code moves long foreground work to the background when this is
/// truthy. The pinned 2.1.259 reads it in two places: a foreground subagent
/// that runs past 120 seconds, and an MCP tool call in print mode. `0` keeps
/// both in the foreground, where the transcript can follow them.
///
/// It does not change how 2.1.259 runs an `Agent` call that leaves
/// `run_in_background` unset: that call starts in the background either way
/// (captured with the variable at `0`).
const AUTO_BACKGROUND_ENV: &str = "CLAUDE_AUTO_BACKGROUND_TASKS";

impl ClaudeSession {
    pub(super) fn new(spec: SessionSpec) -> Self {
        let resume_ref = spec.resume_ref.clone();
        let permission_mode = spec.permission_mode;
        let private_directory =
            std::env::temp_dir().join(format!("tidebreak-claude-{}", uuid::Uuid::new_v4()));
        let plans_directory = private_directory.join("plans");
        Self {
            spec,
            private_directory,
            plans_directory,
            permission_mode: Mutex::new(permission_mode),
            resume_ref: Arc::new(Mutex::new(resume_ref)),
            channel: AsyncMutex::new(None),
            pid: ChildPid::new(),
            unrecognized: Arc::new(AtomicU64::new(0)),
            interrupts_this_turn: AtomicU64::new(0),
            turn_in_flight: AtomicBool::new(false),
            next_control_id: AtomicU64::new(1),
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
            self.spec
                .binary
                .as_deref()
                .ok_or(HarnessError::NotFound)?
                .to_string_lossy()
                .into_owned(),
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
        ensure_private_directory(&self.private_directory)?;
        ensure_private_directory(&self.plans_directory)?;
        argv.extend(settings_flags(
            &self.plans_directory,
            self.spec.fast_mode,
            self.resolved_model(turn_model).as_deref(),
        )?);
        argv.extend(project_config_flags(self.spec.project_config));
        if let Some(config) = crate::claude::browser::mcp_launch_config(
            self.spec.approval.as_ref(),
            self.spec.browser.as_ref(),
            self.spec.native.as_ref(),
            self.spec.apps.as_ref(),
            self.spec.tool_bridge.as_ref(),
        )? {
            // The document carries bearer tokens, and any local account can
            // read a process's arguments. Argv names a file only this user
            // can read.
            let path = self.private_directory.join(MCP_CONFIG_FILE);
            write_private_file(&path, config.document())?;
            argv.extend(config.flags(&path)?);
        }
        if self.permission_mode() == PermissionMode::Ask
            && self.spec.tool_bridge.is_some()
            && self.spec.approval.is_none()
        {
            argv.push("--permission-prompt-tool".into());
            argv.push("mcp__tb-human__permission_prompt".into());
        }
        if let Some(resume) = self.resume_ref.lock().expect("claude resume").clone() {
            argv.push("--resume".into());
            argv.push(resume);
        }
        crate::require_absolute_read_roots(&self.spec.allowed_read_roots)?;
        for root in &self.spec.allowed_read_roots {
            argv.push("--add-dir".into());
            argv.push(root.to_string_lossy().into_owned());
        }
        argv.extend(self.spec.extra_argv.iter().cloned());
        let mut env = self.spec.extra_env.clone();
        env.retain(|(key, _)| {
            !BrowserChannelSpec::is_reserved_env_key_except(key, self.spec.relay_key_env.as_deref())
                && key != "PWD"
        });
        if self.spec.tool_bridge.is_some() {
            // Managed human tools must remain in the foreground of the native turn.
            crate::override_env(&mut env, AUTO_BACKGROUND_ENV, "0");
        } else if !env.iter().any(|(name, _)| name == AUTO_BACKGROUND_ENV) {
            // A settings overlay that names the variable is the session
            // asking for background work, so its value stands.
            env.push((AUTO_BACKGROUND_ENV.into(), "0".into()));
        }
        if self.spec.project_config == ProjectConfig::Skip {
            crate::override_env(&mut env, DISABLE_CRON_ENV, "1");
        }
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
        self.spec.apply_child_env(
            &mut command,
            tidebreak_core::HarnessKind::ClaudeCode,
            &plan.env,
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
        // A child that chatters on stderr would fill its pipe and stall.
        // Drain it for the child's whole life and keep the tail for whichever
        // turn has to explain a death.
        let stderr_task =
            tokio::spawn(async move { drain_capped(stderr, MAX_STDERR_BYTES, &sink).await });
        let reader = Arc::new(ReaderShared {
            sink: self.spec.sink.clone(),
            resume_ref: self.resume_ref.clone(),
            unrecognized: self.unrecognized.clone(),
            state: Mutex::new(ReaderState::default()),
        });
        let reader_task = tokio::spawn(read_stdout(reader.clone(), stdout));

        Ok(Arc::new(EngineChannel {
            stdin: AsyncMutex::new(stdin),
            reader,
            reader_task: Mutex::new(Some(reader_task)),
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

    /// Wait for the engine to confirm the control request `request_id`.
    ///
    /// The stdout reader answers `receiver` when the matching
    /// `control_response` arrives, and every other line on the way still
    /// reaches the parser.
    async fn wait_for_permission_mode_acknowledgement(
        receiver: oneshot::Receiver<ControlReply>,
    ) -> Result<(), HarnessError> {
        match timeout(CONTROL_RESPONSE_TIMEOUT, receiver).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(detail))) => Err(HarnessError::PermissionModeSwitchFailed(detail)),
            Ok(Err(_)) => Err(HarnessError::PermissionModeSwitchFailed(
                "the engine exited before acknowledging the request".into(),
            )),
            Err(_) => Err(HarnessError::PermissionModeSwitchFailed(
                "timed out waiting for the engine acknowledgement".into(),
            )),
        }
    }

    async fn interrupt_process_tree(&self) -> Result<(), HarnessError> {
        let taken = self.channel.lock().await.take();
        self.pid.clear();
        if let Some(channel) = taken {
            channel
                .reader
                .settle_stops(&Err("the native interrupt did not complete".into()));
            channel
                .interrupt_tree(INTERRUPT_GRACE)
                .await
                .map_err(|err| {
                    HarnessError::Other(format!("the process-tree interrupt failed: {err}"))
                })?;
        }
        Ok(())
    }

    async fn run_turn_inner(&self, input: TurnInput) -> Result<TurnOutcome, HarnessError> {
        self.interrupts_this_turn.store(0, Ordering::SeqCst);
        self.turn_in_flight.store(true, Ordering::SeqCst);
        let _in_flight = TurnGuard(&self.turn_in_flight);
        // The engine names this uuid in what it reports about the line, which
        // is how the turn's own `result` is told from any other.
        let uuid = uuid::Uuid::new_v4().to_string();
        let prompt = encode_turn_stdin(&input, &uuid);
        let mut retried = false;
        let (channel, done) = loop {
            let (channel, fresh) = self
                .ensure_channel(input.model.as_deref(), input.reasoning_effort)
                .await?;
            channel.reader.settle_stops(&Err(
                "the prior turn ended before the interrupt was acknowledged".into(),
            ));
            let written = match channel.reader.begin_turn(&uuid) {
                Some(done) => match self.write_line(&channel, &prompt).await {
                    Ok(()) => Ok(done),
                    Err(err) => {
                        channel.reader.abandon_turn();
                        Err(err)
                    }
                },
                None => Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "the engine child closed its output",
                )),
            };
            match written {
                Ok(done) => break (channel, done),
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

        // A reader that went away without a word saw its child go.
        let end = done.await.unwrap_or(TurnEnd {
            saw_terminal: false,
            eof: true,
            error: None,
        });
        if let Some(error) = end.error {
            self.retire_channel().await;
            return Err(error.into());
        }
        if !end.eof {
            let stderr = channel.take_stderr();
            if !stderr.is_empty() {
                warn!(bytes = stderr.len(), "engine stderr (capped)");
            }
            return Ok(turn_outcome(None, end.saw_terminal, &stderr));
        }

        let status = channel.exit_status().await;
        let stderr = channel.take_final_stderr().await;
        if !stderr.is_empty() {
            warn!(bytes = stderr.len(), "engine stderr (capped)");
        }
        self.retire_channel().await;
        Ok(turn_outcome(status, end.saw_terminal, &stderr))
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
        if matches!(
            decision,
            ApprovalDecision::ApproveWithGrant { .. }
                | ApprovalDecision::Answers { .. }
                | ApprovalDecision::PlanDecision { .. }
        ) {
            return Err(HarnessError::DecisionUnsupported(
                "the claude permission prompt takes allow or deny".into(),
            ));
        }
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
        let Some(receiver) = channel.reader.await_control(&request_id, true) else {
            // Stdout already closed: the turn is ending on its own.
            return self.interrupt_process_tree().await;
        };
        let mut line = serde_json::to_vec(&serde_json::json!({
            "type": "control_request",
            "request_id": request_id,
            "request": { "subtype": "interrupt" },
        }))
        .map_err(|err| HarnessError::Other(format!("encode interrupt: {err}")))?;
        line.push(b'\n');
        if let Err(err) = self.write_line(&channel, &line).await {
            channel.reader.cancel_control(&request_id);
            warn!(%err, "engine child refused a stop request; stopping the process");
            return self.interrupt_process_tree().await;
        }

        match timeout(CONTROL_RESPONSE_TIMEOUT, receiver).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(detail))) => {
                warn!(%detail, "engine rejected a stop request; stopping the process");
                self.interrupt_process_tree().await
            }
            Ok(Err(_)) => self.interrupt_process_tree().await,
            Err(_) => {
                channel.reader.cancel_control(&request_id);
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
        let acknowledged = match channel.reader.await_control(&request_id, false) {
            Some(receiver) => match self.write_line(&channel, &line).await {
                Ok(()) => Self::wait_for_permission_mode_acknowledgement(receiver).await,
                Err(error) => {
                    channel.reader.cancel_control(&request_id);
                    self.retire_channel().await;
                    return Err(HarnessError::PermissionModeSwitchFailed(format!(
                        "could not write the engine request: {error}"
                    )));
                }
            },
            None => Err(HarnessError::PermissionModeSwitchFailed(
                "the engine exited before acknowledging the request".into(),
            )),
        };
        if let Err(error) = acknowledged {
            // A lost or malformed acknowledgement cannot prove whether the
            // engine applied the request. Retire the child so the next turn
            // launches under the prior mode that Tidebreak still reports.
            channel.reader.cancel_control(&request_id);
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
        let _ = std::fs::remove_dir_all(&self.private_directory);
        Ok(())
    }
}

impl Drop for ClaudeSession {
    /// The private directory holds a config with live bearer tokens; a
    /// session dropped without [`HarnessSession::shutdown`] still removes it.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.private_directory);
    }
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
///
/// `uuid` is the client id the engine names when it reports what happened to
/// the line and on the `result` of the turn that took it.
pub(crate) fn encode_turn_stdin(input: &TurnInput, uuid: &str) -> Vec<u8> {
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
        "uuid": uuid,
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
        let encoded = encode_turn_stdin(
            &TurnInput {
                turn_id: None,
                text: "hello".into(),
                model: None,
                reasoning_effort: None,
                fast_mode: false,
                images: Vec::new(),
            },
            "turn-uuid",
        );
        let line = String::from_utf8(encoded).unwrap();
        assert!(
            line.ends_with('\n'),
            "stdin stays open, so the line must end"
        );
        let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(value["type"], "user");
        assert_eq!(value["message"]["content"][0]["text"], "hello");
        assert_eq!(value["uuid"], "turn-uuid", "the engine names this id back");
    }

    #[test]
    fn images_ride_stream_json_user_content() {
        let encoded = encode_turn_stdin(
            &TurnInput {
                turn_id: None,
                text: "look".into(),
                model: None,
                reasoning_effort: None,
                fast_mode: false,
                images: vec![TurnImage {
                    media_type: "image/png".into(),
                    bytes: b"pixels".to_vec(),
                }],
            },
            "turn-uuid",
        );
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
            owner: tidebreak_core::OwnerId::local(),
            session_id: tidebreak_core::SessionId::new(),
            worktree: worktree.to_path_buf(),
            allowed_read_roots: Vec::new(),
            permission_mode,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            relay_key_env: None,
            env: Vec::new(),
            approval: None,
            binary: Some(binary),
            sink,
            browser: None,
            native: None,
            tool_bridge: None,
            apps: None,
            project_config: crate::ProjectConfig::Load,
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
            turn_id: None,
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

    /// The MCP config a launch plan names, read from the private file that
    /// `--mcp-config` points at.
    fn read_mcp_config(path: &str) -> serde_json::Value {
        let path = Path::new(path);
        assert!(path.is_absolute(), "--mcp-config names a file: {path:?}");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
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
                turn_id: None,
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
            owner: tidebreak_core::OwnerId::local(),
            session_id: tidebreak_core::SessionId::new(),
            worktree: dir.path().to_path_buf(),
            allowed_read_roots: Vec::new(),
            permission_mode: PermissionMode::Ask,
            model: None,
            reasoning_effort: Some(ReasoningEffort::Ultra),
            fast_mode: false,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            relay_key_env: None,
            env: Vec::new(),
            approval: None,
            binary: Some(PathBuf::from("/usr/bin/claude")),
            sink: Arc::new(Discard),
            browser: None,
            native: None,
            tool_bridge: None,
            apps: None,
            project_config: crate::ProjectConfig::Load,
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
    async fn native_tool_bridge_paths_reach_shell_children_without_ambient_overrides() {
        use tidebreak_core::HarnessKind;
        let dir = tempfile::tempdir().unwrap();
        let mut session = session_with("/bin/true".into(), dir.path(), Arc::new(Discard));
        let spec = &mut session.spec;
        spec.env.extend([
            ("TIDEBREAK_TOOL_HELPER".into(), "/untrusted/helper".into()),
            ("TIDEBREAK_TOOL_SOCKET".into(), "/untrusted/socket".into()),
            ("TIDEBREAK_UNRELATED_SECRET".into(), "private".into()),
        ]);
        for kind in [
            HarnessKind::ClaudeCode,
            HarnessKind::Codex,
            HarnessKind::Opencode,
            HarnessKind::Grok,
        ] {
            let mut absent = tokio::process::Command::new("/bin/sh");
            absent.args(["-c", "test -z \"$TIDEBREAK_TOOL_HELPER$TIDEBREAK_TOOL_SOCKET$TIDEBREAK_UNRELATED_SECRET\""]);
            spec.apply_child_env(&mut absent, kind, &[]);
            assert!(absent.status().await.unwrap().success());
            spec.tool_bridge = Some(crate::ToolBridgeSpec {
                helper: "/trusted/helper".into(),
                socket: "/trusted/socket".into(),
            });
            let mut present = tokio::process::Command::new("/bin/sh");
            present.args(["-c", "test \"$TIDEBREAK_TOOL_HELPER\" = /trusted/helper && test \"$TIDEBREAK_TOOL_SOCKET\" = /trusted/socket && test -z \"$TIDEBREAK_UNRELATED_SECRET\""]);
            spec.apply_child_env(&mut present, kind, &[]);
            assert!(present.status().await.unwrap().success());
            spec.tool_bridge = None;
        }
    }

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
            owner: tidebreak_core::OwnerId::local(),
            session_id: tidebreak_core::SessionId::new(),
            worktree: dir.path().to_path_buf(),
            allowed_read_roots: Vec::new(),
            permission_mode: PermissionMode::Plan,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            relay_key_env: None,
            env: Vec::new(),
            approval: Some(approval),
            binary: Some(dir.path().join("claude")),
            sink: Arc::new(Discard),
            browser: Some(browser),
            native: None,
            tool_bridge: None,
            apps: None,
            project_config: crate::ProjectConfig::Load,
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
        let config = read_mcp_config(&plan.argv[config_index + 1]);
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

    /// Any local account can read another process's arguments. A spawned
    /// engine must find its bearer tokens in a file only its user can read,
    /// never on its command line.
    #[tokio::test]
    async fn a_spawned_engine_reads_bearer_tokens_from_a_private_file_not_its_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let argv_log = dir.path().join("argv.log");
        let seen_config = dir.path().join("seen-config.json");
        let binary = write_engine(
            dir.path(),
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$@" > {argv_log}
previous=""
for arg in "$@"; do
  if [ "$previous" = "--mcp-config" ]; then cat "$arg" > {seen_config}; fi
  previous="$arg"
done
while IFS= read -r line; do
  printf '{{"type":"system","subtype":"init","session_id":"sess-mcp","claude_code_version":"2.1.259"}}\n'
  printf '{{"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed","session_id":"sess-mcp","usage":{{"input_tokens":1,"output_tokens":1}}}}\n'
done
"#,
                argv_log = argv_log.display(),
                seen_config = seen_config.display(),
            ),
        );
        let mut session =
            session_with_mode(binary, dir.path(), Arc::new(Discard), PermissionMode::Ask);
        session.spec.approval = Some(ApprovalChannelSpec {
            mcp_endpoint_url: "http://127.0.0.1:9999/code/mcp/approval-prompt".into(),
            token: "approval-bearer-secret".into(),
            completer: Arc::new(NoopCompleter),
        });
        session.spec.apps = Some(crate::AppsChannelSpec {
            mcp_endpoint_url: "http://127.0.0.1:9999/code/mcp/connected-apps".into(),
            token: "apps-bearer-secret".into(),
        });

        assert!(matches!(
            session.run_turn(turn("hello")).await.unwrap(),
            TurnOutcome::Clean
        ));

        let argv = read_lines(&argv_log);
        let config_path = argv
            .windows(2)
            .find(|pair| pair[0] == "--mcp-config")
            .map(|pair| PathBuf::from(&pair[1]))
            .expect("the engine is still told where its MCP servers are");
        for token in ["approval-bearer-secret", "apps-bearer-secret"] {
            assert!(
                !argv.iter().any(|arg| arg.contains(token)),
                "{token} reached the engine's arguments: {argv:?}"
            );
        }
        let seen = std::fs::read_to_string(&seen_config).unwrap();
        assert!(seen.contains("Bearer approval-bearer-secret"), "{seen}");
        assert!(seen.contains("Bearer apps-bearer-secret"), "{seen}");
        assert!(config_path.starts_with(&session.private_directory));
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode(&config_path),
            0o600,
            "only the session's user reads it"
        );
        assert_eq!(mode(&session.private_directory), 0o700);

        Box::new(session).shutdown().await.unwrap();
        assert!(!config_path.exists(), "shutdown takes the tokens off disk");
    }

    /// Print mode skips the engine's own trust prompt, so an untrusted
    /// repository's settings, MCP servers, and scheduled tasks stay off only
    /// because the launch says so. A trusted one launches as it always did.
    #[test]
    fn an_untrusted_repository_launches_without_its_engine_config() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = session_with_mode(
            PathBuf::from("/usr/bin/claude"),
            dir.path(),
            Arc::new(Discard),
            PermissionMode::Auto,
        );
        session.spec.extra_env = vec![(DISABLE_CRON_ENV.into(), "0".into())];

        session.spec.project_config = ProjectConfig::Skip;
        let plan = session.compose_plan_for(None, None).unwrap();
        assert!(plan
            .argv
            .windows(2)
            .any(|pair| pair == ["--setting-sources", "user"]));
        assert!(plan.argv.iter().any(|arg| arg == "--strict-mcp-config"));
        assert_eq!(
            plan.env
                .iter()
                .filter(|(name, _)| name == DISABLE_CRON_ENV)
                .map(|(_, value)| value.as_str())
                .collect::<Vec<_>>(),
            ["1"],
            "the untrusted switch wins over the settings overlay"
        );

        session.spec.project_config = ProjectConfig::Load;
        let plan = session.compose_plan_for(None, None).unwrap();
        assert!(!plan.argv.iter().any(|arg| arg == "--setting-sources"));
        assert!(!plan.argv.iter().any(|arg| arg == "--strict-mcp-config"));
        assert_eq!(
            plan.env
                .iter()
                .filter(|(name, _)| name == DISABLE_CRON_ENV)
                .map(|(_, value)| value.as_str())
                .collect::<Vec<_>>(),
            ["0"],
            "a trusted repository keeps the settings overlay as it is"
        );
    }

    #[test]
    fn managed_ask_uses_the_native_permission_prompt_without_bypass_flags() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = session_with_mode(
            dir.path().join("claude"),
            dir.path(),
            Arc::new(Discard),
            PermissionMode::Ask,
        );
        session.spec.tool_bridge = Some(crate::ToolBridgeSpec {
            helper: PathBuf::from("/managed-helper"),
            socket: PathBuf::from("/managed.sock"),
        });
        let plan = session.compose_plan_for(None, None).unwrap();
        assert!(plan
            .argv
            .windows(2)
            .any(|pair| pair == ["--permission-mode", "manual"]));
        assert!(plan.argv.windows(2).any(|pair| pair
            == [
                "--permission-prompt-tool",
                "mcp__tb-human__permission_prompt"
            ]));
        assert!(!plan.argv.iter().any(|arg| arg.contains("skip-permissions")));
    }

    #[test]
    fn managed_human_mcp_keeps_existing_servers_and_does_not_extend_other_timeouts() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = session_with_mode(
            dir.path().join("claude"),
            dir.path(),
            Arc::new(Discard),
            PermissionMode::Allow,
        );
        session.spec.tool_bridge = Some(crate::ToolBridgeSpec {
            helper: PathBuf::from("/workspace/tools/tidebreak-supervised-agent"),
            socket: PathBuf::from("/tmp/managed tools.sock"),
        });
        session.spec.apps = Some(crate::AppsChannelSpec {
            mcp_endpoint_url: "http://localhost:9999/apps".into(),
            token: "fixture".into(),
        });
        session.spec.extra_env = vec![
            ("MCP_TOOL_TIMEOUT".into(), "5000".into()),
            ("CLAUDE_AUTO_BACKGROUND_TASKS".into(), "1".into()),
        ];
        let plan = session.compose_plan_for(None, None).unwrap();
        let position = plan
            .argv
            .iter()
            .position(|arg| arg == "--mcp-config")
            .unwrap();
        let config = read_mcp_config(&plan.argv[position + 1]);
        assert!(config["mcpServers"].get("tb-apps").is_some());
        assert!(config["mcpServers"]["tb-apps"].get("timeout").is_none());
        let human = &config["mcpServers"]["tb-human"];
        assert_eq!(
            human["command"],
            "/workspace/tools/tidebreak-supervised-agent"
        );
        assert_eq!(human["args"], serde_json::json!(["human-mcp"]));
        assert_eq!(human["timeout"], crate::ToolBridgeSpec::HUMAN_TIMEOUT_MS);
        assert_eq!(
            human["env"]["TIDEBREAK_TOOL_SOCKET"],
            "/tmp/managed tools.sock"
        );
        assert_eq!(
            plan.env
                .iter()
                .filter(|(name, _)| name == "MCP_TOOL_TIMEOUT")
                .map(|(_, value)| value.as_str())
                .collect::<Vec<_>>(),
            vec!["5000"]
        );
        assert_eq!(
            plan.env
                .iter()
                .filter(|(name, _)| name == "CLAUDE_AUTO_BACKGROUND_TASKS")
                .map(|(_, value)| value.as_str())
                .collect::<Vec<_>>(),
            vec!["0"]
        );
    }

    /// Stream-json input used to be an image-turn flag. It is the session's
    /// whole delivery channel now, so it must be on for every launch.
    #[test]
    fn every_turn_reads_stream_json_from_a_stdin_that_stays_open() {
        let dir = tempfile::tempdir().unwrap();
        let session = ClaudeSession::new(SessionSpec {
            owner: tidebreak_core::OwnerId::local(),
            session_id: tidebreak_core::SessionId::new(),
            worktree: dir.path().to_path_buf(),
            allowed_read_roots: Vec::new(),
            permission_mode: PermissionMode::Plan,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            relay_key_env: None,
            env: Vec::new(),
            approval: None,
            binary: Some(dir.path().join("claude")),
            sink: Arc::new(Discard),
            browser: None,
            native: None,
            tool_bridge: None,
            apps: None,
            project_config: crate::ProjectConfig::Load,
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

    /// Captures from the pinned release of a background task that ends
    /// between turns.
    fn capture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/claude-code/2.1.259")
    }

    /// A stand-in engine that replays a capture from the pinned release.
    ///
    /// For each user line it reads, it prints what the real engine printed
    /// after that line and before the next one. The captured client uuids
    /// become the ones this session sent, the way the engine names back
    /// whatever uuid it receives.
    fn replay_engine(dir: &Path, capture: &str) -> PathBuf {
        let stdout =
            std::fs::read_to_string(capture_dir().join(format!("{capture}.ndjson"))).unwrap();
        let lines: Vec<&str> = stdout.lines().collect();
        let writes: Vec<serde_json::Value> = serde_json::from_str(
            &std::fs::read_to_string(capture_dir().join(format!("{capture}.stdin.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(
            writes[0]["before"], 0,
            "the engine prints nothing unprompted"
        );
        let captured: Vec<&str> = writes
            .iter()
            .map(|write| write["line"]["uuid"].as_str().unwrap())
            .collect();
        for (index, write) in writes.iter().enumerate() {
            let start = usize::try_from(write["before"].as_u64().unwrap()).unwrap();
            let end = writes.get(index + 1).map_or(lines.len(), |next| {
                usize::try_from(next["before"].as_u64().unwrap()).unwrap()
            });
            let mut segment = lines[start..end].join("\n");
            segment.push('\n');
            for (number, uuid) in captured.iter().enumerate() {
                segment = segment.replace(uuid, &format!("@uuid{}@", number + 1));
            }
            std::fs::write(dir.join(format!("segment-{}.ndjson", index + 1)), segment).unwrap();
        }
        write_engine(
            dir,
            &format!(
                r#"#!/bin/sh
n=0
subst=""
while IFS= read -r line; do
  n=$((n+1))
  uuid=$(printf '%s' "$line" | sed -n 's/.*"uuid":"\([^"]*\)".*/\1/p')
  subst="$subst -e s/@uuid$n@/$uuid/g"
  segment="{dir}/segment-$n.ndjson"
  if [ -f "$segment" ]; then sed $subst "$segment"; fi
done
"#,
                dir = dir.display()
            ),
        )
    }

    fn completions(events: &[HarnessEvent]) -> usize {
        events
            .iter()
            .filter(|event| matches!(event, HarnessEvent::TurnCompleted { .. }))
            .count()
    }

    fn says(events: &[HarnessEvent], text: &str) -> bool {
        events.iter().any(|event| {
            matches!(
                event,
                HarnessEvent::AssistantMessage { text: said, parent_call_id: None } if said == text
            )
        })
    }

    async fn run_to_end(session: &ClaudeSession, text: &str) -> TurnOutcome {
        tokio::time::timeout(Duration::from_secs(10), session.run_turn(turn(text)))
            .await
            .unwrap_or_else(|_| panic!("the turn {text:?} never ended"))
            .unwrap()
    }

    /// Captured on 2.1.259: a background command finishes after the turn's
    /// `result`, and the engine reports it and runs a turn of its own, with
    /// its own `result`. That output arrives between turns. It must reach the
    /// transcript as it happens, as background activity, and the next user
    /// turn must run to its own `result`.
    #[tokio::test]
    async fn a_background_task_that_ends_between_turns_does_not_end_the_next_turn() {
        let dir = tempfile::tempdir().unwrap();
        let binary = replay_engine(dir.path(), "background-turn-between-turns");
        let sink = Arc::new(Recorder::default());
        let session = session_with(binary, dir.path(), sink.clone());

        assert!(matches!(
            run_to_end(&session, "start a background job").await,
            TurnOutcome::Clean
        ));
        let first = sink.snapshot();
        assert!(says(
            &first,
            "Started the background job; it reports when it finishes."
        ));
        assert_eq!(completions(&first), 1);

        // Nobody is running a turn, and the engine's output still arrives.
        let notice = "Background command \"Run a short background job\" completed (exit code 0)";
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let events = sink.snapshot();
                let noticed = events.iter().any(|event| {
                    matches!(event, HarnessEvent::HarnessNotice { message, .. } if message == notice)
                });
                if noticed && says(&events, "The background job finished.") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the background task's end never reached the transcript between turns");
        let between = sink.snapshot();
        assert_eq!(
            completions(&between),
            1,
            "the engine's own turn is not a turn the session reports as ended"
        );
        assert!(
            between[first.len()..].iter().all(|event| !matches!(
                event,
                HarnessEvent::AssistantDelta { .. } | HarnessEvent::ReasoningDelta { .. }
            )),
            "streaming text of a turn nobody asked for stays out"
        );

        assert!(matches!(
            run_to_end(&session, "Say the word done.").await,
            TurnOutcome::Clean
        ));
        let events = sink.snapshot();
        let second = &events[between.len()..];
        assert!(says(second, "done."), "the turn ran to its own answer");
        assert!(matches!(
            second.last(),
            Some(HarnessEvent::TurnCompleted { .. })
        ));
        assert_eq!(completions(&events), 2);
    }

    /// The same capture, with the next turn sent the moment the first one
    /// ends. The engine wrote its own turn's `result` before the user's line
    /// went out, so that `result` must not end the user's turn, whichever
    /// side of the write the reader happens to see it on.
    #[tokio::test]
    async fn a_result_written_before_the_users_line_never_ends_that_turn() {
        let dir = tempfile::tempdir().unwrap();
        let binary = replay_engine(dir.path(), "background-turn-between-turns");
        let sink = Arc::new(Recorder::default());
        let session = session_with(binary, dir.path(), sink.clone());

        run_to_end(&session, "start a background job").await;
        let before = sink.snapshot().len();
        run_to_end(&session, "Say the word done.").await;

        let events = sink.snapshot();
        assert!(says(&events[before..], "done."));
        assert_eq!(completions(&events), 2);
        assert!(matches!(
            events.last(),
            Some(HarnessEvent::TurnCompleted { .. })
        ));
    }

    /// Captured on 2.1.259: the user's line arrives while the engine is
    /// running its own turn. The engine queues it and finishes its own turn
    /// first, with a `result` that comes after the line was sent. That
    /// `result` precedes the line's `started` lifecycle, so it is not the
    /// user's, and the user's turn runs to its own.
    #[tokio::test]
    async fn a_line_queued_behind_the_engines_own_turn_waits_for_its_own_result() {
        let dir = tempfile::tempdir().unwrap();
        let binary = replay_engine(dir.path(), "background-turn-queued-prompt");
        let sink = Arc::new(Recorder::default());
        let session = session_with(binary, dir.path(), sink.clone());

        run_to_end(&session, "start a background job").await;
        let before = sink.snapshot().len();
        run_to_end(&session, "Say the word done.").await;

        let events = sink.snapshot();
        let second = &events[before..];
        assert!(says(second, "The background job finished."));
        assert!(says(second, "done."));
        assert!(
            second.iter().all(|event| !matches!(
                event,
                HarnessEvent::AssistantDelta { text } if !"done.".contains(text.as_str())
            )),
            "only the user's own turn streams text: {second:?}"
        );
        assert_eq!(completions(&events), 2);
        assert!(matches!(
            events.last(),
            Some(HarnessEvent::TurnCompleted { .. })
        ));
    }

    /// Captured on 2.1.259: the user's line arrives while the engine's own
    /// turn is running a tool, and the engine folds the line into that turn.
    /// The turn's `result` names no user line and says the engine started
    /// it, yet it is the end of the user's turn: the line's `started`
    /// lifecycle came first. Waiting for another `result` would hang.
    #[tokio::test]
    async fn a_line_folded_into_the_engines_own_turn_ends_with_that_turn() {
        let dir = tempfile::tempdir().unwrap();
        let binary = replay_engine(dir.path(), "background-turn-folded-prompt");
        let sink = Arc::new(Recorder::default());
        let session = session_with(binary, dir.path(), sink.clone());

        run_to_end(&session, "start a background job").await;
        let before = sink.snapshot().len();
        assert!(matches!(
            run_to_end(&session, "Say the word done.").await,
            TurnOutcome::Clean
        ));

        let events = sink.snapshot();
        assert!(says(&events[before..], "done."));
        assert_eq!(completions(&events), 2);
    }

    /// `CLAUDE_AUTO_BACKGROUND_TASKS` lets the engine move long foreground
    /// work to the background. Every session launches with it off, unless the
    /// session's settings ask for it. The managed human tools force it off,
    /// because they must answer inside the turn that asked.
    #[tokio::test]
    async fn every_session_keeps_foreground_work_in_the_foreground_unless_it_asks() {
        let dir = tempfile::tempdir().unwrap();
        let seen = dir.path().join("seen");
        let binary = write_engine(
            dir.path(),
            &format!(
                r#"#!/bin/sh
printf '%s' "${{CLAUDE_AUTO_BACKGROUND_TASKS-unset}}" > {seen}
while IFS= read -r line; do
  printf '{{"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed","session_id":"sess-bg","usage":{{}}}}\n'
done
"#,
                seen = seen.display()
            ),
        );
        let background = |session: &ClaudeSession| {
            session
                .compose_plan_for(None, None)
                .unwrap()
                .env
                .into_iter()
                .filter(|(name, _)| name == AUTO_BACKGROUND_ENV)
                .map(|(_, value)| value)
                .collect::<Vec<_>>()
        };

        let mut session = session_with(binary, dir.path(), Arc::new(Discard));
        assert_eq!(background(&session), ["0"]);
        session.run_turn(turn("hello")).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&seen).unwrap(),
            "0",
            "the engine child sees it"
        );

        session.spec.extra_env = vec![(AUTO_BACKGROUND_ENV.into(), "1".into())];
        assert_eq!(background(&session), ["1"], "the session asked for it");

        session.spec.tool_bridge = Some(crate::ToolBridgeSpec {
            helper: PathBuf::from("/managed-helper"),
            socket: PathBuf::from("/managed.sock"),
        });
        assert_eq!(background(&session), ["0"], "managed human tools win");
    }
}
